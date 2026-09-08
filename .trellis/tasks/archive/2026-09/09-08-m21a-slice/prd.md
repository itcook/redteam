# M21a 批 E：切片装包（文件内一跳 / 切片配方 / 文件外片段）

> 上位：`.next/plans/2026-09-07-redteam-m21a-scan-foundation-plan.md` 批 E（E-1…E-4）+ Global Constraints；design v8 §10.1-10.2〔D8〕/ §10.3；决策 D4 / D8；roadmap `M21a-scan-foundation.md` 交付项 5b 与验收「E2 slice vs full A/B」（A/B 本身在出口 3，非本批）。
> 技术设计 `design.md`，执行清单 `implement.md`；本文 §Key Decisions 的 K 编号被两者引用。
> 分支 `feat/m21a-slice`（自 `feat/m21-review-engine` 4163c46 切出，工作树 = 本目录；批 C 与修补批已合回，前提满足），合回目标 `feat/m21-review-engine`（`--no-ff`，用户操作）。
> 收口方式：按用户 2026-09-08 裁定，M21 期间不跑 ship 评审，改用 code-review 子 agent 审核（memory `no-ship-review-until-core-stable`）。

## Goal

把 scanner 组 pack 的装包单位从「整文件」改成「改动中心切片」：每个改动文件只展开改动 def 全文 + 文件内一跳，其余保留签名地图、函数体折叠；补文件外 caller 调用点与 callee 签名片段；禁真空兜底保证 diff 每个改动行都在包里。`[review.pack] body` 缺省 `slice`（D8，配置侧已落地），`full` 作为出口 3 A/B 的对照臂与逃生口原样保留。用户价值：

- scanner 的职责是广度：改动 def + 一跳 + 全部签名足以「怀疑」，被折叠的函数体它知道存在、缺证据走 `needs`（design D8 依据）。full 的代价在 scanner 上被 组 × 条目 × samples 放大。
- F3 的失败模式（真空、缺依赖）各有对症层：禁真空兜底、文件内一跳、文件地图三层同批交付，缺一不可。
- `--pack-only` 两档可对账：同一 diff 下 slice / full 的字节数与形态统计可读，是出口 3 的仪表。

## Background（仓库实测 2026-09-08，HEAD 4163c46）

