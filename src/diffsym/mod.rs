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
/// ponytail: 与 hunk_anchors 平行的 hunk 游走（drift 风险，见 hunk_anchors 注记）；若再动一起改。
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

/// 每个 hunk 的首个改动行（0-based 新文件坐标），供 0 壳外围 def 定位。
/// ponytail: 与 hunk_changed_lines 的 hunk 游走平行（drift 风险）；两者若再动一起改，热点再统一。
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

// ---------------------------------------------------------------------------------------------
// 文件级 def 集 / 改动 def / 文件内一跳（M21a 批 E E-1，design §10.2 核与一跳层）
// ---------------------------------------------------------------------------------------------

/// 引用边 / 一跳 / 文件外片段只看像「符号」的名字（逐字移植 eval/risk_replay.py `fanin_worthy` / `GENERIC_NAMES`，
/// M21a 批 C K4；批 E K5 迁入本模块作唯一口径）：过滤短名、通用名与纯小写短单词（cancel / resp / object 之类）。
pub(crate) const GENERIC_NAMES: &[&str] = &[
    "main", "init", "new", "string", "error", "handler", "handle", "run", "get", "set", "test", "default",
    "index", "start", "stop", "close", "open", "read", "write", "name", "type", "value", "data", "config",
];

pub(crate) fn ref_worthy(n: &str) -> bool {
    if n.len() < 4 || GENERIC_NAMES.contains(&n.to_ascii_lowercase().as_str()) {
        return false;
    }
    if n.len() < 8 && n.chars().all(|c| c.is_ascii_lowercase()) {
        return false;
    }
    true
}

/// 文件级 def：0-based inclusive 行范围 + 名 + 种类（`Lang.defs` 的 kind 字面量）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DefRange {
    pub start: usize,
    pub end: usize,
    pub name: String,
    pub kind: &'static str,
}

/// 可调用 / 值 def 屏蔽其块内的 def（批 E K4）：fn 体里的 `const LIMIT` / 嵌套 fn、TS 箭头函数或对象字面量
/// （kind = const）体内的 `const x` 都是局部量，不是文件级符号。class / struct / trait 等容器不屏蔽——
/// 体内方法仍是文件级符号。
fn shields_nested(kind: &str) -> bool {
    matches!(kind, "fn" | "func" | "function" | "method" | "const" | "static")
}

/// 容器 def（class / struct / enum / trait / type）：体内方法是独立符号；容器本身不作一跳 caller，
/// 有内部 def 时也不作 callee / 核（整段展开会把整个 class 拖进来，批 E K4 / K5）。
pub(crate) fn is_container(kind: &str) -> bool {
    matches!(kind, "class" | "struct" | "enum" | "trait" | "type")
}

