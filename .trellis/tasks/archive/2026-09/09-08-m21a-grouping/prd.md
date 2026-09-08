# M21a 批 C：引用图分组 + 跨组索引 + `--pack-only`

> 上位：`.next/plans/2026-09-07-redteam-m21a-scan-foundation-plan.md` 批 C（C-1…C-5）+ Global Constraints；design v8 §3.2 / §10.6 / §12.1；决策 D3 / D6 / D7 B6-B7；roadmap `M21a-scan-foundation.md` 交付项 2 / 3 / 8。
> 技术设计 `design.md`，执行清单 `implement.md`；本文 §Key Decisions 的 K 编号被两者引用。
> 分支 `feat/m21a-grouping`（自 `feat/m21-review-engine` c382e54 切出，工作树 = 本目录），合回目标 `feat/m21-review-engine`（`--no-ff`，用户操作）。

## Goal

把快扫的分组从「同目录 / 同词干 / 相对 import 并包 → 恒并单包（E9）」改成 design §3.2 的确定性引用图分组，把边集提升为流水线产物、渲染成正反向跨组索引进 `{{OTHER_CHANGED_FILES}}` 槽，并提供零 LLM 的 `--pack-only` 预算仪表。用户价值：

- 分组决定 scanner cell 的划分与并行度（F-2 `cell = 组 × 条目 × samples`）；分错退化为冗余而非盲区（半径 + reduce dedup），但组数与跨组边数直接决定墙钟与 token。
- 跨组索引让每个 cell 以零 token 成本知道视野外改了什么、谁引用谁，压「无证据实现」类跨组误报。
- `--pack-only` 让预算与真实 input 可对账（D7 B6 ÷3.5、R20 40 PR 分布 spike），也是 `examples/dump_pack.rs` 的产品化替代。

## Background（仓库实测 2026-09-08，HEAD c382e54）

