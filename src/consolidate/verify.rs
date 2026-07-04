use crate::consolidate::dedup::{quote_overlap, title_jaccard};
use crate::db::DismissCard;
use crate::domain::{Finding, Mode, Severity, Verify, VerifyVerdict};
use crate::prompt::{build_refute_prompt, render_precedents};
use crate::provider::{LensRequest, ReviewProvider};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// 机械预筛（design 2026-07-03 #3）：finding 是否疑似某活跃判例的主题变体。
/// 召回优先（阈值宽）、精判交 refute（判例注入的 skeptic 终审）——机械匹配对
/// 换措辞天然半盲，它只负责把候选送进 refute，不做终审。
/// 精确 id 命中不算（那是 mark_dismissed 豁免的职责，refute 纯浪费）。
fn matches_precedent(f: &Finding, c: &DismissCard) -> bool {
    if f.id == c.id {
        return false;
    }
    let t = title_jaccard(&f.title, &c.title);
    (f.file == c.file && t >= 0.25) || t >= 0.5 || quote_overlap(&f.quote, &c.quote) >= 0.5
}

/// 选交叉 model：第一个不在 finding.models 里的；没有则退化用 models[0]（自审）。
fn pick_cross_model<'a>(models: &'a [String], finding_models: &[String]) -> Option<&'a str> {
    models
        .iter()
        .find(|m| !finding_models.contains(m))
        .or_else(|| models.first())
        .map(|s| s.as_str())
}

/// 对单条孤证做 refute：选交叉 model → build_refute_prompt → provider.run。
/// 返回 (Verify, 可选 warning)：空 findings=Upheld、非空=Refuted；调用失败=保守 Upheld
/// 且携带 warning——验证层静默失效必须可观测（dogfood 20260703 r1：4/6 refute 失败零告警）。
async fn refute_one(
    provider: &dyn ReviewProvider, finding: &Finding, models: &[String], mode: Mode,
    precedents: &[DismissCard],
) -> (Verify, Option<String>) {
    let model = match pick_cross_model(models, &finding.models) {
        Some(m) => m.to_string(),
        None => return (
            Verify { verdict: VerifyVerdict::Upheld, model: String::new(), note: "no model".into() },
            None,
        ),
    };
    let notes = render_precedents(finding, precedents);
    let prompt = build_refute_prompt(finding, mode, &notes);
    match provider.run(LensRequest { model: model.clone(), prompt }).await {
        Ok(call) => {
            let (verdict, note, contract_warn) = classify_refute(&call.findings);
            let warn = contract_warn.map(|w| format!("refute [{}] model {model}: {w}", finding.id));
            (Verify { verdict, model, note }, warn)
        }
        // 保守：refute 调用失败不冤枉 finding（保留其 blocking 力），但失败必须进 warnings。
        Err(e) => {
            let warn = format!("refute [{}] model {model}: {e}", finding.id);
            (Verify { verdict: VerifyVerdict::Upheld, model, note: "refute unavailable".into() }, Some(warn))
        }
    }
}

/// refute 输出 → verdict：语义前缀优先，非空性只是违约 fallback（附 warning）。
/// r1 20260703 实证：六字段必填契约后，守约模型想表达 upheld 也会输出 finding
/// （title "Claim upheld: …"），按「非空即 refuted」会静默吞掉真问题。
fn classify_refute(
    findings: &[crate::domain::RawFinding],
) -> (VerifyVerdict, String, Option<&'static str>) {
    let Some(first) = findings.first() else {
        return (VerifyVerdict::Upheld, String::new(), None);
    };
    let title = first.title.trim();
    let lower = title.to_lowercase();
    if lower.starts_with("refuted") {
        // 剥机器信号前缀，note 保留纯理由（"refuted" 为 ASCII，字节切片安全）。
        let note = title["refuted".len()..].trim_start_matches([':', '：', ' ']).to_string();
        (VerifyVerdict::Refuted, note, None)
    } else if lower.starts_with("upheld") || lower.starts_with("claim upheld") {
        (VerifyVerdict::Upheld, title.to_string(), None)
    } else {
        // 违约输出：无 verdict 前缀。沿用旧启发式（非空=refuted）保住变体压制力，但漂移必须可观测。
        (
            VerifyVerdict::Refuted,
            title.to_string(),
            Some("verdict prefix missing in refute output; non-empty treated as refuted (contract drift)"),
        )
    }
}

