# breakpoint 可靠性内核设计（第一批 P0）

> 状态：设计（待审）
> 日期：2026-06-30
> 依据：[`.doc/research/2026-06-30-live-review-operability-lessons.md`](../research/2026-06-30-live-review-operability-lessons.md)（19 轮真实 codex 对抗评审教训）
> 范围：**R1.1 + R1.3 + R2.1 + R1.2 + R2.2**（P0 可靠性内核五条，强耦合宜一次成型）
> 解冻：用户 2026-06-30 **整体授权可靠性改造解冻引擎层**（backends/direct/tool_loop/orchestrate/provider/state/cli）。
> 分支：**feat/reliability-core**（从 dev 新开）。
> 授权：方向性决策（解冻范围 / 第一批 P0 / R6 暂缓）已与用户确认；**其余实现细节按一线经验（research 文档）自决，可被实施中证据推翻**。

---

## 0. 目标与非目标

**目标**：让 `brk` 能**无人值守**跑评审循环——四条致命缺口归零：
1. **永不永久卡死**：单个挂死的 HTTP 连接 / cell 不能让 worker 永停 `Running`。
2. **崩溃不失忆**：已产出的 findings 中途落盘，崩溃/超时只丢「正在飞的那个 cell」，不丢全部。
3. **假死可检测可回收**：原生暴露活性信号（`last_active`），区分「慢」与「死」，崩溃作业转 `failed` 保留证据而非蒸发。
4. **崩溃/超时可 resume**：调 LLM **前**记录每 cell 输入快照（`manifest`，含渲染后 prompt 全文），重启时 `manifest − partial = 待补跑`，**只补跑缺失 cell**、合并已完成结果出完整报告——不让 cell 1–8 为 cell 9/10 的卡死陪葬重做。

**非目标（后续批次）**：错误分类 + turn 级重试（第二批 R4）、NDJSON 进度接线 + status 契约（第二批 R3，但本批先埋 `last_active`/`progress` 字段与刷新点）、id 改 quote 锚定（第三批 R8）、执行级验证（R6，carry）。

---

## 1. 五条改动的依赖关系

```
R1.1 (HTTP/turn 超时) ── 独立、最高 ROI、地基
        │
R1.3 (last_active 心跳) ── 低成本，喂给 watchdog + status
        │
R2.1 (manifest 输入快照 + partial 结果) ── 本批最大重构：扇出前写 manifest、每 cell 完成写 partial
        ├──────────────► R1.2 (job deadline + watchdog)   依赖 1.3 心跳 + 2.1
        ├──────────────► R2.2 (终态部分报告 + prune 不删)  依赖 2.1
        └──────────────► R2.3 (resume：manifest − partial 只补跑缺失 cell) 依赖 2.1
```

实施顺序建议：**R1.1 → R1.3 → R2.1 → R1.2 → R2.2 → R2.3**（每步带测试 + dogfood 验证）。R1.1 可独立先合入见效；R2.3 为第一批收尾、兑现「补跑」红利。

---

## 2. R1.1 — HTTP 超时 + per-turn deadline（地基）

**现状**：`backends.rs:105/240`、`direct.rs:31` 全用 `reqwest::Client::new()`，**无 connect/read 超时**；`tool_loop.rs` 的 `run_tool_loop` 每轮 `backend.turn(...).await?` **无整体 deadline**。`DEFAULT_MAX_ITERS=12` + 强制收尾只防**轮数**不防**时间**——单轮挂起即永久阻塞。

**设计**：
1. **client 级超时**：三处 `Client::new()` → `Client::builder().connect_timeout(CONNECT_TIMEOUT).timeout(REQUEST_TIMEOUT).build()`。建议默认 `CONNECT_TIMEOUT=10s`、`REQUEST_TIMEOUT=120s`。`.build()` 失败回落 `Client::new()`（沿用 probe.rs 既有写法）。
2. **per-turn deadline**：`run_tool_loop` 每轮把 `backend.turn(&msgs, &tools)` 包一层 `tokio::time::timeout(PER_TURN_DEADLINE, ...)`，超时→`ProviderError::Unavailable("turn timeout after Ns")`（第二批 R4 再细分 `Timeout` 变体）。`turn_final` 同样包裹。建议 `PER_TURN_DEADLINE` 略大于 `REQUEST_TIMEOUT`（如 150s，容一次重试余量）。
3. **常量位置**：先放 `tool_loop.rs`/`backends.rs` 常量；第二批做 `[review]` 配置项（`request_timeout_s`/`turn_deadline_s`）时再提到 config。

