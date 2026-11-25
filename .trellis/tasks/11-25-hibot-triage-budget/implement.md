# 实施记录（2026-11-25）

用户裁决：**B2 规模连续保守档**；下限 `min(manifest, 900)`；写入 `budget.estimate_s`；验证方式为单测 + 归档脚本对拍。

## 变更

- `src/triage/budget.rs`
  - 新增整轮常量（B2）：`TOTAL_BASE_S=220`、`TOTAL_PER_KB_S=7.5`、`TOTAL_PER_FILE_S=7.5`、
    `TOTAL_PER_TEXT_KB_S=2.5`、`TOTAL_MIN_S=900`。
  - 新增 `total_estimate_s(diff_kb, files, text_kb, manifest_cap_s)`：
    `clamp(base + 7.5×diff + 7.5×files + 2.5×text, min(manifest, 900), manifest)`，向上取整。
  - 单测 2 个：五点校准（1312 / 1027 / 1800 / 1470 / 1133）+ 下限与 manifest 截断。
- `src/triage/mod.rs`
  - `TriageRequest` / `TriageOutcome` 新增 `estimate_s`。
  - 新增 `total_estimate(pr, changes, cap)`：文本 = `title + body + linked issues(title/body)`；
    diff 字节与文件数只计**非空 diff**（与 `patch_pairs` 一致）。
  - 单测 2 个：口径（title/issue 计入、空 diff 过滤）+ 恒 ≤ manifest / 取下限。
- `src/job/run.rs`：triage 分支先算 `estimate_s`，再
  `effective_deadline(started, manifest.limits.wall_clock_s, Some(estimate_s))`（仍以原始 `started` 计算绝对时刻）。
- `src/job/report.rs`：`extensions.redteam.budget` 增加 `estimate_s`。

## 未改（边界）

- `summary.routing.wall_clock_s`（`tally.rs` 的 risk 三档派生）。
- 超时/中断语义：仍写 `partial` + reasons，`status` 判定表不变。
- `src/config/`、`src/fixverify/`、Phase 4/5/7。

## 验收证据

- `make verify`：PASS（`evidence/make-verify.log`）。
- `cargo test`：PASS（lib 1140 + 集成 21/4/25/7/24/9/3/4，`evidence/cargo-test.log`）。
- 对拍：`evidence/budget_estimate.py` 的 B2 列与实现单测一致（见 `evidence/budget-candidates.md` 第 5 节）。
