# M21b 批C1：门驱动并发派发与 reviewer 分派池轮转——队列项身份解耦/并发派发循环/多条目空闲轮转/墙钟门

## Goal

把批 C 的串行单派发器改成 design §6 规则 5 描述的形态：provider 门有空位就弹出最高优先项交出去（门驱动并发），活跃 reviewer 条目构成分派池按空闲轮转（不双审）。**验收是墙钟，不是质量**——多 reviewer 在 design 里只买吞吐与分散限流，不产生交叉验证价值。

## Background（已确认）

- 前提：批 C 已合回 `feat/m21-review-engine`（`eac0a60`）。零件就位：`DeepQueue`（入口五态、`pop_dispatch`、`mark_settled`、`drain`）、`dispatch_one`（`registry.lease` × pop）、`run_review_shot_with` + `EmitCtx`、`live_agreement`、`backend_factory` / `on_cell` / `snapshot` 回调。
- **现状是串行**：`run_deep` 主循环 `while pending > 0 { dispatch_one().await; run_review_shot_with().await; }`，单格跑完才派下一格；reviewer 只取 `input.reviewers.first()`（BTreeMap 字典序，dogfood 实测 `[glm-5.3, grok-4.6]` → 全归 glm-5.3、grok-4.6 空转且无 warning）。
- **串行基线**（批 C dogfood，base `674583e`，12 格，记录 `.next/private/m21b/2026-09-12-batch-c-dogfood.md`）：grok-4.6 **18.3 min**（91 s/格，最长单格 138 s）；glm-5.3 **50.7 min**（253 s/格，最长单格 508 s）。
- **门按 provider 共享**：`registry.insert_scan` 以 provider 名为键建 `InflightGate`（`gates.entry(provider)`）。dogfood 配置的两个 reviewer 都在 `agc` 下、共享门宽 **20**（`providers.agc.maxConcurrency`）。推论：
  - 墙钟收益**几乎全部来自 C1-2 并发**——12 格 < 门宽 20，单 reviewer 就能一轮发完，理论墙钟 ≈ 最长单格（grok ~2.3 min、glm ~8.5 min），而非各格之和；
  - C1-3 多条目在**同 provider** 下不加门宽，只贡献模型分摊；「买吞吐与分散限流」只在**跨 provider** 配置成立。
- **并发即真的两条留债**（followup「批 C 核查留债」，已有主 → C1-1）：`merge_into` 在 union ranges 后重算 `it.id`，而 InFlight 项已按旧 id 克隆派发——在途期间并入会让 `mark_settled` 找不到项、卡 InFlight、barrier 永不满足；`merge_into` 命中 Skipped 返回 `MergedPending` 却不改回 Pending。串行下入队全在消费循环前所以不可触发，并发一开就真。
- **事件侧疑点共识滞后**（批 C dogfood 修 [2] 时留的口子，plan C1-2 已记）：`EmitCtx` 跑在 await 里读不到队列，`FindingEvent.suspicion_agreement` 是派发时快照；落盘 finding 由主循环按 `live_agreement` 回填。串行下两值恒等；并发后在途并入会让落地值更高、事件值滞后——而批 D 的跳过规则按事件流判。

## Requirements

- **C1-1 队列项身份与 spec 解耦**：`QueueItem.id` 在 `new` 时定死，`merge_into` 不再重算；`spec.ranges` 可变不改 id。Skipped 项被并入时的处置（见 design 决策）。旧快照（已入库 id 形态）`restore` 兼容。
- **C1-2 并发派发循环**：派发仍在主循环做（拿 lease → `pop_dispatch` → 组 future 入池），future 内部不碰队列，完成后回主循环更新队列。在途上限跟随 provider 门宽，不另设角色级池（D13）。barrier 语义不变：Pending 空 ∧ InFlight 0 ∧ 复核在途 0（批 D 前恒真）——此后 InFlight 真会 > 1。事件侧滞后的处置（见 design 决策）。
- **C1-3 分派池轮转**：`reviewers` 全集入池，谁先拿到门位谁消费下一项；每项只由一个模型消费。`cell_id` 序号并发下唯一。`coverage.selected[Reviewer]` 与 `RunMeta.models` 含全部**实际参与**条目；配了但一格没分到的条目给 warning（不再静默）。
- **C1-4 墙钟门与观测**：观测沿用 C-6 字段，不新增 `DeepMeta` 字段（在途峰值走 `gate_stats`）；`item_wait_ms` 在并发下仍量队列等待（派发时定格）。Fake 墙钟门：K 格 × 固定时延、门宽和 W → 总时长 ≈ `ceil(K/W) × 单格时延`。
- 退化零差：单 reviewer、门宽 1 的配置下，行为与批 C 串行逐字节同构。

