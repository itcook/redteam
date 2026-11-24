# M16 并发组 Implementation Plan（终态覆写 / 猝死永久 running / cancel 盲杀 三向死结 + panic 兜底）

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development。Steps 用 checkbox 跟踪。

> 状态: 锁定 · 设计=decisions **E12**（grill 五裁决 2026-07-21）· 威胁模型=B（本地审不可信仓/PR，恶意仓 `--background` 风暴放大僵尸堆积/盲杀面）· base=dev@f028f92（安全组 dogfood hotfix 后）· SDD 终审=opus

**Goal:** 消除后台 job 生命周期状态机的三个并发/崩溃缺陷（①终态覆写 ②猝死永久 Running ③cancel 盲杀）+ 一个 panic 空洞，靠「守卫式原子读改写 + pid 身份锚 + 惰性收尸 + 进程级 panic hook」四件套。

**Architecture:** 三缺陷共根=所有状态写点做非原子 RMW（`read_state → 改 → write_state`），进程内 `JobCellSink.lock` 跨不进独立的 `cancel` 进程。修法：db.rs 加一个事务内 CAS 原语 `job_update_guarded`（`BEGIN IMMEDIATE` 靠 SQLite 写锁跨进程串行化），state 层薄封装 `update_state_if_active` 承载「首个终态封盘」策略；所有写点改走它；cancel/收尸靠 pid 存活探测 + 进程启动时刻身份锚（防 pid 复用误判）；worker 进程装 panic hook 兜底。**不改评审功能行为**——正常评审路径字节级不变，守卫只在竞争/崩溃/复用边界生效。

**Tech Stack:** Rust 单 crate。SQLite via `rusqlite`（已有 `busy_timeout(5s)`+WAL）。pid 探测 `libc::kill(pid,0)`；启动时刻 macOS `libc::proc_pidinfo(PROC_PIDTBSDINFO)` → `proc_bsdinfo.pbi_start_tvsec`（libc 0.2.186 原生提供，无需手搓 FFI 结构）。panic 兜底 `std::panic::set_hook`。

## Global Constraints
- 注释简体中文；`cargo test --lib` + `tests/review_e2e.rs` 全绿；`cargo clippy` 无新增警告；实施者 commit trailer `Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>`（本批实施者档=sonnet）；**永不 push**。base=dev@f028f92。
- 威胁模型=**B**（本地审不可信仓/PR）。不做 C 级（daemon 常驻收尸 / pidfd 订阅）——推 C。
- **不碰评审功能行为**：正常评审（可信仓/自审、无 cancel、无崩溃）路径的状态时间线与 json/md 契约必须字节级不变；守卫只在终态竞争、进程死亡、pid 复用、panic 边界上生效。
- **首个终态封盘**（E12-Q1 核心不变式）：`JobState` 一旦落任一终态（Done/Failed/Cancelled/TimedOut），**永不被后续任何写覆盖**（含被另一终态覆盖）。非终态字段（progress/last_active/phase/models）在非终态期间仍自由更新。
- **IDE 诊断陈旧，一律以真 `cargo test`/`cargo clippy` 为准**（本批多处改函数签名，rust-analyzer 滞后）。
- **平台**：身份锚 `start_time` 仅 macOS 有实现，其它平台返回 `None`（降级为纯存活探测）——用 `#[cfg(target_os = "macos")]` 分支，非 macOS 编译零 FFI。

## File Structure
- **Modify `src/db.rs`**：加 `job_update_guarded`（事务内 CAS，只认 JSON 字符串、零 `JobState` 知识）。
- **Modify `src/state/job.rs`**：`JobStatus::is_terminal()`；`JobState.pid_start: Option<u64>` 字段（+`new()`+测试字面量）；`update_state_if_active()`（承载终态封盘策略的 state 层封装）。
- **Create `src/procinfo.rs`**：`start_time(pid)`（进程启动时刻，身份锚）+ `is_worker_alive(pid, recorded_start)`（存活探测 + 锚比对）。
- **Modify `src/lib.rs`**：`pub mod procinfo;`。
- **Modify `src/cli.rs`**：`JobCellSink` 去 `lock`、四方法改走 `update_state_if_active`；`run_job` 收尾 + `write_partial_result` 改走守卫；新增 `reap_if_dead()`。
- **Modify `src/main.rs`**：spawn 处（:452）捕获子进程 `pid_start`；`Cmd::Status` 列表/单读接 `reap_if_dead`；`Cmd::Cancel` 重写（守卫 + 存活/身份判定 + 条件 SIGTERM）；`Cmd::Worker` 起手装 panic hook。

依赖序：**T1 → T2 → T3 → T4 → T5 → T6**（SDD 串行执行）。T3/T4/T5/T6 依赖 T1（守卫封装）；T4/T5 依赖 T2（身份锚）；T6 依赖 T1+T3（在已改造的 run_job 之上）。

---

### Task 1: db.rs 守卫原语 + `is_terminal()` + state 层封装（终态封盘）

**Files:**
- Modify: `src/db.rs`（`job_update_guarded`，加在 `job_all_states` 之后 ~line 351）
- Modify: `src/state/job.rs`（`JobStatus::is_terminal`；`update_state_if_active`）
- Test: `src/db.rs` 内 `#[cfg(test)]`；`src/state/job.rs` 内 `#[cfg(test)] mod tests`

