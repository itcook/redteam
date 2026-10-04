# 增量轮重建（独立单元）

## Goal

恢复 `run_incremental_review` 主流水线（现在第 18 行就 `bail!`），让 `--incremental-from` 可用。
与 ship 单元强耦合——ship 从 r2 起就用它，现在靠批 D 刚补的降级条目跑全量。

## 开工前的事实核查（修正了一个说重了的判断）

早先说「重建增量轮要先重裁『上轮 finding 要不要携带』」。**这个说重了**：

「要不要携带」不是开放问题，三处已锁定——design §8.2「carry 重锚定进增量轮」、
ship 协议「verdict 仍是合并集口径（携带 ∪ 新发）」、`resolve` 子命令语义
（「下一轮增量据此不再携带该 finding」）。而且**携带机制已存在且在跑**（为 resume 建的）：

| 零件 | 状态 |
|---|---|
| `DeepInput.prior_findings` | resume 在用（settled 项的 finding 并入、不双计） |
| `Origin::Carried` | positioning 认它（跨轮锚点会漂、不判远离） |
| `VerifyDecision::SkipCarried` | 复核认它 |
| `carry()` / `merge_carry()` / `migrate_renames()` / `migrate_resolve_cards()` | 四零件齐，250 行带测试 |
| `incremental_preflight` | 四守卫齐全，**已产出 `IncrementalBaseline { base_sha, prior_findings, renames, delta }`** |

**缺的只是 preflight 之后的接线。**

真正没定过的是**批 C / B 引入的新交叉**——M13 设计 carry 时台账与判例豁免都不存在。
四个交叉点调查结论：

| 交叉点 | 结论 |
|---|---|
| carry 与台账激活会不会重复 | **不会，天然不重叠**：台账只记 `ItemState::Skipped`（没挖成的），carry 只带挖出结论的；批 C 的 `promote_dug_suspicions` 又把挖出 finding 的激活项移出池 |
| 判例豁免对 carried 适用吗 | **碰不到**——豁免在入队侧（疑点层），carried 不入队。这条缺口是 `previously_dismissed` 的 writer 断了，**已于 2026-10-04 单独修掉**（`246f097`） |
| carried 的复核豁免有无守卫 | **没有** → 裁决点 1 |
| carried 进不进 coverage | resume 进 `reused`，carry 来自上一个 job 没有本轮 cell → 裁决点 2 |

## 两条裁定（2026-10-04 用户裁，均按推荐）

**裁定 1：carried finding 加「区域有 delta 就不 `SkipCarried`」守卫。**
与批 B 的判例守卫同构，判据直接复用 `precedent.rs` 的 hunk 相交。
我一度想否掉它（「verifier 缺省关、守卫没效果」），算清后发现那不是缺陷：
**verifier 关着时行为与今天完全一致**（携带且拦路，保守方向正确），**开着时才多一次复核、更准**。
纯增益、无回退风险。

被否的「区域有 delta 就不携带、退回队列重挖」：最彻底且不依赖 verifier，但与 carry 的初衷冲突——
carry 存在的全部意义就是「上轮发现的问题不因这轮没扫到而消失」，而 scanner 有随机性，
退回后未必重新报出来，那条 finding 可能直接丢失。

### 裁定 1 的前提被推翻 → **裁定 1′**（2026-10-04 同日，用户裁「c + 加第二条判据」）

裁定 1 默认「carried finding 会走到 `decide`」。实施时用例红在比断言更深的地方，查下来**前提不成立**：

| 事实 | 位置 |
|---|---|
| `decide` 唯一产品调用点在 landed 循环内，只遍历 `l.rec.output.findings()`（本轮格的输出） | `deep.rs:412` |
| carried 走另一条路：收场 `findings.extend(prior_findings)`，不经过 `decide` | `deep.rs:636` |
| `SkipCarried` 的全部引用只有 `verify.rs` 自己 + 自己的单测，**零产品调用点** | `grep -rn SkipCarried src/` |
| 它不是 M13 遗留，是 M21 复核设计（`8517784`，2026-09-21，D17 §8.1）引入的，**生下来就不可达** | `git log -S` |
| `--help` 一直列着 `carried` 这个跳过分类 | `main.rs:47` |

