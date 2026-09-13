# 批 C 实施清单

> 顺序 C-1 → C-7。每 task 一 commit，简体中文 `type(scope): 摘要——细节`，绝不 push。
> 开工前：从 `feat/m21-review-engine` 切 `feat/m21b-deep` 工作树（hiapi 约定不适用于 hobby；本仓 worktree = `~/hobby/.worktrees/redteam/m21b-deep`）。

## Checklist

### C-1 队列项与入口五态
- [x] `src/orchestrate/queue.rs`：`QueueItem`（`spec: QueueItemSpec` + score/agreement/state/kind）；`enqueue` 五态
- [x] 合并谓词调用 `same_suspicion`（不要复制）；同家族不加 agreement；跨文件不合并；判例守卫空过
- [x] 测试：五态各一；agreement 活值；同家族不加；跨文件不合并
- [ ] Commit: `feat(orchestrate): 深挖队列项与入口五态——merge_suspicions 谓词复用、agreement 活值（design §6 规则 2〔D13〕）`

### C-2 优先级、预算与必挖集
- [x] 优先级键四层；类别权重 const 全 1.0
- [x] `budget = RiskRoute.budget_cells_hint`；必挖不截（含 `maxCells = 0`）；保底 ⌈n/4⌉；drain → `skipped(budget)`
- [x] 测试：排序快照；0 预算主区仍挖；保底配额；drain 清单
- [ ] Commit: `feat(orchestrate): 优先级键+预算+必挖集不截+保底配额+graceful drain（design §6 规则 1/3/4〔D11〕）`

### C-3 门驱动派发器与 reviewer cell 运行时
- [x] 单派发器：`lease` × pop 最高 Pending（`dispatch_one`）；空队列不占门
- [x] `src/orchestrate/review_shot.rs`：`FindingEvent` + `FindingSink`（`()` no-op）；`run_review_shot`（旁路/终判按 id 去重 + 必调 `finalize`）
- [x] `ReviewParseStats.stream_discarded` ← `discarded()`；与 `dropped_quote` 分列
- [x] 顺手：`--tools` 二阶段 + 触发/跳过计数（`on` 档同触发，评审第 2 档修）；`AttemptPolicy` 生产调用点已统一 Some(policy)（single_shot/tool_loop 遵 K11 不动）
- [x] ~~anthropic thinking×tools 守卫~~ **已撤销**（2026-09-12 查证官方文档：「Thinking works alongside tool use」，前提错误）——
      守卫在静默关掉用户配的思考档；真 400 源是模型版本（4.7+ 拒 `type:"enabled"`）与 thinking block 不回传，两条记 followup
- [x] 测试：Fake 429 注入（门收 4→1，单请求速率型棘轮形状）；派发序 + 门宽 1 串行；二阶段触发 e2e；restore 不双计
- [x] 测试：超时路径 finalize + 已闭合保留（核查补——`b93a7d9` 前首轮/REPAIR 轮 turn 返 Err 均未 finalize，实测 discarded == 0）
- [ ] Commit: `feat(orchestrate): 门驱动派发器+reviewer cell 运行时——流式落盘+FindingEvent 发射（design §6 规则 5/§7）`

### C-4 barrier 与 reduce 接线
- [x] `stage0_risk` 返回 `(RiskMeta, RiskRoute)`（区列表不下发 RunMeta；无 reviewer 丢 route）
- [x] barrier：scan 全完 ∧ Pending 空 ∧ InFlight 0 ∧ 复核在途 0（C 恒真）→ dedup → findings 报告（`run_deep` + `overlay_deep`）
- [x] 无 reviewer → scan-only 退化；reviewer 失败 → Approve+2+coverage failed；预算截断 → Approve+2+`budget_skipped`；scanner 失败仍 Incomplete/3
- [x] 测试：Fake e2e 疑点入队 → finding + Approve；reviewer 失败 Approve+2；scan-only 退化仍绿
- [ ] Commit: `feat(cli): barrier 语义+reduce 接线——findings 报告重建、预算截断披露不静默（design §2/§6 规则 6/9）`

