//! 3.9：用**真 triage 报告**（`tests/fixtures/triage/real-cli18.json`，cli #18 实跑）回归 2.3。
//!
//! 断言三件事：
//! 1. `summary.routing.focus[]` 进了 scanner prompt；
//! 2. `summary.routing.specs[]` 的 doc 内容进了共享头，且 `prior_routing` 披露一致；
//! 3. `wall_clock_s` 只能收紧预算（=1s + 慢 backend → `partial` / `wall_clock_exhausted`）。
//!
//! 阻断阈值不变由 `job::run::tests::prior_routing_cannot_change_blocking_thresholds` 直接钉在
//! 生产投影入口；本文件另外断言带/不带 prior_report 时 findings 集合逐条相同（prior 不过滤 finding）。

use redteam::config::EffectiveModel;
use redteam::job::manifest::JobKind;
use redteam::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage, FakeChatBackend};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

const SPEC_TOKEN: &str = "SPEC_TOKEN_TRIAGE_PRIOR";
const QUOTE: &str = "  return db.query('SELECT * FROM users WHERE id=' + id);";

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(dir).args(args).output().expect("git");
    assert!(out.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&out.stderr));
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

fn scanner_json() -> String {
    serde_json::json!({
        "suspicions": [{"file": "src/app.js", "line_start": 2, "line_end": 2, "hint": "sql injection",
                        "category": "L2", "confidence": 0.9, "quote": QUOTE}],
        "needs": [], "scanned_areas": []
    })
    .to_string()
}

fn reviewer_json() -> String {
    serde_json::json!({
        "findings": [{"severity": "low", "title": "non-parameterized query", "body": "concatenation",
                      "file": "src/app.js", "line_start": 2, "line_end": 2, "confidence": 0.35,
                      "recommendation": "parameterize", "quote": QUOTE}],
        "needs": []
    })
    .to_string()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    job: PathBuf,
}

/// `routing_override`：`None` = 不带 prior_report；`Some(json)` = 原样写入（schema 必须是 report.triage/1）。
fn make_job(prior: Option<serde_json::Value>, slow: bool) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@example.com"]);
    git(&repo, &["config", "user.name", "t"]);
    std::fs::write(repo.join("AGENTS.md"), format!("# Conventions\n- {SPEC_TOKEN}: use the shared db wrapper\n")).unwrap();
    std::fs::write(repo.join("src/app.js"), "function find(id) {\n  return 1;\n}\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::write(repo.join("src/app.js"), format!("function find(id) {{\n{QUOTE}\n}}\n")).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-qm", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);

    let job = tmp.path().join("job");
    for d in ["config", "input", "out"] {
        std::fs::create_dir_all(job.join(d)).unwrap();
    }
    std::fs::write(
        job.join("config/executor.redteam.json"),
        serde_json::json!({
            "settings": {
                "language": "en_US",
                "review": {
                    // 非默认阈值：prior_report 不得改这两个值。
                    "blockThreshold": "low", "confFloor": 0.31,
                    "roles": {
                        "scanner": {"models": [{"provider": "p1", "id": "s"}]},
                        "reviewer": {"models": [{"provider": "p1", "id": "r"}]}
                    }
                }
            },
            "providers": {"p1": {"api": "openai-completions", "baseUrl": "http://127.0.0.1:1", "apiKeyEnv": "P1_KEY",
                "models": [{"id": "s"}, {"id": "r"}]}}
        })
        .to_string(),
    )
    .unwrap();
    let diff = git(&repo, &["diff", &format!("{base}..{head}")]);
    std::fs::write(job.join("input/diff.patch"), format!("{diff}\n")).unwrap();
    std::fs::write(job.join("input/merge-base.sha"), format!("{base}\n")).unwrap();
    std::fs::write(job.join("input/dismissed.csv"), "fingerprint,actor,reason,created_at\n").unwrap();

    let decl = |p: &Path| {
        let bytes = std::fs::read(p).unwrap();
        serde_json::json!({"path": p.strip_prefix(&job).unwrap().to_string_lossy(), "sha256": sha256_hex(&bytes)})
    };
    let mut inputs = serde_json::json!({
        "diff_patch": decl(&job.join("input/diff.patch")),
        "merge_base": decl(&job.join("input/merge-base.sha")),
        "dismissed": {"path": "input/dismissed.csv", "sha256": sha256_hex(&std::fs::read(job.join("input/dismissed.csv")).unwrap()), "format": "hibot.audit.dismissed-csv/1"},
    });
    if let Some(mut report) = prior {
        if slow {
            report["summary"]["routing"]["wall_clock_s"] = serde_json::json!(1);
        }
        let p = job.join("input/prior-report.json");
        std::fs::write(&p, serde_json::to_vec(&report).unwrap()).unwrap();
        inputs["prior_report"] = serde_json::json!({
            "path": "input/prior-report.json",
            "sha256": sha256_hex(&std::fs::read(&p).unwrap()),
            "schema": "hibot.audit.report.triage/1",
        });
    }
    let manifest = serde_json::json!({
        "schema": "hibot.audit.job.review/1",
        "job_id": "J-prior",
        "kind": "review",
        "repo": {"id": 1, "full_name": "org/repo"},
        "pr": {"number": 7, "head_sha": head, "base_sha": base},
        "executor": {"name": "redteam", "version": "0.1.0"},
        "limits": {"wall_clock_s": 120},
        "paths": {"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"},
        "inputs": inputs,
    });
    std::fs::write(job.join("job.json"), serde_json::to_vec_pretty(&manifest).unwrap()).unwrap();
    Fixture { _tmp: tmp, job }
}

