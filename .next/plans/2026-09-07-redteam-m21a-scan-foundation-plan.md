# M21a 疑点扫描地基 · 实施计划（六批 + 出口）

> 状态: 锁定（2026-09-08 派生自 design v8〔D11〕）· 上位 `../roadmap/M21a-scan-foundation.md`（范围与验收）· design
> `../design/2026-09-04-redteam-review-engine-architecture-design.md` §3.1-3.2 / §4 / §5 / §10.2-10.3 / §10.6-10.7 / §12 / §15 ·
> 决策 D3 / D6 / D7 / D8 / D9 / D10 / D11 · P0 数据 `../private/research/2026-09-07-redteam-p0-validation-gate-research.md`
> **执行形态**：每批一个 Trellis 任务（`m21a-config` / `m21a-schema` / `m21a-grouping` / `m21a-prompts` / `m21a-slice` / `m21a-cutover`）+
> 一个子分支 `feat/m21a-<批>`（自 `feat/m21-review-engine` 切出，独立工作树）；批内小步提交；批收口用 dev `redteam` 跑 ship 评审，
> 修完 `--no-ff` 合回 `feat/m21-review-engine`。

**Goal:** 把快扫从「finding 单发」改造成「分组并行 + 疑点索引」：用户级唯一配置、schema v4、引用图分组与跨组索引、切片装包、
疑点契约 + 共享头 / scanner 尾 + 段序 + cache 断点、多 scanner 并行、分支硬切、`--pack-only` 与观测。里程碑结束时 `redteam-next`
对 PR181 产出疑点索引与覆盖自报（不产 finding），E2 slice vs full A/B 定 `pack.body` 默认。

## 批间依赖与波次

| 波 | 批（子分支） | 前提 | 碰撞点 |
|---|---|---|---|
| 1 | **A 配置节**（`feat/m21a-config`）· **B schema v4**（`feat/m21a-schema`）· **C 分组与索引 + `--pack-only`**（`feat/m21a-grouping`） | 无，三批并行 | A 与 C 都改 `src/cli/mod.rs`（A 删 `resolve_models` / `--models`，C 加 `--pack-only`）：区域不同，后合者 rebase 一次 |
| 2 | **D 契约与提示词**（`feat/m21a-prompts`）· **E 切片装包**（`feat/m21a-slice`） | D ← B（疑点类型在 domain）+ C（`{{OTHER_CHANGED_FILES}}` 槽已升级）；E ← C（`Bundle` 带边与枢纽序） | D 与 E 无交集；C 与 D 都改 `src/prompt/mod.rs`，故 D 必在 C 合回后 |
| 3 | **F 硬切与并行**（`feat/m21a-cutover`） | A-E 全部合回 | 独占 |
| 4 | 出口验收（本工作树，单 session） | F | — |

```
git worktree add ~/hobby/.worktrees/redteam/m21a-config   -b feat/m21a-config   feat/m21-review-engine
git worktree add ~/hobby/.worktrees/redteam/m21a-schema   -b feat/m21a-schema   feat/m21-review-engine
git worktree add ~/hobby/.worktrees/redteam/m21a-grouping -b feat/m21a-grouping feat/m21-review-engine
```
- `.next/private/`（PR181 原始数据、疑点 oracle、P0 明细）是 gitignored，新工作树没有：第一波不需要；D-5 与出口验收从本工作树
  拷贝或软链 `.next/private`。
- 每个子分支工作树自己 `cargo build`；`make build` 在 F-1 之前不要跑（它会覆盖插件里的 dev `redteam`）。

## Global Constraints（全批隐含）

- 每 task：`cargo test` 全绿 + `cargo clippy --all-targets` 零新增 + `make verify` 绿 + commit（简体中文 `type(scope): 摘要——细节`，
  绝不 push）；改动只在 task 列出的文件与其测试；批内发现的新问题记 `.next/roadmap/followup.md`，不顺手修（Critical 除外）。
- 铁律映射：新编排 pass 三标配（注入互斥 · 证据段预算帽 · 读守卫）；输出契约与 parse 字段级对齐；模板或段序改动必跑前缀稳定回归
  （D-4 / 出口 4）；每个机制带观测仪表（F-5）。
- 无 `engine` 旗、无 `--risk` / `[review.risk]`（D10 / D11）；dev 二进制与插件 `bin/redteam` 全程不动。
- 不做：深挖 / 队列 / 风险评分 / verifier / 台账填充（M21b-c）· google 后端（R17）· LLM 语义分组（`semantic = false`）· 迁移代码（D7 A9 / D10）。
- 公司仓内容不进跟踪文件：测试 fixture 只用合成样本或本仓数据；PR181 相关跑分只落 `.next/private/`。

---

## 批 A · 配置节（`feat/m21a-config`，Trellis `m21a-config`）

先机械拆零语义再接线：A-1 → A-2 → A-3 独立可测，A-4 / A-5 把消费方接到新形状，旧 fan-out 通过 A-4 的适配器继续跑到 F 批为止。