这是本单元第五次撞上「定义了却没人用」，且这次是按裁定 1 新加的守卫自己踩进去。
代码里早有自承（`deep.rs:633`）：「往轮**没验完**的本轮不补验……**没有格就没有派发点**，留待 M21c 一并处理」——
M21c 已交付，缺口还在。

**裁定 1′ = (c) 补派发挂点 + 加第二条判据。** 免验的理由是「出生轮已复核过」，它要同时成立两件事：

- **判据 ①（往轮判过）**：`f.verify.is_none()` = 出生轮压根没给出判决（复核被 kill、或那轮没开 verifier）。
  沿用一个不存在的判决 = 把「未复核」当「已复核」收。这条正是上面注释自承的缺口。
- **判据 ②（区域没动过）**：本轮 delta 碰到它所在区域 → 旧判决可能已不适用（原裁定 1 的内容）。

两条都成立才 `SkipCarried`；任一不成立就落到后续判定。

**派发挂点放主循环之前**（`deep.rs`，`loop {` 之上），不是收场批次——关键是查清了依赖都已就绪：

| 需要的 | 位置 | 状态 |
|---|---|---|
| `verifying` 池 / `verdicts` / `verify_seen` / `verify_rotation` | `deep.rs:236-241` | 主循环前已声明 |
| `env` / `members` / `hunks_by_file` | `229` / `217` / `170` | 都在循环前 |
| `cell_id` = `verify[{id}]@{model}` | `run_verify` 内 | 不依赖源格，无需造假 cell id |
| **收端回填** `apply_known(&mut prior_findings, &verdicts)` | `deep.rs:635` | **早已接通** |

最后一行是关键：判决回贴 carried 的线本来就在，缺的只有派发侧。所以这不是新建设施，是补 ~45 行。

**被否的 (b) 收场另起复核批次**：要重开池、重做 barrier 与 kill 语义，而 (c) 能做到的它没多做。
**被否的 (a) 删掉 `SkipCarried` 与守卫**：最省，但等于正式放弃 D17 §8.1，且留着 design 与代码背离；
真实后果是人改了那块代码、旧判决照样盖住新问题——正是 design §4 点名要防的漏报路径。

**附带收益（已确认，非副作用）**：resume 的 `prior_findings` 过同一道门——往轮已判的被 `known` 拦下记
`resumed`（不发请求），往轮没验完的这轮补验。首跑 `prior_findings` 为空，整段不执行、行为逐字节不变。

**连带清理**：landed 循环里按裁定 1 加的 `f_hunks` 实参成了死参数——那条路上的 `f` 恒 `Origin::New`
（`Origin::Carried` 的唯一产品写入点是 `carry.rs:81`），carried 分支进不去。

### 实施中发现：CLI 入口有一道无条件拒收（2026-10-04）

主流水线接通、全量测试绿之后，核 `--help` 时才看见 `main.rs` 里还有：

```rust
if args.incremental_from.is_some() {
    eprintln!("error: --incremental-from（增量轮）在 M21c 疑点台账回流后恢复；…");
    std::process::exit(1);
}
```

**裸终端用户根本进不到重建好的流水线。** 本单元其余 e2e 全走库函数 `run_review_dispatch`，
命令层这一段没有任何用例覆盖——这是本会话第五次「CLI 入口接线假绿」，也是最严重的一次：
整条功能的总开关。

处置：
- 改成只拦 `--background`（`run_review_dispatch` 的注释写明增量分流「前台与后台 worker 共用本单点」，
  后台本来就能走；现在拒的理由换成「preflight 拒跑会落成 Failed 作业，参数问题不该让用户去
  `status <id>` 才看见」，并记进 followup）。
