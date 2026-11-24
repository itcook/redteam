# redteam × hibot · audit 执行单元接入 + 发布面 实施计划

> 状态: WIP（**仅规划，未实施**）· 写于 2026-11-23 · 基线 `dev` = `ac7c656`（工作树干净）
> **本轮只做规划**：未修改产品代码、未运行测试、未跑 dogfood、未改动 hibot 仓、未动 `~/hiapi/cli`。
>
> 需求正文: `.doc/requirements/2026-11-20-hibot-consumer-requirement.md`（v2，redteam 视角）
> 契约权威: `~/hibot/.docs/audit/contract.md` + `.docs/audit/schema/*.schema.json` + `.docs/examples/audit-pr-468/`（只读）
> 发布参照: `~/hiapi/cli`（install.sh + R2 + `update` 自更新 + `scripts/release.sh` + release runbook）
>
> 对应关系：**Phase 1（1.9）↔ R5**（`config schema`/`config check`，hibot 侧软阻塞 M2）· Phase 2/3/4/5 ↔ hibot `redteam-track.md` 的 **R1/M2 · R2/M3 · R3/M4 · R4/M6**；
> Phase 7 ↔ 本仓 **M19 发布 1.0**（`followup.md`）与 **T11/T13**；Linux/Windows 归 **M20**。
> 里程碑编号: 现 `.doc/roadmap/summary.md` 最大为 **M21**（M19 后置），本文暂称"hibot 接入里程碑"，编号在 0.7 登记时确认。
>
> **证据等级**：`[核实]` 已读源码/契约实例 · `[文档]` 需求或契约文字要求 · `[推定]` 推断无直接证据 · `[待冻结]` 需裁决 · `[拟议]` 本文建议

---

## 0. 一页结论

| # | 结论 |
|---|---|
| 1 | **Phase 0 = 现有引擎收口**：做 M21e 的 1/2/4/5/6/7（aacr 项 3 除外），但**不宣称"整个 M21 无条件完成"**——aacr `gate`/`smoke`/R20 转列**延期债**，关账表述为"完成本次调整后的范围，外部对照缺口保留"。 |
| 2 | **ship 单元降优先级**，单列延期；Phase 0 的 dogfood 数据工作不依赖 ship，且**可与接入开发并行**。 |
| 3 | **Phase 1 是外派运行基础**（job IO / 报告信封 / 指纹 / status / 语言 / 原子写 / SIGTERM 60s / 成果保存），先于第一个可用的 `review --job-dir`。 |
| 4 | **Phase 2 review → 3 triage → 4 verify → 5 心跳（可选并行）→ 6 能力声明与验收**。 |
| 5 | **新增 Phase 7 发布面**（本轮补充）：版本单一来源 + `scripts/release.sh` + `install.sh` + `redteam update` + R2/GitHub 双源 + release runbook。**首期仅 macOS arm64**；Linux/Windows 归 M20。可自 Phase 1 之后并行，不阻塞 hibot 接入。 |
| 6 | **输出语言能力**（本轮修订）：配置 `language: zh_CN \| en_US`，**覆盖过程与结果、三 kind 一致**（不是"仅最终报告"）；job 模式**以外置配置为准**（缺省 en_US）。背景：hibot 把结果渲染成给人看的 sticky comment 且自身无翻译能力，所以语言必须由 redteam 按配置产出。同时修掉"报告语言由环境 locale 决定"这个破坏可复现性的洞；md 骨架维持英文（T13）。 |
| 7 | **契约侧已完全闭合**（hibot 4 个 commit + 6 轮 intercom）：Q1–Q17 全部裁定，S1/S2 已由用户裁决，D73 与 `kinds` 增量已纳入，双方口径表存档于 §12。**当前零未决项**；剩余工作是实现，不是对齐。 |
| 8 | **术语与角色分家**（本轮补充）：`verify` 保留给 hibot 的**修复验证**；评审内的 **finding 验真改名 `refute`**（角色 `refuter`）。四角色：`scanner`/`reviewer`/`refuter`/`verifier`（`verifier` 是 fix 验证，**可选，缺省回退 reviewer**）。两者是两条流程（输入/输出/触发/失败语义/prompt 全不同，见 P16.1），结构上分开、不共用模板与判定表。**尚未发布 → 0.8 硬切、不做兼容层**；落地为 **0.8 批**（改名+新角色）+ **Phase 4**（新流程），**0.8 必须在 Phase 1 开工前**。 |
| 9 | **配置契约要能机器消费**（hibot A2 已裁）：新增 `<cli> config schema`（从类型生成 JSON Schema）+ `<cli> config check <path|json> --kind`（形状 + 语义），生成物入库并用 `make verify` 防漂移。job 模式**配置唯一来源 = manifest `paths.config`**（固定 `config/executor.<cli>.json`），**不接受任何配置 flag**。这样 hibot 的 per-job 渲染不再靠口头约定，而是“拿到 schema → 渲染 → 自己先跑 config check”；job 模式未知键 = error，本地仍 warning。 |

规模（含测试与文档，**前提见 §14**）：Phase 0 ≈ 11–19 人日（含 0.8 改名）· 1 ≈ 7–9 · 2 ≈ 4–6 · 3 ≈ 6–9 · 4 ≈ 5–7 · 5 ≈ 2–3 · 6 ≈ 3–5 · 7 ≈ 3–5。合计 **41–63 人日**。

---

## 1. 现状盘点：redteam 距契约差什么

| 契约要求 | 本仓现状 | 差距 |
|---|---|---|
| `redteam <kind> --job-dir <dir>` 外派模式；出现即拒收一切业务 flag | `review` 是唯一业务命令，带 `--base/--scope/--spec/--focus/--format/--background/--resume/--incremental-from/--pack-only/--tools/...` `[核实] src/main.rs:55-150` | 三 kind 都无 job 形态；需新增 `triage`/`verify` 与 `review --job-dir`，用**独立 clap 参数组**实现"业务 flag 一律拒收" |
| 输入以 `job.json.inputs` 为唯一权威，逐项校验 `sha256` | 输入来自 `git` 与 cwd：`evidence::git::changed_files(cwd, base, scope)` `[核实] src/cli/mod.rs:1006, src/evidence/git.rs:42` | 无 manifest 解析、无 artifact 校验、无 job 目录概念 |
| 报告写 `<job-dir>/out/report.json`，`hibot.audit.report.<kind>/1` 信封 | 报告是 `ReviewReport`（`schema_version:"4"`），前台 `println!(render(&report, fmt))` `[核实] src/main.rs:766-770` | 需独立序列化层薄映射；缺 `fingerprint`、`input.head_sha|base_sha`、`usage` 归一口径 |
| 指纹 = `sha256(file \0 normalize(quote) \0 normalize(title))`，`normalize` = 逐行 trim→丢空行→单空格 join | 内部 id = fnv1a64 over `(file, 去全部空白的 quote, lowercase+折叠的 title)` `[核实] src/orchestrate/mod.rs:36-56` | 算法与归一都不同（契约**不 lowercase**）；需新增独立实现，内部 id 不动 |
| `dismissed.csv`：RFC4180、UTF-8/LF、固定表头 `fingerprint,actor,reason,created_at` | `--dismissed` 现有两种值：sqlite 库路径 / **内联逗号分隔的 16-hex id 串** `[核实] src/project.rs:96-107`；文件路径还会被 `stage0_risk` 当 sqlite 打开 `[核实] src/cli/mod.rs:269-278` | 无 CSV 解析、无 sha256 键空间；现有解析**不能复用** |
| executor config 只从 manifest `paths.config` 读 | `Config::load*` 固定读 `$REDTEAM_HOME`；`load_no_secrets` 能跳过 secrets **但路径仍固定** `[核实] src/config/mod.rs:457-500`；**`resolve_key` 内部仍调 `secrets::load_secrets()`** `[核实] src/config/mod.rs:596-617` | 需新增“只认 manifest `paths.config`”的加载器：凭据链走 env-only，job 模式不接受配置 flag |
| `maxConcurrency` 语义 | `InflightGate` 是 **per-provider** 闸，缺省继承 `review.maxConcurrency` `[核实] src/provider/registry.rs:20-97` | hibot **D72 已裁**：保持 provider 子闸唯一，不引入 job 总闸；要收紧总并发由 hibot 调小渲染值 |
| `out/state.json` 每 ≤5s 原子覆写，`in_flight` 为**当前**在途 | `GateStat` 只有 `cap`/`effective_cap`/`rate_limited`/`peak`/`completions`，**无当前在途** `[核实] src/provider/registry.rs:62-72,270-280` | 需 `inflight()`/`waiting()` 快照 + 心跳任务 |
| `SIGTERM` → 60s 内落 partial 再退出 | 无信号处理；只有后台 worker 的 `40*60` 硬 deadline 与超时 partial 路径 `[核实] src/cli/mod.rs:2068-2205,2273` | 需信号→停止派发→保存成果→写 partial；预算改取 `limits.wall_clock_s` |
| 不写 `paths.repo` | 真实 review 路径**有一处写**：`upgrade_legacy_gitignore(root)` `[核实] src/cli/mod.rs:347`（仅当 `.redteam/.gitignore` 恰为 `*`）；`persist_dormant_suspicions` 仅当 proj.db 已存在时写 `[核实] src/cli/mod.rs:1519-1548` | job 模式需显式闸门：该调用跳过、本地台账关闭、`JobStore`/`proj.db` 不建 |
| 三 kind 能力 + **术语冲突** | `review` 完整；`verify` 只有单条离线 `verify-once`（判 upheld/refuted）`[核实] src/cli/verify_once.rs:1-40`；批内 `verifier` 角色同语义 `[核实] src/domain/finding.rs:126-129`。**同一个 `verify` 词已指两件事**：hibot 的修复验证 kind vs 评审内 finding 验真（详见 P16） | `triage` 全新；`verify` 需批量 + **四态**（resolved/unresolved/regressed/**inconclusive**）；内部验真改名 `refute` |
| 输出语言可控（本轮新增需求） | 内容层已本地化，但**来源是环境 locale**（`LC_ALL`→`LC_MESSAGES`→`LANG`），5 处调用 `[核实] src/prompt/mod.rs:19-44,69,216,329,359,403`；无配置键；`status_reason` 等确定性文案硬编码 | 需配置 `language`（zh_CN/en_US），**job 模式也以外置配置为准、不读 env**；范围覆盖过程与结果、三 kind |
| 发布/分发面（本轮新增需求） | 二进制入库 `plugins/redteam/bin/darwin/aarch64/redteam`；launcher 已按 os/arch 分派 `[核实] plugins/redteam/bin/redteam`；无 install 脚本、无自更新、无 R2、无 release 脚本、无 CHANGELOG、无 CI `[核实] Makefile, 无 .github/` | 需 Phase 7；版本三处不一致（`Cargo.toml` 0.1.0 vs 两个 `plugin.json` 1.0.0 `[核实]`） |
| 配置契约可机器消费（本轮新增需求） | 无 schema 输出；形状守卫靠**手写 known-keys 表**（`shape_guard`）`[核实] src/config/mod.rs:712-728`，未知键只 warning；`schemars` 在依赖里但只用于 `domain/finding.rs` derive，**零 `schema_for!` 调用** | 需 P17：`config schema` + `config check`；job 模式未知键与密钥字段升为 error |

**结论**：评审内核（scan→deep→verify、并发门、证据收集）不重写；缺的是**外派外壳 + 输出契约 + 三 kind 适配 + 发布面**。

---

## 2. 拟议实现选择（P1–P17）`[拟议]`

**P1 · `--job-dir` 即外派模式，用 clap 参数组表达互斥**：`review` 拆 `LocalArgs`/`JobArgs { job_dir }`；`triage`/`verify` 只有 `JobArgs`；另有 `config` 子命令（`schema`/`check`）。**job 模式下不接受任何配置路径 flag**（hibot A1）：配置唯一来源 = manifest `paths.config`；业务 flag 一律拒收（clap 报错，不写 if）。

**P2 · kind ↔ 角色最小集 + 角色枚举定稿**（用户裁决 + hibot 确认）：`review` → scanner + reviewer（refuter 可选，缺则跳过验真）；`triage` → 建议用 scanner 条目；`verify` → **`verifier` 可选，缺省回退 `reviewer`**（仅模型分配复用，不等于流程复用，见 P16.1）；`verifier` 与 `reviewer` 都缺才报配置错误。
**role 枚举定稿：`scanner` / `reviewer` / `refuter` / `verifier`**（hibot 已据此改示例，且他们的 Profile 角色键与我们的枚举**同名、原样搬运**，不做改名映射）。

**P3 · 报告信封独立成层，不动内部 `ReviewReport`**：`verdict` 丢弃；`stats`/`run.usage`/`run.duration_ms` → `usage`（不带 cost）；`scan`/`clusters`/`cells`/`run.*` → `extensions.redteam`。

**P4 · 指纹逐字实现契约，内部 id 不动**：新增 `fingerprint::of(file, quote, title)`（逐行 trim→丢空行→单空格 join，**不 lowercase**）；`enum DismissKey { LocalId, HibotFingerprint }`。
**指纹稳定性口径（hibot B3 已裁）**：`title`/`quote` 是**身份字段**，同一 head 重跑必须稳定；可自由改写的只有 `body`/`recommendation`/`evidence`。§7-6「同夹具连跑两次指纹集合一致」**只对确定性后端**成立，真实模型**只观测、不设阈值、不作门禁**。若后续证明确实做不到 title 稳定，正确路径是提「新增稳定 rule id」的契约变更，**不是**靠措辞容忍度或集合交集率。
指纹稳定性分两层：**管线确定性**（确定性 fake backend 复跑，必须一致）与**真实模型稳定性**（测量并披露交集/差集、title 变更率，不改身份定义，Q3）。**夹具必须显式钉住 `language`**，否则指纹集会因语言漂移。

**P5 · `status` 判定表（确定性）**：

| status | 判据（来自 coverage/RunMeta，不来自 LLM） |
|---|---|
| `error` | job.json 不可读/kind 不一致/manifest 校验失败/artifact sha256 不匹配/**实际 HEAD ≠ `pr.head_sha`**/**输入指纹格式非法**/必需输入缺失/配置非法/角色不足/panic |
| `partial` | scanner cell 失败（`Incomplete`）· reviewer cell 失败 · 预算截断 · reviewer 全部出局 · triage 任一轴失败 · `summary.routing.specs` 读不到 · **verify 出现 `inconclusive`** · deadline 收敛 · quote 逐字校验失败被丢弃 > 0 |

