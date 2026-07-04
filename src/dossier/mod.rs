//! 卷宗引擎（M14 支柱 2）：diff → 改动符号 → 仓内引用点 → 事实卷宗字符串。
//! hermetic：喂 (bundle, root) 出字符串，零 LLM。移植自 eval/harness.py（查全 1.000）。

use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ignore::WalkBuilder;
use regex::Regex;
use std::path::Path;

struct Lang {
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

fn lang_for(path: &str) -> Option<&'static Lang> {
    let ext = path.rsplit('.').next()?;
    LANGS.iter().copied().find(|l| l.exts.contains(&ext))
}

fn compile_defs(lang: &Lang) -> Vec<(&'static str, Regex)> {
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

/// line0（0-based）的最内层外围 def 名：向上首个「块覆盖 line0」的 def。
/// impl/for 头非 def，天然穿透到内层 fn（照 D5 简化：只取名，不分类 impl_trait）。
/// ponytail: 每候选行调 block_end = O(n²) 最坏；单文件几千行够用，热点再优化
fn innermost_enclosing_def(lines: &[&str], line0: usize, defs: &[(&'static str, Regex)]) -> Option<(&'static str, String)> {
    if lines.is_empty() {
        return None;
    }
    for i in (0..=line0.min(lines.len().saturating_sub(1))).rev() {
        if let Some(kd) = match_def(lines[i], defs) {
            if i == line0 || block_end(lines, i) >= line0 {
                return Some(kd);
            }
        }
    }
    None
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
            Some(b'-') => { /* 删除行不推进新行号，不入卷宗 */ }
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

#[derive(Clone, Debug)]
pub struct RefHit {
    pub path: String,
    pub line: usize,
    pub context: String,
    pub is_doc: bool,
}

#[derive(Clone, Debug)]
pub struct SymRefs {
    pub name: String,
    pub kind: &'static str,
    pub defs_same_name: usize,
    pub hits: Vec<RefHit>,
}

fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//") || t.starts_with("/*") || t.starts_with('*') || t.starts_with('#')
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
    let mut out = Vec::new();
    for sym in syms {
        let raw = grep_word(&sym.name, root);
        let mut defs_same_name = 0usize;
        let mut hits = Vec::new();
        for (path, line) in raw {
            let lines = read(&path, &mut file_cache);
            let hit_line = lines.get(line.saturating_sub(1)).map(String::as_str).unwrap_or("");
            // 命中行本身是同名 def → 歧义度，不作引用点
            if let Some((_, n)) = match_def(hit_line, defs) {
                if n == sym.name {
                    defs_same_name += 1;
                    continue;
                }
            }
            let lo = line.saturating_sub(4); // ±3 行（含头行）
            let hi = (line + 3).min(lines.len());
            let context = lines.get(lo..hi).unwrap_or(&[]).join("\n");
            hits.push(RefHit { path, line, context, is_doc: is_comment_line(hit_line) });
        }
        out.push(SymRefs { name: sym.name.clone(), kind: sym.kind, defs_same_name, hits });
    }
    out
}

const DOSSIER_PER_NAME_CAP: usize = 10;
// ≈8K tokens @ bytes/3.5；含 CJK 注释偏保守。文档记假设，live 换真 tokenizer 校常量（D7）。
const DOSSIER_MAX_BYTES: usize = 28_000;

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
            let doc = if h.is_doc { "[doc] " } else { "" };
            block.push_str(&format!("{doc}{}:{}\n{}\n\n", h.path, h.line, h.context));
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
        // 引用点 = c1/c2 的调用行 + doc 提及行；def 行本身被排除
        assert!(r.hits.iter().any(|h| h.path == "src/a.rs" && h.context.contains("c1")));
        assert!(r.hits.iter().any(|h| h.is_doc), "doc 提及命中标 is_doc");
        assert!(!r.hits.iter().any(|h| h.line == 1 && h.path == "src/a.rs"), "def 行不作引用点");
    }

    fn sr(name: &str, defs_same_name: usize, n: usize, doc_idx: Option<usize>) -> SymRefs {
        let hits = (0..n).map(|i| RefHit {
            path: format!("src/f{i}.rs"), line: i + 1,
            context: format!("    {name}(); // hit {i}"),
            is_doc: Some(i) == doc_idx,
        }).collect();
        SymRefs { name: name.into(), kind: "fn", defs_same_name, hits }
    }

    #[test]
    fn render_entries_caps_per_name_and_annotates() {
        let out = render_entries(&[sr("run", 3, 25, Some(0))]);
        assert!(out.matches("src/f").count() <= DOSSIER_PER_NAME_CAP, "每名 ≤10 引用点");
        assert!(out.contains("+15 more, 3 defs same name, heuristic"), "cap 标注: {out}");
        assert!(out.contains("3 defs share this name (incl. trait dispatch), heuristic"), "歧义标注");
        assert!(out.contains("[doc]"), "doc 命中标形态");
    }

    #[test]
    fn render_entries_budget_drop_count_is_exact() {
        // 4 符号 × 2 引用点，每点 context 8000 字节 → 每符号块 ~16KB；预算 28KB 只容 1 个符号整块。
        let big: Vec<SymRefs> = (0..4).map(|i| {
            let mut s = sr(&format!("sym{i}"), 1, 2, None);
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
            "pub fn target() {}\nfn user() { target(); }\n").unwrap();
        // 改动 target 的 bundle（diff 新增 target 的 def 行）
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into(),
        }] };
        let out = build_dossier(&b, root);
        assert!(out.contains("target"), "卷宗含改动符号引用点: {out}");
        assert!(out.contains("user()"), "含 caller 上下文");

        // 非支持语言 → 空（无损降级）
        let py = Bundle { files: vec![FileChange {
            path: "app.py".into(), diff: "@@ -1,0 +1,1 @@\n+def f(): pass\n".into(),
        }] };
        assert_eq!(build_dossier(&py, root), "", "非支持语言降级为空");
    }

    #[test]
    fn render_entries_global_budget_and_empty() {
        assert_eq!(render_entries(&[]), "", "无符号 → 空");
        assert_eq!(render_entries(&[sr("x", 1, 0, None)]), "", "有符号但无引用点 → 空");
        // 灌爆全局预算：多符号大上下文
        let big: Vec<SymRefs> = (0..50).map(|i| {
            let mut s = sr(&format!("sym{i}"), 1, 5, None);
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
        std::fs::write(root.join("src/a.rs"), "pub fn target() {}\nfn user() { target(); }\n").unwrap();
        // 同一 bundle 内混：不支持(.py) + 支持(.rs)。py 跳过、rs 仍产卷宗、不中止。
        let b = Bundle { files: vec![
            FileChange { path: "app.py".into(), diff: "@@ -1,0 +1,1 @@\n+def f(): pass\n".into() },
            FileChange { path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into() },
        ] };
        let out = build_dossier(&b, root);
        assert!(out.contains("target"), "支持语言文件仍产卷宗（不支持文件被跳过不中止）: {out}");
    }
}
