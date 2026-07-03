use crate::domain::{ReviewReport, Severity};

#[derive(Clone, Copy, Debug)]
pub enum Format { Json, Md }

pub fn render(report: &ReviewReport, fmt: Format) -> String {
    match fmt {
        Format::Json => serde_json::to_string_pretty(report).unwrap(),
        Format::Md => render_md(report),
    }
}

/// token 数自适应缩写：≥1M → x.xxM、≥1K → x.xxK、否则原样。
/// 结果进 markdown 表格 cell——禁用裸 `|`（会被解析成列分隔符撕裂表格），缩写用括号。
fn human_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("`{n}` ({:.2}M)", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("`{n}` ({:.2}K)", n as f64 / 1_000.0)
    } else {
        format!("`{n}`")
    }
}

fn sev_emoji(sev: Severity) -> &'static str {
    match sev {
        Severity::Critical => "🔴",
        Severity::High => "🟠",
        Severity::Medium => "🟡",
        Severity::Low => "🔵",
    }
}

/// 人读评审报告（rN.md 模板,用户 2026-07-03 定 + 同日补充调整）：H1 带 #job_id
/// （`-round {n}` 是 ship 协议层概念且 md 在 job 收尾预渲染入库——由 ship 宿主写文件时补）；
/// Summary(verdict/severity 表/Info 表/Warnings 有则显) + Finding Details(severity 在 body 前)。
fn render_md(r: &ReviewReport) -> String {
    let repo = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Redteam".into());
    let verdict = match r.verdict.as_str() {
        "approve" => "✅ approve",
        "reject" => "❌ reject",
        _ => "⚠️ incomplete",
    };
    let mut s = String::new();
    if r.run.job_id.is_empty() {
        s.push_str(&format!("# {repo} Redteam Review\n\n## Summary\n\n"));
    } else {
        s.push_str(&format!("# {repo} Redteam Review #{}\n\n## Summary\n\n", r.run.job_id));
    }
    s.push_str(&format!("- Mode: {}\n\n", if r.mode == crate::domain::Mode::Code { "code" } else { "doc" }));
    s.push_str(&format!("- Verdict: {verdict}\n\n"));
    s.push_str(&format!("- Findings: {}\n", r.stats.total));
    if r.stats.total > 0 {
        s.push('\n');
        s.push_str("  | Severity   | Count |\n  | ---------- | ----- |\n");
        for (e, name, n) in [
            ("🔴", "critical", r.stats.critical),
            ("🟠", "high", r.stats.high),
            ("🟡", "medium", r.stats.medium),
            ("🔵", "low", r.stats.low),
        ] {
            s.push_str(&format!("  | {e} {name} | {n} |\n"));
        }
    }
    // Info 表：Models 显示 model id（缺映射回退 provider 名），failed 标 ❗️fail
    let models: Vec<String> = r
        .run
        .models
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let display = r.run.model_ids.get(i).filter(|s| !s.is_empty()).unwrap_or(name);
            if r.run.failed_models.contains(name) {
                format!("{display} ❗️fail")
            } else {
                display.clone()
            }
        })
        .collect();
    s.push_str("\n- Info\n\n  | Field         | Value |\n  | ------------- | ----- |\n");
    s.push_str(&format!("  | Models        | {} |\n", models.join(", ")));
    s.push_str(&format!("  | Duration      | ≈ {:.1} mins |\n", r.run.duration_ms as f64 / 60_000.0));
    s.push_str(&format!("  | Input Tokens  | {} |\n", human_tokens(r.run.usage.input_tokens)));
    s.push_str(&format!("  | Output Tokens | {} |\n", human_tokens(r.run.usage.output_tokens)));
    if let Some(l) = &r.run.ledger {
        s.push_str(&format!("  | Ledger        | {} card(s) from {} |\n", l.loaded, l.source));
    }
    if !r.run.warnings.is_empty() {
        s.push_str("\n- Warnings\n\n");
        for (i, w) in r.run.warnings.iter().enumerate() {
            s.push_str(&format!("  - W{}: {w}\n", i + 1));
        }
    }
    if !r.findings.is_empty() {
        s.push_str("\n## Finding Details\n");
        let mut idx = 0;
        for sev in [Severity::Critical, Severity::High, Severity::Medium, Severity::Low] {
            for f in r.findings.iter().filter(|f| f.severity == sev) {
                idx += 1;
                s.push_str(&format!("\n### F{idx}: {}\n\n", f.title));
                s.push_str(&format!("Severity: {} {}\n\n", sev_emoji(sev), sev.as_str()));
                s.push_str(&format!("{}\n\n", f.body));
                s.push_str("| Field      | Value |\n| ---------- | ----- |\n");
                s.push_str(&format!("| File       | {} L{}-L{} |\n", f.file, f.line_start, f.line_end));
                s.push_str(&format!("| Found By   | {} |\n", f.models.join(", ")));
                s.push_str(&format!("| Lens       | {} |\n", f.lenses.join(", ")));
                s.push_str(&format!("| Confidence | {:.2} |\n", f.confidence));
                if let Some(v) = &f.verify {
                    let tag = match v.verdict {
                        crate::domain::VerifyVerdict::Refuted => "refuted",
                        crate::domain::VerifyVerdict::Upheld => "upheld",
                    };
                    s.push_str(&format!("| Verify     | {tag} by {} |\n", v.model));
                }
                if f.previously_dismissed {
                    s.push_str("| Dismissed  | previously dismissed (exempt from verdict) |\n");
                }
                // rec 多行时后续行补 `> ` 前缀，防止脱出引用块。
                s.push_str(&format!("\n> **Recommendation**\n>\n> {}\n", f.recommendation.replace('\n', "\n> ")));
            }
        }
    }
    if !r.clusters.is_empty() {
        s.push_str("\n## Correlated Clusters\n\n");
        for c in &r.clusters {
            s.push_str(&format!("- **{}** ({}): {} — {}\n", c.theme, c.relation, c.note, c.finding_ids.join(", ")));
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    fn report() -> ReviewReport {
        ReviewReport { schema_version:"2".into(), mode:Mode::Code, verdict:Verdict::Reject,
            stats:Stats{high:1,total:1,..Default::default()},
            findings:vec![Finding{ id:"1".into(),severity:Severity::High,title:"boom".into(),body:"b".into(),
                file:"a.rs".into(),line_start:3,line_end:5,confidence:0.9,recommendation:"fix".into(),
                lenses:vec!["L1".into()],models:vec!["claude".into()],agreement:1,hard_constraint:false,
                quote:"snippet".into(),cluster_id:None,verify:None,previously_dismissed:false}],
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
    fn md_template_summary_and_finding_details() {
        let s = render(&report(), Format::Md);
        assert!(s.contains("❌ reject"), "verdict emoji 化: {s}");
        assert!(s.contains("a.rs L3-L5") && s.contains("### F1: boom"), "{s}");
        assert!(s.contains("Severity: 🟠 high"), "severity 在 H3 后 body 前: {s}");
        assert!(s.contains("| 🟠 high | 1 |"), "severity 计数表: {s}");
        assert!(s.contains("> **Recommendation**") && s.contains("> fix"), "{s}");
        assert!(s.contains("Redteam Review"), "H1 存在: {s}");
        assert!(!s.contains("#\n"), "job_id 空时 H1 不带悬空 #: {s}");
    }
    #[test]
    fn md_no_debug_vec_rendering() {
        let s = render(&report(), Format::Md);
        // 不含 Debug-vec 格式（如 `["L1"]`）
        assert!(!s.contains("[\""), "md output must not contain Debug-vec like [\"...\"]");
        // 含 lens 名称（不带括号引号）
        assert!(s.contains("L1"), "md output must contain lens name L1");
        assert!(s.contains("reject"), "verdict must be lowercase 'reject'");
    }
    #[test]
    fn md_zero_findings_hides_table_and_details() {
        let mut r = report();
        r.findings.clear();
        r.stats = Stats::default();
        r.verdict = Verdict::Approve;
        let s = render(&r, Format::Md);
        assert!(s.contains("✅ approve"));
        assert!(s.contains("- Findings: 0"));
        assert!(!s.contains("| Severity"), "0 findings 不出计数表: {s}");
        assert!(!s.contains("## Finding Details"), "{s}");
    }
    #[test]
    fn md_info_table_models_tokens_ledger_and_jobid_title() {
        let mut r = report();
        r.run.job_id = "abc123".into();
        r.run.models = vec!["DeepSeek".into(), "Kimi".into()];
        r.run.model_ids = vec!["deepseek-v4".into(), "kimi-k2".into()];
        r.run.failed_models = vec!["Kimi".into()];
        r.run.duration_ms = 528_000;
        r.run.usage = Usage { input_tokens: 2_620_877, output_tokens: 87_187 };
        r.run.ledger = Some(LedgerMeta { loaded: 53, source: ".redteam/proj.db".into() });
        r.run.warnings = vec!["w-one".into()];
        let s = render(&r, Format::Md);
        assert!(s.contains("# breakpoints Redteam Review #abc123") || s.contains("Redteam Review #abc123"), "{s}");
        assert!(s.contains("deepseek-v4, kimi-k2 ❗️fail"), "model id 显示+失败标注: {s}");
        assert!(s.contains("≈ 8.8 mins"), "{s}");
        assert!(s.contains("`2620877` (2.62M)"), "input token 自适应缩写(无裸管道符): {s}");
        assert!(s.contains("`87187` (87.19K)"), "output token 缩写: {s}");
        assert!(s.contains("53 card(s) from .redteam/proj.db"), "ledger 披露: {s}");
        assert!(s.contains("- W1: w-one"), "{s}");
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
    fn md_marks_refuted_verify() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut r = report();
        r.findings[0].verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "gpt".into(), note: "guarded".into() });
        let s = render(&r, Format::Md);
        assert!(s.to_lowercase().contains("refuted"), "md must mark refuted findings");
    }
    #[test]
    fn md_marks_upheld_verify() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut r = report();
        r.findings[0].verify = Some(Verify { verdict: VerifyVerdict::Upheld, model: "gpt".into(), note: String::new() });
        let s = render(&r, Format::Md);
        assert!(s.to_lowercase().contains("upheld"), "md must mark upheld findings");
    }
}
