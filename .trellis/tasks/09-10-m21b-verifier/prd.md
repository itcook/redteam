# M21b 批D：verifier 流式段——角色尾拆分 / 触发跳过选择 / 流式消费 / 离线基线定去留

## Goal

给 finding 加一道**怀疑者复核**：blocking finding 一落地就交给（尽量异族的）verifier「找出它不成立的证据」，
`REFUTED` 的 confidence 归零、永不 blocking，`UPHELD` 原样。复核与深挖尾段重叠，不占额外墙钟段。

**本批的真正交付物是「一个能被评估的 verifier」**，不是「一个开着的 verifier」：design §8.1 写的是「去留由数据决定」，
所以离线二分类基线（111 条台账夹具）与 `verifiers.enabled` 的默认值**一并在本批定死**，阈值先登记、后跑数。

**定案**：2026-09-21 `/grilling` 十一问（Q1–Q11），结论见 §Requirements 与 design §11；权衡与被否见 decisions **D17**（本批新增节）。

## Background（已核实，2026-09-21 · HEAD `2a454d6`）

- **前提**：批 C3 已合回（merge `2a454d6`），plan 批 D 节写的前提「C3 已合回」成立。集成分支四绿（lib 786 / e2e 17 / cli 17 / packaging 7；
  clippy 7 = 基线；`make verify` 绿）。M21b 六批 A / B / C / C1 / C2 / C3 全部合回。
- **`verify.rs` 在 M21 路径上完全没接线**：`verify_findings` 只剩 `pub use` 与自测在引用，签名仍是 pre-M21 的 `&dyn ReviewProvider`，
  不认 `ProviderRegistry`。故本批是**照新架构重写**，可复用的零件是触发集判定、`matches_precedent`、`source_snippet`、verdict 解析。
- **落盘形状早已占位、零消费者**：`CellOutput::Verify { verdicts: Vec<VerifyEntry> }`、`VerifyEntry { id, verdict, note }`（按 finding id 索引）、
  `Phase::Verify`、`RunMeta.refute_rate`、`DeepMeta.verify_tail_ms`（注释写明「批 D 前恒 0」）、`Finding.verify`。schema v4 不用动。
- **触发集实测比预期小**（缺省 `blockThreshold = medium`、`confFloor = 0.5`）：

  | 轮 | 报告 findings | 触发集 | 现口径可跳过 | 实际复核 |
  |---|---|---|---|---|
  | C3 改后 ① / ② / ③ | 16 / 19 / 15 | 10 / 6 / 9 | 3 / 1 / 3 | 7 / 5 / 6 |
  | C3 验证轮 | 13 | 5 | 1 | 4 |

  **每轮实际复核 4–7 条**，单条调用又短（finding + ±60 行快照）→ 不需要「每轮复核上限」这个旋钮。
- **现配置下 verifier 与 reviewer 同族**：`glm-5.3` 同时挂 `roles.reviewer` 与 `roles.verifier`，且是唯一 reviewer；
  按 §8.1「verifiers 活跃集非空则从中选」，默认路径就是 **glm 复核 glm 自己的 finding**。`deepseek-flash` / `qwen3.8-flash`（scanner 档）天然异族。
- **`suspicion_agreement` 是按格盖章**：`EmitCtx` 拿队列项派发时的家族数，盖给该 cell 的**每一条** finding，不问它落在块里哪儿。
  C3 把项放大成整个 def / 标题节之后，这个口径明显偏松。
- **`dedup::merge_into` 完全不碰 `verify`**：合并时 base 的判决胜出、other 的被静默丢；且 confidence 取 max 会与「refuted 归零」打架。
  pre-M21 无害（那时复核在 dedup 之后），流式之后是真缺口。
- **流式旁路跨重试不复位**（followup 标「批 D 接真 sink 前须处理」）：`timed_turn_with` 用 `retry_classified_with` 包 `backend.turn`，
  而 `IncrementalFindings` 每格建一次、跨尝试复用——断流那次的半截 JSON 会与重试输出拼进同一解析器。
  D13 留的 `AttemptPolicy::attempt_begin()` 钩子现成，可挂复位。
