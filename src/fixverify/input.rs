//! verify 的输入解析：`findings.json`（逐条核验清单）与 fix diff 的变更形态。
//!
//! 契约要点（`~/hibot/.docs/audit/contract.md` §1.2 / §3.3）：
//! - `findings` 是 `hibot.audit.findings/1`（finding 数组），**逐条覆盖**它的指纹集合；
//! - 指纹形如 `sha256:<64 lowercase hex>`，格式非法是**输入错误**（`invalid_fingerprint_input`），
//!   不是「这一条没核验」（计划 4.1）；
//! - 集合为空是合法的（唯一允许空 `verifications` 的情形，Q6/B5）；
//! - fix diff 里删除 / 改名要靠 patch 自身的标记解析（删除 → 构造可能整体消失；改名 → 新路径重定位）。

use crate::evidence::git::FileChange;
use crate::job::status::JobFailure;
use serde::Deserialize;
use std::collections::BTreeMap;

/// `findings.json` 的一条（`hibot.audit.finding/1` 的必填字段）。
///
/// 未知字段忽略（契约 §2 新增字段不 bump）；缺必填字段 → `input_schema_mismatch:findings`。
#[derive(Clone, Debug, Deserialize)]
pub struct InputFinding {
    pub fingerprint: String,
    pub severity: String,
    pub confidence: f32,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub quote: String,
    pub title: String,
    pub body: String,
    pub recommendation: String,
}

impl InputFinding {
    /// 行跨度归一：`line_end` 至少等于 `line_start`，两者至少 1。
    pub fn span(&self) -> (u32, u32) {
        let s = self.line_start.max(1);
        (s, self.line_end.max(s))
    }
}

fn findings_schema_error() -> JobFailure {
    JobFailure::new("input_schema_mismatch", Some("findings".into()))
}

/// 解析并**按指纹去重**（集合语义：同一指纹重复出现只核验一次）。
///
/// 返回 `(findings, duplicates)`。指纹缺失或形状非法 → `invalid_fingerprint_input`（错误，不逐条降级）。
pub fn parse_findings(text: &str) -> Result<(Vec<InputFinding>, usize), JobFailure> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| findings_schema_error())?;
    let items = value.as_array().ok_or_else(findings_schema_error)?;
    let mut out: Vec<InputFinding> = Vec::with_capacity(items.len());
    let mut seen = std::collections::BTreeSet::new();
    let mut duplicates = 0usize;
    for item in items {
        let fp = item
            .get("fingerprint")
            .and_then(|v| v.as_str())
            .ok_or_else(|| JobFailure::new("invalid_fingerprint_input", None))?;
        if !crate::job::fingerprint::is_fingerprint(fp) {
            return Err(JobFailure::new("invalid_fingerprint_input", None));
        }
        let finding: InputFinding = serde_json::from_value(item.clone()).map_err(|_| findings_schema_error())?;
        if !seen.insert(fp.to_string()) {
            duplicates += 1;
            continue;
        }
        out.push(finding);
    }
    Ok((out, duplicates))
}

/// fix diff 中一个文件的变更形态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FixChange {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl FixChange {
    pub fn as_str(self) -> &'static str {
        match self {
            FixChange::Added => "added",
            FixChange::Modified => "modified",
            FixChange::Deleted => "deleted",
            FixChange::Renamed => "renamed",
        }
    }
}

/// `FileChange`（patch 切分产物）→ 带变更形态的视图。
#[derive(Clone, Debug)]
pub struct FileFix {
    /// b 侧（post-fix）路径。
    pub path: String,
    /// 改名前的路径（仅 [`FixChange::Renamed`]）。
    pub old_path: Option<String>,
    pub kind: FixChange,
    pub diff: String,
}

/// 从 patch 的每个文件段解析变更形态。
///
/// git 的标记都在段头：`rename from <old>` / `rename to <new>` / `deleted file mode` / `new file mode`。
/// 改名段的 b 侧路径已是新路径（`split_diff_by_file` 取的是 `b/` 侧），这里补出旧路径。
pub fn parse_changes(changes: &[FileChange]) -> Vec<FileFix> {
    changes
        .iter()
        .map(|c| {
            let rename_from = header_value(&c.diff, "rename from ");
            let kind = if rename_from.is_some() {
                FixChange::Renamed
            } else if c.diff.lines().any(|l| l.starts_with("deleted file mode")) {
                FixChange::Deleted
            } else if c.diff.lines().any(|l| l.starts_with("new file mode")) {
                FixChange::Added
            } else {
                FixChange::Modified
            };
            FileFix { path: c.path.clone(), old_path: rename_from, kind, diff: c.diff.clone() }
        })
        .collect()
}

