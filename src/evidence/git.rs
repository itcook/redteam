use std::process::Command;

#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: String,
    pub diff: String,
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

/// 收集逐文件 diff：branch scope = base...HEAD；working-tree = cached + worktree。
/// `dir`：git 仓库根（hermetic，走 `git_at`，不依赖进程 cwd——镜像 `changed_files_since` 等
/// M13 helper；doc 模式退场后 job 测试改走此路，需要可注入的 tempdir 仓库，2026-07-14）。
/// 仅 run_review_dispatch 调用（真实 shell git）；单测注入 FileChange，不走此函数。
pub fn changed_files(dir: &std::path::Path, base: Option<&str>, branch_scope: bool) -> anyhow::Result<Vec<FileChange>> {
    if branch_scope {
        let base = base.ok_or_else(|| anyhow::anyhow!("branch scope requires --base"))?;
        // `--end-of-options`：base 是 --base 传入的 user-arg（威胁模型 B：本身可信，但仍以
        // 攻击者摆布的分支名/字符串形态出现——如 "--output=/tmp/x"）。挡在 base 前，保证它
        // 恒被当 rev 解析、绝不会被 git 当选项吃掉（既往 plain `--` 会把 range 误判成
        // pathspec、静默空输出；`--end-of-options` 无此副作用，见 evidence/git.rs 顶注）。
        let merge_base = git_at(dir, &["merge-base", "--end-of-options", base, "HEAD"])?.trim().to_string();
        let range = format!("{merge_base}...HEAD");
        let files = dedup_files(&git_at(dir, &["diff", "--name-only", "--end-of-options", &range])?);
        Ok(assemble_changes(&files, |p| {
            git_at(dir, &["diff", "--end-of-options", &range, "--", p]).unwrap_or_else(|e| {
                eprintln!("warning: failed to diff {p}: {e}");
                String::new()
            })
        }))
    } else {
        let mut raw = git_at(dir, &["diff", "--cached", "--name-only"])?;
        raw.push_str(&git_at(dir, &["diff", "--name-only"])?);
        let files = dedup_files(&raw);
        Ok(assemble_changes(&files, |p| {
            let warn = |e| { eprintln!("warning: failed to diff {p}: {e}"); String::new() };
            let mut d = git_at(dir, &["diff", "--cached", "--", p]).unwrap_or_else(&warn);
            d.push_str(&git_at(dir, &["diff", "--", p]).unwrap_or_else(&warn));
            d
        }))
    }
}

/// 收集 `merge-base(base, head)...head` 的逐文件 diff（M21b 批 A `risk` 回放子命令）：
/// 与 `changed_files` 的 branch 分支同构，但 head 可为任意历史 commit（回放 Oracle B 历史 diff，
/// 不依赖工作树 checkout）；与 `replay_from_git` 的历史 commit 内容读取配套使用。
/// `-M` 强开重命名检测 + 整份 diff 按文件切分（与 `eval/risk_replay.py` 的 `git diff -M` 口径一致：
/// 纯重命名无 hunk → 不成区、不计行；逐路径 diff 会把重命名看成删除+全量新增产生假区）。
pub fn changed_files_at(dir: &std::path::Path, base: &str, head: &str) -> anyhow::Result<Vec<FileChange>> {
    let merge_base = git_at(dir, &["merge-base", "--end-of-options", base, head])?.trim().to_string();
    let range = format!("{merge_base}...{head}");
    let full = git_at(dir, &["diff", "--no-ext-diff", "-M", "--end-of-options", &range])?;
    Ok(split_diff_by_file(&full))
}