### Task A-1 路径与层序：`~/.redteam/` + `REDTEAM_HOME`，删项目层
- Modify: `src/config/mod.rs`（`config_paths()` → 单路径 `$REDTEAM_HOME/config.toml`，缺省 `~/.redteam/`；`Config::load()` 只读一层；
  删 `from_layers_tagged` 的项目层剥离与 warning 通道，保留 `from_layers` 供测试合并）、`src/config/secrets.rs`（`dirs_config()` →
  `redteam_home()`；`secrets_path()` = `$REDTEAM_HOME/secrets.toml`）、`src/project.rs`（`init` 骨架的 `config.toml` 模板去掉——项目层只剩
  状态与内容）、tests 中 8 处 `XDG_CONFIG_HOME` 串行化 hack → `REDTEAM_HOME`（`ENV_LOCK` 保留）。
- 行为：旧路径 `~/.config/redteam/config.toml` 或 `.redteam/config.toml` 存在 → stderr 一行提示（「已改读 ~/.redteam/，旧文件未读」），不读。
- 测试：路径解析（env 覆盖 / 缺省）；旧路径存在时的提示与不读；`init` 不再写 config.toml；现有 config 测试改走 `REDTEAM_HOME`。
- Commit: `refactor(config): 用户级唯一路径 ~/.redteam/ + REDTEAM_HOME——删项目层与剥离清单，XDG hack 退场`

### Task A-2 新形状解析：providers 键名表 + 三角色列表 + `[review.*]`
- Modify: `src/config/mod.rs`：`ProviderConfig` 加 `max_concurrency: Option<u32>`（缺省继承 `review.max_concurrency`）；新增
  `RoleEntry { name: Option<String>, provider, model_id, kind: Option<ProviderKind>, api_key_env: Option<String>, family: Option<String>,
  context_window: Option<u32>, output_max_tokens: Option<u64>, reasoning: bool, thinking_level: Option<ThinkingLevel>,
  per_turn_timeout_secs: Option<u64> }` 与 `Config { providers, scanners: Vec<RoleEntry>, reviewers, verifiers, review }`；
  `RoleEntry::display_name()` 缺省 `provider/model_id`、`family()` 缺省 model_id 首段（`-` / `_` / `.` 前）；`ReviewConfig` 加子表
  `scanners { workers, samples }` / `reviewers { workers, max_cells: MaxCells(Auto|Fixed(u32)), reenqueue }` / `verifiers { enabled, workers }` /
  `grouping { min_files, bundle_line_threshold, max_files_per_group, semantic }` / `pack { body: Full|Slice, anchor_window, slice_min_bytes }`；
  删 `[models]` / `ModelConfig.priority` / `default_models`；`effective_model(name)` → `effective_entry(role, name)`；`validate()`：条目引用
  不存在的 provider → **聚合**成一条错误列出全部；design §12.1 样例作 doc-test 输入。
- 测试：§12.1 样例整段解析；缺省 name / family；未知 provider 聚合报错文案；`max_cells = "auto"` 与整数两形态；旧键（`[models]`、
  `priority`、`default_models`、`[review.risk]`）出现 → 未知键 warning 而非静默。
- Commit: `feat(config): providers 键名表 + [[scanners]]/[[reviewers]]/[[verifiers]] 内联条目 + [review.*] 子表（design §12.1）`

### Task A-3 secrets 新形状、解析链、守卫、`model setup`
- Modify: `src/config/secrets.rs`（`[providers.X] api_key` + `"<model_id>" = "key" | { api_key = "…" }`；`load_secrets()` 返回
  `BTreeMap<provider, ProviderSecrets { api_key, models: BTreeMap<model_id, SecretEntry> }>`；守卫：provider 表下出现子表且键含 `.` 的裸写
  → 提示「是否漏了引号」；除 `api_key` 外的键在 config 该 provider 下无对应 `model_id` → warning）、`src/config/mod.rs`
  （`resolve_key(entry)` 解析链：条目 `api_key_env` → secrets[X][model_id] → provider `api_key_env` → secrets[X].api_key）、`src/main.rs`
  `ModelCmd::Setup`（新参数 `--role scanner|reviewer|verifier`；写 `[providers.X]` + 追加角色条目；`--key-store secrets` 按新形状写；删
  `--priority`；`context_tokens` → `context_window`）、`src/config/mod.rs::write_model` → `write_entry`。
- 测试：解析链四级各一；引号守卫；未知 model_id warning；`setup` 写出的 TOML 可被 A-2 解析回同值（round-trip）；secrets 永不出现在 config.toml。
- Commit: `feat(config): secrets 新形状（provider api_key + "<model_id>" 覆盖）+ 四级解析链 + 引号守卫；model setup --role 写新形状`

