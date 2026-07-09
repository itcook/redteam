//! 卷宗引擎（M14 支柱 2）：diff → 改动符号 → 仓内引用点 → 事实卷宗字符串。
//! hermetic：喂 (bundle, root) 出字符串，零 LLM。移植自 eval/harness.py（查全 1.000）。

use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ignore::WalkBuilder;
use regex::Regex;
use std::path::Path;

pub(crate) struct Lang {
    exts: &'static [&'static str],
    defs: &'static [(&'static str, &'static str)],
}

// Rust def 正则：逐条照抄 eval/harness.py DEF_PATTERNS（已验证 119/119 符号锚定）。
const RUST: Lang = Lang {
    exts: &["rs"],
    defs: &[
        ("fn", r#"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?(?:default\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?(?:extern\s+"[^"]*"\s+)?fn\s+([A-Za-z_]\w*)"#),
        ("struct", r"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?struct\s+([A-Za-z_]\w*)"),
        ("enum", r"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?enum\s+([A-Za-z_]\w*)"),
        ("trait", r"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?(?:unsafe\s+)?trait\s+([A-Za-z_]\w*)"),
        ("type", r"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?type\s+([A-Za-z_]\w*)"),
        ("macro", r"^\s*macro_rules!\s*([A-Za-z_]\w*)"),
        ("const", r"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?const\s+([A-Za-z_]\w*)\s*:"),
        ("static", r"^\s*(?:pub\s*(?:\([^)]*\))?\s*)?static\s+(?:mut\s+)?([A-Za-z_]\w*)\s*:"),
    ],
};

// Go：func（含方法）/ type / const|var。分组 const( ) 块内多名是已知边角，v1 不覆盖（heuristic）。
const GO: Lang = Lang {
    exts: &["go"],
    defs: &[
        ("func", r"^\s*func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)"),
        ("type", r"^\s*type\s+([A-Za-z_]\w*)"),
        ("const", r"^\s*(?:const|var)\s+([A-Za-z_]\w*)"),
    ],
};

// TS/JS：function/class/interface|type|enum/const|let|var + method（D4a 补的类方法/对象字面量方法洞）。
// method 正则刻意宽松（heuristic），关键字碰撞由 match_def 后置去关键字滤。
const TS: Lang = Lang {
    exts: &["ts", "tsx", "js", "jsx", "mts", "cts"],
    defs: &[
        ("function", r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)"),
        ("class", r"^\s*(?:export\s+)?(?:default\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)"),
        ("type", r"^\s*(?:export\s+)?(?:declare\s+)?(?:interface|type|enum)\s+([A-Za-z_$][\w$]*)"),
        ("const", r"^\s*(?:export\s+)?(?:default\s+)?(?:const|let|var)\s+([A-Za-z_$][\w$]*)"),
        ("method", r"^\s*(?:(?:public|private|protected|static|async|readonly|get|set)\s+|\*\s*)*([A-Za-z_$][\w$]*)\s*\([^)]*\)\s*[:{]"),
    ],
};

const LANGS: &[&Lang] = &[&RUST, &GO, &TS];
// TS method 正则会误吞控制流行（`if (x) {`）；regex crate 无 lookahead，用后置关键字滤。
const TS_KEYWORDS: &[&str] = &[
    "if", "for", "while", "switch", "catch", "return", "do", "else",
    "function", "constructor", "await", "yield", "typeof", "new",
];

pub(crate) fn lang_for(path: &str) -> Option<&'static Lang> {
    let ext = path.rsplit('.').next()?;
    LANGS.iter().copied().find(|l| l.exts.contains(&ext))
}

pub(crate) fn compile_defs(lang: &Lang) -> Vec<(&'static str, Regex)> {
    lang.defs.iter().map(|(k, src)| (*k, Regex::new(src).expect("static def regex"))).collect()
}

