# U3 · Phase 2 验收证据

- 分支：`feature/hibot-audit-exec`
- 基线：`9f27a4a`（Phase 1 归档）
- 证据提交：`cb0ab1f`（本次日志在其后一轮修正前生成；最终提交见 `git log`）
- 工作树：clean

## 命令与退出码

| 命令 | 退出码 | 日志 |
|---|---|---|
| `cargo test` | `0` | `cargo-test.log` |
| `make verify` | `0` | `make-verify.log` |

## 测试计数（cargo test，见日志原文）

| 目标 | 结果 |
|---|---|
| lib | 1072 passed |
| bin | 21 passed |
| `capabilities` | 4 passed |
| `job_conformance` | 22 passed |
| `packaging` | 7 passed |
| `review_e2e` | 24 passed |
| doc-tests | 0 |

`make verify` 内含 `gen` + `schema` 与两处 `git diff --exit-code`（codex-skills / `dist/config.schema.json`），
退出码 0 = 生成物与代码零 diff；日志尾部为 packaging 7/7。

## 说明

- 日志为完整 stdout+stderr 重定向；退出码由外层 shell 记录在本文件（日志本身不含退出码）。
- 已知限制：`build_registry_with` 无注入耗时的测试钩子，"准备工作耗尽预算 → backend 零调用" 只有
  谓词单测（`past_deadline_means_exhausted`）与"在途调用被预算取消"的集成用例
  （`timeout_partial_report_keeps_run_facts_and_corrected_quote`），没有确定性的"准备阶段超时"集成用例。
