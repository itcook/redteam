//! pack 引擎（M15-S1，E8-E 安乐死 SCIP/1 壳引用点后收窄）：每 bundle 的预算治理上下文块——
//! 0 壳改动源文全文（回落链：def 体→骨架→hunk 窗）+ 精确省略清单。唯一路径，无引擎分派。
//! hermetic：喂 (bundle, root, budget) 出字符串，零 LLM。复用 src/diffsym 的 diff/符号解析核心
//! （梯子降级用），不再消费其引用点搜索（该引擎已随 SCIP 一并拆除）。

use std::path::Path;

use regex::Regex;

use crate::bundle::Bundle;
use crate::diffsym::{changed_def_ranges, compile_defs, hunk_changed_lines, lang_for, match_def, parse_hunk_new_range};

/// 省略清单条目上限（防大 fan-in 撑爆；超出末尾计数兜底）。
pub(crate) const OMIT_MAX_ENTRIES: usize = 200;

/// 0 壳段：改动文件全文（粒度对齐模型文件级消费，E7 残余读解剖）；单文件全文超剩余预算
/// → 回落现状 def 体（支持语言）或 omit 坐标（非支持语言）。按 budget 字节封顶。
/// 唯一路径（Task 3：粒度 A/B S 臂符号级 dev 开关已判决拆除,全文恒默认）。
pub(crate) fn body_section(bundle: &Bundle, root: &Path, budget: usize, omit: &mut Vec<String>) -> String {
    let mut out = String::new();
    for fc in &bundle.files {
        // 工作树直读（bundle 路径来自 git diff、非模型可控，沿用 bundle::group 安全不变量）
        let content = std::fs::read_to_string(root.join(&fc.path)).unwrap_or_default();
        if content.is_empty() {
            continue;
        }
        let n_lines = content.lines().count();
        let block = format!("{}:1-{}（全文，{} 行）\n{}\n\n", fc.path, n_lines, n_lines, content);
        if out.len() + block.len() <= budget {
            out.push_str(&block);
            continue;
        }
        // def 体：全文超预算回落（不砍语义只降粒度）
        let lang = match lang_for(&fc.path) {
            Some(l) => l,
            None => {
                // 非支持语言超预算：先试 hunk±20 行窗（禁真空升级，Task 5）→ 装不下才 omit。
                if let Some(block) = hunk_window_block(&fc.path, &content, &fc.diff, n_lines) {
                    if out.len() + block.len() <= budget {
                        out.push_str(&block);
                        continue;
                    }
                }
                omit.push(format!("{}:1-{} — 全文超预算（非支持语言无 def 回落）", fc.path, n_lines));
                continue;
            }
        };
        let defs = compile_defs(lang);
        let lines: Vec<&str> = content.lines().collect();
        let mut wrote_def = false; // 至少一个 def 体入了正文
        let mut omitted_def = false; // 至少一个 def 体超预算进了 omit 坐标
        for (start, end) in changed_def_ranges(&fc.diff, &lines, &defs) {
            let end = end.min(lines.len().saturating_sub(1));
            let body = lines.get(start..=end).unwrap_or(&[]).join("\n");
            if body.is_empty() {
                continue;
            }
            let head = format!("{}:{}-{}（def 体，全文超预算回落）", fc.path, start + 1, end + 1);
            let block = format!("{head}\n{body}\n\n");
            if out.len() + block.len() > budget {
                omit.push(format!("{}:{}-{} — 改动符号体", fc.path, start + 1, end + 1));
                omitted_def = true;
            } else {
                out.push_str(&block);
                wrote_def = true;
            }
        }
        // 骨架档（回落链第 3 级插档，E8 裁决 D + Task 5，评审修 2 档位互斥）：本文件 def 体
        // **零入正文**（全部超预算或锚定失败）才触发；部分成功保持现状——已入 def + 溢出坐标，
        // 不叠加骨架（防同文件内容双份计预算）。溢出 def 的 omit 坐标保留——骨架只带签名+改动行，
        // 其余 def 体确实缺席，坐标仍真。骨架也装不下 → 落回现状消息（下方）。
        let mut wrote_skeleton = false;
        if !wrote_def {
            let skel = skeleton_of(&content, &fc.diff, &defs);
            let head = format!("{}:1-{}（骨架：签名+改动行）", fc.path, n_lines);
            let block = format!("{head}\n{skel}\n");
            if out.len() + block.len() <= budget {
                out.push_str(&block);
                wrote_skeleton = true;
            }
        }
        // E8 裁决 D：降级空产出禁真空——def 体没入、骨架没入、连溢出坐标都没有（锚定失败）时
        // 必须 omit 可观测（T42 场景）。
        if !wrote_def && !wrote_skeleton && !omitted_def {
            omit.push(format!("{}:1-{} — 全文超预算且 def 锚定失败（降级空产出）", fc.path, n_lines));
        }
    }
    if out.is_empty() {
        return String::new();
    }
    format!("## 改动源文（0 壳）\n{out}")
}