**为什么是地基**：没有它，重试、max-iters、watchdog、job-deadline 全可被一个 wedged 连接架空。**若只先合一条，合这条。**

**测试**：`FakeChatBackend` 增一个「永不返回」的 turn（`tokio::time::sleep(forever)`），断言 `run_tool_loop` 在 deadline 内返回 Err 而非挂起（`tokio::time::pause()` 控时，进程内确定性）。

---

## 3. state schema 变更（R1.3 + R2.1 + R2.2 共享地基）

三条 P0 共享一组 state 变更，**一次性加好**避免反复改 `state/job.rs`：

### 3.1 `JobState` 增字段（`state/job.rs`）
```rust
pub struct JobState {
    // ...现有：job_id/status/mode/started_at/progress/models/lenses/pid/exit_code/error/result_path
    pub last_active: u64,         // R1.3：每 cell 完成原子刷新（unix 秒）
    pub deadline_at: Option<u64>, // R1.2：started_at + wall_clock_budget；worker 到点自我中止
}
```
`#[serde(default)]` 保证旧状态文件可读（向后兼容）。

### 3.2 `JobStatus` 增终态（`state/job.rs`）
```rust
pub enum JobStatus { Pending, Running, Done, Failed, Cancelled, TimedOut }
```
> **开放决策 ①**：新增 `TimedOut` 变体 vs 复用 `Failed` + `error="deadline exceeded"`。推荐**新增 `TimedOut`**——上层 SKILL 可据此区分「超时（值得补跑缺失 cell）」vs「真失败（配置/协议错）」，语义更干净。

### 3.3 manifest + partial 两文件（R2.1/R2.3，`state/store.rs`）
job 目录新增两个 NDJSON，`cell` 字段作 join key：

**`<id>.manifest.ndjson`** —— 扇出**前**写，每 cell 一行**输入快照**（含渲染后 prompt 全文，self-contained，§9 决策⑥=A）。是「全集 M」与 resume 的重放源：
```json
{"cell":"bundle[src/a.rs]","model":"DeepSeek","prompt":"<完整渲染 prompt>"}
```

**`<id>.partial.ndjson`** —— 每 cell 完成 append 一行**结果**。是「已完成 N」：
```json
{"ts":1719700000,"cell":"bundle[src/a.rs]","model":"DeepSeek","ok":true,"findings":[...],"usage":[1200,80]}
```
失败 cell 也记一行（`ok:false`+`warn`）。新增 `write_manifest`/`read_manifest`、`append_partial`（原子 append，复用 `append_log` 模式）/`read_partial`。`manifest − partial 完成集 = 待补跑 cell`（R2.3）。

---

## 4. R2.1 — manifest 输入快照 + 增量 checkpoint（orchestrate 改造，本批核心）

**现状**：`run_code_fanout`/`run_doc_fanout`/`run_fanout`（`orchestrate/mod.rs`）都是 `futures::future::join_all` 收集所有 cell future 后**一次性聚合** `RawResults`。`run_job`（`cli.rs:356`）只在 `run_review_dispatch` 返回**完整** `Ok(report)` 后才 `write_result`。**进程死亡 = 全部 findings 丢失**，且无法得知「本该有多少 cell、缺哪些」。

**设计**：扇出从「join_all 后聚合」改为「**扇出前写 manifest（输入快照）+ 每 cell 完成即回调落盘（结果）**」，聚合逻辑不变（最终报告仍从内存 `RawResults` 出，两 NDJSON 仅崩溃恢复 / resume 用）。

0. **扇出前写 manifest**（各 `run_*_fanout` 入口）：枚举出全部 cell（bundle×model / doc×model / lens×model）后、并发执行**前**，把每 cell 的 `{cell_id, model, 渲染后 prompt}` 一次性写 `<id>.manifest.ndjson`。这是 self-contained 输入快照——resume 直接重放（不重收集证据，确定性，§9 决策⑥=A），且让部分报告的「N/M」有了 M。cell_id 用 `enrich` 同源的稳定标识（如 `bundle[路径列表]@model`）。

