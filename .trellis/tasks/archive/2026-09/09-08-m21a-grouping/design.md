# design · M21a 批 C（引用图分组 + 跨组索引 + `--pack-only`）

> 需求与决策 K1-K15 见 `prd.md`；本文只写技术定论。上位 design v8 §3.2 / §10.6，plan C-1…C-5。

## 1. 边界与改动面

| 文件 | 改动 | task |
|---|---|---|
| `src/bundle/mod.rs` | `ImportResolver` + 三语言解析 + `import_edges`；`Edge` / `EdgeKind` / `Bundle` 扩展 / `GroupingOpts` / `GroupingStats` / `Gate` / `Grouping`；`group` 重写；`ref_worthy` / `GENERIC_NAMES`；`CrossIndex` / `cross_index` / `render_cross_index`；删 `other_files_manifest`（`cap_entries` 留给 `two_layer_manifest`） | C-1 / C-2 / C-3 |
| `src/cli/mod.rs` | 删 `merge_bundles`；`group` 调用下移到预算之后；`others` 预渲染；`ReviewArgs.pack_only`；`run_pack_only` + `PackOnlyReport` + `render_pack_only`；`pub fn gen_job_id` | C-2 / C-3 / C-4 |
| `src/cli/incremental.rs` | `group` 调用改；`extra_paths` / `delta` 喂索引；删 `merge_bundles` 两测试 | C-2 / C-3 |
| `src/cli/second_pass.rs` | `Bundle::from_files`；`others: &str` 参数替代内部 `other_files_manifest` | C-2 / C-3 |
| `src/orchestrate/mod.rs` | `run_code_fanout` 加 `others: &[String]`（与 `dossiers` 同形），去内部 manifest 调用 | C-3 |
| `src/prompt/mod.rs` | `build_bundle_prompt` 文档（`other_files` 语义 = 跨组索引串）；空槽占位不变 | C-3 |
| `src/prompt/templates/adversarial-code-bundle.md` | `:80` 一句组感知措辞（K10） | C-3 |
| `src/pack/mod.rs` | `body_section` 按 `bundle.fill_order()` 遍历（3 行） | C-2 |
| `src/main.rs` | clap `pack_only` + `conflicts_with_all`；`--help` 文案；分支到 `run_pack_only`；`gen_job_id` 改调 `cli::gen_job_id` | C-4 |
| `examples/dump_pack.rs` | 删除 | C-2 |
| `eval/README.md`、`eval/e3_contract_ab.py` | 顶注改历史注记 + 指向 `--pack-only` | C-4 |
| `tests/review_e2e.rs` | `pack_only_*` lib 级 e2e | C-4 |
| `.next/roadmap/followup.md` | 三条延后项 | C-5 |

不碰：`src/config`、`src/state`、`src/db`、`src/domain`、`src/provider`、`plugins/`。

## 2. 数据结构（`src/bundle/mod.rs`）

```rust
pub struct ImportEdge { pub to: String }                       // 仓内相对路径，带扩展名（K14）

pub enum EdgeKind { Import, Reference { count: u32 }, SameDir }
pub struct Edge { pub from: String, pub to: String, pub kind: EdgeKind }
// 方向：Import = from 引入 to；Reference = from 的文本里出现 to 的改动 def 名（from 引用 to）；SameDir 无向，存一次（from < to）

pub struct Bundle {
    pub files: Vec<FileChange>,     // 路径升序（cell label 稳定）
    pub edges: Vec<Edge>,           // 与本组文件相关的全部边（组内 + 跨组）
    pub hub_order: Vec<usize>,      // files 的填充序（枢纽先）；空 = 自然序
    pub over_budget: bool,
}
impl Bundle { pub fn from_files(files) -> Bundle; pub fn fill_order(&self) -> impl Iterator<Item = &FileChange>; }

pub struct GroupingOpts { pub min_files: usize, pub bundle_line_threshold: usize, pub max_files_per_group: usize, pub budget_bytes: Option<usize> }
impl Default: 4 / 200 / 10 / None（design §12.1；K1）

pub enum Gate { Single, Grouped }
pub struct GroupingStats { pub gate: Gate, pub groups: usize, pub edges: usize, pub weak_cut: usize, pub cross_edges: usize, pub isolated: usize, pub over_budget: usize }
pub struct Grouping { pub bundles: Vec<Bundle>, pub stats: GroupingStats }

pub struct IndexEntry { pub path: String, pub group: Option<usize>, pub adds: usize, pub dels: usize, pub forward: Vec<String>, pub backward: Vec<String>, pub delta: bool }
pub struct CrossIndex { pub entries: Vec<IndexEntry> }
```

