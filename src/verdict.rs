use crate::domain::{Finding, Severity, Verdict};

/// reject 当存在 finding severity ≥ block_threshold 且 confidence ≥ conf_floor。
pub fn compute_verdict(
    findings: &[Finding],
    block_threshold: Severity,
    conf_floor: f32,
) -> Verdict {
    if blocking_count(findings, block_threshold, conf_floor) > 0 { Verdict::Reject } else { Verdict::Approve }
}

/// blocking finding 条数（[`compute_verdict`] 的同一谓词）：scanner 不全时判定保持 Incomplete，本可 reject 的条数另行披露（C2 F3〔D15〕）。
pub fn blocking_count(findings: &[Finding], block_threshold: Severity, conf_floor: f32) -> usize {
    findings
        .iter()
        .filter(|f| {
            f.severity >= block_threshold
                && f.confidence >= conf_floor
                // 实质修复（carry-to-S4 ①）：被证伪的 finding 永不 blocking，不靠 confidence 数值。
                && !f.is_refuted()
                && !f.previously_dismissed // R8.2：人已驳回 → 不 blocking（证据保留）
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Origin;

    fn f(sev: Severity, conf: f32, hard: bool) -> Finding {
        Finding {
            id: "x".into(),
            severity: sev,
            title: "t".into(),
            body: "b".into(),
            file: "a".into(),
            line_start: 1,
            line_end: 1,
            confidence: conf,
            recommendation: "r".into(),
            models: vec![],
            agreement: 1, suspicion_agreement: 0,
            hard_constraint: hard,
            quote: "snippet".into(),
            cluster_id: None,
            verify: None,
            previously_dismissed: false,
            origin: Origin::New,
            outside_window: false,
        }
    }

    #[test]
    fn blocking_count_uses_the_verdict_predicate() {
        let v = vec![f(Severity::High, 0.9, false), f(Severity::Critical, 0.8, false), f(Severity::High, 0.2, false), f(Severity::Low, 0.9, false)];
        assert_eq!(blocking_count(&v, Severity::Medium, 0.5), 2);
        assert_eq!(blocking_count(&v[2..], Severity::Medium, 0.5), 0);
        assert_eq!(compute_verdict(&v[2..], Severity::Medium, 0.5), Verdict::Approve);
    }

    #[test]
    fn reject_when_high_above_floor() {
        let v = vec![f(Severity::High, 0.9, false)];
        assert_eq!(compute_verdict(&v, Severity::Medium, 0.5), Verdict::Reject);
    }

    #[test]
    fn approve_when_only_low_or_below_floor() {
        let v = vec![
            f(Severity::Low, 0.9, false),
            f(Severity::High, 0.2, false),
        ];
        assert_eq!(compute_verdict(&v, Severity::Medium, 0.5), Verdict::Approve);
    }

    #[test]
    fn refuted_finding_never_blocks_even_at_zero_floor() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut hi = f(Severity::High, 0.0, false);
        hi.verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "beta".into(), note: String::new() });
        // conf_floor=0 退化配置：修复前 0.0>=0.0 会 blocking；修复后 refuted 被排除 → Approve。
        assert_eq!(compute_verdict(&[hi], Severity::Medium, 0.0), Verdict::Approve);
    }

    #[test]
    fn upheld_finding_still_blocks() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut hi = f(Severity::High, 0.9, false);
        hi.verify = Some(Verify { verdict: VerifyVerdict::Upheld, model: "beta".into(), note: String::new() });
        assert_eq!(compute_verdict(&[hi], Severity::Medium, 0.5), Verdict::Reject);
    }

    #[test]
    fn dismissed_blocking_finding_is_exempt() {
        let mut hi = f(Severity::High, 0.9, false);
        hi.previously_dismissed = true;
        assert_eq!(compute_verdict(&[hi], Severity::Medium, 0.5), Verdict::Approve);
    }
}
