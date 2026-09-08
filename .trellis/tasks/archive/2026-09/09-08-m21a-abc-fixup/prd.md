# prd · M21a A/B/C 合并后修补（开 D / E 前）

## Goal
三批（A 配置节 / B schema v4 / C 分组与索引）合回 `feat/m21-review-engine` 后，三路只读核验（2026-09-08）发现两条 bug 级问题、一处
测试缺口与几条一行级边角。开 D（提示词）/ E（切片）前一次修掉，避免 D 重写 prompt 渲染时把跨组索引接错而全绿、E 消费 `anchorWindow`
时吃到零值。用户价值：D / E 建立在正确的分组、索引与守卫之上；`--dismissed` 误传业务库不被改写。

## Background（核验事实，含 file:line）
- B 守卫：`src/db.rs:216-221` 陌生表检查只在 `version < USER_VERSION` 分支内；`user_version == 4` 的陌生库走 `setup_schema` 末尾
  `CREATE TABLE IF NOT EXISTS`（`:241`）并切 WAL（`:163`）。
- C 伪引用边：`src/bundle/mod.rs:1258-1263` 引用计数只排除 `i != j`；文件 j 自身定义同名 changed def 时，其定义行与内部调用被算作对
  其他 owner 的引用（≥ 2 次即强边）。临时仓复现：两个互不引用文件因同名 `validate_input` 粘成一组（期望 `edges=0 isolated=4`，实测 `edges=2`）。
- C super 假边：`resolve_rust_path` 的 `allow_root`（`src/bundle/mod.rs:1046-1052`）对顶层模块 `src/a.rs` 内的 `mod tests { use super::*; }`
  按文件顶层算，产生 `a.rs → lib.rs` Import 边。
- C 测试缺口：跨组索引接线（`run_code_fanout` → `build_bundle_prompt` 的 `others`）无任何 gate=grouped 的集成测试；
  `pack_only_writes_packs_and_reports_bytes` 是 gate=single。
- A 零值：`validate()`（`src/config/mod.rs:659-700`）对 `review.*` 只校验 workers / samples / maxConcurrency / confFloor；
  `pack.anchorWindow` / `reviewers.maxCells`（Fixed）/ `grouping.maxFilesPerGroup` / `grouping.minFiles` 为 0 直通。
- A 空串 key：`resolve_key_with`（`src/config/mod.rs:663-682`）与 `resolve_provider_key` 对 `export X_API_KEY=` 返回 `Some("")`。
- C 契约漂移：`--pack-only` 经 `load_config_or_exit` → `load_with_messages` → `secrets::load_secrets()` 真读 `secrets.json`（prd R-C4.1 /
  design §6 写「不读 secrets」）。
- B 待核：只读快路径（`src/db.rs:149-153`）之后仍执行 `PRAGMA journal_mode=WAL`（`:163`）；delete 模式的只读 v4 库在 sqlite 层报
  readonly（python 复现），rusqlite 侧未验。

## Requirements
- R1（B）陌生表检查移到版本分支之外：任何非空库含非 `TABLES` 表 → 拒开、不改写（含 `version == 4`）。
- R2（C）引用边：j 自身拥有该 changed def 名（j ∈ owners[name]）时不计 j → i；同名跨文件不再互成强边。
- R3（C）Rust `super`：顶层模块（父目录即 crate 根 `src/`）内嵌套 `mod` 块里的 `super::` 指向文件自身，不成边；`src/lib.rs` / `main.rs`
  的 `mod x;` 边不变。
- R4（C）多组索引集成测试：lib 级 e2e（≥ 4 文件、churn ≥ 200、两组），断言每个 cell 的 prompt 索引只列他组文件、带箭头；
  真二进制 `--pack-only` 退出码 0 用例挂到 `clap_parse_error_exits_1_help_exits_0` 同形态。
- R5（A）`validate()` 增：`review.pack.anchorWindow ≥ 1`、`review.grouping.minFiles ≥ 1`、`review.grouping.maxFilesPerGroup ≥ 1`、
  `review.reviewers.maxCells` 为整数时 ≥ 1（`"auto"` 不受影响）；`sliceMinBytes = 0` 合法（全部切片）。
- R6（A）key 解析链四级与 `resolve_provider_key`：env 值去空白后为空视为未设，继续下一级。
- R7（C）`--pack-only` 不读 `secrets.json`：`Config::load_no_secrets()`（同 `load_with_messages` 但跳过 secrets 读取与守卫）供 pack-only 路径；
  design §6 / prd R-C4.1 措辞保持。
- R8（B）只读快路径：`journal_mode=WAL` 只在可写打开时执行；只读打开遇 pragma 失败降级 warning 不拒开；用 rusqlite 测试覆盖
  delete 模式只读 v4 库可开。
- R9 其余核验项（`cross_edges` / `weak_cut` 口径、lib.rs 超级枢纽回放、TS 副作用导入、同顶层模块弱边、`est_tokens` 不含 system 段、
  `ScanOutput.suspicions` 无 default、`over_budget` 未被 pack 消费、`CellOutput` tag 口径、setup 预检文案、并存期 secrets 提示）记
  `.next/roadmap/followup.md`，本任务不改。

## Acceptance Criteria
- AC1：新测试 `foreign_v4_versioned_db_is_refused_untouched`：建 `users` 表 + `user_version=4` → `open_at` Err、文件字节不变、journal 模式不变。
- AC2：新测试：两文件各定义同名 changed def、互不引用 → `edges` 无 Reference 边；已有引用边测试（`group_unions_by_import_across_dirs` 等）不变。
- AC3：新测试：`edges("src/a.rs", "mod tests {\n use super::*;\n}\n", ["src/a.rs","src/lib.rs"])` 为空；嵌套模块 `src/a/b.rs` 的 `super::` 仍解析到
  `src/a/mod.rs` 或 `src/a.rs`（既有 `_rust_use_crate_super_self_and_external_dropped` 不变）。
- AC4：新 e2e：gate=grouped 两组，cell-1 的 prompt 含 cell-2 文件路径与 `→` / `←` 箭头且不含自身文件；真二进制 `--pack-only` exit 0。
- AC5：`Config::from_json` 对 `anchorWindow:0` / `minFiles:0` / `maxFilesPerGroup:0` / `maxCells:0` 各报一条含键名的错误；`maxCells:"auto"`、
  `sliceMinBytes:0` 通过。
- AC6：`REDTEAM_TEST_KEY=""` 时 `resolve_key_with` 落到下一级（测试扩 `key_resolution_chain_four_levels`）。
- AC7：`run_pack_only` 路径不调用 `load_secrets`（用 `REDTEAM_HOME` 指向含非法 `secrets.json` 的目录跑 e2e：pack-only 不产 secrets warning）。
- AC8：新测试：delete 模式 v4 库 chmod 444 → `open_at` Ok（只读）；可写打开仍切 WAL（既有 `readonly_v4_db_opens_without_writing` 不变）。
- AC9：三道门：`cargo test` 全绿、`cargo clippy --all-targets` 零新增、`make verify` 绿；followup 新增行落库。

## Out of Scope
- D / E / F 的任何功能；`cross_edges` 分种计数（F-5）；`two_layer_manifest` 合并（D-3）；`over_budget` 消费（E-2）。
- `ScanOutput.suspicions` 加 default（D-1 parse 层决定）。
