//! pack 引擎（M15-S1）：每 bundle 的预算治理上下文块——0 壳改动符号完整体 + 1 壳引用点 + 精确省略清单。
//! hermetic：喂 (bundle, root, budget) 出字符串，零 LLM。复用 src/dossier 的符号/引用引擎。

use std::path::Path;

use crate::bundle::Bundle;
use crate::dossier::{changed_def_ranges, compile_defs, lang_for, SymRefs};
use crate::dossier::{symbols_in_file, reference_points, ChangedSym};

/// 每符号正文引用点上限（相关性/多样性帽，非预算帽；沿用 dossier 惯例 10）。
pub(crate) const PACK_PER_NAME_CAP: usize = 10;
/// 省略清单条目上限（防大 fan-in 撑爆；超出末尾计数兜底）。
#[allow(dead_code)]
pub(crate) const OMIT_MAX_ENTRIES: usize = 200;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
