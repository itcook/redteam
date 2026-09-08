# M21a 批 A · 配置节：用户级唯一配置 + 三角色列表 + secrets 新形状

> 上位：`.next/plans/2026-09-07-redteam-m21a-scan-foundation-plan.md` 批 A（Task A-1 … A-6）；范围与验收
> `.next/roadmap/M21a-scan-foundation.md` 交付项 1；design v8 §12（12.1 config / 12.2 secrets / 12.3 路由）；决策 D7 A1-A9、D10、D11。
> 分支 `feat/m21a-config`（工作树 `~/hobby/.worktrees/redteam/m21a-config`），收口用 dev `redteam` 做 ship 评审后 `--no-ff` 合回 `feat/m21-review-engine`。

## Goal

配置从「用户层 + 项目层、`[models.*]` + priority 候补链、`--models` 选模」改成 design §12 的形状：`~/.redteam/`（`REDTEAM_HOME`）用户级唯一；
`[providers.X]` 键名表 + `max_concurrency`；`[[scanners]]` / `[[reviewers]]` / `[[verifiers]]` 内联条目（角色属性 `thinking_level` 同写）；
`[review.*]` 子表；secrets `[providers.X] api_key` + `"<model_id>"` 覆盖；四级解析链与两条守卫；`model setup` 写新形状；
`--models` / `--models-all` / 候补链 / 项目层配置 / `XDG_CONFIG_HOME` 退场；`--test-command` 覆盖推断。旧 fan-out 通过临时适配器继续跑到批 F。

## Background（仓库事实，2026-09-08）

- `src/config/mod.rs`（578 行 / 8 测试）：`Config { providers, models: BTreeMap<String, ModelConfig>, review }`；`ModelConfig` 含
  `provider / id / name / kind / api_key_env / context_window / output_limit / reasoning / thinking_level / per_turn_timeout_secs / priority`；
  `EffectiveModel` 是 backend 构造唯一入口（`backend_from_config(&EffectiveModel, key)` 在 `src/provider/backends.rs:657`）；
  `from_layers_tagged` 做项目层剥离（`providers` / `models`）+ 旧形状字段指引（`context_window_tokens` / `model` / `priority` …）；
  `validate()` 聚合报「引用不存在的 provider」；`config_paths()` = `$XDG_CONFIG_HOME/redteam/config.toml` → `.redteam/config.toml`；
  `write_model(...)` 写 `[providers]` + `[models]`；`ENV_LOCK` 串行化 `XDG_CONFIG_HOME` 测试（src/config 两文件共 8 处）。
- `src/config/secrets.rs`（113 行）：`{ provider: { model_id: { api_key } } }` 形状；`legacy_secrets_present()` 旧扁平形状备份；`write_secret` chmod 600；
  `dirs_config()` 读 `XDG_CONFIG_HOME` / `~/.config`。
- `ThinkingLevel { Off, Low, Medium, High }` 已入 schema、零消费（followup 实现债）。
- `ReviewConfig { default_models, block_threshold, conf_floor, max_concurrency, tools: ToolsMode }`；design §12.1 的 `[review]` 无 `tools` 键——
  `tools` 由 `build_registry` 消费（ToolLoop vs SingleShot），本批保留不动。
- 旧形状消费点：`src/cli/mod.rs::resolve_models`（132）与两处调用（257 `run_code_review`、609）、`pack_budget_bytes`（186，读 `cfg.models` 窗）、
  `src/cli/incremental.rs:87`（增量轮）、`src/cli/tail.rs:51 model_ids_for`、`src/main.rs:390 build_registry`（`effective_model` + `resolve_key`）、
  `src/model/mod.rs:62,156`（`check_all` / `render_model_list` 遍历 `cfg.models`）、clap `Review { models, models_all }`（main.rs 50-55）、
  `ModelCmd::Setup { provider, kind, base_url, model, key, priority, key_store, context_tokens }`（main.rs 247-270，处理在 825）。
