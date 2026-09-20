# M21b 批C3：深挖选项稳定性——结构化成区与合并 / 两轴共识 / 分层排序与文件多样性 / needs 入队

## Goal

同一份 diff 跑两次，深挖挖的应当基本是同一批区域。现状不是：实测逐项重合度 **25%**、findings 重合度 19%，而单文件能独占近半预算。
本批只治「分配」层——**项身份从「报告行」改为「代码结构」**（enclosing def / 文档标题节），共识拆成「必挖轴」与「排序轴」两轴，
排序改分层键并接上阶段 0 已算好的风险分，再给每个候选文件保底一格；reviewer 笔记撤掉无信息量的 confidence 数字；
scanner needs 作为 design §6 的第三种生产者首次接线（仅变更集内）。

**目标函数（2026-09-20 定案 Q1）**：主目标是稳定性（可复现 = 可回归、可归因），产出只设护栏——不要求 findings 变多，但不许变少。
理由：单轮 findings 数实测在 6 / 9 / 11 / 12 之间跳，噪声大于信号，不能当主指标；而「这轮没挖到的下轮也未必挖」意味着覆盖靠运气，
与 M21 的确定性地板冲突。

**定案**：2026-09-20 `/grilling` 十二问（Q1–Q12），技术设计与任务清单在 compact 后补 `design.md` / `implement.md`。

## Background（已核实，2026-09-20 · HEAD `df17467`）

- **前提**：C2 已合回（merge `3038ee7`、归档 `fffd595`）；集成分支四绿（lib 742 / e2e 17 / cli 17 / packaging 7；clippy 7 = 基线；`make verify` 绿）。
  无并行批次占用本批文件域；分支与 Trellis 任务名未被占用。
- **改前基线对**（同二进制 `fffd595`、门 6、samples 1、同 diff `674583e...f1d3ca3`，2026-09-20 两轮；明细在 `.next/private/m21b/2026-09-20-batch-c3-baseline-pair.md`）：

  | 层 | 读数 |
  |---|---|
  | 候选集（scanner 疑点位置） | 32% |
  | **选中项（逐项）** | **25%** |
  | 选中项（按文件） | 57% |
  | findings（`file:line`） | 19% |
  | 单文件最大占格 | 6/13、6/12 |
  | 格内 findings / 代码格占比 | 11、15 / 7-of-13、10-of-12 |

- **排序现状**：`priority_score(疑点) = max(confidence) × agreement × 类别权重`，而 `CATEGORY_WEIGHT = [1.0; 6]` 是空操作 →
  实际等价于「confidence 排序」。`cmp_priority` 的末位是 id 字典序，`.next/` 恒排在 `src/` 前。
- **confidence 不可跨轮比**：两轮分布整体平移（min/中位/max = 0.15/0.35/0.55 与 0.20/0.45/0.82）。E3 实证 flash 级自评置信与命中无关。
  design §7 明写疑点以「一位评审者的笔记」注入、指令是「确认、驳回或补充」而非「验证以下」，括注理由即**防锚定**——笔记里的数字与此冲突。
- **规则 2 从未实现**：design §6 规则 2 的「区间扩到 enclosing def、同文件重叠/相邻区间合并为一项」一行没写，`same_item` 只做了 quote 锚那一半。
  零件现成：`risk/regions.rs` 的 `zones_for_file` / `enclosing_decl` / `decl_block_end`（H0 成区就是「hunk → enclosing def 块，无 def → ±10 行」）。
- **行跨度合并会把身份搞不稳**：C1 离线模拟里「相邻 W 行合并 + 代码优先 + 按文件轮转」让重合度从 25% 掉到 21% / 15% / 13%（W = 0 / 10 / 30）；
  今天按文件粒度算出的 57% 则是「合并到极致」的上界。两个数一起说明：**合并有效，但身份必须锚在结构上**。
