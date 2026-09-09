//! 二阶段补查零件（design §2.3）：needs 行窗解析、denylist/git-tracked 读守卫、证据段渲染、草稿注入段。
//! `second_pass_prompt`（第二趟 prompt 构造）随旧 fan-out 在 M21a 批 F-4 删除；scanner 的 needs 只记录不补跑（prd K6），
//! 本模块留给 M21b reviewer 的 needs 二阶段复用（design R2），故暂 `allow(dead_code)`。
#![allow(dead_code)]

use crate::domain::Finding;
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
/// M18 批 1·1-2：判据本身已抽到 `deny::is_denylisted_path`（与 spec.rs 的 `is_sensitive_spec_path`
/// 共用单一真相源、语义=两清单并集——原本各自独有的判据互补齐），本函数只剩薄委托。
/// needs 读点仍不做内容级 PEM 嗅探（E11-Q4 已裁：路径已够收窄威胁面，与 spec 侧信任模型不同）。
fn is_needs_denylisted(rel: &str) -> bool {
    super::deny::is_denylisted_path(rel)
}

/// 拒读事件统一披露（T46②DRY + T46①入账，M18 批 1）：needs 读守卫拒读原分散两处 eprintln
/// 重复段——本函数收敛为单点：eprintln 供本地终端可见（既有行为不变）+ push 进 warnings
/// 供上游并入 `run.warnings`（CI/bot 可见，复用 M17-c 披露通道手法）。
fn deny_need_read(reason: &str, path: &str, warnings: &mut Vec<String>) {
    let msg = format!("needs 读被拒（{reason}）：{path}");
    eprintln!("redteam: {msg}");
    warnings.push(msg);
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
/// `need_read_allowed` 守卫（git-tracked ∧ 非 denylist，E11-Q4）；守卫拒则 skip + 披露
/// （`deny_need_read`：eprintln + 入 `warnings`，T46①②）。
fn need_section(
    ex: &crate::tools::ToolExecutor,
    root: &Path,
    need: &crate::domain::NeedEntry,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if let Err(reason) = need_read_allowed(root, &need.path) {
        deny_need_read(reason, &need.path, warnings);
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

#[cfg(test)]
mod tests {
    use super::*;

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