### Task A-4 CLI 面：删 `--models` / `--models-all` / 候补链，加 `--test-command`
- Modify: `src/cli/mod.rs`（删 `resolve_models` 与 `ReviewArgs.models / models_all`；新增 `active_roles(cfg) -> RoleSets`；**临时适配器**
  `roles_to_legacy_opts(&RoleSets) -> OrchestrateOpts { models: scanners 映射为 PlannedModel{priority:0}, replicate: true, no_fallback: true }`
  让旧 fan-out 以「全部 scanner 各跑一遍」继续工作到 F 批；`ReviewArgs.test_command: Option<String>`；推断 `infer_test_command(root)`：
  `Cargo.toml` → `cargo test`、`package.json` scripts.test → `npm test`、`Makefile` 含 `test:` → `make test`、`go.mod` → `go test ./...`、
  否则 None）、`src/domain/report.rs`（`RunMeta.test_command: Option<String>`）、`src/main.rs`（clap：删两 flag，加 `--test-command`）、
  `plugins/redteam/commands/ship.md`（测试命令探测改读 `redteam result <id>` 的 `run.test_command`，不再读项目 config）+ `make gen`。
- 测试：`--test-command` 覆盖推断；四种推断；`--models` 出现 → clap 报未知参数；适配器让 `run_code_fanout` 对两条 scanner 各跑一 cell
  （复用 `code_replicate_two_models_yield_agreement_two` 形态）。
- Commit: `refactor(cli): --models/--models-all/候补链退场，角色集适配旧 fan-out；--test-command 覆盖推断（ship 壳层改读 run meta）`

### Task A-5 `build_registry` 角色化
- Modify: `src/main.rs::build_registry`（按 `RoleSets` 每条目建 backend：`route_key = entry.display_name()`；同 provider 条目共享
  `max_concurrency` 信号量占位——真正的在途锁在 F-2；kind / api_key_env 条目覆盖 provider）、`src/provider/backends.rs::build_backend`
  签名改收 `&RoleEntry`（`output_max_tokens` → `with_max_tokens`，`per_turn_timeout_secs` 缺省 = `output_max_tokens ÷ 20 TPS + 60s`）。
- 测试：`build_registry_no_provider_returns_err` 改为「条目引用缺失 provider」；同 provider 两条目得两 route_key；条目级 kind 覆盖。
- Commit: `refactor(provider): build_registry 按角色条目建 backend，route_key = 条目名（design §12.3）`

### Task A-6 收口
- `--help`：配置路径说明（`~/.redteam/config.toml` / `secrets.toml` / `REDTEAM_HOME`）、`--test-command`；`redteam model setup --help`。
- 手动：把本机 `~/.config/redteam/{config,secrets}.toml` 迁成新形状写到 `~/.redteam/`（dev `redteam` 继续读旧路径，两者并存）。
- ship 评审（dev `redteam`）→ 修 → `--no-ff` 合回。

---

## 批 B · schema v4（`feat/m21a-schema`，Trellis `m21a-schema`）

### Task B-1 旧库导出固定集
- Add: `eval/export_ledger.py`（零依赖；读 `.redteam/proj.db` 的 `dismissed` 全表与 `results` 中 `verify.verdict = upheld` 的 findings，
  写 `eval/fixtures/ledger-dismissed.jsonl` / `ledger-upheld.jsonl`，每行含 id / file / quote / title / severity / status / round）。
- 验收：本仓导出 dismissed 63 条、upheld 48 条（与 P0 §15.1 一致）；fixture 入库（本仓自己的数据）。
- Commit: `chore(eval): 导出旧 proj.db 的 dismissed / upheld 固定集（verifier 离线回归集数据，D10）`

### Task B-2 domain：疑点类型与角色输出枚举
- Modify: `src/domain/finding.rs` → 拆 `src/domain/suspicion.rs`：`Suspicion { file, line_start, line_end, hint, category: Option<Category>,
  confidence: f32, quote }`、`Category { L0, L1, L2, L3, L4, L5 }`、`ScannedArea { file, ranges: Vec<String> }`、`ScanOutput { suspicions, needs,
  scanned_areas }`；`NeedEntry` 加 `kind: NeedKind { Evidence, Api }`（serde default Evidence）；`Role { Scanner, Reviewer, Verifier }`；
  `CellOutput`（`#[serde(tag = "role", rename_all = "lowercase")]`）= `Scan(ScanOutput)` | `Review { findings, needs }` | `Verify { verdicts }`。
- 测试：serde round-trip 三变体；tag 缺失 → 解析错误而非默认；`NeedEntry` 旧 JSON（无 kind）仍可读。
- Commit: `feat(domain): 疑点 schema（suspicion / category / scanned_areas / needs.kind）+ 按角色分叉的 CellOutput 枚举（D11 R14）`

