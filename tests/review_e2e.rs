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
    let provider = ToolLoopProvider::new(Box::new(backend), root.path().to_path_buf());

    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let args = ReviewArgs { pack_only: false,
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
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
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}},"beta":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("p/alpha", Box::new(FakeProvider::with("p/alpha", vec![rf("src/a.rs", "db.write(token);", Severity::High)])));
    reg.insert("p/beta", Box::new(FakeProvider::with("p/beta", vec![rf("src/a.rs", "db.write(token);", Severity::Critical)])));

    let args = ReviewArgs { pack_only: false,
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
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
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}},"ghost":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("p/alpha", Box::new(FakeProvider::with("p/alpha", vec![rf_low("src/a.rs", "let x = 1;")])));

    let args = ReviewArgs { pack_only: false,
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

    assert_eq!(report.verdict, Verdict::Approve);   // 仅 low → approve
    assert!(report.run.failed_models.contains(&"p/ghost".to_string()));
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
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("p/alpha", Box::new(FakeProvider::with("p/alpha", vec![rf("src/a.rs", "db.write(token);", "missing auth", Severity::High)])));

    let args = ReviewArgs { pack_only: false,
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
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
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}},"m2":{"roles":{"scanner":{}}}}}}}"#).unwrap();

    // ① 全量评审（branch scope，base=dev0=c0）：fake provider 出 1 条 finding（m2 恒空）。
    let full_args = ReviewArgs { pack_only: false,
        base: Some("dev0".into()), scope: "branch".into(),
        spec: vec![], focus: None,
        block_threshold: None, conf_floor: None, background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };
    let mut full_scripted: std::collections::BTreeMap<String, Vec<RawFinding>> = std::collections::BTreeMap::new();
    full_scripted.insert("p/m".into(), vec![RawFinding {
        severity: Severity::High, title: "unvalidated write".into(), body: "b".into(),
        file: "src/a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
        recommendation: "validate".into(), hard_constraint: false, quote: "db.write(token);".into(),
    }]);
    full_scripted.insert("p/m2".into(), vec![]);
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
        cell: "bundle[src/a.rs]".into(), model: "p/m".into(), prompt: "p".into(),
    }]).unwrap();
    store.append_partial(job_id, &CellRecord::ok(
        "bundle[src/a.rs]", "p/m", redteam::domain::CellOutput::review(prior_report.findings.clone()),
    )).unwrap();

    // ② commit B 造 delta：旧问题行原样保留（验携带存活），新增一处问题行。
    std::fs::write(
        dir.path().join("src/a.rs"),
        "fn f() {\n    db.write(token);\n    leak(secret);\n}\n",
    ).unwrap();
    let _c2 = commit_all(dir.path(), "commit B: new leak");

    // ③ 增量轮：fake provider 只报新问题（m2 仍恒空——同一原因，供 refute 挑交叉 model）。
    let inc_args = ReviewArgs { pack_only: false, incremental_from: Some(job_id.into()), ..full_args.clone() };
    let mut inc_scripted: std::collections::BTreeMap<String, Vec<RawFinding>> = std::collections::BTreeMap::new();
    inc_scripted.insert("p/m".into(), vec![RawFinding {
        severity: Severity::High, title: "secret leak".into(), body: "b".into(),
        file: "src/a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
        recommendation: "fix".into(), hard_constraint: false, quote: "leak(secret);".into(),
    }]);
    inc_scripted.insert("p/m2".into(), vec![]);
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

