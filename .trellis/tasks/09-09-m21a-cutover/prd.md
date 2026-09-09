# M21a 批 F：硬切与并行（redteam-next 改名 / scanner 扇出 / 疑点并集 / 删旧编排 / 观测落盘）

> 上位：`.next/plans/2026-09-07-redteam-m21a-scan-foundation-plan.md` 批 F（F-1…F-6）+ Global Constraints + 出口验收 8 / 9 / 10；design v8 §2 / §4 / §5 / §8.4 / §10.3 / §12.3；决策 D6（采纳 1 / 8）、D7 A3 / A5、D10（分支硬切、`redteam-next`）；roadmap `M21a-scan-foundation.md` 交付项 6 / 7 / 8 / 9 与验收。
> 技术设计 `design.md`，执行清单 `implement.md`；本文 §Key Decisions 的 K 编号被两者引用。
> 分支 `feat/m21a-cutover`（自 `feat/m21-review-engine` fff7405 切出，工作树 `~/hobby/.worktrees/redteam/m21a-cutover`；A-E 全部合回 + 合并后核验修补 8622dbc / 2170de4 已在基点），合回目标 `feat/m21-review-engine`（`--no-ff`，用户操作）。
> 收口：不跑 ship 评审（用户 2026-09-08 裁定，memory `no-ship-review-until-core-stable`），code-review 子 agent 审核 → 修 → 合回。

## Goal

把 `review` 从「finding 扇出 → anchor → dedup → verify → verdict」硬切为「scanner 疑点扇出（组 × 条目 × samples）→ 疑点并集 + agreement → scan-only 报告」；
删除旧 fan-out / 候补链 / 单发 finding 路径；scanner cell 与 RunMeta 观测落盘；分支产物改名 `redteam-next` 且 `make build` 不再覆盖插件里的 dev `redteam`。
里程碑结束时 `redteam-next review --base X --format json` 对本仓产出疑点索引 + 覆盖自报（不产 finding），`eval/scan_score.py` 能直接读它。

## Background（仓库实测 2026-09-09，HEAD fff7405；三个只读研究子 agent 的代码地图，行号以此 HEAD 为准）