### Task B-3 CellRecord v4 + proj.db v4 一次重置
- Modify: `src/state/mod.rs`（`CellRecord { cell, role, model, family, ok, output: CellOutput, warn, duration_ms, input_tokens, output_tokens,
  cached_input_tokens, attempts, iterations, tool_use, template_hash: Option<String> }`；`CellRecord::ok(...)` 改收 `CellOutput`）、
  `src/db.rs`（`PRAGMA user_version = 4`；`cells` 表加 `role TEXT NOT NULL`；新表 `queue_items(job_id, item_id, state, json)` 与
  `suspicions(id, file, quote_norm, hint, status, first_job, last_job, activations, json)`——本里程碑只建表不写；`open_at` 遇 `user_version < 4`
  且库非空 → 拒开并提示「先 `eval/export_ledger.py` 导出，再删除 .redteam/proj.db 重建」，不写迁移）。
- 测试：空库建 v4 五表 + user_version；旧库（`user_version 0` 且有行）拒开文案；`CellRecord` v4 round-trip；`read_partial` 读回 role。
- Commit: `feat(state): CellRecord 带 role/output 枚举/template_hash；proj.db schema v4 一次重置（cells.role + queue_items + suspicions 表壳）`

### Task B-4 作业元数据：coverage 五集合 + runs meta
- Modify: `src/domain/report.rs`（`RunMeta` 加 `coverage: Coverage { selected, completed, reused, failed, waived }`（per role 计数 + cell 列表）、
  `template_hashes: BTreeMap<String, String>`、`cache_hit_by_cell: Vec<(cell, f32)>`、`provider_inflight_peak: BTreeMap<provider, u32>`、
  `refute_rate: Option<f32>`——本批只加字段与 serde default，填值在 F-5 / M21b）、`ReviewReport.schema_version` → 4、`src/state/mod.rs`
  `JobState` 加 `roles: Vec<String>`。
- 测试：serde default 全部字段；`result_md` 渲染忽略空 coverage。
- Commit: `feat(report): RunMeta 加 coverage 五集合 / 模板哈希 / cache 命中 / 在途峰值 / refute 率字段；report schema_version 4`

### Task B-5 收口：ship 评审 → `--no-ff` 合回。

---

## 批 C · 分组与索引 + `--pack-only`（`feat/m21a-grouping`，Trellis `m21a-grouping`）

### Task C-1 `extract_imports` 三语言补齐，边带类型
- Modify: `src/bundle/mod.rs`（`extract_imports` → `import_edges(path, content, root) -> Vec<ImportEdge { to: PathBuf, kind: Import }>`：
  TS 保留相对解析，**裸模块**按 `tsconfig.json` 的 `baseUrl` / `paths` 解析、解析不到则丢；Rust `use crate::a::b` / `super::` →
  `src/a/b.rs` 或 `src/a/b/mod.rs`（以 `src/lib.rs` / `main.rs` 为根）；Go `import "<module>/pkg/sub"` 按 `go.mod` module 路径映射到目录，
  目录内任一 changed 文件即成边）。
- 测试：三语言各一临时仓（相对 / 裸模块 / crate / super / go 包）；解析不到不成边；路径中和（`..` 逃逸不越 root）。
- Commit: `feat(bundle): import 边补 TS 裸模块 / Rust use crate / Go 包路径（followup 实现债）`

### Task C-2 引用图分组（design §3.2）
- Modify: `src/bundle/mod.rs`（`group()` 重写：边 = import 边（强）+ 引用边（A 的 changed def 名出现在 B 的 hunk 或全文，强，按次数计；changed
  def 取 `diffsym::changed_def_ranges`）+ 同目录边（弱）；门：`files < min_files || churn < bundle_line_threshold` → 单包；连通分量；分量
  `> max_files_per_group` 或超预算 → 先切弱边；仍超 → 标 `over_budget`（宁降不拆，交装包梯子）；组内填充序 = import 边数降序（枢纽保全文）；
  孤立文件按目录 bin-packing；`Bundle { files, edges: Vec<Edge>, hub_order: Vec<usize>, over_budget: bool }`；`GroupingStats { groups, edges,
  weak_cut, cross_edges, isolated }`）、`src/cli/mod.rs`（`merge_bundles` 删除——单格全审默认由分组门取代；`[review.grouping]` 读入）。
- 测试：合成图（两连通分量 / 弱缝切 / 超限不切强边 / 孤立打包 / 门未过单包）；`merge_bundles_flattens_all_files_into_one` 改为门未过单包。
- Commit: `feat(bundle): 引用图分组——强弱边连通分量、弱缝先切、宁降不拆、孤立 bin-packing、ocr 双阈值门（design §3.2〔D3〕）`

### Task C-3 正反向跨组索引 + `{{OTHER_CHANGED_FILES}}` 升级
- Modify: `src/bundle/mod.rs`（`cross_index(bundles) -> per-group { forward: path → imports[], backward: path ← imported-by[] }`，
  `[delta]` 标注取自增量轮 delta 集）、`src/prompt/mod.rs`（`other_files_manifest` 渲染改为跨组索引：每行 `path (+a/-d) → [..] ← [..] [delta]`，
  条目帽 `MANIFEST_MAX_ENTRIES` / 字符帽 `MANIFEST_MAX_CHARS` 保留；omit 清单每条带指针 `see group N`；全部过 `neutralize_structure`）。
