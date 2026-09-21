# 批 D 实施清单

> 顺序 D-1 → D-5。每 task 一 commit，简体中文 `type(scope): 摘要——细节`，绝不 push。
> 工作树 `~/hobby/.worktrees/redteam/m21b-verifier`，分支 `feat/m21b-verifier`（自 `feat/m21-review-engine` 切）。
> **D-1 的旁路复位必须最先落**：它是「接真 sink 前须处理」的前置债，后面每一步都在真 sink 上跑。
> 定案见 design §12（`/grilling` Q1–Q11）与 decisions D17；判例台账、diff 外 needs 装包、每轮复核上限不在本批。

## Checklist

### D-1 旁路复位 + verifier 角色尾
- [x] `provider/mod.rs`：委托型 `ResetOnAttempt`（`attempt_begin` 复位 `IncrementalFindings`，其余纯委托）；`timed_turn_with` 签名不动
- [x] `review_shot.rs`：reviewer 格把 `GateAttempts` 包一层再传下去
- [x] `templates/verifier.md`（新）：角色尾——「找出这条 finding 不成立的证据」、`analysis` 先于 `verdict` 且不可撤回、契约 `REFUTED | UPHELD` + 一句理由；共享头沿用
- [x] `prompt/mod.rs`：`build_verify_prompt`（finding + `source_snippet` ±60 + 判例摘要空槽 + spec）、`verifier_template_hash()`；
      `adversarial-refute.md` 降级为历史注记（最后引用者消失）
- [x] 测试：首试断流半截 JSON + 重试完整输出 → 不拼接、不发草稿；共享头字节稳定；`analysis` 段序在 `verdict` 之前；模板哈希稳定
- [x] 变异检查：不复位解析器 / 复位放在 `attempt_end` / analysis 与 verdict 段序调换——各自转红
- [x] Commit: `fix(provider,prompt): 流式旁路跨重试复位 + verifier.md 角色尾拆分——analysis 先于 verdict（design §8.1 / §10.7〔D17〕）`

### D-2 触发集、跳过与异族选择（纯判定层，零 provider 调用）
- [x] `review_shot.rs`：`EmitCtx` 带 `hot_ranges`（peak 簇成员疑点的行范围并集），`emit` 只对重叠的 finding 盖 `suspicion_agreement`，其余盖 0
- [x] `deep.rs`：派发时把 peak 簇行范围算出来传进 `EmitCtx`（`QueueItem.members` + `member_families` 现成）
- [x] `verify.rs` 重写判定层：触发（`severity ≥ block ∧ confidence ≥ floor` + 判例变体空槽）、跳过（agreement ≥ 2 / carried / 台账空槽 /
      `enabled = false`）、`pick_verifier`（verifiers 活跃集轮转 → 自动异族 → 同族 + warning）；删批式 `verify_findings`
- [x] 测试：盖章收紧（同格两条 finding 一盖 2 一盖 0）；触发判定表与 `compute_verdict` 的 blocking 集合逐条一致；
      跳过三类各自命中且 `skipped_reason` 分类正确；异族选择序；全同族 → 同族 + warning；`enabled=false` → 全跳过
- [x] 变异检查：盖章不看行范围 / 跳过读 `agreement` 而非 `suspicion_agreement` / 异族选择忽略 family / enabled 不生效——各自转红
- [x] Commit: `feat(consolidate,orchestrate): verifier 触发集与跳过规则——区域共识盖章收紧到 peak 簇行范围、异族选择（design §8.1〔D17〕）`

### D-3 流式消费接线
- [x] `deep.rs`：复核 future 进主循环 `FuturesUnordered`（`registry.lease()` 走同门，复用 C2 等门 FIFO）；
      barrier 增「复核在途 0」；`verify_tail_ms` = 队列排空 → 复核归零。
      **`Phase::Verify` 打点不做**：复核与深挖交织、不是独立阶段，打点会让 `status` 说谎；尾部时长由 `verify_tail_ms` 披露（D-5 记进 design）
- [x] 落盘：cell id `verify[<finding id>]@<model>`、`CellOutput::Verify` 单条 `VerifyEntry`；resume 按 finding id 回填、已复核不重跑
- [x] `dedup.rs`：`merge_into` 补 verify 合并——任一 UPHELD 即 UPHELD、base 无判决继承 other、合并后 REFUTED 则 confidence 归零
- [x] `report.rs`：`Stats` 与顶层计数剔除 refuted；三个计数改挂 **`DeepMeta`**（与 `verify_tail_ms` 同层：判决在深挖循环里产生；serde default、为零不序列化）
- [x] 测试：Fake e2e 链路（落地 → 复核 → REFUTED → 归零不 blocking）；调用失败 → 保守 upheld + warning；
      barrier 等复核归零；resume 零新调用；判决合并三种组合；计数剔除（数组 3 条、stats 2 条）；退化零差与 C3 基线同构
