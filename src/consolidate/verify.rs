use crate::domain::{Finding, Severity, Verify, VerifyVerdict};
use crate::prompt::build_refute_prompt;
use crate::provider::{LensRequest, ReviewProvider};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// 选交叉 model：第一个不在 finding.models 里的；没有则退化用 models[0]（自审）。
fn pick_cross_model<'a>(models: &'a [String], finding_models: &[String]) -> Option<&'a str> {
    models
        .iter()
        .find(|m| !finding_models.contains(m))
        .or_else(|| models.first())
        .map(|s| s.as_str())
}

/// 对单条孤证做 refute：选交叉 model → build_refute_prompt → provider.run。
/// 返回 Verify：空 findings=Upheld、非空=Refuted；调用失败=保守 Upheld。
async fn refute_one(provider: &dyn ReviewProvider, finding: &Finding, models: &[String]) -> Verify {
    let model = match pick_cross_model(models, &finding.models) {
        Some(m) => m.to_string(),
        None => return Verify { verdict: VerifyVerdict::Upheld, model: String::new(), note: "no model".into() },
    };
    let prompt = build_refute_prompt(finding);
    match provider.run(LensRequest { model: model.clone(), prompt }).await {
        Ok(call) if !call.findings.is_empty() => {
            let note = call.findings.first().map(|f| f.title.clone()).unwrap_or_default();
            Verify { verdict: VerifyVerdict::Refuted, model, note }
        }
        Ok(_) => Verify { verdict: VerifyVerdict::Upheld, model, note: String::new() },
        // 保守：refute 调用失败不冤枉 finding（保留其 blocking 力）。
        Err(_) => Verify { verdict: VerifyVerdict::Upheld, model, note: "refute unavailable".into() },
    }
}

/// 三层 targeted-verify（S3-b）。
/// 层①：agreement>=2 跳过（多模型已互证）。层②：引用存在性由 positioning 已砍编造。
/// 层③：仅对孤证 blocking（agreement==1 && severity>=block_threshold）做定向 refute。
/// refuted → verify=Refuted 且 confidence=0.0（降下 floor，不翻 verdict）；upheld → verify=Upheld。
/// Semaphore 限 max_concurrency。
pub async fn verify_findings(
    mut findings: Vec<Finding>,
    provider: &dyn ReviewProvider,
    models: &[String],
    block_threshold: Severity,
    max_concurrency: usize,
) -> Vec<Finding> {
    let targets: Vec<usize> = findings
        .iter()
        .enumerate()
        .filter(|(_, f)| f.agreement == 1 && f.severity >= block_threshold)
        .map(|(i, _)| i)
        .collect();
    if targets.is_empty() {
        return findings;
    }
    let sem = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let verdicts = futures::future::join_all(targets.iter().map(|&i| {
        let sem = sem.clone();
        let f = &findings[i];
        async move {
            let _permit = sem.acquire().await.expect("semaphore not closed");
            refute_one(provider, f, models).await
        }
    }))
    .await;
    for (&i, v) in targets.iter().zip(verdicts) {
        if v.verdict == VerifyVerdict::Refuted {
            // 证伪 → 降 confidence 到 0，使 compute_verdict 的 `confidence >= conf_floor`
            // 过滤将其判为非 blocking（默认 floor=0.5 安全）。注意退化配置 conf_floor=0 下
            // 0.0 >= 0.0 为真，refuted 仍会 blocking——实质修复（compute_verdict 显式排除
            // verify==Refuted，不依赖 confidence 数值）留待 S4 与 doc-verify 一并处理。
            findings[i].confidence = 0.0;
        }
        findings[i].verify = Some(v);
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Severity, VerifyVerdict};
    use crate::provider::fake::FakeProvider;
    use crate::domain::RawFinding;
    use std::collections::BTreeMap;

    fn finding(title: &str, sev: Severity, agreement: u32, model: &str) -> Finding {
        Finding {
            id: title.into(), severity: sev, title: title.into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec!["L1".into()], models: vec![model.into()],
            agreement, hard_constraint: false, cluster_id: None, quote: "q".into(), verify: None,
        }
    }
    fn rawf(title: &str) -> RawFinding {
        RawFinding { severity: Severity::High, title: title.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(), lens: None }
    }

    #[tokio::test]
    async fn lone_blocking_refuted_drops_confidence_and_marks() {
        // skeptic 返回非空（找到反驳）→ refuted；confidence 降 0
        let provider = FakeProvider::with("beta", vec![rawf("actually guarded above")]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(out[0].verify.as_ref().unwrap().model, "beta", "交叉选了非 alpha 的 model");
        assert_eq!(out[0].confidence, 0.0, "refuted 降 confidence 下 floor");
    }

    #[tokio::test]
    async fn lone_blocking_upheld_keeps_confidence() {
        // skeptic 返回空（守不住反驳）→ upheld；confidence 不动
        let provider = FakeProvider::with("beta", vec![]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld);
        assert_eq!(out[0].confidence, 0.9);
    }

    #[tokio::test]
    async fn consensus_finding_is_skipped() {
        // agreement>=2（层①）→ 不 refute，verify 留 None
        let provider = FakeProvider::with("beta", vec![rawf("x")]);
        let f = finding("agreed bug", Severity::Critical, 2, "alpha");
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4).await;
        assert!(out[0].verify.is_none());
        assert_eq!(out[0].confidence, 0.9);
    }

    #[tokio::test]
    async fn below_block_threshold_is_skipped() {
        // severity < block_threshold → 不 refute
        let provider = FakeProvider::with("beta", vec![rawf("x")]);
        let f = finding("minor", Severity::Low, 1, "alpha");
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4).await;
        assert!(out[0].verify.is_none());
    }

    #[tokio::test]
    async fn refute_failure_is_conservatively_upheld() {
        // provider 对所选 model 无 script → 调用失败 → 保守 upheld（保留 finding 的 blocking 力）
        let provider = FakeProvider::with("alpha", vec![]); // 只有 alpha，单 model
        let f = finding("missing auth", Severity::High, 1, "alpha");
        // models 只有 alpha → 退化自审 alpha；但 FakeProvider 有 alpha script(空) → upheld
        let out = verify_findings(vec![f], &provider, &["alpha".into()], Severity::Medium, 4).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld);

        // 真失败：所选 model 无 script
        let provider2 = FakeProvider { scripted: BTreeMap::new() };
        let f2 = finding("missing auth", Severity::High, 1, "ghost");
        let out2 = verify_findings(vec![f2], &provider2, &["ghost".into()], Severity::Medium, 4).await;
        assert_eq!(out2[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld, "调用失败保守 upheld");
        assert_eq!(out2[0].confidence, 0.9, "upheld 不降 confidence");
    }
}