- 测试：渲染快照；帽与截断注记不超帽；路径含 `</bundle>` 被中和；`[delta]` 只标 delta 集内文件。
- Commit: `feat(prompt): 正反向跨组索引进 {{OTHER_CHANGED_FILES}} 槽 + [delta] 标注 + omit 留指针（design §10.6〔D6〕）`

### Task C-4 `--pack-only` 预算仪表
- Modify: `src/cli/mod.rs`（`ReviewArgs.pack_only: bool`；路径：证据收集 → 分组 → 装包 → 打印每 cell `bytes`、`est_tokens = bytes ÷ 3.5`、
  分组统计；packs 落 `.redteam/packs/<job_id>/cell-<n>.md`；不建 registry、不调 provider；退出码 0）、`src/main.rs`（clap flag + `--help`）。
- 测试：临时仓 e2e：输出行含 `cell-1 bytes= est_tokens=`；无 provider 配置也能跑。
- Commit: `feat(cli): --pack-only 预算仪表——只装包、按 cell 报字节与 ÷3.5 估算，不调 provider（D7 B7）`

### Task C-5 收口：`GroupingStats` 暂只在 `--pack-only` 打印（RunMeta 接线在 F-5，避免与 B 批撞 `report.rs`）；ship 评审 → 合回。

---

## 批 D · 契约与提示词（`feat/m21a-prompts`，Trellis `m21a-prompts`；前提 B、C 已合回）

### Task D-1 疑点契约 parse：字段级对齐 + quote 逐字丢弃
- Modify: `src/provider/parse.rs`（`parse_scan_output(raw) -> Result<ScanOutput>`：复用 `</think>` 切除 / 围栏剥离 / 配平扫描；逐条校验：
  `file` 相对路径无 `..`、`line_start ≥ 1`、`hint` ≤ 300 字符、`category` 枚举、`confidence ∈ [0,1]`、`quote` 非空；`needs` cap 5、`kind` 缺省
  evidence；`scanned_areas` 的 `ranges` 形如 `a-b`）、`src/positioning`（quote 去空白定位复用；定位失败 → **丢弃** + 计数进 warning）、
  `REPAIR_PROMPT_SCAN`（「complete suspicions JSON object」）。
- 测试：合成输出（围栏 / 前言 / 截断 / 缺 quote / quote 不在文件 / category 非法 / needs 超帽）；定位失败丢弃计数。
- Commit: `feat(parse): 疑点契约 parse 字段级对齐 + quote 不在文件即丢弃 + needs 帽（design §4〔D11〕）`

### Task D-2 模板拆分：`shared-head.md` + `scanner.md`
- Add: `src/prompt/templates/shared-head.md`（`<role>` 通用对抗评审身份、`<rules>` L0-L5、`<operating_stance>`、`<evidence>`、
  免疫段（原 `NO_TOOLS_SYSTEM` 的不可信输入条款并入）、`<bypass_class>`、`<output_language>`、`<spec_docs>` 槽、仓库简介槽（空，M21d 填））；
  `src/prompt/templates/scanner.md`（角色尾：`<finding_bar>` 疑点版「疑似即列，不需要证明」、判例摘要措辞「以下曾被驳回，若仍报请降低置信」、
  needs 措辞「缺证据声明 needs，不要猜」、`<output_contract>` = D-1 契约；文本以 P0 E3 B 臂为底稿）。
- Modify: `src/prompt/mod.rs`（`build_scan_prompt(head_vars, evidence, diff, tail_vars) -> ScanPrompt { system, blocks }`；旧
  `adversarial-code-bundle.md` / `build_bundle_prompt` **保留**给仍在跑的旧 fan-out，F-4 删）。
- 测试：共享头字节稳定（两组不同 pack 渲染的头逐字节相同）；scanner 尾含契约关键字段；模板文件与契约字段一一对应（字段名扫描）。
- Commit: `feat(prompt): 模板拆分 shared-head.md + scanner.md（疑点模式角色尾，design §10.7〔D9〕）`

### Task D-3 段序翻转 + 注入互斥
- Modify: `src/prompt/mod.rs`（渲染序：共享头 → 参考材料（spec / 仓库简介槽 / 跨组索引 / 同文件判例摘要槽）→ 证据（pack）→ diff 次尾 →
  角色尾；判例摘要槽本批空渲染（M21c 填），但槽位与中和先到位；每个注入段有且只有一个槽（注入互斥）；证据段预算帽沿用 `pack_budget_bytes`）。
- 测试：段序断言（各段起始偏移单调）；E11 注入回归全部改跑新 prompt（`neutralize_structure` 覆盖跨组索引 / 判例摘要 / brief 槽）；
  `DOSSIER_STRUCTURAL_TAGS` 加新段标签。
