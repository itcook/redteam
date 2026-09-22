# 批 D 技术设计：verifier 流式段

> 输入：prd + plan 批 D 节 + design §8.1 / §10.7〔D7, D9, D10, D13〕+ C1/C2/C3 合回后的实际接口。
> 定案：2026-09-21 `/grilling` 十一问（Q1–Q11），结论见 §12；权衡与被否见 decisions **D17**（本批新增节）。
> **不在本批**：判例台账与判例变体触发（M21c 留空槽）、变更集外 needs 装包（C4 / M21d）、`agreement` 轴 legacy 化、
> md 展示 `suspicion_agreement`、每轮复核上限旋钮（实测 4–7 条/轮，数据否掉）、verifier 工具循环。

## 1. 边界与文件

| 文件 | 动作 | 内容 |
|---|---|---|
| `src/prompt/templates/verifier.md`（新） | 新增 | 从 `adversarial-refute.md` 拆角色尾：「找出这条 finding 不成立的证据」、`analysis` 先于 `verdict` 且不可撤回、契约 `REFUTED \| UPHELD` + 一句理由 |
| `src/prompt/mod.rs` | 修改 | `build_verify_prompt`（finding + ±60 快照 + 判例摘要空槽 + spec）、`verifier_template_hash()`；`REFUTE_TPL` 随最后引用者消失降级为历史注记 |
| `src/provider/mod.rs` | 修改 | 委托型 `AttemptPolicy` 包装点（旁路复位，见 §7）——只加类型，不动 `timed_turn_with` 签名 |
| `src/orchestrate/review_shot.rs` | 修改 | 盖章收紧（§2）：`EmitCtx` 带上本项成员疑点的行范围；旁路复位接线 |
| `src/consolidate/verify.rs` | **重写** | 触发集 / 跳过 / 异族选择（§3、§4）+ 单条复核调用（§5）；保留 `source_snippet`、`matches_precedent`、verdict 解析；删 `verify_findings` 批式入口 |
| `src/orchestrate/deep.rs` | 修改 | 复核 future 进主循环 `FuturesUnordered`、barrier 增「复核在途 0」、`verify_tail_ms`、逐条 verifier 格记录、观测三量 |
| `src/consolidate/dedup.rs` | 修改 | `merge_into` 补 verify 合并（§6）——本批唯一动 dedup 处 |
| `src/domain/report.rs` | 修改 | `Stats` 计数剔除 refuted；RunMeta 报被驳条数（`refute_rate` 已在） |
| `eval/verify_score.py`（新） | 新增 | 111 条夹具的二分类评分与 `--baseline` 对比（§8） |

不碰：scanner 路径、深挖的入队 / 合并 / 排序 / 预算（C3 定案全部保持）、`Finding` 与 `CellRecord` 的 schema（v4 不变）。

## 2. 盖章收紧（Q2）

**现状**：`EmitCtx` 拿队列项**派发时**的家族数，盖给该 cell 的每一条 finding；C3 把项放大成整个 def / 标题节后，
同块里 reviewer 自己新发现的 finding 也会拿到 `≥ 2`，等于白送免验。

```text
EmitCtx::new(model, family, suspicion_agreement, hot_ranges, root, sink)
  hot_ranges = 该队列项内**达成共识的簇**所覆盖的行范围（peak 簇的成员疑点行区间并集）

emit(f):
  f.suspicion_agreement = if overlaps(f.line_start..=f.line_end, hot_ranges, ±LINE_GAP) { 派发快照值 } else { 0 }
```

- `hot_ranges` 取的是 `peak_agreement` 那个簇的成员行范围，不是整块——「两家族对上的是这几行」才是跳过的依据。
- `LINE_GAP` 复用 `consolidate::dedup::LINE_GAP`（= 3），与 `same_suspicion` 的行重叠口径同源。
- 仍是**派发时快照**（`event ≤ landed`，D13 裁定）：偏差方向只会多复核、不会漏复核。

## 3. 触发集与跳过

```text
触发 = severity ≥ blockThreshold ∧ confidence ≥ confFloor        // 缺省 medium / 0.5
       ∨ 命中判例变体                                            // M21c 空槽，本批恒 false
跳过 = suspicion_agreement ≥ 2                                   // §2 收紧后的值
       ∨ carried（出生轮已复核）
       ∨ 精确 id 已在台账                                        // M21c 空槽
       ∨ review.verifiers.enabled == false                       // 全跳过，记理由
```

