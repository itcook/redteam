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
- [ ] 顺手：thinking×tools 守卫；`--tools` 二阶段 + 触发/跳过计数；`AttemptPolicy` → `BudgetBackoff` 单路径（C-4 接线时做，本步不扩）
- [ ] 测试：Fake 多 finding 流不丢不双发；超时路径 finalize + 已闭合保留；`()` sink 不阻塞；派发序；scan 完成入队；Fake 429 注入（现 D13 语义）；needs 触发率；anthropic tools 守卫
- [ ] Commit: `feat(orchestrate): 门驱动派发器+reviewer cell 运行时——流式落盘+FindingEvent 发射（design §6 规则 5/§7）`

### C-4 barrier 与 reduce 接线
- [x] `stage0_risk` 返回 `(RiskMeta, RiskRoute)`（区列表不下发 RunMeta；无 reviewer 丢 route）
- [x] barrier：scan 全完 ∧ Pending 空 ∧ InFlight 0 ∧ 复核在途 0（C 恒真）→ dedup → findings 报告（`run_deep` + `overlay_deep`）
- [x] 无 reviewer → scan-only 退化；reviewer 失败 → Approve+2+coverage failed；预算截断 → Approve+2+`budget_skipped`；scanner 失败仍 Incomplete/3
- [x] 测试：Fake e2e 疑点入队 → finding + Approve；reviewer 失败 Approve+2；scan-only 退化仍绿
- [ ] Commit: `feat(cli): barrier 语义+reduce 接线——findings 报告重建、预算截断披露不静默（design §2/§6 规则 6/9）`

### C-5 Phase Deep 与 resume 队列快照
- [ ] `Phase::Deep` / `Verify`；旧 `Refute` 可读
- [ ] `db.rs`：`queue_item_put` / `queue_items_for_job`；enqueue/dispatch/settled/skipped 各写一次；settled 先写 cells 再快照；写失败 warning 不败 job
- [ ] resume：零行 → scan-only；Settled 以 cells 为准（快照有、cells 无 → 重跑）；Pending/InFlight 重跑；registry 按快照角色重建
- [ ] 测试：kill 模拟不丢不双计；未闭合 discard；旧作业零行 scan-only resume；快照写失败本轮仍完成
- [ ] Commit: `feat(state): Phase Deep/Verify+队列快照 checkpoint+resume 读快照重跑（design §17 R14〔D11〕）`

### C-6 观测收口
- [ ] `DeepMeta` 按 design §9 字段进 `RunMeta.deep`（含 `stream_discarded` / needs 触发跳过）；不重复 inflight/rate_limit；`--pack-only` 不跑深挖
- [ ] 测试：RunMeta 快照含 deep 段
- [ ] Commit: `feat(report): 深挖观测收口——队列/预算/等待/agreement 全链入 RunMeta（design §6 观测面）`

### C-7 收口
- [ ] 本仓 dogfood：`redteam-next review --base <recent>` 真 provider 产 finding
- [ ] `cargo test` / `clippy --all-targets` 零新增 / `make verify`
- [ ] ship 评审（dev `redteam`；config schema 错位则走三路 code-review 如批 B）→ 修 → `--no-ff` 合回
- [ ] Commit: 评审修（如有）

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
