//! job 失败与状态文案：`status_reason`（人读、随 `language`）+ `status_code`（机器、附录 E）。
//!
//! 契约要点：`partial` / `error` 必带 `status_reason`；`status_code` 只保留**主导**一条。

use crate::config::OutputLang;
use serde::Serialize;

/// 报告状态（`hibot.audit.report.*`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Ok,
    Partial,
    Error,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Ok => "ok",
            JobStatus::Partial => "partial",
            JobStatus::Error => "error",
        }
    }
}

/// 一条失败/降级原因：`code` 是不带参数的基础码（附录 E），`detail` 填 `<...>` 部分。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobFailure {
    /// 基础码，如 `input_missing` / `config_invalid`。
    pub code: String,
    /// `<...>` 参数（附录 E 的 `<key>` / `<pointer>` 等）。
    pub detail: Option<String>,
    /// 人读消息覆盖：用于"码是 A、但 reason 要保留原始诊断"的场景（如 backend 构造失败）。
    pub reason: Option<String>,
}

impl JobFailure {
    pub fn new(code: impl Into<String>, detail: Option<String>) -> Self {
        Self { code: code.into(), detail, reason: None }
    }

    /// 机器码与参数照旧，`status_reason` 用给定消息（例如 executor config 的原始报错）。
    pub fn with_reason(code: impl Into<String>, detail: Option<String>, reason: impl Into<String>) -> Self {
        Self { code: code.into(), detail, reason: Some(reason.into()) }
    }
    /// 完整机器码：**只有附录 E 里带 `<...>` 的码**才追加参数；任意诊断全文不进机器码。
    pub fn full_code(&self) -> String {
        match &self.detail {
            Some(d) if !d.is_empty() && takes_detail(&self.code) => format!("{}:{}", self.code, sanitize_detail(d)),
            _ => self.code.clone(),
        }
    }
}

impl std::fmt::Display for JobFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.full_code())
    }
}

/// `status_reason` + `extensions.redteam.status_code`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusText {
    pub code: String,
    pub reason: String,
}

/// 附录 E 表：`(基础码, zh, en)`；`{}` 由 `detail` 填充。
fn table(code: &str) -> Option<(&'static str, &'static str)> {
    Some(match code {
        // error
        "job_manifest_unreadable" => ("无法读取或解析 job.json", "cannot read or parse job.json"),
        "job_manifest_unsupported" => ("job.json 的 schema 或 kind 不受支持", "unsupported job schema or kind"),
        "kind_mismatch" => ("命令 kind 与 job.json 不一致", "CLI kind does not match job.json"),
        "input_missing" => ("缺少必需输入 `{}`", "missing required input `{}`"),
        "input_checksum_mismatch" => ("输入 `{}` 的 sha256 不匹配", "sha256 mismatch for input `{}`"),
        "input_schema_mismatch" => ("输入 `{}` 的 schema 或 format 不符合预期", "input `{}` has an unexpected schema or format"),
        "input_path_invalid" => ("输入 `{}` 的路径越界或不存在", "input `{}` path escapes the job dir or is absent"),
        "config_unreadable" => ("无法读取 executor config", "cannot read executor config"),
        "config_invalid" => ("executor config 不合法：`{}`", "invalid executor config: `{}`"),
        "config_contains_secret" => (
            "executor config 含敏感字段 `{}`；凭据只走环境变量",
            "executor config contains secret field `{}`; supply credentials via env vars",
        ),
        "head_sha_mismatch" => ("检出 HEAD 与 manifest 不一致", "checkout HEAD differs from manifest"),
        "invalid_fingerprint_input" => ("findings.json 中有非法指纹", "findings.json contains an invalid fingerprint"),
        "missing_role" => ("缺少 `{}` 角色且无可用回退", "missing role `{}` with no fallback"),
        "internal_panic" => ("内部错误：{}", "internal error: {}"),
        "kind_not_implemented" => (
            "kind `{}` 在本执行单元中尚未实现（Phase 1 只有 review）",
            "kind `{}` is not implemented by this executor yet (Phase 1 ships review only)",
        ),
        // partial
        "scan_incomplete" => ("扫描不完整：{} 个 cell 失败", "scan incomplete: {} cells failed"),
        "review_incomplete" => ("深挖不完整：{} 个 cell 失败", "review incomplete: {} cells failed"),
        "no_reviewer_available" => ("无可用 reviewer 条目", "no reviewer entry available"),
        "budget_truncated" => ("预算不足，{} 个条目未挖", "budget exhausted; {} items skipped"),
        "triage_axis_failed" => ("triage 的 `{}` 轴未完成", "triage axis `{}` incomplete"),
        "routing_specs_unreadable" => ("指定的 {} 份规范文档不可读", "{} routing spec files unreadable"),
        "verifications_inconclusive" => ("{} 条 finding 未能核验（inconclusive）", "{} findings inconclusive"),
        "wall_clock_exhausted" => ("达到墙钟预算或收到终止信号，已收敛落盘", "wall-clock budget exhausted or terminated; results flushed"),
        "quotes_dropped" => ("{} 条 finding 的 quote 未逐字命中，已丢弃", "{} findings dropped: quote not found verbatim"),
        // partial 的“计数”形状（scan_incomplete: f/t）复用 detail 字符串
        _ => return None,
    })
}

