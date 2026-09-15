# M21b 批D：verifier 流式段——verifier.md/触发跳过选择/流消费 refute 率/离线回归基线

## Goal

（前提批 C 合回，消费其 `FindingEvent`）`verifier.md` 角色尾拆分（analysis 先于 verdict 且不可撤回）、触发集与跳过规则与异族选择〔D7 A4〕、`verify.rs` 从 barrier 型改 finding 流消费者（失败保守 upheld、refute 率入 RunMeta、接入批 C barrier 的复核在途计数）、`eval/verify_score.py` 用 `eval/fixtures/ledger-{dismissed,upheld}.jsonl` 做二分类离线回归并记基线。

## Requirements

- `verifier.md`：从 `adversarial-refute.md` 拆角色尾（共享头沿用）；「找出这条 finding 不成立的证据」；输出契约 `analysis → REFUTED | UPHELD + 理由`，analysis 先于 verdict；模板哈希；`adversarial-refute.md` 随最后引用者消失删除或降级注明
- `build_verify_prompt`：finding + ±60 快照（复用 `verify.rs` `source_snippet` 机器）+ 判例摘要槽（M21c 空）+ spec
- 触发集：`severity ≥ block ∧ confidence ≥ floor` + 判例变体槽（枚举预留，判例数据 M21c）；低危不触发；消费 `--block-threshold` / `--conf-floor` 旗（M21a 无消费者旗，`--help` 文案届時改回）
- 跳过：两异族 scanner 标记（finding 区域 agreement ≥ 2，按**落地时刻**值、后涨不回溯〔D13〕）∧ reviewer 确认；`review.verifiers.enabled = false` → 全跳过（记录理由）
- verifier 选择：verifiers 活跃集（拥有 `verifier` 角色的模型，D12 JSON）非空则从中选；缺省自动异族（family 缺省 model_id 首段），优先 reviewers 档 → scanners 档；无异族 → 同族 + warning「无异族 verifier」
- 流式消费：finding 落地即入复核流（与深挖尾部重叠，不等 barrier）；消费循环经 `registry.lease()` 走同一 provider 门；REFUTED → confidence 归零永不 blocking；UPHELD 原样；调用失败 → 保守 upheld + warning；refute 率 = refuted ÷ 触发复核数 → RunMeta `refute_rate`；`Phase::Verify` 打点
- 离线回归集〔D9〕：`eval/verify_score.py` 读 ledger fixtures → 逐条构造 finding+快照 → 二分类准确率/混淆矩阵，`--baseline` 对比；固定集锚定按 `head_sha` 树、空 `head_sha` 按 created_at 推断（followup 行 73）；改 verifier.md 必跑
- 任务级明细：plan §批 D（Task D-1…D-5）

## Acceptance Criteria

- [ ] `cargo test` / `clippy` / `make verify` 全绿
- [ ] 触发/跳过判定表全过；全 deepseek 构造 → 同族 + warning；enabled=false 全跳过
- [ ] Fake e2e：finding 落地 → verifier 消费 → refute 链路；失败保守 upheld；barrier 等复核在途归零
- [ ] 共享头字节稳定；analysis 段序断言；scanner 路径零 diff
- [ ] `verify_score.py` 全量 fixtures 基线数字落 `.next/private/m21b/verify/` 并写 research；收口跑一次不低于首跑
- [ ] 收口：dev `redteam` ship 评审通过 → `--no-ff` 合回

## Notes

- design.md / implement.md 不预写：开工时 grill 或脑暴产出（输入 = 本 prd + plan 批 D 节 + design §8.1 / §10.7〔D7/D9〕）。
- 决策依据：D7 A4（verifier 去留交数据）、D9（角色尾与回归集）、D10-1（流式）、D13（落地时刻 agreement）。
- M21c 接口：判例摘要槽与判例变体触发是空槽——形状先到位，M21c 填。
- **前提变更（2026-09-15，D15）**：批序改为 C2 → C3 → D，本批前提由「批 C 合回」改为「批 C3 合回」。开工时按 C1 / C2 / C3 合回后的接口刷新本 prd：
  `run_deep` 仍传 `&()` sink、barrier 需加「复核在途 0」；C2 的门等门者让位与放门唤醒对 verifier 同门（与 reviewer 同在 agc）直接复用；
  `FindingEvent.suspicion_agreement` 为派发快照（`event ≤ landed`）；「流式旁路跨瞬时重试不复位」须在接真 sink 前修（followup）。