### 现状代码
- 分组 `bundle::group`（`src/bundle/mod.rs:296-347`）：同父目录 ∪ 同词干（排 `GENERIC_STEMS`）∪ 相对 import → union-find → `BUNDLE_MAX_FILES = 8` 切块；`Bundle { files }` 只有文件。
- `extract_imports`（`src/bundle/mod.rs:240-292`）只认相对引用：TS `from './x'` / `require('./x')`、Python `from .x`、Rust `mod x;`；返回去扩展名键，`norm_rel` 处理 `./` `../`。
- `merge_bundles`（`src/cli/mod.rs:200-205`）恒并单包，调用点 `run_code_review`（`:254`）与 `run_incremental_review`（`src/cli/incremental.rs:71`）；测试 `merge_bundles_flattens_all_files_into_one` / `bundles_collapse_to_single_cell_by_default`（`src/cli/incremental.rs:611-636`）；`incremental_review_injects_spec_reference`（`:586`）断言单文件 delta 只一次 provider 调用（门未过仍单包，语义保持）。
- 清单 `other_files_manifest(bundles, exclude)`（`src/bundle/mod.rs:57-84`）= `path (+a/-d)` 逗号串，双帽 `MANIFEST_MAX_ENTRIES=200` / `MANIFEST_MAX_CHARS=4000` / `MANIFEST_SUFFIX_RESERVE=40`（`cap_entries`）；消费点 `orchestrate::run_code_fanout`（`src/orchestrate/mod.rs:285`）与 `second_pass_prompt`（`src/cli/second_pass.rs:210`）。`two_layer_manifest`（`:113-121`）只在增量路径（`src/cli/incremental.rs:77-85`）给 `<incremental_round>` 段渲染全分支文件 + `[delta]`（`src/prompt/mod.rs:214-252`，含 E11-Q5 红测测试 `:596-680`）。
- prompt：`build_bundle_prompt` 对 `other_files` 过 `neutralize_structure`（`src/prompt/mod.rs:138`）后填 `{{OTHER_CHANGED_FILES}}`（`:146`，空 → "(none — this bundle covers the entire changeset)"）；模板槽在 `src/prompt/templates/adversarial-code-bundle.md:79`，其后 `:80` 一句硬断言 "this bundle covers the entire changeset"（单包前提）。`DOSSIER_STRUCTURAL_TAGS`（`:303`）六标签。
- pack：`build_pack(bundle, root, budget, own_diff_bytes)`（`src/pack/mod.rs:160`）；`body_section`（`:19`）按 `bundle.files` 顺序填全文，超预算逐文件回落 def 体 → 骨架 → hunk 窗 → omit。填充顺序 = 遍历序，故「枢纽先填」只需控制遍历序。
- diffsym：`changed_def_ranges(diff, lines, defs) -> Vec<(start,end)>`（`src/diffsym/mod.rs:203`）、`match_def(line, defs) -> Option<(kind,name)>`（`:67`）、`lang_for` 仅 rs / go / ts（`:58`）。
- 预算：`pack_budget_bytes(cfg, model_names)`（`src/cli/mod.rs:183`）= min(768_000, (最小窗 − 16_000)×3 − 24_000)；无模型 / 未声明窗 → 312_000。
- CLI：`ReviewArgs`（`src/cli/mod.rs:28-56`）；clap `Cmd::Review`（`src/main.rs:34-91`）；前台 `build_registry`（`:389`，无 model 即 Err）→ `run_review_dispatch`（`src/cli/mod.rs:334`：增量路由 → `changed_files` → `run_code_review`）；`gen_job_id`（`src/main.rs:295`，private，pid+secs+cwd fnv 前 12 位）；`.redteam/.gitignore` = `*`（`src/project.rs:90`）。
- 证据：`changed_files(dir, base, branch_scope)`（`src/evidence/git.rs:42`）、`branch_files`（`:179`）；`FileChange { path, diff }`。
- 配置：`ReviewConfig`（`src/config/mod.rs:134-145`）无 grouping / pack 子表；design §12.1 的 `[review.grouping] min_files=4 / bundle_line_threshold=200 / max_files_per_group=10 / semantic=false` 由批 A-2 定义。
- `examples/dump_pack.rs`：`Bundle { files }` 字面量 + 照抄 `merge_bundles`，随 `cargo test` 编译；followup「`--pack-only` 落地即删」；`eval/README.md:23`、`eval/e3_contract_ab.py:4` 文档级引用。
- 名字过滤口径 `fanin_worthy` + `GENERIC_NAMES` 只在 `eval/risk_replay.py:113-118, 223`（Python），Rust 侧无。
- 依赖：`serde_json`（tsconfig）、`regex`（词边界引用搜索）、`tempfile`（dev）已在 `Cargo.toml`；不加新依赖。
- Trellis：本任务 `09-08-m21a-grouping`（P1），branch `feat/m21a-grouping`，base `feat/m21-review-engine`。

### 波次碰撞（plan 波 1，三批并行）
- 批 A 改 `src/cli/mod.rs`（删 `resolve_models` / `ReviewArgs.models*`、`pack_budget_bytes` 改读角色条目）、`src/config/mod.rs`（加 `[review.grouping]`）、`src/main.rs`（clap 删两 flag）；批 B 改 `src/state` / `src/db` / `src/domain` / `report.rs`。
- 本批改动面：`src/bundle/mod.rs`、`src/cli/mod.rs`（`--pack-only`、删 `merge_bundles`、`gen_job_id`）、`src/cli/incremental.rs`、`src/cli/second_pass.rs`、`src/orchestrate/mod.rs`（一处调用）、`src/prompt/mod.rs` + 模板一句、`src/pack/mod.rs`（遍历序 3 行）、`src/main.rs`（clap 一 flag + `gen_job_id` 外移）、`examples/dump_pack.rs`（删）、`eval/README.md` / `eval/e3_contract_ab.py`（注记）、`tests/review_e2e.rs`、`.next/roadmap/followup.md`。不碰 config / state / db / domain / report。

## Requirements

