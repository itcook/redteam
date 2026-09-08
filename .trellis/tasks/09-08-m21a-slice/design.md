# design · M21a 批 E（切片装包）

> 需求与决策 K1-K17 见 `prd.md`；本文只写技术定论。上位 design v8 §10.1-10.2〔D8〕，plan E-1…E-4。

## 1. 边界与改动面

| 文件 | 改动 | task |
|---|---|---|
| `src/diffsym/mod.rs` | `DefRange`；`all_defs`；`changed_defs`（全部改动行锚定）；`OneHop` + `one_hop`；`ref_worthy` / `GENERIC_NAMES`（自 bundle 迁入，`pub(crate)`）；`body_names`（体内标识符取名，跳注释行） | E-1 |
| `src/bundle/mod.rs` | 删本地 `ref_worthy` / `GENERIC_NAMES`，改 `use crate::diffsym::ref_worthy`（语义零变） | E-1 |
| `src/pack/mod.rs` | `PackOpts` / `PackStats` / `Packed`；`pack_bundle`（新主入口）+ `build_pack` 薄包装；`body_section` 按 opts 分派 `full_block` / `slice_block`；`slice_file`（区间求解 + 渲染）；`hunk_windows`（±N，取代硬编码 20）；`outside_section`（走仓 + 调用点 + 签名）；`TEST_PATH` | E-2 / E-3 |
| `src/cli/mod.rs` | `pub(crate) mod deny;`（E-3）；`build_packs` 收 `&PackOpts` 返三元组（E-2 / E-4）；`plan_cells` 加 `pack_cfg`、`CellPlan.pack_stats`；`PackCell.pack: PackStats`；`render_pack_only` 追加形态字段 | E-2 / E-3 / E-4 |
| `src/cli/incremental.rs` | `plan_cells` 调用传 `&cfg.review.pack` | E-4 |
| `src/cli/second_pass.rs` | `build_pack` 调用传 `PackOpts::full()` + `changeset`（K2；窗宽取缺省 30，不为退役路径穿 cfg） | E-2 / E-3 |
| `tests/review_e2e.rs` | `pack_only_slices_large_file_and_reports_shape`（> 8K 文件；两档 `REDTEAM_HOME` 对比） | E-4 |
| `.next/roadmap/followup.md` | 四条（prd R-E4.6） | E-4 |

不碰：`src/config`（键已在）、`src/prompt` 与 `templates/`（D）、`src/orchestrate`、`src/state` / `src/db` / `src/domain`、`src/main.rs`（无新旗）、`plugins/`。

## 2. 数据结构

```rust
// diffsym
pub(crate) struct DefRange { pub start: usize, pub end: usize, pub name: String, pub kind: &'static str } // 0-based inclusive
pub(crate) struct OneHop { pub callees: Vec<DefRange>, pub callers: Vec<DefRange> }                     // 按 start 升序、去重
pub(crate) fn all_defs(lines: &[&str], defs: &[(&'static str, Regex)]) -> Vec<DefRange>;               // K4 嵌套过滤
pub(crate) fn changed_defs(diff: &str, lines: &[&str], defs: &[(&'static str, Regex)]) -> Vec<DefRange>; // K3
pub(crate) fn one_hop(lines: &[&str], all: &[DefRange], changed: &[DefRange]) -> OneHop;               // K5
pub(crate) fn body_names(lines: &[&str], r: &DefRange) -> BTreeSet<String>;                            // 体内 ref_worthy 标识符
pub(crate) fn ref_worthy(n: &str) -> bool;                                                             // 自 bundle 迁入

// pack
pub struct PackOpts { pub body: PackBody, pub anchor_window: usize, pub slice_min_bytes: usize }
impl From<&PackCfg> for PackOpts;                      // 缺省唯一真相源在 config（K1）
impl Default for PackOpts;                             // = PackCfg::default()（slice / 30 / 8192）
impl PackOpts { pub fn full() -> Self; }                // full 档，其余缺省（K2）
#[derive(Default, Serialize, Clone, Debug, PartialEq)]
pub struct PackStats { pub body: &'static str, pub files: usize, pub sliced: usize, pub small_full: usize, pub full: usize,
    pub ladder: usize, pub omitted: usize, pub one_hop_defs: usize, pub folded_lines: usize,
    pub outside_sites: usize, pub outside_sigs: usize, pub outside_bytes: usize }
pub struct Packed { pub text: String, pub stats: PackStats }
pub fn pack_bundle(bundle: &Bundle, root: &Path, budget: usize, own_diff_bytes: usize, opts: &PackOpts, changeset: &[String]) -> Packed;
pub fn build_pack(...) -> String { pack_bundle(...).text }   // 同参数；second_pass 与旧测试用
// 实施时的补充：PackStats 含 body / files / sliced / small_full / full / ladder / omitted / one_hop_defs / folded_lines / outside_*；
// diffsym 侧 changed_defs(diff, all) 直接吃 all_defs 产物（少一次扫描）
```

