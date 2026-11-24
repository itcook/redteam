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
    let body = if with_vuln {
        "function find(id) {\n  return db.query('SELECT * FROM users WHERE id=' + id);\n}\n"
    } else {
        "function add(a, b) {\n  return a + b;\n}\n"
    };
    make_repo_and_job_with_body(kind, config, body, with_partial_provider)
}

/// 同上，但 `src/app.js` 的正文由调用方给（2.5 的空白差异用例需要自造源码）。
fn make_repo_and_job_with_body(kind: &str, config: &str, body: &str, with_partial_provider: bool) -> Fixture {
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
    // 报告不得含 cost（跨阶段不变式④）。
    assert!(r["usage"].get("cost").is_none() && r["usage"].get("cost_usd").is_none(), "{r}");
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
async fn triage_rejects_bad_pr_context_and_verify_is_a_stub() {
    // triage：Phase 3 已实现；本夹具的 pr_context 只有 schema（缺 title/author/…）→ error，不伪造 summary。
    let fx = make_repo_and_job("triage", &config_with(Some("en_US"), false), false, true);
    let code = redteam::job::run::run_with(JobKind::Triage, &fx.job, None).await;
    assert_eq!(code, 1, "error 报告 → 1");
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["kind"], "triage");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "input_schema_mismatch:pr_context");
    assert!(r.get("summary").is_none(), "禁止伪造 summary");

    // verify：空输入 → error + verifications: []。
    let vj = make_verify_job(b"[]");
    let code = redteam::job::run::run_with(JobKind::Verify, &vj.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&vj.job);
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
    let vj = make_verify_job(findings.as_bytes());
    let code = redteam::job::run::run_with(JobKind::Verify, &vj.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&vj.job);
    assert_eq!(r["status"], "error");
    let vs = r["verifications"].as_array().unwrap();
    assert_eq!(vs.len(), 2, "非空输入必须逐条覆盖: {r}");
    assert!(vs.iter().all(|v| v["status"] == "inconclusive"));
    assert!(vs.iter().all(|v| v["evidence"].as_str().unwrap().len() > 0));
    assert_eq!(vs[0]["finding_fingerprint"], fp1);

    // verify：非法指纹 → error + invalid_fingerprint_input，且不产 verifications。
    let bad = serde_json::json!([{"fingerprint":"sha256:abc"}]).to_string();
    let vj = make_verify_job(bad.as_bytes());
    let code = redteam::job::run::run_with(JobKind::Verify, &vj.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&vj.job);
    assert_eq!(r["extensions"]["redteam"]["status_code"], "invalid_fingerprint_input");
    assert_eq!(r["verifications"], serde_json::json!([]));

    // verify：findings JSON 非法 → input_schema_mismatch:findings。
    let vj = make_verify_job(b"not json");
    let code = redteam::job::run::run_with(JobKind::Verify, &vj.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&vj.job);
    assert_eq!(r["extensions"]["redteam"]["status_code"], "input_schema_mismatch:findings");
}


/// 建一个合法的 verify job（空/非空 findings 都行）；`TempDir` 随结构体存活，测试结束自清理。
struct VerifyJob {
    _tmp: tempfile::TempDir,
    job: std::path::PathBuf,
}

fn make_verify_job(findings: &[u8]) -> VerifyJob {
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
    VerifyJob { _tmp: tmp, job }
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

/// 改写已声明的输入并同步 manifest 里的 sha256（2.1/2.2 的对照夹具用）。
fn update_input(job: &Path, key: &str, rel: &str, bytes: &[u8]) {
    std::fs::write(job.join(rel), bytes).unwrap();
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(job.join("job.json")).unwrap()).unwrap();
    let old = m["inputs"][key].clone();
    let mut v = serde_json::json!({"path": rel, "sha256": sha256_hex(bytes)});
    for k in ["schema", "format"] {
        if !old[k].is_null() {
            v[k] = old[k].clone();
        }
    }
    m["inputs"][key] = v;
    std::fs::write(job.join("job.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
}

/// 2.1：变更集来自 `inputs.diff_patch`，**不是** git 重新采集。patch 与 git 不一致只披露。
#[tokio::test]
async fn review_uses_diff_patch_not_git_as_change_source() {
    let quote = "return a + b;";
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(quote, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(quote, "src/app.js", 2, "patch marker"))));
    // git 的真实改动是「新建 src/app.js」；artifact patch 伪装成「给它加一行标记」。
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), false, false);
    let fake = "diff --git a/src/app.js b/src/app.js\n\
                index 1111111..2222222 100644\n\
                --- a/src/app.js\n\
                +++ b/src/app.js\n\
                @@ -1,3 +1,4 @@\n\
                 function add(a, b) {\n\
                +  // PATCH_ONLY_MARKER\n\
                   return a + b;\n\
                 }\n";
    update_input(&fx.job, "diff_patch", "input/diff.patch", fake.as_bytes());
    let over = fake_override(scan.clone(), review);
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok", "{r}");
    // scanner 真正看到的确实来自 patch（git 的 diff 里没有这个标记）。
    let prompt = format!("{:?}", scan.messages_at(0));
    assert!(prompt.contains("PATCH_ONLY_MARKER"), "scan 输入必须来自 diff_patch: {prompt}");
    // 不一致只披露，不改行为：评审照常出 finding。
    let ic = &r["extensions"]["redteam"]["input_consistency"];
    assert_eq!(ic["git"], "mismatch", "{r}");
    assert_eq!(ic["differing"], serde_json::json!(["src/app.js"]), "{r}");
    assert_eq!(r["findings"].as_array().unwrap().len(), 1, "只披露、不改行为: {r}");
}

