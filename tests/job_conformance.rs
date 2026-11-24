//! Phase 1 夹具与管线确定性（计划 1.6）：三夹具 + FakeChatBackend + 契约的本地可校验项。
//!
//! **夹具自造**（D6）：不复制 hibot 的样例。`partial` 用「两个 provider，其中一个 `apiKeyEnv` 不注入」
//! （hibot C5）：p2 真实 backend 连 127.0.0.1:1 必然失败，p1 用确定性 fake。

use redteam::config::EffectiveModel;
use redteam::job::fingerprint;
use redteam::job::manifest::JobKind;
use redteam::provider::tool_loop::{AssistantTurn, ChatBackend, FakeChatBackend};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(dir).args(args).output().expect("git");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn turn(text: &str) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        tool_calls: vec![],
        input_tokens: 12,
        output_tokens: 3,
        cached_input_tokens: 4,
        finish_reason: Some("stop".into()),
        reasoning_tokens: None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    job: PathBuf,
    repo: PathBuf,
    base: String,
}

/// 建一个 base→head 的 git 仓；head 在 `src/app.js` 引入已知漏洞行。
fn make_repo_and_job(kind: &str, config: &str, with_vuln: bool, with_partial_provider: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("README.md"), "base\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let body = if with_vuln {
        "function find(id) {\n  return db.query('SELECT * FROM users WHERE id=' + id);\n}\n"
    } else {
        "function add(a, b) {\n  return a + b;\n}\n"
    };
    std::fs::write(repo.join("src/app.js"), body).unwrap();
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
    std::fs::write(job.join("input/dismissed.csv"), "fingerprint,actor,reason,created_at\n").unwrap();

    let mut inputs = serde_json::Map::new();
    inputs.insert("diff_patch".into(), decl("input/diff.patch", job.join("input/diff.patch").as_path(), None, None));
    inputs.insert("merge_base".into(), decl("input/merge-base.sha", job.join("input/merge-base.sha").as_path(), None, None));
    inputs.insert(
        "dismissed".into(),
        decl("input/dismissed.csv", job.join("input/dismissed.csv").as_path(), None, Some("hibot.audit.dismissed-csv/1")),
    );
    if with_partial_provider {
        std::fs::write(job.join("input/pr-context.json"), r#"{"schema":"hibot.audit.pr-context/1"}"#).unwrap();
        inputs.insert("pr_context".into(), decl("input/pr-context.json", job.join("input/pr-context.json").as_path(), Some("hibot.audit.pr-context/1"), None));
    }
    let manifest = serde_json::json!({
        "schema": format!("hibot.audit.job.{kind}/1"),
        "job_id": format!("J-{kind}"),
        "kind": kind,
        "repo": {"id": 1, "full_name": "org/repo"},
        "pr": {"number": 7, "head_sha": head, "base_sha": base},
        "executor": {"name": "redteam", "version": "0.1.0"},
        "limits": {"wall_clock_s": 120},
        "paths": {"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"},
        "inputs": inputs,
    });
    std::fs::write(job.join("job.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    Fixture { _tmp: tmp, job, repo, base }
}

fn decl(path: &str, abs: &Path, schema: Option<&str>, format: Option<&str>) -> serde_json::Value {
    let bytes = std::fs::read(abs).unwrap();
    let mut v = serde_json::json!({"path": path, "sha256": sha256_hex(&bytes)});
    if let Some(s) = schema { v["schema"] = serde_json::json!(s); }
    if let Some(f) = format { v["format"] = serde_json::json!(f); }
    v
}

fn read_report(job: &Path) -> serde_json::Value {
    let p = job.join("out/report.json");
    serde_json::from_slice(&std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn scanner_json(quote: &str, file: &str, line: u32) -> String {
    serde_json::json!({
        "suspicions": [{"file": file, "line_start": line, "line_end": line, "hint": "sql injection",
                        "category": "L2", "confidence": 0.9, "quote": quote}],
        "needs": [], "scanned_areas": []
    }).to_string()
}

fn reviewer_json(quote: &str, file: &str, line: u32, title: &str) -> String {
    serde_json::json!({
        "findings": [{"severity": "high", "title": title, "body": "harm", "file": file,
                      "line_start": line, "line_end": line, "confidence": 0.9,
                      "recommendation": "parameterize", "quote": quote}],
        "needs": []
    }).to_string()
}

/// p1/s、p1/r 用确定性 fake；p2/scan2 返回 None → 真实 backend（baseUrl 127.0.0.1:1 必失败）。
fn fake_override<'a>(
    scan: Arc<FakeChatBackend>,
    review: Arc<FakeChatBackend>,
) -> Box<redteam::cli::BackendOverride<'a>> {
    Box::new(move |em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> {
        if em.provider != "p1" {
            return None;
        }
        match em.id.as_str() {
            "s" => Some(Box::new(scan.clone())),
            "r" => Some(Box::new(review.clone())),
            _ => None,
        }
    })
}

const QUOTE: &str = "return db.query('SELECT * FROM users WHERE id=' + id);";

fn config_with(lang: Option<&str>, partial: bool) -> String {
    let mut models = serde_json::json!({
        "s": {"roles": {"scanner": {}}},
        "r": {"roles": {"reviewer": {}}}
    });
    if partial {
        models["scan2"] = serde_json::json!({"roles": {"scanner": {}}});
    }
    let mut providers = serde_json::json!({
        "p1": {"kind": "openai", "baseUrl": "http://127.0.0.1:1", "apiKeyEnv": "P1_KEY", "models": models}
    });
    if partial {
        providers["p2"] = serde_json::json!({
            "kind": "openai", "baseUrl": "http://127.0.0.1:1", "apiKeyEnv": "P2_KEY_NOT_SET",
            "models": {"scan2": {}}
        });
        // models 已挂在 p1 下；把 scan2 挪到 p2。
        providers["p1"]["models"].as_object_mut().unwrap().remove("scan2");
        providers["p2"]["models"] = serde_json::json!({"scan2": {"roles": {"scanner": {}}}});
    }
    let mut cfg = serde_json::json!({"providers": providers});
    if let Some(l) = lang {
        cfg["language"] = serde_json::json!(l);
    }
    cfg.to_string()
}

#[tokio::test]
async fn clean_fixture_is_ok_with_empty_findings_and_echoes_input() {
    let scan = Arc::new(FakeChatBackend::repeating(turn(r#"{"suspicions":[],"needs":[],"scanned_areas":[]}"#)));
    let review = Arc::new(FakeChatBackend::repeating(turn(r#"{"findings":[],"needs":[]}"#)));
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), false, false);
    let over = fake_override(scan, review);
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["schema"], "hibot.audit.report.review/1");
    assert_eq!(r["status"], "ok");
    assert!(r["findings"].as_array().unwrap().is_empty());
    assert_eq!(r["input"]["base_sha"], fx.base);
    assert_eq!(r["extensions"]["redteam"]["language"]["value"], "en_US");
    // 报告契约必填
    for k in ["job_id", "kind", "usage", "executor", "findings"] {
        assert!(r.get(k).is_some(), "missing {k}: {r}");
    }
    assert!(r["usage"]["input_tokens"].as_u64().unwrap() > 0);
    // job 模式零写 repo
    assert!(git(&fx.repo, &["status", "--porcelain"]).is_empty(), "repo 必须保持干净");
    assert!(!fx.repo.join(".redteam").exists(), "job 模式不得创建 .redteam");
}

#[tokio::test]
async fn known_vuln_fixture_finds_quote_and_fingerprint_is_stable() {
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, "SQL injection"))));
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    let over = fake_override(scan, review);
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok");
    let findings = r["findings"].as_array().unwrap();
    assert_eq!(findings.len(), 1, "{r}");
    let f = &findings[0];
    let line = 2usize;
    let file = std::fs::read_to_string(fx.repo.join("src/app.js")).unwrap();
    let text = file.lines().nth(line - 1).unwrap();
    let quote = f["quote"].as_str().unwrap();
    assert!(text.contains(quote), "quote 必须逐字命中 head 检出: {quote:?} vs {text:?}");
    assert_eq!(f["fingerprint"], fingerprint::of("src/app.js", quote, f["title"].as_str().unwrap()));
    assert_eq!(f["refuted"], false);
    assert_eq!(f["source"]["role"], "reviewer");

    // 确定性后端连跑两次：指纹集合一致（hibot B3；只对确定性后端成立）。
    let scan2 = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review2 = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, "SQL injection"))));
    let over2 = fake_override(scan2, review2);
    let code2 = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over2.as_ref())).await;
    assert_eq!(code2, 0);
    let r2 = read_report(&fx.job);
    let fps = |v: &serde_json::Value| v["findings"].as_array().unwrap().iter().map(|f| f["fingerprint"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(fps(&r), fps(&r2), "同一夹具两次运行的指纹集合必须一致");
}

#[tokio::test]
async fn partial_fixture_uses_missing_provider_key_and_reports_partial() {
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, "SQL injection"))));
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), true), true, false);
    let over = fake_override(scan, review);
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0, "partial 报告也是已写出 → 0");
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    assert!(r["status_reason"].as_str().unwrap().contains("scan incomplete"), "{r}");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "scan_incomplete:1/2");
}