- `src/project.rs` `CONFIG_TEMPLATE`（init 写 `.redteam/config.toml`，含 `[models.*]` 样例与 `test_command` 注释）；`.redteam/.gitignore` 与 `rounds/` 保留。
- `plugins/redteam/commands/ship.md` 第 2 步「测试命令探测」在 prompt 侧做：`.redteam/config.toml test_command` > Cargo.toml > package.json > Makefile >
  记忆约定 > AskUserQuestion；`make gen` 从 commands 重生成 codex-skills，`make verify` 要求零 diff。
- 用户本机：`~/.config/redteam/config.toml`（providers agc / deepseek；models GLM 5.3 / Kimi K3 anthropic kind、DeepSeek v4 flash）+ `secrets.toml`；
  dev `redteam` 继续读它们；`redteam-next` 读 `~/.redteam/`（K5，两套并存）。
- 测试面：cli/mod.rs 46 个测试里 `resolve_models` / `default_models` / `models:` 相关约 8 个；incremental.rs 3 处；main.rs `build_registry_no_provider_returns_err`；
  secrets.rs 1 个；config/mod.rs 8 个。`tests/review_e2e.rs` 直接构造 `ProviderRegistry`，不经 config。

## Requirements（映射 plan Task A-1 … A-6）

- R-A1 路径：`config_paths()` → 单路径 `redteam_home()/config.toml`，`redteam_home()` = `$REDTEAM_HOME` 否则 `~/.redteam`；secrets 同目录；
  删项目层与 `from_layers_tagged` 的剥离分支（`from_layers` 保留为多文本合并的测试口）；旧路径（`~/.config/redteam/config.toml`、`.redteam/config.toml`）
  存在 → stderr 一行提示、不读；`init` 不再写 `config.toml`；8 处 `XDG_CONFIG_HOME` → `REDTEAM_HOME`。
- R-A2 形状：`ProviderConfig` 加 `max_concurrency: Option<u32>`；`RoleEntry` 结构（name? / provider / model_id / kind? / api_key_env? / family? /
  context_window? / output_max_tokens? / reasoning / thinking_level? / per_turn_timeout_secs?）；`Config { providers, scanners, reviewers, verifiers, review }`；
  `ReviewConfig` 加 `scanners { workers=4, samples=1 }`、`reviewers { workers=2, max_cells: Auto|Fixed, reenqueue=false }`、`verifiers { enabled=true, workers=2 }`、
  `grouping { min_files=4, bundle_line_threshold=200, max_files_per_group=10, semantic=false }`、`pack { body: Slice(默认〔D8〕)|Full, anchor_window=30, slice_min_bytes=8192 }`；
  删 `[models]` / `priority` / `default_models` / `output_limit`（改 `output_max_tokens`）；旧键出现 → 指向性错误（沿用现有旧形状指引机制，文案指向 §12.1）；
  `validate()`：条目引用不存在的 provider → 聚合报错；`effective_entry(role, name) -> EffectiveModel`（`EffectiveModel` 加 `family`、`max_concurrency`，删 `priority`）。
- R-A3 secrets：`[providers.X] api_key` + `"<model_id>" = "key" | { api_key = "…" }`；`resolve_key(&RoleEntry)` 四级链（条目 `api_key_env` env → secrets[X][model_id]
  → provider `api_key_env` env → secrets[X].api_key）；守卫①provider 表下出现子表且键名含 `.`（裸写带点 id）→「是否漏了引号」；守卫②非 `api_key` 键在 config 该
  provider 下无对应 `model_id` → warning；`write_secret(provider, model_id: Option, key)` 新形状 + chmod 600；`model setup --role scanner|reviewer|verifier`
  写 `[providers.X]` + 追加条目（`--name` 可选、`--context-window` 整数、`--output-max-tokens`、`--thinking-level`），删 `--priority`。
- R-A4 CLI：删 `--models` / `--models-all` / `resolve_models` / `ReviewArgs.models*`；`active_roles(cfg) -> RoleSets`；临时适配器
  `roles_to_legacy_opts` = scanners → `PlannedModel{priority 0}`、`replicate: true`、`no_fallback: true`（旧 fan-out 对每条 scanner 各跑一 cell）；
  `pack_budget_bytes` / `model_ids_for` / 增量轮改读 `RoleSets`；`--test-command <cmd>` 进 `ReviewArgs` 与 `RunMeta.test_command`；推断
  `infer_test_command(root)`（Cargo.toml → `cargo test`；package.json `scripts.test` → `npm test`；Makefile 含 `test:` → `make test`；go.mod → `go test ./...`；否则 None）。