/// 2.2：HEAD 必须等于 `pr.head_sha`；不匹配 → error（不是 partial/ok）。
#[tokio::test]
async fn head_must_match_manifest_head_sha() {
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), false, false);
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(fx.job.join("job.json")).unwrap()).unwrap();
    // 检出停在 head，manifest 却说是 base —— 两个对象都在，但不是同一个提交。
    m["pr"]["head_sha"] = serde_json::json!(fx.base);
    std::fs::write(fx.job.join("job.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "head_sha_mismatch");
}

/// 2.2：`inputs.merge_base` 必须是 40 位 sha 的文本形态（短 sha 不在契约里）。
#[tokio::test]
async fn merge_base_must_be_a_full_sha() {
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), false, false);
    update_input(&fx.job, "merge_base", "input/merge-base.sha", b"abc\n");
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "input_schema_mismatch:merge_base");
}

/// 声明 `prior_report` 输入（schema 固定 triage 报告）。
fn add_prior_report(job: &Path, body: &str) {
    let rel = "input/prior-report.json";
    std::fs::write(job.join(rel), body).unwrap();
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(job.join("job.json")).unwrap()).unwrap();
    m["inputs"]["prior_report"] = serde_json::json!({
        "path": rel,
        "sha256": sha256_hex(body.as_bytes()),
        "schema": "hibot.audit.report.triage/1",
    });
    std::fs::write(job.join("job.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
}

/// 2.3：prior_report 只调**镜头与预算**，不改阻断阈值；specs 不可读 → partial 点名。
#[tokio::test]
async fn prior_report_only_adjusts_lens_and_budget() {
    // 对照组：无 prior_report。
    let scan_a = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review_a = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, "SQL injection"))));
    let fx_a = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    let over_a = fake_override(scan_a, review_a);
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx_a.job, Some(over_a.as_ref())).await, 0);
    let base_report = read_report(&fx_a.job);
    assert_eq!(base_report["status"], "ok", "{base_report}");
    assert!(base_report["extensions"]["redteam"].get("prior_routing").is_none(), "没有 prior_report 时不写该键");

    // 实验组：focus + 一份可读 spec + 一份不可读 spec + 建议预算。
    let scan_b = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review_b = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, "SQL injection"))));
    let fx_b = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    let prior = serde_json::json!({
        "schema": "hibot.audit.report.triage/1",
        "job_id": "T1", "kind": "triage", "status": "ok",
        "input": {"head_sha": "h", "base_sha": "b"},
        "executor": {"name": "redteam", "version": "0.1.0"},
        // 历史 findings / 计数必须被忽略：它们不是本轮 review 的阻断项。
        "findings": [{"fingerprint": "sha256:deadbeef"}],
        "summary": {
            "security_tier": "critical", "blockers_count": 9, "structural_count": 3, "local_count": 1,
            "intent_undelivered": true, "scope_creep_count": 2, "risk": "high",
            "convention_drift": "high", "fit": "poor",
            "routing": {"focus": ["sql-injection-focus"], "specs": ["README.md", "docs/missing.md"], "wall_clock_s": 600}
        },
        "usage": {"input_tokens": 1, "output_tokens": 1, "cached_input_tokens": 0, "wall_clock_ms": 1},
        "extensions": {}
    }).to_string();
    add_prior_report(&fx_b.job, &prior);
    let over_b = fake_override(scan_b.clone(), review_b);
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx_b.job, Some(over_b.as_ref())).await, 0);
    let r = read_report(&fx_b.job);

    // specs 不可读 → partial + 点名（不是 ok）。
    assert_eq!(r["status"], "partial", "{r}");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "routing_specs_unreadable:1");
    // 镜头生效：focus 进了 scanner prompt。
    let prompt = format!("{:?}", scan_b.messages_at(0));
    assert!(prompt.contains("sql-injection-focus"), "routing.focus 必须进 prompt: {prompt}");
    // 披露只含 routing 三件事。
    let pr = &r["extensions"]["redteam"]["prior_routing"];
    assert_eq!(pr["present"], true);
    assert_eq!(pr["focus"], serde_json::json!(["sql-injection-focus"]));
    assert_eq!(pr["specs_used"], serde_json::json!(["README.md"]));
    assert_eq!(pr["specs_unreadable"], 1);
    assert_eq!(pr["wall_clock_s"], 600);
    // 阈值不变：findings 与无 prior 的对照完全一致（prior 不消费历史 findings / 计数 / tier）。
    assert_eq!(r["findings"], base_report["findings"], "prior_report 不得改判定");
    assert_eq!(r["extensions"]["redteam"]["stats"], base_report["extensions"]["redteam"]["stats"]);
}

