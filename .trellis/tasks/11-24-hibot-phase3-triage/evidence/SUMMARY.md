# U4 · Phase 3 triage 验收证据

- 分支：`feature/hibot-audit-exec`
- **受测提交**：`28f3181`（3.1–3.10 的代码与测试；本目录的日志/对拍证据随后以独立 docs 提交加入）
- 运行前工作树状态：代码提交后 clean（日志与报告是随后新增的证据文件）
- 日志：`cargo-test.log`、`make-verify.log`（完整 stdout+stderr）

## 命令与退出码

| 命令 | 退出码 |
|---|---|
| `cargo test` | `0` |
| `make verify` | `0` |

## 测试计数（见 `cargo-test.log` 原文）

| 目标 | 结果 |
|---|---|
| lib | 1132 passed |
| bin | 21 passed |
| `capabilities` | 4 passed |
| `job_conformance` | 23 passed |
| `packaging` | 7 passed |
| `review_e2e` | 24 passed |
| `triage_conformance` | 9 passed |
| `triage_prior_report` | 3 passed |
| doc-tests | 0 |

## 逐项验收对照

| 项 | 实现 | 关键测试 |
|---|---|---|
| 3.1 输入 | `src/triage/input.rs`、`src/job/run.rs::run_triage`；缺 `pr_context`/`diff_patch`/`merge_base` → error | `triage_conformance::missing_pr_context_is_an_error_report`、`input::tests::*` |
| 3.2 StandardsMap | `src/triage/standards.rs`（doc→linter→peer；no-standard 仅三者皆无；越界/二进制拒收） | `standards::tests::resolves_doc_first_then_linter_then_peer_and_no_standard` 等 4 条 |
| 3.3 三轴 | `src/triage/lenses.rs` + 5 个模板；安全轴 7 面、conf<8 丢弃、无 source→sink 路径封顶 7；意图两阶段（I0 claims/overlap + I1..In diff 切片）；三轴取 `roles.reviewer` | `surfaces_and_tier2_whitelist_match_the_human_baseline`、`shard_formulas_are_the_grill_ones`、`triage_conformance::triage_ok_*`、`intent_slice_delivers_claims_and_discloses_counts`、`unfinished_intent_slices_do_not_create_intent_blockers` |
| 3.4 去重 | 安全/漂移 file+归一 quote+hint；意图按声明身份；无锚意图项不并入代码行 | `tally::tests::dedupe_is_per_axis_and_intent_without_anchor_is_not_merged_into_code_rows` |
| 3.5 tally | 单点实现；tier2 白名单反例；risk/fit/routing/confidence | `tier2_only_for_surfaces_1_3_4_at_conf_8_and_sql_injection_is_not_promoted`、`risk_and_fit_tables_cover_every_rule`、`summary_is_the_nine_keys_plus_nested_routing` |
| 3.6 blockers | 只披露；`findings`/`blockers` 形状不同；review 不读 blockers | `findings_and_blockers_are_distinct_arrays`、`triage_conformance::review_projection_reads_routing_only_not_blockers`、`triage_prior_report::prior_report_does_not_change_the_finding_set` |
| 3.7 输出 | `report::triage_report`；summary 九键 + 内嵌 routing；partial 不写 summary；axes 按轴 dropped/dropped_rows | `triage_conformance::triage_ok_has_nine_keys_nested_routing_and_scanner_findings`、`axis_failure_is_partial_named_and_writes_no_summary`、`below_threshold_and_missing_reach_rows_are_dropped_and_disclosed` |
| 3.8 对拍 | `evidence/COMPARISON.md` + `metrics.jsonl` + `report.*.json`（8/10 次真实跑） | 见下节 |
| 3.9 回归 2.3 | 真 triage 报告 fixture（cli #18）驱动 review | `triage_prior_report` 3 条 |
| 3.10 capabilities | `kinds=["review","triage"]`；`requires.pr_context=false` | `tests/capabilities.rs` |

## 3.8 真实对拍摘要

- 5 个基线 PR：sites #277 / #235 / #328、cli #18、backend #333（tier2）。
- 另有 1 次 sites #333 冒烟（无人工基线）；backend #333 多轮（A、GLM infra、A2、A 流程 A/B）。
- **A 流程 A/B**：DeepSeek high 在 backend #333 为 177.7s / ok / tier1 / blockers0（人工 tier2 / blockers0），
  intent 20/20 claims complete；sites #328 为 114.8s / ok / tier0 / blockers2。
  GLM-5.3 high 仍 too slow（I0 150s 超时 / slice 166s 超时，partial）。
  GLM-5.4 provider 修复后已跑通：backend #333 424–451s partial（I0 20 claims，slice 超时，9/20 delivered），
  sites #328 397s partial（security 完整、drift 2/4、intent 11 claims/4 delivered）；clean PR 仍无 tier2。
- **无「干净判 tier 2」**：4 个 tier0 PR 全部 tier0。
- **tier2 漏报 1 例**：backend #333 人工 tier2，我们 tier0（DeepSeek A/A2 均 0 行；GLM 那次 infra 失败）。
- 丢弃率：backend #333 A 轮安全轴 1 行候选、1 行 <8 被丢（100%）；A2 后 0 行候选。
- 逐条差异见 `COMPARISON.md`；原始 usage/axes 见 `metrics.jsonl`。

## 反例

- **tier2 误升**：面 5（SQL 注入）conf=10 仍判 tier1（单测硬断言）。
- **无锚意图项被误并**：意图 blockers 无 `file` 字段，不与安全/漂移行合并（单测）。
- **blockers 全量透传**：findings 5 / blockers 2 时为两个形状不同的数组；review 的 prior 投影不含 blockers。

## GLM infra 失败的落账（协调者要求）

该次报告 `status=partial`，`status_code=triage_axis_failed:intent`（`status_reason` 同时含墙钟耗尽）；
intent 轴 `{ok:false, rows:0, error:"structured-output parse failed: finish_reason=max_tokens, len=0"}`；
没有静默丢行——解析失败没有产生任何 claim 行，轴级 error 与计数都在 `extensions.redteam.axes` 里。

## 已知限制

- `risk` 的 `blockers≥3 → high` 是 grill 定案的 `[拟议]` 规则，与人工的 medium/high 判断有系统性差异（已记入 COMPARISON）。
- 真实对拍主模型为 DeepSeek Flash；GLM-5.3 在 A 流程下仍无法在 1200s 内完成（I0/slice 超时）；
  GLM-5.4 已可调用，但 slice 验证仍超时（partial；剩余预算未用于重试）。
- `triage` 现在取 `roles.reviewer`（2026-11-24 用户调整）；`config check --kind triage` 也要求 reviewer。
- 3.8 的报告是单次运行；人工基线中 sites #328 本身有 3 次不同结论（blk1/1/4），差异表按全部列出。
- `dropped_rows` 是诊断披露，不是契约必需字段；每轴最多 20 条样本。