- **scanner 请求装不进现有通路**：`ReviewProvider::run(LensRequest { route_key, prompt: String })`，`run_single_shot` 硬编 `NO_TOOLS_SYSTEM` + `User(prompt)` + `parse_lens_output` / `REPAIR_PROMPT`（`src/provider/single_shot.rs:28-78`）；批 D 的 `ScanPrompt { system, blocks }`、`parse_scan_output_with_stats`、`REPAIR_PROMPT_SCAN`、`anchor_suspicions` 全部就位但零调用。
- **`thinking_level` 零消费**：`EffectiveModel.thinking_level` 已按角色解析（`src/config/mod.rs:555`），`backend_from_config`（`src/provider/backends.rs:694-713`）不读；两个请求体构造器无任何 thinking / reasoning 键。
- **provider 帽零消费**：`EffectiveModel.max_concurrency` 无读者；唯一限流是 `run_code_fanout` 里一把全局 `Semaphore(review.max_concurrency)`；`ProviderRegistry` 是裸 `BTreeMap<route_key, Box<dyn ReviewProvider>>`，无 provider 维度。route_key = `EffectiveModel.name` = `provider/id`（plan 写的 `display_name()` 不存在）。
- **前台 `--format json` 的 `run.cells` 恒空**：`assemble_report` 写死 `cells: vec![]`（`src/cli/tail.rs:197`），只有 job 路径经 `attach_cells` 回填；`eval/scan_score.py` 读顶层 `cells[].output.suspicions`（`role == "scanner"`），今天无产品面产出。
- **退出码单点**：`cli::tail::exit_code`（`Reject→10 / Approve+failed_models→2 / Approve→0 / Incomplete→3`），对 `Verdict` 穷举。
- **resume 按 manifest 重放旧 `LensRequest`**（`src/cli/mod.rs:721-758`）；`CellInput { cell, model, prompt: String }` 装不下 `ScanPrompt`。
- **`quote_overlap` 在 `src/consolidate/dedup.rs:23`**（行拆 + 空白折叠 + 交集 ÷ 较小集），`LINE_GAP = 3`；旧 dedup 的 `agreement = models.len()`（模型数，非家族数）。
- **v4 字段已在、恒 default**：`CellRecord { …, template_hash, cached_input_tokens }`、`RunMeta { coverage 五集合, template_hashes, cache_hit_by_cell, provider_inflight_peak, refute_rate }`（`skip_serializing_if`）；`Phase` 无 `Scan`；`JobState.roles` 无人写；`GroupingStats` / `PackStats` 只 `Serialize`。
- **改名面**：`Cargo.toml` `[[bin]] name`；Makefile `BIN` / `cp` / `chmod`；`tests/review_e2e.rs` 六处 `env!("CARGO_BIN_EXE_redteam")`（Cargo 对连字符原样保留 → `CARGO_BIN_EXE_redteam-next`，不改则 e2e 整体编译失败）；`plugins/redteam/bin/darwin/aarch64/redteam`（dev 二进制 5.3 MB）与 launcher `bin/redteam` 均入库，`.gitignore` 无 bin 规则；commands 只引用 `bin/redteam`，`make verify` 不受影响；clap `name = "redteam"` 硬编码。
- **各家 thinking 参数（2026-09-09 官方文档核实）**：DeepSeek openai 形态 `thinking{type: enabled|disabled}` + `reasoning_effort low|high|max`；Z.ai GLM-5.3 openai 形态 `thinking{type: enabled}`（不可 disable）+ `reasoning_effort low|high|max`，anthropic 形态文档无字段说明；Moonshot Kimi 只认 `thinking{type}`、与 `reasoning_effort` 同发即 400；Anthropic 原生 `thinking{type: enabled, budget_tokens}`（`< max_tokens`）；OpenAI / Gemini openai 形态 `reasoning_effort low|medium|high`；Qwen DashScope openai 形态 `enable_thinking: bool`。
- **本机配置**：scanners = `deepseek/deepseek-v4-flash`（openai）+ `qwen/qwen3.8-flash`（openai）；reviewer `agc/grok-4.6`、verifier `agc/glm-5.3`（anthropic，本批不消费）。`review` 节全缺省。
- 基线：`cargo test` lib 552 / 16 / 7 / e2e 17 绿；clippy 7 行（3 条既有）；`make verify` 绿。

## Requirements

### R-F1 产物改名（plan F-1）
- `Cargo.toml` `[[bin]] name = "redteam-next"`（lib 名不动）；`tests/review_e2e.rs` 六处改 `CARGO_BIN_EXE_redteam-next`；Makefile `BIN := plugins/redteam/bin/darwin/aarch64/redteam-next`、`cp` 源改、删 `chmod` 行、加 `build-dev-baseline` 注释（dev 二进制来源 = `dev` 分支 feebc92 重建，入库不重建）；`.gitignore` 加 `plugins/redteam/bin/**/redteam-next`（K12）；`tests/packaging.rs` 加断言：commands 与 codex 镜像不出现 `redteam-next`；clap `name` / `about` / `after_help` 改 `redteam-next（M21 重构分支产物）`；`CLAUDE.md` 常用节一句话更新 `make build` 语义。