/// 2.5：报告里的 quote 必须是**检出原文**——缩进/多空格被纠正，行号重锚；指纹按修正后的 quote 生成。
#[tokio::test]
async fn quote_is_corrected_from_the_checkout_before_fingerprint() {
    // 源码里是多空格；模型报的是折叠后的单空格（上游行级归一能容忍，但 quote 不能原样进报告）。
    let body = "function find(id) {\n  return db.query('SELECT  *  FROM users WHERE id=' + id);\n}\n";
    let model_quote = "return db.query('SELECT * FROM users WHERE id=' + id);";
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(model_quote, "src/app.js", 9))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(model_quote, "src/app.js", 9, "SQL injection"))));
    let fx = make_repo_and_job_with_body("review", &config_with(Some("en_US"), false), body, false);
    let over = fake_override(scan, review);
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok", "{r}");
    let f = &r["findings"][0];
    // 检出原文（首行缩进去掉、行内双空格保留）。
    assert_eq!(f["quote"], "return db.query('SELECT  *  FROM users WHERE id=' + id);");
    assert_eq!((f["line_start"].as_u64().unwrap(), f["line_end"].as_u64().unwrap()), (2, 2), "自报行号 9 要重锚");
    assert_eq!(
        f["fingerprint"],
        fingerprint::of("src/app.js", f["quote"].as_str().unwrap(), f["title"].as_str().unwrap()),
        "指纹必须按修正后的 quote 生成"
    );
}

/// 2.5：逐字命不中的 finding 丢弃 + 计数 + partial（不得用空 findings 冒充 ok）。
#[tokio::test]
async fn unverifiable_quote_is_dropped_and_makes_partial() {
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json("return totally.fabricated();", "src/app.js", 2, "ghost"))));
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    let over = fake_override(scan, review);
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    // 同一幻觉 finding 可能被多个 reviewer 格各丢弃一次（队列项 × 格），计数如实累加。
    let code = r["extensions"]["redteam"]["status_code"].as_str().unwrap();
    assert!(code.starts_with("quotes_dropped:"), "{r}");
    assert!(r["extensions"]["redteam"]["dropped_unverified_quotes"].as_u64().unwrap() >= 1);
    assert!(r["findings"].as_array().unwrap().is_empty(), "幻觉 quote 不得留进 findings");
    assert!(r["status_reason"].as_str().unwrap().contains("quote not found verbatim"), "{}", r["status_reason"]);
}

