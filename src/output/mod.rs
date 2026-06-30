use crate::domain::{ReviewReport, Severity};

#[derive(Clone, Copy, Debug)]
pub enum Format { Json, Md }

pub fn render(report: &ReviewReport, fmt: Format) -> String {
    match fmt {
        Format::Json => serde_json::to_string_pretty(report).unwrap(),
        Format::Md => render_md(report),
    }
}

fn render_md(r: &ReviewReport) -> String {
    let mut s = String::new();
    s.push_str(&format!("# Adversarial Review: {}\n\n", r.verdict.as_str()));
    s.push_str(&format!("- critical {} / high {} / medium {} / low {} ({} total)\n\n",
        r.stats.critical, r.stats.high, r.stats.medium, r.stats.low, r.stats.total));
    for sev in [Severity::Critical, Severity::High, Severity::Medium, Severity::Low] {
        let group: Vec<_> = r.findings.iter().filter(|f| f.severity == sev).collect();
        if group.is_empty() { continue; }
        s.push_str(&format!("## {}\n", sev.as_str()));
        for f in group {
            s.push_str(&format!("- **{}** `{}:{}` _(lens {}, models {}, agree {})_\n  {}\n  → {}\n",
                f.title, f.file, f.line_start,
                f.lenses.join(", "), f.models.join(", "), f.agreement,
                f.body, f.recommendation));
            if let Some(v) = &f.verify {
                let tag = match v.verdict {
                    crate::domain::VerifyVerdict::Refuted => "refuted",
                    crate::domain::VerifyVerdict::Upheld => "upheld",
                };
                s.push_str(&format!("  _[verify: {} by {}]_\n", tag, v.model));
            }
        }
        s.push('\n');
    }
    if !r.clusters.is_empty() {
        s.push_str("## Correlated clusters\n");
        for c in &r.clusters {
            s.push_str(&format!("- **{}** ({}): {} — {}\n", c.theme, c.relation, c.note, c.finding_ids.join(", ")));
        }
        s.push('\n');
    }
    if !r.next_steps.is_empty() {
        s.push_str("## next steps\n");
        for step in &r.next_steps {
            s.push_str(&format!("- {}\n", step));
        }
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    fn report() -> ReviewReport {
        ReviewReport { schema_version:"1".into(), mode:Mode::Code, verdict:Verdict::Reject,
            stats:Stats{high:1,total:1,..Default::default()},
            findings:vec![Finding{ id:"1".into(),severity:Severity::High,title:"boom".into(),body:"b".into(),
                file:"a.rs".into(),line_start:3,line_end:5,confidence:0.9,recommendation:"fix".into(),
                lenses:vec!["L1".into()],models:vec!["claude".into()],agreement:1,hard_constraint:false,
                quote:"snippet".into(),cluster_id:None,verify:None}],
            clusters:vec![], next_steps:vec![], run:RunMeta::default() }
    }
    #[test]
    fn json_contains_verdict_and_provenance() {
        let s = render(&report(), Format::Json);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["verdict"], "reject");
        assert_eq!(v["findings"][0]["models"][0], "claude");
    }
    #[test]
    fn md_groups_and_shows_location() {
        let s = render(&report(), Format::Md);
        assert!(s.contains("reject") && s.contains("a.rs:3") && s.contains("boom"));
    }
    #[test]
    fn md_no_debug_vec_rendering() {
        let s = render(&report(), Format::Md);
        // 不含 Debug-vec 格式（如 `["L1"]`）
        assert!(!s.contains("[\""), "md output must not contain Debug-vec like [\"...\"]");
        // 含 lens 名称（不带括号引号）
        assert!(s.contains("L1"), "md output must contain lens name L1");
        // verdict 是小写
        assert!(s.contains("reject"), "verdict must be lowercase 'reject'");
        // severity 标题是小写
        assert!(s.contains("## high"), "severity heading must be lowercase 'high'");
    }
    #[test]
    fn md_cluster_renders_theme_relation_note() {
        use crate::domain::finding::Cluster;
        let mut r = report();
        r.clusters = vec![Cluster {
            id: "C1".into(),
            theme: "null-deref cluster".into(),
            finding_ids: vec!["1".into()],
            relation: "overlapping".into(),
            note: "share same root cause".into(),
            severity: None,
        }];
        let s = render(&r, Format::Md);
        assert!(s.contains("null-deref cluster"), "cluster theme must appear");
        assert!(s.contains("overlapping"), "cluster relation must appear");
        assert!(s.contains("share same root cause"), "cluster note must appear");
    }
    #[test]
    fn md_next_steps_section_rendered_when_non_empty() {
        let mut r = report();
        r.next_steps = vec!["fix the null check".into(), "add unit test".into()];
        let s = render(&r, Format::Md);
        assert!(s.contains("## next steps"), "next_steps section must appear");
        assert!(s.contains("fix the null check"));
        assert!(s.contains("add unit test"));
    }
    #[test]
    fn md_next_steps_absent_when_empty() {
        let s = render(&report(), Format::Md);
        assert!(!s.contains("## next steps"), "next_steps section must not appear when empty");
    }
    #[test]
    fn md_marks_refuted_verify() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut r = report();
        r.findings[0].verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "gpt".into(), note: "guarded".into() });
        let s = render(&r, Format::Md);
        assert!(s.to_lowercase().contains("refuted"), "md must mark refuted findings");
    }
}