**Interfaces:**
- Produces（db 层）：`pub fn job_update_guarded<F>(&mut self, job_id: &str, f: F) -> Result<()> where F: FnOnce(Option<String>) -> Option<String>` —— 事务内取现 `state_json`（空串视同不存在→`None`），交闭包；闭包返 `Some(json)` 才 UPDATE、返 `None` 放弃写。
- Produces（state 层）：`pub fn update_state_if_active(ws: &Path, id: &str, mutate: impl FnOnce(&mut JobState)) -> std::io::Result<bool>` —— 守卫读现状：已终态则整个更新放弃返 `Ok(false)`；非终态则 `mutate` 后写回返 `Ok(true)`；job 不存在返 `Ok(false)`。
- Produces：`impl JobStatus { pub fn is_terminal(self) -> bool }`。

- [ ] **Step 1: db 层失败测试（守卫 CAS 语义）**

加到 `src/db.rs` 末尾 `#[cfg(test)] mod tests`（若无则新建）：

```rust
#[cfg(test)]
mod guard_tests {
    use super::*;

    #[test]
    fn guarded_none_aborts_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ProjDb::open(dir.path()).unwrap();
        db.job_put_state("j", r#"{"v":1}"#).unwrap();
        // 闭包返 None → 不写
        db.job_update_guarded("j", |_cur| None).unwrap();
        assert_eq!(db.job_get_state("j").unwrap().as_deref(), Some(r#"{"v":1}"#));
    }

    #[test]
    fn guarded_some_writes_and_sees_current() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ProjDb::open(dir.path()).unwrap();
        db.job_put_state("j", r#"{"v":1}"#).unwrap();
        db.job_update_guarded("j", |cur| {
            assert_eq!(cur.as_deref(), Some(r#"{"v":1}"#)); // 事务内见现值
            Some(r#"{"v":2}"#.to_string())
        }).unwrap();
        assert_eq!(db.job_get_state("j").unwrap().as_deref(), Some(r#"{"v":2}"#));
    }

    #[test]
    fn guarded_empty_state_is_none() {
        // args/manifest 先建行、state 空串 → 闭包收 None
        let dir = tempfile::tempdir().unwrap();
        let mut db = ProjDb::open(dir.path()).unwrap();
        db.job_put_args("j", r#"{"a":1}"#).unwrap(); // state_json=''
        let mut seen = Some("x".to_string());
        db.job_update_guarded("j", |cur| { seen = cur.clone(); None }).unwrap();
        assert_eq!(seen, None);
    }
}
```

- [ ] **Step 2: 运行验证失败**

Run: `cargo test --lib guard_tests`
Expected: FAIL —— `no method named job_update_guarded`。

- [ ] **Step 3: 实现 db 层守卫原语**

加在 `src/db.rs` 的 `job_all_states`（~line 351）之后：

```rust
    /// 守卫式读改写：`BEGIN IMMEDIATE` 事务内取现 `state_json` → 交调用方闭包 → 闭包返
    /// `Some(新json)` 才 UPDATE、返 `None` 放弃写。空串（args/manifest 先建行、state 未写）
    /// 视同不存在→传 `None`。跨进程原子性靠 SQLite 写锁：`Immediate` 在 BEGIN 即取写锁，
    /// 配合已设的 `busy_timeout(5s)` 让并发写者排队而非丢更新（后台 worker 收尾 × 宿主 cancel
    /// 同刻竞争终态的关键序列化点）。db 层不识 `JobState`——只搬 JSON 字符串。
    pub fn job_update_guarded<F>(&mut self, job_id: &str, f: F) -> Result<()>
    where
        F: FnOnce(Option<String>) -> Option<String>,
    {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<String> = tx
            .query_row("SELECT state_json FROM jobs WHERE job_id=?1", params![job_id], |r| r.get(0))
            .optional()?;
        let current = current.filter(|s| !s.is_empty());
        if let Some(new_json) = f(current) {
            tx.execute(
                "INSERT INTO jobs (job_id, state_json) VALUES (?1,?2)
                 ON CONFLICT(job_id) DO UPDATE SET state_json=?2",
                params![job_id, new_json],
            )?;
        }
        tx.commit()?;
        Ok(())
    }
```

- [ ] **Step 4: 运行验证通过**

Run: `cargo test --lib guard_tests`
Expected: PASS（3 tests）。

- [ ] **Step 5: state 层失败测试（终态封盘 + 无丢更新）**

加到 `src/state/job.rs` 的 `#[cfg(test)] mod tests`：

```rust
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
```

- [ ] **Step 6: 运行验证失败**

Run: `cargo test --lib`
Expected: FAIL —— `is_terminal`/`update_state_if_active` 未定义。

- [ ] **Step 7: 实现 `is_terminal` + `update_state_if_active`**

在 `src/state/job.rs` 的 `JobStatus` 定义之后加 impl（紧跟 enum，line 14 后）：

```rust
impl JobStatus {
    /// 终态：不可再转移，落定后永不被覆盖（E12 首个终态封盘）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled | JobStatus::TimedOut
        )
    }
}
```