1. **cell sink 抽象**（`orchestrate/mod.rs`）：
   ```rust
   pub trait CellSink: Send + Sync {
       fn on_cell(&self, rec: CellRecord); // cell 完成（Ok/Err）即调，原子 append partial + 刷新 last_active/progress
   }
   ```
   `run_*_fanout` 增参数 `sink: Option<&dyn CellSink>`。每个 cell 的 async block 在 `run_one_*` 返回后、聚合前调 `sink.on_cell(...)`。
   > **开放决策 ②**：同步回调 `&dyn CellSink` vs `tokio::mpsc` channel。推荐**同步回调**——cell 完成是离散事件、写盘是快速 IO，回调最简单、无额外 task 生命周期管理；并发 cell 各自调 `on_cell`，sink 内部用 `&self` + 原子 append（文件 append 天然串行安全）。

2. **run_job 提供 sink**（`cli.rs`）：实现 `JobCellSink { ws, job_id }`，`on_cell` = `append_partial` + 刷新 `last_active`/`progress.done++` 并 `write_state`。`run_review`/`run_code_review` 直接调用（无 job 上下文）时传 `None`，行为不变。

3. **聚合不变**：`RawResults` 仍由 join_all 后聚合产出最终报告。partial.ndjson 是**崩溃时**才被 `run_job` 读取的恢复源（见 R2.2）。

**效果**：「丢全部」→「最多丢正在飞的那个 cell」。Phase-2 的 cell 本就隔离上下文，天然适配 streaming 落盘。

**测试**：`FakeProvider` 多 cell + 一个记录 `on_cell` 调用的测试 sink，断言每 cell 完成各落一条、`progress.done` 递增、`last_active` 刷新（进程内确定性）。

---

## 5. R1.2 — job wall-clock deadline + watchdog

**现状**：`prune`（`state/store.rs`）只回收 **pid 已死**的非终态作业（dogfood S5-a 加的）；**「pid 活着但挂死」的 worker 永不回收**。worker 无总时限。

**设计**：
1. **worker 自我中止**（`main.rs` worker 入口 + `cli.rs::run_job`）：worker 启动时 `deadline_at = started_at + JOB_DEADLINE`（实测单轮 11–34min → 默认建议 **40min**，第二批配 `[review].job_deadline`）。`run_job` 主体用：
   ```rust
   tokio::select! {
       r = run_review_dispatch(...) => { /* 正常终态 */ }
       _ = tokio::time::sleep_until(deadline) => {
           // 到点：读 partial.ndjson 组装部分报告，写终态 TimedOut（见 R2.2）
       }
   }
   ```
2. **心跳 watchdog**（`prune` 扩展）：非终态作业除「pid 死」外，**新增「`last_active` 停滞 > `WEDGE_THRESHOLD`」**判假死 → 转 `Failed`（保留证据，见 R2.2），即便 pid 仍活。
   > **开放决策 ③**：`WEDGE_THRESHOLD` 取值。建议 `max(2×turn_deadline, 5min)`——对齐 19 轮里手工用的「log mtime 冻结 >5min 判 HUNG」。watchdog 由谁触发：当前 `prune` 是机会式（别的命令偶发触发）。本批先复用机会式 prune；是否要独立常驻 watchdog 进程留作后续（无人值守循环里 `status` 轮询会自然触发 prune）。

**测试**：`tokio::time::pause()` + 构造 deadline 过期 → 断言 select 走超时分支、写 `TimedOut` + 部分报告；构造 `last_active` 陈旧 + pid 活 → 断言 prune 转 `Failed`。

---

## 6. R2.2 — 终态部分报告 + prune 不删崩溃作业

**现状**：`run_job` 的 `Err` 分支只写 `status=Failed`（`cli.rs:376`），**不写部分结果**；`prune` 对崩溃作业直接 `remove_job_files` **蒸发**（连失败原因都没了）。

**设计**：
1. **失败/超时/取消也出部分报告**（`cli.rs::run_job`）：进入 `Failed`/`TimedOut`/`Cancelled` 时，读 `<id>.partial.ndjson` 已 checkpoint 的 findings，组装 `result.json`：`verdict: incomplete` + 这些 findings + `run.warnings: ["partial: N/M cells done", "<failure reason>"]`，照常 `write_result`。**绝不静默丢**。
2. **verdict 增 `Incomplete`**（`domain/report.rs` + `verdict.rs`）：
   ```rust
   pub enum Verdict { Approve, Reject, Incomplete }
   ```
   > **开放决策 ④**：新增 `Incomplete` 变体 vs `reject` + 顶层 `complete: false` flag。推荐**新增变体**——退出码可映射独立值（现有 0=approve/10=reject/2=对照集不完整；`incomplete` 可复用 2 或新增），上层循环 SKILL 据此决定「用部分结果」还是「只补跑缺失 cell」。
