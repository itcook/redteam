//! 3.4 去重 + 3.5 确定性 tally（单点实现；`blockers = structural + intent_undelivered`）。
//!
//! 输入是**已锚定**的轴行：quote 换成检出原文、行号取真实范围；`local` 漂移不进 findings，
//! 只计数。任何字段生成规则只在这里实现一次，报告层不重算。

use crate::domain::{Finding, Origin, Severity};
use crate::triage::input::PrContext;
use crate::triage::lenses::{tier2_surface, DriftRow, IntentOutput, MaintainerOverlap, SecurityRow};
use crate::triage::standards::{self, StandardsMap};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// 丢弃原因计数（进 `extensions.redteam.axes[].dropped`，不静默）。
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct AxisDrops {
    pub below_threshold: u32,
    pub no_reach: u32,
    pub off_patch: u32,
    pub unanchored: u32,
    pub invalid: u32,
    pub duplicate: u32,
    pub invalid_claim_source: u32,
    pub overlap_unverified: u32,
    /// 该轴全部丢弃数（`dropped_rows` 只留前 20 条样本）。
    pub dropped_total: u32,
    pub dropped_rows: Vec<DroppedRow>,
}

/// 被丢行的诊断样本（协调者 2026-11-24 裁 A：不计入契约必需字段）。
#[derive(Clone, Debug, serde::Serialize)]
pub struct DroppedRow {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conf: Option<u8>,
    pub reason: String,
}

impl AxisDrops {
    /// 记一次丢弃；`row` = 诊断样本（超出 20 条只增计数）。
    pub fn note(&mut self, reason: &str, row: DroppedRow) {
        self.dropped_total += 1;
        match reason {
            "below_threshold" => self.below_threshold += 1,
            "no_reach" => self.no_reach += 1,
            "off_patch" => self.off_patch += 1,
            "unanchored" => self.unanchored += 1,
            "duplicate" => self.duplicate += 1,
            "invalid_claim_source" => self.invalid_claim_source += 1,
            "overlap_unverified" => self.overlap_unverified += 1,
            _ => self.invalid += 1,
        }
        if self.dropped_rows.len() < 20 {
            self.dropped_rows.push(row);
        }
    }
}

/// 按轴归属的丢弃计数（不做跨轴汇总：某个轴丢弃不能挂到其它轴）。
#[derive(Clone, Debug, Default)]
pub struct Drops {
    pub security: AxisDrops,
    pub drift: AxisDrops,
    pub intent: AxisDrops,
}

#[derive(Clone, Debug)]
pub struct AnchoredSecurity {
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub quote: String,
    pub surface: u8,
    pub confidence: u8,
    pub reach: String,
    pub note: String,
}

impl AnchoredSecurity {
    pub fn tier2(&self) -> bool {
        tier2_surface(self.surface) && self.confidence >= 8
    }
}