在 `write_state`/`read_state` 附近（~line 108 后）加：

```rust
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
```

> 注：`mutate` 装 `Option` 再 `take()`，因闭包签名 `FnOnce(Option<String>) -> Option<String>` 内要移动它一次而外层 `mutate: impl FnOnce` 无法直接被 `FnMut` 借用——`Option::take` 是把 `FnOnce` 搬进 `FnMut` 环境的标准手法。

- [ ] **Step 8: 运行验证通过**

Run: `cargo test --lib`
Expected: PASS（新增 5 测试 + 原有全绿）。

- [ ] **Step 9: Commit**

```bash
git add src/db.rs src/state/job.rs
git commit -m "feat(concurrency): 守卫式状态原语 job_update_guarded + update_state_if_active（首个终态封盘）

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 2: pid 身份锚——`pid_start` 字段 + `procinfo` 模块（存活探测 + 启动时刻比对）

**Files:**
- Create: `src/procinfo.rs`
- Modify: `src/lib.rs`（`pub mod procinfo;`，按字母序插在 `pub mod positioning;` 后、`pub mod project;` 前）
- Modify: `src/state/job.rs`（`JobState.pid_start` 字段 + `new()` + 测试字面量补字段）
- Modify: `src/main.rs`（spawn 处 :452 捕获子进程 `pid_start`）
- Test: `src/procinfo.rs` 内 `#[cfg(test)]`；`src/state/job.rs` 现有测试

**Interfaces:**
- Produces：`crate::procinfo::start_time(pid: u32) -> Option<u64>`（进程启动 Unix 秒；读不到→`None`）。
- Produces：`crate::procinfo::is_worker_alive(pid: u32, recorded_start: Option<u64>) -> bool`（存活探测 + 锚比对；`recorded_start=None` 降级纯存活探测）。
- Produces：`JobState.pid_start: Option<u64>`（父进程 spawn 后记录子进程启动时刻）。
- Consumes：`JobState.pid`（既有）。

- [ ] **Step 1: 写 `procinfo` 失败测试**

新建 `src/procinfo.rs`，先只放测试骨架 + 空模块引用不便；改为先在文件底写测试、Step 3 补实现。本步直接写完整文件含测试（TDD：先测后实，但同文件——先写测试块，实现留空签名使其编译失败）。为清晰，Step 1 写测试，Step 3 写实现体。测试内容：

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_time_self_is_stable() {
        let me = std::process::id();
        let a = start_time(me);
        let b = start_time(me);
        assert!(a.is_some(), "本进程 start_time 应可读");
        assert_eq!(a, b, "同进程两次读取应一致");
    }

    #[test]
    fn alive_self_with_matching_anchor() {
        let me = std::process::id();
        let anchor = start_time(me);
        assert!(is_worker_alive(me, anchor), "本进程 + 吻合锚 → 活");
    }

    #[test]
    fn alive_self_reused_anchor_is_dead() {
        let me = std::process::id();
        // 伪造一个不吻合的锚 → 判定 pid 已被复用（原进程已死）
        let bogus = start_time(me).map(|t| t.wrapping_add(999_999));
        assert!(!is_worker_alive(me, bogus), "锚不符 → 判死（pid 复用）");
    }

    #[test]
    fn alive_self_no_anchor_degrades_to_liveness() {
        let me = std::process::id();
        assert!(is_worker_alive(me, None), "无锚 → 降级纯存活探测 → 活");
    }

    #[test]
    fn dead_pid_is_not_alive() {
        // 起一个立即退出的子进程、回收，再探测其 pid → 已死
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut child = child;
        child.wait().unwrap(); // 回收 zombie，pid 释放
        assert!(!is_worker_alive(pid, None), "已回收的 pid → 死");
    }
}
```

- [ ] **Step 2: 运行验证失败**

先在 `src/lib.rs` 加 `pub mod procinfo;`（否则测试不被编译）。
Run: `cargo test --lib procinfo`
Expected: FAIL —— `start_time`/`is_worker_alive` 未定义。

- [ ] **Step 3: 实现 `procinfo` 模块**

`src/procinfo.rs` 顶部（测试块之上）写：

```rust
//! 进程存活探测 + 身份锚（E12 并发组）：区分「原 job worker 仍存活」与「pid 已被 OS 复用」。
//! 存活探测跨平台（`kill(pid,0)`）；身份锚=进程启动时刻，仅 macOS 有实现（`proc_pidinfo`），
//! 其它平台返回 None → 降级为纯存活探测。