/// 文件级 def 全集（按 start 升序）：逐行 `match_def` + `block_end`；嵌在屏蔽 def 体内的命中丢弃。
/// ponytail: block_end 括号配平被字符串 / 注释里的花括号带偏时，屏蔽区间会吞掉后续 def——与骨架档同源的
/// 已知 heuristic 边角，禁真空兜底保证改动行仍在。
pub(crate) fn all_defs(lines: &[&str], defs: &[(&'static str, Regex)]) -> Vec<DefRange> {
    let mut out = Vec::new();
    // 外围栈：(块尾行, 是否屏蔽)；内层 def 后入栈且先结束，栈顶即最内层。
    let mut stack: Vec<(usize, bool)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        while stack.last().is_some_and(|&(end, _)| end < i) {
            stack.pop();
        }
        let Some((kind, name)) = match_def(line, defs) else { continue };
        if stack.iter().any(|&(_, shield)| shield) {
            continue; // 局部量 / 嵌套 fn
        }
        let end = block_end(lines, i);
        out.push(DefRange { start: i, end, name, kind });
        if end > i {
            stack.push((end, shields_nested(kind)));
        }
    }
    out
}

/// 容器 def 体内是否还有文件级 def（class 带方法 / trait 带方法）。
fn has_inner(all: &[DefRange], d: &DefRange) -> bool {
    all.iter().any(|o| o.start > d.start && o.end <= d.end)
}

/// 改动 def（批 E K3）：按 diff **全部改动行**（`hunk_changed_lines`，非仅每 hunk 首行）取最内层包含它的文件级
/// def，按 start 去重升序。带内部 def 的容器（class 带方法）不整段入核——改动落在字段行时交禁真空窗。
/// 既有 `changed_def_ranges`（hunk 首行锚定）供分组引用边与旧梯子，语义不变。
pub(crate) fn changed_defs(diff: &str, all: &[DefRange]) -> Vec<DefRange> {
    let mut picked = std::collections::BTreeSet::new();
    for l in hunk_changed_lines(diff) {
        let inner = all
            .iter()
            .enumerate()
            .filter(|(_, d)| d.start <= l && l <= d.end)
            .max_by_key(|(_, d)| d.start);
        if let Some((idx, d)) = inner {
            if is_container(d.kind) && has_inner(all, d) {
                continue;
            }
            picked.insert(idx);
        }
    }
    picked.into_iter().map(|i| all[i].clone()).collect()
}

/// def 体内出现的 `ref_worthy` 标识符（去自身名）；注释行（`//` / `#` / `*` / `/*` 起始）不参与。
pub(crate) fn body_names(lines: &[&str], r: &DefRange) -> std::collections::BTreeSet<String> {
    static IDENT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let ident = IDENT.get_or_init(|| Regex::new(r"[A-Za-z_$][A-Za-z0-9_$]*").expect("static ident regex"));
    let mut out = std::collections::BTreeSet::new();
    let end = r.end.min(lines.len().saturating_sub(1));
    for line in lines.iter().take(end + 1).skip(r.start) {
        let t = line.trim_start();
        if t.starts_with("//") || t.starts_with('#') || t.starts_with('*') || t.starts_with("/*") {
            continue;
        }
        for m in ident.find_iter(line) {
            let s = m.as_str();
            if s != r.name && ref_worthy(s) {
                out.insert(s.to_string());
            }
        }
    }
    out
}

/// 文件内一跳（design §10.2 一跳层）：callee = 改动 def 体内出现的同文件其他 def；caller = 体内出现改动 def 名的
/// 其他同文件 def。两组按 start 升序、互斥（既 callee 又 caller 只在 callees）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OneHop {
    pub callees: Vec<DefRange>,
    pub callers: Vec<DefRange>,
}

