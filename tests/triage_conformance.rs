//! Phase 3 出口：triage 三角色夹具（fake backend）+ 报告契约（3.1/3.3/3.5/3.6/3.7/3.10）。
//!
//! 夹具自造：base 提交带模块文档/兄弟文件，head 引入一条代码执行行；三轴各返回确定性 JSON。
//! 断言的是**契约形状与确定性规则**，不是模型质量。

use redteam::config::EffectiveModel;
use redteam::job::manifest::JobKind;
use redteam::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage};
use redteam::provider::ProviderError;
use redteam::tools::ToolSpec;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(dir).args(args).output().expect("git");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

#[allow(dead_code)]
struct Fixture {
    _tmp: tempfile::TempDir,
    job: PathBuf,
    repo: PathBuf,
    base: String,
    head: String,
}

const APP_HEAD: &str = "function find(id) {\n  return db.query('SELECT * FROM users WHERE id=' + id);\n}\n\nfunction run(cmd) {\n  return eval(cmd);\n}\n";
const APP_BASE: &str = "function find(id) {\n  return db.query('SELECT * FROM users WHERE id=' + id);\n}\n";

fn make_job(config: &str, pr_body: &str, with_pr_context: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("README.md"), "base\n").unwrap();
    std::fs::write(repo.join("src/AGENTS.md"), "# Layout\nModules live under `src/<name>`; entrypoints stay in `src/app.js`.\n").unwrap();
    std::fs::write(repo.join("src/helper.js"), "function helper() {\n  return 1;\n}\n").unwrap();
    std::fs::write(repo.join("src/app.js"), APP_BASE).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("src/app.js"), APP_HEAD).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let job = tmp.path().join("job");
    std::fs::create_dir_all(job.join("config")).unwrap();
    std::fs::create_dir_all(job.join("input")).unwrap();
    std::fs::create_dir_all(job.join("out")).unwrap();
    std::fs::write(job.join("config/executor.redteam.json"), config).unwrap();
    let diff = git(&repo, &["diff", &format!("{base}..{head}")]);
    std::fs::write(job.join("input/diff.patch"), format!("{diff}\n")).unwrap();
    std::fs::write(job.join("input/merge-base.sha"), format!("{base}\n")).unwrap();

    let mut inputs = serde_json::Map::new();
    inputs.insert("diff_patch".into(), decl("input/diff.patch", &job.join("input/diff.patch"), None));
    inputs.insert("merge_base".into(), decl("input/merge-base.sha", &job.join("input/merge-base.sha"), None));
    if with_pr_context {
        let pr = serde_json::json!({
            "schema": "hibot.audit.pr-context/1",
            "title": "Add eval helper",
            "body": pr_body,
            "author": "alice",
            "labels": [],
            "comments": [],
            "linked_issues": [],
            "ci": {"state": "success", "failing": []},
            "changed_files": [{"path": "src/app.js", "additions": 6, "deletions": 0, "status": "modified"}]
        });
        std::fs::write(job.join("input/pr-context.json"), serde_json::to_vec_pretty(&pr).unwrap()).unwrap();
        inputs.insert(
            "pr_context".into(),
            decl("input/pr-context.json", &job.join("input/pr-context.json"), Some("hibot.audit.pr-context/1")),
        );
    }
    let manifest = serde_json::json!({
        "schema": "hibot.audit.job.triage/1",
        "job_id": "J-triage",
        "kind": "triage",
        "repo": {"id": 1, "full_name": "org/repo"},
        "pr": {"number": 7, "head_sha": head, "base_sha": base},
        "executor": {"name": "redteam", "version": "0.1.0"},
        "limits": {"wall_clock_s": 120},
        "paths": {"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"},
        "inputs": inputs,
    });
    std::fs::write(job.join("job.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    Fixture { _tmp: tmp, job, repo, base, head }
}

fn decl(path: &str, abs: &Path, schema: Option<&str>) -> serde_json::Value {
    let bytes = std::fs::read(abs).unwrap();
    let mut v = serde_json::json!({"path": path, "sha256": sha256_hex(&bytes)});
    if let Some(s) = schema {
        v["schema"] = serde_json::json!(s);
    }
    v
}

fn read_report(job: &Path) -> serde_json::Value {
    let p = job.join("out/report.json");
    serde_json::from_slice(&std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn config() -> String {
    serde_json::json!({
        "providers": {"p1": {"kind": "openai", "baseUrl": "http://127.0.0.1:1", "apiKeyEnv": "P1_KEY",
            "models": {"s": {"roles": {"scanner": {}}}, "r": {"roles": {"reviewer": {}}}}}},
        "language": "en_US"
    })
    .to_string()
}

fn turn(text: &str) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        tool_calls: vec![],
        input_tokens: 10,
        output_tokens: 2,
        cached_input_tokens: 0,
        finish_reason: Some("stop".into()),
        reasoning_tokens: None,
    }
}

/// 三轴共享一个 backend：按 system/首块里的 `# Axis:` 分派确定性 JSON。
#[derive(Clone)]
struct TriageFake {
    security: String,
    drift: String,
    claims: String,
    slice: String,
}

fn flatten(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for m in messages {
        match m {
            ChatMessage::System(s) | ChatMessage::User(s) => out.push_str(s),
            ChatMessage::UserBlocks(bs) => out.push_str(&redteam::prompt::flatten_blocks(bs)),
            ChatMessage::Assistant { text, .. } => out.push_str(text),
            ChatMessage::ToolResults(_) => {}
        }
        out.push('\n');
    }
    out
}

#[async_trait::async_trait]
impl ChatBackend for TriageFake {
    async fn turn(&self, messages: &[ChatMessage], _tools: &[ToolSpec]) -> Result<AssistantTurn, ProviderError> {
        let text = flatten(messages);
        if text.contains("# Axis: security") {
            Ok(turn(&self.security))
        } else if text.contains("# Axis: convention drift") {
            Ok(turn(&self.drift))
        } else if text.contains("# Axis: intent claims") {
            Ok(turn(&self.claims))
        } else if text.contains("# Axis: intent slice") {
            Ok(turn(&self.slice))
        } else {
            Err(ProviderError::Unavailable("unknown triage axis".into()))
        }
    }
}

fn fake_override<'a>(fake: Arc<TriageFake>) -> Box<redteam::cli::BackendOverride<'a>> {
    Box::new(move |_em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> { Some(Box::new(fake.clone())) })
}

fn good_fake() -> Arc<TriageFake> {
    Arc::new(TriageFake {
        security: serde_json::json!({"rows": [{
            "file": "src/app.js", "line_start": 5, "line_end": 6,
            "quote": "return eval(cmd);", "surface": 4, "confidence": 9,
            "reach": "HTTP body field cmd -> eval at line 6", "note": "eval on request input"
        }]})
        .to_string(),
        drift: serde_json::json!({"rows": [{
            "file": "src/app.js", "line_start": 5, "line_end": 5,
            "quote": "function run(cmd) {", "drift": "structural", "category": "placement",
            "standard": "src/AGENTS.md#layout", "note": "entrypoint helper added to app.js"
        }]})
        .to_string(),
        claims: serde_json::json!({
            "claims": [{"claim": "add input validation", "source": "body", "note": ""}],
            "maintainer_overlap": {"flagged": false, "author": "", "quote": ""},
            "notes": []
        })
        .to_string(),
        slice: serde_json::json!({
            "delivered": [],
            "scope_creep": [{"title": "adds eval helper", "files": ["src/app.js"], "note": "not stated"}],
            "notes": []
        })
        .to_string(),
    })
}

#[tokio::test]
async fn triage_ok_has_nine_keys_nested_routing_and_scanner_findings() {
    let fx = make_job(&config(), "This PR adds input validation for user input.", true);
    let fake = good_fake();
    let over = fake_override(fake);
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["schema"], "hibot.audit.report.triage/1");
    assert_eq!(r["kind"], "triage");

    let s = &r["summary"];
    for k in [
        "security_tier", "blockers_count", "structural_count", "local_count", "intent_undelivered",
        "scope_creep_count", "risk", "convention_drift", "fit", "routing",
    ] {
        assert!(s.get(k).is_some(), "missing summary key {k}: {r}");
    }
    assert!(s.get("focus").is_none(), "routing 不得提到顶层: {r}");
    assert_eq!(s["security_tier"], 2, "面 4 ∧ conf 9 → tier 2");
    assert_eq!(s["structural_count"], 1);
    assert_eq!(s["intent_undelivered"], 1);
    assert_eq!(s["blockers_count"], 2);
    assert_eq!(s["convention_drift"], "structural");
    assert_eq!(s["fit"], "needs-scope-decision");
    assert!(s["routing"]["wall_clock_s"].as_u64().unwrap() <= 120);
    assert!(s["routing"]["specs"].as_array().unwrap().contains(&serde_json::json!("src/AGENTS.md")));

    // findings = 1 security + 1 structural drift；source.role = scanner。
    let findings = r["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 2, "{r}");
    assert!(findings.iter().all(|f| f["source"]["role"] == "scanner"));
    assert!(findings.iter().any(|f| f["title"].as_str().unwrap().contains("Security (surface 4)")));
    // 指纹按契约可复算（title/quote 来自报告本身）。
    for f in findings {
        let expect = redteam::job::fingerprint::of(f["file"].as_str().unwrap(), f["quote"].as_str().unwrap(), f["title"].as_str().unwrap());
        assert_eq!(f["fingerprint"], expect);
    }

    // 3.6：blockers 是披露面，不是 findings 的复制。
    let blockers = r["extensions"]["redteam"]["blockers"].as_array().unwrap();
    assert_eq!(blockers.len(), 2, "{r}");
    assert!(blockers.iter().any(|b| b["kind"] == "structural"));
    assert!(blockers.iter().any(|b| b["kind"] == "intent" && b.get("file").is_none()));
    // 两个数组形状不同：findings 有契约指纹/severity，blockers 只有定位信息与 kind。
    assert!(findings.iter().all(|f| f.get("fingerprint").is_some() && f.get("kind").is_none()));
    assert!(blockers.iter().all(|b| b.get("fingerprint").is_none() && b.get("kind").is_some()));

    let axes = r["extensions"]["redteam"]["axes"].as_array().unwrap();
    assert_eq!(axes.len(), 3, "{r}");
    assert!(axes.iter().all(|a| a["ok"] == true), "{r}");
}

#[tokio::test]
async fn axis_failure_is_partial_named_and_writes_no_summary() {
    let fx = make_job(&config(), "This PR adds input validation for user input.", true);
    let mut fake = (*good_fake()).clone();
    fake.security = "not json at all".into();
    let over = fake_override(Arc::new(fake));
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0, "partial 已写出报告 → 退出码 0");
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    assert!(r.get("summary").is_none(), "partial 不得用零值凑九键: {r}");
    assert!(r["status_reason"].as_str().unwrap().contains("security"), "{r}");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "triage_axis_failed:security");
    let axes = r["extensions"]["redteam"]["axes"].as_array().unwrap();
    let sec = axes.iter().find(|a| a["axis"] == "security").unwrap();
    assert_eq!(sec["ok"], false);
}

