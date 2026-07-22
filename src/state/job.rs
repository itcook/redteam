use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
    #[serde(rename = "timed_out")]
    TimedOut,
}

impl JobStatus {
    /// 终态：不可再转移，落定后永不被覆盖（E12 首个终态封盘）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled | JobStatus::TimedOut
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Collecting, // worker 起手，收集证据，fanout 前
    Fanout,     // cell 并发执行中（progress.done 推进）
    Finalizing, // 尾段：dedup/verify/verdict（progress/last_active 静止）
    Refute,     // finalizing 子阶段：targeted refute（LLM 并发；note 带 targets 数）
}

/// phase 时间线一格（2026-07-03 grill 定案：finalizing 黑盒可观测）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhaseStamp {
    pub phase: Phase,
    pub at: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobState {
    pub job_id: String,
    pub status: JobStatus,
    pub started_at: u64,
    #[serde(default)]
    pub progress: Progress,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result_path: Option<String>,
    #[serde(default)]
    pub last_active: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub deadline_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub phase: Option<Phase>,
    /// phase 转换时间线（append-only；旧 state 缺字段 default 空）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub phase_history: Vec<PhaseStamp>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub workspace: Option<String>,
}

impl JobState {
    pub fn new(job_id: &str, started_at: u64) -> Self {
        JobState {
            job_id: job_id.to_string(),
            status: JobStatus::Pending,
            started_at,
            progress: Progress::default(),
            models: vec![],
            pid: None,
            exit_code: None,
            error: None,
            result_path: None,
            last_active: started_at,
            deadline_at: None,
            phase: None,
            phase_history: vec![],
            workspace: None,
        }
    }

    /// 设当前 phase 并 append 时间线（所有 phase 赋值点统一走这里，防漏 stamp）。
    pub fn set_phase(&mut self, phase: Phase, note: Option<String>) {
        self.phase = Some(phase);
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.phase_history.push(PhaseStamp { phase, at, note });
    }
}

/// 状态落库（design 2026-07-03 #8：state 层入 proj.db；`ws` = 项目根）。
pub fn write_state(ws: &Path, st: &JobState) -> std::io::Result<()> {
    let json = serde_json::to_string(st)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    crate::db::ProjDb::open(ws)
        .and_then(|d| d.job_put_state(&st.job_id, &json))
        .map_err(std::io::Error::other)
}

/// 读状态；不存在或解析失败 → None。
pub fn read_state(ws: &Path, id: &str) -> Option<JobState> {
    crate::db::ProjDb::open(ws)
        .ok()
        .and_then(|d| d.job_get_state(id).ok().flatten())
        .and_then(|j| serde_json::from_str(&j).ok())
}

/// 列出本项目全部 job 状态（跨项目聚合已随 workspace bucket 退役，YAGNI 定案）。
pub fn list_states(ws: &Path) -> Vec<JobState> {
    crate::db::ProjDb::open(ws)
        .ok()
        .and_then(|d| d.job_all_states().ok())
        .map(|rows| rows.iter().filter_map(|j| serde_json::from_str(j).ok()).collect())
        .unwrap_or_default()
}