3. **prune 保留崩溃证据**（`state/store.rs::prune`）：假死/孤儿作业 → 转 `Failed`（保留 result + error），**不再 `remove_job_files`**；只有**终态 + 超 TTL** 才真正删文件。即「回收」语义从「删文件」改为「先转终态留证、过期再删」。

**效果**：cell 9/10 卡死不再让 cell 1–8 陪葬；上层可「只补跑缺失 cell」——巨大效率红利。

**测试**：构造 dispatch 返回 Err + 已有 partial 记录 → 断言 result.json 出 `incomplete` + 部分 findings + warnings；构造孤儿作业 → 断言 prune 转 Failed 且文件仍在。

---

## 6.5 R2.3 — resume：只补跑缺失 cell（第一批收尾）

**动机**：R2.2 的「巨大效率红利」（cell 1–8 不为 cell 9/10 陪葬）只有靠 resume 才真正兑现——光出部分报告还缺 9/10 的覆盖。

**设计**：
1. **入口**：`brk review --resume <job_id>`（worker 重启亦走此路径）。读 `manifest`（全集 M）+ `partial`（已完成 N＝`ok:true` 的 cell_id 集）。
2. **差集补跑**：`待跑 = M − 已完成`。对待跑 cell **直接用 manifest 里的渲染 prompt 重放** `provider.run(LensRequest{model, prompt})`——不重收集证据、不重 build prompt（确定性，决策⑥=A）。复用 R2.1 的 sink，把补跑结果 append 进同一 `partial`。
3. **合并出报告**：补跑后 `read_partial` 取全部 `ok:true` 的 findings（已完成 + 新补）→ 走正常尾段（dedup→positioning→verify→verdict）→ 出**完整** report；全集补齐则 verdict 正常，仍有缺失则 `incomplete`。
4. **provider 重建**：resume 时 `build_registry` 照常按 cfg 重建（manifest 只存 prompt+model 名，不存 provider 实例）。

> **⚠️ 候补链 model 决策（T4 评审转入，实施前必答）**：非 replicate 候补链的 `CellInput.model`（T3 单值 schema）只是「最高 priority 候选」的**近似快照**——真正命中的候选要跑时才知道。而 resume 最常见的触发正是「高优先模型卡死/超时」的 R1.2 deadline；若严格按 manifest.model 重放，会在**同一个卡死模型**上再卡一次（相关性失败，非随机小概率）。二选一：**(a)** resume 对候补链类 cell **不信任 manifest.model、重走完整候补链**（略偏离「确定性重放」换正确性，推荐——replicate 显式 cell 才严格按 manifest.model 重放）；**(b)** 把 `CellInput.model` 升级为候选链 `Vec<String>` 快照。

**与 R1.2 配合**：worker 超时 `TimedOut` 后上层 SKILL 见 `incomplete` → 调 `--resume` 续跑；resume 自身亦受 deadline 约束，可多次 resume 直至全集补齐。

**测试**：`FakeProvider` + 预置 manifest（3 cell）+ partial（1 cell 完成）→ resume → 断言只对缺失 2 cell 调 provider、最终 report 含 3 cell findings。

---

## 7. 文件影响清单

| 文件 | 改动 | R |
|---|---|---|
| `src/provider/backends.rs` | 两处 client → builder + 超时 | R1.1 |
| `src/provider/direct.rs` | client → builder + 超时 | R1.1 |
| `src/provider/tool_loop.rs` | 每 turn `tokio::time::timeout` | R1.1 |
| `src/state/job.rs` | `last_active`/`deadline_at` 字段 + `TimedOut` 状态 | R1.3/1.2 |
| `src/state/store.rs` | `write_manifest`/`read_manifest`/`append_partial`/`read_partial`；prune 改「转终态留证」 | R2.1/2.2/2.3 |
| `src/orchestrate/mod.rs` | 扇出前写 manifest + `CellSink` + 三 fanout 加 sink 参数 + 每 cell 回调；resume 差集补跑 | R2.1/2.3 |
| `src/cli.rs` | `run_job`：sink 接线 + deadline `select!` + 终态部分报告；`--resume` 路径 | R2.1/1.2/2.2/2.3 |
| `src/main.rs` + `ReviewArgs` | worker 入口设 `deadline_at`；`--resume <job_id>` flag | R1.2/2.3 |
| `src/domain/report.rs` + `src/verdict.rs` | `Verdict::Incomplete` | R2.2 |