/// 2.4：dismissed 命中 → 保留在 findings 里作审计、`refuted` 恒 false、不阻塞（status 仍 ok）。
#[tokio::test]
async fn dismissed_is_audited_but_never_refuted() {
    let title = "SQL injection";
    let fp = fingerprint::of("src/app.js", QUOTE, title);
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, title))));
    // 复核开启且模型判 REFUTED：人的驳回仍优先——报告的 refuted 必须保持 false。
    let refute = Arc::new(FakeChatBackend::repeating(turn(r#"{"verdict":"REFUTED","reason":"safe"}"#)));
    let mut cfg: serde_json::Value = serde_json::from_str(&config_with(Some("en_US"), false)).unwrap();
    cfg["review"] = serde_json::json!({"refuters": {"enabled": true}});
    cfg["providers"]["p1"]["models"]["v"] = serde_json::json!({"roles": {"refuter": {}}});
    let fx = make_repo_and_job("review", &cfg.to_string(), true, false);
    let csv = format!("fingerprint,actor,reason,created_at\n{fp},alice,\"false positive, see #12\",2026-11-20T10:00:00Z\n");
    update_input(&fx.job, "dismissed", "input/dismissed.csv", csv.as_bytes());
    let over = Box::new({
        let (scan, review, refute) = (scan, review, refute);
        move |em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> {
            if em.provider != "p1" {
                return None;
            }
            match em.id.as_str() {
                "s" => Some(Box::new(scan.clone())),
                "r" => Some(Box::new(review.clone())),
                "v" => Some(Box::new(refute.clone())),
                _ => None,
            }
        }
    }) as Box<redteam::cli::BackendOverride<'_>>;
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok", "被驳回项不阻塞: {r}");
    let f = &r["findings"][0];
    assert_eq!(f["fingerprint"], fp, "指纹必须与 CSV 对得上");
    assert_eq!(f["refuted"], false, "人的决定不是证据证伪");
    let ledger = &f["extensions"]["redteam"]["ledger"];
    assert_eq!(ledger["dismissed"], true);
    assert_eq!(ledger["actor"], "alice");
    assert_eq!(ledger["reason"], "false positive, see #12");
    // 不阻塞：不计入任何计数口径（severity 统计也不认它）。
    assert_eq!(r["extensions"]["redteam"]["stats"]["total"], 0, "被驳回项不计入计数: {r}");
}

/// 2.4：坏 CSV → error（不静默当空台账）。
#[tokio::test]
async fn malformed_dismissed_csv_is_an_error() {
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    update_input(
        &fx.job,
        "dismissed",
        "input/dismissed.csv",
        b"fingerprint,actor,reason,created_at\nnot-a-fingerprint,alice,b,2026-11-20T10:00:00Z\n",
    );
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "input_schema_mismatch:dismissed");
}

/// P2（用户 2026-11-24 裁）：job + enabled + 无 refuter → 跳过验真并披露（不得静默）。
#[tokio::test]
async fn job_mode_without_refuter_skips_refute_and_discloses() {
    let mut cfg: serde_json::Value = serde_json::from_str(&config_with(Some("en_US"), false)).unwrap();
    cfg["review"] = serde_json::json!({"refuters": {"enabled": true}});
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, "SQL injection"))));
    let fx = make_repo_and_job("review", &cfg.to_string(), true, false);
    let over = fake_override(scan, review);
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["extensions"]["redteam"]["refute_skipped"], "no_refuter", "{r}");
    let deep = &r["extensions"]["redteam"]["run"]["deep"];
    assert_eq!(deep["refute_triggered"].as_u64().unwrap_or(0), 0, "不得偷偷真跑");
    assert_eq!(deep["refute_skipped"]["disabled"].as_u64().unwrap_or(0) >= 1, true, "{deep}");
    assert_eq!(r["findings"][0]["refuted"], false, "跳过验真不得塞 refuted");
}

