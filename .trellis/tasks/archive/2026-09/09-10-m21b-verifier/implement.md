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
- [x] `cargo test` 803 + 17 / `clippy --all-targets` 3（本分支基线，非 7）/ `make verify` 绿 /
      `risk_replay.py` H0 对拍 jaccard 1.0、R 差 2e-8（**须显式 `--theta 1.5`**：Python 侧缺省 2.0，不传两侧 θ 不一致会假红）
- [x] `make build` → dogfood 一轮（同 C3 口径，复核临时开；跑完还原配置并校验指纹）
  - [x] 护栏过：格内 findings **22** ≥ 11；复核四量 `refute_rate` 0.2 / 触发 5 / 跳过 consensus 1 + not_triggered 16 /
        `verify_tail_ms` 3.5 min，且 **5+1+16 = 22 = 格内 findings**（同分母）
  - [x] 深挖路径零行为 diff：格数 / 选项五量 / `enqueue_reasons` / 阻塞 / 拆簇全部落在 C3 三轮散布内，
        选中项对 C3 验证轮重合 50%（C3 同二进制三对 50/41/41 的上沿）；退化路径由 803 条单测（全跑 `verify: None`）保证
- [x] 记入 `.next/private/m21b/2026-09-21-batch-d-dogfood.md` + `raw/`，账目进 followup；decisions 增 D17 节、
      design **v13** §8.1 全节重写 + §2 + §6 可观测面；plan 批 D 节加「实际执行」注记、roadmap 交付项 5 标 ✅
- [x] `/code-review max` 审核（只传 effort + SendMessage 钉范围）→ 15 条 + 9 条 runner-up，分三档：
  - 第 1 档（正确性，全修 `0290798`）：verifier 角色尾没过 render（`{{OUTPUT_LANGUAGE}}` 字面量进了每次调用，含 D-4 基线）/
        热区盖章不比文件（跨文件白拿共识 → 永久免验）/ 复核快照绕过读守卫 / 没跑成的复核落成 ok 格并被 `known` 固化 /
        `refute_rate` 分母含没跑成的 / 触发集不吃 CLI 覆盖阈值 / 空变更集早退漏回填往轮判决 / 缓存判定让时序决定验不验
  - 第 2 档（语义与用户可见，全修 `48073b0`）：事件侧盖章两份值打架 / REFUTED 没归零 confidence /
        显式声明的 verifier 不查独立性（自己验自己无告警，而 --help 承诺异族）/ verifier 造不出 backend 拒跑整条命令 /
        md 头条与详情对不上 / eval 零分母判成通过
  - 第 3 档（记账进 followup）：复核无上限且在门上造成优先级反转 / `verify_refuted` 解释不了头条差 /
        `(v, None)` 让孤条 REFUTED 盖过免验兄弟 / verifier 格三个观测字段不准 / `SkipCarried` 空话 / `cached` 按条次计
  - 变异：第 1 档 7 项 + 第 2 档 5 项逐条转红（`.next/private/m21b/risk/mutate_d5{,b}.py`）
- [x] **模板缺陷牵出的重测**：#4 说明 D-4 那批基线是拿带模板标记的 prompt 跑的 → 修好后按同一预登记阈值重跑两臂
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