/// 三层 targeted-verify（S3-b）+ 判例预筛扩面（design 2026-07-03 #3）。
/// 层①：agreement>=2 跳过（多模型已互证）。层②：引用存在性由 positioning 已砍编造。
/// 层③：孤证 blocking（agreement==1 && severity>=block_threshold）**或预筛命中判例变体**
/// （后者无视 agreement/severity——review 不再注入勿再报后，变体可能多模型同报/低严出现）。
/// 精确 id 已在台账者不 refute（mark_dismissed 将豁免，烧 LLM 纯浪费）。
/// refuted → verify=Refuted 且 confidence=0.0（降下 floor，不翻 verdict）；upheld → verify=Upheld。
/// Semaphore 限 max_concurrency。
/// `precedents`：活跃判例卡——refute prompt 按相关性渲染注入，skeptic 见过往人裁，
/// 主题变体（换文件/措辞/锚点）跨轮一致地被 refute（dogfood 20260702 最高信号缺陷）。
pub async fn verify_findings(
    mut findings: Vec<Finding>,
    provider: &dyn ReviewProvider,
    models: &[String],
    block_threshold: Severity,
    max_concurrency: usize,
    mode: Mode,
    precedents: &[DismissCard],
) -> (Vec<Finding>, Vec<String>) {
    let exact_ids: std::collections::HashSet<&str> = precedents.iter().map(|c| c.id.as_str()).collect();
    let targets: Vec<usize> = findings
        .iter()
        .enumerate()
        .filter(|(_, f)| !exact_ids.contains(f.id.as_str()))
        .filter(|(_, f)| {
            (f.agreement == 1 && f.severity >= block_threshold)
                || precedents.iter().any(|c| matches_precedent(f, c))
        })
        .map(|(i, _)| i)
        .collect();
    if targets.is_empty() {
        return (findings, Vec::new());
    }
    let sem = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let results = futures::future::join_all(targets.iter().map(|&i| {
        let sem = sem.clone();
        let f = &findings[i];
        async move {
            let _permit = sem.acquire().await.expect("semaphore not closed");
            refute_one(provider, f, models, mode, precedents).await
        }
    }))
    .await;
    let mut warnings = Vec::new();
    for (&i, (v, warn)) in targets.iter().zip(results) {
        if v.verdict == VerifyVerdict::Refuted {
            // 证伪 → 降 confidence 到 0（作为信号，md 渲染/排序可用）。
            // verdict 的实质排除已落在 compute_verdict（显式 verify==Refuted 不 blocking），
            // 不再依赖该 confidence 数值——故退化配置 conf_floor=0 下亦安全。
            findings[i].confidence = 0.0;
        }
        findings[i].verify = Some(v);
        warnings.extend(warn);
    }
    (findings, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Mode, Severity, VerifyVerdict};
    use crate::provider::fake::FakeProvider;
    use crate::domain::RawFinding;
    use std::collections::BTreeMap;

    fn finding(title: &str, sev: Severity, agreement: u32, model: &str) -> Finding {
        Finding {
            id: title.into(), severity: sev, title: title.into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec!["L1".into()], models: vec![model.into()],
            agreement, hard_constraint: false, cluster_id: None, quote: "q".into(), verify: None,
            previously_dismissed: false,
        }
    }
    fn rawf(title: &str) -> RawFinding {
        RawFinding { severity: Severity::High, title: title.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(), lens: None }
    }

    #[test]
    fn classify_refute_prefix_semantics() {
        // 空 → upheld（原契约不变）
        let (v, note, w) = classify_refute(&[]);
        assert_eq!((v, note.as_str(), w), (VerifyVerdict::Upheld, "", None));
        // REFUTED 前缀 → refuted，note 剥前缀
        let (v, note, w) = classify_refute(&[rawf("REFUTED: guard exists at L10")]);
        assert_eq!(v, VerifyVerdict::Refuted);
        assert_eq!(note, "guard exists at L10");
        assert!(w.is_none());
        // upheld 语义前缀 → upheld（r1 12bbeb4c 型误杀不再发生）
        let (v, _, w) = classify_refute(&[rawf("Claim upheld: body interpolated raw")]);
        assert_eq!(v, VerifyVerdict::Upheld);
        assert!(w.is_none());
        // 无前缀违约 → fallback refuted + contract-drift warning
        let (v, note, w) = classify_refute(&[rawf("actually guarded above")]);
        assert_eq!(v, VerifyVerdict::Refuted);
        assert_eq!(note, "actually guarded above");
        assert!(w.unwrap().contains("contract drift"));
    }

    #[tokio::test]
    async fn lone_blocking_refuted_drops_confidence_and_marks() {
        // skeptic 返回非空（找到反驳）→ refuted；confidence 降 0
        let provider = FakeProvider::with("beta", vec![rawf("actually guarded above")]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let (out, _w) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(out[0].verify.as_ref().unwrap().model, "beta", "交叉选了非 alpha 的 model");
        assert_eq!(out[0].confidence, 0.0, "refuted 降 confidence 下 floor");
    }

    #[tokio::test]
    async fn lone_blocking_upheld_keeps_confidence() {
        // skeptic 返回空（守不住反驳）→ upheld；confidence 不动
        let provider = FakeProvider::with("beta", vec![]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let (out, _w) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld);
        assert_eq!(out[0].confidence, 0.9);
    }

    #[tokio::test]
    async fn consensus_finding_is_skipped() {
        // agreement>=2（层①）→ 不 refute，verify 留 None
        let provider = FakeProvider::with("beta", vec![rawf("x")]);
        let f = finding("agreed bug", Severity::Critical, 2, "alpha");
        let (out, _w) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, &[]).await;
        assert!(out[0].verify.is_none());
        assert_eq!(out[0].confidence, 0.9);
    }

    #[tokio::test]
    async fn below_block_threshold_is_skipped() {
        // severity < block_threshold → 不 refute
        let provider = FakeProvider::with("beta", vec![rawf("x")]);
        let f = finding("minor", Severity::Low, 1, "alpha");
        let (out, _w) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, &[]).await;
        assert!(out[0].verify.is_none());
    }

    #[tokio::test]
    async fn doc_omission_lone_blocking_refuted() {
        use crate::domain::Mode;
        // doc 模式：空引用遗漏类孤证 High → refute（doc 模板路径）。skeptic 返回非空 → refuted。
        let provider = FakeProvider::with("beta", vec![rawf("actually specified in section 3")]);
        let mut f = finding("missing rollback branch", Severity::High, 1, "alpha");
        f.quote = String::new();
        let (out, _w) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Doc, &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(out[0].confidence, 0.0);
    }

    fn card(id: &str, file: &str, title: &str) -> DismissCard {
        DismissCard {
            id: id.into(), status: "dismissed".into(), title: title.into(), file: file.into(),
            quote: "ledger quote".into(), severity: "high".into(), reason: "既往误报".into(),
            round: "r1".into(), updated_at: String::new(),
        }
    }

    #[tokio::test]
    async fn precedent_prescreen_forces_refute_despite_consensus() {
        // agreement=2 本该层① 跳过，但预筛命中判例变体（同 file + title 相似）→ 强制进 refute
        let provider = FakeProvider::with("beta", vec![rawf("same theme as precedent")]);
        let mut f = finding("prompt injection via ledger notes", Severity::Medium, 2, "alpha");
        f.models = vec!["alpha".into(), "gamma".into()];
        let cards = vec![card("ffffffffffffffff", "a.rs", "prompt injection via dismissed notes")];
        let (out, _w) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::High, 4, Mode::Code, &cards).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted,
            "预筛命中 → 无视 agreement/severity 进 refute");
    }

    #[tokio::test]
    async fn exact_ledger_id_skips_refute() {
        // 精确 id 已在台账 → mark_dismissed 将豁免，refute 纯浪费 → 跳过
        let provider = FakeProvider::with("beta", vec![rawf("would refute")]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let cards = vec![card(&f.id.clone(), "a.rs", "missing auth")];
        let (out, _w) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, &cards).await;
        assert!(out[0].verify.is_none(), "台账精确命中不烧 refute");
    }

    #[tokio::test]
    async fn refute_failure_is_conservatively_upheld() {
        // provider 对所选 model 无 script → 调用失败 → 保守 upheld（保留 finding 的 blocking 力）
        let provider = FakeProvider::with("alpha", vec![]); // 只有 alpha，单 model
        let f = finding("missing auth", Severity::High, 1, "alpha");
        // models 只有 alpha → 退化自审 alpha；但 FakeProvider 有 alpha script(空) → upheld
        let (out, w) = verify_findings(vec![f], &provider, &["alpha".into()], Severity::Medium, 4, Mode::Code, &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld);
        assert!(w.is_empty(), "成功 upheld 无 warning");

        // 真失败：所选 model 无 script → 保守 upheld + warning 上浮（验证层失效可观测）
        let provider2 = FakeProvider { scripted: BTreeMap::new() };
        let f2 = finding("missing auth", Severity::High, 1, "ghost");
        let (out2, w2) = verify_findings(vec![f2], &provider2, &["ghost".into()], Severity::Medium, 4, Mode::Code, &[]).await;
        assert_eq!(out2[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld, "调用失败保守 upheld");
        assert_eq!(out2[0].confidence, 0.9, "upheld 不降 confidence");
        assert_eq!(w2.len(), 1, "refute 失败必须产出 warning");
        assert!(w2[0].starts_with("refute [") && w2[0].contains("model ghost"), "warning 带 finding id 与 model: {}", w2[0]);
    }
}
