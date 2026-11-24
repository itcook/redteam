# Breakpoint status 契约（P1-D）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `brk status` 成为上层 supervisor 可靠依赖的机读契约——单 job 与列表同 schema、可跨 workspace 查询/筛选、能区分「尾段慢 vs 真卡死」，并删掉说谎的空操作 `--wait`。

**Architecture:** 三层：(1) JobState schema（`state/job.rs`）加 `phase`/`workspace` 字段 + `deadline_at` 一致性；(2) CLI 契约（`main.rs`）status 列表改 JSON 数组 + `--workspaces [substr]` 跨桶查询 + 删 `--wait`；(3) phase 埋点（`cli.rs`/`orchestrate.rs`）worker 在阶段转换处刷新。

**Tech Stack:** Rust 2021、serde/serde_json、clap（derive）。

**Spec:** `.doc/design/2026-07-01-breakpoint-status-contract-design.md`（§7 决策为准）。

## Global Constraints

- **零新增依赖**；**TLS rustls，openssl-sys 不得出现**（本 D 不碰 provider/TLS，但全批次约束仍在）。
- **所有测试 hermetic**：纯序列化 + tempdir 状态读写，不打网络。
- **向后兼容**：新字段 `phase`/`workspace` 均 `Option` + `skip_serializing_if` + `#[serde(default)]`，旧 job 文件读得回（缺键→None）。
- **PONYTAIL**：最短正确 diff；phase 埋点全 sink-gated/worker-scoped，前台（sink=None）零影响；不加未要求抽象。
- `state/store.rs`、`state/job.rs` 属状态层（非冻结引擎层），可改。

---

## File Structure

| 文件 | 职责（本计划内） |
|---|---|
| `src/state/job.rs` | `Phase` enum；JobState 增 `phase`/`workspace`；`deadline_at` 加 `skip_serializing_if`；`list_all_states`/`select_jobs` 跨桶聚合+筛选 helper |
| `src/orchestrate/mod.rs` | `CellSink` trait 增 `fn on_phase(&self, _: Phase) {}`（默认 no-op） |
| `src/cli.rs` | `JobCellSink` 写 phase（manifest→Fanout、on_phase→Finalizing）；`run_job` 起手 Collecting；dispatch 尾段前调 `on_phase(Finalizing)` |
| `src/main.rs` | `Cmd::Status` 加 `--workspaces` + 列表改 JSON 数组、删 `render_job_list`；后台建 job 写 `workspace`；`Cmd::Result` doc 补终态；`Cmd::Review` 删 `wait` |

---

## Task 1: JobState schema —— phase/workspace 字段 + deadline_at 一致性 + 跨桶聚合 helper

**Files:**
- Modify: `src/state/job.rs`

**Interfaces:**
- Produces:
  - `pub enum Phase { Collecting, Fanout, Finalizing }`（serde lowercase）
  - JobState 新字段 `pub phase: Option<Phase>`、`pub workspace: Option<String>`
  - `pub fn list_all_states(state_root: &Path) -> Vec<JobState>`
  - `pub fn select_jobs(cwd_ws: &Path, state_root: &Path, workspaces: Option<&str>) -> Vec<JobState>`

- [ ] **Step 1: 写失败测试**

`src/state/job.rs` tests 追加：

```rust
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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoint --lib state::job 2>&1 | tail -20`
Expected: 编译失败——`Phase`/`phase`/`workspace`/`select_jobs`/`list_all_states` 未定义。

- [ ] **Step 3: 实现枚举 + 字段 + helper**