**报告可写性（hibot B5 最终口径）**：**只有 manifest 不可解析时**才不写 `report.json`（runner 记 `no-report`/`error`）；其余任何失败都必须写出合法报告——
- `review`：kind 无附加必填，`error` 报告恒合法；
- `triage`：`summary` **仅在 `status=ok` 时必需**，`partial` 可省、`error` 不写摘要 `[核实] audit-report.triage.schema.json`；
- `verify`：`verifications` **仅当输入指纹集合为空时**允许空数组，其余必须逐条覆盖 `[核实] audit-report.verify.schema.json`。

**禁止零值 `summary`、伪造 `verification` 去凑 schema。**

| `ok` | 其余（含"ok 且 findings 里有 high"——那是有效结论） |

`status_reason` 与 `partial`/`error` 同时非空且点名缺什么；**同一判定同时产出 `extensions.redteam.status_code`（机器码）**，文案与 code 见附录 E；**禁止零值 summary 或伪造判决填 schema**。

**P6 · 配置投递（`job_native`）：唯一来源是 manifest `paths.config`**
术语：**用户配置** = `$REDTEAM_HOME/config.json`（仅本地模式，含 provider/凭据引用）；**executor config** = hibot 每 job 渲染的配置档案，落 `paths.config`（**不含 token 类信息**）；**manifest** = `job.json`（输入/输出/预算）。**代码内部类型名仍为 `Config`**。
路径（hibot A1 已裁，**唯一来源**）：job 模式只认 manifest `paths.config`，其值固定为 `$JOB/config/executor.<cli_name>.json`（redteam → `config/executor.redteam.json`，schema 有 pattern 约束）`[核实] audit-job.schema.json:paths.config`。**job 模式不接受 `--config`/`--profile`，也不做 `$JOB/profile.json` 回退**；文件不存在 → `error`（不得静默用空配置）。本地模式仍读 `$REDTEAM_HOME/config.json`；`--config <path>` 仅作为本地/手工重跑便利，**不进契约、不要求 hibot 传**。
**相对路径基准（已定）**：manifest `paths.*`、`inputs.*.path` 的**相对路径一律以 job 目录为基准**，绝对路径原样；解析后必须落在 job 目录内（`paths.repo` 例外，允许 `../repo` 兄弟）。**cwd 永不参与**。
其余规则：① 只读该文件，**不读 `$REDTEAM_HOME`/`~/.redteam/`/`secrets.json`**；② 凭据只从 `apiKeyEnv` 指向的环境变量取——**复用现成接缝 `resolve_key_with(provider, id, &Secrets)`**（`[核实] src/config/mod.rs:600`），传空 `Secrets` 即 env-only，不必新造凭据链；③ **executor config 内出现密钥类字段（`apiKey`/`token`/`secret`/`password`…）→ `error`**（约定该文件不含敏感信息）；④ 缺字段用内置默认且稳定；job 模式停用 `REDTEAM_MAX_TOKENS`/`REDTEAM_PER_TURN_SECS`；⑤ 不做 `isolated_home` 过渡。

**P7 · 只读 repo、唯一写点 `out/`**：跳过 `upgrade_legacy_gitignore`；不建/不解析 proj.db；不调 `JobStore`；**五条台账路径**（激活 / 晋升 / 落账 / 披露 / 收敛）显式关闭——代码自述“五条都必须过这一道”`[核实] src/cli/mod.rs:1521-1524`。

**P8 · 预算与终态（hibot D73）**：实际预算 = `min(manifest.limits.wall_clock_s, summary.routing.wall_clock_s)`，**manifest 恒为硬上限**；接近预算停止新开 cell 并收成 `partial`；SIGTERM → 停止派发 + **原子写 partial** + ≤60s 退出，不留子进程。
**终态判定在 runner 侧**：它阻塞等子进程退出（硬超时 = SIGTERM → 60s → SIGKILL）后读 `out/report.json`；**不轮询、不回调**。因此我方必须：① **`report.json` 始终原子写**（`report.json.tmp` → `rename`），**正常结束时也是**，不得直接截断写；② `state.json`/`events.jsonl` 不是终态信号。

**P9 · 成果保存复用 `CellSink` trait**：新类型叫 **`JobDirCellSink`**（`JobCellSink` 已被占用——那是写 proj.db 的 `Arc<JobStore>` 实现，`[核实] src/cli/mod.rs:1195`，而 P7 要求 job 模式不碰 `JobStore`）。`JobDirCellSink` 内存 + 周期落 `out/.cells.jsonl`，SIGTERM/超时据此组 partial；**复用的只是 trait**（`[核实] src/orchestrate/mod.rs:18`）。

**P10 · triage 不产结论**：只产 `summary`（**`routing` 嵌在 `summary` 内**，契约 §3.4）+ `findings`；三轴不砍，预算不足降条目数与分片数。

**P11 · verify 不重评 PR，且“未核验”≠“确认未修复”**：逐条只喂 finding + 相关 hunks + 快照；未核验用第四态 **`inconclusive`**（hibot B6）。

**P12 · 并发：只有 provider 子闸（hibot D72 已裁，不引入 job 总闸）**
- 唯一闸 = 现有 `InflightGate`（`providers.*.maxConcurrency`，缺省继承 `review.maxConcurrency`）；**不新增 `jobMaxConcurrency`，hibot 不渲染该键**。
- 要收紧 job 总并发 → hibot **调小渲染出的子闸值**；运行中叠加过载由 `state.json` 接纳刹车（hibot D68）处理，配置不做动态产能表达。
- `state.json.llm.max_concurrency` = **Σ provider 子闸帽**（实际生效口径）；`extensions.redteam.concurrency = {mode:"per-provider", provider_caps:{...}}` 如实披露。
- `providers[].name` 用**配置里的 provider key**（= Registry 的 provider key，hibot 保证其稳定）；不是 apiType、也不是模型 id。

**P13 · 退出码不参与语义（hibot D47/D73）**：我们仍按“写出报告即 0、error 报告写出后 1、job.json 定位不到 2”方便人手调试；但**hibot 只读报告不读退出码**——“非零退出 + 合法报告”他们仍用报告。

**P14 · dismissed 两个开关分开**：本地 DB 台账恒关，传入 CSV 必读。

**P15 · 输出语言（本轮修订：过程与结果全涵盖，三 kind 一致）**
背景：hibot 会把阶段结果以 **sticky comment** 发到 PR，**给人看**；hibot 本身无 LLM、不会翻译（其评论模板只负责拼接与排版 `[核实] ~/hibot/.docs/audit/comment.md`）。所以不是"hibot 感知语言"，而是 **redteam 必须按配置输出目标语言**。
- 配置键：`language: "zh_CN" | "en_US"`（**顶层键**，非 `review.*`）。**由 hibot 从“仓策略”渲染进每份 executor config**（org 默认、仓可覆盖、缺省 `en_US`），计入 `rendered_sha256`；**同一 PR 三 kind 冻结同一值**（首个 job 创建时定）。
- **不走 passthrough**（hibot C6 最终口径）：passthrough 是 per-runner 的运维补充块，按 runner 各写一份会让同一 PR 中英混排；`language` 属 hibot 写入的键，**passthrough 试图覆盖它 → hibot 拒绝渲染**（D69 冲突规则）。我方 schema 只要接受顶层 `language` 即可。
- serde 严格枚举，非法值 → 配置解析失败。
- **取值顺序（确定性）**（注意：这里说的“缺省”指 **`language` 这个键缺失**，不是配置文件缺失——配置文件缺失是 `error`，P6）：
  1. **配置有 `language` → 用它；job 模式同样如此**（外置配置是权威，不是"恒 en_US"）；
  2. 本地模式无配置 → 沿用现有 locale 探测（`LC_ALL→LC_MESSAGES→LANG`），行为与今日一致；
  3. **job 模式无配置 → 缺省 `en_US`**，并记 `extensions.redteam.language = {value, source:"default"}`；job 模式**不读 env**（P6 可复现性）。
- **作用域 = 过程 + 结果，覆盖全部三 kind**（不是"仅最终报告"）：

| 面 | 具体 | 随 `language` | 说明 |
|---|---|---|---|
| **过程中模型的产出** | review 的 scanner 疑点、reviewer findings、refute 的 reason；triage 三轴行与意图/漂移结论；verify 的逐条 evidence | ✅ | 由各角色 prompt 的输出语言槽控制（同一机制） |
| **报告人类可读字段** | finding 的 `title`/`body`/`recommendation`、triage 行文本与 blockers 说明、verification `evidence` | ✅ | hibot 评论的主内容 |
| `status_reason` | 非 ok 时的原因叙述 | ✅（用户已确认） | hibot 会在非 ok 的评论里展示，人读；确定性字符串 → 固定对照表 |
| `--format md` 内容层 | 叙述与引用 | ✅ | 骨架标签仍英文（T13） |
| `run.warnings` / `extensions` / `cells[].warn` | 诊断与机器面 | ❌ | 排障用，不进 PR 评论 |
| `state.json.phase` / `progress.unit` | `preparing`/`running`/`finishing`、`cells`/`items` | ❌（保持英文 token） | 机器/看板面；由 hibot 映射成人读标签（Q17） |
| md 骨架标签 | `Summary`/`Finding Details` | ❌ | T13 既有裁决：devtool 惯例 |
| CLI 日志/错误（stderr） | — | ❌ | 开发者面 |

- **实现方式**：`output_language()` 拆成"解析一次 → `OutputLang` 值 → 传给所有 prompt builder 与三 kind 的渲染点"，不再各处读 env；prompt 仍只填 `{{OUTPUT_LANGUAGE}}` 槽（只改输出语言，不改推理）。`status_reason` 用与之同源的固定对照表。
- **一致性要求**：同一 PR 的 triage / review / verify 三个 job 必须用**同一个 `language`**（hibot 渲染时保证），否则 sticky comment 会中英混排。
- **披露**：`extensions.redteam.language`（value + source），便于审计与排障。

