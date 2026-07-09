//! pack 引擎（M15-S1）：每 bundle 的预算治理上下文块——0 壳改动符号完整体 + 1 壳引用点 + 精确省略清单。
//! hermetic：喂 (bundle, root, budget) 出字符串，零 LLM。复用 src/dossier 的符号/引用引擎。

use std::path::Path;

use crate::bundle::Bundle;
use crate::dossier::{changed_def_ranges, compile_defs, lang_for, SymRefs};
use crate::dossier::{symbols_in_file, reference_points, ChangedSym, Bucket};
use crate::scip::ScipIndex;

/// 单符号截断长 context（照 dossier 惯例，防 minified 行撑爆）。
const CONTEXT_MAX: usize = 200;

/// 每符号正文引用点上限（相关性/多样性帽，非预算帽；沿用 dossier 惯例 10）。
pub(crate) const PACK_PER_NAME_CAP: usize = 10;
/// 省略清单条目上限（防大 fan-in 撑爆；超出末尾计数兜底）。
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
            block.push_str(&format!("{}:{}\n{}\n\n", h.path, h.line, h.context));
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

/// SCIP 版 1 壳：每改动符号按位置对连 SCIP symbol → callers_of 引用点；
/// 位置对查无 → strict 跳过、否则退回单符号 regex reference_points。返回同构 Vec<SymRefs>（复用 refs_section 渲染）。
fn scip_refs(syms: &[ChangedSym], root: &Path, index: &ScipIndex, strict: bool, fallback_count: &mut usize) -> Vec<SymRefs> {
    let mut out = Vec::new();
    for s in syms {
        // 位置对：改动符号 def 行（symbols_in_file 已带 s.def_line0）→ SCIP symbol，精确无同名歧义
        let sym = index.symbol_at(&s.file, s.def_line0);
        match sym {
            Some(symbol) => {
                let hits: Vec<crate::dossier::RefHit> = index.callers_of(symbol).into_iter().map(|(path, line0)| {
                    let c = std::fs::read_to_string(root.join(&path)).unwrap_or_default();
                    let clines: Vec<&str> = c.lines().collect();
                    let context = if s.bucket == Bucket::A {
                        // A 桶：签名面变，caller 给完整外围体（enclosing def 范围切片）——方向性 D9
                        let cdefs = compile_defs(lang_for(&path).unwrap_or(&crate::dossier::RUST));
                        match crate::dossier::enclosing_def_start(&clines, line0, &cdefs) {
                            Some(st) => {
                                let en = crate::dossier::block_end(&clines, st).min(clines.len().saturating_sub(1));
                                clines.get(st..=en).unwrap_or(&[]).join("\n")
                            }
                            None => clines.get(line0).unwrap_or(&"").chars().take(CONTEXT_MAX).collect(),
                        }
                    } else {
                        clines.get(line0).unwrap_or(&"").chars().take(CONTEXT_MAX).collect()
                    };
                    crate::dossier::RefHit { path, line: line0 + 1, context }
                }).collect();
                out.push(SymRefs { name: s.name.clone(), kind: s.kind, defs_same_name: 1, hits });
            }
            None if strict => { /* 诊断臂：纯 SCIP、不兜底 */ }
            None => {
                *fallback_count += 1;
                // 符号级 regex 兜底：只对本符号跑 reference_points
                let defs = compile_defs(lang_for(&s.file).unwrap_or(&crate::dossier::RUST));
                let mut one = reference_points(std::slice::from_ref(s), root, &defs);
                out.append(&mut one);
            }
        }
    }
    out
}

