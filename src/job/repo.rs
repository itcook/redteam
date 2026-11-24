//! 2.6 检出完整性：hibot 保证 repo 是 **detached、非 shallow 的完整检出**，
//! head/base 对象本地可得、merge-base 一定可算（hibot C1 / 计划 §12 Q5）。
//!
//! 本模块只校验执行单元**真正依赖**的事实：浅克隆、`pr.head_sha` / `pr.base_sha` /
//! `inputs.merge_base` 的对象可得性、`git merge-base <base> <head>` 可算。
//! **缺必需对象 → `status=error`**（不得披露后仍报 `ok`）。
//!
//! 两条刻意不做的判断：
//! - 不设 `merge_base == pr.base_sha` 的门（Q2：`merge_base` 是实际 diff 左端，base 分支前进后本就不同）；
//! - 非 detached 不判错（同一 commit 的分支检出语义等价；detached 是 hibot 的输入保证，不是我们的门禁）。

use crate::evidence::git;
use crate::job::manifest::PrRef;
use crate::job::status::JobFailure;
use std::path::Path;

fn incomplete(what: &str) -> JobFailure {
    JobFailure::new("repo_incomplete", Some(what.to_string()))
}

/// `inputs.merge_base` 是 40 位小写 hex（契约 §1.2：「40 位 sha（纯文本）」）。
pub fn is_full_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// 校验完整性并返回解析后的**完整 HEAD sha**（后续作为 `report.input.head_sha` 的对照事实）。
///
/// 检查顺序即错误码优先级：浅克隆 → HEAD 可解析 → HEAD == `pr.head_sha`（解析成完整 commit 再比）
/// → `pr.base_sha` 对象 → `inputs.merge_base` 对象 → merge-base 可算。
pub fn ensure_complete(repo: &Path, pr: &PrRef, merge_base: &str) -> Result<String, JobFailure> {
    // manifest 的 `pr.head_sha` / `pr.base_sha` 是契约 SHA，不是 revision 表达式（`HEAD` / 分支名一律拒）。
    if !is_full_sha(&pr.head_sha) || !is_full_sha(&pr.base_sha) {
        return Err(JobFailure::new(
            "job_manifest_unsupported",
            Some("pr.head_sha / pr.base_sha must be 40-hex sha".into()),
        ));
    }
    if git::is_shallow_repository(repo).map_err(|_| incomplete("shallow"))? {
        return Err(incomplete("shallow"));
    }
    let head = git::head_sha_at(repo).map_err(|_| incomplete("head"))?;
    // 按合法 SHA 解析成完整 commit 再核对确切提交：前缀/SHA 形状的宽容只发生在 rev-parse，
    // 不在这里做双向 `starts_with`（那会把「同一前缀的另一提交」当成匹配）。
    let want_head = git::resolve_commit(repo, &pr.head_sha).map_err(|_| incomplete("head"))?;
    if want_head != head {
        return Err(JobFailure::new("head_sha_mismatch", None));
    }
    git::resolve_commit(repo, &pr.base_sha).map_err(|_| incomplete("base"))?;
    git::resolve_commit(repo, merge_base).map_err(|_| incomplete("merge_base"))?;
    // 只要求可算；**不**与 inputs.merge_base 比相等（Q2）。
    git::merge_base_of(repo, &pr.base_sha, &pr.head_sha).map_err(|_| incomplete("merge_base"))?;
    // commit 对象存在不等于 diff 可算：两侧 tree/blob 也可能缺。只验本次 diff 所需对象（不做全仓历史扫描）。
    git::diff_available(repo, merge_base).map_err(|_| incomplete("merge_base"))?;
    Ok(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;
    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn pr(head: &str, base: &str) -> PrRef {
        PrRef {
            number: 7,
            head_sha: head.into(),
            base_sha: base.into(),
        }
    }

    /// base→head 两提交；返回 (base, head) 与临时目录（持有着，drop 即删）。
    fn repo() -> (tempfile::TempDir, PathBuf, String, String) {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.txt"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("a.txt"), "head\n").unwrap();
        git(&repo, &["commit", "-qam", "head"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        (d, repo, base, head)
    }
    #[test]
    fn complete_repo_returns_full_head() {
        let (_d, repo, base, head) = repo();
        let got = ensure_complete(&repo, &pr(&head, &base), &base).unwrap();
        assert_eq!(got, head);
    }

    /// 契约的 `pr.*` 是 40 位 SHA；`HEAD` / 分支名 / 短 sha 这类 revision 表达式一律拒。
    #[test]
    fn non_sha_revisions_in_manifest_are_rejected() {
        let (_d, repo, base, head) = repo();
        for bad in ["HEAD", "main", &head[..8]] {
            let e = ensure_complete(&repo, &pr(bad, &base), &base).unwrap_err();
            assert_eq!(e.code, "job_manifest_unsupported", "{bad}");
        }
    }

    /// 2.6 反例：base 的 commit 对象被删（真实缺对象）→ error，不是 partial/ok。
    #[test]
    fn missing_base_commit_object_is_repo_incomplete() {
        let (_d, repo, base, head) = repo();
        // 松散对象路径（刚提交、未 gc）。
        let p = repo.join(".git/objects").join(&base[..2]).join(&base[2..]);
        assert!(p.is_file(), "fixture 期望松散对象: {}", p.display());
        std::fs::remove_file(&p).unwrap();
        // pr.base_sha 指向 head（存在），merge_base 指向被删的 base。
        let e = ensure_complete(&repo, &pr(&head, &head), &base).unwrap_err();
        assert_eq!(e.full_code(), "repo_incomplete:merge_base", "{e:?}");
    }

    /// 2.6 反例：浅克隆 → error（base 对象根本不在本地）。
    #[test]
    fn shallow_checkout_is_repo_incomplete() {
        let (d, repo, base, head) = repo();
        let shallow = d.path().join("shallow");
        let out = Command::new("git")
            .args(["clone", "-q", "--depth", "1", &format!("file://{}", repo.display()), shallow.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let e = ensure_complete(&shallow, &pr(&head, &base), &base).unwrap_err();
        assert_eq!(e.full_code(), "repo_incomplete:shallow", "{e:?}");
    }

    #[test]
    fn head_mismatch_is_head_sha_mismatch_not_repo_incomplete() {
        let (_d, repo, base, head) = repo();
        // 检出在 base 上，manifest 说 head：HEAD 对象存在、能解析，但不是一个提交。
        git(&repo, &["checkout", "-q", &base]);
        let e = ensure_complete(&repo, &pr(&head, &base), &base).unwrap_err();
        assert_eq!(e.code, "head_sha_mismatch");
    }

    #[test]
    fn missing_base_object_is_repo_incomplete_base() {
        let (_d, repo, base, head) = repo();
        let e = ensure_complete(
            &repo,
            &pr(&head, "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"),
            &base,
        )
        .unwrap_err();
        assert_eq!(e.full_code(), "repo_incomplete:base");
    }

    #[test]
    fn missing_merge_base_object_is_repo_incomplete_merge_base() {
        let (_d, repo, base, head) = repo();
        let e = ensure_complete(
            &repo,
            &pr(&head, &base),
            "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef",
        )
        .unwrap_err();
        assert_eq!(e.full_code(), "repo_incomplete:merge_base");
    }

    #[test]
    fn merge_base_need_not_equal_pr_base_sha() {
        // Q2 判例：diff 左端（merge_base）是 base 的祖先时，两者不同也合法。
        let (d, repo, base, head) = repo();
        git(&repo, &["checkout", "-q", "-b", "side", &base]);
        std::fs::write(repo.join("side.txt"), "side\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "side"]);
        let side = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["checkout", "-q", "-"]);
        let _ = d;
        // side 与 base 同源：merge-base(side, head) == base，而 pr.base_sha 取 side —— 不相等但可算。
        assert_eq!(
            ensure_complete(&repo, &pr(&head, &side), &base).unwrap(),
            head
        );
    }

    #[test]
    fn full_sha_shape() {
        assert!(is_full_sha(&"a".repeat(40)));
        assert!(!is_full_sha(&"A".repeat(40)), "大写不合法");
        assert!(!is_full_sha("abc"), "短 sha 不是契约的 merge_base 形态");
    }
}
