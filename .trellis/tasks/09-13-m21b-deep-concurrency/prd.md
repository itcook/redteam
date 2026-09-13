# M21b 批C1：门驱动并发派发与 reviewer 分派池轮转——队列项身份解耦/并发派发循环/多条目空闲轮转/墙钟门

## Goal

把批 C 的串行单派发器改成 design §6 规则 5 描述的派发形态：provider 门有空位就弹出最高优先项交出去（门驱动并发），活跃 reviewer 条目构成分派池按空闲轮转（不双审）。**验收是墙钟，不是质量**——多 reviewer 在 design 里只买吞吐与分散限流，不产生交叉验证价值。

scanner 与深挖的**时间重叠**（design §2 墙钟公式 `max(scan, 队列排空)`、H0 在 t=0 开挖）**不在本批**，另立批 C2；本批为它预留真实的入队通道。

## Background（已确认）

- 前提：批 C 已合回 `feat/m21-review-engine`（`eac0a60`）。零件就位：`DeepQueue`（入口五态、`pop_dispatch`、`mark_settled`、`drain`）、`dispatch_one`、`run_review_shot_with` + `EmitCtx`、`live_agreement`、`backend_factory` / `on_cell` / `snapshot` 回调。
- **现状是串行**：`run_deep` 主循环 `while pending > 0 { dispatch_one().await; run_review_shot_with().await; }`；reviewer 只取 `input.reviewers.first()`（BTreeMap 字典序，dogfood 实测 `[glm-5.3, grok-4.6]` → 全归 glm-5.3、grok-4.6 空转且无 warning）。
- **串行基线**（批 C dogfood，base `674583e`，12 格，`.next/private/m21b/2026-09-12-batch-c-dogfood.md`）：grok-4.6 **18.3 min**（最长单格 138 s）；glm-5.3 **50.7 min**（最长单格 508 s）。只并发深挖的理论值：grok ≈ 8.0 min、glm ≈ 10.7 min；加上 C2 重叠后 glm ≈ 8.5 min。
- **门按 provider 共享**（`gates.entry(provider)`）：两个 reviewer 同在 `agc`、共享门宽 **20**。墙钟收益几乎全来自并发；多条目在同 provider 下不加吞吐。
- **门的实现**：`InflightGate` 自研（`Mutex<GateState>` + `Notify`），`enter()` = `held < cap` 即取、否则等唤醒；`cap` 是 429 收敛后的有效门宽。
- **队列合并的真实路径**：上游 `merge_suspicions` 组内 union-find（`same_suspicion` 传递闭包），一个文件只属一个组 → 进队列时同文件两条 scan 疑点必然不满足 `same_suspicion`，生产上 scan 项在队列里的合并**全部**走 `find_merge_idx` 的 id 捷径（`file:origin:ranges` 逐字相同），并入家族、抬高 agreement——即使两条是不同问题。
- **批 C 的 resume 数据丢失 bug（主干上活的）**：`run_deep` 的 `n` 每次调用从 1 开始，`cell_put` 按 cell 名 upsert → resume 的 `review[1]@m` 覆盖首跑的 `review[1]@m`（不同队列项）→ 第二次 resume 时该项 finding 永久丢失。批 C C-5 清单的「Settled 以 cells 为准」也未实现。`redteam-next` 在 M21c 切名前不进插件与 ship 循环，故不热修、由本批修。
- **二阶段补查的四处静默**（`second_stage_round`）：重新 `lease` 失败 `.ok()?` 吞掉；用共享 backend（第二轮旁路不通）；policy 的 429 重试 / 等待未回读；第二轮失败 `r.ok()` 吞掉（无 warning、烧掉的 token 不入账）；`needs_two_stage_triggered` 在第二轮没真发时也 +1。
- **兼任角色**：角色级配置 `RoleAttrs` 只有 `thinking_level`；深挖 per-cell backend 由 reviewer 自己的 `EffectiveModel` 构造，已正确。残余仅「兼任 scanner 的 reviewer 的 thinkingLevel 无映射 warning 被 `continue` 跳过」。
- **dedup 的顺序依赖**：输入序第一个当 base，标题 / 正文 / 建议 / 行号保留 base 的；`item_wait_ms` 在派发时 push、其余 meta 数组在完成时 push、失败格只 push 前者。
- design「到期 graceful drain」写在预算行，指预算吃完，不是时间闸；后台 job 40 min 上限走既有 TimedOut + 部分报告。

