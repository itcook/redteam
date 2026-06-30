use breakpoints::cli::{run_code_review, run_review, ReviewArgs};
use breakpoints::config::Config;
use breakpoints::domain::{Severity, Verdict};
use breakpoints::evidence::git::FileChange;
use breakpoints::provider::fake::FakeProvider;
use breakpoints::provider::tool_loop::{AssistantTurn, FakeChatBackend, ToolLoopProvider};
use breakpoints::tools::ToolCall;
use serde_json::json;


#[tokio::test]
async fn doc_review_end_to_end_rejects() {
    use breakpoints::domain::RawFinding;
    use std::collections::BTreeMap;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("spec.md");
    std::fs::write(&p, "# design\nlet x = 1;\n").unwrap();
    let ps = p.to_string_lossy().to_string();

    // file 必须与 target 路径匹配，anchor_doc 才能在文档内容中定位 quote。
    // 两个 model 都报同一 High finding → agreement=2 → 跳过 verify → Reject。
    let finding = || RawFinding {
        severity: Severity::High,
        title: "auth missing".into(),
        body: "b".into(),
        file: ps.clone(),
        line_start: 1,
        line_end: 1,
        confidence: 0.9,
        recommendation: "add check".into(),
        hard_constraint: false,
        quote: "let x = 1;".into(),
        lens: None,
    };
    let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
    scripted.insert("claude".into(), vec![finding()]);
    scripted.insert("claude2".into(), vec![finding()]);
    let provider = FakeProvider { scripted };
    let cfg = Config::from_layers(&[
        "[providers.claude]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
         [providers.claude2]\nkind=\"openai\"\nmodel=\"b\"\npriority=5\n\
         [review]\ndefault_models=[\"claude\",\"claude2\"]\n",
    ]).unwrap();
    let args = ReviewArgs {
        mode: Some("doc".into()),
        base: None,
        scope: "working-tree".into(),
        spec: vec![],
        focus: None,
        lenses: Some("D1".into()),
        models: None,
        models_all: false,
        no_correlate: true,
        block_threshold: None,
        conf_floor: None,
        background: false,
        format: "json".into(),
        targets: vec![ps],
    };

    let (report, code) = run_review(args, &provider, &cfg).await.unwrap();
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
    // 两 model 合并为 agreement=2 的单条 finding
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].agreement, 2);
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
        models_all: false,
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

#[tokio::test]
async fn code_replicate_two_models_yield_agreement_two() {
    use breakpoints::cli::{run_code_review, ReviewArgs};
    use breakpoints::config::Config;
    use breakpoints::domain::{Severity, Verdict};
    use breakpoints::evidence::git::FileChange;
    use breakpoints::provider::registry::ProviderRegistry;
    use breakpoints::provider::fake::FakeProvider;
    use breakpoints::domain::RawFinding;

    fn rf(file: &str, quote: &str, sev: Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: "shared issue".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()),
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();

    // default_models=[alpha,beta] → replicate；两 model 对同位置同引用各报一条
    let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha","beta"]
"#]).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("alpha", Box::new(FakeProvider::with("alpha", vec![rf("src/a.rs", "db.write(token);", Severity::High)])));
    reg.insert("beta", Box::new(FakeProvider::with("beta", vec![rf("src/a.rs", "db.write(token);", Severity::Critical)])));

    let args = ReviewArgs {
        mode: Some("code".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: Some("L1".into()), models: None, models_all: false,
        no_correlate: true, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![],
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path()).await.unwrap();

    // 两 model 同 file 同行同引用 → anchor 行2 → dedup 合并 → 1 条 agreement=2、取最高 severity
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].agreement, 2);
    assert_eq!(report.findings[0].line_start, 2);
    assert_eq!(report.findings[0].severity, Severity::Critical);
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
}

