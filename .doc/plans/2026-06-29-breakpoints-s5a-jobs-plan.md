# breakpoints S5-a — 作业侧（状态存储 + job 状态机 + detached worker） Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development（推荐）。Steps 用 checkbox（`- [ ]`）跟踪。

**Goal:** 让 `brk` 二进制可被 SKILL 异步消费：`brk review --background` 立即返回 job-id、detached worker 后台跑评审写状态、`brk status/result/cancel` 读盘协调；状态文件 ephemeral（机会式 TTL prune）。

**Architecture:** 无常驻进程（设计 §6 模型 A）。`src/state/` 纯存储层（路径分桶 / 原子写 / job 状态机 / NDJSON 日志 / result 一次性投递 / TTL prune），零评审知识。`cli.rs` 加 `run_review_dispatch`（统一前后台的 mode 探测+评审分发）与 `run_job`（状态机包裹评审，FakeProvider 进程内可测）。`main.rs` 加 `status/result/cancel/worker` 子命令 + detached spawn（re-exec 自身 + `libc::setsid`，薄缝）。

**Tech Stack:** Rust 2021、serde/serde_json、std::fs（原子写 tmp+rename）、std::process::Command + CommandExt（spawn/pre_exec/process_group）、libc（setsid/kill，**已在依赖树**，仅声明为直接依赖）、std::time::SystemTime。

## Global Constraints

- **引擎层冻结**：`src/tools/`、`src/provider/tool_loop.rs`、`src/provider/backends.rs` 不得改。
- **TLS 必须 rustls 无 OpenSSL**（`cargo tree -i openssl-sys` 须空）。本计划新增的**唯一**直接依赖是 `libc`（已在树，经 tokio/aws-lc-sys 间接引入），不引其他 crate。
- **状态文件 ephemeral**：`$BRK_STATE_DIR`（默认 `$TMPDIR/breakpoints`，回落 `/tmp/breakpoints`）/ `<workspace-fnv1a64>` / `jobs/`。原子写=tmp+rename。无 purge 命令，靠机会式 TTL prune + OS /tmp 清理兜底。
- **workspace 分桶用 `fnv1a64_hex`**（偏离设计原文 sha256：目录桶键非安全敏感，避免 sha2 依赖）。
- **detach = re-exec 自身隐藏 `worker` 子命令 + `libc::setsid`**（新会话脱离 controlling tty）。父进程 `--background` 立即打印 job-id 退出。
- **退出码约定**：复用 `exit_code`（reject→10 / approve+failed→2 / clean→0）；`run_job` 失败→2。
- **测试无真实 key**：`run_job` 用 FakeProvider + doc 模式（临时 doc 文件、无 git）进程内测；detach spawn glue 作薄缝 reviewed-not-unit-tested。
- **时间注入**：`run_job(started_at)` / `prune(now)` 取 `u64` unix 秒参数，生产传 `SystemTime::now()`、测试传固定值（确定性）。
- `.doc/`、`.superpowers/` 为 gitignored scratch。

## 文件结构（创建/修改）

- 创建 `src/state/mod.rs`（re-export）、`src/state/store.rs`（路径/原子写/ttl/log/result/prune）、`src/state/job.rs`（JobState/JobStatus/Progress + read/write/list）。
- 修改 `src/lib.rs`：`pub mod state;`。
- 修改 `Cargo.toml`：`[dependencies]` 加 `libc = "0.2"`。
- 修改 `src/cli.rs`：`ReviewArgs` 加 serde derive；新增 `run_review_dispatch` + `run_job`。
- 修改 `src/main.rs`：`Cmd` 加 `Status/Result/Cancel/Worker` 变体 + `--background` 实装 + detached spawn + 子命令 handler + 启动时机会式 prune。

---

### Task 1: state 路径分桶 + 原子写 + TTL 解析

**Files:**
- Create: `src/state/mod.rs`、`src/state/store.rs`
- Modify: `src/lib.rs`（加 `pub mod state;`）
- Test: `src/state/store.rs`（内联）

**Interfaces:**
- Produces:
  - `pub fn state_root() -> PathBuf`（`$BRK_STATE_DIR` → `$TMPDIR/breakpoints` → `/tmp/breakpoints`）
  - `pub fn workspace_bucket(root: &Path) -> PathBuf`（`state_root()/fnv1a64_hex(canonical root)`）
  - `pub fn jobs_dir(ws: &Path) -> PathBuf`、`pub fn job_json/job_log/job_result(ws,id) -> PathBuf`
  - `pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()>`
  - `pub fn parse_ttl(s: &str) -> std::time::Duration`

