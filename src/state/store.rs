use crate::hash::fnv1a64_hex;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::io::Write;
use serde::{Deserialize, Serialize};

// ── manifest / partial 数据结构 ────────────────────────────────────────────────

/// manifest 每行：输入快照（含渲染后 prompt 全文）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellInput {
    pub cell: String,
    pub model: String,
    pub prompt: String,
}

/// partial 每行：单个 cell 的执行结果。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellRecord {
    pub cell: String,
    pub model: String,
    pub ok: bool,
    pub findings: Vec<crate::domain::Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<String>,
}

impl CellRecord {
    /// 便捷构造：ok=true, warn=None。
    pub fn ok(cell: impl Into<String>, model: impl Into<String>, findings: Vec<crate::domain::Finding>) -> Self {
        Self { cell: cell.into(), model: model.into(), ok: true, findings, warn: None }
    }
}

// ── manifest / partial 文件路径 ────────────────────────────────────────────────

pub fn job_manifest(ws: &Path, id: &str) -> PathBuf {
    jobs_dir(ws).join(format!("{id}.manifest.ndjson"))
}

pub fn job_partial(ws: &Path, id: &str) -> PathBuf {
    jobs_dir(ws).join(format!("{id}.partial.ndjson"))
}

// ── manifest 读写 ──────────────────────────────────────────────────────────────

/// 原子写全部 manifest 行（一次性覆盖）。
pub fn write_manifest(ws: &Path, id: &str, inputs: &[CellInput]) -> std::io::Result<()> {
    let mut buf = String::new();
    for inp in inputs {
        buf.push_str(&serde_json::to_string(inp).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?);
        buf.push('\n');
    }
    atomic_write(&job_manifest(ws, id), buf.as_bytes())
}

/// 读 manifest；文件不存在或行解析失败均跳过，不 panic。
pub fn read_manifest(ws: &Path, id: &str) -> Vec<CellInput> {
    let content = match std::fs::read_to_string(job_manifest(ws, id)) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    content.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

// ── partial 读写 ───────────────────────────────────────────────────────────────

/// 逐行原子 append 一条 CellRecord（仿 append_log）。
pub fn append_partial(ws: &Path, id: &str, record: &CellRecord) -> std::io::Result<()> {
    let p = job_partial(ws, id);
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(record).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&p)?;
    writeln!(f, "{line}")
}