- Commit: `feat(prompt): 段序翻转——共享头 → 参考 → 证据 → diff 次尾 → 角色尾；注入段互斥与中和覆盖（design §10.3〔D6〕）`

### Task D-4 模板哈希 + anthropic 三段界 cache 断点
- Modify: `src/prompt/mod.rs`（`template_hash(shared_head)` / `template_hash(scanner_tail)`：`hash::fnv1a64_hex`）、`src/provider/tool_loop.rs`
  （`ChatMessage::UserBlocks(Vec<Block { text, cache_boundary: bool }>)`）、`src/provider/backends.rs`（anthropic：system = 共享头带
  `cache_control`；user blocks 在「证据段末」「角色尾前」两处打 `cache_control`（连同 system 共三段界）；openai kind：blocks 拼接为一条
  content，不变）、`src/state/mod.rs`（`CellRecord.template_hash` 填值——B-3 字段）。
- 测试：anthropic body 三处 `cache_control` 位置断言（复用现有 `cache_control` 测试形态）；openai body 与拼接文本逐字节相同；哈希稳定。
- Commit: `feat(provider): anthropic kind 三段界 cache_control + 模板哈希入 CellRecord（D10）`

### Task D-5 scanner 离线回归集（零 LLM 评分器）
- Add: `eval/scan_score.py`（从 `eval/e3_contract_ab.py score` 抽出：读 `redteam-next` 结果 JSON 的 `suspicions[]`（或 E3 run 格式），
  quote 定位 → 区间级命中 oracle（`.next/private/p0-raw/oracle-pr181.json`，或本仓 `oracle-hobby.json`）→ 命中数 / 噪音比 / 未定位数；
  `--baseline` 对比上次分数）、`eval/README.md`。
- 用法（改模板必跑）：`redteam-next review --base a7c827b70 --format json > out.json && python3 eval/scan_score.py out.json --oracle …`；
  基线 = P0 E3 B 臂三跑（自动区间命中并集 9、人工主题并集 5）。
- Commit: `chore(eval): scan_score.py——scanner 疑点对 oracle 的离线回归评分（模板改动门）`

### Task D-6 收口：ship 评审 → 合回。

---

## 批 E · 切片装包（`feat/m21a-slice`，Trellis `m21a-slice`；前提 C 已合回）

### Task E-1 文件内一跳解析
- Modify: `src/diffsym/mod.rs`（`one_hop(file_lines, defs, changed: &[DefRange]) -> OneHop { callees: Vec<DefRange>, callers: Vec<DefRange> }`：
  改动 def 体内出现的同文件 def 名 → callee；体内引用改动 def 名的其他 def → caller；名字过滤沿用 `fanin_worthy` 口径：≥ 4 字符、非通用名）。
- 测试：三语言合成文件；自引用不计；同名局部变量不误判（局部 const 非 def）。
- Commit: `feat(diffsym): 文件内一跳（改动 def 的同文件 callee / caller）`

### Task E-2 切片配方（核 / 一跳 / 文件地图 / 折叠 / 兜底）
- Modify: `src/pack/mod.rs`（`body = slice`：每文件 = 核（改动 def 全文 + diff）→ 一跳全文 → 文件地图（全部签名行 + 文件头 / 模块 doc /
  常量块骨架，复用 `skeleton_of`）→ 其余函数体折叠为 `… (N lines folded: name)` → 兜底：锚定失败的 hunk → `hunk_window_block(±anchor_window)` →
  仍失败 → 全文；文件 ≤ `slice_min_bytes`（8K）直接全文；`over_budget` 组仍走原梯子（全文 → def 体 → 骨架）作为 slice 之后的第二级降级）。
- 测试：禁真空性质测试（diff 每个改动行都出现在 slice 输出）；小文件 slice 与 full 逐字节相同；折叠计数正确；预算超限时梯子接管。
- Commit: `feat(pack): 改动中心切片——核/一跳/文件地图/折叠/禁真空兜底，slice_min_bytes 小文件全文（design §10.2〔D8〕）`

### Task E-3 文件外片段：caller 调用点 ±N + callee 签名
- Modify: `src/pack/mod.rs`（对改动 def 名在仓内（排除 changed 文件与测试）`grep-searcher` 词边界搜索 → 每名 ≤ K 处 ±`anchor_window` 行片段；
  改动 def 体内引用的、本文件未定义的标识符 → 搜其 def 行作签名 + 紧邻 doc 注释；总量受证据段预算帽，超帽进 omit 清单）。
- 测试：临时仓两文件；K 帽；omit 记录；不越 root。
- Commit: `feat(pack): 文件外片段——caller 调用点 ±N 与 callee 签名，受预算帽（design §10.2 文件外层）`

### Task E-4 收口：`[review.pack] body` 默认 `slice`、`full` 留旋钮；`--pack-only` 两档对比字节数入本批记录；ship 评审 → 合回。