/// P2：job + enabled + 有 refuter → 真跑（refute_triggered > 0，且不披露 refute_skipped）。
#[tokio::test]
async fn job_mode_with_explicit_refuter_runs_refute() {
    let mut cfg: serde_json::Value = serde_json::from_str(&config_with(Some("en_US"), false)).unwrap();
    cfg["review"] = serde_json::json!({"refuters": {"enabled": true}});
    cfg["providers"]["p1"]["models"]["v"] = serde_json::json!({"roles": {"refuter": {}}});
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, "SQL injection"))));
    let refute = Arc::new(FakeChatBackend::repeating(turn(r#"{"verdict":"UPHELD","reason":"real SQL string concat"}"#)));
    let over = Box::new({
        let (scan, review, refute) = (scan.clone(), review.clone(), refute.clone());
        move |em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> {
            if em.provider != "p1" {
                return None;
            }
            match em.id.as_str() {
                "s" => Some(Box::new(scan.clone())),
                "r" => Some(Box::new(review.clone())),
                "v" => Some(Box::new(refute.clone())),
                _ => None,
            }
        }
    }) as Box<redteam::cli::BackendOverride<'_>>;
    let fx = make_repo_and_job("review", &cfg.to_string(), true, false);
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);
    let r = read_report(&fx.job);
    assert!(r["extensions"]["redteam"].get("refute_skipped").is_none(), "{r}");
    let deep = &r["extensions"]["redteam"]["run"]["deep"];
    assert!(deep["refute_triggered"].as_u64().unwrap_or(0) >= 1, "有 refuter 必须真跑: {deep}");
    assert!(refute.turn_count() >= 1, "refuter 后端必须被调用");
    assert_eq!(r["findings"][0]["refuted"], false, "UPHELD 不算证伪");
}

/// 2.7：`language` 落地——scanner / reviewer / refuter 的 prompt 语言槽、模型产出（cell 与 finding 文本）、
/// `status_reason` 三处一致；`run.warnings` / `extensions` 的机器字段不随语言变化。
#[tokio::test]
async fn language_is_consistent_across_prompts_findings_and_status_reason() {
    let mut warnings_by_lang = Vec::new();
    for (value, prompt_name, title, body, recommendation, reason_needle, hint) in [
        (
            "zh_CN",
            "Simplified Chinese (简体中文)",
            "SQL 注入",
            "拼接 SQL 导致注入",
            "改用参数化查询",
            "规范文档不可读",
            "SQL 注入风险",
        ),
        (
            "en_US",
            "English",
            "SQL injection",
            "string concatenation enables injection",
            "use parameterized queries",
            "spec files unreadable",
            "sql injection risk",
        ),
    ] {
        let scanner = format!(
            r#"{{"suspicions":[{{"file":"src/app.js","line_start":2,"line_end":2,"hint":"{hint}","category":"L2","confidence":0.9,"quote":"{QUOTE}"}}],"needs":[],"scanned_areas":[]}}"#
        );
        let reviewer = serde_json::json!({
            "findings": [{"severity":"high","title":title,"body":body,"file":"src/app.js",
                          "line_start":2,"line_end":2,"confidence":0.9,
                          "recommendation":recommendation,"quote":QUOTE}],
            "needs": []
        })
        .to_string();
        let refuter = serde_json::json!({"verdict": "UPHELD", "reason": reason_needle}).to_string();
        let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner)));
        let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer)));
        let refute = Arc::new(FakeChatBackend::repeating(turn(&refuter)));

        // 开启验真并显式声明 refuter（P2：job + enabled + 有 refuter → 真跑）。
        let mut cfg: serde_json::Value = serde_json::from_str(&config_with(Some(value), false)).unwrap();
        cfg["review"] = serde_json::json!({"refuters": {"enabled": true}});
        cfg["providers"]["p1"]["models"]["v"] = serde_json::json!({"roles": {"refuter": {}}});
        let fx = make_repo_and_job("review", &cfg.to_string(), true, false);
        // 用「声明了但读不到的 spec」制造 partial，才能断言 status_reason 的语言。
        let prior = serde_json::json!({
            "schema": "hibot.audit.report.triage/1", "job_id": "T1", "kind": "triage", "status": "ok",
            "input": {"head_sha": "h", "base_sha": "b"},
            "executor": {"name": "redteam", "version": "0.1.0"},
            "findings": [],
            "summary": {"routing": {"focus": [], "specs": ["docs/missing.md"], "wall_clock_s": 120}},
            "usage": {"input_tokens": 1, "output_tokens": 1, "cached_input_tokens": 0, "wall_clock_ms": 1},
            "extensions": {}
        })
        .to_string();
        add_prior_report(&fx.job, &prior);

        let over = Box::new({
            let (scan, review, refute) = (scan.clone(), review.clone(), refute.clone());
            move |em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> {
                if em.provider != "p1" {
                    return None;
                }
                match em.id.as_str() {
                    "s" => Some(Box::new(scan.clone())),
                    "r" => Some(Box::new(review.clone())),
                    "v" => Some(Box::new(refute.clone())),
                    _ => None,
                }
            }
        }) as Box<redteam::cli::BackendOverride<'_>>;
        assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);
        let r = read_report(&fx.job);

        let system_of = |b: &FakeChatBackend| match &b.messages_at(0)[0] {
            redteam::provider::tool_loop::ChatMessage::System(s) => s.clone(),
            other => panic!("首条须是 system: {other:?}"),
        };
        // ① 三类模型请求的语言槽
        assert!(system_of(&scan).contains(prompt_name), "{value} scanner 语言槽缺失");
        assert!(system_of(&review).contains(prompt_name), "{value} reviewer 语言槽缺失");
        assert!(system_of(&refute).contains(prompt_name), "{value} refuter 语言槽缺失");
        // ② 模型产出原样落地：cell hint + finding 文本
        assert_eq!(
            r["extensions"]["redteam"]["cells"][0]["output"]["suspicions"][0]["hint"],
            hint,
            "{r}"
        );
        assert_eq!(r["findings"][0]["title"], title);
        assert_eq!(r["findings"][0]["body"], body);
        assert_eq!(r["findings"][0]["recommendation"], recommendation);
        // refuter 的实际判决正文（finding 级 extensions.refute.reason，附录 D）
        assert_eq!(
            r["findings"][0]["extensions"]["redteam"]["refute"]["reason"],
            reason_needle,
            "refuter 判决正文未随语言落地: {r}"
        );
        // ③ status_reason 随 language
        assert_eq!(r["status"], "partial", "{r}");
        let reason = r["status_reason"].as_str().unwrap();
        assert!(reason.contains(reason_needle), "{value} 的 status_reason 语言不符: {reason}");
        // 验真确实跑过（不是靠“跳过”凑出来的报告）
        assert!(
            r["extensions"]["redteam"]["run"]["deep"]["refute_triggered"].as_u64().unwrap_or(0) >= 1,
            "{r}"
        );
        // 机器字段不随语言
        assert_eq!(r["extensions"]["redteam"]["language"]["value"], value);
        warnings_by_lang.push(
            r["extensions"]["redteam"]["run"]["warnings"].as_array().cloned().unwrap_or_default(),
        );
    }
    assert_eq!(warnings_by_lang[0], warnings_by_lang[1], "run.warnings 不得随 language 翻译");
}

