use crate::domain::{Finding, Severity, Verdict};
use crate::lens::by_id;

/// 规范遵循 lens 的 finding 且未标 hard_constraint → severity 钳到 ≤ medium。
pub fn clamp_compliance(findings: &mut [Finding]) {
    for f in findings.iter_mut() {
        if f.hard_constraint {
            continue;
        }
        let capped = f
            .lenses
            .iter()
            .any(|id| by_id(id).and_then(|l| l.max_severity).is_some());
        if capped && f.severity > Severity::Medium {
            f.severity = Severity::Medium;
        }
    }
}

/// reject 当存在 finding severity ≥ block_threshold 且 confidence ≥ conf_floor。
pub fn compute_verdict(
    findings: &[Finding],
    block_threshold: Severity,
    conf_floor: f32,
) -> Verdict {
    let blocking = findings
        .iter()
        .any(|f| f.severity >= block_threshold && f.confidence >= conf_floor);
    if blocking {
        Verdict::Reject
    } else {
        Verdict::Approve
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(sev: Severity, conf: f32, lens: &str, hard: bool) -> Finding {
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
            lenses: vec![lens.into()],
            models: vec![],
            agreement: 1,
            hard_constraint: hard,
            quote: "snippet".into(),
            cluster_id: None,
        }
    }

    #[test]
    fn reject_when_high_above_floor() {
        let v = vec![f(Severity::High, 0.9, "L1", false)];
        assert_eq!(compute_verdict(&v, Severity::Medium, 0.5), Verdict::Reject);
    }

    #[test]
    fn approve_when_only_low_or_below_floor() {
        let v = vec![
            f(Severity::Low, 0.9, "L1", false),
            f(Severity::High, 0.2, "L1", false),
        ];
        assert_eq!(compute_verdict(&v, Severity::Medium, 0.5), Verdict::Approve);
    }

    #[test]
    fn clamp_compliance_lens_to_medium() {
        let mut v = vec![f(Severity::Critical, 0.9, "L5", false)];
        clamp_compliance(&mut v);
        assert_eq!(v[0].severity, Severity::Medium);
    }

    #[test]
    fn hard_constraint_not_clamped() {
        let mut v = vec![f(Severity::High, 0.9, "L5", true)];
        clamp_compliance(&mut v);
        assert_eq!(v[0].severity, Severity::High);
    }
}