fn real_report() -> serde_json::Value {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/triage/real-cli18.json");
    serde_json::from_slice(&std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))).unwrap()
}

fn read_report(job: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(job.join("out/report.json")).unwrap()).unwrap()
}

fn fake_override<'a>(
    scan: Arc<FakeChatBackend>,
    review: Arc<FakeChatBackend>,
) -> Box<redteam::cli::BackendOverride<'a>> {
    Box::new(move |em: &EffectiveModel| -> Option<Box<dyn ChatBackend>> {
        match em.id.as_str() {
            "s" => Some(Box::new(scan.clone())),
            "r" => Some(Box::new(review.clone())),
            _ => None,
        }
    })
}

#[tokio::test]
async fn real_triage_report_drives_focus_and_specs() {
    let fx = make_job(Some(real_report()), false);
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json())));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json())));
    let over = fake_override(scan.clone(), review);
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0);
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "ok", "{r}");

    // focus 进了 scanner 请求。
    let asks = format!("{:?}", scan.messages_at(0));
    assert!(asks.contains("src/commands/content.rs"), "focus 必须进 scanner prompt: {asks}");
    // specs 的 doc 内容进了共享头（system）。
    let sys = match &scan.messages_at(0)[0] {
        ChatMessage::System(s) => s.clone(),
        other => panic!("首条须是 system: {other:?}"),
    };
    assert!(sys.contains(SPEC_TOKEN), "spec 内容必须注入: {sys}");

    let pr = &r["extensions"]["redteam"]["prior_routing"];
    assert_eq!(pr["focus"][0], "src/commands/content.rs");
    assert_eq!(pr["specs_used"], serde_json::json!(["AGENTS.md"]));
    assert_eq!(pr["specs_unreadable"], 0);
    assert_eq!(pr["wall_clock_s"], 900);
}

#[tokio::test]
async fn prior_report_does_not_change_the_finding_set() {
    let baseline = make_job(None, false);
    let with_prior = make_job(Some(real_report()), false);
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json())));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json())));
    let over = fake_override(scan.clone(), review.clone());
    let c1 = redteam::job::run::run_with(JobKind::Review, &baseline.job, Some(over.as_ref())).await;
    let over2 = fake_override(scan, review);
    let c2 = redteam::job::run::run_with(JobKind::Review, &with_prior.job, Some(over2.as_ref())).await;
    assert_eq!((c1, c2), (0, 0));
    let a = read_report(&baseline.job);
    let b = read_report(&with_prior.job);
    let fps = |r: &serde_json::Value| {
        r["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["fingerprint"].as_str().unwrap().to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(fps(&a), fps(&b), "prior_report 不得过滤/改写 findings（阻断阈值不变的可观测面）");
    assert_eq!(fps(&b).len(), 1);
}

#[tokio::test]
async fn routing_wall_clock_only_tightens_and_is_observable_as_partial() {
    let fx = make_job(Some(real_report()), true);
    // 1s 建议预算 + 2s backend → 预算先到，报告必须是 partial/wall_clock_exhausted。
    let scan = Arc::new(FakeChatBackend::repeating(turn(&scanner_json())).with_delay(std::time::Duration::from_secs(2)));
    let review = Arc::new(FakeChatBackend::repeating(turn(&reviewer_json())).with_delay(std::time::Duration::from_secs(2)));
    let over = fake_override(scan, review);
    let code = redteam::job::run::run_with(JobKind::Review, &fx.job, Some(over.as_ref())).await;
    assert_eq!(code, 0, "partial 报告已写出 → 0");
    let r = read_report(&fx.job);
    assert_eq!(r["status"], "partial", "{r}");
    assert_eq!(r["extensions"]["redteam"]["status_code"], "wall_clock_exhausted", "{r}");
    assert_eq!(r["extensions"]["redteam"]["prior_routing"]["wall_clock_s"], 1);
}