### R-F2 scanner 扇出（plan F-2）
- `provider::scan_shot::run_scan_shot(backend, &ScanPrompt, per_turn, retry_budget) -> ScanCall`：`[System(head), UserBlocks(blocks)]` → `timed_turn` → `parse_scan_output_with_stats` → 失败 push `User(REPAIR_PROMPT_SCAN)` + `timed_turn_final` → 再解析 / `diagnostic_parse_error`；usage 跨轮累加；`ScanCall { output, stats, input/output/cached tokens, iterations, finish_reason, reasoning_tokens }`（K3）。
- `ProviderRegistry` 加 scan 表 + per-provider 在途门（`Semaphore` + 当前 / 峰值计数）+ warnings；`insert_scan(route_key, provider, cap, backend, per_turn, retry_budget)`、`async scan(route_key, &ScanPrompt)`、`inflight_peaks()`；`cap = provider.maxConcurrency` 缺省继承 `review.max_concurrency`（D7 A5）；旧 `insert` / `run` 表保留给 M21b（K2）。
- `backends.rs`：`thinking_params(kind, family, level, max_tokens) -> Option<Vec<(String, Value)>>` 映射表（K4）；两 body 构造器加 `thinking` 参数（`None` → 逐字节不变）；`AnthropicBackend` / `OpenAiBackend` 加 `with_thinking`；`backend_from_config` 注入 `em.thinking_level`；无映射 → 不发 + warning 文案（由 `build_registry` 收进 registry warnings → `RunMeta.warnings`）。
- `AssistantTurn.reasoning_tokens: Option<u64>`（openai 流式 / 非流式 `completion_tokens_details.reasoning_tokens`；anthropic None）；钉档 ≠ Off ∧ `reasoning_tokens == Some(0)` ∧ 输出 < 200 token → `CellRecord.warn` 观测，不重跑（K5）。
- `orchestrate::run_scan_fanout(registry, &ScanFanoutInput, sink) -> ScanResults`：cell = 组 × scanner 条目 × `review.scanners.samples`；`cell_id = scan[g<n>]@<provider/id>#<k>`（n / k 均从 1 起）；prompt 每组构造一次（`build_scan_prompt`，precedents / repo_brief 传空）；manifest 经 `CellSink::manifest`（`CellInput` 加 `scan: Option<ScanPrompt>`，K7）；并发 = 角色 `workers` 信号量（fanout 内）× provider 帽（registry 内）；单 cell：`registry.scan` → `anchor_suspicions` → `CellRecord::ok(cell, route_key, CellOutput::Scan(..)).with_family(family)` + usage / duration / iterations / `template_hash`；parse 丢弃计数、anchor 丢弃计数、零推理观测拼进 `warn`（cell 仍 ok）；失败 → `CellRecord::failed(.., Role::Scanner, ..)` 带 burned usage，进 `ScanResults.failed`；needs 只随 cell 落库、`needs_declared` 计数，不补跑（K6）；`sink.on_cell` 每格即时回调、`on_roles([Scanner])` 新钩子。
- `build_registry`：scanner 条目一律 scan 单发（`--tools` 对 scanner 无效，K3）；reviewers / verifiers 仍不注册（M21b）。

### R-F3 疑点并集与 agreement、scan-only 报告（plan F-3）
- `consolidate::scan::merge_suspicions(cells: &[ScanCellResult]) -> Vec<MergedSuspicion>`：同组 ∧ 同文件 ∧（去空白 quote 相等 ∨ `quote_overlap ≥ QUOTE_SIM`）∧ 行重叠 ±`LINE_GAP` → 合并；`agreement = families.len()`（同家族多 samples 计一次）；代表疑点 = confidence 最高者（并列取先到）；保留全部 hint（不归一，M21c R4）与 `cell_ids`；跨组不合并；输出按组、文件、行排序（确定性）（K8）。
- `domain::report`：`Verdict::ScanOnly`（kebab `scan-only`）；`ScanReport { groups: Vec<ScanGroup { label, files, cells, suspicions, pack: PackStats }>, suspicions: Vec<MergedSuspicion>, scanned_coverage: Option<f32>, stats: ScanStats { total, cells_ok, cells_failed, agreement_ge2, by_category, needs_declared } }`；`ReviewReport.scan: Option<ScanReport>`、顶层 `cells: Vec<CellRecord>`（scan_score.py 形态，零改脚本；`run.cells` 摘要保留）；`schema_version` 仍 `"4"`（K9）。
- `exit_code`：`ScanOnly → 0`；任一 scanner cell 失败 → verdict `Incomplete` → 3（design §8.4）；旧「failed_models → 2」在 scan 路径不可达。
- md 渲染：verdict 行、每组表（label / 文件数 / cells / 疑点数 / 切片形态）、疑点列表（file:a-b · category · confidence · agreement · families · hint；quote 首行）、覆盖自报、needs 计数、失败 cell。

