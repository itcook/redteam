# M21c · 台账·收敛·默认翻转

> 状态: ⏳ · plan `../plans/2026-09-23-redteam-m21c-ledger-convergence-flip-plan.md`（批 0 + 四批，2026-09-23 派生）
> · 依赖 M21b ✅ · design §8 / §4 判例前移 / §9 L2 重定位 · 决策 D6, **D19**
> 出口：ship 内循环 dogfood 通过；收敛三维与 coverage 五集合语义落地。
> **切名不再是出口而是入口**〔D19-1〕：`redteam-next` → `redteam` 提到批 0 先做——日常 dogfood 产生的数据
> 比继续打 PR181 这个固定靶子有价值（其 oracle 有 9 条取自 dev 自己的产出），且 dev 二进制保留、回退只需改插件 bin 指向。

## 目标

让预算截断从「漏」变「延迟到下一轮」，让收敛与 coverage 在新形态下仍有确定性语义，然后翻转默认。

## 交付项

| # | 项 | design | 落点 |
|---|---|---|---|
| 1 ✅ | 疑点台账表（proj.db，镜像 DismissCard / ResolveCard）：预算外疑点落账（quote 锚 + hint + confidence + origin + 落入原因）；报告披露为低置信线索；**不进 verdict** | §8.2 | `src/db.rs`, `src/output` |
| 2 ✅ | 增量轮激活（**挂全量轮**，`--incremental-from` 仍 bail〔裁定 1，2026-10-02〕）：`delta ∩ 锚定文件 ≠ ∅` → 重入队优先级 +1 档；carry 机器（anchor_near / rename 迁移）复用；注入措辞照抄 claimed-resolved 卡；preflight 守卫扩展（上轮 Incomplete → 台账疑点不可信） | §8.2 | `src/cli/incremental.rs`, `src/consolidate/carry.rs` |
| 3 ✅ | 收敛三维（无 blocking ∧ 台账无 delta 邻接未处理项 ∧ 无激活超限）+ 活锁守卫（**两段式 N=3 强制必挖 / N=5 停车**〔裁定 4〕）；ship 协议消费新 RunMeta 字段 | §8.3 | `src/verdict.rs` 不动；RunMeta；`plugins/redteam/commands/ship.md` + `make gen` |
| 4 ✅ | coverage 五集合（selected / completed / reused / failed / waived，per cell 类型）+ 角色分级：快扫失败 → 硬 Incomplete；深挖截断/失败 → 带债 approve | §8.4 | `src/orchestrate` CellSink.manifest, `src/cli/incremental.rs` preflight |
| 5 ✅ | 判例前移进快扫 prompt：同文件活跃判例摘要；机械匹配命中 → **跨轮沿用结案带守卫**（锚定可重定位 ∧ 区域自判例以来无 delta → dismissed 豁免不入队 / upheld carry 不重挖；守卫不过 → 入队带判例笔记）〔D13，替代「降级候选」〕 | §4, §6 规则 2 | `src/prompt/mod.rs` render_precedents 复用；`src/consolidate/verify.rs` matches_precedent 前移调用点 |
| 6 ✅ | 小项打包（①③ 核实后发现早已交付，本批只做②）：refute prompt `analysis` 先于 verdict；positioning 跨文件重定位（changed files 内唯一命中，多/零命中拒绝）；`scanned_areas` 消费进 coverage | §8.1, §9 L2 | `src/prompt/templates/adversarial-refute.md`, `src/positioning` |
| 7 | ~~**切名前 aacr `smoke`**~~ → **移出本里程碑〔D19-2〕**：降为可观测、推迟 M21e；切名门改用仓内口径（PR181 一轮机械面复现 + 全量测试 + `make verify`）。原文：（D7 B4 重启点）：fork 上 5 个 rs / go / ts 正样本，legacy 与 v2 各 N=1，`JUDGE_USE_MOCK=true` 取 Line-P / Line-R；结果写回 research；`reviewers/redteam.py` 适配器与 pipeline 挂载随本项一并接线（本仓 `eval/README` + `AACR_BENCH_DIR`） | design §16 | `~/codes/aacr-bench`；本仓 `eval/` |
| 8 | **切名**（D10）：`redteam-next` → `redteam`，`make build` 放置新二进制，插件与 ship 指向它；dev 二进制归档为对照基线（R6 销案〔D11〕：对照 = 并排跑 PR181 比 oracle 命中）；`--help` / README 初稿按新 CLI 面；**库切换程序**（followup 分库裁定）：导出旧 `proj.db` 固定集 → 删 → `proj-next.db` 回名 `proj.db`、`--dismissed` 指向旧库的只读复用一并议；ship.md 第 2 步改调 `redteam test-command`、删 `--models` 透传与旧路径引用；并存期 secrets 提示收敛再议 | 铁律 6 | Makefile, `plugins/redteam/`, `src/cli`, `src/db.rs` |
| 9 | dogfood 内循环：ship r1-rN 至 approve；发现修复回流；台账激活至少观测到一例 | — | — |

