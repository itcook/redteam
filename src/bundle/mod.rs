use std::collections::BTreeMap;
use std::path::Path;

use crate::diffsym::ref_worthy;
use crate::evidence::git::FileChange;

/// 噪声文件名（锁文件等，评审无价值）。
const NOISE_NAMES: &[&str] = &[
    "Cargo.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
    "poetry.lock", "Gemfile.lock", "composer.lock",
];

fn filename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

fn is_noise(path: &str) -> bool {
    let f = filename(path);
    NOISE_NAMES.contains(&f) || f.ends_with(".lock")
}

/// 二进制 diff 判定：git 对二进制改动只产 "Binary files … differ" 标记行（无 hunk）。
/// 标记行顶格无前缀；文本 hunk 正文行恒带 +/-/空格前缀，同款字样不会误伤。
/// Task 14 回放实证：单个 build 产物开 3 cell 烧掉 36%/1.46M input——无可审文本，筛选层消灭。
fn is_binary_diff(diff: &str) -> bool {
    diff.lines().any(|l| l.starts_with("Binary files ") && l.ends_with(" differ"))
}

/// 确定性文件筛选：丢掉空 diff、噪声（锁文件）与二进制 diff。代码决定，不交模型。
pub fn select_files(changes: Vec<FileChange>) -> Vec<FileChange> {
    changes
        .into_iter()
        .filter(|c| !c.diff.trim().is_empty())
        .filter(|c| !is_noise(&c.path))
        .filter(|c| !is_binary_diff(&c.diff))
        .collect()
}

/// 边类型（design §3.2〔D3〕）：import 强；引用边按次数计（≥ 2 强、= 1 弱，K3）；同目录弱。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    Import,
    Reference { count: u32 },
    SameDir,
}

/// 改动文件间的一条边。方向：Import = `from` 引入 `to`；Reference = `from` 的文本里出现 `to` 的
/// 改动 def 名（`from` 引用 `to`）；SameDir 无向、存一次（`from < to`）。端点用路径而非索引：跨组边
/// 要同时出现在两端 bundle 的 `edges` 里，路径键免去全局索引表传递。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub kind: EdgeKind,
}

impl Edge {
    pub fn is_strong(&self) -> bool {
        match &self.kind {
            EdgeKind::Import => true,
            EdgeKind::Reference { count } => *count >= 2,
            EdgeKind::SameDir => false,
        }
    }
}

/// 一个评审组（cell 的装包单位）。
#[derive(Clone, Debug)]
pub struct Bundle {
    /// 路径升序（cell label 稳定）。
    pub files: Vec<FileChange>,
    /// 与本组文件相关的全部边（组内 + 跨组）。
    pub edges: Vec<Edge>,
    /// `files` 的填充序（枢纽先：组内 import 边数降序、同分按路径）；空 = 自然序。
    pub hub_order: Vec<usize>,
    /// 组仍超 `max_files_per_group` / 预算（切完弱边后）：宁降不拆，交装包梯子降内容。
    pub over_budget: bool,
}

impl Bundle {
    /// 无边、自然序、不超预算——测试与二阶段增强包用。
    pub fn from_files(files: Vec<FileChange>) -> Bundle {
        Bundle { files, edges: Vec::new(), hub_order: Vec::new(), over_budget: false }
    }

    /// cell 标签：文件路径逗号串（fan-out cell id 与 `--pack-only` 共用）。
    pub fn label(&self) -> String {
        self.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(",")
    }

    /// 按填充序遍历（`hub_order` 缺失或长度不符 → 自然序）。
    pub fn fill_order(&self) -> impl Iterator<Item = &FileChange> {
        let order: Vec<usize> = if self.hub_order.len() == self.files.len() && !self.hub_order.is_empty() {
            self.hub_order.clone()
        } else {
            (0..self.files.len()).collect()
        };
        order.into_iter().map(move |i| &self.files[i])
    }
}

/// 分组旋钮（design §12.1 `[review.grouping]`，缺省 ocr 实测 4 / 200 / 10；配置解析归批 A，K1）。
/// `budget_bytes`：组估算字节（Σ 文件全文 + diff）上限，由调用方喂 pack 预算；None 不按预算判。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupingOpts {
    pub min_files: usize,
    pub bundle_line_threshold: usize,
    pub max_files_per_group: usize,
    pub budget_bytes: Option<usize>,
}

impl Default for GroupingOpts {
    fn default() -> Self {
        GroupingOpts { min_files: 4, bundle_line_threshold: 200, max_files_per_group: 10, budget_bytes: None }
    }
}

/// 双阈值门结果：`Single` = 文件数或 churn 未过门，全变更集一包。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Gate {
    Single,
    Grouped,
}

impl Gate {
    pub fn as_str(&self) -> &'static str {
        match self {
            Gate::Single => "single",
            Gate::Grouped => "grouped",
        }
    }
}

/// 分组观测（M21a 只在 `--pack-only` 打印与 summary.json；RunMeta 接线 F-5）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct GroupingStats {
    pub gate: Gate,
    pub groups: usize,
    /// 全图边数（import + 引用 + 同目录）。
    pub edges: usize,
    /// 超限分量里被切掉的弱边数。
    pub weak_cut: usize,
    /// 端点异组的边数（分组切错的先验度量，R8）。
    pub cross_edges: usize,
    /// 原图无边的文件数。
    pub isolated: usize,
    /// `over_budget` 的组数。
    pub over_budget: usize,
}

#[derive(Clone, Debug)]
pub struct Grouping {
    pub bundles: Vec<Bundle>,
    pub stats: GroupingStats,
}

/// 改动 def 名（diffsym 支持语言）：hunk 外围 def 起行经 `match_def` 取名，`ref_worthy` 过滤，去重升序。
fn changed_def_names(path: &str, diff: &str, content: &str) -> Vec<String> {
    let Some(lang) = crate::diffsym::lang_for(path) else { return Vec::new() };
    let defs = crate::diffsym::compile_defs(lang);
    let lines: Vec<&str> = content.lines().collect();
    let mut names = std::collections::BTreeSet::new();
    for (start, _end) in crate::diffsym::changed_def_ranges(diff, &lines, &defs) {
        if let Some((_, name)) = lines.get(start).and_then(|l| crate::diffsym::match_def(l, &defs)) {
            if ref_worthy(&name) {
                names.insert(name);
            }
        }
    }
    names.into_iter().collect()
}

/// 引用边扫描：改动文件数超此值时只扫对方 hunk（diff 文本）而非全文（design 允许「hunk 或全文」）。
const REF_FULLTEXT_MAX_FILES: usize = 200;

/// diff 增删行计数（排除 `+++ ` / `--- ` 文件头；裸 `---sep` 正文行计入——dogfood 20260703 r4）。
/// 门的 churn 与清单 `(+a/-d)` 共用同一口径（K2）。
pub(crate) fn count_plus_minus(diff: &str) -> (usize, usize) {
    let (mut adds, mut dels) = (0usize, 0usize);
    for l in diff.lines() {
        if l.starts_with("+++ ") || l.starts_with("--- ") {
            continue;
        }
        if l.starts_with('+') {
            adds += 1;
        } else if l.starts_with('-') {
            dels += 1;
        }
    }
    (adds, dels)
}

