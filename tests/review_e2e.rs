use breakpoints::cli::{check_multi_model, run_code_review, run_review, ReviewArgs};
use breakpoints::config::Config;
use breakpoints::domain::{RawFinding, Severity, Verdict};
use breakpoints::evidence::git::FileChange;
use breakpoints::provider::fake::FakeProvider;
use breakpoints::provider::tool_loop::{AssistantTurn, FakeChatBackend, ToolLoopProvider};
use breakpoints::tools::ToolCall;
use serde_json::json;

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
        quote: "let x = 1;".into(),
        lens: None,
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
        background: false,
        format: "json".into(),
        targets: vec![p.to_string_lossy().into()],
    };

    let (report, code) = run_review(args, &provider, &cfg).await.unwrap();
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
    assert_eq!(report.findings[0].models, vec!["claude".to_string()]);
}

#[test]
fn multi_model_explicit_is_rejected() {
    // 单个或省略 --models：通过
    assert!(check_multi_model(&None).is_ok());
    assert!(check_multi_model(&Some("claude".into())).is_ok());
    assert!(check_multi_model(&Some("claude,claude".into())).is_ok()); // 去重后只有 1 个

    // 两个不同 provider：拒绝
    let err = check_multi_model(&Some("claude,gpt4".into())).unwrap_err();
    assert!(err.to_string().contains("Plan 2"), "错误应提到 Plan 2: {err}");

    // 三个不同 provider：拒绝
    let err = check_multi_model(&Some("a,b,c".into())).unwrap_err();
    assert!(err.to_string().contains("3"), "错误应包含数量: {err}");
}

#[tokio::test]
async fn code_bundle_tool_loop_anchors_and_drops_fabricated() {
    let root = tempfile::tempdir().unwrap();
    // 真实文件：db.write(token); 在第 3 行
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("src/auth.rs"),
        "fn login() {\n    let token = user_input;\n    db.write(token);\n}\n",
    )
    .unwrap();

    // 模型脚本：先 read_file，再吐两条 finding（一真一编造）
    let findings_json = r#"{"findings":[
        {"severity":"high","title":"unvalidated write","body":"writes unvalidated input","file":"src/auth.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"validate","quote":"db.write(token);","lens":"L1"},
        {"severity":"critical","title":"phantom sqli","body":"fabricated","file":"src/auth.rs","line_start":1,"line_end":1,"confidence":0.95,"recommendation":"x","quote":"execute_raw_sql(evil)","lens":"L1"}
    ]}"#;
    let backend = FakeChatBackend::new(vec![
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![ToolCall { id: "c1".into(), name: "read_file".into(), args: json!({ "path": "src/auth.rs" }) }],
            input_tokens: 2,
            output_tokens: 2,
        },
        AssistantTurn { text: findings_json.into(), tool_calls: vec![], input_tokens: 3, output_tokens: 5 },
    ]);
    let provider = ToolLoopProvider::new(Box::new(backend), root.path().to_path_buf());

    let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
    let args = ReviewArgs {
        mode: Some("code".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: Some("L1".into()), models: Some("m".into()),
        no_correlate: true, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![],
    };
    let changes = vec![FileChange { path: "src/auth.rs".into(), diff: "+    db.write(token);".into() }];

    let (report, code) = run_code_review(&args, &provider, &cfg, changes, root.path()).await.unwrap();

    // 编造引用被 positioning 丢弃 → 只剩 1 条
    assert_eq!(report.findings.len(), 1, "fabricated finding must be dropped");
    let f = &report.findings[0];
    assert_eq!(f.file, "src/auth.rs");
    assert_eq!(f.line_start, 3, "db.write(token); is on line 3");
    assert_eq!(f.quote, "db.write(token);");
    assert_eq!(f.lenses, vec!["L1".to_string()]);
    // 编造的是 critical；被丢后只剩 high → 仍 ≥ medium block → reject、exit 10
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
    // usage 跨工具轮累加（2+3, 2+5）
    assert_eq!(report.run.usage.input_tokens, 5);
    assert_eq!(report.run.usage.output_tokens, 7);
}