/// 骨架档（回落链第 3 级，E8 裁决 D + Task 5）：全文/def 体双超预算时的降级——逐行扫描，
/// 命中 def 正则的签名行全收（骨架地图）+ diff 全部改动行（`hunk_changed_lines`，原样保留，
/// 让改动本身可见——评审修 1：hunk_anchors 只给每 hunk 首行，多行 hunk 会丢内容）；
/// 其余连续行聚合折叠为 `…（N 行折叠）`。只产正文，不含块头（块头由
/// `body_section` 拼路径），故不判 budget——调用方按整块判。
/// ponytail: `changed_def_ranges` 命中的整个外围 def 体不整体保留（可能几百行，撑爆本就
/// 更紧的骨架预算，且骨架仅在 def 体已超预算时触发——含 def 全文必然同样超预算、自败）——
/// 只保信号密度最高的签名行 + 全部改动行，二者通常已覆盖诊断所需。
pub(crate) fn skeleton_of(content: &str, diff: &str, defs: &[(&'static str, Regex)]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let anchors: std::collections::HashSet<usize> = hunk_changed_lines(diff).into_iter().collect();
    let mut body = String::new();
    let mut fold = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if anchors.contains(&i) || match_def(line, defs).is_some() {
            if fold > 0 {
                body.push_str(&format!("…（{fold} 行折叠）\n"));
                fold = 0;
            }
            body.push_str(line);
            body.push('\n');
        } else {
            fold += 1;
        }
    }
    if fold > 0 {
        body.push_str(&format!("…（{fold} 行折叠）\n"));
    }
    body
}

/// 非支持语言超预算兜底（回落链第 2 级插档，Task 5）：全文放不下、无 def 可回落时，从 diff
/// 各 hunk 头解析新文件起止行 `(c, d)`，各取 `[c−20, c+d+20]` 窗、跨 hunk 合并成一个窗
/// （单 cell diff 罕见多 hunk；合并窗简单可靠，偶尔多带无关行也由调用方 budget 判定兜底裁掉）。
/// 窗内容原样保留、不折叠——非支持语言无符号结构，折叠无意义。无可解析 hunk 头 → None。
fn hunk_window_block(path: &str, content: &str, diff: &str, n_lines: usize) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for raw in diff.lines() {
        if !raw.starts_with("@@") {
            continue;
        }
        if let Some((c, d)) = parse_hunk_new_range(raw) {
            lo = lo.min(c.saturating_sub(20).max(1));
            hi = hi.max((c + d + 20).min(n_lines));
        }
    }
    if hi == 0 {
        return None;
    }
    let body = lines.get(lo.saturating_sub(1)..hi).unwrap_or(&[]).join("\n");
    if body.is_empty() {
        return None;
    }
    Some(format!("{path}:{lo}-{hi}（hunk±20 窗）\n{body}\n\n"))
}

