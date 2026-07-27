use redteam::cli::{run_code_review, ReviewArgs};
use redteam::config::Config;
use redteam::domain::Verdict;
use redteam::evidence::git::FileChange;
use redteam::provider::tool_loop::{AssistantTurn, FakeChatBackend, ToolLoopProvider};
use redteam::tools::ToolCall;
use serde_json::json;

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
            cached_input_tokens: 0,
            finish_reason: None,
        },
        AssistantTurn { text: findings_json.into(), tool_calls: vec![], input_tokens: 3, output_tokens: 5, cached_input_tokens: 0, finish_reason: None },
    ]);
    let provider = ToolLoopProvider::new(Box::new(backend), root.path().to_path_buf(), true);

    let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
    let args = ReviewArgs {
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, models: Some("m".into()),
        models_all: false,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
    };
    let changes = vec![FileChange { path: "src/auth.rs".into(), diff: "+    db.write(token);".into() }];

    let (report, code) = run_code_review(&args, &provider, &cfg, changes, root.path(), None, None).await.unwrap();

    // 编造引用被 positioning 丢弃 → 只剩 1 条
    assert_eq!(report.findings.len(), 1, "fabricated finding must be dropped");
    let f = &report.findings[0];
    assert_eq!(f.file, "src/auth.rs");
    assert_eq!(f.line_start, 3, "db.write(token); is on line 3");
    assert_eq!(f.quote, "db.write(token);");
    // 编造的是 critical；被丢后只剩 high → 仍 ≥ medium block → reject、exit 10
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
    // usage 跨工具轮累加（2+3, 2+5）
    assert_eq!(report.run.usage.input_tokens, 5);
    assert_eq!(report.run.usage.output_tokens, 7);
}

