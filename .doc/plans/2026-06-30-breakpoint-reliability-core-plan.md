# breakpoint 可靠性内核 — 第一批实施计划（R1.1/1.3/2.1/1.2/2.2/2.3）

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development。Steps 用 `- [ ]`。
> 设计依据：[`.doc/design/2026-06-30-breakpoint-reliability-core-design.md`](../design/2026-06-30-breakpoint-reliability-core-design.md)（含完整规格 + §9 已定决策）。
> 一线依据：[`.doc/research/2026-06-30-live-review-operability-lessons.md`](../research/2026-06-30-live-review-operability-lessons.md)。

**Goal:** 让 `brk` 无人值守跑评审循环——永不永久卡死、崩溃不失忆、假死可回收、超时可 resume 补跑缺失 cell。

**Architecture:** HTTP/turn 超时（地基）→ state 心跳/终态 schema → manifest 输入快照 + partial 结果落盘 → job deadline + watchdog → 终态部分报告 + prune 留证 → resume 差集补跑。

**Tech Stack:** tokio（time::timeout/select/pause）、futures（已有）、serde。**零新增 crate 依赖**。

## Global Constraints

- 分支 **feat/reliability-core**（从 dev）；引擎层**已整体解冻**（可改 backends/direct/tool_loop/orchestrate/provider/state/cli/main/domain）。
- **零新增 crate 依赖**；TLS 仍 rustls（不碰 features）。
- 默认值（本批硬编码常量，第二批入 config）：**connect 10s / request 120s / per-turn 150s / job 40min / wedge max(2×per-turn,5min)**。
- **manifest=A**：每 cell 记 `{cell_id, model, 渲染后 prompt 全文}`（self-contained，resume 重放不重收集证据）。
- 全改动**确定性进程内可测**（短 timeout / `tokio::time::pause` 控时 + Fake* 替身）；真挂起/真超时/真 detached 作薄缝（dogfood 验）。
- 每 task 末尾 `cargo test` 全绿 + `cargo clippy --all-targets` 干净 + commit。引擎逻辑改动后 dogfood 前需 `make build`。
- `Config::load`/secrets/密钥规则不动；`.gitignore` 不动。

---

## File Structure

```
src/provider/mod.rs          # T1: default_http_client() 共享 helper
src/provider/backends.rs     # T1: 两处 client 用 helper
src/provider/direct.rs       # T1: client 用 helper
src/provider/tool_loop.rs    # T1: run_tool_loop 加 per_turn deadline 参数 + timeout 包裹
src/state/job.rs             # T2: JobState{last_active,deadline_at} + JobStatus::TimedOut
src/domain/report.rs         # T2: Verdict::Incomplete
src/verdict.rs               # T2: compute_verdict 不产 Incomplete（仅终态路径产，见 T5）
src/state/store.rs           # T3: write/read_manifest + append/read_partial；T6: prune 留证
src/orchestrate/mod.rs       # T4: CellSink + 扇出前 manifest + 每 cell on_cell；T7: resume 差集
src/cli.rs                   # T5: run_job sink+deadline+部分报告；T7: --resume；ReviewArgs.resume
src/main.rs                  # T5: worker deadline_at；T7: --resume flag 透传
```

任务依赖：T1 独立 → T2 schema 地基 → T3 store IO → T4(依赖T3) → T5(依赖T2/3/4) → T6(依赖T2) → T7(依赖T3/4/5)。

---

### Task 1: R1.1 — HTTP 超时 + per-turn deadline（地基）

**Files:** Modify `src/provider/mod.rs`、`backends.rs`、`direct.rs`、`tool_loop.rs`。Test: `tool_loop.rs`。

**Interfaces:**
- Produces: `provider::default_http_client() -> reqwest::Client`（pub(crate)）；`run_tool_loop(..., per_turn: Duration)` 新增末参；`ToolLoopProvider` 持 `per_turn` 字段（默认 `PER_TURN_DEADLINE`）。