## 批序变更（2026-10-02）

摸台账发现 **批 B 的前提读错了机制**，故换序为**先 C 后 B**：

| | 素材 | 存量 | 结论 |
|---|---|---|---|
| 批 B（判例前移） | `dismissed` / `resolved` 判例卡 | 新库 **0 / 0**；旧库 63 / 3，其中仅 22 条带 quote 锚、**实测只有 9 条还能命中当前代码** | 判例只由 ship 循环的人工裁定产生，等不来。但〔D19-5〕早已把验收改成构造用例，**喂料不是开工前提**——10-02 当天开工并交付，喂料改从新鲜 finding 的误报建卡 |
| 批 C（增量激活） | `suspicions` 休眠疑点 | 本仓 27 + PR181 74，全 `dormant`、落入原因 100% `budget` | 够起步；本仓实测出现过跨轮项（第二轮 10 条披露里 5 条来自上轮 HEAD） |

批 B 的喂料最终取「从新鲜 finding 的误报建卡」：旧库那 9 条是 M18 时代针对旧代码的判断，不值一条新读库路径（D19-4 议题就此搁置）。
**「记账不修」不等于误报**——拿优先级低的真问题建判例卡会教坏评审器。批 B 建的第一张卡是一条
因切片折叠导致的真误报（reviewer 自己写明了不确定却选择猜而不是声明 need）。

同时发现的两处口径问题已随批 C 修：披露段把本轮刚截断的项当「历史线索」列出（实测 4 轮 164 条里 159 条是本轮自产）；
`ledger.rs` 关于「needs 项不会落账」的解释与实测矛盾。

## 验收

- 三轮增量 dogfood：≥ 1 例休眠疑点被 delta 激活并晋升为 finding；收敛判定与 exit code 五档契约一致。
- coverage 五集合不相交并 = 全集；人工 kill 一个快扫 cell → verdict Incomplete；kill 一个 deep cell → 带债 approve + warning。
- 判例前移后〔D19-5 改口径〕：主判据 = 两条构造用例（已 dismissed 变体在快扫层不产疑点或入队被豁免；判例区域有 delta → 必须重挖）；
  观测量 = 判例命中导致的免入队 / 免重挖项数（新 RunMeta 字段）+ 同一 PR 连续两轮重复报同一 finding 的比例。
  ~~refute 调用数较 M21b 下降可观测~~ 撤销：M21b 三轮触发 24/22/24、驳回 0/2/1，基数太小，「下降」落在噪声里。
- ~~aacr `smoke` 硬门~~〔D19-2 撤销，移 M21e〕→ 切名门 = PR181 一轮机械面复现（失败格 / 截断 / 解析失败皆 0）+ 全量测试绿 + `make verify` 绿。
- 切名后全量测试绿；dev 二进制 PR181 基线数字已归档；`make verify` 绿（commands 改动随 gen）。

## 风险

- 台账激活的「+1 档」若把大量低价值疑点顶到必挖集边缘 → 以激活计数与 N 上限（R12）封顶。
- 跨轮沿用结案若守卫过松会盖住 fix 引入的新 bug：守卫要求区域无 delta，有 delta 一律重挖；refute 仍是终审。