### R-C1 import 边三语言补齐（plan C-1）
- R-C1.1 `bundle::import_edges(path, content, &ImportResolver) -> Vec<ImportEdge { to: String }>`（仓内相对路径、带扩展名）替代 `extract_imports` 的去扩展名键；只对目标 ∈ 改动集成边。
- R-C1.2 TS/JS：保留相对解析；裸模块按最近的 `tsconfig.json`（文件目录向上到 root）`compilerOptions.baseUrl` / `paths`（单 `*` 通配）解析，`extends` 链（相对路径，深度 ≤ 5）合并；JSONC 容错（`//`、`/* */`、尾逗号）；候选 `<映射>.{ts,tsx,mts,cts,js,jsx,mjs,cjs}` 与 `<映射>/index.*`；解析不到丢。
- R-C1.3 Rust：`use crate::a::b` / `use super::x` / `use self::x`（含 `pub use`、`{}` 组、`as`、`*`）+ 既有 `mod x;`；crate 根 = 文件最近祖先 `src/` 且含 `lib.rs` 或 `main.rs`（workspace 多 crate 各自为根），退化为最近 `src/`；模块路径 → `src/a/b.rs` | `src/a/b/mod.rs`，尾段是 item 时按最长存在前缀回退（`crate::a::b::Item` → `src/a/b.rs` 或 `src/a.rs`）。
- R-C1.4 Go：`import "…"` 单行与块形式；最近 `go.mod` 的 `module` 行 → 包路径映射到目录；该目录下任一改动 `.go` 文件即成边；标准库 / 外部模块不成边。
- R-C1.5 路径中和：映射结果规范化（`.` / `..`）后不得越出 root（越出 → 丢）；除 tsconfig / go.mod（root 内）外不读改动集外文件。
- R-C1.6 Python 相对 import 行为不回退（现有测试 `extract_imports_python_relative` 语义保留）。

### R-C2 引用图分组（plan C-2，design §3.2〔D3〕）
- R-C2.1 `bundle::group(changes, root, &GroupingOpts) -> Grouping { bundles, stats }`；`GroupingOpts { min_files: 4, bundle_line_threshold: 200, max_files_per_group: 10, budget_bytes: Option<usize> }` 带 `Default`（K1）。
- R-C2.2 边：import 边（强）；引用边 = A 的改动 def 名（`changed_def_ranges` 起行经 `match_def` 取名，`ref_worthy` 过滤，K4）以词边界出现在 B 的 hunk 或全文，按次数计（≥ 2 强、= 1 弱）；同目录边（弱）。三类都进 `Bundle.edges` 与统计；改动文件 > 200 时引用边只扫 hunk。
- R-C2.3 门：`files < min_files || churn < bundle_line_threshold` → 单包（`stats.gate = Single`），边仍计算保留（索引用）。churn = Σ 改动行（`+` / `-`，排除 `+++ ` / `--- ` 头），与清单计数同源（K2）。
- R-C2.4 连通分量 = 初始组；分量 `files > max_files_per_group` 或估算字节 > `budget_bytes`（Σ 文件全文 + diff，K5）→ 切掉该分量全部弱边重算（K3）；仍超 → 整组保留、`over_budget = true`（宁降不拆，交 pack 梯子）；`stats.weak_cut` 记被切弱边数。
- R-C2.5 组内填充序 `hub_order`：按「与组内其他文件的 import 边数（出 + 入）」降序、同分按路径；`Bundle.files` 保持路径升序（cell label 稳定），`pack::body_section` 按 `hub_order` 遍历（枢纽保全文，K7）。
- R-C2.6 孤立文件（原图无边）与切弱边后落单的文件同池，按目录排序后贪心装箱：箱 ≤ `max_files_per_group` 且累计估算字节 ≤ `budget_bytes`；单文件超预算独占并标 `over_budget`；`stats.isolated` 记原图孤立文件数（K6）。
- R-C2.7 输出确定性：相同输入 → 相同组序（组按首文件路径升序）、相同 `hub_order`、相同 stats。
- R-C2.8 `merge_bundles` 删除；`run_code_review` 与 `run_incremental_review` 直接用 `group(...).bundles`（分组下移到 pack 预算算出之后，预算喂 `budget_bytes`）；空输入仍返回空（上游两处 `bundles.is_empty()` 空证据守卫语义不变）。
- R-C2.9 `Bundle { files, edges: Vec<Edge>, hub_order: Vec<usize>, over_budget: bool }`，`edges` 含与本组文件相关的全部边（组内 + 跨组，端点用路径）；`Bundle::from_files(files)`（无边、自然序、不超预算）供测试与 second_pass 增强包使用。
- R-C2.10 `examples/dump_pack.rs` 在本 task 删除（`Bundle` 形状变更后无法编译，且 followup 已定「`--pack-only` 落地即删」，K13）。

