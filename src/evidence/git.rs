use std::process::Command;

pub const INLINE_MAX_FILES: usize = 2;
pub const INLINE_MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: String,
    pub diff: String,
}

/// 阈值决策：返回 (内联 diff 或 None, 指引文本)。
pub fn decide_inline(files: &[String], diff_bytes: usize, diff: &str) -> (Option<String>, String) {
    if files.len() <= INLINE_MAX_FILES && diff_bytes <= INLINE_MAX_BYTES {
        (Some(diff.to_string()), String::new())
    } else {
        let list = files.join("\n  - ");
        let guidance = format!(
            "The diff is large ({} files / {} bytes) and has not been inlined. Changed files:\n  - {}\nPlease read these files and their changes yourself using your tools to perform the review.",
            files.len(), diff_bytes, list
        );
        (None, guidance)
    }
}

/// 将换行分隔的文件名列表去重（保留首次出现顺序），忽略空行。
/// 供 working-tree 分支使用：staged 和 unstaged 的 --name-only 输出拼接后可能含重复文件。
pub fn dedup_files(raw: &str) -> Vec<String> {
    let mut seen = std::collections::BTreeSet::new();
    let mut result = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if !line.is_empty() && seen.insert(line.to_string()) {
            result.push(line.to_string());
        }
    }
    result
}

/// 把文件列表与「逐文件取 diff」的闭包组装成 FileChange；跳过空 diff（纯模式变更等）。
fn assemble_changes(files: &[String], per_file: impl Fn(&str) -> String) -> Vec<FileChange> {
    files
        .iter()
        .filter_map(|p| {
            let diff = per_file(p);
            if diff.trim().is_empty() {
                None
            } else {
                Some(FileChange { path: p.clone(), diff })
            }
        })
        .collect()
}

fn git(args: &[&str]) -> anyhow::Result<String> {
    // 关死 ANSI 污染（dogfood 20260703 r2 b4d096e0 / r3 661e3973 活体实证）：
    // color.ui=false 压不住更具体的 color.diff=always（git 具体键优先），必须
    // 同时给 color.diff=false——命令行 -c 恒优先于文件 config。
    let out = Command::new("git")
        .args(["-c", "color.ui=false", "-c", "color.diff=false"])
        .args(args)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 收集 code 证据：branch scope = base...HEAD；working-tree = cached+worktree。
/// 返回 (body, guidance, target_label)。
pub fn collect_code(base: Option<&str>, branch_scope: bool) -> anyhow::Result<(String, String, String)> {
    let (diff, files, label) = if branch_scope {
        let base = base.ok_or_else(|| anyhow::anyhow!("branch scope requires --base"))?;
        let merge_base = git(&["merge-base", base, "HEAD"])?.trim().to_string();
        let diff = git(&["diff", &format!("{merge_base}...HEAD")])?;
        let files = git(&["diff", "--name-only", &format!("{merge_base}...HEAD")])?;
        (diff, files, format!("branch {base}...HEAD"))
    } else {
        let mut diff = git(&["diff", "--cached"])?;
        diff.push_str(&git(&["diff"])?);
        let mut files = git(&["diff", "--cached", "--name-only"])?;
        files.push_str(&git(&["diff", "--name-only"])?);
        (diff, files, "working tree".to_string())
    };
    let file_list: Vec<String> = dedup_files(&files);
    let (inline, guidance) = decide_inline(&file_list, diff.len(), &diff);
    let body = inline.unwrap_or_else(|| format!("(diff not inlined)\nChanged files:\n{}", file_list.join("\n")));
    Ok((body, guidance, label))
}

/// 收集逐文件 diff：branch scope = base...HEAD；working-tree = cached + worktree。
/// 仅 run_review_dispatch 调用（真实 shell git）；单测注入 FileChange，不走此函数。
pub fn changed_files(base: Option<&str>, branch_scope: bool) -> anyhow::Result<Vec<FileChange>> {
    if branch_scope {
        let base = base.ok_or_else(|| anyhow::anyhow!("branch scope requires --base"))?;
        let merge_base = git(&["merge-base", base, "HEAD"])?.trim().to_string();
        let range = format!("{merge_base}...HEAD");
        let files = dedup_files(&git(&["diff", "--name-only", &range])?);
        Ok(assemble_changes(&files, |p| {
            git(&["diff", &range, "--", p]).unwrap_or_else(|e| {
                eprintln!("warning: failed to diff {p}: {e}");
                String::new()
            })
        }))
    } else {
        let mut raw = git(&["diff", "--cached", "--name-only"])?;
        raw.push_str(&git(&["diff", "--name-only"])?);
        let files = dedup_files(&raw);
        Ok(assemble_changes(&files, |p| {
            let warn = |e| { eprintln!("warning: failed to diff {p}: {e}"); String::new() };
            let mut d = git(&["diff", "--cached", "--", p]).unwrap_or_else(&warn);
            d.push_str(&git(&["diff", "--", p]).unwrap_or_else(&warn));
            d
        }))
    }
}

/// 指定目录跑 git（M13 增量基建：hermetic 测试可用 tempdir 仓库，不依赖进程 cwd）。
/// ANSI 关死同 `git()`（color.diff 必须显式 false，命令行 -c 恒优先于文件 config）。
fn git_at(dir: &std::path::Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "color.ui=false", "-c", "color.diff=false"])
        .args(args)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// R3 stamp：证据 HEAD（权威时刻 = 证据收集实跑刻，调用方与 changed_files 同刻调用）。