### 现状代码
- pack `src/pack/mod.rs`：`build_pack(bundle, root, budget, own_diff_bytes) -> String`（:161）→ `body_section`（:19）按 `bundle.fill_order()`（:22）逐文件：全文块 `path:1-N（全文，N 行）` → 超预算回落 def 体（`changed_def_ranges`）→ 全部 def 体零入正文才 `skeleton_of`（:103，签名行 + 全部改动行 + `…（N 行折叠）`）→ 非支持语言 `hunk_window_block`（:130，硬编码 ±20 行、跨 hunk 合并）→ omit 坐标；`OMIT_MAX_ENTRIES = 200`（:14）。**没有任何代码读 `review.pack`**：`body` / `anchor_window` / `slice_min_bytes` 三键在 pack 侧零消费。
- diffsym `src/diffsym/mod.rs`：`Lang { exts, defs }`（:8）三语言 def 正则 RUST / GO / TS（:14 / :29 / :40，TS method 正则宽松 + `TS_KEYWORDS` 后置滤 :53）；`lang_for`（:58）、`compile_defs`（:63）、`match_def -> (kind, name)`（:67）、`block_end` 括号配平（:81，4000 行护栏）、`enclosing_def_start`（:107，impl 头穿透）、`parse_hunk_new_range`（:118）、`hunk_changed_lines`（:135，全部 `+` 行与删除点，0-based 新文件坐标）、`hunk_anchors`（:163，每 hunk 首个改动行）、`changed_def_ranges(diff, lines, defs) -> Vec<(start, end)>`（:203，**只按每 hunk 首行锚定**——一个 hunk 横跨两个 def 时第二个 def 丢失）。无 `DefRange` 类型、无一跳、无「全部 def」枚举。
- bundle `src/bundle/mod.rs`：`GENERIC_NAMES`（:162）+ `ref_worthy`（:167，≥ 4 字符、非通用名、< 8 字符全小写排除；即 plan 所说 `fanin_worthy` 口径）+ `changed_def_names`（:178）均为私有；引用边用一条 `\b(?:name|…)\b` 合成 regex 扫全部改动文件（:1259-1295）。`Bundle { files, edges, hub_order, over_budget }`（:71）、`from_files`（:84）、`fill_order`（:94）；`over_budget` 到 pack 层只可见未消费（followup :55）。
- cli `src/cli/mod.rs`：`build_packs(bundles, root, budget)`（:186）→ `render_dossier_section(build_pack(...))`；`plan_cells(selected, root, budget, grouping_cfg, extra_paths, delta)`（:223）是全量 / 增量 / `--pack-only` 三路唯一入口（:294 / `src/cli/incremental.rs:96-106` / :431）；`grouping_opts`（:241）是「config 子表 → 模块 opts」的既有模式；二阶段 `second_pass_prompt`（`src/cli/second_pass.rs:210`）直接调 `build_pack`，增强包用 `Bundle::from_files` 重建（needs 文件前插，changeset 外文件 `diff` 为空串）。`PackCell`（:390）/ `PackOnlyReport`（:404）/ `run_pack_only`（:418）/ `render_pack_only`（:479）；`PACK_BYTES_PER_TOKEN = 3.5`（:385）。`mod deny;`（:14）私有，`deny::is_denylisted_path`（`src/cli/deny.rs:25`）`pub(crate)`；needs 读守卫 `need_read_allowed`（second_pass.rs:45）= denylist ∧ 逐文件 `git ls-files --error-unmatch`。
- tools `src/tools/mod.rs`：`ignore::WalkBuilder::new(&scope).require_git(false)`（:320 / :365，gitignore 感知、隐藏文件默认跳过、不跟符号链接）+ `grep_searcher::SearcherBuilder`（:379）已是模型 `search_repo` 工具的实现；`NEED_READ_MAX_BYTES = 512 KiB`（:283）。`Cargo.toml` 已有 `ignore` / `grep-regex` / `grep-searcher` / `regex`，不加依赖。
- 配置 `src/config/mod.rs`：`PackBody { Full, Slice }`（:259）、`PackCfg { body, anchor_window: u32, slice_min_bytes: u64 }`（:321）缺省 `Slice / 30 / 8192`（:342-344，D8）、已知键表 `body / anchorWindow / sliceMinBytes`（:710）、`anchorWindow` 零值拒、`sliceMinBytes` 0 合法；`Config::default()` 即 slice。
- prompt `src/prompt/mod.rs`：`render_dossier_section`（:308）前言散文仍描述「0 壳全文 + 1 壳引用点」，`DOSSIER_STRUCTURAL_TAGS`（:304）六标签中和；模板与 prompt 层归批 D（D-2/D-3/D-4 重写共享头 / scanner 尾 / 段序），E 不碰。
- 测试口径：`eval/risk_replay.py:221` `TEST_PATH = (_test\.go$|\.test\.(ts|js|tsx)$|(^|/)tests?/|(^|/)__tests__/|_spec\.)`（Rust 侧无对应）；e2e `tests/review_e2e.rs` `pack_only_*` 四例（:754 / :801 / :825 / :874）fixture 文件均 < 8 K。
- 基线：`cargo test` 499 lib + 16 + 7 + 16 绿、clippy 3 条既有、`make verify` 绿（Session 9）。

