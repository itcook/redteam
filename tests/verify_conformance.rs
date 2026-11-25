//! Phase 4 `verify` kind 的合成夹具（计划 4.5）：**不用 hibot 的示例**（那是编造结论 + 节选 diff），
//! 自己造 base→head 真仓 + 确定性 provider，逐条钉住四态、删除式修复、改名/移行与 quote 反例。
//!
//! 每个用例的「模型判决」由 [`Router`] 按 prompt 里的指纹路由，判决本身是假的——被测的是
//! **管线**：证据装配、逐条覆盖、quote 逐字校验、四态 → 报告状态、`inconclusive` 的降级链路。

use redteam::config::EffectiveModel;
use redteam::job::fingerprint;
use redteam::job::manifest::JobKind;
use redteam::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage};
use redteam::provider::ProviderError;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

const VULN: &str = "return db.query('SELECT * FROM users WHERE id=' + id);";
const FIXED: &str = "return db.query('SELECT * FROM users WHERE id=?', [id]);";

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
        input_tokens: 100,
        output_tokens: 10,
        cached_input_tokens: 5,
        finish_reason: Some("stop".into()),
        reasoning_tokens: None,
    }
}

/// 按 prompt 里出现的指纹前缀决定回什么；记录看到的 prompt（断言「只喂该文件」）。
enum Rule {
    Text(String),
    Auth(String),
}

struct Router {
    rules: Vec<(String, Rule)>,
    seen: Mutex<Vec<String>>,
}