---

## 批 F · 硬切与并行（`feat/m21a-cutover`，Trellis `m21a-cutover`；前提 A-E 已合回）

### Task F-1 产物改名 `redteam-next`
- Modify: `Cargo.toml`（`[[bin]] name = "redteam-next"`）、`Makefile`（`BIN := plugins/redteam/bin/darwin/aarch64/redteam-next`；`build` 不再
  `chmod` / 覆盖 `bin/redteam`；新增 `build-dev-baseline` 注释说明 dev 二进制来源）、`tests/packaging.rs`（加断言：commands 仍只引用
  `bin/redteam`，不出现 `redteam-next`）、`src/main.rs`（`--help` 头标「redteam-next（M21 重构分支产物）」）。
- Commit: `chore(build): 分支产物改名 redteam-next，make build 不覆盖插件 dev redteam（D10）`

### Task F-2 scanner 路径：按角色发 route_key + 钉推理档 + provider 在途锁
- Modify: `src/orchestrate/mod.rs`（新 `run_scan_fanout(registry, bundles, scanners, cfg) -> ScanResults`：cell = 组 × 条目 × `samples`；
  `cell_id = scan[g<n>]@<entry>#<k>`；`LensRequest.route_key = entry.display_name()`；single_shot 形态 → D-1 parse → `CellOutput::Scan`；
  needs 只记录不补跑（M21b 入队）；并发 = min(`review.scanners.workers`, provider `max_concurrency`)：`ProviderRegistry` 加 per-provider
  `Semaphore`）、`src/provider/backends.rs`（`thinking_level` → 请求参数：anthropic `thinking { budget_tokens }` 按档；openai kind
  `reasoning_effort = low|medium|high`（DeepSeek v4 的参数名以实测为准，写成映射表）；映射不存在 → 不发 + RunMeta warning；零推理 + 短输出
  记 `CellRecord.warn`）。
- 测试：`FakeChatBackend` 捕获请求体断言 thinking 参数；两条目 × 两组 = 四 cell 且 route_key 正确；在途上限 = provider 帽（信号量计数）。
- Commit: `feat(orchestrate): scanner 扇出——组 × 条目 × samples，route_key 按角色条目，thinking_level 首个消费者，provider 在途锁（design §5 / §12.3）`

### Task F-3 疑点并集与 agreement，扫描报告
- Modify: `src/consolidate/`（新 `merge_suspicions`：同组内跨 cell 以 quote 去空白重叠为锚合并，`agreement` = 不同 `family` 数；同家族多
  samples 计一次；hint 不归一（后置 M21c））、`src/domain/report.rs`（`ScanReport { groups, suspicions[], scanned_coverage, stats }`；
  `ReviewReport.verdict` 在 scan-only 下为 `ScanOnly`，退出码 0；md 渲染按组列疑点 + 覆盖自报）。
- 测试：两家族同 quote → agreement 2；同家族两 samples → 1；跨组不合并；md 快照。
- Commit: `feat(consolidate): 疑点并集与 agreement（家族数）+ scan-only 报告（design §5〔D7〕）`

### Task F-4 删旧编排
- Modify: `src/orchestrate/mod.rs`（删 `run_code_fanout` finding 路径、`top_priority_model`、候补链、`replicate` / `no_fallback`、
  `PlannedModel.priority`）、`src/cli/mod.rs`（删 A-4 适配器与 `run_code_review` 的 finding 尾段：anchor → dedup → verify → verdict 改为
  scan 路径；`second_pass` 保留模块（M21b reviewer 用）但不再从 scan 调用）、`src/prompt/`（删 `adversarial-code-bundle.md` /
  `build_bundle_prompt`；`adversarial-refute.md` 保留给 M21b verifier 拆分）、`tests/review_e2e.rs`（四个 FakeProvider 用例改为 scan 路径：
  fake 返回 `ScanOutput`；增量轮 smoke 暂标 `#[ignore]` 并记 followup——carry 在 M21c 接台账后恢复）、`src/consolidate/{dedup,verify,carry}.rs`
  保留编译（M21b-c 复用），加 `#[allow(dead_code)]` 注明。
- 测试：全量绿；`cargo clippy` 无新增；`--models` 不再出现在 `--help`。
- Commit: `refactor(orchestrate): 硬切旧 fan-out / 候补链 / 单发 finding 路径——scanner 疑点路径唯一（铁律 6〔D10〕）`

### Task F-5 观测与落盘
- Modify: `src/state/mod.rs` / `src/cli/mod.rs`（每 cell `CellRecord`（role scanner、`CellOutput::Scan`、usage、`template_hash`、
  `cached_input_tokens`）；`RunMeta`：`grouping`（C-2 stats）、每组疑点数、`scanned_coverage`（`scanned_areas` 并集 ÷ 改动行）、
  `cache_hit_by_cell`（cached ÷ input）、每 cell 墙钟（`CellSummary.duration_ms`）、`provider_inflight_peak`、coverage 五集合（scanner cell 失败 →
  `failed` → verdict 硬 Incomplete，退出码 3）、`phase_history` 加 `Scan`）。
