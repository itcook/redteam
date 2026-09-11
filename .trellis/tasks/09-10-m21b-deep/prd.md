# M21b 批C：深挖队列与 reviewer 运行时——入口五态/优先级预算/门驱动派发/barrier/Phase Deep+resume/观测

## Goal

`redteam-next review` 端到端产 finding：风险 H0 与 scanner 疑点进同一条优先级队列，reviewer 按 provider 门空位消费（预算随 R、必挖不截），finding 落地即流式持久化并经 `FindingEvent` 交给批 D；barrier 后 reduce 出报告。finding 只由深挖产出。

## Background（已确认）

- 前提：批 A `eaf8bb5`、批 B `27bf391` 已合回 `feat/m21-review-engine`。零件：`risk::route` / `Hotspot` / `budget_cells`；`QueueItemSpec` / `RegionPack::build` / `build_review_prompt` / `parse_review_output_with_stats` / `IncrementalFindings` / `on_text_delta` take 语义。
- `stage0_risk`（`cli/mod.rs:247`）目前丢掉 H0 区列表，只写 `RiskMeta` 计数——C 必须把 `RiskRoute` 留在 `run_code_review` 里入队，不从报告反推。
- `queue_items` 表壳 v4 已建（`db.rs:81`），本批开始写。`Phase` 尚无 `Deep`/`Verify`；旧 `Refute`/`Fanout` 只读。
- 合并谓词已有：`consolidate::scan::same_suspicion`（`scan.rs:22`）。`ScanCall` 观测口径已有。
- 429 棘轮（2026-09-10 裁）：沿用 D13 只降不升 + 次数上限 + RA 夹 30s；①②③ 等真速率型 429 再裁。C-3 Fake 429 测现语义。
- 类别权重：上位 design §6 写了因子、无数字表；L0-L5 自标归 R4/M21c。**本批取舍**中性 1.0（不是既有 spec），排序靠 confidence × agreement 与 H0 分层。

## Requirements

- 入口五态〔D13 Q-A〕：无命中 / 排队中并入 / 在途只改记录 / 本轮已结案沿用 / 跨轮判例守卫空过（按无命中）。谓词复用 `same_suspicion`；同家族不加 agreement。
- 优先级：主区 H0 → 涟漪 → 疑点 `confidence × agreement × 类别权重(1.0)` → needs 中档；非 FIFO、confidence 不做门〔D11-5〕。
- 预算：`RiskRoute.budget_cells_hint`；必挖 = 主区 ∪ {agreement ≥ 2} 不截（`maxCells = 0` 时主区仍挖）；疑点保底 ⌈n/4⌉；drain 截断项 `skipped(budget)` 进 `DeepMeta` 不静默。
- 派发：单派发器 × `registry.lease()`；t=0 scanner 先于 H0；scan 项不等同组兄弟 cell；记派发时/落地时 agreement。
- `run_review_shot`：per-cell backend + 流式落盘。终态 parse 权威；旁路 `take` 先落盘/发射，终判按 `finding.id` 去重补发；必调 `IncrementalFindings::finalize()`（正常/REPAIR/超时/取消）；`discarded` → `ReviewParseStats.stream_discarded` + `DeepMeta.stream_discarded`。`FindingEvent` 经 `FindingSink`（缺省 `()` no-op，C 不建 mpsc）。C 不改 `verify.rs`。
- barrier：scanner 全完 ∧ Pending 空 ∧ InFlight 0 ∧ 复核在途 0（C 恒真）→ dedup → findings 报告。无 reviewer 条目 → scan-only 退化。
- 退出码（用户裁，路径表见 design §6）：reviewer 失败 → Approve+2+coverage failed；预算截断 → Approve+2+`budget_skipped`；scanner 失败仍 Incomplete/3；verifier 失败不由 C 定义。
- resume：`Phase::Deep`/`Verify`（旧 `Refute` 可读）；`queue_item_put` / `queue_items_for_job`；零行=旧作业 scan-only；Settled 以 cells 为准、快照写失败只 warning 不败 job。
- 观测：`RunMeta.deep: DeepMeta` 字段见 design §9（不重复 inflight/rate_limit/queue_wait）。
- 顺手：thinking×tools 守卫；`--tools` 二阶段 + 触发率；`AttemptPolicy` → `BudgetBackoff` 单路径。

任务级明细：plan §批 C + 本任务 `design.md` / `implement.md`。

## Acceptance Criteria

- [ ] `cargo test` / `clippy --all-targets` 零新增 / `make verify` 全绿；派发器经 Fake 多轮压测（乱序完成/慢 consumer/429 注入，现 D13 语义）再接真 provider
- [ ] 五态构造用例全过；agreement 活值；同家族不加；`maxCells = 0` 主区仍挖
- [ ] Fake 全流程 e2e：疑点入队 → reviewer 消费 → 流式落盘不丢 → findings 报告；kill → resume 不丢不双计
- [ ] 无 reviewer 配置 → scan-only 报告与 exit 0 同构
- [ ] reviewer cell 失败 → Approve + exit 2 + coverage failed + warning；预算截断 → Approve + exit 2 + `DeepMeta.budget_skipped`；scanner 失败仍 Incomplete/3
- [ ] `RunMeta.deep` 含 design §9 全字段；`FindingSink` 缺省 no-op 不阻塞派发；`finalize()` 在超时路径必调；旁路/终判按 id 不双发
- [ ] 本仓 dogfood：`redteam-next review` 真 provider 产 finding
- [ ] 收口：ship 评审或三路 code-review 通过 → `--no-ff` 合回 `feat/m21-review-engine`

## Out of Scope

- 速率型 429 棘轮改公式（①②③）与放弃改墙钟
- verifier 流式段 / `verify.rs` / `verifier.md`（批 D）
- 台账激活、跨轮判例真守卫、增量轮（M21c）
- 三方契约填槽（M21d）、第二家族 scanner（M21e）
- 类别权重校准、hint 归一化（R4）
- 候补 / failover / `--workers`

## Technical Notes

- Origin 双名：`pack::region::Origin` vs `domain::finding::Origin`——队列导入改名。
- `FindingEvent` 不泄漏 `QueueItem`。C 用 `FindingSink` 缺省 no-op，不创建 channel；barrier 复核在途恒 0。
- 快照写失败 ≠ job 失败（warning）；finding 权威在 cells，queue_items 是 resume 索引。
- 回滚 = 不合回子分支；schema 仍 v4。