pub fn head_sha_at(dir: &std::path::Path) -> anyhow::Result<String> {
    Ok(git_at(dir, &["rev-parse", "HEAD"])?.trim().to_string())
}

/// 谱系守卫（增量守卫①）：sha 是否 HEAD 祖先。rebase/reset/amend 后 false
/// （锚定不变式失效——delta 范围不再覆盖「使锚失效的改动」）。
/// merge-base --is-ancestor 以退出码表意（0=是/1=否），不能走 git_at 的 success 判定。
pub fn is_ancestor(dir: &std::path::Path, sha: &str) -> anyhow::Result<bool> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["merge-base", "--is-ancestor", sha, "HEAD"])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!(
            "git merge-base --is-ancestor {sha} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// 增量守卫辅助：工作树相对 HEAD 是否有未提交改动（staged+unstaged；不看 untracked——
/// 未跟踪文件承载不了上轮 finding 的锚）。exit 0=净 / 1=脏。
pub fn worktree_dirty(dir: &std::path::Path) -> anyhow::Result<bool> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["diff", "HEAD", "--quiet"])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => anyhow::bail!(
            "git diff HEAD --quiet failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// R1 rename 映射：base..HEAD 的旧→新路径对（-M 相似度检测，status R*）。
/// 相似度 <50% 的 rename 不被 -M 识别 → 不进映射，carry 走 carried_dropped 降级（可审非静默）。
pub fn rename_map(dir: &std::path::Path, base: &str) -> anyhow::Result<Vec<(String, String)>> {
    let out = git_at(dir, &["diff", "--name-status", "-M", &format!("{base}..HEAD")])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let status = it.next()?;
            if !status.starts_with('R') {
                return None;
            }
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect())
}

/// 增量轮 delta（决议 2）：base..HEAD 的 -M diff，按文件切分。
/// **不走既有「逐文件 pathspec 再 diff」模式**：`git diff -M <range> -- <新路径>` 的
/// pathspec 会把旧路径滤出配对域，rename 检测失效 → 纯 rename 膨胀成整文件新增
/// （R1 经济性被击穿）。单次全量 diff + 按 `diff --git` 边界切分则天然保留 rename 元数据。
pub fn changed_files_since(dir: &std::path::Path, base: &str) -> anyhow::Result<Vec<FileChange>> {
    let full = git_at(dir, &["diff", "-M", &format!("{base}..HEAD")])?;
    Ok(split_diff_by_file(&full))
}