- [ ] **Step 1: lib.rs 注册模块 + 建 mod.rs**

`src/lib.rs` 在已有 `pub mod` 列表里加一行 `pub mod state;`（位置按字母序，靠近 `pub mod positioning;`）。

创建 `src/state/mod.rs`：
```rust
pub mod store;
pub mod job;
```

- [ ] **Step 2: 写失败测试** 创建 `src/state/store.rs`，先只放测试：

```rust
use crate::hash::fnv1a64_hex;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
}
```

Run: `cargo test --lib state::store::tests::parse_ttl_units`
Expected: FAIL（函数未定义）

- [ ] **Step 3: 实现**（`src/state/store.rs`，加在 `use` 之后、`#[cfg(test)]` 之前）

```rust
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
```

- [ ] **Step 4: 跑测试** Run: `cargo test --lib state::store` → PASS
- [ ] **Step 5: Commit**
```bash
git add src/lib.rs src/state/mod.rs src/state/store.rs
git commit -m "feat(s5a): state store paths, atomic write, ttl parse"
```

---

### Task 2: JobState / JobStatus 类型 + 状态读写列出

**Files:**
- Create: `src/state/job.rs`
- Test: `src/state/job.rs`（内联）

**Interfaces:**
- Consumes: `store::{job_json, jobs_dir, atomic_write}`、`domain::Mode`。
- Produces:
  - `pub enum JobStatus { Pending, Running, Done, Failed, Cancelled }`（serde lowercase, PartialEq）
  - `pub struct Progress { pub done: u32, pub total: u32 }`
  - `pub struct JobState { job_id, status, mode: Option<Mode>, started_at: u64, progress, models, lenses, pid: Option<u32>, exit_code: Option<i32>, error: Option<String>, result_path: Option<String> }`（Serialize+Deserialize）
  - `pub fn write_state(ws, &JobState) -> io::Result<()>`、`pub fn read_state(ws, id) -> Option<JobState>`、`pub fn list_states(ws) -> Vec<JobState>`

- [ ] **Step 1: 写失败测试**（`src/state/job.rs`）

```rust
use crate::domain::Mode;
use crate::state::store::{atomic_write, job_json, jobs_dir};
use serde::{Deserialize, Serialize};
use std::path::Path;

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
}
```

Run: `cargo test --lib state::job::tests::state_round_trips_through_disk` → FAIL