#[derive(Clone, Debug)]
pub struct AnchoredDrift {
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub quote: String,
    pub drift: DriftKind,
    pub category: String,
    pub standard: String,
    pub note: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftKind {
    Structural,
    Local,
}

#[derive(Clone, Debug)]
pub struct ClaimRow {
    pub claim: String,
    pub source: String,
    pub delivered: bool,
    pub note: String,
    /// slice 全成（或该 claim 已有交付证据）才为 true；false 时不计入 undelivered/blockers。
    pub verified: bool,
}

#[derive(Clone, Debug)]
pub struct ScopeCreepRow {
    pub title: String,
    pub files: Vec<String>,
    pub note: String,
}

#[derive(Clone, Debug, Default)]
pub struct Prepared {
    pub security: Vec<AnchoredSecurity>,
    pub drift: Vec<AnchoredDrift>,
    pub claims: Vec<ClaimRow>,
    pub scope_creep: Vec<ScopeCreepRow>,
    pub overlap: MaintainerOverlap,
    pub drops: Drops,
}

/// 锚定 + 丢弃 + 按轴去重。`changed` = `diff_patch` 里的文件集（真相源）。
pub fn prepare(
    root: &Path,
    pr: &PrContext,
    changed: &BTreeSet<String>,
    security: &[SecurityRow],
    drift: &[DriftRow],
    intent: Option<&IntentOutput>,
) -> Prepared {
    let mut out = Prepared::default();

    // ── 安全：<8 丢弃；无 source→sink 路径封顶 7（即丢弃）；file 必须在 patch 里；逐字重锚 ──
    let sample = |file: &str, line: u32, surface: Option<u8>, conf: Option<u8>, reason: &str| DroppedRow {
        file: Some(file.to_string()),
        line: Some(line),
        surface,
        conf,
        reason: reason.to_string(),
    };
    let mut sec_seen: BTreeMap<(String, String, u8), usize> = BTreeMap::new();
    for r in security {
        let file = r.base.file.trim().to_string();
        if !changed.contains(&file) {
            out.drops.security.note("off_patch", sample(&file, r.base.line_start, Some(r.surface), Some(r.base.confidence), "off_patch"));
            continue;
        }
        if !(1..=7).contains(&r.surface) {
            out.drops.security.note("invalid", sample(&file, r.base.line_start, Some(r.surface), Some(r.base.confidence), "invalid_surface"));
            continue;
        }
        if r.base.confidence < 8 {
            out.drops.security.note("below_threshold", sample(&file, r.base.line_start, Some(r.surface), Some(r.base.confidence), "below_threshold"));
            continue;
        }
        if r.reach.trim().is_empty() {
            out.drops.security.note("no_reach", sample(&file, r.base.line_start, Some(r.surface), Some(r.base.confidence), "no_reach"));
            continue;
        }
        let Some((ls, le, quote)) = crate::positioning::verbatim_span(root, &file, &r.base.quote, r.base.line_start) else {
            out.drops.security.note("unanchored", sample(&file, r.base.line_start, Some(r.surface), Some(r.base.confidence), "unanchored"));
            continue;
        };
        let key = (file.clone(), crate::orchestrate::normalize_quote(&quote), r.surface);
        if let Some(&i) = sec_seen.get(&key) {
            out.drops.security.note("duplicate", sample(&file, r.base.line_start, Some(r.surface), Some(r.base.confidence), "duplicate"));
            let kept = &mut out.security[i];
            if r.base.confidence > kept.confidence {
                kept.confidence = r.base.confidence;
            }
            if kept.note.trim().is_empty() {
                kept.note = r.base.note.trim().to_string();
            }
            if kept.reach.trim().is_empty() {
                kept.reach = r.reach.trim().to_string();
            }
            continue;
        }
        sec_seen.insert(key, out.security.len());
        out.security.push(AnchoredSecurity {
            file,
            line_start: ls,
            line_end: le,
            quote,
            surface: r.surface,
            confidence: r.base.confidence,
            reach: r.reach.trim().to_string(),
            note: r.base.note.trim().to_string(),
        });
    }

    // ── 漂移：只认 structural|local；category 归一到 5 值；同 file+quote+category 取更强的一档 ──
    let mut drift_seen: BTreeMap<(String, String, String), usize> = BTreeMap::new();
    for r in drift {
        let file = r.base.file.trim().to_string();
        if !changed.contains(&file) {
            out.drops.drift.note("off_patch", sample(&file, r.base.line_start, None, Some(r.base.confidence), "off_patch"));
            continue;
        }
        let drift_kind = match r.drift.trim().to_ascii_lowercase().as_str() {
            "structural" => DriftKind::Structural,
            "local" => DriftKind::Local,
            _ => {
                out.drops.drift.note("invalid", sample(&file, r.base.line_start, None, Some(r.base.confidence), "invalid_drift"));
                continue;
            }
        };
        let category = normalize_category(&r.category);
        let Some((ls, le, quote)) = crate::positioning::verbatim_span(root, &file, &r.base.quote, r.base.line_start) else {
            out.drops.drift.note("unanchored", sample(&file, r.base.line_start, None, Some(r.base.confidence), "unanchored"));
            continue;
        };
        let key = (file.clone(), crate::orchestrate::normalize_quote(&quote), category.clone());
        if let Some(&i) = drift_seen.get(&key) {
            out.drops.drift.note("duplicate", sample(&file, r.base.line_start, None, Some(r.base.confidence), "duplicate"));
            if drift_kind == DriftKind::Structural {
                out.drift[i].drift = DriftKind::Structural;
            }
            continue;
        }
        drift_seen.insert(key, out.drift.len());
        out.drift.push(AnchoredDrift {
            file,
            line_start: ls,
            line_end: le,
            quote,
            drift: drift_kind,
            category,
            standard: r.standard.trim().to_string(),
            note: r.base.note.trim().to_string(),
        });
    }

    // ── 意图：声明只认 body / issue；按 (声明文本归一, 来源) 去重；scope creep 按标题归一 ──
    if let Some(intent) = intent {
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        // 未验证 claim 只披露、不计入 undelivered/blockers（协调者 2026-11-24 裁）。
        let verification_complete = intent.verification_complete;
        for c in &intent.claims {
            let Some(source) = normalize_claim_source(&c.source) else {
                out.drops.intent.note("invalid_claim_source", DroppedRow { file: None, line: None, surface: None, conf: None, reason: "invalid_claim_source".into() });
                continue;
            };
            let claim = c.claim.trim();
            if claim.is_empty() {
                out.drops.intent.note("invalid", DroppedRow { file: None, line: None, surface: None, conf: None, reason: "empty_claim".into() });
                continue;
            }
            let key = (normalize_claim_text(claim), source.clone());
            if !seen.insert(key) {
                out.drops.intent.note("duplicate", DroppedRow { file: None, line: None, surface: None, conf: None, reason: "duplicate_claim".into() });
                continue;
            }
            out.claims.push(ClaimRow {
                claim: claim.to_string(),
                source,
                delivered: c.delivered,
                note: c.note.trim().to_string(),
                verified: c.delivered || verification_complete,
            });
        }
        let mut seen_creep: BTreeSet<String> = BTreeSet::new();
        for s in &intent.scope_creep {
            let title = s.title.trim();
            if title.is_empty() {
                out.drops.intent.note("invalid", DroppedRow { file: None, line: None, surface: None, conf: None, reason: "empty_scope_creep".into() });
                continue;
            }
            if !seen_creep.insert(normalize_claim_text(title)) {
                out.drops.intent.note("duplicate", DroppedRow { file: None, line: None, surface: None, conf: None, reason: "duplicate_scope_creep".into() });
                continue;
            }
            out.scope_creep.push(ScopeCreepRow {
                title: title.to_string(),
                files: s.files.iter().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect(),
                note: s.note.trim().to_string(),
            });
        }
        // 维护者重复信号必须能在 pr_context.comments 里落到一条非 bot、非作者的评论引用上。
        let overlap = intent.maintainer_overlap.clone();
        let verified = overlap.flagged
            && !overlap.author.trim().is_empty()
            && !overlap.quote.trim().is_empty()
            && pr.comments.iter().any(|c| {
                !c.is_bot
                    && c.author == overlap.author
                    && c.author != pr.author
                    && c.body.contains(overlap.quote.trim())
            });
        if overlap.flagged && !verified {
            out.drops.intent.note("overlap_unverified", DroppedRow { file: None, line: None, surface: None, conf: None, reason: "overlap_unverified".into() });
        }
        if verified {
            out.overlap = overlap;
        }
    }

    out
}

fn normalize_category(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "placement" => "placement".into(),
        "naming" => "naming".into(),
        "ownership" => "ownership".into(),
        "versioning" => "versioning".into(),
        _ => "other".into(),
    }
}