/// 读 partial；文件不存在或行解析失败均跳过，不 panic。
pub fn read_partial(ws: &Path, id: &str) -> Vec<CellRecord> {
    let content = match std::fs::read_to_string(job_partial(ws, id)) {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    content.lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// 状态根目录：$BRK_STATE_DIR → $TMPDIR/breakpoint → /tmp/breakpoint。
pub fn state_root() -> PathBuf {
    if let Ok(d) = std::env::var("BRK_STATE_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(t) = std::env::var("TMPDIR") {
        if !t.is_empty() {
            return PathBuf::from(t).join("breakpoint");
        }
    }
    PathBuf::from("/tmp/breakpoint")
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

use crate::state::job::{list_states, write_state, JobStatus};

/// 假死阈值：`last_active` 停滞超过此值即判「挂死」（pid 仍活也算）。
/// **兜底语义**：`last_active` 只在**每个 cell 完成**时刷新（心跳粒度=cell），而单 cell 合法
/// 可跑 11–34min（max_iters 12 × per_turn 150s + 候补链）。worker 有自己的 40min job deadline
/// 会先自杀（R1.2）；wedge 只 reap 连自己 deadline 都没触发的**真·卡死**进程，故必须 **> job
/// deadline**，否则会误杀第一个 cell 还没跑完、last_active 仍停在 started_at 的正常作业。
/// 取 60min（> 40min deadline + 余量）。（原 5min 值源自 codex 的 per-turn log 粒度，不适配本
/// 项目的 per-cell 心跳——一线经验推翻。）
const WEDGE_THRESHOLD_SECS: u64 = 3600;

/// 检测 pid 是否仍存活：kill(pid, 0) 成功或 errno==EPERM → alive；ESRCH → dead。
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: kill(pid, 0) 是纯查询调用，不发送任何信号，只用于探测进程是否存在。
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    let errno = unsafe { *libc::__error() };
    // EPERM: 进程存在但无权限发信号 → alive
    // ESRCH: 进程不存在 → dead
    errno == libc::EPERM
}

fn remove_job_files(ws: &Path, id: &str) {
    let _ = std::fs::remove_file(job_json(ws, id));
    let _ = std::fs::remove_file(job_log(ws, id));
    let _ = std::fs::remove_file(job_result(ws, id));
    let _ = std::fs::remove_file(job_result_md(ws, id));
    let _ = std::fs::remove_file(consumed_marker(ws, id));
    let _ = std::fs::remove_file(job_args(ws, id));
}

/// 机会式 prune：
/// - 终态作业（done/failed/cancelled/timed_out）：started_at+ttl < now 时才真删文件（回收磁盘）。
/// - 非终态作业（pending/running）：
///   - pid 已死，或 last_active 停滞超过 `WEDGE_THRESHOLD_SECS`（即便 pid 仍活）→ 转 `Failed`
///     留证（写 error），**不删文件**。
///   - pid 不存在且 started_at+ttl < now：从未真正起过 worker，无证据可留，回收删除。
pub fn prune(ws: &Path, ttl: Duration, now: u64) {
    let ttl_secs = ttl.as_secs();
    for mut st in list_states(ws) {
        let terminal = matches!(
            st.status,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled | JobStatus::TimedOut
        );
        if terminal {
            if st.started_at.saturating_add(ttl_secs) < now {
                remove_job_files(ws, &st.job_id);
            }
        } else if let Some(pid) = st.pid {
            let dead = !pid_alive(pid);
            let wedged = now.saturating_sub(st.last_active) > WEDGE_THRESHOLD_SECS;
            if dead || wedged {
                st.error = Some(if dead { "reaped: dead pid" } else { "reaped: wedged (no heartbeat)" }.to_string());
                st.status = JobStatus::Failed;
                let _ = write_state(ws, &st);
            }
        } else if st.started_at.saturating_add(ttl_secs) < now {
            // 无 pid 且超过 TTL：悬空 pending（从未起过 worker），无证据可留，回收删除
            remove_job_files(ws, &st.job_id);
        }
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
        dated_job(ws, "old-running", JobStatus::Running, 800);  // 无 pid，过期 → 现在也会被删（悬空 pending）
        prune(ws, ttl, now);
        assert!(crate::state::job::read_state(ws, "old-done").is_none(), "expired terminal job pruned");
        assert!(crate::state::job::read_state(ws, "fresh-done").is_some(), "fresh job kept");
        assert!(crate::state::job::read_state(ws, "old-running").is_none(), "no-pid expired running job pruned");
        // old-done 的 .log 也被删
        assert!(!job_log(ws, "old-done").exists());
    }

    /// I1/R2.2：spawn `true`（立即退出），等待其结束（pid 已死），写 Running + pid=Some(dead)
    /// → prune（now 未过 ttl）→ 断言转 `Failed` 留证（不再蒸发，文件仍在）。
    /// 另保留 pid=自己（alive）+ last_active 新鲜的 Running，断言仍原样（未被回收/转态）。
    #[test]
    fn prune_reaps_dead_pid_non_terminal_job() {
        use crate::state::job::{write_state, JobState};
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        // 用很长的 TTL，确保时间维度不会触发回收
        let ttl = Duration::from_secs(86400);
        let now = 1000u64;

        // spawn `true`（立即退出），等它结束确保 pid 已 dead
        let mut child = Command::new("true").spawn().expect("spawn true");
        let dead_pid = child.id();
        child.wait().expect("wait child");

        // 写 Running 状态 + dead pid
        let mut st_dead = JobState::new("dead-worker", 500);
        st_dead.status = JobStatus::Running;
        st_dead.pid = Some(dead_pid);
        write_state(ws, &st_dead).unwrap();

        // 保留 pid=自己（alive）+ last_active 贴近 now（未挂死）的 Running
        let self_pid = std::process::id();
        let mut st_alive = JobState::new("alive-worker", 500);
        st_alive.status = JobStatus::Running;
        st_alive.pid = Some(self_pid);
        st_alive.last_active = now;
        write_state(ws, &st_alive).unwrap();

        prune(ws, ttl, now);

        let dead_back = crate::state::job::read_state(ws, "dead-worker");
        assert_eq!(
            dead_back.as_ref().map(|s| s.status),
            Some(JobStatus::Failed),
            "dead-pid Running job should transition to Failed"
        );
        assert!(
            dead_back.unwrap().error.is_some(),
            "Failed transition should record error reason"
        );
        assert!(job_json(ws, "dead-worker").exists(), "证据保留：文件不删");

        let alive_back = crate::state::job::read_state(ws, "alive-worker").unwrap();
        assert_eq!(
            alive_back.status,
            JobStatus::Running,
            "alive-pid + fresh heartbeat job should NOT be reaped/transitioned"
        );
    }

    /// R1.2/2.2：活 pid（自身进程）但 `last_active` 远古停滞 > WEDGE_THRESHOLD → 判假死转 `Failed`，
    /// 文件保留（留证，不蒸发）。
    #[test]
    fn prune_wedged_live_pid_transitions_failed_keeps_files() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = crate::state::job::JobState::new("j", 0);
        st.status = crate::state::job::JobStatus::Running;
        st.pid = Some(std::process::id()); // 活 pid（自身）
        st.last_active = 0;                 // 远古，停滞
        crate::state::job::write_state(ws, &st).unwrap();
        prune(ws, parse_ttl("24h"), 99999);  // now 远超 wedge
        let back = crate::state::job::read_state(ws, "j").unwrap();
        assert_eq!(back.status, crate::state::job::JobStatus::Failed, "假死活 pid 应转 Failed");
        assert!(crate::state::job::read_state(ws, "j").is_some(), "证据保留不删");
    }

    /// R1.2 兜底语义回归：正常作业第一个 cell 未完成、last_active 停在 started_at 但仍在 wedge
    /// 窗口内（此处 20min < 60min 阈值）→ **不**被误判假死（活 pid），保持 Running。
    /// 防「per-cell 心跳粒度 + 过小 wedge 阈值」误杀在跑作业。
    #[test]
    fn prune_keeps_fresh_running_job_within_wedge() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = crate::state::job::JobState::new("j", 0);
        st.status = crate::state::job::JobStatus::Running;
        st.pid = Some(std::process::id()); // 活 pid
        st.last_active = 0;
        crate::state::job::write_state(ws, &st).unwrap();
        prune(ws, parse_ttl("24h"), 1200); // now=20min < WEDGE(60min)：不该 reap
        let back = crate::state::job::read_state(ws, "j").unwrap();
        assert_eq!(back.status, crate::state::job::JobStatus::Running, "wedge 窗口内正常作业不该转 Failed");
    }

    /// R2.2：终态（TimedOut，新纳入终态集合）+ 超 TTL → 真删文件，确认删除路径仍工作。
    #[test]
    fn prune_removes_expired_terminal_timed_out_job() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let ttl = Duration::from_secs(100);
        let now = 1000u64;
        dated_job(ws, "old-timeout", JobStatus::TimedOut, 800); // 800+100=900 < 1000 → 删
        prune(ws, ttl, now);
        assert!(
            crate::state::job::read_state(ws, "old-timeout").is_none(),
            "expired TimedOut terminal job pruned (真删)"
        );
    }

    /// I1：验证 pid_alive 对自身 pid 返回 true（基础正确性）。
    #[test]
    fn pid_alive_self_is_alive() {
        assert!(pid_alive(std::process::id()), "自身进程应为 alive");
    }

    #[test]
    fn manifest_and_partial_roundtrip_and_diff() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let inputs = vec![
            CellInput { cell: "bundle[a]".into(), model: "m1".into(), prompt: "P1".into() },
            CellInput { cell: "bundle[b]".into(), model: "m1".into(), prompt: "P2".into() },
            CellInput { cell: "bundle[c]".into(), model: "m1".into(), prompt: "P3".into() },
        ];
        write_manifest(ws, "j", &inputs).unwrap();
        assert_eq!(read_manifest(ws, "j").len(), 3);
        // 完成 1 个
        append_partial(ws, "j", &CellRecord::ok("bundle[a]", "m1", vec![])).unwrap();
        let done: std::collections::BTreeSet<String> =
            read_partial(ws, "j").into_iter().filter(|r| r.ok).map(|r| r.cell).collect();
        let pending: Vec<&CellInput> =
            inputs.iter().filter(|c| !done.contains(&c.cell)).collect();
        assert_eq!(pending.len(), 2, "manifest − partial(done) = 待补跑");
    }
}