- [ ] **Step 2: 实现**（`src/state/job.rs`，`use` 之后、`#[cfg(test)]` 之前）

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
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
```

注：`Mode`（`src/domain/input.rs`）当前是否 `Deserialize`？若只有 `Serialize`，本 task 需给 `Mode` 补 `Deserialize` derive（它是简单 enum，安全）。实现时确认并补齐。

- [ ] **Step 3: 跑测试** Run: `cargo test --lib state::job` → PASS
- [ ] **Step 4: Commit**
```bash
git add src/state/job.rs src/domain/input.rs
git commit -m "feat(s5a): JobState/JobStatus types + state read/write/list"
```

---

### Task 3: NDJSON 进度日志 + result 一次性投递

**Files:**
- Modify: `src/state/store.rs`（新增 `append_log` / `write_result` / `take_result` / `mark_consumed` / `result_consumed`）
- Test: `src/state/store.rs`（内联）

**Interfaces:**
- Produces:
  - `pub fn append_log(ws: &Path, id: &str, line: &str) -> io::Result<()>`（追加一行 NDJSON）
  - `pub fn write_result(ws, id, json: &str)` / `pub fn read_result(ws, id) -> Option<String>`（result.json 原文）
  - `pub fn job_result_md(ws, id) -> PathBuf` / `pub fn write_result_md(ws, id, md: &str)` / `pub fn read_result_md(ws, id) -> Option<String>`（result.md：worker 完成时同渲染落盘，`result --format md` 直接读，**避免反序列化整个 report 图**）
  - `pub fn mark_consumed(ws: &Path, id: &str) -> io::Result<()>`（写 `<id>.consumed` 标记，记 grace 起点）
  - `pub fn result_consumed_at(ws: &Path, id: &str) -> Option<u64>`（读 consumed 标记时间戳，无则 None）

- [ ] **Step 1: 写失败测试**（追加到 `src/state/store.rs` 的 `mod tests`）

```rust
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
```

注：`mark_consumed` 写入的时间戳来自 `SystemTime::now()`（生产用）；本 task 测试只断言 Some/None，不钉具体值。

Run: `cargo test --lib state::store::tests::append_log_accumulates_ndjson` → FAIL

- [ ] **Step 2: 实现**（`src/state/store.rs`，加在 `parse_ttl` 之后）

```rust
use std::io::Write;

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
```

- [ ] **Step 3: 跑测试** Run: `cargo test --lib state::store` → PASS
- [ ] **Step 4: Commit**
```bash
git add src/state/store.rs
git commit -m "feat(s5a): NDJSON progress log + result one-time delivery + consume marker"
```

---

### Task 4: 机会式 TTL prune

**Files:**
- Modify: `src/state/store.rs`（新增 `prune`）
- Test: `src/state/store.rs`（内联）

**Interfaces:**
- Produces: `pub fn prune(ws: &Path, ttl: Duration, now: u64)`——删除 workspace 下 `started_at + ttl < now` 的 done/failed/cancelled 作业全部文件（`.json`/`.log`/`.result.json`/`.consumed`），以及 result 已 consumed 且超 grace 的 result 文件。pending/running 不删（即便超 ttl 也保留，避免误删在跑作业；靠 OS /tmp 兜底）。

> 设计 §6：作业结束且 result 被取用后、或超 state_ttl 兜底 → 自动删。本实现取保守策略：只 prune 已终态（done/failed/cancelled）且超 ttl 的作业；活跃作业（pending/running）永不 prune。

- [ ] **Step 1: 写失败测试**（追加到 `mod tests`）

```rust
    use crate::state::job::{write_state, JobState, JobStatus};

    fn dated_job(ws: &Path, id: &str, status: JobStatus, started_at: u64) {
        let mut st = JobState::new(id, started_at);
        st.status = status;
        write_state(ws, &st).unwrap();
        append_log(ws, id, "{}").unwrap();
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
```

Run: `cargo test --lib state::store::tests::prune_removes_expired_terminal_jobs_keeps_active` → FAIL

- [ ] **Step 2: 实现**（`src/state/store.rs`，加在 `result_consumed_at` 之后）

```rust
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
    }
}
```

注：`consumed_marker` 当前是私有 `fn`；`prune` 同模块可见，无需改可见性。Task 5 还会在此删除列表补 `job_args(ws, id)`。

- [ ] **Step 3: 跑测试** Run: `cargo test --lib state::store` → PASS
- [ ] **Step 4: Commit**
```bash
git add src/state/store.rs
git commit -m "feat(s5a): opportunistic TTL prune of expired terminal jobs"
```

---

### Task 5: ReviewArgs serde + args 文件读写

**Files:**
- Modify: `src/cli.rs`（`ReviewArgs` 加 `Serialize, Deserialize`）
- Modify: `src/state/store.rs`（新增 `job_args` 路径 + `write_args_raw`/`read_args_raw`）
- Test: `src/state/store.rs`（内联）

**Interfaces:**
- Produces:
  - `ReviewArgs: Clone + Debug + Serialize + Deserialize`
  - `pub fn job_args(ws, id) -> PathBuf`、`pub fn write_args_raw(ws, id, json: &str) -> io::Result<()>`、`pub fn read_args_raw(ws, id) -> Option<String>`

- [ ] **Step 1: ReviewArgs 加 serde derive**（`src/cli.rs:15`）

```rust
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReviewArgs {
```
（其余字段不变——全部是 serde 友好类型 Option<String>/String/Vec<String>/bool/Option<f32>。）

- [ ] **Step 2: 写失败测试**（追加到 `src/state/store.rs` 的 `mod tests`）

```rust
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
```

Run: `cargo test --lib state::store::tests::args_raw_round_trips` → FAIL

- [ ] **Step 3: 实现 args 路径**（`src/state/store.rs`，加在 `job_result` 之后）

```rust
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
```

并在 Task 4 的 `prune` 删除列表里补一行 `let _ = std::fs::remove_file(job_args(ws, id));`（终态作业也清 args 文件）。

- [ ] **Step 4: 跑测试** Run: `cargo test --lib state::store && cargo test --lib cli` → PASS
- [ ] **Step 5: Commit**
```bash
git add src/cli.rs src/state/store.rs
git commit -m "feat(s5a): ReviewArgs serde + per-job args file"
```

---

### Task 6: `run_review_dispatch` + `run_job` 状态机包裹

**Files:**
- Modify: `src/cli.rs`（新增 `run_review_dispatch` + `run_job`）
- Test: `src/cli.rs`（内联，FakeProvider + doc 模式）

**Interfaces:**
- Consumes: `run_code_review`/`run_review`、`detect_mode`/`collect`、`state::job::*`、`state::store::{write_result, job_result}`、`render`。
- Produces:
  - `pub async fn run_review_dispatch(args: &ReviewArgs, provider: &dyn ReviewProvider, cfg: &Config, cwd: &Path) -> anyhow::Result<(ReviewReport, i32)>`（统一 mode 探测 + 评审分发；前台 main 与 worker 共用）
  - `pub async fn run_job(ws: &Path, job_id: &str, args: &ReviewArgs, provider: &dyn ReviewProvider, cfg: &Config, cwd: &Path, started_at: u64) -> i32`（pending→running→done+result / failed+error）

- [ ] **Step 1: 写失败测试**（追加到 `src/cli.rs` 末尾的测试模块，或新建 `#[cfg(test)] mod job_tests`）

```rust
#[cfg(test)]
mod job_tests {
    use super::*;
    use crate::state::job::{read_state, JobStatus};
    use crate::state::store::read_result;
    use crate::provider::fake::FakeProvider;
    use crate::domain::{RawFinding, Severity};
    use std::collections::BTreeMap;

    fn doc_args(target: &str) -> ReviewArgs {
        ReviewArgs {
            mode: Some("doc".into()), base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, lenses: None, models: None, models_all: false,
            no_correlate: false, block_threshold: None, conf_floor: None,
            background: true, format: "json".into(), targets: vec![target.into()],
        }
    }
    fn cfg() -> Config {
        Config::from_layers(&[
            "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
             [review]\ndefault_models=[\"alpha\"]\n",
        ]).unwrap()
    }

    #[tokio::test]
    async fn run_job_done_writes_result_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("spec.md");
        std::fs::write(&doc, "intro\nthe wrong claim here\nend\n").unwrap();
        let dp = doc.to_string_lossy().to_string();
        // alpha 报一条锚得到的 finding
        let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
        scripted.insert("alpha".into(), vec![RawFinding {
            severity: Severity::High, title: "t".into(), body: "b".into(), file: dp.clone(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "the wrong claim here".into(), lens: None,
        }]);
        let provider = FakeProvider { scripted };
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        crate::state::job::write_state(&ws, &crate::state::job::JobState::new("j1", 500)).unwrap();

        let code = run_job(&ws, "j1", &doc_args(&dp), &provider, &cfg(), dir.path(), 500).await;

        let st = read_state(&ws, "j1").unwrap();
        assert_eq!(st.status, JobStatus::Done);
        assert_eq!(st.exit_code, Some(code));
        assert!(st.result_path.is_some());
        // result.json 落盘且是合法 JSON
        let res = read_result(&ws, "j1").unwrap();
        assert!(res.contains("\"verdict\""));
    }

    #[tokio::test]
    async fn run_job_failed_records_error() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        crate::state::job::write_state(ws, &crate::state::job::JobState::new("j2", 0)).unwrap();
        // targets 指向不存在文件 → collect_doc 失败 → run_review_dispatch Err → failed
        let provider = FakeProvider { scripted: BTreeMap::new() };
        let code = run_job(ws, "j2", &doc_args("/nonexistent/zzz.md"), &provider, &cfg(), dir.path(), 0).await;
        assert_eq!(code, 2);
        let st = read_state(ws, "j2").unwrap();
        assert_eq!(st.status, JobStatus::Failed);
        assert!(st.error.is_some());
    }
}
```

Run: `cargo test --lib cli::job_tests::run_job_done_writes_result_and_state` → FAIL

- [ ] **Step 2: 实现 `run_review_dispatch`**（`src/cli.rs`，加在 `run_code_review` 之后；把 main.rs 里的 mode 探测+证据+分发逻辑抽到此处）

```rust
/// 统一 mode 探测 + 评审分发（前台 main 与后台 worker 共用）。
/// code：git changed_files → run_code_review；doc：run_review。
pub async fn run_review_dispatch(
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    cwd: &Path,
) -> anyhow::Result<(ReviewReport, i32)> {
    let scope_enum = if args.scope == "branch" { Scope::Branch } else { Scope::WorkingTree };
    let mode_opt = args.mode.as_deref().map(|m| if m == "code" { Mode::Code } else { Mode::Doc });
    let eopts = EvidenceOpts {
        mode: mode_opt,
        base: args.base.clone(),
        scope: scope_enum,
        spec: args.spec.clone(),
        targets: args.targets.clone(),
    };
    let detected = detect_mode(&eopts)?;
    match detected {
        Mode::Code => {
            let changes = crate::evidence::git::changed_files(args.base.as_deref(), scope_enum == Scope::Branch)?;
            run_code_review(args, provider, cfg, changes, cwd).await
        }
        Mode::Doc => run_review(args.clone(), provider, cfg).await,
    }
}
```
需在 cli.rs 顶部 import 补：`use crate::evidence::detect_mode;`（`EvidenceOpts`/`Scope` 已 import；确认 `Mode` 已 import）。

- [ ] **Step 3: 实现 `run_job`**（`src/cli.rs`，加在 `run_review_dispatch` 之后）

```rust
/// 状态机包裹评审：running → (评审) → done+result.json / failed+error。返回退出码。
/// 已被 cancel（状态 Cancelled）时不覆盖为 Done，保留 cancelled 终态。
pub async fn run_job(
    ws: &Path,
    job_id: &str,
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    cwd: &Path,
    started_at: u64,
) -> i32 {
    use crate::state::job::{read_state, write_state, JobState, JobStatus};
    use crate::state::store::{job_result, write_result};

    let mut st = read_state(ws, job_id).unwrap_or_else(|| JobState::new(job_id, started_at));
    st.status = JobStatus::Running;
    let _ = write_state(ws, &st);

    match run_review_dispatch(args, provider, cfg, cwd).await {
        Ok((report, code)) => {
            // 同渲染 json + md 落盘（result 命令按 format 直读，避免反序列化 report 图）。
            let json = crate::output::render(&report, crate::output::Format::Json);
            let md = crate::output::render(&report, crate::output::Format::Md);
            let _ = write_result(ws, job_id, &json);
            let _ = crate::state::store::write_result_md(ws, job_id, &md);
            // 若期间已被 cancel，保留 cancelled；否则 done。
            let latest = read_state(ws, job_id);
            let cancelled = matches!(latest.as_ref().map(|s| s.status), Some(JobStatus::Cancelled));
            st.status = if cancelled { JobStatus::Cancelled } else { JobStatus::Done };
            st.exit_code = Some(code);
            st.mode = Some(report.mode);
            st.models = report.run.models.clone();
            st.lenses = report.run.lenses.clone();
            st.result_path = Some(job_result(ws, job_id).to_string_lossy().into_owned());
            let _ = write_state(ws, &st);
            code
        }
        Err(e) => {
            st.status = JobStatus::Failed;
            st.error = Some(e.to_string());
            st.exit_code = Some(2);
            let _ = write_state(ws, &st);
            2
        }
    }
}
```

- [ ] **Step 4: 跑测试** Run: `cargo test --lib cli::job_tests` → PASS
- [ ] **Step 5: Commit**
```bash
git add src/cli.rs
git commit -m "feat(s5a): run_review_dispatch + run_job state-machine wrapper"
```

---

### Task 7: detached spawn + `worker` 子命令 + `--background` 实装

**Files:**
- Modify: `Cargo.toml`（加 `libc = "0.2"`）
- Modify: `src/main.rs`（`Cmd` 加 `Worker` 变体；`--background` 分支；`spawn_detached_worker`；重构前台路径用 `run_review_dispatch`）
- Test: 无单测（detach spawn 为薄缝 reviewed-not-unit-tested；run_job 已在 T6 测）

**Interfaces:**
- Consumes: `run_job`/`run_review_dispatch`（T6）、`state::*`。
- Produces: `brk review --background` → 写 pending state + args 文件 + spawn detached `brk worker --job-id <id>` + 打印 job-id 退出；`brk worker --job-id <id>`（hidden）→ 读 args + 建 registry + `run_job`。

> 本 task 的 spawn glue 不可单测（需真进程）。验收靠 `cargo build` + 手动 smoke（可选）+ 评审。job-id 生成用 `fnv1a64_hex(&[pid, secs, cwd])` 取前 12 位 + 计数，确保进程内唯一。

- [ ] **Step 1: Cargo.toml 加 libc**（`[dependencies]` 末尾）

```toml
libc = "0.2"
```
Run: `cargo build`（确认 libc 解析，无新增 OpenSSL）；`cargo tree -i openssl-sys` 须空。

- [ ] **Step 2: main.rs — `Cmd` 加 `Worker` 变体 + job-id/spawn 辅助**

在 `enum Cmd` 里 `Review { ... }` 之后加：
```rust
    /// 隐藏：后台 worker（由 `review --background` re-exec 拉起，用户不直接调）。
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        job_id: String,
    },
```

在 `main.rs` 加辅助函数（`#[tokio::main] async fn main` 之外）：
```rust
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 生成进程内唯一 job-id（pid+secs+cwd 的 fnv 前 12 位）。
fn gen_job_id(cwd: &std::path::Path) -> String {
    let seed = format!("{}-{}", std::process::id(), now_secs());
    let h = breakpoints::hash::fnv1a64_hex(&[&seed, &cwd.to_string_lossy()]);
    h[..12].to_string()
}

/// spawn detached worker：re-exec 自身 `worker --job-id <id>`，setsid 新会话、stdio→null。
/// 返回子进程 pid。
fn spawn_detached_worker(job_id: &str) -> std::io::Result<u32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("worker").arg("--job-id").arg(job_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid 仅调一个 async-signal-safe 系统调用，新建会话脱离 controlling tty。
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                // setsid 失败不致命（已 fork 的子进程），继续；进程组隔离仍由 fork 提供。
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    Ok(child.id())
}
```

- [ ] **Step 3: main.rs — `--background` 分支 + 前台重构**

在 `Cmd::Review { .. }` 分支里，构造 `args` 与 `cwd` 之后、构造 registry **之前**插入后台短路：
```rust
            // 后台：写 pending + args 文件 → spawn detached worker → 打印 job-id 退出。
            if background {
                let ws = breakpoints::state::store::workspace_bucket(&cwd);
                let job_id = gen_job_id(&cwd);
                // 机会式 prune（与前台一致，见 Task 8 也在前台跑）
                breakpoints::state::store::prune(
                    &ws,
                    breakpoints::state::store::parse_ttl(&cfg.review.state_ttl),
                    now_secs(),
                );
                let args_json = serde_json::to_string(&args).unwrap_or_default();
                if breakpoints::state::store::write_args_raw(&ws, &job_id, &args_json).is_err() {
                    eprintln!("error: failed to write job args");
                    std::process::exit(2);
                }
                let mut st = breakpoints::state::job::JobState::new(&job_id, now_secs());
                let _ = breakpoints::state::job::write_state(&ws, &st);
                match spawn_detached_worker(&job_id) {
                    Ok(pid) => {
                        st.pid = Some(pid);
                        let _ = breakpoints::state::job::write_state(&ws, &st);
                        println!("{job_id}");
                        std::process::exit(0);
                    }
                    Err(e) => {
                        eprintln!("error: failed to spawn worker: {e}");
                        std::process::exit(2);
                    }
                }
            }
```
（前台路径：把原 registry 构造 + `match detected { run_code_review / run_review }` 替换为构造 registry 后调 `run_review_dispatch(&args, &registry, &cfg, &cwd)` 再 `render` + exit。registry 构造逻辑不变——见现有 main.rs 第 108–133 行；分发改用 `run_review_dispatch`。）

- [ ] **Step 4: main.rs — `Cmd::Worker` 分支**

在 `match cli.cmd { ... }` 加：
```rust
        Cmd::Worker { job_id } => {
            let cfg = Config::load().unwrap_or_default();
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoints::state::store::workspace_bucket(&cwd);
            let args_json = match breakpoints::state::store::read_args_raw(&ws, &job_id) {
                Some(j) => j,
                None => { eprintln!("error: no args for job {job_id}"); std::process::exit(2); }
            };
            let args: ReviewArgs = match serde_json::from_str(&args_json) {
                Ok(a) => a,
                Err(e) => { eprintln!("error: bad args: {e}"); std::process::exit(2); }
            };
            // 探测 mode 以建 registry（worker 用真 provider）。
            let detected = /* 同前台：detect_mode(&eopts) */;
            // 建 registry（复用前台逻辑：resolve_models → 每 model 一个 ToolLoopProvider 实例）
            let registry = /* 同前台 registry 构造 */;
            let started = breakpoints::state::job::read_state(&ws, &job_id).map(|s| s.started_at).unwrap_or_else(now_secs);
            let code = breakpoints::cli::run_job(&ws, &job_id, &args, &registry, &cfg, &cwd, started).await;
            std::process::exit(code);
        }
```
注：worker 的 registry 构造与 mode 探测**与前台完全相同**——为避免重复，把前台「探测 mode → resolve_models → 构造 ProviderRegistry」抽成一个 main.rs 内的辅助 `fn build_registry(args, cfg, cwd) -> (Mode, ProviderRegistry)` 或 `Result`，前台与 worker 都调它。实现时落地这个抽取（消除两处重复）。

- [ ] **Step 5: 验收** Run: `cargo build && cargo clippy --all-targets -- -D warnings && cargo test`
Expected: 编译通过、clippy 干净、既有测试全绿。`cargo tree -i openssl-sys` 空。
（可选手动 smoke：配一个真 provider 后 `brk review --background --mode doc <doc>` 应立即打印 job-id；本 task 不强制。）

- [ ] **Step 6: Commit**
```bash
git add Cargo.toml Cargo.lock src/main.rs
git commit -m "feat(s5a): --background spawns detached worker (re-exec + setsid); hidden worker subcommand"
```

---

### Task 8: `status` / `result` / `cancel` 子命令 + 启动 prune

**Files:**
- Modify: `src/main.rs`（`Cmd` 加 `Status/Result/Cancel`；handler；前台启动机会式 prune）
- Test: `src/main.rs` 或 `tests/` — handler 核心逻辑抽函数后单测；`cancel` 的 kill 为薄缝

**Interfaces:**
- Produces:
  - `brk status [job-id]`：无 id → 列本 workspace 全部作业（id/status/mode/progress 一行）；有 id → 打印该 JobState（JSON）。
  - `brk result <job-id> [--format json|md]`：done → 渲染 result（json 原样 / md 重渲染）+ `mark_consumed`；未完成 → 提示 + 非零退出；缺失/过期 → 错误。退出码沿用作业自身 exit_code（reject→10 等）。
  - `brk cancel <job-id>`：running/pending 且有 pid → `libc::kill(pid, SIGTERM)` + 写 status=cancelled；否则提示。
  - 前台每次 `brk` 启动机会式 `prune` 本 workspace（已在 T7 后台分支加；本 task 补前台 Review 与 status/result/cancel 入口）。

- [ ] **Step 1: main.rs — `Cmd` 加三个变体**

```rust
    Status { job_id: Option<String> },
    Result {
        job_id: String,
        #[arg(long, default_value = "json")]
        format: String,
    },
    Cancel { job_id: String },
```

- [ ] **Step 2: 写可测的纯逻辑 + 失败测试**

把 status 列表渲染抽成纯函数便于测：
```rust
/// 把作业列表渲染成每行一条的摘要（status 命令无 id 时用）。
fn render_job_list(jobs: &[breakpoints::state::job::JobState]) -> String {
    let mut lines: Vec<String> = jobs.iter().map(|s| {
        let mode = s.mode.map(|m| format!("{m:?}")).unwrap_or_else(|| "?".into());
        format!("{}  {:?}  {}  {}/{}", s.job_id, s.status, mode, s.progress.done, s.progress.total)
    }).collect();
    lines.sort();
    lines.join("\n")
}
```
在 `main.rs` 的 `#[cfg(test)] mod tests` 加：
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use breakpoints::state::job::{JobState, JobStatus};
    #[test]
    fn render_job_list_one_line_per_job_sorted() {
        let mut a = JobState::new("aaa", 0); a.status = JobStatus::Done;
        let mut b = JobState::new("bbb", 0); b.status = JobStatus::Running;
        let out = render_job_list(&[b, a]);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("aaa"), "sorted by id");
        assert!(lines[0].contains("Done") && lines[1].contains("Running"));
    }
}
```
注：`Mode` 需 `Debug`（已是）。`main.rs` 作为 binary crate，其 `#[cfg(test)]` 可测内部 `fn`。