`src/state/job.rs`：
1. 加 `Phase` enum（放在 `JobStatus` 附近）：
```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Collecting,  // worker 起手，收集证据，fanout 前
    Fanout,      // cell 并发执行中（progress.done 推进）
    Finalizing,  // 尾段：dedup/correlate/verify/verdict（progress/last_active 静止）
}
```
2. JobState 加两字段（放在 `deadline_at` 后）：
```rust
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub phase: Option<Phase>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub workspace: Option<String>,
```
3. `deadline_at` 字段属性从 `#[serde(default)]` 改为：
```rust
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub deadline_at: Option<u64>,
```
4. `JobState::new` 尾部加 `phase: None, workspace: None,`。
5. 加聚合 + 选取 helper（`Path` 已在 `use std::path::Path`）：
```rust
/// 遍历 state_root 下所有 workspace 桶，聚合全部 job 状态。
pub fn list_all_states(state_root: &Path) -> Vec<JobState> {
    let mut out = Vec::new();
    let Ok(buckets) = std::fs::read_dir(state_root) else { return out; };
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
```

> 既有测试 `state_round_trips_through_disk`/`jobstate_new_fields_default_and_roundtrip` 里手工构造的
> `JobState { ... }` 字面量若不含新字段会编译错——给它们补 `phase: None, workspace: None,`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p breakpoint --lib state::job 2>&1 | tail -20`
Expected: 全绿（新 7 个 + 既有）。

- [ ] **Step 5: clippy + Commit**

Run: `cargo clippy -p breakpoint --lib 2>&1 | tail -5`
Expected: 无 warning。
```bash
git add src/state/job.rs
git commit -m "feat(status): JobState phase/workspace fields + deadline_at consistency + cross-bucket select

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: CLI 契约 —— status JSON 数组 + `--workspaces` + 删 render_job_list + workspace 落盘 + 删 `--wait`

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `select_jobs`（Task 1）、`state_root()`（`state::store`）、`workspace_bucket`（`state::store`）。

- [ ] **Step 1: `Cmd::Status` 加 `--workspaces` 参数**

`src/main.rs` `enum Cmd` 的 `Status` 变体（现 `Status { job_id: Option<String> }`）改为：
```rust
    /// 查询作业状态：无 id 列当前 workspace 全部；有 id 打印该作业 JSON。
    /// `--workspaces` 聚合所有 workspace，`--workspaces <substr>` 按路径子串筛。
    Status {
        job_id: Option<String>,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        workspaces: Option<String>,
    },
```

- [ ] **Step 2: `Cmd::Status` 分支改用 select_jobs + JSON 数组**

`src/main.rs` `Cmd::Status { job_id }` 分支（现 main.rs:385-400）——解构改为 `Cmd::Status { job_id, workspaces }`，body 改为：
```rust
        Cmd::Status { job_id, workspaces } => {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let ws = breakpoint::state::store::workspace_bucket(&cwd);
            prune_here(&ws); // 机会式 prune（仅当前 workspace，见决策⑥）
            match job_id {
                None => {
                    let jobs = breakpoint::state::job::select_jobs(
                        &ws,
                        &breakpoint::state::store::state_root(),
                        workspaces.as_deref(),
                    );
                    println!("{}", serde_json::to_string_pretty(&jobs).unwrap_or_else(|_| "[]".into()));
                    0
                }
                Some(id) => match breakpoint::state::job::read_state(&ws, &id) {
                    Some(st) => { println!("{}", serde_json::to_string_pretty(&st).unwrap_or_default()); 0 }
                    None => { eprintln!("no such job: {id}"); 2 }
                },
            }
        }
```

- [ ] **Step 3: 删 `render_job_list` + 其测试**

`src/main.rs`：删 `render_job_list` 函数（现 152-158）。若 tests mod 里有 `render_job_list` 的测试，一并删。grep 确认无其他调用：`grep -n render_job_list src/main.rs` → 应为空。

- [ ] **Step 4: 后台建 job 时写 workspace**

`src/main.rs` 后台分支（现 main.rs:302 附近，`let mut st = JobState::new(...)` 之后、首次 `write_state` 之前）插入：
```rust
                st.workspace = Some(
                    std::fs::canonicalize(&cwd)
                        .unwrap_or_else(|_| cwd.clone())
                        .to_string_lossy()
                        .into_owned(),
                );
```

- [ ] **Step 5: `Cmd::Result` doc 补终态规则 + 删 `--wait`**

