//! 卷宗引擎（M14 支柱 2）：diff → 改动符号 → 仓内引用点 → 事实卷宗字符串。
//! hermetic：喂 (bundle, root) 出字符串，零 LLM。移植自 eval/harness.py（查全 1.000）。

// ponytail: 2-A 引擎自底向上构建，wiring 前内部项仅被测试引用 → dead_code 假阳。
// Task 10 接线（build_dossier 进 cli）后全链可达，届时删除此 allow 并验证零警告。
#![allow(dead_code)]

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
}