### 与批 D 的并行边界
D（`feat/m21a-prompts`，同基点 4163c46）改 `src/prompt/*`、`src/prompt/templates/*`、`src/provider/parse.rs`、`src/domain/*`、report（模板哈希）。E 改 `src/diffsym`、`src/pack`、`src/bundle`（仅 `ref_worthy` 迁出）、`src/cli/{mod,incremental,second_pass}.rs` 接线、`tests/review_e2e.rs`。交集为空；`src/cli/mod.rs` 若 D 也动（RunMeta 模板哈希）则后合者 rebase 一次。

## Requirements

### R-E1 文件内一跳解析（plan E-1，`src/diffsym/mod.rs`）
- R-E1.1 `DefRange { start, end, name, kind }`（0-based inclusive）；`all_defs(lines, defs) -> Vec<DefRange>`：逐行 `match_def` + `block_end`，**嵌在可调用 def（fn / func / function / method）块内的 def 不计**（局部 const / 嵌套 fn / 闭包变量不是文件级符号，K4）；class / struct / trait / impl / enum 块内的 def 照计。
- R-E1.2 `changed_defs(diff, lines, defs) -> Vec<DefRange>`：按 `hunk_changed_lines` **全部改动行**锚定外围 def（去重、升序，K3）；既有 `changed_def_ranges`（hunk 首行锚定）签名与语义不变，分组引用边与旧梯子零漂移。
- R-E1.3 `one_hop(lines, all: &[DefRange], changed: &[DefRange]) -> OneHop { callees, callers }`：callee = 改动 def 体内以词边界出现的同文件其他 def 名；caller = 体内出现改动 def 名的其他同文件 def；名字过滤 `ref_worthy`（从 bundle 迁到 diffsym `pub(crate)`，bundle 改引用，K5）；自引用不计、改动 def 之间互引不重复计入、只一跳；体内注释行（`//` / `#` / `*` / `/*` 起始）不参与取名。
- R-E1.4 测试：三语言合成文件各一（Rust impl 内方法、Go 方法、TS class 方法）；自引用不计；局部 `const` / 嵌套 fn 不作 def；通用名（`handle` / `data`）不成一跳；横跨两 def 的单 hunk 两个 def 都进 `changed_defs`。

### R-E2 切片配方（plan E-2，`src/pack/mod.rs`）
- R-E2.1 `PackOpts { body: PackBody, anchor_window: usize, slice_min_bytes: usize }`，`build_pack(bundle, root, budget, own_diff_bytes, &opts, changeset: &[String]) -> String` 与 `pack_bundle(...) -> Packed { text, stats: PackStats }`（K1 / K13）。
- R-E2.2 `body = Slice` 每文件：`content.len() <= slice_min_bytes` → 全文块（与 Full **逐字节相同**，K8）；否则切片块：核（`changed_defs` 全文）→ 一跳（callee / caller 全文）→ 文件地图（文件头：首个 def 前的注释 / doc / `use` / `import` / `package` / `mod` 行，> 80 行折叠；全部 def 签名行，多行签名保到首个 `{` 或 `;`、≤ 6 行；顶层 const / static / type 行按 def 正则本就是签名）→ 其余 def 体折叠 `…（a-b 行折叠：name）`、顶层胶水行连续 > 3 行折叠 `…（a-b 行折叠）`（K6）。块头 `path:1-N（切片：核 c · 一跳 h · 折叠 f 段）`。
- R-E2.3 禁真空（K7）：`hunk_changed_lines` 中任一改动行不落在已展开区间 → 该 hunk 的 `[c−N, c+d+N]` 窗（N = `anchor_window`）并入展开区间（相邻合并）；非支持语言（`lang_for` None）切片 = 文件头 + 各 hunk 窗 + 折叠；无可解析 hunk 头 → 全文。**性质**：diff 每个 `+` 行的文本逐字出现在输出中。
- R-E2.4 梯子（K9）：切片块装不下 → 既有回落链（def 体 → 骨架 → hunk 窗 → omit），slice 只是替换掉「全文」这一级；`hunk_window_block` 的 ±20 改为 ±`anchor_window`（两档共用，K16）；`Bundle.over_budget` **不由 pack 消费**（梯子纯按预算触发，标记留仪表；关闭 followup :55）。
- R-E2.5 `body = Full` 输出与现状逐字节相同（唯一差别是 R-E2.4 的窗宽），保证出口 3 对照臂纯净（K10）。
- R-E2.6 测试：禁真空性质测试（rs / go / ts / md 各一，含改动落在文件头 `use` 区、hunk 横跨两 def、纯删除 hunk）；小文件 slice 与 full 逐字节相同；一跳函数展开、无关大函数折叠且折叠计数 / 行号范围正确；预算不足时梯子接管（def 体 / 骨架标注在场）；Full 模式既有 pack 测试全部保留。