/// 读进程启动时刻（Unix 秒），作 pid 身份锚。读不到（进程已死/无权限/非 macOS）→ None。
#[cfg(target_os = "macos")]
pub fn start_time(pid: u32) -> Option<u64> {
    // libc 0.2.x 原生提供 proc_bsdinfo（含 pbi_start_tvsec: u64）与 PROC_PIDTBSDINFO=3。
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    // proc_pidinfo 成功时返回写入字节数（== 结构体大小）；失败返回 0 或负。
    if n == size {
        Some(info.pbi_start_tvsec)
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
pub fn start_time(_pid: u32) -> Option<u64> {
    None
}

/// 判 job worker 进程是否仍存活（且身份未变）：
/// - `kill(pid,0)` ESRCH（进程不存在）→ false；
/// - 存活但记录了 `recorded_start` 且当前 start-time 不符 → false（pid 已被复用，原进程已死）；
/// - 存活但记录了锚却读不到当前 start-time → false（本属我们的活进程必能读到；读不到=非我们的）；
/// - 存活且（无锚[旧状态/非 macOS 降级] 或 锚吻合）→ true。
pub fn is_worker_alive(pid: u32, recorded_start: Option<u64>) -> bool {
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    // ret==0：可发信号=存活；ret==-1 且 errno==EPERM：存在但无权限（仍算存活）。
    let alive = ret == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !alive {
        return false;
    }
    match recorded_start {
        None => true, // 无锚 → 降级纯存活探测
        Some(rec) => match start_time(pid) {
            Some(cur) => cur == rec,
            None => false, // 存活却读不到锚 → 非原进程，保守判死
        },
    }
}
```

- [ ] **Step 4: 运行验证通过**

Run: `cargo test --lib procinfo`
Expected: PASS（5 tests）。

- [ ] **Step 5: 加 `JobState.pid_start` 字段**

`src/state/job.rs`：在 `pub pid: Option<u32>,`（line 50）之后加：

```rust
    /// pid 身份锚：`pid` 对应进程的启动时刻（Unix 秒），防 pid 复用误判（E12-Q3）。
    /// 父进程 spawn worker 后记录；旧状态缺字段 default None → 降级纯存活探测。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pid_start: Option<u64>,
```

在 `JobState::new` 的字段初始化里（`pid: None,` line 78 之后）加 `pid_start: None,`。
在 `state_round_trips_through_disk` 测试的结构体字面量里（line 140 `pid: Some(4242),` 附近）加 `pid_start: None,`（否则字面量缺字段编译失败）。

- [ ] **Step 6: 加字段兼容性测试**

加到 `src/state/job.rs` 测试块：

```rust
    #[test]
    fn pid_start_roundtrips_and_legacy_defaults_none() {
        let mut s = JobState::new("j", 0);
        s.pid_start = Some(1_700_000_000);
        let back: JobState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.pid_start, Some(1_700_000_000));
        // 旧状态（无 pid_start 键）→ None
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[]}"#
        ).unwrap();
        assert_eq!(old.pid_start, None);
        // pid_start=None 省略键
        assert!(!serde_json::to_string(&JobState::new("j", 0)).unwrap().contains("pid_start"));
    }
```

- [ ] **Step 7: spawn 处捕获子进程 `pid_start`**

`src/main.rs` 的 background 分支（:451-453）：

```rust
                    Ok(pid) => {
                        st.pid = Some(pid);
                        st.pid_start = redteam::procinfo::start_time(pid);
                        let _ = redteam::state::job::write_state(&ws, &st);
                        println!("{job_id}");
                        std::process::exit(0);
                    }
```

> 说明：spawn 刚返回、子进程几乎必然仍活，此刻读其 start-time 最可靠；此处仍是父进程初始化，job 尚未被并发写，直接 `write_state` 无需守卫。

- [ ] **Step 8: 运行验证全绿**

Run: `cargo test --lib`
Expected: PASS（含新字段测试 + procinfo）。

- [ ] **Step 9: Commit**

```bash
git add src/procinfo.rs src/lib.rs src/state/job.rs src/main.rs
git commit -m "feat(concurrency): pid 身份锚 pid_start + procinfo 存活/复用探测

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 3: 接线所有状态写点走守卫封装（`JobCellSink` + `run_job` 收尾 + `write_partial_result`）

**Files:**
- Modify: `src/cli.rs`（`JobCellSink` 结构 + 四方法 line 1017-1059；`write_partial_result` line 1341-1359；`run_job` 三终态臂 line 1385/1407-1446）
- Test: `src/cli.rs` 内既有 `job_tests`（保持全绿即验收）

**Interfaces:**
- Consumes：`crate::state::job::update_state_if_active`（Task 1）。
- Produces：`JobCellSink { ws, job_id }`（去掉 `lock` 字段）；`write_partial_result(ws, job_id, status, report, code, error)`（去掉 `st: &mut JobState` 参数）。

- [ ] **Step 1: 改 `JobCellSink` 结构 + 四方法**

`src/cli.rs` line 1012-1059 整块替换（注释同步：进程内 Mutex 退役，改由守卫跨进程串行化）：

