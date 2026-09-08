# M21a 批 A 重做：配置改 JSON + provider → models → roles（D12）

> 上位：决策 D12（取代 D7 A1 / A2）、design v9 §12、roadmap M21a 交付项 1、plan 批 A 注。分支 `feat/m21a-config-json`
> （工作树 `~/hobby/.worktrees/redteam/m21a-config-json`，自 `feat/m21-review-engine@4704076` 切出，含首版 TOML 实现 be87c44）。
> 首版评审记录：`.trellis/tasks/09-08-m21a-config/implement.md`（ship 评审）+ 本 session 的 code-review（high）报告（#1-#14）。

## Goal

把首版（TOML、内联角色条目）改为 D12 形状：`~/.redteam/config.json` + `secrets.json`，providers → models（按 id 做键）→ roles；
模型固有字段只写一次、角色属性挂模型下；`model setup` 三层只合并显式字段、写前校验；并入 code-review 的必修 #1-#4 与建议 #5-#11 #13 #14。
`toml` 依赖退出。旧 fan-out 仍经适配器跑到批 F（不变）。

## Background（分支现状，2026-09-08）

- 现有实现（be87c44）：`src/config/mod.rs`（TOML：`RoleEntry` 三列表、`RoleSets`、`EffectiveModel`、`legacy_shape_guard`、`write_entry`）、
  `src/config/secrets.rs`（TOML `[providers.X] api_key + "<id>"`）、`src/main.rs`（`ModelCmd::Setup --role …`、`build_registry` 只注册 scanners、
  `Cmd::TestCommand`）、`src/model/mod.rs`（按 `all_entries()` 探测 / 渲染）、`src/cli/{mod,incremental,tail}.rs`（`legacy_opts_from_roles`、
  `pack_budget_bytes(&RoleSets)`、`model_ids_for`）、`src/project.rs`（`infer_test_command`）、`src/domain/report.rs`（`RunMeta.test_command`）。
- 消费方只依赖 `RoleSets` / `EffectiveModel` / `Config::resolve_key` / `config_paths()` / `redteam_home()`——形状改动可收敛在 config 模块 + `main.rs` setup / registry + `model/mod.rs`。
- `toml` 只在 `src/config/` 使用（重写后可从 Cargo.toml 删除）；`serde_json` 已是依赖。
- 本机：`~/.redteam/{config,secrets}.toml`（首版迁移产物）与 `~/.config/redteam/`（dev 用）；重做后本机改生成 `config.json` / `secrets.json`，TOML 留 `.bak`。
- code-review（high）报告要点：#1 HOME+REDTEAM_HOME 皆缺退化相对路径（安全）；#2 原位更新整条替换；#3 覆写共享 provider 字段；#4 写前不校验；
  #5 探测用 `resolve_provider_key`；#6 超时优先级 env 在公式之后；#7 `contextWindow` 无下限；#8 `write_secret` 整档重写 + 固定 bak；#9 纯注释旧模板刷提示；
  #10 secrets provider 拼错零提示；#11 `test:=` / npm 占位脚本；#12 ship.md `--models`（followup）；#13 读错误吞成空配置；#14 `--resume` 帮助文案。

## Requirements

- R1 **形状与解析**（design §12.1）：`Config { providers: BTreeMap<name, ProviderConfig{ kind?, baseUrl?, apiKeyEnv?, maxConcurrency?, models: BTreeMap<id, ModelConfig> }>, review }`；
  `ModelConfig { name?, kind?, apiKeyEnv?, family?, contextWindow?, maxOutputTokens?, perTurnTimeoutSecs?, reasoning, roles: BTreeMap<Role, RoleAttrs{ thinkingLevel? }> }`；
  `review` 五子表 camelCase（`maxCells: "auto" | int`、`pack.body` 缺省 `slice`）；严格 JSON、无注释；解析错误带行列。
- R2 **活跃集与身份**：`active_roles()` = 每角色拥有该角色键的模型，顺序 provider 名、id 字典序；`EffectiveModel.name` = `provider/id`（route_key / 报告名）、`display` = `name?`；
  `family` 缺省 id 首段；`kind` = 模型层 > provider 层，都缺 → `validate()` 报错；`contextWindow` < 32 000 → 报错（#7）；同 provider 同 id 由键唯一天然去重。
- R3 **路径与加载**：`redteam_home()` = `REDTEAM_HOME` > `HOME/.redteam`，两者皆缺 → `Err`（#1，不退化相对路径）；`config_paths()` = `[home/config.json]`；
  读文件 NotFound → 空配置，其他 io 错误 → `Err`（#13）；旧路径提示：`$XDG_CONFIG_HOME/redteam/config.toml`、`~/.config/redteam/config.toml`、`./.redteam/config.toml`
  （纯注释 / 空文件不提示，#9）、`$REDTEAM_HOME/config.toml`（首版 TOML）各一行，不读；与当前生效路径相同不算旧。
- R4 **旧形状指引**：JSON 顶层出现 `scanners` / `reviewers` / `verifiers` / `models`，或 provider 下 `models` 为数组 → 指向 D12 形状的错误（`--help`）。
- R5 **secrets.json**（§12.2）：`{"providers": {"X": {"apiKey"?, "models"?: {"<id>": "key"}}}}`；解析链 模型 `apiKeyEnv` env → secrets.models[id] → provider `apiKeyEnv` env → secrets.apiKey；
  守卫：secrets 里 provider 名不在 config、或 model id 不在该 provider 下 → warning（#10）；`write_secret` 现有文件解析失败拒写（#8），chmod 600；`resolve_provider_key` 只保留给 `model list --remote`，
  不再回退「任一 model key」。