`GroupingStats` 派生 `serde::Serialize`（`summary.json` 用）；`Gate` 序列化为 `"single" | "grouped"`。

## 3. C-1 三语言 import 解析

`ImportResolver::new(root: &Path, changed: &[String])` 预建：改动路径集、去扩展名键 → 路径表（沿用 `norm_file`，`x/index.ts` 同时登记 `x`），并惰性缓存 tsconfig / go.mod / crate 根（按目录）。`import_edges(path, content, &resolver)` 按扩展名分派；所有候选先 `norm_rel` 规范化，规范化后含逃逸（栈空再 pop）或不在改动集 → 丢（R-C1.5）。

| 语言 | 规则 | 定位 |
|---|---|---|
| TS/JS | 相对：现有 `quoted_rel_after`；裸模块：最近 `tsconfig.json`（文件目录向上至 root），`extends` 链（相对路径、深 ≤ 5，子覆盖父），`paths` 键精确或单 `*` 模式 → 替换 → 相对 `baseUrl`（缺省 tsconfig 目录）；仅 `baseUrl` 时直接 `baseUrl/spec`；候选 `<映射>` 与 `<映射>/index`，按去扩展名键查表 | `jsonc_to_json`：逐字符状态机剥 `//` / `/* */`（字符串内不剥）+ 尾逗号，再 `serde_json::from_str`；失败 → 无映射 |
| Rust | `use` 行（`pub`/`pub(crate)` 前缀、`;` 结尾），`{}` 组按顶层逗号递归展开，`as X` / `*` / `self` 尾段剥除；首段 `crate` → 根、`super`（可重复）→ 当前模块上溯、`self` → 当前模块；其余首段（外部 crate）丢。当前模块：`src/a/b.rs` → `[a,b]`，`src/a/mod.rs` → `[a]`，根文件 → `[]`。段 `s1..sn` 从 n 到 1 回退：`<root>/s1/../sk.rs` 或 `/sk/mod.rs` 命中改动集即成边 | crate 根 = 最近祖先 `src/` 且含 `lib.rs` 或 `main.rs`；否则最近 `src/`；无 → 丢 |
| Go | `import "p"` 与 `import ( … )` 块（每行首个引号串，别名前缀允许）；最近 `go.mod` 的 `module m`：`p == m` 或 `p` 以 `m/` 开头 → 目录 `<gomod_dir>/<p[len(m)+1..]>`；该目录下每个改动 `.go`（非自身）各成一条边 | 嵌套 go.mod 最近者优先 |
| Python | 现有相对逻辑不动 | — |

## 4. C-2 分组算法

