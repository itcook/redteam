use redteam::cli::{run_code_review, ReviewArgs};
use redteam::config::Config;
use redteam::domain::{Role, Verdict};
use redteam::evidence::git::FileChange;
use redteam::provider::registry::ProviderRegistry;
use redteam::provider::tool_loop::{AssistantTurn, FakeChatBackend};
use std::time::Duration;

fn scan_turn(json: &str) -> AssistantTurn {
    AssistantTurn { text: json.into(), tool_calls: vec![], input_tokens: 3, output_tokens: 5, cached_input_tokens: 1, finish_reason: None, reasoning_tokens: None }
}
/// 每条目一个 repeating fake backend（同 provider `p`，在途帽 8）。
fn scan_registry(entries: &[(&str, &str)]) -> ProviderRegistry {
    let mut reg = ProviderRegistry::new();
    for (key, json) in entries {
        reg.insert_scan(*key, "p", 8, Box::new(FakeChatBackend::repeating(scan_turn(json))), Duration::from_secs(5), 0);
    }
    reg
}
fn args_wt() -> ReviewArgs {
    ReviewArgs { pack_only: false,
        base: None, scope: "working-tree".into(),
        spec: vec![], focus: None,
        block_threshold: None, conf_floor: None,
        background: false, format: "json".into(),
        resume: None, dismissed: None, no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    }
}

/// M21a 批 F-4（原 code_bundle_tool_loop_anchors_and_drops_fabricated）：scanner 疑点经 `anchor_suspicions`——真 quote 行号被改写、
/// 编造 quote 丢弃并记 cell warn；needs 只计数；覆盖自报；scan-only 退出 0；usage 入账；顶层 cells 带 scanner 输出。
#[tokio::test]
async fn scan_anchors_real_suspicion_and_drops_fabricated() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("src/auth.rs"),
        "fn login() {\n    let token = user_input;\n    db.write(token);\n}\n",
    )
    .unwrap();
    let json = r#"{"suspicions":[
        {"file":"src/auth.rs","line_start":1,"line_end":1,"hint":"unvalidated write","category":"L1","confidence":0.9,"quote":"db.write(token);"},
        {"file":"src/auth.rs","line_start":1,"line_end":1,"hint":"phantom sqli","category":"L1","confidence":0.95,"quote":"execute_raw_sql(evil)"}
    ],"needs":[{"path":"src/db.rs","reason":"write contract"}],"scanned_areas":[{"file":"src/auth.rs","ranges":["1-4"]}]}"#;
    let reg = scan_registry(&[("p/m", json)]);
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let changes = vec![FileChange { path: "src/auth.rs".into(), diff: "@@ -3,1 +3,1 @@\n+    db.write(token);".into() }];

    let (report, code) = run_code_review(&args_wt(), &reg, &cfg, changes, root.path(), None, None).await.unwrap();

    assert_eq!((report.verdict, code), (Verdict::ScanOnly, 0));
    let scan = report.scan.as_ref().expect("scan 段");
    assert_eq!(scan.suspicions.len(), 1, "编造 quote 被丢: {:?}", scan.suspicions);
    let s = &scan.suspicions[0].suspicion;
    assert_eq!((s.file.as_str(), s.line_start, s.quote.as_str()), ("src/auth.rs", 3, "db.write(token);"));
    assert_eq!(scan.stats.needs_declared, 1);
    assert_eq!(scan.scanned_coverage, Some(1.0), "改动行 3 在自报 1-4 内");
    assert_eq!(report.cells.len(), 1);
    assert!(report.cells[0].warn.as_deref().unwrap_or("").contains("1 suspicions dropped: quote not found"), "{:?}", report.cells[0].warn);
    assert_eq!(report.cells[0].output.role(), Role::Scanner);
    assert_eq!((report.run.usage.input_tokens, report.run.usage.output_tokens, report.run.usage.cached_input_tokens), (3, 5, 1));
    assert!(report.findings.is_empty() && report.run.failed_models.is_empty());
    assert_eq!(report.run.coverage.completed.get(&Role::Scanner).map(Vec::len), Some(1));
    assert_eq!(report.run.cache_hit_by_cell.len(), 1);
    assert!(report.run.template_hashes.contains_key("shared_head") && report.run.template_hashes.contains_key("scanner_tail"));
    assert_eq!(report.run.provider_inflight_peak.get("p"), Some(&1));
}

