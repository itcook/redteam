# M21c 批 A：台账落账与 coverage 分级

> 上位：plan `.next/plans/2026-09-23-redteam-m21c-ledger-convergence-flip-plan.md` 批 A · roadmap 项 1 / 4 · design §8.2 / §8.4 · 决策 D19
> 前提：批 0 已合回（`8b9d82d`），插件与 ship 跑新引擎；库已是 `proj.db`（v4）。

## 开工调研（2026-09-28，改了两处对范围的理解）

1. **`suspicions` 表已建未写**（M21a 就留好了壳）：`id / file / quote_norm / hint / status / first_job / last_job / activations / json`。
2. **预算截断项的明细已经在库里**：队列项状态转换经 `state::put_queue_item` → `db::queue_item_put` 落 `queue_items` 表
   （state=`skipped`，json 是完整 `QueueItem`）。所以 A-1 不是「从头记录」，而是**把按作业的快照提炼成跨轮的台账**
   （queue_items 按 job 增长、是 resume 的工作区；suspicions 以 quote 锚为身份、跨轮累积）。
3. **`waived` 的语义澄清**（design §8.4 写「per cell 类型」）：预算截断的项**没有 cell**，它的去处是
   「台账化（A-1）+ RunMeta 披露（`budget_skipped` 已有）+ warning（已有）」，**不是** waived。
   自洽的不变式是 **selected = completed ⊎ reused ⊎ failed ⊎ waived**（每角色、两两不相交）。
   waived 当前**无来源**，要到批 B 的判例豁免（dismissed 变体不入队 / upheld carry 不重挖）才有内容——
   本批把语义写进代码注释与用例，避免后人误填。
4. **发现一处真实不一致**（A-3 要收）：scanner 侧分桶是「失败优先」（`ok && reused` → reused、`ok && !reused` → completed、
   失败 → failed），而 `fold_role_cells`（reviewer / verifier）是「复用优先」（`(true, _)` → reused）——
   失败的复用格在两处会落进不同的桶，后者会把失败静默记成 reused。

## Task

### A-1 预算外疑点落账（roadmap 项 1 前半）
- 深挖收场后，把 `ItemState::Skipped` 的队列项 upsert 进 `suspicions`：身份 = 文件 + quote 归一（与
  `same_suspicion` 的 quote 归一化同源，不另立口径）；带 hint / confidence / origin（scan / rule / needs）/
  **落入原因**（预算截断 / 规则豁免）/ first_job / last_job；`status = dormant`、`activations = 0`（激活是批 C 的事）。
- 幂等：同一疑点跨轮只有一行，`last_job` 更新、`first_job` 不动。
- 写失败不败 job（与队列快照同口径：warning）。

### A-2 报告披露低置信线索（roadmap 项 1 后半）
- 报告新增段：台账里与本轮变更集**锚定相邻**的休眠疑点，标为低置信线索；**不进 verdict、不计入 findings 数、不影响退出码**。
- 验收用例：披露段存在且 verdict / exit code 与不披露时逐字节相同。

### A-3 coverage 不变式与分桶口径统一（roadmap 项 4）
- 统一两处分桶为**失败优先**（失败的复用格属于 failed，不该被记成 reused）。
- 钉住不变式 `selected = completed ⊎ reused ⊎ failed ⊎ waived`（三个角色各一条用例，含空集情形）。
- 角色分级已在（scanner 失败 → 硬 Incomplete；深挖失败 / 截断 → 带债 approve；verifier 失败 → 保守 upheld），
  本批只补**用例**把它钉住，不改行为。
- `waived` 语义写进注释：当前无来源，批 B 判例豁免才填。

### A-4 resume 账目重建（followup 债）
- 续跑收口时按 `cells` 里的记录重建复核四量与 `close_ms`（口径与正常路径同源），或显式标「续跑不可重建」而非静默 null。
- 实测背景（M21b 第 6 项）：复用格不入深挖计数，极端情况全空。

### A-5 回归门
- PR181 一轮：台账落账不改变 verdict 与 findings 集合（只增披露）；数字进私有记录。

## 约束（继承 plan 的 Global Constraints）

每处行为改动配会转红的用例 + 变异检查；私有数据只落 `.next/private/`；`make verify` 绿；改代码走本分支、
合回前过 `/code-review`（批 0 的教训：3 条严重问题全是评审抓的，其中一条直接废掉 README 承诺的回退路径）。
**变异脚本本身也要被怀疑**（批 0 有 2 条假绿是脚本判据与范围写错造成的）。
