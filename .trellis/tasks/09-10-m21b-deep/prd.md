# M21b 批C：深挖队列与 reviewer 运行时——入口五态/优先级预算/门驱动派发/barrier/Phase Deep+resume/观测

## Goal

M21b 核心编排批（前提批 A/B 合回）：`src/orchestrate/queue.rs` 深挖优先级队列（入口五态、agreement 活值、优先级键、预算/必挖集不截/保底配额/graceful drain）、tokio mpsc 门驱动单派发器（provider 门空位驱动，无角色 workers〔D13〕）、`run_review_shot`（流式落盘 + `FindingEvent` 发射）、barrier → reduce 接线（findings 报告重建、截断披露不静默）、Phase Deep/Verify + `queue_items` 快照 resume、RunMeta 观测收口。finding 只由深挖产出（C7）。

## Requirements

- 入口五态〔D13 Q-A〕：无命中/排队中并入/在途只改记录/本轮已结案沿用/跨轮判例守卫（判例 M21c，本批空过按无命中入队）；合并谓词复用 `merge_suspicions`（quote 主锚、hint 辅助，R4）；同家族重复只合并不加 agreement
- 优先级键：主区 H0 最高 → 涟漪 → 疑点 `confidence × agreement × 类别权重` → needs 中档；非 FIFO、不用置信阈值切〔D11-5〕
- 预算：A 批 `budget_cells`；必挖集 = 主区 ∪ {agreement ≥ 2} 不截（`maxCells = 0` 时 H0 仍被挖）；疑点保底 ⌈max_cells/4⌉ 格；到期 graceful drain，截断项 `deep: skipped(budget)` 进 RunMeta 不静默〔D11-4/5〕
- 派发：单派发器按 reviewer 条目 provider 门空位弹出最高优先项（`registry.lease()`）；t=0 入队序 scanner 先于 H0〔D13 Q2〕；scan 来源项不等同组兄弟 cell（先到先挖、后到并入记录，记派发时/落地时 agreement〔D13 Q4〕）；scanner 逐 cell 交付〔D13 Q-B〕
- reviewer cell：`run_review_shot` = `build_review_prompt` + 批 B `on_text_delta` 流式落盘 + `parse_review_output` 终判；finding 落地即发射 `FindingEvent`（批 D 接口：finding + 落地时刻 agreement + reviewer family）；usage/attempts/queue_wait_ms 对齐 `ScanCall` 口径
- barrier〔design §2〕：scanner 全完 ∧ 队列空 ∧ 在途 0 ∧ 复核在途 0 → dedup → 报告；verdict 从 findings 重建；无 reviewer 条目 → scan-only 退化路径
- resume〔D11-7/R14〕：Phase 加 `Deep`/`Verify`（serde lowercase，旧 `Refute` 可读）；`queue_items` 快照 checkpoint；resume 读快照——Settled 沿用不双计、Pending/InFlight 重跑；未闭合对象丢弃计 warning
- 观测：队列深度峰值/入队原因分布/去重命中/预算截断数/项等待时长/每 cell 输出 token（< `maxOutputTokens`/2 口径）/per-provider 每角色在途峰值/门等待/429 收敛门宽（已有）/派发时落地时 agreement
- 无候补：reviewer cell 重试耗尽即失败 → 带债 approve + coverage failed + 披露
- 顺手项（followup 已归 M21b）：Anthropic thinking×tools 守卫（`build_anthropic_tools_body_ext` 按 tools 非空）；reviewer needs 二阶段（R2，`--tools` 首个消费者，复用 `second_pass.rs`，触发率入 RunMeta）；`AttemptPolicy` Option 三层重构（`BudgetBackoff` 单路径）；exit_code 第二参（failed_models）语义定夺；resume 按快照角色条目重建 registry / RunMeta.models
- 任务级明细：plan §批 C（Task C-1…C-7）

## Acceptance Criteria

- [ ] `cargo test` / `clippy` / `make verify` 全绿；派发器经 Fake 多轮压测（乱序完成/慢 consumer/429 注入）再接真 provider
- [ ] 五态构造用例全过；agreement 活值重算优先级；同家族不加；`maxCells = 0` 必挖用例
- [ ] Fake 全流程 e2e：疑点入队 → reviewer 消费 → 流式落盘不丢 → findings 报告；kill 模拟 → resume 续跑不丢不双计
- [ ] RunMeta deep 段全字段；reviewer cell 失败 → 带债 approve + warning
- [ ] 本仓 dogfood：`redteam-next review` 端到端产 finding（真 provider）
- [ ] 收口：dev `redteam` ship 评审通过 → `--no-ff` 合回

## Notes

- design.md / implement.md 不预写：开工时 grill 或脑暴产出（输入 = 本 prd + plan 批 C 节 + design §2/§6/§7/§12.3〔D13〕）。
- 决策依据：D2（队列五细节+勘误）、D6 采纳 2/3、D10-4（分支形态）、D11-4/5/7、D13 全节。
- `FindingEvent` 是与批 D 的唯一接口，先定义契约再动 verify.rs（避免队列内部状态泄漏进复核）。
