# Phase 6 证据索引（A–D）

> 基线 `bc71ac3` · 分支 `feature/hibot-audit-exec` · 2026-11-25
> 协调者 `01a20ba0`（U8）。本目录只放证据与交付包，代码与脚本在仓内。

## 提交

| 提交 | 内容 |
|---|---|
| `e2ee553` | feat(capabilities)：`invoke` 改 kind 通用形状 + 契约钉子 |
| `bd0160d` | test(acceptance)：18 组合验收矩阵 + hibot schema 证据脚本 |
| `aae1626` | docs(cli)：job 模式配置/凭据/语言/输出/退出码；修 `--help` 的 Phase 1 残留 |
| `b21b7de` | test(acceptance)：兼容示例 job 的 fallback 准备脚本（非产品路径） |
| `7d22c97` | chore(task)：Phase 6 交付记录与证据 |
| `59ff3de` | chore(task)：archive 11-25-hibot-phase6-acceptance |
| `6eb31f5` | fix(job)：中断报告不再重复 `status_reason`；`usage.wall_clock_ms` 改用 job 级 elapsed |

## A. capabilities（计划 §9.1）

- `capabilities/redteam.json`：`invoke = "redteam <triage|review|verify> --job-dir <dir>"`；其余字段不变。
- `tests/capabilities.rs`（7 项）：kinds 恰三值且顺序稳定 · version==CARGO_PKG_VERSION ·
  config_schema.schemaVersion==`dist/config.schema.json` · `requires` **精确等于**
  `{repo_checkout:true, pr_context:false, network:"provider-only"}` · `limits.max_wall_clock_s ≥ 1` ·
  `invoke` 覆盖三 kind 且带 `--job-dir`。
- 证据：
  - `evidence/A-capabilities/schema-check.log`：`PASS capabilities/redteam.json (capabilities-1.json)`
  - `evidence/A-capabilities/cargo-test.log`：7 passed
  - `make verify`：零 diff + packaging 7 + version 4 全绿

## B. 验收矩阵（计划 §9.4）

`tests/acceptance_matrix.rs`：**3 kind × 3 夹具 × 2 语言 = 18 组合**，真实 git 检出 + 真实 job 目录 +
`run_with` + 确定性 backend。夹具语义见文件头表（clean / known-vuln / partial 按 kind 落到具体能力）。

六条契约自动项：①报告形状/必填 ②指纹 `fingerprint::of` 复算 ③quote 逐字命中（verify 可选 quote 命中即校验）
④status 只看 coverage/axes/counts ⑤verify 输入指纹集合 == 输出集合 ⑥无 cost + 附录 D 键形状 + `state.json` 合法。
语言成对断言：prompt 语言指令随配置；partial 的 `status_reason` 与 triage 合成 finding 标题随语言；
机器字段（`status_code` / `concurrency` / `budget` / `template_hashes` / `verify.counts` / `warnings`）跨语言相等。

- 证据：`evidence/B-matrix/run.log`、`evidence/B-matrix/out/schema-check.log`（**37 PASS / 0 FAIL**：capabilities + 18 报告 + 18 心跳）
- 复跑：`HIBOT_SCHEMA_DIR=~/hibot/.docs/audit/schema scripts/acceptance-matrix.sh`
- 单测：`cargo test --test acceptance_matrix` → 3 passed（18 组合，约 7s）

## C. 手工 job 目录联调（计划 §9.3 修订版）

- 官方脚本：`~/hibot/.docs/examples/audit-pr-468/prepare-job.sh`（hibot `91834c2` 修复后）
- 本机 `~/hiapi/backend` 是 shallow；按脚本文档用 `HIBOT_BACKEND_REPO` 指向 blob-filtered 非 shallow 克隆。
- 三个 job：`target/phase6-official/<kind>/{job,repo}`；配置用 v2 job 配置（deepseek-flash，凭据 env-only）。
- 预检（`evidence/real-run/preflight/preflight.log`，3 kind × 8 项全 PASS）：`repo/.git` 存在、HEAD==manifest、
  base/head 对象可查、diff 首行 `diff --git`、diff sha 与 manifest 一致、`git status` 空、非 shallow。
- 真跑（每 job ≤600s；`evidence/real-run/run-one.sh` 记录墙钟、退出码、三帧心跳）：