/// M21a 批 F-3（原 code_replicate_two_models_yield_agreement_two）：两家族对同 quote 各报一条 → 合并 agreement 2、家族两个、hint 双收。
#[tokio::test]
async fn two_scanner_families_same_quote_yield_agreement_two() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}},"beta":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let reg = scan_registry(&[
        ("p/alpha", r#"{"suspicions":[{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"shared issue","confidence":0.9,"quote":"db.write(token);"}],"scanned_areas":[]}"#),
        ("p/beta", r#"{"suspicions":[{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"write without validation","confidence":0.7,"quote":"db.write(token);"}],"scanned_areas":[]}"#),
    ]);
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
    let (report, code) = run_code_review(&args_wt(), &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

    assert_eq!((report.verdict, code), (Verdict::ScanOnly, 0));
    let scan = report.scan.as_ref().unwrap();
    assert_eq!(scan.suspicions.len(), 1);
    let m = &scan.suspicions[0];
    assert_eq!((m.agreement, m.suspicion.line_start, m.suspicion.confidence), (2, 2, 0.9));
    assert_eq!(m.families, vec!["alpha", "beta"]);
    assert_eq!(m.hints.len(), 2);
    assert_eq!(m.cell_ids, vec!["scan[g1]@p/alpha#1", "scan[g1]@p/beta#1"]);
    assert_eq!(scan.stats.agreement_ge2, 1);
    assert_eq!(report.cells.len(), 2);
}

/// M21a 批 F-5（原 code_replicate_unreachable_model_exits_two）：未注册的 scanner 条目 → 该 cell 失败 → coverage `failed` →
/// verdict 硬 Incomplete、退出码 3（design §8.4；旧「failed_models → 2」不再可达）；可达条目的疑点仍在报告里。
#[tokio::test]
async fn unreachable_scanner_cell_makes_scan_incomplete_exit_three() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    let x = 1;\n}\n").unwrap();
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"alpha":{"roles":{"scanner":{}}},"ghost":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let reg = scan_registry(&[("p/alpha", r#"{"suspicions":[{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"minor","confidence":0.3,"quote":"let x = 1;"}],"scanned_areas":[]}"#)]);
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];
    let (report, code) = run_code_review(&args_wt(), &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

    assert_eq!((report.verdict, code), (Verdict::Incomplete, 3));
    assert_eq!(report.run.coverage.failed.get(&Role::Scanner), Some(&vec!["scan[g1]@p/ghost#1".to_string()]));
    assert_eq!(report.run.coverage.completed.get(&Role::Scanner), Some(&vec!["scan[g1]@p/alpha#1".to_string()]));
    assert!(report.run.failed_models.is_empty(), "scan 路径失败进 coverage，不走 failed_models");
    assert!(report.run.warnings.iter().any(|w| w.contains("scanner not in registry")), "{:?}", report.run.warnings);
    assert!(report.run.warnings.iter().any(|w| w.contains("verdict hard Incomplete")), "{:?}", report.run.warnings);
    let scan = report.scan.as_ref().unwrap();
    assert_eq!((scan.suspicions.len(), scan.stats.cells_ok, scan.stats.cells_failed), (1, 1, 1));
}

/// 评审帽外项：`--incremental-from` 与 `--background` 同给 → 命令层直接退 1，不建 job（否则 worker 内 bail 落成 Failed 作业）。
#[test]
fn incremental_from_background_is_rejected_before_spawning_job() {
    let bin = env!("CARGO_BIN_EXE_redteam");
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir.path()).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.json"), r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let out = std::process::Command::new(bin)
        .current_dir(dir.path())
        .env("REDTEAM_HOME", home.path())
        .args(["review", "--background", "--base", "main", "--incremental-from", "prior1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("M21c"), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!dir.path().join(".redteam").join(redteam::db::DB_FILE).exists(), "不建 job / 不落库");
}

/// M21a 批 F-4：增量轮在 M21c 台账回流前显式报错（不静默降级全量），走 `run_review_dispatch` 真路由。
#[tokio::test]
async fn incremental_from_bails_until_m21c() {
    use redteam::cli::run_review_dispatch;
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let args = ReviewArgs { base: Some("main".into()), scope: "branch".into(), incremental_from: Some("prior1".into()), ..args_wt() };
    let err = run_review_dispatch(&args, &ProviderRegistry::new(), &cfg, dir.path(), None).await.unwrap_err();
    assert!(err.to_string().contains("M21c") && err.to_string().contains("prior1"), "{err}");
}

/// M17-c Task 6（批 F-4 scan 版）：背景 job 冒烟 e2e——真 worker 落盘链路首条端到端锚（C-Q5）：进程内驱动 `run_job`（pub API，
/// main.rs Worker 命令的真实调用点）跑全链路，manifest / partial / state / result 四张表全部经同一个 `Arc<JobStore>` 写入与读回。
#[tokio::test]
async fn background_job_smoke_full_lifecycle_single_store() {
    use redteam::cli::{exit_code, run_job, ReviewArgs};
    use redteam::domain::ReviewReport;
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
    let reg = scan_registry(&[("p/alpha", r#"{"suspicions":[{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"unvalidated write","category":"L1","confidence":0.9,"quote":"db.write(token);"}],"scanned_areas":[{"file":"src/a.rs","ranges":["1-3"]}]}"#)]);

    let args = ReviewArgs { pack_only: false,
        base: None, scope: "working-tree".into(), spec: vec![], focus: None,
        block_threshold: None, conf_floor: None,
        background: true, format: "json".into(), resume: None, dismissed: None,
        no_dismissed: false, incremental_from: None, tools: None, test_command: None,
    };

    let job_id = "smoke1";
    let store = Arc::new(JobStore::open(dir.path()).unwrap());
    store.write_args_raw(job_id, &serde_json::to_string(&args).unwrap()).unwrap();
    store.create_job(job_id, 1_000).unwrap();

    let code = run_job(job_id, &args, &reg, &cfg, dir.path(), 1_000, Arc::clone(&store)).await;

    // ── 终态：Done ──
    let st = store.read_state(job_id).expect("job state 应在场");
    assert_eq!(st.status, JobStatus::Done, "全链路应正常收敛到 Done");
    assert_eq!(st.exit_code, Some(code));
    assert!(st.result_path.is_some());
    assert_eq!(st.roles, vec![Role::Scanner], "on_roles 写 JobState.roles");

    // ── progress / phase_history（Scan 取代 Fanout；无 Refute）──
    assert_eq!((st.progress.total, st.progress.done), (1, 1));
    let phases: Vec<Phase> = st.phase_history.iter().map(|p| p.phase).collect();
    assert!(phases.contains(&Phase::Collecting) && phases.contains(&Phase::Scan) && phases.contains(&Phase::Finalizing), "{phases:?}");
    assert!(!phases.contains(&Phase::Refute) && !phases.contains(&Phase::Fanout), "{phases:?}");

    // ── result 落库可读、verdict scan-only、exit code 映射一致、顶层 cells 带 scanner 输出 ──
    let raw = store.read_result(job_id).expect("result 应可读");
    let report: ReviewReport = serde_json::from_str(&raw).expect("result 应是合法 JSON");
    assert_eq!(report.verdict, Verdict::ScanOnly);
    assert_eq!(report.run.job_id, job_id, "run.job_id 应由 run_job 回填（dispatch 内恒空串）");
    assert_eq!(code, 0);
    assert_eq!(code, exit_code(report.verdict, !report.run.failed_models.is_empty()));
    assert_eq!(report.cells.len(), 1);
    assert_eq!(report.cells[0].output.role(), Role::Scanner);
    assert_eq!(report.scan.as_ref().unwrap().suspicions[0].suspicion.line_start, 2);
    assert_eq!(report.run.cells[0].cell, "scan[g1]@p/alpha#1");

    // ── 全程单一 store ──
    let manifest = store.read_manifest(job_id);
    assert_eq!(manifest.len(), 1, "manifest 应可经同一 store 读回");
    assert!(manifest[0].scan.is_some(), "manifest 带 scan 请求（resume 重放输入）");
    assert_eq!(store.read_partial(job_id).len(), 1, "partial checkpoint 应可经同一 store 读回");
    assert_eq!(store.read_partial(job_id)[0].template_hash.as_deref(), Some(redteam::prompt::scan_template_hash().as_str()), "scanner cell 记录带 head+tail 模板哈希");
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
    let (report, code) = run_resume(job_id, &ProviderRegistry::new(), &cfg, dir.path(), &store).await;
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
    let report_b = ReviewReport { scan: None, cells: vec![],
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
            agreement: 1, suspicion_agreement: 0,
            hard_constraint: false,
            quote: "db.write(token);".into(),
            cluster_id: None,
            verify: None,
            previously_dismissed: false,
            origin: Origin::New, outside_window: false,
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

/// M18 发布覆盖门·测试二（批 F-4 scan 版）：历史台账（`dismiss add`）zero-config 自动加载并在 `run.ledger` 披露——M21a scan-only
/// 只披露不消费（豁免 / refute 裁决与判例前移在 M21c 疑点台账起）；verdict 不受影响。
#[tokio::test]
async fn dismissed_ledger_is_loaded_and_disclosed_in_scan_only() {
    use redteam::orchestrate::finding_id;

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();

    let title = "unvalidated write";
    let file = "src/a.rs";
    let quote = "db.write(token);";
    let id = finding_id(file, quote, title, 1, 1);

    // ① `dismiss add`：登记一条人裁驳回判例。
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

    // ② 同仓跑 scan 评审：root 下 `.redteam/proj.db` 已由上一步建出——zero-config 自动加载（无需 --dismissed）。
    let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
    let reg = scan_registry(&[("p/m", r#"{"suspicions":[{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"unvalidated write","confidence":0.9,"quote":"db.write(token);"}],"scanned_areas":[]}"#)]);
    let changes = vec![FileChange { path: file.into(), diff: "+    db.write(token);".into() }];
    let (report, code) = run_code_review(&args_wt(), &reg, &cfg, changes, dir.path(), None, None).await.unwrap();

    let ledger = report.run.ledger.as_ref().expect("台账应自动加载，run.ledger 应披露");
    assert_eq!(ledger.loaded, 1, "应加载到刚写入的这 1 条判例");
    assert!(ledger.source.contains(redteam::db::DB_FILE), "来源应指向 .redteam/{}: {}", redteam::db::DB_FILE, ledger.source);
    assert!(report.run.dismissed_reappeared.is_empty(), "scan-only 不做豁免标记（M21c）");
    assert_eq!((report.verdict, code), (Verdict::ScanOnly, 0));
    assert_eq!(report.scan.as_ref().unwrap().suspicions.len(), 1);
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
    assert!(text.contains("grouping gate=single groups=1 edges=") && text.contains(" cross_import=") && text.contains(" cross_same_dir=") && text.contains(" pooled_small="), "{text}");
    assert!(text.contains("risk hotspots=") && text.contains(" tier="), "{text}");
    assert!(text.contains(&format!("packs={}", report.packs_dir)), "{text}");

    let packs = std::path::Path::new(&report.packs_dir);
    let cell1 = std::fs::read_to_string(packs.join("cell-1.md")).unwrap();
    assert_eq!(cell1.len(), c.bytes, "落盘即 prompt 本体");
    assert!(cell1.contains("alpha_marker") && cell1.contains("预算试跑"), "prompt 含 0 壳与 focus");
    let summary: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(packs.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["cells"][0]["bytes"], c.bytes);
    assert_eq!(summary["grouping"]["gate"], "single");
    assert_eq!(summary["job_id"], report.job_id);
    assert_eq!(std::fs::read_to_string(dir.path().join(".redteam/.gitignore")).unwrap(), "*\n!.gitignore\n!brief.md\n", "未 init 项目补 .gitignore");
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
    assert!(text.contains("risk hotspots=0 r=0") && text.contains("tier=low"), "empty diff → r=0 low: {text}");
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
    // 每分量 4 文件（超小分量 ≤3 阈值，2026-09-10 前置任务：小分量已与孤立同池，2 文件对会被并格）
    for (rel, body) in [
        ("src/core/a.ts", big("a", "./b")), ("src/core/b.ts", big("b", "./a")),
        ("src/core/e.ts", "import { a } from './a';\nexport const e_side = 1;\n".into()),
        ("src/core/f.ts", "import { b } from './b';\nexport const f_side = 1;\n".into()),
        ("src/other/c.ts", big("c", "./d")), ("src/other/d.ts", big("d", "./c")),
        ("src/other/g.ts", "import { c } from './c';\nexport const g_side = 1;\n".into()),
        ("src/other/h.ts", "import { d } from './d';\nexport const h_side = 1;\n".into()),
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
    assert_eq!(report.cells.len(), 2, "两个 4 文件连通分量（超小分量阈值）→ 两组: {:?}", report.cells);
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

/// brief 与 spec 同进 system，**预算也要同待遇**：不扣的话，小窗 scanner 上 8KB 的 brief
/// 能吃掉三分之一的壳预算，溢出表现是 provider 报错或静默截断，不是可披露的降级
/// （2026-09-29 `/code-review` 指出）。仪表报的 `budget_bytes` 要跟真实那条路一致。
#[test]
fn the_repo_brief_is_charged_against_the_pack_budget() {
    let bin = env!("CARGO_BIN_EXE_redteam");
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir.path()).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "pub fn alpha_marker() {}\n").unwrap();
    git(&["add", "-A"]);
    let home = tempfile::tempdir().unwrap();
    let budget = || -> usize {
        let out = std::process::Command::new(bin)
            .current_dir(dir.path())
            .env("REDTEAM_HOME", home.path())
            .args(["review", "--pack-only"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        let packs = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("packs=").map(str::to_string))
            .expect("packs 行");
        let sum: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(std::path::Path::new(&packs).join("summary.json")).unwrap()).unwrap();
        sum["budget_bytes"].as_u64().unwrap() as usize
    };

    let before = budget();
    let body = "简介".repeat(500); // 1500 字节（每个汉字 3 字节）
    std::fs::create_dir_all(dir.path().join(".redteam")).unwrap();
    std::fs::write(dir.path().join(".redteam/brief.md"), &body).unwrap();
    let after = budget();

    assert_eq!(before - after, body.len(), "brief 占的字节要从 pack 预算里扣掉");
}

/// 老项目的 `.gitignore` 升级要挂在**真实路径**上：存量用户早就 init 过、不会专门再跑一次 init，
/// 不升级就等于 brief.md 的修复到不了他们手上（2026-09-29 `/code-review` 指出）。
#[test]
fn a_legacy_gitignore_is_upgraded_by_review_not_only_by_init() {
    let bin = env!("CARGO_BIN_EXE_redteam");
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir.path()).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "pub fn alpha_marker() {}\n").unwrap();
    git(&["add", "-A"]);
    // 老项目形态：init 过、`.gitignore` 是光秃秃一个 `*`
    std::fs::create_dir_all(dir.path().join(".redteam")).unwrap();
    std::fs::write(dir.path().join(".redteam/.gitignore"), "*\n").unwrap();
    let home = tempfile::tempdir().unwrap();

    let out = std::process::Command::new(bin)
        .current_dir(dir.path())
        .env("REDTEAM_HOME", home.path())
        .args(["review", "--pack-only"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));

    assert_eq!(
        std::fs::read_to_string(dir.path().join(".redteam/.gitignore")).unwrap(),
        "*\n!.gitignore\n!brief.md\n",
        "跑一次评审就该把老项目的 .gitignore 升级掉"
    );

    // 用户改过的不碰
    std::fs::write(dir.path().join(".redteam/.gitignore"), "*\n!mine.txt\n").unwrap();
    let out2 = std::process::Command::new(bin)
        .current_dir(dir.path())
        .env("REDTEAM_HOME", home.path())
        .args(["review", "--pack-only"])
        .output()
        .unwrap();
    assert_eq!(out2.status.code(), Some(0));
    assert_eq!(std::fs::read_to_string(dir.path().join(".redteam/.gitignore")).unwrap(), "*\n!mine.txt\n");
}

/// M21d 批 B-1（design §10.5）：`.redteam/brief.md` 进**稳定前缀**里的 `<repo_brief>` 段；
/// 没有它时 prompt 与今日逐字节一致。走真实二进制 + 真实落盘 prompt，钉的是跨层接线。
#[test]
fn repo_brief_enters_the_stable_prefix_and_is_absent_without_the_file() {
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
    let run = || {
        let out = std::process::Command::new(bin)
            .current_dir(dir.path())
            .env("REDTEAM_HOME", home.path())
            .args(["review", "--pack-only"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        let packs = String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("packs=").map(str::to_string))
            .expect("packs 目录行");
        (std::fs::read_to_string(std::path::Path::new(&packs).join("cell-1.md")).unwrap(), String::from_utf8_lossy(&out.stderr).into_owned())
    };

    // ① 没有 brief：今日形态（空槽占位），不多不少
    let (without, _) = run();
    assert!(without.contains("<repo_brief>\n(none)\n</repo_brief>"), "缺失时是空槽占位: {}", &without[..without.len().min(600)]);

    // ② 有 brief：内容进稳定前缀，且位置在 spec_docs 之前（design §10.3 段序）
    std::fs::create_dir_all(dir.path().join(".redteam")).unwrap();
    std::fs::write(dir.path().join(".redteam/brief.md"), "本仓是一个对抗式代码评审器，BRIEF_MARKER_7F。\n").unwrap();
    let (with, _) = run();
    assert!(with.contains("BRIEF_MARKER_7F"), "brief 没进 prompt");
    let (b, sp) = (with.find("BRIEF_MARKER_7F").unwrap(), with.find("<spec_docs>").unwrap());
    assert!(b < sp, "仓库简介要排在 spec 之前（design §10.3 段序）");
    // 除 brief 段外，其余前缀逐字节不变——cache 前缀的稳定性就是这条
    assert_eq!(
        with.replace("本仓是一个对抗式代码评审器，BRIEF_MARKER_7F。\n", "(none)\n").replace("\n(none)\n\n", "\n(none)\n"),
        without.replace("\n(none)\n\n", "\n(none)\n"),
        "brief 之外的前缀不该有任何变化"
    );

    // ③ brief 里粘了私钥 → 整份跳过 + 披露（它是仓内文件，任何 PR 都能改）
    std::fs::write(
        dir.path().join(".redteam/brief.md"),
        "简介\n-----BEGIN OPENSSH PRIVATE KEY-----\nZm9v\n-----END OPENSSH PRIVATE KEY-----\n",
    ).unwrap();
    let (guarded, stderr) = run();
    assert!(!guarded.contains("PRIVATE KEY"), "私钥不能进 prompt");
    assert!(stderr.contains("疑似私钥"), "要说清为什么没注入: {stderr}");
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
    assert_eq!((c.pack.body.as_str(), c.pack.files, c.pack.sliced, c.pack.ladder), ("slice", 1, 1, 0), "{:?}", c.pack);
    assert!(c.pack.folded_lines > 500 && c.pack.outside_sites == 1, "{:?}", c.pack);
    let packs = std::path::Path::new(&slice.packs_dir);
    let cell_slice = std::fs::read_to_string(packs.join("cell-1.md")).unwrap();
    assert!(cell_slice.contains("src/big.rs:1-610（切片：核 1") && cell_slice.contains("行折叠：unrelated_big）"), "{cell_slice}");
    assert!(cell_slice.contains("## 文件外片段\n") && cell_slice.contains("src/user.rs:1-4（调用点：changed_target）"), "{cell_slice}");
    assert!(!cell_slice.contains("let u7 = 7;"), "无关体折叠: {cell_slice}");
    let text = render_pack_only(&slice);
    assert!(text.contains(" body=slice sliced=1 small=0 ladder=0 folded_lines=") && text.contains(" outside_sites=1 outside_sigs=0 contracts=0 contract_misses=0 contract_spilled=0"), "{text}");
    let summary: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(packs.join("summary.json")).unwrap()).unwrap();
    assert_eq!(summary["cells"][0]["pack"]["body"], "slice");
    assert_eq!(summary["cells"][0]["pack"]["sliced"], 1);

    // full 档（对照臂，K10 / K14：配置切档，无 CLI 旗）
    let cfg_full = Config::from_json(r#"{"review":{"pack":{"body":"full"}}}"#).unwrap();
    let full = run_pack_only(&args, &cfg_full, dir.path()).unwrap();
    let f = &full.cells[0];
    assert_eq!((f.pack.body.as_str(), f.pack.full, f.pack.sliced, f.pack.outside_sites), ("full", 1, 0, 0), "{:?}", f.pack);
    assert!(f.pack_bytes > c.pack_bytes, "full {} > slice {}", f.pack_bytes, c.pack_bytes);
    let cell_full = std::fs::read_to_string(std::path::Path::new(&full.packs_dir).join("cell-1.md")).unwrap();
    assert!(cell_full.contains("src/big.rs:1-610（全文，610 行）") && cell_full.contains("let u7 = 7;"), "{cell_full}");
    assert!(!cell_full.contains("## 文件外片段\n"), "full 档无文件外片段段（前言散文提到该段名不算）: {cell_full}");
}

/// B-1 两处同口径（M21c 批 B）：`--pack-only` 与真实扇出对同一输入必须产出同一判例段。
///
/// 这条测 pack-only 那一侧；真实扇出那一侧在 `orchestrate::scan_tests` 里（两套 prompt 构造，
/// 只测一侧等于没测另一侧——本仓栽过这个坑）。这里另外钉住：判例来自 `.redteam/proj.db`，
/// 而 `--no-dismissed` 必须把它整段关掉。
#[test]
fn pack_only_injects_precedents_and_honors_no_dismissed() {
    use redteam::cli::{run_pack_only, ReviewArgs};
    use redteam::config::Config;
    let dir = tempfile::tempdir().unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").current_dir(dir.path()).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn a() {\n    risky();\n}\n").unwrap();
    git(&["add", "-A"]); // working-tree scope 要的是已入 index 的改动，untracked 不算

    // 判例卡走 dismiss_upsert 写入（不手搓 DB 行——夹具要经真实写入口）
    redteam::project::init_project(dir.path()).unwrap();
    let mut db = redteam::db::ProjDb::open(dir.path()).unwrap();
    db.dismiss_upsert(&redteam::db::DismissCard {
        id: "abcdef0123456789".into(), status: "dismissed".into(),
        title: "risky() 是本仓的测试桩".into(), file: "src/a.rs".into(),
        quote: "risky();".into(), severity: "high".into(),
        reason: "PACKONLY_PRECEDENT_MARK".into(), round: "r1".into(), updated_at: String::new(),
    })
    .unwrap();
    drop(db);

    let args = |no_dismissed: bool| ReviewArgs {
        pack_only: true,
        base: None, scope: "working-tree".into(), spec: vec![], focus: None,
        block_threshold: None, conf_floor: None, background: false,
        format: "json".into(), resume: None, dismissed: None, no_dismissed, incremental_from: None,
        tools: None, test_command: None,
    };
    let read_packs = |root: &std::path::Path| -> String {
        let packs = root.join(".redteam").join("packs");
        let mut out = String::new();
        for job in std::fs::read_dir(&packs).unwrap().filter_map(Result::ok) {
            for cell in std::fs::read_dir(job.path()).unwrap().filter_map(Result::ok) {
                out.push_str(&std::fs::read_to_string(cell.path()).unwrap());
            }
        }
        out
    };

    let r = run_pack_only(&args(false), &Config::default(), dir.path()).unwrap();
    assert_eq!(r.cells.len(), 1, "{:?}", r.warnings);
    let with = read_packs(dir.path());
    assert!(with.contains("PACKONLY_PRECEDENT_MARK"), "判例没进 pack-only 的 scanner prompt");
    assert!(with.contains("<precedents>"), "{with}");

    // `--no-dismissed` = 本次评审完全关闭台账 → 判例段必须是空槽占位
    std::fs::remove_dir_all(dir.path().join(".redteam").join("packs")).unwrap();
    let r2 = run_pack_only(&args(true), &Config::default(), dir.path()).unwrap();
    assert_eq!(r2.cells.len(), 1);
    let without = read_packs(dir.path());
    assert!(!without.contains("PACKONLY_PRECEDENT_MARK"), "--no-dismissed 下不得注入判例: {without}");
    assert!(without.contains("(none)\n</precedents>"), "要留空槽占位: {without}");
}