/// M17-c Task 6：背景 job 冒烟 e2e——真 worker 落盘链路首条端到端锚（C-Q5）。
///
/// 方案①（真 spawn `redteam review --background` 二进制 + fake provider 经 config 注入）
/// 已判死：`[providers.*].kind` 只收 anthropic/openai/openai_compatible/google 四种真实网络
/// backend（`backend_from_config`/`ProviderKind` 收口），`FakeProvider` 是纯 Rust 代码内桩，
/// 无法经配置文件被选中——真 spawn 意味着必须打真实模型 API，不可行也不该为此引入网络依赖/
/// 密钥。改走方案②：进程内驱动 `run_job`（pub API，main.rs Worker 命令的真实调用点）跑全
/// 链路——这正是本批（M17-c）要锚的「worker 全生命周期单连接贯穿」的可观测面：manifest/
/// partial/state/result 四张表全部经同一个 `Arc<JobStore>` 写入与读回。
#[tokio::test]
async fn background_job_smoke_full_lifecycle_single_store() {
    use redteam::cli::{exit_code, run_job, ReviewArgs};
    use redteam::config::Config;
    use redteam::domain::{RawFinding, ReviewReport, Severity, Verdict};
    use redteam::provider::fake::FakeProvider;
    use redteam::state::{JobStatus, JobStore, Phase};
    use std::sync::Arc;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@t"]);
    git(dir.path(), &["config", "user.name", "t"]);
    git(dir.path(), &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();
    git(dir.path(), &["add", "src/a.rs"]); // working-tree scope 读 staged 改动

    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}}}}}}"#).unwrap();

    // 单 model 报一条 High（孤证 blocking）——既锚 Fanout 又必然触发 Refute 子阶段打点
    // （agreement==1 且 severity>=block_threshold）；单 model 交叉 refute 退化自审，
    // FakeProvider 对 review/refute 两种调用回同一份脚本，无 upheld/refuted 前缀走违约
    // 兜底判 Refuted（既有 `code_replicate_lone_blocking_refuted_stays_approve` 已证此
    // 契约）→ confidence 归零、不 blocking → 全链路应稳定收敛到 Approve/exit 0（非 flake）。
    let mut scripted = std::collections::BTreeMap::new();
    scripted.insert(
        "p/alpha".to_string(),
        vec![RawFinding {
            severity: Severity::High,
            title: "unvalidated write".into(),
            body: "b".into(),
            file: "src/a.rs".into(),
            line_start: 1,
            line_end: 1,
            confidence: 0.9,
            recommendation: "validate".into(),
            hard_constraint: false,
            quote: "db.write(token);".into(),
        }],
    );
    let provider = FakeProvider { scripted, tokens: (0, 0, 0) };

    let args = ReviewArgs { pack_only: false,
        base: None, scope: "working-tree".into(), spec: vec![], focus: None,
        block_threshold: None, conf_floor: None,
        background: true, format: "json".into(), resume: None, dismissed: None,
        no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };

    // 模拟 main.rs 后台派发起手态（写 args 快照 + 起手 Pending）：单一 `JobStore::open`
    // 此后全程复用（Arc 化），与真实 worker 进程「main 建 store → run_job/sink 全生命周期
    // 共享同一连接」的落点等价——这条 store 之后不再重开，manifest/partial/状态/result
    // 全部经它写、也经它读回，直接锚「一条连接跑完整 job 生命周期」。
    let job_id = "smoke1";
    let store = Arc::new(JobStore::open(dir.path()).unwrap());
    store.write_args_raw(job_id, &serde_json::to_string(&args).unwrap()).unwrap();
    store.create_job(job_id, 1_000).unwrap();

    let code = run_job(job_id, &args, &provider, &cfg, dir.path(), 1_000, Arc::clone(&store)).await;

    // ── 终态：Done，非 Failed/TimedOut ──────────────────────────────────
    let st = store.read_state(job_id).expect("job state 应在场");
    assert_eq!(st.status, JobStatus::Done, "全链路应正常收敛到 Done");
    assert_eq!(st.exit_code, Some(code), "落盘 exit_code 应与 run_job 返回值一致");
    assert!(st.result_path.is_some(), "result_path 应回填");

    // ── progress/phase_history 有痕（worker 单连接贯穿要锚的可观测面）───────────
    assert_eq!(st.progress.total, 1, "manifest 应写 1 个 cell");
    assert_eq!(st.progress.done, 1, "sink 应推进 1 个 cell 完成");
    let phases: Vec<Phase> = st.phase_history.iter().map(|p| p.phase).collect();
    assert!(phases.contains(&Phase::Collecting), "起手应打 Collecting");
    assert!(phases.contains(&Phase::Fanout), "manifest 写出后应打 Fanout");
    assert!(phases.contains(&Phase::Finalizing), "尾段前应打 Finalizing");
    assert!(phases.contains(&Phase::Refute), "孤证 blocking 应触发 refute 子阶段打点");

    // ── result 落库可读、verdict 合法、exit code 与 verdict 映射一致 ─────────────
    let raw = store.read_result(job_id).expect("result 应可读");
    let report: ReviewReport = serde_json::from_str(&raw).expect("result 应是合法 JSON");
    assert_eq!(report.verdict, Verdict::Approve, "孤证被自审 refute 证伪后不应 blocking");
    assert_eq!(report.run.job_id, job_id, "run.job_id 应由 run_job 回填（dispatch 内恒空串）");
    assert!(report.run.failed_models.is_empty(), "单 model 全程可达");
    let expected_code = exit_code(report.verdict, !report.run.failed_models.is_empty());
    assert_eq!(code, 0, "approve 且对照集完整 → exit 0");
    assert_eq!(code, expected_code, "exit code 必须与 verdict 映射一致（非硬编码脱钩）");

    // ── 全程单一 store：同一 Arc<JobStore> 既是 run_job 的落盘方，也是本处的校验读方——
    // manifest/partial 两张表经它可读，证明写读全在一条连接上，未散落到别的打开实例。
    assert_eq!(store.read_manifest(job_id).len(), 1, "manifest 应可经同一 store 读回");
    assert_eq!(store.read_partial(job_id).len(), 1, "partial checkpoint 应可经同一 store 读回");
}