### R-C3 正反向跨组索引 + `{{OTHER_CHANGED_FILES}}` 升级（plan C-3，design §10.6〔D6〕）
- R-C3.1 `bundle::cross_index(bundles, extra_paths, delta) -> CrossIndex`：对每个改动文件记 group 号、`(+a/-d)`、forward（它 import / 引用的改动文件）、backward（import / 引用它的改动文件）；`extra_paths` = 有路径无 diff 的文件（增量轮的全分支非 delta 文件），只有路径、group 为空；同目录边不进索引。
- R-C3.2 `render_cross_index(&idx, for_group) -> String` 替代 `other_files_manifest`：每行 `path (gN +a/-d) → [p1, p2] ← [p3] [delta]`（空的 `→` / `←` 段省略）；排序：与 `for_group` 有边的文件在前，其次按 group 号、路径；双帽（条目 / 字符）沿用，帽内先降质（超出部分渲染为 `path → see group N`，不带边与计数），仍超 → 尾注 `… and K more files (see groups a, b)`；总输出含尾注 ≤ `MANIFEST_MAX_CHARS`。
- R-C3.3 `[delta]` 只标 `delta` 集内文件；全量路径 `delta = None` 不标；单组且无 `extra_paths` 时渲染空串（保持既有 "(none — …)" 占位）。
- R-C3.4 索引串由调用方一次渲染为 `others: Vec<String>`（与 `dossiers` 等长等序）传入 `run_code_fanout` 与 `second_pass_prompt`，在 `build_bundle_prompt` 内继续过 `neutralize_structure`（路径含 `</bundle>` 被中和）。
- R-C3.5 增量轮：`run_incremental_review` 把 `branch_files ∖ delta` 作 `extra_paths`、delta 集作 `delta` 喂索引；`two_layer_manifest` 与 `<incremental_round>` 段本批不动（K9）。
- R-C3.6 模板 `adversarial-code-bundle.md:80` 一句改为组感知措辞（K10），其余模板字节不动。

### R-C4 `--pack-only` 预算仪表（plan C-4，D7 B6 / B7）
- R-C4.1 `redteam review --pack-only [--base …] [--scope …] [--spec …] [--focus …]`：证据收集 → 门 / 分组 → 装包 → 按 cell 组 prompt（现行 `build_bundle_prompt` 路径，与真实评审同源）→ 落盘 `.redteam/packs/<job_id>/cell-<n>.md`（完整 user prompt）与 `summary.json`；不建 registry、不读 secrets、不调 provider；退出码 0；无任何 provider / model 配置也能跑（预算按 312_000 缺省，K15）。
- R-C4.2 stdout 每 cell 一行：`cell-<n> files=<k> bytes=<prompt 字节> est_tokens=<bytes÷3.5 取整> pack_bytes=<dossier 字节> diff_bytes=<…> over_budget=<bool>`；随后一行 `grouping gate=<single|grouped> groups= edges= weak_cut= cross_edges= isolated=`；最后一行 `packs=<目录>`（K12）。空改动集 → `cells=0` + 一行 warning，退出 0。
- R-C4.3 `--pack-only` 与 `--background` / `--resume` / `--incremental-from` 互斥（clap `conflicts_with`，退出码 1）；`--format` 对本模式无效（`--help` 写明，K11）。
- R-C4.4 `--help` 描述该 flag（裸终端在场文档）。
- R-C4.5 `eval/README.md` 与 `eval/e3_contract_ab.py` 顶注改指 `--pack-only`（K13）。
- R-C4.6 `gen_job_id` 从 `src/main.rs` 外移为 `cli::gen_job_id`（pub），主程序与 `--pack-only` 共用。