- 测试：RunMeta 快照；一 cell 失败 → Incomplete；覆盖率计算。
- Commit: `feat(state): scanner cell 落盘 + RunMeta 观测（分组 / 疑点数 / 覆盖率 / cache / 墙钟 / 在途峰值 / coverage 五集合）`

### Task F-6 收口
- 本仓 dogfood：`redteam-next review --base dev` 走通（scan-only 报告）；`make build` 产出 `bin/darwin/aarch64/redteam-next`，
  `git status` 显示插件 `redteam` 未变。
- ship 评审（dev `redteam`）→ 修 → `--no-ff` 合回；roadmap M21a 交付项表逐项打勾。

---

## 出口验收（本工作树；`BK=~/hiapi/.worktrees/backend/artifact-transfer-reliability`，数据只落 `.next/private/m21a/`）

| # | 验收 | 命令 / 数据 | 门槛 |
|---|---|---|---|
| 1 | PR181 v2 scan（DeepSeek flash 单 scanner） | `cd $BK && redteam-next review --base a7c827b70 --format json` ×3 → `eval/scan_score.py --oracle .next/private/p0-raw/oracle-pr181.json` | 分组 ≤ 5、跨组边数可读；墙钟 ≤ 3 min；自动区间命中并集 ≥ 9、人工主题并集 ≥ 5（E3 基线） |
| 2 | 双 scanner 并集与 agreement | 配置 `[[scanners]]` = DeepSeek flash + GLM 5.3；同上 ×1 | 并集 ⊇ 单 scanner；存在 agreement = 2 的疑点且其 quote 在两家族输出中均出现；GLM cell 走 anthropic 路径 |
| 3 | **E2 slice vs full A/B**（design §16） | `[review.pack] body` 两档，PR181 ×3 + 本仓一次 dogfood 提交 ×3；比命中 / 噪音比 / 输入 token / cache 命中 | slice 命中 ≥ full → 维持默认；否则按 §10.2 回退：先加二跳再 `full`，记 followup R7 |
| 4 | cache 命中率 | RunMeta `cache_hit_by_cell`：GLM / Kimi（anthropic 三段界）与 DeepSeek（磁盘缓存） | ≥ dev 基线（dogfood L1 50% / Kimi 99%）；第二跑起 DeepSeek 命中 ≥ 90% |
| 5 | `--pack-only` 误差 | `redteam-next review --base a7c827b70 --pack-only` 的 est_tokens vs 验收 1 的 `prompt_tokens` | 差 < 20% |
| 6 | 模板回归集 | 任一模板改动后重跑验收 1 的评分 | 命中不低于改动前 |
| 7 | dev 基线归档 | K2：复用 P0 §15.1（GLM 6 / Kimi 3 ~30 min，DeepSeek 3 5.3 min，oracle 22 条）；research 加「M21a 对照基线」一行 | 已归档即过 |
| 8 | schema v4 | `eval/export_ledger.py` 计数；`rm .redteam/proj.db && redteam-next init` 空建；旧库拒开文案 | 63 / 48；五表 + user_version 4 |
| 9 | 配置验收 | 旧路径存在 → 一行提示不读；缺 provider 聚合报错；secrets 裸点 id 守卫；未知 model_id warning；`--test-command` 覆盖推断 | 五项各一手动用例 + 单测 |
| 10 | 门与文档 | `make verify`；`redteam-next --help` | 绿；含 `--test-command` / `--pack-only` / 配置路径；无 `engine` / `--risk` / `--models` |

出口通过后：roadmap `M21a-scan-foundation.md` 状态 ✅、README 表更新、E2 结论与数字写进
`../private/research/2026-09-07-redteam-p0-validation-gate-research.md` 新节「E2」或另立 research；主 checkout `.doc/roadmap/done.md` 记 M21a。

## 风险与回滚

- 批 A 是最大删改面：每个 task 独立可编译可测，适配器保证旧 fan-out 到 F-4 之前始终能跑（dogfood 靠 dev 二进制，不靠它）。
- 段序翻转与共享头：D-3 的 E11 注入回归与 D-4 的前缀稳定测试是硬门；cache 命中率在出口 4 对照 dev 基线。
- 切片配方若没修好 F3：出口 3 暴露，回退路径已定（先二跳再 `full`）；`full` 旋钮全程可用。
- 子分支回滚 = 不合回；合回后回滚 = revert 该 `--no-ff` merge commit；schema v4 重置只影响 `.redteam/proj.db`（导出固定集在库外）。
- `redteam-next` 在 M21a-b 不产 finding：插件与 ship 循环全程指向 dev `redteam`（F-1 保证 `make build` 不覆盖）。