### R-E3 文件外片段（plan E-3，`src/pack/mod.rs`）
- R-E3.1 仅 `body = Slice` 产出 `## 文件外片段` 段（位于 0 壳之后、省略清单之前，K10）。
- R-E3.2 caller 调用点：组内全部改动 def 名（`ref_worthy`）合成一条 `\b(?:…)\b` regex；`ignore::WalkBuilder::new(root).require_git(false)` 遍历 + `grep-searcher` 逐文件搜；跳过：变更集全部文件（`changeset`）、`TEST_PATH` 口径测试文件（K12）、`is_denylisted_path` 命中、`lang_for` None、> 512 KiB；遍历超 20_000 文件停止并在 omit 记「文件外搜索截断」（K11）。命中按 (path, line) 排序后每名 ≤ K = 3 处，每处 ±`anchor_window` 行窗（同文件相邻合并），块头 `path:a-b（调用点：name）`。
- R-E3.3 callee 签名：改动 def 体内引用、本文件无同名 def 的 `ref_worthy` 标识符 → 在同一遍遍历里找 `match_def` 命中该名的 def 行 → 输出签名行（多行签名规则同 R-E2.2）+ 紧邻上方连续 doc / 注释行（≤ 10 行），每名取路径序首个；解析不到静默（不入 omit）。
- R-E3.4 预算：段总字节 ≤ min(剩余预算, 组预算 / 8)（K11），帽 < 256 字节不走仓；超帽的条目进 omit 清单 `path:a-b — 文件外片段超帽（类别：name）`，逐条 ≤ 10 其余合并计数；名字上限：调用点名 64、签名名 256 搜 / 40 渲染，截断记 omit 一行。
- R-E3.5 读守卫：只读 root 内（walker 不跟符号链接、`strip_prefix(root)` 失败即跳）；路径非模型可控，不逐文件 `git ls-files`（与 `search_repo` 工具同口径，K11）。
- R-E3.6 测试：临时仓两文件（caller 在他文件 → ±N 片段；callee 签名带 doc）；K 帽；字节帽 → omit；变更集文件 / `tests/` / `.env` / `.py` 不出现；root 外符号链接目标不出现；Full 模式无该段；确定性（两次调用输出相同）。

