# implement · M21a 批 A（分支 feat/m21a-config）

三道门每步：`cargo test`、`cargo clippy --all-targets`、`make verify`。
实际落地：A-1 … A-5 + `test-command` 因 `ModelConfig` 删除牵一发动全身，合为**一笔代码提交**（各步中间态无法独立编译）；A-6 收口另一笔。

## A-1 路径与层序
- [x] `redteam_home()` / `config_paths()` / `secrets_path()`；删项目层与剥离分支；`legacy_config_paths_present()` + load 提示；`init` 不写 config.toml（`project.rs` 删模板、改 `--help`）。
- [x] 8 处 `XDG_CONFIG_HOME` → `REDTEAM_HOME`；测试：env 覆盖 / 缺省 / 旧路径提示 / init 不写 config。

## A-2 新形状解析
- [x] `Role` / `RoleEntry` / `MaxCells` / `PackBody` / 五个 `[review.*]` 子表 / `EffectiveModel` 新字段；删 `ModelConfig`、`priority`、`default_models`、`output_limit`；`validate()` 聚合；旧键指引。
- [x] 测试：§12.1 样例整段解析；缺省 name / family；聚合报错文案；`max_cells` 两形态；旧键指引；`[review.risk]` warning。

## A-3 secrets、解析链、守卫、model setup
- [x] `secrets.rs` 新形状解析 + 两守卫 + `write_secret`；`resolve_key(&RoleEntry)` 四级链；`resolve_provider_key`；未知键 warning 接到 `Config::load()`。
- [x] `main.rs` `ModelCmd::Setup --role/--name/--model-key/--context-window/--output-max-tokens/--reasoning/--thinking-level`；`write_entry`；`model list` / `check` 按角色；`src/model/mod.rs` 遍历改条目。
- [x] 测试：四级链；引号守卫；未知 model_id warning；write_entry round-trip 与原位更新；secrets 永不进 config。

## A-4 CLI 面 + test-command
- [x] 删 `--models` / `--models-all` / `resolve_models`；`RoleSets` + `legacy_opts_from_roles`；`pack_budget_bytes` / `model_ids_for` / incremental 改读；`--test-command` + `RunMeta.test_command`（三条尾段路径接线）；`project::infer_test_command`；`Cmd::TestCommand`。
- [x] 测试：适配器两 scanner → 两 cell（`roles_adapter_replicates_all_scanners_without_fallback` + e2e `code_replicate_two_models_yield_agreement_two`）；`--models` 未知参数（手动）；四种推断 + override；RunMeta 推断与覆盖。
- [x] `.next/roadmap/followup.md` 加四行：test-command 过渡拆卸、ship.md M21c 改调、resume 按当前 scanners、适配器 F-4 删。

## A-5 build_registry 角色化
- [x] 按 scanners 建 registry（route_key = display_name）；`backend_from_config` 读 `output_max_tokens`；per_turn 缺省公式 `output_max_tokens ÷ 20 + 60`。
- [x] 测试：`build_registry_no_provider_returns_err` 改写为「无 [[scanners]]」；条目 kind 覆盖（config 单测）。

## A-6 收口
- [x] `--help`：配置路径、`--test-command`、`test-command` 子命令、`model setup --role`。
- [x] 本机迁移：`/tmp/rt-migrate-config.py` 自 `~/.config/redteam/` 生成 `~/.redteam/{config,secrets}.toml`（GLM / Kimi / DeepSeek 作 scanner，GLM / Kimi 兼作 reviewer；secrets 4 个 model 级 key，mode 600）；`cargo run -- model list` 五条全部 ✓ available，旧路径提示一行、grok 未知键 warning 一条。
- [x] ship 评审：插件里的 dev 二进制（feebc92，09-04 03:07）早于配置分离，读不了本机 09-04 形状配置（`model 'DeepSeek' has no [providers.DeepSeek]`），改用主 checkout dev HEAD 的 release 二进制；作业 `0da087094044`（DeepSeek + GLM）首跑两格失败（DeepSeek 150s 缺省超时、GLM 网关流式解码错），`REDTEAM_PER_TURN_SECS=900 --resume` 后 DeepSeek 产 3 条：11f694cd（medium，write_entry 解析失败静默覆写 → 已修：拒写）、6ddeea91（low，旧路径探测不认 XDG / 与 REDTEAM_HOME 矛盾 → 已修）、8fc07250（low，resume 用当前 scanners → 已知，followup）；GLM 格两次同一网关错误未参与。
- [ ] 主工作树 `git merge --no-ff feat/m21a-config`（回到 m21-review-engine 工作树执行）。

## 风险文件 / 回滚
- `src/config/mod.rs`（整体重写）、`src/config/secrets.rs`（整体重写）、`src/cli/{mod,incremental,tail}.rs`、`src/main.rs`、`src/model/mod.rs`、`src/project.rs`、
  `src/provider/backends.rs`、`src/domain/report.rs`（`RunMeta.test_command`，与批 B 可能小冲突）、`examples/dump_pack.rs`、`tests/review_e2e.rs`。
- 回滚 = 不合回；无数据与 schema 改动。