```rust
/// R2.1 job checkpoint sink：扇出前落 manifest（+刷 progress.total），每 cell 完成落 partial
/// （+刷 last_active/progress.done）。所有 JobState 写走 `update_state_if_active`（守卫式
/// 读改写）——事务内 `progress.done += 1` 消除并发 cell 的 RMW 丢更新（原进程内 Mutex 只护
/// worker 自身、跨不进 cancel 进程；守卫靠 SQLite 写锁跨进程串行化，故 Mutex 退役）。
/// 终态封盘：job 已终态（如被 cancel）时守卫拒写，sink 的迟到进度不复活状态。
struct JobCellSink {
    ws: std::path::PathBuf,
    job_id: String,
}

impl CellSink for JobCellSink {
    fn manifest(&self, inputs: &[crate::state::store::CellInput]) {
        let _ = crate::state::store::write_manifest(&self.ws, &self.job_id, inputs);
        let total = inputs.len() as u32;
        let _ = crate::state::job::update_state_if_active(&self.ws, &self.job_id, |st| {
            st.progress.total = total;
            st.set_phase(crate::state::job::Phase::Fanout, None);
        });
    }

    fn on_cell(&self, rec: crate::state::store::CellRecord) {
        let _ = crate::state::store::append_partial(&self.ws, &self.job_id, &rec);
        let _ = crate::state::job::update_state_if_active(&self.ws, &self.job_id, |st| {
            st.last_active = now_unix();
            st.progress.done += 1;
        });
    }

    fn on_phase(&self, phase: crate::state::job::Phase, note: Option<String>) {
        let _ = crate::state::job::update_state_if_active(&self.ws, &self.job_id, |st| {
            st.set_phase(phase, note);
        });
    }

    fn on_meta(&self, models: &[String]) {
        let models = models.to_vec();
        let _ = crate::state::job::update_state_if_active(&self.ws, &self.job_id, |st| {
            st.models = models;
        });
    }
}
```

- [ ] **Step 2: 改 `write_partial_result`（去 `st` 参数、走守卫）**

`src/cli.rs` line 1340-1359 替换：

```rust
/// 落终态部分报告：渲染 json+md、写 result 文件、守卫式把 state 转终态并记录错误原因。
/// 走 `update_state_if_active`：若期间已被 cancel（终态），守卫拒写、保留 Cancelled（首个终态封盘）。
fn write_partial_result(
    ws: &Path,
    job_id: &str,
    status: crate::state::job::JobStatus,
    report: &ReviewReport,
    code: i32,
    error: String,
) {
    let json = crate::output::render(report, crate::output::Format::Json);
    let md = crate::output::render(report, crate::output::Format::Md);
    let _ = crate::state::store::write_result(ws, job_id, &json);
    let _ = crate::state::store::write_result_md(ws, job_id, &md);
    let _ = crate::state::job::update_state_if_active(ws, job_id, |st| {
        st.status = status;
        st.error = Some(error);
        st.exit_code = Some(code);
        st.result_path = Some(format!("proj.db#results/{job_id}"));
    });
}
```

- [ ] **Step 3: 改 `run_job` 三终态臂 + sink 构造**

`src/cli.rs`：line 1385 sink 构造去 `lock`：

```rust
    let sink = JobCellSink { ws: ws.to_path_buf(), job_id: job_id.to_string() };
```

line 1407-1446 三终态臂替换（Done 走守卫、不再手动读 latest 判 cancelled；Err/TimedOut 去掉 `st` 读取与传参）：

```rust
    match outcome {
        Outcome::Done(Ok((mut report, code))) => {
            // dispatch 无 job 上下文（TailCtx.job_id 恒空），此处回填。
            report.run.job_id = job_id.to_string();
            let json = crate::output::render(&report, crate::output::Format::Json);
            let md = crate::output::render(&report, crate::output::Format::Md);
            let _ = write_result(ws, job_id, &json);
            let _ = crate::state::store::write_result_md(ws, job_id, &md);
            let models = report.run.models.clone();
            // 守卫写 Done：若期间已被 cancel（Cancelled 终态），守卫拒写、保留 Cancelled
            // （首个终态封盘）。progress/last_active 由 sink 运行期已写，这里只叠终态字段。
            let _ = crate::state::job::update_state_if_active(ws, job_id, |st| {
                st.status = JobStatus::Done;
                st.exit_code = Some(code);
                st.models = models;
                st.result_path = Some(format!("proj.db#results/{job_id}"));
            });
            code
        }
        Outcome::Done(Err(e)) => {
            let (report, code) = partial_report(ws, job_id, e.to_string());
            write_partial_result(ws, job_id, JobStatus::Failed, &report, code, e.to_string());
            code
        }
        Outcome::TimedOut => {
            let reason = "timed out: deadline exceeded".to_string();
            let (report, code) = partial_report(ws, job_id, reason.clone());
            write_partial_result(ws, job_id, JobStatus::TimedOut, &report, code, reason);
            code
        }
    }
```

> 说明：删除原 line 1421-1423 的 `let latest = read_state(...)`/`cancelled`/`let mut st = latest.unwrap_or(st)` 三行——守卫封装已在事务内原子完成「读现状→判终态→叠加」，无需在应用层再读一遍（那正是 bug① 的窗口）。`run_job` 顶部的 `let mut st = read_state(...)`（line 1376，用于 Running 写与 deadline）保留不动。`read_state` 导入若因此变未用，按 clippy 提示清理。

- [ ] **Step 4: 运行验证生命周期测试全绿**

Run: `cargo test --lib job_tests` 然后 `cargo test --test review_e2e`
Expected: PASS —— 现有 done/cancelled/failed/timeout 生命周期测试全绿（行为保持：正常路径不变、cancelled 仍被保留）。

- [ ] **Step 5: clippy 净**

Run: `cargo clippy --lib 2>&1 | tail -20`
Expected: 无新增警告（若 `read_state` 在某臂变未用，清理未用导入/变量）。

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs
git commit -m "refactor(concurrency): 状态写点全线接守卫封装，进程内 Mutex 退役

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 4: 惰性收尸 `reap_if_dead` + 接入 `Cmd::Status`