/// callee 段：改动符号 def 区间内 callee symbol → 注入其签名（1 跳、不递归）。外部 dep 打 [dep]。
fn callee_section(syms: &[crate::dossier::ChangedSym], root: &Path, index: &ScipIndex, budget: usize, omit: &mut Vec<String>) -> String {
    let mut body = String::new();
    let mut seen = std::collections::HashSet::new();
    for s in syms {
        let content = std::fs::read_to_string(root.join(&s.file)).unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        // 位置对精确 def 行（Task 2 带的 s.def_line0）→ 区间 [def_line0, block_end]
        let end0 = crate::dossier::block_end(&lines, s.def_line0).min(lines.len().saturating_sub(1));
        let own = index.symbol_at(&s.file, s.def_line0).unwrap_or("");
        for callee in index.callees_in(&s.file, s.def_line0, end0, own) {
            let sig = match index.signature_of(&callee) { Some(sig) => sig, None => continue };
            if !seen.insert(callee.clone()) { continue; }
            let dep = if index.is_local(&callee) { "" } else { " [dep]" };
            // 展示名取 signature 本身（已含名+签名），dep 标后缀
            let line = format!("- {sig}{dep}\n");
            if body.len() + line.len() > budget {
                omit.push(format!("callee 签名 {callee}"));
            } else {
                body.push_str(&line);
            }
        }
    }
    if body.is_empty() { return String::new(); }
    format!("## 被调契约（callee）\n{body}")
}