- **离线夹具现成**：`eval/fixtures/ledger-dismissed.jsonl` 63 条 + `ledger-upheld.jsonl` 48 条 = 111 条；`eval/verify_score.py` **尚不存在**。
  夹具里的 dismissed 多是「刻意如此」类判断（例：「壳层 commands/*.md 刻意薄透传」），verifier 只拿 finding + 快照 + spec，未必看得到这类仓库约定。

## Requirements（定案）

1. **交付边界**（Q1）：照 plan 做完 D-1…D-5，**含**离线二分类基线与 `verifiers.enabled` 默认值的裁定。
2. **跳过规则的「reviewer 确认」= 盖章收紧**（Q2）：只给「行范围与该项某条成员疑点重叠（±`LINE_GAP`）」的 finding 盖
   `suspicion_agreement`，其余盖 0；跳过判定仍是 `≥ 2`。一处改同时办两件事——给「确认」一个可计算定义，并修掉 C3 放大块后的盖章过宽。
3. **流式旁路复位**（Q3）：包一层委托 `AttemptPolicy`，在 `attempt_begin` 复位 `IncrementalFindings`；重复对象由 `EmitCtx` 的 id 去重挡住。
   落在 D-1 的第一步，**先于**任何真 sink 接线。
4. **区域轴合并维持 max**（Q4）：`suspicion_agreement` 是「这个 bug 的位置被几个家族标过」的区域属性，现实现与用例不动。
5. **判决合并 = UPHELD 优先 + 按 id 缓存**（Q11）：复核结果按 finding id 缓存（同 id 不重验，与逐条 verifier 格记录同一份数据）；
   `merge_into` 补 verify 合并——**任一 UPHELD 即 UPHELD**、base 无判决时继承 other；合并后若为 `REFUTED` 则 confidence 统一归零。
   方向与验收门槛一致：误驳比漏驳危险。
6. **落盘粒度 = 每条 finding 一条 verifier 格记录**（Q7）：cell id `verify[<finding id>]@<model>`、`CellOutput::Verify` 带单条 `VerifyEntry`；
   `append_partial` 本就是按 cell id 的 upsert；resume 按 finding id 回填、已复核的不重跑。
7. **接线形态 = 挂进深挖主循环**（Q8）：复核 future 进同一个 `FuturesUnordered`，复用 C2 造好的等门 FIFO / 阻塞计时 / 取消安全；
   `empty_for_barrier` 增「复核在途 0」；`verify_tail_ms` = 队列排空到复核归零那段。
8. **计数口径**（Q9）：refuted 保留在 `findings` 数组（已有 `refuted` 标注）供审计与 M21c 台账回流，但 **stats 与顶层计数剔除**，
   RunMeta 另报被驳条数。理由：verdict 已经不认它们，头条数字不该认。
9. **触发集消费 `--block-threshold` / `--conf-floor`**（缺省 medium / 0.5）——M21a 起的两个「无消费者旗」自此有消费者，与 verdict 同源。
10. **verifier 选择**（design §8.1 不改）：verifiers 活跃集非空则从中选；缺省自动异族（family 缺省取 model_id 首段），优先 reviewers 档 → scanners 档；
    无异族 → 同族 + warning「无异族 verifier」；`review.verifiers.enabled = false` → 全跳过并记理由。
11. **失败保守 upheld + warning**；verifier 不入队新项（§6 规则 8）；判例变体触发槽留空（M21c 台账起）。

## Acceptance Criteria

- **预登记的开关裁定**（Q6，阈值先于跑数定死）：111 条夹具上
  **误驳率**（48 条 upheld 被判 `REFUTED` 的比例）**≤ 5%** 且 **驳回率**（63 条 dismissed 被判 `REFUTED` 的比例）**≥ 30%**
  → `verifiers.enabled` 默认 `true`；否则默认 `false`（旗仍在、可手动开）。不设中间态。
- **基线两轮对照**（Q5）：异族（`deepseek-flash`）与同族（`glm-5.3`）各跑一轮全量夹具，数字都写进私有记录与 followup。
- **判定表**：触发 / 跳过逐条过；全同族构造 → 同族 + warning；`enabled = false` → 全跳过且记理由。
- **Fake e2e**：finding 落地 → 复核 → refute 链路；调用失败 → 保守 upheld；barrier 等复核在途归零。
- **回归哨**：scanner 与深挖路径零行为 diff（退化零差基线同构）；共享头字节稳定 + analysis 先于 verdict 的段序断言；
  四绿（lib / e2e / cli / packaging）；clippy 不超基线 7；`make verify` 绿；`eval/risk_replay.py` H0 对拍逐字不变。
- **dogfood**：同 C3 口径跑一轮（门 6、samples 1、同 diff），复核段不得让 findings 低于 C3 护栏（格内 ≥ 11），
  `refute_rate` / 触发数 / 跳过数 / `verify_tail_ms` 四个读数落私有记录。

## Out of Scope

- 判例台账与判例变体触发（M21c）：本批只留空槽，形状到位。
- 变更集外 needs 的装包归属（C3 留账）→ C4 / M21d。
- `agreement`（模型数轴）legacy 化：**保留不动**，schema 稳定、carry 与旧报告仍读。
- md 报告展示 `suspicion_agreement`：不加，它是内部机制；读者看到的是 `refuted` / `upheld` 标注。
- 每轮复核上限旋钮：实测 4–7 条/轮，数据否掉。
- verifier 的多轮工具循环 / 自主探查：不做，复核是单发判断题。

## Technical Notes

- **风险文件**：`src/consolidate/verify.rs`（整体重写）、`src/orchestrate/deep.rs`（主循环加复核分支 + barrier + 尾段计时）、
  `src/orchestrate/review_shot.rs`（盖章收紧 + 旁路复位）、`src/consolidate/dedup.rs`（verify 合并——本批唯一动 dedup 处）、
  `src/prompt/`（verifier.md + `build_verify_prompt` + 模板哈希）。
- **拆分**（五步，每步一 commit，先红后绿 + 变异检查）：
  - **D-1** 旁路复位 + `verifier.md` 角色尾拆分 + `build_verify_prompt` + 模板哈希。
  - **D-2** 触发集 + 盖章收紧的跳过规则 + 异族选择（纯判定层，零 provider 调用）。
  - **D-3** 流式消费接线：深挖主循环、逐条 verifier 格记录、barrier 与 `verify_tail_ms`、`merge_into` 的 verify 合并、计数口径。
  - **D-4** `eval/verify_score.py` + 异族 / 同族两轮基线 → 按预登记阈值裁定 `enabled` 默认值。
  - **D-5** 收口：四绿 + dogfood + `/code-review` 分档修 + `--no-ff` 合回 + 私有记录与 followup。
- **形态**：分支 `feat/m21b-verifier`（自 `feat/m21-review-engine` 切）、工作树 `~/hobby/.worktrees/redteam/m21b-verifier`。
- **收口用 `/code-review`**，不跑 ship（M21 期间的约定；plan D-5 的「ship 评审」措辞随本批作废）。