/// T47③：REDTEAM_TEST_PANIC 钩子 + 真 panic 全链路 e2e
/// 真 spawn 二进制，设 REDTEAM_TEST_PANIC=1，fan-out 前触发 panic；
/// panic hook 落 Failed 终态；断言封盘不变式（终态落定后不被覆盖）。
#[test]
fn worker_panic_hook_lands_failed_and_seals() {
    use redteam::state::{JobState, JobStatus, JobStore};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path();

    // 预建 job 记录（worker 起手需要 args_raw + create_job）
    let job_id = "panic-e2e-1";
    let store = JobStore::open(run_dir).unwrap();
    let fake_args = serde_json::json!({
        "base": null, "scope": "working-tree", "spec": [], "focus": null,
        "models": null, "models_all": false, "block_threshold": null, "conf_floor": null,
        "background": true, "format": "json", "resume": null, "dismissed": null,
        "no_dismissed": false, "incremental_from": null, "tools": null
    });
    store.write_args_raw(job_id, &fake_args.to_string()).unwrap();
    store.create_job(job_id, 0).unwrap();

    // spawn 真二进制，设 REDTEAM_TEST_PANIC=1
    let bin = env!("CARGO_BIN_EXE_redteam");
    let status = std::process::Command::new(bin)
        .arg("worker")
        .arg("--job-id")
        .arg(job_id)
        .env("REDTEAM_TEST_PANIC", "1")
        .current_dir(run_dir)
        .status()
        .expect("spawn 真二进制失败");
    // panic 导致非零退出
    assert!(!status.success(), "panic 应导致非零退出");

    // 轮询终态（最多 5 秒）
    let deadline = Instant::now() + Duration::from_secs(5);
    let final_state: JobState = loop {
        let st = store.read_state(job_id).expect("job state 应在场");
        if st.status.is_terminal() {
            break st;
        }
        assert!(Instant::now() < deadline, "超时：job 未在 5s 内落终态");
        std::thread::sleep(Duration::from_millis(50));
    };

    // 断言 Failed + error 含 panic 消息
    assert_eq!(final_state.status, JobStatus::Failed, "panic 应落 Failed 终态");
    let err = final_state.error.as_deref().unwrap_or("");
    assert!(
        err.contains("REDTEAM_TEST_PANIC"),
        "error 字段应含 panic 消息，实际: {err:?}"
    );

    // 封盘不变式：sleep 100ms 后再读，状态仍是 Failed
    std::thread::sleep(Duration::from_millis(100));
    let st2 = store.read_state(job_id).expect("二次读 state 应在场");
    assert_eq!(st2.status, JobStatus::Failed, "封盘：终态不应被后续写覆盖");
}

/// 3-3①：exit 1——CLI 参数校验失败（--conf-floor 越界）→ 进程退出码 1。
/// 注：CLI 无 --config 参数；--conf-floor 越界是最简的命令层 exit 1 触发路径
/// （main.rs 显式 `process::exit(1)`，五档契约：命令自身失败）。
#[test]
fn exit_code_one_on_invalid_conf_floor() {
    let bin = env!("CARGO_BIN_EXE_redteam");
    let dir = tempfile::tempdir().unwrap();
    let status = std::process::Command::new(bin)
        .args(["review", "--conf-floor", "5.0"])
        .current_dir(dir.path())
        .status()
        .expect("spawn binary");
    assert_eq!(status.code(), Some(1), "--conf-floor 越界应退出码 1");
}

/// 3-3②：exit 3——run_resume 空 manifest（job 在 fan-out 前崩溃）→ Verdict::Incomplete → exit 3。
#[tokio::test]
async fn exit_code_three_on_incomplete_resume() {
    use redteam::cli::{exit_code, run_resume};
    use redteam::config::Config;
    use redteam::domain::Verdict;
    use redteam::provider::fake::FakeProvider;
    use redteam::state::JobStore;

    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path()).unwrap();
    let job_id = "inc1";
    // 写 args 但不写 manifest → run_resume 走 partial_report → Verdict::Incomplete → exit 3
    let fake_args = serde_json::json!({
        "base": null, "scope": "working-tree", "spec": [], "focus": null,
        "models": null, "models_all": false, "block_threshold": null, "conf_floor": null,
        "background": false, "format": "json", "resume": null, "dismissed": null,
        "no_dismissed": false, "incremental_from": null, "tools": null
    });
    store.write_args_raw(job_id, &fake_args.to_string()).unwrap();

    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let provider = FakeProvider { scripted: std::collections::BTreeMap::new(), tokens: (0, 0, 0) };
    let (report, code) = run_resume(job_id, &provider, &cfg, dir.path(), &store).await;
    assert_eq!(report.verdict, Verdict::Incomplete, "空 manifest → Incomplete");
    assert_eq!(code, 3, "Incomplete → exit 3");
    assert_eq!(exit_code(Verdict::Incomplete, false), 3, "exit_code 映射一致");
}

