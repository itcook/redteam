# U3 · Phase 2 验收证据

- 分支：`feature/hibot-audit-exec`
- 基线：`9f27a4a`（Phase 1 归档）
- **受测提交**：`c92e0482189bdfa8bba4c803253e8d16f9a3e92f`
- 运行前工作树状态：**clean**（`git status --porcelain` 无输出；日志生成后才产生证据文件改动）
- 日志文件：`cargo-test.log`、`make-verify.log`（完整 stdout+stderr）

## 命令与退出码

| 命令 | 退出码 |
|---|---|
| `cargo test` | `0` |
| `make verify` | `0` |

## 测试计数（见 `cargo-test.log` 原文）

| 目标 | 结果 |
|---|---|
| lib | 1073 passed |
| bin | 21 passed |
| `capabilities` | 4 passed |
| `job_conformance` | 23 passed |
| `packaging` | 7 passed |
| `review_e2e` | 24 passed |
| doc-tests | 0 |

`make verify` 内含 `gen` + `schema` 与两处 `git diff --exit-code`（codex-skills / `dist/config.schema.json`）：
退出码 0 = 生成物与代码零 diff；日志尾部为 packaging 7/7。

## 说明

- 日志与 SUMMARY 是一笔**仅文档**的后续提交，不改变受测源码。
- 覆盖边界：准备阶段超预算由 `tests/job_conformance.rs::budget_exhausted_during_prep_makes_no_backend_calls`
  用 `BackendOverride` 的同步耗时模拟（无需生产测试钩子）。

## 逐项验收对照（实现 → 证据）

| 项 | 实现 | 关键测试（`tests/job_conformance.rs` / 单测） |
|---|---|---|
| 2.0 数据流 | `src/job/run.rs` 顶部注释 + 本任务 PRD | — |
| 2.1 输入真相源 | `src/job/projection.rs`、`src/evidence/git.rs::split_diff_by_file` | `review_uses_diff_patch_not_git_as_change_source`（patch 与 git 故意不同，scan prompt 仍含 patch 独有标记）；`projection::tests::*` |
| 2.2 投影 | `run.rs::review_args`（唯一构造点）、`repo::ensure_complete` | `head_must_match_manifest_head_sha`、`merge_base_must_be_a_full_sha`、`repo::tests::non_sha_revisions_in_manifest_are_rejected` |
| 2.3 prior_report | `src/job/prior.rs`、`cli::spec_budget_bytes` / `render_spec_section_reporting` | `prior_report_only_adjusts_lens_and_budget`、`run::tests::prior_routing_cannot_change_blocking_thresholds`、`prior::tests::specs_share_the_production_budget_cap` / `pem_content_is_rejected_*` |
| 2.4 dismissed | `src/job/dismissed.rs`、`report::map_finding`、`cli::stage0_risk` job 守卫 | `dismissed_is_audited_but_never_refuted`（含 REFUTED 并存）、`malformed_dismissed_csv_is_an_error`、`dismissed::tests::*` |
| 2.5 quote 逐字 | `positioning::verbatim_span`、`job::quote` | `quote_is_corrected_from_the_checkout_before_fingerprint`、`unverifiable_quote_is_dropped_and_makes_partial`、`positioning::tests::report_quote_with_correct_lines_but_wrong_whitespace_is_rejected`（独立区间校验器）、CRLF / 多字节结尾 |
| 2.6 repo 完整性 | `src/job/repo.rs`、`evidence::git::{resolve_commit,merge_base_of,diff_available}` | `missing_blob_produces_valid_error_report`、`shallow_checkout_produces_valid_error_report`、`unrelated_histories_produce_valid_error_report`、`repo::tests::*` |
| 2.7 语言 | `prompt` 语言槽 + `status` 文案（Phase 1 已铺） | `language_is_consistent_across_prompts_findings_and_status_reason`（scanner/reviewer/refuter 请求 + finding title/body/recommendation + refuter 判决正文 + status_reason；warnings 不随语言） |
| P2 验真回退 | `Config::refute_enabled`（两个 RefuteInput 构造点）、`report` 披露 | `job_mode_without_refuter_skips_refute_and_discloses`、`job_mode_with_explicit_refuter_runs_refute`、`config::tests::job_mode_without_refuter_skips_refute_but_local_falls_back` |
| 出口 capabilities | `capabilities/redteam.json` | `tests/capabilities.rs`（kinds 只含 review、version 同源、契约形状） |
| 中断路径 | `lifecycle::interrupted_report` 共享 `ReviewExtras` | `timeout_partial_report_keeps_run_facts_and_corrected_quote`、`lifecycle::tests::interrupted_report_shares_the_finalize_chain_and_run_facts` |
| 预算（含准备阶段） | 绝对 deadline + 派发前二次检查 | `budget_exhausted_during_prep_makes_no_backend_calls`、`run::tests::*deadline*` |

## 验证范围与已知限制

- 2.7 的语言证据来自**确定性 Fake 后端**：证明语言指令被传进三类模型请求、模型产出按配置语言原样落地，
  **不是**真实 provider 的语言遵从率验证。
- patch 的 **Git 引号转义路径**（如路径含空格/非 ASCII 时 `diff --git "a/x y" "b/x y"`）当前不支持：
  `projection::from_patch` 会返回 `input_schema_mismatch:diff_patch`（不静默并文件）。
- `prior_report.routing.specs` 的"已用"以**实际 renderer 接纳结果**为准；预算截断未注入的不计入
  `specs_used`，也不冒充"不可读"。
- 未做（禁区）：Phase 3 triage / Phase 4 verify 分析、`state.json` 心跳、发布面。