**Files:**
- Modify: `src/cli.rs`（新增 `pub fn reap_if_dead`，放在 `run_job` 之后）
- Modify: `src/main.rs`（`Cmd::Status` line 536-539 列表/单读接 `reap_if_dead`）
- Test: `src/cli.rs` 内 `job_tests`（新增收尸测试）

**Interfaces:**
- Consumes：`crate::procinfo::is_worker_alive`（Task 2）、`crate::state::job::update_state_if_active`（Task 1）。
- Produces：`pub fn reap_if_dead(ws: &Path, st: JobState) -> JobState` —— Running 且 worker 已死（或 pid 复用）→ 守卫落 Failed，返回更新后状态；否则原样返回。

- [ ] **Step 1: 写收尸失败测试**

加到 `src/cli.rs` 的 `job_tests`：

```rust
    #[test]
    fn reap_dead_running_job_to_failed() {
        use crate::state::job::{write_state, JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // 起一个立即退出的子进程做「死 worker」
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut child = child; child.wait().unwrap();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        st.pid = Some(pid);
        st.pid_start = None; // 降级纯存活探测；pid 已回收 → 死
        write_state(ws, &st).unwrap();
        let out = reap_if_dead(ws, read_state(ws, "j").unwrap());
        assert_eq!(out.status, JobStatus::Failed);
        assert_eq!(read_state(ws, "j").unwrap().status, JobStatus::Failed);
        assert!(out.error.as_deref().unwrap_or("").contains("reaped"));
    }

    #[test]
    fn reap_leaves_alive_running_untouched() {
        use crate::state::job::{write_state, JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let me = std::process::id(); // 本进程恒活
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        st.pid = Some(me);
        st.pid_start = crate::procinfo::start_time(me);
        write_state(ws, &st).unwrap();
        let out = reap_if_dead(ws, read_state(ws, "j").unwrap());
        assert_eq!(out.status, JobStatus::Running); // 活着 → 不动
    }

    #[test]
    fn reap_ignores_terminal_jobs() {
        use crate::state::job::{write_state, JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Done;
        st.pid = Some(999_999); // 不存在的 pid，但已终态 → 不该被碰
        write_state(ws, &st).unwrap();
        let out = reap_if_dead(ws, read_state(ws, "j").unwrap());
        assert_eq!(out.status, JobStatus::Done);
    }
```

- [ ] **Step 2: 运行验证失败**

Run: `cargo test --lib job_tests::reap`
Expected: FAIL —— `reap_if_dead` 未定义。

- [ ] **Step 3: 实现 `reap_if_dead`**

加在 `src/cli.rs` 的 `run_job` 之后（`#[cfg(test)] mod job_tests` 之前）：

```rust
/// 惰性收尸（E12-Q2）：`status`/`list` 读状态时，对 Running 但 worker 进程已死（或 pid 被复用）
/// 的 job，就地落 **Failed** 终态并返回更新后状态（无常驻守护进程——裸终端工具无 daemon，YAGNI）。
/// 走守卫写：若此刻 worker 恰好真完成写了 Done，守卫拒写、重读返回真终态（无误杀真完成的 job）。
pub fn reap_if_dead(ws: &Path, st: crate::state::job::JobState) -> crate::state::job::JobState {
    use crate::state::job::{read_state, JobStatus};
    if st.status != JobStatus::Running {
        return st;
    }
    let pid = match st.pid {
        Some(p) => p,
        None => return st, // 无 pid 无从判定，保守放过
    };
    if crate::procinfo::is_worker_alive(pid, st.pid_start) {
        return st;
    }
    let _ = crate::state::job::update_state_if_active(ws, &st.job_id, |s| {
        s.status = JobStatus::Failed;
        s.error = Some("worker process died (reaped)".to_string());
        s.exit_code = Some(2);
    });
    read_state(ws, &st.job_id).unwrap_or(st)
}
```

- [ ] **Step 4: 运行验证通过**

Run: `cargo test --lib job_tests::reap`
Expected: PASS（3 tests）。

- [ ] **Step 5: 接入 `Cmd::Status`**

`src/main.rs` line 536-539 替换（列表与单读都过收尸）：

```rust
            let jobs: Option<Vec<redteam::state::job::JobState>> = match &job_id {
                None => Some(
                    redteam::state::job::list_states(&ws)
                        .into_iter()
                        .map(|st| redteam::cli::reap_if_dead(&ws, st))
                        .collect(),
                ),
                Some(id) => redteam::state::job::read_state(&ws, id)
                    .map(|st| vec![redteam::cli::reap_if_dead(&ws, st)]),
            };
```

- [ ] **Step 6: 全量测试 + e2e**

Run: `cargo test --lib && cargo test --test review_e2e`
Expected: PASS。

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat(concurrency): 惰性收尸 reap_if_dead——status/list 见死 worker 落 Failed

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 5: `Cmd::Cancel` 重写（守卫 RMW + 存活/身份判定 + 条件 SIGTERM）

**Files:**
- Modify: `src/main.rs`（`Cmd::Cancel` line 569-587 整块）

