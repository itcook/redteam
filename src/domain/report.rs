use super::finding::{Cluster, Finding, Severity};
use super::input::Mode;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Approve,
    Reject,
    #[serde(rename = "incomplete")]
    Incomplete,
}

impl Verdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::Reject => "reject",
            Verdict::Incomplete => "incomplete",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Stats { pub critical: u32, pub high: u32, pub medium: u32, pub low: u32, pub total: u32 }

impl Stats {
    pub fn from_findings(fs: &[Finding]) -> Self {
        let mut s = Stats::default();
        for f in fs {
            match f.severity {
                Severity::Critical => s.critical += 1,
                Severity::High => s.high += 1,
                Severity::Medium => s.medium += 1,
                Severity::Low => s.low += 1,
            }
            s.total += 1;
        }
        s
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Usage { pub input_tokens: u64, pub output_tokens: u64 }

#[derive(Clone, Debug, Default, Serialize)]
pub struct RunMeta {
    pub job_id: String,
    pub models: Vec<String>,
    /// 与 `models`（provider 名）同序的 model id（config 的 providers.*.model）——
    /// 人读报告显示 model 名而非 provider 名（rN.md 模板,2026-07-03）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub model_ids: Vec<String>,
    pub lenses: Vec<String>,
    pub duration_ms: u64,
    pub usage: Usage,
    pub warnings: Vec<String>,
    pub failed_models: Vec<String>,
    /// R8.2 派生索引：本次命中 dismissed 的 finding id（权威源是 findings.previously_dismissed）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dismissed_reappeared: Vec<String>,
    /// 台账自动加载披露——zero-config 下 verdict 受台账影响必须可见（design 2026-07-03 #6）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ledger: Option<LedgerMeta>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LedgerMeta {
    /// 加载的活跃判例卡数（withdrawn/stale 不计）。
    pub loaded: usize,
    /// 来源描述：db 路径或 "csv"。
    pub source: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReviewReport {
    pub schema_version: String,
    pub mode: Mode,
    pub verdict: Verdict,
    pub stats: Stats,
    pub findings: Vec<Finding>,
    pub clusters: Vec<Cluster>,
    pub next_steps: Vec<String>,
    pub run: RunMeta,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::finding::*;
    fn f(sev: Severity) -> Finding {
        Finding { id: "x".into(), severity: sev, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec![], models: vec![], agreement: 1,
            hard_constraint: false, quote: "snippet".into(), cluster_id: None, verify: None,
            previously_dismissed: false }
    }
    #[test]
    fn stats_counts_by_severity() {
        let s = Stats::from_findings(&[f(Severity::High), f(Severity::High), f(Severity::Low)]);
        assert_eq!((s.high, s.low, s.total), (2, 1, 3));
    }
    #[test]
    fn verdict_serializes_kebab() {
        assert_eq!(serde_json::to_string(&Verdict::Reject).unwrap(), "\"reject\"");
    }

    #[test]
    fn verdict_incomplete_serializes() {
        assert_eq!(serde_json::to_string(&Verdict::Incomplete).unwrap(), "\"incomplete\"");
    }
}
