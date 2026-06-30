use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Severity { Low, Medium, High, Critical } // 声明顺序即 Ord：Low<Medium<High<Critical

impl Severity {
    pub fn as_str(&self) -> &'static str {
        match self {
            Severity::Low => "low",
            Severity::Medium => "medium",
            Severity::High => "high",
            Severity::Critical => "critical",
        }
    }
}

/// 模型每次 lens 调用返回的单条 finding（codex 形状 + 可选 hard_constraint）。
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct RawFinding {
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub confidence: f32,
    pub recommendation: String,
    #[serde(default)]
    pub hard_constraint: bool,
    /// 逐字引用出问题的代码原文（positioning 用；prompt 契约要求必填）。
    #[serde(default)]
    pub quote: String,
    /// 模型给本条 finding 打的 rule/lens id（保住 §7.4 severity 钳制）。
    #[serde(default)]
    pub lens: Option<String>,
}

/// 模型每次 lens 调用的完整输出。
#[derive(Clone, Debug, Deserialize, JsonSchema, Default)]
pub struct LensOutput {
    pub findings: Vec<RawFinding>,
}

/// 本地在 dedup 时 enrich 后的 finding。
#[derive(Clone, Debug, Serialize)]
pub struct Finding {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    pub body: String,
    pub file: String,
    pub line_start: u32,
    pub line_end: u32,
    pub confidence: f32,
    pub recommendation: String,
    pub lenses: Vec<String>,
    pub models: Vec<String>,
    pub agreement: u32,
    pub hard_constraint: bool,
    /// 逐字引用（positioning 锚定依据，序列化进输出）。
    pub quote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<Verify>,
}

/// targeted-verify（S3-b）的判定：upheld=守住反驳（finding 成立）、refuted=被证伪。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyVerdict { Upheld, Refuted }

/// 一条 finding 的 verify 结果（refute pass 产出）。
#[derive(Clone, Debug, Serialize)]
pub struct Verify {
    pub verdict: VerifyVerdict,
    /// 执行 refute 的 model 名（交叉验证用哪个）。
    pub model: String,
    /// refute 理由（refuted 时为反驳要点；upheld 时可空）。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Cluster {
    pub id: String,
    pub theme: String,
    pub finding_ids: Vec<String>,
    pub relation: String,
    pub note: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<Severity>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn severity_orders_low_to_critical() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
        assert!(Severity::High < Severity::Critical);
    }
    #[test]
    fn severity_serializes_lowercase() {
        assert_eq!(serde_json::to_string(&Severity::High).unwrap(), "\"high\"");
    }

    #[test]
    fn raw_finding_deserializes_quote_and_lens() {
        let j = r#"{"severity":"high","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":2,"confidence":0.9,"recommendation":"r","quote":"let x = secret();","lens":"L1"}"#;
        let r: RawFinding = serde_json::from_str(j).unwrap();
        assert_eq!(r.quote, "let x = secret();");
        assert_eq!(r.lens.as_deref(), Some("L1"));
    }

    #[test]
    fn raw_finding_quote_defaults_empty_when_absent() {
        let j = r#"{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r"}"#;
        let r: RawFinding = serde_json::from_str(j).unwrap();
        assert_eq!(r.quote, "");
        assert_eq!(r.lens, None);
    }

    #[test]
    fn finding_serializes_quote() {
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec![], models: vec![], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "snippet".into(), verify: None,
        };
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["quote"], "snippet");
    }

    #[test]
    fn finding_serializes_verify_when_present_and_skips_when_none() {
        let mut f = Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec![], models: vec![], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "snippet".into(), verify: None,
        };
        // None → 不出现
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert!(v.get("verify").is_none(), "verify=None must be skipped");
        // Some(Refuted) → 出现，verdict 小写
        f.verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "gpt".into(), note: "guarded above".into() });
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["verify"]["verdict"], "refuted");
        assert_eq!(v["verify"]["model"], "gpt");
        assert_eq!(v["verify"]["note"], "guarded above");
    }
}
