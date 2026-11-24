# M15-S1「regex 地板 pack」Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 M14 卷宗演进成「预算治理的 pack」——每 bundle 的 `{{DOSSIER}}` 槽从「仅引用点」升级为「改动符号完整体(0壳) + 引用点(1壳) + 精确省略清单」，直打残余 tool_use，且保留 A/B 基线臂。

**Architecture:** 最省落法——**不引入 `Cell` struct**。现有 `build_dossiers(bundles)→Vec<String>→{{DOSSIER}}` 平行数组已是「每 bundle 一段上下文扇多模型」的 cell 机制；S1 只把该槽的**字符串内容**从 `build_dossier`(引用点)换成新 `pack::build_pack`(3 段)。`run_code_fanout`/`build_bundle_prompt` 签名一行不改。复用 `src/dossier` 全部符号/引用引擎，不重写。

**Tech Stack:** Rust 单 crate；`grep_regex`/`grep_searcher`/`ignore`/`regex`（dossier 已用）；测试 `cargo test` + `tempfile`。

## Global Constraints

- **权威设计**：`.doc/design/2026-07-07-redteam-context-management-s1-design.md`（读它；6 决策 + §6 差异账 + §5 退出门）。上位架构 `.doc/design/2026-07-06-redteam-context-management-design.md`。
- **复用不重写**：`src/dossier/mod.rs` 的 `symbols_in_file / innermost_enclosing_def / block_end / match_def / compile_defs / lang_for / reference_points / RefHit / SymRefs / parse_hunk_new_start` 全部复用；引擎逻辑零改（仅加 `pub(crate)` 可见性 + 新增函数）。
- **`{{DOSSIER}}` 槽 + `<dossier>` 包裹标签维持不变**（模型可见接口稳定，省 churn）；diff 恒留 `{{BUNDLE_DIFFS}}` 原位、永不被预算砍。
- **无 `Cell` struct、不改 `run_code_fanout`/`build_bundle_prompt` 签名**（缓到 S2）。
- **预算 = flat 常量 `PACK_BUDGET_BYTES`**（provisional，S0 校准；跨模型同值）。
- **确定性输出**：一切排序稳定（`reference_points` 已按 (path,line) 排序；范围/坐标按行升序）。
- **路径安全**：工作树读走 `root.join(rel)` 直读——bundle 路径来自 git diff（仓内相对、git 跟踪、非模型可控），沿用 `bundle::group` 同款安全不变量（读失败即降级跳过，绝不 panic）。
- **注释用简体中文**，密度/风格随 `src/dossier/mod.rs`。
- **绿线**：每 task 末 `cargo test`（相关模块）绿；全 plan 收尾 `cargo test` 全绿 + `cargo clippy -- -D warnings` 无新警。

---

### Task 1: dossier 暴露内部件 + 0 壳 def 范围抽取

**Files:**
- Modify: `src/dossier/mod.rs`（加 `pub(crate)` 可见性；新增 `enclosing_def_start` / `hunk_anchors` / `changed_def_ranges`；`innermost_enclosing_def` DRY 化）

**Interfaces:**
- Produces（供 Task 2/3 复用）：
  - `pub(crate) fn changed_def_ranges(diff: &str, file_lines: &[&str], defs: &[(&'static str, Regex)]) -> Vec<(usize, usize)>`（每改动 hunk 的外围 def 完整体 0-based inclusive 行范围，按 def 起行去重、升序）
  - `pub(crate) fn compile_defs(lang: &Lang) -> Vec<(&'static str, Regex)>`、`pub(crate) fn lang_for(path: &str) -> Option<&'static Lang>`、`pub(crate) struct Lang`（Task 2 编 defs 用）
  - `pub(crate)` 化：`reference_points`、`SymRefs`、`RefHit`、`symbols_in_file`、`ChangedSym`（已 pub 的保持）

- [ ] **Step 1: 写失败测试**（追加到 `src/dossier/mod.rs` 的 `mod tests`）

```rust
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
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam changed_def_ranges 2>&1 | tail -20`
Expected: 编译失败（`changed_def_ranges` 未定义）

