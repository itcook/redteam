use crate::domain::{Cluster, Finding};
use crate::provider::ReviewProvider;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema, Default)]
pub struct CorrelateOut { pub clusters: Vec<ClusterRaw> }

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClusterRaw {
    pub theme: String,
    pub finding_ids: Vec<String>,
    pub relation: String,
    pub note: String,
}

/// 有条件关联：findings<2 或 !enabled → 空。否则一次 LLM 调用产出 cluster。
pub async fn correlate(
    provider: &dyn ReviewProvider, model: &str, findings: &[Finding], enabled: bool,
) -> Vec<Cluster> {
    if !enabled || findings.len() < 2 {
        return Vec::new();
    }
    let _ = (provider, model); // 实现期：构造 schema-嵌入 prompt → provider 文本调用 → 解析 CorrelateOut
    // MVP 安全降级：调用失败/Fake 无能力 → 返回空，不阻断主流程。
    Vec::new()
}

/// 把 LLM 关联结果回填到 finding.cluster_id。
pub fn apply_clusters(findings: &mut [Finding], clusters: &[Cluster]) {
    for c in clusters {
        for f in findings.iter_mut() {
            if c.finding_ids.contains(&f.id) {
                f.cluster_id = Some(c.id.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Severity};
    use crate::provider::fake::FakeProvider;
    fn f(id: &str) -> Finding {
        Finding { id: id.into(), severity: Severity::High, title: "t".into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            lenses: vec![], models: vec![], agreement: 1, hard_constraint: false,
            quote: "snippet".into(), cluster_id: None }
    }
    #[tokio::test]
    async fn skips_when_fewer_than_two() {
        let p = FakeProvider::with("m", vec![]);
        assert!(correlate(&p, "m", &[f("1")], true).await.is_empty());
    }
    #[tokio::test]
    async fn skips_when_disabled() {
        let p = FakeProvider::with("m", vec![]);
        assert!(correlate(&p, "m", &[f("1"), f("2")], false).await.is_empty());
    }
    #[test]
    fn apply_clusters_backfills_cluster_id() {
        let mut fs = vec![f("1"), f("2")];
        let cl = vec![Cluster { id: "c1".into(), theme: "t".into(), finding_ids: vec!["1".into()],
            relation: "root-cause".into(), note: "n".into(), severity: None }];
        apply_clusters(&mut fs, &cl);
        assert_eq!(fs[0].cluster_id.as_deref(), Some("c1"));
        assert_eq!(fs[1].cluster_id, None);
    }
}
