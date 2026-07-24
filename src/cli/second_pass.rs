//! 二阶段补查（design §2.3）：needs 行窗解析、denylist/git-tracked 读守卫、
//! 证据段渲染、草稿注入段与第二趟 prompt 构造（`second_pass_prompt`）。

use crate::domain::Finding;
use crate::evidence::git::FileChange;
use std::path::Path;

/// 二阶段补查行窗（design §2.3）：lines 命中区间外扩 ±30 行。
const NEED_LINES_WINDOW: usize = 30;

/// 补查证据单条段字节帽（Important 修复：此前无上限——长单行文件上 ±30 行窗可逼近
/// read_capped 的 512KB 读帽，doc 线整文件 need 直接就是整文件，均绕开预算纪律）。
/// 量级对齐 verify.rs::SNIPPET_FULL_FILE_CAP（64KB）。
const NEED_SECTION_BYTE_CAP: usize = 64_000;

/// NeedEntry.lines 解析："a-b"（1-based、a<=b）→ Some((a,b))；缺失/坏格式 → None（整文件语义）。
fn parse_need_lines(lines: Option<&str>) -> Option<(u32, u32)> {
    let (a, b) = lines?.split_once('-')?;
    let a: u32 = a.trim().parse().ok()?;
    let b: u32 = b.trim().parse().ok()?;
    (a >= 1 && a <= b).then_some((a, b))
}

/// needs 读 denylist（E11-Q4）：即便 git-tracked 也拒——凭据类文件 + 工具自建/VCS 内部目录。
/// 与 `is_sensitive_spec_path`（--spec 路径，用户提供、信任模型不同）同族但独立：路径前缀/
/// 后缀匹配，不判内容（needs 读点无 PEM 嗅探，路径已够收窄威胁面）。
fn is_needs_denylisted(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    // .env 家族：.env / .env.local / .env.production 等变体全拒。
    if base == ".env" || base.ends_with(".env") || base.starts_with(".env.") || base.contains(".env.") {
        return true;
    }
    if base.ends_with(".pem") || base.ends_with(".key") {
        return true;
    }
    if base == "id_rsa" || base == "id_ed25519" {
        return true;
    }
    // 工具自建目录（.redteam/）与 VCS 内部目录（.git/）——任意路径深度，逐段匹配。
    lower.split(['/', '\\']).any(|seg| seg == ".redteam" || seg == ".git")
}

/// 二阶段补查 needs 读守卫（E11-Q4，dogfood 抓）：needs 是模型声明的路径（"我要读 X 文件"）——
/// 威胁模型 B（本地审不可信仓）下若不设限即为任意文件读。仅放行「git-tracked ∧ 非 denylist」。
/// git 判定失败（非 git 仓/git 缺失/命令报错）→ 保守拒（fail-closed，安全侧优先于可用性）。
/// worktree 下必须用 `root`（`current_dir`），不能信进程 cwd——镜像 evidence/git.rs 的 git_at 手法。
pub(crate) fn need_read_allowed(root: &Path, rel: &str) -> Result<(), &'static str> {
    if is_needs_denylisted(rel) {
        return Err("敏感路径（denylist 命中）");
    }
    // rel 是模型控制路径（dogfood 2026-07-17 抓）——`--` 停止选项解析（防 `-x`/`--cached` 式
    // 参数注入），`GIT_LITERAL_PATHSPECS=1` 禁 pathspec 魔法（防 `:(glob)`/`:!` 式让未跟踪文件
    // 蒙混过 --error-unmatch）。两道叠加使 rel 恒被当字面路径校验。
    match std::process::Command::new("git")
        .current_dir(root)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(["ls-files", "--error-unmatch", "--", rel])
        .output()
    {
        Ok(out) if out.status.success() => Ok(()),
        Ok(_) => Err("非 git-tracked"),
        Err(_) => Err("git 不可用（fail-closed）"),
    }
}