Run: `cargo test --bin brk render_job_list_one_line_per_job_sorted`（若 bin 名不同按实际）→ FAIL（函数未定义）

- [ ] **Step 3: 实现三个 handler**（`main.rs` 的 `match cli.cmd`）

```rust
        Cmd::Status { job_id } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoints::state::store::workspace_bucket(&cwd);
            prune_here(&ws); // 机会式 prune（见 Step 4）
            match job_id {
                None => {
                    let jobs = breakpoints::state::job::list_states(&ws);
                    println!("{}", render_job_list(&jobs));
                    0
                }
                Some(id) => match breakpoints::state::job::read_state(&ws, &id) {
                    Some(st) => { println!("{}", serde_json::to_string_pretty(&st).unwrap_or_default()); 0 }
                    None => { eprintln!("no such job: {id}"); 2 }
                },
            }
        }
        Cmd::Result { job_id, format } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoints::state::store::workspace_bucket(&cwd);
            prune_here(&ws);
            let st = breakpoints::state::job::read_state(&ws, &job_id);
            match st {
                Some(s) if matches!(s.status, breakpoints::state::job::JobStatus::Done) => {
                    // 按 format 直读对应渲染文件（worker 已同落 json+md，无需反序列化）。
                    let content = if format == "md" {
                        breakpoints::state::store::read_result_md(&ws, &job_id)
                            .or_else(|| breakpoints::state::store::read_result(&ws, &job_id))
                    } else {
                        breakpoints::state::store::read_result(&ws, &job_id)
                    };
                    match content {
                        Some(raw) => {
                            println!("{raw}");
                            let _ = breakpoints::state::store::mark_consumed(&ws, &job_id);
                            s.exit_code.unwrap_or(0)
                        }
                        None => { eprintln!("result expired or missing for {job_id}"); 2 }
                    }
                }
                Some(s) => { eprintln!("job {job_id} not done (status: {:?})", s.status); 2 }
                None => { eprintln!("no such job: {job_id}"); 2 }
            }
        }
        Cmd::Cancel { job_id } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoints::state::store::workspace_bucket(&cwd);
            match breakpoints::state::job::read_state(&ws, &job_id) {
                Some(mut s) if matches!(s.status, breakpoints::state::job::JobStatus::Running | breakpoints::state::job::JobStatus::Pending) => {
                    if let Some(pid) = s.pid {
                        // SAFETY: kill 单系统调用；pid 来自本 workspace 自己写的状态。
                        unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
                    }
                    s.status = breakpoints::state::job::JobStatus::Cancelled;
                    let _ = breakpoints::state::job::write_state(&ws, &s);
                    println!("cancelled {job_id}");
                    0
                }
                Some(s) => { eprintln!("job {job_id} not cancellable (status: {:?})", s.status); 2 }
                None => { eprintln!("no such job: {job_id}"); 2 }
            }
        }
```