/// 只一跳、不传递；自引用与改动 def 之间的互引不计（它们本就在核里）；名字过滤 `ref_worthy`（批 E K5）。
/// callee 允许无内部 def 的容器（struct / enum / type：字段可见有用），排除带方法的 class；caller 只取非容器。
pub(crate) fn one_hop(lines: &[&str], all: &[DefRange], changed: &[DefRange]) -> OneHop {
    let changed_starts: std::collections::BTreeSet<usize> = changed.iter().map(|d| d.start).collect();
    let changed_names: std::collections::BTreeSet<&str> = changed.iter().map(|d| d.name.as_str()).collect();
    let mut callee_idx = std::collections::BTreeSet::new();
    for c in changed {
        let names = body_names(lines, c);
        for (i, d) in all.iter().enumerate() {
            if changed_starts.contains(&d.start) || !names.contains(&d.name) {
                continue;
            }
            if is_container(d.kind) && has_inner(all, d) {
                continue;
            }
            callee_idx.insert(i);
        }
    }
    let mut caller_idx = std::collections::BTreeSet::new();
    for (i, d) in all.iter().enumerate() {
        if changed_starts.contains(&d.start) || is_container(d.kind) || callee_idx.contains(&i) {
            continue;
        }
        if body_names(lines, d).iter().any(|n| changed_names.contains(n.as_str())) {
            caller_idx.insert(i);
        }
    }
    OneHop {
        callees: callee_idx.into_iter().map(|i| all[i].clone()).collect(),
        callers: caller_idx.into_iter().map(|i| all[i].clone()).collect(),
    }
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

    // ---- 批 E E-1：文件级 def / 改动 def / 一跳 ----

    fn names(v: &[DefRange]) -> Vec<&str> { v.iter().map(|d| d.name.as_str()).collect() }

    #[test]
    fn all_defs_skips_nested_in_callable_keeps_class_methods() {
        // Rust：fn 体内的局部 const / 嵌套 fn 不是文件级 def；impl 内方法是。
        let rs: Vec<&str> = "pub struct Cfg { a: u8 }\nimpl Cfg {\n    pub fn build_value(&self) -> u8 {\n        const LIMIT: u8 = 3;\n        fn inner_helper() {}\n        self.a + LIMIT\n    }\n}\nfn top_level_helper() {}\n".lines().collect();
        let all = all_defs(&rs, &rust_defs());
        assert_eq!(names(&all), vec!["Cfg", "build_value", "top_level_helper"], "{all:?}");
        assert_eq!((all[1].start, all[1].end, all[1].kind), (2, 6, "fn"));
        // TS：class 内方法保留；方法体与箭头函数体内的 const 不计。
        let ts: Vec<&str> = "export class Widget {\n  render(props: P): void {\n    const local_value = 1;\n  }\n  private helperMethod() {}\n}\nconst arrowFn = () => {\n  const inner_thing = 2;\n};\n".lines().collect();
        let all = all_defs(&ts, &compile_defs(lang_for("x.ts").unwrap()));
        assert_eq!(names(&all), vec!["Widget", "render", "helperMethod", "arrowFn"], "{all:?}");
        // Go：方法 + 顶层 func + type。
        let go: Vec<&str> = "type Server struct {}\nfunc (s *Server) TableName() string {\n    return \"x\"\n}\nfunc helperFunc() {}\n".lines().collect();
        let all = all_defs(&go, &compile_defs(lang_for("x.go").unwrap()));
        assert_eq!(names(&all), vec!["Server", "TableName", "helperFunc"], "{all:?}");
    }

    #[test]
    fn changed_defs_anchors_every_changed_line() {
        // 单 hunk 横跨两 def：hunk 首行锚定（changed_def_ranges）只见 alpha，全改动行锚定两者都见（K3）。
        let file: Vec<&str> = "fn alpha_one() {\n    a();\n}\nfn beta_two() {\n    b();\n}\n".lines().collect();
        let diff = "@@ -2,3 +2,3 @@\n+    a();\n }\n+fn beta_two() {\n";
        let d = rust_defs();
        assert_eq!(changed_def_ranges(diff, &file, &d).len(), 1, "旧口径只锚 hunk 首行");
        let all = all_defs(&file, &d);
        assert_eq!(names(&changed_defs(diff, &all)), vec!["alpha_one", "beta_two"]);
        // 改动落在 def 之外（文件头 use 区）→ 无核（交禁真空窗）。
        let file2: Vec<&str> = "use std::fmt;\n\nfn only_fn() {}\n".lines().collect();
        assert!(changed_defs("@@ -1,0 +1,1 @@\n+use std::fmt;\n", &all_defs(&file2, &d)).is_empty());
    }

    #[test]
    fn changed_defs_skips_container_with_inner_defs_and_takes_innermost() {
        let ts: Vec<&str> = "export class Widget {\n  private count_value = 0;\n  render(props: P): void {\n    draw(props);\n  }\n}\n".lines().collect();
        let all = all_defs(&ts, &compile_defs(lang_for("x.ts").unwrap()));
        // 改字段行：最内层是带方法的 class → 不整段入核
        assert!(changed_defs("@@ -2,1 +2,1 @@\n+  private count_value = 0;\n", &all).is_empty());
        // 改方法体：最内层是方法 → 只取方法
        assert_eq!(names(&changed_defs("@@ -4,1 +4,1 @@\n+    draw(props);\n", &all)), vec!["render"]);
    }

    #[test]
    fn one_hop_callee_and_caller_rust_go_ts() {
        let rs: Vec<&str> = "fn compute_total(xs: &[u8]) -> u8 { xs.iter().sum() }\nfn wrap_body(x: u8) -> u8 { compute_total(&[x]) + helper_thing() }\nfn helper_thing() -> u8 { 1 }\nfn outer_caller() -> u8 { wrap_body(3) }\nfn unrelated_fn() -> u8 { 9 }\n".lines().collect();
        let d = rust_defs();
        let all = all_defs(&rs, &d);
        let changed: Vec<DefRange> = all.iter().filter(|x| x.name == "wrap_body").cloned().collect();
        let hop = one_hop(&rs, &all, &changed);
        assert_eq!(names(&hop.callees), vec!["compute_total", "helper_thing"], "{hop:?}");
        assert_eq!(names(&hop.callers), vec!["outer_caller"], "{hop:?}");

        let go: Vec<&str> = "func computeTotal(xs []int) int { return 0 }\nfunc wrapBody(x int) int { return computeTotal(nil) }\nfunc outerCaller() int { return wrapBody(1) }\n".lines().collect();
        let gd = compile_defs(lang_for("x.go").unwrap());
        let all = all_defs(&go, &gd);
        let changed: Vec<DefRange> = all.iter().filter(|x| x.name == "wrapBody").cloned().collect();
        let hop = one_hop(&go, &all, &changed);
        assert_eq!((names(&hop.callees), names(&hop.callers)), (vec!["computeTotal"], vec!["outerCaller"]));

        let ts: Vec<&str> = "export class Widget {\n  computeTotal(): number { return 1 }\n  wrapBody(): number { return this.computeTotal() }\n  outerCaller(): number { return this.wrapBody() }\n}\n".lines().collect();
        let td = compile_defs(lang_for("x.ts").unwrap());
        let all = all_defs(&ts, &td);
        let changed: Vec<DefRange> = all.iter().filter(|x| x.name == "wrapBody").cloned().collect();
        let hop = one_hop(&ts, &all, &changed);
        assert_eq!((names(&hop.callees), names(&hop.callers)), (vec!["computeTotal"], vec!["outerCaller"]));
        assert!(!hop.callers.iter().any(|c| c.name == "Widget"), "容器不作 caller");
    }

    #[test]
    fn one_hop_excludes_self_generic_names_containers_with_inner_and_comment_mentions() {
        let rs: Vec<&str> = "pub struct RouteTable { pub depth: u8 }\npub trait Walker {\n    fn walk_tree(&self);\n}\nfn handle() {}\nfn wrap_body(c: RouteTable) -> u8 {\n    handle();\n    wrap_body(c);\n    c.depth\n}\nfn commenter() {\n    // wrap_body is mentioned only in a comment\n    let x = 1;\n}\nfn data() {}\n".lines().collect();
        let d = rust_defs();
        let all = all_defs(&rs, &d);
        let changed: Vec<DefRange> = all.iter().filter(|x| x.name == "wrap_body").cloned().collect();
        let hop = one_hop(&rs, &all, &changed);
        // RouteTable（无内部 def 的容器）作 callee；handle / data 通用名不计；自引用不计；Walker（带方法的 trait）不计
        assert_eq!(names(&hop.callees), vec!["RouteTable"], "{hop:?}");
        assert!(hop.callers.is_empty(), "注释里提到不算 caller: {hop:?}");
        assert!(body_names(&rs, &changed[0]).contains("RouteTable") && !body_names(&rs, &changed[0]).contains("wrap_body"));
    }
}
