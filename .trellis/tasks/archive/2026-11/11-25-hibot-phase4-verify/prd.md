# Phase 4 · `verify` kind（fix 验证，与 `refute` 零共享）

> 基线 `cdc6716`（`feature/hibot-audit-exec`）· 2026-11-25 · 协调者 `01a20ba0`
> 计划：`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §7（4.0–4.7）
> 契约：`~/hibot/.docs/audit/contract.md` §1.2 / §3.3；schema `audit-report.verify.schema.json`、`audit-input.findings.schema.json`

## 背景

hibot M4 需要 `kind=verify`：上一轮评审已确认的 finding，本轮修复后逐条核验
「修复是否发生 / 是否引入新问题」。它与评审内验真（`refute`，判 claim 成不成立）是**两个问题**，
因此 Phase 4 的硬约束是**流程隔离**：新模块、新模板、新枚举，不复用 `consolidate::refute` 的
模板 / 解析 / 判定表（只允许 provider 传输层）。

config v2 已合入：verify 模型取 `settings.verify.models[]`（轮转池，长度 1 合法），
**不再回退 reviewer**；`roles.verifier` 已不存在。

## 范围（4.0 → 4.7 + 临时码清理）

| 项 | 要求 |
|---|---|
| 4.0 | 新模块 `src/fixverify/` + 新模板 `src/prompt/templates/fix-verify.md` + 新枚举 `FixVerifyStatus`；**零 import `consolidate`**；`settings.verify.models` 为空 → `error / missing_role:verifier` |
| 4.1 | 覆盖不变量：输出指纹集合 == 输入指纹集合；输入指纹格式非法 → `error / invalid_fingerprint_input`（不逐条 unresolved） |
| 4.2 | 逐条专属 prompt：finding + 该文件 fix hunks + ±N 快照 + 可选 `prior_report`（佐证，非结论）；不喂整 PR；**finding 已成立是前置假设**，prompt 禁止重新裁定；沿用 `neutralize_structure`；`evidence` 语言随 `settings.language` |
| 4.3 | 删除 → 按「构造消失」判定（可 resolved，须证据）；改名 → 新路径重定位；跨文件修复只在相关 hunks 内取证 |
| 4.4 | 四态 `resolved`/`unresolved`/`regressed`/`inconclusive` 均需证据；quote 逐字命中；模型失败/无法定位/证据不足 → `inconclusive`；**出现 inconclusive ⇒ `status ∈ {partial,error}` + `status_reason`（`verifications_inconclusive:<n>`）**；禁止塞进 `unresolved` |
| 4.5 | 自建合成夹具：四态各一 + 删除式修复 + 改名/移行（`audit-pr-468` 的示例不作金标） |
| 4.6 | `findings` 为空 → `verifications: []` 合法、报告 `ok`；不伪造判决 |
| 4.7 | `capabilities.kinds = ["review","triage","verify"]` + `tests/capabilities.rs` |
| 清理 | 删除 Phase 1 `kind_not_implemented` 临时码与 verify stub、逐条 `inconclusive` 占位；附录 E 对应行标注「已删除（Phase 4）」 |

## 验收（自证，反例优先）

1. 依赖审查：`src/fixverify/` 对 `consolidate` 零 import（源码文本硬断言）；`fix-verify.md` 与 `refuter.md` 模板哈希不同（硬断言）
2. 覆盖不变量：少一条/多一条 → 断言失败；坏指纹 → `error/invalid_fingerprint_input`
3. 四态各一 + 删除式修复 + 改名/移行；quote 逐字命中反例（只改空白 → 失败）
4. provider 401 → 该条 `inconclusive` + 整体 `partial` + `status_reason`；**不得落 `unresolved`**
5. 空输入 → 合法报告且 `verifications: []`
6. `config check --kind verify` 对空 `settings.verify.models` 报 `missing_role:verifier`
7. `make verify` + `cargo test` 全绿；告警 0

## 禁区

- 不做 Phase 5/6/7；不改 config 语义；`~/hibot` 只读；不改 `dev`；不重建入库二进制；报告不含 cost。
- 所需的唯一跨模块接线：`src/cli/mod.rs` 增 `RegistryMode::Verify` + `build_registry_for_verify`
  （平行于既有 triage 版，用于按 `Role::Verifier` 注册 `settings.verify.models`）。

## 提交切分（Conventional Commits）

1. `feat(fixverify): 流程隔离骨架 + 逐条 prompt + 四态判定（4.0–4.2）`
2. `feat(fixverify): 覆盖不变量 + 删除/改名定位 + 空输入（4.1/4.3/4.6）`
3. `feat(verify): job 接线 + 报告信封 + 心跳进度（4.4）`
4. `test(verify): 合成夹具四态/删除/改名/quote 反例/401（4.5）`
5. `feat(capabilities): 声明 verify kind（4.7）`
6. `chore(verify): 删除 Phase 1 kind_not_implemented 临时码与 stub`

## 交付记录（2026-11-25）

**提交**（`feature/hibot-audit-exec`，基线 `cdc6716`）

| 提交 | 内容 |
|---|---|
| `75dcd97` | `feat(fixverify)`：模块骨架 + 输入解析 + 证据装配 + 专属 prompt + 四态（4.0–4.3/4.6） |
| `60ce92d` | `feat(verify)`：job 接线 + 报告信封 + capabilities + 删除 Phase 1 临时码（4.4/4.7） |
| `dde0b33` | `test(verify)`：合成夹具（四态/删除/改名/移行/quote 反例/401/流程隔离） |

**验收证据**

- 隔离性：`src/fixverify/*` 对 `consolidate` 零 import（源码文本硬断言）；`fix_verify=c103ef8753ded8ce`
  ≠ `refuter_tail=59beb77ec6f22aa3` ≠ `reviewer_tail=e18d02f84e94fff2`；模板字节 `assert_ne`。
- 覆盖不变量：`same_fingerprint_set` 单测（少一条 / 多一条 / 恰等）；缺口由 `finalize` 补 `inconclusive`，
  多余条目被裁剪；坏指纹 → `error/invalid_fingerprint_input`；空集合 → `verifications: []` + `ok`。
- 四态：`tests/verify_conformance.rs` 逐条覆盖（输入顺序）+ 证据非空；出现 inconclusive ⇒
  `partial` + `status_reason(verifications_inconclusive:1)`；`quote_rejected` / `model_failures` 披露。
- 删除式修复 → `resolved`（quote 取自 diff 的 `-` 行）；改名 → 新路径重定位（prompt 带 `Renamed from`）；
  移行 → `related_hunks`；quote 只改内部空白 → 降级 `inconclusive`（对照组逐字 → `resolved`）。
- provider 401 → 该条 `inconclusive` + 整体 `partial`，不落 `unresolved`。
- `config check --kind verify` + 空 `settings.verify.models` → `missing_role:verifier`。
- `make verify` exit 0；`cargo test` exit 0（lib 1162 + job_conformance 25 + verify_conformance 8 + 其余全绿）；
  `cargo build --all-targets` 告警 0。
- 协调者独立复核并合入 `hibot-executor` = `b771736`。

**批准的跨模块接触点**（协调者 2026-11-25 批准）

- `src/cli/mod.rs`：`RegistryMode::Verify` + `build_registry_for_verify`（按 `Role::Verifier` 注册
  `settings.verify.models`，与 triage 版平行）。
- `src/positioning/mod.rs`：`safe_read` 私有 → `pub(crate)`（仅可见性，复用越界/符号链接读守卫）。

**遗留（followup，未改代码 —— 协调者要求收口后停止编码）**

1. **作业级 error 的覆盖**：`findings` 可解析但作业级失败（检出缺对象 / 缺 verifier / registry 构造失败）时，
   当前输出 `verifications: []`，而契约 §3.3 说「集合为空是唯一允许空数组的情形」。提议
   `report::error_report_covering(...)`：findings 可解析时逐条给 `inconclusive`（证据句绑定错误码），
   仅 findings 不可解析/集合为空才留空数组。已报告协调者，待裁定。
2. `missing_role` 诊断的 pointer 落在 `/providers`，对 `settings.verify.models` 这类池缺失不够直观
   （协调者记为 followup，不返工）。
