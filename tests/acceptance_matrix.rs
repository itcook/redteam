//! Phase 6 验收矩阵（消费计划 §9.4）：**3 kind × 3 夹具 × 2 语言 = 18 组合**。
//!
//! 每组都建真实 git 检出 + 真实 job 目录，经 `run_with` + 确定性 backend 跑完整管线，再对报告做契约校验。
//! 断言的是**契约自动项**（形状 / 指纹 / quote / status 判定 / verify 覆盖 / 机器字段），不是模型质量。
//!
//! 夹具语义（同一名字在三种 kind 下落到各自能表达的能力）：
//!
//! | 夹具 | review | triage | verify |
//! |---|---|---|---|
//! | clean | 无疑点、无 finding → `ok` | 三轴空 → `ok` + summary | findings 为空 → `ok` + 空 verifications |
//! | known-vuln | SQL 注入 finding（quote 逐字） | security 行 → blocker + finding | 一条 finding 判 `unresolved`（缺陷仍在） |
//! | partial | 第二个 scanner provider 401 → `scan_incomplete` | security 轴解析失败 → `triage_axis_failed:security` | provider 401 → `inconclusive` → `verifications_inconclusive:1` |
//!
//! 语言用同一夹具跑两遍（`zh_CN` / `en_US`）：prompt 的语言指令与 `status_reason` 必须随语言，
//! 机器字段（`status_code` / `concurrency` / `budget` / `template_hashes` / 计数 / warnings）不得随语言。
use redteam::config::EffectiveModel;
use redteam::job::fingerprint;
use redteam::job::manifest::JobKind;
use redteam::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage, FakeChatBackend};
use redteam::provider::ProviderError;
use redteam::tools::ToolSpec;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

const LANGS: [(&str, &str); 2] =
    [("zh_CN", "Simplified Chinese (简体中文)"), ("en_US", "English")];

/// review / triage 共用的漏洞行（逐字 quote 的素材）。
const VULN: &str = "return db.query('SELECT * FROM users WHERE id=' + id);";
const EVAL: &str = "return eval(cmd);";
const REVIEW_BASE_BODY: &str = "function add(a, b) {\n  return a + b;\n}\n";
const REVIEW_HEAD_BODY: &str = "function find(id) {\n  return db.query('SELECT * FROM users WHERE id=' + id);\n}\n";
const TRIAGE_BASE_BODY: &str = "function find(id) {\n  return db.query('SELECT * FROM users WHERE id=' + id);\n}\n";
const TRIAGE_HEAD_BODY: &str = "function find(id) {\n  return db.query('SELECT * FROM users WHERE id=' + id);\n}\n\nfunction run(cmd) {\n  return eval(cmd);\n}\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fixture {
    Clean,
    KnownVuln,
    Partial,
}

impl Fixture {
    fn name(self) -> &'static str {
        match self {
            Fixture::Clean => "clean",
            Fixture::KnownVuln => "known-vuln",
            Fixture::Partial => "partial",
        }
    }
}

// ───────────────────────────── 基础工具 ─────────────────────────────

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(dir).args(args).output().expect("git");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
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

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())))
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

// ───────────────────────────── 401 服务（review partial 的确定性失败源） ─────────────────────────────

/// 永远 401 的本地服务：让「第二个 scanner provider」以不可重试的 Auth 失败（连接拒绝会走 2s 起的瞬断退避）。
struct Always401 {
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Always401 {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let port = listener.local_addr().unwrap().port();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let handle = std::thread::spawn(move || {
            use std::io::{Read, Write};
            while !stop2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(2)));
                        let mut buf = [0u8; 4096];
                        let _ = s.read(&mut buf);
                        let body = r#"{"error":{"message":"unauthorized"}}"#;
                        let resp = format!(
                            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = s.write_all(resp.as_bytes());
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
                }
            }
        });
        Self { port, stop, handle: Some(handle) }
    }
}

