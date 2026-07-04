use super::finding::{Cluster, Finding, Severity};
use super::input::Mode;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub critical: u32, pub high: u32, pub medium: u32, pub low: u32, pub total: u32,
    /// M13：仅本轮新发（origin=new）的计数子块；主字段维持合并集口径（verdict 依据）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub new: Option<Box<Stats>>,
}

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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// 命中缓存读的输入量（input_tokens 子集）；实付全价 ≈ input − cached。跨 provider 归一，
    /// 缺失/未知字段记 0（可观测：cached=0 且 input 大 = 未命中或字段未知）。L1 缓存观测。
    #[serde(default)]
    pub cached_input_tokens: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
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
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub dismissed_reappeared: Vec<String>,
    /// 台账自动加载披露——zero-config 下 verdict 受台账影响必须可见（design 2026-07-03 #6）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ledger: Option<LedgerMeta>,
    /// R3 stamp：证据收集实跑刻的 rev-parse HEAD（code 模式全路径回填；doc 为 None）。
    /// 增量轮以上轮此值为 delta base。serde default 兼容 v2。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head_sha: Option<String>,
    /// M13：本次是否增量轮（--incremental-from 路径）。
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub incremental: bool,
    /// 增量轮 delta 基线（上轮 stamped HEAD）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub delta_base: Option<String>,
    /// 命中 resolve 台账被排除携带的 id（销案生效审计）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub resolved: Vec<String>,
    /// 假修复回路：claimed-resolved 但同 id 再现的 finding id（ship 按修复未生效重开）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub resolution_disputed: Vec<String>,
    /// 重锚定失败被消灭的携带候选 id（使锚失效的改动必在本轮 delta 内，审计用）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub carried_dropped: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerMeta {
    /// 加载的活跃判例卡数（withdrawn/stale 不计）。
    pub loaded: usize,
    /// 来源描述：db 路径或 "csv"。
    pub source: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewReport {
    pub schema_version: String,
    pub mode: Mode,
    pub verdict: Verdict,
    pub stats: Stats,
    pub findings: Vec<Finding>,
    pub clusters: Vec<Cluster>,
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
            previously_dismissed: false, origin: Origin::New }
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

    #[test]
    fn report_v2_json_deserializes_with_defaults() {
        // v2 真实形状：无 head_sha/incremental/聚合数组/stats.new/dismissed_reappeared
        let j = r#"{"schema_version":"2","mode":"code","verdict":"approve",
            "stats":{"critical":0,"high":0,"medium":0,"low":0,"total":0},
            "findings":[],"clusters":[],
            "run":{"job_id":"j1","models":["m"],"lenses":["L1"],"duration_ms":1,
                   "usage":{"input_tokens":1,"output_tokens":1},
                   "warnings":[],"failed_models":[]}}"#;
        let r: ReviewReport = serde_json::from_str(j).unwrap();
        assert_eq!(r.verdict, Verdict::Approve);
        assert_eq!(r.run.head_sha, None);
        assert!(!r.run.incremental);
        assert!(r.run.resolved.is_empty() && r.run.carried_dropped.is_empty());
        assert!(r.stats.new.is_none());
    }

    #[test]
    fn runmeta_v3_fields_roundtrip_and_skip_when_default() {
        let mut m = RunMeta { head_sha: Some("abc".into()), incremental: true,
            delta_base: Some("def".into()), resolved: vec!["1".into()],
            resolution_disputed: vec!["2".into()], carried_dropped: vec!["3".into()],
            ..Default::default() };
        m.job_id = "j".into();
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("\"head_sha\":\"abc\"") && j.contains("\"incremental\":true"));
        // 默认态（非增量）不产键——v2 消费者视角零噪音
        let d = serde_json::to_string(&RunMeta::default()).unwrap();
        for k in ["head_sha", "incremental", "delta_base", "resolved", "resolution_disputed", "carried_dropped"] {
            assert!(!d.contains(k), "默认态不应含 {k}: {d}");
        }
    }
}