impl Router {
    fn new(rules: Vec<(String, String)>) -> Self {
        Self {
            rules: rules.into_iter().map(|(needle, body)| (needle, Rule::Text(body))).collect(),
            seen: Mutex::new(Vec::new()),
        }
    }
    fn failing(needle: &str, message: &str) -> Self {
        Self { rules: vec![(needle.to_string(), Rule::Auth(message.to_string()))], seen: Mutex::new(Vec::new()) }
    }
    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ChatBackend for Router {
    async fn turn(&self, messages: &[ChatMessage], _tools: &[redteam::tools::ToolSpec]) -> Result<AssistantTurn, ProviderError> {
        let text: String = messages
            .iter()
            .map(|m| match m {
                ChatMessage::System(s) => s.clone(),
                ChatMessage::User(u) => u.clone(),
                ChatMessage::UserBlocks(bs) => bs.iter().map(|b| b.text.clone()).collect::<Vec<_>>().join("\n"),
                ChatMessage::Assistant { text, .. } => text.clone(),
                ChatMessage::ToolResults(_) => String::new(),
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.seen.lock().unwrap().push(text.clone());
        for (needle, body) in &self.rules {
            if text.contains(needle) {
                return match body {
                    Rule::Text(t) => Ok(turn(t)),
                    Rule::Auth(m) => Err(ProviderError::Auth(m.clone())),
                };
            }
        }
        Err(ProviderError::BadRequest("no router rule matched this prompt".into()))
    }
}

/// 模型输出（契约形态）：`{"status","evidence","quote?"}`。
fn verdict(status: &str, evidence: &str, quote: Option<&str>) -> String {
    match quote {
        Some(q) => serde_json::json!({"status": status, "evidence": evidence, "quote": q}).to_string(),
        None => serde_json::json!({"status": status, "evidence": evidence}).to_string(),
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    job: PathBuf,
    repo: PathBuf,
}

/// base 提交 → `mutate` 改工作树 → head 提交；再把它做成一个 verify job（真 diff / 真 merge_base）。
fn make_job(base_files: &[(&str, &str)], mutate: impl FnOnce(&Path), findings: &str) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    for (path, body) in base_files {
        let p = repo.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    mutate(&repo);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let diff = git(&repo, &["diff", &format!("{base}..{head}")]);

    let job = tmp.path().join("job");
    std::fs::create_dir_all(job.join("config")).unwrap();
    std::fs::create_dir_all(job.join("input")).unwrap();
    std::fs::create_dir_all(job.join("out")).unwrap();
    std::fs::write(job.join("config/executor.redteam.json"), CONFIG).unwrap();
    std::fs::write(job.join("input/findings.json"), findings).unwrap();
    let patch = format!("{diff}\n");
    std::fs::write(job.join("input/diff.patch"), &patch).unwrap();
    let mb = format!("{base}\n");
    std::fs::write(job.join("input/mb.sha"), &mb).unwrap();
    let inputs = serde_json::json!({
        "findings": {"path": "input/findings.json", "sha256": sha256_hex(findings.as_bytes()), "schema": "hibot.audit.findings/1"},
        "diff_patch": {"path": "input/diff.patch", "sha256": sha256_hex(patch.as_bytes())},
        "merge_base": {"path": "input/mb.sha", "sha256": sha256_hex(mb.as_bytes())},
    });
    let manifest = serde_json::json!({
        "schema": "hibot.audit.job.verify/1", "job_id": "J-verify", "kind": "verify",
        "repo": {"id": 1, "full_name": "org/repo"},
        "pr": {"number": 9, "head_sha": head, "base_sha": base},
        "executor": {"name": "redteam", "version": "0.1.0"},
        "limits": {"wall_clock_s": 120},
        "paths": {"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"},
        "inputs": inputs,
    });
    std::fs::write(job.join("job.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    Fixture { _tmp: tmp, job, repo }
}

const CONFIG: &str = r#"{
  "settings": {
    "language": "en_US",
    "verify": {"models": [{"provider": "p1", "id": "v"}]}
  },
  "providers": {
    "p1": {"api": "openai-completions", "baseUrl": "http://127.0.0.1:1", "apiKeyEnv": "P1_KEY",
           "models": [{"id": "v"}]}
  }
}"#;

fn finding(file: &str, quote: &str, line: u32, title: &str) -> serde_json::Value {
    serde_json::json!({
        "fingerprint": fingerprint::of(file, quote, title),
        "severity": "high", "confidence": 0.9,
        "file": file, "line_start": line, "line_end": line,
        "quote": quote, "title": title, "body": "harm", "recommendation": "parameterize",
    })
}

fn db_line(table: &str) -> String {
    format!("return db.query('SELECT * FROM {table} WHERE id=' + id);")
}

fn fixed_line(table: &str) -> String {
    format!("return db.query('SELECT * FROM {table} WHERE id=?', [id]);")
}

fn file_with(line: &str) -> String {
    format!("function find(id) {{\n  {line}\n}}\n")
}

fn findings_json(items: &[serde_json::Value]) -> String {
    serde_json::json!(items).to_string()
}

/// 路由 needle：指纹的前 24 字符（`sha256:` + 17 hex），在 prompt 里唯一。
fn needle_of(f: &serde_json::Value) -> String {
    f["fingerprint"].as_str().unwrap().chars().take(24).collect()
}

fn override_for(router: Arc<Router>) -> Box<redteam::cli::BackendOverride<'static>> {
    Box::new(move |em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> {
        (em.provider == "p1" && em.id == "v").then(|| Box::new(router.clone()) as Box<dyn ChatBackend>)
    })
}

fn read_report(job: &Path) -> serde_json::Value {
    let p = job.join("out/report.json");
    serde_json::from_slice(&std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

async fn run(fx: &Fixture, router: Arc<Router>) -> serde_json::Value {
    let over = override_for(router);
    let code = redteam::job::run::run_with(JobKind::Verify, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0, "verify 报告写出即 0（partial 也算已交付）");
    read_report(&fx.job)
}

// ───────────────────────────── 四态 ─────────────────────────────

#[tokio::test]
async fn four_states_are_reported_with_evidence_and_inconclusive_makes_partial() {
    let v_resolved = db_line("t_resolved");
    let v_unresolved = db_line("t_unresolved");
    let v_regressed = db_line("t_regressed");
    let v_inconclusive = db_line("t_inconclusive");
    let base = [
        ("src/resolved.js", file_with(&v_resolved)),
        ("src/unresolved.js", file_with(&v_unresolved)),
        ("src/regressed.js", file_with(&v_regressed)),
        ("src/inconclusive.js", file_with(&v_inconclusive)),
    ];
    let base_refs: Vec<(&str, &str)> = base.iter().map(|(p, b)| (*p, b.as_str())).collect();
    let f_resolved = finding("src/resolved.js", &v_resolved, 2, "sql injection in resolved.js");
    let f_unresolved = finding("src/unresolved.js", &v_unresolved, 2, "sql injection in unresolved.js");
    let f_regressed = finding("src/regressed.js", &v_regressed, 2, "sql injection in regressed.js");
    let f_inconclusive = finding("src/inconclusive.js", &v_inconclusive, 2, "sql injection in inconclusive.js");
    let router = Arc::new(Router::new(vec![
        (needle_of(&f_resolved), verdict("resolved", "改用占位参数，注入面消失", Some(&fixed_line("t_resolved")))),
        (needle_of(&f_unresolved), verdict("unresolved", "拼接触点仍在", Some(&v_unresolved))),
        (
            needle_of(&f_regressed),
            verdict("regressed", "修复把 id 拼进了 SQL 片段，注入面扩大", Some("const sql = 'SELECT * FROM users WHERE id=' + id;")),
        ),
        (needle_of(&f_inconclusive), verdict("inconclusive", "材料不足以判定", None)),
    ]));
    let findings = findings_json(&[f_resolved.clone(), f_unresolved.clone(), f_regressed.clone(), f_inconclusive.clone()]);
    let fx = make_job(&base_refs, |repo| {
        std::fs::write(repo.join("src/resolved.js"), file_with(&fixed_line("t_resolved"))).unwrap();
        // unresolved.js / inconclusive.js 原样不动：缺陷仍在。
        std::fs::write(
            repo.join("src/regressed.js"),
            "function find(id) {\n  const sql = 'SELECT * FROM users WHERE id=' + id;\n  return db.query(sql);\n}\n",
        )
        .unwrap();
        std::fs::write(repo.join("src/inconclusive.js"), file_with(&v_inconclusive)).unwrap();
    }, &findings);

    let r = run(&fx, router.clone()).await;
    assert_eq!(r["kind"], "verify");
    assert_eq!(r["schema"], "hibot.audit.report.verify/1");
    assert_eq!(r["status"], "partial", "出现 inconclusive ⇒ 不得 ok");
    let reason = r["status_reason"].as_str().unwrap();
    assert!(reason.contains("1 findings inconclusive"), "{reason}");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "verifications_inconclusive:1");

    let vs = r["verifications"].as_array().unwrap();
    let got: Vec<(&str, &str)> = vs
        .iter()
        .map(|v| (v["finding_fingerprint"].as_str().unwrap(), v["status"].as_str().unwrap()))
        .collect();
    assert_eq!(
        got,
        vec![
            (f_resolved["fingerprint"].as_str().unwrap(), "resolved"),
            (f_unresolved["fingerprint"].as_str().unwrap(), "unresolved"),
            (f_regressed["fingerprint"].as_str().unwrap(), "regressed"),
            (f_inconclusive["fingerprint"].as_str().unwrap(), "inconclusive"),
        ],
        "四态按输入顺序逐条覆盖: {r}"
    );
    assert_eq!(vs[0]["quote"].as_str().unwrap(), fixed_line("t_resolved"), "逐字命中 → 原样保留");
    assert_eq!(vs[1]["quote"].as_str().unwrap(), v_unresolved);
    assert!(vs[3].get("quote").is_none());
    assert!(vs.iter().all(|v| !v["evidence"].as_str().unwrap().is_empty()), "四态都要证据");
    assert_eq!(r["extensions"]["redteam"]["verify"]["counts"]["inconclusive"], 1);
    assert!(r["findings"].as_array().unwrap().is_empty(), "verify 不产 finding");

    // 4.2：每条 prompt 只带自己那个文件的 hunk（不含整 PR、不含别人的段）。
    let seen = router.seen();
    assert_eq!(seen.len(), 4, "逐条一次调用");
    for p in &seen {
        assert!(!p.contains("diff --git"), "整 PR 的 patch 头不得进 prompt");
        let others: Vec<&str> = ["src/resolved.js", "src/unresolved.js", "src/regressed.js", "src/inconclusive.js"]
            .into_iter()
            .filter(|f| !p.contains(&format!("File: {f}")))
            .collect();
        for other in others {
            assert!(!p.contains(&format!("### {other}")), "不得把 {other} 的 hunk 喂给别条: {p}");
        }
    }
}

#[tokio::test]
async fn resolved_unresolved_regressed_without_inconclusive_is_ok() {
    let v_a = db_line("t_a");
    let v_b = db_line("t_b");
    let base = [("src/resolved.js", file_with(&v_a)), ("src/unresolved.js", file_with(&v_b))];
    let f_resolved = finding("src/resolved.js", &v_a, 2, "sql injection a");
    let f_unresolved = finding("src/unresolved.js", &v_b, 2, "sql injection b");
    let router = Arc::new(Router::new(vec![
        (needle_of(&f_resolved), verdict("resolved", "已参数化", Some(&fixed_line("t_a")))),
        (needle_of(&f_unresolved), verdict("unresolved", "拼接仍在", Some(&v_b))),
    ]));
    let findings = findings_json(&[f_resolved, f_unresolved]);
    let base_refs: Vec<(&str, &str)> = base.iter().map(|(p, b)| (*p, b.as_str())).collect();
    let fx = make_job(&base_refs, |repo| {
        std::fs::write(repo.join("src/resolved.js"), file_with(&fixed_line("t_a"))).unwrap();
    }, &findings);
    let r = run(&fx, router).await;
    assert_eq!(r["status"], "ok");
    assert!(r.get("status_reason").is_none());
    assert_eq!(r["extensions"]["redteam"]["verify"]["counts"]["resolved"], 1);
    assert_eq!(r["extensions"]["redteam"]["verify"]["counts"]["unresolved"], 1);
}

// ───────────────────── 删除 / 改名 / 移行（4.3） ─────────────────────

#[tokio::test]
async fn deleted_file_is_resolved_by_construct_disappearance() {
    let path = "src/legacy.js";
    let f = finding(path, VULN, 2, "sql injection in legacy");
    let router = Arc::new(Router::new(vec![(
        needle_of(&f),
        verdict("resolved", "整个文件被删除，构造不复存在", Some(VULN)),
    )]));
    let findings = findings_json(&[f]);
    let fx = make_job(&[(path, &format!("function find(id) {{\n  {VULN}\n}}\n"))], |repo| {
        std::fs::remove_file(repo.join(path)).unwrap();
    }, &findings);
    let r = run(&fx, router.clone()).await;
    assert_eq!(r["status"], "ok", "删除式修复可判 resolved: {r}");
    assert_eq!(r["verifications"][0]["status"], "resolved");
    assert_eq!(r["verifications"][0]["quote"].as_str().unwrap(), VULN, "quote 来自被删行（diff 的 - 行）");
    let seen = router.seen();
    assert!(seen[0].contains("deleted"), "变更形态必须告知模型: {}", seen[0]);
    assert!(seen[0].contains("absent from the post-fix checkout"), "删除后没有快照可读，必须显式说明");
}

#[tokio::test]
async fn renamed_file_still_carrying_the_construct_is_unresolved_at_the_new_path() {
    let old = "src/old.js";
    let new = "src/new.js";
    let f = finding(old, VULN, 2, "sql injection renamed");
    let router = Arc::new(Router::new(vec![(needle_of(&f), verdict("unresolved", "改名后仍在", Some(VULN)))]));
    let findings = findings_json(&[f]);
    let fx = make_job(&[(old, &format!("function find(id) {{\n  {VULN}\n}}\n"))], |repo| {
        std::fs::rename(repo.join(old), repo.join(new)).unwrap();
    }, &findings);
    let r = run(&fx, router.clone()).await;
    assert_eq!(r["status"], "ok");
    assert_eq!(r["verifications"][0]["status"], "unresolved");
    let seen = router.seen();
    assert!(seen[0].contains(&format!("Renamed from (pre-fix path): {old}")), "{}", seen[0]);
    assert!(seen[0].contains(&format!("Path (post-fix): {new}")), "{}", seen[0]);
    assert!(seen[0].contains(VULN), "改名后仍要取到新路径的代码（快照）");
}

#[tokio::test]
async fn moved_construct_is_resolved_and_evidence_comes_from_related_hunks() {
    let path = "src/a.js";
    let f = finding(path, VULN, 2, "sql injection moved away");
    // 构造被搬到 src/b.js：related hunks 必须带上 b.js 的 + 行，否则模型看不到"去哪了"。
    let router = Arc::new(Router::new(vec![(
        needle_of(&f),
        verdict("resolved", "构造已移出本文件（见 related hunks）", Some(VULN)),
    )]));
    let findings = findings_json(&[f]);
    let fx = make_job(
        &[(path, &format!("function find(id) {{\n  {VULN}\n}}\n"))],
        |repo| {
            std::fs::write(repo.join(path), "function find(id) {\n  return fetchById(id);\n}\n").unwrap();
            std::fs::write(repo.join("src/b.js"), format!("function findRaw(id) {{\n  {VULN}\n}}\n")).unwrap();
        },
        &findings,
    );
    let r = run(&fx, router.clone()).await;
    assert_eq!(r["status"], "ok");
    assert_eq!(r["verifications"][0]["status"], "resolved");
    let seen = router.seen();
    assert!(seen[0].contains("### src/b.js"), "跨文件修复必须取相关 hunks: {}", seen[0]);
    assert!(seen[0].contains(&format!("+  {VULN}")), "related hunk 要含新增行");
}

// ───────────────────── quote 反例（4.4） ─────────────────────

#[tokio::test]
async fn quote_with_different_whitespace_is_downgraded_but_verbatim_is_kept() {
    // 反例：只改缩进 → quote 未逐字命中 → 降级 inconclusive（不得把它当作 resolved 的证据）。
    let f = finding("src/app.js", VULN, 2, "sql injection whitespace");
    let drifted = FIXED.replace(", [id])", ",  [id])"); // 内部多一个空格（首尾空白按约定忽略）
    let router = Arc::new(Router::new(vec![(needle_of(&f), verdict("resolved", "已参数化", Some(&drifted)))]));
    let findings = findings_json(&[f.clone()]);
    let fx = make_job(&[("src/app.js", &format!("function find(id) {{\n  {VULN}\n}}\n"))], |repo| {
        std::fs::write(repo.join("src/app.js"), format!("function find(id) {{\n  {FIXED}\n}}\n")).unwrap();
    }, &findings);
    let r = run(&fx, router).await;
    assert_eq!(r["status"], "partial");
    assert_eq!(r["verifications"][0]["status"], "inconclusive", "空白不符的 quote 必须使判决失效: {r}");
    assert_eq!(r["extensions"]["redteam"]["verify"]["quote_rejected"], 1);
    assert!(r["status_reason"].as_str().unwrap().contains("1 findings inconclusive"));

    // 对照：同一场景，逐字 quote → resolved + ok。
    let f2 = finding("src/app.js", VULN, 2, "sql injection whitespace");
    let router = Arc::new(Router::new(vec![(needle_of(&f2), verdict("resolved", "已参数化", Some(FIXED)))]));
    let findings = findings_json(&[f2]);
    let fx = make_job(&[("src/app.js", &format!("function find(id) {{\n  {VULN}\n}}\n"))], |repo| {
        std::fs::write(repo.join("src/app.js"), format!("function find(id) {{\n  {FIXED}\n}}\n")).unwrap();
    }, &findings);
    let r = run(&fx, router).await;
    assert_eq!(r["status"], "ok");
    assert_eq!(r["verifications"][0]["status"], "resolved");
}

// ───────────────────── provider 失败（4.4） ─────────────────────

#[tokio::test]
async fn provider_auth_failure_is_inconclusive_partial_never_unresolved() {
    let f = finding("src/app.js", VULN, 2, "sql injection auth");
    let router = Arc::new(Router::failing(&needle_of(&f), "401 unauthorized"));
    let findings = findings_json(&[f]);
    let fx = make_job(&[("src/app.js", &format!("function find(id) {{\n  {VULN}\n}}\n"))], |repo| {
        std::fs::write(repo.join("src/app.js"), format!("function find(id) {{\n  {FIXED}\n}}\n")).unwrap();
    }, &findings);
    let r = run(&fx, router).await;
    assert_eq!(r["status"], "partial", "模型失败 ⇒ partial，不得 ok: {r}");
    let v = &r["verifications"][0];
    assert_eq!(v["status"], "inconclusive", "失败不得塞进 unresolved: {r}");
    assert!(v["evidence"].as_str().unwrap().contains("failed"), "{v}");
    assert!(v.get("quote").is_none());
    assert_eq!(r["extensions"]["redteam"]["status_code"], "verifications_inconclusive:1");
    assert!(r["status_reason"].as_str().unwrap().contains("1 findings inconclusive"));
    assert_eq!(r["extensions"]["redteam"]["verify"]["model_failures"], 1);
}

// ───────────── 作业级失败的覆盖（契约 §3.3，协调者 2026-11-25 裁 (a)） ─────────────

#[tokio::test]
async fn job_level_error_still_covers_every_fingerprint_as_inconclusive() {
    // findings 可解析 + 检出消失 → 作业级 error，但覆盖不破：每条都是「未核验」。
    let f1 = finding("src/app.js", VULN, 2, "sql injection repo gone a");
    let f2 = finding("src/other.js", VULN, 2, "sql injection repo gone b");
    let router = Arc::new(Router::new(vec![]));
    let findings = findings_json(&[f1.clone(), f2.clone()]);
    let fx = make_job(&[("src/app.js", &format!("function find(id) {{\n  {VULN}\n}}\n"))], |_repo| {}, &findings);
    std::fs::remove_dir_all(&fx.repo).unwrap();
    let over = override_for(router);
    let code = redteam::job::run::run_with(JobKind::Verify, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 1, "error 报告 → 1");
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error", "补条目不得把状态改成 partial");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "input_path_invalid:paths.repo");
    assert!(r["status_reason"].as_str().unwrap().contains("paths.repo"));
    let vs = r["verifications"].as_array().unwrap();
    assert_eq!(vs.len(), 2, "findings 可解析 ⇒ 逐条覆盖: {r}");
    assert!(vs.iter().all(|v| v["status"] == "inconclusive"), "作业失败不得冒充 resolved/unresolved");
    let ev = vs[0]["evidence"].as_str().unwrap();
    assert!(ev.contains("job failed before verification") && ev.contains("input_path_invalid:paths.repo"), "{ev}");
    assert_eq!(vs[0]["finding_fingerprint"].as_str().unwrap(), f1["fingerprint"].as_str().unwrap());
    assert_eq!(vs[1]["finding_fingerprint"].as_str().unwrap(), f2["fingerprint"].as_str().unwrap());
}

#[tokio::test]
async fn broken_findings_are_the_only_case_allowed_to_leave_verifications_empty() {
    // findings 本身坏 → 给不出指纹集合 → 空数组（唯一合法例外），且仍是 error。
    let router = Arc::new(Router::new(vec![]));
    let findings = findings_json(&[serde_json::json!({"fingerprint": "sha256:not-a-fingerprint"})]);
    let fx = make_job(&[("src/app.js", &format!("function find(id) {{\n  {VULN}\n}}\n"))], |_repo| {}, &findings);
    let over = override_for(router);
    let code = redteam::job::run::run_with(JobKind::Verify, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 1);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "error");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "invalid_fingerprint_input");
    assert_eq!(r["verifications"], serde_json::json!([]), "findings 坏 → 无指纹可覆盖");
}

// ───────────────────── 流程隔离（4.0） ─────────────────────

#[test]
fn fixverify_module_is_isolated_from_consolidate_and_refuter_templates() {
    for (name, src) in [
        ("mod.rs", include_str!("../src/fixverify/mod.rs")),
        ("input.rs", include_str!("../src/fixverify/input.rs")),
        ("evidence.rs", include_str!("../src/fixverify/evidence.rs")),
        ("prompt.rs", include_str!("../src/fixverify/prompt.rs")),
    ] {
        assert!(
            !src.contains("use crate::consolidate") && !src.contains("crate::consolidate::"),
            "src/fixverify/{name} 不得引用 consolidate（4.0 流程隔离：模板/解析/判定表零复用）"
        );
        assert!(
            !src.contains("templates/refuter.md"),
            "src/fixverify/{name} 不得复用 refuter 模板"
        );
    }
    // 模板字节本身也必须不同（哈希取自整份模板原文）。
    let fix_tpl = include_str!("../src/prompt/templates/fix-verify.md");
    let refuter_tpl = include_str!("../src/prompt/templates/refuter.md");
    assert_ne!(fix_tpl, refuter_tpl, "fix-verify.md 与 refuter.md 必须是两份不同的模板");

    let fix_hash = redteam::fixverify::prompt::template_hash();
    assert_ne!(fix_hash, redteam::prompt::template_hashes().refuter_tail, "fix-verify 模板哈希必须与 refuter 不同");
    assert_ne!(fix_hash, redteam::prompt::template_hashes().reviewer_tail);
    assert_ne!(fix_hash, redteam::prompt::template_hashes().scanner_tail);
}