- R6 **`model setup`**：`--provider --model <id> --role a[,b]`（必填）+ 可选 `--name --kind --base-url --max-concurrency --context-window --max-output-tokens --reasoning --thinking-level --key --key-store --model-key`；
  三层只合并显式给出的字段（新 provider 缺 `--kind` 时按 `derive_kind` 补，已存在则不动，#3）；合并后完整解析 + `validate()`，失败拒写（#4）；现有 JSON 解析失败拒写（#2 消失于按键合并）；
  落盘后探测走 `resolve_key(provider, id)`（#5）；`--model-key` 无 `--key-store secrets` → 报错。
- R7 **超时优先级**：模型 `perTurnTimeoutSecs` > `REDTEAM_PER_TURN_SECS` > `实际生效 max_tokens(maxOutputTokens | REDTEAM_MAX_TOKENS | 32768) ÷ 20 + 60` > 150s（#6）。
- R8 **其他并入**：`infer_test_command` 的 Makefile 目标匹配排除 `test:=` / 支持 `test :`，npm `scripts.test` 含 "no test specified" 不算（#11）；`--resume` 帮助改为「按当前配置 scanners 重建」（#14）。
- R9 `toml` 从 Cargo.toml 删除；`--help` / `model setup --help` / `init` 文案指 JSON；本机迁移脚本改出 JSON。
- R10 不变：`RoleSets` / `legacy_opts_from_roles` / `pack_budget_bytes` / `model_ids_for` / `Cmd::TestCommand` / `RunMeta.test_command` / `build_registry` 只注册 scanners。

## Acceptance Criteria

- [ ] AC1 design §12.1 样例整段解析；`active_roles()` 三角色集合与顺序正确；`provider/id` 身份；`kind` 两层都缺 / `contextWindow` 过小 → 聚合报错。
- [ ] AC2 HOME 与 REDTEAM_HOME 皆缺 → `Config::load()` 报错（单测）；权限错误不吞（单测：目录当文件读 → Err）；旧路径四类各提示一行、纯注释不提示、与当前路径相同不提示。
- [ ] AC3 secrets：解析链四级；未知 provider / model id 各一 warning；解析失败拒写且原文件逐字保留；chmod 600。
- [ ] AC4 `model setup`：手写字段（`name` / `family` / `perTurnTimeoutSecs` / 模型级 `apiKeyEnv`）在重跑 setup 后保留；已存在 provider 的 `kind` / `apiKeyEnv` 不被无 flag 的 setup 改动；
  合并结果校验失败（如 `contextWindow` 过小）拒写、文件不变；`--model-key` 无 `--key-store secrets` 报错。
- [ ] AC5 超时优先级四级各一单测（含 env 覆盖公式、公式用 `REDTEAM_MAX_TOKENS`）。
- [ ] AC6 `infer_test_command`：`test:=1` 不算、`test :` 算、npm 占位脚本不算。
- [ ] AC7 `cargo test` 全绿、clippy 零新增、`make verify` 绿；`grep -rn toml src Cargo.toml` 仅剩注释或无；`--help` 无 TOML 提法。
- [ ] AC8 本机 `~/.redteam/config.json` + `secrets.json` 生成，`cargo run -- model list` 五条可用、旧路径提示合理。

## Out of Scope

批 B-F；provider 在途锁（F-2）；ship.md（M21c，followup #12）；`tools` 档位（M21b）；`--resume` 读快照（M21b）；自动迁移旧配置（D7 A9，本机脚本是一次性工具）。

## Key Decisions

| # | 决策 | 依据 |
|---|---|---|
| K1 | JSON + provider → models → roles，无注释（**用户 2026-09-08 定**，D12） | D7 A1 三条顾虑 JSON 全绕开；按 id 做键消灭表数组与原位更新歧义 |
| K2 | 先改形状再修评审项，两者合一次重做 | #2 #3 #4 所在代码被整体重写；#1 落在同一路径解析里 |
| K3 | 活跃集顺序按 provider 名、id 字典序（`BTreeMap`），不引 `preserve_order` | 顺序无语义，不为保序加依赖 |
| K4 | `kind` 模型层 > provider 层，两者都缺校验报错；`contextWindow` 下限 32 000 | 预算公式 `(ctx−16000)×3−24000` 在 24 000 以下饱和为 0 |
| K5 | `model setup` 三层显式字段合并，新 provider 才用 `derive_kind` 补 kind | #3；显式即权威、隐式不覆盖 |
| K6 | `resolve_provider_key` 不再回退任一 model key；探测走 `resolve_key(provider, id)` | #5 |
| K7 | `toml` 依赖删除；本机 TOML 留 `.bak` | D12 |

## Risks / Deferred

- 批 B / C 若已基于 be87c44 开工，合回后需 rebase：`domain/report.rs`（B）与 `cli/mod.rs` 测试夹具（C）可能小冲突；本任务不碰 `domain/` 与 `bundle/`。
- `serde_json` 对未知字段静默忽略（与旧 TOML 同）：`maxOutputToken` 拼错不报——记 followup（可用 `deny_unknown_fields` 换取严格，但会把新增字段变成硬错误，M21e 再议）。
- 首版 TOML 已在 `feat/m21-review-engine` 历史里；本任务合回后 `config.toml` 只在旧路径提示中出现。
