# implement · 批 A 重做（feat/m21a-config-json）

三道门每步：`cargo test`、`cargo clippy --all-targets`、`make verify`。步序按编译可独立通过划分；R-1 … R-4 合一笔代码提交（形状牵动全部消费方），R-5 一笔。

## R-1 config.json 解析与路径
- [x] `src/config/mod.rs` 重写：camelCase 三层类型、`RoleAttrs`、`EffectiveModel{name=provider/id, display}`、`active_roles()`、`model()` / `effective()`、
  `validate()`（kind 两层、`contextWindow ≥ 32_000`、roles 空 warning）、`legacy_shape_guard`（JSON 旧形状）、`redteam_home() -> Result`（HOME 缺失报错）、
  `load()`（NotFound 空 / 其他 io 错误传播 / 解析错误带行列）、`legacy_config_paths_present()`（四类 + 纯注释过滤）。
- [x] 测试：§12.1 样例；活跃集顺序；kind 解析 / 缺失；contextWindow 下限；旧形状指引三种；HOME 缺失报错；目录当文件读 → Err；旧路径四类 + 纯注释不提示 + 当前路径不提示。

## R-2 secrets.json
- [x] `src/config/secrets.rs` 重写：`Secrets::parse`（Value 手走）、`load_secrets`（NotFound 空）、`write_secret`（解析失败拒写、chmod 600）、`resolve_key(provider,id)` 四级链、
  `resolve_provider_key` 去掉任一 model key 回退、`secret_key_warnings` 未知 provider / model。
- [x] 测试：解析链四级；未知 provider / model warning；拒写保留原文；chmod 600；类型错误 warning 不整档失败。

## R-3 消费方与 setup / list
- [x] `src/main.rs`：`ModelCmd::Setup` 新参数（`--role` 逗号列表、`--api-key-env`、`--max-concurrency`，删 TOML 时代文案）；`config::write_setup(SetupPatch)` 三层显式合并 + 校验拒写；
  落盘后探测走 `resolve_key(provider,id)`；`build_registry` 按 `provider/id`；`per_turn_secs(em)` 四级（env 在公式前）；`--resume` 帮助文案；`--help` 文案指 JSON。
- [x] `src/model/mod.rs`：`check_all` / `render_model_list` 按 `active_roles()` 行格式 `[role] provider/id (display) [kind] status`；测试夹具改 JSON。
- [x] `src/cli/{mod,incremental,tail}.rs`、`tests/review_e2e.rs`、`examples/dump_pack.rs`：夹具改 JSON（`Config::from_json(&str)`），逻辑不变。
- [x] `src/project.rs`：`infer_test_command` 的 `test:=` / `test :` / npm 占位脚本；旧模板纯注释判定供路径提示复用。
- [x] `Cargo.toml` 删 `toml`；`grep -rn 'toml' src` 只剩注释。
- [x] 测试：setup 三层显式合并保留手写字段；已存在 provider 不被改 kind / apiKeyEnv；校验失败拒写文件不变；`--model-key` 无 secrets 报错；per_turn 四级；infer 边角。

## R-4 门与提交
- [x] `cargo test`（455 lib + 15 bin + 7 packaging + 12 e2e 绿）/ `cargo clippy --all-targets`（2 lib + 1 bin 均既有）/ `make verify` 绿。
- [x] Commit 9b817da `feat(config): 批 A 重做——config.json + provider→models→roles（D12），setup 三层显式合并 + 写前校验，HOME 缺失报错，超时优先级，contextWindow 下限；toml 退出`

## R-4b 重做评审（子 agent 人工评审 9b817da）+ 身份串裁定
- [x] 用户裁定：身份串固定 `provider/id`，`name` 改 `displayName` 只作显示（D12 定案 3 原文即如此，首版实现偏宽）。
- [x] 评审并入：必修 #1（env 提示指向不存在变量）；建议 #2 #3 #4 #5 #6 #7；nit #8 #9 #11 #12 #13；#10（键序）记 followup。
- [x] 旧路径 notice 只在 config.json 尚不存在时打印。
- [x] 三道门绿；commit `fix(config): 身份串固定 provider/id（name→displayName）+ 重做评审修复…`。

## R-5 收口
- [x] 本机：`/tmp/rt-migrate-config-json.py` 生成 `~/.redteam/config.json` + `secrets.json`（TOML 改 `.bak`）；`cargo run -- model list` 五条 ✓（`name` 已手改 `displayName`）。
- [x] `.next/roadmap/followup.md`：未知字段（已做 warning）、首版 TOML 迁移脚本仅本机一次性、ship.md 第 28 行旧路径引用、键序观感。
- [x] ship 评审按用户 2026-09-08 裁定不跑（核心能力稳定前无需/不能跑），改为 code-review 子 agent 人工评审（R-4b）。
- [x] 先把 `feat/m21-review-engine`（已含批 B/C）合进本分支解冲突（1f2f89c，三处机械冲突），合并树 493+16+7+14 绿；顺手接批 C followup 的 `review.grouping → GroupingOpts`。
- [ ] 主工作树 `git merge --no-ff feat/m21a-config-json`（会话退出本工作树后执行）。

## 风险文件 / 回滚
- 重写：`src/config/mod.rs`、`src/config/secrets.rs`；改：`src/main.rs`、`src/model/mod.rs`、`src/project.rs`、`Cargo.toml`、各测试夹具。
- 不碰：`src/domain/`、`src/bundle/`、`src/pack/`、`src/prompt/`（批 B / C / D / E 领地）。
- 回滚 = 不合回。