- [x] 变异检查：13 项全红（barrier 不等 / 跳过不拦 / known 不查 / 落地格与往轮两处回填 / 往轮判决不并入 / 复核格不入账 /
      驳回不计数 / 尾部记成整轮墙钟 / 读不懂倒向 refuted / 合并 base 胜出 / REFUTED 不归零 confidence / 计数不剔除）
- [x] Commit: `feat(orchestrate,consolidate): verifier 流式消费——落地即复核、逐条格记录、barrier 增复核在途、判决合并 UPHELD 优先（design §8.1 / §2〔D17〕）`

### D-4 离线基线与开关裁定
- [x] `eval/verify_score.py`（新）：读 `eval/fixtures/ledger-{dismissed,upheld}.jsonl` → 构造 finding + 快照 → `build_verify_prompt` → 单发 →
      混淆矩阵 + 误驳率 + 驳回率；`--baseline` 对比；零第三方依赖（照 `eval/harness.py` 纪律）
- [x] 跑两轮：异族 `deepseek/deepseek-flash`、同族 `agc/glm-5.3`（config sha256 跑前跑后一致）。
      **各 41 条不是 111**：固定集 111 行里，无 quote 41（产品 parse 层本就丢这种形状）、oracle 自相矛盾 14、
      引用锚不回记录的树 9、无 head_sha 6 —— 逐条排除可审计（`cases.json`）
- [x] **按预登记阈值裁定**：两臂都没过（异族 37.5%/12.0%、同族 18.8%/20.0%）→ `verifiers.enabled` 缺省 **false**，
      已写死进配置默认值（serde 缺省与 `Default` 同源 + 用例钉住）与 `review --help`
- [ ] 数字落 `.next/private/m21b/verify/` + followup；research 引用
- [ ] Commit: `chore(eval,config): verify_score.py 与两轮离线基线——按预登记阈值裁定 verifiers.enabled 默认值（design §8.1「去留由数据决定」）`

### D-5 收口
- [ ] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify` / `eval/risk_replay.py` H0 对拍
- [ ] `make build` → dogfood 一轮（同 C3 口径：临时工作树检出 `f1d3ca3`、`--base 674583e`、门 6、samples 1）
  - 护栏：格内 findings ≥ 11（C3 口径）；复核四量（`refute_rate` / 触发 / 跳过 / `verify_tail_ms`）落私有记录
  - scanner 与深挖路径零行为 diff（退化零差基线同构）
- [ ] 记入 `.next/private/m21b/`，账目进 followup；decisions 增 D17 节、design §8.1 / §2 与可观测面同步改写
- [ ] `/code-review` 审核（技能只传 effort，起来后 SendMessage 钉范围：只评 `feat/m21-review-engine...HEAD`）→ 分档修
- [ ] `--no-ff` 合回 `feat/m21-review-engine`；私有记录拷回集成工作树；归档任务 + journal

## 验证命令

```bash
cargo test && cargo clippy --all-targets   # 基线 7
make verify
python3 eval/risk_replay.py                # H0 成区对拍（参数见脚本头）
python3 eval/verify_score.py --fixtures eval/fixtures --verifier deepseek/deepseek-flash
```

## 风险文件与回滚

- 高风险：`deep.rs`（主循环再加一路 future + barrier 条件，C2/C3 的等门与收场语义不得回退——退化零差基线是第一道哨）；
  `dedup.rs`（本批唯一动它的地方，合并规则错会静默改判）；`review_shot.rs`（盖章收紧直接决定跳过面，变异检查覆盖两个方向）。
- `verifiers.enabled` 默认值是本批唯一的产品面开关：基线不达标就默认关，回滚成本 = 改一个默认值。
- 回滚 = 不合回；合回后 revert 该 merge。

## start 前核对

- [x] 批 C3 已合回（`2a454d6`）并归档；集成分支四绿、clippy 7、`make verify` 绿
- [x] `/grilling` Q1–Q11 定案已落 prd 与 design §12
- [x] `implement.jsonl` / `check.jsonl` 各 ≥ 1 条真条目（开工时补）
- [ ] decisions D17 节与 design §8.1 / §2 的改写（D-5 收口时一并落）