/// 声明来源归一：`issue` / `body`；其它（title/comments/commit）一律丢弃。
pub fn normalize_claim_source(raw: &str) -> Option<String> {
    let s = raw.trim().to_ascii_lowercase();
    if s.contains("issue") || s.contains("#") && s.chars().any(|c| c.is_ascii_digit()) {
        return Some("issue".into());
    }
    if s.contains("body") || s.contains("description") || s.contains("pr description") {
        return Some("body".into());
    }
    None
}

fn normalize_claim_text(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

/// 3.5 的确定性摘要 + findings + blockers。
#[derive(Clone, Debug, Default)]
pub struct Tallied {
    pub summary: Option<Value>,
    pub findings: Vec<Finding>,
    pub blockers: Vec<Value>,
    pub focus: Vec<String>,
}

pub fn tally(prepared: &Prepared, map: &StandardsMap, manifest_cap_s: u64, zh: bool) -> Tallied {
    let structural_count = prepared.drift.iter().filter(|d| d.drift == DriftKind::Structural).count() as u32;
    let local_count = prepared.drift.iter().filter(|d| d.drift == DriftKind::Local).count() as u32;
    let intent_undelivered = prepared.claims.iter().filter(|c| c.verified && !c.delivered).count() as u32;
    let scope_creep_count = prepared.scope_creep.len() as u32;
    let blockers_count = structural_count + intent_undelivered;
    let security_tier = security_tier(&prepared.security);
    let convention_drift = if structural_count > 0 {
        "structural"
    } else if local_count > 0 {
        "local"
    } else {
        "none"
    };
    let risk = risk_of(security_tier, blockers_count, scope_creep_count);
    let fit = fit_of(prepared);

    // findings = 安全 tier≥1（全部锚定行）+ 结构性漂移；local 只计数。
    let mut findings = Vec::new();
    for s in &prepared.security {
        findings.push(sec_finding(s, zh));
    }
    for d in prepared.drift.iter().filter(|d| d.drift == DriftKind::Structural) {
        findings.push(drift_finding(d, zh));
    }

    // blockers = structural findings + 未兑现声明（可无代码锚）。
    let mut blockers = Vec::new();
    for d in prepared.drift.iter().filter(|d| d.drift == DriftKind::Structural) {
        blockers.push(json!({
            "kind": "structural",
            "file": d.file,
            "line_start": d.line_start,
            "line_end": d.line_end,
            "quote": d.quote,
            "category": d.category,
        }));
    }
    for c in prepared.claims.iter().filter(|c| c.verified && !c.delivered) {
        blockers.push(json!({
            "kind": "intent",
            "claim": c.claim,
            "source": c.source,
            "note": c.note,
        }));
    }

    let focus = focus_of(prepared, 5);
    let priority_files: Vec<String> = prepared
        .security
        .iter()
        .map(|s| s.file.clone())
        .chain(prepared.drift.iter().filter(|d| d.drift == DriftKind::Structural).map(|d| d.file.clone()))
        .collect();
    let specs = standards::doc_specs(map, &priority_files, 8);
    let wall_clock_s = tier_budget(risk, manifest_cap_s);

    let summary = json!({
        "security_tier": security_tier,
        "blockers_count": blockers_count,
        "structural_count": structural_count,
        "local_count": local_count,
        "intent_undelivered": intent_undelivered,
        "scope_creep_count": scope_creep_count,
        "risk": risk,
        "convention_drift": convention_drift,
        "fit": fit,
        "routing": { "focus": focus, "specs": specs, "wall_clock_s": wall_clock_s },
    });
    Tallied { summary: Some(summary), findings, blockers, focus }
}

/// `security_tier`：2 仅限面 1/3/4 ∧ conf≥8；否则有安全行 = 1；无 = 0。
pub fn security_tier(rows: &[AnchoredSecurity]) -> u8 {
    if rows.iter().any(AnchoredSecurity::tier2) {
        2
    } else if rows.is_empty() {
        0
    } else {
        1
    }
}

/// `risk`：tier≥1 ∨ blockers≥3 → high；blockers≥1 ∨ scope_creep>0 → medium；else low。
pub fn risk_of(tier: u8, blockers: u32, scope_creep: u32) -> &'static str {
    if tier >= 1 || blockers >= 3 {
        "high"
    } else if blockers >= 1 || scope_creep > 0 {
        "medium"
    } else {
        "low"
    }
}

