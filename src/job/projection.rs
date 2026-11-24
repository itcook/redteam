//! 2.1 输入真相源：`inputs.diff_patch` → `FileChange[]`，**不**用 git 重新采集。
//!
//! hibot 的 patch 由 runner 在本地 `git diff merge-base..head` 生成并带 sha256 交付；执行单元
//! 消费它即可（契约 §1.2：「执行单元不得自行换 diff 基线」）。git 只用于**证据收集**
//! （风险路由 / 分组 / 装包要读工作树），以及两边都可得时的一致性校验——不一致**只披露**，
//! 不改行为（计划 2.1）。

use crate::evidence::git::{self, FileChange};
use crate::job::status::JobFailure;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// 披露列表的封顶（协调者 2026-11-24 裁：各 ≤20 项 + `*_total`，按路径字典序）。
const MAX_DISCLOSED: usize = 20;

/// patch → 逐文件变更。**非空 patch 切不出任何文件 = 形态不支持**（例如 `--no-prefix`），
/// 必须报错而不是静默退化成「零变更的 ok」。
pub fn from_patch(patch: &str) -> Result<Vec<FileChange>, JobFailure> {
    let changes = git::split_diff_by_file(patch);
    if changes.is_empty() && !patch.trim().is_empty() {
        return Err(JobFailure::new(
            "input_schema_mismatch",
            Some("diff_patch".into()),
        ));
    }
    Ok(changes)
}

/// 与 `git diff <merge_base>..HEAD` 的一致性披露。
///
/// - 一致：`{match: true, git: "match", files, git_files}`；
/// - 不一致：附三张**封顶排序**的表（`only_in_patch` / `only_in_git` / `differing`）与 `*_total`；
/// - git 不可得：`git: "unavailable"`（只披露，不影响本次评审用 patch 跑）。
pub fn consistency(repo: &Path, merge_base: &str, patch: &[FileChange]) -> Value {
    let patch_digests = digest_map(patch);
    let git_changes = match git::changed_files_since(repo, merge_base) {
        Ok(c) => c,
        Err(_) => {
            return json!({
                "match": false,
                "git": "unavailable",
                "files": patch_digests.len(),
            })
        }
    };
    let git_digests = digest_map(&git_changes);
    let only_in_patch: Vec<&str> = patch_digests
        .keys()
        .filter(|k| !git_digests.contains_key(*k))
        .map(String::as_str)
        .collect();
    let only_in_git: Vec<&str> = git_digests
        .keys()
        .filter(|k| !patch_digests.contains_key(*k))
        .map(String::as_str)
        .collect();
    let differing: Vec<&str> = patch_digests
        .iter()
        .filter(|(k, v)| git_digests.get(*k).is_some_and(|g| g != *v))
        .map(|(k, _)| k.as_str())
        .collect();
    let matched = only_in_patch.is_empty() && only_in_git.is_empty() && differing.is_empty();
    let mut out = json!({
        "match": matched,
        "git": if matched { "match" } else { "mismatch" },
        "files": patch_digests.len(),
        "git_files": git_digests.len(),
        "only_in_patch": cap(&only_in_patch),
        "only_in_patch_total": only_in_patch.len(),
        "only_in_git": cap(&only_in_git),
        "only_in_git_total": only_in_git.len(),
        "differing": cap(&differing),
        "differing_total": differing.len(),
    });
    if matched {
        // 一致时不重复三张空表：只留计数，报告更小。
        out.as_object_mut()
            .unwrap()
            .retain(|k, _| !k.starts_with("only_") && k != "differing");
        out["only_in_patch_total"] = json!(0);
        out["only_in_git_total"] = json!(0);
        out["differing_total"] = json!(0);
    }
    out
}

fn cap(items: &[&str]) -> Vec<String> {
    items
        .iter()
        .take(MAX_DISCLOSED)
        .map(|s| (*s).to_string())
        .collect()
}

