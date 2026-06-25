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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
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
}
