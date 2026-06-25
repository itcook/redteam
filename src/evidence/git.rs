use std::process::Command;

pub const INLINE_MAX_FILES: usize = 2;
pub const INLINE_MAX_BYTES: usize = 256 * 1024;

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

fn git(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").args(args).output()?;
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

#[cfg(test)]
mod tests {
    use super::*;
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
}
