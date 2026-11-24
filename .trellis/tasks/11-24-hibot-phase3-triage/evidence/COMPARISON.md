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