- [ ] **Step 1: 写失败测试**（`tool_loop.rs` tests 追加）

```rust
    struct HangBackend;
    #[async_trait]
    impl ChatBackend for HangBackend {
        async fn turn(&self, _m: &[ChatMessage], _t: &[ToolSpec]) -> Result<AssistantTurn, ProviderError> {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            unreachable!("should be cut by per-turn timeout")
        }
    }

    #[tokio::test]
    async fn per_turn_timeout_returns_err_not_hang() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        let r = run_tool_loop(&HangBackend, &ex, "sys", "review", 12,
                              std::time::Duration::from_millis(30)).await;
        assert!(matches!(r, Err(ProviderError::Unavailable(_))), "got: {r:?}");
    }
```

- [ ] **Step 2: 跑测试确认编译失败**（`run_tool_loop` 尚无 `per_turn` 参数）

Run: `cargo test --lib provider::tool_loop::tests::per_turn_timeout`
Expected: 编译错误（参数数量不符）

- [ ] **Step 3: 实现**

`provider/mod.rs` 加：
```rust
/// 带超时的共享 HTTP client（connect 10s / 整请求 120s）；构建失败回落默认 client。
pub(crate) fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}
```
`backends.rs` 两处 `client: reqwest::Client::new()` → `client: crate::provider::default_http_client()`；`direct.rs` 同。

`tool_loop.rs`：`run_tool_loop` 末尾加参 `per_turn: std::time::Duration`；循环内与 `turn_final` 处把 `backend.turn(...)`/`backend.turn_final(...)` 包：
```rust
let turn = match tokio::time::timeout(per_turn, backend.turn(&messages, &tools)).await {
    Ok(r) => r?,
    Err(_) => return Err(ProviderError::Unavailable(
        format!("turn timeout after {}s", per_turn.as_secs()))),
};
```
`ToolLoopProvider` 加字段 `per_turn: Duration`，`new` 设 `PER_TURN_DEADLINE`，`run` 传之；加 `const PER_TURN_DEADLINE: Duration = Duration::from_secs(150);`。

- [ ] **Step 4: 跑测试确认通过 + 全量**

Run: `cargo test` ; `cargo clippy --all-targets`
Expected: 全绿 + 干净（旧 `run_tool_loop` 调用点同步加 `per_turn` 实参）

- [ ] **Step 5: Commit** `feat(reliability): R1.1 HTTP client timeout + per-turn deadline`

---

### Task 2: R1.3/1.2/2.2 — state schema 地基（字段 + 终态 + verdict）

**Files:** Modify `src/state/job.rs`、`src/domain/report.rs`。Test: 同文件。

**Interfaces:**
- Produces: `JobState{ last_active: u64, deadline_at: Option<u64> }`（`#[serde(default)]`）；`JobStatus::TimedOut`；`Verdict::Incomplete`（serde `"incomplete"`）。

- [ ] **Step 1: 写失败测试**（`state/job.rs` tests 追加 + `domain/report.rs` tests 追加）

`state/job.rs`：
```rust
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
```
`domain/report.rs`：
```rust
    #[test]
    fn verdict_incomplete_serializes() {
        assert_eq!(serde_json::to_string(&Verdict::Incomplete).unwrap(), "\"incomplete\"");
    }
```

- [ ] **Step 2: 跑确认失败**（字段/变体不存在）

Run: `cargo test --lib state::job::tests::jobstate_new_fields domain::report`
Expected: 编译失败

- [ ] **Step 3: 实现**
- `state/job.rs`：`JobStatus` enum 加 `TimedOut`（serde kebab `"timed-out"`？——与现有 lowercase 一致用 `#[serde(rename_all="lowercase")]` 既有规则，`TimedOut`→`"timedout"`；**为清晰显式** `#[serde(rename="timed_out")]`）。`JobState` 加 `#[serde(default)] pub last_active: u64,` 与 `#[serde(default)] pub deadline_at: Option<u64>,`；`JobState::new` 初始化 `last_active: started_at, deadline_at: None`。
- `domain/report.rs`：`Verdict` 加 `Incomplete`，`as_str`/serde 映射 `"incomplete"`。

