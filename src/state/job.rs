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
    #[serde(default)]
    pub deadline_at: Option<u64>,
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
            last_active: 1000, deadline_at: None,
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
}