impl Drop for Always401 {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

// ───────────────────────────── 夹具：仓库 + job 目录 ─────────────────────────────

struct Scenario {
    _tmp: tempfile::TempDir,
    job: PathBuf,
    repo: PathBuf,
    base: String,
    head: String,
}

struct InputDecl {
    key: &'static str,
    path: String,
    schema: Option<&'static str>,
    format: Option<&'static str>,
}

/// 建 base→head 的两提交仓，再把 head 做成一个合法 job 目录。
///
/// `prepare` 在 job 目录建好后写 kind 专属输入（pr_context / findings），返回它们的声明；
/// diff / merge_base / dismissed 由本函数统一生成（dismissed 只给 review 声明）。
fn scenario<F>(
    kind: &str,
    config: &str,
    base_files: &[(&str, &str)],
    head_files: &[(&str, &str)],
    prepare: F,
) -> Scenario
where
    F: FnOnce(&Path) -> Vec<InputDecl>,
{
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "matrix@example.com"]);
    git(&repo, &["config", "user.name", "matrix"]);
    for (rel, body) in base_files {
        let p = repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    for (rel, body) in head_files {
        let p = repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let job = tmp.path().join("job");
    std::fs::create_dir_all(job.join("config")).unwrap();
    std::fs::create_dir_all(job.join("input")).unwrap();
    std::fs::create_dir_all(job.join("out")).unwrap();
    std::fs::write(job.join("config/executor.redteam.json"), config).unwrap();

    let diff = format!("{}\n", git(&repo, &["diff", &format!("{base}..{head}")]));
    let mb = format!("{base}\n");
    std::fs::write(job.join("input/diff.patch"), &diff).unwrap();
    std::fs::write(job.join("input/merge-base.sha"), &mb).unwrap();

    let mut inputs = serde_json::Map::new();
    inputs.insert(
        "diff_patch".into(),
        json!({"path": "input/diff.patch", "sha256": sha256_hex(diff.as_bytes())}),
    );
    inputs.insert(
        "merge_base".into(),
        json!({"path": "input/merge-base.sha", "sha256": sha256_hex(mb.as_bytes())}),
    );
    if kind == "review" {
        let csv = "fingerprint,actor,reason,created_at\n";
        std::fs::write(job.join("input/dismissed.csv"), csv).unwrap();
        inputs.insert(
            "dismissed".into(),
            json!({"path": "input/dismissed.csv", "sha256": sha256_hex(csv.as_bytes()),
                   "format": "hibot.audit.dismissed-csv/1"}),
        );
    }
    for d in prepare(&job) {
        let bytes = std::fs::read(job.join(&d.path)).unwrap_or_else(|e| panic!("{}: {e}", d.path));
        let mut decl = json!({"path": d.path, "sha256": sha256_hex(&bytes)});
        if let Some(s) = d.schema {
            decl["schema"] = json!(s);
        }
        if let Some(f) = d.format {
            decl["format"] = json!(f);
        }
        inputs.insert(d.key.into(), decl);
    }

    let manifest = json!({
        "schema": format!("hibot.audit.job.{kind}/1"),
        "job_id": format!("J-matrix-{kind}"),
        "kind": kind,
        "repo": {"id": 1, "full_name": "org/repo"},
        "pr": {"number": 7, "head_sha": head, "base_sha": base},
        "executor": {"name": "redteam", "version": "0.1.0"},
        "limits": {"wall_clock_s": 120},
        "paths": {"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"},
        "inputs": inputs,
    });
    std::fs::write(job.join("job.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    Scenario { _tmp: tmp, job, repo, base, head }
}

fn pr_context() -> String {
    serde_json::to_string_pretty(&json!({
        "schema": "hibot.audit.pr-context/1",
        "title": "Add eval helper",
        "body": "This PR adds input validation for user input.",
        "author": "alice",
        "labels": [],
        "comments": [],
        "linked_issues": [],
        "ci": {"state": "success", "failing": []},
        "changed_files": [{"path": "src/app.js", "additions": 6, "deletions": 0, "status": "modified"}]
    }))
    .unwrap()
}

// ───────────────────────────── 配置（v2 形状） ─────────────────────────────

fn review_config(lang: &str, partial: bool, p2_port: u16) -> String {
    let mut scanner_models = vec![json!({"provider": "p1", "id": "s"})];
    let mut providers = json!({
        "p1": {"api": "openai-completions", "baseUrl": "http://127.0.0.1:1", "apiKeyEnv": "P1_KEY",
               "models": [{"id": "s"}, {"id": "r"}]}
    });
    let mut scanners = json!({});
    if partial {
        // 两个 scanner 条目都要跑（v2 轮转）；p2 的 key 不注入 → 401 → 该格失败。
        scanner_models.push(json!({"provider": "p2", "id": "scan2"}));
        providers["p2"] = json!({
            "api": "openai-completions",
            "baseUrl": format!("http://127.0.0.1:{p2_port}"),
            "apiKeyEnv": "P2_KEY_MATRIX_NOT_SET",
            "models": [{"id": "scan2"}]
        });
        scanners = json!({"samples": 2});
    }
    json!({
        "settings": {
            "language": lang,
            "review": {
                "scanners": scanners,
                "roles": {
                    "scanner": {"models": scanner_models},
                    "reviewer": {"models": [{"provider": "p1", "id": "r"}]}
                }
            }
        },
        "providers": providers
    })
    .to_string()
}

fn triage_config(lang: &str) -> String {
    json!({
        "settings": {"language": lang, "triage": {"models": [{"provider": "p1", "id": "t"}]}},
        "providers": {"p1": {"api": "openai-completions", "baseUrl": "http://127.0.0.1:1",
                             "apiKeyEnv": "P1_KEY", "models": [{"id": "t"}]}}
    })
    .to_string()
}

fn verify_config(lang: &str) -> String {
    json!({
        "settings": {"language": lang, "verify": {"models": [{"provider": "p1", "id": "v"}]}},
        "providers": {"p1": {"api": "openai-completions", "baseUrl": "http://127.0.0.1:1",
                             "apiKeyEnv": "P1_KEY", "models": [{"id": "v"}]}}
    })
    .to_string()
}

// ───────────────────────────── 确定性 backend ─────────────────────────────

fn scanner_json(fixture: Fixture) -> String {
    match fixture {
        Fixture::Clean => json!({"suspicions": [], "needs": [], "scanned_areas": []}).to_string(),
        _ => json!({
            "suspicions": [{"file": "src/app.js", "line_start": 2, "line_end": 2,
                            "hint": "sql injection", "category": "L2", "confidence": 0.9, "quote": VULN}],
            "needs": [], "scanned_areas": []
        })
        .to_string(),
    }
}

fn reviewer_json(fixture: Fixture) -> String {
    match fixture {
        Fixture::Clean => json!({"findings": [], "needs": []}).to_string(),
        _ => json!({
            "findings": [{"severity": "high", "title": "SQL injection", "body": "harm",
                          "file": "src/app.js", "line_start": 2, "line_end": 2, "confidence": 0.9,
                          "recommendation": "parameterize", "quote": VULN}],
            "needs": []
        })
        .to_string(),
    }
}

/// review：scanner 用 p1/s，reviewer 用 p1/r；p2 条目返回 None → 走真实 backend（401 服务）。
fn review_override<'a>(
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

/// triage：按 prompt 里的 `# Axis:` 分派；同时记录所有 prompt（语言断言用）。
struct TriageFake {
    security: String,
    drift: String,
    claims: String,
    slice: String,
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ChatBackend for TriageFake {
    async fn turn(&self, messages: &[ChatMessage], _tools: &[ToolSpec]) -> Result<AssistantTurn, ProviderError> {
        let text = flatten(messages);
        self.seen.lock().unwrap().push(text.clone());
        let body = if text.contains("# Axis: security") {
            &self.security
        } else if text.contains("# Axis: convention drift") {
            &self.drift
        } else if text.contains("# Axis: intent claims") {
            &self.claims
        } else if text.contains("# Axis: intent slice") {
            &self.slice
        } else {
            return Err(ProviderError::Unavailable("unknown triage axis".into()));
        };
        Ok(turn(body))
    }
}

fn triage_override<'a>(fake: Arc<TriageFake>) -> Box<redteam::cli::BackendOverride<'a>> {
    Box::new(move |_em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> { Some(Box::new(fake.clone())) })
}

fn triage_fake(fixture: Fixture) -> Arc<TriageFake> {
    let empty = json!({"rows": []}).to_string();
    let (security, drift) = match fixture {
        Fixture::Clean => (empty.clone(), empty.clone()),
        Fixture::KnownVuln => (
            json!({"rows": [{"file": "src/app.js", "line_start": 6, "line_end": 6, "quote": EVAL,
                             "surface": 4, "confidence": 9,
                             "reach": "HTTP body field cmd -> eval at line 6", "note": "eval on request input"}]})
            .to_string(),
            // structural drift 进 blockers（安全行只进 findings，不算 blocker）。
            json!({"rows": [{"file": "src/app.js", "line_start": 5, "line_end": 5,
                             "quote": "function run(cmd) {", "drift": "structural", "category": "placement",
                             "standard": "src/AGENTS.md#layout", "note": "entrypoint helper added to app.js"}]})
            .to_string(),
        ),
        // 解析失败一条就够：轴失败必须让报告 partial，而不是把轴当成「没发现问题」。
        Fixture::Partial => ("not json at all".to_string(), empty.clone()),
    };
    Arc::new(TriageFake {
        security,
        drift,
        claims: match fixture {
            Fixture::KnownVuln => json!({"claims": [{"claim": "add input validation", "source": "body", "note": ""}],
                                      "maintainer_overlap": {"flagged": false, "author": "", "quote": ""},
                                      "notes": []})
            .to_string(),
            _ => json!({"claims": [], "maintainer_overlap": {"flagged": false, "author": "", "quote": ""}, "notes": []})
                .to_string(),
        },
        slice: match fixture {
            Fixture::KnownVuln => json!({"delivered": [],
                                      "scope_creep": [{"title": "adds eval helper", "files": ["src/app.js"], "note": "not stated"}],
                                      "notes": []})
            .to_string(),
            _ => json!({"delivered": [], "scope_creep": [], "notes": []}).to_string(),
        },
        seen: Mutex::new(Vec::new()),
    })
}

fn verify_input_finding(fp: &str) -> Value {
    json!({
        "fingerprint": fp,
        "severity": "high",
        "confidence": 0.9,
        "file": "src/app.py",
        "line_start": 2,
        "line_end": 2,
        "quote": VULN,
        "title": "SQL injection in app.py",
        "body": "string concatenation reaches the query",
        "recommendation": "parameterize"
    })
}

fn verify_verdict() -> String {
    // 缺陷仍在（fix 只补了注释）：unresolved，quote 仍然逐字命中。
    json!({"status": "unresolved", "evidence": "拼接仍在，注入面未消失", "quote": VULN}).to_string()
}

// ───────────────────────────── 一次组合的运行结果 ─────────────────────────────

struct CaseRun {
    scenario: Scenario,
    report: Value,
    prompts: Vec<String>,
    /// verify 用：输入 findings 的指纹集合（覆盖不变量的左端）。
    input_fingerprints: Vec<String>,
}

async fn run_case(kind: JobKind, fixture: Fixture, lang: &str, p2_port: u16) -> CaseRun {
    match kind {
        JobKind::Review => {
            let sc = scenario(
                "review",
                &review_config(lang, fixture == Fixture::Partial, p2_port),
                &[("README.md", "base\n"), ("src/app.js", REVIEW_BASE_BODY)],
                &[("src/app.js", REVIEW_HEAD_BODY)],
                |_job| Vec::new(),
            );
            let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json(fixture))));
            let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json(fixture))));
            let over = review_override(scan.clone(), review.clone());
            let code = redteam::job::run::run_with(JobKind::Review, &sc.job, Some(over.as_ref())).await;
            assert_eq!(code, 0, "review/{}/{}: 报告写出即 0", fixture.name(), lang);
            let report = read_json(&sc.job.join("out/report.json"));
            assert_report_common(&sc, &report, JobKind::Review, lang);
            let prompts = (0..scan.turn_count())
                .map(|i| flatten(&scan.messages_at(i)))
                .chain((0..review.turn_count()).map(|i| flatten(&review.messages_at(i))))
                .collect();
            let run = CaseRun { scenario: sc, report, prompts, input_fingerprints: Vec::new() };
            dump_artifacts(&run, JobKind::Review, fixture, lang);
            run
        }
        JobKind::Triage => {
            let pr = pr_context();
            let sc = scenario(
                "triage",
                &triage_config(lang),
                &[
                    ("README.md", "base\n"),
                    ("src/AGENTS.md", "# Layout\nModules live under `src/<name>`; entrypoints stay in `src/app.js`.\n"),
                    ("src/helper.js", "function helper() {\n  return 1;\n}\n"),
                    ("src/app.js", TRIAGE_BASE_BODY),
                ],
                &[("src/app.js", TRIAGE_HEAD_BODY)],
                move |job| {
                    std::fs::write(job.join("input/pr-context.json"), &pr).unwrap();
                    vec![InputDecl {
                        key: "pr_context",
                        path: "input/pr-context.json".into(),
                        schema: Some("hibot.audit.pr-context/1"),
                        format: None,
                    }]
                },
            );
            let fake = triage_fake(fixture);
            let over = triage_override(fake.clone());
            let code = redteam::job::run::run_with(JobKind::Triage, &sc.job, Some(over.as_ref())).await;
            assert_eq!(code, 0, "triage/{}/{}: 报告写出即 0", fixture.name(), lang);
            let report = read_json(&sc.job.join("out/report.json"));
            assert_report_common(&sc, &report, JobKind::Triage, lang);
            let prompts = fake.seen.lock().unwrap().clone();
            let run = CaseRun { scenario: sc, report, prompts, input_fingerprints: Vec::new() };
            dump_artifacts(&run, JobKind::Triage, fixture, lang);
            run
        }
        JobKind::Verify => {
            let finding = verify_input_finding(&fingerprint::of(
                "src/app.py",
                VULN,
                "SQL injection in app.py",
            ));
            let input_fingerprints = match fixture {
                Fixture::Clean => Vec::new(),
                _ => vec![finding["fingerprint"].as_str().unwrap().to_string()],
            };
            let findings = match fixture {
                Fixture::Clean => "[]".to_string(),
                _ => serde_json::to_string_pretty(&vec![finding.clone()]).unwrap(),
            };
            let sc = scenario(
                "verify",
                &verify_config(lang),
                &[("src/app.py", &format!("def find(id):\n    {VULN}\n"))],
                // fix 只补注释：缺陷行仍在（known-vuln / partial 的素材）；clean 也保留合法 diff。
                &[(
                    "src/app.py",
                    &format!("def find(id):\n    {VULN}\n\n# reviewed: query construction untouched\n"),
                )],
                move |job| {
                    std::fs::write(job.join("input/findings.json"), &findings).unwrap();
                    vec![InputDecl {
                        key: "findings",
                        path: "input/findings.json".into(),
                        schema: Some("hibot.audit.findings/1"),
                        format: None,
                    }]
                },
            );
            let backend = match fixture {
                Fixture::Clean => Arc::new(FakeChatBackend::new_results(vec![])),
                Fixture::KnownVuln => Arc::new(FakeChatBackend::new_results(vec![Ok(turn(&verify_verdict()))])),
                Fixture::Partial => Arc::new(FakeChatBackend::new_results(vec![Err(ProviderError::Auth(
                    "http 401".into(),
                ))])),
            };
            let backend_for_override = backend.clone();
            let over: Box<redteam::cli::BackendOverride> = Box::new(move |em: &EffectiveModel| {
                (em.provider == "p1" && em.id == "v")
                    .then(|| Box::new(backend_for_override.clone()) as Box<dyn ChatBackend>)
            });
            let code = redteam::job::run::run_with(JobKind::Verify, &sc.job, Some(over.as_ref())).await;
            assert_eq!(code, 0, "verify/{}/{}: 报告写出即 0", fixture.name(), lang);
            let report = read_json(&sc.job.join("out/report.json"));
            assert_report_common(&sc, &report, JobKind::Verify, lang);
            // verify 的 prompt 由 backend 记录（空输入时零次调用）。
            let prompts = (0..backend.turn_count())
                .map(|i| flatten(&backend.messages_at(i)))
                .collect();
            let run = CaseRun { scenario: sc, report, prompts, input_fingerprints };
            dump_artifacts(&run, JobKind::Verify, fixture, lang);
            run
        }
    }
}

