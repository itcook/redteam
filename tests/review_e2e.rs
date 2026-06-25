use breakpoints::cli::{run_review, ReviewArgs};
use breakpoints::config::Config;
use breakpoints::domain::{RawFinding, Severity, Verdict};
use breakpoints::provider::fake::FakeProvider;

fn raw(sev: Severity) -> RawFinding {
    RawFinding {
        severity: sev,
        title: "auth missing".into(),
        body: "b".into(),
        file: "a.rs".into(),
        line_start: 1,
        line_end: 1,
        confidence: 0.9,
        recommendation: "add check".into(),
        hard_constraint: false,
    }
}

#[tokio::test]
async fn doc_review_end_to_end_rejects() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("spec.md");
    std::fs::write(&p, "# design").unwrap();

    let provider = FakeProvider::with("claude", vec![raw(Severity::High)]);
    let cfg = Config::from_layers(&[r#"[review]
default_models=["claude"]
"#])
    .unwrap();
    let args = ReviewArgs {
        mode: Some("doc".into()),
        base: None,
        scope: "working-tree".into(),
        spec: vec![],
        focus: None,
        lenses: Some("D1".into()),
        models: None,
        no_correlate: true,
        block_threshold: None,
        conf_floor: None,
        wait: true,
        background: false,
        format: "json".into(),
        targets: vec![p.to_string_lossy().into()],
    };

    let (report, code) = run_review(args, &provider, &cfg).await.unwrap();
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
    assert_eq!(report.findings[0].models, vec!["claude".to_string()]);
}