/// 并查集连通分量：`n` 个节点 + 边对 → 分量列表（每分量成员升序、分量按最小成员升序）。
fn components(n: usize, pairs: impl Iterator<Item = (usize, usize)>) -> Vec<Vec<usize>> {
    let mut parent: Vec<usize> = (0..n).collect();
    for (a, b) in pairs {
        union(&mut parent, a, b);
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = uf_find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    groups.into_values().collect()
}


/// 清单条目上限——超出截断为「… and N more files」（与 dismissed notes 同思路的体积防护）。
const MANIFEST_MAX_ENTRIES: usize = 200;
/// 清单总字符预算——条目数之外的直接 token 防护：每 bundle prompt 注入一份清单，
/// 大变更集下 O(bundles×manifest) 叠加，字符帽对症（dogfood 20260703 r1 6db068d2）。
const MANIFEST_MAX_CHARS: usize = 4000;
/// 截断注记（"… and N more files"）的预留额——预算含注记本身，总输出不超帽
/// （dogfood 20260703 r2 5361b545：注记后置追加曾致溢出预算）。
const MANIFEST_SUFFIX_RESERVE: usize = 40;

/// 双帽截断（条目数 + 总字符预算，先到者截断，尾注被截条数）——two_layer_manifest 的体积防线
/// （跨组索引 `render_cross_index` 用同一对帽常量，但有降质档，不走本函数）。
fn cap_entries(entries: Vec<String>) -> String {
    let total = entries.len();
    let mut kept = 0usize;
    let mut chars = 0usize;
    for e in &entries {
        if kept >= MANIFEST_MAX_ENTRIES
            || chars + e.len() + 2 > MANIFEST_MAX_CHARS - MANIFEST_SUFFIX_RESERVE
        {
            break;
        }
        chars += e.len() + 2;
        kept += 1;
    }
    let mut entries = entries;
    entries.truncate(kept);
    let mut s = entries.join(", ");
    let extra = total - kept;
    if extra > 0 {
        if s.is_empty() {
            s = format!("… and {extra} more files");
        } else {
            s.push_str(&format!(", … and {extra} more files"));
        }
    }
    s
}

/// M13 双层 manifest（决议 4）：全分支变更文件清单 + 标注哪些在本轮 delta 内——
/// 压增量轮的视野塌缩型「无证据实现」误报。体积防线复用双帽。
pub fn two_layer_manifest(all_files: &[String], delta: &std::collections::BTreeSet<String>) -> String {
    let entries: Vec<String> = all_files
        .iter()
        .map(|p| if delta.contains(p) { format!("{p} [delta]") } else { p.clone() })
        .collect();
    cap_entries(entries)
}

/// 跨组索引条目（design §10.6〔D6〕）：改动文件在哪一组、增删行数、正向（它 import / 引用的改动
/// 文件）与反向（import / 引用它的改动文件）邻居、是否在增量轮 delta 集内。同目录边不进索引。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexEntry {
    pub path: String,
    /// 所在组（0-based）；`None` = 只有路径的分支文件（增量轮非 delta，无 diff、无边）。
    pub group: Option<usize>,
    pub adds: usize,
    pub dels: usize,
    pub forward: Vec<String>,
    pub backward: Vec<String>,
    pub delta: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrossIndex {
    pub entries: Vec<IndexEntry>,
}

/// 由分组产物建索引（零 token：边集是 `group()` 的副产品）。`extra_paths` = 有路径无 diff 的文件
/// （增量轮的全分支非 delta 文件）；`delta` 为 `Some` 时按集合标 `[delta]`，`None` 不标。
pub fn cross_index(
    bundles: &[Bundle],
    extra_paths: &[String],
    delta: Option<&std::collections::BTreeSet<String>>,
) -> CrossIndex {
    let in_delta = |p: &str| delta.map(|d| d.contains(p)).unwrap_or(false);
    let mut entries = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for (g, b) in bundles.iter().enumerate() {
        for fc in &b.files {
            let (adds, dels) = count_plus_minus(&fc.diff);
            let mut forward: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            let mut backward: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for e in &b.edges {
                if e.kind == EdgeKind::SameDir {
                    continue;
                }
                if e.from == fc.path {
                    forward.insert(e.to.clone());
                }
                if e.to == fc.path {
                    backward.insert(e.from.clone());
                }
            }
            seen.insert(fc.path.clone());
            entries.push(IndexEntry {
                path: fc.path.clone(),
                group: Some(g),
                adds,
                dels,
                forward: forward.into_iter().collect(),
                backward: backward.into_iter().collect(),
                delta: in_delta(&fc.path),
            });
        }
    }
    for p in extra_paths {
        if seen.insert(p.clone()) {
            entries.push(IndexEntry {
                path: p.clone(),
                group: None,
                adds: 0,
                dels: 0,
                forward: Vec::new(),
                backward: Vec::new(),
                delta: in_delta(p),
            });
        }
    }
    CrossIndex { entries }
}

/// 为第 `for_group` 组渲染 `{{OTHER_CHANGED_FILES}}` 槽内容：本组之外的每个文件一行
/// `path (gN +a/-d) → [imports] ← [imported-by] [delta]`（空段省略；只有路径的分支文件只渲染路径）。
/// 排序：与本组有边的在前，其次按组号、路径。双帽（`MANIFEST_MAX_ENTRIES` / `MANIFEST_MAX_CHARS`）
/// 沿用：帽内先降质（`path → see group N`，不带边与计数）再尾注 `… and K more files (see groups …)`，
/// 含尾注总长不超字符帽——「降质 + 留指针」，文件名不丢。候选为空（单组且无分支文件）→ 空串。
/// 非空时以换行起头（槽位于冒号之后，多行条目各占一行）。组号 1-based，与 `cell-<n>` 对齐。
pub fn render_cross_index(idx: &CrossIndex, for_group: usize) -> String {
    let mine: std::collections::BTreeSet<&str> = idx
        .entries
        .iter()
        .filter(|e| e.group == Some(for_group))
        .map(|e| e.path.as_str())
        .collect();
    let connected = |e: &IndexEntry| e.forward.iter().chain(e.backward.iter()).any(|p| mine.contains(p.as_str()));
    let mut cands: Vec<&IndexEntry> = idx.entries.iter().filter(|e| e.group != Some(for_group)).collect();
    cands.sort_by(|a, b| {
        (!connected(a), a.group.is_none(), a.group.unwrap_or(0), &a.path)
            .cmp(&(!connected(b), b.group.is_none(), b.group.unwrap_or(0), &b.path))
    });
    if cands.is_empty() {
        return String::new();
    }
    let full = |e: &IndexEntry| -> String {
        let mut s = match e.group {
            Some(g) => format!("{} (g{} +{}/-{})", e.path, g + 1, e.adds, e.dels),
            None => e.path.clone(),
        };
        if !e.forward.is_empty() {
            s.push_str(&format!(" → [{}]", e.forward.join(", ")));
        }
        if !e.backward.is_empty() {
            s.push_str(&format!(" ← [{}]", e.backward.join(", ")));
        }
        if e.delta {
            s.push_str(" [delta]");
        }
        s
    };
    let degraded = |e: &IndexEntry| -> String {
        match e.group {
            Some(g) => format!("{} → see group {}", e.path, g + 1),
            None => e.path.clone(),
        }
    };
    let budget = MANIFEST_MAX_CHARS - MANIFEST_SUFFIX_RESERVE;
    let mut lines: Vec<String> = Vec::new();
    let mut chars = 0usize;
    let mut i = 0usize;
    for tier in [&full as &dyn Fn(&IndexEntry) -> String, &degraded] {
        while i < cands.len() && lines.len() < MANIFEST_MAX_ENTRIES {
            let l = tier(cands[i]);
            if chars + l.len() + 1 > budget {
                break;
            }
            chars += l.len() + 1;
            lines.push(l);
            i += 1;
        }
    }
    let tail_note = |omitted: &[&IndexEntry]| -> String {
        let groups: std::collections::BTreeSet<usize> = omitted.iter().filter_map(|e| e.group.map(|g| g + 1)).collect();
        let mut note = format!("… and {} more files", omitted.len());
        if !groups.is_empty() {
            let shown: Vec<String> = groups.iter().take(5).map(|g| g.to_string()).collect();
            let more = if groups.len() > 5 { ", …" } else { "" };
            note.push_str(&format!(" (see groups {}{more})", shown.join(", ")));
        }
        note
    };
    // 尾注计入总帽：放不下就再退掉尾部条目（它们并入尾注计数）。
    loop {
        let omitted = &cands[i..];
        let note = if omitted.is_empty() { None } else { Some(tail_note(omitted)) };
        let total = chars + note.as_ref().map(|n| n.len() + 1).unwrap_or(0);
        if total <= MANIFEST_MAX_CHARS || lines.is_empty() {
            if let Some(n) = note {
                lines.push(n);
            }
            break;
        }
        if let Some(last) = lines.pop() {
            chars -= last.len() + 1;
            i -= 1;
        }
    }
    format!("\n{}", lines.join("\n"))
}

/// 每组各渲染一份（与 `dossiers` 等长等序，供 fan-out / 二阶段补查按组取用）。
pub fn render_cross_indices(idx: &CrossIndex, n_groups: usize) -> Vec<String> {
    (0..n_groups).map(|g| render_cross_index(idx, g)).collect()
}

fn uf_find(parent: &mut [usize], x: usize) -> usize {
    let mut root = x;
    while parent[root] != root {
        root = parent[root];
    }
    // 路径压缩
    let mut cur = x;
    while parent[cur] != cur {
        let next = parent[cur];
        parent[cur] = root;
        cur = next;
    }
    root
}

/// union-by-min-index：指向较小代表元，使 BTreeMap 按「组内最早文件」排序，bundle 顺序稳定直觉。
fn union(parent: &mut [usize], i: usize, j: usize) {
    let ri = uf_find(parent, i);
    let rj = uf_find(parent, j);
    if ri != rj {
        parent[ri.max(rj)] = ri.min(rj);
    }
}

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

fn ext_of(path: &str) -> &str {
    let f = filename(path);
    match f.rfind('.') {
        Some(i) if i > 0 => &f[i + 1..],
        _ => "",
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

/// 目录自身到仓根的祖先链（含自身与 ""）：`a/b/c` → `[a/b/c, a/b, a, ""]`。
fn ancestors(dir: &str) -> Vec<String> {
    let mut out = vec![dir.to_string()];
    let mut cur = dir;
    while !cur.is_empty() {
        cur = dir_of(cur);
        out.push(cur.to_string());
    }
    out
}

/// 去掉 basename 最后一个扩展名；dotfile（basename 首字符即 '.'）整名保留。
fn strip_ext(p: &str) -> String {
    let slash = p.rfind('/');
    let dot = p.rfind('.');
    match (slash, dot) {
        (Some(s), Some(d)) if d > s + 1 => p[..d].to_string(), // basename 内、非首字符
        (None, Some(d)) if d > 0 => p[..d].to_string(),
        _ => p.to_string(),
    }
}

/// 改动集文件 → 模块键（去最后扩展名）。
fn norm_file(path: &str) -> String {
    strip_ext(path)
}

/// 把 base_dir 下的相对引用 rel 规范化为仓内路径（处理 `./` 与 `../`，**不**去扩展名）。
/// `..` 上溯越过仓根 → `None`（R-C1.5：映射结果不得越出 root，越出即丢，不做钳制）。
fn norm_join(base_dir: &str, rel: &str) -> Option<String> {
    let mut stack: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop()?;
            }
            s => stack.push(s),
        }
    }
    Some(stack.join("/"))
}

/// 提取一行里 needle 之后首个引号包裹的模块说明符（相对或裸，原样返回）。
fn quoted_after(line: &str, needle: &str) -> Option<String> {
    let start = line.find(needle)? + needle.len();
    let rest = &line[start..];
    let qpos = rest.find(['\'', '"'])?;
    let qchar = rest.as_bytes()[qpos] as char;
    let after = &rest[qpos + 1..];
    let end = after.find(qchar)?;
    Some(after[..end].to_string())
}

/// Python 相对 import token（前导点 + 点分模块）→ 相对路径片段；纯 "." 当前包返回 None。
fn py_rel(token: &str) -> Option<String> {
    let dots = token.chars().take_while(|&c| c == '.').count();
    if dots == 0 {
        return None;
    }
    let modpart = &token[dots..];
    let ups = dots - 1; // 1 点=当前目录，每多一点上溯一级
    if modpart.is_empty() && ups == 0 {
        return None; // 纯 "from . import x"，无具体文件
    }
    let mut rel = String::new();
    for _ in 0..ups {
        rel.push_str("../");
    }
    rel.push_str(&modpart.replace('.', "/"));
    Some(rel)
}

/// import 边：文件引入的**同在改动集里**的仓内文件（仓内相对路径，带扩展名；K14）。
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ImportEdge {
    pub to: String,
}

const TS_EXTS: &[&str] = &["ts", "tsx", "mts", "cts", "js", "jsx", "mjs", "cjs"];
/// tsconfig `extends` 链最大深度（相对路径链；包名 extends 不解析）。
const TSCONFIG_EXTENDS_MAX_DEPTH: usize = 5;

/// tsconfig 解析结果（`extends` 链已合并：父先载入、子覆盖）。
#[derive(Clone, Debug, Default)]
struct TsConfig {
    /// `compilerOptions.baseUrl`，已规范化为仓内相对目录。
    base_url: Option<String>,
    /// `compilerOptions.paths`：模式 → 目标列表（原样，`*` 通配）。
    paths: Vec<(String, Vec<String>)>,
    /// 声明 `paths` 的 tsconfig 所在目录——TS 语义：paths 相对 baseUrl；无 baseUrl 则相对声明它的配置文件。
    paths_dir: String,
}

/// 最近 `go.mod`：所在目录 + `module` 路径。
#[derive(Clone, Debug)]
struct GoModule {
    dir: String,
    module: String,
}

/// JSONC → JSON：剥 `//` / `/* */` 注释（字符串内不剥）与尾逗号，供 `serde_json` 解析 tsconfig。
fn jsonc_to_json(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut stripped = String::with_capacity(s.len());
    let mut i = 0usize;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            stripped.push(c);
            if c == '\\' && i + 1 < chars.len() {
                stripped.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                stripped.push(c);
                i += 1;
            }
            '/' if chars.get(i + 1) == Some(&'/') => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if chars.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            _ => {
                stripped.push(c);
                i += 1;
            }
        }
    }
    // 尾逗号：字符串外的 `,` 后仅空白即遇 `}` / `]` → 丢弃
    let chars: Vec<char> = stripped.chars().collect();
    let mut out = String::with_capacity(stripped.len());
    let mut i = 0usize;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < chars.len() {
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == '"' {
            in_str = true;
        } else if c == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i += 1;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Rust `use` 树展开：`a::{b, c::{d, self}}` → `[a,b]`、`[a,c,d]`、`[a,c]`；`as X` / `*` 剥除。
fn expand_use_tree(prefix: &[String], s: &str, out: &mut Vec<Vec<String>>) {
    let s = s.trim();
    if s.is_empty() {
        return;
    }
    if let Some(inner) = s.strip_prefix('{') {
        let inner = inner.strip_suffix('}').unwrap_or(inner);
        for part in split_top_level(inner) {
            expand_use_tree(prefix, part, out);
        }
        return;
    }
    let (head, rest) = match s.find('{') {
        Some(i) => (s[..i].trim().trim_end_matches("::").to_string(), Some(&s[i..])),
        None => (s.to_string(), None),
    };
    let head = head.split(" as ").next().unwrap_or("").trim().to_string();
    let mut segs: Vec<String> = prefix.to_vec();
    segs.extend(head.split("::").map(|x| x.trim().to_string()).filter(|x| !x.is_empty()));
    match rest {
        Some(r) => expand_use_tree(&segs, r, out),
        None => {
            if matches!(segs.last().map(|x| x.as_str()), Some("*") | Some("self")) {
                segs.pop();
            }
            if !segs.is_empty() {
                out.push(segs);
            }
        }
    }
}

/// 按顶层逗号切分（花括号内不切）。
fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&s[start..]);
    out
}