### R-C5 收口（plan C-5）
- R-C5.1 `GroupingStats` 只在 `--pack-only` 打印与 `summary.json`（RunMeta 接线 F-5，避免撞批 B 的 `report.rs`）。
- R-C5.2 每 task 一 commit（简体中文 `type(scope): 摘要——细节`，文案按 plan C-1…C-4），绝不 push；批收口由用户以 dev `redteam` 跑 ship 评审 → 修 → `--no-ff` 合回。
- R-C5.3 新发现的问题记 `.next/roadmap/followup.md`，不顺手修（Critical 除外）。

## Technical Notes
见 `design.md`：模块边界与改动面、数据结构、三语言解析器、分组算法伪码、索引渲染规则、`--pack-only` 路径、与 A / B / D / E / F 的合并策略、回滚。

## Acceptance Criteria
- [x] AC1（R-C1）三语言各一临时仓单测：TS 相对 / `baseUrl` / `paths` 通配 / `extends` / JSONC；Rust `use crate::` / `super::` / `mod`（含 `mod.rs` 与 item 尾段回退）；Go `go.mod` 映射（单行 + 块）；解析不到不成边；`../../..` 逃逸不越 root；Python 相对 import 既有测试不变。
- [x] AC2（R-C2）合成图单测：两连通分量 → 两组；弱缝切（同目录 + 单次引用）后分量降到限内；强分量超 `max_files_per_group` 不切且 `over_budget=true`；孤立文件按目录装箱；门未过（3 文件或 churn < 200）→ 单包但 `edges` 非空；确定性（两次调用输出相等）。
- [x] AC3（R-C2.5）枢纽序：预算只够一文件全文时 `hub_order[0]` 文件得全文、其余回落。
- [x] AC4（R-C3）渲染快照测试：行格式、有边文件优先、降质行 `see group N`、尾注计数、总长 ≤ 4000；`[delta]` 只标 delta；路径 `</bundle>` 在最终 prompt 中被中和；单组空串；增量轮 `extra_paths` 无 `[delta]`。
- [x] AC5（R-C4）lib 级 e2e（临时 git 仓、`Config::default()`、无 provider）：`--pack-only` 退出 0，输出含 `cell-1 files=` `bytes=` `est_tokens=`，`.redteam/packs/<id>/cell-1.md` 与 `summary.json` 存在且可解析；clap 测试 `--pack-only --background` 报错退出 1；`--help` 含 `--pack-only`。
- [x] AC6 `merge_bundles` 及其两测试删除；`incremental_review_injects_spec_reference` 等既有 e2e 全绿；`cargo test` 全绿、`cargo clippy --all-targets` 零新增、`make verify` 绿。
- [x] AC7 `examples/dump_pack.rs` 不存在；`eval/README.md` 不再指向它。
- [x] AC8 `--pack-only` 输出含 `grouping gate= groups= edges= weak_cut= cross_edges= isolated=`。
- [x] AC9 四个功能 commit（C-1…C-4）+ 收口 commit，无 push；工作树只改 §Background「波次碰撞」列出的文件。