- [ ] **Step 3: 加可见性 + 三个新函数 + DRY 化**

在 `src/dossier/mod.rs`：把 `struct Lang`、`fn compile_defs`、`fn lang_for`、`fn reference_points`、`struct SymRefs`、`struct RefHit`、`fn symbols_in_file` 的 `fn`/`struct` 前缀改为 `pub(crate) fn`/`pub(crate) struct`（已 `pub` 者不动）。

`innermost_enclosing_def` 改为委托新 `enclosing_def_start`（同行为、DRY）：

```rust
/// line0（0-based）最内层外围 def 的**起行索引**：向上首个「块覆盖 line0」的 def 行。
pub(crate) fn enclosing_def_start(lines: &[&str], line0: usize, defs: &[(&'static str, Regex)]) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    for i in (0..=line0.min(lines.len().saturating_sub(1))).rev() {
        if match_def(lines[i], defs).is_some() && (i == line0 || block_end(lines, i) >= line0) {
            return Some(i);
        }
    }
    None
}

fn innermost_enclosing_def(lines: &[&str], line0: usize, defs: &[(&'static str, Regex)]) -> Option<(&'static str, String)> {
    enclosing_def_start(lines, line0, defs).and_then(|i| match_def(lines[i], defs))
}
```

新增 hunk 锚点抽取（镜像 `symbols_in_file` 的 hunk 游走，只收锚点）：

```rust
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
```

- [ ] **Step 4: 跑测试确认通过 + 未回归**

Run: `cargo test -p redteam dossier 2>&1 | tail -20`
Expected: 新增 2 测试 PASS，dossier 既有 23 测试全绿（DRY 化零行为变化）

- [ ] **Step 5: 提交**

```bash
git add src/dossier/mod.rs
git commit -m "feat(pack): dossier 暴露内部件 + changed_def_ranges（0 壳 def 范围）

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 2: pack 模块 + 0 壳「改动符号完整体」段

**Files:**
- Create: `src/pack/mod.rs`
- Modify: `src/lib.rs`（加 `pub mod pack;`）

**Interfaces:**
- Consumes: `dossier::{changed_def_ranges, compile_defs, lang_for}`；`bundle::Bundle`；`evidence::git::FileChange`
- Produces: `pub(crate) fn body_section(bundle: &Bundle, root: &Path, budget: usize, omit: &mut Vec<String>) -> String`（0 壳段：改动符号完整体，按 `budget` 字节封顶；放不下的 body → `omit` 追加 `path:start-end` 坐标；无改动符号 → ""）

- [ ] **Step 1: 注册模块**

在 `src/lib.rs` 现有 `pub mod dossier;` 邻近加：

```rust
pub mod pack;
```

- [ ] **Step 2: 写失败测试**（`src/pack/mod.rs` 的 `mod tests`）

```rust
#[test]
fn body_section_renders_full_enclosing_body() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"),
        "fn helper() {}\n\nfn caller() {\n    helper();\n    let z = 2;\n}\n").unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "src/a.rs".into(),
        diff: "@@ -4,0 +5,1 @@\n+    let z = 2;\n".into(),
    }] };
    let mut omit = Vec::new();
    let s = body_section(&b, root, 1_000_000, &mut omit);
    assert!(s.contains("## 改动符号完整体"), "含 0 壳标题: {s}");
    assert!(s.contains("fn caller() {") && s.contains("let z = 2;"), "含 caller 完整体");
    assert!(!s.contains("fn helper"), "未改动的 helper 不进 0 壳");
    assert!(omit.is_empty(), "预算充足无省略");
}

#[test]
fn body_section_budget_cut_body_goes_to_omit() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn caller() {\n    work();\n}\n").unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "src/a.rs".into(), diff: "@@ -2,0 +2,1 @@\n+    work();\n".into(),
    }] };
    // 预算极小 → body 放不下 → 进 omit 坐标、正文空
    let mut omit = Vec::new();
    let s = body_section(&b, root, 5, &mut omit);
    assert_eq!(s, "", "预算耗尽正文空");
    assert_eq!(omit.len(), 1, "body 降级为坐标");
    assert!(omit[0].contains("src/a.rs:"), "带坐标: {omit:?}");
}

