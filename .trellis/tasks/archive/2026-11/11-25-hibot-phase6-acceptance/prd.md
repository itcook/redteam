# hibot Phase 6：能力声明、验收矩阵、手工联调、文档收尾

> 基线 `bc71ac3` · 2026-11-25 · 协调者 `01a20ba0`（U8）
> 依据：`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §9（9.1 / 9.3 修订版 / 9.4 / 9.5）

## 前提（已就绪，不再改动）

- 三 kind（triage / review / verify）全部实现；config v2.0.1 冻结（`schemaVersion=2`，schema sha `246d2c97…514a`）。
- hibot 提供 `~/hibot/.docs/examples/audit-pr-468/prepare-job.sh`（依赖 `~/hiapi/backend`）。
- **runner 真调不可用**（hibot `apps/` 为空，M0/M1 未开始）→ 本单元只做**手工 job 目录联调**，runner 不进验收。

## A. capabilities 全量（9.1）

- `capabilities/redteam.json` 的 `invoke` 改为 kind 通用形状：`redteam <triage|review|verify> --job-dir <dir>`。
- 过 hibot `audit-capabilities.schema.json`（本地 python jsonschema，证据落盘）。
- `tests/capabilities.rs` 钉子补齐：
  1. `kinds` 恰为 `["review","triage","verify"]`（顺序稳定）；
  2. `version == CARGO_PKG_VERSION`（已有，保留）；
  3. `config_schema.schemaVersion == dist/config.schema.json` 的 `schemaVersion`（已有，保留）；
  4. `requires` **恰为** `{repo_checkout:true, pr_context:false, network:"provider-only"}`（键集合精确，无多余键）；
  5. `limits.max_wall_clock_s >= 1`；
  6. `invoke` 覆盖三 kind 且带 `--job-dir`。

## B. 验收矩阵（9.4）——fake backend，可复跑，进 `cargo test`

新增 `tests/acceptance_matrix.rs`：**3 kind × 3 夹具（clean / known-vuln / partial）× 2 语言 = 18 组合**，
每组合经 `run_with` + 确定性 backend 跑真实 job 目录，产出 `out/report.json`。

夹具语义（按 kind 落到具体能力，测试文件内注明）：

| 夹具 | review | triage | verify |
|---|---|---|---|
| clean | 无 finding，`ok` | 三轴空，`ok` + summary | findings 为空，`ok` + 空 verifications |
| known-vuln | SQL 注入 finding（quote 逐字） | security 行 → blocker + finding | 一条 finding 判决 `unresolved` |
| partial | 第二 scanner provider 不可用 → `scan_incomplete` | security 轴解析失败 → `triage_axis_failed:security` | provider 失败 → `inconclusive` → `verifications_inconclusive:1` |

契约自动项 6 条，逐项断言：

1. **报告形状/必填字段**：kind 对应 schema 常量、`job_id/kind/status/input/executor/findings/usage/extensions` 齐备。
2. **指纹复算**：finding 的 `fingerprint == fingerprint::of(file, quote, title)`；verify 的
   `finding_fingerprint` 必须在输入指纹集合内。
3. **quote 逐字命中**：review/triage 的 `findings[].quote` 在检出原文中逐字出现；verify 的
   `verifications[].quote`（可选）出现时同样校验。
4. **status 判定表**：ok/partial 与 coverage/axes/counts 一致（不看模型输出）；`ok` ⇒ 无 `status_reason`/`status_code`；
   `partial` ⇒ 两者都有且 `status_code` 是附录 E 的主导码。
5. **verify 覆盖不变量**：输入指纹集合 == `verifications[].finding_fingerprint` 集合；空输入才允许空输出。
6. **无 cost / 附录 D 键形状 / state.json**：`usage` 无 cost；`extensions.redteam` 的
   `language{value,source}` / `concurrency{mode,provider_caps}` / `budget{wall_clock_s,truncated_items}` 恒在，
   各 kind 专属键（triage `axes`/`blockers`/`template_hashes.triage`；verify `verifier`/`verify.*`/`template_hashes.fix_verify`；
   review `run.template_hashes`/`input_consistency`）齐备，废弃键（`repo_degradation` / `verified` / `weak_evidence`）
   不出现；`out/state.json` 满足 `audit-state` 结构（schema/job_id/phase/at，phase ∈ preparing|running|finishing）。

语言：断言 **prompt 里的语言指令**（`Simplified Chinese (简体中文)` / `English`）随配置进入三 kind，
`status_reason` 文案随 language（zh 含中文、en 含英文模板），机器字段（`status_code` / `concurrency` /
`budget` / `template_hashes` / verify `counts`/`warnings`）不随 language 变化。

另加 `scripts/acceptance-matrix.sh`：跑矩阵 + `HIBOT_SCHEMA_DIR` 存在时用 hibot schema 校验（缺失跳过），
证据写任务 `evidence/` 目录。

## C. 手工 job 目录联调（9.3 修订版）——真实 provider，限量

- 用 `prepare-job.sh <kind> <dir>` 生成 review / triage / verify 三个 job 目录。
- `redteam <kind> --job-dir <dir>` 用本机已有 provider 真跑（deepseek-flash 为主）。
- 边界：每 job 墙钟 ≤600s，总真跑 ≤30 分钟；失败先报协调者，不反复重试。
- 每份 report 过 hibot schema（`$id` registry）+ 指纹复算 + quote 逐字 + verify 覆盖；抽 3 帧 `state.json`。
- 产物：命令 + job dir + report + 校验输出 → 任务 `evidence/real-run/`；给 hibot 一页交付包。

## D. 文档收尾（9.5）

- README「作为 hibot 执行单元」补：三 kind、`--job-dir`、配置投递（`paths.config`、无配置 flag）、
  凭据 env-only、`language`、`state.json`、退出码、临时限制。
- `--help` 核对：`triage`/`verify` 的「Phase 1 仅命令面」已过期；`config schema` 的 `$id` 仍是 `/1`（现为 `/2`）。
- 报告语言说明：过程（prompt）与结果（`status_reason` / verify 证据）随 `settings.language`，机器字段不随。

## 不做（禁区）

- 不实现 runner；不做 release；**不重建入库二进制**；不改 config schema / `schemaVersion` / kinds 语义。
- `~/hibot` **只读**（`prepare-job.sh` 只写临时目录）；不改 `dev`。
- 发现产品 bug 先报协调者，不顺手改（本单元只做验收/文档/测试）。

## 交付记录（2026-11-25）

| 提交 | 内容 |
|---|---|
| `e2ee553` | `feat(capabilities)`：invoke 改 kind 通用形状 + 契约钉子 |
| `bd0160d` | `test(acceptance)`：18 组合验收矩阵 + `scripts/acceptance-matrix.sh` + `scripts/hibot-schema-check.py` |
| `aae1626` | `docs(cli)`：job 模式配置/凭据/语言/输出/退出码 + `--help` 修 Phase 1 残留 |
| `b21b7de` | `test(acceptance)`：兼容示例 job 的 fallback 准备脚本（非产品路径）+ `--help` 完整头部 |

- A：capabilities 过 hibot `capabilities-1.json`；7 项钉子；`make verify` 通过。
- B：`cargo test --test acceptance_matrix` 18 组合 / 3 passed；证据 37 PASS（capabilities + 18 报告 + 18 心跳）。
- C：官方 `prepare-job.sh`（hibot `91834c2`）产三 kind job，预检 8×3 全 PASS；真跑 triage `ok` 40s、
  verify `ok` 9s（2/2 resolved）、review `partial`（600s 外部上限，`wall_clock_exhausted`，11 findings）；
  校验 `evidence/real-run/check.log` **83 PASS / 0 FAIL**。
- D：README job 模式章节 + `--help` 三处修正；证据 `evidence/D-help/help.txt`。
- 门禁（修复后）：`cargo test` 全绿（1183+21+3+7+26+7+24+10+3+15+4+4）；`make verify` 通过。
- 真跑观察 → 已修（`6eb31f5` `fix(job)`）：中断路径 `status_reason` 不再重复；`usage.wall_clock_ms` 改用 job 级 elapsed。
  修复后重跑 review（600s 上限）：`partial` + 单句 reason + `wall_clock_ms=598987` + 13 findings；`check.log` 87 PASS / 0 FAIL。
- 证据索引：`evidence/REPORT.md`；hibot 一页交付包：`delivery/hibot-handoff.md`。
