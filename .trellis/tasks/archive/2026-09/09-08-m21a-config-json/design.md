# design · 批 A 重做（JSON，D12）

> 需求见 `prd.md`；形状真相源 design v9 §12。本文只写落地细节。

## 1. 类型（`src/config/mod.rs`，全部 `#[serde(rename_all = "camelCase")]`）

```rust
pub struct Config { providers: BTreeMap<String, ProviderConfig>, review: ReviewConfig }
pub struct ProviderConfig { kind: Option<ProviderKind>, base_url, api_key_env, max_concurrency: Option<u32>, models: BTreeMap<String, ModelConfig> }
pub struct ModelConfig { name: Option<String>, kind: Option<ProviderKind>, api_key_env: Option<String>, family: Option<String>,
    context_window: Option<u32>, max_output_tokens: Option<u64>, per_turn_timeout_secs: Option<u64>, reasoning: bool,
    roles: BTreeMap<Role, RoleAttrs> }
pub struct RoleAttrs { thinking_level: Option<ThinkingLevel> }          // 角色属性；将来加字段只在这里
pub struct ReviewConfig { block_threshold, conf_floor, max_concurrency, tools, scanners, reviewers, verifiers, grouping, pack }  // camelCase
pub struct EffectiveModel { name: String /* provider/id */, display: Option<String>, id, provider, role, kind, base_url, context_window,
    max_output_tokens, per_turn_timeout_secs, reasoning, thinking_level, family, max_concurrency }
pub struct RoleSets { scanners, reviewers, verifiers: Vec<EffectiveModel> }   // 不变；`id_of(name)` 不变
```
- `Role` 作 map 键：serde 用 `lowercase` 字符串键（`scanner` / `reviewer` / `verifier`），`Ord` 已派生。
- `Config::model(provider, id) -> Option<(&ProviderConfig, &ModelConfig)>`；`effective(provider, id, role)`；`active_roles()`（按 `BTreeMap` 序）。
- `validate()` 聚合：每模型 `kind` 可解析（模型层 > provider 层）；`context_window` ≥ `MIN_CONTEXT_WINDOW = 32_000`；`roles` 空的模型给 warning
  （配置了但不参与）；`review.*` 数值 ≥ 1。`scanners` 空不在 load 报错（review 时报，不变）。
- 旧形状指引 `legacy_shape_guard(&serde_json::Value)`：顶层 `scanners|reviewers|verifiers|models`、provider 下 `models` 为数组、键含 `_`
  （snake_case 疑似首版拼写）→ 错误指向 D12 / `--help`。
- 解析：`serde_json::from_str::<Value>` 先过指引，再 `from_value::<Config>`；错误文案带 `line:column`（serde_json 自带）。

## 2. 路径与加载
- `redteam_home() -> anyhow::Result<PathBuf>`：`REDTEAM_HOME`（非空）> `HOME/.redteam`；都缺 → `Err("REDTEAM_HOME 与 HOME 皆未设置")`（#1）。
  `config_paths()` / `secrets_path()` 随之返回 `Result`；`Config::load()` 传播。
- `load()`：`read_to_string` → `NotFound` 视为空 `{}`；其他 io 错误（EACCES / EISDIR / 非 UTF-8）→ `Err`（#13）。
- 旧路径提示 `legacy_config_paths_present()`：候选 `$XDG_CONFIG_HOME/redteam/config.toml`、`~/.config/redteam/config.toml`、`./.redteam/config.toml`、
  `$REDTEAM_HOME/config.toml`（首版）；过滤：非文件、纯注释 / 空白（TOML 注释 `#`，#9）、canonicalize 等于当前 `config.json` 所在目录下同名（不可能）——保留
  canonical 去重。每条一行 notice。

## 3. secrets（`src/config/secrets.rs`）
- `Secrets { providers: BTreeMap<String, ProviderSecrets { api_key: Option<String>, models: BTreeMap<String, String> }>, warnings }`；
  `Secrets::parse(&str)`：`serde_json::Value` 手动走（类型不对 → warning，不整档失败）。