- 补 e2e `cli_entry_lets_incremental_through_to_the_pipeline`：**用真二进制**跑
  `review --base main --incremental-from prior1`，断言 stderr 不再含那句过期文案、
  且走到了增量流水线自己的检查。判据选「错误不再是那句」而非「跑通」——跑通要配 provider
  和上一轮 job，成本远超这条要守的东西。
- `--help` 文案一并订正（CLAUDE.md：`--help` 是裸终端唯一的在场文档，与代码同权重）：
  原文还写着「M21a 硬切后**暂不可用**」。
- `plugins/redteam/commands/ship.md` 的守卫清单里「增量轮尚未恢复 → 去掉 flag 跑全量」
  那段同步删掉（`0d0a21f` 加的，前提已消失），`make gen` 重生成 codex-skills 镜像。

**裁定 2：carried finding 不进 coverage 五集合，另记 RunMeta 观测。**
coverage 的定义是 **per cell**（design §8.4），carried finding 没有本轮 cell——它属于上一个 job 的格、
在那份报告的 coverage 里。不记就不会破坏 `selected = completed ⊎ reused ⊎ failed ⊎ waived` 的划分不变式。
但 `CarryOutcome` 的三个量（`carried` / `carried_dropped` / `resolved_applied`）**必须进 RunMeta**——
否则读报告的人不知道这轮有几条是携带来的。这正是本会话已踩过两次的「加了观测没出口」
（`Phase::Verify` 没人写、`relocated` 没人读）。

## 实施

| 步 | 内容 |
|---|---|
| 1 | `run_code_review` 加 `carried: Vec<Finding>` 参数 → 传进 `DeepInput.prior_findings`（首跑传空，行为逐字节不变） |
| 2 | `decide` 加 delta 守卫：`Origin::Carried` ∧ 区域与本轮 hunk 不相交 → `SkipCarried`；相交 → 落到后续判定 |
| 3 | `RunMeta` 加 carry 观测三量 + md 渲染 |
| 4 | `run_incremental_review` 主流水线：preflight（已有）→ `migrate_renames` → `carry`（排除已 resolved）→ 调 `run_code_review`（bundle = delta、带 carried）→ `merge_carry` 在 deep 内由 `prior_findings` 完成 |
| 5 | 删 `incremental_from_bails_until_m21c` 这条 e2e 锚，换成真实增量轮 e2e |
| 6 | 连带债见下 |

**明确不做**：不动 carry 四零件的算法（它们有测试、语义未变）；不碰台账激活与判例豁免
（交叉点已查清不重叠）；不改 coverage 五集合的契约。

## 连带债（followup 标着「增量轮恢复时一并处理」）——核实后两条变性质、一条已过期

| 债 | 核实结果 |
|---|---|
| followup:136 `resolve --help` 承诺的消费者不存在 | **已解一半**：本单元接通了「resolve 了就不携带」。但 `--help` 还承诺了后半句——「quote 未随修复消失的会注入下轮**顺带验证**防假修复」，那部分没实现，见下 |
| followup:80 增量轮框架语无槽 | 它的实质不是 prompt 槽，而是**假修复检测**这个功能缺口，与上一条同根 |
| followup:214 `ItemKind::Needs` 无生产路径 | **已过期**：批 C3〔D16〕Q8 给了它生产路径（`orchestrate/mod.rs:306` 的 needs 入队），不用删 |
| followup:95 `extra_paths` 泄漏口径 | 仍开放。增量轮现在只传 delta 做证据范围，没有 `extra_paths` 那一层，所以**当前不触发**；若将来补「全分支索引」要一并定 |

### 本单元必须补的：假修复检测（`resolution_disputed`）

又一个「有读无写」——`RunMeta.resolution_disputed` 有渲染（md 的「⚠️ Disputed」行）、有测试，
**产品路径里没有任何地方写它**。这是本会话第四次撞上同一形状。

它不是可选的：`resolve --help` 白纸黑字承诺「quote 未随修复消失的会注入下轮顺带验证防假修复」。
而本单元刚接通的 carry 只做了「resolve 了就不携带」——于是**假修复会被静默放过**
（人 resolve 了说已修、quote 其实还在、这轮不携带、报告里也不提）。

