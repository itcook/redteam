# implement · M21a 批 C

> 顺序执行，每 task 一 commit（简体中文 `type(scope): 摘要——细节`），绝不 push；`make build` 在 F-1 前禁跑。决策编号见 `prd.md` K1-K15，定论见 `design.md`。

## 0. 预飞
- [x] `git status --short` 空、`git branch --show-current` = `feat/m21a-grouping`。
- [x] 基线：`cargo test 2>&1 | tail -3`（记录通过数）、`cargo clippy --all-targets 2>&1 | grep -c warning`（记录基线数）、`make verify`。

## 1. C-1 import 边（`src/bundle/mod.rs`）
- [x] `ImportResolver`（root、改动键表、tsconfig / go.mod / crate 根惰性缓存）；`jsonc_to_json`；`TsConfig { base_url, paths }` + `extends` 合并（深 ≤ 5）。
- [x] `import_edges(path, content, &resolver) -> Vec<ImportEdge>`：TS 相对 + 裸模块；Rust `use crate / super / self`（`{}` 展开、`as`、`*`）+ `mod`；Go 单行 / 块；Python 相对沿用；删 `extract_imports`（唯一外部引用 `examples/dump_pack.rs` 于 C-2 删除，此处先改为内部委托保编译）。
- [x] 测试：`import_edges_ts_relative`（改自旧测）、`_ts_bare_via_paths_wildcard`、`_ts_base_url_only`、`_ts_extends_chain`、`_ts_jsonc_tolerant`、`_ts_unresolvable_dropped`；`_rust_use_crate_and_super`、`_rust_mod_rs_and_item_tail`、`_rust_workspace_sub_crate_root`；`_go_module_dir_single_and_block`、`_go_external_dropped`；`_never_escapes_root`（`../../../etc/x`、`paths` 值越 root）；`_python_relative`（旧语义）。
- [x] `cargo test bundle::` 绿 → commit `feat(bundle): import 边补 TS 裸模块 / Rust use crate / Go 包路径（followup 实现债）`。

## 2. C-2 引用图分组
- [x] 类型：`EdgeKind` / `Edge` / `Bundle { files, edges, hub_order, over_budget }` + `from_files` + `fill_order` / `GroupingOpts` + `Default` / `Gate` / `GroupingStats`（Serialize）/ `Grouping`。
- [x] `ref_worthy` + `GENERIC_NAMES`（移植 `eval/risk_replay.py:113-118, 223`）；`changed_def_names(path, diff, content)`（`changed_def_ranges` + `match_def`）；`count_plus_minus(diff)`（与清单计数同源，抽 helper 供 C-3 复用）。
- [x] `group(changes, root, &opts) -> Grouping`：边 → 门 → 分量 → 弱缝切 → `over_budget` → 落单 + 孤立装箱 → 组排序 → `hub_order` → `edges` 归属 → stats（design §4）。
- [x] 消费方：`src/cli/mod.rs` 删 `merge_bundles`，`run_code_review` 把 `select_files` 提前、`group` 下移到 `budget` 之后（`GroupingOpts { budget_bytes: Some(budget), ..Default }`）；`src/cli/incremental.rs:71` 同（预算同样在其后算出，需同步下移）；`second_pass.rs:206` 与 `pack/mod.rs`、`incremental.rs`、`cli/mod.rs` 各测试的 `Bundle { files }` → `Bundle::from_files`；`pack::body_section` 改 `for fc in bundle.fill_order()`；删 `merge_bundles_flattens_all_files_into_one` / `bundles_collapse_to_single_cell_by_default`；`git rm examples/dump_pack.rs`（K13）。
- [x] 测试（bundle）：`group_two_components`、`group_cuts_weak_seam_when_oversized`、`group_keeps_strong_component_marks_over_budget`、`group_bin_packs_isolated_by_dir`、`group_repacks_singletons_after_cut`、`group_gate_single_bundle_keeps_edges`（3 文件 / churn<200 两例）、`group_is_deterministic`、`hub_order_prefers_import_hubs`、`group_empty_input_is_empty`；旧 `group_*` 测试按新语义改写或删除（`group_splits_oversized_at_cap` → 改为强分量不切）。pack：`body_section_fills_hub_first`。cli：`run_code_review` 门未过仍单 cell（沿用 `incremental_review_injects_spec_reference` 断言）。
- [x] `cargo test` + `cargo clippy --all-targets` → commit `feat(bundle): 引用图分组——强弱边连通分量、弱缝先切、宁降不拆、孤立 bin-packing、ocr 双阈值门（design §3.2〔D3〕）`。