/// 守卫式状态更新（E12 并发组核心）：db 事务内取现状 → 反序列化 → **若已终态则整个更新放弃**
/// （首个终态封盘：任一终态落定后，progress/终态/任何字段都不再被写）→ 否则 `mutate` 改后写回。
/// 返回是否实际写入（`false`=被终态封盘拦下 / job 不存在 / 反序列化失败）。
/// 所有 job 状态写点（进度、phase、终态、cancel、收尸、panic 兜底）统一走这里，杜绝非原子 RMW。
pub fn update_state_if_active(
    ws: &Path,
    id: &str,
    mutate: impl FnOnce(&mut JobState),
) -> std::io::Result<bool> {
    let mut db = crate::db::ProjDb::open(ws).map_err(std::io::Error::other)?;
    let mut wrote = false;
    let mut mutate = Some(mutate);
    db.job_update_guarded(id, |cur| {
        let json = cur?;
        let mut st: JobState = serde_json::from_str(&json).ok()?;
        if st.status.is_terminal() {
            return None; // 首个终态封盘
        }
        (mutate.take().unwrap())(&mut st);
        let out = serde_json::to_string(&st).ok()?;
        wrote = true;
        Some(out)
    })
    .map_err(std::io::Error::other)?;
    Ok(wrote)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let st = JobState {
            job_id: "job1".into(), status: JobStatus::Running,
            started_at: 1000, progress: Progress { done: 1, total: 3 },
            models: vec!["alpha".into()],
            pid: Some(4242), exit_code: None, error: None, result_path: None,
            last_active: 1000, deadline_at: None, phase: None, phase_history: vec![], workspace: None,
        };
        write_state(ws, &st).unwrap();
        let back = read_state(ws, "job1").unwrap();
        assert_eq!(back.status, JobStatus::Running);
        assert_eq!(back.progress.total, 3);
        assert_eq!(back.pid, Some(4242));
    }

    #[test]
    fn list_states_returns_all_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        for id in ["a", "b", "c"] {
            write_state(ws, &JobState::new(id, 0)).unwrap();
        }
        let mut ids: Vec<String> = list_states(ws).into_iter().map(|s| s.job_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn read_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_state(dir.path(), "nope").is_none());
    }

    #[test]
    fn jobstate_new_fields_default_and_roundtrip() {
        let mut s = JobState::new("j", 100);
        s.last_active = 150;
        s.deadline_at = Some(2500);
        s.status = JobStatus::TimedOut;
        let j = serde_json::to_string(&s).unwrap();
        let back: JobState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.last_active, 150);
        assert_eq!(back.deadline_at, Some(2500));
        assert_eq!(back.status, JobStatus::TimedOut);
        // 旧文件（无新字段）可读
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[]}"#
        ).unwrap();
        assert_eq!(old.last_active, 0);
        assert_eq!(old.deadline_at, None);
    }

    #[test]
    fn jobstate_ignores_legacy_lenses_key() {
        // E8-I：旧 proj.db 记录仍带 "lenses" 键——serde 静默忽略，resume/status 不炸。
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[],"lenses":[]}"#
        ).unwrap();
        assert_eq!(old.job_id, "x");
        assert_eq!(old.status, JobStatus::Running);
    }

    #[test]
    fn phase_serializes_lowercase_and_roundtrips() {
        let mut s = JobState::new("j", 100);
        s.phase = Some(Phase::Fanout);
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"phase\":\"fanout\""), "phase 应小写序列化: {j}");
        let back: JobState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.phase, Some(Phase::Fanout));
    }

    #[test]
    fn phase_and_workspace_absent_default_none() {
        // 旧文件（无 phase/workspace 键）→ None
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[]}"#
        ).unwrap();
        assert_eq!(old.phase, None);
        assert_eq!(old.workspace, None);
    }

    #[test]
    fn deadline_at_none_omits_key() {
        // §3.1 一致性：deadline_at=None 序列化不含键（与 pid 等一致）
        let s = JobState::new("j", 100); // deadline_at=None
        let j = serde_json::to_string(&s).unwrap();
        assert!(!j.contains("deadline_at"), "deadline_at=None 应省略键: {j}");
        let mut s2 = JobState::new("j", 100);
        s2.deadline_at = Some(2500);
        assert!(serde_json::to_string(&s2).unwrap().contains("\"deadline_at\":2500"));
    }

    #[test]
    fn workspace_roundtrips() {
        let mut s = JobState::new("j", 100);
        s.workspace = Some("/Users/x/repo".into());
        let back: JobState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.workspace.as_deref(), Some("/Users/x/repo"));
    }

    #[test]
    fn set_phase_appends_history_with_note() {
        let mut st = JobState::new("j", 0);
        st.set_phase(Phase::Fanout, None);
        st.set_phase(Phase::Refute, Some("9 targets".into()));
        assert_eq!(st.phase, Some(Phase::Refute));
        assert_eq!(st.phase_history.len(), 2);
        assert_eq!(st.phase_history[1].note.as_deref(), Some("9 targets"));
        assert!(st.phase_history[1].at >= st.phase_history[0].at);
        // 旧 state（无 phase_history 字段）反序列化零值兼容
        let old = r#"{"job_id":"x","status":"running","started_at":1}"#;
        let back: JobState = serde_json::from_str(old).unwrap();
        assert!(back.phase_history.is_empty());
    }

    #[test]
    fn is_terminal_covers_four() {
        assert!(!JobStatus::Pending.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert!(JobStatus::Done.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
        assert!(JobStatus::Cancelled.is_terminal());
        assert!(JobStatus::TimedOut.is_terminal());
    }

    #[test]
    fn update_active_applies_and_freezes_on_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        write_state(ws, &st).unwrap();
        // 非终态 → 应用
        let wrote = update_state_if_active(ws, "j", |s| s.status = JobStatus::Done).unwrap();
        assert!(wrote);
        assert_eq!(read_state(ws, "j").unwrap().status, JobStatus::Done);
        // 已终态 → 拒写（首个终态封盘），status 不动
        let wrote2 = update_state_if_active(ws, "j", |s| s.status = JobStatus::Cancelled).unwrap();
        assert!(!wrote2);
        assert_eq!(read_state(ws, "j").unwrap().status, JobStatus::Done);
    }

    #[test]
    fn update_active_missing_job_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!update_state_if_active(dir.path(), "nope", |_s| {}).unwrap());
    }

    #[test]
    fn concurrent_progress_no_lost_update() {
        // 10 线程各 +1，跨独立连接靠守卫串行化 → done 精确 10（无 RMW 丢更新）
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let ws: Arc<std::path::PathBuf> = Arc::new(dir.path().to_path_buf());
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        write_state(&ws, &st).unwrap();
        let mut hs = vec![];
        for _ in 0..10 {
            let ws = Arc::clone(&ws);
            hs.push(std::thread::spawn(move || {
                update_state_if_active(&ws, "j", |s| s.progress.done += 1).unwrap();
            }));
        }
        for h in hs { h.join().unwrap(); }
        assert_eq!(read_state(&ws, "j").unwrap().progress.done, 10);
    }
}
