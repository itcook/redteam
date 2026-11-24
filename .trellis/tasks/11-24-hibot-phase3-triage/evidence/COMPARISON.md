# Phase 3.8 · 真实 PR 对拍（2026-11-24）

模型：DeepSeek Flash（`deepseek-flash`，job 模式只从 env 取 key）；单次 ≤900s；总跑数 8/10（含 1 次 GLM infra 失败）。原始报告见同目录 `report.*.json`。

## 机读汇总

| repo | PR | status | tier | blockers | struct | local | intent | creep | risk | drift | findings | in/out tokens | 报告 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| backend | #333 | ok | 0 | 1 | 1 | 2 | 0 | 0 | medium | structural | 1 | 239805/72884 | `report.hiapi-backend-pr-333.json` |
| cli | #18 | ok | 0 | 4 | 2 | 5 | 2 | 0 | high | structural | 2 | 143310/63618 | `report.hiapi-cli-pr-18.json` |
| sites | #235 | ok | 0 | 7 | 4 | 4 | 3 | 2 | high | structural | 4 | 360968/76068 | `report.hiapi-sites-pr-235.json` |
| sites | #277 | ok | 0 | 3 | 3 | 2 | 0 | 0 | high | structural | 3 | 242898/75973 | `report.hiapi-sites-pr-277.json` |
| sites | #328 | ok | 0 | 5 | 5 | 5 | 0 | 1 | high | structural | 5 | 181935/60723 | `report.hiapi-sites-pr-328.json` |

## 与人工 pr-triage 的差异

人工值来自 `~/hiapi/<repo>/.rpiv/artifacts/triage/*.md` 的 frontmatter；同一 PR 多次分诊时全部列出。

### backend #333

- 人工（`2026-10-20_07-54-25_pr-333-feat-feishu-hiapi推广助手后端直连飞书长连接.md`）：tier 2 / blockers 0 (0 struct + 0 intent) / local 3 / creep 6 / risk high / drift local
- 本次：tier 0 / blockers 1 (1 struct + 0 intent) / local 2 / creep 0 / risk medium / drift structural

### cli #18

- 人工（`2026-11-20_02-49-52_pr-18-feat-content-支持-showcase-文案编辑和-accepted-待审修订.md`）：tier 0 / blockers 3 (2 struct + 1 intent) / local 11 / creep 5 / risk medium / drift structural
- 本次：tier 0 / blockers 4 (2 struct + 2 intent) / local 5 / creep 0 / risk high / drift structural

### sites #235

- 人工（`2026-11-14_08-04-19_pr-235-feat-dashboard-request-encapsulation.md`）：tier 0 / blockers 7 (3 struct + 4 intent) / local 5 / creep 7 / risk medium / drift structural
- 本次：tier 0 / blockers 7 (4 struct + 3 intent) / local 4 / creep 2 / risk high / drift structural

### sites #277

- 人工（`2026-11-17_08-26-08_pr-277-content-api-overload.md`）：tier 0 / blockers 8 (7 struct + 1 intent) / local 4 / creep 3 / risk high / drift structural
- 本次：tier 0 / blockers 3 (3 struct + 0 intent) / local 2 / creep 0 / risk high / drift structural

### sites #328

- 人工（`2026-11-20_06-36-31_pr-328-feat-content-showcase-copy-patch-and-accepted-revisions.md`）：tier 0 / blockers 4 (3 struct + 1 intent) / local 5 / creep 5 / risk medium / drift structural
- 人工（`2026-11-20_07-04-27_pr-328-feat-content-showcase-copy-patch-and-accepted-revisions.md`）：tier 0 / blockers 1 (1 struct + 0 intent) / local 4 / creep 0 / risk medium / drift structural
- 人工（`2026-11-20_07-06-14_pr-328-feat-content-showcase-copy-patch-and-accepted-revisions.md`）：tier 0 / blockers 1 (1 struct + 0 intent) / local 0 / creep 0 / risk medium / drift structural
- 本次：tier 0 / blockers 5 (5 struct + 0 intent) / local 5 / creep 1 / risk high / drift structural

## 结论与已知差异