**P16 · 术语：`verify` 留给 hibot 的修复验证，评审内验真改名 `refute`** `[已裁 2026-11-23]`
同一个词目前指两件事：hibot 的 `verify` kind = **修复验证**（resolved/unresolved/regressed）；redteam 内部的 `verifier`/`verify` = **评审内 finding 验真**（upheld/refuted）。建议改后者，`verify` 只保留一个含义。
三条硬事实：
1. hibot 契约已把这一结果命名为 `refuted` `[核实] audit-finding.schema.json + contract.md §3.2`，我们现在把同一件事叫两个名字；
2. 代码里本就双词并存：同一 `RunMeta` 有 `verify_refuted` 与 `refute_rate`；`Phase` 同时有 `Verify`（live）与 `Refute`（legacy，仅 serde 可读）`[核实] src/state/mod.rs:41-52` —— 改名是**收敛既有重复**，不是造词；
3. 唯一真会误伤的可见面是**配置角色**：hibot 渲染 Profile 时 `roles.verifier` 会被运维读成"给 verify kind 用的模型"。
改名映射（按层，见 0.8 批）：配置 `roles.verifier`/`review.verifiers` → `refuter`/`refuters`（**硬切，无别名**——尚无任何发布）；本地报告 `Finding.verify`/`VerifyVerdict` → `refute`/`RefuteVerdict`；`RunMeta.verify_*` → `refute_*`；`Phase::Verify` → `Phase::Refute`；隐藏命令 `verify-once` → `refute-once`；模板 `verifier.md` → `refuter.md`；模块 `consolidate/verify.rs` → `refute.rs`；`eval/verify_score.py` → `refute_score.py`；md 文案同步。
**角色安排（用户裁决）**：改完名后 `verifier` 这个名字腾出来，**给 hibot 的 verify kind 用**，定义为**可选角色**；缺省时**回退复用 `reviewer` 条目**，两者都缺才报配置错误。于是四个角色：`scanner` / `reviewer` / `refuter`（评审内验真）/ `verifier`（fix 验证，可缺省）。
**注意同名不同义的瞬时窗口**：旧本地配置里的 `roles.verifier` 会从“验真角色”变成“fix 验证角色”（静默变义）。因为尚未发布，不做兼容层；但 0.8 加一条**护栏**：review job 若发现存在 `verifier` 而缺 `refuter`，打 warning 提示“验真角色已更名为 refuter；`verifier` 现在只用于 verify kind”。
**前置**：`build_refute_prompt` + `adversarial-refute.md` 是死代码（**6 个单测调用、零产品路径**）`[核实] src/prompt/mod.rs:51,692,707,722,725,741,761` —— 必须先删（0.8 第 0 步），否则新 `refute` 与死 `refute` 撞名。
**另一处撞名**：`Phase` 枚举已有 **legacy `Refute`**（`[核实] src/state/mod.rs:52`，仅 serde 可读）。0.8 的做法：**删掉 live 的 `Verify`、把 legacy `Refute` 提升为唯一变体**（复用其 serde 名），而不是新增第二个 `Refute`。

**P16.1 · 两条流程必须结构上分开（不是一个流程的两个名字）** `[已裁 2026-11-23]`
它们只共享传输层（provider 在途门 / 重试 / usage 计量 / JSON 修复轮），**不得共享 prompt、输出契约、解析器与判定表**：

| 维度 | 评审内验真（更名为 **refute**） | hibot **verify** kind（fix 验证） |
|---|---|---|
| 目的 | 这条 finding 真的成立吗（降误报） | 修复是否真的发生、是否引入新问题 |
| 输入 | 单条 finding + 其位置快照 + 判例 | 单条 finding + **fix diff hunks** + 修复后快照 +（可选）原 review 证据 |
| 触发 | review 管道内部，`severity≥阈值 ∧ confidence≥floor` 自动触发（可关） | hibot 独立派一个 job（kind=verify），输入由 `findings.json` 给 |
| 输出 | `RefuteVerdict { Upheld, Refuted }` → `Finding.refute` | `FixVerifyStatus { Resolved, Unresolved, Regressed }` → `verifications[]` |
| 消费者 | 内部 verdict / blocking 计数 / dedup | hibot 报告与评论；**不参与 review verdict** |
| 失败语义 | 保守方向 = Upheld（宁漏不误伤）；整轮标 incomplete | 保守方向 = **`inconclusive`**（未核验）+ 整体 `partial`（hibot B6）；不塞 `unresolved` |
| 报告字段 | 本地 `Finding.refute` → hibot finding 的 `refuted` | 顶层 `verifications[]`（`finding_fingerprint`/`status`/`evidence`） |

**硬规则**：① 两个枚举之间**不提供互相转换**（`refuted ≠ resolved`，转换即语义谎言）；② 各用自己的模板（`refuter.md` vs 新建 `fix-verify.md`）与自己的解析入口（per-backend 单点）；③ 各用各的判定表，不抽“公共判决器”。
**代码落点**：`src/consolidate/refute.rs`（验真）与 `src/fixverify/`（verify kind），后者不 import 前者。
**为什么不能合**：两者 prompt 目标相反（一个在找“能不能推翻这条”，一个在找“修复有没有发生”），合并会让模型同时背两个任务，输出契约也会退化到二者交集——而 hibot 契约要求 `verifications` 逐条覆盖 + 证据必填。

**P17 · 配置契约外化：`config schema` + `config check`（hibot A2 已裁）**
问题：hibot 要 per-job 渲染 executor config，但渲染错了（拼错键/枚举错/漏角色）只能等 job 报错才发现。把契约变成**可机器消费的产物**：
- `<cli> config schema [--out <path>]`：从**类型**生成 JSON Schema（`schemars` 已在依赖，现仅 `domain/finding.rs` 用 `[核实]`），不手写。derive 到 `Config` 及 18 个类型 + 枚举；`MaxCells`（`"auto"|N`）手写 impl。身份：`$id: "urn:redteam:config/1"`、`title: "redteam executor config"`、`schemaVersion: 1`。
- `<cli> config check <path|json> [--kind triage|review|verify]`：形状 + 语义两层；错误带 **JSON Pointer**；**密钥类字段（`apiKey`/`token`/`secret`/`password`）单独报错**。位置参数（与 `verify-once <finding>` 同风格）。
- **输出与退出码（hibot 已定）**：诊断只走 **stdout** 一处，格式 `ERROR|WARN <JSON Pointer> <code>: <message>`，末行 `RESULT ok|error <n> errors, <m> warnings`；**退出码严格二值 0/非0**（不设第三态；工具自身崩溃用 stderr 文案区分，不用退出码）；**`WARN` 不影响退出码**（要拦就升级为 `ERROR`）。**不加 `--format json`**（他们只看退出码，输出当日志）。
- 与 kind 命令退出码区分：`review`/`triage`/`verify` 的 0/1/2 只是人手调试便利，hibot 不看；**`config check` 的二值才是他们 CI 的判定依据**。
- **可发现性（契约只记发现约定）**：`capabilities.extensions.config_schema = {command: "<cli> config schema", schemaVersion: 1}`——**不叫 `profile_schema`**。
- **hibot 的用法**：只在 CI 用 schema 校验渲染样例；**运行时不 exec、不 vendor**。
- **语义层**（schema 表达不了）：至少一个 scanner、kind 必需角色、数值下限、provider 名不含 `/`——复用 `Config::validate()`。
- **严格度分场景**：job 模式**未知键 = error**（渲染器与实现漂移必须报）；本地用户配置维持 warning。
- **稳定物与防漂移**：生成物入库 `dist/config.schema.json`，`make verify` 断言重生成零 diff；schema 内**只嵌 `schemaVersion`，不嵌 crate 版本**。
- **待回报（hibot B1）**：先本地用 `config check` 跑一遍他们示例 `config/executor.redteam.json`，差异逐条列回。
- **契约版本**：`schemaVersion: 1`；破坏性变更→升 2，hibot 拒发旧 schema 的作业；hibot 可把 schema 的 sha256 与 `rendered_sha256` 一起记账。

---

## 3. Phase 0 · 现有引擎收口（M21e 1/2/4/5/6/7；aacr 项 3 除外）