/// 路径 → 文件 diff 的摘要（BTreeMap 保证按路径字典序）。
/// `trim_end` 抹掉 patch 文件末尾多一个换行这类搬运差异——那属于**文件末尾空白**，不是内容。
fn digest_map(changes: &[FileChange]) -> BTreeMap<String, String> {
    changes
        .iter()
        .map(|c| {
            let mut h = Sha256::new();
            h.update(c.diff.trim_end().as_bytes());
            (
                c.path.clone(),
                h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn change(path: &str, body: &str) -> FileChange {
        FileChange {
            path: path.into(),
            diff: body.into(),
        }
    }

    #[test]
    fn empty_patch_is_empty_but_nonempty_unparsable_patch_is_an_error() {
        assert!(from_patch("").unwrap().is_empty());
        assert!(from_patch("\n\n").unwrap().is_empty());
        let e = from_patch("--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n").unwrap_err();
        assert_eq!(
            e.full_code(),
            "input_schema_mismatch:diff_patch",
            "无 diff --git 头 = 形态不支持"
        );
    }

    #[test]
    fn splits_patch_into_file_changes() {
        let patch = "diff --git a/a.js b/a.js\nindex 1..2 100644\n--- a/a.js\n+++ b/a.js\n@@ -1 +1 @@\n-x\n+y\n\
                     diff --git a/b.js b/b.js\nindex 3..4 100644\n--- a/b.js\n+++ b/b.js\n@@ -1 +1 @@\n-p\n+q\n";
        let c = from_patch(patch).unwrap();
        assert_eq!(
            c.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
            vec!["a.js", "b.js"]
        );
        assert!(c[0].diff.contains("-x") && c[1].diff.contains("+q"));
    }

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

    fn repo_with_change() -> (tempfile::TempDir, std::path::PathBuf, String, String) {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "t"]);
        std::fs::write(repo.join("a.js"), "old\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        std::fs::write(repo.join("a.js"), "new\n").unwrap();
        git(&repo, &["commit", "-qam", "head"]);
        let patch = git(&repo, &["diff", &format!("{base}..HEAD")]);
        (d, repo, base, patch)
    }

    #[test]
    fn matching_patch_and_git_disclose_match_without_lists() {
        let (_d, repo, base, patch) = repo_with_change();
        let changes = from_patch(&patch).unwrap();
        let v = consistency(&repo, &base, &changes);
        assert_eq!(v["match"], true, "{v}");
        assert_eq!(v["git"], "match");
        assert_eq!(v["files"], 1);
        assert!(v.get("only_in_patch").is_none(), "一致时不带空表");
        assert_eq!(v["only_in_patch_total"], 0);
    }

    #[test]
    fn mismatch_is_disclosed_with_sorted_capped_lists() {
        let (_d, repo, base, _patch) = repo_with_change();
        // 伪造 patch：一个 git 没有的文件 + 一个内容不同的同名文件。
        let fake = vec![
            change("zz.js", "diff --git a/zz.js b/zz.js\n@@ -1 +1 @@\n-a\n+b\n"),
            change(
                "a.js",
                "diff --git a/a.js b/a.js\n@@ -1 +1 @@\n-other\n+other2\n",
            ),
        ];
        let v = consistency(&repo, &base, &fake);
        assert_eq!(v["match"], false);
        assert_eq!(v["git"], "mismatch");
        assert_eq!(v["only_in_patch"], json!(["zz.js"]));
        assert_eq!(v["only_in_git"], json!([]));
        assert_eq!(v["differing"], json!(["a.js"]));
        assert_eq!(v["only_in_patch_total"], 1);
        assert_eq!(v["differing_total"], 1);
    }

    #[test]
    fn cap_keeps_twenty_and_reports_total() {
        let items: Vec<String> = (0..25).map(|i| format!("f{i:02}.js")).collect();
        let refs: Vec<&str> = items.iter().map(String::as_str).collect();
        let capped = cap(&refs);
        assert_eq!(capped.len(), MAX_DISCLOSED);
        assert_eq!(capped[0], "f00.js");
    }

    #[test]
    fn unavailable_git_is_disclosed_not_fatal() {
        let d = tempfile::tempdir().unwrap();
        let v = consistency(d.path(), "deadbeef", &[change("a.js", "x")]);
        assert_eq!(v["git"], "unavailable");
        assert_eq!(v["files"], 1);
    }
}
