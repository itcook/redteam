# M14 卷宗（dossier）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** fan-out 前二进制为每 bundle 产一份纯事实卷宗（改动符号的仓内引用点，≤8K tokens），注入该 bundle 全部 cell，把「每 cell 自己 grep callers/callees」一次性做掉、三模型共享——cell 工具轮数 12→3-6，与 M13 正交相乘。

**Architecture:** 新增 hermetic 引擎模块 `src/dossier/`（LangSpec 数据表 + 通用提取器 + in-process grep 引用搜索 + 两级帽/四标注），零耦合、可脱 LLM 单测。注入走 M13 `{{INCREMENTAL_ROUND}}` 同款穿线：新 `{{DOSSIER}}` 占位符 + `render_dossier_section` + `run_code_fanout` 收 per-bundle 卷宗段。

**Tech Stack:** Rust 单 crate；复用既有依赖 `grep-regex`/`grep-searcher`/`ignore`（in-process 搜索，**不 shell 到 rg**）；新增 `regex`（逐行 def 匹配，已在依赖树中传递引入）。

**上游 spec：** `.doc/design/2026-07-04-redteam-dossier-design.md`（D1-D9）。移植参照：`eval/harness.py`（穷人版启发式的已验证实现，查全 1.000/431）。

## Global Constraints

- **只装事实不装观点**：卷宗条目 = 代码摘录 + 位置 + heuristic 标注；判断归模型（D2/D8）。
- **空渲染 = 零行为变化**：非支持语言 / 无改动符号 / 无引用点 → `build_dossier` 返回 `""`，`render_dossier_section("")` 返回 `""`，退回今天现状（D6）。
- **引用搜索 in-process**：复用 `grep_regex::RegexMatcher` + `ignore::WalkBuilder`（照 `src/tools/mod.rs:223 do_search_repo`），词边界 `\b<name>\b`，**不 shell rg**。
- **体积字节估算，零 tokenizer**：`.len()` 字节口径（照 `bundle::cap_entries`），`DOSSIER_MAX_BYTES` 按 ~8K tokens×bytes/3.5 标定；文档记假设（D7）。
- **两级帽**：per-name `DOSSIER_PER_NAME_CAP=10` + per-bundle `DOSSIER_MAX_BYTES` 全局兜底（D7）。
- **四标注串（英文，遵 devtool 骨架惯例）**：cap 咬 `+N more, K defs same name, heuristic`；歧义 `K defs share this name (incl. trait dispatch), heuristic`；doc 命中条目前缀 `[doc]`；全局丢弃 `… and N reference points omitted for budget`（D7）。
- **v1 语言 = Rust + Go + TS/JS**，按扩展名派发，其余 `None` 降级（D4）。
- **不建 trait、不建后端槽位**（scout/tree-sitter 已毙）；加语言 = 加一个 LangSpec 条目（D5）。
- **命名**：产品/模块名 `dossier`（redteam 空间），勿用旧名。commit 前缀 `feat(dossier):` / `test(dossier):`。
- Commit trailer：`Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`（hobby 身份 S.Kai<laiwei@me.com> 已配）。

---

## File Structure

- `src/dossier/mod.rs`（新建，2-A 引擎全部）：公开 API `build_dossier`；内含 LangSpec 表 + `lang_for` + `match_def` + `block_end`/`innermost_enclosing_def` + `changed_symbols` + `reference_points` + `render_entries` + 常量/标注。单文件（照 `bundle/mod.rs` 单文件惯例；超 ~600 行再拆）。
- `src/lib.rs:7`（改）：`pub mod dossier;`（domain 后、evidence 前）。
- `Cargo.toml`（改）：`regex = "1"`。
- `src/prompt/mod.rs`（改，2-B）：新增 `render_dossier_section`；`build_bundle_prompt` 加 `dossier: &str` 参数。
- `src/prompt/templates/adversarial-code-bundle.md`（改，2-B）：加 `<dossier>{{DOSSIER}}` 块 + `<tools>` 一句和解。
- `src/orchestrate/mod.rs`（改，2-B）：`run_code_fanout` 加 `dossiers: &[String]` 参数。
- `src/cli.rs`（改，2-B）：两处 fanout 调用点（`run_review`/`run_incremental_review`）建 per-bundle 卷宗段 + `REDTEAM_NO_DOSSIER` 开关（A/B off 臂）。