fn match_def(line: &str, defs: &[(&'static str, Regex)]) -> Option<(&'static str, String)> {
    for (kind, rx) in defs {
        if let Some(c) = rx.captures(line) {
            let name = c.get(1)?.as_str().to_string();
            if *kind == "method" && TS_KEYWORDS.contains(&name.as_str()) {
                return None; // 关键字碰撞：`if (…) {` 一类，非方法定义
            }
            return Some((kind, name));
        }
    }
    None
}

/// 从 0-based def 行 i 起括号配平找块尾行（0-based）；无块（; 结尾）返回 i。
fn block_end(lines: &[&str], i: usize) -> usize {
    let mut depth: i32 = 0;
    let mut seen_open = false;
    // ponytail: 上限 4000 行护栏（照 harness.py），超大函数罕见，够用
    for j in i..(i + 4000).min(lines.len()) {
        for ch in lines[j].chars() {
            if ch == '{' {
                depth += 1;
                seen_open = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        if seen_open && depth <= 0 {
            return j;
        }
        if !seen_open && lines[j].trim_end().ends_with(';') {
            return j;
        }
    }
    i
}

/// line0（0-based）最内层外围 def 的**起行索引**：向上首个「块覆盖 line0」的 def 行。
/// impl/for 头非 def，天然穿透到内层 fn（照 D5 简化：只取名，不分类 impl_trait）。
/// ponytail: 每候选行调 block_end = O(n²) 最坏；单文件几千行够用，热点再优化
pub(crate) fn enclosing_def_start(lines: &[&str], line0: usize, defs: &[(&'static str, Regex)]) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    (0..=line0.min(lines.len().saturating_sub(1)))
        .rev()
        .find(|&i| match_def(lines[i], defs).is_some() && (i == line0 || block_end(lines, i) >= line0))
}

/// line0（0-based）的最内层外围 def 名：向上首个「块覆盖 line0」的 def。
fn innermost_enclosing_def(lines: &[&str], line0: usize, defs: &[(&'static str, Regex)]) -> Option<(&'static str, String)> {
    enclosing_def_start(lines, line0, defs).and_then(|i| match_def(lines[i], defs))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChangedSym {
    pub name: String,
    pub kind: &'static str,
    pub file: String,
}

// @@ -a,b +c,d @@ 抓新文件起始行 c 与计数 d
fn parse_hunk_new_start(line: &str) -> Option<usize> {
    let after_plus = line.split('+').nth(1)?; // "c,d @@ …"
    let c = after_plus.split([',', ' ']).next()?;
    c.parse::<usize>().ok()
}

/// diff（任意上下文级别）→ 改动符号，按 name 去重（grep 按名一次即可）。
pub fn symbols_in_file(diff: &str, file_lines: &[&str], file: &str, defs: &[(&'static str, Regex)]) -> Vec<ChangedSym> {
    let mut out: Vec<ChangedSym> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let push = |name: String, kind: &'static str, out: &mut Vec<ChangedSym>, seen: &mut std::collections::HashSet<String>| {
        if seen.insert(name.clone()) {
            out.push(ChangedSym { name, kind, file: file.to_string() });
        }
    };
    let mut new_line = 0usize; // 当前 + / 上下文行的新文件行号（1-based）
    let mut hunk_anchor: Option<usize> = None; // 本 hunk 首个改动行（0-based）
    for raw in diff.lines() {
        if raw.starts_with("+++ ") || raw.starts_with("--- ") {
            continue;
        }
        if raw.starts_with("@@") {
            // 上一 hunk 收尾：外围 def
            if let Some(a) = hunk_anchor.take() {
                if let Some((kind, n)) = innermost_enclosing_def(file_lines, a, defs) {
                    push(n, kind, &mut out, &mut seen);
                }
            }
            new_line = parse_hunk_new_start(raw).unwrap_or(1);
            continue;
        }
        // `\ No newline at end of file` 是 newline 状态标记非内容行，不得推进 new_line（T01④ 卫生守卫；
        // 当前架构下 drift 被 hunk_anchor 首行锚定 + 每 hunk 重置掩盖，此守卫防未来 per-行锚定时暴露）。
        if raw.starts_with('\\') {
            continue;
        }
        match raw.as_bytes().first() {
            Some(b'+') => {
                let anchor0 = new_line.saturating_sub(1);
                if hunk_anchor.is_none() {
                    hunk_anchor = Some(anchor0);
                }
                if let Some((kind, name)) = match_def(&raw[1..], defs) {
                    push(name, kind, &mut out, &mut seen);
                }
                new_line += 1;
            }
            Some(b'-') => {
                // 纯删除 hunk 无 + 行：删除点(新文件坐标)仍在外围块内，用它锚定外围 def。
                if hunk_anchor.is_none() {
                    hunk_anchor = Some(new_line.saturating_sub(1));
                }
                // 删除行不推进 new_line，不入卷宗（旧调用点靠编译器/评审兜底）
            }
            _ => { new_line += 1; } // 上下文行
        }
    }
    if let Some(a) = hunk_anchor.take() {
        if let Some((kind, n)) = innermost_enclosing_def(file_lines, a, defs) {
            push(n, kind, &mut out, &mut seen);
        }
    }
    out
}

/// 每个 hunk 的首个改动行（0-based 新文件坐标），供 0 壳外围 def 定位。
/// ponytail: 与 symbols_in_file 的 hunk 游走平行（drift 风险）；两者若再动一起改，热点再统一。
pub(crate) fn hunk_anchors(diff: &str) -> Vec<usize> {
    let mut anchors = Vec::new();
    let mut new_line = 0usize;
    let mut hunk_anchor: Option<usize> = None;
    for raw in diff.lines() {
        if raw.starts_with("+++ ") || raw.starts_with("--- ") {
            continue;
        }
        if raw.starts_with("@@") {
            if let Some(a) = hunk_anchor.take() {
                anchors.push(a);
            }
            new_line = parse_hunk_new_start(raw).unwrap_or(1);
            continue;
        }
        if raw.starts_with('\\') {
            continue; // No-newline 标记非内容行
        }
        match raw.as_bytes().first() {
            Some(b'+') => {
                if hunk_anchor.is_none() {
                    hunk_anchor = Some(new_line.saturating_sub(1));
                }
                new_line += 1;
            }
            Some(b'-') => {
                if hunk_anchor.is_none() {
                    hunk_anchor = Some(new_line.saturating_sub(1));
                }
            }
            _ => new_line += 1,
        }
    }
    if let Some(a) = hunk_anchor.take() {
        anchors.push(a);
    }
    anchors
}

/// 改动 hunk 的外围 def 完整体行范围（0-based inclusive），按 def 起行去重、升序。
pub(crate) fn changed_def_ranges(diff: &str, file_lines: &[&str], defs: &[(&'static str, Regex)]) -> Vec<(usize, usize)> {
    let mut starts = std::collections::BTreeSet::new();
    for a in hunk_anchors(diff) {
        if let Some(i) = enclosing_def_start(file_lines, a, defs) {
            starts.insert(i);
        }
    }
    starts.into_iter().map(|i| (i, block_end(file_lines, i))).collect()
}

#[derive(Clone, Debug)]
pub struct RefHit {
    pub path: String,
    pub line: usize,
    pub context: String,
}

#[derive(Clone, Debug)]
pub struct SymRefs {
    pub name: String,
    pub kind: &'static str,
    pub defs_same_name: usize,
    pub hits: Vec<RefHit>,
}

/// 单行超长（minified/生成物单行可达 MB）时把内容截到 ≤512B（另加固定截断注记）防单个引用点
/// 撑爆卷宗预算（T01②）。按 UTF-8 char 边界回退，绝不切在多字节字符中间。
fn truncate_line(line: &str) -> std::borrow::Cow<'_, str> {
    const CTX_LINE_MAX: usize = 512;
    if line.len() <= CTX_LINE_MAX {
        return std::borrow::Cow::Borrowed(line);
    }
    let mut end = CTX_LINE_MAX;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    std::borrow::Cow::Owned(format!("{}… [line truncated]", &line[..end]))
}

/// Rust 词法剥离：注释（`//` 行 / `/* */` 嵌套块）与字符串（`"…"` / `b"…"` / raw `r#"…"#`）
/// 内容替成空格，保留 `\n` 与字节长度 → 行号列号对齐。用于剔「命中落在注释/字符串里」的引用假阳
/// （S1 地板 B：redteam 代码注释/prompt 字符串满是域名词 finding/bundle，regex `\b名\b` 全命中）。
/// 不处理 char/lifetime `'…'`（单字符几乎不撞符号名，低危；多语言各自词法 → TODO T38）。
fn strip_rust_noncode(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let c = b[i];
        // 行注释
        if c == b'/' && i + 1 < n && b[i + 1] == b'/' {
            while i < n && b[i] != b'\n' { out.push(b' '); i += 1; }
            continue;
        }
        // 块注释（Rust 允许嵌套）
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            let mut depth = 1;
            out.extend_from_slice(b"  "); i += 2;
            while i < n && depth > 0 {
                if b[i] == b'\n' { out.push(b'\n'); i += 1; }
                else if b[i] == b'/' && i + 1 < n && b[i + 1] == b'*' { depth += 1; out.extend_from_slice(b"  "); i += 2; }
                else if b[i] == b'*' && i + 1 < n && b[i + 1] == b'/' { depth -= 1; out.extend_from_slice(b"  "); i += 2; }
                else { out.push(b' '); i += 1; }
            }
            continue;
        }
        // raw 字符串 r"…" / r#"…"# / br#"…"#（区分 raw identifier r#type：# 后须是 " 或更多 #）
        if c == b'r' || (c == b'b' && i + 1 < n && b[i + 1] == b'r') {
            let mut j = if c == b'b' { i + 2 } else { i + 1 };
            let mut hashes = 0;
            while j < n && b[j] == b'#' { hashes += 1; j += 1; }
            if j < n && b[j] == b'"' {
                out.resize(out.len() + (j - i + 1), b' ');
                i = j + 1;
                while i < n {
                    if b[i] == b'"' {
                        let mut k = i + 1;
                        let mut cnt = 0;
                        while k < n && cnt < hashes && b[k] == b'#' { cnt += 1; k += 1; }
                        if cnt == hashes {
                            out.resize(out.len() + 1 + hashes, b' ');
                            i = k;
                            break;
                        }
                    }
                    if b[i] == b'\n' { out.push(b'\n'); } else { out.push(b' '); }
                    i += 1;
                }
                continue;
            }
            // 非 raw string：r/br 当普通字符落到代码区
        }
        // 普通/字节字符串 "…" / b"…"
        if c == b'"' || (c == b'b' && i + 1 < n && b[i + 1] == b'"') {
            if c == b'b' { out.push(b' '); i += 1; }
            out.push(b' '); i += 1; // 开引号
            while i < n && b[i] != b'"' {
                if b[i] == b'\\' && i + 1 < n {
                    out.push(b' ');
                    if b[i + 1] == b'\n' { out.push(b'\n'); } else { out.push(b' '); }
                    i += 2;
                    continue;
                }
                if b[i] == b'\n' { out.push(b'\n'); } else { out.push(b' '); }
                i += 1;
            }
            if i < n { out.push(b' '); i += 1; } // 闭引号
            continue;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_default()
}

// 一次全仓 walk 搜某名的所有词边界命中，返回 (rel_path, 1based_line)。照 do_search_repo。
fn grep_word(name: &str, root: &Path) -> Vec<(String, usize)> {
    let matcher = match RegexMatcher::new(&format!(r"\b{}\b", regex::escape(name))) {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let mut hits = Vec::new();
    for dent in WalkBuilder::new(root).require_git(false).build().flatten() {
        if !dent.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        let path = dent.path().to_path_buf();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_string_lossy().to_string(),
            Err(_) => continue,
        };
        let mut searcher = SearcherBuilder::new().line_number(true).build();
        let _ = searcher.search_path(&matcher, &path, UTF8(|lnum, _line| {
            hits.push((rel.clone(), lnum as usize));
            Ok(true)
        }));
    }
    hits
}

pub fn reference_points(syms: &[ChangedSym], root: &Path, defs: &[(&'static str, Regex)]) -> Vec<SymRefs> {
    let mut file_cache: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let read = |rel: &str, cache: &mut std::collections::HashMap<String, Vec<String>>| -> Vec<String> {
        cache.entry(rel.to_string())
            .or_insert_with(|| std::fs::read_to_string(root.join(rel))
                .map(|s| s.lines().map(String::from).collect())
                .unwrap_or_default())
            .clone()
    };
    let mut strip_cache: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let read_stripped = |rel: &str, cache: &mut std::collections::HashMap<String, Vec<String>>| -> Vec<String> {
        cache.entry(rel.to_string())
            .or_insert_with(|| strip_rust_noncode(&std::fs::read_to_string(root.join(rel)).unwrap_or_default())
                .lines().map(String::from).collect())
            .clone()
    };
    let mut out = Vec::new();
    for sym in syms {
        let word = Regex::new(&format!(r"\b{}\b", regex::escape(&sym.name))).ok();
        let raw = grep_word(&sym.name, root);
        let mut defs_same_name = 0usize;
        let mut hits = Vec::new();
        for (path, line) in raw {
            let lines = read(&path, &mut file_cache);
            let hit_line = lines.get(line.saturating_sub(1)).map(String::as_str).unwrap_or("");
            // B（S1 地板）：命中若全落在注释/字符串（剥离后该行无词边界命中）→ 假阳，剔除。
            // 只处理 .rs（Rust 词法）；多语言词法各异 → TODO T38，非 Rust 文件不剥、原样保留。
            if let Some(w) = &word {
                if path.ends_with(".rs") {
                    let sl = read_stripped(&path, &mut strip_cache);
                    if !sl.get(line.saturating_sub(1)).map(|s| w.is_match(s)).unwrap_or(false) {
                        continue;
                    }
                }
            }
            // 命中行本身是同名 def → 歧义度，不作引用点
            if let Some((_, n)) = match_def(hit_line, defs) {
                if n == sym.name {
                    defs_same_name += 1;
                    continue;
                }
            }
            let lo = line.saturating_sub(4); // ±3 行（含头行）
            let hi = (line + 3).min(lines.len());
            let context = lines.get(lo..hi).unwrap_or(&[])
                .iter()
                .map(|l| truncate_line(l))
                .collect::<Vec<_>>()
                .join("\n");
            hits.push(RefHit { path, line, context });
        }
        // ponytail: 确定性排序，WalkBuilder 遍历顺序与 OS 目录顺序相关（非确定性）
        // 但这里 hits 数量有限 O(1000s) 且 cap 后只取前 10，sort 成本可忽略
        hits.sort_by(|a, b| (a.path.as_str(), a.line).cmp(&(b.path.as_str(), b.line)));
        out.push(SymRefs { name: sym.name.clone(), kind: sym.kind, defs_same_name, hits });
    }
    out
}

const DOSSIER_PER_NAME_CAP: usize = 10;
// ≈8K tokens @ bytes/3.5；含 CJK 注释偏保守。文档记假设，live 换真 tokenizer 校常量（D7）。
const DOSSIER_MAX_BYTES: usize = 28_000;
/// 调用密度门（live A/B 定，2026-07-05）：改动符号的仓内引用点总数低于此阈值时不产卷宗——
/// 稀疏场景（如纯新增代码，符号尚无既有 caller）注入成本 > 收益，返空走空渲染。
/// 阈值系 provisional，待更多 A/B 校准；调高=更保守（更少产卷宗）。
const DOSSIER_MIN_REFS: usize = 5;

/// SymRefs 列表 → 卷宗事实块（不含 <dossier> 包裹与散文，那是 prompt 层的活）。空引用 → ""。
pub fn render_entries(refs: &[SymRefs]) -> String {
    // 全部「可渲染」引用点（受 per-name cap 约束，不含 cap 溢出——那由 "+N more" 单独标注）。
    let total_renderable: usize = refs
        .iter()
        .filter(|s| !s.hits.is_empty())
        .map(|s| s.hits.len().min(DOSSIER_PER_NAME_CAP))
        .sum();
    let mut out = String::new();
    let mut rendered = 0usize;
    for sr in refs {
        if sr.hits.is_empty() {
            continue;
        }
        let ambig = if sr.defs_same_name > 1 {
            format!(" [{} defs share this name (incl. trait dispatch), heuristic]", sr.defs_same_name)
        } else {
            String::new()
        };
        // 组本符号整块（header + ≤cap 引用点 + 可选 cap 标注），再整体判预算——
        // 原子：一个符号全进或全不进，杜绝悬空 header / 半截符号。
        let mut block = format!("## {} ({}){}\n", sr.name, sr.kind, ambig);
        let shown = sr.hits.len().min(DOSSIER_PER_NAME_CAP);
        for h in &sr.hits[..shown] {
            block.push_str(&format!("{}:{}\n{}\n\n", h.path, h.line, h.context));
        }
        if sr.hits.len() > DOSSIER_PER_NAME_CAP {
            block.push_str(&format!(
                "+{} more, {} defs same name, heuristic\n\n",
                sr.hits.len() - DOSSIER_PER_NAME_CAP,
                sr.defs_same_name
            ));
        }
        if out.len() + block.len() > DOSSIER_MAX_BYTES {
            break; // 预算耗尽：本符号及其后全部计入 dropped（由 total_renderable - rendered 兜底）
        }
        out.push_str(&block);
        rendered += shown;
    }
    let dropped = total_renderable.saturating_sub(rendered);
    if dropped > 0 {
        out.push_str(&format!("… and {dropped} reference points omitted for budget\n"));
    }
    out
}

/// 引擎入口：为一个 bundle 产事实卷宗块（不含 <dossier> 包裹）。空 = 无卷宗（降级）。
pub fn build_dossier(bundle: &crate::bundle::Bundle, root: &Path) -> String {
    let mut all_syms: Vec<ChangedSym> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for fc in &bundle.files {
        let lang = match lang_for(&fc.path) {
            Some(l) => l,
            None => continue, // 非支持语言：无损降级
        };
        let defs = compile_defs(lang);
        let content = std::fs::read_to_string(root.join(&fc.path)).unwrap_or_default();
        let file_lines: Vec<&str> = content.lines().collect();
        for s in symbols_in_file(&fc.diff, &file_lines, &fc.path, &defs) {
            if seen.insert(s.name.clone()) {
                all_syms.push(s);
            }
        }
    }
    if all_syms.is_empty() {
        return String::new();
    }
    // 引用搜索用 Rust defs 判同名 def（跨语言歧义度是次要信号，用首文件的 lang 即可）；
    // 简化：用改动符号所属文件的 lang 编 defs。混合 bundle 极少，取第一个支持语言的 defs。
    let defs = bundle.files.iter()
        .find_map(|fc| lang_for(&fc.path))
        .map(compile_defs)
        .unwrap_or_default();
    let refs = reference_points(&all_syms, root, &defs);
    // 调用密度门：引用点太稀疏 → 卷宗不回本，返空（同空渲染，该 bundle 行为不变）
    if refs.iter().map(|r| r.hits.len()).sum::<usize>() < DOSSIER_MIN_REFS {
        return String::new();
    }
    render_entries(&refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rust_defs() -> Vec<(&'static str, Regex)> { compile_defs(lang_for("a.rs").unwrap()) }

    #[test]
    fn match_def_rust_fn_struct_trait() {
        let d = rust_defs();
        assert_eq!(match_def("pub async fn do_read_file(x: u8) {", &d), Some(("fn", "do_read_file".into())));
        assert_eq!(match_def("    struct Bundle {", &d), Some(("struct", "Bundle".into())));
        assert_eq!(match_def("pub(crate) trait CellSink {", &d), Some(("trait", "CellSink".into())));
        assert_eq!(match_def("const MANIFEST_MAX_CHARS: usize = 4000;", &d), Some(("const", "MANIFEST_MAX_CHARS".into())));
        assert_eq!(match_def("let x = foo();", &d), None);
    }

    #[test]
    fn match_def_go_and_ts() {
        let g = compile_defs(lang_for("x.go").unwrap());
        assert_eq!(match_def("func (s *Server) TableName() string {", &g), Some(("func", "TableName".into())));
        assert_eq!(match_def("type Config struct {", &g), Some(("type", "Config".into())));
        let t = compile_defs(lang_for("x.ts").unwrap());
        assert_eq!(match_def("export default class Widget {", &t), Some(("class", "Widget".into())));
        assert_eq!(match_def("  async render(props: P): void {", &t), Some(("method", "render".into())));
        assert_eq!(match_def("  if (x) {", &t), None, "关键字不作方法名");
    }

    #[test]
    fn lang_for_dispatch() {
        assert!(lang_for("a.rs").is_some());
        assert!(lang_for("a.go").is_some());
        assert!(lang_for("a.tsx").is_some());
        assert!(lang_for("a.js").is_some());
        assert!(lang_for("a.py").is_none(), "非支持语言降级");
    }

    #[test]
    fn block_end_balances_braces() {
        let src: Vec<&str> = "fn a() {\n    if x {\n        y();\n    }\n}\nfn b() {}".lines().collect();
        assert_eq!(block_end(&src, 0), 4, "fn a 的块尾在第 5 行(0based 4)");
        assert_eq!(block_end(&src, 5), 5, "fn b 单行块");
    }

    #[test]
    fn innermost_enclosing_walks_up() {
        let d = rust_defs();
        // impl 头非 def，穿透到内层 fn；改动落在 bar 体内 → 外围是 bar
        let src: Vec<&str> = "impl Foo {\n    fn bar(&self) {\n        let z = 1;\n    }\n}".lines().collect();
        assert_eq!(innermost_enclosing_def(&src, 2, &d), Some(("fn", "bar".into())));
        // 落在两 fn 之间的空行 → 无外围
        let src2: Vec<&str> = "fn a() {}\n\nfn b() {}".lines().collect();
        assert_eq!(innermost_enclosing_def(&src2, 1, &d), None);
    }

    #[test]
    fn innermost_enclosing_empty_lines_is_none() {
        let d = rust_defs();
        assert_eq!(innermost_enclosing_def(&[], 0, &d), None, "空文件不得 panic");
    }

    #[test]
    fn symbols_from_diff_plus_lines_and_enclosing() {
        let d = rust_defs();
        // 新文件内容（root 侧真实文件）
        let file: Vec<&str> = "fn helper() {}\n\nfn caller() {\n    helper();\n    let z = 2;\n}".lines().collect();
        // diff：在 caller 体内改了一行（+ let z），未含 def 行 → 外围 caller
        let diff = "@@ -4,0 +5,1 @@\n+    let z = 2;\n";
        let syms = symbols_in_file(diff, &file, "src/x.rs", &d);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["caller"], "改动落在 caller 体内 → 外围 caller");
    }

    #[test]
    fn symbols_from_pure_deletion_hunk_anchors_enclosing() {
        let d = rust_defs();
        // caller 体内纯删除一行（无 + 行）→ 外围 caller 仍应被提取为改动符号
        let file: Vec<&str> = "fn caller() {\n    keep();\n}".lines().collect();
        let diff = "@@ -2,1 +2,0 @@\n-    let dead = 1;\n";
        let syms = symbols_in_file(diff, &file, "src/x.rs", &d);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["caller"], "纯删除 hunk 应锚定外围 caller");
    }

    #[test]
    fn symbols_from_diff_new_def_line() {
        let d = rust_defs();
        let file: Vec<&str> = "fn old() {}\nfn added() {\n    work();\n}".lines().collect();
        let diff = "@@ -1,0 +2,3 @@\n+fn added() {\n+    work();\n+}\n";
        let syms = symbols_in_file(diff, &file, "src/y.rs", &d);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["added"], "只此一个改动符号");
    }

    #[test]
    fn symbols_enclosing_kind_is_real_not_always_fn() {
        let d = rust_defs();
        let file: Vec<&str> = "struct Config {\n    a: u8,\n    b: u8,\n}".lines().collect();
        let diff = "@@ -2,0 +3,1 @@\n+    b: u8,\n";
        let syms = symbols_in_file(diff, &file, "src/c.rs", &d);
        assert_eq!(syms, vec![ChangedSym { name: "Config".into(), kind: "struct", file: "src/c.rs".into() }]);
    }

    #[test]
    fn reference_points_finds_callers_excludes_def_and_marks_ambiguity() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "fn helper() {}\nfn c1() { helper(); }\n").unwrap();
        std::fs::write(root.join("src/b.rs"),
            "fn helper() {}\n/// see helper for details\nfn c2() { helper(); }\n").unwrap();
        let d = rust_defs();
        let syms = vec![ChangedSym { name: "helper".into(), kind: "fn", file: "src/a.rs".into() }];
        let refs = reference_points(&syms, root, &d);
        assert_eq!(refs.len(), 1);
        let r = &refs[0];
        assert_eq!(r.defs_same_name, 2, "两处同名 def → 歧义度 2");
        // 引用点 = c1/c2 的调用行；def 行、doc 提及行（B 剔注释）本身被排除
        assert!(r.hits.iter().any(|h| h.path == "src/a.rs" && h.context.contains("c1")));
        assert!(r.hits.iter().any(|h| h.path == "src/b.rs" && h.context.contains("c2")), "c2 真调用保留");
        assert!(!r.hits.iter().any(|h| h.path == "src/b.rs" && h.line == 2), "B: doc 注释提及行(2)被剔");
        assert!(!r.hits.iter().any(|h| h.line == 1 && h.path == "src/a.rs"), "def 行不作引用点");
    }

    #[test]
    fn reference_points_hits_are_sorted_by_path_line() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 创建多个文件引用同一个符号，确保 walk 顺序可能不同
        std::fs::write(root.join("src/z.rs"),
            "fn helper() {}\nfn zc() { helper(); }\n").unwrap();
        std::fs::write(root.join("src/a.rs"),
            "fn ac1() { helper(); }\nfn ac2() { helper(); }\n").unwrap();
        std::fs::write(root.join("src/m.rs"),
            "fn mc1() { helper(); }\n/// doc helper\nfn mc2() { helper(); }\n").unwrap();
        let d = rust_defs();
        let syms = vec![ChangedSym { name: "helper".into(), kind: "fn", file: "src/z.rs".into() }];
        let refs = reference_points(&syms, root, &d);
        assert_eq!(refs.len(), 1, "应有一个符号");
        let hits = &refs[0].hits;
        assert!(hits.len() >= 4, "应找到至少 4 处引用点（z/a/a/m）");

        // 验证按 (path, line) 排序
        let keys: Vec<(String, usize)> = hits.iter().map(|h| (h.path.clone(), h.line)).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "hits 应按 (path, line) 排序: {:?}", keys);
    }

    #[test]
    fn truncate_line_caps_long_and_is_utf8_safe() {
        assert_eq!(truncate_line("short").as_ref(), "short", "短行原样返回（借用）");
        let long = "x".repeat(2000);
        let t = truncate_line(&long);
        assert!(t.len() < 600, "超长行截断到 ~512B，实得 {}", t.len());
        assert!(t.ends_with("… [line truncated]"), "带截断注记: {t}");
        // 512 字节边界恰落在多字节 CJK 字符中间（每字 3 字节，边界在 3 的倍数）：必须回退到 510B，
        // 否则 `&line[..512]` 会 panic。断言精确落点（非恒真的 from_utf8）以证明边界回退真的跑了。
        let cjk = "字".repeat(1000);
        let tc = truncate_line(&cjk);
        let content = tc.strip_suffix("… [line truncated]").expect("CJK 超长行也带截断注记");
        assert_eq!(content, "字".repeat(170), "回退到 ≤512 的最近 char 边界 = 510B = 170 个字");
    }

    #[test]
    fn reference_points_truncates_minified_long_line() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // minified 风格：一行 ~15KB 里引用 target
        let long = format!("const X = [{}]; target(); {}", "0,".repeat(5000), "z".repeat(5000));
        std::fs::write(root.join("src/a.rs"), format!("fn target() {{}}\n{long}\n")).unwrap();
        let d = rust_defs();
        let syms = vec![ChangedSym { name: "target".into(), kind: "fn", file: "src/a.rs".into() }];
        let refs = reference_points(&syms, root, &d);
        let hit = refs[0].hits.iter().find(|h| h.line == 2).expect("超长行应命中 target");
        assert!(hit.context.contains("[line truncated]"), "超长行被截断");
        // 单行不再是 MB 级：整个 ±3 context 收在少数 KB（否则一个引用点即撑爆 28KB 预算）
        assert!(hit.context.len() < 3000, "context 不再撑爆预算，实得 {}", hit.context.len());
    }

    #[test]
    fn strip_rust_noncode_blanks_comments_and_strings() {
        let s = strip_rust_noncode(
            "let x = finding();       // finding in comment\n\
             let y = \"finding str\";\n\
             /* block finding */ code_finding();\n\
             let r = r#\"raw finding\"#;\n",
        );
        let l: Vec<&str> = s.lines().collect();
        assert!(l[0].contains("finding") && !l[0].contains("comment"), "代码区 finding 留、行尾注释剥: {:?}", l[0]);
        assert!(!l[1].contains("finding"), "字符串字面量里的 finding 剥: {:?}", l[1]);
        assert!(l[2].contains("code_finding") && !l[2].contains("block"), "块注释后代码留、块内剥: {:?}", l[2]);
        assert!(!l[3].contains("finding"), "raw 字符串里的 finding 剥: {:?}", l[3]);
        assert_eq!(s.lines().count(), 4, "行数保持=行号对齐");
    }

    #[test]
    fn reference_points_drops_comment_and_string_false_positives() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // target 定义 + 真调用(2) + 行尾注释(3) + 字符串(4) + 块注释(5)
        std::fs::write(root.join("src/a.rs"),
            "fn target() {}\n\
             fn real() { target(); }\n\
             fn c() { let _x = 1; } // call target here\n\
             fn s() { let _m = \"target not found\"; }\n\
             /* target in block */\n\
             fn tail() {}\n").unwrap();
        let d = rust_defs();
        let syms = vec![ChangedSym { name: "target".into(), kind: "fn", file: "src/a.rs".into() }];
        let refs = reference_points(&syms, root, &d);
        let lines: Vec<usize> = refs[0].hits.iter().map(|h| h.line).collect();
        assert_eq!(lines, vec![2], "只留代码区真调用(行2)，剔行尾注释(3)/字符串(4)/块注释(5)");
    }

    #[test]
    fn symbols_from_diff_ignores_no_newline_marker() {
        let d = rust_defs();
        let file: Vec<&str> = "fn caller() {\n    helper();\n}".lines().collect();
        // 末行无换行：`\ No newline at end of file` 标记不得当内容行推进 new_line、不得污染符号提取
        let diff = "@@ -1,3 +1,3 @@\n fn caller() {\n-    old();\n+    helper();\n\\ No newline at end of file\n";
        let syms = symbols_in_file(diff, &file, "src/x.rs", &d);
        let names: Vec<&str> = syms.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["caller"], "含 No-newline 标记仍正确锚定 caller，标记行被跳过");
    }

    fn sr(name: &str, defs_same_name: usize, n: usize) -> SymRefs {
        let hits = (0..n).map(|i| RefHit {
            path: format!("src/f{i}.rs"), line: i + 1,
            context: format!("    {name}(); // hit {i}"),
        }).collect();
        SymRefs { name: name.into(), kind: "fn", defs_same_name, hits }
    }

    #[test]
    fn render_entries_caps_per_name_and_annotates() {
        let out = render_entries(&[sr("run", 3, 25)]);
        assert!(out.matches("src/f").count() <= DOSSIER_PER_NAME_CAP, "每名 ≤10 引用点");
        assert!(out.contains("+15 more, 3 defs same name, heuristic"), "cap 标注: {out}");
        assert!(out.contains("3 defs share this name (incl. trait dispatch), heuristic"), "歧义标注");
    }

    #[test]
    fn render_entries_budget_drop_count_is_exact() {
        // 4 符号 × 2 引用点，每点 context 8000 字节 → 每符号块 ~16KB；预算 28KB 只容 1 个符号整块。
        let big: Vec<SymRefs> = (0..4).map(|i| {
            let mut s = sr(&format!("sym{i}"), 1, 2);
            for h in &mut s.hits { h.context = "x".repeat(8000); }
            s
        }).collect();
        let out = render_entries(&big);
        assert!(out.contains("## sym0"), "首符号整块渲染");
        assert!(!out.contains("## sym1"), "预算耗尽后续符号整块丢弃");
        // total_renderable = 4×2 = 8；渲染了 sym0 的 2 个 → dropped = 6
        assert!(out.contains("… and 6 reference points omitted for budget"),
            "丢弃计数须精确 = total_renderable - rendered，实得: {out}");
    }

    #[test]
    fn build_dossier_end_to_end_and_graceful() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "pub fn target() {}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\nfn u4() { target(); }\nfn u5() { target(); }\n").unwrap();
        // 改动 target 的 bundle（diff 新增 target 的 def 行）
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into(),
        }] };
        let out = build_dossier(&b, root);
        assert!(out.contains("target"), "卷宗含改动符号引用点: {out}");
        assert!(out.contains("u1()"), "含 caller 上下文");

        // 非支持语言 → 空（无损降级）
        let py = Bundle { files: vec![FileChange {
            path: "app.py".into(), diff: "@@ -1,0 +1,1 @@\n+def f(): pass\n".into(),
        }] };
        assert_eq!(build_dossier(&py, root), "", "非支持语言降级为空");
    }

    #[test]
    fn render_entries_global_budget_and_empty() {
        assert_eq!(render_entries(&[]), "", "无符号 → 空");
        assert_eq!(render_entries(&[sr("x", 1, 0)]), "", "有符号但无引用点 → 空");
        // 灌爆全局预算：多符号大上下文
        let big: Vec<SymRefs> = (0..50).map(|i| {
            let mut s = sr(&format!("sym{i}"), 1, 5);
            for h in &mut s.hits { h.context = "x".repeat(2000); }
            s
        }).collect();
        let out = render_entries(&big);
        assert!(out.len() <= DOSSIER_MAX_BYTES + 200, "全局字节预算兜底: {}", out.len());
        assert!(out.contains("omitted for budget"), "全局丢弃标注");
    }

    #[test]
    fn build_dossier_mixed_bundle_skips_unsupported_keeps_supported() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "pub fn target() {}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\nfn u4() { target(); }\nfn u5() { target(); }\n").unwrap();
        // 同一 bundle 内混：不支持(.py) + 支持(.rs)。py 跳过、rs 仍产卷宗、不中止。
        let b = Bundle { files: vec![
            FileChange { path: "app.py".into(), diff: "@@ -1,0 +1,1 @@\n+def f(): pass\n".into() },
            FileChange { path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into() },
        ] };
        let out = build_dossier(&b, root);
        assert!(out.contains("target"), "支持语言文件仍产卷宗（不支持文件被跳过不中止）: {out}");
    }

    #[test]
    fn build_dossier_sparse_callers_are_gated_to_empty() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // target 只有 1 个 caller → 引用点 < DOSSIER_MIN_REFS → 门返空
        std::fs::write(root.join("src/a.rs"), "pub fn target() {}\nfn only_one() { target(); }\n").unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into(),
        }] };
        assert_eq!(build_dossier(&b, root), "", "引用点稀疏 → 调用密度门返空");
    }

    #[test]
    fn changed_def_ranges_covers_enclosing_body() {
        let d = rust_defs();
        // 文件：helper 在 0-1 行，caller 在 3-6 行（含 helper() 调用改动）
        let file: Vec<&str> = "fn helper() {}\n\nfn caller() {\n    helper();\n    let z = 2;\n}".lines().collect();
        // diff：改 caller 体内一行（新文件第 5 行 let z）
        let diff = "@@ -4,0 +5,1 @@\n+    let z = 2;\n";
        let ranges = changed_def_ranges(diff, &file, &d);
        // caller 起行 0-based = 2，块尾 = 5（`}`）
        assert_eq!(ranges, vec![(2usize, 5usize)], "外围 caller 的完整体行范围");
    }

    #[test]
    fn changed_def_ranges_dedups_multi_hunk_same_def() {
        let d = rust_defs();
        let file: Vec<&str> = "fn caller() {\n    a();\n    b();\n}".lines().collect();
        // 同一 caller 内两个改动 hunk → 只出一个范围
        let diff = "@@ -2,1 +2,1 @@\n-    a0();\n+    a();\n@@ -3,1 +3,1 @@\n-    b0();\n+    b();\n";
        let ranges = changed_def_ranges(diff, &file, &d);
        assert_eq!(ranges, vec![(0usize, 3usize)], "同 def 多 hunk 去重成一体");
    }
}