## Key Decisions（本 PRD 内定案；plan 未明写处）
- K1 `[review.grouping]` 归批 A：本批不改 `src/config/mod.rs`，`GroupingOpts::default()` 内置 4 / 200 / 10；cli 只填 `budget_bytes = pack 预算`；`cfg.review.grouping → GroupingOpts` 接线由后合者一行完成。
- K2 门 churn 口径 = Σ(+ / −) 行（排 diff 头），与清单计数同源。
- K3 弱边 = 同目录边 + 单次引用边；强边 = import 边 + ≥ 2 次引用边；切弱边一次性整分量切，不做逐边最小割。
- K4 引用边名字过滤移植 `fanin_worthy`（≥ 4 字符、非 `GENERIC_NAMES`、纯小写字母且 < 8 字符排除）为 Rust `ref_worthy`。
- K5 超预算估算 = Σ(文件全文字节 + diff 字节) 对比 `budget_bytes`（cli 传 `pack_budget_bytes` 值）；`None` 不按预算判。
- K6 切弱边后落单（分量大小 1）的文件与原图孤立文件同池装箱，防「15 个同目录文件切成 15 个 cell」。
- K7 `Bundle.files` 路径升序（label 稳定），`hub_order` 为填充序；`pack::body_section` 3 行改按 `hub_order` 遍历（E-2 重写同函数时沿用）。
- K9 增量轮：跨组索引覆盖 delta 组 + `extra_paths`（全分支非 delta 文件，无边）并标 `[delta]`；`two_layer_manifest` 与 `<incremental_round>` 段本批不动（其 E11-Q5 红测测试群不拆）；两份清单的合并留给 D-3 段序翻转（记 followup）。备选（被否）：本批删 `two_layer_manifest`——拆 prompt 段与测试群，超出 C 范围。
- K10 旧模板 `:80` 一句改组感知措辞（单组时槽仍渲染 "(none — this bundle covers the entire changeset)"）；C-2 后旧 fan-out 即多组，原句成谎言；D-2 拆模板时整段重写。
- K11 `--pack-only` 与 `--background` / `--resume` / `--incremental-from` 互斥；不支持增量轮预算仪表（需要时 M21b 再议）。
- K12 `--pack-only` stdout 为固定文本行 + 目录内 `summary.json`（机读，R20 spike 用）；不按 `--format` 切换。
- K13 `examples/dump_pack.rs` 在 C-2 删除（编译门），`eval/` 两处引用在 C-4 改为历史注记。
- K14 `ImportEdge.to` 用 `String`（仓内相对路径）而非 plan 字面的 `PathBuf`，与 `FileChange.path` 同型。
- K15 `--pack-only` 预算用「全部已配置模型的最小窗」（`cfg.models` 键；A 合回后改读角色条目，一行）。→ 已偿（merge 00081cc）：三条路径统一走 `budget_and_spec(&cfg.active_roles(), …)`。

## Out of Scope
- `[review.grouping]` / `[review.pack]` 配置解析（A-2）；schema v4 / CellRecord（B）；模板拆分与段序（D）；切片装包（E）；RunMeta 接线与 scanner 并行（F）。
- LLM 语义分组（`semantic = false`，R8）；TS workspace 包名 / `package.json#imports` / `references` 解析；Python 裸 import；tree-sitter。
- `.redteam/packs/` 清理 / 保留策略（目录在 `.redteam/.gitignore` 下，随 job 落盘）。
- cell 命名（`bundle[label]@model`）与 proj.db 契约不动。

## Risks / Deferred
- 与 A 的 `src/cli/mod.rs` / `src/main.rs` 合并冲突：本批新增代码集中在独立函数（`run_pack_only`、`gen_job_id`）与 `ReviewArgs` 末尾字段；K15 一行为已知改点。
- 引用边全文扫描 O(改动文件² × 改动 def 名)，规模受改动集限制（PR181 40 文件）；> 200 文件退化到只扫 hunk（design 允许「hunk 或全文」）。
- tsconfig JSONC 解析失败 → 视为无映射（静默降级，与既有读失败 `continue` 一致）。
- 索引行带边后 4000 字符帽更紧 → 降质与指针保证不丢文件名；帽值调优 → M21e（R3 同批）。
- 旧模板措辞改动只改指令句、不改标签结构，`neutralize_structure` 覆盖不变；E11 注入回归随 `cargo test` 验证。
- 延后：`GroupingStats` 进 RunMeta（F-5）；增量轮两份清单合并（D-3）；增量轮 `--pack-only`；语义分组触发度量（`cross_edges` 已可观测，R8）。
