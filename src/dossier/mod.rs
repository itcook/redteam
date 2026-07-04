//! 卷宗引擎（M14 支柱 2）：diff → 改动符号 → 仓内引用点 → 事实卷宗字符串。
//! hermetic：喂 (bundle, root) 出字符串，零 LLM。移植自 eval/harness.py（查全 1.000）。

// ponytail: 2-A 引擎自底向上构建，wiring 前内部项仅被测试引用 → dead_code 假阳。
// Task 10 接线（build_dossier 进 cli）后全链可达，届时删除此 allow 并验证零警告。
#![allow(dead_code)]

use regex::Regex;

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
fn innermost_enclosing_def(lines: &[&str], line0: usize, defs: &[(&'static str, Regex)]) -> Option<String> {
    if lines.is_empty() {
        return None;
    }
    for i in (0..=line0.min(lines.len().saturating_sub(1))).rev() {
        if let Some((_, name)) = match_def(lines[i], defs) {
            if i == line0 || block_end(lines, i) >= line0 {
                return Some(name);
            }
        }
    }
    None
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
        assert_eq!(innermost_enclosing_def(&src, 2, &d), Some("bar".into()));
        // 落在两 fn 之间的空行 → 无外围
        let src2: Vec<&str> = "fn a() {}\n\nfn b() {}".lines().collect();
        assert_eq!(innermost_enclosing_def(&src2, 1, &d), None);
    }

    #[test]
    fn innermost_enclosing_empty_lines_is_none() {
        let d = rust_defs();
        assert_eq!(innermost_enclosing_def(&[], 0, &d), None, "空文件不得 panic");
    }
}