/// `fit`（grill 定案混合口径）：structural 含 placement/naming/ownership/versioning 或声明未兑现
/// → needs-scope-decision；维护者重复且无未兑现 → possibly-redundant；否则 in-scope。
pub fn fit_of(prepared: &Prepared) -> &'static str {
    let placement_break = prepared.drift.iter().any(|d| {
        d.drift == DriftKind::Structural
            && matches!(d.category.as_str(), "placement" | "naming" | "ownership" | "versioning")
    });
    let undelivered = prepared.claims.iter().any(|c| c.verified && !c.delivered);
    if placement_break || undelivered {
        "needs-scope-decision"
    } else if prepared.overlap.flagged {
        "possibly-redundant"
    } else {
        "in-scope"
    }
}

/// `wall_clock_s`：high 100% / medium 75% / low 50%，下限 300s，恒 ≤ manifest 上限。
pub fn tier_budget(risk: &str, manifest_cap_s: u64) -> u64 {
    let pct = match risk {
        "high" => 100,
        "medium" => 75,
        _ => 50,
    };
    let scaled = manifest_cap_s.saturating_mul(pct) / 100;
    scaled.max(300.min(manifest_cap_s)).min(manifest_cap_s).max(1)
}

/// `focus` ≤5：安全（面 1/3/4 优先、再按置信度）→ 结构性漂移文件 → 未兑现声明。
pub fn focus_of(prepared: &Prepared, cap: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut sec = prepared.security.clone();
    sec.sort_by(|a, b| {
        tier2_surface(b.surface)
            .cmp(&tier2_surface(a.surface))
            .then(b.confidence.cmp(&a.confidence))
            .then(a.file.cmp(&b.file))
            .then(a.surface.cmp(&b.surface))
    });
    for s in &sec {
        push_unique(&mut out, format!("{}@{}", crate::triage::lenses::surface_key(s.surface), s.file), cap);
    }
    let mut drift_files: Vec<&str> = prepared
        .drift
        .iter()
        .filter(|d| d.drift == DriftKind::Structural)
        .map(|d| d.file.as_str())
        .collect();
    drift_files.sort();
    for f in drift_files {
        push_unique(&mut out, f.to_string(), cap);
    }
    for c in prepared.claims.iter().filter(|c| c.verified && !c.delivered) {
        let short: String = c.claim.chars().take(60).collect();
        push_unique(&mut out, format!("intent:{short}"), cap);
    }
    out
}

