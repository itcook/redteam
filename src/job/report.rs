//! hibot 报告信封（`hibot.audit.report.<kind>/1`）：**结构映射**，不改内部 `ReviewReport`（P3 / D2）。
//!
//! Phase 1 只做：`findings`（补指纹）、`usage`（无 cost）、`status` 判定、`extensions.redteam` 承载内部字段。

use crate::config::{Config, OutputLang};
use crate::domain::{ReviewReport, Verdict};
use crate::job::fingerprint;
use crate::job::manifest::{JobKind, JobManifest};
use crate::job::status::{render_partial, JobFailure, JobStatus, StatusText};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Clone, Debug, Serialize)]
pub struct ReportInput {
    pub head_sha: String,
    pub base_sha: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ExecutorOut {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SourceOut {
    pub executor: String,
    pub model: String,
    pub role: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct HibotFinding {
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
    pub refuted: bool,
    pub source: SourceOut,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub extensions: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageOut {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub wall_clock_ms: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct HibotReport {
    pub schema: String,
    pub job_id: String,
    pub kind: String,
    pub status: JobStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_reason: Option<String>,
    pub input: ReportInput,
    pub executor: ExecutorOut,
    pub findings: Vec<HibotFinding>,
    /// verify 专用：Phase 1 的 error 报告也必须带（schema `report.verify` 要求该键）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifications: Option<Vec<Value>>,
    /// triage 专用：只在 `status=ok` 必需；Phase 1 的 error 报告不带。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Value>,
    pub usage: UsageOut,
    pub extensions: Value,
}

impl HibotReport {
    fn base(manifest: &JobManifest) -> Self {
        Self {
            schema: manifest.kind.report_schema().to_string(),
            job_id: manifest.job_id.clone(),
            kind: manifest.kind.as_str().to_string(),
            status: JobStatus::Ok,
            status_reason: None,
            input: ReportInput {
                head_sha: manifest.pr.head_sha.clone(),
                base_sha: manifest.pr.base_sha.clone(),
            },
            // 报告描述**实际执行单元**，不是 manifest 里的期待值（P3）。
            executor: ExecutorOut {
                name: crate::job::manifest::CLI_NAME.to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            findings: Vec::new(),
            verifications: match manifest.kind {
                JobKind::Verify => Some(Vec::new()),
                _ => None,
            },
            summary: None,
            usage: UsageOut { input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, wall_clock_ms: 0 },
            extensions: json!({}),
        }
    }
}

/// 错误 / 未实现报告：结构合法、`status=error`、带 reason。
pub fn error_report(manifest: &JobManifest, lang: &OutputLang, failure: &JobFailure) -> HibotReport {
    let text = failure.status_text(lang);
    let mut r = HibotReport::base(manifest);
    r.status = JobStatus::Error;
    r.status_reason = Some(text.reason);
    r.extensions = json!({ "redteam": { "language": language_json(lang), "status_code": text.code } });
    r
}


/// D1 + A 裁决（Phase 1）：未实现的 verify 对每个有效输入指纹输出 `inconclusive`（未核验），
/// 整体 `status=error` + `kind_not_implemented`——满足逐条覆盖，又不伪造 resolved/unresolved。
pub fn kind_not_implemented_verify(
    manifest: &JobManifest,
    lang: &OutputLang,
    fingerprints: &[String],
) -> HibotReport {
    let mut r = error_report(manifest, lang, &JobFailure::new("kind_not_implemented", Some(manifest.kind.as_str().into())));
    let evidence = if lang.value.starts_with("zh") {
        "本执行单元尚未实现 verify（Phase 1）；未对该 finding 做任何修复核验"
    } else {
        "verify is not implemented by this execution unit (Phase 1); no fix verification was performed"
    };
    r.verifications = Some(
        fingerprints
            .iter()
            .map(|fp| json!({"finding_fingerprint": fp, "status": "inconclusive", "evidence": evidence}))
            .collect(),
    );
    r
}

/// Phase 2 收集的报告期事实（不污染内部 `ReviewReport`）：只影响 hibot 信封。
#[derive(Clone, Debug, Default)]
pub struct ReviewExtras {
    /// 2.1：`diff_patch` 与 git 采集的一致性与否（只披露）。
    pub input_consistency: Option<Value>,
    /// 2.3：`prior_report` 的 routing 投影（只披露 actual 用了什么）。
    pub prior_routing: Option<Value>,
    /// 2.5：本轮因 quote 无法逐字命中而丢弃的 finding 条数（含上游锚定阶段的）。>0 时才进 extensions。
    pub dropped_unverified_quotes: u32,
    /// 2.1–2.5 之外的额外 `partial` 原因（如 `quotes_dropped` / `routing_specs_unreadable`）。
    pub extra_partial: Vec<JobFailure>,
    /// 2.4：命中 `dismissed.csv` 的 finding 审计行（指纹 → actor/reason）。
    pub ledger: std::collections::BTreeMap<String, crate::job::dismissed::Entry>,
    /// P2：job 模式 + 验真开启 + 未声明 refuter → 跳过验真的披露（`"no_refuter"`）。
    pub refute_skipped: Option<&'static str>,
}

/// review 的 `ReviewReport` → hibot 信封。
pub fn review_report(
    manifest: &JobManifest,
    internal: &ReviewReport,
    cfg: &Config,
    lang: &OutputLang,
    extras: &ReviewExtras,
) -> HibotReport {
    let (status, mut reasons) = derive_review_status(internal);
    reasons.extend(extras.extra_partial.iter().cloned());
    // 附录 E：`status_code` 只保留**主导**一条，按表从上到下优先级——不依赖拼接顺序。
    reasons.sort_by_key(|f| code_rank(&f.code));
    let partial = render_partial(&reasons, lang);
    let mut r = HibotReport::base(manifest);
    r.status = if reasons.is_empty() { status } else { JobStatus::Partial };
    r.status_reason = partial.as_ref().map(|t| t.reason.clone());
    r.findings = internal.findings.iter().map(|f| map_finding(f, &extras.ledger)).collect();
    r.usage = UsageOut {
        input_tokens: internal.run.usage.input_tokens,
        output_tokens: internal.run.usage.output_tokens,
        cached_input_tokens: internal.run.usage.cached_input_tokens,
        wall_clock_ms: internal.run.duration_ms,
    };
    let mut redteam = json!({
        "language": language_json(lang),
        "concurrency": concurrency_json(cfg),
        "budget": {
            "wall_clock_s": manifest.limits.wall_clock_s,
            "truncated_items": internal.run.deep.as_ref().map(|d| d.budget_skipped).unwrap_or(0),
        },
        "stats": internal.stats,
        "run": internal.run,
        "scan": internal.scan,
        "clusters": internal.clusters,
        "cells": internal.cells,
    });
    if let Some(t) = partial {
        redteam["status_code"] = json!(t.code);
    }
    // 2.3：prior_report 的 routing 投影（只披露实际用了什么）。
    if let Some(v) = &extras.prior_routing {
        redteam["prior_routing"] = v.clone();
    }
    // 2.1：patch 与 git 的不一致只披露，不改行为。
    if let Some(v) = &extras.input_consistency {
        redteam["input_consistency"] = v.clone();
    }
    // P2：job 模式没有显式 refuter → 跳过验真，必须显式披露（不得静默、不得塞 refuted）。
    if let Some(s) = extras.refute_skipped {
        redteam["refute_skipped"] = json!(s);
    }
    // 2.5：quote 无法逐字命中的丢弃计数（>0 才披露）。
    if extras.dropped_unverified_quotes > 0 {
        redteam["dropped_unverified_quotes"] = json!(extras.dropped_unverified_quotes);
    }
    r.extensions = json!({ "redteam": redteam });
    r
}
/// 附录 E 的 partial 优先级：`status_code` 取最靠前的一条（`render_partial` 取首条）。
fn code_rank(code: &str) -> usize {
    const ORDER: &[&str] = &[
        "scan_incomplete",
        "review_incomplete",
        "no_reviewer_available",
        "budget_truncated",
        "triage_axis_failed",
        "routing_specs_unreadable",
        "verifications_inconclusive",
        "wall_clock_exhausted",
        "quotes_dropped",
    ];
    ORDER.iter().position(|c| *c == code).unwrap_or(ORDER.len())
}

fn language_json(lang: &OutputLang) -> Value {
    json!({ "value": lang.value, "source": lang.source.as_str() })
}

fn concurrency_json(cfg: &Config) -> Value {
    let mut caps = serde_json::Map::new();
    for (name, p) in &cfg.providers {
        let cap = p.max_concurrency.unwrap_or(cfg.review.max_concurrency as u32);
        caps.insert(name.clone(), json!(cap));
    }
    json!({ "mode": "per-provider", "provider_caps": Value::Object(caps) })
}

/// 内部 finding → hibot finding（指纹在这里**唯一**生成；refuted 是顶层契约字段）。
///
/// `ledger` = 该指纹在 `dismissed.csv` 里的行：命中时挂 finding 级审计扩展，**不改 `refuted`**
/// （人的驳回不是证据证伪，Q8）。
pub fn map_finding(
    f: &crate::domain::Finding,
    ledger: &std::collections::BTreeMap<String, crate::job::dismissed::Entry>,
) -> HibotFinding {
    let fingerprint = fingerprint::of(&f.file, &f.quote, &f.title);
    let model = f.models.first().cloned().unwrap_or_default();
    let mut ext = serde_json::Map::new();
    if !matches!(f.origin, crate::domain::Origin::New) {
        ext.insert("origin".into(), json!("carried"));
    }
    if let Some(c) = &f.cluster_id {
        ext.insert("cluster_id".into(), json!(c));
    }
    if f.outside_window {
        ext.insert("outside_window".into(), json!(true));
    }
    let dismissed = ledger.get(&fingerprint);
    if let Some(e) = dismissed {
        ext.insert(
            "ledger".into(),
            serde_json::json!({ "dismissed": true, "actor": e.actor, "reason": e.reason }),
        );
    } else if let Some(r) = &f.refute {
        // 附录 D finding 级 `refute = {reason}`：复核判决正文（`refuted` 仍是顶层契约字段）。
        // 被 dismissed 的条目只留人的决定——模型原始判决留在 cell 审计里，不再往 finding 上叠。
        ext.insert("refute".into(), serde_json::json!({ "reason": r.note }));
    }
    HibotFinding {
        fingerprint,
        severity: f.severity.as_str().to_string(),
        confidence: f.confidence,
        file: f.file.clone(),
        line_start: f.line_start,
        line_end: f.line_end,
        quote: f.quote.clone(),
        title: f.title.clone(),
        body: f.body.clone(),
        recommendation: f.recommendation.clone(),
        // 人的驳回 ≠ 证据证伪：命中 ledger 的条目对外恒 refuted=false（正常与 partial 共用这一处）。
        refuted: f.is_refuted() && dismissed.is_none(),
        source: SourceOut { executor: "redteam".into(), model, role: "reviewer".into() },
        extensions: if ext.is_empty() { Value::Null } else { json!({ "redteam": Value::Object(ext) }) },
    }
}

/// triage 报告（`report.triage/1`，3.6/3.7）：`summary` 只在 `status=ok` 出现；
/// `extensions.redteam.blockers[]` 是**给 hibot 的披露面**，review 不读它（`prior.rs` 只读 routing）。
pub fn triage_report(
    manifest: &JobManifest,
    lang: &OutputLang,
    cfg: &Config,
    outcome: &crate::triage::TriageOutcome,
    template_hash: &str,
) -> HibotReport {
    let reasons = outcome.reasons.clone();
    let partial = render_partial(&reasons, lang);
    let ok = outcome.summary.is_some() && reasons.is_empty();
    let mut r = HibotReport::base(manifest);
    r.status = if ok { JobStatus::Ok } else { JobStatus::Partial };
    r.status_reason = partial.as_ref().map(|t| t.reason.clone());
    r.findings = outcome.findings.iter().map(map_triage_finding).collect();
    r.summary = if ok { outcome.summary.clone() } else { None };
    r.usage = UsageOut {
        input_tokens: outcome.usage.0,
        output_tokens: outcome.usage.1,
        cached_input_tokens: outcome.usage.2,
        wall_clock_ms: 0,
    };
    let mut redteam = json!({
        "language": language_json(lang),
        "concurrency": concurrency_json(cfg),
        "budget": { "wall_clock_s": manifest.limits.wall_clock_s, "truncated_items": 0 },
        "template_hashes": { "triage": template_hash },
        "axes": outcome.axes,
        "blockers": outcome.blockers,
    });
    if let Some(t) = partial {
        redteam["status_code"] = json!(t.code);
    }
    r.extensions = json!({ "redteam": redteam });
    r
}

/// triage 的 finding → hibot 信封（`source.role = scanner`；指纹仍走唯一实现）。
fn map_triage_finding(f: &crate::domain::Finding) -> HibotFinding {
    let fingerprint = fingerprint::of(&f.file, &f.quote, &f.title);
    HibotFinding {
        fingerprint,
        severity: f.severity.as_str().to_string(),
        confidence: f.confidence,
        file: f.file.clone(),
        line_start: f.line_start,
        line_end: f.line_end,
        quote: f.quote.clone(),
        title: f.title.clone(),
        body: f.body.clone(),
        recommendation: f.recommendation.clone(),
        refuted: false,
        source: SourceOut {
            executor: "redteam".into(),
            model: f.models.first().cloned().unwrap_or_default(),
            role: "scanner".into(),
        },
        extensions: Value::Null,
    }
}

/// P5 status 判定表（来自 coverage / RunMeta，不来自 LLM）。
pub fn derive_review_status(r: &ReviewReport) -> (JobStatus, Vec<JobFailure>) {
    let mut reasons = Vec::new();
    let failed = |role: crate::domain::Role| r.run.coverage.failed.get(&role).map(Vec::len).unwrap_or(0);
    let selected = |role: crate::domain::Role| r.run.coverage.selected.get(&role).map(Vec::len).unwrap_or(0);
    let scan_failed = failed(crate::domain::Role::Scanner);
    if scan_failed > 0 {
        reasons.push(JobFailure::new("scan_incomplete", Some(format!("{scan_failed}/{}", selected(crate::domain::Role::Scanner)))));
    }
    let rev_failed = failed(crate::domain::Role::Reviewer);
    if rev_failed > 0 {
        reasons.push(JobFailure::new("review_incomplete", Some(format!("{rev_failed}/{}", selected(crate::domain::Role::Reviewer)))));
    }
    if let Some(d) = &r.run.deep {
        if d.budget_skipped > 0 {
            reasons.push(JobFailure::new("budget_truncated", Some(d.budget_skipped.to_string())));
        }
    }
    // verdict 已经是 Incomplete 但 coverage 没给出计数（旧报告 / scan-only 边界）：仍不得报 ok。
    if reasons.is_empty() && r.verdict == Verdict::Incomplete {
        reasons.push(JobFailure::new("scan_incomplete", Some("?".into())));
    }
    if reasons.is_empty() {
        (JobStatus::Ok, reasons)
    } else {
        (JobStatus::Partial, reasons)
    }
}

/// 解析失败 / 超时时的 partial 报告（1.5）：findings 来自已完成的 cell。
///
/// Phase 2（2.5）：partial 路径与正常路径走**同一条**「重锚 → 指纹 → dismissed」链，
/// 所以同样接受 [`ReviewExtras`]（丢弃计数 / ledger / 额外 partial 原因）。
pub fn partial_report(
    manifest: &JobManifest,
    lang: &OutputLang,
    reason: &StatusText,
    findings: Vec<crate::domain::Finding>,
    usage: UsageOut,
    cfg: &Config,
    truncated_items: u32,
    extras: &ReviewExtras,
) -> HibotReport {
    // `StatusText.code` 可能带 `<detail>`：拆回 JobFailure，再与额外原因一起按附录 E 优先级排序。
    let base = reason.code.split(':').next().unwrap_or(reason.code.as_str()).to_string();
    let detail = reason.code.split_once(':').map(|(_, d)| d.to_string());
    let mut reasons = vec![JobFailure { code: base, detail, reason: Some(reason.reason.clone()) }];
    reasons.extend(extras.extra_partial.iter().cloned());
    reasons.sort_by_key(|f| code_rank(&f.code));
    let partial = render_partial(&reasons, lang).unwrap_or_else(|| reason.clone());
    let mut r = HibotReport::base(manifest);
    r.status = JobStatus::Partial;
    r.status_reason = Some(partial.reason.clone());
    r.findings = findings.iter().map(|f| map_finding(f, &extras.ledger)).collect();
    r.usage = usage;
    let mut redteam = json!({
        "language": language_json(lang),
        "status_code": partial.code,
        "concurrency": concurrency_json(cfg),
        "budget": { "wall_clock_s": manifest.limits.wall_clock_s, "truncated_items": truncated_items },
    });
    if let Some(s) = extras.refute_skipped {
        redteam["refute_skipped"] = json!(s);
    }
    // 中断报告与正常报告共享同一份运行事实（2.3 / 2.1）。
    if let Some(v) = &extras.prior_routing {
        redteam["prior_routing"] = v.clone();
    }
    if let Some(v) = &extras.input_consistency {
        redteam["input_consistency"] = v.clone();
    }
    if extras.dropped_unverified_quotes > 0 {
        redteam["dropped_unverified_quotes"] = json!(extras.dropped_unverified_quotes);
    }
    r.extensions = json!({ "redteam": redteam });
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Coverage, Finding, Origin, Refute, RefuteVerdict, Role, RunMeta, Severity, Stats};

    fn manifest() -> JobManifest {
        JobManifest::parse(r#"{
          "schema":"hibot.audit.job.review/1","job_id":"J1","kind":"review",
          "repo":{"id":1,"full_name":"o/r"},
          "pr":{"number":7,"head_sha":"abcdef0","base_sha":"1234567"},
          "executor":{"name":"redteam","version":"0.1.0"},
          "limits":{"wall_clock_s":600},
          "paths":{"repo":"../repo","out":"out","config":"config/executor.redteam.json"},
          "inputs":{}
        }"#).unwrap()
    }

    fn finding() -> Finding {
        Finding {
            id: "x".into(), severity: Severity::High, title: "SQL 注入".into(),
            body: "b".into(), file: "src/db.ts".into(), line_start: 44, line_end: 46,
            confidence: 0.9, recommendation: "参数化".into(), models: vec!["p/m".into()],
            agreement: 1, suspicion_agreement: 0, hard_constraint: false,
            quote: "db.query(sql)".into(), previously_dismissed: false,
            origin: Origin::New, outside_window: false, cluster_id: None, refute: None,
        }
    }

    fn report(verdict: Verdict, findings: Vec<Finding>, cov: Coverage, deep: Option<crate::domain::DeepMeta>) -> ReviewReport {
        ReviewReport {
            schema_version: "4".into(),
            verdict,
            stats: Stats::from_findings(&findings),
            findings,
            clusters: vec![],
            run: RunMeta {
                job_id: "j".into(), coverage: cov, deep,
                usage: crate::domain::Usage { input_tokens: 10, output_tokens: 2, cached_input_tokens: 1, refute: None },
                duration_ms: 1234,
                ..Default::default()
            },
            scan: None,
            cells: vec![],
        }
    }

    #[test]
    fn maps_findings_with_contract_fingerprint_and_refuted_flag() {
        let mut f = finding();
        f.refute = Some(Refute { verdict: RefuteVerdict::Refuted, model: "p/r".into(), note: "".into() });
        let m = manifest();
        let rep = report(Verdict::Approve, vec![f.clone()], Coverage::default(), None);
        let out = review_report(
            &m,
            &rep,
            &Config::default(),
            &crate::config::Language::EnUs.output(),
            &ReviewExtras::default(),
        );
        let hf = &out.findings[0];
        assert_eq!(hf.fingerprint, crate::job::fingerprint::of(&f.file, &f.quote, &f.title));
        assert!(hf.refuted, "refuted 是顶层契约字段");
        assert_eq!(hf.source.role, "reviewer");
        assert_eq!(out.usage.wall_clock_ms, 1234);
        assert_eq!(out.schema, "hibot.audit.report.review/1");
        // P3：verdict 丢弃；executor 描述实际执行单元（不是 manifest 的期待版本）。
        assert!(out.extensions["redteam"].get("verdict").is_none(), "verdict 必须丢弃");
        assert_eq!(out.executor.name, "redteam");
        assert_eq!(out.executor.version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn incomplete_scan_maps_to_partial_with_code() {
        let mut cov = Coverage::default();
        cov.selected.insert(Role::Scanner, vec!["a".into(), "b".into(), "c".into()]);
        cov.failed.insert(Role::Scanner, vec!["b".into()]);
        let rep = report(Verdict::Incomplete, vec![], cov, None);
        let (st, reasons) = derive_review_status(&rep);
        assert_eq!(st, JobStatus::Partial);
        assert_eq!(reasons[0].full_code(), "scan_incomplete:1/3");
    }

    #[test]
    fn budget_truncation_and_review_failures_are_partial_but_clean_is_ok() {
        let mut cov = Coverage::default();
        cov.selected.insert(Role::Reviewer, vec!["r".into()]);
        cov.failed.insert(Role::Reviewer, vec!["r".into()]);
        let rep = report(Verdict::Approve, vec![], cov, Some(crate::domain::DeepMeta { budget_skipped: 2, ..Default::default() }));
        let (st, reasons) = derive_review_status(&rep);
        assert_eq!(st, JobStatus::Partial);
        assert_eq!(reasons.iter().map(|r| r.code.as_str()).collect::<Vec<_>>(), ["review_incomplete", "budget_truncated"]);
        let clean = report(Verdict::Approve, vec![], Coverage::default(), None);
        assert_eq!(derive_review_status(&clean).0, JobStatus::Ok);
        // 反向断言：空 findings 不决定状态（ok 可以有 0 条；incomplete 不能因为 0 条就变 ok）。
        let empty_incomplete = report(Verdict::Incomplete, vec![], Coverage::default(), None);
        assert_eq!(derive_review_status(&empty_incomplete).0, JobStatus::Partial);
    }

    #[test]
    fn error_and_kind_not_implemented_reports_are_schema_shaped() {
        let m = manifest();
        let lang = crate::config::Language::ZhCn.output();
        let e = error_report(&m, &lang, &JobFailure::new("input_missing", Some("diff_patch".into())));
        assert_eq!((e.status, e.status_reason.as_deref()), (JobStatus::Error, Some("缺少必需输入 `diff_patch`")));
        assert_eq!(e.extensions["redteam"]["status_code"], "input_missing:diff_patch");
        assert_eq!(e.extensions["redteam"]["language"]["value"], "zh_CN");

        let vm = JobManifest::parse(r#"{
          "schema":"hibot.audit.job.verify/1","job_id":"J2","kind":"verify",
          "repo":{"id":1,"full_name":"o/r"},
          "pr":{"number":7,"head_sha":"abcdef0","base_sha":"1234567"},
          "executor":{"name":"redteam","version":"0.1.0"},
          "limits":{"wall_clock_s":600},
          "paths":{"repo":"../repo","out":"out","config":"config/executor.redteam.json"},
          "inputs":{}
        }"#).unwrap();
        let v = kind_not_implemented_verify(&vm, &lang, &[]);
        assert_eq!(v.kind, "verify");
        assert_eq!(v.extensions["redteam"]["status_code"], "kind_not_implemented:verify");
        assert_eq!(v.verifications.as_deref(), Some(&[][..]), "verify 报告必须带 verifications（空也可）");
        assert!(v.findings.is_empty(), "禁止伪造 findings");
    }
}