1. **无「干净判 tier 2」**：4 个 tier0 干净 PR（sites #277/#235/#328、cli #18）本次全部 tier0。
2. **tier2 漏报 1 例**：backend #333 人工 tier2（surface-4 LLM→特权写，conf 8–9）；DeepSeek 两轮均 0 行（A2 已补「缓解不打断可达性」定义仍未命中），按协调者裁决记为 model/定义差异，不伪造人工结论。GLM-5.3 尝试因 900s 墙钟 + `finish_reason=max_tokens, len=0` infra 失败，未形成对比。
3. **risk 口径差异**：`blockers≥3 → high` 使 sites #235(人工 medium)、sites #328(medium)、cli #18(medium) 全部判 high；该公式是 grill 定案的 `[拟议]` 规则，本次不改，记入差异。
4. **漂移计数**：sites #235 总量与人工精确一致（7=7），但构成不同（我们 4 struct+3 intent，人工 3 struct+4 intent）；sites #277 我们 3 blockers vs 人工 8（欠报 5）。
5. **安全轴丢弃率**：backend #333 A 轮 1 行候选、1 行 <8 被丢（100%）；A2/A2 后 0 行候选。sites #328 漂移轴 1 行 unanchored 被丢（已披露在 `axes[].dropped.dropped_rows`）。


## GLM-5.3 复跑（backend #333，第 9/10 次；32k output / 1200s）

- `status=partial`（1200s 墙钟耗尽；drift/intent 两轴 interrupted）。
- 安全轴：**2 行**，`dropped_rows=[]`；两行均非 tier2 白名单面：
  - surface 5（injection），`service/feishu_finance_agent_client.go:286`，conf 9/10；
  - surface 7（SSRF），`service/feishu_finance_agent_ws.go:101–102`，conf 8/10。
  → 若 drift/intent 完成，`security_tier` 应为 **1**（无面 1/3/4 行），仍不是人工的 tier2。
- usage 45,074 in / 61,696 out；原始报告：`report.hiapi-backend-pr-333-glm-partial.json`。
- 与 DeepSeek + A2（安全轴 0 行）并列：**GLM 能召回两条安全行（含 injection），但仍未命中人工的 surface-4 特权写链路**。
- 注：backend #333 的 A2（DeepSeek）原始报告已被本次 GLM 复跑覆盖；A2 数值来自覆盖前生成的 `metrics.jsonl` 与本文件上表。

## GLM-5.3 墙钟优化验证（backend #333，32k output / 1200s）

- 实际墙钟 **540.1s**（优化前同配置 1200.1s），`status=partial`。
- 单格预算：security/intent 539.95s、drift 315.36s（排队时间计入本格）。
- security 2 行 + 1 行 conf6 被丢（`dropped_rows` 披露）；drift 1 cell；intent 0 行（540s 内未返回）。
- usage 60,093 in / 63,797 out；原始报告：`report.hiapi-backend-pr-333-glm-optimized.json`。

## A 两阶段 intent · 模型 A/B（2026-11-24）

> A 流程：I0 串行抽 claims/overlap + I1..In 按 diff 切片验证/scope creep；三轴模型取 `roles.reviewer`（high）。
> 夹具：backend #333（人工 tier2）、sites #328（人工 tier0，clean）。

### backend #333

| 模型 | 墙钟 | status | tier | blockers | struct | intent | local | creep | intent 轴 |
|---|---|---|---|---|---|---|---|---|---|
| DeepSeek high（I0 修正前） | 282.0s | ok | 1 | 3 | 1 | 2 | 1 | 2 | 31 claims / 29 delivered（2 个验证性声明被误算，虚增 undelivered） |
| **DeepSeek high（I0 修正后 + 公平性）** | **177.7s** | **ok** | **1** | **0** | **0** | **0** | **3** | **5** | 20 claims / 20 delivered / complete |
| GLM-5.3 high | 622.8s | partial | — | — | — | — | — | — | I0 在 150s 内未返回（0 行） |
| GLM-5.4 high | 4.4s | partial | — | — | — | — | — | — | **provider 404**（未进入模型） |

- 人工基线：tier **2** / blockers 0 / structural 0 / local 3 / creep 6 / high。DeepSeek high 修正后除 tier2 外基本对齐。
- 安全轴：surface 5（injection）与 surface 7（SSRF）各 1 行；无 surface 1/3/4 → tier1（人工的 surface-4 特权写仍未召回）。
- 原始报告：`report.hiapi-backend-pr-333-deepseek-a2.json`、`...-deepseek-claims31.json`、`...-glm-a.json`、`...-glm54.json`。

### sites #328（clean，精度）