1. `Cmd::Result` 的 doc-comment（现「取回已完成作业的评审结果（读后标记已消费）。」）改为：
```rust
    /// 取回已完成作业的评审结果（读后标记已消费）。仅作业处于终态 `done` 时可取；未终态返回退出码 2。
```
2. `Cmd::Review` 删 `--wait`：删这两行
```rust
        #[arg(long, default_value_t = true)]
        wait: bool,
```
3. `Cmd::Review` 解构处（现 main.rs:244）删 `wait: _wait,` 那一行。
4. `src/cli.rs:28` 提及 `--wait` 的注释清理（现「`--wait` 是默认前台同步路径（与 `--background` 对称）。」→ 删该行或改为不提 --wait 的等价说明）。

- [ ] **Step 6: 编译 + 手验 + 全套测试**

Run: `cargo build -p breakpoint 2>&1 | tail -5`
Expected: 编译通过（`wait` 已删、`workspaces` 已接）。
Run: `./target/debug/brk review --wait 2>&1 | head -3 || true`（若 debug 产物路径不同，用 `cargo run -p breakpoint -- review --wait`）
Expected: clap 报未知参数 `--wait`（证明已删、不再静默空操作）。
Run: `cargo test -p breakpoint 2>&1 | grep "test result" | tail -6`
Expected: 全绿。
Run: `cargo clippy -p breakpoint --all-targets 2>&1 | tail -5`
Expected: 无 warning。

- [ ] **Step 7: Commit**

