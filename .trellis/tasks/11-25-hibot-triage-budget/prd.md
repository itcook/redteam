# triage 整轮预算按 PR 规模动态估算（2026-11-25）

## 背景

`hibot` 的 triage 整轮 deadline 目前恒等于 manifest `limits.wall_clock_s`（eval 场景 1800s）：
`src/job/run.rs:444` 调 `effective_deadline(started, manifest.limits.wall_clock_s, None)`。
现有 `src/triage/budget.rs` 只在**单 cell / I0 包线**层面按规模放大，没有按 PR 规模估算整轮。

用户要求：整轮预算按 PR 规模（`title + body + linked issues` 文本 + `diff/files`）动态估算。
规模只决定**包线**，跑得快就提前结束；manifest 仍是硬上限。

## 目标

- 新增一个纯函数（`triage::budget::total_estimate_s`），由 PR 文本规模
  （title + body + linked issues 的 title/body）、diff 字节与文件数给出整轮预算秒数。
- triage 分支的 deadline 改为 `effective_deadline(started, manifest.wall_clock_s, Some(estimate))`
  （即 `min(manifest, started + estimate)`），仍以**原始 `started`** 计算绝对时刻。
- 单测覆盖：小 PR 估算取 `min(manifest, 900)` 下限；大 PR 接近/等于 manifest；`estimate > manifest` 时取 manifest；
  空 diff 同样受下限与 manifest 约束；review 路径不受影响。

## 非目标

- 不改 `summary.routing.wall_clock_s`（那是给下游 review 的建议，按 risk 三档，`tally.rs:404`）。
- 不预测模型实际耗时；估算只是上限。提前完成照常提前结束。
- 不改超时/中断语义：仍写 `partial` + 原因，`status` 判定表不变。
- 不碰 `src/config/`（另一 session 正在重写 config v2）、`src/fixverify/`、Phase 4/5/7、`~/hibot`、`dev`、入库二进制。
- 不新增配置块；不引入运行期调度模拟（gate 排队/429 收敛）。

## 现状与证据

- 数据、口径、缺口、公式候选与 5 PR 对照见
  `evidence/budget-candidates.md`；计算脚本 `evidence/budget_estimate.py` 可离线复跑。
- 关键事实：
  - 5 个真实 PR 的 DeepSeek ok 轮整轮 116–172s；GLM-5.4 在 backend #333 唯一 ok 轮 1050.1s（1800s + 1.5×）。
  - 整轮墙钟 ≈ 最慢轴 + 收口（GLM 三轴 217/618/1050s，整轮 1050s），轴时间不可相加。
  - 5 个 DeepSeek 样本未呈现随规模单调增长，所以估算只能是包线。
  - 历史 provider gate cap = 4（`extensions.redteam.concurrency`）。
- 证据缺口（已披露，不补造）：backend #333 的 DeepSeek 164.1s 报告缺失，仅存于 `metrics.phase3-8.jsonl`；
  旧报告 `axes[].elapsed_ms` 未记录；GLM 只有一个成功点；partial 样本不当成功时间。

## 已批准决策（2026-11-25，用户裁决）

1. **公式**：B2（规模连续保守档）
   `220 + 7.5×diff_kb + 7.5×files + 2.5×text_kb`。
2. **下限**：`min(manifest, 900)`。
3. **报告**：写入 `extensions.redteam.budget.estimate_s`。
4. **验证方式**：单测 + 归档脚本对拍，不重跑模型。
大 PR 顶到 manifest 是包线语义的自然结果（非独立批准项）；实现按上述参数，不再改动系数。

## 批准后的实现范围

- `src/triage/budget.rs`：新增 `total_estimate_s(...)` 纯函数 + 单测（常量、clamp、下限、manifest 截断）。
- `src/job/run.rs` triage 分支：解析出 `pr` / `changes` 后计算 estimate，传给 `effective_deadline` 的
  `suggested_s` 参数；其余不动。
- 如需写报告：`src/triage/tally.rs` 或报告组装处的 `extensions.redteam.budget` 增加一个字段。
- 不新增依赖。

## 验收

- `make verify` 通过（生成物同步 + packaging/version 测试；**不含 fmt/clippy**）。
- `cargo test` 相关用例通过，新增用例覆盖上述边界。已知 `job_conformance` 的 partial fixture 在全量并行时有既有 flaky（单跑与基线均通过，详见 `implement.md`）。
- 用 5 个归档 PR 的真实输入（`diff.patch` + `pr-context.json`）复核：`tests/triage_budget_archive.rs`
  断言 1313 / 1027 / 1800 / 1470 / 1133，与 `evidence/budget_estimate.py`（ceil）一致。
- `git diff --stat` 只含上述文件的改动。