/// 去 `pub` / `pub(crate)` 等可见性前缀（要求 `pub` 后跟空白或 `(`，`pubsub::x` 之类标识符不剥）。
fn strip_pub(t: &str) -> &str {
    match t.strip_prefix("pub") {
        Some(r) if r.starts_with([' ', '\t', '(']) => {
            let r = r.trim_start();
            let r = if r.starts_with('(') {
                match r.find(')') {
                    Some(i) => &r[i + 1..],
                    None => r,
                }
            } else {
                r
            };
            r.trim_start()
        }
        _ => t,
    }
}

/// Go import 说明符：单行 `import "x"` 与 `import ( … )` 块（别名前缀允许，行尾注释允许）。块内只认
/// 「[别名] "spec"」整行——普通代码里的字符串字面量不算；`import ("fmt")` 单行块不进块态。
fn go_imports(content: &str) -> Vec<String> {
    fn spec_of(line: &str) -> Option<String> {
        let code = line.split("//").next().unwrap_or("").trim();
        let toks: Vec<&str> = code.split_whitespace().collect();
        let quoted = match toks.as_slice() {
            [q] => *q,
            [alias, q] if alias.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') => *q,
            _ => return None,
        };
        let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
        (!inner.is_empty() && !inner.contains('"')).then(|| inner.to_string())
    }
    let mut out = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        let t = line.trim();
        if in_block {
            if t.starts_with(')') {
                in_block = false;
                continue;
            }
            if let Some(spec) = spec_of(t.trim_end_matches(')')) {
                out.push(spec);
            }
            if t.ends_with(')') {
                in_block = false;
            }
            continue;
        }
        let Some(rest) = t.strip_prefix("import") else { continue };
        if !rest.is_empty() && !rest.starts_with([' ', '\t', '(']) {
            continue; // `imports := …` 之类
        }
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix('(') {
            if let Some(spec) = spec_of(r.trim_end_matches(')')) {
                out.push(spec);
            }
            in_block = !r.contains(')');
        } else if let Some(spec) = spec_of(rest) {
            out.push(spec);
        }
    }
    out
}

/// import 解析上下文：改动集键表 + 各语言项目根映射（tsconfig / go.mod / crate 根）惰性缓存。
/// 只读改动集外的 tsconfig.json / go.mod（root 内，路径由祖先链构造、非模型可控）；`root=None`
/// 时只做不需要磁盘的解析（相对引用、Rust `crate::` 按路径中的 `src` 段定根）。
pub struct ImportResolver<'a> {
    root: Option<&'a Path>,
    changed: std::collections::BTreeSet<String>,
    /// 去扩展名键 → 改动路径（同键可多文件：widget.ts / widget.css，按扩展名再筛）。
    keys: BTreeMap<String, Vec<String>>,
    ts_cache: std::cell::RefCell<BTreeMap<String, Option<std::rc::Rc<TsConfig>>>>,
    go_cache: std::cell::RefCell<BTreeMap<String, Option<std::rc::Rc<GoModule>>>>,
    rs_cache: std::cell::RefCell<BTreeMap<String, Option<String>>>,
}

impl<'a> ImportResolver<'a> {
    pub fn new(root: Option<&'a Path>, changed: &[String]) -> Self {
        let mut keys: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for p in changed {
            keys.entry(norm_file(p)).or_default().push(p.clone());
        }
        ImportResolver {
            root,
            changed: changed.iter().cloned().collect(),
            keys,
            ts_cache: Default::default(),
            go_cache: Default::default(),
            rs_cache: Default::default(),
        }
    }

    fn lookup(&self, key: &str, exts: &[&str]) -> Vec<String> {
        self.keys
            .get(key)
            .map(|v| v.iter().filter(|p| exts.contains(&ext_of(p))).cloned().collect())
            .unwrap_or_default()
    }

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.root?.join(rel)).ok()
    }

    fn exists(&self, rel: &str) -> bool {
        self.root.map(|r| r.join(rel).exists()).unwrap_or(false)
    }

    // ---- TS/JS ----

    fn tsconfig_for(&self, dir: &str) -> Option<std::rc::Rc<TsConfig>> {
        if let Some(c) = self.ts_cache.borrow().get(dir) {
            return c.clone();
        }
        let mut found = None;
        for anc in ancestors(dir) {
            let p = join(&anc, "tsconfig.json");
            if self.exists(&p) {
                found = self.load_tsconfig(&p, 0);
                break;
            }
        }
        self.ts_cache.borrow_mut().insert(dir.to_string(), found.clone());
        found
    }

    fn load_tsconfig(&self, rel: &str, depth: usize) -> Option<std::rc::Rc<TsConfig>> {
        if depth > TSCONFIG_EXTENDS_MAX_DEPTH {
            return None;
        }
        let text = self.read(rel)?;
        let v: serde_json::Value = serde_json::from_str(&jsonc_to_json(&text)).ok()?;
        let dir = dir_of(rel).to_string();
        let mut cfg = TsConfig::default();
        let parents: Vec<String> = match v.get("extends") {
            Some(serde_json::Value::String(s)) => vec![s.clone()],
            Some(serde_json::Value::Array(a)) => a.iter().filter_map(|x| x.as_str().map(String::from)).collect(),
            _ => Vec::new(),
        };
        for parent in parents {
            if !parent.starts_with('.') {
                continue; // 包名 extends（@tsconfig/node 等）不解析
            }
            let Some(mut prel) = norm_join(&dir, &parent) else { continue };
            if !prel.ends_with(".json") {
                prel.push_str(".json");
            }
            if let Some(pc) = self.load_tsconfig(&prel, depth + 1) {
                // 按属性合并（TS 语义）：后一个父配置只覆盖它声明了的属性，不抹掉前者的 baseUrl / paths。
                if pc.base_url.is_some() {
                    cfg.base_url = pc.base_url.clone();
                }
                if !pc.paths.is_empty() {
                    cfg.paths = pc.paths.clone();
                    cfg.paths_dir = pc.paths_dir.clone();
                }
            }
        }
        let co = v.get("compilerOptions");
        if let Some(b) = co.and_then(|c| c.get("baseUrl")).and_then(|b| b.as_str()) {
            if let Some(n) = norm_join(&dir, b) {
                cfg.base_url = Some(n);
            }
        }
        if let Some(serde_json::Value::Object(m)) = co.and_then(|c| c.get("paths")) {
            cfg.paths = m
                .iter()
                .map(|(k, v)| {
                    let targets = v
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                        .unwrap_or_default();
                    (k.clone(), targets)
                })
                .collect();
            cfg.paths_dir = dir;
        }
        Some(std::rc::Rc::new(cfg))
    }

    /// 候选（可带或不带扩展名、可为目录）→ 改动集里的 TS/JS 文件：先按原样键查（`user.service` /
    /// `app.module` 这类点号基名是键的一部分，不能剥），只有尾扩展名是 TS/JS 扩展名才剥（`./x.js` 指向
    /// x.ts），最后试 `<cand>/index`。`./styles.css` 之类非代码扩展名不再误连同名 `styles.ts`。
    fn lookup_ts(&self, cand: &str) -> Vec<String> {
        let mut v = self.lookup(cand, TS_EXTS);
        if v.is_empty() && TS_EXTS.contains(&ext_of(cand)) {
            v = self.lookup(&strip_ext(cand), TS_EXTS);
        }
        if v.is_empty() {
            v = self.lookup(&format!("{cand}/index"), TS_EXTS);
        }
        v
    }

    fn resolve_ts(&self, file_dir: &str, spec: &str) -> Vec<String> {
        if spec.starts_with('.') {
            return norm_join(file_dir, spec).map(|c| self.lookup_ts(&c)).unwrap_or_default();
        }
        if spec.starts_with('/') {
            return Vec::new();
        }
        let Some(cfg) = self.tsconfig_for(file_dir) else { return Vec::new() };
        // paths：最长前缀模式优先；精确键优先于通配。
        let mut best: Option<(usize, &str, &[String])> = None;
        for (pat, targets) in &cfg.paths {
            let score = match pat.find('*') {
                Some(i) => {
                    let (pre, suf) = (&pat[..i], &pat[i + 1..]);
                    if spec.len() >= pre.len() + suf.len() && spec.starts_with(pre) && spec.ends_with(suf) {
                        Some(pre.len())
                    } else {
                        None
                    }
                }
                None => (pat == spec).then_some(pat.len() + 1),
            };
            if let Some(sc) = score {
                if best.map(|b| sc > b.0).unwrap_or(true) {
                    best = Some((sc, pat.as_str(), targets.as_slice()));
                }
            }
        }
        let paths_base = cfg.base_url.clone().unwrap_or_else(|| cfg.paths_dir.clone());
        let mut cands: Vec<String> = Vec::new();
        if let Some((_, pat, targets)) = best {
            let star = match pat.find('*') {
                Some(i) => {
                    let suf_len = pat.len() - i - 1;
                    spec[i..spec.len() - suf_len].to_string()
                }
                None => String::new(),
            };
            for t in targets {
                if let Some(c) = norm_join(&paths_base, &t.replace('*', &star)) {
                    cands.push(c);
                }
            }
        } else if let Some(b) = &cfg.base_url {
            if let Some(c) = norm_join(b, spec) {
                cands.push(c);
            }
        }
        cands.iter().flat_map(|c| self.lookup_ts(c)).collect()
    }

    // ---- Rust ----

    /// crate 根目录：文件最近祖先 `src/` 且含 `lib.rs` / `main.rs`（workspace 子 crate 各自为根）；
    /// 无磁盘（root=None）或都不含 → 最近的 `src/` 段；没有 `src/` → None（不解析 `crate::`）。
    fn crate_root_for(&self, file: &str) -> Option<String> {
        let dir = dir_of(file).to_string();
        if let Some(c) = self.rs_cache.borrow().get(&dir) {
            return c.clone();
        }
        let ancs = ancestors(&dir);
        let mut found = None;
        if self.root.is_some() {
            found = ancs
                .iter()
                .find(|a| filename(a) == "src" && (self.exists(&join(a, "lib.rs")) || self.exists(&join(a, "main.rs"))))
                .cloned();
        }
        if found.is_none() {
            found = ancs.iter().find(|a| filename(a) == "src").cloned();
        }
        self.rs_cache.borrow_mut().insert(dir, found.clone());
        found
    }

    /// 文件的模块路径（相对 crate 根）：`src/a/b.rs` → `[a,b]`、`src/a/mod.rs` → `[a]`、根文件 → `[]`。
    fn rust_module_of(file: &str, crate_root: &str) -> Vec<String> {
        let rel = file.strip_prefix(crate_root).unwrap_or(file).trim_start_matches('/');
        let rel = rel.strip_suffix(".rs").unwrap_or(rel);
        let mut segs: Vec<String> = rel.split('/').filter(|s| !s.is_empty()).map(String::from).collect();
        match segs.last().map(|s| s.as_str()) {
            Some("mod") => {
                segs.pop();
            }
            Some("lib") | Some("main") if segs.len() == 1 => {
                segs.pop();
            }
            _ => {}
        }
        segs
    }

    /// 模块路径 → 改动集里的 `.rs`：`<root>/a/b.rs` | `<root>/a/b/mod.rs`；尾段可能是 item，按最长存在前缀回退。
    /// 根文件（lib.rs / main.rs）回退只在「基模块就是 crate 根且只剩 ≤ 1 段」时成立：`use super::Thing`
    /// （自顶层模块）/ `use crate::Thing` 指 crate 根里的 item。`super::Config` 自 `[net, client]` 指向 net
    /// 模块文件、`crate::util::helper` 指向 util 模块——找不到就是没边，不能连到改动的根文件（否则每个
    /// `mod tests { use super::*; }` 都会把文件粘到 lib.rs 上）。
    fn resolve_rust_path(&self, crate_root: &str, module: &[String], segs: &[String]) -> Vec<String> {
        let mut full: Vec<String> = Vec::new();
        let mut rest = segs;
        match rest.first().map(|s| s.as_str()) {
            Some("crate") => rest = &rest[1..],
            Some("self") => {
                rest = &rest[1..];
                full.extend_from_slice(module);
            }
            Some("super") => {
                full.extend_from_slice(module);
                while rest.first().map(|s| s.as_str()) == Some("super") {
                    rest = &rest[1..];
                    if full.pop().is_none() {
                        return Vec::new();
                    }
                }
            }
            _ => return Vec::new(), // 外部 crate / 2018 裸模块路径：不解析
        }
        let allow_root = full.is_empty() && rest.len() <= 1;
        full.extend(rest.iter().cloned());
        for k in (0..=full.len()).rev() {
            let hits: Vec<String> = if k == 0 {
                if !allow_root {
                    break;
                }
                let mut v = self.lookup(&join(crate_root, "lib"), &["rs"]);
                v.extend(self.lookup(&join(crate_root, "main"), &["rs"]));
                v
            } else {
                let cand = join(crate_root, &full[..k].join("/"));
                let mut v = self.lookup(&cand, &["rs"]);
                v.extend(self.lookup(&join(&cand, "mod"), &["rs"]));
                v
            };
            if !hits.is_empty() {
                return hits;
            }
        }
        Vec::new()
    }

    fn resolve_rust_file(&self, file: &str, content: &str) -> Vec<String> {
        let mut out = Vec::new();
        let crate_root = self.crate_root_for(file);
        let module = crate_root.as_deref().map(|r| Self::rust_module_of(file, r)).unwrap_or_default();
        // `mod x;` 的子模块目录：mod.rs / lib.rs / main.rs 用所在目录，其余文件用同名目录（2018 布局）。
        let mod_dir = match filename(file) {
            "mod.rs" | "lib.rs" | "main.rs" => dir_of(file).to_string(),
            _ => strip_ext(file),
        };
        let mut pending: Option<String> = None; // 多行 use 语句累积
        // 内联 `mod x { … }` 块栈（名, 进入时花括号深度）：块内的 `super::` 指向本文件模块自身而非文件的父模块
        // （修补批 X-2：原按文件顶层算，`mod tests { use super::*; }` 把每个顶层模块粘到 lib.rs）。行级花括号计数是
        // 启发式（字符串 / 注释里的括号会失准），最坏只退回旧行为。上一行的括号变化在本行开始时生效。
        let mut inline_mods: Vec<(String, usize)> = Vec::new();
        let mut depth: usize = 0;
        let (mut prev_opens, mut prev_closes) = (0usize, 0usize);
        for line in content.lines() {
            depth = (depth + prev_opens).saturating_sub(prev_closes);
            while inline_mods.last().is_some_and(|(_, d)| depth <= *d) {
                inline_mods.pop();
            }
            // 可见性前缀只在语句起始行剥；多行 use 的续行（`pubsub::Broker,`）原样累积。
            let raw = line.trim();
            (prev_opens, prev_closes) = (raw.matches('{').count(), raw.matches('}').count());
            let t = if pending.is_none() { strip_pub(raw) } else { raw };
            if pending.is_none() {
                if let Some(name) = t.strip_prefix("mod ").and_then(|r| r.strip_suffix('{')).map(str::trim) {
                    if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                        inline_mods.push((name.to_string(), depth));
                    }
                }
            }
            let stmt = match pending.take() {
                Some(mut acc) => {
                    acc.push(' ');
                    acc.push_str(t);
                    acc
                }
                None => match t.strip_prefix("use ") {
                    Some(rest) => rest.to_string(),
                    None => {
                        if let Some(rest) = t.strip_prefix("mod ") {
                            if let Some(name) = rest.strip_suffix(';') {
                                let name = name.trim();
                                if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                                    out.extend(self.lookup(&join(&mod_dir, name), &["rs"]));
                                    out.extend(self.lookup(&join(&join(&mod_dir, name), "mod"), &["rs"]));
                                }
                            }
                        }
                        continue;
                    }
                },
            };
            let Some(end) = stmt.find(';') else {
                pending = Some(stmt);
                continue;
            };
            let Some(root) = crate_root.as_deref() else { continue };
            let mut paths = Vec::new();
            expand_use_tree(&[], &stmt[..end], &mut paths);
            let module_here: Vec<String> = module.iter().cloned().chain(inline_mods.iter().map(|(n, _)| n.clone())).collect();
            for p in paths {
                out.extend(self.resolve_rust_path(root, &module_here, &p));
            }
        }
        out
    }

    // ---- Go ----

    fn gomod_for(&self, dir: &str) -> Option<std::rc::Rc<GoModule>> {
        if let Some(c) = self.go_cache.borrow().get(dir) {
            return c.clone();
        }
        let mut found = None;
        for anc in ancestors(dir) {
            if let Some(text) = self.read(&join(&anc, "go.mod")) {
                let module = text
                    .lines()
                    .find_map(|l| l.trim().strip_prefix("module "))
                    .map(|m| m.trim().trim_matches('"').to_string());
                if let Some(m) = module {
                    found = Some(std::rc::Rc::new(GoModule { dir: anc, module: m }));
                    break;
                }
            }
        }
        self.go_cache.borrow_mut().insert(dir.to_string(), found.clone());
        found
    }

    fn resolve_go(&self, file: &str, imp: &str) -> Vec<String> {
        let Some(gm) = self.gomod_for(dir_of(file)) else { return Vec::new() };
        let (gdir, module) = (&gm.dir, &gm.module);
        let sub = if imp == module {
            ""
        } else if let Some(s) = imp.strip_prefix(&format!("{module}/")) {
            s
        } else {
            return Vec::new(); // 标准库 / 外部模块
        };
        let Some(dir) = norm_join(gdir, sub) else { return Vec::new() };
        self.changed
            .iter()
            .filter(|p| dir_of(p) == dir && ext_of(p) == "go" && p.as_str() != file)
            .cloned()
            .collect()
    }
}