> **决策点（实现者按设计⑤/④）**：`TimedOut` serde 串名取 `"timed_out"`；`Incomplete` 取 `"incomplete"`。

- [ ] **Step 4: 跑确认通过 + 全量 + clippy**
- [ ] **Step 5: Commit** `feat(reliability): R1.3/1.2/2.2 state schema — last_active/deadline_at + TimedOut + Verdict::Incomplete`

---

### Task 3: R2.1/2.3 — store manifest + partial 读写

**Files:** Modify `src/state/store.rs`。Test: 同文件。

**Interfaces:**
- Produces: `CellInput{cell:String, model:String, prompt:String}`、`CellRecord{cell:String, model:String, ok:bool, findings:Vec<Finding>, ... }`（serde）；`write_manifest(ws,id,&[CellInput])`、`read_manifest(ws,id)->Vec<CellInput>`、`append_partial(ws,id,&CellRecord)`、`read_partial(ws,id)->Vec<CellRecord>`。文件 `<id>.manifest.ndjson` / `<id>.partial.ndjson`（路径 helper 仿 `job_result`）。

- [ ] **Step 1: 写失败测试**

```rust
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
```
（`CellRecord::ok(cell,model,findings)` 为便捷构造）

- [ ] **Step 2: 跑确认失败** → **Step 3: 实现**（NDJSON 原子 append 仿 `append_log`；read 逐行 `serde_json::from_str` 跳过坏行）→ **Step 4: 测试通过 + clippy** → **Step 5: Commit** `feat(reliability): R2.1 store manifest + partial NDJSON`

---

### Task 4: R2.1 — orchestrate CellSink + 扇出前 manifest + 每 cell 回调

**Files:** Modify `src/orchestrate/mod.rs`。Test: 同文件。

**Interfaces:**
- Consumes: T3 `CellInput`/`CellRecord`。
- Produces: `pub trait CellSink: Send + Sync { fn on_cell(&self, rec: CellRecord); }`；三个 `run_*_fanout` 末加 `sink: Option<&dyn CellSink>` 参数；扇出前枚举 cell 写 manifest（经 sink 的 `manifest(&[CellInput])` 或独立回调——见下）。`cell_id` 规则：code=`bundle[路径列表]`、doc=`doc`、lens=`lens:<id>`，配 `@model`。

> **设计要点**：`CellSink` 扩为两法 `fn manifest(&self, inputs: &[CellInput])`（扇出前一次）+ `fn on_cell(&self, rec: CellRecord)`（每 cell 完成）。`run_*_fanout` 在构造 futures 前先收集 `Vec<CellInput>`（cell_id+model+prompt）调 `sink.manifest(...)`；每个 cell 的 async 块在 `run_one_*` 返回后调 `sink.on_cell(...)`。`sink=None` 时全跳过（`run_review`/`run_code_review` 直调路径行为不变）。

- [ ] **Step 1: 写失败测试**（记录型 sink）

```rust
    struct RecSink { manifest_n: std::sync::Mutex<usize>, cells: std::sync::Mutex<Vec<String>> }
    impl CellSink for RecSink {
        fn manifest(&self, inputs: &[CellInput]) { *self.manifest_n.lock().unwrap() = inputs.len(); }
        fn on_cell(&self, rec: CellRecord) { self.cells.lock().unwrap().push(rec.cell); }
    }

    #[tokio::test]
    async fn fanout_writes_manifest_and_each_cell() {
        let p = FakeProvider::with("m", vec![raw_q("found")]);
        let opts = OrchestrateOpts { models: vec![PlannedModel{name:"m".into(),priority:10}],
            replicate:true, no_fallback:false, max_concurrency:4, retries:0 };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs"), bundle("src/c.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let sink = RecSink { manifest_n: 0.into(), cells: Vec::new().into() };
        let _ = run_code_fanout(&p, "wt", &bundles, &rules, "", &opts, Some(&sink)).await;
        assert_eq!(*sink.manifest_n.lock().unwrap(), 3, "manifest 全集=3 cell");
        assert_eq!(sink.cells.lock().unwrap().len(), 3, "每 cell 各回调一次");
    }
```

