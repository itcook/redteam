# design · M21a 批 A 配置节

> 需求见 `prd.md`；本文只写技术定论。design v8 §12 是形状真相源，这里只定落地细节。

## 1. 类型（`src/config/mod.rs`）

```rust
pub struct ProviderConfig { kind, base_url, api_key_env, max_concurrency: Option<u32> }
pub enum Role { Scanner, Reviewer, Verifier }                         // 批 B 的 domain 直接 `pub use crate::config::Role`
pub struct RoleEntry {
    name: Option<String>, provider: String, model_id: String, kind: Option<ProviderKind>, api_key_env: Option<String>,
    family: Option<String>, context_window: Option<u32>, output_max_tokens: Option<u64>, reasoning: bool,
    thinking_level: Option<ThinkingLevel>, per_turn_timeout_secs: Option<u64>,
}
impl RoleEntry { display_name() = name | "{provider}/{model_id}";  family() = family | model_id 按 '-' '_' '.' 首段 }
pub struct Config { providers, scanners: Vec<RoleEntry>, reviewers: Vec<RoleEntry>, verifiers: Vec<RoleEntry>, review: ReviewConfig }
pub struct ReviewConfig { block_threshold, conf_floor, max_concurrency, tools /*保留*/, scanners: ScannersCfg{workers=4, samples=1},
    reviewers: ReviewersCfg{workers=2, max_cells: MaxCells(Auto|Fixed(u32)), reenqueue=false}, verifiers: VerifiersCfg{enabled=true, workers=2},
    grouping: GroupingCfg{min_files=4, bundle_line_threshold=200, max_files_per_group=10, semantic=false},
    pack: PackCfg{body: PackBody(Slice 默认|Full), anchor_window=30, slice_min_bytes=8192} }
pub struct EffectiveModel { name(display), id, provider, kind, base_url, context_window(缺省 128_000), output_max_tokens, per_turn_timeout_secs,
    reasoning, thinking_level, family, max_concurrency: Option<u32>, role: Role }
```
- `MaxCells` 自定义 Deserialize：字符串 `"auto"` 或整数；其余报错。`PackBody` / `Role` kebab-case。
- `Config::entries(role)`、`effective_entry(role, name)`、`active_roles() -> RoleSets { scanners, reviewers, verifiers: Vec<EffectiveModel> }`。
- `validate()` 聚合：条目引用不存在 provider；同角色 display_name 重复；`scanners` 空**不**在 load 报错（`review` 时报「未配置 [[scanners]]」）。
- 旧形状指引（沿用现有机制，文案指向 `--help` 与 design §12.1）：顶层 `models` / `review.default_models` / 条目含 `priority` 或 `output_limit` /
  provider 含 `model` 等旧字段 → `bail!`。

## 2. 路径（`src/config/mod.rs` + `secrets.rs`）
- `redteam_home()` = `$REDTEAM_HOME` | `~/.redteam`；`config_paths()` = `[home/config.toml]`（保留 Vec 形状给 `render_model_list` 显示）；
  `secrets_path()` = `home/secrets.toml`。
- `legacy_config_paths_present() -> Vec<PathBuf>`（`~/.config/redteam/config.toml`、`./.redteam/config.toml`）→ `Config::load()` 里 stderr 一行：
  `notice: 配置已改读 ~/.redteam/（REDTEAM_HOME 可覆盖）；旧文件 <path> 未读，请手动迁移（redteam model setup --help）`。
- 测试：`ENV_LOCK` 保留，环境变量改 `REDTEAM_HOME`；`tests/` 不涉及。

## 3. secrets（`src/config/secrets.rs`）
```toml
[providers.X]
api_key = "…"
"model.id" = "…"                # 或 { api_key = "…" }
```
- 解析：顶层只认 `providers` 表 → `BTreeMap<provider, ProviderSecrets { api_key: Option<String>, models: BTreeMap<model_id, SecretEntry> }>`；
  provider 表内：`api_key` 字符串；其他键 = 字符串 → `SecretEntry`；= 含 `api_key` 的表 → `SecretEntry`；= 不含 `api_key` 的表（裸写带点 id 的产物）
  → warning「secrets.toml [providers.X] 下出现子表 "<k>"：model_id 含点须加引号（"<k>.…" = "…"）」。顶层出现 `providers` 以外的表 → warning「旧形状（09-04
  `[provider."model"]`），请手动迁到 [providers.X]」，不自动迁移。