**Interfaces:**
- Consumes：`crate::procinfo::is_worker_alive`（Task 2）、`crate::state::job::update_state_if_active`（Task 1）。

- [ ] **Step 1: 重写 `Cmd::Cancel`**

`src/main.rs` line 569-587 整块替换：

```rust
        Cmd::Cancel { job_id } => {
            let ws = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            match redteam::state::job::read_state(&ws, &job_id) {
                Some(s) if matches!(s.status, redteam::state::job::JobStatus::Running | redteam::state::job::JobStatus::Pending) => {
                    // 仅当 worker 进程确活（pid 存活 且 身份锚吻合）才发信号——否则 pid 可能已被
                    // OS 复用给无辜进程，盲发 SIGTERM 会误杀（E12-Q3/bug③）。
                    if let Some(pid) = s.pid {
                        if redteam::procinfo::is_worker_alive(pid, s.pid_start) {
                            // SAFETY: kill 单系统调用；已确认 pid 属本 job 原进程。
                            unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM); }
                        }
                    }
                    // 守卫写 Cancelled：若 worker 已抢先落终态（Done/Failed/TimedOut），守卫拒写、
                    // 保留其终态（首个终态封盘/bug①）。wrote=false 即被封盘。
                    let wrote = redteam::state::job::update_state_if_active(&ws, &job_id, |st| {
                        st.status = redteam::state::job::JobStatus::Cancelled;
                    })
                    .unwrap_or(false);
                    if wrote {
                        println!("cancelled {job_id}");
                        0
                    } else {
                        let now = redteam::state::job::read_state(&ws, &job_id).map(|s| s.status);
                        eprintln!("job {job_id} already finished ({now:?}), not cancelled");
                        2
                    }
                }
                Some(s) => { eprintln!("job {job_id} not cancellable (status: {:?})", s.status); 2 }
                None => { eprintln!("no such job: {job_id}"); 2 }
            }
        }
```

> 行为变更（相对旧码）：①不再无条件 SIGTERM——死/复用 pid 不发信号；②Cancelled 写走守卫——worker 已终态时不覆盖，退 2 并回显真终态（旧码会把 Cancelled 盖在已完成状态上）。正常场景（活 worker 被取消）：SIGTERM + Cancelled，与旧码一致。

- [ ] **Step 2: 手动烟测（活 worker 取消 + 已完成 job 取消）**

Run（活 worker）:
```bash
cargo build --release 2>&1 | tail -3
```
Expected: 编译通过。

单元级验证放在 e2e/既有测试；cancel 的进程级行为靠下一步 e2e + 手测覆盖。

- [ ] **Step 3: 全量测试 + e2e**

Run: `cargo test --lib && cargo test --test review_e2e`
Expected: PASS（既有 cancel 相关测试若断言「cancel 后状态 Cancelled」需仍成立：测试里 job 通常无真实 pid 或 pid 为本测试进程——若既有测试因新增存活判定失败，说明其 fixture 依赖旧的无条件行为，需在测试里把 `pid=None` 或用本进程 pid+锚，使 `is_worker_alive` 走「无 pid 放过」或「活」分支；这属测试 fixture 适配，不改产品行为。将此作为 ⚠️ 项报给控制者裁定）。

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "fix(concurrency): cancel 守卫写 + 存活/身份判定，杜绝盲杀复用 pid 与终态覆盖

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

### Task 6: worker panic hook（真·顶层兜底 → Failed）

**Files:**
- Modify: `src/cli.rs`（新增 `pub fn mark_failed_panic`）
- Modify: `src/main.rs`（`Cmd::Worker` 分支起手、调 `run_job` 前装 panic hook；line ~484-528）
- Test: `src/cli.rs` 内 `job_tests`（`mark_failed_panic` 单测）；panic→Failed 端到端靠手测/e2e

**Interfaces:**
- Consumes：`crate::state::job::update_state_if_active`（Task 1）。
- Produces：`pub fn mark_failed_panic(ws: &Path, job_id: &str, msg: &str)` —— 守卫式把 Running job 落 Failed（error=`panic: <msg>`）。

- [ ] **Step 1: 写 `mark_failed_panic` 失败测试**

加到 `src/cli.rs` 的 `job_tests`：

```rust
    #[test]
    fn mark_failed_panic_sets_failed_with_message_only() {
        use crate::state::job::{write_state, JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        write_state(ws, &st).unwrap();
        mark_failed_panic(ws, "j", "index out of bounds");
        let back = read_state(ws, "j").unwrap();
        assert_eq!(back.status, JobStatus::Failed);
        let err = back.error.unwrap_or_default();
        assert!(err.contains("panic"), "error 应标 panic: {err}");
        assert!(err.contains("index out of bounds"));
    }

    #[test]
    fn mark_failed_panic_respects_terminal_freeze() {
        // panic 发生在真完成之后 → 守卫拒写、保留 Done
        use crate::state::job::{write_state, JobState, JobStatus};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Done;
        write_state(ws, &st).unwrap();
        mark_failed_panic(ws, "j", "late panic");
        assert_eq!(read_state(ws, "j").unwrap().status, JobStatus::Done);
    }
```

- [ ] **Step 2: 运行验证失败**