#[tokio::test]
async fn missing_pr_context_is_an_error_report() {
    let fx = make_job(&config(), "body", false);
    let over = fake_override(good_fake());
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error", "{r}");
    assert!(r["status_reason"].as_str().unwrap().contains("pr_context"), "{r}");
}

#[tokio::test]
async fn below_threshold_and_missing_reach_rows_are_dropped_and_disclosed() {
    let fx = make_job(&config(), "body", true);
    let mut fake = (*good_fake()).clone();
    fake.security = serde_json::json!({"rows": [
        {"file": "src/app.js", "line_start": 6, "line_end": 6, "quote": "return eval(cmd);", "surface": 4, "confidence": 7, "reach": "r"},
        {"file": "src/app.js", "line_start": 6, "line_end": 6, "quote": "return eval(cmd);", "surface": 4, "confidence": 9, "reach": ""}
    ]})
    .to_string();
    let over = fake_override(Arc::new(fake));
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["summary"]["security_tier"], 0);
    let sec = r["extensions"]["redteam"]["axes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["axis"] == "security")
        .unwrap();
    assert_eq!(sec["dropped"]["below_threshold"], 1, "{r}");
    assert_eq!(sec["dropped"]["no_reach"], 1, "{r}");
    assert_eq!(sec["dropped"]["dropped_total"], 2, "{r}");
    let rows = sec["dropped"]["dropped_rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().any(|x| x["reason"] == "below_threshold" && x["conf"] == 7 && x["surface"] == 4));
    let drift = r["extensions"]["redteam"]["axes"].as_array().unwrap().iter().find(|a| a["axis"] == "drift").unwrap();
    assert_eq!(drift["dropped"]["dropped_total"], 0, "安全轴的丢弃不得挂到漂移轴");
}

#[tokio::test]
async fn en_us_output_has_no_chinese_residue() {
    let fx = make_job(&config(), "This PR adds input validation for user input.", true);
    let over = fake_override(good_fake());
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    let mut text = serde_json::to_string(&r).unwrap();
    // 被引用的源码与路径可含任意字符；本夹具里没有中文源码。
    text.retain(|c| c != ' ');
    assert!(!text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)), "en_US 报告不得有中文残留: {r}");
}

