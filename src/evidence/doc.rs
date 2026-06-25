/// 读取 doc 模式目标文档，拼成 {{REVIEW_INPUT}} 主体。
pub fn collect_doc(targets: &[String]) -> anyhow::Result<String> {
    let mut out = String::new();
    for t in targets {
        let text = std::fs::read_to_string(t)
            .map_err(|e| anyhow::anyhow!("failed to read doc {t}: {e}"))?;
        out.push_str(&format!("\n===== FILE: {t} =====\n{text}\n"));
    }
    if out.is_empty() {
        anyhow::bail!("doc mode requires at least one target document");
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
}