- [ ] **Step 2–5**：跑失败 → 实现（三 fanout 加 sink 参数 + 调用点全部传 `None` 除测试；cli.rs 调用点暂传 `None`，T5 替换）→ 测试通过（既有 fanout 测试同步加 `None` 实参）+ clippy → Commit `feat(reliability): R2.1 orchestrate CellSink + manifest + per-cell checkpoint`

---

### Task 5: R1.3/1.2/2.2 — run_job 接线（sink + deadline + 部分报告）

**Files:** Modify `src/cli.rs`、`src/main.rs`。Test: `cli.rs`。

**Interfaces:**
- Consumes: T2 schema、T3 store、T4 CellSink。
- Produces: `JobCellSink{ws,job_id}`（impl `CellSink`：`manifest`→`write_manifest`+刷 `progress.total`；`on_cell`→`append_partial`+刷 `last_active`/`progress.done`+`write_state`）；`run_job` 用 `tokio::select!` 套 deadline；`Failed`/`TimedOut`/`Cancelled` 经 `read_partial` 组装 `verdict:incomplete` 部分报告写 `result.json`。`main.rs` worker 起手设 `deadline_at`。

> **设计要点（实现按设计 §5/§6）**：`run_job` 把 `run_review_dispatch` 包进 `tokio::select!{ r = dispatch => ..., _ = sleep_until(deadline) => 超时分支 }`；超时/失败分支调 `partial_report(ws,job_id,reason)`（读 partial→组 incomplete report→`write_result`+`write_result_md`）。`run_review_dispatch`/`run_review`/`run_code_review` 需把 `JobCellSink` 透传到 `run_*_fanout`（新增可选 sink 形参，一路传下）。**确定性测试**用注入式短 deadline + FakeProvider。

- [ ] **Step 1: 写失败测试**（`cli.rs` tests）

```rust
    #[tokio::test]
    async fn run_job_failure_writes_incomplete_partial_report() {
        // dispatch 失败（无可用 provider/脚本），但已有 partial 记录 → result.json = incomplete + 已有 findings
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // 预置一条 partial（模拟崩溃前已完成 1 cell）
        crate::state::store::append_partial(ws, "j", 
            &crate::state::store::CellRecord::ok("bundle[a]", "m", vec![/* 1 raw finding 略 */])).unwrap();
        // 构造一个必失败的 dispatch（provider 空），run_job 应写 incomplete 而非静默
        // ... 断言 read_result 含 "incomplete" 且含已 checkpoint 的 finding
    }
```
> 实现者据 `run_job` 实际签名补全 fixture（参照既有 `run_job_done_writes_result_and_state` 测试）；核心断言：失败/超时路径 `result.json` 的 `verdict=="incomplete"` 且并入 `read_partial` 的 findings + `warnings` 含 `"partial: N/M"`。

- [ ] **Step 2–5**：跑失败 → 实现（JobCellSink + select deadline + partial_report + worker deadline_at + sink 透传链）→ 测试通过 + clippy → Commit `feat(reliability): R1.2/1.3/2.2 run_job deadline + checkpoint sink + incomplete partial report`

---

### Task 6: R2.2/1.2 — prune 改造（转终态留证 + 假死回收）

**Files:** Modify `src/state/store.rs`。Test: 同文件。

**Interfaces:**
- Consumes: T2 `last_active`/`TimedOut`。
- Produces: `prune` 新语义——非终态作业：pid 死 **或** `last_active` 停滞 > `WEDGE_THRESHOLD` → **转 `Failed`（写 error，保留文件）**，不再 `remove_job_files`；仅**终态 + 超 TTL** 才真删。`const WEDGE: u64`。

- [ ] **Step 1: 写失败测试**

```rust
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
```

