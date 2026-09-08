use std::collections::BTreeMap;
use std::path::Path;

use crate::evidence::git::FileChange;

/// 噪声文件名（锁文件等，评审无价值）。
const NOISE_NAMES: &[&str] = &[
    "Cargo.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
    "poetry.lock", "Gemfile.lock", "composer.lock",
];

/// 通用入口/聚合文件词干：同词干不代表逻辑相关，不参与同词干 union（只靠同目录 + import）。
const GENERIC_STEMS: &[&str] = &["mod", "lib", "main", "index", "__init__", "init"];

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

#[derive(Clone, Debug)]
pub struct Bundle {
    pub files: Vec<FileChange>,
}

/// 清单条目上限——超出截断为「… and N more files」（与 dismissed notes 同思路的体积防护）。
const MANIFEST_MAX_ENTRIES: usize = 200;
/// 清单总字符预算——条目数之外的直接 token 防护：每 bundle prompt 注入一份清单，
/// 大变更集下 O(bundles×manifest) 叠加，字符帽对症（dogfood 20260703 r1 6db068d2）。
const MANIFEST_MAX_CHARS: usize = 4000;
/// 截断注记（"… and N more files"）的预留额——预算含注记本身，总输出不超帽
/// （dogfood 20260703 r2 5361b545：注记后置追加曾致溢出预算）。
const MANIFEST_SUFFIX_RESERVE: usize = 40;

/// 全变更集清单（dogfood 20260702 #2）：`exclude` 号 bundle 之外所有变更文件的
/// `path (+adds/-dels)`。注入单包评审 prompt，让模型知晓视野外还有哪些文件在变——
/// 消「无证据实现/无 version」式跨 bundle 误报。空串 = 该包已覆盖全变更集。
pub fn other_files_manifest(bundles: &[Bundle], exclude: usize) -> String {
    let mut entries: Vec<String> = Vec::new();
    for (i, b) in bundles.iter().enumerate() {
        if i == exclude {
            continue;
        }
        for fc in &b.files {
            let (mut adds, mut dels) = (0usize, 0usize);
            for l in fc.diff.lines() {
                // 文件头恒为 "--- a/…"/"+++ b/…"/"--- /dev/null"（带空格）；裸 "---"/"+++"
                // 前缀会误跳正文增删行（如删除 "--separator" → diff 行 "---separator"，
                // dogfood 20260703 r4 8eae1f24 agree=2）。
                if l.starts_with("+++ ") || l.starts_with("--- ") {
                    continue;
                }
                if l.starts_with('+') {
                    adds += 1;
                } else if l.starts_with('-') {
                    dels += 1;
                }
            }
            entries.push(format!("{} (+{adds}/-{dels})", fc.path));
        }
    }
    cap_entries(entries)
}

/// 双帽截断（条目数 + 总字符预算，先到者截断，尾注被截条数）——
/// other_files_manifest 与 two_layer_manifest 共用同一体积防线。
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

/// 单个文件包的文件数上限（超出按排序切块）。
pub const BUNDLE_MAX_FILES: usize = 8;

fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// 词干：文件名去掉首个 '.' 之后部分（widget.test.ts 与 widget.ts 同词干 "widget"）。
/// dotfile（首字符即 '.'）整名作词干，避免空词干把所有 dotfile 误并一包。
fn stem(path: &str) -> &str {
    let f = filename(path);
    match f.find('.') {
        Some(0) => f,
        Some(i) => &f[..i],
        None => f,
    }
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

/// 去 `pub` / `pub(crate)` 等可见性前缀。
fn strip_pub(t: &str) -> &str {
    match t.strip_prefix("pub") {
        Some(r) => {
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
        None => t,
    }
}

/// Go import 说明符（单行 `import "x"` 与 `import ( … )` 块，别名前缀允许）。
fn go_imports(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_block = false;
    for line in content.lines() {
        let t = line.trim();
        if in_block {
            if t.starts_with(')') {
                in_block = false;
            } else if let Some(s) = quoted_after(t, "") {
                out.push(s);
            }
            continue;
        }
        let Some(rest) = t.strip_prefix("import") else { continue };
        if !rest.is_empty() && !rest.starts_with([' ', '\t', '(']) {
            continue; // `imports := …` 之类
        }
        let rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix('(') {
            in_block = true;
            if let Some(s) = quoted_after(r, "") {
                out.push(s);
            }
        } else if let Some(s) = quoted_after(rest, "") {
            out.push(s);
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
                cfg = (*pc).clone(); // 数组 extends：后者覆盖前者
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

    /// 候选（可带或不带扩展名、可为目录）→ 改动集里的 TS/JS 文件：`key` 或 `key/index`。
    fn lookup_ts(&self, cand: &str) -> Vec<String> {
        let key = strip_ext(cand);
        let mut v = self.lookup(&key, TS_EXTS);
        if v.is_empty() {
            v = self.lookup(&format!("{key}/index"), TS_EXTS);
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
    /// `allow_root`：回退到 crate 根文件（lib.rs / main.rs）——只给 `super::` / `self::` 相对路径
    /// （`crate::x` 的首段恒是模块，回退到根文件会把所有 `use crate::…` 都连到改动的 lib.rs）。
    fn resolve_rust_path(&self, crate_root: &str, module: &[String], segs: &[String]) -> Vec<String> {
        let mut full: Vec<String> = Vec::new();
        let mut rest = segs;
        let allow_root;
        match rest.first().map(|s| s.as_str()) {
            Some("crate") => {
                rest = &rest[1..];
                allow_root = false;
            }
            Some("self") => {
                rest = &rest[1..];
                full.extend_from_slice(module);
                allow_root = true;
            }
            Some("super") => {
                full.extend_from_slice(module);
                while rest.first().map(|s| s.as_str()) == Some("super") {
                    rest = &rest[1..];
                    if full.pop().is_none() {
                        return Vec::new();
                    }
                }
                allow_root = true;
            }
            _ => return Vec::new(), // 外部 crate / 2018 裸模块路径：不解析
        }
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
        for line in content.lines() {
            let t = strip_pub(line.trim());
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
            for p in paths {
                out.extend(self.resolve_rust_path(root, &module, &p));
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

/// 轻量分包（确定性）：同父目录、或同词干（排除通用入口名）、或 import 互引 → 并到一包；
/// 超 cap 按排序切块。`root=Some` 时读各改动文件全文做 import union；`None` 退化到目录+词干。
pub fn group(mut changes: Vec<FileChange>, root: Option<&Path>) -> Vec<Bundle> {
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let n = changes.len();
    if n == 0 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..n).collect();
    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        for j in (i + 1)..n {
            let same_dir = parent_dir(&changes[i].path) == parent_dir(&changes[j].path);
            let si = stem(&changes[i].path);
            let same_stem = si == stem(&changes[j].path) && !GENERIC_STEMS.contains(&si);
            if same_dir || same_stem {
                union(&mut parent, i, j);
            }
        }
    }
    // import union（仅当给定 root）：A 引用了同在改动集里的 B → 并包。
    if let Some(root) = root {
        let paths: Vec<String> = changes.iter().map(|c| c.path.clone()).collect();
        let resolver = ImportResolver::new(Some(root), &paths);
        let idx: BTreeMap<&str, usize> = paths.iter().enumerate().map(|(i, p)| (p.as_str(), i)).collect();
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            // 安全不变量：changes[i].path 来自 git diff（仓内相对、git 跟踪、非模型可控），
            // 故此处 root.join 直读无需 positioning::safe_read 的 canonical-root/软链防逃逸；
            // 读失败即 continue 降级。若未来 caller 改喂不可信来源（spec/LLM 提议清单），须补该防护。
            let full = root.join(&changes[i].path);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue, // 读失败/二进制 → 跳过该文件 import（退化）
            };
            for e in import_edges(&changes[i].path, &content, &resolver) {
                if let Some(&j) = idx.get(e.to.as_str()) {
                    if i != j {
                        union(&mut parent, i, j);
                    }
                }
            }
        }
    }
    // 按代表元聚组（成员索引升序，因已排序）
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = uf_find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut bundles = Vec::new();
    for (_root_idx, idxs) in groups {
        for chunk in idxs.chunks(BUNDLE_MAX_FILES) {
            let files = chunk.iter().map(|&k| changes[k].clone()).collect();
            bundles.push(Bundle { files });
        }
    }
    bundles
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

    #[test]
    fn group_unions_same_directory() {
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("src/auth/token.rs", "+b")], None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].files.len(), 2);
    }

    #[test]
    fn group_unions_same_stem_across_dirs() {
        // widget.tsx 与 widget.css 同词干 → 同包
        let out = group(vec![fc("ui/widget.tsx", "+a"), fc("styles/widget.css", "+b")], None);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn group_separates_unrelated() {
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("docs/readme.md", "+b")], None);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn group_splits_oversized_at_cap() {
        // 同目录 10 文件 → 切成 8 + 2
        let changes: Vec<FileChange> = (0..10)
            .map(|i| fc(&format!("src/m/f{i:02}.rs"), "+x"))
            .collect();
        let out = group(changes, None);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|b| b.files.len() <= BUNDLE_MAX_FILES));
        assert_eq!(out[0].files.len() + out[1].files.len(), 10);
        assert_eq!(out[0].files.len(), 8);
        assert_eq!(out[1].files.len(), 2);
        assert!(out[0].files[0].path.ends_with("f00.rs"));
        assert!(out[1].files[0].path.ends_with("f08.rs"));
    }

    #[test]
    fn group_does_not_union_distinct_dotfiles() {
        // 修复前：两个 dotfile 空词干 → 被错误并一包；修复后各自成包
        let out = group(vec![fc("a/.gitignore", "+x"), fc("b/.eslintrc", "+y")], None);
        assert_eq!(out.len(), 2);
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

    #[test]
    fn import_edges_ts_relative() {
        let src = "import {a} from '../util/b';\nconst c = require('./helper');\nimport x from 'react';\nexport * from './styles.css';\n";
        let got = edges("src/feat/x.ts", src, &["src/feat/x.ts", "src/feat/helper.ts", "src/util/b.tsx", "src/feat/styles.css"], None);
        // '../util/b' → src/util/b.tsx；'./helper' → src/feat/helper.ts；'react' 裸模块无 tsconfig → 丢；css 非 TS 扩展名不成边
        assert_eq!(got, vec!["src/feat/helper.ts".to_string(), "src/util/b.tsx".to_string()]);
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
        let src = "pub mod token;\nmod helper;\nuse crate::util::helper::Item;\nuse crate::util::{other, third::Thing};\nuse crate::{\n    x::One,\n    y,\n};\nuse crate::nothing::Here;\n";
        let changed = ["src/auth/mod.rs", "src/auth/token.rs", "src/auth/helper/mod.rs", "src/util/helper.rs", "src/util/other/mod.rs", "src/util/third.rs", "src/x.rs", "src/y.rs", "src/lib.rs"];
        let got = edges("src/auth/mod.rs", src, &changed, None);
        // mod 声明 → 同目录子模块（.rs 或 /mod.rs）；`Item` / `Thing` 尾段是 item → 回退到最长存在前缀；
        // 多行 use 累积到 `;`；`crate::nothing::Here` 不存在且 `crate::` 不回退到 lib.rs
        assert_eq!(got, vec![
            "src/auth/helper/mod.rs".to_string(), "src/auth/token.rs".into(), "src/util/helper.rs".into(),
            "src/util/other/mod.rs".into(), "src/util/third.rs".into(), "src/x.rs".into(), "src/y.rs".into(),
        ]);
        // 非 mod.rs 文件的 `mod x;` 指向同名目录（2018 布局）
        let got = edges("src/auth.rs", "mod helper;\n", &["src/auth.rs", "src/auth/helper.rs", "src/helper.rs"], None);
        assert_eq!(got, vec!["src/auth/helper.rs".to_string()]);
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

    #[test]
    fn group_unions_by_relative_import_across_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/feat")).unwrap();
        std::fs::create_dir_all(dir.path().join("util")).unwrap();
        std::fs::write(dir.path().join("src/feat/x.ts"), "import {b} from '../../util/b';\n").unwrap();
        std::fs::write(dir.path().join("util/b.ts"), "export const b = 1;\n").unwrap();
        // 不同目录、不同词干，仅靠 import 关联
        let changes = vec![fc("src/feat/x.ts", "+import"), fc("util/b.ts", "+export")];
        let out = group(changes, Some(dir.path()));
        assert_eq!(out.len(), 1, "relative import should union across dirs");
        assert_eq!(out[0].files.len(), 2);
    }

    #[test]
    fn group_generic_stems_do_not_over_union() {
        // 两个不同目录的 mod.rs：词干都是 "mod"（通用），不应仅因同词干并包
        let out = group(vec![fc("a/mod.rs", "+x"), fc("b/mod.rs", "+y")], None);
        assert_eq!(out.len(), 2, "generic stem 'mod' must not union unrelated files");
    }

    #[test]
    fn group_none_root_falls_back_to_dir_and_stem() {
        // root=None：无 import 提取，退化到目录+词干（同目录并包）
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("src/auth/token.rs", "+b")], None);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn other_files_manifest_lists_counts_and_excludes_self() {
        let bundles = vec![
            Bundle { files: vec![fc("a.rs", "+one\n+two\n-gone")] },
            Bundle { files: vec![fc("b.rs", "--- a/b.rs\n+++ b/b.rs\n+x")] },
        ];
        let m = other_files_manifest(&bundles, 0);
        assert_eq!(m, "b.rs (+1/-0)", "排除自身；+++/--- 头不计增删");
        let m1 = other_files_manifest(&bundles, 1);
        assert_eq!(m1, "a.rs (+2/-1)");
    }

    #[test]
    fn other_files_manifest_counts_body_lines_starting_with_dashes() {
        // 正文删除行内容为 "--sep"（diff 行 "---sep"）不得被当文件头跳过；真头 "--- a/…" 仍跳
        let bundles = vec![
            Bundle { files: vec![fc("self.rs", "+s")] },
            Bundle { files: vec![fc("a.rs", "--- a/a.rs\n+++ b/a.rs\n---sep\n+++x")] },
        ];
        assert_eq!(other_files_manifest(&bundles, 0), "a.rs (+1/-1)", "裸 ---/+++ 正文行计入增删");
    }

    #[test]
    fn other_files_manifest_single_bundle_is_empty() {
        let bundles = vec![Bundle { files: vec![fc("a.rs", "+x")] }];
        assert!(other_files_manifest(&bundles, 0).is_empty(), "单包即全变更集 → 空");
    }

    #[test]
    fn other_files_manifest_truncates_over_cap() {
        let files: Vec<_> = (0..MANIFEST_MAX_ENTRIES + 5).map(|i| fc(&format!("f{i}.rs"), "+x")).collect();
        let bundles = vec![Bundle { files: vec![fc("self.rs", "+s")] }, Bundle { files }];
        let m = other_files_manifest(&bundles, 0);
        assert!(m.contains("more files"), "超上限截断并注记条数: {m}");
        assert!(m.len() <= MANIFEST_MAX_CHARS, "含注记总输出不超预算: len={}", m.len());
    }

    #[test]
    fn other_files_manifest_char_budget_truncates_long_paths() {
        // 50 个超长路径（每条 ~120 字符）→ 字符帽先于条目帽触发
        let files: Vec<_> = (0..50)
            .map(|i| fc(&format!("very/deeply/nested/module/path/segment/that/keeps/going/and/going/file_number_{i:04}_with_long_name.rs"), "+x"))
            .collect();
        let bundles = vec![Bundle { files: vec![fc("self.rs", "+s")] }, Bundle { files }];
        let m = other_files_manifest(&bundles, 0);
        assert!(m.len() <= MANIFEST_MAX_CHARS, "含注记总输出不超预算: len={}", m.len());
        assert!(m.contains("more files"), "被截条数有注记: {m}");
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
