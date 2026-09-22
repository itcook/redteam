# M21b 出口验收

> 输入：plan `2026-09-10-redteam-m21b-risk-deep-queue-plan.md`「出口验收」十一项 + roadmap `M21b-risk-deep-queue.md`「验收」节。
> 前提：七批（A / B / C / C1 / C2 / C3 / D）全部合回，集成分支 `7790a9b`。
> 数据纪律：PR181 是公司仓——读数、原始报告、oracle 命中明细**只落 `.next/private/m21b/`**，永不入库；
> 公开侧（roadmap / research）只留汇总数字，不出现路径、提交号、文件名。

## 现状总表（2026-09-22 开工前核定）

| # | 项 | 状态 | 证据 / 还差什么 |
|---|---|---|---|
| 1 | PR181 端到端 ×3 | ⏳ **待跑** | 门槛：findings 命中 oracle ≥ 6（「GLM 单发 6 条」= P0 dev 作业里 GLM 的 6 条）；墙钟分段可读。**缺 findings 评分器**（见 D5） |
| 2 | dev 对照 | ⏳ **待定**（D4） | P0 已有 dev 作业（GLM + Kimi，39.5 min，8 findings，其中 GLM 6 条进 oracle） |
| 3 | 预算与必挖 | ✅ 单测 | `must_items_are_never_cut_even_with_zero_budget`（K = 0 时主区照挖、其余收场 Skipped 且进 RunMeta）、`drain_zero_budget_keeps_primary` |
| 4 | verifier | ⏳ **待跑 + 待定口径**（D2） | 判定层单测已覆盖「全同族 → 同族 + warning」（`all_same_family_falls_back_with_a_warning`）；「复核尾部 ≤ 1 min」本仓 dogfood 实测 3.5 min（glm 自己验自己） |
| 5 | provider 门 | ✅ 单测 + 部分实测 | `inflight_peak_equals_gate_width`、`shared_gate_scanner_enters_before_h0_at_t0`、`shared_gate_release_goes_to_waiting_scanner_first`、`second_stage_round_holds_the_gate`（门宽 1）；PR181 两轮 scan-only 零 429 / 503；端到端那轮再确认一次 |
| 6 | kill → resume | ⏳ **待跑** | resume 单测大量（`resume_tests`）；真 provider、后台 job、中途 kill 这一条还没真跑过 |
| 7 | 输出有界性 | ⚠️ **现有数据已不过 + 待定口径**（D3） | 本仓 dogfood 12 个 reviewer 格里 6 格 > `maxOutputTokens/2`（16384）：单轮 16.6k / 17.2k，两轮（二阶段）20.4k–55.6k；FINALIZE / REPAIR 零触发 |
| 8 | 回归集 | ✅ 三项全过 | **risk 对拍**（PR181，θ = 1.5，不带台账）：H0 17 / 17、jaccard 1.000、R 差 2e-7；**verify_score**：D-4 两臂基线已入 research；**scan_score**（PR181 两轮 scan-only 对 oracle 全并集 22 条）：均值命中 **11.0**、并集 **12**、噪音 0.835（P0 E3 B 臂 5.33 / 9 / 0.55；M21a 单 DeepSeek slice 9.33 / 16 / 0.843——噪音持平 M21a，其随条数涨的成因 M21a 已记） |
| 9 | 门与文档 | ✅ | `make verify` 绿；`--help` 有 `risk` 子命令、`review --help` 有深挖说明；无 `--engine` / `--risk` / `--models` / `--workers` |
| 10 | 墙钟分段 | ⏳ 取自第 1 项 | 分段字段已齐（`close_ms` / `first_dispatch_ms` / `drain_done_ms` / `verify_tail_ms` / `fill_block_*`）；本仓 dogfood 阻塞 max 36 ms、累计 200 ms，远低于挪 `spawn_blocking` 的线 |
| 11 | 深挖选项稳定性 | ✅ 沿用 C3 | C3 三轮同 diff：选中项重合度中位 41%（门槛 40%）、单文件最大占格 ≤ 1/3；数字已入 followup / D16 |