/// 段头里 `<prefix><value>` 的值（路径可能含空格 → 只裁掉行尾空白）。
fn header_value(diff: &str, prefix: &str) -> Option<String> {
    diff.lines()
        .find_map(|l| l.strip_prefix(prefix))
        .map(|v| v.trim_end().to_string())
        .filter(|v| !v.is_empty())
}

/// 找到 finding 对应的文件段：优先 post-fix 路径，其次改名前的旧路径（4.3）。
pub fn locate<'a>(fixes: &'a [FileFix], file: &str) -> Option<&'a FileFix> {
    fixes
        .iter()
        .find(|f| f.path == file)
        .or_else(|| fixes.iter().find(|f| f.old_path.as_deref() == Some(file)))
}

/// `prior_report`（`report.review/1`）里与本轮 finding 同指纹的**佐证**（不是结论）。
///
/// 只留该指纹那一条：verify 的 prior_report 是「上次评审怎么说」，不是重跑评审的素材。
#[derive(Clone, Debug, Default)]
pub struct PriorEvidence {
    entries: BTreeMap<String, String>,
}

impl PriorEvidence {
    pub fn get(&self, fingerprint: &str) -> Option<&str> {
        self.entries.get(fingerprint).map(String::as_str)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

const PRIOR_FIELD_CAP: usize = 600;

/// 解析 `prior_report.findings[]`，按指纹建索引。
///
/// 形态不合法（不是对象 / `findings` 不是数组）→ `input_schema_mismatch:prior_report`：
/// 可选输入不等于「坏了也能用」。
pub fn parse_prior(text: &str) -> Result<PriorEvidence, JobFailure> {
    let schema_err = || JobFailure::new("input_schema_mismatch", Some("prior_report".into()));
    let value: serde_json::Value = serde_json::from_str(text).map_err(|_| schema_err())?;
    let obj = value.as_object().ok_or_else(schema_err)?;
    let items = obj.get("findings").and_then(|v| v.as_array()).ok_or_else(schema_err)?;
    let mut entries = BTreeMap::new();
    for f in items {
        let Some(fp) = f.get("fingerprint").and_then(|v| v.as_str()) else { continue };
        if !crate::job::fingerprint::is_fingerprint(fp) {
            continue;
        }
        entries.insert(fp.to_string(), render_entry(f));
    }
    Ok(PriorEvidence { entries })
}

fn field(f: &serde_json::Value, key: &str) -> String {
    let raw = f.get(key).and_then(|v| v.as_str()).unwrap_or("").trim();
    take_chars(raw, PRIOR_FIELD_CAP)
}

fn take_chars(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let mut out: String = s.chars().take(cap).collect();
    out.push('…');
    out
}

fn render_entry(f: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str(&format!("- title: {}\n", field(f, "title")));
    out.push_str(&format!("- severity: {}\n", field(f, "severity")));
    out.push_str(&format!("- quote: {}\n", field(f, "quote")));
    if let Some(body) = f.get("body").and_then(|v| v.as_str()).filter(|b| !b.trim().is_empty()) {
        out.push_str(&format!("- body: {}\n", take_chars(body.trim(), PRIOR_FIELD_CAP)));
    }
    if let Some(rec) = f.get("recommendation").and_then(|v| v.as_str()).filter(|r| !r.trim().is_empty()) {
        out.push_str(&format!("- recommendation: {}\n", take_chars(rec.trim(), PRIOR_FIELD_CAP)));
    }
    if f.get("refuted").and_then(|v| v.as_bool()).unwrap_or(false) {
        out.push_str("- refuted by the prior review: true\n");
    }
    if let Some(reason) = f
        .get("extensions")
        .and_then(|e| e.get("redteam"))
        .and_then(|r| r.get("refute"))
        .and_then(|r| r.get("reason"))
        .and_then(|v| v.as_str())
    {
        out.push_str(&format!("- refute reason: {}\n", take_chars(reason.trim(), PRIOR_FIELD_CAP)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(path: &str, diff: &str) -> FileChange {
        FileChange { path: path.into(), diff: diff.into() }
    }

    fn fp(n: char) -> String {
        format!("sha256:{}", n.to_string().repeat(64))
    }

    fn finding(fp: &str, quote: &str) -> serde_json::Value {
        serde_json::json!({
            "fingerprint": fp, "severity": "high", "confidence": 0.9,
            "file": "a.rs", "line_start": 1, "line_end": 2,
            "quote": quote, "title": "t", "body": "b", "recommendation": "r",
        })
    }

    #[test]
    fn findings_are_deduped_by_fingerprint_and_bad_shapes_are_errors() {
        let text = serde_json::json!([finding(&fp('a'), "q"), finding(&fp('a'), "q")]).to_string();
        let (parsed, dup) = parse_findings(&text).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(dup, 1);

        assert_eq!(parse_findings("[{\"fingerprint\":\"sha256:abc\"}]").unwrap_err().code, "invalid_fingerprint_input");
        assert_eq!(parse_findings("[{\"severity\":\"high\"}]").unwrap_err().code, "invalid_fingerprint_input");
        assert_eq!(parse_findings("not json").unwrap_err().code, "input_schema_mismatch");
        assert_eq!(parse_findings("{}").unwrap_err().code, "input_schema_mismatch");
        // 指纹合法但缺必填字段 → 输入 schema 错误。
        assert_eq!(parse_findings(&serde_json::json!([{"fingerprint": fp('b')}]).to_string()).unwrap_err().code, "input_schema_mismatch");
        assert!(parse_findings("[]").unwrap().0.is_empty(), "空集合合法（Q6/B5）");
    }

    #[test]
    fn change_kinds_come_from_patch_markers() {
        let fixes = parse_changes(&[
            change("new.rs", "diff --git a/old.rs b/new.rs\nrename from old.rs\nrename to new.rs\n"),
            change("gone.rs", "diff --git a/gone.rs b/gone.rs\ndeleted file mode 100644\n--- a/gone.rs\n+++ /dev/null\n"),
            change("added.rs", "diff --git a/added.rs b/added.rs\nnew file mode 100644\n"),
            change("mod.rs", "diff --git a/mod.rs b/mod.rs\n@@ -1 +1 @@\n-a\n+b\n"),
        ]);
        assert_eq!(fixes[0].kind, FixChange::Renamed);
        assert_eq!(fixes[0].old_path.as_deref(), Some("old.rs"));
        assert_eq!(fixes[0].path, "new.rs");
        assert_eq!(fixes[1].kind, FixChange::Deleted);
        assert_eq!(fixes[2].kind, FixChange::Added);
        assert_eq!(fixes[3].kind, FixChange::Modified);
        assert!(fixes[3].old_path.is_none());
    }

    #[test]
    fn location_follows_the_rename_to_the_new_path() {
        let fixes = parse_changes(&[change("src/new.rs", "diff --git a/src/old.rs b/src/new.rs\nrename from src/old.rs\nrename to src/new.rs\n")]);
        assert_eq!(locate(&fixes, "src/old.rs").map(|f| f.path.as_str()), Some("src/new.rs"));
        assert_eq!(locate(&fixes, "src/new.rs").map(|f| f.path.as_str()), Some("src/new.rs"));
        assert!(locate(&fixes, "src/other.rs").is_none());
    }

    #[test]
    fn prior_report_only_keeps_the_matching_fingerprint() {
        let text = serde_json::json!({
            "schema": "hibot.audit.report.review/1",
            "findings": [
                {"fingerprint": fp('a'), "title": "A", "quote": "qa", "body": "body a", "severity": "high"},
                {"fingerprint": fp('b'), "title": "B", "quote": "qb", "refuted": true,
                 "extensions": {"redteam": {"refute": {"reason": "not reachable"}}}},
            ],
        })
        .to_string();
        let prior = parse_prior(&text).unwrap();
        assert_eq!(prior.len(), 2);
        assert!(prior.get(&fp('a')).unwrap().contains("body a"));
        let b = prior.get(&fp('b')).unwrap();
        assert!(b.contains("refuted by the prior review: true") && b.contains("not reachable"), "{b}");
        assert!(prior.get("sha256:dead").is_none());
        assert_eq!(parse_prior("[]").unwrap_err().code, "input_schema_mismatch");
        assert_eq!(parse_prior("{\"findings\":{}}").unwrap_err().detail.as_deref(), Some("prior_report"));
    }
}