### R-E4 接线、默认与观测（plan E-4）
- R-E4.1 `cli::pack_opts(&PackCfg) -> PackOpts`（镜像 `grouping_opts`）；`build_packs` / `plan_cells` 加 `pack_cfg: &PackCfg`，三条路径传 `&cfg.review.pack`；`changeset` = 全部 bundle 的文件路径。
- R-E4.2 二阶段 `second_pass_prompt`（F-4 拆除的旧路径）用 `PackOpts::second_pass()`：每文件全文块 + 文件外片段（K2 修订）。
- R-E4.3 `PackStats { body, files, sliced, small_full, full, ladder, omitted, one_hop_defs, folded_lines, outside_sites, outside_sigs, outside_bytes }`（Serialize）；`PackCell` 加 `pack: PackStats`，stdout cell 行追加 `body=… sliced=… small=… ladder=… folded_lines=… outside_sites=… outside_sigs=…`；RunMeta 接线仍留 F-5（K13）。
- R-E4.4 A/B 切档不加 CLI 旗：用 `REDTEAM_HOME=<临时目录>` + `config.json` `{"review":{"pack":{"body":"full"}}}`（K14）；`make build` 不跑（F-1 前禁）。
- R-E4.5 `cli/mod.rs` `mod deny;` → `pub(crate) mod deny;`；`deny::is_credential_path` 拆出供走仓（K11）。`--pack-only` 帮助文案补装包形态（CLAUDE.md `--help` 同权重）。
- R-E4.6 followup 登记：① dossier 前言 / scanner 尾需描述切片层次与「折叠体走 needs」措辞（交 D-2）；② `--pack-body` CLI 覆盖旗是否要（M21e 或用户裁）；③ K / N / 1/8 帽 / 40 名上限归 R7 调优；④ 关闭 :55「over_budget 未消费」（K9 定论）。
- R-E4.7 本批记录：对本仓同一 diff 跑 `--pack-only` 两档，把 cell 字节 / est_tokens / 形态统计与 E-3 遍历耗时写进 `implement.md` §收口。

## Key Decisions

| # | 决策 | 依据 / 代价 |
|---|---|---|
| K1 | `PackOpts` 由 cli 从 `PackCfg` 转出，`build_pack` 显式收 opts + `changeset`；不让 pack 依赖 config 类型 | 镜像批 C `grouping_opts`；pack 保持 hermetic（喂参数出字符串） |
| K2 | 二阶段补查 `PackOpts::second_pass()` = slice 档 + `slice_min_bytes = MAX`（每文件全文块，仍产文件外片段；评审修 9 修订，原「恒 Full」会丢第一趟 draft 引用的调用点块） | needs 语义 = 要全文；旧路径 F-4 退役，不为它做切片 |
| K3 | 切片核按**全部改动行**锚定（新 `changed_defs`），`changed_def_ranges` 不动 | 修 hunk 跨 def 漏核；不扰动分组引用边与 C 的确定性测试 |
| K4 | 一跳 / 地图的 def 集排除「嵌在可调用 def 内」的 def；值 / 类型别名 def 块尾按三种括号配平（评审修 3） | 局部 const / 嵌套 fn 非文件级符号；class / impl 内方法保留；Go `var X = f()` 不吞下一个函数 |
| K5 | `ref_worthy` 迁 diffsym `pub(crate)`，bundle 改引用 | 一处口径（plan「沿用 fanin_worthy」）；不复制 |
| K6 | 单块自顶向下渲染：展开区间逐字、签名行保留、折叠标记带行号范围与 def 名 | 行号可推算（折叠标记含范围）；quote 逐字（D-1 对齐靠 quote） |
| K7 | 禁真空 = 未覆盖改动行所在 hunk 的 ±N 窗并入展开区间；非支持语言 = 顶部 80 行的头 + hunk 窗 + 折叠（评审修 7）；每 hunk 按 + 行文本重锚定坐标、±2000 行内找整体偏移（评审修 4，staged + unstaged 拼接 / 脏树） | F3 真空失败模式的对症层；性质测试锁死 |
| K8 | `≤ slice_min_bytes` 全文块与 Full 逐字节相同 | D8 保险；既有 e2e fixture 全 < 8K → 零改动 |
| K9 | 梯子按预算触发；`over_budget` 不消费 | slice 已是缩形，标记无法再降；消费它会污染 Full 对照臂 |
| K10 | 文件外片段只在 Slice；Full = 现状输出 | 出口 3 对照臂纯净（除窗宽 20→30） |
| K11 | 文件外搜索：ignore 走仓 + 凭据判据（`deny::is_credential_path`，不含敏感词子串，评审修 10）+ 跳测试 / 非支持语言 / 大文件 / 变更集；20_000 文件走仓上限；段帽 = 组预算 / 8、帽 < 256 字节不走仓（评审修 5）；每名 K=3；`use` / `import` 行不算调用点（评审修 6）；调用点名可调用优先、填充序、≤ 64，测试代码（TEST_PATH / `#[cfg(test)]` 后）不搜（评审修 1）；签名名 ≤ 256 搜、≤ 40 渲染、sink 只计有效命中（评审修 2） | 与 `search_repo` 工具同守卫口径；路径非模型可控故不逐文件 git 校验；帽防大 fan-in 撑段 |
| K12 | `TEST_PATH` 正则从 `eval/risk_replay.py:221` 逐字移植 | 与风险规则同口径（M21b 复用） |
| K13 | `PackStats` 进 `--pack-only`（stdout + summary.json）；RunMeta 留 F-5 | 铁律 1；批 C `GroupingStats` 同模式 |
| K14 | 不加 `--pack-body` 旗，A/B 走 `REDTEAM_HOME` | plan 范围；出口 3 双 scanner 配置本就要临时 config；旗记 followup |
| K15 | prompt / 模板措辞不动，切片层次说明交 D-2 | D 与 E 无交集是 plan 前提 |
| K16 | `hunk_window_block` 窗宽 = `anchor_window`（两档） | plan E-2 原文；`anchorWindow` 键终于有消费点 |
| K17 | 折叠标记 `…（a-b 行折叠：name）` 沿用现有中文块头风格 | 与 `skeleton_of` 标记同族；plan 英文示例视为示意 |