`PackBody` / `PackCfg` 复用 `crate::config`（pack 依赖这两个类型做转换，不读 `Config` 本体）。

## 3. 算法

### 3.1 E-1 一跳（diffsym）
1. `all_defs`：逐行 `match_def`；对每个命中算 `block_end`；维护「当前可调用 def 的 (start, end)」栈：若命中行落在某个 kind ∈ {fn, func, function, method} 的 def 区间内（start < i ≤ end）→ 丢弃（局部 const / 嵌套 fn）；class / struct / enum / trait / type / const / static / macro 不构成屏蔽区间。impl / mod / 顶层 `{}` 不是 def，天然穿透。
2. `changed_defs`：`hunk_changed_lines(diff)` 去重 → 每行 `enclosing_def_start` → 起行集合 → 与 `all_defs` 按 start 对齐取 `DefRange`（起行在 `all_defs` 之外——即嵌套 def——时向上取其外围可调用 def）。
3. `body_names`：`lines[start..=end]` 中跳过 trim 后以 `//`、`#`、`*`、`/*` 起始的行，其余按 `[A-Za-z_$][A-Za-z0-9_$]*` 切 token，过 `ref_worthy`，排除该 def 自身名。
4. `one_hop`：`changed_set` = changed 名集；对每个 changed def：callees = `body_names ∩ (all 名集 ∖ changed_set)`；对每个非 changed def d：`body_names(d) ∩ changed_set ≠ ∅` → caller。结果按 start 排序去重；同一 def 既 callee 又 caller 只出现在 callees。

### 3.2 E-2 切片（pack）
`slice_file(path, content, diff, defs: Option<&[..]>, opts) -> Option<(String, FileShape)>`：
1. `lines`；`n = lines.len()`；`content.len() <= slice_min_bytes` → None（调用方走 `full_block`，K8）。
2. 展开区间 `expand: Vec<(usize, usize)>`：
   - 支持语言：`all = all_defs`，`changed = changed_defs`，`hop = one_hop` → `expand = changed ∪ hop.callees ∪ hop.callers` 的 (start, end)。
   - 禁真空（K7）：对 `hunk_changed_lines` 每个 `l` 不在任何 expand 内 → 找它所属 hunk 的 `(c, d)`（`parse_hunk_new_range`，按 hunk 游走定位）→ 加 `[max(c−1−N, 0), min(c−1+d+N, n−1)]`；无支持语言时 expand 只由此规则产生；`hunk_changed_lines` 为空（无可解析 hunk）→ None 且调用方给全文。
   - 区间按 start 排序、相邻 / 重叠合并。
3. 保留行 `keep: BTreeSet<usize>`：
   - 文件头：从 0 到首个 `all_defs.start`（或 n）之间，行 trim 非空且以注释 / doc 前缀（`//` `///` `//!` `/*` `*` `#` `--`）或 `use ` / `pub use ` / `import ` / `export ` / `package ` / `mod ` / `pub mod ` / `require(` / `from ` 起始 → keep；超过 80 行后停（其余落入折叠）。
   - 签名：每个 `all_defs` 的 start 行，并向下延续到首个含 `{` 或以 `;` 结尾的行（含），最多 6 行。
4. 渲染：`i = 0..n` 扫描；若 i 在 expand 内 → 逐字输出直到区间尾；若 i ∈ keep → 输出；否则累积折叠段直到下一个 expand / keep 行：
   - 折叠段落在某 def 的 (start, end] 内（即函数体）→ `…（{a}-{b} 行折叠：{name}）`（1-based，a-b 为被折叠范围）；
   - 顶层胶水（不在任何 def 内）：段长 ≤ 3 → 原样输出；否则 `…（{a}-{b} 行折叠）`。
   - 一个折叠段跨越「def 体尾 + 顶层胶水」时按边界拆成两段。
5. 块头 `{path}:1-{n}（切片：核 {changed.len()} · 一跳 {hop.len()} · 折叠 {folds} 段）`；`FileShape { one_hop_defs, folded_lines }`。
6. `body_section` 每文件：`Slice` 且 `slice_file` 返回 Some → 试装切片块；装不下或 None → 既有梯子（Full 全文块只在 `Full` 或 K8 小文件 / 无 hunk 时是第一级；Slice 下第一级是切片块，其后 def 体 → 骨架 → hunk 窗 → omit 不变）；`hunk_window_block(…, n_lines, N)` 窗宽改 `opts.anchor_window`（K16）。统计：`sliced` / `small_full` / `full` / `ladder`（进入 def 体及以下任一级）/ `omitted`（进 omit 的文件数）。