> 出口：**完成本次调整后的范围**——代码支线 0.1/0.4/**0.8** 完成；数据支线 **0.6 数据归档**、0.2/0.5、0.3、0.7 按前述顺序落定。
> **不宣称**"M21 全部 ✅、看板归零"——aacr `gate`、`smoke`、`R20` 一并转列**延期债**并在看板留行；ship 单列"待重估"。
> **依赖声明（两条平行支线，详见 §11）**：
> **代码支线**（阻塞 Phase 1）：0.1 → 0.4 → **0.8**（0.8 可与 0.1/0.4 并行，但必须早于 Phase 1）；
> **数据支线**（可与 Phase 1 并行）：0.6 → 0.2/0.5 → 0.3（拆仪器须在数据归档后）→ 0.7。

| 项 | 内容 | 候选落点 | 依赖 | 验收 | 规模 |
|---|---|---|---|---|---|
| 0.1 | **M21e-2 旧 CLI 面残余清理**：核对 `--models`/`--models-all`/旧 `--priority` 的**flag 名**与错误提示；`resolve_models` 角色驱动版收口 | `src/cli/mod.rs`、`src/main.rs`、`plugins/redteam/commands/*.md` | 无 | `--help` 不再出现上述 flag；`make verify` 零 diff。**不要求全文 grep 零命中**（`priority` 作普通词合法） | 0.5–1d |
| 0.2 | **M21e-4 校准收口**：R3/R8/R9/R12 + 同组 hold。**开工前先读最新 D22 与 followup**，核对是否仍成立（D22 已结案"拆格不做"，不复活） | `src/config/mod.rs` + followup R 表回填 | 0.6 数据 | 每项"默认值+依据数据+可回滚开关"；`eval/risk_replay.py` 对拍绿 | 2–3d |
| 0.3 | **M21e-5 仪器拆除**：**数据采集/归档完成后**才拆相关仪器；dev 基线归档、scratch 脚本归 `eval/`；清「M21e 清扫」小债。**历史债先核实消费者再删**；D22 保留的观测字段（`PackStats.diff_bytes`、md 组表）**不删** | `eval/`、`src/domain/report.rs` 注释、`src/provider/registry.rs:81` | 0.6 数据 | 每项删除有"消费者已核实为零/已迁移"记录；`cargo test` 全绿 | 1–2d |
| 0.4 | **M21e-6 `--help` 定稿 + README 重写**；`make gen` 同步 codex-skills | `src/main.rs`、`README.md`、`plugins/redteam/commands/`、`Makefile` | 0.1 | `make verify` 零 diff；旧 flag 零残留 | 2–3d |
| 0.5 | **M21e-1 第二家族 scanner 落地验证**：第二家族模型加 `scanner` 角色；疑点并集/agreement 入优先级键；合并口径收口 | `src/orchestrate/`、`src/consolidate/dedup.rs`、配置样例 | 0.6 数据 + 双家族模型 | PR181 oracle 覆盖 ≥ 单家族；墙钟不增 | 3–5d |
| 0.6 | **dogfood 数据采集**：定目标 PR、跑 N 轮、归档 `.next/private/` | `.next/private/`、`eval/` | provider/凭据 + 目标 PR | 可复算数据 | 1–2d 组织 + 跑数等待 |
| 0.7 | **范围收口与账目登记**：done.md 记明细；`.next/roadmap/` 标注非 aacr 范围完成；**aacr/ship 各留延期行**；看板逐行复核；HANDOFF 更新/删除；登记本里程碑（编号待确认） | `.doc/roadmap/`、`.next/roadmap/`、`.next/HANDOFF.md` | 0.1–0.5 结论 | 看板每行有状态与日期，延期项仍可见 | 0.5–1d |
| 0.8 | **术语分家批（P16）**：**第 0 步 = 删死代码 `build_refute_prompt`/`adversarial-refute.md`（仅单测调用，不依赖 0.6 数据）**；再分层硬切（**无别名/无 serde alias/无 clap alias——尚无发布**）：① 配置 `roles.verifier`/`review.verifiers` → `refuter`/`refuters`（注意：前者是**角色层**、后者是 **review 配置节**，不同层不冲突），**同一提交内新增可选 `verifier`（fix 验证，缺省回退 reviewer）** + 护栏 warning；② 本地报告 `Finding.verify`/`VerifyVerdict`/`RunMeta.verify_*` → `refute*`；③ 内部符号 + `Phase::Verify`→`Refute` + 模板 `verifier.md`→`refuter.md` + 模块 `verify.rs`→`refute.rs` + `verify-once`→`refute-once` + `eval/verify_score.py`；④ md 文案与文档 | `src/config/`、`src/domain/`、`src/consolidate/`、`src/prompt/`、`src/state/`、`src/cli/`、`eval/` | **无**（死代码删除已内置于本批第 0 步，不锁 0.3/0.6） | 每层 `cargo test` 绿再下一层；**不欠兼容层**；`--help` 与配置样例只用新词；护栏：review job 有 `verifier` 无 `refuter` → warning；自检：`verifier` 仅剩新语义与历史文档命中 | 1–1.5d |
| 0.8-note | **硬切的已知影响（不修，但要知道）**：本机 `~/.redteam/config.json` 需手改一行（`verifier`→`refuter`）；旧 `proj.db` 历史报告的 `refute` 会读成空（无 `deny_unknown_fields`，**静默丢失**）；旧作业 state 里的 `"verify"` 反序列化失败（`status`/`resume` 报错）。未发布 + `proj.db` job 结束即弃 → 可接受；需留史就先导出 | — | — | 在 0.8 提交说明里写明这三条 | — |

**Phase 0 不做**：aacr `gate`/`smoke`/`R20`、ship 六条输入条件、拆格（D22）。

---

## 4. Phase 1 · 外派运行基础（与 kind 无关）

> 出口：`redteam review --job-dir <fixture>` 完成"读 manifest → 校验输入 → 写合法 report.json"，中途 SIGTERM 能落 partial（心跳属 Phase 5）。

| 项 | 内容 | 候选落点 | 验收 |
|---|---|---|---|
| 1.1 | **job IO**：manifest 解析（`job-1` + `job.{kind}-1`）、artifact 读取 + sha256 + schema 常量 + **相对路径以 job 目录为基准**并 canonicalize 后必须落在其中（`paths.repo` 允许 `../repo` 但须为兄弟）、**`report.json`/`state.json` 恒走 `*.tmp` → `rename`**（D73：正常结束也一样）；**不读 stdin、不依赖 cwd** | `src/job/{manifest,artifact,io}.rs` | sha256 不符/缺 key/路径逃逸 → `error` + reason；给 stdin 也会忽略；**cwd 改到任意目录结果不变**；写盘中途 kill → 只可能“无报告”或“上一份完整报告” |
| 1.2 | **executor config 加载**（P6/A1）：**只读 manifest `paths.config`**（固定 `config/executor.<cli>.json`）+ 复用 `from_json_warned`（定义在 `[核实] src/config/mod.rs:501`）；env-only 凭据；job 模式**不接受任何配置 flag** | `src/config/mod.rs`、`src/job/executor_config.rs` | 脏 `REDTEAM_HOME` + 脏 `secrets.json` 都读不到；`paths.config` 缺失/不存在 → `error`；`--config` 在 job 模式下被 clap 拒收 |
| 1.3 | **报告信封 + 指纹 + status**（P3/P4/P5） | `src/job/{report,fingerprint,status}.rs` | `audit-pr-468` 作**结构**金标；指纹测试向量（空行/缩进/大小写）；status 表驱动 + 反向断言（空 findings 推不出 `ok`） |
| 1.4 | **输出语言**（P15）：`Config.language` 枚举 + 解析顺序 + `OutputLang` 贯穿全部 prompt builder（scanner/reviewer/refuter/verifier/triage 三轴）与 `status_reason` 对照表；`extensions.redteam.language` | `src/config/mod.rs`、`src/prompt/mod.rs`、`src/job/report.rs` | 用例：**job 模式配置 `language=zh_CN` → 中文**（不是 en_US）；job 模式缺配置 + `LANG=zh_CN` → 仍 en_US（不读 env）；本地模式缺配置 → 与今日 locale 行为逐字一致；非法值 → 配置错误 |
| 1.5 | **生命周期**（P8/P9）：deadline、**`JobDirCellSink`**、SIGTERM → partial → ≤60s | `src/job/lifecycle.rs`、`src/cli/mod.rs` | 注入 SIGTERM → partial + reason；无遗留子进程（`procinfo`）；**不触碰 `JobStore`** |
| 1.6 | **夹具与管线确定性**：`clean`/`known-vuln`/`partial`；`FakeChatBackend`；**夹具钉住 `language`**；同夹具连跑两次断言指纹集合一致（**仅确定性后端**，hibot B3） | `tests/job_conformance.rs`、`tests/fixtures/job/*` | 5 项可本地化契约校验全绿；**`partial` 夹具用“两个 provider，其中一个 `apiKeyEnv` 不注入”构造**（hibot C5，不引入专有失败开关） |
| 1.7 | **参数面**（P1/P13）：`triage`/`verify`（不隐藏）+ `review --job-dir`；kind 一致；退出码 | `src/main.rs` | `review --job-dir X --base main` → 非零；kind 不一致 → `error` |
| 1.8 | **错误路径的报告可写性**（hibot B5 已裁）：① **仅 manifest 不可解析** → 不写 `report.json`（runner 按无报告=error），**禁止伪造 job_id/SHA/零值 summary**；② triage 的 `summary` **只在 `status=ok` 时必需**，`partial` 可省、`error` 不写摘要；③ verify 的 `verifications` **仅当输入指纹集合为空时允许空数组**，其余必须逐条覆盖 | `src/job/report.rs` | 三 kind 各一条：manifest 损坏→无报告非零退出；triage error→有报告但无 summary；verify 空输入→有报告且 `verifications: []` |
| 1.9 | **配置契约外化**（P17）：`config schema` + `config check <path|json> --kind`；生成物入库 `dist/config.schema.json`，`make verify` 零 diff；job 模式未知键与密钥类字段 → error。**生成 schema 的硬前提（hibot 要求，四条缺一不可）**：① 顶层 `language` 已入 `Config`（1.4）；② `review.verifiers{enabled}` 已在（**已具备**）；③ `roles` 枚举已含 `scanner`/`reviewer`/`refuter`/`verifier`（**依赖 0.8 改名**）；④ provider `kind` 为 kebab-case（**已具备**，`openai-compatible`）。**否则 hibot 示例会被判 error** | `src/config/`（derive + schema 生成）、`src/cli/`、`dist/config.schema.json`、`tests/packaging.rs`、**`Makefile`**（`verify` 加 `git diff --exit-code dist/`） | 四条前提齐备后才生成；合法样例通过；未知键/错枚举/错类型/密钥字段四类各报错；`--kind review` 缺 scanner 报错；**退出码二值 + WARN 不影响退出码 + 诊断只走 stdout**（hibot 三点要求）；**用 hibot 示例跑一遍并回报**（B1） |

---

## 5. Phase 2 · review 端到端（解锁 hibot M2）

**2.0 数据流（先写清再实现）**

> **Phase 2 出口附带**：`capabilities.kinds = ["review"]`——此时**只有 review 实现完毕**，不得提前声明 triage/verify（hibot 只会派已声明的类型）。
```
inputs.diff_patch ──sha256校验──► FileChange[] ──► run_code_review(args, registry, cfg, changes, root, head_sha, sink, carried)
                                            （**不经** run_review_dispatch：那条路会重新采集 git diff）
inputs.merge_base ──读内容──► 分析基准 base（风险路由/证据收集）
paths.repo ──► root（只读检出，HEAD 必须等于 pr.head_sha）
```
`run_code_review` 已按 `changes` 入参接收全量变更 `[核实] src/cli/mod.rs:301-330`；`split_diff_by_file` 可把 patch 切成 `FileChange` `[核实] src/evidence/git.rs:178-186`（需放宽可见性）。

| 项 | 内容 | 候选落点 | 验收 |
|---|---|---|---|
| 2.1 | **输入真相源**：优先消费已校验 hash 的 `diff_patch`；git 只用于证据收集；两边都可得时做一致性校验（不一致只披露） | `src/evidence/git.rs`、`src/cli/mod.rs` | 只用 patch 也能跑完整评审；差异进 `extensions` |
| 2.2 | **投影**：`root=paths.repo`、**分析基准 `base=merge_base` 内容**（它是 diff 左端，**可能 ≠ `pr.base_sha`**）、`scope=branch`、`background/resume/incremental_from/pack_only=false`、不打印；**校验 HEAD == `pr.head_sha`** | `src/cli/mod.rs` | 投影与手写等价；HEAD 不符 → `error`；**不得断言 `merge_base == pr.base_sha`** |
| 2.3 | **消费 `prior_report`**：`summary.routing.focus[]`→`args.focus`；`summary.routing.specs[]`→`args.spec`（须在 repo 内且可读；读不到 → `partial`+点名）；`summary.routing.wall_clock_s` → 建议预算（受 manifest 硬上限约束）；`summary` 的 risk/漂移只调**预算与镜头**，**不改阻断阈值**；只有 triage **blockers** 视为已确认，不是全部 findings | `src/cli/mod.rs`、`src/risk/` | specs 缺失 → `partial`；阻断阈值不变（断言）；**断言只读 `summary.routing`** |
| 2.4 | **`dismissed.csv` 闭环**（P14/C2）：严格 RFC4180 + 固定表头；引入 `csv` crate；`HashSet<fingerprint>`；被驳回项**不复报**、不阻塞，在 `extensions.redteam.ledger = {dismissed, actor, reason}` 留审计；**绝不写 `refuted: true`**（人的决定 ≠ 证据证伪）；**verdict 与评论的过滤责任在 hibot**；`stage0_risk` 不再当 sqlite | `src/job/dismissed.rs`、`src/orchestrate/deep.rs:723` | 引号/内嵌逗号/CRLF 正确解析；坏格式 → `error`；命中 → 审计字段完整且 `refuted` 仍为 false |
| 2.5 | **quote 逐字校验**：模糊定位只找候选；命中后**用检出原文替换 `quote` 与 `line_start/line_end`**，再断言"报告行范围内逐字命中"。顺序固定：**重锚 → 修正 quote → 生成 fingerprint → 匹配 dismissed**。仍不命中 → 丢弃 + 计数 + `partial` | `src/job/finding.rs`、`src/positioning/` | 缩进/多空格/差一行 → 修正后命中；只改行号不改空白 **必须被判失败**；幻觉 quote → 丢弃且 partial |
| 2.6 | **repo 完整性**（hibot C1 已裁）：repo 是 detached、**非 shallow** 的完整检出（超大仓 mirror 可 blob-filter，但 checkout 所需 blob 已补齐）；head/base 对象本地可得、**merge-base 一定可算**。**缺必需对象 → `status=error`**，不得披露后仍报 `ok` | `src/evidence/git.rs`、`src/job/status.rs` | 缺失对象构造 → `error`，不是 `partial`/`ok` |
| 2.7 | **语言落地校验（过程 + 结果）**：scanner/reviewer/refute 的模型产出与报告字段随 P15；`status_reason` 也随；枚举/警告/心跳不随 | `src/job/report.rs` | `zh_CN`/`en_US` 各跑一次：断言 cell 输出、finding 文本、`status_reason` 三者语言一致；`run.warnings`/`extensions` 不变 |

---

## 6. Phase 3 · triage（解锁 hibot M3）

> 出口：三夹具产出合法 `report.triage/1`；字段有确定生成规则且可被 review 消费；与人工 pr-triage 对拍 3–5 个 PR。

| 项 | 内容 | 候选落点 | 验收 |
|---|---|---|---|
| 3.1 | **输入**：`pr_context`+`diff_patch`+`merge_base`（必需）；repo 检出按 Q4 冻结结果 | `src/triage/mod.rs` | 输入缺失 → `error` |
| 3.2 | **`StandardsMap`**：逐模块 doc→linter→peer；`no-standard` 仅三者皆无 | `src/triage/standards.rs` | 三种 source 各命中 |
| 3.3 | **三轴 lens + 扇出**：安全轴读 patch（7 类 sink 面，行内置信度 **0–10**，<8 丢弃）；漂移轴；意图轴。每轴 × N 并发，走 provider 子闸（**无 job 总闸**，见 Q15/D72）。三轴的行文本与 blockers 说明随 P15 语言 | `src/triage/lenses.rs`、`src/prompt/templates/triage-*.md` | 三轴各 1 fake backend → 行数正确；某轴失败 → `partial` 点名；`en_US` 下无中文残留（除引用原文） |
| 3.4 | **去重按轴分开**：安全/漂移按 `file+quote 归一+hint`；**意图未兑现项可能无代码锚 → 按声明身份去重**，不硬推 file+quote | `src/triage/tally.rs` | 无锚意图项不被误并；跨分片只计一次 |
| 3.5 | **确定性 tally 与字段生成**（单点实现 + 表驱动单测）：<br>· `blockers = structural + intent_undelivered`（local 不计）<br>· `security_tier`：**2 仅限 supply-chain/CI 投毒/代码执行且 confidence≥8**；其它安全问题 1；无 0<br>· `convention_drift`：structural>0→structural；仅 local→local；否则 none<br>· `risk` `[拟议]`：tier≥1 ∨ blockers≥3 → high；blockers≥1 ∨ scope_creep>0 → medium；否则 low<br>· `fit` `[拟议]`：有声明未兑现 → needs-scope-decision；scope_creep>0 且无声明 → possibly-redundant；否则 in-scope<br>· **`summary.routing`**：`focus`=top blockers 类别/文件、`specs`=StandardsMap doc、`wall_clock_s`=三档且 ≤ manifest<br>· report `confidence` 0..1：**8/10→0.8**，单点函数 | `src/triage/tally.rs` | 覆盖需求 §4.1 每条口径；`blockers != structural+intent` 断言失败；tier2 白名单有反例（SQL 注入不升 2） |
| 3.6 | **blockers 传给 review**：`extensions.redteam.blockers[]` 给可定位列表；**禁止把全部 findings 当阻塞项** | `src/job/report.rs`、`src/cli/mod.rs` | findings 5 条 / blockers 2 条 → review 只吸收 2 条 |
| 3.7 | **输出契约**：`findings[]` = 安全 tier≥1 + 结构性漂移（带指纹）；**`summary` = 九键 + `routing`（内嵌，不得提到顶层）**；`extensions.axes[]` | `src/job/report.rs` | schema 合法；`routing` 仅在 `summary` 内（用例断言）；无 verdict 类字段；语言随 P15 |
| 3.8 | **对拍**：3–5 个真实 PR vs 人工 pr-triage | `.next/private/`、`eval/` | 差异表；无"干净判成 tier 2" |
| 3.9 | **回归 Phase 2.3（顺序补偿）**：hibot B7 已把 `prior_report` 改为可选，但 M3 起由 orchestrator 保证 triage 前置——本项用**真 triage 报告**重跑 2.3 全部用例 | `tests/`、`.next/private/` | 真 triage 报告驱动 review：focus/specs/预算生效、阻断阈值不变 |
| 3.10 | **capabilities 增量：加 `triage`**（9.1 的按阶段声明） | `capabilities/redteam.json` | `kinds` = `["review","triage"]`，不含 `verify` |

---

## 7. Phase 4 · `verify` kind（fix 验证）——与 `refute` 零共享（解锁 hibot M4）

| 项 | 内容 | 候选落点 | 验收 |
|---|---|---|---|
| 4.0 | **流程隔离前置**（P16.1）：新模块 `src/fixverify/` + 新模板 `fix-verify.md` + 新枚举 `FixVerifyStatus`；**不 import `consolidate::refute`，不复用其模板/解析/判定表**（只可用 provider 传输层）。角色解析：`verifier` 有则用，无则回退 `reviewer`，两者都无 → `error` | `src/fixverify/`、`src/prompt/templates/fix-verify.md`、`src/config/` | 依赖审查：`fixverify` 对 `consolidate` 零依赖；角色回退用例（有 verifier / 无 verifier 有 reviewer / 两者皆无） |
| 4.1 | **覆盖不变量**：输入指纹集合 == 输出指纹集合；输入指纹格式非法 → **`error`**（不是逐条 unresolved） | `src/fixverify/mod.rs` | 少/多一条 → 断言失败；坏指纹 → error |
| 4.2 | **逐条 prompt（专属）**：finding + 该文件 fix hunks + ±N 快照 + **可选 `prior_report`（review 报告，作佐证而非结论）**；不喂整 PR。**前置假设：finding 已成立**（来自 hibot 编排器传入的 triage blockers，见 2.3/3.6）——只判“修复是否发生/是否引入新问题”，**prompt 里禁止重新裁定 finding 成不成立**（那是 refute 的活）；注入中和沿用 `neutralize_structure`；`evidence` 语言随 P15 | `src/prompt/templates/fix-verify.md`、`src/fixverify/prompt.rs` | prompt 不含其它文件 hunk；**反例用例**：误用 `refuter.md` 时模板哈希断言失败（两族哈希必须不同）；喂 prior_report 时只取该 finding 相关证据 |
| 4.3 | **删除/改名/移行**：从 patch 解析 `removed|renamed` 与 rename 目标；删除 → 按"构造消失"判定（可 resolved，须证据）；改名 → 新路径重定位；跨文件修复只在相关 hunks 内取证 | 同上 | 三构造用例（删除即修复/改名后仍在/移行后消失）各 1 |
| 4.4 | **四态与证据**（`FixVerifyStatus`，hibot B6 已新增 `inconclusive`）：`resolved`/`unresolved`/`regressed`/`inconclusive` 均需证据；quote 必须逐字命中；**模型失败/无法定位/证据不足 → `inconclusive`**（不确认已修复也不确认未修复）；**出现 inconclusive ⇒ 报告 `status ∈ {partial,error}` 且必带 `status_reason`** | `src/fixverify/mod.rs`、`src/job/status.rs` | provider 401 → 该条 `inconclusive` + 整体 `partial` + reason；**不得把失败塞进 `unresolved`** |
| 4.5 | **自建质量夹具**：`audit-pr-468` 的 verify 示例是编造结论+节选 diff `[核实]`，**不作质量金标**；自造合成夹具（**四态各一** + 删除式修复） | `tests/fixtures/job/verify-*` | 四态判定符合构造预期 |
| 4.6 | **空输入**（hibot B5）：`findings` 指纹集合为空 → **允许 `verifications: []`**，报告照写（`status=ok`）；**不伪造一条判决** | `src/fixverify/mod.rs` | 空输入 → 有合法报告且 `verifications` 为空；非空输入少一条 → 断言失败 |
| 4.7 | **capabilities 增量：加 `verify`**（9.1 的按阶段声明） | `capabilities/redteam.json` | `kinds` = 三 kind 齐（此时才齐） |

---

## 8. Phase 5 · `out/state.json` 心跳（hibot M6，可选，可与 2–4 并行）

| 项 | 内容 | 候选落点 | 验收 |
|---|---|---|---|
| 5.1 | `InflightGate` 增 `inflight()`/`waiting()`；`ProviderRegistry::snapshot()`；`providers[].name` = 配置 provider key（P12）；`llm.max_concurrency` 按 P12 口径。**计数唯一来源 = `InflightGate.active`**；`queue::inflight_count()` 是队列项不是请求；`FakeChatBackend` 的 `inflight/peak_inflight`（`[核实] src/provider/tool_loop.rs:284-350`）是测试替身，**不进心跳** | `src/provider/registry.rs:69-97,406-410` | 并发 2 → `in_flight==2`；429 退避中 `retrying>0` 且不计入 `in_flight`；name 不是 apiType/model id；**心跳值与 gate 快照逐字一致** |
| 5.2 | 心跳任务：≤5s 原子覆写；`phase` 用 **job 视角的通用三值**（附录 F）：`preparing` → `running` → `finishing`；**不写 done/fail**（终态唯一真相 = `report.json.status`）。不直挂 `CellSink::on_phase`（该钩子只吃内部 `Phase` 枚举、triage/verify 无变体）；`progress{done,total}` | `src/job/state.rs` | 采样三次 `at` 递增且间隔 ≤5s；`providers[]` 正确；三 kind 的 phase 都在三值内；**退出后最后一帧不是终态值** |
| 5.3 | 缺失/过期语义：停更不影响正确性；退出后不再写 | 同上 | 文件被删后不 panic |

---

## 9. Phase 6 · 能力声明、联调、验收

| 项 | 内容 | 验收 |
|---|---|---|
| 9.1 | `capabilities/redteam.json`：**`kinds` 必须按实际实现增量声明**（hibot 只会派已声明的类型，提前声明会把未完成能力派出去）——`review` 在 Phase 2 出口后加、`triage` 在 Phase 3 后加、`verify` 在 Phase 4 后加；其余字段：`form:cli`/`config_delivery:job_native`/`limits`/`requires{repo_checkout, pr_context, network}`；**`requires.*` = 对“所有声明 kind”的要求**（hibot B4），单 kind 必需输入以 `job.<kind>` schema 为权威；**声明 `pr_context: false`**（triage 仍会拿到它）；另声明 `extensions.config_schema` | hibot 能按声明派活；**任一时刻 `kinds` 里不含未实现项** |
| 9.2 | **配置契约交接（hibot B1/A2）**：把 `dist/config.schema.json` + `config check` 交给 hibot；hibot 只在 CI 调 `config check`（**不解析 schema、不 vendor、不生成类型**），并把 schema 哈希与 `rendered_sha256` 一起记账。**draft-07 已被接受**（只有将来要用 schema 生成类型/做静态校验时才会要求 2020-12，那是新增需求） | hibot 渲染样例全过 `config check`；双方存档同一份 schema 哈希 |
| 9.3 | **端到端联调**：hibot runner 真调（job_native）跑一次真 PR | hibot conformance 六项全过 |
| 9.4 | **验收矩阵**：三 kind × 三夹具 × 两语言；契约自动项 6 条。**资源分位数不我方产**（hibot C5：他们从 `usage`/`state.json` 统计） | 可复跑矩阵；README 只记单 job 量级参考 |
| 9.5 | **文档收尾**：`--help` 增补；README 加"作为 hibot 执行单元"与"报告语言"；`redteam-track.md` 回执（hibot 侧改） | 文档自洽 |

---

## 10. Phase 7 · 发布面与分发（本轮新增；可与 2–6 并行）

> 参照 `~/hiapi/cli`：install script + R2 + `update` 自更新 + `scripts/release.sh` + release runbook。
> **首期平台：仅 macOS arm64**。Linux/Windows 归 M20（followup 已挂"linux/win 交叉构建 + T11"）。
> 出口：`curl -fsSL <url>/install.sh | sh` 装得上、`redteam update` 能在 standalone 安装上自更新、release 可一键重发、runbook 可照着做。

**7.0 两条分发渠道与边界（先定这条，其余都好办）**
| 渠道 | 形态 | 谁更新 | 版本来源 |
|---|---|---|---|
| 插件（marketplace） | `plugins/redteam/`（launcher + `bin/darwin/aarch64/redteam` 入库） | 插件市场/`git pull`；`make build` 重建 | `Cargo.toml`（单一真相） |
| standalone CLI | `install.sh` 装到 `~/.local/bin/redteam` | `redteam update` | 同上 |

**规则**：`redteam update` 在**插件路径下拒写**（见 7.4）；两渠道版本必须由同源派生，禁止手改一处。

| 项 | 内容 | 候选落点 | 验收 |
|---|---|---|---|
| 7.1 | **版本与发布文本单一来源**：`Cargo.toml` 为准；两个 `plugin.json` 的 `version` 由 `make gen`（或 `make version-sync`）写入/校验；报告 `executor.version` 与 capabilities 取 `env!("CARGO_PKG_VERSION")`；**新增 `CHANGELOG.md` 骨架**（`[Unreleased]` + Keep-a-Changelog），供 release notes 提取。现状：三处版本不一致（0.1.0 vs 1.0.0）、无 CHANGELOG `[核实]` | `Cargo.toml`、`Makefile`、`scripts/gen-codex-skills`、`tests/packaging.rs`、`CHANGELOG.md` | `make verify` 断言：Cargo == 两个 plugin.json == 二进制 `--version`；CHANGELOG 顶层含 `[Unreleased]` |
| 7.2 | **`scripts/release.sh`**（改自 hiapi）：前置检查（工作区干净、**`tag` 必须指向 `HEAD`**、tag==Cargo 版本、`gh` 已登录、`make verify` 绿）→ `make gen && make build` → 打包 **`redteam-{version}-aarch64-apple-darwin.tar.gz`**（二进制 + README + LICENSE）+ `SHA256SUMS` → 打 tag/推 tag → `gh release create`（**预发布带 `--prerelease`**；资产含 `SHA256SUMS`、`install.sh`）/已存在则 `upload --clobber`；**release notes 从 CHANGELOG 对应段提取**（`CHANGELOG.md` 的初始骨架在 **7.1** 建） → R2 镜像（可选）→ **下载回验**（HEAD 200 + 下载比对 SHA256）。预发布不更新 `latest`；`--dry-run`/`--skip-r2`/`--no-push` 变体 | `scripts/release.sh`、`dist/`、`LICENSE`、`CHANGELOG.md` | 一次 `--dry-run` 产物齐全；tag≠HEAD 时拒发；rc 不污染 `latest` 且 GitHub `releases/latest` 不指向 rc；`curl <release>/install.sh` 可达；发布后回验全绿 |
| 7.3 | **`install.sh`**：`REDTEAM_VERSION`/`REDTEAM_INSTALL_DIR`/`REDTEAM_BASE_URL`/`REDTEAM_FROM`（`github`\|`r2`，**默认 github**——本仓公开，无需 R2 凭据）；`latest` 解析；`SHA256SUMS` 校验；staged `.tmp.$$` + `mv` 原子安装；PATH 提示；装完跑 `--version`；**平台守卫**：非 `Darwin`+`arm64` 明确报"首期仅 macOS arm64" | `dist/install.sh`、`scripts/release.sh` | 在干净 `HOME` 下装成功；改一位哈希 → 中止且旧二进制未被覆盖；Intel Mac → 明确报错 |
| 7.4 | **`redteam update`**：`--check`（只查不写）/ `--version <v>` / `--force`；同目录锁（`.redteam-update.lock`，用 `libc` flock 或 `fs2`）；原子替换；替换后跑新二进制 `--version` 自检；失败不覆盖旧件。**插件路径守卫**：当前 exe 落在 `plugins/redteam/bin/` 下 → 拒绝并提示"插件版随插件更新，或 `make build`"（`--force` 才写） | `src/cli/` 新子命令、`src/update/*` | 自更新成功；`--check` 零写入；插件路径下默认拒绝；并发两次 update 第二个等锁 |
| 7.5 | **R2 镜像布局**：复用 `hiapi-static` 桶、前缀 `redteam/`（`redteam/install.sh`、`redteam/latest`、`redteam/releases/vX/...`）；全部 `--cache-control no-cache`（CDN 会供旧对象）；凭据沿用本机 `.env` 四键（`R2_STATIC_ENDPOINT`/`R2_ACCESS_KEY_ID`/`R2_SECRET_ACCESS_KEY`/`R2_BUCKET`，可选 `R2_BUCKET_DOMAIN`），**不入库** | `scripts/release.sh`、`.env`（gitignored） | `https://static.hiapi.ai/redteam/latest` 返回正式版；预发布不更新 |
| 7.6 | **发布 runbook**：前提 / 版本准备 / 发布 / 产物核对清单 / 安装验证（含篡改拒绝用例）/ R2 镜像 / 回滚（`latest` 指回上一版、重发资产）/ 已知限制（**未签名未公证**：macOS 首次运行提示与 `xattr -d` 绕过；插件版 `bin` 入库带来的 diff 需单独提交） | `.doc/plans/`（私有）或 `docs/release-runbook.md`（若决定公开） | 照 runbook 从零发布一次成功 |
| 7.7 | **平台扩展位（只留位，不实现）**：launcher 已按 `uname` 分派 os/arch `[核实]`；`install.sh` 的 `TARGET` 收敛为单点常量 + 守卫；Linux/Windows 需交叉构建与 `install.ps1`，归 M20 | `plugins/redteam/bin/redteam`、`dist/install.sh` | 加平台只改常量与产物矩阵，不动主逻辑 |

**Phase 7 内部顺序**：7.1 → 7.2 → 7.3 → 7.4 → 7.5/7.6；7.7 随 7.3 留位。
**依赖**：0.1/0.4（CLI 面与 `--help` 定稿）与 1.7（命令面）之后即可开工；**不依赖** hibot 联调（9.x）。

---

## 11. 依赖与顺序

```
Phase 0 是**两条平行支线**，不是一条线性链：

【代码支线 —— 阻塞 Phase 1】
  0.1 旧 CLI 面清理 → 0.4 --help/README → 0.8 术语+角色硬切 → **Phase 1 开工**
  · 0.8 可与 0.1/0.4 并行，但**必须早于 Phase 1**（它改角色名，1.2 与 Phase 2 都依赖）

【数据支线 —— 与 Phase 1 并行，不阻塞代码】
  0.6 数据采集 → 0.2 校准收口 / 0.5 第二家族 → 0.3 拆仪器（**必须在数据归档之后**）→ 0.7 账目登记
  · 0.3 的“拆仪器”晚于 0.6 的数据归档（提前拆就没数据了）
  · 0.7 依赖 0.1–0.5 的结论；但 **hibot 接入里程碑的登记要在 Phase 1 开工前完成**，避免两套账并行

Phase 1 (外派基础) ──► Phase 2 (review) ──► hibot M2
        │                    └──► Phase 3 (triage) ──► hibot M3
        │                                    └──► Phase 4 (verify) ──► hibot M4
        ├──► Phase 5 (心跳，可并行) ──► hibot M6
        ├──► Phase 6 (能力声明/联调/验收) ◄── 需 Phase 2+3+4 全齐 + 1.9 产物
        └──► Phase 7 (发布面，可并行；门禁 = 0.4 + 1.7 + make verify)


Phase 1 内部硬序：1.1/1.2/1.3/1.4 → 1.5 → 1.6/1.7/1.8/**1.9**（1.9 的 schema 可先写，但其 `config check --kind` 验收要等 1.7；**且生成 schema 必须等 0.8 改名 + 1.4 language 落地**，否则 hibot 示例会被判 error——hibot 已把这条列为硬前提）
（1.5 不可后置：中断会丢已完成成果，而"无报告"在 hibot 侧等于 error）
```

注意：`prior_report` 已由 hibot 改为**可选**（Q14 关闭），M2 顺序不变；M3 起由 hibot 编排器保证 triage 前置。Phase 2 仍用**手写夹具** `prior_report` 验收，**Phase 3.9 用真 triage 报告回归 2.3**。

**跨阶段不变式（每批都验）**：① 报告原子写；② **除“manifest 不可解析”外，任何失败路径都写出合法 `report.json`**（hibot B5）；③ job 模式零写 repo；④ 报告不含 cost；⑤ 指纹实现只有一处；⑥ 报告 quote 逐字可查；⑦ **job 模式零读 env**（语言/密钥/并发上限全来自配置）；⑧ **配置契约只有一处真相**（schema 从类型 derive，不手写；生成物与代码零 diff）。

---

## 12. 契约裁定台账（hibot 已冻结，commits 370d96e/e01a449/cce2645/7305161）

> 版本策略（hibot）：M2 验收前 `hibot.audit.*` 的 `/1` 视为 **draft**，语义修订不 bump，逐条记 contract §8.1；M2 后冻结。
> 下表为**已裁**结论，本计划已按此更新。S1/S2 两条已由用户裁决；`config check` 输出与退出码由 hibot 定为**退出码二值 0/非0、WARN 不影响退出码、诊断只走 stdout、不做 `--format json`**。

| Q | 裁定 | 对实现的影响 |
|---|---|---|
| Q1 配置契约 | 先给 `config schema` 产物，hibot 据此冻结样例；用 `config check` 跑他们的 `config/executor.redteam.json` 并回报差异 | P17 命令名回 `config`；9.2 改为"交接 + 回报" |
| Q2 `base_sha` | `report.input` = manifest `pr` 的 **`head_sha`/`base_sha` 原样回显**；`inputs.merge_base` 是**实际 diff 左端，可能 ≠ `pr.base_sha`**（示例故意不同）；**不得用两者相等做判断** | 2.2/2.5 改为回显；`base` 分析基准仍取 `merge_base` |
| Q3 指纹一致性 | 指纹**继续含 `title`**；`title`/`quote` 是身份字段，可改写的只有 `body`/`recommendation`/`evidence`；§7-6 **只对确定性后端**成立，真实模型只观测、不作门禁 | P4/1.6 按此写死；不引入交集率 |
| Q4 `requires` | 语义 = "对**所有声明 kind** 的要求"，只用于收窄派发；单 kind 必需输入以 `job.<kind>` 为权威；**声明 `pr_context: false` 可接受** | 9.1 改口径 |
| Q5 repo | detached、**非 shallow** 完整检出，merge-base **一定可算**；缺必需对象 → **`error`**，不得披露后仍报 `ok` | 2.6 改写（不再是"逐项降级披露"） |
| Q6 错误/空输入 | ① `verifications` 空数组合法**仅当输入指纹集合为空**；② triage `summary` **只在 `status=ok` 时必需**；③ **manifest 不可解析 → 不写报告**，禁止伪造 job_id/SHA/零值 summary | 1.8/4.6 按此写死；"写不出合法报告就不写"仅剩 manifest 场景 |
| Q7 未核验 | 新增第四态 **`inconclusive`**；出现即要求报告 `status ∈ {partial,error}` + `status_reason`；**不得塞进 `unresolved`** | 4.4 改四态；附录 D 去掉 `verified:false` 方案 |
| Q8 dismissed | **hibot 按指纹同时过滤 verdict 与评论**；我们可以不复报，但**不得写 `refuted: true`** | 2.4 改口径 |
| Q9 bot 评论 | `comments[]` **只含非 hibot 评论**（sticky 与命令回复已剔除）；其它 bot 保留，`is_bot` **必填** | triage 意图轴**不再自行排除 hibot**；`is_bot=true` 的评论只在判“其他 bot 的意见”时使用（3.3） |
| Q10 `unresolved[]` | 语义是"评审未能定论、需人判断"，**不是**"未修复"；v1 不产 | 维持不做 |
| Q11 `paths.config` | **保留该名**，值固定 `config/executor.<cli_name>.json`（schema 有 pattern） | P6/1.2 改回；示例已更新 |
| Q12 夹具/资源 | `partial` 夹具 = 两个 provider，其中一个 `apiKeyEnv` **故意不注入**（确定性失败）；资源分位数 **hibot 自己统计**，不要求我们产出 | 1.6 改写；9.4 删除"资源参考值"自产要求 |
| Q13 language | hibot 渲染进每份 executor config，缺省 `en_US`，**同一 PR 三 kind 冻结同值**，计入 `rendered_sha256`；hibot 不翻译 | 顶层键、**不走 passthrough**（S2 已定） |
| Q14 `prior_report` | 改为**可选**；有则必须是 `report.triage/1`；M2 顺序不变，M3 起由 orchestrator 保证 triage 前置 | 2.3/3.9 保留；Q14 关闭 |
| Q15 并发 | **不引入 `jobMaxConcurrency`**（D72）：保持 provider 子闸唯一；要收紧总并发由 hibot **调小渲染出的子闸值**；`state.llm.max_concurrency` 按实际生效口径（= Σ provider 帽） | P12 已改为 provider-only |
| Q16 命名 | **不同意 `--profile`/`paths.profile`**：job 模式**没有配置 flag**，`paths.config` 保留；`config_delivery` 保留；`flag` 只属适配器层 | P1/P6/1.2 已改；命令名按 A2 用 `config` |
| Q17 phase | 三值 `preparing → running → finishing`，不写终态；**v1 不展示**（评论模板与管理面都没有该字段） | 附录 F 维持 |

### 两条歧义已裁决（用户 2026-11-23 确认）

**S1 ✅ 命令名采用 `config`**（不叫 `profile`）
`<cli> config schema` / `<cli> config check <path|json> --kind`；`$id: urn:redteam:config/1`、`title: "redteam executor config"`、产物 `dist/config.schema.json`。
理由：manifest 字段 `paths.config`、文件 `config/executor.redteam.json`、`config_delivery` 全带 config 词——命令再叫 profile 就是"一个东西两个名"（同 `verify`/`refute` 的坑）。用户配置与 executor config 靠文档措辞区分。

**S2 ✅ `language` = 顶层键，由 hibot 从仓策略渲染（不走 passthrough）**
最终产物里是**顶层 `language`**；**hibot 自己写这个键**（org 默认 / 仓覆盖 / 缺省 `en_US`，计 `rendered_sha256`，同一 PR 三 kind 冻结同值）。**不经 passthrough**：passthrough 是 per-runner 运维块，按 runner 各写会中英混排；试图覆盖 `language` 会被 hibot **拒绝渲染**。我方 schema 只负责接受顶层 `language`。

### 双方最终口径（存档，三轮对齐后闭合）

| 主题 | 结论 |
|---|---|
| 路径 | `paths.config` = `config/executor.redteam.json`；**job 模式无配置 flag** |
| 角色 | `scanner` / `reviewer` / `refuter` / `verifier`；**同名原样搬运、不做映射** |
| 验真 | 我方**产品缺省关**；hibot 示例显式 `verifiers.enabled=true`，生产按 Profile passthrough 决定 |
| `language` | hibot 从**仓策略**渲染**顶层键**，**不经 passthrough**，同一 PR 三 kind 冻结同值 |
| 并发 | **无 job 级总闸**；provider 子闸唯一，`state.llm.max_concurrency` = Σ 帽 |
| schema 版本 | **draft-07 可**；将来要用它生成类型/静态校验再提 2020-12（新增需求） |
| 终态 | 进程退出 + `*.tmp`→`rename`；无报告 `no-report`、不可解析 `bad-report`；**不读退出码** |
| 未知键 | 我方 job 模式判 `error`；**hibot 渲染后必过 `config check`，不过则拒绝发 job** |
| `capabilities.kinds` | **按实现增量声明**；hibot 只派已声明类型 |
| `config check` 输出 | 诊断**只走 stdout**（`ERROR|WARN <pointer> <code>: <message>` + 末行 `RESULT ...`）；**退出码严格二值 0/非0**（无第三态）；**`WARN` 不影响退出码**；**不提供 `--format json`**（YAGNI，他们只看退出码） |

### 仍待我方交付（实现后）
1. `config schema` 产物 + 文件哈希；
2. `config check` 对其示例 `config/executor.redteam.json` 的逐条输出（人工核对已无 `verifier` 差异）；

### B1 预回报：人工核对 `config/executor.redteam.json`（hibot 已按我方意见更新）

（`config check` 尚未实现，先人工逐字段核对；实现后跑工具复核）

**兼容 ✓（无需改任何字段）**：
`kind: "openai-compatible"`（= `ProviderKind::OpenaiCompatible` 的 kebab-case serde 名）· `blockThreshold: "medium"`（Severity lowercase）· `tools: "two-stage"`（ToolsMode kebab-case）· `confFloor` · `review.maxConcurrency` · `providers.*.maxConcurrency` / `baseUrl` / `apiKeyEnv` · 模型层 `displayName` / `contextWindow` / `maxOutputTokens` / `perTurnTimeoutSecs` / `reasoning` / `roles.<role>.reasoningEffort` · **顶层 `language`**。

**已修（我方提出、hibot 采纳）**：
- `roles.verifier` → **`roles.refuter`**（deepseek-chat 现为 `scanner`/`reviewer`/`refuter`）——改名后 `verifier` 专指 verify kind 的 fix 验证；
- 新增 `review.verifiers.enabled = true`（示例显式开验真；该开关我方**产品缺省为关**，见下）。

**交界约定（写进双方文档，避免再漂）**：
1. `refuter` **配了不等于会跑**——需 `review.verifiers.enabled = true`；缺省关是我方产品行为（离线基线裁定），不是契约；
2. hibot 的 Profile 角色键与我们的 role 枚举**同名、原样搬运**，不做映射；
3. `verifier` 可选、缺省回退 `reviewer`；`reviewer` 与 `verifier` 都缺才报配置错误；
4. 我方 `config check` 对它这份样例的预期结果是**零错误**（含 `verifiers.enabled` 与 `refuter`）。

---

## 13. 风险登记

| # | 风险 | 概率/影响 | 缓解 |
|---|---|---|---|
| R1 | 配置契约靠口口相传 → 渲染器写错字段，到 job 才发现 | 中 / 中 | P17：schema 从类型 derive + `config check`（hibot CI 跑）；但仍需先用他们的样例回报差异（B1） |
| R2 | 真实模型下指纹集合不稳 → 验收口径争议 | 高 / 中 | 管线确定性 vs 模型稳定性分开度量；夹具钉住语言 |
| R3 | quote 只重锚不校逐字 → 报告被校验器拒 | 中 / 高 | 2.5 固定顺序 + 反例用例 |
| R4 | triage 三轴 × 分片烧钱/超时 | 中 / 中 | 降条目数/分片数，不砍轴 |
| R5 | verify 把“未核验”写成“未修复” | 中 / 高 | 用第四态 `inconclusive`（hibot B6）+ 整体 `partial`；**禁止塞进 `unresolved`** |
| R6 | 删除可能正是修复却被判 unresolved | 中 / 中 | 4.3 三路处理 |
| R7 | job 级并发改动波及本地路径 | 中 / 中 | 仅在 job 模式启用；本地对照用例逐字节不变 |
| R8 | SIGTERM 与 tokio 取消竞争 → 半截报告 | 中 / 中 | 单一封盘入口 + 原子写 + 信号注入测 |
| R9 | 双键空间回归 → 判例失效/误豁免 | 中 / 高 | `DismissKey` 类型化 + 遍历式不变式测试 |
| R10 | 把 `.redteam`/proj.db 写进共享检出 | 低 / 高 | P7 闸门 + 跑完断言 repo 树干净 |
| R11 | Phase 0 数据门拿不到目标 PR/凭据 | 中 / 中 | 0.2/0.5 等数据；Phase 1 可先开工 |
| R12 | 新增依赖（sha2/csv/tokio signal）与 rustls-only/体积约束冲突 | 低 / 中 | §15 依赖检查；纯 Rust 无 OpenSSL |
| R13 | 按旧账删"零消费者"字段，实际有观测价值（D22 保留项） | 中 / 中 | 0.3 先核实消费者；D22 保留项列"不删"清单 |
| R14 | `redteam update` 覆盖插件入库二进制 → git 脏 + 版本错配 | 中 / 中 | 7.4 路径守卫（默认拒写）+ 7.1 版本同源校验 |
| R15 | 未签名/未公证 → macOS 首次运行提示，被误判为损坏 | 中 / 低 | 与 hiapi 同策略：runbook 明确 + `xattr -d` 绕过文档；本期不做签名 |
| R16 | 发布脚本只能在有工具链的本机跑（无 CI） | 中 / 低 | 前置检查 + `--dry-run` + 下载回验（含 SHA256 比对） |
| R17 | 语言配置与 locale 双来源 → 同一 job 重跑结果不同 | 中 / 高 | P15 顺序：配置 > locale（仅本地）> job 缺省 en_US；不变式⑦ |
| R18 | 术语改名波及面大（`verify`/`verifier` 在产品代码里上千处引用），机械 sed 会误改 hibot `verify` kind 的新代码 | 中 / 中 | 分层提交（配置→报告→内部→文档）+ 先删死代码 + 建“不改”白名单（`src/job/`、契约新词）；每层 `cargo test` 绿 |
| R19 | 两条流程被“顺手复用”（共用 prompt/解析/判决表）→ 语义漂移：把 refuted 当成 resolved，或让 hibot 收到无证据的解决结论 | 中 / 高 | P16.1 硬规则 + 4.0 依赖审查 + 模板哈希分手用例；代码分层（`consolidate/refute.rs` vs `fixverify/`） |
| R20 | `verifier` 同名不同义：旧本地配置静默从“验真”变成“fix 验证”，验真角色悄悄消失 | 中 / 中 | 0.8 硬切同一提交引入新 `verifier` + 护栏 warning（review job 有 `verifier` 无 `refuter` → 提示）；无发布、无兼容负担 |
| R21 | schema 与实现漂移（手写 known-keys 表与 derive 双份维护）→ hibot 校验通过但程序读不懂 | 中 / 高 | schema 从类型 derive（不手写）+ 生成物入库 + `make verify` 零 diff；`shape_guard` 键表加一致性用例或改为从 schema 派生 |
| R22 | 同一 PR 的三个 job 语言不一致（hibot 各 kind 分别渲染）→ sticky comment 中英混排 | 中 / 中 | hibot 已采纳：`language` 由他们从仓策略渲染，**同一 PR 首个 job 创建时冻结、三 kind 复用**；我方在 `extensions.redteam.language` 披露实际值供核对 |

---

## 14. 规模与估算前提

| Phase | 人日 | 说明 |
|---|---|---|
| 0 | 11–19 | 含 0.6 组织成本与 0.8 改名（1–1.5）；不含跑数等待；aacr 不在内 |
| 1 | 7–9 | 新增模块 + 夹具 + 生命周期 + 语言解析 + 配置 schema 外化（1.9：18 个类型 derive + 两个子命令 + 入库生成物） |
| 2 | 4–6 | 投影/映射/后处理 |
| 3 | 6–9 | 三轴 + 标准解析 + tally + 对拍 |
| 4 | 5–7 | 专属 prompt 族（与 refute 分离）+ **四态**（含 inconclusive）+ 删除/改名/移行 + 覆盖不变量 + 隔离用例 |
| 5 | 2–3 | 快照 + 心跳 |
| 6 | 3–5 | 联调 + 验收矩阵 + 文档 |
| 7 | 3–5 | release.sh 1.5 / install.sh 0.5 / update 1 / runbook 0.5 / 版本同源 0.5–1 |
| **合计** | **41–63** | 与各项加总一致（11–19 + 7–9 + 4–6 + 6–9 + 5–7 + 2–3 + 3–5 + 3–5） |

**前提**：熟悉本仓、无外部等待（不含 hibot 侧改造与渲染器工时）、Q2/Q3 不返工、单点负责、发布机工具链齐备。若 hibot 要求 schema 改 2020-12（升 schemars 1.x）追加 0.5 人日。

---

## 15. 依赖与前置检查

| 需求 | 现状 | 处理 |
|---|---|---|
| SHA-256 | 依赖树无 `sha2`/`sha1`（`ring`/`aws-lc-rs` 只是 rustls 传递）`[核实] Cargo.lock` | 引入 `sha2`；fingerprint 与 artifact 校验共用一处封装 |
| RFC4180 CSV | 无 `csv` 依赖 `[核实]` | 引入 `csv`，不自写解析器 |
| SIGTERM | `tokio` 未开 `signal` feature `[核实] Cargo.toml:16` | 开 `signal` 或用 `libc` 装 handler，记录理由 |
| 自更新锁 | 无 `fs2`；`libc` 已在依赖 `[核实] Cargo.toml` | 用 `libc::flock`（零新依赖）或引入 `fs2`；二选一 |
| 发布工具链 | 无 release 脚本/CI `[核实]` | `aws` CLI（R2 上传）、`shasum`/`tar`、`gh`（已用于 hiapi）、`cargo` 目标 `aarch64-apple-darwin` |
| JSON Schema 校验（CI） | 无 `jsonschema`；hibot `validate.py` 需 PyPI `jsonschema` `[核实]` | 本仓用 `$HIBOT_SCHEMA_DIR` 可选校验；不复制 hibot schema 进公开仓 |
| 契约只读 | `~/hibot` 本地存在 `[核实]` | 若需扩展读取，先 `add_directory ~/hibot` |

---

## 16. 明确不做（v1 非目标）

- GitHub API、PR 评论、runner 调度、多仓编排（需求 §10）。
- `fix` 写代码（hibot 自管 skill）。
- `status` 子命令（心跳写文件即可）。
- `out/events.jsonl`（v1 只做 `state.json`）。
- 理解 hibot 的 D1 schema / 状态机。
- cost/金额字段。
- aacr `gate`/`smoke`/`R20`（Phase 0 排除，转列延期债）。
- 拆大组 / 拆大 def（D22 已结案）。
- **md 骨架 i18n**（T13 既有裁决：骨架英文、内容层本地化；本计划只做内容层的语言开关）。
- **Linux / Windows 产物与 `install.ps1`**（M20）；**代码签名 / 公证**（与 hiapi 同策略，本期不做）。
- 报告语言扩到其它语种（本期仅 zh_CN / en_US；枚举预留扩展位但不实现）。

---

## 附录 A · 契约必填字段速查

- **report 核心**：`schema`(`hibot.audit.report.<kind>/1`) · `job_id` · `kind` · `status` · `input.head_sha|base_sha`（= manifest `pr.*` **原样回显**，hibot B2） · `executor.name|version` · `findings[]` · `usage{input_tokens,output_tokens,cached_input_tokens,wall_clock_ms}`；`status_reason` **仅在 `partial`/`error` 时必填**（`ok` 可省）`[核实] audit-report.schema.json`
- **finding**：`fingerprint` · `severity` · `confidence`(0..1) · `file` · `line_start|line_end` · `quote` · `title` · `body` · `recommendation`（可选 `refuted`/`source`/`extensions`）
- **triage 追加**：`summary{security_tier, blockers_count, structural_count, local_count, intent_undelivered, scope_creep_count, risk, convention_drift, fit, routing?{focus[],specs[],wall_clock_s}}`——**`routing` 在 `summary` 内部**，不是顶层键
- **verify 追加**：`verifications[]`（每项 `finding_fingerprint`/`status`∈`resolved|unresolved|regressed|`**`inconclusive`**`/`evidence`，可选 `quote`）；**必须逐条覆盖输入指纹集合；该集合为空时允许空数组**（hibot B5）
- **job**：`schema` · `job_id` · `kind` · `repo` · `pr` · `executor` · `limits.wall_clock_s` · `paths{repo,out,config}` · `inputs{key:{path,sha256,schema?,format?}}`
- **state**：`schema`(`hibot.audit.state/1`) · `job_id` · `phase` · `at`（可选 `llm`/`progress`）
- **artifact.sha256**：64 hex，不带 `sha256:` 前缀（与 fingerprint 不同）`[核实]`

## 附录 B · 模块职责（候选落点）

| 模块 | 职责 | 候选落点 |
|---|---|---|
| 外派入口 | kind 分派、参数校验、退出码 | `src/main.rs`、`src/job/mod.rs` |
| manifest / artifact | job.json 解析、输入读取校验（sha256 + schema + 路径约束） | `src/job/` |
| 报告 / 指纹 / status | hibot 信封映射、指纹唯一实现、确定性判定 | `src/job/` |
| 语言 | `language` 解析与贯穿（OutputLang） | `src/config/mod.rs`、`src/prompt/mod.rs` |
| 配置契约（外部面） | `config schema` 生成 + `config check` 校验（形状 + 语义 + kind 角色 + 密钥字段拒绝） | `src/config/`、`src/cli/`、`dist/config.schema.json` |
| 生命周期 | deadline、SIGTERM、成果保存、心跳 | `src/job/` |
| 模板与解析（**两族分离**） | `refuter.md`（验真，产出 Upheld/Refuted）与 `fix-verify.md`（fix 验证，产出 Resolved/Unresolved/Regressed）各自解析入口，per-backend 单点 | `src/prompt/`、`src/triage/`、`src/fixverify/` |
| 配置与角色 | job 形态加载、env-only 凭据、按 kind 取角色；四角色 scanner/reviewer/refuter/verifier（后两者可缺省/回退） | `src/config/mod.rs`、`src/job/` |
| 并发与观测 | 当前在途/等待快照、provider 分项（**无 job 总闸**，D72） | `src/provider/registry.rs` |
| 发布 | release.sh / install.sh / update / 版本同源 | `scripts/`、`dist/`、`src/cli/`、`Makefile` |
| 测试 | 三夹具 + 管线确定性 + 生命周期 + verify 合成夹具 | `tests/`、`tests/fixtures/job/` |

## 附录 C · 本计划核实过的证据位置

- 契约：`~/hibot/.docs/audit/contract.md` §1.1.2 · §1.1.3 · §3.2 · §3.6 · §7
- schema：`audit-report.schema.json`（`$defs.usage/verification/triageSummary`）· `audit-finding.schema.json` · `audit-job{,.triage,.review,.verify}.schema.json` · `audit-state.schema.json` · `audit-input.{findings,dismissed,pr-context}.schema.json` · `audit-capabilities.schema.json`
- 样例：`~/hibot/.docs/examples/audit-pr-468/`（README 自述：结论编造、diff 节选 → 只作结构参照）
- hibot 文档：`~/hibot/.docs/roadmap/redteam-track.md` · `~/hibot/.docs/runtime/config.md` §1–§4
- 发布参照：`~/hiapi/cli/{install.sh,scripts/release.sh,docs/release-runbook.md,src/update/{mod,replace}.rs}`（R2 基址/`latest`/SHA256SUMS/staged 安装/锁+原子替换/`--version` 自检/未签名限制）
- 本仓：`src/main.rs:14-150,516-566,584-776` · `src/cli/mod.rs:34-70,259-286,301-330,347,537,604-723,1008,1064-1146,1512-1570,1572-1611,2034-2072,2068-2205,2273` · `src/config/mod.rs:67-84`（`Role` 枚举，含 `Verifier`）/ `298-302`（`VerifiersCfg`）/ `384`（`review.verifiers`）/ `457-500` / `596-618`（`resolve_key` + secrets）/ `652-700`（validate）/ `852-864`（配置根）· `src/prompt/mod.rs:19-44,51,69,216,329,359,403,692-761`（含死代码 `build_refute_prompt`）· `src/state/mod.rs:41-52`（`Phase::Verify` 与 legacy `Refute` 共存）· `src/domain/finding.rs:120-160`（`verify` 字段/`VerifyVerdict`）· `src/consolidate/verify.rs:167,193-243` · `src/provider/registry.rs:16-97,270-280,406-410` · `src/orchestrate/mod.rs:18-56` · `src/orchestrate/deep.rs:723-743` · `src/domain/report.rs:12-20,59-70,322-380` · `src/evidence/git.rs:42-81,178-186` · `src/positioning/mod.rs:116-160` · `src/project.rs:5-40,96-107` · `src/consolidate/carry.rs:36-80` · `src/cli/verify_once.rs:1-40` · `Makefile` · `plugins/redteam/bin/redteam` · `plugins/redteam/.{claude,codex}-plugin/plugin.json` · `Cargo.toml:16` · `Cargo.lock`
- 账目：`.doc/roadmap/summary.md`（M19/M20/M21）· `.doc/roadmap/followup.md`（M19 范围、M20 linux/win + T11）· `.doc/TODO/followup.md` T13（md 骨架 i18n 暂缓）· `.next/decisions/…` D22（拆格否决，观测字段保留）
- 参考技能：`~/.pi/agent/skills/pr-triage/SKILL.md`

## 附录 D · `extensions.redteam.*` 命名表（已定）

> 全部 **snake_case**（与契约字段风格一致）；集中在 `src/job/extensions.rs` 一处定义，禁止散写字符串。

**报告级（`report.extensions.redteam`）**

| key | 类型 | 何时出现 | 说明 |
|---|---|---|---|
| `language` | `{value, source}` | 总是 | source ∈ `config`/`locale`/`default`（P15） |
| `status_code` | string | `partial`/`error` | 机器码，见附录 E；`status_reason` 是人读句 |
| `concurrency` | `{mode, provider_caps{}}` | 总是 | mode 恒为 `per-provider`（P12/D72） |
| `repo_degradation` | ~~已废弃~~ | — | hibot C1：repo 完整性有保证；缺必需对象直接 `error` |
| `dropped_unverified_quotes` | int | >0 时 | 2.5 丢弃计数 |
| `budget` | `{wall_clock_s, truncated_items}` | 总是 | `truncated_items` 含队列项与 cell |
| `template_hashes` | `{scan, review, refute, triage, fix_verify}` | 按 kind | 只列本次用到的 |
| `axes` | `[{axis, ok, rows, error?}]` | triage | 三轴各自用量/失败 |
| `blockers` | `[{file?, line?, quote?, intent?}]` | triage | 传给 review 的“已确认项”（可无代码锚） |

**finding 级（`finding.extensions.redteam`）**

| key | 类型 | 说明 |
|---|---|---|
| `ledger` | `{dismissed, actor?, reason?}` | 被 CSV 驳回；**不写 `refuted`** |
| `refute` | `{reason}` | refute 判决正文；`refuted` 仍是顶层契约字段 |
| `origin` | `"new" \| "carried"` | 增量轮携带标记 |
| `cluster_id` | string | 相关簇 |
| `outside_window` | bool | 改动窗口外的观测标记 |

**verification 级（`verifications[].extensions.redteam`）**

| key | 类型 | 说明 |
|---|---|---|
| `verified` | bool | 已废弃（hibot B6 改用第四态 `inconclusive`）；不写 |
| `weak_evidence` | bool | 已废弃（并入 `inconclusive`）；不写 |
| `model` | string | 实际执行核验的模型 |

## 附录 E · `status_reason` 文案与 `status_code`（已定）

> `status_reason` 放**人读句子**（随 `language`），机器字段放 `report.extensions.redteam.status_code`。下表中的 `<x>` 为运行时填充。

**error**

| status_code | zh_CN | en_US |
|---|---|---|
| `job_manifest_unreadable` | 无法读取 job.json | cannot read job.json |
| `job_manifest_unsupported` | job.json 的 schema 或 kind 不受支持 | unsupported job schema or kind |
| `kind_mismatch` | 命令 kind 与 job.json 不一致 | CLI kind does not match job.json |
| `input_missing:<key>` | 缺少必需输入 `<key>` | missing required input `<key>` |
| `input_checksum_mismatch:<key>` | 输入 `<key>` 的 sha256 不匹配 | sha256 mismatch for input `<key>` |
| `input_schema_mismatch:<key>` | 输入 `<key>` 的 schema 不是预期值 | input `<key>` has an unexpected schema |
| `input_path_invalid:<key>` | 输入 `<key>` 的路径越界或不存在 | input `<key>` path escapes the job dir or is absent |
| `config_unreadable` | 无法读取 executor config | cannot read executor config |
| `config_invalid:<pointer>` | executor config 在 `<pointer>` 处不合法 | invalid executor config at `<pointer>` |
| `config_contains_secret:<pointer>` | executor config 含敏感字段 `<pointer>`；凭据只走环境变量 | executor config contains secret field `<pointer>`; supply credentials via env vars |
| `head_sha_mismatch` | 检出 HEAD 与 manifest 不一致 | checkout HEAD differs from manifest |
| `invalid_fingerprint_input` | findings.json 中有非法指纹 | findings.json contains an invalid fingerprint |
| `missing_role:<role>` | 缺少 `<role>` 角色且无可用回退 | missing role `<role>` with no fallback |
| `internal_panic` | 内部错误：`<msg>` | internal error: `<msg>` |
| `kind_not_implemented:<kind>` | kind `<kind>` 在本执行单元中尚未实现（**临时码**，Phase 3/4 实现后删除） | kind `<kind>` is not implemented yet (**temporary**; removed in Phase 3/4) |

**partial**

| status_code | zh_CN | en_US |
|---|---|---|
| `scan_incomplete:<f>/<t>` | 扫描不完整：<f>/<t> 个 cell 失败 | scan incomplete: <f>/<t> cells failed |
| `review_incomplete:<f>/<t>` | 深挖不完整：<f>/<t> 个 cell 失败 | review incomplete: <f>/<t> cells failed |
| `no_reviewer_available` | 无可用 reviewer 条目 | no reviewer entry available |
| `budget_truncated:<n>` | 预算不足，<n> 个条目未挖 | budget exhausted; <n> items skipped |
| `triage_axis_failed:<axis>` | triage 的 `<axis>` 轴未完成 | triage axis `<axis>` incomplete |
| `routing_specs_unreadable:<n>` | 指定的 <n> 份规范文档不可读 | <n> routing spec files unreadable |
| `verifications_inconclusive:<n>` | <n> 条 finding 未能核验（inconclusive） | <n> findings inconclusive |
| `wall_clock_exhausted` | 达到墙钟预算，已收敛落盘 | wall-clock budget exhausted; results flushed |
| `quotes_dropped:<n>` | <n> 条 finding 的 quote 未逐字命中，已丢弃 | <n> findings dropped: quote not found verbatim |

**拼接规则**：多条 partial 原因用 `; ` 连接（全部输出，不只第一条），总数不超过 512 字符；`status_code` 只保留**主导**那一条（按上表从上到下优先级）。

## 附录 F · `state.json.phase` 取值表（已定）

> 设计前提：hibot 把执行单元当**黑盒**，phase 只用于评论与排障，不参与判定；**终态唯一真相是 `report.json.status`**。

| 值 | 何时 | 备注 |
|---|---|---|
| `preparing` | 读 manifest、解析 executor config、校验输入、建 registry 之前 | 进程起手的第一帧心跳就是这个值 |
| `running` | 派发与消费中（review 的 scan/deep/refute、triage 三轴、verify 的逐条核验全在此段） | 不暴露内部阶段；`progress` 提供粒度 |
| `finishing` | 合并 / 去重 / quote 重锚 / 写 `report.json` | 通常数秒；让 hibot 区分“在跑”与“在收尾” |

**规则**：① 三值单调前进，不跳段、不回退；② **不写 `done`/`fail`**——终态由报告表达，心跳随进程退出停止；③ `phase` 在 `audit-state` schema 里是必填，所以首帧必须有值（= `preparing`）；④ `progress.unit` 固定 `cells`（review）或 `items`（triage/verify），不计入 i18n。