/// 指定目录跑 git（M13 增量基建：hermetic 测试可用 tempdir 仓库，不依赖进程 cwd）。
/// ANSI 关死同 `git()`（color.diff 必须显式 false，命令行 -c 恒优先于文件 config）。
fn git_at(dir: &std::path::Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .current_dir(dir)
        // core.quotePath=false：非 ASCII 路径原样输出，不转成引号转义串——
        // split_diff_by_file 的 strip_prefix 与 rename_map 的相等匹配都依赖裸路径。
        .args(["-c", "color.ui=false", "-c", "color.diff=false", "-c", "core.quotePath=false"])
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

/// 任意 rev 的 blob 内容（`git show <rev>:<path>`；M21b 批 A `risk` 回放：读历史 commit 文件，不依赖工作树）。
/// 文件在该 rev 不存在 / 二进制 / IO 失败 → Err（调用方跳过该文件）。
pub fn show_blob(dir: &std::path::Path, rev: &str, path: &str) -> anyhow::Result<String> {
    git_at(dir, &["show", "--end-of-options", &format!("{rev}:{path}")])
}

/// 谱系守卫（增量守卫①）：sha 是否 HEAD 祖先。rebase/reset/amend 后 false
/// （锚定不变式失效——delta 范围不再覆盖「使锚失效的改动」）。
/// merge-base --is-ancestor 以退出码表意（0=是/1=否），不能走 git_at 的 success 判定。
pub fn is_ancestor(dir: &std::path::Path, sha: &str) -> anyhow::Result<bool> {
    // `--end-of-options`：sha 是位置参数（虽是内部产出的 SHA，非直接 user-arg，但作为纵深
    // 防御同族补齐——理由与 changed_files 顶注一致）。
    let out = Command::new("git")
        .current_dir(dir)
        .args(["merge-base", "--is-ancestor", "--end-of-options", sha, "HEAD"])
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
// ponytail: 路径含 tab 的病态仓不在威胁模型内；quotePath 已关，中文等非 ASCII 路径原样可配。
pub fn rename_map(dir: &std::path::Path, base: &str) -> anyhow::Result<Vec<(String, String)>> {
    // `--end-of-options`：base 拼进 range 字符串里的位置参数，同 changed_files 顶注防注入。
    let out = git_at(dir, &["diff", "--name-status", "-M", "--end-of-options", &format!("{base}..HEAD")])?;
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
    // `--end-of-options`：同 rename_map，base 拼进 range 字符串的位置参数防注入。
    let full = git_at(dir, &["diff", "-M", "--end-of-options", &format!("{base}..HEAD")])?;
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
    out
}

/// 全分支变更文件清单（双层 manifest 第一层）：merge-base(base,HEAD)...HEAD 的 name-only。
pub fn branch_files(dir: &std::path::Path, base: &str) -> anyhow::Result<Vec<String>> {
    // `--end-of-options`：base 是位置参数，同 changed_files 顶注防注入；mb 是 merge-base 的
    // 输出（已由 git 自身校验落回合法 SHA），但仍一并加固——不假设下游拼接恒安全。
    let mb = git_at(dir, &["merge-base", "--end-of-options", base, "HEAD"])?.trim().to_string();
    Ok(dedup_files(&git_at(dir, &["diff", "--name-only", "--end-of-options", &format!("{mb}...HEAD")])?))
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

    /// 1-5：git 参数注入回归锚——`--base` 若长成 `--output=<file>` 这种形态（威胁模型 B：
    /// 攻击者摆布 CLI 参数字符串），此前会被当选项吃掉，git diff/merge-base 借 `--output`
    /// 把 diff 内容任意写到攻击者选择的路径（真实验证过的漏洞，见 2026-07-14 dogfood）。
    /// `--end-of-options` 令这类字符串恒被当 rev 解析失败（fail-closed：函数返回 Err），
    /// 而不是被当选项静默执行——本测试断言两件事：调用失败 + 目标文件确实没被写出。
    #[test]
    fn changed_files_branch_scope_rejects_option_shaped_base_without_file_write() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        commit_all(dir.path(), "c1");
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        commit_all(dir.path(), "c2");

        let target = dir.path().join("pwned_by_base");
        let malicious_base = format!("--output={}", target.display());
        let result = changed_files(dir.path(), Some(&malicious_base), true);
        assert!(result.is_err(), "option 形态的 base 须 fail-closed（Err），而非被当选项静默执行");
        assert!(!target.exists(), "--output 注入不应能任意写文件：{target:?}");
    }

    #[test]
    fn branch_files_rejects_option_shaped_base_without_file_write() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        commit_all(dir.path(), "c1");

        let target = dir.path().join("pwned_by_branch_files");
        let malicious_base = format!("--output={}", target.display());
        let result = branch_files(dir.path(), &malicious_base);
        assert!(result.is_err(), "option 形态的 base 须 fail-closed（Err）");
        assert!(!target.exists(), "--output 注入不应能任意写文件：{target:?}");
    }

    /// rename_map/changed_files_since 把 base 直接拼进 range 字符串（`{base}..HEAD`）——
    /// 与 changed_files 的独立 merge-base 调用是不同的注入面（这里连 merge-base 校验都没有，
    /// base 直接进 git diff 的位置参数），同样须被 `--end-of-options` 挡住。
    #[test]
    fn changed_files_since_and_rename_map_reject_option_shaped_base_without_file_write() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        commit_all(dir.path(), "c1");
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        commit_all(dir.path(), "c2");

        let target1 = dir.path().join("pwned_by_delta");
        let malicious_base = format!("--output={}", target1.display());
        assert!(changed_files_since(dir.path(), &malicious_base).is_err());
        assert!(!target1.exists(), "changed_files_since 不应被注入写文件：{target1:?}");

        let target2 = dir.path().join("pwned_by_rename_map");
        let malicious_base2 = format!("--output={}", target2.display());
        assert!(rename_map(dir.path(), &malicious_base2).is_err());
        assert!(!target2.exists(), "rename_map 不应被注入写文件：{target2:?}");
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

    #[test]
    fn non_ascii_filename_survives_split_and_rename_map() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("配置.rs"), "line1\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::fs::write(dir.path().join("配置.rs"), "line1\nline2\n").unwrap();
        let c2 = commit_all(dir.path(), "c2");
        // quotePath=false：changed_files_since 的 path 与 diff 都不应被引号转义
        let delta = changed_files_since(dir.path(), &c1).unwrap();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].path, "配置.rs");
        assert!(delta[0].diff.contains("+line2"), "{}", delta[0].diff);
        // rename：配置.rs → 配置2.rs，rename_map 靠裸路径相等匹配（转义后永不相等）
        std::fs::rename(dir.path().join("配置.rs"), dir.path().join("配置2.rs")).unwrap();
        commit_all(dir.path(), "rename");
        assert_eq!(
            rename_map(dir.path(), &c2).unwrap(),
            vec![("配置.rs".to_string(), "配置2.rs".to_string())]
        );
    }
}
