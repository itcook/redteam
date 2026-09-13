# 批 C1 实施清单

> 顺序 C1-1 → C1-5。每 task 一 commit，简体中文 `type(scope): 摘要——细节`，绝不 push。
> 工作树 `~/hobby/.worktrees/redteam/m21b-deep-concurrency`，分支 `feat/m21b-deep-concurrency`（自 `feat/m21-review-engine` 切出）。
> **C1-1 必须先合**：id 不定死就开并发，在途并入会让 `mark_settled` 找不到项、barrier 永不满足。

## Checklist

### C1-1 队列项身份与 spec 解耦
- [ ] `queue.rs`：`merge_into` 删除 `it.id = item_id(&it.spec)`；id 只在 `new` 时算
- [ ] `EnqueueOutcome::MergedSkipped`；Skipped 并入后 `is_must` → Pending（重置 `enqueued_ms`）+ `MergedPending`，否则保持 Skipped + `MergedSkipped`
- [ ] `deep.rs` `note_enq`：`MergedSkipped` 计入 `dedup_hits`
- [ ] 测试：在途并入后派发时 id 仍命中；id 对 ranges 扩展稳定；Skipped 异族并入复位 / 同家族并入不复位；批 C 形态旧快照 restore 兼容
- [ ] Commit: `fix(queue): 队列项身份与 spec 解耦——id 定死不随 ranges 重算、Skipped 仅成必挖才复位（批 C1 前置，followup 留债）`

### C1-2 并发派发循环
- [ ] `registry.rs`：`InflightGate::try_enter` + `ProviderRegistry::try_lease`（尊重 429 收门后的 cap）
- [ ] `deep.rs`：cell 完整执行（per-cell backend → 两轮单发 → policy 回读 → drop lease → **needs 二阶段**）搬进 `run_cell` future，返回 `CellDone`
- [ ] `second_stage_round` 的 `&mut warnings` 改 future 本地 Vec，经 `CellDone.warnings` 带回
- [ ] 主循环：`FuturesUnordered`；① try_lease 填门位 ② barrier（在途 0 ∧ Pending 0）③ `running.next().await` 处理完成；队列 / meta / cells / 回调全在主循环
- [ ] barrier 兜底：在途 0 ∧ Pending > 0 ∧ try_lease 全 None → 阻塞 lease 一次（理论不可达，防空转）
- [ ] `FindingEvent.suspicion_agreement` 注释补不变式 `event ≤ landed` 与安全方向
- [ ] 测试：在途上限 = 门宽（`gate_stats().peak`）；乱序完成不错配；慢 consumer 不堵；二阶段补查不阻塞其余格；429 注入收敛无死锁；**分叉用例**（`#[cfg(test)]` 钩子在途并入 → 落盘 = 落地值 = `landed_agreement`，事件值严格 <）
- [ ] Commit: `feat(orchestrate): 门驱动并发派发——FuturesUnordered 主循环、cell 整格进 future、在途上限随 provider 门（design §6 规则 5〔D13〕）`

### C1-3 分派池轮转
- [ ] `deep.rs`：`next_lease` 游标轮转，`reviewers` 全集入池；`cell_id` 用主循环派发序
- [ ] 删除 `input.reviewers.first()` 单点；每项仍只交一个条目（不双审）
- [ ] `cli/mod.rs`：`RunMeta.models` 补齐实际参与的 reviewer；配了但 0 格的条目 → warning
- [ ] 测试：两条目（不同 provider、门宽各 1）两格并行分落不同模型；一条目占满 → 全落另一条；队列 1 项时未分到者有 warning；单 reviewer 退化零差
- [ ] Commit: `feat(orchestrate): reviewer 分派池按空闲轮转——多条目买吞吐与分散限流、未分到格不再静默（design §6 规则 5）`

### C1-4 墙钟门与观测
- [ ] Fake 墙钟门：K=12 × 固定时延、门宽和 W=4 → ≈ ceil(K/W) × 时延（容差放宽）
- [ ] 确认 `item_wait_ms` 并发下仍是派发时定格（队列等待，非墙钟）；`DeepMeta` 字段集不变
- [ ] 退化零差：单 reviewer + 门宽 1 与批 C 串行报告同构
- [ ] Commit: `test(deep): 并发墙钟门与退化零差——对照批 C 串行基线`

### C1-5 收口
- [ ] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify`
- [ ] 本仓 dogfood：`make build` → `redteam-next review --base 674583e`（同批 C 配置：reviewers = glm-5.3 + grok-4.6，同在 agc 门宽 20）→ 墙钟对照串行基线（grok 18.3 min / glm 50.7 min）；记入 `.next/private/m21b/`
- [ ] 三路 code-review 或 dogfood 自评审 → 修 → `--no-ff` 合回 `feat/m21-review-engine`
- [ ] followup：「批 C 核查留债」前两条标已解；与批 D 的 sink 接线现状（若 D 先合回则对齐）

## 验证命令

```bash
cargo test
cargo clippy --all-targets        # 基线 7
make verify
# C1-2 在途上限：断言 registry.gate_stats()["p"].peak == 门宽
# C1-5：make build && ./plugins/redteam/bin/darwin/aarch64/redteam-next review --base 674583e --format json
```

## 风险文件与回滚

- 高风险：`deep.rs` 主循环（future 边界划错 → 共享可变状态跨 await / 二阶段阻塞主循环）；`queue.rs` id 语义（resume 快照兼容）。
- `registry.rs` 只加不改：`try_enter` / `try_lease` 与既有 `enter` / `lease` 并存，scanner 路径零 diff。
- 回滚 = 不合回；合回后 revert 该 merge。退化零差用例是第一道回归哨。

## start 前核对

- [x] 批 C 已合回 HEAD（`eac0a60`）并归档（`5f53262`）
- [x] 两处设计裁定已落（2026-09-13：Skipped 仅成必挖才复位；事件滞后接受并钉 event ≤ landed）
- [x] prd.md / design.md / implement.md 已写
- [x] `implement.jsonl` / `check.jsonl` 各 ≥1 条真条目