/// `REDTEAM_ACCEPTANCE_OUT` 存在时把报告与心跳复制到耐久目录（`scripts/acceptance-matrix.sh` 的证据链）。
fn dump_artifacts(run: &CaseRun, kind: JobKind, fixture: Fixture, lang: &str) {
    let Ok(root) = std::env::var("REDTEAM_ACCEPTANCE_OUT") else {
        return;
    };
    let dir = Path::new(&root).join(kind.as_str()).join(fixture.name()).join(lang);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["report.json", "state.json"] {
        let src = run.scenario.job.join("out").join(name);
        std::fs::copy(&src, dir.join(name)).unwrap_or_else(|e| panic!("{}: {e}", src.display()));
    }
}

// ───────────────────────────── 契约自动项 ─────────────────────────────

/// ①报告形状/必填字段 + ⑥无 cost + `state.json` 形状（kind 无关部分）。
fn assert_report_common(sc: &Scenario, report: &Value, kind: JobKind, lang: &str) {
    assert_eq!(
        report["schema"],
        kind.report_schema(),
        "{}/{}: schema 必须是 kind 对应常量",
        kind.as_str(),
        lang
    );
    assert_eq!(report["kind"], kind.as_str());
    assert_eq!(report["job_id"], format!("J-matrix-{}", kind.as_str()));
    assert_eq!(report["input"]["head_sha"], sc.head);
    assert_eq!(report["input"]["base_sha"], sc.base);
    assert_eq!(report["executor"]["name"], "redteam");
    assert_eq!(report["executor"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(report["findings"].is_array(), "findings 必须是数组");
    for k in ["input_tokens", "output_tokens", "cached_input_tokens", "wall_clock_ms"] {
        assert!(report["usage"][k].is_u64(), "usage.{k} 必须是整数: {}", report["usage"]);
    }
    // ⑥ 报告无 cost（跨阶段不变式④）。
    assert_no_cost(report, "report");

    let redteam = &report["extensions"]["redteam"];
    assert!(redteam.is_object(), "extensions.redteam 必须是对象: {}", report["extensions"]);

    // ⑥ 附录 D：三种 kind 恒在的键与形状。
    let language = &redteam["language"];
    assert_eq!(language["value"], lang, "language.value 必须回显配置");
    assert_eq!(language["source"], "config");
    assert_eq!(
        language.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["source", "value"],
        "language 只有 {{value, source}}"
    );
    assert_eq!(redteam["concurrency"]["mode"], "per-provider");
    let caps = redteam["concurrency"]["provider_caps"].as_object().expect("provider_caps 必须是对象");
    assert!(!caps.is_empty(), "至少要有一个 provider 的并发上限");
    assert!(caps.values().all(|v| v.as_u64().is_some_and(|n| n >= 1)), "{caps:?}");
    assert!(redteam["budget"]["wall_clock_s"].as_u64().is_some_and(|n| n >= 1));
    assert!(redteam["budget"]["truncated_items"].is_u64());
    // 已废弃键不得回潮（附录 D）。
    assert!(redteam.get("repo_degradation").is_none(), "repo_degradation 已废弃");

    match kind {
        JobKind::Review => {
            // review 的模板哈希在 RunMeta 里（内部报告结构映射，不改形状）。
            let hashes = redteam["run"]["template_hashes"].as_object().expect("run.template_hashes");
            assert!(hashes.contains_key("scanner_tail"), "{hashes:?}");
            assert!(hashes.contains_key("reviewer_tail"), "{hashes:?}");
            let ic = &redteam["input_consistency"];
            assert!(ic["match"].is_boolean(), "input_consistency.match: {ic}");
            assert!(ic["files"].is_u64(), "input_consistency.files: {ic}");
        }
        JobKind::Triage => {
            assert!(redteam["template_hashes"]["triage"].is_string(), "template_hashes.triage");
            let axes = redteam["axes"].as_array().expect("axes 必须是数组");
            assert_eq!(axes.len(), 3, "三轴各一条: {axes:?}");
            let names: Vec<&str> = axes.iter().map(|a| a["axis"].as_str().unwrap()).collect();
            assert_eq!(names, vec!["security", "drift", "intent"], "轴名与顺序稳定");
            for a in axes {
                assert!(a["ok"].is_boolean(), "{a}");
                assert!(a["rows"].is_u64(), "{a}");
            }
            assert!(redteam["blockers"].is_array(), "blockers 必须是数组");
        }
        JobKind::Verify => {
            assert!(redteam["template_hashes"]["fix_verify"].is_string(), "template_hashes.fix_verify");
            assert!(redteam["verifier"].as_str().is_some_and(|s| s.contains('/')), "verifier = provider/id");
            let v = &redteam["verify"];
            for k in [
                "calls",
                "model_failures",
                "quote_rejected",
                "duplicate_fingerprints",
            ] {
                assert!(v[k].is_u64(), "verify.{k} 必须是整数: {v}");
            }
            for k in ["resolved", "unresolved", "regressed", "inconclusive"] {
                assert!(v["counts"][k].is_u64(), "verify.counts.{k}: {v}");
            }
            assert!(v["latency_ms"]["total"].is_u64() && v["latency_ms"]["max"].is_u64(), "{v}");
            assert!(v["rate_limit"]["retries"].is_u64() && v["rate_limit"]["wait_ms"].is_u64(), "{v}");
            assert!(v["warnings"].is_array(), "warnings 必须是数组: {v}");
            assert!(report["findings"].as_array().unwrap().is_empty(), "verify 不产 finding");
        }
    }

    // ⑥ 心跳：out/state.json 合法，且不写终态。
    let state = read_json(&sc.job.join("out/state.json"));
    assert_eq!(state["schema"], "hibot.audit.state/1");
    assert_eq!(state["job_id"], format!("J-matrix-{}", kind.as_str()));
    let phase = state["phase"].as_str().expect("phase 必填");
    assert_eq!(phase, "finishing", "收尾后最后一帧必须是 finishing（终态只在 report 里）");
    let at = state["at"].as_str().expect("at 必填");
    assert!(at.len() >= 19 && at.contains('T'), "at 必须是 RFC3339 前缀: {at}");
    if let Some(p) = state.get("progress") {
        let unit = p["unit"].as_str().unwrap_or("");
        let want = if kind == JobKind::Review { "cells" } else { "items" };
        assert_eq!(unit, want, "progress.unit 固定 {want}: {p}");
        assert!(p["done"].as_u64().unwrap() <= p["total"].as_u64().unwrap(), "{p}");
    }
    if let Some(llm) = state.get("llm") {
        assert!(llm["in_flight"].as_u64().is_some(), "llm.in_flight 必填: {llm}");
    }
}

fn assert_no_cost(v: &Value, path: &str) {
    match v {
        Value::Object(m) => {
            for (k, val) in m {
                assert!(!k.to_ascii_lowercase().contains("cost"), "报告不得含 cost 字段: {path}.{k}");
                assert_no_cost(val, &format!("{path}.{k}"));
            }
        }
        Value::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                assert_no_cost(x, &format!("{path}[{i}]"));
            }
        }
        _ => {}
    }
}

