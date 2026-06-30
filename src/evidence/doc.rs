/// 读取每个目标文档，返回 (path, content) 对——供 anchor_doc 按文件锚定引文。
pub fn collect_doc_files(targets: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    if targets.is_empty() {
        anyhow::bail!("doc mode requires at least one target document");
    }
    let mut out = Vec::new();
    for t in targets {
        let text = std::fs::read_to_string(t)
            .map_err(|e| anyhow::anyhow!("failed to read doc {t}: {e}"))?;
        out.push((t.clone(), text));
    }
    Ok(out)
}

/// 读取 doc 模式目标文档，拼成 {{REVIEW_INPUT}} 主体。
pub fn collect_doc(targets: &[String]) -> anyhow::Result<String> {
    let files = collect_doc_files(targets)?;
    let mut out = String::new();
    for (t, text) in &files {
        out.push_str(&format!("\n===== FILE: {t} =====\n{text}\n"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn doc_mode_inlines_file_contents() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("spec.md");
        std::fs::write(&p, "# Design\nhello").unwrap();
        let body = collect_doc(&[p.to_string_lossy().into()]).unwrap();
        assert!(body.contains("hello") && body.contains("spec.md"));
    }
    #[test]
    fn empty_targets_errors() {
        assert!(collect_doc(&[]).is_err());
    }

    #[test]
    fn collect_doc_files_returns_path_content_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("spec.md");
        std::fs::write(&p, "# Design\nhello").unwrap();
        let ps = p.to_string_lossy().to_string();
        let files = collect_doc_files(std::slice::from_ref(&ps)).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, ps);
        assert!(files[0].1.contains("hello"));
    }

    #[test]
    fn collect_doc_files_empty_errors() {
        assert!(collect_doc_files(&[]).is_err());
    }
}
