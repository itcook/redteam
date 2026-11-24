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
use std::sync::{Arc, Mutex};

/// 串行化会改 process env 的用例（std::env 与并发线程不安全）。
static ENV_LOCK: Mutex<()> = Mutex::new(());

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


/// 极简本地 HTTP 服务：正确 key → 返回 scanner JSON；无 key → 401。
struct TestServer {
    port: u16,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TestServer {
    fn start(scan_body: String) -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (seen2, stop2) = (seen.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                        let mut buf = Vec::new();
                        let mut tmp = [0u8; 4096];
                        loop {
                            match s.read(&mut tmp) {
                                Ok(0) => break,
                                Ok(n) => {
                                    buf.extend_from_slice(&tmp[..n]);
                                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let req = String::from_utf8_lossy(&buf).to_string();
                        if let Ok(mut v) = seen2.lock() { v.push(req.clone()); }
                        let authorized = req.to_lowercase().contains("authorization: bearer sk-test-key");
                        let body = if authorized {
                            serde_json::json!({
                                "choices": [{"message": {"role": "assistant", "content": scan_body}, "finish_reason": "stop"}],
                                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
                            }).to_string()
                        } else {
                            r#"{"error":{"message":"unauthorized"}}"#.to_string()
                        };
                        let status = if authorized { "200 OK" } else { "401 Unauthorized" };
                        let resp = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(), body
                        );
                        let _ = s.write_all(resp.as_bytes());
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                }
            }
        });
        Self { port, seen, stop, handle: Some(handle) }
    }
    fn seen(&self) -> Vec<String> { self.seen.lock().map(|v| v.clone()).unwrap_or_default() }
    fn stop(mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() { let _ = h.join(); }
    }
}

#[tokio::test]
async fn partial_fixture_uses_missing_provider_key_and_reports_partial() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // 受控本地服务：带正确 key 的请求返回合法 scanner 输出；无 key 返回 401。
    // 这样「缺 key → partial / 注入 key → ok」是同输入对照，不再靠 127.0.0.1:1 的必然失败。
    let quote = QUOTE;
    let server_body = scanner_json(quote, "src/app.js", 2);
    let server = TestServer::start(server_body);
    let config = format!(
        r#"{{"language":"en_US","providers":{{
          "p1":{{"kind":"openai","baseUrl":"http://127.0.0.1:1","apiKeyEnv":"P1_KEY","models":{{"s":{{"roles":{{"scanner":{{}}}}}},"r":{{"roles":{{"reviewer":{{}}}}}}}}}},
          "p2":{{"kind":"openai","baseUrl":"http://127.0.0.1:{}","apiKeyEnv":"P2_KEY","models":{{"scan2":{{"roles":{{"scanner":{{}}}}}}}}}}
        }}}}"#,
        server.port
    );
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(quote, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(quote, "src/app.js", 2, "SQL injection"))));
    let fx = make_repo_and_job("review", &config, true, false);
    let over = fake_override(scan, review);

    // 缺 key：p2 的请求无 Authorization → 401 → scanner 格失败 → partial。
    std::env::remove_var("P2_KEY");
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0, "partial 报告也是已写出 → 0");
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    assert!(r["status_reason"].as_str().unwrap().contains("scan incomplete"), "{r}");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "scan_incomplete:1/2");
    let seen = server.seen();
    assert!(!seen.is_empty(), "p2 必须真的发过请求");
    assert!(seen.iter().all(|r| !r.contains("sk-test-key")), "缺 key 时不得携带任何密钥");

    // 注入 key：同一夹具 → p2 成功 → ok（证明 partial 确实由缺 key 导致）。
    std::env::set_var("P2_KEY", "sk-test-key");
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    std::env::remove_var("P2_KEY");
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok", "注入 key 后必须恢复 ok: {r}");
    assert!(server.seen().iter().any(|r| r.to_lowercase().contains("authorization: bearer sk-test-key")), "带 key 的请求必须可见");
    server.stop();
}