| 模型 | 墙钟 | status | tier | blockers | struct | intent | local | creep | 备注 |
|---|---|---|---|---|---|---|---|---|---|
| DeepSeek high | 114.8s | ok | 0 | 2 | 2 | 0 | 8 | 2 | intent 5 cells 全成；security 2 行均 <8 丢弃（conf 6/2） |
| GLM-5.3 high（公平性修复后） | 425.5s | partial | — | — | — | — | — | — | security 4 cells 完成；drift 1/4、intent slice 166s 超时 |
| GLM-5.4 high | — | — | — | — | — | — | — | — | 同 provider 404，未跑 |

- **无「干净判 tier2」**：DeepSeek high 与 GLM-5.3 的安全轴都没有把 clean PR 判成 tier2。
- 原始报告：`report.hiapi-sites-pr-328-deepseek-a.json`、`...-glm-a2.json`。

### GLM-5.4 阻塞（provider 侧）

- `agc` 网关 `/v1/messages` 与 `/chat/completions` 均返回
  `404 Model "glm-5.4" is not available for this group`；
  `/v1/models` 只列出 `glm-5.3` / `glm-5.3-flash`。
- 用同一 key 试 Zhipu 官方 `https://open.bigmodel.cn/api/paas/v4` → `401 令牌已过期或验证不正确`。
- 结论：secrets.json 里的 `glm-5.4` key 在当前 agc group 下不可用；需要正确的 baseUrl/group/model id（或换 key）后再跑。

## GLM-5.4 provider 修复后复跑（2026-11-24，补跑）

> `/v1/models` 已列出 `glm-5.4`；同一 key/agc group 可调用。配置：reviewer high / 32k output / 1200s。

| PR | 墙钟 | status | security | drift | intent | findings | 备注 |
|---|---|---|---|---|---|---|---|
| backend #333 | 424.4s | partial | ok（4 行，2 行 <8 丢） | 3/4 cells | I0 20 claims，slice 166s 超时，0 delivered | 2（surface 5/7） | 首次补跑 |
| backend #333（per-chunk 满预算） | 451.0s | partial | ok（3 行，2 行 <8 丢） | 2/4 cells | I0 20 claims，9 delivered，slice 321s 超时（repair） | 1（surface 5） | 第二跑 |
| sites #328 | 396.7s | partial | ok（1 行 <8 丢） | 2/4 cells（10 行→6 structural） | 11 claims / 4 delivered / 2 creep，slice 89.6s 超时 | 6（全 structural） | clean 精度仍通过（无 tier2） |

- **5.4 的 I0 已能在 150s 内完成**（5.3 不能），20 claims 与人工 15 项的规模接近。
- 但 slice 验证仍超时：backed #333 第二次跑到 451s 时，全局 1200s 预算还剩 ~749s 未用——
  原因是 `run_intent_slices` 在 chunk 失败后 `break`，没有用剩余预算重试。
- 建议下一步（需确认）：chunk 失败时用剩余预算重试（绝对 deadline 封顶），而不是立即收敛；
  若首轮是 `max_tokens` 截断，再把 GLM 的 maxOutputTokens 提到 64k。
- 原始报告：`report.hiapi-backend-pr-333-glm54.json` / `...-glm54b.json`、`report.hiapi-sites-pr-328-glm54.json`。

## GLM-5.4 slice 超时根因（诊断跑，2026-11-24）

- run：backend #333，`-glm54diag`，451.4s，partial。
- 轴错误原文：
  - drift：`timeout: triage cell `triage-drift` exceeded its 154.958681816s budget (attempts=1 last_call_ms=None in_flight_ms=Some(154960) last_finish=None last_text_len=None last_error=None)`
  - intent slice：`timeout: triage cell `triage-intent-slice` exceeded its 320.849922316s budget (attempts=1 last_call_ms=None in_flight_ms=Some(320853) last_finish=None last_text_len=None last_error=None)`
- 判读：`attempts=1`、`last_error=None`、`last_finish=None` → **单次调用在预算到期时仍在飞**；
  没有网关错误、没有重试、没有 `max_tokens` 截断/解析结果。根因是**模型/网关单次响应耗时超过单格预算**，
  不是我们的重试链或输出上限。
- 结论：要拿完整 intent，需要给 slice 更大的单格预算（把 intent 的预算份额提高/减少波次），
  或换更快的模型/更低 reasoning；单纯“用剩余预算重试”只在单次调用能在重试预算内返回时才有意义。

## 预算 v1（scale-based）验证（2026-11-24）

常量：`BASE=60s / PER_KB=6s / SAFETY=1.15 / MIN=120 / MAX=600`；I0 初版 60–180s，实测后调到 **120–240s**。

### backend #333

