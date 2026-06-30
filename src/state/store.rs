use crate::hash::fnv1a64_hex;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::io::Write;

/// 状态根目录：$BRK_STATE_DIR → $TMPDIR/breakpoints → /tmp/breakpoints。
pub fn state_root() -> PathBuf {
    if let Ok(d) = std::env::var("BRK_STATE_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(t) = std::env::var("TMPDIR") {
        if !t.is_empty() {
            return PathBuf::from(t).join("breakpoints");
        }
    }
    PathBuf::from("/tmp/breakpoints")
}

/// 按 workspace 路径 fnv1a64 分桶（canonicalize 失败则用原路径字符串）。
pub fn workspace_bucket(root: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let key = fnv1a64_hex(&[&canon.to_string_lossy()]);
    state_root().join(key)
}

pub fn jobs_dir(ws: &Path) -> PathBuf { ws.join("jobs") }
pub fn job_json(ws: &Path, id: &str) -> PathBuf { jobs_dir(ws).join(format!("{id}.json")) }
pub fn job_log(ws: &Path, id: &str) -> PathBuf { jobs_dir(ws).join(format!("{id}.log")) }
pub fn job_result(ws: &Path, id: &str) -> PathBuf { jobs_dir(ws).join(format!("{id}.result.json")) }

/// 原子写：创建父目录 → 写唯一 tmp → rename 覆盖目标。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("f");
    let tmp = dir.join(format!(".{fname}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 解析 TTL 字符串（"60s"/"30m"/"24h"/"2d"）；任何解析失败回落 24h。
pub fn parse_ttl(s: &str) -> Duration {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = match num.parse() {
        Ok(n) => n,
        Err(_) => return Duration::from_secs(86400),
    };
    let secs = match unit.trim() {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return Duration::from_secs(86400),
    };
    Duration::from_secs(secs)
}

/// 追加一行 NDJSON 进度事件到 jobs/<id>.log。
pub fn append_log(ws: &Path, id: &str, line: &str) -> std::io::Result<()> {
    let p = job_log(ws, id);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
    writeln!(f, "{line}")
}

/// 原子写 result.json（后台作业的一次性投递缓冲）。
pub fn write_result(ws: &Path, id: &str, json: &str) -> std::io::Result<()> {
    atomic_write(&job_result(ws, id), json.as_bytes())
}

/// 读 result.json 原文；不存在 → None。
pub fn read_result(ws: &Path, id: &str) -> Option<String> {
    std::fs::read_to_string(job_result(ws, id)).ok()
}

pub fn job_args(ws: &Path, id: &str) -> PathBuf {
    jobs_dir(ws).join(format!("{id}.args.json"))
}

/// 原子写 jobs/<id>.args.json（父 --background 序列化 ReviewArgs，worker 读回）。
pub fn write_args_raw(ws: &Path, id: &str, json: &str) -> std::io::Result<()> {
    atomic_write(&job_args(ws, id), json.as_bytes())
}

pub fn read_args_raw(ws: &Path, id: &str) -> Option<String> {
    std::fs::read_to_string(job_args(ws, id)).ok()
}

pub fn job_result_md(ws: &Path, id: &str) -> PathBuf {
    jobs_dir(ws).join(format!("{id}.result.md"))
}

/// 原子写 result.md（worker 完成时与 result.json 一并落，避免 result 命令反序列化 report 图）。
pub fn write_result_md(ws: &Path, id: &str, md: &str) -> std::io::Result<()> {
    atomic_write(&job_result_md(ws, id), md.as_bytes())
}

pub fn read_result_md(ws: &Path, id: &str) -> Option<String> {
    std::fs::read_to_string(job_result_md(ws, id)).ok()
}

fn consumed_marker(ws: &Path, id: &str) -> PathBuf {
    jobs_dir(ws).join(format!("{id}.consumed"))
}

/// 标记 result 已被首次取用（写当前 unix 秒，作 grace 起点）。
pub fn mark_consumed(ws: &Path, id: &str) -> std::io::Result<()> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    atomic_write(&consumed_marker(ws, id), now.to_string().as_bytes())
}

/// 读 consumed 标记时间戳；无则 None。
pub fn result_consumed_at(ws: &Path, id: &str) -> Option<u64> {
    std::fs::read_to_string(consumed_marker(ws, id)).ok()?.trim().parse().ok()
}

use crate::state::job::{list_states, JobStatus};

/// 机会式 prune：删已终态（done/failed/cancelled）且 started_at+ttl < now 的作业全部文件。
/// pending/running 永不删（避免误删在跑作业，靠 OS /tmp + 后续 prune 兜底）。
pub fn prune(ws: &Path, ttl: Duration, now: u64) {
    let ttl_secs = ttl.as_secs();
    for st in list_states(ws) {
        let terminal = matches!(
            st.status,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled
        );
        if !terminal {
            continue;
        }
        if st.started_at.saturating_add(ttl_secs) >= now {
            continue;
        }
        let id = &st.job_id;
        let _ = std::fs::remove_file(job_json(ws, id));
        let _ = std::fs::remove_file(job_log(ws, id));
        let _ = std::fs::remove_file(job_result(ws, id));
        let _ = std::fs::remove_file(job_result_md(ws, id));
        let _ = std::fs::remove_file(consumed_marker(ws, id));
        let _ = std::fs::remove_file(job_args(ws, id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn state_root_prefers_brk_state_dir() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK 串行化 env 变更
        unsafe { std::env::set_var("BRK_STATE_DIR", "/custom/brk"); }
        assert_eq!(state_root(), PathBuf::from("/custom/brk"));
        unsafe { std::env::remove_var("BRK_STATE_DIR"); }
    }

    #[test]
    fn workspace_bucket_is_stable_hash_under_root() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("BRK_STATE_DIR", "/s"); }
        let b1 = workspace_bucket(Path::new("/proj/a"));
        let b2 = workspace_bucket(Path::new("/proj/a"));
        assert_eq!(b1, b2, "same root → same bucket");
        assert!(b1.starts_with("/s"), "bucket under state_root");
        assert_ne!(workspace_bucket(Path::new("/proj/b")), b1, "different root → different bucket");
        unsafe { std::env::remove_var("BRK_STATE_DIR"); }
    }

    #[test]
    fn atomic_write_creates_dirs_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("jobs/x.json");
        atomic_write(&p, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
        // 覆盖写
        atomic_write(&p, b"world").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "world");
        // 不留 tmp 文件
        let leftover: Vec<_> = std::fs::read_dir(dir.path().join("jobs")).unwrap()
            .filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".tmp.")).collect();
        assert!(leftover.is_empty(), "no tmp leftover");
    }

    #[test]
    fn parse_ttl_units() {
        assert_eq!(parse_ttl("60s"), Duration::from_secs(60));
        assert_eq!(parse_ttl("30m"), Duration::from_secs(1800));
        assert_eq!(parse_ttl("24h"), Duration::from_secs(86400));
        assert_eq!(parse_ttl("2d"), Duration::from_secs(172800));
        assert_eq!(parse_ttl("garbage"), Duration::from_secs(86400), "回落 24h");
    }

    #[test]
    fn append_log_accumulates_ndjson() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        append_log(ws, "j", r#"{"event":"start"}"#).unwrap();
        append_log(ws, "j", r#"{"event":"done"}"#).unwrap();
        let content = std::fs::read_to_string(job_log(ws, "j")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("start") && lines[1].contains("done"));
    }

    #[test]
    fn result_write_read_and_consume_marker() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        assert!(read_result(ws, "j").is_none());
        write_result(ws, "j", r#"{"verdict":"reject"}"#).unwrap();
        assert_eq!(read_result(ws, "j").unwrap(), r#"{"verdict":"reject"}"#);
        // md 同样往返
        assert!(read_result_md(ws, "j").is_none());
        write_result_md(ws, "j", "# Review\nreject").unwrap();
        assert_eq!(read_result_md(ws, "j").unwrap(), "# Review\nreject");
        // 未取用 → 无 consumed 标记
        assert!(result_consumed_at(ws, "j").is_none());
        mark_consumed(ws, "j").unwrap();
        // grace 标记记了一个时间戳（写入用 0，断言读回 Some）
        assert!(result_consumed_at(ws, "j").is_some());
    }

    use crate::state::job::{write_state, JobState, JobStatus};

    fn dated_job(ws: &Path, id: &str, status: JobStatus, started_at: u64) {
        let mut st = JobState::new(id, started_at);
        st.status = status;
        write_state(ws, &st).unwrap();
        append_log(ws, id, "{}").unwrap();
    }

    #[test]
    fn args_raw_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        assert!(read_args_raw(ws, "j").is_none());
        write_args_raw(ws, "j", r#"{"mode":"doc"}"#).unwrap();
        assert_eq!(read_args_raw(ws, "j").unwrap(), r#"{"mode":"doc"}"#);
    }

    #[test]
    fn review_args_serde_round_trips() {
        // ReviewArgs 可序列化/反序列化（worker 经 args 文件重建）
        let json = serde_json::to_string(&crate::cli::ReviewArgs {
            mode: Some("doc".into()), base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, lenses: None, models: None, models_all: false,
            no_correlate: false, block_threshold: None, conf_floor: None,
            background: true, format: "json".into(), targets: vec!["spec.md".into()],
        }).unwrap();
        let back: crate::cli::ReviewArgs = serde_json::from_str(&json).unwrap();
        assert_eq!(back.targets, vec!["spec.md".to_string()]);
        assert_eq!(back.mode.as_deref(), Some("doc"));
    }

    #[test]
    fn prune_removes_expired_terminal_jobs_keeps_active() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let ttl = Duration::from_secs(100);
        let now = 1000u64;
        dated_job(ws, "old-done", JobStatus::Done, 800);        // 800+100=900 < 1000 → 删
        dated_job(ws, "fresh-done", JobStatus::Done, 950);      // 950+100=1050 > 1000 → 留
        dated_job(ws, "old-running", JobStatus::Running, 800);  // 活跃 → 留（即便过期）
        prune(ws, ttl, now);
        assert!(crate::state::job::read_state(ws, "old-done").is_none(), "expired terminal job pruned");
        assert!(crate::state::job::read_state(ws, "fresh-done").is_some(), "fresh job kept");
        assert!(crate::state::job::read_state(ws, "old-running").is_some(), "active job kept");
        // old-done 的 .log 也被删
        assert!(!job_log(ws, "old-done").exists());
    }
}