/// 单条 need → `## 补查证据` 段：lines 合法 → ±30 行窗；缺失/坏格式 → 整文件。
/// 路径安全走 ToolExecutor 不变量（绝对路径/`..`/symlink 逃逸/不可读 → None、不 panic）+
/// `need_read_allowed` 守卫（git-tracked ∧ 非 denylist，E11-Q4）；守卫拒则 skip + eprintln 披露。
fn need_section(ex: &crate::tools::ToolExecutor, root: &Path, need: &crate::domain::NeedEntry) -> Option<String> {
    if let Err(reason) = need_read_allowed(root, &need.path) {
        eprintln!("redteam: needs 读被拒（{reason}）：{}", need.path);
        return None;
    }
    let text = ex.read_capped(&need.path)?;
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    if n == 0 {
        return None;
    }
    let (a, b) = match parse_need_lines(need.lines.as_deref()) {
        Some((la, lb)) => (
            (la as usize).saturating_sub(NEED_LINES_WINDOW).max(1).min(n),
            (lb as usize).saturating_add(NEED_LINES_WINDOW).min(n),
        ),
        None => (1, n),
    };
    let b = b.max(a);
    let mut body = lines[a - 1..b].join("\n");
    if body.len() > NEED_SECTION_BYTE_CAP {
        let mut cut = NEED_SECTION_BYTE_CAP;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n…（截断）");
    }
    Some(format!("## 补查证据\n{}:{}-{}\n{}\n\n", need.path, a, b, body))
}

/// 草稿注入段（design §2.3）：第一趟 findings 以 CONFIRM-or-REVISE 契约随第二趟 prompt 带回；
/// 空草稿也渲染块（契约「不再声明 needs」对零草稿格同样生效）。
fn draft_block(drafts: &[Finding]) -> String {
    let mut s = String::from(
        "\n<first_pass_draft>\nYour first pass produced these draft findings. CONFIRM or REVISE each using the\nnewly added evidence — drop a draft ONLY if the evidence refutes it, and do not\nre-declare needs.\n",
    );
    for d in drafts {
        let quote_head: String = d.quote.chars().take(80).collect();
        s.push_str(&format!(
            "- [{}] {} @ {}:{}（{}）\n",
            d.severity.as_str(), d.title, d.file, d.line_start, quote_head
        ));
    }
    s.push_str("</first_pass_draft>\n");
    s
}

