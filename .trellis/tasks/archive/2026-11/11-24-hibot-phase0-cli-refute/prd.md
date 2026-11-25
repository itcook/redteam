# Phase 0 代码支线：CLI 收口 + 术语硬切 + help/README 定稿

## 背景

hibot × redteam audit 执行单元接入的 U1 单元（协调 session 01a20b82 派发）。
计划：`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §3 Phase 0（代码支线）。
目标：Phase 1（外派运行基础）开工前的最后一批代码收口——旧 CLI 面清干净、`verify` 一词分家、用户面文档与新词汇对齐。

## 范围（严格按 0.1 → 0.8 → 0.4）

### 0.1 M21e-2 旧 CLI 面残余清理

- 核对 `--models` / `--models-all` / 旧 `--priority`：flag 名、帮助文本、错误提示。
- `resolve_models` 已由 `Config::active_roles()`（角色驱动）取代：核对无残留 dead path。
- 落点：`src/cli/mod.rs`、`src/main.rs`、`plugins/redteam/commands/*.md`。
- 验收：`--help` 不再出现上述 flag；`make verify` 零 diff。**不要求全文 grep 零命中**（`priority` 作普通词合法）。

### 0.8 术语与角色硬切（P16）

第 0 步：删死代码 `build_refute_prompt` + `templates/adversarial-refute.md`（当前仅 6 个单测调用、零产品路径）。

四层，**每层 `cargo test` 绿再下一层**：

1. **配置层**：`roles.verifier` → `refuter`、`review.verifiers` → `refuters`；同一提交内新增可选 `verifier`
   （fix 验证角色，缺省回退 `reviewer`）+ 护栏 warning（review job 有 `verifier` 无 `refuter` → 提示“验真角色已更名 refuter”）。
2. **报告层**：`Finding.verify` / `VerifyVerdict` / `RunMeta.verify_*` → `refute*`；
   `CellOutput::Verify` / `VerifyEntry` / `DeepMeta.verify_*` 同步。
3. **内部符号**：`Phase::Verify` → `Phase::Refute`（删 live 的 `Verify`、把 legacy `Refute` 提升为唯一变体，复用其 serde 名，
   不新增第二个 `Refute`）；模板 `verifier.md`→`refuter.md`；模块 `consolidate/verify.rs`→`refute.rs`；
   隐藏命令 `verify-once`→`refute-once`；`eval/verify_score.py`→`refute_score.py`；prompt 构造器/解析器随名。
4. **md 文案与文档**：`--help`、README、`plugins/redteam/commands/*.md`、`eval/README.md` 等现行文档。

硬切：无别名、无 serde alias、无 clap alias（尚未发布，不欠兼容层）。

白名单（本单元不创建）：`src/fixverify/`、`src/job/`；不引入 hibot verify kind 的新词。

## 明确不做

- aacr `gate`/`smoke`/R20、ship 六条输入条件、拆格（D22 已结案）。
- 数据支线全部（0.6/0.2/0.5/0.3/0.7）。
- Phase 1 及之后（`--job-dir`、报告信封、fingerprint、`config schema`/`config check`、`language` 配置键）。
- 不改 `/Users/weilai/hibot`；不把 hibot schema 复制进本仓；不碰 `dev` 分支。

## 验收

- 0.1：`--help` 无 `--models` / `--models-all` / `--priority`；`make verify` 零 diff。
- 0.8：每层 `cargo test` 绿；`--help` 与配置样例只用新词；护栏用例；自检 `verifier` 仅剩新语义与历史文档命中。
  三条已知影响写进 0.8 提交说明：本机 `~/.redteam/config.json` 需手改一行 `verifier`→`refuter`；
  旧 `proj.db` 历史报告的 `refute` 读成空（静默丢失）；旧作业 state 里的 `"verify"` 反序列化失败。
- 0.4：`make verify` 零 diff；旧 flag 零残留；README 不写死里程碑编号。

## 提交

Conventional Commits；0.8 **分层提交**（配置 → 报告 → 内部符号 → 文档），每层可独立构建。

## 结果（2026-11-24，验收通过）

- 协调者（01a20ba0）独立复核全绿，已 `git merge --no-ff` 合入 `hibot-executor` = `b11bb29`。
- 提交：本分支 `e750bc0..ff47049` 共 11 个（0.1 / 0.8 四层 / 0.4 / 测试 / 风险双读 / 任务卡）。
- **P2 口径裁决（协调者 2026-11-24）**：方案 A——本单元仅改名，`pick_refuter` 自动异族回退**保留不改**；
  「refuter 缺则跳过验真」列为 Phase 2 的显式决策点，不得漏成默认行为。
- 计划外修复保留：`risk/signals.rs` 与 eval 脚本对历史 `results` JSON 双读（`refute` 优先、`verify` 回退）
  ——读历史数据、非 schema 别名；不修会让升级后 `ledgerUpheld` 恒 0。
- 移交后续：本机 `~/.redteam/config.json` 的 `verifier`→`refuter` 手改与入库二进制重建，
  协调者裁「U2 完成后 / Phase 7 版本同源时统一处理」。