- 两个「无消费者旗」（`--block-threshold` / `--conf-floor`）自此有消费者，与 `verdict::compute_verdict` 同源——
  「会让 verdict reject 的那批 finding」正是「值得花一次复核的那批」。
- 跳过也要**记账**：`skipped_reason` 分布进 RunMeta（agreement / carried / disabled 三类），否则「没触发」与「触发了被跳过」分不开。

## 4. verifier 选择（design §8.1，不改）

```text
pick_verifier(finding, roles):
  if !roles.verifiers.is_empty() → 从 verifiers 活跃集选（轮转，与 reviewer 分派池同款）
  else → 自动异族：family ≠ finding 的 reviewer family，优先 reviewers 档 → scanners 档
  若一个异族都没有 → 同族 + warning「无异族 verifier」
```

现配置下 `glm-5.3` 同时是唯一 reviewer 与唯一 verifier → 走第一条、同族自我复核。**这正是 §8 要量的那件事**。

## 5. 流式接线与落盘（Q7 / Q8）

```text
run_deep 主循环（C2/C3 的 select! + FuturesUnordered 不改形状，只加一路）：
  finding 落地（land）→ 判触发 / 跳过 → 命中则 registry.lease(verifier) → 复核 future 进同一个 FuturesUnordered
  复核完成 → 写 verifier 格记录 → 回填 Finding.verify → 计数

落盘：cell id = verify[<finding id>]@<model>，CellOutput::Verify { verdicts: [VerifyEntry{id, verdict, note}] }
      append_partial 本就是按 cell id 的 upsert → 同 id 重复写幂等
resume：read_partial 里的 verifier 记录按 finding id 回填，已复核的不重跑（与 Q11 的 id 缓存是同一份数据）
barrier：empty_for_barrier() 增「复核在途 0」；verify_tail_ms = 队列排空 → 复核归零
```

- 复核走 `registry.lease()`，与 reviewer 共用 provider 门；C2 的等门 FIFO / 首试让位 / 放门唤醒**原样复用**，不新写一套。
- 复核 future 与 reviewer 格在同一个 `select!` 里 → 阻塞计时（`fill_block_*`）自动覆盖复核的同步段。

## 6. 判决合并与计数口径（Q11 / Q9）

```text
merge_into(base, other):
  …现有逻辑不动…
  base.verify = match (base.verify, other.verify) {
      (None, v) | (v, None)            => v,                 // 无判决的一侧不掩盖有判决的一侧
      (Some(a), Some(b))               => 任一 UPHELD → UPHELD（note 取 UPHELD 那条）; 否则 REFUTED
  }
  if base.verify == REFUTED { base.confidence = 0.0 }          // 与「refuted 归零」统一，避免 max 取出 0.7
```

- 方向与 Q6 的门槛一致：**误驳比漏驳危险**，所以合并偏向保留。
- `Stats::from_findings` 与顶层计数剔除 `verify == REFUTED` 的条目；`findings` 数组原样保留（带标注）供审计与 M21c 回流。

## 7. 旁路复位（Q3）

```text
struct ResetOnAttempt<'a> { inner: &'a dyn AttemptPolicy, inc: Arc<Mutex<IncrementalFindings>> }
impl AttemptPolicy for ResetOnAttempt<'_> {
    fn attempt_begin(&self) { self.inc.lock().unwrap().reset(); self.inner.attempt_begin() }
    // attempt_end / on_rate_limited 纯委托
}
```

reviewer 格把 `GateAttempts` 包一层再传给 `timed_turn_with`。重试轮仍有流式落盘（D10 立旁路就是为了「格中途死亡时已闭合对象不丢」），
重复对象由 `EmitCtx` 的 id 去重挡住。**先于**任何真 sink 接线落地。

## 8. 离线基线与开关裁定（Q5 / Q6）

```bash
python3 eval/verify_score.py --fixtures eval/fixtures --verifier <route_key> [--baseline <prev.json>]
# 逐条：夹具 → 构造 Finding + source_snippet → build_verify_prompt → 单发 → 判决
# 输出：混淆矩阵 + 误驳率（upheld 被判 REFUTED）+ 驳回率（dismissed 被判 REFUTED）
```

| 轮 | verifier | 用途 |
|---|---|---|
| 异族 | `deepseek/deepseek-flash` | §8.1 异族原则的实测值 |
| 同族 | `agc/glm-5.3` | 现配置默认路径（glm 复核 glm）的实测值 |

