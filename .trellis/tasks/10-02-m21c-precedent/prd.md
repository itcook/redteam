# M21c 批 B 判例前移与小项

## Goal

判例摘要进快扫 prompt（B-1）+ `matches_precedent` 前移到入队侧带守卫（B-2）+ 小项打包（B-3）。
依据 design §4 / §6 规则 2、〔D13〕〔D19-5〕、roadmap 项 5 / 6、plan 批 B。

## 开工前的事实核查（纠正 10-02 早先的一个误判）

上次把批 B 判为「前提不成立、卡着不能动」——**后半句错了**。查 plan 原文：

| Task | 验收 | 要真实判例吗 |
|---|---|---|
| B-1 判例摘要进 prompt | prompt 快照用例；无判例时零注入、逐字节不变 | 否 |
| B-2 入队侧豁免 | 〔D19-5〕**两条构造用例** | 否 |
| B-3 小项打包 | 与判例无关 | 否 |
| B-4 观测 | 重复报同一 finding 的比例，**不设门槛** | 是，但不设门槛 |

更关键：**〔D19-5〕在 2026-09-23 就为「真实判例太少」改过口径**——原判据「refute 调用数下降」
因「M21b 三轮触发 24/22/24、驳回 0/2/1，基数太小、下降落在噪声里」而撤销，改成构造用例。
这个问题当时已处理，十天后我又把它当成未解决的阻塞。**喂料不是开工前提，是收益验证。**

零件都在：`render_precedents`（`prompt/mod.rs:448`，带测试）、`matches_precedent`
（`consolidate/verify.rs:138`，标着 `#[allow(dead_code)] // 判例变体触发是 M21c 的空槽`）。

### 喂料方案（实测对比后取 C）

| | 产出 | 判断 |
|---|---|---|
| A 导入旧库〔D19-4 搁置项〕 | 旧库 22 条带 quote 锚中：**命中 9 · 锚失效 10 · 文件已删 7** | 剩 9 条且是 M18 时代针对旧代码的判断，不值一条新读库路径。搁置——真要用时 `dismiss gc` 本就会把锚失效的标 `stale` |
| **C 从新鲜 finding 建卡** | r1/r2/PR181 三轮共 150 条 finding | **取此**。锚必然命中（就是从当前代码产出的）；素材不用重判——followup 里的 medium 余项 5 条、r2 里判「方向安全不修」的 2 条，本就是「人看过、决定不处理」。`dismiss add --from` 只读 `findings[].{id,title,file,quote,severity}`，当前 schema 全有 |

附带价值：C 顺手验了 `dismiss add --from` 这条 M21 期间从没跑过的路径。

## 三条自定的设计决定（design 给了意图，这里定实现）

| # | 决定 | 理由 |
|---|---|---|
| 1 | 「区域自判例以来无 delta」= **判例的 file + 行范围与本轮 delta 相交则视为有 delta**（不豁免） | 判例卡只有 `round`（字符串）与天粒度 `updated_at`，推不出 commit，按日期跑 `git log` 既不准也贵。本轮 diff 就是已知的改动集，直接相交即可。方向保守（宁可重挖），正是 design §4「无守卫的漏报路径 = fix 引入新 bug 被旧判例盖住」要防的 |
| 2 | Suspicion 侧机械匹配以 **file + `quote_overlap`** 为主，不拿 `hint` 当 title 用 | `matches_precedent` 原用 `title_jaccard`，而 Suspicion 没有 title；`hint` 是 <50 token 的一句话提示，与 finding title 不同质，拿它算 jaccard 会制造虚假相关 |
| 3 | 豁免**只计数、不列明细** | design 只要求「观测量（新 RunMeta 字段）：判例命中导致的免入队 / 免重挖项数」。列明细会让报告膨胀，而豁免项本就是「人已经判过不用看」的那批 |

## 改动边界（before-dev step 7）

**最小行为差**：scanner prompt 的 `{{PRECEDENTS}}` 槽**恒空**（两个构造点都传 `""`），
入队侧不认判例。应该：槽按组填该组文件上的活跃判例摘要；入队侧机械匹配命中且守卫过 → 豁免。

**行为住在哪**（不是最容易拦的地方）：
- B-1 的槽在 `orchestrate/mod.rs:214`（真实扇出）与 `cli/mod.rs:1095`（`--pack-only`）。
  **两处必须同口径**——「pack-only 与真实扇出不一致」是本仓五次栽坑之一。