**另已知**：PR181 扇出段基线（scan-only ×2）已建——最慢 scanner 格 ≈ 285s，明细 `.next/private/m21b/2026-09-22-pr181-scanout-baseline.md`。

## 开跑前要定的事

**D1 · reviewer 的 `thinkingLevel: high` 一直没生效**（影响第 1 / 2 / 4 / 7 项）
每轮报告都有 warning「thinkingLevel high 对 reviewer agc/glm-5.3（kind Anthropic，family glm）无参数映射，未发送」——
`thinking_params` 的 anthropic 分支只认 `claude` 家族，C1 → D 的全部 dogfood 里 reviewer 都在**思考关**下跑。
- 选项 a：先查网关（aigocode）对 glm 走 anthropic 格式是否接受思考参数，接受就补 `(anthropic, glm)` 映射再验收。
- 选项 b：带着思考关验收，记录里写明。
- 注意交互：anthropic 口径下 `output_tokens` **含思考 token**——开了思考，第 7 项会更难过。

**D2 · 第 4 项「复核尾部 ≤ 1 min」怎么判**（复核缺省已关）
- 选项 a：验收那几轮临时开复核，**显式声明 `deepseek/deepseek-flash` 为 verifier**（异族、离线基线单条中位 26s）——测的是「开了复核的正确配置」。
- 选项 b：按缺省配置（复核关）判，尾部恒 0，这一条记「缺省关、不适用」。
- 选项 c：按现配置（glm 兼 reviewer + verifier）判——大概率不过（dogfood 3.5 min），且测的是一个已知较差的配置。

**D3 · 第 7 项「输出有界性」的口径**
plan 原意是「单次生成不撞输出帽 → 不触发截断 / REPAIR」，但现有字段 `reviewer_output_tokens` 是**按格求和**（两轮相加）。
- 选项 a：按**轮**判（< cap / 2）——需要给 cell 记录加「单轮最大输出」字段（小改动）。
- 选项 b：按**格**判——现有数据已不过（6 / 12）。
- 选项 c：按「**零截断**」判（FINALIZE / REPAIR 不触发）——现有数据已过；把 cap / 2 降为观测量。

**D4 · 第 2 项 dev 对照：复用还是重跑**
dev 二进制冻结于 9-07，重跑只会量到模型抖动；P0 已有作业（GLM + Kimi，39.5 min，GLM 6 条进 oracle）。
- 选项 a（倾向）：**复用 P0 dev 数字**作 dev 侧，只跑 `redteam-next` 侧。省 ~40 min 与一轮 GLM + Kimi 的花费。
- 选项 b：dev 与 `redteam-next` **同日**并排重跑，排除「两个月里模型 / 网关变了」这个混杂。

**D5 · findings 评分器**（第 1 / 2 项的前置）
`eval/scan_score.py` 只评 scanner 疑点，**没有现成的 findings × oracle 评分器**。P0 的口径是
「quote 去空白定位 head 树 → 区间级自动命中全并集 22 条 → 人工按『命名同一缺陷机制』严格确认」。
- 方案：`eval/finding_score.py`，复用 `_oracle.py`（`anchor_quote` / `overlap` / `load_oracle` / `match_entries`），
  自动命中那半落代码；人工确认那半产出一张待确认表（我读、你终审）。dev 与 `redteam-next` 用同一个评分器。

**D6 · 轮数与花费**
plan 写 ×3。PR181 tier high、`budget_cells = 32`：单轮预计 30–60 min（本仓 dogfood 12 格就 18.5 min）。
×3 ≈ 2–3 h 墙钟 + 3 轮 32 格 reviewer 的 token。第 6 项（kill → resume）可以复用其中一轮（中途 kill 再 resume），不另开。

## 做完后

roadmap `M21b-risk-deep-queue.md` 交付项打勾、状态 ✅、README 表更新；三档预算与 refute 率数字写进 research；
followup 记未过项的处置。若 D1 / D3 / D5 需要改代码，开小分支 `feat/m21b-exit`，走 `/code-review` 后合回。