#[tokio::test]
async fn intent_slice_delivers_claims_and_discloses_counts() {
    let fx = make_job(&config(), "body", true);
    let mut fake = (*good_fake()).clone();
    fake.claims = serde_json::json!({
        "claims": [
            {"claim": "add input validation", "source": "body", "note": ""},
            {"claim": "remove legacy path", "source": "issue#1", "note": ""}
        ],
        "maintainer_overlap": {"flagged": false, "author": "", "quote": ""},
        "notes": []
    })
    .to_string();
    fake.slice = serde_json::json!({
        "delivered": [{"id": "c1", "evidence": [{"file": "src/app.js", "line_start": 5, "line_end": 6}], "note": ""}],
        "scope_creep": [],
        "notes": []
    })
    .to_string();
    let over = fake_override(Arc::new(fake));
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok", "{r}");
    assert_eq!(r["summary"]["intent_undelivered"], 1, "c1 已交付，c2 未兑现");
    let intent = r["extensions"]["redteam"]["axes"].as_array().unwrap().iter().find(|a| a["axis"] == "intent").unwrap();
    assert_eq!(intent["claims"], 2);
    assert_eq!(intent["delivered"], 1);
    assert_eq!(intent["verification_complete"], true);
}

#[tokio::test]
async fn unfinished_intent_slices_do_not_create_intent_blockers() {
    let fx = make_job(&config(), "body", true);
    let mut fake = (*good_fake()).clone();
    fake.slice = "not json".into();
    let over = fake_override(Arc::new(fake));
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    assert!(r.get("summary").is_none());
    let blockers = r["extensions"]["redteam"]["blockers"].as_array().unwrap();
    assert!(!blockers.iter().any(|b| b["kind"] == "intent"), "未验证 claim 不得成 blocker: {r}");
    assert!(blockers.iter().any(|b| b["kind"] == "structural"));
    let intent = r["extensions"]["redteam"]["axes"].as_array().unwrap().iter().find(|a| a["axis"] == "intent").unwrap();
    assert_eq!(intent["ok"], false);
    assert_eq!(intent["verification_complete"], false);
    assert_eq!(intent["claims"], 1, "I0 的 claims 仍披露");
}