这个风险是**接通增量轮新暴露的**：此前增量轮不能跑，这条路径根本不存在。所以不能留作 followup。

实现：`carry()` 在排除 resolved 那一支里顺手查一次 quote 是否仍锚得上——锚得上 = 没真修 = 争议。
`CarryOutcome` 加 `resolution_disputed`，主流水线写进 `RunMeta`。

## 状态机 checklist 自查

- [x] **加第 N 条读写同一份状态的路径** → 命中：增量轮要读台账（激活）、判例（豁免）、上轮 result（carry）。
      处置：三者都走既有入口（`activate_dormant` / `ledger.cards` / `store.read_result`），不新开读库路径。
- [ ] 加状态值 → 不加（carry 用既有 `Origin::Carried`）
- [x] **夹具直接摆中间态** → 要特别注意：`Origin::Carried` 目前没有产品 writer，
      用例必须**走 `carry()` 真实产出**而不是手搓 `origin: Carried` 的 finding。

## Acceptance Criteria

- [x] `--incremental-from` 可跑：r1 全量 → 改一处 → r2 增量，carried finding 在合并集里
      （`incremental_round_carries_prior_findings_into_the_merged_set`）
- [x] **裁定 1′**：carried 的复核挂点接通 + 两条判据，三条用例
      （`carried_in_a_touched_region_is_reverified` / `..._outside_the_delta_keeps_its_prior_verdict` /
      `..._never_judged_is_verified_even_outside_the_delta`），`decide` 侧单测同步补判据 ①
- [x] **裁定 2**：carry 三量进 RunMeta 并渲染；coverage 五集合划分不变式仍成立
- [x] 已 resolved 的 finding 不被携带（`carry` 的 `resolved_applied`）
- [x] 改名文件的 carried finding 跟着迁移（`migrate_renames`）
- [x] 四守卫照旧拦（`incremental_preflight` 与 `IncrementalBaseline` 的 `#[allow(dead_code)]` 已摘——
      主流水线接通后它们有了生产调用）
- [x] 删 bail 的 e2e 锚，换真实增量轮 e2e（`incremental_from_is_refused_by_preflight_guards`）
- [x] 变异检查：五个变异（判据 ① / 判据 ② 两向 / 派发段整段 / 派发段不接 hunks）**各自精确转红**
      对应用例，无交叉、无遗漏
- [x] 全量测试 **1008 项绿**（957+20+7+24）；`make verify` 绿（commit 后，gen 产物与 HEAD 零 diff）；
      clippy **本批零新增**（只剩 3 条既有：`diffsym:91` / `output:133` / `main:1286`，均在本批改动范围外）
- [x] 假修复检测（`resolution_disputed`）端到端：主 e2e 的 ②′ 断言——`resolve` 后 quote 仍锚得上
      → 不携带 + `r2.run.resolution_disputed` 点名。此前只有 `carry()` 的函数级测试，
      主流水线那行赋值无人盯
- [x] **CLI 入口真能跑**（裸终端验证，新二进制）：`--help` 文案已订正；
      `review --base dev --incremental-from <不存在的 job>` 报的是 preflight 守卫
      （"no persisted result for job …"）而非过期的「M21c」文案；`--background` 组合被命令层拦

## 未做（留给后续）

- **真实增量轮 dogfood**：本单元的 e2e 全用 fake backend，验的是逻辑与接线。真跑一轮
  r1 全量 → 改一处 → r2 增量需要 provider 调用（有成本），且最好挑一个有实质改动的目标。
  与 ship 单元天然同场——ship 的 r2 走的正是这条路径。
- **后台增量轮**：见 followup「增量轮重建带出的账」。

## Notes

- 与 ship 单元强耦合：增量轮恢复后，ship 的 r2 才走它设计时的主路径（现在靠降级跑全量）。
- 远端是公开仓：PR181 衍生物只进 `.next/private/`。