---

# 批次 2-A：卷宗引擎（hermetic，纯新模块，零 LLM 可测）

### Task 1: LangSpec 数据表 + `match_def`

**Files:**
- Create: `src/dossier/mod.rs`
- Modify: `src/lib.rs:7`（加 `pub mod dossier;`）
- Modify: `Cargo.toml`（`[dependencies]` 段加 `regex = "1"`）

**Interfaces:**
- Produces:
  - `struct Lang { exts: &'static [&'static str], defs: &'static [(&'static str, &'static str)] }`（`defs` = (kind, 正则源)）
  - `fn lang_for(path: &str) -> Option<&'static Lang>`
  - `fn compile_defs(lang: &Lang) -> Vec<(&'static str, regex::Regex)>`
  - `fn match_def(line: &str, defs: &[(&'static str, regex::Regex)]) -> Option<(&'static str, String)>`（返回 (kind, name)；TS method 命中且 name 属关键字 → None）

- [ ] **Step 1: 写失败测试**（`src/dossier/mod.rs` 末尾 `#[cfg(test)] mod tests`）

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn rust_defs() -> Vec<(&'static str, regex::Regex)> { compile_defs(lang_for("a.rs").unwrap()) }

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
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam dossier::tests 2>&1 | tail -20`
Expected: 编译失败 / `lang_for` 未定义。

- [ ] **Step 3: 写实现**（`src/dossier/mod.rs` 顶部）

```rust
//! 卷宗引擎（M14 支柱 2）：diff → 改动符号 → 仓内引用点 → 事实卷宗字符串。
//! hermetic：喂 (bundle, root) 出字符串，零 LLM。移植自 eval/harness.py（查全 1.000）。
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
```

并在 `src/lib.rs` 第 6 行（`pub mod domain;`）后插入：
```rust
pub mod dossier;
```
`Cargo.toml` `[dependencies]` 段加一行（`regex` 经 grep-regex 已在依赖树，直接声明）：
```toml
regex = "1"
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p redteam dossier::tests 2>&1 | tail -20`
Expected: 3 tests PASS。

- [ ] **Step 5: Commit**

```bash
git add src/dossier/mod.rs src/lib.rs Cargo.toml Cargo.lock
git commit -m "feat(dossier): LangSpec 数据表 + match_def（Rust/Go/TS，D4/D5）"
```

---

### Task 2: 括号配平 enclosing——`block_end` + `innermost_enclosing_def`

**Files:**
- Modify: `src/dossier/mod.rs`

**Interfaces:**
- Consumes: `match_def`（Task 1）
- Produces:
  - `fn block_end(lines: &[&str], i: usize) -> usize`（从 def 行 i 括号配平找块尾行；无块 `;` 结尾返回 i）
  - `fn innermost_enclosing_def(lines: &[&str], line0: usize, defs: &[(&'static str, Regex)]) -> Option<String>`（line0 最内层外围 def 名）

- [ ] **Step 1: 写失败测试**（追加进 `mod tests`）

```rust
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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam dossier::tests::block_end_balances_braces dossier::tests::innermost_enclosing_walks_up 2>&1 | tail -20`
Expected: 编译失败 / 函数未定义。

- [ ] **Step 3: 写实现**（追加进 `src/dossier/mod.rs`，移植 harness.py `block_end`/`enclosing_chain`）

```rust
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
    for i in (0..=line0.min(lines.len().saturating_sub(1))).rev() {
        if let Some((_, name)) = match_def(lines[i], defs) {
            if i == line0 || block_end(lines, i) >= line0 {
                return Some(name);
            }
        }
    }
    None
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p redteam dossier::tests 2>&1 | tail -20`
Expected: 全 PASS。

- [ ] **Step 5: Commit**

```bash
git add src/dossier/mod.rs
git commit -m "feat(dossier): 括号配平 enclosing（block_end + innermost_enclosing_def）"
```

---

### Task 3: `changed_symbols`——diff → 改动符号名（上下文级别无关）

**Files:**
- Modify: `src/dossier/mod.rs`

**Interfaces:**
- Consumes: `match_def`, `innermost_enclosing_def`（Task 1/2）
- Produces:
  - `struct ChangedSym { pub name: String, pub kind: &'static str, pub file: String }`
  - `fn symbols_in_file(diff: &str, file_lines: &[&str], file: &str, defs: &[(&'static str, Regex)]) -> Vec<ChangedSym>`（按 `+` 行 def + 每 hunk 首个改动行的外围 def；按 name 去重）

**设计注**：只认 diff 的 `+` 行（改动行，上下文级别无关，摆脱 -U0 依赖）+ 每 hunk 首个 `+` 行处的外围 def（覆盖「改了既有函数体」情形）。删除符号（`-` 行 def）不入卷宗（旧调用点靠编译器/评审兜底，D 非目标）。

- [ ] **Step 1: 写失败测试**

```rust
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
    assert!(names.contains(&"added"), "新增 def 行进符号集；实得 {names:?}");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam dossier::tests::symbols_from_diff 2>&1 | tail -20`
Expected: 编译失败 / 未定义。

- [ ] **Step 3: 写实现**（追加）

```rust
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
    let mut push = |name: String, kind: &'static str, out: &mut Vec<ChangedSym>, seen: &mut std::collections::HashSet<String>| {
        if seen.insert(name.clone()) {
            out.push(ChangedSym { name, kind, file: file.to_string() });
        }
    };
    let mut new_line = 0usize; // 当前 + / 上下文行的新文件行号（1-based）
    let mut hunk_anchor: Option<usize> = None; // 本 hunk 首个改动行（0-based）
    for raw in diff.lines() {
        if raw.starts_with("+++") || raw.starts_with("---") {
            continue;
        }
        if raw.starts_with("@@") {
            // 上一 hunk 收尾：外围 def
            if let Some(a) = hunk_anchor.take() {
                if let Some(n) = innermost_enclosing_def(file_lines, a, defs) {
                    push(n, "fn", &mut out, &mut seen);
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
        if let Some(n) = innermost_enclosing_def(file_lines, a, defs) {
            push(n, "fn", &mut out, &mut seen);
        }
    }
    out
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p redteam dossier::tests 2>&1 | tail -20`
Expected: 全 PASS。

- [ ] **Step 5: Commit**

```bash
git add src/dossier/mod.rs
git commit -m "feat(dossier): changed_symbols（+行 def + hunk 外围，上下文无关）"
```

---

### Task 4: `reference_points`——in-process 词边界搜引用 + ±3 上下文 + 歧义度

**Files:**
- Modify: `src/dossier/mod.rs`

**Interfaces:**
- Consumes: `match_def`, `ChangedSym`（Task 1/3）
- Produces:
  - `struct RefHit { pub path: String, pub line: usize, pub context: String, pub is_doc: bool }`
  - `struct SymRefs { pub name: String, pub kind: &'static str, pub defs_same_name: usize, pub hits: Vec<RefHit> }`
  - `fn reference_points(syms: &[ChangedSym], root: &Path, defs: &[(&'static str, Regex)]) -> Vec<SymRefs>`

**设计注**：照 `tools/mod.rs:223 do_search_repo` 用 `grep_regex::RegexMatcher(\bNAME\b)` + `ignore::WalkBuilder` in-process 搜（**不 shell rg**）。命中行若本身是同名 def → 计入 `defs_same_name`、不作引用点。引用点取 ±3 行拼 `context`；命中行是注释 → `is_doc`。

- [ ] **Step 1: 写失败测试**

```rust
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
```

（`tempfile` 已是 dev/test 依赖——`cli.rs` 测试已用 `tempdir`；若 `cargo test` 报缺，`Cargo.toml [dev-dependencies]` 加 `tempfile = "3"`。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam dossier::tests::reference_points 2>&1 | tail -20`
Expected: 编译失败 / 未定义。

- [ ] **Step 3: 写实现**（追加；`use` 补到文件顶部）

```rust
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ignore::WalkBuilder;

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
    let mut read = |rel: &str, cache: &mut std::collections::HashMap<String, Vec<String>>| -> Vec<String> {
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
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p redteam dossier::tests 2>&1 | tail -20`
Expected: 全 PASS。

- [ ] **Step 5: Commit**

```bash
git add src/dossier/mod.rs Cargo.toml Cargo.lock
git commit -m "feat(dossier): reference_points（in-process 词边界搜 + ±3 上下文 + 歧义度）"
```

---

### Task 5: `render_entries`——两级帽 + 四标注

**Files:**
- Modify: `src/dossier/mod.rs`

**Interfaces:**
- Consumes: `SymRefs`（Task 4）
- Produces:
  - `const DOSSIER_PER_NAME_CAP: usize = 10;`
  - `const DOSSIER_MAX_BYTES: usize = 28_000;`（≈8K tokens @ bytes/3.5；含 CJK 注释偏保守，live 校）
  - `fn render_entries(refs: &[SymRefs]) -> String`（空引用 → `""`）

- [ ] **Step 1: 写失败测试**

```rust
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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam dossier::tests::render_entries 2>&1 | tail -20`
Expected: 编译失败 / 未定义。

- [ ] **Step 3: 写实现**（追加）

```rust
const DOSSIER_PER_NAME_CAP: usize = 10;
// ≈8K tokens @ bytes/3.5；含 CJK 注释偏保守。文档记假设，live 换真 tokenizer 校常量（D7）。
const DOSSIER_MAX_BYTES: usize = 28_000;

/// SymRefs 列表 → 卷宗事实块（不含 <dossier> 包裹与散文，那是 prompt 层的活）。空引用 → ""。
pub fn render_entries(refs: &[SymRefs]) -> String {
    let mut out = String::new();
    let mut dropped_for_budget = 0usize;
    'sym: for sr in refs {
        if sr.hits.is_empty() {
            continue;
        }
        let ambig = if sr.defs_same_name > 1 {
            format!(" [{} defs share this name (incl. trait dispatch), heuristic]", sr.defs_same_name)
        } else {
            String::new()
        };
        let header = format!("## {} ({}){}\n", sr.name, sr.kind, ambig);
        if out.len() + header.len() > DOSSIER_MAX_BYTES {
            dropped_for_budget += sr.hits.len();
            continue;
        }
        out.push_str(&header);
        let shown = sr.hits.len().min(DOSSIER_PER_NAME_CAP);
        for h in &sr.hits[..shown] {
            let doc = if h.is_doc { "[doc] " } else { "" };
            let entry = format!("{doc}{}:{}\n{}\n\n", h.path, h.line, h.context);
            if out.len() + entry.len() > DOSSIER_MAX_BYTES {
                dropped_for_budget += sr.hits.len(); // 本符号剩余全算丢
                break 'sym;
            }
            out.push_str(&entry);
        }
        if sr.hits.len() > DOSSIER_PER_NAME_CAP {
            out.push_str(&format!(
                "+{} more, {} defs same name, heuristic\n\n",
                sr.hits.len() - DOSSIER_PER_NAME_CAP, sr.defs_same_name
            ));
        }
    }
    if dropped_for_budget > 0 {
        out.push_str(&format!("… and {dropped_for_budget} reference points omitted for budget\n"));
    }
    out
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p redteam dossier::tests 2>&1 | tail -20`
Expected: 全 PASS。

- [ ] **Step 5: Commit**

```bash
git add src/dossier/mod.rs
git commit -m "feat(dossier): render_entries（两级帽 + cap/歧义/doc/预算四标注，D7）"
```

---

### Task 6: `build_dossier`——引擎公开入口（per-file 派发 + 无损降级）

**Files:**
- Modify: `src/dossier/mod.rs`

**Interfaces:**
- Consumes: `lang_for`, `compile_defs`, `symbols_in_file`, `reference_points`, `render_entries`；`crate::bundle::Bundle`
- Produces: `pub fn build_dossier(bundle: &crate::bundle::Bundle, root: &Path) -> String`（无符号/无引用/全非支持语言 → `""`）

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn build_dossier_end_to_end_and_graceful() {
    use crate::bundle::{Bundle, FileChange};
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
```

（若 `FileChange` 字段多于 `path`/`diff`，用 `..Default::default()` 或按其真实构造；实现者以 `src/bundle/mod.rs` 定义为准。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam dossier::tests::build_dossier 2>&1 | tail -20`
Expected: 编译失败 / 未定义。

- [ ] **Step 3: 写实现**（追加）

```rust
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
```

- [ ] **Step 4: 跑测试确认通过 + 全套回归**

Run: `cargo test -p redteam 2>&1 | tail -15`
Expected: 全 PASS（含既有 312+ 测试），零新警告。

- [ ] **Step 5: Commit**

```bash
git add src/dossier/mod.rs
git commit -m "feat(dossier): build_dossier 引擎入口（per-file 派发 + 无损降级，2-A 完）"
```

---

# 批次 2-B：注入接线（走 M13 {{INCREMENTAL_ROUND}} 同款穿线）

### Task 7: `render_dossier_section`——`<dossier>` 块 + D8 散文

**Files:**
- Modify: `src/prompt/mod.rs`

**Interfaces:**
- Produces: `pub fn render_dossier_section(entries: &str) -> String`（`entries` 空 → `""`；否则 `<dossier>` + D8 三句散文 + entries）

- [ ] **Step 1: 写失败测试**（`src/prompt/mod.rs` 的 `mod tests` 内）

```rust
#[test]
fn dossier_section_empty_renders_empty() {
    assert_eq!(render_dossier_section(""), "");
}

#[test]
fn dossier_section_wraps_with_prose_and_scope_guard() {
    let s = render_dossier_section("## foo (fn)\nsrc/a.rs:3\n    foo();\n");
    assert!(s.starts_with("<dossier>"));
    assert!(s.contains("blast radius"), "D8 措辞在场");
    assert!(s.contains("do not audit them for pre-existing defects"), "范围划线在场");
    assert!(s.contains("duty to verify is undiminished"));
    assert!(s.contains("## foo (fn)"), "entries 在场");
    assert!(s.trim_end().ends_with("</dossier>"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam prompt::tests::dossier_section 2>&1 | tail -20`
Expected: 未定义。

- [ ] **Step 3: 写实现**（`src/prompt/mod.rs`，照 `render_incremental_section` 形状）

```rust
/// 卷宗段（M14 D6/D8）：事实引用点 + 惰性保真散文。entries 空 → 空串（空渲染纪律）。
pub fn render_dossier_section(entries: &str) -> String {
    if entries.trim().is_empty() {
        return String::new();
    }
    format!(
        "<dossier>\n\
         The entries below are where this change's changed symbols are used elsewhere in the repo — its blast radius — found by word-boundary search (heuristic facts, not judgments; renamed or dynamic uses may be missing). Judge whether THIS change breaks these sites; do not audit them for pre-existing defects this change does not cause. The ±3-line excerpts are a starting point — read the full file at any site you must judge, and search yourself for uses you suspect are missing; your duty to verify is undiminished.\n\n\
         {entries}\
         </dossier>"
    )
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p redteam prompt::tests 2>&1 | tail -20`
Expected: PASS。

- [ ] **Step 5: Commit**

```bash
git add src/prompt/mod.rs
git commit -m "feat(dossier): render_dossier_section（<dossier> 块 + D8 惰性保真散文）"
```

---

### Task 8: 模板 `{{DOSSIER}}` + `<tools>` 和解 + `build_bundle_prompt` 加参

**Files:**
- Modify: `src/prompt/templates/adversarial-code-bundle.md`
- Modify: `src/prompt/mod.rs`（`build_bundle_prompt`）

**Interfaces:**
- Consumes: `render_dossier_section`（Task 7 已在渲染侧，本任务只在模板填 `{{DOSSIER}}`）
- Produces: `build_bundle_prompt(target_label, bundle, rules, user_focus, other_files, incremental, dossier: &str)`（末位加 `dossier`）

- [ ] **Step 1: 写失败测试**（`src/prompt/mod.rs` 的 `mod tests`）

```rust
#[test]
fn bundle_prompt_carries_dossier_and_tools_reconciliation() {
    let lens = crate::lens::builtin();
    let refs: Vec<&crate::lens::Lens> = lens.iter().collect();
    let b = crate::bundle::Bundle { files: vec![crate::bundle::FileChange {
        path: "src/a.rs".into(), diff: "@@ -1 +1 @@\n-a\n+b".into(),
    }] };
    let dossier = render_dossier_section("## foo (fn)\nsrc/a.rs:3\n    foo();\n");
    let p = build_bundle_prompt("wt", &b, &refs, "", "", "", &dossier);
    assert!(p.contains("<dossier>"), "卷宗块注入");
    assert!(p.contains("start from it and search only beyond what it lists"), "<tools> 和解句在场");
    // 空卷宗 → 无 <dossier> 块，行为不变
    let p2 = build_bundle_prompt("wt", &b, &refs, "", "", "", "");
    assert!(!p2.contains("<dossier>"), "空卷宗不渲染块");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam prompt::tests::bundle_prompt_carries_dossier 2>&1 | tail -20`
Expected: 签名不匹配 / 未含 `{{DOSSIER}}`。

- [ ] **Step 3: 改模板**（`src/prompt/templates/adversarial-code-bundle.md`）

第 17 行 `<tools>` 内那句，在「…callers, and callees yourself before judging.」后插入一句（空卷宗时 vacuously 成立）：
```
You have tools to inspect the repository: read_file (read any file under the review root, returned with line numbers), search_repo (regex search, gitignore-aware), and list_files. The diffs below show only what changed — read the surrounding files, callers, and callees yourself before judging. Where a dossier of reference points is provided below, start from it and search only beyond what it lists. Do not invent code you have not read.
```
在 `</bundle>`（第 70 行）之后、文件末尾插入卷宗块占位：
```

{{DOSSIER}}
```

- [ ] **Step 4: 改 `build_bundle_prompt`**（`src/prompt/mod.rs:142`）

签名末位加 `dossier: &str`；`render(...)` vars 末位加 `("{{DOSSIER}}", dossier)`：
```rust
pub fn build_bundle_prompt(
    target_label: &str,
    bundle: &Bundle,
    rules: &[&Lens],
    user_focus: &str,
    other_files: &str,
    incremental: &str,
    dossier: &str,
) -> String {
    // ... 现有 rules_str/diffs_block/file_list 不变 ...
    render(CODE_BUNDLE_TPL, &[
        // ... 现有 8 项不变 ...
        ("{{INCREMENTAL_ROUND}}", incremental),
        ("{{DOSSIER}}", dossier),
    ])
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p redteam prompt:: 2>&1 | tail -20`
Expected: PASS（本任务新测 + 既有 prompt 测；`build_bundle_prompt` 的其它调用者暂编译错，Task 9 修）。
说明：此步 `cargo test` 全库可能因调用点未更新而编译失败——只需确认 `prompt` 模块内新测逻辑正确；整库绿由 Task 9 达成。若要本步即绿，可先给 orchestrate 调用点传 `""` 占位（Task 9 会正式接线）。

- [ ] **Step 6: Commit**

```bash
git add src/prompt/mod.rs src/prompt/templates/adversarial-code-bundle.md
git commit -m "feat(dossier): 模板 {{DOSSIER}} 占位 + <tools> 和解 + build_bundle_prompt 加参"
```

---

### Task 9: `run_code_fanout` 收 per-bundle 卷宗段

**Files:**
- Modify: `src/orchestrate/mod.rs`

**Interfaces:**
- Consumes: `build_bundle_prompt`（Task 8，7 参）
- Produces: `run_code_fanout(provider, target_label, bundles, rules, focus, incremental, dossiers: &[String], opts, sink)`（`incremental` 后加 `dossiers`）

**设计注**：`dossiers` 与 `bundles` 平行索引；用 `.get(i)…unwrap_or("")` 容短切片（测试传 `&[]` 即全空卷宗）。

- [ ] **Step 1: 写失败测试**（`src/orchestrate/mod.rs` 的 `mod tests`，复用既有 `bundle`/probe 脚手架）

```rust
#[tokio::test]
async fn code_fanout_threads_per_bundle_dossier() {
    let p = ProbeProvider::default(); // 既有测试 provider，记录收到的 prompt
    let rules = builtin_refs();
    let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs")];
    let opts = OrchestrateOpts::default();
    let dossiers = vec![
        "<dossier>\nDOSSIER-A\n</dossier>".to_string(),
        "<dossier>\nDOSSIER-B\n</dossier>".to_string(),
    ];
    let _ = run_code_fanout(&p, "wt", &bundles, &rules, "", "", &dossiers, &opts, None).await;
    let prompts = p.seen_prompts();
    assert!(prompts.iter().any(|s| s.contains("DOSSIER-A")));
    assert!(prompts.iter().any(|s| s.contains("DOSSIER-B")));
}
```

（若既有 ProbeProvider 未暴露 `seen_prompts`，实现者按既有 probe 记录方式改；关键断言 = 各 bundle 收到各自卷宗段。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam orchestrate::tests::code_fanout_threads 2>&1 | tail -20`
Expected: 签名不匹配。

- [ ] **Step 3: 改实现**（`src/orchestrate/mod.rs:185` 签名 + 内部 map）

签名 `incremental: &str,` 后加 `dossiers: &[String],`；per-bundle map（第 200 行 `.map(|(i, bundle)|`）内 `build_bundle_prompt` 调用加末位参：
```rust
let dossier = dossiers.get(i).map(String::as_str).unwrap_or("");
let prompt = build_bundle_prompt(target_label, bundle, rules, focus, &others, incremental, dossier);
```

- [ ] **Step 4: 修既有调用点 + 跑测试**

`src/orchestrate/mod.rs` 内既有测试的 4 处 `run_code_fanout(...)` 调用（约 482/514/529/559 行）在 `""`（incremental）后各加 `&[]`。

Run: `cargo test -p redteam orchestrate:: 2>&1 | tail -20`
Expected: PASS（含新测）。

- [ ] **Step 5: Commit**

```bash
git add src/orchestrate/mod.rs
git commit -m "feat(dossier): run_code_fanout 收 per-bundle 卷宗段（dossiers: &[String]）"
```

---

### Task 10: cli.rs 两处 fanout 接线 + `REDTEAM_NO_DOSSIER` A/B 开关

**Files:**
- Modify: `src/cli.rs`（`run_review` ~460 行；`run_incremental_review` ~585 行）

**Interfaces:**
- Consumes: `crate::dossier::build_dossier`, `crate::prompt::render_dossier_section`, `run_code_fanout`（Task 9，9 参）
- Produces: 无新公开 API；`REDTEAM_NO_DOSSIER` 环境变量非空 → 全空卷宗（A/B off 臂，D9）

- [ ] **Step 1: 写失败测试**（`src/cli.rs` 的 `mod tests`，或一个薄的 helper 单测）

抽一个可测 helper（避免整条 review 管道起 provider）：
```rust
// src/cli.rs 内（非 #[cfg(test)]）
pub(crate) fn build_dossiers(bundles: &[crate::bundle::Bundle], root: &std::path::Path) -> Vec<String> {
    if std::env::var_os("REDTEAM_NO_DOSSIER").is_some() {
        return vec![String::new(); bundles.len()]; // A/B off 臂：全空
    }
    bundles.iter()
        .map(|b| crate::prompt::render_dossier_section(&crate::dossier::build_dossier(b, root)))
        .collect()
}
```
测试：
```rust
#[test]
fn build_dossiers_toggle_off_arm_all_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let bundles = vec![/* 一个含 Rust 改动的 bundle，同 Task 6 */];
    // on 臂：至少一段非空（前提是该 bundle 有引用点）
    std::env::remove_var("REDTEAM_NO_DOSSIER");
    // off 臂：全空
    std::env::set_var("REDTEAM_NO_DOSSIER", "1");
    let off = build_dossiers(&bundles, tmp.path());
    assert!(off.iter().all(|s| s.is_empty()), "off 臂全空");
    std::env::remove_var("REDTEAM_NO_DOSSIER");
}
```
（env 测试需串行化——照 `prompt/mod.rs` 既有 `ENV_LOCK` Mutex 模式加锁，避免与其它 env 测并发。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam cli::tests::build_dossiers_toggle 2>&1 | tail -20`
Expected: 未定义。

- [ ] **Step 3: 写 helper + 两处接线**

在 `run_review`（line 460）`run_code_fanout` 调用前：
```rust
let dossiers = build_dossiers(&bundles, root);
let mut raw = run_code_fanout(provider, &target_label, &bundles, &rules, &focus, "", &dossiers, &oopts, sink).await;
```
在 `run_incremental_review`（line 585）`run_code_fanout` 调用前：
```rust
let dossiers = build_dossiers(&bundles, root);
let raw = run_code_fanout(provider, &target_label, &bundles, &rules, &focus, &inc_section, &dossiers, &oopts, sink).await;
```

- [ ] **Step 4: 跑全套 + verify**

Run: `cargo test -p redteam 2>&1 | tail -15 && make verify 2>&1 | tail -5`
Expected: 全 PASS；`make verify` 绿（commands↔codex-skills 无漂移——本批未动 commands/*.md，应无 gen diff）。

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs
git commit -m "feat(dossier): cli 两处 fanout 接线 + REDTEAM_NO_DOSSIER A/B 开关（2-B 完）"
```

---

# 批次 2-C：自带 A/B 的 live（验收，非 TDD 代码——维护者执行）

**不是 subagent 可实施的代码任务**，是 M13 live + M14 验收合并跑的实测程序（D9 双重身份）。执行者=维护者，泳道纪律见设计文档。

**程序：**
1. 选真实变更集（M14 各批实施本身即素材；或 dogfood 目标仓的一个分支）。
2. **off 臂**（= M13 live 基线 + ≥50% input 降验收）：`REDTEAM_NO_DOSSIER=1 /redteam:ship --base dev`，记 `result.json`（job A）。
3. **on 臂**：`/redteam:ship --base dev`（同变更集、同模型三件套），记 `result.json`（job B）。
4. **对比三门**（读两份 result.json 现成字段，零新仪器）：
   - **成本**：`status <job>` 的 input_tokens/耗时/per-cell 轮数（支柱 5 `iterations` 字段）——on 臂应进一步降（轮数 12→3-6）。不达标 → **调参**（`DOSSIER_PER_NAME_CAP`/`DOSSIER_MAX_BYTES`/散文），不 revert。
   - **四判据**：真 finding 不丢（on 的 findings ⊇ off 的真 finding）/ 误报不增 / id 稳定 / reappeared 零。回退 = **硬门**，改 D8 措辞或引擎内容后重跑。
   - **独立性**：两 job 的 `agreement` 分布不显著收窄（on 不向共识锐化）。硬门。
5. **达标** → 卷宗默认开启（去掉 off 臂开关的临时性，`REDTEAM_NO_DOSSIER` 保留作诊断），合 dev；同时 M13 live ≥50% 达标一并收编。

**注**：若某语言（Go/TS）live 表现失望，才复跑 `eval/` harness 换 scip-go/scip-typescript oracle 定位（D4a：oracle 是诊断非门）。

---

## Self-Review

**1. Spec 覆盖（D1-D9）：**
- D1 分段读剥离 → 不在本 plan（已 TODO），✓。
- D2 v1 仅引用点（无 hunk 完整块/文件大纲）→ 引擎只产引用点（Task 4/5/6），✓。
- D3 每(轮,bundle)重建 + 吃 M13 delta → `build_dossier` 吃 bundle（增量轮 bundle 即 fix-delta，Task 10 两处接线分别在全量/增量管道），✓；blast-radius 措辞 D8（Task 7），✓。
- D4 三语言 + 无损降级 → LangSpec 表（Task 1）+ `lang_for` None 降级（Task 6），✓。
- D4a TS 洞补全 → TS method 正则（Task 1），✓；oracle 诊断非门 → 2-C 注，✓。
- D5 数据表无 trait 无后端槽位 → `const RUST/GO/TS`（Task 1），✓。
- D6 注入抄 M13 + 空渲染 + per-bundle + 去重 → Task 7/8/9/10，✓；静态注入（prompt 内非 tool 拉取）✓。
- D7 两级帽 + 字节估算 + 四标注 + 不预抽泛型 → Task 5，✓。
- D8 惰性散文 + `<tools>` 和解 + 重排延后 → Task 7/8 + TODO，✓。
- D9 A/B 读现成 agreement + 双重身份 + 三门 → 2-C，✓。

**2. Placeholder 扫描：** 各 code step 均有完整 Rust；无「TODO/后续填」。两处「实现者以真实定义为准」（FileChange 字段 Task 6、ProbeProvider 记录方式 Task 9）是既有代码适配点、非逻辑占位，已给兜底措辞。✓

**3. 类型一致性：** `ChangedSym`（Task 3）→ `reference_points`（Task 4 消费）→ `SymRefs`（Task 4 产）→ `render_entries`（Task 5 消费）→ `build_dossier`（Task 6 串）；`render_dossier_section`（Task 7）→ `build_bundle_prompt` 加参（Task 8）→ `run_code_fanout` 加参（Task 9）→ cli 接线（Task 10）。签名逐链对齐。✓

**已知简化（ponytail，已在代码注释标注上升路径）：**
- `innermost_enclosing_def` 每候选行调 `block_end` = O(n²) 最坏（单文件几千行够用）。
- `reference_points` per-name 全仓 walk（增量轮符号数少；N 大再改单次 alternation walk）。
- TS method 正则宽松 + 关键字后置滤（无 lookahead）；over-match 由 cap 兜。
- 混合语言 bundle 的歧义度 defs 用首个支持语言的 defs（混合 bundle 极少）。