**冻结层触及**：backends/direct/tool_loop/orchestrate（已整体授权解冻）。

---

## 8. 测试策略（全确定性进程内可测）

- **R1.1**：`FakeChatBackend` 永不返回的 turn + `tokio::time::pause`/`timeout` → 断言返回 Err 不挂起。
- **R2.1**：扇出前 manifest 写全集；`FakeProvider` 多 cell + 记录型测试 sink → 断言每 cell 各落一条 partial、progress 递增、manifest 行数=cell 数。
- **R2.3**：预置 manifest（3 cell）+ partial（1 完成）→ resume → 断言只对缺失 2 cell 调 provider、最终 report 含 3 cell findings。
- **R1.2**：`tokio::time::pause` 触发 deadline → 断言走超时分支 + `TimedOut`；陈旧 `last_active` + 活 pid → 断言 prune 转 Failed。
- **R2.2**：Err + 已有 partial → 断言 `incomplete` 部分报告；孤儿 → 断言 prune 留文件转终态。
- **薄缝（reviewed-not-unit-tested）**：真实 HTTP 超时、真实 detached worker 到点自杀——真进程/真网络行为，靠 dogfood 验。

---

## 9. 已定决策（按一线经验自决，2026-06-30）

| # | 决策 | 已定 |
|---|---|---|
| ① | TimedOut 状态 | **新增 `TimedOut`**——语义区分「超时（值得补跑缺失 cell）」vs「真失败」，利于上层 SKILL 决策 |
| ② | cell sink | **同步回调 `&dyn CellSink`**——最简、无 task 生命周期；并发 cell 各自原子 append（文件 append 串行安全） |
| ③ | 假死阈值/watchdog | `WEDGE_THRESHOLD=3600s`（60min）——**T6 修正**：last_active 是 per-cell 心跳（单 cell 合法 11–34min），wedge 是 job deadline(40min) 的兜底、只 reap 自杀失败的真卡死进程，故须 > deadline；原 5min 源自 codex per-turn log 粒度，不适配本项目，一线推翻。本批复用机会式 `prune`（status 轮询触发），常驻 watchdog 留后续 |
| ④ | Incomplete | **新增 `Verdict::Incomplete`**，退出码复用 `2`（部分结果 ＝ 对照集不完整，与现有 exit-2 语义一致） |
| ⑤ | 默认值 | connect **10s** / request **120s** / per-turn **150s** / job **40min**（本批硬编码常量，第二批入 `[review]` config） |
| ⑥ | manifest 形式 | **A：渲染后 prompt 全文快照（self-contained）**——resume 用当时实际输入、确定性、不受 resume 时代码变化影响，附审计价值；体积换可靠（**用户 2026-06-30 确认**） |

> ①②③⑤ 为实现细节，按 research 经验自决；实施中若一线证据指向更优解，**直接推翻并更新本表**。方向性决策（解冻范围、第一批 P0、R6 暂缓）与 ④⑥（Incomplete/resume + manifest 形式）已与用户确认。

---

## 10. carry（暂缓，本批不做）

- **R6 执行级验证**（跑模型生成代码 / 项目测试）：违背「纯静态二进制」硬约束、需重沙箱设计、争议大 → 暂缓，待 P0/P1 稳后专题论证（用户 2026-06-30 决定）。
- **第二批**：R3（NDJSON 进度接线 + 统一 status 契约 + `--wait` 不说谎）、R4（错误分类 + turn 级重试 + 模型中途降级）。本批已埋 `last_active`/`progress`/partial 地基。
- **第三批**：R8.1（id 改 `hash(file+normalized_quote+normalized_title)`，行无关）、R8.2（`--dismissed` 结构化 + 升级刹车）。契合已落地的 quote 基建。
- **第四批**：R7.1（类级 prompt，便宜可早插）、R7.2（fuzz，配 R6）。