```
files = sort_by_path(changes); n = len; content[i] = read(root/path) or None（读失败降级）
churn = Σ_i count_plus_minus(diff_i)                         # K2
E = []
for i: for e in import_edges(path_i, content_i, resolver): j = idx[e.to]; i≠j → E.push(Import i→j)
names_i = { name | (s,_) in changed_def_ranges(diff_i, lines_i, defs), match_def(lines_i[s]) = (_, name), ref_worthy(name) }   # 仅 lang_for 支持语言
re_i = Regex(r"\b(?:n1|n2|…)\b")                              # 每文件编译一次
text_j = if n ≤ REF_FULLTEXT_MAX_FILES(200) { content_j } else { diff_j }
for i (names_i ≠ ∅): for j≠i: c = re_i.find_iter(text_j).count(); c>0 → E.push(Reference{count:c} j→i)
for i<j: parent_dir 相同 → E.push(SameDir i–j)
strong(e) = Import | Reference{count ≥ 2}                      # K3

gate: n < min_files || churn < threshold → 单包（全部文件、全部边、hub_order、over_budget = est(all) > budget）; stats.gate = Single; return

comps = union_find(all E)
for comp in comps:
    if oversize(comp):                                         # |comp| > max_files || est(comp) > budget
        weak_cut += |weak edges inside comp|
        subs = union_find(strong edges inside comp)
        for sub in subs: |sub| == 1 → pool; else groups.push(sub, over_budget = oversize(sub))
    else groups.push(comp)
pool += { i | degree(i) == 0 }; isolated = |{ i | degree(i) == 0 }|  # K6
bin-pack(pool sorted by (dir, path)): 新箱条件 = 文件数达 max_files 或 est 超 budget；单文件超 budget 独占且 over_budget
groups.sort_by(min path); for g: hub_order = files sorted by (−import_degree_in_group, path)
Bundle.edges = { e ∈ E | e 端点 ∈ g }; stats.cross_edges = |{ e | 端点异组 }|; stats.edges = |E|
```

`est(S) = Σ_{i∈S} (len(content_i) + len(diff_i))`（K5）。`ref_worthy` 与 `GENERIC_NAMES` 逐字移植 `eval/risk_replay.py:113-118, 223`（K4）。确定性：所有集合用 `BTreeMap` / 排序后遍历。

## 5. C-3 跨组索引

- `cross_index(bundles, extra_paths, delta)`：遍历各组 `files` → entry（group = Some(g)，adds / dels 同 `other_files_manifest` 计法，forward = 该文件为 `from` 的 Import / Reference 边的 `to`，backward = 反向；SameDir 不进索引；去重、排序）；`extra_paths` 中不在任何组的路径 → entry（group None、计数 0、无边）；`delta` 为 `Some` 时 `delta = set.contains(path)`。
- `render_cross_index(&idx, for_group)`：候选 = `group != Some(for_group)` 的 entry；排序键 `(¬connected_to_for_group, group, path)`（connected = forward ∪ backward 与 for_group 文件相交）；行格式 `path (gN +a/-d) → [x, y] ← [z] [delta]`（extra 行只有 `path`，无边段）；三档填充：完整行 → 降质行 `path → see group N`（extra 无 group → 只 `path`）→ 尾注 `… and K more files (see groups a, b)`；预算 = `MANIFEST_MAX_CHARS − MANIFEST_SUFFIX_RESERVE`，条目帽 `MANIFEST_MAX_ENTRIES`；行以 `\n` 连接、非空时前置 `\n`（槽在冒号后）。候选为空 → 空串。
- 消费链：`run_code_review` / `run_incremental_review` 算一次 `idx`，`others[i] = render_cross_index(&idx, i)` → `run_code_fanout(…, &dossiers, &others, …)` 与 `second_pass_prompt(…, others[idx], …)` → `build_bundle_prompt(other_files = others[i])`（中和不变）。
- 模板 `:80` 新句：`Do NOT claim something is missing, undeclared, or unimplemented ACROSS THE CHANGESET … without first checking the cross-group index above (those files are changed in this changeset but reviewed in other groups; a → / ← edge means they interact with this group) and the dossier below — the dossier's omission list is the ONLY signal that content was cut for budget rather than absent. …`（其余原句保留）。

## 6. C-4 `--pack-only`