### R-F4 删旧编排（plan F-4）
- 删：`orchestrate` 旧扇出（`run_code_fanout` / `run_one_bundle` / `maybe_second_pass` / `top_priority_model` / `CellRun` / `RawResults` / `SecondPrompt` / `OrchestrateOpts` / `PlannedModel.priority`）与 13 条旧测试；cli `legacy_opts_from_roles` + 两测、`run_code_review` finding 尾段、`TailCtx` / `assemble_report` / `mark_dismissed` / `CarryCtx` 及测试；`run_resume` 改 scan 重放；`partial_report` 收 `CellOutput::Scan`；`run_incremental_review` 改为显式 bail（preflight 与其测试保留）；`second_pass_prompt`（依赖旧模板）删、模块只留 `need_read_allowed` / `need_section` / `draft_block`；prompt `build_bundle_prompt` / `CODE_BUNDLE_TPL` / `templates/adversarial-code-bundle.md` / `TemplateHashes.legacy_bundle` / `render_dossier_section` + 旧前言 / `DOSSIER_STRUCTURAL_TAGS`（若无他用）/ `render_incremental_section` 及测试群；`bundle::two_layer_manifest` 及测试；`build_packs` 改产裸 `Packed.text`（`build_scan_prompt` 已接受裸文）；`run_pack_only` 改 `build_scan_prompt`（K15）。
- 留：`consolidate/{dedup,verify,carry}`（pub 无警告；私有项 `#[allow(dead_code)]` 注明 M21b-c）、`single_shot` / `tool_loop` / `FakeProvider` / `ReviewProvider`（M21b reviewer / verifier）、`enrich` / `finding_id`、`adversarial-refute.md`。
- e2e：四例 FakeProvider 用例改 scan 路径（fake 回 `{"suspicions":[…]}`）；`incremental_round_smoke` 与 `dismissed_ledger_previously_dismissed_marks_and_feeds_second_stage` 标 `#[ignore]` 并记 followup（M21c）；`code_replicate_lone_blocking_refuted_stays_approve` 删（verifier M21b）；`code_replicate_unreachable_model_exits_two` 期望 2 → 3。
- `--help`：Review doc 改 scan-only 措辞与退出码；`--block-threshold` / `--conf-floor` / `--dismissed` / `--no-dismissed` / `--incremental-from` / `--tools` 注明生效里程碑（K13）。

### R-F5 观测与落盘（plan F-5）
- 每 cell `CellRecord`（role scanner、`CellOutput::Scan`、usage、`cached_input_tokens`、`template_hash = "<shared_head>+<scanner_tail>"`）经 sink 落 `cells` 表并进程内进报告顶层 `cells`。
- `RunMeta`：`grouping: Option<GroupingStats>`（新，补 `Deserialize`）、`coverage` 五集合（selected = 全部 scan cell；completed / failed 按 `ok`；reused = resume 沿用；waived 空）、`template_hashes = {shared_head, scanner_tail}`、`cache_hit_by_cell = cached ÷ input`、`provider_inflight_peak`（registry 门）、`cells: Vec<CellSummary>` 加 `cell` / `role` / `cached_input_tokens`、`needs`（declared / 0）、`models` / `model_ids` = scanners；`ScanReport.scanned_coverage` = `scanned_areas` ∪ ∩ 改动行 ÷ 改动行（改动行取 `diffsym::parse_hunks`）；`Phase::Scan`（`phase_history`）；`JobState.roles = [Scanner]`（K10）。
- 一 cell 失败 → `coverage.failed[scanner]` 非空 → `Incomplete` / 3；RunMeta 快照测试。

