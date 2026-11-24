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