- **区域级共识会撑爆必挖集**：现状每轮 `agreement ≥ 2` 的疑点只有 4 条；若按「区域里出现过的家族并集」算，两轮分别有 7/9、7/10 个文件 ≥ 2 家族，
  PR181 规模 18/23。必挖不受预算截断 → `maxCells` 形同虚设。
- **samples ≥ 2 不抬共识**（followup 旧括注有误）：`agreement = 去重家族数`，D7 明确「同家族多 samples 计一次」。samples 的作用只是扩候选覆盖；
  成本是 scanner token 翻倍（现占全轮 48% in / 40% out），墙钟基本不变（8 格仍全并行，门 16/20）。配置里没有 temperature 旋钮。
- **needs 是最稳的信号**：两轮唯一路径 7 vs 8、**Jaccard 67%**，全部 `kind = evidence` 且全部带 lines；但约六成指向变更集之外
  （`verify.rs`、`deep.rs`、`queue.rs`、`stream_json.rs`、`state/mod.rs`）。`pick_bundle` 对不在任何 bundle 的文件会静默退回 `bundles[0]`，
  会产出「文件名是 A、证据是 B」的错配包。
- **类别标注本身在漂**：L0 占比两轮 59% vs 21%。

## Requirements（定案）

1. **项身份 = enclosing def 边界**（Q3）：疑点落在哪个 def 块就成那一项，区间 = def 块行范围；复用 `risk/regions.rs` 的成区逻辑
   （`enclosing_decl` / `decl_block_end` 提 `pub(crate)`，**逻辑不动**）。解析不出 def 的文档类退到「最近标题行 → 下一同级标题」的标题节。
2. **合并谓词改结构化**（Q10）：`same_item(Scan|Needs)` = 同文件 ∧ 同 def 块；`same_suspicion`（quote 相似）降级为**项内聚簇**依据；
   取消 id 捷径特例。C1 决策「共识只经谓词、id 捷径不计共识」在 design / decisions 改写为「**结构定项、谓词定共识**」，注明 C3 起生效与理由。
3. **共识两轴分离**（Q4）：项内按 `same_suspicion` 聚簇 → `peak_agreement` = 最大簇的家族数，**必挖 = `peak_agreement ≥ 2`**（语义与 D7 一致、不膨胀）；
   区域级家族数与项内疑点条数只进排序。C2 已存的 `QueueItem.members` 即聚簇输入，不新增存储。
4. **分层排序键**（Q5）：层内从高到低 —— `peak_agreement` 降序 → 所在风险区分数**分档**（`RiskRoute.zones`，已传入 `run_deep` 未用；粗分档避免小数抖动）
   → 区域内疑点条数降序 → 代码优先于文档 → confidence 降序（**末位 tie-break**）→ id。类别权重维持 1.0（不启用）。
5. **文件多样性**（Q6）：每个候选文件先保底一格（排序键前置一层「文件内序号 == 1」），其余按上条分层键排。
6. **reviewer 笔记去 confidence**（Q7）：`render_notes` 只留 `file:lines — hint — quote`。连带退化零差基线重采，commit 写明原因（prompt 字节与模板哈希会变）。
7. **scanner needs 入队**（Q8）：仅 `path ∈ 变更集` 的 needs 入队（`ItemKind::Needs`，层级仍在疑点之后）；变更集外的照旧只记录并计数。
   diff 外 needs 的装包归属另记账给 C4 / M21d。
8. **观测**（Q11）：`run.deep` 增五个标量 `max_cells_per_file` / `files_selected` / `files_candidate` / `items_from_suspicions` / `suspicions_arrived`
   （serde default，不破 schema v4）；`enqueue_reasons` 增 `needs` 与 `needs_out_of_diff` 两个 key。逐格数组不加（可从 `DeepRun.items` + scan 报告离线复算）。

## Acceptance Criteria