/// 文件的 import 边（目标 ∈ 改动集，去重、路径序、不含自身）：
/// TS/JS 相对 + 裸模块（tsconfig `baseUrl` / `paths` / `extends`）；Rust `use crate::` / `super::` /
/// `self::` + `mod x;`；Go `import` 按 `go.mod` module 映射到目录（目录内任一改动 .go 即成边）；
/// Python 相对 import。解析不到（外部依赖、无映射、越出 root）即丢。
pub fn import_edges(path: &str, content: &str, r: &ImportResolver<'_>) -> Vec<ImportEdge> {
    let base = dir_of(path);
    let mut targets: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    match ext_of(path) {
        e if TS_EXTS.contains(&e) => {
            for line in content.lines() {
                for needle in ["from ", "require(", "import("] {
                    if let Some(spec) = quoted_after(line, needle) {
                        targets.extend(r.resolve_ts(base, &spec));
                    }
                }
            }
        }
        "py" => {
            for line in content.lines() {
                let t = line.trim_start();
                if let Some(rest) = t.strip_prefix("from ") {
                    let token = rest.split_whitespace().next().unwrap_or("");
                    if let Some(rel) = py_rel(token) {
                        if let Some(c) = norm_join(base, &rel) {
                            let mut v = r.lookup(&c, &["py"]);
                            if v.is_empty() {
                                v = r.lookup(&format!("{c}/__init__"), &["py"]);
                            }
                            targets.extend(v);
                        }
                    }
                }
            }
        }
        "rs" => targets.extend(r.resolve_rust_file(path, content)),
        "go" => {
            for imp in go_imports(content) {
                targets.extend(r.resolve_go(path, &imp));
            }
        }
        _ => {}
    }
    targets.remove(path);
    targets.into_iter().map(|to| ImportEdge { to }).collect()
}

