use crate::domain::{Mode, Severity};

pub struct Lens {
    pub id: &'static str,
    pub title: &'static str,
    pub focus: &'static str,        // 注入 prompt 的"本 lens 攻击面"
    pub max_severity: Option<Severity>,
}

static CODE: &[Lens] = &[
    Lens { id: "L1", title: "Security & Boundaries", focus: "auth, authorization, tenant isolation, trust boundaries, input validation", max_severity: None },
    Lens { id: "L2", title: "Data & State", focus: "data loss/corruption/duplication, irreversible state changes, rollback safety, idempotency", max_severity: None },
    Lens { id: "L3", title: "Concurrency & Failure", focus: "race conditions, ordering assumptions, re-entrancy, retries, partial failure, timeouts, degraded dependencies, null/empty state", max_severity: None },
    Lens { id: "L4", title: "Evolution & Observability", focus: "version skew, schema drift, migration hazards, compatibility regressions, observability gaps", max_severity: None },
    Lens { id: "L0", title: "Spec Consistency", focus: "whether the implementation drifts from the design intent of the provided spec docs", max_severity: None },
    Lens { id: "L5", title: "Standards Compliance", focus: "coding conventions, naming, structure, team standards; severity low/medium unless a hard project constraint", max_severity: Some(Severity::Medium) },
];

static DOC: &[Lens] = &[
    Lens { id: "D1", title: "Internal Consistency", focus: "section contradictions, mutually exclusive requirements, self-conflicting constraints", max_severity: None },
    Lens { id: "D2", title: "Completeness & Ambiguity", focus: "missing branches, undefined error handling, under-specification, ambiguity that will bite implementation", max_severity: None },
    Lens { id: "D3", title: "Hidden Assumptions & Risk", focus: "unstated assumptions, unvalidated dependencies; whether the design addresses security/data/failure/migration", max_severity: None },
    Lens { id: "D4", title: "Testability & Success Criteria", focus: "whether success criteria are measurable and acceptance conditions are defined", max_severity: None },
    Lens { id: "D5", title: "Standards Compliance", focus: "whether the document follows team doc standards/templates; severity low/medium unless a hard project constraint", max_severity: Some(Severity::Medium) },
];

pub fn all_for(mode: Mode) -> Vec<&'static Lens> {
    match mode { Mode::Code => CODE.iter().collect(), Mode::Doc => DOC.iter().collect() }
}

pub fn by_id(id: &str) -> Option<&'static Lens> {
    CODE.iter().chain(DOC.iter()).find(|l| l.id == id)
}

pub fn select(mode: Mode, ids: &[String]) -> anyhow::Result<Vec<&'static Lens>> {
    if ids.is_empty() {
        return Ok(all_for(mode));
    }
    let mut out = Vec::new();
    for id in ids {
        let l = by_id(id).ok_or_else(|| anyhow::anyhow!("unknown lens: {id}"))?;
        out.push(l);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Mode;
    #[test]
    fn code_has_six_default_lenses_incl_l0_l5() {
        let ids: Vec<_> = all_for(Mode::Code).iter().map(|l| l.id).collect();
        assert!(ids.contains(&"L1") && ids.contains(&"L0") && ids.contains(&"L5"));
    }
    #[test]
    fn compliance_lens_caps_at_medium() {
        assert_eq!(by_id("L5").unwrap().max_severity, Some(crate::domain::Severity::Medium));
        assert_eq!(by_id("D5").unwrap().max_severity, Some(crate::domain::Severity::Medium));
        assert_eq!(by_id("L1").unwrap().max_severity, None);
    }
    #[test]
    fn select_subset_and_reject_unknown() {
        assert_eq!(select(Mode::Code, &["L1".into(), "L2".into()]).unwrap().len(), 2);
        assert!(select(Mode::Code, &["NOPE".into()]).is_err());
    }
}