fn render_one(code: &str, detail: Option<&str>, lang: &OutputLang) -> StatusText {
    let (zh, en) = table(code).unwrap_or(("未能完成（{}）", "incomplete ({})"));
    let template = if lang.value.starts_with("zh") { zh } else { en };
    let reason = match detail {
        Some(d) if template.contains("{}") => template.replacen("{}", d, 1),
        _ => template.replace("{}", "").trim().to_string(),
    };
    let code = match detail {
        Some(d) if !d.is_empty() && takes_detail(code) => format!("{code}:{}", sanitize_detail(d)),
        _ => code.to_string(),
    };
    StatusText { code, reason }
}

/// 附录 E 里带参数的码；其余码（如 `internal_panic` / `config_unreadable`）不把诊断全文拼进 `status_code`。
fn takes_detail(code: &str) -> bool {
    matches!(
        code,
        "input_missing" | "input_checksum_mismatch" | "input_schema_mismatch" | "input_path_invalid"
            | "config_invalid" | "config_contains_secret" | "missing_role" | "kind_not_implemented"
            | "scan_incomplete" | "review_incomplete" | "budget_truncated" | "triage_axis_failed"
            | "routing_specs_unreadable" | "verifications_inconclusive" | "quotes_dropped"
    )
}

/// `status_code` 只允许一行短参数（JSON Pointer / key / 计数）；空白折叠、截断。
fn sanitize_detail(d: &str) -> String {
    let one = d.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() <= 120 { one } else { one.chars().take(120).collect() }
}

impl JobFailure {
    /// 转成 `error` 报告用的 `status_reason` / `status_code`。
    pub fn status_text(&self, lang: &OutputLang) -> StatusText {
        let mut t = render_one(&self.code, self.detail.as_deref(), lang);
        if let Some(r) = &self.reason {
            t.reason = r.clone();
        }
        t
    }
}

/// 多条 partial 原因：reason 全部输出（`; ` 连接，≤512 字符），code 取**第一条**（主导码）。
pub fn render_partial(entries: &[JobFailure], lang: &OutputLang) -> Option<StatusText> {
    if entries.is_empty() {
        return None;
    }
    let first = entries[0].status_text(lang);
    let mut reason = String::new();
    for e in entries {
        let r = e.status_text(lang).reason;
        if !reason.is_empty() {
            reason.push_str("; ");
        }
        if reason.chars().count() + r.chars().count() > 512 {
            break;
        }
        if !r.is_empty() {
            reason.push_str(&r);
        }
    }
    Some(StatusText { code: first.code, reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Language, LangSource};

    fn zh() -> OutputLang { Language::ZhCn.output() }
    fn en() -> OutputLang { Language::EnUs.output() }

    #[test]
    fn full_code_includes_detail() {
        let f = JobFailure::new("input_missing", Some("diff_patch".into()));
        assert_eq!(f.full_code(), "input_missing:diff_patch");
        let g = JobFailure::new("config_unreadable", None);
        assert_eq!(g.full_code(), "config_unreadable");
    }

    #[test]
    fn error_reason_follows_language_and_carries_code() {
        let f = JobFailure::new("input_missing", Some("diff_patch".into()));
        let z = f.status_text(&zh());
        assert_eq!(z.code, "input_missing:diff_patch");
        assert!(z.reason.contains("缺少必需输入") && z.reason.contains("diff_patch"), "{z:?}");
        let e = f.status_text(&en());
        assert!(e.reason.contains("missing required input") && e.reason.contains("diff_patch"), "{e:?}");
    }

    #[test]
    fn partial_chain_keeps_all_reasons_and_dominant_code() {
        let entries = vec![
            JobFailure::new("scan_incomplete", Some("2/5".into())),
            JobFailure::new("budget_truncated", Some("3".into())),
        ];
        let t = render_partial(&entries, &en()).unwrap();
        assert_eq!(t.code, "scan_incomplete:2/5");
        assert!(t.reason.contains("2/5") && t.reason.contains("3 items skipped"), "{t:?}");
        assert!(render_partial(&[], &en()).is_none());
    }

    #[test]
    fn unknown_code_falls_back_without_panicking() {
        let t = JobFailure::new("mystery", Some("x".into())).status_text(&en());
        assert_eq!(t.code, "mystery", "未知码不拼诊断参数");
        assert!(t.reason.contains('x'));
        assert_eq!(JobFailure::new("internal_panic", Some("boom\nmore".into())).full_code(), "internal_panic");
        assert_eq!(JobFailure::new("input_missing", Some("diff_patch".into())).full_code(), "input_missing:diff_patch");
        let _ = LangSource::Locale.as_str();
    }
}