/// 2.5/2.3/2.4 + 1.5：reviewer 已完成、后续 refute 调用挂起 → 超时收口落盘的真实报告
/// 仍保留 prior、dismissed、修正后的 quote 与 partial 原因。
#[tokio::test]
async fn timeout_partial_report_keeps_run_facts_and_corrected_quote() {
    let body = "function find(id) {\n  return db.query('SELECT  *  FROM users WHERE id=' + id);\n}\n";
    let model_quote = "return db.query('SELECT * FROM users WHERE id=' + id);";
    let fixed = "return db.query('SELECT  *  FROM users WHERE id=' + id);";
    let title = "SQL injection";
    let fp = fingerprint::of("src/app.js", fixed, title);

    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(model_quote, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(model_quote, "src/app.js", 2, title))));
    // 复核故意挂起：预算 1s 到点后必须收敛成 partial，而不是等它回。
    let refute = Arc::new(
        FakeChatBackend::repeating(turn(r#"{"verdict":"UPHELD","reason":"x"}"#))
            .with_delay(std::time::Duration::from_secs(5)),
    );
    let mut cfg: serde_json::Value = serde_json::from_str(&config_with(Some("en_US"), false)).unwrap();
    cfg["review"] = serde_json::json!({"refuters": {"enabled": true}});
    cfg["providers"]["p1"]["models"]["v"] = serde_json::json!({"roles": {"refuter": {}}});
    let fx = make_repo_and_job_with_body("review", &cfg.to_string(), body, false);
    // 预算 1s
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(fx.job.join("job.json")).unwrap()).unwrap();
    m["limits"]["wall_clock_s"] = serde_json::json!(1);
    std::fs::write(fx.job.join("job.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    // prior + dismissed（指纹按**修正后**的 quote 算）
    let prior = serde_json::json!({
        "schema": "hibot.audit.report.triage/1", "job_id": "T1", "kind": "triage", "status": "ok",
        "input": {"head_sha": "h", "base_sha": "b"},
        "executor": {"name": "redteam", "version": "0.1.0"},
        "findings": [],
        "summary": {"routing": {"focus": ["auth"], "specs": ["README.md", "docs/missing.md"], "wall_clock_s": 120}},
        "usage": {"input_tokens": 1, "output_tokens": 1, "cached_input_tokens": 0, "wall_clock_ms": 1},
        "extensions": {}
    })
    .to_string();
    add_prior_report(&fx.job, &prior);
    let csv = format!("fingerprint,actor,reason,created_at\n{fp},alice,false positive,2026-11-20T10:00:00Z\n");
    update_input(&fx.job, "dismissed", "input/dismissed.csv", csv.as_bytes());

    let over = Box::new({
        let (scan, review, refute) = (scan, review, refute);
        move |em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> {
            if em.provider != "p1" {
                return None;
            }
            match em.id.as_str() {
                "s" => Some(Box::new(scan.clone())),
                "r" => Some(Box::new(review.clone())),
                "v" => Some(Box::new(refute.clone())),
                _ => None,
            }
        }
    }) as Box<redteam::cli::BackendOverride<'_>>;
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    assert!(r["status_reason"].as_str().unwrap().contains("wall-clock"), "{}", r["status_reason"]);
    assert_eq!(r["extensions"]["redteam"]["prior_routing"]["focus"], serde_json::json!(["auth"]));
    // 中断报告同时保留准备阶段的 partial 原因（specs 不可读）与墙钟原因。
    assert_eq!(r["extensions"]["redteam"]["prior_routing"]["specs_unreadable"], 1);
    assert_eq!(r["extensions"]["redteam"]["status_code"], "routing_specs_unreadable:1");
    let reason = r["status_reason"].as_str().unwrap();
    assert!(reason.contains("unreadable") && reason.contains("wall-clock"), "{reason}");
    assert_eq!(r["extensions"]["redteam"]["input_consistency"]["git"], "match");
    let f = &r["findings"][0];
    assert_eq!(f["quote"], fixed, "超时报告同样要用检出原文修正 quote");
    assert_eq!(f["fingerprint"], fp);
    assert_eq!(f["refuted"], false);
    assert_eq!(f["extensions"]["redteam"]["ledger"]["actor"], "alice");
}

/// 2.6：缺 blob（commit/tree/检出都在）经 `run_with` 落合法 error 报告。
#[tokio::test]
async fn missing_blob_produces_valid_error_report() {
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    // 删掉 head 侧 src/app.js 的 blob：diff 生成必须读它，commit/tree/检出文件都还在。
    let blob = git(&fx.repo, &["rev-parse", "HEAD:src/app.js"]);
    let p = fx.repo.join(".git/objects").join(&blob[..2]).join(&blob[2..]);
    assert!(p.is_file(), "fixture 期望松散 blob: {}", p.display());
    std::fs::remove_file(&p).unwrap();
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "repo_incomplete:merge_base", "{r}");
    assert!(r["findings"].as_array().unwrap().is_empty());
}

/// 2.6：浅克隆经 `run_with` → 合法 error 报告（不是 partial/ok）。
#[tokio::test]
async fn shallow_checkout_produces_valid_error_report() {
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    let shallow = fx.repo.parent().unwrap().join("repo-shallow");
    let out = std::process::Command::new("git")
        .args(["clone", "-q", "--depth", "1", &format!("file://{}", fx.repo.display()), shallow.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(fx.job.join("job.json")).unwrap()).unwrap();
    m["paths"]["repo"] = serde_json::json!("../repo-shallow");
    std::fs::write(fx.job.join("job.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "repo_incomplete:shallow", "{r}");
}

/// 2.6：base 与 head 无共同祖先（两 commit 都在本地、merge-base 不可算）→ 合法 error 报告。
#[tokio::test]
async fn unrelated_histories_produce_valid_error_report() {
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    // 另起一条无共同祖先的根提交。
    let orphan = fx.repo.parent().unwrap().join("repo-orphan");
    std::fs::create_dir_all(&orphan).unwrap();
    git(&orphan, &["init", "-q"]);
    git(&orphan, &["config", "user.email", "t@example.com"]);
    git(&orphan, &["config", "user.name", "t"]);
    std::fs::write(orphan.join("x.txt"), "x\n").unwrap();
    git(&orphan, &["add", "."]);
    git(&orphan, &["commit", "-qm", "root"]);
    // 本地 fetch 导入（无网络），HEAD 不动。
    let out = std::process::Command::new("git")
        .current_dir(&fx.repo)
        .args(["fetch", "-q", orphan.to_str().unwrap(), "HEAD"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let root = git(&fx.repo, &["rev-parse", "FETCH_HEAD"]);
    // 前置事实：两个 commit 都在本地，但 merge-base 不可算。
    git(&fx.repo, &["rev-parse", "--verify", &format!("{root}^{{commit}}")]);
    let mb = std::process::Command::new("git")
        .current_dir(&fx.repo)
        .args(["merge-base", &root, "HEAD"])
        .output()
        .unwrap();
    assert!(!mb.status.success(), "fixture 必须真的没有共同祖先");

    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(fx.job.join("job.json")).unwrap()).unwrap();
    m["pr"]["base_sha"] = serde_json::json!(root);
    std::fs::write(fx.job.join("job.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, None).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "repo_incomplete:merge_base", "{r}");
}

/// 1.5/2.3：准备阶段（registry 构造）耗尽 1s 预算 → 不进入任何模型派发，落 partial。
#[tokio::test]
async fn budget_exhausted_during_prep_makes_no_backend_calls() {
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(QUOTE, "src/app.js", 2))));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(QUOTE, "src/app.js", 2, "SQL injection"))));
    let over = Box::new({
        let (scan, review) = (scan.clone(), review.clone());
        move |em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> {
            // BackendOverride 在 `build_registry_with` 里**同步**调用：模拟准备阶段慢于预算。
            std::thread::sleep(std::time::Duration::from_millis(1200));
            match em.id.as_str() {
                "s" => Some(Box::new(scan.clone())),
                "r" => Some(Box::new(review.clone())),
                _ => None,
            }
        }
    }) as Box<redteam::cli::BackendOverride<'_>>;
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), true, false);
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(fx.job.join("job.json")).unwrap()).unwrap();
    m["limits"]["wall_clock_s"] = serde_json::json!(1);
    std::fs::write(fx.job.join("job.json"), serde_json::to_vec_pretty(&m).unwrap()).unwrap();

    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "wall_clock_exhausted");
    assert!(r["findings"].as_array().unwrap().is_empty());
    assert_eq!(scan.turn_count(), 0, "预算耗尽后不得再派发");
    assert_eq!(review.turn_count(), 0, "预算耗尽后不得再派发");
}