#[tokio::test]
async fn code_replicate_unreachable_model_exits_two() {
    use breakpoints::cli::{run_code_review, ReviewArgs};
    use breakpoints::config::Config;
    use breakpoints::domain::{Severity, Verdict};
    use breakpoints::evidence::git::FileChange;
    use breakpoints::provider::registry::ProviderRegistry;
    use breakpoints::provider::fake::FakeProvider;
    use breakpoints::domain::RawFinding;

    fn rf_low(file: &str, quote: &str) -> RawFinding {
        RawFinding {
            severity: Severity::Low, title: "minor".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()),
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    let x = 1;\n}\n").unwrap();

    // default=[alpha,ghost]：alpha 产 low（approve），ghost 不在 registry → failed_models → exit 2
    let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha","ghost"]
"#]).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("alpha", Box::new(FakeProvider::with("alpha", vec![rf_low("src/a.rs", "let x = 1;")])));

    let args = ReviewArgs {
        mode: Some("code".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: Some("L1".into()), models: None, models_all: false,
        no_correlate: true, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![],
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path()).await.unwrap();

    assert_eq!(report.verdict, Verdict::Approve);   // 仅 low → approve
    assert!(report.run.failed_models.contains(&"ghost".to_string()));
    assert_eq!(code, 2);                            // approve 但对照集不完整
}

#[tokio::test]
async fn doc_review_keeps_omission_drops_fabricated_anchors_contradiction() {
    use breakpoints::cli::{run_review, ReviewArgs};
    use breakpoints::domain::{RawFinding, Severity, Verdict};
    use std::collections::BTreeMap;

    let dir = tempfile::tempdir().unwrap();
    let doc_path = dir.path().join("spec.md");
    std::fs::write(
        &doc_path,
        "intro line\nthe system MUST never retry\nlater the system retries 3 times\nconclusion\n",
    ).unwrap();
    let dp = doc_path.to_string_lossy().to_string();

    fn rf(file: &str, title: &str, quote: &str, sev: Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: quote.into(), lens: None,
        }
    }
    let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
    scripted.insert("alpha".into(), vec![
        rf(&dp, "self-contradiction on retry", "the system MUST never retry", Severity::High),
        rf(&dp, "missing rollback branch", "", Severity::High),                  // 遗漏：空引用
        rf(&dp, "fabricated claim", "this text is nowhere in the doc", Severity::High), // 编造
    ]);
    scripted.insert("beta".into(), vec![
        rf(&dp, "self-contradiction on retry", "the system MUST never retry", Severity::High),
    ]);
    let provider = breakpoints::provider::fake::FakeProvider { scripted };

    let cfg = breakpoints::config::Config::from_layers(&[
        "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
         [providers.beta]\nkind=\"openai\"\nmodel=\"b\"\npriority=5\n\
         [review]\ndefault_models=[\"alpha\",\"beta\"]\n",
    ]).unwrap();

    let args = ReviewArgs {
        mode: Some("doc".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: None, models: None, models_all: false,
        no_correlate: false, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![dp.clone()],
    };
    let (report, code) = run_review(args, &provider, &cfg).await.unwrap();

    // 矛盾：两 model 合并 agreement=2、锚到第 2 行。
    let contra = report.findings.iter()
        .find(|f| f.title.contains("contradiction")).expect("contradiction kept");
    assert_eq!(contra.agreement, 2);
    assert_eq!(contra.line_start, 2, "anchored to the contradicting line");
    // 遗漏：空引用，未被 anchor 丢弃（doc carve-out 的核心）。
    assert!(report.findings.iter().any(|f| f.title.contains("rollback")),
        "omission finding must survive anchor");
    // 编造引文：被丢弃。
    assert!(!report.findings.iter().any(|f| f.title.contains("fabricated")),
        "fabricated quote dropped");
    // verdict：矛盾 High agreement=2（跳过 verify）→ Reject、exit 10。
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
}

#[tokio::test]
async fn code_replicate_lone_blocking_refuted_stays_approve() {
    use breakpoints::cli::{run_code_review, ReviewArgs};
    use breakpoints::config::Config;
    use breakpoints::domain::{Severity, Verdict, VerifyVerdict, RawFinding};
    use breakpoints::evidence::git::FileChange;
    use breakpoints::provider::registry::ProviderRegistry;
    use breakpoints::provider::fake::FakeProvider;

    fn rf(file: &str, quote: &str, title: &str, sev: Severity) -> RawFinding {
        RawFinding { severity: sev, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()) }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();

    // 单 model alpha 自审：review 报一条 High 孤证(agreement=1)；refute 退化自审 alpha，
    // FakeProvider 同脚本对 refute 也返回非空 → refuted → confidence 0 → 不翻 verdict。
    let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha"]
"#]).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("alpha", Box::new(FakeProvider::with("alpha", vec![rf("src/a.rs", "db.write(token);", "missing auth", Severity::High)])));

    let args = ReviewArgs {
        mode: Some("code".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: Some("L1".into()), models: None, models_all: false,
        no_correlate: true, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![],
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path()).await.unwrap();

    let f = report.findings.iter().find(|f| f.file == "src/a.rs").expect("finding present");
    assert_eq!(f.verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted, "孤证被交叉 refute 证伪");
    assert_eq!(f.confidence, 0.0, "refuted 降 confidence 下 floor");
    assert_eq!(report.verdict, Verdict::Approve, "证伪后不翻 verdict");
    assert!(code == 0 || code == 2, "approve → 0/2，非 10");
}