- [ ] **Step 2–5**：跑失败 → 实现（prune 非终态分支加 wedge 判定转 Failed；终态删保持）→ 测试通过（既有 prune 测试可能需调：原「pid 死即删」改「pid 死转 Failed」——同步更新）+ clippy → Commit `feat(reliability): R2.2 prune transitions wedged/dead jobs to Failed, preserves evidence`

---

### Task 7: R2.3 — resume 差集补跑

**Files:** Modify `src/cli.rs`、`src/main.rs`、`ReviewArgs`。Test: `cli.rs`。

**Interfaces:**
- Consumes: T3 read_manifest/read_partial、T4 CellSink、T5 JobCellSink。
- Produces: `ReviewArgs.resume: Option<String>`；`run_resume(ws, job_id, provider, cfg, cwd)`：读 manifest（全集）+ partial（已完成 ok），对差集 cell **直接用 manifest.prompt 重放** `provider.run(LensRequest{model,prompt})`、`append_partial`；再 `read_partial` 全量 → 尾段（dedup→anchor→verify→verdict）→ 完整 report。`main.rs` `--resume <id>` flag 路由到此。

- [ ] **Step 1: 写失败测试**

```rust
    #[tokio::test]
    async fn resume_only_runs_missing_cells() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // manifest 3 cell（同 model "m"），partial 已完成 1
        let inputs = vec![
            CellInput{cell:"c1".into(),model:"m".into(),prompt:"P1".into()},
            CellInput{cell:"c2".into(),model:"m".into(),prompt:"P2".into()},
            CellInput{cell:"c3".into(),model:"m".into(),prompt:"P3".into()},
        ];
        crate::state::store::write_manifest(ws,"j",&inputs).unwrap();
        crate::state::store::append_partial(ws,"j",
            &crate::state::store::CellRecord::ok("c1","m",vec![/*1 finding*/])).unwrap();
        // 计数型 provider：记录被调用的 prompt
        // resume → 断言只对 c2/c3 调 provider（2 次）、最终 read_partial 有 3 cell
    }
```
> 实现者用一个记录调用 prompt 的 FakeProvider 变体断言「只补跑 2 个缺失 cell」。

- [ ] **Step 2–5**：跑失败 → 实现（`run_resume` + `--resume` 路由 + ReviewArgs 字段）→ 测试通过 + clippy → Commit `feat(reliability): R2.3 resume — replay missing cells from manifest`

---

## 验证（全 task 完成后）

- [ ] `cargo test` 全绿（新增可靠性测试 + 既有全过）；`cargo clippy --all-targets` 干净；openssl-sys 缺席。
- [ ] `make build` 重出二进制 → dogfood：① 正常 review 仍 OK（sink=None 路径无回归）；② `--background` 跑作业，`status` 看 `last_active`/`progress` 推进；③ 人为造超时/中断 → `result` 出 `incomplete` + 部分 findings；④ `--resume <id>` 补齐缺失 cell 出完整报告。
- [ ] opus 全分支评审（MERGE_BASE=dev e48e359 .. HEAD）。

## Self-Review

- **Spec 覆盖**：设计六条 R1.1/1.3/2.1(含 manifest)/1.2/2.2/2.3(resume) 各对应 T1–T7（T2 合并三处 schema 地基，T5 合并 run_job 三关注点）✓。
- **Placeholder**：T5/T7 的 fixture 因依赖真实 `run_job`/`run_resume` 签名，给了断言契约 + 参照测试，由实现者补全——非空占位，是「按实际签名补 fixture」的明确指令 ✓。
- **类型一致**：`CellInput`/`CellRecord`（T3）贯穿 T4/T5/T7；`CellSink`（T4）→ `JobCellSink`（T5）；`per_turn`（T1）；`last_active`/`deadline_at`/`TimedOut`/`Incomplete`（T2）全程一致 ✓。
- **零新依赖**：tokio time/select、futures、serde 均已有 ✓。
- **冻结层**：全在已授权解冻范围 ✓。