#[test]
fn body_section_empty_when_no_supported_lang() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "app.py".into(), diff: "@@ -1,0 +1,1 @@\n+def f(): pass\n".into(),
    }] };
    let mut omit = Vec::new();
    assert_eq!(body_section(&b, tmp.path(), 1_000_000, &mut omit), "", "非支持语言 → 空");
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p redteam body_section 2>&1 | tail -20`
Expected: 编译失败（`body_section` 未定义）

- [ ] **Step 4: 写实现**（`src/pack/mod.rs` 顶部）

```rust
//! pack 引擎（M15-S1）：每 bundle 的预算治理上下文块——0 壳改动符号完整体 + 1 壳引用点 + 精确省略清单。
//! hermetic：喂 (bundle, root, budget) 出字符串，零 LLM。复用 src/dossier 的符号/引用引擎。

use std::path::Path;

use crate::bundle::Bundle;
use crate::dossier::{changed_def_ranges, compile_defs, lang_for};

/// 0 壳段：改动 hunk 的外围 def 完整体（工作树侧全文），按 budget 字节封顶。
/// 最近壳、优先级最高故先填；但极压下放不下的 body → omit 追加坐标（design §3/§4 兜底）。无改动符号 → ""。
pub(crate) fn body_section(bundle: &Bundle, root: &Path, budget: usize, omit: &mut Vec<String>) -> String {
    let mut out = String::new();
    for fc in &bundle.files {
        let lang = match lang_for(&fc.path) {
            Some(l) => l,
            None => continue, // 非支持语言：无损降级
        };
        let defs = compile_defs(lang);
        // 工作树直读（bundle 路径来自 git diff、非模型可控，沿用 bundle::group 安全不变量）
        let content = std::fs::read_to_string(root.join(&fc.path)).unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        for (start, end) in changed_def_ranges(&fc.diff, &lines, &defs) {
            let end = end.min(lines.len().saturating_sub(1));
            let body = lines.get(start..=end).unwrap_or(&[]).join("\n");
            if body.is_empty() {
                continue;
            }
            let block = format!("{}:{}-{}\n{}\n\n", fc.path, start + 1, end + 1, body);
            if out.len() + block.len() > budget {
                omit.push(format!("{}:{}-{} — 改动符号体", fc.path, start + 1, end + 1));
            } else {
                out.push_str(&block);
            }
        }
    }
    if out.is_empty() {
        return String::new();
    }
    format!("## 改动符号完整体（0 壳）\n{out}")
}