#[tokio::test]
async fn job_language_comes_from_config_not_env() {
    // 配置 zh_CN → prompt 用中文（不受 LANG 影响）。
    let scan = Arc::new(FakeChatBackend::repeating(turn(r#"{"suspicions":[],"needs":[],"scanned_areas":[]}"#)));
    let review = Arc::new(FakeChatBackend::repeating(turn(r#"{"findings":[],"needs":[]}"#)));
    let fx = make_repo_and_job("review", &config_with(Some("zh_CN"), false), false, false);
    let over = fake_override(scan.clone(), review);
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let sys = match &scan.messages_at(0)[0] {
        redteam::provider::tool_loop::ChatMessage::System(s) => s.clone(),
        other => panic!("首条须是 system: {other:?}"),
    };
    assert!(sys.contains("Simplified Chinese (简体中文)"), "配置语言必须进 prompt: {sys}");
    assert_eq!(read_report(&fx.job)["extensions"]["redteam"]["language"]["value"], "zh_CN");

    // job 模式无配置语言 + LANG=zh_CN → 仍 en_US（不读 env）。
    let scan2 = Arc::new(FakeChatBackend::repeating(turn(r#"{"suspicions":[],"needs":[],"scanned_areas":[]}"#)));
    let review2 = Arc::new(FakeChatBackend::repeating(turn(r#"{"findings":[],"needs":[]}"#)));
    let fx2 = make_repo_and_job("review", &config_with(None, false), false, false);
    let over2 = fake_override(scan2.clone(), review2);
    let old = std::env::var("LANG").ok();
    std::env::set_var("LANG", "zh_CN.UTF-8");
    std::env::set_var("REDTEAM_HOME", "/nonexistent/decoy-home");
    std::env::set_var("REDTEAM_MAX_TOKENS", "123");
    let code2 = redteam::job::run::run_with(JobKind::Review, &fx2.job, Some(over2.as_ref())).await;
    // 环境隔离（不变式⑦）：job 进程启动即清掉这些旋钮，证明"零读 env"不是靠自觉。
    assert!(std::env::var("LANG").is_err(), "LANG 必须被清掉");
    assert!(std::env::var("REDTEAM_HOME").is_err(), "REDTEAM_HOME 必须被清掉");
    assert!(std::env::var("REDTEAM_MAX_TOKENS").is_err(), "REDTEAM_MAX_TOKENS 必须被清掉");
    match old { Some(v) => std::env::set_var("LANG", v), None => std::env::remove_var("LANG") }
    assert_eq!(code2, 0);
    let sys2 = match &scan2.messages_at(0)[0] {
        redteam::provider::tool_loop::ChatMessage::System(s) => s.clone(),
        other => panic!("首条须是 system: {other:?}"),
    };
    assert!(sys2.contains("English") && !sys2.contains("简体中文"), "job 模式不得读 env: {sys2}");
    assert_eq!(read_report(&fx2.job)["extensions"]["redteam"]["language"]["source"], "default");
}

#[tokio::test]
async fn triage_and_verify_are_schema_shaped_stubs() {
    // triage：合法 manifest + 必需输入 → error + kind_not_implemented，无 summary。
    let fx = make_repo_and_job("triage", &config_with(Some("en_US"), false), false, true);
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, None).await;
    assert_eq!(code, 1, "error 报告 → 1");
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["kind"], "triage");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "kind_not_implemented:triage");
    assert!(r.get("summary").is_none(), "禁止伪造 summary");

    // verify：合法 manifest（空 findings 也合法）→ error + verifications: []。
    let tmp = tempfile::tempdir().unwrap();
    let job = tmp.path().join("job");
    std::fs::create_dir_all(job.join("config")).unwrap();
    std::fs::create_dir_all(job.join("input")).unwrap();
    std::fs::write(job.join("config/executor.redteam.json"), config_with(Some("en_US"), false)).unwrap();
    std::fs::write(job.join("input/findings.json"), "[]").unwrap();
    std::fs::write(job.join("input/diff.patch"), "").unwrap();
    std::fs::write(job.join("input/mb.sha"), "abc\n").unwrap();
    let inputs = serde_json::json!({
        "findings": {"path":"input/findings.json","sha256":sha256_hex(b"[]"),"schema":"hibot.audit.findings/1"},
        "diff_patch": {"path":"input/diff.patch","sha256":sha256_hex(b"")},
        "merge_base": {"path":"input/mb.sha","sha256":sha256_hex(b"abc\n")},
    });
    let m = serde_json::json!({
        "schema": "hibot.audit.job.verify/1", "job_id": "J-v", "kind": "verify",
        "repo": {"id":1,"full_name":"o/r"},
        "pr": {"number":7,"head_sha":"abcdef0","base_sha":"1234567"},
        "executor": {"name":"redteam","version":"0.1.0"},
        "limits": {"wall_clock_s":60},
        "paths": {"repo":"../repo","out":"out","config":"config/executor.redteam.json"},
        "inputs": inputs,
    });
    std::fs::write(job.join("job.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    let code = redteam::job::run::run_with(JobKind::Verify, &job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["schema"], "hibot.audit.report.verify/1");
    assert_eq!(r["verifications"], serde_json::json!([]));
    assert_eq!(r["extensions"]["redteam"]["status_code"], "kind_not_implemented:verify");
    assert!(r["findings"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn kind_mismatch_and_bad_manifest_are_reported_correctly() {
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), false, false);
    // kind 不一致 → error 报告（kind 以 manifest 为准）。
    let code = redteam::job::run::run_with(JobKind::Verify, &fx.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["kind"], "review");
    assert_eq!(r["extensions"]["redteam"]["status_code"].as_str().unwrap().starts_with("kind_mismatch"), true);

    // manifest 不可解析 → 不写报告 + 退出码 2。
    let bad = tempfile::tempdir().unwrap();
    std::fs::write(bad.path().join("job.json"), "{not json").unwrap();
    let code = redteam::job::run::run_with(JobKind::Review, bad.path(), None).await;
    assert_eq!(code, 2);
    assert!(!bad.path().join("out/report.json").exists());
}
