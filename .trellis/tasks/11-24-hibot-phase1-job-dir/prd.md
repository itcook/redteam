# Phase 1 外派运行基础（1.1–1.9）

## 背景

hibot audit 执行单元接入的 U2 单元（协调者 01a20ba0 派发）。计划权威：`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §4 / §11 / §12 / 附录 A·D·E·F；
契约权威：`~/hibot/.docs/audit/contract.md` + `schema/*.schema.json`（**只读**，不复制进本仓）。

前置：U1（0.1/0.8/0.4）已合入 `hibot-executor`。本单元在 `feature/hibot-audit-exec` 继续，不 rebase。

## 硬序

`1.1 / 1.2 / 1.3 / 1.4 → 1.5 → 1.6 / 1.7 / 1.8 / 1.9`，每项按计划验收列自证；1.1–1.4 完成后先给协调者 3–5 行简报。

## 范围与落点

| 项 | 内容 | 落点 |
|---|---|---|
| 1.1 | manifest（job-1 + job.<kind>-1）、artifact sha256、相对路径以 job 目录为基准且 canonicalize 不越界（paths.repo 允许兄弟）、report/state 恒 `*.tmp`→rename、不读 stdin、不依赖 cwd | `src/job/{manifest,artifact,io}.rs` |
| 1.2 | 只读 `paths.config`（固定 `config/executor.redteam.json`）；复用 `from_json_warned`；env-only 凭据；job 模式拒收配置/业务 flag；缺文件→error | `src/job/executor_config.rs`、`src/config/mod.rs` |
| 1.3 | hibot 信封 + 指纹（逐行 trim→丢空行→单空格 join，不 lowercase）+ status 判定表 | `src/job/{report,fingerprint,status,extensions}.rs` |
| 1.4 | 顶层 `language: zh_CN\|en_US`；配置 > 本地 locale > job 缺省 en_US；job 不读 env；`OutputLang` 贯穿 prompt builder 与 status_reason；`extensions.redteam.language{value,source}` | `src/config/mod.rs`、`src/prompt/mod.rs`、`src/job/` |
| 1.5 | deadline、`JobDirCellSink`、SIGTERM → 停派发 + 原子写 partial + ≤60s、不触碰 JobStore | `src/job/lifecycle.rs`、`src/cli/mod.rs` |
| 1.6 | 三夹具（clean/known-vuln/partial）+ FakeChatBackend + 钉 language + 确定性后端连跑指纹一致；partial 用“两 provider 其一 apiKeyEnv 不注入” | `tests/job_conformance.rs`、`tests/fixtures/job/*` |
| 1.7 | `triage`/`verify`（不隐藏）+ `review --job-dir`；kind 一致；退出码 | `src/main.rs` |
| 1.8 | 仅 manifest 不可解析不写报告；triage summary 仅 ok 必需；verify verifications 仅空输入可空 | `src/job/report.rs` |
| 1.9 | `config schema` + `config check <path\|json> --kind`；`dist/config.schema.json` 入库 + make verify 零 diff；未知键/密钥字段 error；stdout 诊断 + RESULT 行 + 二值退出码 | `src/config/`、`src/cli/`、`dist/`、`Makefile` |

## 协调者默认裁决（D1–D6，照做）

- **D1**：Phase 1 的 triage/verify 无分析实现：job 合法时写 `status=error` + `status_code=kind_not_implemented`（临时码，Phase 3/4 删除）；禁止伪造 summary/findings/verifications。
- **D2**：1.3 envelope 只做结构映射；review 仍走现有 git 路径（`paths.repo` + `merge_base`），不消费 `diff_patch`、不做 2.x 投影/一致性/quote 重锚/dismissed/prior_report。
- **D3**：本单元不写 `state.json`；`report.json` 现在就原子写。
- **D4**：job 模式未知键 = error；本地仍 warning。
- **D5**：新依赖 `sha2`、`csv` 必加；SIGTERM 优先 tokio signal（否则 libc，理由写提交说明）；不得引入 OpenSSL。
- **D6**：夹具自造，不复制 hibot 样例进本仓。

## 禁区

- `~/hibot` 只读；不复制其 schema/样例进本仓
- 不实现 Phase 2+；不写 `capabilities/redteam.json`
- 不碰 `dev`；不重建 `plugins/redteam/bin/`；不 push `hibot-executor`

## 验收

每项计划表格验收列 + 跨阶段不变式（报告原子写、失败路径写合法报告、job 模式零写 repo、无 cost、指纹唯一实现、job 零读 env、配置契约单一真相）。
最终：`cargo test` 全绿、`make verify` 零 diff（含 `dist/config.schema.json`）、Conventional Commits 按 1.x 分提交。

## 结果（2026-11-24，待协调者验收）

- 提交（`feature/hibot-audit-exec`）：097ebea（1.1–1.5/1.7/1.8）· 9fa3c2a（1.9 dist+Makefile）· b0a4c47（env 隔离 + 附录 E）· e4c0d70（env 断言）+ 本轮修复提交。
- `cargo test`：1010+ lib + 21 bin + 6 conformance + 7 packaging + 24 e2e；`make verify` exit 0（schema 零 diff）。
- **verify 覆盖（协调者 2026-11-24 裁 A）**：非空 findings → 逐条 `inconclusive` + `status=error + kind_not_implemented`；
  空输入 → `verifications: []`；非法指纹 → `invalid_fingerprint_input` 且不产 verifications；JSON 非法 → `input_schema_mismatch:findings`。
  已补四类用例。
- SIGTERM 手工证据（含已完成 finding）：reviewer 两格已落盘、refuter 请求挂起时注入 SIGTERM → 0.03s 退出、
  `status=partial`、findings 去重后保留 2 条；空 partial 路径另测 0.06s。
- 墙钟证据：`limits.wall_clock_s=1` → 1.79s 收口 partial（`wall_clock_exhausted`）。
- 契约验证：本仓报告与 manifest 用 hibot 的 schema **原位只读**校验（draft 2020-12）→ manifest PASS / report PASS；
  quote 在 head 检出逐字命中，指纹用独立预计算向量 + 逐条复算。
- env 隔离（不变式⑦）：`seal_job_env` 清 `REDTEAM_*` 与 `LC_*`/`LANG`；
  `job_mode_key_resolution_is_env_only` 证明 job 分支凭据只走 env；语言用例证明配置优先且 LANG 被清。
  **未做**独立子进程诱饵配置测试（advisor 建议的理想证明），列为已知覆盖限制。
- `partial` 夹具改为受控本地 HTTP 服务：同输入「缺 key → 401 → partial」与「注入 key → ok」对照，
  不再依赖 `127.0.0.1:1` 必然失败；fake reviewer 走 registry（per-cell factory 无 key 时不接管）。
- B1（hibot 样例）：`config check ... --kind review` exit 1：
  `ERROR /review/verifiers unknown_key ...`、`ERROR / legacy_shape: ... review.verifiers 已更名 review.refuters ...`、
  `RESULT error 2 errors, 0 warnings`。按 P16 应改为 `review.refuters.enabled`；协调者回执 hibot。
- 已知限制：`check_text` 仍另建严格扫描（未复用 `from_json_warned` 的内部单点，但两侧共用键表；
  后续可统一）；Phase 1 不写 `state.json`（D3）；triage 无 summary（D1）。
- 临时行为（Phase 4 删除）：`kind_not_implemented:<kind>`（附录 E 已登记）。
