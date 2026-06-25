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
            "改动较大（{} 文件 / {} 字节），未内联完整 diff。改动文件：\n  - {}\n请自己用工具读取这些文件与其改动来评审。",
            files.len(), diff_bytes, list
        );
        (None, guidance)
    }
}

fn git(args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").args(args).output()?;
    if !out.status.success() {
        anyhow::bail!("git {:?} 失败: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// 收集 code 证据：branch scope = base...HEAD；working-tree = cached+worktree。
/// 返回 (body, guidance, target_label)。
pub fn collect_code(base: Option<&str>, branch_scope: bool) -> anyhow::Result<(String, String, String)> {
    let (diff, files, label) = if branch_scope {
        let base = base.ok_or_else(|| anyhow::anyhow!("branch scope 需要 --base"))?;
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
    let file_list: Vec<String> = files.lines().filter(|l| !l.is_empty()).map(|s| s.to_string()).collect();
    let (inline, guidance) = decide_inline(&file_list, diff.len(), &diff);
    let body = inline.unwrap_or_else(|| format!("(diff 未内联)\n改动文件:\n{}", file_list.join("\n")));
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
        assert!(guidance.contains("a.rs") && guidance.contains("自己"));
    }
    #[test]
    fn small_diff_inlines() {
        let files = vec!["a.rs".to_string()];
        let (inline, _) = decide_inline(&files, 10, "DIFF");
        assert_eq!(inline.as_deref(), Some("DIFF"));
    }
}