- B-2 的入口在 `orchestrate/queue.rs` 的入队侧（`enqueue_affected` 或其调用方 `deep.rs::enqueue_scan_cells`），
  不是 consolidate 层——判例前移的全部意义就是把判断提到烧 token 之前。

**预期改的文件，每个为什么必要**：

| 文件 | 为什么 |
|---|---|
| `src/prompt/mod.rs` | 加按**文件集**筛选+渲染的判例函数。现有 `render_precedents` 吃 `&Finding` 做相关性排序，scanner 侧没有 finding |
| `src/orchestrate/mod.rs` | `ScanFanoutInput` 加 `precedents`，真实扇出填槽 |
| `src/cli/mod.rs` | pack-only 路径同步填槽；两处判例来源都是 `ledger.cards` |
| `src/consolidate/verify.rs` | `matches_precedent` 的 Suspicion 变体（或提到共用位置），摘掉 `#[allow(dead_code)]` |
| `src/orchestrate/deep.rs` / `queue.rs` | 入队侧豁免判定 + 观测计数 |
| `src/domain/report.rs` | 观测字段（免入队 / 免重挖项数） |
| `src/prompt/templates/adversarial-refute.md` | B-3：`analysis` 先于 `verdict` |
| `src/positioning/` | B-3：跨文件重定位（changed files 内唯一命中才接受，多 / 零命中拒绝） |

**明确不做**：
- **不新开读库路径**：判例一律从已加载的 `ledger.cards` 取，它由 `load_ledger(args.dismissed, args.no_dismissed, root)`
  产出、**已经过 `--no-dismissed` 这道旗**。这是「保护放在入口」的既有实例，批 C 刚因为五条路径各自开库栽过第六次。
- 不碰 `suspicions` 台账与激活链（批 C 的地盘）。
- 不设 B-4 的观测量门槛〔D19-5 已定〕。
- 不做判例的 CLI 面改动（`dismiss` 子命令族照旧）。

**安全面已有保护，不重做**：`{{PRECEDENTS}}` 走 `slot` 闭包，对非空内容一律
`neutralize_structure(s, SCAN_STRUCTURAL_TAGS)`；`render_precedents` 的七字段各过 `oneline`
（E11-Q5 实证过换行可伪造 `- [fake]` 条目）。

## 状态机 checklist 自查（`.trellis/spec/guides/state-machine-thinking-guide.md`）

- [x] **加第 N 条读写同一份状态的路径** → **命中**。B-1/B-2 都要读 `dismissed` 台账。
      处置：从 `ledger.cards` 取而不自己开库，自动受 `--no-dismissed` 保护（见「明确不做」第一条）。
- [ ] 加状态值 → 不加。豁免是**读**判例，不写新状态；队列项侧也不加状态（只加观测计数）。
- [ ] 改状态转换规则 → 不改。
- [ ] 夹具直接摆中间态 → 实施时注意：判例卡夹具要走 `dismiss_upsert`，不手搓 DB 行。
- [ ] 依赖累计量的逻辑 → 无。

## Acceptance Criteria

- [ ] **B-1 prompt 快照**：有判例时槽被填且模板哈希变化可见；**无判例时 prompt 逐字节不变**。
- [ ] **B-1 两处同口径**：真实扇出与 `--pack-only` 对同一输入产出同一判例段。
- [ ] **B-1 按组筛**：A 组的判例不出现在 B 组的 prompt 里。
- [ ] **B-2 构造用例①**：已 dismissed 的变体在入队被豁免（不入队、计数可见）。
- [ ] **B-2 构造用例②**：判例区域有 delta → 必须重挖（守卫不过，入队并带判例笔记）。
- [ ] **B-2 守卫两项各自可单独判假**：锚定重定位失败 → 不豁免；区域有 delta → 不豁免。
- [ ] **`--no-dismissed` 关掉判例注入与豁免**（并入批 C 那条遍历式不变式测试，`names.len()` 随之加）。
- [ ] **B-3** 三小项各一条用例。
- [ ] **喂料**：从三轮 finding 里挑已裁定的误报建 `dismiss` 卡，`dismiss add --from` 路径实跑通。
- [ ] **变异检查**：每处行为改动都能被某条用例转红（判据用 returncode，范围跑全部 target）。
- [ ] 全量测试绿、`make verify` 绿、clippy 本批零新增。

## Notes

- 远端是公开仓：PR181 与公司仓衍生物只进 `.next/private/`（gitignored）。
- 与 M21d 的分工〔D19-3〕：prompt 模板**独占**——判例前移在本批动 `src/prompt/`，M21d 已交付，无并行冲突。