/// 引擎入口：为一个 bundle 产 pack 正文（0 壳 + 省略清单），预算近→远填。
/// 不含 `<dossier>` 包裹（那是 prompt 层的活）。空 = 无 pack（降级）。
/// `own_diff_bytes`：本变更集 diff 的字节数（mandate——diff 是 prompt 恒渲染部分，永不砍，
/// 但占预算记账）：先从 budget 扣出预留，pack 各段再按剩余近→远填充。由调用方传入而非在此
/// 对 `bundle.files` 求和：二阶段补查传入的 augmented bundle 前插了 needs 文件，记账仍只按
/// 原始变更集 diff 算。
/// 唯一路径（E8-E 安乐死 SCIP/1 壳引用点后）：0 壳全文（+ 回落链：def 体→骨架→hunk 窗）是
/// 全部内容，无 1 壳/callee 段、无引擎分派。
pub fn build_pack(bundle: &Bundle, root: &Path, budget: usize, own_diff_bytes: usize) -> String {
    let remaining = budget.saturating_sub(own_diff_bytes);

    let mut omit: Vec<String> = Vec::new();

    // 0 壳：改动源文全文（唯一内容；overflow → omit）
    let bodies = body_section(bundle, root, remaining, &mut omit);

    // 组装：非空段拼接 + 省略清单（封顶 + 计数兜底）
    let mut sections: Vec<String> = Vec::new();
    if !bodies.is_empty() {
        sections.push(bodies);
    }
    if !omit.is_empty() {
        let total = omit.len();
        omit.truncate(OMIT_MAX_ENTRIES);
        let mut sec = String::from("## 已省略（预算裁剪）\n");
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
    fn body_section_renders_full_file() {
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
        assert!(s.contains("## 改动源文（0 壳）"), "含 0 壳标题: {s}");
        assert!(s.contains("fn caller() {") && s.contains("let z = 2;"), "含 caller 完整体");
        assert!(s.contains("fn helper"), "全文含未改动的 helper（粒度对齐模型文件级消费，E7）");
        assert!(omit.is_empty(), "预算充足无省略");
    }

    #[test]
    fn body_section_gives_full_file_within_budget() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "use std::fmt;\n\nfn helper() {}\n\nfn caller() {\n    helper();\n    let z = 2;\n}\n").unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -6,0 +6,1 @@\n+    let z = 2;\n".into(),
        }] };
        let mut omit = Vec::new();
        let s = body_section(&b, root, 1_000_000, &mut omit);
        assert!(s.contains("## 改动源文（0 壳）"), "新段标题: {s}");
        assert!(s.contains("src/a.rs:1-8（全文，8 行）"), "全文块头带行数: {s}");
        // 粒度对齐（E7）：全文包含改动 def 之外的内容——imports 与相邻函数
        assert!(s.contains("use std::fmt;") && s.contains("fn helper() {}"), "含 imports+相邻函数");
        assert!(omit.is_empty(), "预算充足无省略");
    }

    #[test]
    fn body_section_falls_back_to_def_body_when_file_exceeds_budget() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 大文件：filler 函数撑体积 + 一个小的改动函数
        let filler = format!("fn filler() {{\n{}}}\n", "    let x = 0;\n".repeat(200));
        std::fs::write(root.join("src/a.rs"),
            format!("{filler}\nfn caller() {{\n    work();\n}}\n")).unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -204,0 +204,1 @@\n+    work();\n".into(),
        }] };
        // 预算 500B：全文（~3.4KB）放不下 → 回落 caller def 体（~30B）放得下
        let mut omit = Vec::new();
        let s = body_section(&b, root, 500, &mut omit);
        assert!(s.contains("def 体，全文超预算回落"), "回落标注: {s}");
        assert!(s.contains("fn caller() {") && s.contains("work();"), "回落仍给改动 def 体");
        assert!(!s.contains("fn filler"), "回落不含无关大函数");
    }

    #[test]
    fn body_section_gives_full_file_for_unsupported_lang() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("notes.md"), "# 标题\n\n改动的文档内容。\n").unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "notes.md".into(), diff: "@@ -3,0 +3,1 @@\n+改动的文档内容。\n".into(),
        }] };
        let mut omit = Vec::new();
        let s = body_section(&b, root, 1_000_000, &mut omit);
        assert!(s.contains("改动的文档内容。"), "非支持语言也给全文（E7：templates 格整读实证）: {s}");
        // 超预算但装得下 hunk±20 窗（Task 5 骨架档兜底升级：非支持语言不再直接 omit）→ 窗内容替代 omit
        let mut omit2 = Vec::new();
        let s2 = body_section(&b, root, 68, &mut omit2);
        assert!(s2.contains("改动的文档内容。") && s2.contains("hunk±20 窗"), "窗兜底取代 omit: {s2}");
        assert!(omit2.is_empty(), "窗已装下，无需 omit: {omit2:?}");
        // 连窗都装不下才落回现状消息（禁真空兜底不能丢）
        let mut omit3 = Vec::new();
        let s3 = body_section(&b, root, 10, &mut omit3);
        assert!(s3.is_empty() && omit3.len() == 1 && omit3[0].contains("notes.md"), "窗也装不下才进 omit: {omit3:?}");
    }

    #[test]
    fn body_section_falls_to_skeleton_when_def_body_too_big() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 改动 def 本身巨大（def 体回落也超预算）+ 另有若干小 fn 签名
        let big_body = "    let x = 0;\n".repeat(300);
        std::fs::write(root.join("src/a.rs"),
            format!("fn tiny_one() {{}}\nfn changed_big() {{\n{big_body}}}\nfn tiny_two() {{}}\n")).unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -3,0 +3,1 @@\n+    let x = 0;\n".into(),
        }] };
        let mut omit = Vec::new();
        // 预算 800B：全文（~4.5KB）超、def 体（~4.2KB）超 → 骨架（3 签名行 + 折叠标记）应放得下
        let s = body_section(&b, root, 800, &mut omit);
        // 块头对齐真实语义（评审修 3）：签名+改动行（非改动 def 全文——那会自败超预算）
        assert!(s.contains("（骨架：签名+改动行）"), "骨架档启用: {s}");
        assert!(s.contains("fn tiny_one()") && s.contains("fn tiny_two()"), "签名行全收");
        assert!(s.contains("折叠"), "折叠标记在场");
    }

    #[test]
    fn skeleton_keeps_all_changed_lines_of_multiline_hunk() {
        // 评审修 1：hunk_anchors 只给每 hunk 首个改动行，多行 hunk 其余改动行被折叠吞掉。
        // 骨架必须保留 hunk 的全部改动行（hunk_changed_lines）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let filler = "    let f = 0;\n".repeat(300);
        let changed = "    let c1 = 1;\n    let c2 = 2;\n    let c3 = 3;\n    let c4 = 4;\n    let c5 = 5;\n";
        std::fs::write(root.join("src/a.rs"),
            format!("fn big_changed() {{\n{filler}{changed}}}\n")).unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(),
            // 新文件 302-306 行 = c1..c5（一个 5 行 hunk）
            diff: format!("@@ -301,0 +302,5 @@\n{}", changed.lines().map(|l| format!("+{l}\n")).collect::<String>()),
        }] };
        let mut omit = Vec::new();
        // 全文/def 体（~4.6KB）双超 800B → 骨架；5 个改动行必须全在
        let s = body_section(&b, root, 800, &mut omit);
        assert!(s.contains("骨架"), "骨架档启用: {s}");
        for c in ["let c1", "let c2", "let c3", "let c4", "let c5"] {
            assert!(s.contains(c), "多行 hunk 改动行全保留，缺 {c}: {s}");
        }
    }

    #[test]
    fn skeleton_not_stacked_when_some_def_bodies_fit() {
        // 评审修 2：档位互斥——部分 def 体已入正文时不再叠加骨架（防同文件内容双份计预算）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let big_body = "    let x = 0;\n".repeat(300);
        std::fs::write(root.join("src/a.rs"),
            format!("fn small_changed() {{\n    let a = 1;\n}}\nfn big_changed() {{\n{big_body}}}\n")).unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(),
            // 两 hunk：一个落 small 体内、一个落 big 体内
            diff: "@@ -2,0 +2,1 @@\n+    let a = 1;\n@@ -5,0 +5,1 @@\n+    let x = 0;\n".into(),
        }] };
        let mut omit = Vec::new();
        // 800B：全文超、small def 体（~100B）入正文、big def 体（~4.2KB）超 → 溢出坐标，无骨架
        let s = body_section(&b, root, 800, &mut omit);
        assert!(!s.contains("骨架"), "部分成功不叠加骨架: {s}");
        assert!(s.contains("fn small_changed") && s.contains("let a = 1;"), "已入 def 体保留: {s}");
        assert!(omit.iter().any(|o| o.contains("改动符号体")), "溢出 def 进 omit 坐标: {omit:?}");
    }

    #[test]
    fn body_section_unsupported_lang_falls_to_hunk_window() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        std::fs::write(root.join("notes.md"), &body).unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "notes.md".into(),
            diff: "@@ -100,0 +100,1 @@\n+line 100\n".into(),
        }] };
        let mut omit = Vec::new();
        let s = body_section(&b, root, 900, &mut omit);
        assert!(s.contains("line 100") && s.contains("line 85"), "hunk±20 窗在场: {s}");
        assert!(!s.contains("line 10\n"), "窗外内容不在");
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
    fn body_section_empty_when_file_missing_from_worktree() {
        // 文件不存在于工作树（如已删除的文件仍在 diff 里）→ content 空 → 静默跳过，
        // 不进正文也不进 omit（评审修：原名 …_no_supported_lang，实测的一直是本场景）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "src/gone.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn f() {}\n".into(),
        }] };
        let mut omit = Vec::new();
        assert_eq!(body_section(&b, tmp.path(), 1_000_000, &mut omit), "", "文件缺失 → 空");
        assert!(omit.is_empty(), "文件缺失不进 omit: {omit:?}");
    }

    #[test]
    fn body_section_degrade_empty_output_goes_to_omit() {
        // E8 裁决 D：全文超预算且 def 锚不到（如 hunk 全在 mod tests，T42 场景）→ 禁真空。
        // Task 5 骨架档插档后：即使无签名行匹配，骨架仍能用折叠标记+diff 改动行兜底承接，
        // 不再需要走到最终 omit 消息——骨架本身就是「禁真空」更具体的实现（比空坐标更有信号）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 大文件（超 500B 预算）+ diff 行落在无 def 可锚处（文件头注释区）
        let filler = format!("// header\n{}", "// filler line\n".repeat(200));
        std::fs::write(root.join("src/a.rs"), &filler).unwrap();
        let b = Bundle { files: vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -1,0 +2,1 @@\n+// filler line\n".into(),
        }] };
        let mut omit = Vec::new();
        let s = body_section(&b, root, 500, &mut omit);
        assert!(s.contains("src/a.rs") && s.contains("骨架"), "骨架档兜底承接：无签名行也不真空: {s}");
        assert!(omit.is_empty(), "骨架已承接，不再需要 omit 坐标: {omit:?}");
    }

    fn bundle_one(path: &str, diff: &str) -> crate::bundle::Bundle {
        crate::bundle::Bundle { files: vec![crate::evidence::git::FileChange {
            path: path.into(), diff: diff.into(),
        }] }
    }

    #[test]
    fn build_pack_end_to_end_has_bodies_and_diff_reserved() {
        // E8-E 安乐死后：build_pack 唯一内容 = 0 壳全文（无 1 壳/callee 段）。
        // 全文含改动 target 之外的 u1/u2/u3（粒度对齐模型文件级消费，E7），非靠引用点引擎。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "pub fn target() {\n    work();\n}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\n").unwrap();
        let b = bundle_one("src/a.rs", "@@ -1,0 +1,3 @@\n+pub fn target() {\n+    work();\n+}\n");
        let own = b.files.iter().map(|fc| fc.diff.len()).sum();
        let pack = build_pack(&b, root, 1_000_000, own);
        assert!(pack.contains("## 改动源文（0 壳）"), "有 0 壳: {pack}");
        assert!(pack.contains("pub fn target() {"), "0 壳含 target 完整体");
        assert!(pack.contains("u1()"), "0 壳全文含 caller（非 1 壳引用点，1 壳已拆除）");
        assert!(!pack.contains("## 引用点（1 壳）"), "1 壳引用点段已随 E8-E 拆除: {pack}");
    }

    #[test]
    fn build_pack_tiny_budget_spills_to_omission_list_with_coords() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "pub fn target() {}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\n").unwrap();
        let b = bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn target() {}\n");
        // 预算极小：引用点填不下 → 省略清单带坐标（无工具默认下不指挥不存在的工具）
        let own = b.files.iter().map(|fc| fc.diff.len()).sum();
        let pack = build_pack(&b, root, 60, own);
        assert!(pack.contains("## 已省略（预算裁剪）"), "有省略段: {pack}");
        assert!(!pack.contains("read_files"), "无工具默认下省略段不得指挥不存在的工具");
        assert!(pack.contains("src/a.rs:"), "省略项带 file:line 坐标");
    }

    #[test]
    fn build_pack_full_file_for_unsupported_lang() {
        // 语义翻转（E7）：非支持语言曾经全跳过、pack 空；0 壳升级整文件后即使无 def 可抽，
        // 全文仍在预算内 → pack 非空、含改动源文全文（brief 原断言基于「app.py 未写盘故 pack 空」
        // 的旧读法——此处补写盘以实测真实的「非支持语言给全文」行为，见 task-1 报告）。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("app.py"), "def f(): pass\n").unwrap();
        let b = bundle_one("app.py", "@@ -1,0 +1,1 @@\n+def f(): pass\n");
        let pack = build_pack(&b, root, 1_000_000, 0);
        assert!(pack.contains("## 改动源文（0 壳）"), "非支持语言也走 0 壳: {pack}");
        assert!(pack.contains("def f(): pass"), "含全文: {pack}");
    }
}