/// 二阶段补查 prompt（code 线，design §2.3 置换而非追加）：整文件 needs 前插 augmented bundle
/// （build_pack 顺序填充先进先占预算 → 自家非 needs 文件预算紧张时自动让位=置换语义）；lines 行窗 needs 走
/// `## 补查证据` 独立段置于卷宗最前；prompt 尾部追加第一趟草稿 CONFIRM-or-REVISE 段。
/// `incremental`：增量轮的 <incremental_round> 段原样带入第二趟（claimed-resolved/dispute
/// 契约不因补跑丢失）；全量路径传空串。全部 needs 逃逸/不可读 → None（不补跑，省一次 LLM 调用）。
/// `spec_docs`：--spec 参照段（D2），透传给 `build_bundle_prompt` 的 `{{SPEC_DOCS}}` 槽——
/// 第一趟算好的同一份 spec_section 原样复用，不重读；空串走既有占位散文（回归零漂移）。
#[allow(clippy::too_many_arguments)] // 与 build_bundle_prompt 同源的参数面；收拢 struct 是过度设计
pub(crate) fn second_pass_prompt(
    bundles: &[crate::bundle::Bundle],
    root: &Path,
    budget: usize,
    target_label: &str,
    focus: &str,
    idx: usize,
    needs: &[crate::domain::NeedEntry],
    drafts: &[Finding],
    incremental: &str,
    spec_docs: &str,
) -> Option<String> {
    let own = bundles.get(idx)?;
    let ex = crate::tools::ToolExecutor::new(root);
    // 变更集文件路径集合（披露用，E11-Q4）：needs 读的文件若不在其中即「变更集外」——
    // 即便 git-tracked 守卫放行，人（威胁模型 B 在场）也该知道读出了 diff 之外的内容。
    let changeset: std::collections::HashSet<&str> =
        bundles.iter().flat_map(|b| &b.files).map(|fc| fc.path.as_str()).collect();
    let disclose_scope = |path: &str| {
        if !changeset.contains(path) {
            eprintln!("redteam: needs 读取变更集外文件：{path}");
        }
    };
    // 两遍走：先收整文件条目——同 path 全文与行窗并存时全文优先（全文是超集，互斥防双重注入）。
    let mut full_paths: Vec<String> = Vec::new();
    for need in needs {
        if parse_need_lines(need.lines.as_deref()).is_none() && !full_paths.contains(&need.path) {
            match need_read_allowed(root, &need.path) {
                Ok(()) => {
                    // ponytail: read_capped 在此只当路径安全验证用（内容由 build_pack 自己读）——
                    // needs ≤5 条/格，多读一次成本可忽略，省一个只为验证的旁路 API。
                    if ex.read_capped(&need.path).is_some() {
                        disclose_scope(&need.path);
                        full_paths.push(need.path.clone());
                    }
                }
                Err(reason) => eprintln!("redteam: needs 读被拒（{reason}）：{}", need.path),
            }
        }
    }
    let mut evidence = String::new();
    for need in needs {
        if parse_need_lines(need.lines.as_deref()).is_some() && !full_paths.contains(&need.path) {
            if let Some(sec) = need_section(&ex, root, need) {
                disclose_scope(&need.path);
                evidence.push_str(&sec);
            }
        }
    }
    if evidence.is_empty() && full_paths.is_empty() {
        return None;
    }
    // augmented bundle：needs 前插（changeset 内的 need 取真 diff，防超预算 def 回落失锚）
    // → 自家文件（单格全审默认下 own 即全变更集，无兄弟可镜像）；预算记账仍只扣自家原 diff。
    let mut files: Vec<FileChange> = full_paths
        .into_iter()
        .map(|p| {
            let diff = bundles.iter().flat_map(|b| &b.files)
                .find(|f| f.path == p).map(|f| f.diff.clone()).unwrap_or_default();
            FileChange { path: p, diff }
        })
        .collect();
    for fc in own.files.iter() {
        if !files.iter().any(|e| e.path == fc.path) {
            files.push(fc.clone());
        }
    }
    let own_diff_bytes: usize = own.files.iter().map(|fc| fc.diff.len()).sum();
    let pb = crate::bundle::Bundle { files };
    // 第二趟恒用 build_pack（唯一路径，E8-E 安乐死 SCIP 后无引擎可选）——needs 只消费
    // 0 壳全文分支（body_section）。
    // Important 修复：evidence（行窗证据段，已在上方按 NEED_SECTION_BYTE_CAP 分段封顶）先
    // 扣出 pack 预算再传入 build_pack——否则 evidence+pack 联合越过每模型字节预算，撞墙全归零
    // （design §2.2）。budget 耗尽时 build_pack 自身的降级链兜底，不会 panic。
    let pack = crate::pack::build_pack(&pb, root, budget.saturating_sub(evidence.len()), own_diff_bytes);
    let dossier = crate::prompt::render_dossier_section(&format!("{evidence}{pack}"));
    let others = crate::bundle::other_files_manifest(bundles, idx);
    let mut p = crate::prompt::build_bundle_prompt(target_label, own, focus, &others, incremental, &dossier, spec_docs);
    p.push_str(&draft_block(drafts));
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;

    #[test]
    fn second_pass_prompt_injects_needs_evidence_and_drafts() {
        use crate::bundle::Bundle;
        use crate::domain::NeedEntry;
        // fixture：两 bundle 各一文件 + 一个变更集外文件 evidence.rs
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn own() { let z = 1; }\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "fn sib() {}\n").unwrap();
        std::fs::write(root.join("src/evidence.rs"), "pub const LIMIT: usize = 7;\n").unwrap();
        // needs 读守卫（E11-Q4）要求 git-tracked——固件须是提交过的 git 仓。
        crate::cli::incremental::incremental_tests::git_repo(root);
        crate::cli::incremental::incremental_tests::commit_all(root, "init");
        let bundles = vec![
            Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+ let z = 1;\n".into() }] },
            Bundle { files: vec![FileChange { path: "src/b.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn sib() {}\n".into() }] },
        ];
        let needs = vec![NeedEntry { path: "src/evidence.rs".into(), lines: None, reason: "verify LIMIT".into() }];
        let drafts = vec![]; // 空草稿也合法
        let p = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &drafts, "", "").unwrap();
        assert!(p.contains("pub const LIMIT"), "needs 文件全文进包: {p}");
        assert!(p.contains("first_pass_draft"), "草稿段在场");
        assert!(p.contains("CONFIRM or REVISE"), "确认或修订语义");
        assert!(!p.contains("<incremental_round>"), "全量路径第二趟无增量段");
        // 增量轮：inc_section 原样带入第二趟（claimed-resolved/dispute 契约不因补跑丢失）
        let inc = crate::prompt::render_incremental_section("abc123def456", "src/a.rs [delta]", &[]);
        let pi = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &drafts, &inc, "").unwrap();
        assert!(pi.contains("<incremental_round>") && pi.contains("abc123def456"), "增量段带入第二趟: {pi}");
        assert!(pi.contains("pub const LIMIT"), "增量第二趟 needs 证据仍在场");
        // 路径逃逸：绝对路径 needs 被 skip、不 panic、其余照常
        let evil = vec![NeedEntry { path: "/etc/passwd".into(), lines: None, reason: "x".into() }];
        let p2 = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &evil, &drafts, "", "");
        if let Some(p2) = p2 { assert!(!p2.contains("root:")); }
    }

    #[test]
    fn second_pass_injects_spec() {
        // spec 参照段接入第二趟补查（Task 3）：非空 spec_docs 与 needs 证据段各自独立并存；
        // 空 spec_docs 回归到既有占位散文，不泄漏内容。
        use crate::bundle::Bundle;
        use crate::domain::NeedEntry;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn own() { let z = 1; }\n").unwrap();
        std::fs::write(root.join("src/evidence.rs"), "pub const LIMIT: usize = 7;\n").unwrap();
        // needs 读守卫（E11-Q4）要求 git-tracked——固件须是提交过的 git 仓。
        crate::cli::incremental::incremental_tests::git_repo(root);
        crate::cli::incremental::incremental_tests::commit_all(root, "init");
        let bundles = vec![Bundle { files: vec![FileChange {
            path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+ let z = 1;\n".into(),
        }] }];
        let needs = vec![NeedEntry { path: "src/evidence.rs".into(), lines: None, reason: "verify LIMIT".into() }];
        let spec = "<spec_docs>\nDESIGN D8: fail-open\n</spec_docs>\n";
        let p = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &[], "", spec).unwrap();
        assert!(p.contains("DESIGN D8: fail-open"), "spec 段在第二趟原样注入: {p}");
        assert!(p.contains("pub const LIMIT"), "needs 证据段独立并存、不被 spec 段挤占: {p}");

        let p_empty = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &[], "", "").unwrap();
        assert!(p_empty.contains("(none — no spec docs provided)"), "空 spec 走既有占位: {p_empty}");
        assert!(!p_empty.contains("DESIGN D8"), "空 spec 不泄漏内容: {p_empty}");

        // T44：增量分支——inc_section 与 spec 同时非空须共存不互相挤占（run_incremental_review
        // 的 mk 闭包现同时透传两者，此前 spec 处恒占位 ""）。
        let inc = crate::prompt::render_incremental_section("abc123def456", "src/a.rs [delta]", &[]);
        let pi = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &[], &inc, spec).unwrap();
        assert!(pi.contains("<incremental_round>") && pi.contains("abc123def456"), "增量段在场: {pi}");
        assert!(pi.contains("<spec_docs>") && pi.contains("DESIGN D8: fail-open"), "spec 段与增量段共存: {pi}");
    }

    #[test]
    fn second_pass_prompt_lines_window_and_draft_format() {
        use crate::bundle::Bundle;
        use crate::domain::{NeedEntry, Origin};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn own() {}\n").unwrap();
        let body: String = (1..=200).map(|i| format!("line{i}\n")).collect();
        std::fs::write(root.join("src/long.rs"), &body).unwrap();
        std::fs::write(root.join("src/other.rs"), "pub fn other_evidence() {}\n").unwrap();
        // needs 读守卫（E11-Q4）要求 git-tracked——固件须是提交过的 git 仓。
        crate::cli::incremental::incremental_tests::git_repo(root);
        crate::cli::incremental::incremental_tests::commit_all(root, "init");
        let bundles = vec![Bundle { files: vec![FileChange {
            path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn own() {}\n".into(),
        }] }];
        let needs = vec![
            // 合法 lines → ±30 行窗独立补查段
            NeedEntry { path: "src/long.rs".into(), lines: Some("100-102".into()), reason: "r".into() },
            // 坏格式 lines → 按整文件（前插 bundle 路径，全文进 0 壳）
            NeedEntry { path: "src/other.rs".into(), lines: Some("abc".into()), reason: "r".into() },
        ];
        let drafts = vec![Finding {
            id: "x".into(), severity: Severity::High, title: "weak hash".into(), body: "b".into(),
            file: "src/a.rs".into(), line_start: 5, line_end: 6, confidence: 0.9,
            recommendation: "r".into(), models: vec![], agreement: 1,
            hard_constraint: false, quote: "md5(pw)".into(), cluster_id: None, verify: None,
            previously_dismissed: false, origin: Origin::New,
        }];
        let p = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &drafts, "", "").unwrap();
        assert!(p.contains("## 补查证据"), "行窗补查段在场");
        assert!(p.contains("src/long.rs:70-132"), "±30 行窗头: {p}");
        assert!(p.contains("line70") && p.contains("line132"), "窗内行在场");
        assert!(p.contains("pub fn other_evidence"), "坏格式 lines 走整文件路径（全文进 0 壳）");
        assert!(p.contains("- [high] weak hash @ src/a.rs:5（md5(pw)）"), "草稿行格式: {p}");

        // 互斥：同 path 既有整文件条目又有行窗条目 → 全文优先（超集），行窗段不再重复注入
        let both = vec![
            NeedEntry { path: "src/long.rs".into(), lines: Some("100-102".into()), reason: "r".into() },
            NeedEntry { path: "src/long.rs".into(), lines: None, reason: "r".into() },
        ];
        let pb = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &both, &drafts, "", "").unwrap();
        assert!(pb.contains("src/long.rs:1-200（全文"), "全文条目在场: {pb}");
        assert!(!pb.contains("## 补查证据"), "同 path 行窗段被全文超集互斥掉: {pb}");
    }

    #[test]
    fn second_pass_prompt_caps_huge_lines_window_evidence() {
        // Important 修复回归：单行超长（如压缩/minified 文件）时 ±30 行窗退化成"整行"，
        // 旧代码无字节帽——body 可逼近 read_capped 的 512KB 读帽。新帽 NEED_SECTION_BYTE_CAP
        // 应把它摁在 64KB，且 evidence 扣减后 pack 仍正常渲染（预算纪律不因证据段被撞穿）。
        use crate::bundle::Bundle;
        use crate::domain::NeedEntry;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn own() {}\n").unwrap();
        let huge_line = "x".repeat(100_000); // 无换行——lines() 只有 1 行
        std::fs::write(root.join("src/minified.rs"), &huge_line).unwrap();
        // needs 读守卫（E11-Q4）要求 git-tracked——固件须是提交过的 git 仓。
        crate::cli::incremental::incremental_tests::git_repo(root);
        crate::cli::incremental::incremental_tests::commit_all(root, "init");
        let bundles = vec![Bundle { files: vec![FileChange {
            path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn own() {}\n".into(),
        }] }];
        let needs = vec![NeedEntry { path: "src/minified.rs".into(), lines: Some("1-1".into()), reason: "r".into() }];
        let p = second_pass_prompt(&bundles, root, 1_000_000, "t", "", 0, &needs, &[], "", "").unwrap();
        assert!(p.contains("…（截断）"), "证据段超帽应带截断注记: {}", &p[..p.len().min(200)]);
        assert!(p.contains("fn own()"), "证据段截断后 pack 仍照常渲染自家文件");
        assert!(p.len() < 300_000, "总 prompt 长度被证据帽拘住，未逼近原 100KB 单行体量: {}", p.len());
    }

    #[test]
    fn needs_rejects_untracked_and_sensitive_paths() {
        // E11-Q4 dogfood 抓：二阶段补查 needs 是模型声明路径——威胁模型 B 下不设限即任意文件读。
        // git repo fixture：tracked src/foo.rs 提交后放行；untracked .env / 工具自建
        // .redteam/proj.db 命中 denylist 拒（即便后者恰好也 untracked，denylist 独立于 git 判定）。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/foo.rs"), "fn a(){}\n").unwrap();
        crate::cli::incremental::incremental_tests::git_repo(root);
        crate::cli::incremental::incremental_tests::commit_all(root, "init");
        std::fs::write(root.join(".env"), "SECRET=x\n").unwrap(); // 提交后再写，保证真 untracked
        assert!(need_read_allowed(root, "src/foo.rs").is_ok(), "tracked 源文件放行");
        assert!(need_read_allowed(root, ".env").is_err(), "untracked 敏感文件拒");
        assert!(need_read_allowed(root, ".redteam/proj.db").is_err(), "工具自建 untracked 拒（denylist）");

        // fail-closed：非 git 仓（或 git 判定失败）时即便文件存在也保守拒，不放行。
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::write(tmp2.path().join("bare.rs"), "fn b(){}\n").unwrap();
        assert!(need_read_allowed(tmp2.path(), "bare.rs").is_err(), "非 git 仓 fail-closed 拒");

        // dogfood 2026-07-17：git 参数/pathspec 注入——`--` + GIT_LITERAL_PATHSPECS 后，
        // 选项样式路径与 pathspec 魔法均不能让未跟踪内容蒙混放行。
        assert!(need_read_allowed(root, "--cached").is_err(), "选项样式路径须拒（-- 停止选项解析）");
        assert!(need_read_allowed(root, ":(glob)**/*.env").is_err(), "pathspec 魔法须拒（字面路径校验）");
        assert!(need_read_allowed(root, ":!src").is_err(), "pathspec 排除魔法须拒");
    }
}