/// Phase 5.2/5.3：job 模式落 `out/state.json`；终帧 `finishing`（非终态）、llm 来自 gate、progress=cells；
/// 退出后停写且无 tmp 残留。
#[tokio::test]
async fn heartbeat_state_file_reports_gate_snapshot_and_stops_after_exit() {
    let scan = Arc::new(FakeChatBackend::repeating(turn(r#"{"suspicions":[],"needs":[],"scanned_areas":[]}"#)));
    let review = Arc::new(FakeChatBackend::repeating(turn(r#"{"findings":[],"needs":[]}"#)));
    let fx = make_repo_and_job("review", &config_with(Some("en_US"), false), false, false);
    let over = fake_override(Arc::clone(&scan), Arc::clone(&review));
    assert_eq!(redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await, 0);

    let path = fx.job.join("out/state.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).expect("state.json")).unwrap();
    assert_eq!(v["schema"], "hibot.audit.state/1");
    assert_eq!(v["job_id"], "J-review");
    assert_eq!(v["phase"], "finishing", "最后一帧是非终态值: {v}");
    assert!(!["done", "fail"].contains(&v["phase"].as_str().unwrap()));
    assert_eq!(v["llm"]["providers"][0]["name"], "p1", "配置 provider key，不是模型 id");
    assert_eq!(v["llm"]["max_concurrency"], 10, "Σ provider 帽（缺省 review.maxConcurrency=10）");
    assert_eq!(v["llm"]["in_flight"], 0, "退出帧没有在途请求");
    assert_eq!(v["extensions"]["redteam"]["concurrency"]["mode"], "per-provider");
    assert_eq!(v["progress"]["unit"], "cells");
    assert_eq!(v["progress"]["done"], v["progress"]["total"]);
    assert!(v["progress"]["total"].as_u64().unwrap() >= 1);
    assert!(!fx.job.join("out/state.json.tmp").exists(), "rename 后不留 tmp");

    // 退出后停写：report 已落盘，等超过一个周期后 state.json 字节不变。
    let before = std::fs::read(&path).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(2_300)).await;
    assert_eq!(std::fs::read(&path).unwrap(), before, "退出后不再写 state.json");
}

/// Phase 5.2：triage / verify 也有三值心跳（分析未实现，不伪造 progress/llm）。
#[tokio::test]
async fn triage_and_verify_write_three_value_heartbeat() {
    let fx = make_repo_and_job("triage", &config_with(Some("en_US"), false), false, true);
    assert_eq!(redteam::job::run::run_with(JobKind::Triage, &fx.job, None).await, 1);
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(fx.job.join("out/state.json")).unwrap()).unwrap();
    assert_eq!(v["job_id"], "J-triage");
    assert_eq!(v["phase"], "finishing");
    assert!(v.get("progress").is_none(), "triage 无可计数项，不伪造 progress");
    assert!(v.get("llm").is_none(), "未建 registry，不伪造 llm");

    let fp = "sha256:".to_string() + &"a".repeat(64);
    let findings = serde_json::json!([{
        "fingerprint": fp, "severity":"high", "confidence":0.9, "file":"a.rs",
        "line_start":1, "line_end":1, "quote":"q", "title":"t", "body":"b", "recommendation":"r"
    }]).to_string();
    let vj = make_verify_job(findings.as_bytes());
    assert_eq!(redteam::job::run::run_with(JobKind::Verify, &vj.job, None).await, 1);
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(vj.job.join("out/state.json")).unwrap()).unwrap();
    assert_eq!(v["job_id"], "J-v");
    assert_eq!(v["phase"], "finishing");
    assert_eq!(v["progress"], serde_json::json!({"unit": "items", "done": 0, "total": 1}));
}