## 3. C-3 跨组索引
- [x] `IndexEntry` / `CrossIndex` / `cross_index(bundles, extra_paths, delta)` / `render_cross_index(&idx, for_group)`（三档填充 + 双帽 + 尾注，design §5）；删 `other_files_manifest`（`cap_entries` 保留）。
- [x] 消费链：`run_code_fanout` 加 `others: &[String]`；`second_pass_prompt` 加 `others: &str`；`run_code_review` 与 `run_incremental_review`（`extra_paths = branch_files ∖ delta`，`delta = Some(delta_files)`）预渲染；`two_layer_manifest` / `<incremental_round>` 不动（K9）。
- [x] 模板 `adversarial-code-bundle.md:80` 一句（design §5 文案，K10）；`build_bundle_prompt` 文档注释更新。
- [x] 测试：`cross_index_forward_backward_and_groups`、`render_cross_index_snapshot`（两组三文件逐字节）、`render_cross_index_connected_first`、`render_cross_index_degrades_then_truncates`（总长 ≤ 4000 且含 `see group` 与 `more files`）、`render_cross_index_delta_only_marks_delta_set`（子集）、`render_cross_index_extra_paths_have_no_edges_or_delta`、`render_cross_index_single_group_is_empty`；prompt：`bundle_prompt_neutralizes_cross_index_path`（路径含 `</bundle>`）；orchestrate / second_pass 既有测试改传 `others`。
- [x] `cargo test` → commit `feat(prompt): 正反向跨组索引进 {{OTHER_CHANGED_FILES}} 槽 + [delta] 标注 + omit 留指针（design §10.6〔D6〕）`。

## 4. C-4 `--pack-only`
- [x] `ReviewArgs.pack_only`（`#[serde(default)]`）；`pub fn gen_job_id` 外移到 `src/cli/mod.rs`（main 改调）；`PackOnlyReport` / `PackCell`（Serialize）/ `run_pack_only` / `render_pack_only`（design §6）。
- [x] `src/main.rs`：clap flag（`conflicts_with_all = ["background", "resume", "incremental_from"]`，doc 注释写明 `--format` 无效、落盘目录、÷3.5 估算）；`args` 构造后分支；`Cmd::Review` 顶部 doc 提一句。
- [x] `eval/README.md:23`、`eval/e3_contract_ab.py:4` 顶注：`examples/dump_pack.rs` 已随 M21a 批 C 拆除，等价产物 = `redteam review --pack-only`（`.redteam/packs/<job>/cell-*.md`）。
- [x] 测试：`tests/review_e2e.rs::pack_only_writes_packs_and_reports_bytes`（tempdir git 仓两文件、`Config::default()`；断言退出报告 cells=1、`render_pack_only` 含 `cell-1 files=2 bytes=` / `est_tokens=` / `grouping gate=single`、`cell-1.md` 与 `summary.json` 存在且 `serde_json` 可解析、`est_tokens == round(bytes/3.5)`）；`pack_only_empty_changes_reports_zero_cells`；`src/main.rs` 测试 `pack_only_conflicts_with_background`（`Cli::try_parse_from` Err）与 `--help` 含 `--pack-only`。
- [x] `cargo test` → commit `feat(cli): --pack-only 预算仪表——只装包、按 cell 报字节与 ÷3.5 估算，不调 provider（D7 B7）`。

## 5. C-5 收口
- [x] 本仓真实 smoke（不提交产物）：`cargo run -q -- review --pack-only --base feat/m21-review-engine`，核对 stdout 三段与 `.redteam/packs/<id>/`。
- [x] `cargo test`、`cargo clippy --all-targets`（≤ 基线）、`make verify`。
- [x] `.next/roadmap/followup.md` 追加：① 增量轮两份清单（跨组索引 + `<incremental_round>` 双层清单）待 D-3 合并；② `--pack-only` 预算 `cfg.models` → 角色条目（A 合回后）；③ `GroupingStats` → RunMeta（F-5）。commit `docs(followup): 批 C 延后项——增量轮清单合并 / 预算改角色条目 / GroupingStats 接线`。
- [ ] （用户）dev `redteam` 跑 ship 评审 → 修 → `--no-ff` 合回 `feat/m21-review-engine`。

## 6. 评审修与基线整合（2026-09-08）
- [x] `/code-review high` 十条 findings 全修 + 补测试（a3d2908）；技能目标写法注意：传分支名 = 评审「该分支相对 dev」，评本分支须让它取「当前分支 vs 基线」范围。
- [x] `git merge feat/m21-review-engine`（含批 A）：6 处冲突已解，预算改读 `cfg.active_roles()`（00081cc）；lib 475 测试全绿、clippy 7（仅 A 侧遗留 1 条）、`make verify` 绿。

## 验证命令
```
cargo test
cargo clippy --all-targets
make verify
cargo run -q -- review --pack-only --base feat/m21-review-engine
git log --oneline feat/m21-review-engine..HEAD
```

## 风险文件 / 回滚点
- `src/cli/mod.rs`、`src/main.rs`：与批 A 碰撞面，新增代码保持独立函数 + 末尾字段。
- `src/pack/mod.rs`：只改遍历序，E-2 后续重写。
- 模板 `:80`：只改一句，E11 注入回归随 `cargo test`。
- 回滚 = 逆序 revert 五个 commit；应急开关见 `design.md` §9。