Run: `cargo test --lib job_tests::mark_failed_panic`
Expected: FAIL —— `mark_failed_panic` 未定义。

- [ ] **Step 3: 实现 `mark_failed_panic`**

加在 `src/cli.rs` 的 `reap_if_dead` 附近：

```rust
/// panic 兜底落终态（E12-Q4）：worker 评审体 panic 时由 panic hook 调用，守卫式把 Running
/// 落 Failed（error 只带 message、不带 backtrace/location——B 下报告要干净）。若 panic 发生在
/// 真完成之后，守卫拒写、保留真终态。
pub fn mark_failed_panic(ws: &Path, job_id: &str, msg: &str) {
    let msg = msg.to_string();
    let _ = crate::state::job::update_state_if_active(ws, job_id, |st| {
        st.status = crate::state::job::JobStatus::Failed;
        st.error = Some(format!("panic: {msg}"));
        st.exit_code = Some(2);
    });
}
```

- [ ] **Step 4: 运行验证通过**

Run: `cargo test --lib job_tests::mark_failed_panic`
Expected: PASS（2 tests）。

- [ ] **Step 5: `Cmd::Worker` 起手装 panic hook**

`src/main.rs` `Cmd::Worker { job_id } => {` 分支内、`let cfg = load_config_warned();`（line 485）**之前**插入 hook 安装（`ws` 在 line 487 才定义，故 hook 需用 `cwd` 派生的 ws；调整为在 ws 定义后、build_registry 前安装）。精确插入点：line 487 `let ws = cwd.clone();` 之后、line 488 读 args 之前：

```rust
            // 进程级 panic 兜底（E12-Q4，真·顶层）：worker 评审体若 panic，落 Failed 终态再正常
            // 展开——否则状态永停 Running（惰性收尸只治「进程已死」，panic 与退出间有窗口）。
            // hook 是 'static Fn，捕获 ws/job_id 的 clone；只取 payload message，不带 backtrace。
            {
                let ws_h = ws.clone();
                let jid_h = job_id.clone();
                let default_hook = std::panic::take_hook();
                std::panic::set_hook(Box::new(move |info| {
                    let msg = info
                        .payload()
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| info.payload().downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown".to_string());
                    redteam::cli::mark_failed_panic(&ws_h, &jid_h, &msg);
                    default_hook(info);
                }));
            }
```

> 为何进程级 hook 而非 `tokio::spawn`+`JoinError::is_panic()`（grill 初拟）：`run_job` 的 `provider: &dyn ReviewProvider`、`sink` 均借用、非 `'static`，`tokio::spawn` 要求 `Future: Send + 'static`——需 Arc 化 provider（跨 main.rs 签名重构）或撞 Send 炸弹。进程级 hook 无借用/`'static`/`Send` 约束、装在 worker 进程独有、且捕获任意 tokio worker 线程的 panic，是「顶层兜底」的字面兑现。详见 decisions E12-Q4。

- [ ] **Step 6: 端到端验证（编译 + 全量）**

Run: `cargo test --lib && cargo test --test review_e2e && cargo build --release 2>&1 | tail -3`
Expected: PASS + 编译通过。

- [ ] **Step 7: Commit**

```bash
git add src/cli.rs src/main.rs
git commit -m "feat(concurrency): worker 进程级 panic hook 兜底落 Failed，消除永久 Running

Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>"
```

---

## 收尾（SDD 控制者执行，非某一任务）

- 六任务全绿后，整支交 **opus** 终审（`scripts/review-package $(git merge-base dev HEAD) HEAD`）——微妙并发变更，用最强模型。
- 终审清后 `make build`（放置插件二进制到 `target/aarch64-apple-darwin/release/redteam`）+ `make verify`（防命令镜像漂移；本批不动 commands，应零 diff）。
- 合 dev（本地 merge，**永不 push**）。刷 `.doc/roadmap/` 三联 + `.doc/TODO`：M16 并发组 ✅。
- Minor findings 滚进本文件下方 ledger 段（控制者维护），终审据此裁定合并前必修项。

## Self-Review（写者已核）

- **Spec 覆盖**：三死结（①终态覆写=T1 封盘策略 + T3 收尾走守卫；②猝死永久 Running=T4 收尸 + T6 panic hook；③cancel 盲杀=T5 存活/身份判定）+ panic 兜底（T6）+ 身份锚（T2 补 ②③的 pid 复用缝）全覆盖。
- **占位符扫描**：无 TBD/TODO；每步含完整代码或精确命令 + 期望。
- **类型一致**：`update_state_if_active`（T1 定义 → T3/T4/T5/T6 消费）签名一致；`is_worker_alive(pid, Option<u64>)`（T2 定义 → T4/T5 消费）一致；`reap_if_dead(&Path, JobState) -> JobState`、`mark_failed_panic(&Path,&str,&str)`、`write_partial_result`（去 `st` 参）在定义与调用点一致；`JobState.pid_start`（T2 加 → T2 spawn 写 / T4/T5 读）一致。
- **已知 ⚠️ 交控制者**：T5 Step 3 —— 既有 cancel 测试 fixture 若依赖旧无条件行为，需适配 pid/锚（测试改，非产品改）。