```
main: args 构造后、background 分支前：if args.pack_only { match cli::run_pack_only(&args, &cfg, &cwd) { Ok(r) => { print!("{}", render_pack_only(&r)); exit(0) } Err(e) => { eprintln!("error: {e}"); exit(1) } } }
run_pack_only(args, cfg, cwd) -> anyhow::Result<PackOnlyReport>:
  changes = changed_files(cwd, base, branch_scope); head = head_sha_at(cwd).ok()
  names = cfg.models.keys()（K15）; budget_raw = pack_budget_bytes(cfg, names); (spec, used, _) = render_spec_section_capped(args.spec, min(SPEC_BUDGET_BYTES, budget_raw/2)); budget = budget_raw − used
  grouping = group(select_files(changes), Some(cwd), &GroupingOpts { budget_bytes: Some(budget), ..Default })
  (dossiers, _) = build_packs(&bundles, cwd, budget); idx = cross_index(&bundles, &[], None)
  for (i, b): others = render_cross_index(&idx, i); prompt = build_bundle_prompt(target_label, b, focus, &others, "", &dossiers[i], &spec)
      write .redteam/packs/<job_id>/cell-<i+1>.md; cells.push { cell: i+1, label, files, bytes: prompt.len(), est_tokens: (bytes as f64 / 3.5).round(), pack_bytes: dossiers[i].len(), diff_bytes: Σ diff, over_budget }
  write summary.json { job_id, head_sha, target, budget_bytes, gate, cells, grouping: stats }
```
- `job_id = cli::gen_job_id(cwd)`（原 `main::gen_job_id` 逐字外移）。目录 `.redteam/packs/` 在 `.redteam/.gitignore = *` 之下；`.redteam/` 不存在则 `create_dir_all`（不调用 `init_project`，不建库）。
- clap：`#[arg(long, conflicts_with_all = ["background", "resume", "incremental_from"])] pack_only: bool`；`ReviewArgs.pack_only` 带 `#[serde(default)]`（旧 `args.json` 兼容）。
- 输出（K12）：`cell-<n> files=<k> bytes=<b> est_tokens=<t> pack_bytes=<p> diff_bytes=<d> over_budget=<bool>` × N，`grouping gate=… groups=… edges=… weak_cut=… cross_edges=… isolated=…`，`packs=<abs dir>`；N = 0 时 `cells=0` + `warning: no reviewable evidence …`。

## 7. 兼容与合并策略

- **批 A**（`src/cli/mod.rs` / `src/main.rs`）：本批不引用 `resolve_models`；`--pack-only` 预算走 `cfg.models.keys()`（A 合回后改 `active_roles` 一行）；`ReviewArgs` 新字段在末尾；clap flag 放 `incremental_from` 之后；`gen_job_id` 外移不改语义。后合者 rebase 一次（plan 波 1 已预告）。
- **批 B**：零交集（不碰 state / db / domain / report）。
- **批 D**：`build_bundle_prompt` 签名不变、只升级 `other_files` 语义；模板只改 `:80` 一句；D-2 拆模板时把索引槽与新句迁入 `scanner.md`。
- **批 E**：`Bundle.edges` / `hub_order` / `over_budget` 与 `fill_order()` 就位；E-2 重写 `body_section` 时沿用遍历序。
- **批 F**：F-2 `run_scan_fanout` 消费 `Grouping.bundles` 与 `others`；F-5 把 `GroupingStats` 接进 RunMeta。
- 旧 `args.json`（`--resume`）：`pack_only` serde default false。

## 8. 权衡

- 边端点用路径而非索引：跨组边要出现在两端 bundle 的 `edges` 里，路径键免去全局索引表传递。
- 索引由调用方预渲染（`others: Vec<String>`）而非在 fan-out 内计算：与 `dossiers` 同形，增量轮的 `extra_paths` / `delta` 不必穿透 fan-out 签名。
- 弱缝整分量切而非最小割：确定性 + 可解释（stats.weak_cut 可观测）；切错退化为冗余（半径 + dedup）。
- 落单文件回池装箱（K6）：牺牲「同目录即同组」的纯度，换 cell 数上界。

## 9. 回滚

每 task 一 commit，逆序 revert。分组质量事故的临时开关：cli 处 `GroupingOpts { min_files: usize::MAX, ..Default }` 即恢复旧「恒单包」行为（门口径即 E9）。