### R-F6 收口（plan F-6）
- `make build` 产出 `plugins/redteam/bin/darwin/aarch64/redteam-next`，`git status` 不显示插件 `redteam` 变化、`redteam-next` 被 ignore；本仓 dogfood：`cargo run -- review --base feat/m21-review-engine --format json`（本分支小 diff，K14）走通 scan-only 报告并用 `eval/scan_score.py` 自识别形态（无 oracle 只验读取）；三道门；code-review 子 agent → 修；followup 登记；roadmap 交付项 6 / 7 / 8 / 9 打勾（出口验收另起 session）。

## Key Decisions

| # | 决策 | 依据 / 代价 |
|---|---|---|
| K1 | route_key = `EffectiveModel.name`（`provider/id`）；cell_id `scan[g<n>]@<name>#<k>`，n / k 从 1 起 | plan 的 `display_name()` 不存在；同模型跨角色键冲突留 M21b（registry 注释已预告） |
| K2 | scanner 通路 = registry scan 表 + per-provider 在途门；旧 `ReviewProvider` 表与 `single_shot` / `tool_loop` 保留给 M21b | 两层限流（角色 workers × provider 帽）；同 provider 的 reviewer 届时共享同一把门（design §12.3）；不改 `ChatBackend` trait 与 `LensRequest` |
| K3 | scanner 恒单发两轮（首发 `turn` 不用 `turn_final`，修复轮 `turn_final`）；`--tools` 对 scanner 无效 | design §11 工具分层授予归 reviewer（M21b）；E3 疑点模式单发已验证 |
| K4 | thinking 映射表按 (kind, family, level)，无映射不发 + warning；档位是 backend 实例属性 | 各家参数名互斥（Kimi 同发即 400、GLM-5.3 不可关）；warning 进 RunMeta 可观测（铁律 1） |
| K5 | `AssistantTurn.reasoning_tokens` 新字段（16 处构造点机械补 `None`），零推理只观测不重跑 | plan 原文；anthropic 无分列即 None |
| K6 | needs 只落库计数，不补跑；`second_pass_prompt` 随旧模板删，读守卫与 `need_section` 留 | M21b 深挖队列消费 needs（design §4） |
| K7 | `CellInput` 加 `scan: Option<ScanPrompt>`（serde），`prompt` 存 flatten 供人读；resume 差集用 `scan` 重放 | manifest 是 resume 的唯一输入；扁平串会丢 system 与 cache 断点 |
| K8 | 合并谓词复用 `dedup::quote_overlap` + `LINE_GAP`，`agreement = 家族数`，不复用 `dedup::merge_into` | 后者按模型数计（design §5 明写家族数） |
| K9 | `Verdict::ScanOnly` → 0；scanner cell 失败 → `Incomplete` → 3；报告顶层 `cells: Vec<CellRecord>` 供评分器 | design §8.4「scanner 失败硬 Incomplete」；followup :80「结果 JSON 加 scanner 输出」；schema 不翻号（followup :70） |
| K10 | `template_hash = "<head>+<tail>"`（两段 16 hex，`+` 连）；`RunMeta.template_hashes` 分存两键 | 单值字段（批 B）；A/B 归因两段都可读 |
| K11 | 增量轮 `run_incremental_review` 改显式报错（preflight 保留），dismissed 台账 e2e `#[ignore]` | plan F-4 明文（M21c 台账回流恢复）；删 `run_code_fanout` 后该函数无法编译 |
| K12 | `.gitignore` 加 `plugins/redteam/bin/**/redteam-next`；clap `name` 同步 `redteam-next` | 公开仓防 5 MB 分支二进制误入（plan 未写，本批加）；`--help` 是唯一在场文档，Usage 与文件名一致 |
| K13 | 无消费者的旗（`--block-threshold` 等）保留、文案注明生效里程碑，不删不隐藏 | 避免 M21b 再加回；用户可见语义诚实 |
| K14 | dogfood 用本分支相对 `feat/m21-review-engine` 的 diff，不跑 `--base dev` | `--base dev` 含 4.5 MB 文档 diff 组必失败（cell-3，`--pack-only` 实证） |
| K15 | `--pack-only` 改 `build_scan_prompt`：`bytes` = system + flatten，`cell-n.md` = system + `\n\n` + flatten | 关闭 followup :52 / :110「`--pack-only` 口径切到 scan prompt（含 system 段）」 |

