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
    let verdict = serde_json::to_string(&r.verdict).unwrap().replace('"', "");
    s.push_str(&format!("# 对抗评审：{}\n\n", verdict));
    s.push_str(&format!("- critical {} / high {} / medium {} / low {}（共 {}）\n\n",
        r.stats.critical, r.stats.high, r.stats.medium, r.stats.low, r.stats.total));
    for sev in [Severity::Critical, Severity::High, Severity::Medium, Severity::Low] {
        let group: Vec<_> = r.findings.iter().filter(|f| f.severity == sev).collect();
        if group.is_empty() { continue; }
        s.push_str(&format!("## {:?}\n", sev));
        for f in group {
            s.push_str(&format!("- **{}** `{}:{}` _(lens {:?}, models {:?}, agree {})_\n  {}\n  → {}\n",
                f.title, f.file, f.line_start, f.lenses, f.models, f.agreement, f.body, f.recommendation));
        }
        s.push('\n');
    }
    if !r.clusters.is_empty() {
        s.push_str("## 关联 clusters\n");
        for c in &r.clusters {
            s.push_str(&format!("- **{}** ({}): {} — {:?}\n", c.theme, c.relation, c.note, c.finding_ids));
        }
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
                lenses:vec!["L1".into()],models:vec!["claude".into()],agreement:1,hard_constraint:false,cluster_id:None}],
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
}