fn push_unique(out: &mut Vec<String>, v: String, cap: usize) {
    if out.len() < cap && !out.contains(&v) {
        out.push(v);
    }
}

/// 10 分制 → 0..1 的**单点函数**（8/10→0.8）。
pub fn confidence_of_10(score: u8) -> f32 {
    f32::from(score.min(10)) / 10.0
}

fn sec_finding(s: &AnchoredSecurity, zh: bool) -> Finding {
    let severity = if s.tier2() { Severity::High } else { Severity::Medium };
    let title = crate::triage::lenses::surface_title(s.surface, zh);
    let body = if zh {
        format!("可达路径：{}\n说明：{}", s.reach, s.note)
    } else {
        format!("Reachability: {}\nNote: {}", s.reach, s.note)
    };
    let recommendation = if zh {
        "请修复该问题，或提供该路径不可达的证据。".to_string()
    } else {
        "Fix the issue, or provide evidence that the path is not reachable.".to_string()
    };
    make_finding(s.file.clone(), s.line_start, s.line_end, &s.quote, severity, &title, &body, &recommendation, s.confidence)
}

fn drift_finding(d: &AnchoredDrift, zh: bool) -> Finding {
    let title = if zh {
        format!("结构性漂移：{}", d.category)
    } else {
        format!("Structural drift: {}", d.category)
    };
    let body = if zh {
        format!("标准依据：{}\n说明：{}", d.standard, d.note)
    } else {
        format!("Standard: {}\nNote: {}", d.standard, d.note)
    };
    let recommendation = if zh {
        format!("按 {} 调整放置 / 命名 / 归属。", if d.standard.is_empty() { "模块标准" } else { d.standard.as_str() })
    } else {
        format!("Align placement / naming / ownership with {}.", if d.standard.is_empty() { "the module standard" } else { d.standard.as_str() })
    };
    make_finding(
        d.file.clone(),
        d.line_start,
        d.line_end,
        &d.quote,
        Severity::Medium,
        &title,
        &body,
        &recommendation,
        8,
    )
}

