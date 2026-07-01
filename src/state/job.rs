use crate::domain::Mode;
use crate::state::store::{atomic_write, job_json, jobs_dir};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Collecting, // worker 起手，收集证据，fanout 前
    Fanout,     // cell 并发执行中（progress.done 推进）
    Finalizing, // 尾段：dedup/correlate/verify/verdict（progress/last_active 静止）
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub mode: Option<Mode>,
    pub started_at: u64,
    #[serde(default)]
    pub progress: Progress,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub lenses: Vec<String>,
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
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub workspace: Option<String>,
}

impl JobState {
    pub fn new(job_id: &str, started_at: u64) -> Self {
        JobState {
            job_id: job_id.to_string(),
            status: JobStatus::Pending,
            mode: None,
            started_at,
            progress: Progress::default(),
            models: vec![],
            lenses: vec![],
            pid: None,
            exit_code: None,
            error: None,
            result_path: None,
            last_active: started_at,
            deadline_at: None,
            phase: None,
            workspace: None,
        }
    }
}

/// 原子写 jobs/<id>.json。
pub fn write_state(ws: &Path, st: &JobState) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(st)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    atomic_write(&job_json(ws, &st.job_id), &bytes)
}

/// 读 jobs/<id>.json；不存在或解析失败 → None。
pub fn read_state(ws: &Path, id: &str) -> Option<JobState> {
    let bytes = std::fs::read(job_json(ws, id)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 列出 workspace 下所有 job 状态（跳过 .log/.result.json，只读 *.json 非 result）。
pub fn list_states(ws: &Path) -> Vec<JobState> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(jobs_dir(ws)) else {
        return out;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        // 只认 "<id>.json"，排除 "<id>.result.json"
        if name.ends_with(".json") && !name.ends_with(".result.json") {
            if let Ok(bytes) = std::fs::read(e.path()) {
                if let Ok(st) = serde_json::from_slice::<JobState>(&bytes) {
                    out.push(st);
                }
            }
        }
    }
    out
}

/// 遍历 state_root 下所有 workspace 桶，聚合全部 job 状态。
pub fn list_all_states(state_root: &Path) -> Vec<JobState> {
    let mut out = Vec::new();
    let Ok(buckets) = std::fs::read_dir(state_root) else {
        return out;
    };
    for b in buckets.flatten() {
        if b.path().is_dir() {
            out.extend(list_states(&b.path()));
        }
    }
    out
}

/// status 列表选取：workspaces=None → 仅当前桶；Some(substr) → 扫全部按 workspace 子串筛
/// （空 substr = --workspaces 无值 = 不筛，保留含 workspace=None 的 job）。
pub fn select_jobs(cwd_ws: &Path, state_root: &Path, workspaces: Option<&str>) -> Vec<JobState> {
    match workspaces {
        None => list_states(cwd_ws),
        Some(filter) => list_all_states(state_root)
            .into_iter()
            .filter(|j| j.workspace.as_deref().unwrap_or("").contains(filter))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let st = JobState {
            job_id: "job1".into(), status: JobStatus::Running, mode: Some(Mode::Doc),
            started_at: 1000, progress: Progress { done: 1, total: 3 },
            models: vec!["alpha".into()], lenses: vec!["D1".into()],
            pid: Some(4242), exit_code: None, error: None, result_path: None,
            last_active: 1000, deadline_at: None, phase: None, workspace: None,
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
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[],"lenses":[]}"#
        ).unwrap();
        assert_eq!(old.last_active, 0);
        assert_eq!(old.deadline_at, None);
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
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[],"lenses":[]}"#
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
    fn select_jobs_default_current_ws_only() {
        let root = tempfile::tempdir().unwrap();
        let ws_a = root.path().join("bucketA");
        let ws_b = root.path().join("bucketB");
        write_state(&ws_a, &JobState::new("a", 0)).unwrap();
        write_state(&ws_b, &JobState::new("b", 0)).unwrap();
        // workspaces=None → 只当前桶（ws_a）
        let got = select_jobs(&ws_a, root.path(), None);
        let ids: Vec<_> = got.iter().map(|s| s.job_id.clone()).collect();
        assert_eq!(ids, vec!["a"]);
    }

    #[test]
    fn select_jobs_all_and_substr_filter() {
        let root = tempfile::tempdir().unwrap();
        let ws_a = root.path().join("bucketA");
        let ws_b = root.path().join("bucketB");
        let mut a = JobState::new("a", 0); a.workspace = Some("/repos/sites".into());
        let mut b = JobState::new("b", 0); b.workspace = Some("/repos/api".into());
        write_state(&ws_a, &a).unwrap();
        write_state(&ws_b, &b).unwrap();
        // workspaces=Some("")（--workspaces 无值）→ 全部
        let mut all: Vec<_> = select_jobs(&ws_a, root.path(), Some("")).into_iter().map(|s| s.job_id).collect();
        all.sort();
        assert_eq!(all, vec!["a", "b"]);
        // workspaces=Some("sites") → 只 workspace 含 "sites" 的 a
        let filtered: Vec<_> = select_jobs(&ws_a, root.path(), Some("sites")).into_iter().map(|s| s.job_id).collect();
        assert_eq!(filtered, vec!["a"]);
    }

    #[test]
    fn select_jobs_substr_excludes_none_workspace() {
        let root = tempfile::tempdir().unwrap();
        let ws_a = root.path().join("bucketA");
        write_state(&ws_a, &JobState::new("nows", 0)).unwrap(); // workspace=None
        // 非空子串筛 → workspace=None 的 job 不匹配、被排除
        assert!(select_jobs(&ws_a, root.path(), Some("x")).is_empty());
        // 空子串（--workspaces 无值）→ 保留（含 None-workspace）
        assert_eq!(select_jobs(&ws_a, root.path(), Some("")).len(), 1);
    }
}
