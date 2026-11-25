# 实施记录（2026-11-25）

用户裁决：**B2 规模连续保守档**；下限 `min(manifest, 900)`；写入 `budget.estimate_s`；验证方式为单测 + 归档脚本对拍。

## 变更

- `src/triage/budget.rs`
  - 新增整轮常量（B2）：`TOTAL_BASE_S=220`、`TOTAL_PER_KB_S=7.5`、`TOTAL_PER_FILE_S=7.5`、
    `TOTAL_PER_TEXT_KB_S=2.5`、`TOTAL_MIN_S=900`。
  - 新增 `total_estimate_s(diff_kb, files, text_kb, manifest_cap_s)`：
    `clamp(base + 7.5×diff + 7.5×files + 2.5×text, min(manifest, 900), manifest)`；
    先把浮点包线 `ceil` 成 `u64` 再与 manifest 取 min（不把 u64 manifest 转 f64）。
  - 单测 2 个：五点校准（1313 / 1027 / 1800 / 1470 / 1133，输入为归档字节数）+ 下限与 manifest 截断
    （含 `u64::MAX` 与 `f64::MAX` 边界）。
- `src/triage/mod.rs`
  - `TriageRequest` / `TriageOutcome` 新增 `estimate_s`。
  - 新增 `total_estimate(pr, changes, cap)`：文本 = `title + body + linked issues(title/body)`；
    diff 字节与文件数只计**非空 diff**（与 `patch_pairs` 一致）。
  - 单测 2 个：逐项文本字段（6KB UTF-8 title / 6KB issue title / 50KB issue body）的贡献与空 diff 过滤；
    恒 ≤ manifest / 取下限。
- `src/job/run.rs`：triage 分支先算 `estimate_s`，再
  `effective_deadline(started, manifest.limits.wall_clock_s, Some(estimate_s))`（仍以原始 `started` 计算绝对时刻）。
- `src/job/report.rs`：`extensions.redteam.budget` 增加 `estimate_s`；单测覆盖正常与 partial 两条路径。
- `tests/triage_budget_archive.rs`：读 `tests/fixtures/triage-budget/` 的 5 份历史 patch / pr-context，走
  `projection::from_patch` → `input::parse` → `triage::total_estimate`，断言 1313 / 1027 / 1800 / 1470 / 1133。
  夹具路径稳定，不依赖 Trellis 任务目录（归档后测试仍可跑）。

## 未改（边界）

- `summary.routing.wall_clock_s`（`tally.rs` 的 risk 三档派生）。
- 超时/中断语义：仍写 `partial` + reasons，`status` 判定表不变。
- `src/config/`、`src/fixverify/`、Phase 4/5/7。

## 验收证据

- `make verify`：PASS（生成物同步 + packaging/version；**不含 fmt/clippy**）。
- `cargo test`：最后一次全量 **PASS**（lib 1141 + 集成全部通过，`evidence/cargo-test.log`）。
  review 的 `job_conformance::partial_fixture_uses_missing_provider_key_and_reports_partial` 曾多次间歇性失败；
  **根因尚未定位**。单独跑该测试、单独跑 `--test job_conformance`（25/25）、以及暂存对照（`a67ae1d`，
  仍含 B2 实现、非原始基线）均通过。失败与对照日志见 `evidence/cargo-test-flaky-attempt1.log`、
  `cargo-test-baseline.log`、`cargo-test-job-conformance-only.log`。
- 对拍：`evidence/budget_estimate.py` 用真实字节 + `ceil`，B2 列与 Rust 单测/集成测试一致
  （1313 / 1027 / 1800 / 1470 / 1133；见 `evidence/budget-candidates.md` 第 5 节）。
- 格式检查（已观察事实）：`cargo fmt --check` 返回 1，包含本次未修改文件的格式差异；未做全仓格式化。