| kind | status | 墙钟 | 产出 | usage(in/out/cached) | 证据目录 |
|---|---|---|---|---|---|
| triage | ok | 40s | 0 findings | 35968 / 11985 / 26240 | `evidence/real-run/triage/` |
| verify | ok | 9s | 2/2 resolved | 10486 / 2550 / 10109 | `evidence/real-run/verify/` |
| review | partial（`wall_clock_exhausted`） | 600s（外部上限，SIGTERM 收敛） | 13 findings | 231096 / 390305 / 176256（`usage.wall_clock_ms`=598987） | `evidence/real-run/review/` |

- review 真跑在**外部 600s 上限**被 SIGTERM：引擎按契约收敛出合法 `partial` 报告（11 findings，
  schema/指纹/quote 全过，`status_code=wall_clock_exhausted`），说明中断路径可用。
- 校验：`python3 evidence/real-run/check-real.py --root evidence/real-run --jobs target/phase6-official --schema-dir ~/hibot/.docs/audit/schema --checker scripts/hibot-schema-check.py`
  → `evidence/real-run/check.log`（**87 PASS / 0 FAIL**：schema + 指纹复算 + quote 逐字 + verify 覆盖 + 心跳三帧）。
- 交付包（给 hibot 的一页）：`delivery/hibot-handoff.md`。

### 示例数据的三个缺陷（已由 hibot `91834c2` 修复）

| # | 缺陷 | 执行单元表现 | 修复 |
|---|---|---|---|
| 1 | diff_patch 无 `diff --git` 头的节选 | `input_schema_mismatch:diff_patch` | 真实检出里 `git diff base..head` + 重算 sha |
| 2 | paths.repo 是 `git archive` 树（无 .git） | `repo_incomplete:head` | `git worktree add --detach` |
| 3 | `pr.base_sha` 是假 sha | `repo_incomplete:base` | 钉成真实 merge-base |

兼容工具（保留作 fallback/对照，不作主证据）：`scripts/prepare-hibot-example-fallback.sh`。

### 真跑暴露的两条缺陷（已修复，`fix(job)`）

1. **SIGTERM / 预算耗尽路径 `status_reason` 重复拼接**：`partial_report` 把已渲染的 `StatusText.reason`
   塞回 `JobFailure.reason` 覆盖，`status_text` 再拼一次 → `…results flushed: …results flushed`。
   修复：`partial_report` 只拆回 code/detail 交给 `render_partial` 重新渲染（不再覆盖 reason）。
   测试：`job::report::tests::partial_report_*`（en/zh、带 detail、多原因）；job_conformance 的 SIGTERM 用例。
2. **中断路径 `usage.wall_clock_ms` 取「最长 cell 时长」**：本次修复前 442775ms，而 job 墙钟 600s。
   修复：`interrupted_report` 增加 `wall_clock_ms` 参数，由调用方传 `started.elapsed()`，与正常路径同口径。
   测试：`interrupted_report_*` 单测（4321/9876 断言）+ SIGTERM 集成用例（`(20..10_000)` 区间，变异验证会红）。

修复后重跑 review（同一官方 job）：`status=partial`、`status_reason` 单句、`usage.wall_clock_ms=598987`（≈600s），
13 findings 全部过 schema/指纹/quote，`check.log` 87 PASS / 0 FAIL。

## D. 文档收尾（计划 §9.5）

- README「作为 hibot 执行单元」新增：配置投递（`paths.config`、无 `--config` flag）、凭据 env-only、
  报告语言（过程+结果随 `settings.language`，机器字段不随）、输出（`report.json` / `state.json`）、
  退出码（0/1/2）、临时限制（runner 未接、verify 只核验 anchored finding、refute 披露、无 cost）。
- `--help`：triage/verify 去掉「Phase 1 仅命令面」，改为现行行为；`config schema` 的 `$id` 由 `/1` 改为 `/2`。

## 门禁原文

- `make verify`：`sync-version --check` 通过、gen/schema 零 diff、packaging 7 passed、version 4 passed。
- `cargo test`：lib 1183 + bin 21 + acceptance_matrix 3 + capabilities 7 + job_conformance 26 +
  packaging 7 + review_e2e 24 + triage_conformance 10 + triage_prior_report 3 + verify_conformance 15 +
  verify_fixtures 4 + version 4，**全绿 0 失败**（`evidence/cargo-test.log`）。

## 未做（按禁区）

- 不实现 hibot runner、不做 release、不重建入库二进制、不改 config schema / schemaVersion / kinds 语义。
- 不为兼容示例放宽 diff 解析（headerless unified diff 仍按 `input_schema_mismatch:diff_patch` 拒绝）。