#[tokio::test]
async fn code_replicate_two_models_yield_agreement_two() {
    use redteam::cli::{run_code_review, ReviewArgs};
    use redteam::config::Config;
    use redteam::domain::{Severity, Verdict};
    use redteam::evidence::git::FileChange;
    use redteam::provider::registry::ProviderRegistry;
    use redteam::provider::fake::FakeProvider;
    use redteam::domain::RawFinding;

    fn rf(file: &str, quote: &str, sev: Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: "shared issue".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(),
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
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, models: None, models_all: false,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

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
    use redteam::cli::{run_code_review, ReviewArgs};
    use redteam::config::Config;
    use redteam::domain::{Severity, Verdict};
    use redteam::evidence::git::FileChange;
    use redteam::provider::registry::ProviderRegistry;
    use redteam::provider::fake::FakeProvider;
    use redteam::domain::RawFinding;

    fn rf_low(file: &str, quote: &str) -> RawFinding {
        RawFinding {
            severity: Severity::Low, title: "minor".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(),
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
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, models: None, models_all: false,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

    assert_eq!(report.verdict, Verdict::Approve);   // 仅 low → approve
    assert!(report.run.failed_models.contains(&"ghost".to_string()));
    assert_eq!(code, 2);                            // approve 但对照集不完整
}

#[tokio::test]
async fn code_replicate_lone_blocking_refuted_stays_approve() {
    use redteam::cli::{run_code_review, ReviewArgs};
    use redteam::config::Config;
    use redteam::domain::{Severity, Verdict, VerifyVerdict, RawFinding};
    use redteam::evidence::git::FileChange;
    use redteam::provider::registry::ProviderRegistry;
    use redteam::provider::fake::FakeProvider;

    fn rf(file: &str, quote: &str, title: &str, sev: Severity) -> RawFinding {
        RawFinding { severity: sev, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into() }
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
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, models: None, models_all: false,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

    let f = report.findings.iter().find(|f| f.file == "src/a.rs").expect("finding present");
    assert_eq!(f.verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted, "孤证被交叉 refute 证伪");
    assert_eq!(f.confidence, 0.0, "refuted 降 confidence 下 floor");
    assert_eq!(report.verdict, Verdict::Approve, "证伪后不翻 verdict");
    assert!(code == 0 || code == 2, "approve → 0/2，非 10");
}

/// M17-a Task 4：增量轮冒烟锚（进程外 e2e，走 `run_review_dispatch` 真路由，非 lib 内部单测）。
/// ①临时仓 commit A → 全量 review（branch scope）产出 1 条 finding，落库为 prior job
///  （模拟 `--background` 持久化：`incremental_preflight` 只信已入库的作业）；
/// ②commit B 造 delta：旧问题行原样保留（验携带存活）+ 新增一处问题行；
/// ③`--incremental-from <prior>` 跑增量轮：fake provider 只报新问题，携带项由 carry 机制
///  就近重锚补入 → 断言携带(origin=Carried)+新(origin=New)两条在场、`run.incremental`、
///  `run.delta_base`、verdict/exit code 映射。完整矩阵（drop/dispute/re-raise 等）已在
///  cli.rs 的 `run_incremental_review` 单测覆盖，本锚只证「端到端走得通」。
#[tokio::test]
async fn incremental_round_smoke() {
    use redteam::cli::{exit_code, run_review_dispatch};
    use redteam::domain::{Origin, RawFinding, Severity, Verdict};
    use redteam::provider::fake::FakeProvider;
    use redteam::state::{CellInput, CellRecord};

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
    fn commit_all(dir: &std::path::Path, msg: &str) -> String {
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", msg]);
        git(dir, &["rev-parse", "HEAD"]).trim().to_string()
    }

    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    // .redteam/ 是测试副作用（proj.db 落库 prior job）——ignore 掉，防止污染后续 commit 的 delta 统计。
    std::fs::write(dir.path().join(".gitignore"), ".redteam/\n").unwrap();
    let c0 = commit_all(dir.path(), "c0: empty baseline");
    git(dir.path(), &["branch", "dev0", &c0]);

    // commit A：引入一处真实问题（unvalidated write）。
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();
    let c1 = commit_all(dir.path(), "commit A: introduce finding");

    // 两 model（m + m2）：refute 挑交叉 model 时需要一个真实的「非自审」对象——单 model 场景下
    // pick_cross_model 会退化自审，FakeProvider 对 review/refute 两种调用回同一份脚本，自审趟
    // 会把刚报出的孤证当「非空回复」误判 refuted（title 无 upheld/refuted 前缀走违约兜底），
    // confidence 归零、静默吞掉 blocking 力——这不是本锚要测的东西（cross-refute 契约已有专门
    // 单测覆盖），m2 恒空脚本 = 交叉 refute 稳定判 Upheld，让 verdict 走真实、可预期的路径。
    let cfg = Config::from_layers(&[r#"[providers.m]
kind="openai"
model="m1"
priority=10
[providers.m2]
kind="openai"
model="m2model"
priority=5
[review]
default_models=["m","m2"]
"#]).unwrap();

    // ① 全量评审（branch scope，base=dev0=c0）：fake provider 出 1 条 finding（m2 恒空）。
    let full_args = ReviewArgs {
        base: Some("dev0".into()), scope: "branch".into(),
        spec: vec![], focus: None, models: None, models_all: false,
        block_threshold: None, conf_floor: None, background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None,
    };
    let mut full_scripted: std::collections::BTreeMap<String, Vec<RawFinding>> = std::collections::BTreeMap::new();
    full_scripted.insert("m".into(), vec![RawFinding {
        severity: Severity::High, title: "unvalidated write".into(), body: "b".into(),
        file: "src/a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
        recommendation: "validate".into(), hard_constraint: false, quote: "db.write(token);".into(),
    }]);
    full_scripted.insert("m2".into(), vec![]);
    let full_provider = FakeProvider { scripted: full_scripted, tokens: (0, 0, 0) };
    let (prior_report, _prior_code) =
        run_review_dispatch(&full_args, &full_provider, &cfg, dir.path(), None).await.unwrap();
    assert_eq!(prior_report.findings.len(), 1, "全量轮应锚定出 1 条 finding");
    assert_eq!(prior_report.run.head_sha.as_deref(), Some(c1.as_str()), "R3 stamp = 全量证据收集刻 HEAD");

    // 落库为 prior job（模拟 --background 持久化——incremental_preflight 只信「已入库」的作业）。
    let job_id = "prior1";
    let store = redteam::state::JobStore::open(dir.path()).unwrap();
    store.write_args_raw(job_id, &serde_json::to_string(&full_args).unwrap()).unwrap();
    store.write_result(job_id, &serde_json::to_string(&prior_report).unwrap()).unwrap();
    store.write_manifest(job_id, &[CellInput {
        cell: "bundle[src/a.rs]".into(), model: "m".into(), prompt: "p".into(),
    }]).unwrap();
    store.append_partial(job_id, &CellRecord::ok(
        "bundle[src/a.rs]", "m", prior_report.findings.clone(),
    )).unwrap();

    // ② commit B 造 delta：旧问题行原样保留（验携带存活），新增一处问题行。
    std::fs::write(
        dir.path().join("src/a.rs"),
        "fn f() {\n    db.write(token);\n    leak(secret);\n}\n",
    ).unwrap();
    let _c2 = commit_all(dir.path(), "commit B: new leak");

    // ③ 增量轮：fake provider 只报新问题（m2 仍恒空——同一原因，供 refute 挑交叉 model）。
    let inc_args = ReviewArgs { incremental_from: Some(job_id.into()), ..full_args.clone() };
    let mut inc_scripted: std::collections::BTreeMap<String, Vec<RawFinding>> = std::collections::BTreeMap::new();
    inc_scripted.insert("m".into(), vec![RawFinding {
        severity: Severity::High, title: "secret leak".into(), body: "b".into(),
        file: "src/a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
        recommendation: "fix".into(), hard_constraint: false, quote: "leak(secret);".into(),
    }]);
    inc_scripted.insert("m2".into(), vec![]);
    let inc_provider = FakeProvider { scripted: inc_scripted, tokens: (0, 0, 0) };
    let (report, code) =
        run_review_dispatch(&inc_args, &inc_provider, &cfg, dir.path(), None).await.unwrap();

    assert!(report.run.incremental, "增量轮标记应为 true");
    assert_eq!(report.run.delta_base.as_deref(), Some(c1.as_str()), "delta_base = 上轮完整 stamped sha");

    let carried = report.findings.iter().find(|f| f.origin == Origin::Carried)
        .expect("携带项（origin=Carried）应在场");
    assert_eq!(carried.title, "unvalidated write");
    let fresh = report.findings.iter().find(|f| f.origin == Origin::New)
        .expect("新 finding（origin=New）应在场");
    assert_eq!(fresh.title, "secret leak");
    assert_eq!(report.findings.len(), 2, "携带 + 新 各一条，恰好两条");

    // 携带项（High、confidence 0.9、verify=Upheld（第一轮交叉 refute 继承））恒 blocking，与新 finding 的 refute 结果无关
    // →本场景 verdict 确定性为 Reject（非泛泛的「合法枚举」断言，锚住实际计算路径）。
    assert_eq!(report.verdict, Verdict::Reject, "携带的 High finding 应 blocking");
    assert!(report.run.failed_models.is_empty(), "双 model 全程可达");
    let expected_code = exit_code(report.verdict, !report.run.failed_models.is_empty());
    assert_eq!(code, 10, "reject → exit 10");
    assert_eq!(code, expected_code, "exit code 必须与 verdict 映射一致（非硬编码脱钩）");
}