**预登记裁定**（阈值先于跑数定死）：误驳率 ≤ 5% **且** 驳回率 ≥ 30% → `verifiers.enabled` 默认 `true`；否则默认 `false`。
两轮都不达标 → 默认关且在 followup 写明「本批交付的是可评估的 verifier，不是开着的 verifier」。

## 9. 观测

| 字段 | 定义 |
|---|---|
| `refute_rate` | refuted ÷ 实际复核数（已在 RunMeta，本批填值） |
| `verify_triggered` / `verify_skipped` | 触发数 / 跳过数（跳过按 agreement / carried / disabled 分类） |
| `verify_refuted` | 被驳条数（计数剔除的那批，报告头条与 stats 之差由它解释） |
| `verify_tail_ms` | 队列排空 → 复核归零（DeepMeta 已占位） |

## 10. 测试形状

| 用例 | 构造 | 断言 |
|---|---|---|
| 盖章收紧 | 一项两条疑点（peak 簇在 10-12 行），reviewer 产两条 finding（11 行 / 40 行） | 11 行那条盖 2、40 行那条盖 0 |
| 触发判定表 | severity × confidence 网格 | 与 `compute_verdict` 的 blocking 集合逐条一致 |
| 跳过三类 | agreement ≥ 2 / carried / enabled=false | 各自跳过且 `skipped_reason` 分类正确 |
| 异族选择 | verifiers 空 + 多家族 / 全同族 | 选异族；全同族 → 同族 + warning |
| 流式链路 | Fake：finding 落地 → 复核 → REFUTED | confidence 归零、不进 blocking、verifier 格记录落盘 |
| 失败保守 | verifier 调用失败 | upheld + warning，不吞 finding |
| barrier | 复核在途未归零 | 不进 reduce；`verify_tail_ms > 0` |
| resume 不重验 | 快照里已有 verifier 格记录 | 按 id 回填、零新调用 |
| 判决合并 | (UPHELD, REFUTED) / (None, REFUTED) / (REFUTED, REFUTED) | 依次 UPHELD / REFUTED / REFUTED；REFUTED 时 confidence = 0 |
| 计数剔除 | 3 条 finding 中 1 条 refuted | stats 与顶层计数为 2，findings 数组仍 3 条 |
| 旁路复位 | 首试断流半截 JSON + 重试完整输出 | 不拼接、不发草稿对象 |
| 退化零差 | 单成员门宽 1、无 verifier | 与 C3 基线逐字同构 |

## 11. 兼容与回滚

- schema v4 不变：`CellOutput::Verify` / `VerifyEntry` / `Finding.verify` / `Phase::Verify` / `refute_rate` 全是既有形状，本批只是首个消费者。
- 旧作业 resume：无 verifier 记录 → 按未复核处理（触发则复核一次），不报错。
- `enabled = false`（若基线不达标即为默认）→ 整段行为等价于批 C3 收口态，回滚成本 = 改一个默认值。
- 回滚 = 不合回；合回后 revert 该 merge。

## 12. 定案记录（2026-09-21，`/grilling` 十一问）

| # | 问题 | 决定 |
|---|---|---|
| Q1 | 交付边界 | 照 plan D-1…D-5，含离线基线与开关裁定 |
| Q2 | 「reviewer 确认」的可计算定义 | 盖章收紧：行范围压在 peak 簇上才盖 `suspicion_agreement` |
| Q3 | 流式旁路跨重试 | `attempt_begin` 复位解析器，落 D-1 第一步 |
| Q4 | 区域轴合并 | 维持 max |
| Q5 | 基线 verifier | 异族（deepseek-flash）与同族（glm-5.3）各一轮对照 |
| Q6 | 验收门槛 | 预登记：误驳 ≤ 5% ∧ 驳回 ≥ 30% → 默认开，否则默认关 |
| Q7 | 落盘粒度 | 每条 finding 一条 verifier 格记录，resume 按 id 回填 |
| Q8 | 接线形态 | 挂进深挖主循环，复用等门 FIFO；barrier 增复核在途 |
| Q9 | 计数口径 | refuted 留记录、剔计数，RunMeta 另报 |
| Q10 | 拆分 | 五步（旁路复位并进 D-1） |
| Q11 | 判决合并 | UPHELD 优先 + 按 finding id 缓存；REFUTED 时 confidence 归零 |