- R-A5 registry：`build_registry` 按 `RoleSets` 逐条目建 backend，`route_key = entry.display_name()`（缺省 `provider/model_id`）；条目 kind / api_key_env 覆盖 provider；
  `per_turn_timeout_secs` 缺省 = `output_max_tokens ÷ 20 + 60` 秒（无 output_max_tokens 则沿用 `per_turn_deadline()`）；provider `max_concurrency` 本批只读入不加锁（F-2）。
- R-A6 收口：`--help`（配置路径三行、`--test-command`、`model setup --role`）；本机手动迁移；ship 评审；`--no-ff` 合回。

## Acceptance Criteria（对应 roadmap M21a「配置」验收）

- [x] AC1 `REDTEAM_HOME=/tmp/x redteam-next model list` 只读 `/tmp/x/config.toml`；旧路径文件存在时 stderr 恰一行提示且内容未被读入（单测 + 手动）。
- [x] AC2 design §12.1 样例整段解析通过；引用不存在 provider 的两条目 → 一条聚合错误列出两者；`[models]` / `priority` / `default_models` 出现 → 指向性错误。
- [x] AC3 secrets 四级解析链各一单测；裸写带点 id → 提示含「引号」；未知 model_id 键 → warning；`model setup --role scanner --key-store secrets` 写出的两文件可被解析回同值。
- [x] AC4 `redteam-next review --models x` → clap 未知参数；`--test-command "make check"` 出现在 `redteam-next result <id>` 的 `run.test_command`；无 flag 时四种推断各一单测。
- [x] AC5 两条 `[[scanners]]` 时旧 fan-out 产生两个 cell、`route_key` 为条目名（复用 `code_replicate_two_models_yield_agreement_two` 形态）。
- [x] AC6 `cargo test` 全绿、`cargo clippy --all-targets` 零新增、`make verify` 绿；`--help` 含配置路径与 `--test-command`、不含 `--models`。

## Out of Scope

批 B-F 的内容（schema v4、分组、契约、切片、硬切）；provider 在途锁（F-2）；`tools` 档位与 `--tools` 的去留（M21b）；自动迁移旧配置（D7 A9）；`redteam-next` 改名（F-1）。

## Key Decisions

| # | 决策 | 依据 |
|---|---|---|
| K1 | 加 `redteam test-command [--override]` 子命令（打印生效测试命令，推不出退出码 2）；`review --test-command` 记入 `RunMeta.test_command`（**用户 2026-09-08 定**） | CLAUDE.md 内置命令优先；项目层 `test_command` 来源随项目 config 消失 |
| K2 | ship.md 本批不改，M21c 切名后再改调子命令；子命令为过渡形态，ship 协议进二进制时评估拆卸 / 合并——两条均记 followup（**用户要求**） | 插件在 M21a-b 跑 dev 二进制，无此子命令 |
| K3 | `build_registry` 本批只注册 scanners（route_key = 条目 display_name）；reviewers / verifiers 在 F-2 接线 | 避免同一模型跨角色同名键冲突；本批产品态只跑 scanner |
| K4 | `Role` 枚举放 `src/config/mod.rs`，批 B 的 domain `pub use` 它 | 两批并行，避免各自定义后合并冲突 |
| K5 | `ReviewConfig.tools` 与 `--tools` 保留不动 | design §12.1 未列但 §11 工具分层由 M21b 定 |
| K6 | `[review.pack] body` 缺省 `slice`（D8），本批只定形状，pack 侧实现在批 E | 形状与 design 一致，批 E 前该值不生效 |

## Risks / Deferred

- `src/domain/report.rs` 加 `RunMeta.test_command` 与批 B 的 RunMeta 字段改动可能小冲突，后合者 rebase。
- 旧作业 `--resume`：registry 按当前 scanners 建，与原作业模型可能不同——M21a 产品态不产 finding，可接受；记 followup 观察。
- 本机迁移是手动步（A-6），dev `redteam` 的旧配置不动。