#[tokio::test]
async fn job_language_comes_from_config_not_env() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
    let code2 = redteam::job::run::run_with(JobKind::Review, &fx2.job, Some(over2.as_ref())).await;
    // 语言决策不依赖 LANG（seal 只在真实 CLI 入口做，进程内测试不动全局 env）。
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

    // verify：空输入 → error + verifications: []。
    let job = make_verify_job(b"[]");
    let code = redteam::job::run::run_with(JobKind::Verify, &job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["schema"], "hibot.audit.report.verify/1");
    assert_eq!(r["verifications"], serde_json::json!([]));
    assert_eq!(r["extensions"]["redteam"]["status_code"], "kind_not_implemented:verify");
    assert!(r["findings"].as_array().unwrap().is_empty());

    // verify：非空输入 → 逐条 inconclusive（协调者 2026-11-24 裁 A），整体 error。
    let fp1 = "sha256:".to_string() + &"a".repeat(64);
    let fp2 = "sha256:".to_string() + &"b".repeat(64);
    let findings = serde_json::json!([
        {"fingerprint": fp1, "severity":"high","confidence":0.9,"file":"a.rs","line_start":1,"line_end":1,
         "quote":"q","title":"t","body":"b","recommendation":"r"},
        {"fingerprint": fp2, "severity":"low","confidence":0.5,"file":"b.rs","line_start":2,"line_end":2,
         "quote":"q2","title":"t2","body":"b","recommendation":"r"}
    ]).to_string();
    let job = make_verify_job(findings.as_bytes());
    let code = redteam::job::run::run_with(JobKind::Verify, &job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&job);
    assert_eq!(r["status"], "error");
    let vs = r["verifications"].as_array().unwrap();
    assert_eq!(vs.len(), 2, "非空输入必须逐条覆盖: {r}");
    assert!(vs.iter().all(|v| v["status"] == "inconclusive"));
    assert!(vs.iter().all(|v| v["evidence"].as_str().unwrap().len() > 0));
    assert_eq!(vs[0]["finding_fingerprint"], fp1);

    // verify：非法指纹 → error + invalid_fingerprint_input，且不产 verifications。
    let bad = serde_json::json!([{"fingerprint":"sha256:abc"}]).to_string();
    let job = make_verify_job(bad.as_bytes());
    let code = redteam::job::run::run_with(JobKind::Verify, &job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&job);
    assert_eq!(r["extensions"]["redteam"]["status_code"], "invalid_fingerprint_input");
    assert_eq!(r["verifications"], serde_json::json!([]));

    // verify：findings JSON 非法 → input_schema_mismatch:findings。
    let job = make_verify_job(b"not json");
    let code = redteam::job::run::run_with(JobKind::Verify, &job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&job);
    assert_eq!(r["extensions"]["redteam"]["status_code"], "input_schema_mismatch:findings");
}


/// 建一个合法的 verify job（空/非空 findings 都行）。
fn make_verify_job(findings: &[u8]) -> std::path::PathBuf {
    let tmp = tempfile::tempdir().unwrap();
    let job = tmp.path().join("job");
    std::fs::create_dir_all(job.join("config")).unwrap();
    std::fs::create_dir_all(job.join("input")).unwrap();
    std::fs::create_dir_all(job.join("out")).unwrap();
    std::fs::write(job.join("config/executor.redteam.json"), config_with(Some("en_US"), false)).unwrap();
    std::fs::write(job.join("input/findings.json"), findings).unwrap();
    std::fs::write(job.join("input/diff.patch"), "").unwrap();
    std::fs::write(job.join("input/mb.sha"), "abc\n").unwrap();
    let inputs = serde_json::json!({
        "findings": {"path":"input/findings.json","sha256":sha256_hex(findings),"schema":"hibot.audit.findings/1"},
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
    // 保持 tmp 目录存活：把 job 移到一个泄漏的临时根（测试进程短命，可接受）。
    let keep = std::env::temp_dir().join(format!("job-verify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&keep);
    std::fs::rename(&job, &keep).unwrap();
    keep
}

#[tokio::test]
async fn backend_construction_failure_is_config_invalid_not_internal_panic() {
    // kind=google 的 backend 尚未支持：这是 executor config 的问题，不是内部错误。
    let config = r#"{"language":"en_US","providers":{"p":{"kind":"google","models":{"s":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#;
    let fx = make_repo_and_job("review", config, true, false);
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "config_invalid", "{r}");
    assert!(r["status_reason"].as_str().unwrap().to_lowercase().contains("google"), "{r}");
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