| 模型 | 墙钟 | status | security | drift | intent | 备注 |
|---|---|---|---|---|---|---|
| GLM-5.4 high（I0 60s 基线） | 467.9s | partial | ok，393s | ok，94.6s | **I0 73.6s 超时** | I0 文本预算太低；据此把 I0 基线抬到 120s |
| **GLM-5.4 high（I0 120s 基线）** | **840.5s** | partial | ok，272s，4 行（2 行 <8 丢） | **1/3 cells**；等门后只剩 49.9s 超时 | **ok：20 claims / 20 delivered / 3 creep，839s，complete** | intent 首次完整；drift 被 security+intent 占满门饿死 |
| **DeepSeek high（同预算）** | **251.8s** | **ok** | ok，3 cells | ok，3 cells | ok，4 cells | tier1 / blockers0 / local2 / creep4 |

- 规模分片：backend #333 = 3 片（130KB / 13 文件），与旧公式一致；sites #328 按新公式应为 3 片（旧公式 4 片，本轮未跑）。
- per-cell 预算：security 44KB ≈ 373s，实测 3 cells 共 272s；drift 44KB+pack，实际可用 <373s（排队后只剩 49.9s）。
- **剩余问题**：join 顺序是 intent → security → drift，drift 永远最后拿门；cap=4 时 security(3)+intent(2) 已占满，drift 只能等。需要 per-axis 最小配额或 round-robin 调度。
- 原始报告：`report.hiapi-backend-pr-333-glm54budget.json`（I0 73.6s）、`...-glm54budget2.json`（I0 120s）、`...-deepseekbudget.json`。

### 预算 v1 后续三次 5.4 验证（门公平性 / 字节均衡）

| 轮次 | 改动 | 墙钟 | status | security | drift | intent |
|---|---|---|---|---|---|---|
| `-glm54budget2` | I0 基线 120s | 840.5s | partial | ok，272s | 1/3（等门后只剩 49.9s） | **ok：20/20 delivered，839s** |
| `-glm54budget3` | 等门不再吃 cell 预算 | 675.2s | partial | 2/3（64KB shard 538s 在飞） | **ok，3/3** | I0 网络超时（attempts=2，last_error=network timeout） |
| `-glm54budget4` | 分片按字节均衡 | 496.1s | partial | ok | 2/3（169.8s 在飞） | I0 ok + 1 slice 391s 在飞 |

- 字节均衡后 security 稳定完成；drift 从“0/3 饿死”提升到 2/3；intent 的 slice 仍会因 5.4 单次 >390s 而超时。
- 诊断为 **5.4 high 的单次调用方差大**（有时 300s 内完成 3 slice，有时 391s 不返回），不是预算公式或网关重试； 
  网络超时只在 `-glm54budget3` 的 I0 出现一次（`last_error=network timeout`，attempts=2）。
- DeepSeek high 在同预算下 251.8s / ok / tier1 / blockers0，intent 4 cells 全成——**快模型下 scale 预算不增加墙钟**。

## 总预算 1800s（2026-11-24 用户决定）

为让总预算真正生效，cell 预算乘以 `time_factor = clamp(manifest/1200, 1.0, 1.5)`（1800→1.5×，上限 900s），
I0 同系数放大（上限 360s）。

| 轮次 | 改动 | 墙钟 | status | security | drift | intent |
|---|---|---|---|---|---|---|
| `-glm54-1800` | 仅 manifest 1800（cell 仍 1200 规模） | 613.1s | partial | 2/3（356s 在飞） | ok 3/3 | I0 133s 超时 |
| `-glm54-1800b` | cell ×1.5，I0 未放大 | 462.3s | partial | ok 3/3（2 行 conf7 丢） | 2/3（254s 在飞） | I0 133s 超时 |
| **`-glm54-1800c`** | **cell ×1.5 + I0 ×1.5** | **1051.4s** | **ok** | **ok 3/3**（2 行 conf7 丢，tier0） | **ok 3/3** | **ok：20/20 delivered，1 creep，complete** |

- 1800s 下三轴全部完成，`summary` 合法；`risk=medium`、`local=3`、`blockers=0`。
- 仍是 **tier0 vs 人工 tier2**：两行安全候选都是 conf7（模型这次没给到 8–9），不是预算问题。
- 结论：**1800s 总预算 + 1.5× 单格/I0 放大可以让 GLM-5.4 稳定完成 triage**；tier2 召回仍取决于模型评分。
- 原始报告：`report.hiapi-backend-pr-333-glm54-1800{,b,c}.json`。
