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

    let cfg = Config::from_layers(&[r#"[providers.alpha]
kind="openai"
model="a"
priority=10
[review]
default_models=["alpha"]
"#])
    .unwrap();

    // 单 model 报一条 High（孤证 blocking）——既锚 Fanout 又必然触发 Refute 子阶段打点
    // （agreement==1 且 severity>=block_threshold）；单 model 交叉 refute 退化自审，
    // FakeProvider 对 review/refute 两种调用回同一份脚本，无 upheld/refuted 前缀走违约
    // 兜底判 Refuted（既有 `code_replicate_lone_blocking_refuted_stays_approve` 已证此
    // 契约）→ confidence 归零、不 blocking → 全链路应稳定收敛到 Approve/exit 0（非 flake）。
    let mut scripted = std::collections::BTreeMap::new();
    scripted.insert(
        "alpha".to_string(),
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

    let args = ReviewArgs {
        base: None, scope: "working-tree".into(), spec: vec![], focus: None,
        models: None, models_all: false, block_threshold: None, conf_floor: None,
        background: true, format: "json".into(), resume: None, dismissed: None,
        no_dismissed: false, incremental_from: None, tools: None,
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