/// M18 发布覆盖门·测试一：`result --format md` 三态（真 spawn 二进制，走 `resolve_result` 真路由）。
/// ①`rN.md` 在场 → 逐字直出，不重渲染；②`rN.md` 缺但 `rN.result.json` 在场 → 现算 `render_md`
/// 出结构化 markdown（D3，M18批2：不静默回落成 json 格式）；③两者皆缺 → 报错、退出码 1。
#[test]
fn result_md_three_states() {
    use redteam::domain::{Finding, Origin, ReviewReport, RunMeta, Severity, Stats, Verdict};
    use redteam::state::{JobStatus, JobStore};

    let dir = tempfile::tempdir().unwrap();
    let store = JobStore::open(dir.path()).unwrap();
    let bin = env!("CARGO_BIN_EXE_redteam");

    fn spawn_result_md(bin: &str, dir: &std::path::Path, job_id: &str) -> std::process::Output {
        std::process::Command::new(bin)
            .args(["result", job_id, "--format", "md"])
            .current_dir(dir)
            .output()
            .expect("spawn 真二进制失败")
    }

    // ① rN.md 存在 → 直出该文件内容（逐字，不经 render_md 重算）。
    let id_a = "mdA";
    store.create_job(id_a, 0).unwrap();
    store
        .update_if_active(id_a, |s| {
            s.status = JobStatus::Done;
            s.exit_code = Some(0);
            s.result_path = Some("result.json".into());
        })
        .unwrap();
    let literal_md = "# Literal MD\n\nhand-written content, not re-rendered\n";
    store.write_result_md(id_a, literal_md).unwrap();
    let out_a = spawn_result_md(bin, dir.path(), id_a);
    assert!(out_a.status.success(), "状态①应成功退出: {out_a:?}");
    let stdout_a = String::from_utf8_lossy(&out_a.stdout);
    assert_eq!(
        stdout_a.trim_end(),
        literal_md.trim_end(),
        "rN.md 在场时应逐字直出其内容，不应被重渲染: {stdout_a}"
    );

    // ② rN.md 缺失但 rN.result.json 在场 → 反序列化 ReviewReport 现算 render_md。
    let id_b = "mdB";
    store.create_job(id_b, 0).unwrap();
    store
        .update_if_active(id_b, |s| {
            s.status = JobStatus::Done;
            s.exit_code = Some(10);
            s.result_path = Some("result.json".into());
        })
        .unwrap();
    let report_b = ReviewReport {
        schema_version: redteam::domain::SCHEMA_VERSION.into(),
        verdict: Verdict::Reject,
        stats: Stats { high: 1, total: 1, ..Default::default() },
        findings: vec![Finding {
            id: "abc123".into(),
            severity: Severity::High,
            title: "unvalidated write".into(),
            body: "writes unvalidated input".into(),
            file: "src/a.rs".into(),
            line_start: 3,
            line_end: 3,
            confidence: 0.9,
            recommendation: "validate".into(),
            models: vec!["m".into()],
            agreement: 1,
            hard_constraint: false,
            quote: "db.write(token);".into(),
            cluster_id: None,
            verify: None,
            previously_dismissed: false,
            origin: Origin::New,
        }],
        clusters: vec![],
        run: RunMeta { job_id: id_b.into(), models: vec!["m".into()], ..Default::default() },
    };
    store.write_result(id_b, &serde_json::to_string(&report_b).unwrap()).unwrap();
    let out_b = spawn_result_md(bin, dir.path(), id_b);
    // `result` 命令回填该 job 落盘的 exit_code（此处沿用当年 reject 的 10），不是「取回成功=0」；
    // 断言点是「拿到了渲染出的 md」而非进程状态本身。
    assert_eq!(out_b.status.code(), Some(10), "应回填该 job 落盘的 exit_code: {out_b:?}");
    let stdout_b = String::from_utf8_lossy(&out_b.stdout);
    // md 渲染骨架（render_md 段名，非静默回落 JSON）：Summary + Finding Details 两段都在。
    assert!(stdout_b.contains("## Summary"), "应含 Summary 段: {stdout_b}");
    assert!(stdout_b.contains("## Finding Details"), "应含 Finding Details 段: {stdout_b}");
    assert!(stdout_b.contains("### F1: unvalidated write"), "应含现算出的 finding 标题: {stdout_b}");
    assert!(stdout_b.contains("❌ reject"), "verdict 应被渲染: {stdout_b}");
    assert!(!stdout_b.trim_start().starts_with('{'), "不应静默回落成原始 JSON 格式: {stdout_b}");

    // ③ 两者都缺 → 报错，退出码 1。
    let id_c = "mdC";
    store.create_job(id_c, 0).unwrap();
    store
        .update_if_active(id_c, |s| {
            s.status = JobStatus::Done;
            s.exit_code = Some(0);
        })
        .unwrap();
    let out_c = spawn_result_md(bin, dir.path(), id_c);
    assert_eq!(out_c.status.code(), Some(1), "两者皆缺应退出码 1: {out_c:?}");
    let stderr_c = String::from_utf8_lossy(&out_c.stderr);
    assert!(stderr_c.contains("result missing"), "stderr 应报结果缺失: {stderr_c}");
}