## Acceptance Criteria

- [ ] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify` 全绿
- [ ] C1-1：在途并入后 `mark_settled` 仍命中；id 对 ranges 变化稳定；旧快照 restore 兼容
- [ ] C1-2：门宽 N 时在途上限 = N；乱序完成不错配 cell_id 与 item；慢 consumer 不堵住其余；429 注入（现 D13 收门语义）下收敛；无死锁（压测带超时）
- [ ] C1-2：**派发 ≠ 落地分叉用例**——在途期间并入异族 → 落盘 `suspicion_agreement` = 落地值、`landed_agreement` 同值；事件侧按 design 决策的形态有测试钉住
- [ ] C1-3：两条目 Fake → 两格并行且分落不同模型；一条目 provider 满 → 另一条目继续吃单；未分到格的条目有 warning
- [ ] C1-4：Fake 墙钟门过；门宽 1 / 单 reviewer 退化用例与批 C 同构
- [ ] 本仓 dogfood：同 base `674583e`、同配置重跑，墙钟对照串行基线（grok 18.3 min / glm 50.7 min）记入 `.next/private/m21b/`
- [ ] 收口：三路 code-review 或 dogfood 自评审 → 修 → `--no-ff` 合回

## Out of Scope

- 队列语义（入口五态 / 优先级 / 预算 / 必挖 / 保底）、region pack、reviewer prompt、观测字段集——本批只改派发形态
- 批 D 的真 sink（`&()` → mpsc）：C1 与 D 同属波 3 可并行，唯一碰撞点是 `run_deep` 的 sink 接线；先合回者在 followup 记接口现状
- 注入互斥放宽（followup「批 C dogfood 自评审未修项」，与并发无关）
- 跨 provider 负载均衡策略、候补 / failover、`--workers`（D13 否决）
- 429 棘轮改公式（速率型 ①②③，等真速率型 429 再裁）

## Technical Notes

- **实现路径**（plan C1-2）：先试 `FuturesUnordered` 同 task 内并发——`ScanLease<'a>` 借用 registry、`snapshot` / `on_cell` / `sink` 都是 `&dyn`，同 task 并发时借用形态全不用改；cell 是纯 IO 等待，协作式并发足够。回退路径 `JoinSet` spawn 需要 `'static`，会连锁要求 registry `Arc` 化 + 回调改 `Arc<dyn Fn + Send + Sync>`，改动面大一圈，仅在 A 被证明不够时走。
- 填门位与收结果：`tokio::select!` 一边 `registry.lease(...)`、一边 `running.next()`；或给 registry 加 `try_lease()`（`Semaphore::try_acquire`）做非阻塞试探。多条目轮转天然落在这里——对每个有 pending 的 reviewer 条目尝试取门位。
- 队列锁（若引入）**绝不跨 await**；死锁退路 = 整体超时 + 现有 `drain`。
- 单格超时上限 `per_turn = max_tokens/20 + 60` = 默认 1698 s（28 min）：并发后单格超时不变，但总墙钟不再是各格串行相加。
- 分支 `feat/m21b-deep-concurrency`，工作树 `~/hobby/.worktrees/redteam/m21b-deep-concurrency`（自 `feat/m21-review-engine` 切出）。
