//! 3.1 `inputs.pr_context`（`hibot.audit.pr-context/1`）的消费视图。
//!
//! `comments[]` 只含非 hibot 评论、`is_bot` 必填（Q9）：本模块**不**自行排除 hibot，
//! 也不把 bot 评论当作者意图；`is_bot` 只在重复判定时用于排除 bot。

use crate::job::status::JobFailure;
use serde::Deserialize;

pub const PR_CONTEXT_SCHEMA: &str = "hibot.audit.pr-context/1";

#[derive(Clone, Debug, Deserialize)]
pub struct PrContext {
    pub schema: String,
    pub title: String,
    pub body: String,
    pub author: String,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub linked_issues: Vec<LinkedIssue>,
    pub ci: Ci,
    pub changed_files: Vec<ChangedFile>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Comment {
    pub author: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub created_at: String,
    /// 必填（契约 schema）；缺失 → `input_schema_mismatch:pr_context`。
    pub is_bot: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LinkedIssue {
    pub number: u64,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Ci {
    pub state: String,
    #[serde(default)]
    pub failing: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    #[serde(default)]
    pub status: Option<String>,
}

/// 解析 + 最小语义校验：schema 必须是 `pr-context/1`；`author` / `changed_files[].path` 非空。
pub fn parse(text: &str) -> Result<PrContext, JobFailure> {
    let pr: PrContext = serde_json::from_str(text)
        .map_err(|_| JobFailure::new("input_schema_mismatch", Some("pr_context".into())))?;
    if pr.schema != PR_CONTEXT_SCHEMA || pr.author.trim().is_empty() {
        return Err(JobFailure::new("input_schema_mismatch", Some("pr_context".into())));
    }
    if pr.changed_files.iter().any(|f| f.path.trim().is_empty()) {
        return Err(JobFailure::new("input_schema_mismatch", Some("pr_context".into())));
    }
    Ok(pr)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal() -> String {
        r#"{
          "schema": "hibot.audit.pr-context/1",
          "title": "t", "body": "b", "author": "alice",
          "labels": [], "comments": [{"author":"bob","body":"x","created_at":"2026-01-01T00:00:00Z","is_bot":false}],
          "linked_issues": [],
          "ci": {"state":"success","failing":[]},
          "changed_files": [{"path":"src/a.rs","additions":1,"deletions":0,"status":"modified"}]
        }"#
        .to_string()
    }

    #[test]
    fn parses_and_keeps_is_bot() {
        let pr = parse(&minimal()).unwrap();
        assert_eq!(pr.comments[0].is_bot, false);
        assert_eq!(pr.changed_files[0].path, "src/a.rs");
    }

    #[test]
    fn is_bot_is_required_and_schema_is_enforced() {
        let no_bot = minimal().replace(r#","is_bot":false"#, "");
        assert_eq!(parse(&no_bot).unwrap_err().full_code(), "input_schema_mismatch:pr_context");
        let wrong_schema = minimal().replace("pr-context/1", "pr-context/2");
        assert_eq!(parse(&wrong_schema).unwrap_err().full_code(), "input_schema_mismatch:pr_context");
    }

    #[test]
    fn empty_path_or_author_is_a_schema_error() {
        assert_eq!(
            parse(&minimal().replace(r#""author": "alice""#, r#""author": " ""#)).unwrap_err().full_code(),
            "input_schema_mismatch:pr_context"
        );
        assert_eq!(
            parse(&minimal().replace(r#""path":"src/a.rs""#, r#""path":"""#)).unwrap_err().full_code(),
            "input_schema_mismatch:pr_context"
        );
    }
}