/// M18 发布覆盖门·测试二：历史台账（`dismiss add`）标记链路——先建判例卡 → 同 id 复现 →
/// 断言 `previously_dismissed` 标记落地 + 该历史记录确实进入尾段（consolidate/verify，紧随
/// fan-out 之后的「第二阶段」）处理管线，而非停留在台账文件里未被消费。
/// 可观测证据取两处：①`run.ledger` 披露（台账被 zero-config 自动加载并喂入 TailCtx）；
/// ②`run.dismissed_reappeared` 派生索引收到该 id（`mark_dismissed` 是尾段专属步骤，
/// 在 anchor→dedup→verify 之后、compute_verdict 之前，见 `cli::tail::assemble_report`）。
/// 精确 id 命中判例按设计（`refute_targets` 注释）不再进 refute 目标集，故 `verify` 字段
/// 应保持 `None`——一并断言，避免测试预期与代码实际行为脱节。
#[tokio::test]
async fn dismissed_ledger_previously_dismissed_marks_and_feeds_second_stage() {
    use redteam::cli::{run_code_review, ReviewArgs};
    use redteam::config::Config;
    use redteam::domain::{RawFinding, Severity, Verdict};
    use redteam::evidence::git::FileChange;
    use redteam::orchestrate::finding_id;
    use redteam::provider::fake::FakeProvider;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();

    let title = "unvalidated write";
    let file = "src/a.rs";
    let quote = "db.write(token);";
    // finding_id 是 file + 归一化 quote + 归一化 title 的确定性哈希、行号无关（orchestrate::finding_id
    // 文档）——dismiss add 与后续评审各自独立构造 raw finding，仍确定性落到同一 id，无需手工同步行号。
    let id = finding_id(file, quote, title, 1, 1);

    // ① `dismiss add`：先登记一条人裁驳回判例（模拟上一轮 ship 判过此 finding 是误报）。
    let bin = env!("CARGO_BIN_EXE_redteam");
    let add_out = std::process::Command::new(bin)
        .args([
            "dismiss", "add", &id,
            "--reason", "确认误报：token 已在上游校验",
            "--title", title,
            "--file", file,
            "--quote", quote,
            "--severity", "high",
        ])
        .current_dir(dir.path())
        .output()
        .expect("spawn dismiss add 失败");
    assert!(add_out.status.success(), "dismiss add 应成功: {add_out:?}");

    // ② 同一 id 再跑一次评审：FakeProvider 复现同 file/title/quote 的 raw finding，
    // enrich() 算出的 finding.id 与卡上 id 逐字相等。root 下 `.redteam/proj.db` 已由
    // 上一步的 `dismiss add` 建出——zero-config 自动加载（无需 --dismissed）。
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let mut scripted = std::collections::BTreeMap::new();
    scripted.insert(
        "p/m".to_string(),
        vec![RawFinding {
            severity: Severity::High,
            title: title.into(),
            body: "writes unvalidated input".into(),
            file: file.into(),
            line_start: 1,
            line_end: 1,
            confidence: 0.9,
            recommendation: "validate".into(),
            hard_constraint: false,
            quote: quote.into(),
        }],
    );
    let provider = FakeProvider { scripted, tokens: (0, 0, 0) };
    let args = ReviewArgs { pack_only: false,
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };
    let changes = vec![FileChange { path: file.into(), diff: "+    db.write(token);".into() }];
    let (report, code) =
        run_code_review(&args, &provider, &cfg, changes, dir.path(), None, None).await.unwrap();

    let f = report.findings.iter().find(|f| f.id == id).expect("重现的 finding 应在场且 id 与卡一致");
    assert!(f.previously_dismissed, "命中台账的条目应带 previously_dismissed 标记");

    // ③ 该历史记录确实进入了尾段（第二阶段）处理，而非台账文件里的一条死数据：
    // run.ledger 披露证明它被 zero-config 自动加载并喂入 TailCtx（precedents/dismissed 两路）；
    // dismissed_reappeared 派生索引证明 mark_dismissed（尾段专属步骤）真的处理过它。
    let ledger = report.run.ledger.as_ref().expect("台账应自动加载，run.ledger 应披露");
    assert_eq!(ledger.loaded, 1, "应加载到刚写入的这 1 条判例");
    assert!(ledger.source.contains(redteam::db::DB_FILE), "来源应指向 .redteam/{}: {}", redteam::db::DB_FILE, ledger.source);
    assert!(
        report.run.dismissed_reappeared.contains(&id),
        "id 应进入 dismissed_reappeared 派生索引（mark_dismissed 尾段处理的直接产物）"
    );
    // 精确 id 命中判例按设计不再进 refute 目标集（refute_targets：「精确 id 已在台账者不
    // refute，烧 LLM 纯浪费」）——verify 应保持 None，与 mark_dismissed 的豁免语义一致。
    assert!(f.verify.is_none(), "精确 id 命中的历史记录不应再触发 refute（design 明确排除）");

    // previously_dismissed 的 finding 不参与 blocking（R8.2）→ 干净 approve。
    assert_eq!(report.verdict, Verdict::Approve, "已驳回判例命中不应 blocking");
    assert_eq!(code, 0, "approve 且对照集完整 → exit 0");
}