- 未知键守卫在 `Config::load()` 后：secrets 里非 `api_key` 的键若不匹配该 provider 下任一条目 `model_id` → warning。
- `resolve_key(&RoleEntry)`：条目 `api_key_env`(env) → `secrets[provider].models[model_id]` → provider `api_key_env`(env) → `secrets[provider].api_key`。
  `resolve_provider_key(provider)`：provider env → `secrets[provider].api_key` → 任一 model key（`model list --remote` 用）。
- `write_secret(provider, model_id: Option<&str>, key)`：读现有 `toml::Value`，写 `[providers.X].api_key` 或 `"model_id" = key`，chmod 600；
  旧形状文件先 `.bak`（沿用 `legacy_secrets_present` 思路：顶层非 `providers` 表即旧）。

## 4. CLI / registry / 子命令
- `ReviewArgs`：删 `models` / `models_all`；加 `test_command: Option<String>`（serde default；旧 args.json 兼容）。
- `RoleSets` + 适配器 `roles_to_legacy_opts(&RoleSets, max_concurrency) -> OrchestrateOpts { models: scanners→PlannedModel{name: display, priority: 0},
  replicate: true, no_fallback: true }`；`run_code_review` / `run_incremental_review` / `build_registry` 三处改用；scanners 空 → `Err("未配置 [[scanners]]…")`。
- `pack_budget_bytes(&RoleSets)`（scanner 最小窗）；`model_ids_for(names, &RoleSets)`。
- `build_registry`：本批只注册 **scanners**（route_key = display_name；reviewers / verifiers 在 F-2 接线，避免同名跨角色键冲突）；
  `backend_from_config(&EffectiveModel, key)` 改读 `output_max_tokens`；`per_turn` = 条目值 | `output_max_tokens ÷ 20 + 60` 秒 | `per_turn_deadline()`。
- `model setup`：`--role scanner|reviewer|verifier`（必填）、`--name`、`--provider`、`--kind`、`--base-url`、`--model`（model_id）、`--key`、
  `--key-store env|secrets`（secrets 写 `[providers.X].api_key`，`--model-key` 时写 model 级）、`--context-window <u32>`、`--output-max-tokens <u64>`、
  `--thinking-level`；删 `--priority` / `--context-tokens`；`write_entry(role, &RoleEntry, provider_cfg)` 深合并写 `[providers.X]` + 追加 `[[<role>s]]`
  （同 provider/model_id 已存在则更新而非重复追加）。`model list` / `check` 按角色分组显示条目。
- `redteam test-command [--override <cmd>]`：`project::infer_test_command(root) -> Option<String>`（Cargo.toml → `cargo test`；package.json
  `scripts.test` → `npm test`；Makefile 含 `^test:` → `make test`；go.mod → `go test ./...`）；打印命令；推不出 → stderr 提示 + 退出码 2。
  `review --test-command` 优先级同 `--override`，写入 `RunMeta.test_command`（无 flag 时写推断值）。
- `init`：不再写 `config.toml`；`CONFIG_TEMPLATE` 删除；`--help` 文案同步。

## 5. 兼容与迁移
- 无自动迁移（D7 A9）；dev `redteam` 读旧路径、`redteam-next` 读新路径，两套并存（K5）。
- 旧 `args.json`（含 `models` 字段）由 serde 忽略未知字段照常反序列化；`--resume` 老作业时 registry 按当前配置 scanners 建，与 M21a「不产 finding」的产品态一致。
- ship.md **本批不改**（插件跑 dev 二进制，无 `test-command`）；M21c 切名后改调子命令（followup）。

## 6. 回滚
- 子分支不合回即回滚；无数据迁移、无 schema 改动。