## Acceptance Criteria

- AC1（F-1）：`cargo build` 产物 `target/debug/redteam-next`；`cargo test --test packaging` 含新断言绿；`make verify` 绿；`make build` 后 `git status` 干净（`redteam-next` 被 ignore、dev `redteam` 未变）；`redteam-next --help` 头标含「M21 重构分支产物」。
- AC2（F-2）：body 单测：每个 (kind, family) 映射一断言 + `None` 逐字节不变；`registry` 在途门：provider 帽 2 / workers 4 → 峰值 ≤ 2，workers 1 / 帽 8 → 峰值 ≤ 1；两条目 × 两组 = 四 cell、cell_id 与 route_key 正确；失败 cell 进 `failed` 且 usage 入账；parse 失败走 `REPAIR_PROMPT_SCAN` 且首发不用 `turn_final`；`reasoning_tokens` 解析；零推理 warn。
- AC3（F-3）：两家族同 quote → agreement 2；同家族两 samples → 1；跨组不合并；`quote_overlap` 半重叠合并；md 快照（含覆盖自报与失败 cell）；`exit_code(ScanOnly) == 0`。
- AC4（F-4）：全量绿；clippy ≤ 基线；`--help` 无 `--models` / `engine` / `--risk`；`adversarial-code-bundle.md` 不存在；e2e 四例改 scan 通过、两例 `#[ignore]`。
- AC5（F-5）：前台 `--format json` 顶层 `cells[].output.role == "scanner"` 且含 `suspicions`；RunMeta 快照测试含 coverage / template_hashes / cache_hit_by_cell / provider_inflight_peak / grouping；一 cell 失败 → `Incomplete` / 3；覆盖率单测（全覆盖 1.0 / 半覆盖 0.5 / 无改动行 None）；`phase_history` 含 `scan`。
- AC6（F-6）：本仓 dogfood scan-only 报告落盘；`python3 eval/scan_score.py score out.json --oracle <合成或 selftest oracle>` 能读取 cells 形态；三道门绿；code-review 修复记录进 `implement.md` §评审修；followup 登记；roadmap 交付项 6 / 7 / 8 / 9 打勾。

## Out of Scope

出口验收 1-7（PR181 / 双 scanner / E2 A/B / cache 命中 / `--pack-only` 误差 / 模板回归 / dev 基线归档，需 `.next/private`，另起 session）· 风险路由 / 队列 / reviewer / verifier（M21b）· 疑点台账 / 增量轮恢复 / 判例前移 / 切名（M21c）· reviewers / verifiers 注册进 registry（M21b）· `GroupingStats` 分种计数（followup :49，M21b 前）· `hint` 归一化（R4）· google 后端。

## Risks / Deferred

- 删除面最大（orchestrate / cli 尾段 / prompt 旧模板 / e2e）：按 task 顺序 F-2（新旧并存可编译）→ F-3 → F-4（一次切换）→ F-5；每步三道门。
- thinking 映射未在真网关实测（Qwen `enable_thinking`、GLM anthropic 形态）：本批只对 openai 形态的 DeepSeek / Qwen 在 dogfood 里实测；GLM anthropic 形态记 warning 不发（出口 2 前补）。
- resume 改 scan 重放但无生产验证（只有 e2e 空 manifest 例）：M21b 队列快照落地后重做，followup 记。
- `Stats`（finding 计数）在 scan-only 下恒零；插件 `result.md` 文案「呈现 findings」归 M21c。