/// T52：clap 解析错误退出码归 1（命令自身失败档）——此前 clap 默认退 2，与五档契约
/// 冲突（2=降级 approve）。真 spawn 二进制四路锚：非法 flag / 非法枚举值 / 裸命令无
/// 子命令均退 1；--help 属正常展示退 0（DisplayHelp 分支不被误伤）。
#[test]
fn clap_parse_error_exits_1_help_exits_0() {
    let bin = env!("CARGO_BIN_EXE_redteam");
    let run = |args: &[&str]| {
        std::process::Command::new(bin)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn 真二进制失败")
            .code()
            .expect("应有退出码")
    };
    assert_eq!(run(&["--badflag"]), 1, "非法 flag 应退 1（命令错误档）");
    assert_eq!(run(&["model", "setup", "--provider", "X", "--model", "m", "--role", "scanner", "--thinking-level", "ultra"]), 1,
        "非法枚举值应退 1");
    assert_eq!(run(&[]), 1, "裸命令缺子命令应退 1");
    assert_eq!(run(&["--help"]), 0, "--help 是正常展示，应退 0");
}

/// M21a 批 C（R-C4）：`--pack-only` 在无 provider / 无模型配置下跑通——只装包落盘，不调 provider。
#[test]
fn pack_only_writes_packs_and_reports_bytes() {
    use redteam::cli::{render_pack_only, run_pack_only, ReviewArgs, PACK_BYTES_PER_TOKEN};
    use redteam::config::Config;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "pub fn alpha_marker() {}\n").unwrap();
    std::fs::write(dir.path().join("src/b.rs"), "fn beta() { alpha_marker(); }\n").unwrap();
    git(dir.path(), &["add", "-A"]); // working-tree scope：staged 新文件即证据

    let args = ReviewArgs {
        pack_only: true,
        base: None, scope: "working-tree".into(), spec: vec![], focus: Some("预算试跑".into()),
        block_threshold: None, conf_floor: None, background: false,
        format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };
    let report = run_pack_only(&args, &Config::default(), dir.path()).expect("pack-only 无需 provider");
    assert_eq!(report.cells.len(), 1, "两文件 < min_files → 门未过单包");
    let c = &report.cells[0];
    assert_eq!((c.cell, c.files, c.over_budget), (1, 2, false));
    assert_eq!(c.est_tokens, (c.bytes as f64 / PACK_BYTES_PER_TOKEN).round() as u64);
    assert!(c.bytes > c.pack_bytes && c.pack_bytes > 0, "prompt 含 dossier: {c:?}");
    assert_eq!(report.budget_bytes, 312_000, "无模型配置 → 128K 窗保守预算");

    let text = render_pack_only(&report);
    assert!(text.contains("cell-1 files=2 bytes=") && text.contains(" est_tokens="), "{text}");
    assert!(text.contains("grouping gate=single groups=1 edges="), "{text}");
    assert!(text.contains(&format!("packs={}", report.packs_dir)), "{text}");

    let packs = std::path::Path::new(&report.packs_dir);
    let cell1 = std::fs::read_to_string(packs.join("cell-1.md")).unwrap();
    assert_eq!(cell1.len(), c.bytes, "落盘即 prompt 本体");
    assert!(cell1.contains("alpha_marker") && cell1.contains("预算试跑"), "prompt 含 0 壳与 focus");
    let summary: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(packs.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["cells"][0]["bytes"], c.bytes);
    assert_eq!(summary["grouping"]["gate"], "single");
    assert_eq!(summary["job_id"], report.job_id);
    assert_eq!(std::fs::read_to_string(dir.path().join(".redteam/.gitignore")).unwrap(), "*\n", "未 init 项目补 .gitignore");
    assert!(!dir.path().join(".redteam/proj.db").exists(), "不建库");
}