- `Config::secret_key_warnings(&Secrets)`：provider 不在 `cfg.providers` → warning；model id 不在该 provider `models` → warning（#10）。
- `resolve_key(provider, id)` 四级链；`resolve_provider_key(provider)` = provider env → `apiKey`（无「任一 model key」回退，#5/K6）。
- `write_secret(provider, model_id: Option<&str>, key)`：现有文件解析失败 → `Err` 拒写（#8）；写 `providers.X.apiKey` 或 `providers.X.models[id]`；pretty JSON；chmod 600。

## 4. `model setup`（`src/main.rs` + `config::write_setup`）
- 参数：`--provider X --model <id> --role scanner[,reviewer,verifier]`（必填）；可选 `--name --kind --base-url --max-concurrency --context-window
  --max-output-tokens --reasoning --thinking-level --key --key-store env|secrets --model-key`。`--model-key` 且 `key_store != secrets` → 报错。
- `config::write_setup(patch: SetupPatch) -> Result<()>`：读现有 JSON（解析失败 → 拒写）→ `Value` 三层合并：
  `providers[X]`：不存在则新建（`kind` = `--kind` 或 `derive_kind`；`apiKeyEnv` = `--api-key-env` 或 `derive_api_key_env`），存在则只写显式 flag（#3）；
  `models[id]`：只写显式 flag（不存在则新建空对象 + `reasoning` 显式时才写）；`roles[r]`：不存在建 `{}`，`--thinking-level` 显式时写。
  合并后 `from_value::<Config>` + `validate()` 失败 → 拒写并回显错误（#4）；成功 → `to_string_pretty` 落盘。
- 落盘后探测：`cfg.resolve_key(X, id)` 取 key（#5），`effective_base(provider)` 用合并后的 provider。
- `model list` 每角色一行：`[scanner] deepseek/deepseek-v4-flash  (DeepSeek V4 Flash)  [openai]  ✓ available`；`model check` 同源。

## 5. 超时（`src/main.rs::build_registry`）
`per_turn = 模型 per_turn_timeout_secs` → `REDTEAM_PER_TURN_SECS` → `max_tokens_effective ÷ 20 + 60`（`max_tokens_effective` = `maxOutputTokens` |
`REDTEAM_MAX_TOKENS` | 32 768，与 backend 实际发送一致）→ `per_turn_deadline()` 的 150s（#6）。抽成 `config::per_turn_secs(em) -> u64` 纯函数，单测四级。

## 6. 其他并入
- `project::infer_test_command`：Makefile 目标行正则 `^test\s*:(?!=)`（regex crate 无 lookahead → 用 `starts_with("test")` + 去空白后首字符 `:` 且下一字符非 `=`）；
  package.json `scripts.test` 含 `no test specified` → 不算（#11）。
- `--resume` 帮助：「其余 flag 忽略；模型集按当前配置的 scanners 重建（M21b 起读队列快照）」（#14）。
- `Cargo.toml` 删 `toml`；`--help` / `init` 文案指 `config.json`。

## 7. 兼容与迁移
- 首版 TOML（be87c44）在历史里；`$REDTEAM_HOME/config.toml` 存在只提示不读；dev 二进制照旧读 `~/.config/redteam/`。
- 本机：`/tmp/rt-migrate-config-json.py` 自 `~/.config/redteam/{config,secrets}.toml` 生成 JSON（GLM / Kimi / DeepSeek 作 scanner，GLM / Kimi 兼 reviewer；
  4 个 model 级 key），旧 `~/.redteam/*.toml` 改名 `.bak`。
- `RoleSets` / 适配器 / `pack_budget_bytes` / `model_ids_for` / registry 只注册 scanners / `test-command` 全部不变，消费方零改。

## 8. 回滚
子分支不合回即回滚；无数据与 schema 改动。