## Acceptance Criteria

- AC1（R-E1）：`cargo test diffsym::` 覆盖 R-E1.4 五类；`bundle::` 既有分组 / 确定性测试零改动通过。
- AC2（R-E2）：禁真空性质测试四语言通过；小文件 slice == full（`assert_eq!` 字节串）；折叠计数与范围断言；预算不足梯子接管断言；`body = Full` 时既有 pack 测试（仅窗宽相关的两例调预算数字）通过。
- AC3（R-E3）：R-E3.6 八项断言；根外符号链接目标内容不在输出。
- AC4（R-E4）：`Config::default()` 下 `--pack-only` 对一个 > 8K 的改动文件报 `sliced=1` 且 `pack_bytes` 小于 `body: full` 的同 diff 运行；`summary.json` 含 `cells[].pack.body`；四例既有 `pack_only_*` e2e 不改断言通过。
- AC5：`cargo test` 全绿（lib ≥ 499 + 新增）、`cargo clippy --all-targets` 不新增、`make verify` 绿；`git diff --stat` 不含 `src/prompt/`、`src/config/`、`src/orchestrate/`、`src/state/`、`src/db/`、`src/domain/`、`plugins/`（`src/main.rs` 只改 `--pack-only` 一句帮助文案）。
- AC6：两档对比数字与 E-3 走仓耗时写入 `implement.md` §收口；followup 四条已登记。

## Out of Scope

二跳 / `slice_min_bytes` 调优（R7，出口 3 回退信号后）· `--pack-body` CLI 旗 · RunMeta 接线（F-5）· prompt / 模板措辞（D-2 / D-3）· dossier 前言改写（D）· reviewer 区域 pack（M21b，构造即切片）· PR181 A/B（出口 3，需 `.next/private`）· Python def 支持 · LLM 语义分组 · `make build`。

## Risks / Deferred

- 大仓走仓耗时：K11 的 20_000 文件上限 + 本仓实测耗时记录；超标再议按 `edges` 目录邻近性收窄范围。
- 一跳之外的依赖被折叠：设计出口是 scanner `needs{kind: evidence}`（D-2 措辞）+ 出口 3 A/B 回退信号（先二跳再 full）。
- 与 D 的 `src/cli/mod.rs` 潜在碰撞：E 只加参数与字段，后合者 rebase。
- 窗宽 20→30 让两个既有 pack 测试的预算常数需调；语义不变。
