//! diff/符号解析核心（M14 支柱 2 遗产，E8-E 安乐死引用点引擎后收窄）：diff → 改动符号 →
//! def 体/骨架坐标。hermetic：喂 (diff, file_lines, defs) 出坐标，零 LLM、零全仓 walk——
//! 引用点搜索（reference_points/build_dossier）已随 SCIP/1 壳一并拆除，见 pack::body_section。
//! 移植自 eval/harness.py（查全 1.000）。

use regex::Regex;

pub(crate) struct Lang {
    exts: &'static [&'static str],
    defs: &'static [(&'static str, &'static str)],
}

// Rust def 正则：逐条照抄 eval/harness.py DEF_PATTERNS（已验证 119/119 符号锚定）。
pub(crate) const RUST: Lang = Lang {
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

pub(crate) fn match_def(line: &str, defs: &[(&'static str, Regex)]) -> Option<(&'static str, String)> {
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
pub(crate) fn block_end(lines: &[&str], i: usize) -> usize {
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

/// 契约桶（M15-S2-b）：A=签名面变（传递展/caller 给体）、B=仅体变（caller 给片段）。
/// C 装饰桶（注释/空白 skip）缓，见 mvp-delta-ledger #12。
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bucket {
    A,
    B,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChangedSym {
    pub name: String,
    pub kind: &'static str,
    pub file: String,
    pub bucket: Bucket,
    pub def_line0: usize, // 0-based 新树 def 行 = 位置对连接键（symbol_at 直接用）
}

// @@ -a,b +c,d @@ 抓新文件起始行 c 与计数 d（d 缺省按 1 处理——unified diff 单行 hunk
// 允许省略计数，本仓生成的 diff 目前总带显式计数，防御性兜底）。
pub(crate) fn parse_hunk_new_range(line: &str) -> Option<(usize, usize)> {
    let after_plus = line.split('+').nth(1)?; // "c,d @@ …" 或 "c @@ …"
    let seg = after_plus.split([' ', '@']).next()?; // "c,d" 或 "c"
    let mut parts = seg.split(',');
    let c = parts.next()?.parse::<usize>().ok()?;
    let d = parts.next().and_then(|s| s.parse::<usize>().ok()).unwrap_or(1);
    Some((c, d))
}

fn parse_hunk_new_start(line: &str) -> Option<usize> {
    parse_hunk_new_range(line).map(|(c, _)| c)
}

/// diff 全部改动行（0-based 新文件坐标）：每个 + 行的行号；纯删除行取删除点坐标
/// （同 hunk_anchors 语义——删除发生处的新文件位置，连续删除会重复、调用方去重）。
/// 供骨架档保留 hunk 的全部改动行（hunk_anchors 只给每 hunk 首行，多行 hunk 会丢内容）。
/// ponytail: 第三个平行 hunk 游走（见 hunk_anchors 注记的 drift 风险）；若再动一起改。
pub(crate) fn hunk_changed_lines(diff: &str) -> Vec<usize> {
    let mut out = Vec::new();
    let mut new_line = 0usize;
    for raw in diff.lines() {
        if raw.starts_with("+++ ") || raw.starts_with("--- ") {
            continue;
        }
        if raw.starts_with("@@") {
            new_line = parse_hunk_new_start(raw).unwrap_or(1);
            continue;
        }
        if raw.starts_with('\\') {
            continue; // No-newline 标记非内容行
        }
        match raw.as_bytes().first() {
            Some(b'+') => {
                out.push(new_line.saturating_sub(1));
                new_line += 1;
            }
            Some(b'-') => out.push(new_line.saturating_sub(1)),
            _ => new_line += 1,
        }
    }
    out
}

/// diff（任意上下文级别）→ 改动符号，按 name 去重（grep 按名一次即可）。
pub fn symbols_in_file(diff: &str, file_lines: &[&str], file: &str, defs: &[(&'static str, Regex)]) -> Vec<ChangedSym> {
    let mut out: Vec<ChangedSym> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let push = |name: String, kind: &'static str, bucket: Bucket, def_line0: usize, out: &mut Vec<ChangedSym>, seen: &mut std::collections::HashSet<String>| {
        if seen.insert(name.clone()) {
            out.push(ChangedSym { name, kind, file: file.to_string(), bucket, def_line0 });
        }
    };
    let mut new_line = 0usize; // 当前 + / 上下文行的新文件行号（1-based）
    let mut hunk_anchor: Option<usize> = None; // 本 hunk 首个改动行（0-based）
    for raw in diff.lines() {
        if raw.starts_with("+++ ") || raw.starts_with("--- ") {
            continue;
        }
        if raw.starts_with("@@") {
            // 上一 hunk 收尾：外围 def（体变=B，def_line0=外围 def 起行）
            if let Some(a) = hunk_anchor.take() {
                if let Some(st) = enclosing_def_start(file_lines, a, defs) {
                    if let Some((kind, n)) = match_def(file_lines[st], defs) {
                        push(n, kind, Bucket::B, st, &mut out, &mut seen);
                    }
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
                    // + 行本身命中 def 正则=签名行进 diff → A，def_line0=该 + 行
                    push(name, kind, Bucket::A, anchor0, &mut out, &mut seen);
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
        if let Some(st) = enclosing_def_start(file_lines, a, defs) {
            if let Some((kind, n)) = match_def(file_lines[st], defs) {
                push(n, kind, Bucket::B, st, &mut out, &mut seen);
            }
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
        let st = enclosing_def_start(&src, 2, &d).unwrap();
        assert_eq!(match_def(src[st], &d), Some(("fn", "bar".into())));
        // 落在两 fn 之间的空行 → 无外围
        let src2: Vec<&str> = "fn a() {}\n\nfn b() {}".lines().collect();
        assert_eq!(enclosing_def_start(&src2, 1, &d), None);
    }

    #[test]
    fn innermost_enclosing_empty_lines_is_none() {
        let d = rust_defs();
        assert_eq!(enclosing_def_start(&[], 0, &d), None, "空文件不得 panic");
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
        assert_eq!(syms, vec![ChangedSym { name: "Config".into(), kind: "struct", file: "src/c.rs".into(), bucket: Bucket::B, def_line0: 0 }]);
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

    #[test]
    fn symbols_in_file_tags_bucket_and_def_line() {
        let lines: Vec<&str> = "pub fn foo(x: i32) {\n    helper();\n}\n".lines().collect();
        let defs = compile_defs(&RUST);
        // 改签名行（+ 行匹配 fn 正则）→ A，def_line0=0
        let a = symbols_in_file("@@ -1,0 +1,1 @@\n+pub fn foo(x: i32) {\n", &lines, "src/a.rs", &defs);
        let fa = a.iter().find(|s| s.name == "foo").unwrap();
        assert_eq!(fa.bucket, Bucket::A, "签名行进 diff = A");
        assert_eq!(fa.def_line0, 0, "A 桶 def_line0=签名行");
        // 只改体（+ 行是 helper 调用、外围 def=foo 在第0行）→ B，def_line0=0
        let b = symbols_in_file("@@ -2,0 +2,1 @@\n+    helper();\n", &lines, "src/a.rs", &defs);
        let fb = b.iter().find(|s| s.name == "foo").unwrap();
        assert_eq!(fb.bucket, Bucket::B, "仅体变 = B");
        assert_eq!(fb.def_line0, 0, "B 桶 def_line0=外围 def 行");
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