- **主门槛**：同 diff 两跑（同二进制、门 6、samples 1、`674583e...f1d3ca3`）选中项**逐项重合度 ≥ 40%**（改前 25%）。
- **辅助门槛**：**单文件最大占格 ≤ 总格数的 1/3**（改前 6/13 ≈ 46%、6/12 = 50%）——直接对应文件多样性机制是否奏效，且不受候选集波动影响。
- **护栏**（Q1）：格内 findings 合计 **≥ 11**（改前两轮 11 / 15 的下界）且**代码格占比 ≥ 68%**（改前均值）。
- **边界带**：主门槛落在 30%–50% 时加跑第三轮，取三对（1-2 / 1-3 / 2-3）中位数再判。
- **回归哨**：`H0 成区行为逐字不变`（`eval/risk_replay.py` 对拍）；lib / e2e / cli / packaging 四绿；clippy 不超基线 7；`make verify` 绿。
- **退化零差基线**：因笔记去 confidence 而重采，commit 说明原因；其余差异需逐条解释。
- **度量口径**：`.next/private/m21b/raw/selection_overlap.py` 三层 Jaccard；**findings 按 id 的重合度不作判定依据**（id 哈希含 title，跨轮措辞即变，实测恒 0%）。
- **一次性测量轮**（Q2）：samples = 2 跑一轮（不进产品配置），量候选集覆盖与文档洪水的构成，数据进 followup 供后批决策。

## Out of Scope

- 候选集源头手段产品化：scanner `samples ≥ 2`、temperature 旋钮（无配置面）——只做一次性测量轮。
- 变更集外 needs 的装包归属与读守卫口径 → C4 / M21d。
- 类别权重启用（标注自身在漂）。
- resume「总是复位 Skipped」的简化（C2 评审结论：与门控等价，维持 D15）。
- 快照批量单事务、手写 FIFO 门改 tokio `Semaphore`、组内闭包并查集重复计算（C2 评审帽外项，留 followup）。
- verifier / 批 D 的任何接线；`run_deep` 的 sink 类型不动。

## Technical Notes

- **风险文件**：`queue.rs`（合并谓词 + 排序键 + 两轴共识）、`pack/region.rs`（成区对齐 + 笔记渲染）、`risk/regions.rs`（**只提可见性**）、
  `deep.rs`（多样性层与观测）、`orchestrate/mod.rs`（needs 入队）。
- **H0 不受影响**：H0 项仍走 `route.hotspots`，成区逻辑共用但行为不变——`eval/risk_replay.py` 对拍是第一道哨。
- **与 C2 语义的衔接**：到达键（含内容指纹）、`members`、往轮在途占位、收场 drain 一次、`budget_skipped` 按最终状态——均不变；
  结构化合并只改「哪些疑点进同一项」，不改入口五态与幂等。
- **拆分**（四步，每步一 commit，先红后绿 + 变异检查）：
  - **C3-1** 结构化成区与合并：def 对齐、文档标题节、`same_item` 改结构化、id 形态、项内聚簇算两轴、取消 id 捷径。
  - **C3-2** 排序与多样性：分层排序键、风险分接 `zones`、每文件保底一格、笔记去 confidence、基线重采。
  - **C3-3** needs 入队（变更集内）+ 五个观测标量与两个 `enqueue_reasons` key。
  - **C3-4** 收口：两轮 dogfood + 三量门槛判定（必要时第三轮）、samples 2 测量轮、`/code-review` 分档修、`--no-ff` 合回、私有记录与 followup。
- **形态**：分支 `feat/m21b-deep-stability`（自 `feat/m21-review-engine` 切）、工作树 `~/hobby/.worktrees/redteam/m21b-deep-stability`。
- **compact 后补**：`design.md`（把上述定案展开为技术设计，含排序键伪码、标题节切分规则、needs 项的 pack 归属、观测字段定义）与 `implement.md`（C3-1…C3-4 任务清单）。