/// 确定性引用图分组（design §3.2〔D3〕）：边 = import（强）+ 引用（按次数）+ 同目录（弱）→
/// 双阈值门（文件数 / churn 未过 → 单包）→ 连通分量 → 超限分量先整体切弱边（K3）→ 仍超标
/// `over_budget`、宁降不拆 → 孤立与落单文件按目录 bin-packing（K6）→ 组按首文件路径升序，
/// 组内 `hub_order` 按 import 边数降序（枢纽保全文）。`root=None` 时无全文：只有同目录边。
/// 安全不变量：路径来自 git diff（仓内相对、非模型可控），`root.join` 直读；读失败即降级为无全文。
pub fn group(mut changes: Vec<FileChange>, root: Option<&Path>, opts: &GroupingOpts) -> Grouping {
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let n = changes.len();
    let empty_stats = |gate: Gate| GroupingStats {
        gate, groups: 0, edges: 0, weak_cut: 0, cross_edges: 0, isolated: 0, over_budget: 0,
    };
    if n == 0 {
        return Grouping { bundles: Vec::new(), stats: empty_stats(Gate::Single) };
    }
    let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
    let idx: BTreeMap<&str, usize> = paths.iter().enumerate().map(|(i, p)| (p.as_str(), i)).collect();
    let contents: Vec<Option<String>> = match root {
        Some(r) => changes.iter().map(|c| std::fs::read_to_string(r.join(&c.path)).ok()).collect(),
        None => vec![None; n],
    };

    // ---- 边 ----
    let mut edges: Vec<Edge> = Vec::new();
    let resolver = ImportResolver::new(root, &paths);
    for i in 0..n {
        let Some(content) = &contents[i] else { continue };
        for e in import_edges(&paths[i], content, &resolver) {
            if let Some(&j) = idx.get(e.to.as_str()) {
                if i != j {
                    edges.push(Edge { from: paths[i].clone(), to: paths[j].clone(), kind: EdgeKind::Import });
                }
            }
        }
    }
    // 引用边：全部文件的改动 def 名合成一个 regex，每个文件只扫一遍，命中按名反查归属（O(n × size)，
    // 而非每对文件一次全文 regex）。
    let mut owners: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let Some(content) = &contents[i] else { continue };
        for name in changed_def_names(&paths[i], &changes[i].diff, content) {
            owners.entry(name).or_default().push(i);
        }
    }
    if !owners.is_empty() {
        let pat = format!(r"\b(?:{})\b", owners.keys().map(|s| regex::escape(s)).collect::<Vec<_>>().join("|"));
        if let Ok(re) = regex::Regex::new(&pat) {
            let full_text = n <= REF_FULLTEXT_MAX_FILES;
            let mut counts: BTreeMap<(usize, usize), u32> = BTreeMap::new(); // (引用方 j, 被引用 i)
            for j in 0..n {
                let text: &str = if full_text {
                    contents[j].as_deref().unwrap_or(&changes[j].diff)
                } else {
                    &changes[j].diff
                };
                for m in re.find_iter(text) {
                    if let Some(is) = owners.get(m.as_str()) {
                        // 修补批 X-2：j 自身也定义同名 changed def 时，这些命中是它自己的定义与内部调用，不是对别人的引用
                        // （trait 方法 / Go `Validate()` / TS 同名类方法会把互不相关的文件粘成无法切割的强边）。
                        if is.contains(&j) {
                            continue;
                        }
                        for &i in is {
                            if i != j {
                                *counts.entry((j, i)).or_insert(0) += 1;
                            }
                        }
                    }
                }
            }
            for ((j, i), count) in counts {
                edges.push(Edge { from: paths[j].clone(), to: paths[i].clone(), kind: EdgeKind::Reference { count } });
            }
        }
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if dir_of(&paths[i]) == dir_of(&paths[j]) {
                edges.push(Edge { from: paths[i].clone(), to: paths[j].clone(), kind: EdgeKind::SameDir });
            }
        }
    }
    let pair = |e: &Edge| (idx[e.from.as_str()], idx[e.to.as_str()]);
    let est = |ids: &[usize]| -> usize {
        ids.iter().map(|&i| contents[i].as_ref().map(|c| c.len()).unwrap_or(0) + changes[i].diff.len()).sum()
    };
    let oversize = |ids: &[usize]| {
        ids.len() > opts.max_files_per_group || opts.budget_bytes.map(|b| est(ids) > b).unwrap_or(false)
    };
    let make_bundle = |ids: &[usize], over_budget: bool| -> Bundle {
        let set: std::collections::BTreeSet<usize> = ids.iter().copied().collect();
        let files: Vec<FileChange> = ids.iter().map(|&i| changes[i].clone()).collect();
        let edges_g: Vec<Edge> = edges
            .iter()
            .filter(|e| {
                let (a, b) = pair(e);
                set.contains(&a) || set.contains(&b)
            })
            .cloned()
            .collect();
        let pos = |g: usize| ids.iter().position(|&x| x == g).unwrap_or(0);
        let mut score = vec![0usize; ids.len()];
        for e in &edges_g {
            if e.kind == EdgeKind::Import {
                let (a, b) = pair(e);
                if set.contains(&a) && set.contains(&b) {
                    score[pos(a)] += 1;
                    score[pos(b)] += 1;
                }
            }
        }
        let mut hub_order: Vec<usize> = (0..ids.len()).collect();
        hub_order.sort_by(|&x, &y| score[y].cmp(&score[x]).then_with(|| files[x].path.cmp(&files[y].path)));
        Bundle { files, edges: edges_g, hub_order, over_budget }
    };
    let mut degree = vec![0usize; n];
    for e in &edges {
        let (a, b) = pair(e);
        degree[a] += 1;
        degree[b] += 1;
    }
    let isolated = degree.iter().filter(|&&d| d == 0).count();

    // ---- 门 ----
    let churn: usize = changes
        .iter()
        .map(|c| {
            let (a, d) = count_plus_minus(&c.diff);
            a + d
        })
        .sum();
    if n < opts.min_files || churn < opts.bundle_line_threshold {
        let all: Vec<usize> = (0..n).collect();
        let bundle = make_bundle(&all, oversize(&all));
        let stats = GroupingStats {
            gate: Gate::Single, groups: 1, edges: edges.len(), weak_cut: 0, cross_edges: 0, isolated,
            over_budget: bundle.over_budget as usize,
        };
        return Grouping { bundles: vec![bundle], stats };
    }

    // ---- 分量 → 弱缝切 → 落单回池 ----
    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut pool: Vec<usize> = Vec::new();
    let mut weak_cut = 0usize;
    for comp in components(n, edges.iter().map(pair)) {
        if comp.len() == 1 {
            pool.push(comp[0]);
            continue;
        }
        if !oversize(&comp) {
            groups.push(comp);
            continue;
        }
        let set: std::collections::BTreeSet<usize> = comp.iter().copied().collect();
        let inside: Vec<&Edge> = edges
            .iter()
            .filter(|e| {
                let (a, b) = pair(e);
                set.contains(&a) && set.contains(&b)
            })
            .collect();
        weak_cut += inside.iter().filter(|e| !e.is_strong()).count();
        // 强边子分量：comp 内局部索引做并查集，再映射回全局索引
        let local: BTreeMap<usize, usize> = comp.iter().enumerate().map(|(k, &g)| (g, k)).collect();
        let strong_pairs = inside.iter().filter(|e| e.is_strong()).map(|e| {
            let (a, b) = pair(e);
            (local[&a], local[&b])
        });
        for sub in components(comp.len(), strong_pairs) {
            let sub: Vec<usize> = sub.iter().map(|&k| comp[k]).collect();
            if sub.len() == 1 {
                pool.push(sub[0]);
            } else {
                groups.push(sub);
            }
        }
    }
    // 孤立 / 落单文件：按（目录，路径）排序后贪心装箱到文件数与预算上限（K6）。
    pool.sort_by(|&a, &b| (dir_of(&paths[a]), &paths[a]).cmp(&(dir_of(&paths[b]), &paths[b])));
    let mut bin: Vec<usize> = Vec::new();
    for i in pool {
        let fits = bin.len() < opts.max_files_per_group
            && opts.budget_bytes.map(|b| est(&bin) + est(&[i]) <= b).unwrap_or(true);
        if !bin.is_empty() && !fits {
            groups.push(std::mem::take(&mut bin));
        }
        bin.push(i);
    }
    if !bin.is_empty() {
        groups.push(bin);
    }
    for g in &mut groups {
        g.sort_unstable();
    }
    groups.sort_by_key(|g| g[0]);

    let mut group_of = vec![0usize; n];
    for (gi, g) in groups.iter().enumerate() {
        for &i in g {
            group_of[i] = gi;
        }
    }
    let cross_edges = edges
        .iter()
        .filter(|e| {
            let (a, b) = pair(e);
            group_of[a] != group_of[b]
        })
        .count();
    let bundles: Vec<Bundle> = groups.iter().map(|g| make_bundle(g, oversize(g))).collect();
    let stats = GroupingStats {
        gate: Gate::Grouped,
        groups: bundles.len(),
        edges: edges.len(),
        weak_cut,
        cross_edges,
        isolated,
        over_budget: bundles.iter().filter(|b| b.over_budget).count(),
    };
    Grouping { bundles, stats }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fc(path: &str, diff: &str) -> FileChange {
        FileChange { path: path.into(), diff: diff.into() }
    }

    #[test]
    fn select_keeps_normal_code_files() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("src/b.rs", "+y")]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn select_drops_lockfiles() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("Cargo.lock", "+dep"), fc("ui/yarn.lock", "+pkg")]);
        let paths: Vec<&str> = out.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs"]);
    }

    #[test]
    fn select_drops_empty_diff() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("src/blank.rs", "   ")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "src/a.rs");
    }

    #[test]
    fn select_drops_binary_diffs_but_not_lookalike_content() {
        // Task 14 回放实证：build 产物二进制被开 3 cell 烧掉 36%/1.46M input——
        // 二进制 diff 无可审文本，必须在筛选层消灭。
        let bin = "diff --git a/plugins/redteam/bin/darwin/aarch64/redteam b/plugins/redteam/bin/darwin/aarch64/redteam\n\
                   index 1111111..2222222 100755\n\
                   Binary files a/plugins/redteam/bin/darwin/aarch64/redteam and b/plugins/redteam/bin/darwin/aarch64/redteam differ\n";
        // 正文里出现同款字样的行带 +/-/空格前缀 → 不得误伤文本文件
        let lookalike = "@@ -1,2 +1,3 @@\n+// Binary files a and b differ\n context\n";
        let out = select_files(vec![
            fc("src/a.rs", "+x"),
            fc("plugins/redteam/bin/darwin/aarch64/redteam", bin),
            fc("src/doc.rs", lookalike),
        ]);
        let paths: Vec<&str> = out.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs", "src/doc.rs"]);
    }

    fn edges(path: &str, src: &str, changed: &[&str], root: Option<&Path>) -> Vec<String> {
        let changed: Vec<String> = changed.iter().map(|s| s.to_string()).collect();
        let r = ImportResolver::new(root, &changed);
        import_edges(path, src, &r).into_iter().map(|e| e.to).collect()
    }

    fn write(root: &Path, rel: &str, content: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    /// 修补批 X-2：顶层模块内联 `mod tests { use super::*; }` 的 super 指向自身，不连 lib.rs；文件顶层的 `use super::X` 仍指 crate 根；
    /// 嵌套模块两种写法各归其位。
    #[test]
    fn import_edges_rust_inline_mod_super_points_to_self_not_root() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/lib.rs", "pub mod a;\npub mod b;\npub struct Thing;\n");
        write(dir.path(), "src/a.rs", "pub fn f() {}\n");
        write(dir.path(), "src/b/mod.rs", "pub fn g() {}\npub mod c;\n");
        write(dir.path(), "src/b/c.rs", "pub fn h() {}\n");
        let root = Some(dir.path());
        let inline = "pub fn f() {}\n#[cfg(test)]\nmod tests {\n    use super::*;\n    #[test]\n    fn t() { f(); }\n}\n";
        assert!(edges("src/a.rs", inline, &["src/a.rs", "src/lib.rs"], root).is_empty(), "内联 mod 里的 super = 自身，不成边");
        assert_eq!(edges("src/a.rs", "use super::Thing;\n", &["src/a.rs", "src/lib.rs"], root), vec!["src/lib.rs"], "文件顶层 super 仍指 crate 根");
        assert_eq!(edges("src/b/c.rs", "use super::g;\n", &["src/b/c.rs", "src/b/mod.rs"], root), vec!["src/b/mod.rs"]);
        assert!(edges("src/b/c.rs", "mod tests {\n    use super::*;\n}\n", &["src/b/c.rs", "src/b/mod.rs"], root).is_empty());
        // 块结束后恢复：同文件后续的顶层 `use super::Thing` 照旧指 crate 根
        let after = "mod tests {\n    use super::*;\n}\nuse super::Thing;\n";
        assert_eq!(edges("src/a.rs", after, &["src/a.rs", "src/lib.rs"], root), vec!["src/lib.rs"]);
    }

    #[test]
    fn import_edges_ts_relative() {
        let src = "import {a} from '../util/b';\nconst c = require('./helper');\nimport x from 'react';\nexport * from './styles.css';\nimport {S} from './user.service';\nimport {m} from './mod.js';\n";
        let changed = ["src/feat/x.ts", "src/feat/helper.ts", "src/util/b.tsx", "src/feat/styles.css", "src/feat/styles.ts",
            "src/feat/user.service.ts", "src/feat/user.ts", "src/feat/mod.ts"];
        let got = edges("src/feat/x.ts", src, &changed, None);
        // '../util/b' → src/util/b.tsx；'./helper' → src/feat/helper.ts；'react' 裸模块无 tsconfig → 丢；
        // './styles.css' 非代码扩展名不成边、也不误连同名 styles.ts；'./user.service' 点号基名精确匹配、不误连 user.ts；
        // './mod.js'（ESM 写法指向 .ts）剥 js 扩展名后命中 mod.ts
        assert_eq!(got, vec!["src/feat/helper.ts".to_string(), "src/feat/mod.ts".into(), "src/feat/user.service.ts".into(), "src/util/b.tsx".into()]);
    }

    #[test]
    fn import_edges_ts_bare_via_paths_wildcard_and_exact() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "tsconfig.json", r#"{"compilerOptions":{"baseUrl":".","paths":{"@app/*":["src/app/*"],"@lib":["src/lib/index.ts"],"@app/legacy/*":["src/legacy/*"]}}}"#);
        let src = "import {svc} from '@app/svc/auth';\nimport lib from '@lib';\nimport old from '@app/legacy/thing';\nimport z from 'zod';\n";
        let changed = ["src/feat/x.ts", "src/app/svc/auth.ts", "src/lib/index.ts", "src/legacy/thing.ts", "src/app/legacy/thing.ts"];
        let got = edges("src/feat/x.ts", src, &changed, Some(dir.path()));
        // 通配替换；精确键 `@lib` 直指 index.ts；最长前缀模式 `@app/legacy/*` 优先于 `@app/*`；zod 无映射丢
        assert_eq!(got, vec!["src/app/svc/auth.ts".to_string(), "src/legacy/thing.ts".into(), "src/lib/index.ts".into()]);
    }

    #[test]
    fn import_edges_ts_base_url_only_and_dir_index() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "tsconfig.json", r#"{"compilerOptions":{"baseUrl":"src"}}"#);
        let src = "import {b} from 'util/b';\nimport {m} from 'models';\n";
        let got = edges("src/feat/x.ts", src, &["src/feat/x.ts", "src/util/b.ts", "src/models/index.ts"], Some(dir.path()));
        assert_eq!(got, vec!["src/models/index.ts".to_string(), "src/util/b.ts".into()]);
    }

    #[test]
    fn import_edges_ts_extends_chain_and_nearest_tsconfig() {
        let dir = tempfile::tempdir().unwrap();
        // 根 base 配置定义 baseUrl + paths；子包 tsconfig 只 extends（省略 .json 后缀）
        write(dir.path(), "tsconfig.base.json", r#"{"compilerOptions":{"baseUrl":".","paths":{"~/*":["packages/web/src/*"]}}}"#);
        write(dir.path(), "packages/web/tsconfig.json", r#"{"extends":"../../tsconfig.base","compilerOptions":{"strict":true}}"#);
        let got = edges("packages/web/src/a.ts", "import {b} from '~/b';\n", &["packages/web/src/a.ts", "packages/web/src/b.ts"], Some(dir.path()));
        assert_eq!(got, vec!["packages/web/src/b.ts".to_string()]);
        // 数组 extends（TS 5）：后一个父配置只设 strict，不得抹掉前一个父配置的 baseUrl / paths
        write(dir.path(), "packages/api/tsconfig.paths.json", r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#);
        write(dir.path(), "packages/api/tsconfig.strict.json", r#"{"compilerOptions":{"strict":true}}"#);
        write(dir.path(), "packages/api/tsconfig.json", r#"{"extends":["./tsconfig.paths.json","./tsconfig.strict.json"]}"#);
        let got = edges("packages/api/src/a.ts", "import {b} from '@/b';\n", &["packages/api/src/a.ts", "packages/api/src/b.ts"], Some(dir.path()));
        assert_eq!(got, vec!["packages/api/src/b.ts".to_string()], "数组 extends 按属性合并");
    }

    #[test]
    fn import_edges_ts_jsonc_tolerant() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "tsconfig.json", "{\n  // 注释\n  \"compilerOptions\": {\n    \"baseUrl\": \".\", /* 块注释 */\n    \"paths\": {\n      \"@/*\": [\"src/*\",],\n    },\n  },\n}\n");
        let got = edges("src/a.ts", "import {b} from '@/b';\n", &["src/a.ts", "src/b.ts"], Some(dir.path()));
        assert_eq!(got, vec!["src/b.ts".to_string()]);
        assert_eq!(jsonc_to_json(r#"{"a": "x // not comment", "b": [1,2,],}"#), r#"{"a": "x // not comment", "b": [1,2]}"#);
    }

    #[test]
    fn import_edges_ts_unresolvable_dropped() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "tsconfig.json", r#"{"compilerOptions":{"paths":{"@app/*":["src/app/*"]}}}"#);
        let src = "import a from 'lodash';\nimport b from '@app/missing';\nimport c from '/abs/path';\n";
        let got = edges("src/x.ts", src, &["src/x.ts", "lodash.ts", "src/app/other.ts"], Some(dir.path()));
        assert!(got.is_empty(), "无映射 / 目标不在改动集 / 绝对路径 → 不成边: {got:?}");
        // 坏 JSON 的 tsconfig → 视为无映射（静默降级）
        write(dir.path(), "tsconfig.json", "{ not json");
        let got = edges("src/y.ts", "import b from '@app/other';\n", &["src/y.ts", "src/app/other.ts"], Some(dir.path()));
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn import_edges_rust_use_crate_super_self_and_external_dropped() {
        let src = "use crate::util::helper;\npub use super::sibling::{X, Y as Z};\nuse self::inner::*;\nuse std::fmt;\nuse serde::Serialize;\nuse super::super::top;\n";
        let changed = ["src/a/b.rs", "src/util/helper.rs", "src/a/sibling.rs", "src/a/b/inner.rs", "src/top.rs", "src/lib.rs"];
        // root=None：crate 根按路径中的 `src` 段定；模块 [a,b]
        let got = edges("src/a/b.rs", src, &changed, None);
        assert_eq!(got, vec!["src/a/b/inner.rs".to_string(), "src/a/sibling.rs".into(), "src/top.rs".into(), "src/util/helper.rs".into()]);
    }

    #[test]
    fn import_edges_rust_mod_rs_item_tail_and_multiline() {
        let src = "pub mod token;\nmod helper;\nuse crate::util::helper::Item;\nuse crate::util::{other, third::Thing};\nuse crate::{\n    pubsub::Broker,\n    x::One,\n    y,\n};\npub(crate) use crate::publish::sink;\nuse crate::nothing::Here;\n";
        let changed = ["src/auth/mod.rs", "src/auth/token.rs", "src/auth/helper/mod.rs", "src/util/helper.rs", "src/util/other/mod.rs", "src/util/third.rs", "src/x.rs", "src/y.rs", "src/lib.rs", "src/pubsub.rs", "src/publish.rs"];
        let got = edges("src/auth/mod.rs", src, &changed, None);
        // mod 声明 → 同目录子模块（.rs 或 /mod.rs）；`Item` / `Thing` 尾段是 item → 回退到最长存在前缀；
        // 多行 use 累积到 `;`（续行 `pubsub::Broker,` 不被 pub 前缀剥除）；`publish` 单行同理；
        // `crate::nothing::Here` 多段不存在 → 无边、不回退到 lib.rs
        assert_eq!(got, vec![
            "src/auth/helper/mod.rs".to_string(), "src/auth/token.rs".into(), "src/publish.rs".into(), "src/pubsub.rs".into(),
            "src/util/helper.rs".into(), "src/util/other/mod.rs".into(), "src/util/third.rs".into(), "src/x.rs".into(), "src/y.rs".into(),
        ]);
        // 非 mod.rs 文件的 `mod x;` 指向同名目录（2018 布局）
        let got = edges("src/auth.rs", "mod helper;\n", &["src/auth.rs", "src/auth/helper.rs", "src/helper.rs"], None);
        assert_eq!(got, vec!["src/auth/helper.rs".to_string()]);
    }

    #[test]
    fn import_edges_rust_root_fallback_only_when_parent_is_crate_root() {
        // [net, client] 的 `super::Config` / `super::*` 指向 net 模块文件（未改动）→ 无边，绝不连到改动的 lib.rs
        let src = "use super::Config;\nuse super::*;\n#[cfg(test)]\nmod tests {\n    use super::*;\n}\n";
        let got = edges("src/net/client.rs", src, &["src/net/client.rs", "src/lib.rs", "src/db/store.rs"], None);
        assert!(got.is_empty(), "{got:?}");
        // 顶层模块 [a] 的 `super::Thing` / `crate::Thing`：父模块即 crate 根 → lib.rs 合法
        let got = edges("src/a.rs", "use super::Thing;\n", &["src/a.rs", "src/lib.rs"], None);
        assert_eq!(got, vec!["src/lib.rs".to_string()]);
        let got = edges("src/a.rs", "use crate::Thing;\nuse crate::util::helper;\n", &["src/a.rs", "src/lib.rs"], None);
        assert_eq!(got, vec!["src/lib.rs".to_string()], "多段路径不回退到根文件");
    }

    #[test]
    fn import_edges_rust_workspace_sub_crate_root() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "crates/foo/src/lib.rs", "pub mod a;\npub mod b;\n");
        write(dir.path(), "crates/foo/src/a.rs", "use crate::b::thing;\n");
        write(dir.path(), "crates/foo/src/b.rs", "pub fn thing() {}\n");
        write(dir.path(), "src/main.rs", "fn main() {}\n");
        let changed = ["crates/foo/src/a.rs", "crates/foo/src/b.rs", "src/b.rs"];
        let got = edges("crates/foo/src/a.rs", "use crate::b::thing;\n", &changed, Some(dir.path()));
        // crate 根 = crates/foo/src（含 lib.rs），不是顶层 src/
        assert_eq!(got, vec!["crates/foo/src/b.rs".to_string()]);
    }

    #[test]
    fn import_edges_go_module_dir_single_and_block() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "go.mod", "module example.com/svc\n\ngo 1.22\n");
        let src = "package api\n\nimport \"example.com/svc/internal/store\"\n\nimport (\n\t\"fmt\"\n\ts \"example.com/svc/pkg/util\"\n\t\"github.com/x/y\"\n)\n";
        let changed = ["internal/api/handler.go", "internal/store/store.go", "internal/store/repo.go", "pkg/util/u.go", "pkg/util/u_test.go", "internal/store/README.md"];
        let got = edges("internal/api/handler.go", src, &changed, Some(dir.path()));
        // 包目录内每个改动 .go 各成一边（含测试文件）；fmt / 外部模块丢；非 .go 不算
        assert_eq!(got, vec!["internal/store/repo.go".to_string(), "internal/store/store.go".into(), "pkg/util/u.go".into(), "pkg/util/u_test.go".into()]);
        // 无 go.mod → 不解析
        let dir2 = tempfile::tempdir().unwrap();
        assert!(edges("internal/api/handler.go", src, &changed, Some(dir2.path())).is_empty());
        // 单行块 `import ("fmt")` 不进块态：其后代码里的字符串字面量不算 import；块内注释行与行尾注释允许
        let src2 = "package api\n\nimport (\"fmt\")\n\nfunc f() {\n\tlog.Printf(\"example.com/svc/internal/store failed\")\n}\n\nimport (\n\t// 注释行\n\ts \"example.com/svc/pkg/util\" // 别名 + 行尾注释\n\t\"github.com/x/y\")\n";
        let got = edges("internal/api/handler.go", src2, &changed, Some(dir.path()));
        assert_eq!(got, vec!["pkg/util/u.go".to_string(), "pkg/util/u_test.go".into()], "{got:?}");
        assert_eq!(go_imports("import (\n\t\"a\"\n)\nx := \"b\"\n"), vec!["a".to_string()]);
    }

    #[test]
    fn import_edges_never_escape_root() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "tsconfig.json", r#"{"compilerOptions":{"baseUrl":".","paths":{"@out/*":["../../outside/*"]}}}"#);
        let src = "import a from '../../../x';\nimport b from '@out/x';\n";
        let got = edges("a/b.ts", src, &["a/b.ts", "x.ts", "outside/x.ts"], Some(dir.path()));
        assert!(got.is_empty(), "`..` 越出仓根 → 丢而非钳制: {got:?}");
        assert_eq!(norm_join("a/b", "../c"), Some("a/c".to_string()));
        assert_eq!(norm_join("a", "../../c"), None);
    }

    #[test]
    fn import_edges_python_relative() {
        let src = "from .models import User\nfrom ..db.repo import Repo\nfrom .pkg import x\nimport os\n";
        let got = edges("pkg/api/view.py", src, &["pkg/api/view.py", "pkg/api/models.py", "pkg/db/repo.py", "pkg/api/pkg/__init__.py"], None);
        // '.models' → pkg/api/models.py；'..db.repo' 上溯一级 → pkg/db/repo.py；'.pkg' 目录包 → __init__.py；'os' 忽略
        assert_eq!(got, vec!["pkg/api/models.py".to_string(), "pkg/api/pkg/__init__.py".into(), "pkg/db/repo.py".into()]);
    }

    /// 过门旋钮（文件数 / churn 门关掉，只看图算法）。
    fn open_gate(max_files: usize, budget: Option<usize>) -> GroupingOpts {
        GroupingOpts { min_files: 1, bundle_line_threshold: 0, max_files_per_group: max_files, budget_bytes: budget }
    }

    fn paths_of(b: &Bundle) -> Vec<&str> {
        b.files.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn group_empty_input_is_empty() {
        let g = group(vec![], None, &GroupingOpts::default());
        assert!(g.bundles.is_empty());
        assert_eq!(g.stats.groups, 0);
    }

    #[test]
    fn group_gate_single_bundle_keeps_edges() {
        // 文件数 < min_files → 单包，但边照算（跨组索引与观测要用）
        let g = group(vec![fc("src/a.rs", "+a"), fc("src/b.rs", "+b"), fc("src/c.rs", "+c")], None, &GroupingOpts::default());
        assert_eq!(g.bundles.len(), 1);
        assert_eq!(g.stats.gate, Gate::Single);
        assert_eq!(g.bundles[0].edges.len(), 3, "同目录三边: {:?}", g.bundles[0].edges);
        assert_eq!(g.bundles[0].hub_order.len(), 3);
        assert!(!g.bundles[0].over_budget);
        // 文件数够但 churn < bundle_line_threshold → 仍单包
        let five: Vec<FileChange> = (0..5).map(|i| fc(&format!("d{i}/f.rs"), "+x\n-y")).collect();
        let g = group(five.clone(), None, &GroupingOpts::default());
        assert_eq!((g.stats.gate, g.bundles.len()), (Gate::Single, 1));
        // churn 过门（每文件 40 增删行 × 5 = 200）→ 分组
        let big = "+x\n".repeat(40);
        let five: Vec<FileChange> = (0..5).map(|i| fc(&format!("d{i}/f.rs"), &big)).collect();
        let g = group(five, None, &GroupingOpts::default());
        assert_eq!(g.stats.gate, Gate::Grouped);
        assert_eq!(g.stats.isolated, 5, "五个不同目录、无全文 → 全孤立");
    }

    #[test]
    fn group_two_components_by_same_dir() {
        let g = group(
            vec![fc("a/1.rs", "+x"), fc("a/2.rs", "+x"), fc("b/1.rs", "+x"), fc("b/2.rs", "+x")],
            None,
            &open_gate(10, None),
        );
        assert_eq!(g.bundles.len(), 2);
        assert_eq!(paths_of(&g.bundles[0]), vec!["a/1.rs", "a/2.rs"]);
        assert_eq!(paths_of(&g.bundles[1]), vec!["b/1.rs", "b/2.rs"]);
        assert_eq!(g.stats, GroupingStats { gate: Gate::Grouped, groups: 2, edges: 2, weak_cut: 0, cross_edges: 0, isolated: 0, over_budget: 0 });
    }

    #[test]
    fn group_unions_by_import_across_dirs() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/feat/x.ts", "import {b} from '../../util/b';\n");
        write(dir.path(), "util/b.ts", "export const b = 1;\n");
        write(dir.path(), "zzz/c.py", "c = 1\n");
        write(dir.path(), "zzz/d.py", "d = 1\n");
        let changes = vec![fc("src/feat/x.ts", "+import"), fc("util/b.ts", "+export"), fc("zzz/c.py", "+c"), fc("zzz/d.py", "+d")];
        let g = group(changes, Some(dir.path()), &open_gate(2, None));
        assert_eq!(g.bundles.len(), 2, "import 跨目录并组 + 同目录弱边并组: {:?}", g.bundles.iter().map(paths_of).collect::<Vec<_>>());
        assert_eq!(paths_of(&g.bundles[0]), vec!["src/feat/x.ts", "util/b.ts"]);
        assert!(g.bundles[0].edges.iter().any(|e| e.kind == EdgeKind::Import && e.from == "src/feat/x.ts" && e.to == "util/b.ts"));
        assert_eq!(paths_of(&g.bundles[1]), vec!["zzz/c.py", "zzz/d.py"]);
    }

    /// 修补批 X-2：同名 changed def 是「各自定义」不是引用——文件自身拥有该名时不计引用边，否则互不相关的文件被强边粘死。
    #[test]
    fn group_same_name_defs_do_not_make_reference_edges() {
        let dir = tempfile::tempdir().unwrap();
        let def_line = "pub fn validate_input(x: &str) -> bool { x.len() > 1 }";
        write(dir.path(), "p/a.rs", &format!("{def_line}\nfn run_a() {{ validate_input(\"a\"); validate_input(\"b\"); }}\n"));
        write(dir.path(), "q/b.rs", &format!("{def_line}\nfn run_b() {{ validate_input(\"c\"); validate_input(\"d\"); }}\n"));
        write(dir.path(), "r/c.rs", "fn use_it() { validate_input(\"e\"); }\n");
        let def = format!("@@ -1,0 +1,1 @@\n+{def_line}\n");
        let changes = vec![fc("p/a.rs", &def), fc("q/b.rs", &def), fc("r/c.rs", "+u")];
        let g = group(changes, Some(dir.path()), &open_gate(10, None));
        let all: Vec<&Edge> = g.bundles.iter().flat_map(|b| b.edges.iter()).collect();
        assert!(
            !all.iter().any(|e| (e.from == "p/a.rs" || e.from == "q/b.rs") && matches!(e.kind, EdgeKind::Reference { .. })),
            "同名定义者之间无引用边: {all:?}"
        );
        // 真引用方 c 对两个定义者各一条（count 1）
        assert!(all.iter().any(|e| e.from == "r/c.rs" && e.to == "p/a.rs" && e.kind == EdgeKind::Reference { count: 1 }), "{all:?}");
        assert!(all.iter().any(|e| e.from == "r/c.rs" && e.to == "q/b.rs" && e.kind == EdgeKind::Reference { count: 1 }), "{all:?}");
    }

    #[test]
    fn group_reference_edges_by_changed_def_name() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "pub fn compute_total() -> u32 { 1 }\n");
        write(dir.path(), "src/b.rs", "fn x() { compute_total(); compute_total(); }\n");
        write(dir.path(), "other/c.rs", "fn y() { let t = compute_total(); }\n");
        write(dir.path(), "other/d.rs", "fn z() {}\n");
        let changes = vec![
            fc("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn compute_total() -> u32 { 1 }\n"),
            fc("src/b.rs", "+x"), fc("other/c.rs", "+y"), fc("other/d.rs", "+z"),
        ];
        let g = group(changes, Some(dir.path()), &open_gate(10, None));
        let all: Vec<&Edge> = g.bundles.iter().flat_map(|b| b.edges.iter()).collect();
        assert!(all.iter().any(|e| e.from == "src/b.rs" && e.to == "src/a.rs" && e.kind == EdgeKind::Reference { count: 2 }), "{all:?}");
        assert!(all.iter().any(|e| e.from == "other/c.rs" && e.to == "src/a.rs" && e.kind == EdgeKind::Reference { count: 1 }), "{all:?}");
        assert!(!all.iter().any(|e| e.from == "other/d.rs" && e.kind != EdgeKind::SameDir), "d 不引用 a");
        // 全部连通（a-b 同目录+强引用，c→a 弱引用，c-d 同目录）且 ≤ 10 → 一组
        assert_eq!(g.bundles.len(), 1);
    }

    #[test]
    fn group_cuts_weak_seam_when_oversized() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "m/a.ts", "import {b} from './b';\n");
        write(dir.path(), "m/b.ts", "export const b = 1;\n");
        write(dir.path(), "m/c.ts", "import {d} from './d';\n");
        write(dir.path(), "m/d.ts", "export const d = 1;\n");
        let changes = ["m/a.ts", "m/b.ts", "m/c.ts", "m/d.ts"].iter().map(|p| fc(p, "+x")).collect();
        let g = group(changes, Some(dir.path()), &open_gate(2, None));
        // 四文件同目录一个分量（6 弱 + 2 强）超 max=2 → 切全部弱边 → 强子分量 {a,b} {c,d}
        assert_eq!(g.bundles.len(), 2, "{:?}", g.bundles.iter().map(paths_of).collect::<Vec<_>>());
        assert_eq!(paths_of(&g.bundles[0]), vec!["m/a.ts", "m/b.ts"]);
        assert_eq!(paths_of(&g.bundles[1]), vec!["m/c.ts", "m/d.ts"]);
        assert_eq!(g.stats.weak_cut, 6);
        assert_eq!(g.stats.cross_edges, 4, "跨组的 4 条同目录边仍是流水线产物");
        assert_eq!(g.stats.over_budget, 0);
        assert!(g.bundles[0].edges.iter().any(|e| e.kind == EdgeKind::SameDir && e.to == "m/c.ts"), "跨组边出现在两端 bundle");
    }

    #[test]
    fn group_keeps_strong_component_marks_over_budget() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a/a.ts", "import {b} from '../b/b';\n");
        write(dir.path(), "b/b.ts", "import {c} from '../c/c';\n");
        write(dir.path(), "c/c.ts", "export const c = 1;\n");
        let changes: Vec<FileChange> = ["a/a.ts", "b/b.ts", "c/c.ts"].iter().map(|p| fc(p, "+x")).collect();
        let g = group(changes.clone(), Some(dir.path()), &open_gate(2, None));
        // 强链 a→b→c 超 max=2、无弱边可切 → 不拆、标 over_budget
        assert_eq!(g.bundles.len(), 1);
        assert!(g.bundles[0].over_budget);
        assert_eq!((g.stats.weak_cut, g.stats.over_budget), (0, 1));
        // 预算门：文件数够但估算字节超 → 同样 over_budget
        let g = group(changes, Some(dir.path()), &open_gate(10, Some(10)));
        assert_eq!(g.bundles.len(), 1);
        assert!(g.bundles[0].over_budget);
    }

    #[test]
    fn group_bin_packs_isolated_by_dir() {
        let changes: Vec<FileChange> = ["e/1.rs", "a/1.rs", "c/1.rs", "b/1.rs", "d/1.rs"].iter().map(|p| fc(p, "+x")).collect();
        let g = group(changes.clone(), None, &open_gate(2, None));
        assert_eq!(g.bundles.iter().map(paths_of).collect::<Vec<_>>(), vec![vec!["a/1.rs", "b/1.rs"], vec!["c/1.rs", "d/1.rs"], vec!["e/1.rs"]]);
        assert_eq!(g.stats.isolated, 5);
        assert_eq!(g.stats.over_budget, 0);
        // 预算装箱：无全文时估算 = diff 字节（"+x" = 2）→ 预算 3 每箱只装一个
        let g = group(changes, None, &open_gate(10, Some(3)));
        assert_eq!(g.bundles.len(), 5);
        assert!(g.bundles.iter().all(|b| !b.over_budget));
        // 单文件超预算 → 独占并标 over_budget
        let g = group(vec![fc("a/1.rs", "+xxxxxxxx"), fc("b/1.rs", "+x")], None, &open_gate(10, Some(4)));
        assert_eq!(g.bundles.len(), 2);
        assert!(g.bundles[0].over_budget && !g.bundles[1].over_budget);
    }

    #[test]
    fn group_repacks_singletons_after_cut() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "m/a.ts", "import {b} from './b';\n");
        write(dir.path(), "m/b.ts", "export const b = 1;\n");
        write(dir.path(), "m/z.ts", "export const z = 1;\n");
        let changes = ["m/a.ts", "m/b.ts", "m/z.ts"].iter().map(|p| fc(p, "+x")).collect();
        let g = group(changes, Some(dir.path()), &open_gate(2, None));
        // {a,b,z} 同目录超 max=2 → 切 3 弱边 → {a,b} 强 + z 落单回池装箱
        assert_eq!(g.bundles.iter().map(paths_of).collect::<Vec<_>>(), vec![vec!["m/a.ts", "m/b.ts"], vec!["m/z.ts"]]);
        assert_eq!(g.stats.weak_cut, 3);
        assert_eq!(g.stats.isolated, 0, "z 原图有同目录边，不算孤立");
    }

    #[test]
    fn hub_order_prefers_import_hubs() {
        let dir = tempfile::tempdir().unwrap();
        for f in ["a", "b", "c"] {
            write(dir.path(), &format!("m/{f}.ts"), "import {h} from './hub';\n");
        }
        write(dir.path(), "m/hub.ts", "export const h = 1;\n");
        let changes = ["m/a.ts", "m/b.ts", "m/c.ts", "m/hub.ts"].iter().map(|p| fc(p, "+x")).collect();
        let g = group(changes, Some(dir.path()), &open_gate(10, None));
        assert_eq!(g.bundles.len(), 1);
        assert_eq!(paths_of(&g.bundles[0]), vec!["m/a.ts", "m/b.ts", "m/c.ts", "m/hub.ts"], "files 保持路径序");
        assert_eq!(g.bundles[0].hub_order, vec![3, 0, 1, 2], "hub（3 条 import 边）先填，其余按路径");
        let filled: Vec<&str> = g.bundles[0].fill_order().map(|f| f.path.as_str()).collect();
        assert_eq!(filled[0], "m/hub.ts");
    }

    #[test]
    fn group_is_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "m/a.ts", "import {b} from './b';\n");
        write(dir.path(), "m/b.ts", "export const b = 1;\n");
        write(dir.path(), "n/c.ts", "export const c = 1;\n");
        let mk = |order: &[&str]| order.iter().map(|p| fc(p, "+x")).collect::<Vec<_>>();
        let g1 = group(mk(&["n/c.ts", "m/b.ts", "m/a.ts"]), Some(dir.path()), &open_gate(10, None));
        let g2 = group(mk(&["m/a.ts", "n/c.ts", "m/b.ts"]), Some(dir.path()), &open_gate(10, None));
        assert_eq!(g1.stats, g2.stats);
        assert_eq!(g1.bundles.iter().map(paths_of).collect::<Vec<_>>(), g2.bundles.iter().map(paths_of).collect::<Vec<_>>());
        assert_eq!(g1.bundles.iter().map(|b| b.hub_order.clone()).collect::<Vec<_>>(), g2.bundles.iter().map(|b| b.hub_order.clone()).collect::<Vec<_>>());
        assert_eq!(g1.bundles.iter().map(|b| b.edges.clone()).collect::<Vec<_>>(), g2.bundles.iter().map(|b| b.edges.clone()).collect::<Vec<_>>());
    }

    #[test]
    fn count_plus_minus_skips_headers_but_counts_dash_body_lines() {
        // dogfood 20260703 r4：正文删除行 "--sep"（diff 行 "---sep"）不得被当文件头跳过；真头 "--- a/…" 仍跳。
        // 本口径同时驱动分组门的 churn 与跨组索引的 (+a/-d)。
        assert_eq!(count_plus_minus("--- a/a.rs\n+++ b/a.rs\n---sep\n+++x"), (1, 1));
        assert_eq!(count_plus_minus("+one\n+two\n-gone"), (2, 1));
    }

    #[test]
    fn ref_worthy_filters_short_generic_and_plain_lowercase() {
        assert!(ref_worthy("handleRequest") && ref_worthy("wrap_body") && ref_worthy("compute_total"));
        assert!(!ref_worthy("cancel") && !ref_worthy("main") && !ref_worthy("Get") && !ref_worthy("abc"));
        assert!(ref_worthy("subscribers"), "≥ 8 字符纯小写放行");
    }

    fn idx_bundle(files: &[(&str, &str)], edges: Vec<Edge>) -> Bundle {
        let mut b = Bundle::from_files(files.iter().map(|(p, d)| fc(p, d)).collect());
        b.edges = edges;
        b
    }

    fn imp(from: &str, to: &str) -> Edge {
        Edge { from: from.into(), to: to.into(), kind: EdgeKind::Import }
    }

    /// 两组三文件：g0 = {src/a.rs}，g1 = {src/b.rs, lib/c.rs}；a → b（import）、c → a（引用 ×2）、a–b 同目录。
    fn two_groups() -> Vec<Bundle> {
        let a_b = imp("src/a.rs", "src/b.rs");
        let c_a = Edge { from: "lib/c.rs".into(), to: "src/a.rs".into(), kind: EdgeKind::Reference { count: 2 } };
        let same = Edge { from: "src/a.rs".into(), to: "src/b.rs".into(), kind: EdgeKind::SameDir };
        vec![
            idx_bundle(&[("src/a.rs", "+one\n+two\n-gone")], vec![a_b.clone(), c_a.clone(), same.clone()]),
            idx_bundle(&[("src/b.rs", "--- a/b.rs\n+++ b/b.rs\n+x"), ("lib/c.rs", "+c")], vec![a_b, c_a, same]),
        ]
    }

    #[test]
    fn cross_index_forward_backward_and_groups() {
        let idx = cross_index(&two_groups(), &["docs/readme.md".to_string()], None);
        assert_eq!(idx.entries.len(), 4);
        let a = &idx.entries[0];
        assert_eq!((a.path.as_str(), a.group, a.adds, a.dels), ("src/a.rs", Some(0), 2, 1));
        assert_eq!(a.forward, vec!["src/b.rs"], "a import b");
        assert_eq!(a.backward, vec!["lib/c.rs"], "c 引用 a；同目录边不进索引");
        let b = &idx.entries[1];
        assert_eq!((b.group, b.adds, b.dels), (Some(1), 1, 0));
        assert_eq!(b.backward, vec!["src/a.rs"]);
        assert!(b.forward.is_empty());
        let extra = &idx.entries[3];
        assert_eq!((extra.path.as_str(), extra.group, extra.adds), ("docs/readme.md", None, 0));
        assert!(idx.entries.iter().all(|e| !e.delta), "delta=None 不标");
    }

    #[test]
    fn render_cross_index_snapshot_and_connected_first() {
        let idx = cross_index(&two_groups(), &[], None);
        // 组 0（a）看到 g1 的 b、c：两者都与 a 有边；组号 1-based
        assert_eq!(
            render_cross_index(&idx, 0),
            "\nlib/c.rs (g2 +1/-0) → [src/a.rs]\nsrc/b.rs (g2 +1/-0) ← [src/a.rs]"
        );
        // 组 1 看到 a
        assert_eq!(render_cross_index(&idx, 1), "\nsrc/a.rs (g1 +2/-1) → [src/b.rs] ← [lib/c.rs]");
        // 有边者优先：给组 1 加一个与本组无边的 g3 文件 + 分支文件 → 排在有边者之后，分支文件最后
        let mut bundles = two_groups();
        bundles.push(idx_bundle(&[("aaa/z.rs", "+z")], vec![]));
        let idx = cross_index(&bundles, &["branch/only.rs".to_string()], None);
        assert_eq!(
            render_cross_index(&idx, 1),
            "\nsrc/a.rs (g1 +2/-1) → [src/b.rs] ← [lib/c.rs]\naaa/z.rs (g3 +1/-0)\nbranch/only.rs"
        );
        let all = render_cross_indices(&idx, 3);
        assert_eq!(all.len(), 3);
        assert!(all[2].contains("src/a.rs (g1") && all[2].contains("src/b.rs (g2"));
    }

    #[test]
    fn render_cross_index_single_group_is_empty() {
        let b = idx_bundle(&[("src/a.rs", "+x"), ("src/b.rs", "+y")], vec![imp("src/a.rs", "src/b.rs")]);
        let idx = cross_index(&[b], &[], None);
        assert!(render_cross_index(&idx, 0).is_empty(), "单组无分支文件 → 空串（占位由 prompt 层渲染）");
    }

    #[test]
    fn render_cross_index_delta_only_marks_delta_set_and_extra_paths_plain() {
        use std::collections::BTreeSet;
        let delta: BTreeSet<String> = ["src/b.rs".to_string()].into();
        let extra = vec!["branch/x.rs".to_string(), "src/a.rs".to_string()]; // 与组内重复的路径不重复登记
        let idx = cross_index(&two_groups(), &extra, Some(&delta));
        assert_eq!(idx.entries.len(), 4);
        let s = render_cross_index(&idx, 0);
        assert!(s.contains("src/b.rs (g2 +1/-0) ← [src/a.rs] [delta]"), "{s}");
        assert!(!s.contains("lib/c.rs (g2 +1/-0) → [src/a.rs] [delta]"), "非 delta 不标: {s}");
        assert!(s.ends_with("\nbranch/x.rs"), "分支文件只有路径、无边无计数无 delta: {s}");
    }

    #[test]
    fn render_cross_index_degrades_then_truncates_within_cap() {
        // 300 个长路径文件各自成组（带边），当前组 0：完整行装到帽 → 降质行 `see group N` → 尾注
        let mut bundles: Vec<Bundle> = Vec::new();
        for i in 0..300 {
            let p = format!("very/deeply/nested/module/path/segment/that/keeps/going/file_number_{i:04}_with_long_name.rs");
            let e = imp(&p, "very/deeply/nested/module/path/segment/that/keeps/going/file_number_0000_with_long_name.rs");
            bundles.push(idx_bundle(&[(&p, "+x")], vec![e]));
        }
        let idx = cross_index(&bundles, &[], None);
        let s = render_cross_index(&idx, 0);
        assert!(s.len() <= MANIFEST_MAX_CHARS, "含尾注总长不超帽: {}", s.len());
        assert!(s.contains(" → ["), "帽内先有完整行");
        assert!(s.contains(" → see group "), "超出部分降质为指针: {s}");
        assert!(s.contains("more files (see groups "), "仍超 → 尾注带组指针: {s}");
        // 条目帽：短路径 300 组 → 至多 MANIFEST_MAX_ENTRIES 行 + 尾注
        let bundles: Vec<Bundle> = (0..300).map(|i| idx_bundle(&[(&format!("f{i}.rs"), "+x")], vec![])).collect();
        let s = render_cross_index(&cross_index(&bundles, &[], None), 0);
        assert!(s.lines().count() <= MANIFEST_MAX_ENTRIES + 2, "{}", s.lines().count());
        assert!(s.len() <= MANIFEST_MAX_CHARS, "{}", s.len());
        let tail = s.lines().last().unwrap();
        assert!(tail.starts_with("… and ") && tail.contains(" more files (see groups ") && tail.ends_with(", …)"),
            "尾注列被截组号（至多 5 个 + …）: {tail}");
    }

    #[test]
    fn two_layer_manifest_marks_delta_membership_and_caps() {
        use std::collections::BTreeSet;
        let all = vec!["src/a.rs".to_string(), "src/b.rs".into(), "src/c.rs".into()];
        let delta: BTreeSet<String> = ["src/b.rs".to_string()].into();
        let s = two_layer_manifest(&all, &delta);
        assert!(s.contains("src/b.rs [delta]"), "{s}");
        assert!(s.contains("src/a.rs") && !s.contains("src/a.rs [delta]"), "{s}");
        // 复用双帽：大清单截断注记
        let many: Vec<String> = (0..500).map(|i| format!("src/file{i:04}.rs")).collect();
        let s = two_layer_manifest(&many, &BTreeSet::new());
        assert!(s.contains("more files"), "{s}");
        assert!(s.chars().count() <= 4000, "{}", s.chars().count());
    }
}
