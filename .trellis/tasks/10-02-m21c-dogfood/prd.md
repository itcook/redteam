# M21c 批 D dogfood 内循环与出口

## Goal

逐条核验 M21c 的五条出口验收，补缺的验证，然后收口。**这是验收批不是功能批**——
核验的诚实度比产出代码量重要。

## 核验结果

| # | 验收项 | 结论 | 证据 |
|---|---|---|---|
| ① | 三轮增量 dogfood：≥ 1 例休眠疑点被 delta 激活并晋升为 finding | ✓ **12 例**（远超 1） | 台账 `status='promoted'` 12 行，`max(activations)=3`；实际跑了五轮（本仓 r1/r2、批 B r1、PR181、小 diff） |
| ① | 收敛判定一致 | ✓ | 四轮 `converged=false`（各有 20+ 条预算截断）+ 小 diff 轮 **`converged=true`**（零截断）——**首次在真实运行里观测到收敛可达**，也反过来印证前四轮的 false 是对的 |
| ① | exit code 五档契约一致 | ✓ | 纯函数 `exit_code_maps_all_cases` 覆盖全部七种 (verdict, debt) 组合；e2e 覆盖 3 档与 2 档；真跑覆盖 10 档（四轮）与 0 档（小 diff 轮 `exit=0`） |
| ② | coverage 五集合不相交并 = 全集 | ✓ | `check_partition` + 性质用例 `coverage_partition_invariant` |
| ② | kill 快扫 cell → verdict Incomplete | ✓ | e2e `unreachable_scanner_cell_makes_scan_incomplete_exit_three` |
| ② | kill deep cell → 带债 approve + warning | ✓ | `deep_reviewer_failure_is_approve_exit_2` |
| ③ | 判例前移主判据〔D19-5〕：两条构造用例 | ✓ | 批 B 的 `dismissed_variant_is_exempted_at_enqueue_time` / `delta_in_the_precedent_region_forces_a_redig_through_the_queue`，另加 resume 路径与 CLI 入口两条 |
| ③ | 观测量：判例命中致免入队 / 免重挖项数 | ✓ 字段已有 | `precedent_exempt` / `precedent_redug`；批 B 真实轮 0 命中，原因查清（那张卡指向的 `queue.rs` 本轮未改，按组筛正确排除） |
| ③ | 观测量：同一 PR 连续两轮重复报同一 finding 的比例 | ✓ **基线已测** | 见下「重复率的口径」 |
| ④ | 切名门：PR181 一轮机械面（失败格 / 截断 / 解析失败皆 0） | ✓ | 批 0 已过（2026-09-28 记录）；批 C 的 PR181 轮复核：失败格 0、`max_tokens` 截断 0、解析 warning 0、修复轮 0 |
| ⑤ | 全量测试绿 · `make verify` 绿 · dev 二进制基线已归档 | ✓ | 953 lib + 48 其他；基线在 M21a 出口记录第 7 项与 research §17 |

## 重复率的口径（差点用错分母）

观测量原文是「同一 PR 连续两轮重复报同一 finding 的比例」。第一次算我用的是本仓 r1 / r2 / 批 B r1 ——
**那三轮的 diff 各不相同**（代码一直在改），不是「同一 PR」，算出来的 0% 量的不是设计想量的东西。
这与 M21d 批 C-1 栽过的是同一类错（「这个比值量的是不是我以为的那件事」）。

正确的分母是 **PR181 同 HEAD 的两对连续轮**：

| 对齐方式 | 09-23 两轮 | 09-30 两轮 |
|---|---|---|
| **file:line** | 24/47 = **51%** | 28/60 = **47%** |
| **finding id**（title + quote 的内容指纹） | **0** | **0** |

**机制在同一处代码上反复报，但每次措辞不同。** 这坐实了批 B 的设计选择：
判例若按 id 精确匹配**完全无效**（id 每轮都变），机械匹配（file + `quote_overlap`）才是对的路子。
两对都在判例前移落地**之前**，所以这是基线——判例前移后的效果要等下一轮 PR181 才能测。

## 一处差点误判的机械面

PR181 那轮有 **24 格 `attempts > 1`**，我一度当成修复轮（切名门要求修复轮为 0）。查下去：
`queue_wait_ms` 全 0、`provider_rate_limit` 空 → 不是 429 重试；而 `needs_two_stage_triggered = 24`
**恰好相等** → 是二阶段补查轮，正常功能。

根因是 `rec.attempts = iterations + rate_limit_retries`，把**修复轮、二阶段补查轮、429 重试**
混进一个数。已记 followup。

## Acceptance Criteria

- [x] 五条出口验收逐条核验，每条附可追的证据（不靠记忆）
- [x] 补上缺的那一项：真实运行里观测到 `converged = true`（小 diff 轮）
- [x] 观测量「重复报率」用正确的分母算出基线
- [x] 机械面复核，含辨清 `attempts` 的三重含义
- [x] followup 结账（四笔新账）、私有记录归档
- [x] **roadmap 标 M21c ✅** —— 那处未达成已裁（2026-10-03）：ship 内循环**挪出本里程碑**、
      单独成一个调整 / 优化 / 重构单元。M21c 这边「修复回流」与「激活观测到一例」（12 例）都已达成，
      故交付项 9 标 ✅。那个单元的六条输入条件记在 followup 的「ship 作为独立单元」节。

## Notes

- 远端是公开仓：PR181 与公司仓衍生物只进 `.next/private/`（gitignored）。
- 本批不写功能代码。若核验中发现缺陷，就地修并记在案。