## Requirements

- **C1-1 身份与 resume 完整性**
  - `QueueItem.id` 在 `new` 时定死，`merge_into` 不再重算；`spec.ranges` 可变不改 id。
  - **共识只经谓词**：`find_merge_idx` 仍用 `same_item ∨ id 相等` 决定合并（保住一个区间只挖一次、id 唯一），但**只有 `same_item` 成立时才并入家族、重算 agreement**；id 捷径合并只并笔记与区间。
  - Skipped 项被并入：并入后 `is_must`（Primary ∨ agreement ≥ 2）→ 复位 Pending（重置 `enqueued_ms`）返回 `MergedPending`；否则保持 Skipped 返回新变体 `MergedSkipped`。
  - reviewer cell_id = `review[{item.id}]@{model}`（跨 resume 稳定，同项重跑即覆盖自身）。
  - resume「Settled 以 cells 为准」：快照 Settled 但不存在 `review[{item.id}]@` 前缀 cell 的项，restore 前改回 Pending。「kill 窗口内换模型重跑留两条 cell」接受并注释。
- **C1-2 并发派发循环**
  - 取门位：新增 `InflightGate::try_enter` / `ProviderRegistry::try_lease`（非阻塞，尊重有效 cap）。
  - 主循环 `FuturesUnordered`：future 内 = 一格完整执行，future 外 = 队列 / meta / cells / 回调。
  - **一格从头到尾持有门位**：二阶段复用首轮 lease（per_turn / retry_budget / policy），第二轮新造 per-cell backend + inc。
  - **入队通道**：`DeepInput` 加可选 `mpsc::Receiver<EnqueueReq>`（生产传 `None`）；`select!` 同时等完成与到达；到达项走入口五态 + 写快照 + 计入队原因 + 更新 `queue_depth_peak`（运行中最大值）。C1 里到达项**不过预算**（在线预算归 C2）。
  - **barrier**：在途 0 ∧ Pending 0 ∧ 通道已关闭（`None` 视为已关）。
  - **报告按派发序组装**：持久化（落盘 / 快照 / `mark_settled` / 读 landed）在完成时做；findings → dedup、cells、meta 数组在收场时按派发序喂入；meta 四个数组按派发序严格对齐（失败格也各一条：output token 记烧掉的、landed 记失败时刻活值）。
  - 事件侧疑点共识滞后接受：`FindingEvent.suspicion_agreement` 为派发快照，不变式 `event ≤ landed`（agreement 单调不减）写进注释并有测试。
- **C1-3 分派池轮转与成员健康**
  - `reviewers` 全集入池，游标轮转，谁先拿到门位谁消费下一项；每项只交一个成员。
  - 成员首次返回 `Auth`（穿透 `attach_usage` 包装判定）→ 踢出池（warning 点名）；已失败格保持失败、不改派。超时 / 5xx / 429 / BadRequest / Parse 不踢。
  - 池空：停止派发；通道未关则继续收项直到关闭；剩余项保持 Pending（resume 可续）+ warning 报数 + 计债。
  - `RunMeta.models` 含实际参与的 reviewer；配了但 0 格的成员给 warning。
  - 兼任 scanner 的 reviewer 补发 thinkingLevel 无映射 warning。
- **二阶段补查契约**
  - triggered 只在第二轮真发出时计；合格但所有 needs 被读守卫拒绝 / 不可读 → skipped、不计债（拒绝原因经 warnings 披露）。
  - 第二轮发出但失败，或第二轮建 pack 内部错误 → **保留第一轮 finding + warning + 计债**。
  - 第二轮烧掉的 token 入账；policy 重试 / 等待在两轮结束后统一回读。