/// 3.6 + 3.9 的接缝：review 消费 triage 报告时只读 `summary.routing`（blockers 数组不进投影）。
#[test]
fn review_projection_reads_routing_only_not_blockers() {
    let fx = make_job(&config(), "body", true);
    let _ = fx;
    let triage = serde_json::json!({
        "schema": "hibot.audit.report.triage/1", "kind": "triage", "status": "ok",
        "summary": {"routing": {"focus": ["code-execution@src/app.js"], "specs": ["src/AGENTS.md"], "wall_clock_s": 90}},
        "extensions": {"redteam": {"blockers": [{"kind": "structural", "file": "src/app.js"}, {"kind": "intent", "claim": "x"}]}}
    });
    let repo = fx._tmp.path().join("repo");
    let p = redteam::job::prior::parse(&triage.to_string(), &repo, 1 << 20).unwrap();
    assert_eq!(p.focus.as_deref(), Some("code-execution@src/app.js"));
    assert_eq!(p.wall_clock_s, Some(90));
    assert_eq!(p.specs.len(), 1);
    // 投影结构里没有 blockers/finding 载体：blockers 只经 routing.focus 影响 review。
    assert!(p.disclosure.get("blockers").is_none());
}

/// 墙钟优化：单格预算 = 剩余/波次 × 0.9 —— 5s 慢 backend 在 2s 墙钟下必须 <3.5s 收敛为 partial。
#[derive(Clone)]
struct SlowBackend {
    inner: Arc<TriageFake>,
    delay: std::time::Duration,
}

#[async_trait::async_trait]
impl ChatBackend for SlowBackend {
    async fn turn(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> Result<AssistantTurn, ProviderError> {
        tokio::time::sleep(self.delay).await;
        <TriageFake as ChatBackend>::turn(&self.inner, messages, tools).await
    }
}

#[tokio::test]
async fn slow_cell_is_capped_by_its_share_of_the_job_budget() {
    let fx = make_job(&config(), "body", true);
    let mpath = fx.job.join("job.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&mpath).unwrap()).unwrap();
    m["limits"]["wall_clock_s"] = serde_json::json!(2);
    std::fs::write(&mpath, serde_json::to_vec_pretty(&m).unwrap()).unwrap();

    let slow = SlowBackend { inner: good_fake(), delay: std::time::Duration::from_secs(5) };
    let over: Box<redteam::cli::BackendOverride<'_>> = Box::new(move |_em: &EffectiveModel| {
        Some(Box::new(slow.clone()) as Box<dyn ChatBackend>)
    });
    let t0 = std::time::Instant::now();
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, Some(over.as_ref())).await;
    let elapsed = t0.elapsed();
    assert_eq!(code, 0);
    assert!(elapsed < std::time::Duration::from_millis(3500), "单格封顶后必须提前收敛: {elapsed:?}");
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    assert!(r.get("summary").is_none());
    assert!(r["status_reason"].as_str().unwrap().contains("triage axis"), "{r}");
    assert!(r["extensions"]["redteam"]["axes"].as_array().unwrap().iter().all(|a| a["ok"] == false));
    // 超时诊断：必须带出 attempts / in_flight / finish_reason 线索，便于区分“慢/截断/网关”。
    let intent = r["extensions"]["redteam"]["axes"].as_array().unwrap().iter().find(|a| a["axis"] == "intent").unwrap();
    let err = intent["error"].as_str().unwrap_or_default();
    assert!(err.contains("attempts="), "{err}");
    assert!(err.contains("in_flight_ms="), "{err}");
}