#[cfg(test)]
mod tests {
    use super::*;
    // 测试见 Step 2
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p redteam pack 2>&1 | tail -20`
Expected: 2 测试 PASS

- [ ] **Step 6: 提交**

```bash
git add src/lib.rs src/pack/mod.rs
git commit -m "feat(pack): 0 壳改动符号完整体段（body_section）

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 3: 1 壳引用点段 + 坐标省略（预算治理）

**Files:**
- Modify: `src/pack/mod.rs`

**Interfaces:**
- Consumes: `dossier::{reference_points, SymRefs}`；`SymRefs { name, kind, defs_same_name, hits: Vec<RefHit> }`、`RefHit { path, line, context, is_doc }`
- Produces:
  - `pub(crate) const PACK_PER_NAME_CAP: usize = 10;`
  - `pub(crate) const OMIT_MAX_ENTRIES: usize = 200;`
  - `fn refs_section(refs: &[SymRefs], budget: usize, omit: &mut Vec<String>) -> String`（1 壳段：每符号 ≤cap 引用点入正文、正文按 `budget` 字节封顶；cap 溢出 + 预算砍掉的引用点 → `omit` 追加 `path:line` 坐标）

- [ ] **Step 1: 写失败测试**（`src/pack/mod.rs` 的 `mod tests`）

```rust
fn sr(name: &str, n: usize) -> crate::dossier::SymRefs {
    let hits = (0..n).map(|i| crate::dossier::RefHit {
        path: format!("src/f{i}.rs"), line: i + 1,
        context: format!("    {name}(); // hit {i}"), is_doc: false,
    }).collect();
    crate::dossier::SymRefs { name: name.into(), kind: "fn", defs_same_name: 1, hits }
}

#[test]
fn refs_section_caps_per_name_and_omits_overflow_as_coords() {
    let mut omit = Vec::new();
    // 25 引用点，per-name 帽 10 → 正文 10 个，溢出 15 个进 omit 坐标
    let s = refs_section(&[sr("run", 25)], 1_000_000, &mut omit);
    assert!(s.contains("## 引用点（1 壳）"), "含 1 壳标题");
    assert_eq!(s.matches("src/f").count(), PACK_PER_NAME_CAP, "正文每名 ≤10");
    assert_eq!(omit.len(), 15, "cap 溢出 15 个进省略坐标");
    assert!(omit.iter().all(|o| o.contains(":")), "坐标形如 path:line: {omit:?}");
}

#[test]
fn refs_section_budget_cut_goes_to_omit() {
    let mut omit = Vec::new();
    // 极小预算：正文放不下 → 该符号引用点全进 omit
    let s = refs_section(&[sr("run", 5)], 10, &mut omit);
    assert!(!s.contains("src/f"), "预算耗尽正文无引用点: {s}");
    assert_eq!(omit.len(), 5, "预算砍掉的 5 个引用点全进省略坐标");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam refs_section 2>&1 | tail -20`
Expected: 编译失败（`refs_section` 未定义）

- [ ] **Step 3: 写实现**（`src/pack/mod.rs`）

```rust
use crate::dossier::SymRefs;

/// 每符号正文引用点上限（相关性/多样性帽，非预算帽；沿用 dossier 惯例 10）。
pub(crate) const PACK_PER_NAME_CAP: usize = 10;
/// 省略清单条目上限（防大 fan-in 撑爆；超出末尾计数兜底）。
pub(crate) const OMIT_MAX_ENTRIES: usize = 200;

/// 1 壳段：每符号 ≤cap 引用点入正文、正文按 budget 字节封顶。
/// cap 溢出（第 11+）与预算砍掉的引用点 → omit 追加 `path:line` 坐标。空 → ""。
fn refs_section(refs: &[SymRefs], budget: usize, omit: &mut Vec<String>) -> String {
    let mut body = String::new();
    for sr in refs {
        if sr.hits.is_empty() {
            continue;
        }
        let ambig = if sr.defs_same_name > 1 {
            format!(" [{} defs share this name, heuristic]", sr.defs_same_name)
        } else {
            String::new()
        };
        // 组本符号整块（header + ≤cap 引用点），整体判预算——原子进出，杜绝悬空 header。
        let shown = sr.hits.len().min(PACK_PER_NAME_CAP);
        let mut block = format!("### {} ({}){}\n", sr.name, sr.kind, ambig);
        for h in &sr.hits[..shown] {
            let doc = if h.is_doc { "[doc] " } else { "" };
            block.push_str(&format!("{doc}{}:{}\n{}\n\n", h.path, h.line, h.context));
        }
        // cap 溢出的引用点（第 11+）→ 坐标进省略
        for h in &sr.hits[shown..] {
            omit.push(format!("{}:{} — {} 引用点", h.path, h.line, sr.name));
        }
        if body.len() + block.len() > budget {
            // 预算耗尽：本符号正文进不去 → 其 shown 引用点也降级为坐标
            for h in &sr.hits[..shown] {
                omit.push(format!("{}:{} — {} 引用点", h.path, h.line, sr.name));
            }
            continue;
        }
        body.push_str(&block);
    }
    if body.is_empty() {
        return String::new();
    }
    format!("## 引用点（1 壳）\n{body}")
}
```

> 注：`refs` 由 `build_pack`（Task 4）经 `reference_points` 取得——本段只管渲染 + 记账。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p redteam refs_section 2>&1 | tail -20`
Expected: 2 测试 PASS

- [ ] **Step 5: 提交**

```bash
git add src/pack/mod.rs
git commit -m "feat(pack): 1 壳引用点段 + 坐标省略（预算治理）

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 4: `build_pack` 组装（近→远填满预算 + 省略清单封顶）

**Files:**
- Modify: `src/pack/mod.rs`

**Interfaces:**
- Consumes: `body_section`(Task 2)、`refs_section`(Task 3)、`dossier::{symbols_in_file, reference_points, compile_defs, lang_for, ChangedSym}`
- Produces: `pub fn build_pack(bundle: &Bundle, root: &Path, budget: usize) -> String`（一个 bundle 的 3 段 pack 正文，不含 `<dossier>` 包裹；空 → ""）

- [ ] **Step 1: 写失败测试**（`src/pack/mod.rs` 的 `mod tests`）

```rust
fn bundle_one(path: &str, diff: &str) -> crate::bundle::Bundle {
    crate::bundle::Bundle { files: vec![crate::evidence::git::FileChange {
        path: path.into(), diff: diff.into(),
    }] }
}

#[test]
fn build_pack_end_to_end_has_bodies_refs_and_diff_reserved() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"),
        "pub fn target() {\n    work();\n}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\n").unwrap();
    let b = bundle_one("src/a.rs", "@@ -1,0 +1,3 @@\n+pub fn target() {\n+    work();\n+}\n");
    let pack = build_pack(&b, root, 1_000_000);
    assert!(pack.contains("## 改动符号完整体（0 壳）"), "有 0 壳: {pack}");
    assert!(pack.contains("pub fn target() {"), "0 壳含 target 完整体");
    assert!(pack.contains("## 引用点（1 壳）"), "有 1 壳");
    assert!(pack.contains("u1()"), "1 壳含 caller");
}

#[test]
fn build_pack_tiny_budget_spills_to_omission_list_with_coords() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"),
        "pub fn target() {}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\n").unwrap();
    let b = bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn target() {}\n");
    // 预算极小：引用点填不下 → 省略清单带坐标 + 用法提示
    let pack = build_pack(&b, root, 60);
    assert!(pack.contains("## 已省略（可 read_files 定点读）"), "有省略段: {pack}");
    assert!(pack.contains("read_files({paths:"), "省略段首用法提示");
    assert!(pack.contains("src/a.rs:"), "省略项带 file:line 坐标");
}

#[test]
fn build_pack_empty_for_unsupported_and_no_symbols() {
    let tmp = tempfile::tempdir().unwrap();
    let b = bundle_one("app.py", "@@ -1,0 +1,1 @@\n+def f(): pass\n");
    assert_eq!(build_pack(&b, tmp.path(), 1_000_000), "", "非支持语言 → 空 pack");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam build_pack 2>&1 | tail -20`
Expected: 编译失败（`build_pack` 未定义）

- [ ] **Step 3: 写实现**（`src/pack/mod.rs`）

先补 use（Task 2/3 已引入 `changed_def_ranges, compile_defs, lang_for, SymRefs`；本 Task 再加）：

```rust
use crate::dossier::{symbols_in_file, reference_points, ChangedSym};
```

```rust
/// 引擎入口：为一个 bundle 产 3 段 pack 正文（0 壳 + 1 壳 + 省略清单），预算近→远填。
/// 不含 `<dossier>` 包裹（那是 prompt 层的活）。空 = 无 pack（降级）。
pub fn build_pack(bundle: &Bundle, root: &Path, budget: usize) -> String {
    // diff 字节（mandate，永不砍，但占预算记账）
    let diff_bytes: usize = bundle.files.iter().map(|fc| fc.diff.len()).sum();
    let remaining = budget.saturating_sub(diff_bytes);

    let mut omit: Vec<String> = Vec::new();

    // 0 壳：改动符号完整体（最近壳，先填；overflow → omit）
    let bodies = body_section(bundle, root, remaining, &mut omit);
    let after_bodies = remaining.saturating_sub(bodies.len());

    // 1 壳：引用点（用剩余预算）。改动符号 + 引用点走 dossier 引擎。
    let mut all_syms: Vec<ChangedSym> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for fc in &bundle.files {
        let lang = match lang_for(&fc.path) {
            Some(l) => l,
            None => continue,
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
    let defs = bundle.files.iter().find_map(|fc| lang_for(&fc.path)).map(compile_defs).unwrap_or_default();
    let refs = reference_points(&all_syms, root, &defs);
    let refs = refs_section(&refs, after_bodies, &mut omit);

    // 组装：非空段拼接 + 省略清单（封顶 + 计数兜底）
    let mut sections: Vec<String> = Vec::new();
    if !bodies.is_empty() {
        sections.push(bodies);
    }
    if !refs.is_empty() {
        sections.push(refs);
    }
    if !omit.is_empty() {
        let total = omit.len();
        omit.truncate(OMIT_MAX_ENTRIES);
        let mut sec = String::from("## 已省略（可 read_files 定点读）\nread_files({paths:[{path,start,end}]})\n");
        for line in &omit {
            sec.push_str(line);
            sec.push('\n');
        }
        if total > OMIT_MAX_ENTRIES {
            sec.push_str(&format!("… +{} more omitted\n", total - OMIT_MAX_ENTRIES));
        }
        sections.push(sec);
    }
    sections.join("\n")
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p redteam pack 2>&1 | tail -20`
Expected: Task 2/3/4 全部 pack 测试 PASS

- [ ] **Step 5: 提交**

```bash
git add src/pack/mod.rs
git commit -m "feat(pack): build_pack 组装（近→远填预算 + 省略清单封顶）

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 5: prompt 层加宽 `<dossier>` 散文覆盖 0 壳体

**Files:**
- Modify: `src/prompt/mod.rs`（`render_dossier_section` 散文加宽；包裹标签/机制不变）

**Interfaces:**
- Consumes/Produces: `render_dossier_section(entries: &str) -> String` 签名不变（entries 现可含 0 壳 + 1 壳 + 省略清单）

- [ ] **Step 1: 写失败测试**（`src/prompt/mod.rs` 的 `mod tests`）

```rust
#[test]
fn dossier_section_prose_mentions_changed_bodies_and_omission() {
    let s = render_dossier_section("## 改动符号完整体（0 壳）\nsrc/a.rs:1-3\nfn t() {}\n");
    assert!(s.starts_with("<dossier>"));
    assert!(s.contains("full body") || s.contains("changed symbol"), "散文提及改动符号完整体");
    assert!(s.contains("read_files"), "散文提及省略项可 read_files");
    assert!(s.trim_end().ends_with("</dossier>"));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam dossier_section_prose_mentions 2>&1 | tail -20`
Expected: FAIL（现散文只讲引用点、无 read_files/changed body 措辞）

- [ ] **Step 3: 加宽散文**（`src/prompt/mod.rs` `render_dossier_section` 的 `format!` 散文段，替换首句解说）

```rust
    format!(
        "<dossier>\n\
         The sections below are context for this change, assembled mechanically (heuristic facts, not judgments): the full current body of each CHANGED symbol (0-shell), where those symbols are used elsewhere in the repo — its blast radius — found by word-boundary search (1-shell), and an omission list of file:line coordinates that overflowed the budget. Judge whether THIS change breaks these sites; do not audit them for pre-existing defects this change does not cause. Excerpts are a starting point — read the full file at any site you must judge, and for any omitted coordinate use read_files({{paths:[{{path,start,end}}]}}); your duty to verify is undiminished.\n\n\
         {entries}\
         </dossier>"
    )
```

> 保留：空 entries → 空串（`entries.trim().is_empty()` 分支不动）；`</dossier>` 字面中和不动。注意 `format!` 内 `{{`/`}}` 转义花括号。

- [ ] **Step 4: 跑测试确认通过 + 未回归**

Run: `cargo test -p redteam prompt 2>&1 | tail -20`
Expected: 新测试 PASS；`dossier_section_*` 既有测试（空渲染/包裹/中和）全绿

- [ ] **Step 5: 提交**

```bash
git add src/prompt/mod.rs
git commit -m "feat(pack): <dossier> 散文加宽覆盖 0 壳体 + 省略项 read_files 指引

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 6: cli 接缝 `build_dossiers`→`build_packs` + 预算常量 + A/B 基线开关

**Files:**
- Modify: `src/cli.rs`（`build_dossiers` 改 `build_packs`；加 `PACK_BUDGET_BYTES` 常量 + `REDTEAM_PACK=0` 基线臂；改两处调用点 494/621 与 meta 字段）

**Interfaces:**
- Consumes: `pack::build_pack`；`prompt::render_dossier_section`；`dossier::build_dossier`（基线臂复用）
- Produces: `pub(crate) fn build_packs(bundles: &[Bundle], root: &Path) -> (Vec<String>, DossierMeta)`（默认产 pack；`REDTEAM_PACK=0` → 退回 dossier-only 基线；`REDTEAM_NO_DOSSIER` → 全空）；`run_code_fanout(...)` 调用点 `dossiers` 变量改名 `packs`（值语义不变，仍 `&[String]`）

- [ ] **Step 1: 写失败测试**（`src/cli.rs` 的 `mod tests`，仿现有 `build_dossiers_toggle_off_arm...`）

```rust
#[test]
fn build_packs_default_has_bodies_off_arm_dossier_only_null_arm_empty() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"),
        "pub fn target() {}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\nfn u4() { target(); }\nfn u5() { target(); }\n").unwrap();
    let bundles = vec![Bundle { files: vec![FileChange {
        path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+pub fn target() {}\n".into(),
    }] }];

    // 默认臂：pack（含 0 壳完整体）
    unsafe { std::env::remove_var("REDTEAM_NO_DOSSIER"); std::env::remove_var("REDTEAM_PACK"); }
    let (on, _) = build_packs(&bundles, root);
    assert!(on[0].contains("改动符号完整体"), "默认臂 = pack 含 0 壳: {}", on[0]);

    // 基线臂：REDTEAM_PACK=0 → dossier-only（无 0 壳段、仍有引用点）
    unsafe { std::env::set_var("REDTEAM_PACK", "0"); }
    let (base, _) = build_packs(&bundles, root);
    assert!(!base[0].contains("改动符号完整体"), "基线臂无 0 壳");
    assert!(base[0].contains("target"), "基线臂仍是 dossier 引用点");

    // 空臂：REDTEAM_NO_DOSSIER
    unsafe { std::env::set_var("REDTEAM_NO_DOSSIER", "1"); }
    let (null, _) = build_packs(&bundles, root);
    assert_eq!(null[0], "", "空臂全空段");
    unsafe { std::env::remove_var("REDTEAM_NO_DOSSIER"); std::env::remove_var("REDTEAM_PACK"); }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p redteam build_packs 2>&1 | tail -20`
Expected: 编译失败（`build_packs` 未定义）

- [ ] **Step 3: 写实现**（`src/cli.rs`）

在文件顶部常量区加（provisional 预算）：

```rust
/// pack flat 预算（字节，provisional）——S0 校准前初值；跨模型同值（design §2/§5）。
/// ponytail: ~2× 旧 28KB dossier 帽以容 0 壳完整体；S0 双基线跑完换真值。
const PACK_BUDGET_BYTES: usize = 64_000;
```

把 `build_dossiers`（line 327 起）改为 `build_packs`（保持返回 `(Vec<String>, DossierMeta)` 与既有 meta 契约）：

```rust
/// 每 bundle 产 pack 段（M15-S1）。三臂：默认=pack（0 壳+1 壳+省略）；`REDTEAM_PACK=0`=
/// dossier-only 基线臂（A/B 对照）；`REDTEAM_NO_DOSSIER`=全空段（null 臂）。degrade 静默。
pub(crate) fn build_packs(bundles: &[crate::bundle::Bundle], root: &std::path::Path) -> (Vec<String>, DossierMeta) {
    let suppressed = std::env::var_os("REDTEAM_NO_DOSSIER").is_some();
    let baseline = std::env::var_os("REDTEAM_PACK").map(|v| v == "0").unwrap_or(false);
    let entries: Vec<String> = bundles
        .iter()
        .map(|b| {
            if suppressed {
                String::new()
            } else if baseline {
                crate::prompt::render_dossier_section(&crate::dossier::build_dossier(b, root))
            } else {
                crate::prompt::render_dossier_section(&crate::pack::build_pack(b, root, PACK_BUDGET_BYTES))
            }
        })
        .collect();
    let injected = entries.iter().filter(|s| !s.is_empty()).count();
    let meta = DossierMeta { bundles: bundles.len(), injected, suppressed };
    (entries, meta)
}
```

> `DossierMeta` 字段沿用（`bundles`/`injected`/`suppressed`）——观测契约不变。若 `DossierMeta` 构造字段名不同，照现有 `build_dossiers` 内构造原样复制。

改两处调用点（原 line 494、621）：`build_dossiers` → `build_packs`，局部变量 `dossiers`/`dossier_meta` 可保留名（值语义不变），最省改法只换函数名：

```rust
    let (dossiers, dossier_meta) = build_packs(&bundles, root);
```

（`run_code_fanout(..., &dossiers, ...)` 与 meta stamp 逻辑一行不改。）

- [ ] **Step 4: 改既有测试名 + 跑全测**

原 `build_dossiers_toggle_off_arm_all_empty_on_arm_nonempty` 测试改调 `build_packs`（函数已改名）：把测试体内两处 `build_dossiers(` → `build_packs(`（断言不变——null 臂/on 臂语义保留；on 臂现是 pack，仍非空，断言 `!on[0].is_empty()` 仍成立）。

Run: `cargo test -p redteam 2>&1 | tail -30`
Expected: 全绿（新 `build_packs_*` PASS，改名后既有 toggle 测试 PASS，全 crate 无回归）

- [ ] **Step 5: 提交**

```bash
git add src/cli.rs
git commit -m "feat(pack): cli 接 build_packs + PACK_BUDGET_BYTES + REDTEAM_PACK 基线臂

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

### Task 7: 收尾——全量绿线 + clippy + 差异账回填确认

**Files:**
- （无新代码；核查 + 可能的注释/文档回填）

- [ ] **Step 1: 全量测试**

Run: `cargo test -p redteam 2>&1 | tail -30`
Expected: 全绿（含 dossier 既有 23 + pack 新增 + cli + prompt）

- [ ] **Step 2: clippy 无新警**

Run: `cargo clippy -p redteam --all-targets 2>&1 | tail -30`
Expected: 无 `-D warnings` 级新增告警（新 `src/pack/mod.rs` 干净）

- [ ] **Step 3: make verify 防漂移**（确认未误动 commands 镜像）

Run: `make verify 2>&1 | tail -20`
Expected: 绿（本 plan 全在 `src/`，未碰 `plugins/redteam/commands/`；若红且仅因既有漂移，与本 plan 无关）

- [ ] **Step 4: 差异账核对**

打开 `.doc/design/2026-07-07-redteam-context-management-s1-design.md §6`，逐条确认实现与「S1 实取」列一致：无 Cell struct ✓、0 壳 enclosing def 体无 per-body 帽 ✓、省略清单封顶 200 ✓、仅内联 read_files 提示 ✓、flat 预算常量 ✓、`REDTEAM_PACK` 基线臂 ✓。任何偏离 → 更新 §6 或回改代码。

- [ ] **Step 5: 提交（若有回填）**

```bash
git add -A
git commit -m "chore(pack): S1 收尾 verify + 差异账核对

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## 退出门（非本 plan 交付，登记）

S1 **验收门**（design §5）= 残余 tool_use 跨模型 robust 下降 **且** agreement 不收窄——**咬死 S0 基线**（未跑）。本 plan 只交付**机制**；跑门 = S0 rig 就绪后用 `REDTEAM_PACK` 三臂在 6f95c25 上 matched A/B（串行/逐模型/N=3）。**S1「达标」判定不在本 plan 范围**。

## 缓项（design §6 差异账 → TODO）

T27（per-model 真预算精度→S2）· T28（pack 语义包裹重塑）· T29（0 壳小文件全文）· T30（省略 cap 分组）· T31（省略全局 prompt 指令）。实现时若触碰，勿顺手做，记 TODO。