/// SCIP 引擎入口：0 壳复用（body_section 不变）+ SCIP 1 壳 + callee 段 + 省略清单。
/// strict=true 为 scip-strict 诊断臂（关兜底）。
pub fn build_pack_scip(bundle: &Bundle, root: &Path, budget: usize, index: &ScipIndex, strict: bool) -> String {
    let diff_bytes: usize = bundle.files.iter().map(|fc| fc.diff.len()).sum();
    let remaining = budget.saturating_sub(diff_bytes);
    let mut omit: Vec<String> = Vec::new();

    let bodies = body_section(bundle, root, remaining, &mut omit);
    let after_bodies = remaining.saturating_sub(bodies.len());

    // 改动符号（复用 symbols_in_file，带 bucket）
    let mut all_syms: Vec<ChangedSym> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for fc in &bundle.files {
        let lang = match lang_for(&fc.path) { Some(l) => l, None => continue };
        let defs = compile_defs(lang);
        let content = std::fs::read_to_string(root.join(&fc.path)).unwrap_or_default();
        let file_lines: Vec<&str> = content.lines().collect();
        for s in symbols_in_file(&fc.diff, &file_lines, &fc.path, &defs) {
            if seen.insert(s.name.clone()) { all_syms.push(s); }
        }
    }
    let mut fallback_count = 0usize;
    let refs = scip_refs(&all_syms, root, index, strict, &mut fallback_count);
    let refs = refs_section(&refs, after_bodies, &mut omit);
    if fallback_count > 0 {
        eprintln!("redteam: scip 引擎 {fallback_count} 个符号退回 regex 兜底（位置对无 Definition occurrence）");
    }

    let after_refs = after_bodies.saturating_sub(refs.len());
    let callees = callee_section(&all_syms, root, index, after_refs, &mut omit);

    let mut sections: Vec<String> = Vec::new();
    if !bodies.is_empty() { sections.push(bodies); }
    if !refs.is_empty() { sections.push(refs); }
    if !callees.is_empty() { sections.push(callees); }
    if !omit.is_empty() {
        let total = omit.len();
        omit.truncate(OMIT_MAX_ENTRIES);
        let mut sec = String::from("## 已省略（可 read_files 定点读）\nread_files({paths:[{path,start,end}]})\n");
        for line in &omit { sec.push_str(line); sec.push('\n'); }
        if total > OMIT_MAX_ENTRIES { sec.push_str(&format!("… +{} more omitted\n", total - OMIT_MAX_ENTRIES)); }
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
            context: format!("    {name}(); // hit {i}"),
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

    #[test]
    fn build_pack_scip_uses_index_callers_and_falls_back_per_symbol() {
        use crate::scip::ScipIndex;
        use ::scip::types::{Document, Index, Occurrence};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // target def 在第0行；caller u1 在第1行调 target
        std::fs::write(root.join("src/a.rs"),
            "pub fn target(x: i32) {}\nfn u1() { target(1); }\n").unwrap();
        let occ = |l: i32, s: &str, r: i32| Occurrence { range: vec![l,0,0], symbol: s.into(), symbol_roles: r, ..Default::default() };
        let idx = ScipIndex::from_index(Index { documents: vec![Document {
            relative_path: "src/a.rs".into(),
            occurrences: vec![occ(0, "S target().", 1), occ(1, "S target().", 0)],
            ..Default::default()
        }], ..Default::default() });
        let b = bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn target(x: i32) {}\n");
        let pack = build_pack_scip(&b, root, 1_000_000, &idx, false);
        assert!(pack.contains("## 引用点（1 壳）"), "有 1 壳: {pack}");
        assert!(pack.contains("u1()") || pack.contains("target(1)"), "SCIP caller 命中 u1: {pack}");
    }

    #[test]
    fn build_pack_scip_falls_back_when_symbol_absent_from_index() {
        use crate::scip::ScipIndex;
        use ::scip::types::Index;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "pub fn ghost() {}\nfn u1() { ghost(); }\n").unwrap();
        let idx = ScipIndex::from_index(Index::default()); // 空索引 → 位置对必查无
        let b = bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn ghost() {}\n");
        let pack = build_pack_scip(&b, root, 1_000_000, &idx, false);
        assert!(pack.contains("ghost"), "空索引 → regex 兜底仍抓到 caller: {pack}");
        // strict 臂：关兜底 → 无引用点
        let strict = build_pack_scip(&b, root, 1_000_000, &idx, true);
        assert!(!strict.contains("## 引用点"), "strict 关兜底 → 空: {strict}");
    }

    #[test]
    fn build_pack_scip_a_bucket_gives_caller_full_body_b_gives_snippet() {
        use crate::scip::ScipIndex;
        use ::scip::types::{Document, Index, Occurrence};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // target def 第0行；caller u1 跨 2-4 行（多行体）
        std::fs::write(root.join("src/a.rs"),
            "pub fn target(x: i32) {}\nfn u1() {\n    let z = 1;\n    target(z);\n}\n").unwrap();
        let occ = |l: i32, s: &str, r: i32| Occurrence { range: vec![l,0,0], symbol: s.into(), symbol_roles: r, ..Default::default() };
        let idx = ScipIndex::from_index(Index { documents: vec![Document {
            relative_path: "src/a.rs".into(),
            occurrences: vec![occ(0,"S target().",1), occ(3,"S target().",0)],
            ..Default::default()
        }], ..Default::default() });
        // A：改签名行
        let a = build_pack_scip(&bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn target(x: i32) {}\n"), root, 1_000_000, &idx, false);
        assert!(a.contains("fn u1() {") && a.contains("let z = 1;"), "A 桶 caller 给完整外围体: {a}");
        // B：只改体
        let b = build_pack_scip(&bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+    // touched body\n"), root, 1_000_000, &idx, false);
        assert!(!b.contains("let z = 1;"), "B 桶 caller 只给片段、不含 caller 体其余行: {b}");
    }

    #[test]
    fn build_pack_scip_injects_callee_signatures() {
        use crate::scip::ScipIndex;
        // 注：signature_documentation 内层类型是 Signature（非 Document）——Task 1 已核实 scip 0.9.0 生成源，Signature 有同名 text: String。
        use ::scip::types::{Document, Index, Occurrence, Signature, SymbolInformation};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // caller def 第0-2行，内部调 helper
        std::fs::write(root.join("src/a.rs"),
            "pub fn caller() {\n    helper(1);\n}\n").unwrap();
        let occ = |l: i32, s: &str, r: i32| Occurrence { range: vec![l,0,0], symbol: s.into(), symbol_roles: r, ..Default::default() };
        let mut helper_si = SymbolInformation { symbol: "rust-analyzer cargo redteam 0.1.0 x/helper().".into(), ..Default::default() };
        helper_si.signature_documentation = protobuf::MessageField::some(Signature { text: "fn helper(n: i32) -> i32".into(), ..Default::default() });
        let idx = ScipIndex::from_index(Index { documents: vec![Document {
            relative_path: "src/a.rs".into(),
            occurrences: vec![
                occ(0, "rust-analyzer cargo redteam 0.1.0 x/caller().", 1),
                occ(1, "rust-analyzer cargo redteam 0.1.0 x/helper().", 0), // caller 体内调 helper
            ],
            symbols: vec![helper_si],
            ..Default::default()
        }], ..Default::default() });
        let b = bundle_one("src/a.rs", "@@ -1,0 +1,3 @@\n+pub fn caller() {\n+    helper(1);\n+}\n");
        let pack = build_pack_scip(&b, root, 1_000_000, &idx, false);
        assert!(pack.contains("## 被调契约（callee）"), "有 callee 段: {pack}");
        assert!(pack.contains("fn helper(n: i32) -> i32"), "注入 helper 签名: {pack}");
    }

    /// 回归：[dep] 判据须与跑仓的 crate 名无关（Important finding 修复）。
    /// 旧实现用 `callee.contains(" cargo redteam ")` 硬编码本 crate名——跑在任意别的仓上，
    /// 本仓 local callee 的 symbol 前缀就不是 "cargo redteam"，会被误标 [dep]。
    /// 正确判据：symbol 在索引里有 Definition occurrence（本 crate 名叫 myproj，helper 仍判 local；
    /// bufio 只有 Reference、无 Definition，判 [dep]）。
    #[test]
    fn callee_section_dep_tag_is_repo_agnostic_not_crate_name_based() {
        use crate::scip::ScipIndex;
        use ::scip::types::{Document, Index, Occurrence, Signature, SymbolInformation};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // caller 体内调 helper（本仓内定义、非 redteam crate 名）与 read（外部依赖，索引里只有引用无定义）
        std::fs::write(root.join("src/a.rs"),
            "pub fn caller() {\n    helper(1);\n    read();\n}\n").unwrap();
        let occ = |l: i32, s: &str, r: i32| Occurrence { range: vec![l, 0, 0], symbol: s.into(), symbol_roles: r, ..Default::default() };
        let mut helper_si = SymbolInformation { symbol: "rust-analyzer cargo myproj 0.1.0 x/helper().".into(), ..Default::default() };
        helper_si.signature_documentation = protobuf::MessageField::some(Signature { text: "fn helper(n: i32) -> i32".into(), ..Default::default() });
        let mut read_si = SymbolInformation { symbol: "rust-analyzer cargo bufio 1.0.0 x/read().".into(), ..Default::default() };
        read_si.signature_documentation = protobuf::MessageField::some(Signature { text: "fn read() -> usize".into(), ..Default::default() });
        let a_doc = Document {
            relative_path: "src/a.rs".into(),
            occurrences: vec![
                occ(0, "rust-analyzer cargo myproj 0.1.0 x/caller().", 1), // caller def
                occ(1, "rust-analyzer cargo myproj 0.1.0 x/helper().", 0), // 引用 helper
                occ(2, "rust-analyzer cargo bufio 1.0.0 x/read().", 0),    // 引用 read（外部依赖）
            ],
            symbols: vec![helper_si, read_si],
            ..Default::default()
        };
        // helper 的 Definition occurrence 落在另一文件（同一工作区索引常见跨文件情形）
        let b_doc = Document {
            relative_path: "src/b.rs".into(),
            occurrences: vec![occ(0, "rust-analyzer cargo myproj 0.1.0 x/helper().", 1)],
            ..Default::default()
        };
        let idx = ScipIndex::from_index(Index { documents: vec![a_doc, b_doc], ..Default::default() });
        let b = bundle_one("src/a.rs", "@@ -1,0 +1,4 @@\n+pub fn caller() {\n+    helper(1);\n+    read();\n+}\n");
        let pack = build_pack_scip(&b, root, 1_000_000, &idx, false);
        assert!(pack.contains("- fn helper(n: i32) -> i32\n"), "本仓 helper（非 redteam crate 名，有 Definition）不打 [dep]: {pack}");
        assert!(pack.contains("- fn read() -> usize [dep]\n"), "外部依赖 read（无 Definition）打 [dep]: {pack}");
    }
}