```bash
git add src/main.rs src/cli.rs
git commit -m "feat(status): JSON-array list + --workspaces cross-bucket query; drop no-op --wait

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: phase 埋点 —— CellSink on_phase + JobCellSink 写 phase + run_job/dispatch 转换点

**Files:**
- Modify: `src/orchestrate/mod.rs`（`CellSink` trait）
- Modify: `src/cli.rs`（`JobCellSink`、`run_job`、dispatch 尾段）

**Interfaces:**
- Consumes: `Phase`（Task 1）。
- Produces: `CellSink::on_phase(&self, phase: Phase)`（默认 no-op）。

- [ ] **Step 1: 写失败测试**

`src/cli.rs` tests 追加（JobCellSink 单元级——manifest 写 Fanout、on_phase 写 Finalizing）：

```rust
    #[tokio::test]
    async fn jobcellsink_manifest_sets_phase_fanout() {
        use crate::state::job::{JobState, Phase, write_state, read_state};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        write_state(&ws, &JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { ws: ws.clone(), job_id: "j".into(), lock: std::sync::Mutex::new(()) };
        use crate::orchestrate::CellSink;
        sink.manifest(&[crate::state::store::CellInput {
            cell: "c".into(), model: "m".into(), prompt: "p".into() }]);
        assert_eq!(read_state(&ws, "j").unwrap().phase, Some(Phase::Fanout));
    }

    #[tokio::test]
    async fn jobcellsink_on_phase_writes_phase() {
        use crate::state::job::{JobState, Phase, write_state, read_state};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        write_state(&ws, &JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { ws: ws.clone(), job_id: "j".into(), lock: std::sync::Mutex::new(()) };
        use crate::orchestrate::CellSink;
        sink.on_phase(Phase::Finalizing);
        assert_eq!(read_state(&ws, "j").unwrap().phase, Some(Phase::Finalizing));
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoint --lib cli::tests::jobcellsink 2>&1 | tail -15`
Expected: 编译失败——`CellSink` 无 `on_phase`、`JobCellSink::manifest` 未写 phase。

- [ ] **Step 3: `CellSink` trait 增默认 no-op `on_phase`**

`src/orchestrate/mod.rs` `CellSink` trait 增：
```rust
    /// 阶段转换钩子（后台 job 用；默认 no-op，前台 sink=None / 其他 sink 零影响）。
    fn on_phase(&self, _phase: crate::state::job::Phase) {}
```
（`Phase` 若未在 orchestrate 作用域，用全路径 `crate::state::job::Phase` 即可，无需 `use`。）

- [ ] **Step 4: `JobCellSink` 写 phase**

`src/cli.rs` `JobCellSink::manifest`（现 cli.rs:390）里，写 `progress.total` 的同一 `if let Some(mut st)` 块内加 `st.phase = Some(crate::state::job::Phase::Fanout);`。
`impl CellSink for JobCellSink` 增 `on_phase`：
```rust
    fn on_phase(&self, phase: crate::state::job::Phase) {
        let _g = self.lock.lock().unwrap();
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.phase = Some(phase);
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }
```

- [ ] **Step 5: run_job 起手 Collecting + dispatch 尾段前 Finalizing**

1. `src/cli.rs` `run_job`（现 cli.rs:612）起手、调 dispatch 前，写 Collecting——在读回 state 设置的位置加：
```rust
    if let Some(mut st) = crate::state::job::read_state(ws, job_id) {
        st.phase = Some(crate::state::job::Phase::Collecting);
        let _ = crate::state::job::write_state(ws, &st);
    }
```
（若 run_job 内已有读-改-写 state 的段落，合并进去，避免多次读写。）

2. dispatch 尾段前调 `on_phase(Finalizing)`：`run_review`/`run_code_review`/`run_resume` 三处在
`run_*_fanout(...)` 返回后、`assemble_report(...)` 调用前，各加一行（仅当有 sink 时）：
```rust
    if let Some(s) = sink { s.on_phase(crate::state::job::Phase::Finalizing); }
```
（`run_resume` 的 sink 变量名以现网为准；若某 dispatch 函数当前不持有 `sink` 变量，则该函数尾段
不设 Finalizing——phase 仅后台 job 有意义，前台无 sink 本就不写。plan 实施者按现网 sink 可见性
决定插入点，保证「有 JobCellSink 的后台路径」尾段前置 Finalizing 即可。）

- [ ] **Step 6: 跑测试确认通过 + clippy**

Run: `cargo test -p breakpoint --lib 2>&1 | grep "test result" | tail -6`
Expected: 全绿（含新 2 个 jobcellsink 测试）。
Run: `cargo clippy -p breakpoint --all-targets 2>&1 | tail -5`
Expected: 无 warning。

- [ ] **Step 7: Commit**

```bash
git add src/orchestrate/mod.rs src/cli.rs
git commit -m "feat(status): phase field wiring — Collecting/Fanout/Finalizing via CellSink

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## 消费者核查（Task 2 内或 Task 3 后一并做）

`grep -rn "brk status\|status --" plugins/breakpoint codex* 2>/dev/null` 定位任何解析 `status` 全列表
**文本**的消费者；若有（旧格式 `{id} {status} {mode} {done}/{total}` 文本行解析），改为解析 JSON 数组。
仅调 `status <id>` 或原样透传的无需改。无消费者则跳过（在报告里注明 grep 结果为空）。

---

## Self-Review 记录（写计划者自查）

- **Spec 覆盖**：R3.2 JSON 数组=T2；§3.1 deadline_at=T1；§3.2 workspace 字段=T1、`--workspaces`/select_jobs=T1(helper)+T2(接线)、workspace 落盘=T2；phase=T1(字段)+T3(埋点)；R3.3 删 --wait=T2；result 终态 doc=T2；消费者核查=独立段。全覆盖。
- **类型一致**：`Phase`/`select_jobs`/`list_all_states`/`on_phase`/`workspace`/`phase` 跨任务同名一致。
- **占位符**：无 TBD。T3 Step 5 的 Finalizing 插入点因 dispatch 的 sink 可见性以现网为准，给了明确判据（有 JobCellSink 的后台路径尾段前置即可），非占位。
- **风险点**：JobCellSink 字段（ws/job_id/lock）须 `pub(crate)` 或测试同模块可见——现 struct 在 cli.rs、测试也在 cli.rs::tests，同模块可直接构造（Step 1 测试已假设可见）；若字段私有且测试在子模块，`super::JobCellSink { .. }` 可访问私有字段（同文件），无需改可见性。