### 3.3 E-3 文件外片段（pack）
`outside_section(bundle, root, opts, changeset, cap: usize, omit) -> (String, OutsideShape)`，仅 `Slice`：
1. 名集：每个组文件 `changed_defs` 名（`ref_worthy`）→ `callers_of: BTreeSet<String>`；`body_names` ∖ 本文件 `all_defs` 名集 ∖ `callers_of` → `sig_of: BTreeSet<String>`；各截 40（记 omit「文件外名字截断 +k」）。两集皆空 → 空段。
2. 一条 regex `\b(?:a|b|…)\b`（两集合并，`regex::escape`）→ `grep_regex::RegexMatcher`；`ignore::WalkBuilder::new(root).require_git(false).build()`，只取 `is_file`，`rel = strip_prefix(root)`（失败跳），跳过 `changeset.contains(rel)` / `TEST_PATH.is_match(rel)` / `is_denylisted_path(rel)` / `lang_for(rel).is_none()` / `metadata.len() > 512 KiB`；计数超 20_000 → break 并记 omit「文件外搜索截断（>20000 文件）」。
3. 每命中文件：`SearcherBuilder::new().line_number(true)` 收集 `(lnum, line)`（单文件 ≤ 200 命中）；对每行按 regex 找出命中名：
   - 名 ∈ `sig_of` 且 `match_def(line, defs_of(file)) == Some((_, name))` → 候选签名 `(path, lnum)`；
   - 名 ∈ `callers_of` 且该行不是 `match_def` 命中同名（排除定义行本身）→ 候选调用点 `(path, lnum, name)`。
4. 收尾排序：候选按 (path, lnum) 排序；调用点每名取前 K = 3；签名每名取首个。读文件（`read_to_string`，≤ 512 KiB）切片：调用点窗 `[l−N, l+N]`，同文件同名相邻 / 重叠窗合并；签名 = def 行起按 §3.2 步 3 多行签名规则 + 上方连续注释 / doc 行 ≤ 10。
5. 渲染：`## 文件外片段\n` + 调用点块 `{path}:{a}-{b}（调用点：{name}）\n{body}\n\n` + 签名块 `{path}:{a}-{b}（签名：{name}）\n{doc+sig}\n\n`；累计字节 > `cap` 的块转 omit `{path}:{a}-{b} — 文件外片段超帽（{name}）`。`cap = min(remaining_after_bodies, budget / 8)`。
6. 统计 `outside_sites` / `outside_sigs` / `outside_bytes`。

### 3.4 组装（`pack_bundle`）
`remaining = budget − own_diff_bytes` → `body_section`（同现状）→ `outside_section`（Slice）→ 省略清单（帽 `OMIT_MAX_ENTRIES`）。段序：`## 改动源文（0 壳）` → `## 文件外片段` → `## 已省略（预算裁剪）`。

## 4. 数据流

```
cfg.review.pack ──pack_opts──▶ PackOpts ─┐
plan_cells(selected, root, budget, grouping_cfg, pack_cfg, extra, delta)
   └─ group ──▶ bundles ──▶ build_packs(bundles, root, budget, &PackOpts::from(pack_cfg)) ──▶ (dossiers, meta, pack_stats)
                                └─ changeset = ∪ bundles.files.path
                                └─ pack_bundle(b, …, &opts, &changeset) ──▶ Packed{text, stats}
                                       └─ render_dossier_section(text) ──▶ dossiers[i]；stats ──▶ CellPlan.pack_stats[i]
run_pack_only ──▶ PackCell { …, pack: stats[i] } ──▶ summary.json / stdout
second_pass_prompt ──▶ build_pack(pb, …, &PackOpts::full(), &changeset)   // K2
```
`CellPlan` 加 `pack_stats: Vec<PackStats>`（与 bundles 等长）；`run_code_review` / 增量路径暂不消费（F-5 进 RunMeta）。

## 5. 兼容与不变量
- `Full` 输出 = 现状（仅窗宽 ±20 → ±30）；小文件 `Slice` 输出 = `Full`；既有 e2e fixture 全 < 8K → 断言零改。
- `changed_def_ranges` / `changed_def_names` / 分组 stats 不变（K3 / K5 迁移不改语义）。
- 确定性：走仓结果排序后再截断；`BTreeSet` / `BTreeMap` 贯穿。
- 安全：pack 只读 root 内文件；outside 走仓不跟符号链接、denylist 过滤；路径来自 walker 与 git diff，非模型可控；输出经 `render_dossier_section` 中和结构标签（既有）。
- 空渲染纪律：无候选 → 无 `## 文件外片段` 段；无文件 → 空串。

## 6. 回滚
- 子分支回滚 = 不合回；合回后 revert `--no-ff` merge。
- 运行期逃生口：`~/.redteam/config.json` `review.pack.body = "full"`（D8 旋钮），无需重编译。