#[allow(clippy::too_many_arguments)]
fn make_finding(
    file: String,
    line_start: u32,
    line_end: u32,
    quote: &str,
    severity: Severity,
    title: &str,
    body: &str,
    recommendation: &str,
    confidence_10: u8,
) -> Finding {
    let id = crate::orchestrate::finding_id(&file, quote, title, line_start, line_end);
    Finding {
        id,
        severity,
        title: title.to_string(),
        body: body.to_string(),
        file,
        line_start,
        line_end,
        confidence: confidence_of_10(confidence_10.max(8)),
        recommendation: recommendation.to_string(),
        models: Vec::new(),
        agreement: 1,
        suspicion_agreement: 0,
        hard_constraint: false,
        quote: quote.to_string(),
        previously_dismissed: false,
        cluster_id: None,
        refute: None,
        origin: Origin::New,
        outside_window: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triage::lenses::{Claim, Row, ScopeCreep, SecurityRow};

    fn sec(surface: u8, conf: u8, reach: &str, file: &str, quote: &str) -> SecurityRow {
        SecurityRow {
            base: Row { file: file.into(), line_start: 1, line_end: 1, quote: quote.into(), confidence: conf, note: "n".into() },
            surface,
            reach: reach.into(),
        }
    }

    fn repo_with(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        for (p, body) in files {
            let full = d.path().join(p);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        let root = d.path().to_path_buf();
        (d, root)
    }

    fn pr() -> PrContext {
        serde_json::from_str(
            r#"{"schema":"hibot.audit.pr-context/1","title":"t","body":"b","author":"alice",
                "labels":[],"comments":[],"linked_issues":[],"ci":{"state":"success","failing":[]},
                "changed_files":[{"path":"src/a.rs","additions":1,"deletions":0}]}"#,
        )
        .unwrap()
    }

    fn changed() -> BTreeSet<String> {
        ["src/a.rs".to_string()].into_iter().collect()
    }

    #[test]
    fn tier2_whitelist_secrets_and_code_exec_conf8_sql_injection_stays_tier1() {
        let mut sec = vec![AnchoredSecurity {
            file: "a".into(), line_start: 1, line_end: 1, quote: "q".into(),
            surface: 5, confidence: 10, reach: "r".into(), note: String::new(),
        }];
        assert_eq!(security_tier(&sec), 1, "SQL 注入（面 5）不得升 2");
        // 2026-11-24 用户裁决：secrets（面 2）conf≥8 → tier2。
        sec[0].surface = 2;
        sec[0].confidence = 8;
        assert_eq!(security_tier(&sec), 2, "secrets conf=8 可升 2");
        sec[0].confidence = 10;
        assert_eq!(security_tier(&sec), 2, "secrets conf=10 可升 2");
        sec[0].confidence = 7;
        assert_eq!(security_tier(&sec), 1, "secrets conf=7 仍是 tier1");
        // 面 4 仍按 conf≥8 升 2。
        sec[0].surface = 4;
        sec[0].confidence = 8;
        assert_eq!(security_tier(&sec), 2);
        assert_eq!(security_tier(&[]), 0);
    }

    #[test]
    fn risk_and_fit_tables_cover_every_rule() {
        assert_eq!(risk_of(0, 0, 0), "low");
        assert_eq!(risk_of(0, 0, 1), "medium");
        assert_eq!(risk_of(0, 1, 0), "medium");
        assert_eq!(risk_of(0, 3, 0), "high");
        assert_eq!(risk_of(1, 0, 0), "high");

        let (_d, root) = repo_with(&[("src/a.rs", "let a = 1;\n")]);
        let mut p = prepare(&root, &pr(), &changed(), &[], &[], None);
        assert_eq!(fit_of(&p), "in-scope");
        p.drift = vec![AnchoredDrift {
            file: "src/a.rs".into(), line_start: 1, line_end: 1, quote: "let a = 1;".into(),
            drift: DriftKind::Structural, category: "placement".into(), standard: "s".into(), note: String::new(),
        }];
        assert_eq!(fit_of(&p), "needs-scope-decision", "placement 破坏 → needs-scope-decision");
        p.drift.clear();
        p.claims = vec![ClaimRow { claim: "x".into(), source: "body".into(), delivered: false, note: String::new(), verified: true }];
        assert_eq!(fit_of(&p), "needs-scope-decision", "声明未兑现 → needs-scope-decision");
        p.claims.clear();
        p.overlap = MaintainerOverlap { flagged: true, author: "m".into(), quote: "dup".into() };
        assert_eq!(fit_of(&p), "possibly-redundant");
    }

    #[test]
    fn budget_tiers_are_capped_and_floored() {
        assert_eq!(tier_budget("high", 3600), 3600);
        assert_eq!(tier_budget("medium", 3600), 2700);
        assert_eq!(tier_budget("low", 3600), 1800);
        assert_eq!(tier_budget("low", 100), 100, "manifest 上限优先");
        assert_eq!(tier_budget("low", 1000), 500);
        assert_eq!(tier_budget("medium", 200), 200);
    }

    #[test]
    fn prepare_drops_below_threshold_no_reach_off_patch_and_fabricated_quotes() {
        let (_d, root) = repo_with(&[("src/a.rs", "let a = 1;\nlet b = 2;\n")]);
        let sec = vec![
            sec(4, 7, "reach", "src/a.rs", "let a = 1;"),
            sec(4, 9, "", "src/a.rs", "let a = 1;"),
            sec(4, 9, "reach", "src/other.rs", "let a = 1;"),
            sec(4, 9, "reach", "src/a.rs", "fabricated();"),
            sec(4, 9, "reach", "src/a.rs", "let b = 2;"),
        ];
        let p = prepare(&root, &pr(), &changed(), &sec, &[], None);
        assert_eq!(p.security.len(), 1);
        let s = &p.drops.security;
        assert_eq!((s.below_threshold, s.no_reach, s.off_patch, s.unanchored), (1, 1, 1, 1));
        assert_eq!(s.dropped_total, 4);
        assert!(s.dropped_rows.iter().any(|r| r.reason == "below_threshold" && r.surface == Some(4) && r.conf == Some(7)));
        assert_eq!(p.drops.drift.dropped_total, 0, "安全轴的丢弃不得挂到漂移轴");
        assert_eq!(p.security[0].line_start, 2, "行号按检出原文修正");
    }

    #[test]
    fn dedupe_is_per_axis_and_intent_without_anchor_is_not_merged_into_code_rows() {
        let (_d, root) = repo_with(&[("src/a.rs", "let a = 1;\nlet b = 2;\n")]);
        let sec = vec![sec(5, 9, "r", "src/a.rs", "let a = 1;"), sec(5, 9, "r2", "src/a.rs", "let a = 1;")];
        let intent = IntentOutput {
            verification_complete: true,
            claims: vec![
                Claim { id: String::new(), claim: "add X".into(), source: "body".into(), delivered: false, evidence: vec![], note: String::new() },
                Claim { id: String::new(), claim: "add X".into(), source: "body".into(), delivered: true, evidence: vec![], note: String::new() },
                Claim { id: String::new(), claim: "remove Y".into(), source: "issue#3".into(), delivered: false, evidence: vec![], note: String::new() },
                Claim { id: String::new(), claim: "from comment".into(), source: "comment".into(), delivered: false, evidence: vec![], note: String::new() },
            ],
            scope_creep: vec![
                ScopeCreep { title: "extra".into(), files: vec!["src/a.rs".into()], note: String::new() },
                ScopeCreep { title: "extra".into(), files: vec!["src/a.rs".into()], note: String::new() },
            ],
            maintainer_overlap: MaintainerOverlap::default(),
            notes: vec![],
        };
        let p = prepare(&root, &pr(), &changed(), &sec, &[], Some(&intent));
        assert_eq!(p.security.len(), 1, "安全轴按 file+quote+hint 去重");
        assert_eq!(p.claims.len(), 2, "同一声明只保留一次；comment 来源被丢弃");
        assert_eq!(p.scope_creep.len(), 1);
        assert_eq!(p.drops.intent.invalid_claim_source, 1);

        let tallied = tally(&p, &StandardsMap::default(), 3600, false);
        let s = tallied.summary.unwrap();
        assert_eq!(s["blockers_count"], 2, "0 structural + 2 undelivered intent（安全不计 blocker）");
        assert_eq!(s["intent_undelivered"], 2, "重复声明只计一次");
        // 无锚意图项只进 blockers，不硬塞进 findings。
        assert!(tallied.findings.iter().all(|f| f.file == "src/a.rs"));
        assert!(tallied.blockers.iter().any(|b| b["kind"] == "intent" && b.get("file").is_none()));
    }

    #[test]
    fn unverified_claims_are_disclosed_but_never_blockers() {
        let (_d, root) = repo_with(&[("src/a.rs", "let a = 1;\n")]);
        let intent = IntentOutput {
            claims: vec![Claim {
                id: "c1".into(), claim: "x".into(), source: "body".into(), delivered: false,
                evidence: vec![], note: String::new(),
            }],
            verification_complete: false,
            ..Default::default()
        };
        let p = prepare(&root, &pr(), &changed(), &[], &[], Some(&intent));
        assert_eq!(p.claims.len(), 1, "未验证 claim 仍披露");
        assert!(!p.claims[0].verified);
        let t = tally(&p, &StandardsMap::default(), 3600, false);
        let s = t.summary.unwrap();
        assert_eq!(s["intent_undelivered"], 0, "未验证不计 undelivered");
        assert_eq!(s["blockers_count"], 0);
        assert!(!t.focus.iter().any(|f| f.starts_with("intent:")));
    }

    #[test]
    fn overlap_requires_a_real_non_bot_non_author_comment_quote() {
        let (_d, root) = repo_with(&[("src/a.rs", "let a = 1;\n")]);
        let mut pr = pr();
        pr.comments = vec![crate::triage::input::Comment {
            author: "maint".into(), body: "this duplicates the existing helper".into(),
            created_at: "2026-01-01T00:00:00Z".into(), is_bot: false,
        }];
        let intent = IntentOutput {
            verification_complete: true,
            claims: vec![],
            scope_creep: vec![],
            maintainer_overlap: MaintainerOverlap {
                flagged: true, author: "maint".into(), quote: "duplicates the existing helper".into(),
            },
            notes: vec![],
        };
        let p = prepare(&root, &pr, &changed(), &[], &[], Some(&intent));
        assert!(p.overlap.flagged, "真实维护者引用 → 采信");
        assert_eq!(p.drops.intent.overlap_unverified, 0);

        let mut bot_pr = pr.clone();
        bot_pr.comments[0].is_bot = true;
        let p2 = prepare(&root, &bot_pr, &changed(), &[], &[], Some(&intent));
        assert!(!p2.overlap.flagged, "bot 评论不得作重复信号");
        assert_eq!(p2.drops.intent.overlap_unverified, 1);
    }

    #[test]
    fn focus_and_specs_are_bounded_and_priority_ordered() {
        let (_d, root) = repo_with(&[("src/a.rs", "let a = 1;\n")]);
        let sec = vec![sec(4, 9, "r", "src/a.rs", "let a = 1;")];
        let intent = IntentOutput {
            verification_complete: true,
            claims: (0..8)
                .map(|i| Claim { id: String::new(), claim: format!("claim {i}"), source: "body".into(), delivered: false, evidence: vec![], note: String::new() })
                .collect(),
            ..Default::default()
        };
        let p = prepare(&root, &pr(), &changed(), &sec, &[], Some(&intent));
        let t = tally(&p, &StandardsMap::default(), 3600, false);
        let s = t.summary.unwrap();
        assert_eq!(s["routing"]["focus"].as_array().unwrap().len(), 5);
        assert_eq!(s["routing"]["focus"][0], "code-execution@src/a.rs");
        assert_eq!(s["fit"], "needs-scope-decision");
        assert!(s["routing"]["wall_clock_s"].as_u64().unwrap() <= 3600);
    }

    #[test]
    fn confidence_is_the_single_point_8_of_10_mapping() {
        assert_eq!(confidence_of_10(8), 0.8);
        assert!((confidence_of_10(9) - 0.9).abs() < f32::EPSILON);
        assert_eq!(confidence_of_10(10), 1.0);
        assert_eq!(confidence_of_10(7), 0.7);
    }

    #[test]
    fn summary_is_the_nine_keys_plus_nested_routing() {
        let (_d, root) = repo_with(&[("src/a.rs", "let a = 1;\n")]);
        let p = prepare(&root, &pr(), &changed(), &[], &[], None);
        let t = tally(&p, &StandardsMap::default(), 3600, false);
        let s = t.summary.unwrap();
        let obj = s.as_object().unwrap();
        for k in [
            "security_tier", "blockers_count", "structural_count", "local_count", "intent_undelivered",
            "scope_creep_count", "risk", "convention_drift", "fit", "routing",
        ] {
            assert!(obj.contains_key(k), "missing {k}");
        }
        assert!(s.get("focus").is_none(), "routing 不得提到顶层");
    }

    /// 3.6 反例：findings 5 条 / blockers 2 条 —— blockers 只是披露面，不得把全部 findings 当阻塞项。
    #[test]
    fn findings_and_blockers_are_distinct_arrays() {
        let (_d, root) = repo_with(&[
            ("src/a.rs", "let a = 1;\nlet b = 2;\nlet c = 3;\nlet d = 4;\nlet e = 5;\n"),
        ]);
        let mut changed = BTreeSet::new();
        changed.insert("src/a.rs".to_string());
        let quotes = ["let a = 1;", "let b = 2;", "let c = 3;", "let d = 4;", "let e = 5;"];
        let sec: Vec<SecurityRow> = quotes
            .iter()
            .enumerate()
            .map(|(i, q)| SecurityRow {
                base: Row { file: "src/a.rs".into(), line_start: (i + 1) as u32, line_end: (i + 1) as u32, quote: (*q).into(), confidence: 8, note: String::new() },
                surface: 5,
                reach: "request -> engine".into(),
            })
            .collect();
        let intent = IntentOutput {
            verification_complete: true,
            claims: vec![
                Claim { id: String::new(), claim: "claim one".into(), source: "body".into(), delivered: false, evidence: vec![], note: String::new() },
                Claim { id: String::new(), claim: "claim two".into(), source: "issue#1".into(), delivered: false, evidence: vec![], note: String::new() },
            ],
            ..Default::default()
        };
        let p = prepare(&root, &pr(), &changed, &sec, &[], Some(&intent));
        let t = tally(&p, &StandardsMap::default(), 3600, false);
        assert_eq!(t.findings.len(), 5, "5 条安全 finding");
        assert_eq!(t.blockers.len(), 2, "只有 2 条意图阻塞项");
        let findings = serde_json::to_value(&t.findings).unwrap();
        for f in findings.as_array().unwrap() {
            assert!(f.get("fingerprint").is_some() || f.get("id").is_some());
            assert!(f.get("kind").is_none());
        }
        assert!(t.blockers.iter().all(|b| b.get("kind").is_some() && b.get("fingerprint").is_none()));
    }
}