fn repo_text(sc: &Scenario, rel: &str) -> String {
    std::fs::read_to_string(sc.repo.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn assert_quote_verbatim(text: &str, quote: &str, ctx: &str) {
    assert!(!quote.is_empty(), "{ctx}: quote 不得为空");
    assert!(text.contains(quote), "{ctx}: quote 必须在检出原文中逐字命中: {quote:?}");
}

/// ②指纹复算 ③quote 逐字 ④status 判定 ⑤verify 覆盖。
fn assert_fixture_case(sc: &Scenario, fixture: Fixture, run: &CaseRun, kind: JobKind, lang: &str) {
    let report = &run.report;
    let ctx = format!("{}/{}/{}", kind.as_str(), fixture.name(), lang);
    let redteam = &report["extensions"]["redteam"];
    let status = report["status"].as_str().unwrap();

    // ②③：finding 的指纹必须能由报告自身复算，quote 必须在检出里逐字命中。
    for f in report["findings"].as_array().unwrap() {
        let file = f["file"].as_str().unwrap();
        let quote = f["quote"].as_str().unwrap();
        let title = f["title"].as_str().unwrap();
        let want = fingerprint::of(file, quote, title);
        assert_eq!(f["fingerprint"], want, "{ctx}: 指纹必须可按 (file, quote, title) 复算");
        assert!(fingerprint::is_fingerprint(f["fingerprint"].as_str().unwrap()), "{ctx}");
        assert_quote_verbatim(&repo_text(sc, file), quote, &ctx);
        assert!(f["source"]["role"].is_string(), "{ctx}: source.role 必填");
        assert!(!f["severity"].as_str().unwrap().is_empty(), "{ctx}: severity 非空");
    }

    match kind {
        JobKind::Review => {
            let failed_scanner = redteam["run"]["coverage"]["failed"]["scanner"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            let failed_reviewer = redteam["run"]["coverage"]["failed"]["reviewer"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            let truncated = redteam["budget"]["truncated_items"].as_u64().unwrap();
            // ④ status 判定只看 coverage / RunMeta，不看 LLM。
            match fixture {
                Fixture::Clean | Fixture::KnownVuln => {
                    assert_eq!(status, "ok", "{ctx}: {report}");
                    assert_eq!(failed_scanner + failed_reviewer + truncated as usize, 0, "{ctx}: ok 不得有失败格");
                    assert!(report.get("status_reason").is_none(), "{ctx}: ok 不写 reason");
                    assert!(redteam.get("status_code").is_none(), "{ctx}: ok 不写 status_code");
                }
                Fixture::Partial => {
                    assert_eq!(status, "partial", "{ctx}: {report}");
                    assert_eq!(failed_scanner, 1, "{ctx}: 恰有一个 scanner 格失败");
                    assert_eq!(redteam["status_code"], "scan_incomplete:1/2");
                    // 文案随 language，机器码不随：这里只断言语言中立的计数片段。
                    assert!(report["status_reason"].as_str().unwrap().contains("1/2"), "{ctx}");
                }
            }
            if fixture == Fixture::KnownVuln {
                assert_eq!(report["findings"].as_array().unwrap().len(), 1, "{ctx}: {report}");
            }
            if fixture == Fixture::Clean {
                assert!(report["findings"].as_array().unwrap().is_empty(), "{ctx}");
            }
        }
        JobKind::Triage => {
            let axes = redteam["axes"].as_array().unwrap();
            let failed: Vec<&str> = axes
                .iter()
                .filter(|a| a["ok"] == false)
                .map(|a| a["axis"].as_str().unwrap())
                .collect();
            match fixture {
                Fixture::Clean | Fixture::KnownVuln => {
                    assert_eq!(status, "ok", "{ctx}: {report}");
                    assert!(failed.is_empty(), "{ctx}: ok 不得有失败轴");
                    assert!(report["summary"].is_object(), "{ctx}: ok 必须带 summary");
                    assert!(report.get("status_reason").is_none(), "{ctx}: ok 不写 reason");
                }
                Fixture::Partial => {
                    assert_eq!(status, "partial", "{ctx}: {report}");
                    assert_eq!(failed, vec!["security"], "{ctx}: security 轴失败");
                    assert!(report.get("summary").is_none(), "{ctx}: partial 不得用零值凑 summary");
                    assert_eq!(redteam["status_code"], "triage_axis_failed:security");
                    assert!(report["status_reason"].as_str().unwrap().contains("security"));
                }
            }
            if fixture == Fixture::KnownVuln {
                assert_eq!(report["findings"].as_array().unwrap().len(), 2, "security 行 + structural drift");
                let blockers = redteam["blockers"].as_array().unwrap();
                assert_eq!(blockers.len(), 2, "blockers = structural + 未兑现声明: {report}");
                assert!(blockers.iter().all(|b| b.get("kind").is_some()), "{blockers:?}");
                assert!(blockers.iter().all(|b| b.get("fingerprint").is_none()), "blockers 不得冒充 findings");
                assert_eq!(report["summary"]["blockers_count"], 2);
            }
            if fixture == Fixture::Clean {
                assert!(report["findings"].as_array().unwrap().is_empty(), "{ctx}");
                assert!(redteam["blockers"].as_array().unwrap().is_empty(), "{ctx}");
                assert_eq!(report["summary"]["blockers_count"], 0);
            }
        }
        JobKind::Verify => {
            let vs = report["verifications"].as_array().expect("verify 必须带 verifications");
            // ⑤ 覆盖不变量：输入指纹集合 == 输出集合；空输入才允许空输出。
            let mut got: Vec<&str> = vs.iter().map(|v| v["finding_fingerprint"].as_str().unwrap()).collect();
            got.sort_unstable();
            let mut want = run.input_fingerprints.clone();
            want.sort();
            assert_eq!(got, want, "{ctx}: 逐条覆盖输入指纹集合，没有多余/遗漏");
            assert_eq!(got.is_empty(), run.input_fingerprints.is_empty(), "{ctx}: 空输出仅当空输入");
            for v in vs {
                assert!(!v["evidence"].as_str().unwrap().is_empty(), "{ctx}: 四态都要证据");
                if let Some(q) = v["quote"].as_str() {
                    assert_quote_verbatim(&repo_text(sc, "src/app.py"), q, &ctx);
                }
            }
            let counts = &redteam["verify"]["counts"];
            match fixture {
                Fixture::Clean => {
                    assert_eq!(status, "ok", "{ctx}");
                    assert!(vs.is_empty(), "{ctx}");
                    assert_eq!(redteam["verify"]["calls"], 0, "{ctx}");
                    assert!(report.get("status_reason").is_none(), "{ctx}");
                }
                Fixture::KnownVuln => {
                    assert_eq!(status, "ok", "{ctx}: unresolved 不是 partial");
                    assert_eq!(vs.len(), 1, "{ctx}");
                    assert_eq!(vs[0]["status"], "unresolved", "{ctx}");
                    assert_eq!(counts["unresolved"], 1);
                    assert_eq!(vs[0]["quote"].as_str(), Some(VULN), "逐字命中 → 原样保留");
                    // evidence 降级警告是机器字段：不得随语言变化（下面成对比较）。
                    assert!(
                        redteam["verify"]["warnings"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|w| w.as_str().unwrap().starts_with("symbol_blast_radius_skipped:unsupported_language:")),
                        "非支持语言必须留 warning: {}",
                        redteam["verify"]["warnings"]
                    );
                }
                Fixture::Partial => {
                    assert_eq!(status, "partial", "{ctx}: 模型失败 ⇒ partial，不得 ok");
                    assert_eq!(vs[0]["status"], "inconclusive", "{ctx}: 失败不得塞进 unresolved");
                    assert_eq!(counts["inconclusive"], 1);
                    assert_eq!(redteam["verify"]["model_failures"], 1);
                    assert_eq!(redteam["status_code"], "verifications_inconclusive:1");
                    assert!(report["status_reason"].as_str().unwrap().contains("inconclusive"));
                }
            }
        }
    }
}

/// 语言成对断言：过程（prompt 指令）与结果（`status_reason`）随语言；机器字段不随。
fn assert_language_pair(kind: JobKind, fixture: Fixture, zh: &CaseRun, en: &CaseRun) {
    let ctx = format!("{}/{}", kind.as_str(), fixture.name());

    // 过程：三种 kind 的 prompt 都必须带配置语言；en 运行还要证明没有混入中文指令。
    for (run, lang, directive, anti) in [
        (zh, "zh_CN", "Simplified Chinese (简体中文)", None),
        (en, "en_US", "English", Some("简体中文")),
    ] {
        // verify 的空输入夹具不发任何 prompt（没有可核验项）——除此外三种 kind 必须带语言指令。
        if run.prompts.is_empty() {
            assert!(
                kind == JobKind::Verify && fixture == Fixture::Clean,
                "{ctx}/{lang}: 只有 verify 空输入允许零 prompt"
            );
            assert_eq!(run.report["extensions"]["redteam"]["language"]["value"], lang, "{ctx}");
            continue;
        }
        assert!(run.prompts.iter().any(|p| p.contains(directive)), "{ctx}/{lang}: prompt 缺语言指令");
        // 反向检查只对无歧义的标记做：`简体中文` 只可能来自语言指令；`English`
        // 在 verify 模板里是契约用词（“Keep the status value in English”），不能当反例。
        if let Some(anti) = anti {
            if let Some(bad) = run.prompts.iter().find(|p| p.contains(anti)) {
                let head: String = bad.chars().take(600).collect();
                panic!("{ctx}/{lang}: prompt 混入了另一种语言指令 {anti:?}: {head}");
            }
        }
        assert_eq!(run.report["extensions"]["redteam"]["language"]["value"], lang, "{ctx}");
    }

    // 结果：partial 的 status_reason 必须随语言（中文模板 vs 英文模板）。
    let zh_reason = zh.report.get("status_reason").and_then(Value::as_str);
    let en_reason = en.report.get("status_reason").and_then(Value::as_str);
    if fixture == Fixture::Partial {
        let (zr, er) = (zh_reason.expect("zh partial 必须有人读 reason"), en_reason.expect("en partial 必须有人读 reason"));
        assert_ne!(zr, er, "{ctx}: status_reason 必须随 language 变化");
        assert!(zr.chars().any(is_cjk), "{ctx}: zh_CN 的 reason 必须是中文: {zr}");
        assert!(!er.chars().any(is_cjk), "{ctx}: en_US 的 reason 不该是中文: {er}");
    }

    // finding 文本：triage 的合成标题随 language（review 的 finding 文本来自模型，固定夹具下两语言同文案）。
    if kind == JobKind::Triage && fixture == Fixture::KnownVuln {
        let titles = |run: &CaseRun| -> Vec<String> {
            run.report["findings"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| f["title"].as_str().unwrap().to_string())
                .collect()
        };
        let (zt, et) = (titles(zh), titles(en));
        assert_ne!(zt, et, "{ctx}: finding 标题必须随 language 变化");
        assert!(zt.iter().any(|t| t.chars().any(is_cjk)), "{ctx}: zh_CN 标题应为中文: {zt:?}");
        assert!(et.iter().all(|t| !t.chars().any(is_cjk)), "{ctx}: en_US 标题不应是中文: {et:?}");
    }

    // 机器字段：不得随语言变化。
    let zr = &zh.report["extensions"]["redteam"];
    let er = &en.report["extensions"]["redteam"];
    assert_eq!(zr["status_code"], er["status_code"], "{ctx}: status_code 是机器码");
    assert_eq!(zr["concurrency"], er["concurrency"], "{ctx}: concurrency 是机器字段");
    assert_eq!(zr["budget"], er["budget"], "{ctx}: budget 是机器字段");
    match kind {
        JobKind::Review => {
            assert_eq!(
                zr["run"]["template_hashes"], er["run"]["template_hashes"],
                "{ctx}: template_hashes 不随语言"
            );
            assert_eq!(zr["input_consistency"], er["input_consistency"], "{ctx}: 一致性披露不随语言");
        }
        JobKind::Triage => {
            assert_eq!(zr["template_hashes"], er["template_hashes"], "{ctx}");
            let axes = |r: &Value| -> Vec<Value> {
                r["axes"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|a| json!({"axis": a["axis"], "ok": a["ok"], "rows": a["rows"]}))
                    .collect()
            };
            assert_eq!(axes(zr), axes(er), "{ctx}: 轴结果不随语言");
            assert_eq!(zr["blockers"], er["blockers"], "{ctx}: blockers 不随语言");
        }
        JobKind::Verify => {
            assert_eq!(zr["template_hashes"], er["template_hashes"], "{ctx}");
            assert_eq!(zr["verifier"], er["verifier"], "{ctx}");
            let machine = |r: &Value| -> Value {
                let v = &r["verify"];
                json!({
                    "counts": v["counts"],
                    "calls": v["calls"],
                    "model_failures": v["model_failures"],
                    "quote_rejected": v["quote_rejected"],
                    "duplicate_fingerprints": v["duplicate_fingerprints"],
                    "warnings": v["warnings"],
                })
            };
            assert_eq!(machine(zr), machine(er), "{ctx}: verify 计数与 warnings 不随语言");
        }
    }
}

fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}

// ───────────────────────────── 矩阵：每 kind 一个用例（3 夹具 × 2 语言） ─────────────────────────────

/// 环境变量是进程级的：partial 夹具要保证 p2 的 key 真的没注入。
static ENV_LOCK: Mutex<()> = Mutex::new(());

#[tokio::test]
async fn review_matrix_covers_three_fixtures_in_two_languages() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("P2_KEY_MATRIX_NOT_SET");
    let server = Always401::start();
    for fixture in [Fixture::Clean, Fixture::KnownVuln, Fixture::Partial] {
        let mut runs = Vec::new();
        for (lang, _) in LANGS {
            let run = run_case(JobKind::Review, fixture, lang, server.port).await;
            assert_fixture_case(&run.scenario, fixture, &run, JobKind::Review, lang);
            runs.push(run);
        }
        assert_language_pair(JobKind::Review, fixture, &runs[0], &runs[1]);
    }
}

#[tokio::test]
async fn triage_matrix_covers_three_fixtures_in_two_languages() {
    for fixture in [Fixture::Clean, Fixture::KnownVuln, Fixture::Partial] {
        let mut runs = Vec::new();
        for (lang, _) in LANGS {
            let run = run_case(JobKind::Triage, fixture, lang, 0).await;
            assert_fixture_case(&run.scenario, fixture, &run, JobKind::Triage, lang);
            runs.push(run);
        }
        assert_language_pair(JobKind::Triage, fixture, &runs[0], &runs[1]);
    }
}

#[tokio::test]
async fn verify_matrix_covers_three_fixtures_in_two_languages() {
    for fixture in [Fixture::Clean, Fixture::KnownVuln, Fixture::Partial] {
        let mut runs = Vec::new();
        for (lang, _) in LANGS {
            let run = run_case(JobKind::Verify, fixture, lang, 0).await;
            assert_fixture_case(&run.scenario, fixture, &run, JobKind::Verify, lang);
            runs.push(run);
        }
        assert_language_pair(JobKind::Verify, fixture, &runs[0], &runs[1]);
    }
}