### C-5 Phase Deep 与 resume 队列快照
- [x] `Phase::Deep` / `Verify`；旧 `Refute` 可读
- [x] `db.rs`：`queue_item_put` / `queue_items_for_job`；`JobStore.put_queue_item` / `read_queue_items`；`run_deep` 状态转换调 snapshot 回调；`JobCellSink.on_queue_item` 接入（写失败 warning 不败 job）
- [x] resume：零行仍 scan-only；有快照时允许 reviewer cell 记录；未完项（Pending/InFlight）重建 bundles 后恢复队列重跑，Settled 沿用不双计（`run_deep.restore`）
- [x] 测试：Phase round-trip；queue_items 空/覆盖；JobStore 快照 round-trip；restore 不双计 + skipped 保留
- [ ] Commit: `feat(state): Phase Deep/Verify+队列快照 checkpoint+resume 读快照重跑（design §17 R14〔D11〕）`

### C-6 观测收口
- [x] `DeepMeta` 按 design §9 字段进 `RunMeta.deep`（含 `stream_discarded` / `item_wait_ms` / needs 触发跳过）；不重复 inflight/rate_limit；`--pack-only` 不跑深挖
- [x] 测试：RunMeta deep 段在 e2e 断言；DeepMeta 全字段形状在 report.rs
- [ ] Commit: `feat(report): 深挖观测收口——队列/预算/等待/agreement 全链入 RunMeta（design §6 观测面）`

### C-7 收口
- [x] 本仓 dogfood：`redteam-next review --base 674583e` 真 provider 两轮——grok-4.6 exit 2 / 0 finding
      （needs 被注入互斥跳过、模型选择不臆断）；glm-5.3 exit 10 / **13 finding**（验收达成）。
      记录 `.next/private/m21b/2026-09-12-batch-c-dogfood.md`（含串行墙钟基线 91 s / 253 s 每格）
- [x] `cargo test`（lib 639 / e2e 17 / packaging 7 / cli 17）/ `clippy --all-targets` 零新增（7 = 基线逐条一致）/ `make verify` 全绿
- [x] 自查修 4 条（`b93a7d9`）：resume 空变更集 panic、错误路径漏 finalize、resume 丢 spec 段、landed agreement 死值；
      余 3 条不可触发项记 `.next/roadmap/followup.md`「批 C 核查留债」
- [x] 三路 code-review（M21 期间替代 ship）15 条 → 第 1/2 档修 11 条、thinking×tools 守卫前提证伪撤销、
      agreement 拆轴；dogfood 自评审 13 条 → 修 3 条（含一条 code-review 漏掉的空断言测试，变异验证）
- [x] `--no-ff` 合回 `feat/m21-review-engine`（`eac0a60`），主干四绿复验通过
- [x] 未兑现项另立批 C1（门驱动并发 + 分派池轮转，plan 已写）

## 验证命令

```bash
cargo test
cargo clippy --all-targets
make verify
# C-3：Fake 乱序完成 / 慢 consumer / 429 注入（现 D13 收门语义）
# C-7：redteam-next review --base <sha>   # 配 reviewers 档，真 provider
```

## 风险文件与回滚

- 高风险：`src/orchestrate/queue.rs` 派发循环（死锁退路 = 超时报警 + drain）；`src/cli/mod.rs` 主流程（scanner 退化路径必须零差）。
- `AttemptPolicy` 重构碰 scanner 调用点——每步留行为不变测试。
- 回滚 = 不合回；合回后 revert 该 merge。快照写失败 ≠ job 失败（design §7.1）。

## start 前核对

- [x] A/B 已合回 HEAD
- [x] 429 棘轮开工裁定已落（沿用 D13）
- [x] design.md / implement.md 已写
- [x] `implement.jsonl` / `check.jsonl` 各 ≥1 条真条目