- [ ] **Step 4: 前台启动机会式 prune helper + 接入 Review 前台**

加 `main.rs` 辅助：
```rust
fn prune_here(ws: &std::path::Path) {
    let cfg = Config::load().unwrap_or_default();
    breakpoints::state::store::prune(
        ws,
        breakpoints::state::store::parse_ttl(&cfg.review.state_ttl),
        now_secs(),
    );
}
```
在 `Cmd::Review` 前台路径（非 background）构造 registry 前，加一行 `prune_here(&breakpoints::state::store::workspace_bucket(&cwd));`（前台评审也顺手清过期作业）。

- [ ] **Step 5: 验收** Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo tree -i openssl-sys`
Expected: 全绿、clippy 干净、openssl-sys 空。

- [ ] **Step 6: Commit**
```bash
git add src/main.rs
git commit -m "feat(s5a): status/result/cancel subcommands + opportunistic prune on startup"
```

---

## Self-Review

- **Spec 覆盖**（设计 §6 + §12 + S5-a 决策）：状态布局分桶=T1 ✓；原子写=T1 ✓；TTL 解析+机会式 prune=T1/T4/T8 ✓；job 状态机 pending→running→done|failed|cancelled=T2/T6/T8 ✓；NDJSON 日志=T3 ✓；result 一次性投递+grace consume=T3/T8 ✓；detached worker(re-exec+setsid)=T7 ✓；`--background` 返回 job-id=T7 ✓；status/result/cancel=T8 ✓；libc 直接依赖=T7 ✓。
- **类型一致性**：`JobState`/`JobStatus`/`Progress`（T2）字段在 run_job（T6）、prune（T4）、handlers（T8）一致；`run_review_dispatch`/`run_job` 签名（T6）与 worker 调用（T7）一致；`ReviewArgs` serde（T5）与 args 文件读写（T5）+ worker 反序列化（T7）一致。
- **占位符扫描**：T7 Step 4 的 `/* 同前台 */` 注释是**刻意指示实现者抽取 `build_registry` 复用**（非占位代码），已在该 step 注明落地抽取；其余步骤均完整代码。
- **跨 task 编译依赖**：T2 可能需给 `Mode` 补 `Deserialize`（已注明）；T5 给 `ReviewArgs` 加 serde；T6 抽 `run_review_dispatch` 改动 main.rs 前台分发（T7 落地）；T4 prune 删文件列表在 T5 补 args 文件、保持同步。
- **冻结约束**：无 task 触碰 src/tools/、tool_loop.rs、backends.rs。
- **测试无 key**：run_job（T6）走 doc 模式 + FakeProvider + 临时文件，无 git、无真实 key；状态层（T1–T4）纯 tempdir；detach spawn（T7）与 cancel kill（T8）为薄缝。
- **时间确定性**：prune（T4）/run_job（T6）取 now/started_at 参数，测试传固定值。