/// 按 "diff --git a/X b/Y" 边界切分整份 diff；path 取 b 侧（rename 挂新路径下）。
// ponytail: 路径含 " b/" 子串或引号转义的病态仓不在本项目威胁模型内，rfind 足够；真撞见再上引号解析。
fn split_diff_by_file(full: &str) -> Vec<FileChange> {
    let mut out: Vec<FileChange> = Vec::new();
    for line in full.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            let path = rest
                .rfind(" b/")
                .map(|i| rest[i + 3..].to_string())
                .unwrap_or_else(|| rest.to_string());
            out.push(FileChange { path, diff: String::new() });
        }
        if let Some(last) = out.last_mut() {
            last.diff.push_str(line);
            last.diff.push('\n');
        }
    }
    out.retain(|fc| !fc.diff.trim().is_empty());
    out
}

/// 全分支变更文件清单（双层 manifest 第一层）：merge-base(base,HEAD)...HEAD 的 name-only。
pub fn branch_files(dir: &std::path::Path, base: &str) -> anyhow::Result<Vec<String>> {
    let mb = git_at(dir, &["merge-base", base, "HEAD"])?.trim().to_string();
    Ok(dedup_files(&git_at(dir, &["diff", "--name-only", &format!("{mb}...HEAD")])?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 测试基建：tempdir 里造真实 git 仓库（后续 rename/delta/谱系测试共用）。
    fn git_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
    }

    fn commit_all(dir: &Path, msg: &str) -> String {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", msg]);
        run(&["rev-parse", "HEAD"]).trim().to_string()
    }

    #[test]
    fn head_sha_at_returns_current_commit() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
        let sha = commit_all(dir.path(), "c1");
        assert_eq!(head_sha_at(dir.path()).unwrap(), sha);
        // 非 git 目录 → Err（调用方 .ok() 折 None，不炸评审）
        let plain = tempfile::tempdir().unwrap();
        assert!(head_sha_at(plain.path()).is_err());
    }

    #[test]
    fn large_diff_falls_back_to_file_list() {
        let files = vec!["a.rs".to_string(), "b.rs".into(), "c.rs".into()]; // >2 文件
        let (inline, guidance) = decide_inline(&files, 10, "DIFF");
        assert!(inline.is_none());
        assert!(guidance.contains("a.rs") && guidance.contains("yourself"));
    }
    #[test]
    fn small_diff_inlines() {
        let files = vec!["a.rs".to_string()];
        let (inline, _) = decide_inline(&files, 10, "DIFF");
        assert_eq!(inline.as_deref(), Some("DIFF"));
    }
    #[test]
    fn dedup_files_removes_duplicates_and_preserves_order() {
        // 模拟 staged+unstaged --name-only 拼接：同一文件出现两次
        let raw = "src/lib.rs\nsrc/main.rs\nsrc/lib.rs\n\nsrc/util.rs\n";
        let result = dedup_files(raw);
        assert_eq!(result, vec!["src/lib.rs", "src/main.rs", "src/util.rs"]);
        assert_eq!(result.len(), 3); // 去重后 3 个，不是 4 个
    }
    #[test]
    fn dedup_files_empty_input() {
        assert_eq!(dedup_files(""), Vec::<String>::new());
        assert_eq!(dedup_files("\n\n"), Vec::<String>::new());
    }
    #[test]
    fn assemble_changes_pairs_files_with_diffs() {
        let files = vec!["a.rs".to_string(), "b.rs".to_string()];
        let out = assemble_changes(&files, |p| format!("diff for {p}"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], FileChange { path: "a.rs".into(), diff: "diff for a.rs".into() });
        assert_eq!(out[1].path, "b.rs");
    }

    #[test]
    fn assemble_changes_skips_empty_diffs() {
        let files = vec!["a.rs".to_string(), "empty.rs".to_string(), "whitespace.rs".to_string()];
        let out = assemble_changes(&files, |p| {
            match p {
                "empty.rs" => String::new(),
                "whitespace.rs" => "   \n".into(),
                _ => "x".into(),
            }
        });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "a.rs");
    }

    #[test]
    fn rename_map_and_delta_since_pure_rename() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("old.rs"), "fn keep() { body(); }\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();
        commit_all(dir.path(), "rename");
        // R1：纯 rename → 映射一条 (old,new)
        assert_eq!(rename_map(dir.path(), &c1).unwrap(), vec![("old.rs".to_string(), "new.rs".to_string())]);
        // delta：-M 下纯 rename 只产 rename 元数据 diff（不炸整删整加），挂新路径名下
        let delta = changed_files_since(dir.path(), &c1).unwrap();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].path, "new.rs");
        assert!(delta[0].diff.contains("rename from old.rs"), "{}", delta[0].diff);
        assert!(!delta[0].diff.contains("-fn keep"), "纯 rename 不应展开成删加: {}", delta[0].diff);
    }

    #[test]
    fn changed_files_since_content_change_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "line1\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        // HEAD 未动 → 空 delta（守卫④判据）
        assert!(changed_files_since(dir.path(), &c1).unwrap().is_empty());
        std::fs::write(dir.path().join("a.rs"), "line1\nline2\n").unwrap();
        commit_all(dir.path(), "c2");
        let delta = changed_files_since(dir.path(), &c1).unwrap();
        assert_eq!(delta.len(), 1);
        assert!(delta[0].diff.contains("+line2"));
        assert!(rename_map(dir.path(), &c1).unwrap().is_empty(), "内容改动不产 rename 映射");
    }

    #[test]
    fn is_ancestor_true_for_parent_false_after_amend() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        commit_all(dir.path(), "c2");
        assert!(is_ancestor(dir.path(), &c1).unwrap());
        // amend 重写 c2 → 旧 c2 不再是 HEAD 祖先（谱系守卫①判据）
        let out = std::process::Command::new("git").current_dir(dir.path())
            .args(["rev-parse", "HEAD"]).output().unwrap();
        let old_head = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let ok = std::process::Command::new("git").current_dir(dir.path())
            .args(["commit", "-q", "--amend", "-m", "c2-rewritten"]).output().unwrap();
        assert!(ok.status.success());
        assert!(!is_ancestor(dir.path(), &old_head).unwrap(), "amend 后旧 HEAD 非祖先");
    }

    #[test]
    fn worktree_dirty_detects_uncommitted_tracked_edits_but_not_untracked() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        commit_all(dir.path(), "c1");
        assert!(!worktree_dirty(dir.path()).unwrap(), "刚提交，净树");
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        assert!(worktree_dirty(dir.path()).unwrap(), "tracked 文件未提交编辑 → 脏");
        // 撤销改动回到净树，再加一个 untracked 新文件——不应算脏（diff HEAD 看不到 untracked）
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        assert!(!worktree_dirty(dir.path()).unwrap());
        std::fs::write(dir.path().join("untracked.rs"), "new\n").unwrap();
        assert!(!worktree_dirty(dir.path()).unwrap(), "untracked 新文件不算脏");
    }

    #[test]
    fn branch_files_lists_merge_base_to_head() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("base.rs"), "x\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        // 打 base 分支引用在 c1，之后 main 前进两个 commit
        let ok = std::process::Command::new("git").current_dir(dir.path())
            .args(["branch", "dev0", &c1]).output().unwrap();
        assert!(ok.status.success());
        std::fs::write(dir.path().join("f1.rs"), "a\n").unwrap();
        commit_all(dir.path(), "c2");
        std::fs::write(dir.path().join("f2.rs"), "b\n").unwrap();
        commit_all(dir.path(), "c3");
        let files = branch_files(dir.path(), "dev0").unwrap();
        assert_eq!(files, vec!["f1.rs".to_string(), "f2.rs".to_string()]);
    }

    #[test]
    fn split_diff_by_file_segments_and_takes_b_side_path() {
        let full = "diff --git a/old.rs b/new.rs\nsimilarity index 100%\nrename from old.rs\nrename to new.rs\ndiff --git a/x.rs b/x.rs\nindex 111..222 100644\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1,2 @@\n line1\n+line2\n";
        let out = split_diff_by_file(full);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, "new.rs", "rename 段挂 b 侧新路径");
        assert!(out[0].diff.contains("rename from old.rs"));
        assert_eq!(out[1].path, "x.rs");
        assert!(out[1].diff.contains("+line2"));
        assert!(split_diff_by_file("").is_empty(), "空 diff → 空 delta");
    }
}