- **C1-4 墙钟门**：Fake + tokio 虚拟时间（`start_paused`），K 格 × 固定时延、门宽和 W → ≈ `ceil(K/W)` 轮；单 reviewer + 门宽 1 退化与批 C 同构。
- 观测字段集不新增（`DeepMeta` 不加字段；在途峰值走 `gate_stats`）。

## Acceptance Criteria

- [ ] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify` 全绿
- [ ] C1-1：在途并入后派发时 id 仍命中；id 捷径合并不抬 agreement、谓词合并照抬；Skipped 异族并入复位 / 同家族并入保持 + `MergedSkipped`；resume 两次不丢 finding（复现批 C bug 的用例翻绿）；快照 Settled 无 cell → 重挖
- [ ] C1-2：门宽 N 时在途峰值 = N；乱序完成不错配；慢格不堵其余；二阶段持门位不饿死；429 注入收敛无死锁；**经真实通道的在途并入**：落盘 `suspicion_agreement` = 落地值 = `landed_agreement`、事件值严格 <；通道未关不收场
- [ ] C1-2：同一输入两次运行报告逐字节一致（完成序被打乱时）；meta 四数组等长且按派发序对齐
- [ ] 二阶段：第二轮失败 → 第一轮 finding 在报告中 + exit 2；全被拒 → skipped、exit 0；triggered 只计真发出
- [ ] C1-3：两成员（不同 provider、门宽各 1）两格并行分落不同模型；一成员 `Auth` → 踢出、其余项全落另一成员；池空 → 剩余 Pending + exit 2；0 格成员有 warning
- [ ] C1-4：虚拟时间墙钟门过；退化用例与批 C 同构
- [ ] dogfood 两轮（base `674583e`，`make build`）：① 当前配置——墙钟对照串行基线 + 每模型每格产出；② `providers.agc.maxConcurrency = 4` 争门轮（跑前备份、跑后恢复）。记入 `.next/private/m21b/`
- [ ] 收口：三路 code-review 或 dogfood 自评审 → 修 → `--no-ff` 合回

## Out of Scope

- **scanner 与深挖时间重叠 → 批 C2**：流式入队接线（scanner 扇出接本批通道）、在线预算与疑点保底预留、等待者感知的门让位（scanner 先于 H0 入门）、reviewer 已开挖时 scanner 失败的报告口径
- 队列优先级 / 预算 / 必挖 / 保底规则本身、region pack、reviewer prompt、`DeepMeta` 字段集
- 批 D 的真 sink（`&()` → mpsc）：C1 与 D 可并行，唯一碰撞点是 `run_deep` 的 sink 接线
- 模型权重 / 主次 / 按项分流（design「无 priority、顺序无语义」不变）；候补 / failover（D7 A3）
- 注入互斥放宽、429 棘轮公式

## Technical Notes

- 并发用 `futures::stream::FuturesUnordered`（`futures 0.3` 已是依赖），同 task 内并发：`ScanLease<'a>` / `&dyn` 回调 / `&dyn FindingSink` 借用形态不变，不要求 `Send` / `'static`。回退路径 `JoinSet` 需 registry `Arc` 化，仅在前者不够时走。
- 虚拟时间测试需 dev-dependency `tokio` 开 `test-util`；Fake backend 的 `with_delay` 走 `tokio::time::sleep`，天然兼容。
- 单格超时 `per_turn = max_tokens/20 + 60` = 默认 1698 s；并发后单格超时不变。
- 分支 `feat/m21b-deep-concurrency`，工作树 `~/hobby/.worktrees/redteam/m21b-deep-concurrency`。
- 定案过程：2026-09-13 两轮裁定（Skipped 复位、事件滞后）+ `/grilling` 十问（Q1–Q10），结论已并入本文与 design.md。