#[test]
fn pack_only_empty_changes_reports_zero_cells() {
    use redteam::cli::{render_pack_only, run_pack_only, ReviewArgs};
    use redteam::config::Config;
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir.path()).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    let args = ReviewArgs {
        pack_only: true,
        base: None, scope: "working-tree".into(), spec: vec![], focus: None,
        block_threshold: None, conf_floor: None, background: false,
        format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };
    let report = run_pack_only(&args, &Config::default(), dir.path()).unwrap();
    assert!(report.cells.is_empty());
    assert!(report.warnings.iter().any(|w| w.contains("no reviewable evidence")), "{:?}", report.warnings);
    let text = render_pack_only(&report);
    assert!(text.contains("cells=0\n") && text.contains("warning: no reviewable evidence"), "{text}");
}

/// 修补批 X-3：gate=grouped 两组时，每个 cell 的 `{{OTHER_CHANGED_FILES}}` 索引只列他组文件且带箭头——D 批重写 prompt 渲染的回归网。
#[test]
fn pack_only_two_groups_cross_index_lists_other_group_only() {
    use redteam::cli::{run_pack_only, ReviewArgs};
    use redteam::config::Config;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let big = |name: &str, import: &str| -> String {
        let mut s = format!("import {{ x }} from '{import}';\n");
        for i in 0..70 {
            s.push_str(&format!("export const {name}_{i} = {i};\n"));
        }
        s
    };
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    for (rel, body) in [
        ("src/core/a.ts", big("a", "./b")), ("src/core/b.ts", big("b", "./a")),
        ("src/other/c.ts", big("c", "./d")), ("src/other/d.ts", big("d", "./c")),
    ] {
        let p = dir.path().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    git(dir.path(), &["add", "-A"]);
    let args = ReviewArgs {
        pack_only: true,
        base: None, scope: "working-tree".into(), spec: vec![], focus: None,
        block_threshold: None, conf_floor: None, background: false,
        format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };
    let report = run_pack_only(&args, &Config::default(), dir.path()).unwrap();
    assert_eq!(report.cells.len(), 2, "四文件 / churn ≥ 200 / 两个连通分量 → 两组: {:?}", report.cells);
    let packs = std::path::Path::new(&report.packs_dir);
    let cell1 = std::fs::read_to_string(packs.join("cell-1.md")).unwrap();
    let cell2 = std::fs::read_to_string(packs.join("cell-2.md")).unwrap();
    // 索引行形如 `path (gN +a/-d) → [..] ← [..]`：cell-1 只列 g2，cell-2 只列 g1
    assert!(cell1.contains("src/other/c.ts (g2") && cell1.contains("src/other/d.ts (g2"), "cell-1 索引须列他组文件:\n{cell1}");
    assert!(!cell1.contains("src/core/a.ts (g1") && !cell1.contains("src/core/b.ts (g1"), "cell-1 索引不列自身组:\n{cell1}");
    assert!(cell1.contains("→ [src/other/") && cell1.contains("← [src/other/"), "带正反向箭头:\n{cell1}");
    assert!(cell2.contains("src/core/a.ts (g1") && !cell2.contains("src/other/c.ts (g2"), "cell-2 反之:\n{cell2}");
    let summary: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(packs.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["grouping"]["gate"], "grouped");
    assert_eq!(summary["grouping"]["groups"], 2);
}

/// 修补批 X-3 / X-5：真二进制 `review --pack-only` 退出码 0，且不读 `secrets.json`（坏 secrets 不产 warning）。
#[test]
fn pack_only_binary_exits_zero_without_reading_secrets() {
    let bin = env!("CARGO_BIN_EXE_redteam");
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir.path()).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "pub fn alpha_marker() {}\n").unwrap();
    git(&["add", "-A"]);
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("secrets.json"), "{\"providers\": {").unwrap(); // 非法：pack-only 不该读它
    let out = std::process::Command::new(bin)
        .current_dir(dir.path())
        .env("REDTEAM_HOME", home.path())
        .args(["review", "--pack-only"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    assert!(!stderr.contains("secrets.json"), "--pack-only 不读 secrets（修补批 X-5）: {stderr}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("cell-1 files=1 bytes="), "{}", String::from_utf8_lossy(&out.stdout));
}

/// M21a 批 E（K1 / K13 / K14）：`Config::default()` 即 slice——> 8K 的改动文件走切片、他文件调用点进文件外片段、
/// summary.json / stdout 报装包形态；`review.pack.body = "full"` 回到整文件且无文件外片段（对照臂）。
#[test]
fn pack_only_slices_large_file_and_reports_shape() {
    use redteam::cli::{render_pack_only, run_pack_only, ReviewArgs};
    use redteam::config::Config;

    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "t"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let mut big = String::from("//! big module\nuse std::fmt;\n\npub fn changed_target(x: u8) -> u8 {\n    x + 1\n}\n\nfn unrelated_big() -> u8 {\n");
    for i in 0..600 {
        big.push_str(&format!("    let u{i} = 7;\n"));
    }
    big.push_str("    0\n}\n");
    assert!(big.len() > 8192);
    std::fs::write(dir.path().join("src/big.rs"), &big).unwrap();
    std::fs::write(dir.path().join("src/user.rs"), "/// consumer\nfn consume() -> u8 {\n    changed_target(1)\n}\n").unwrap();
    git(dir.path(), &["add", "-A"]);
    git(dir.path(), &["commit", "-q", "-m", "base"]);
    // 改 changed_target 一行并暂存：working-tree scope 的证据 = 这一个 hunk
    let modified = big.replace("    x + 1\n", "    x + 2\n");
    std::fs::write(dir.path().join("src/big.rs"), &modified).unwrap();
    git(dir.path(), &["add", "-A"]);

    let args = ReviewArgs {
        pack_only: true,
        base: None, scope: "working-tree".into(), spec: vec![], focus: None,
        block_threshold: None, conf_floor: None, background: false,
        format: "json".into(), resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };
    // slice 档（缺省〔D8〕）
    let slice = run_pack_only(&args, &Config::default(), dir.path()).unwrap();
    assert_eq!(slice.cells.len(), 1);
    let c = &slice.cells[0];
    assert_eq!((c.pack.body, c.pack.files, c.pack.sliced, c.pack.ladder), ("slice", 1, 1, 0), "{:?}", c.pack);
    assert!(c.pack.folded_lines > 500 && c.pack.outside_sites == 1, "{:?}", c.pack);
    let packs = std::path::Path::new(&slice.packs_dir);
    let cell_slice = std::fs::read_to_string(packs.join("cell-1.md")).unwrap();
    assert!(cell_slice.contains("src/big.rs:1-610（切片：核 1") && cell_slice.contains("行折叠：unrelated_big）"), "{cell_slice}");
    assert!(cell_slice.contains("## 文件外片段") && cell_slice.contains("src/user.rs:1-4（调用点：changed_target）"), "{cell_slice}");
    assert!(!cell_slice.contains("let u7 = 7;"), "无关体折叠: {cell_slice}");
    let text = render_pack_only(&slice);
    assert!(text.contains(" body=slice sliced=1 small=0 ladder=0 folded_lines=") && text.contains(" outside_sites=1 outside_sigs=0"), "{text}");
    let summary: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(packs.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["cells"][0]["pack"]["body"], "slice");
    assert_eq!(summary["cells"][0]["pack"]["sliced"], 1);

    // full 档（对照臂，K10 / K14：配置切档，无 CLI 旗）
    let cfg_full = Config::from_json(r#"{"review":{"pack":{"body":"full"}}}"#).unwrap();
    let full = run_pack_only(&args, &cfg_full, dir.path()).unwrap();
    let f = &full.cells[0];
    assert_eq!((f.pack.body, f.pack.full, f.pack.sliced, f.pack.outside_sites), ("full", 1, 0, 0), "{:?}", f.pack);
    assert!(f.pack_bytes > c.pack_bytes, "full {} > slice {}", f.pack_bytes, c.pack_bytes);
    let cell_full = std::fs::read_to_string(std::path::Path::new(&full.packs_dir).join("cell-1.md")).unwrap();
    assert!(cell_full.contains("src/big.rs:1-610（全文，610 行）") && cell_full.contains("let u7 = 7;"), "{cell_full}");
    assert!(!cell_full.contains("文件外片段"), "{cell_full}");
}
