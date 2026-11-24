//! job 模式入口：kind 分派、错误报告可写性、review 执行与终态落盘（1.5 / 1.7 / 1.8）。
//!
//! 退出码（P13，「写出报告即 0」的本地调试便利，hibot 不读）：
//! - `0`：`ok` / `partial` 报告已写出；
//! - `1`：`error` 报告已写出；
//! - `2`：job.json 定位不到 / 不可解析（**不写报告**，runner 按 no-report=error）。

use crate::cli::ReviewArgs;
use crate::config::{Config, OutputLang};
use crate::job::lifecycle::{self, JobDirCellSink};
use crate::job::manifest::{JobKind, JobManifest};
use crate::job::report;
use crate::job::status::{JobFailure, JobStatus};
use crate::job::{artifact, executor_config, io};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// 跑一个 job，返回进程退出码。
pub async fn run(cli_kind: JobKind, job_dir_arg: &Path) -> i32 {
    run_with(cli_kind, job_dir_arg, None).await
}

/// 同 [`run`]，但允许注入确定性 backend（集成测试 / 夹具；生产不传）。
pub async fn run_with(
    cli_kind: JobKind,
    job_dir_arg: &Path,
    over: Option<&crate::cli::BackendOverride<'_>>,
) -> i32 {
    seal_job_env();
    let job_dir = match io::canonical_job_dir(job_dir_arg) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {} ({})", e, job_dir_arg.display());
            return 2;
        }
    };
    if !io::manifest_path(&job_dir).is_file() {
        eprintln!("error: job.json not found in {}", job_dir.display());
        return 2;
    }
    // manifest 不可读/不可解析 → **不写报告**（1.8 / hibot B5）。
    let manifest = match JobManifest::load(&job_dir) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("error: {} ({})", e, job_dir.display());
            return 2;
        }
    };
    let started = std::time::Instant::now();
    let budget = Duration::from_secs(manifest.limits.wall_clock_s);
    // 信号在入口安装：准备阶段（manifest/artifact/config）也计入预算，且早到的 SIGTERM 不会被丢掉。
    let mut sigterm = install_sigterm();
    let out_dir = match io::out_dir(&job_dir, &manifest.paths) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {} (paths.out)", e);
            return 2;
        }
    };
    // 从这里起 manifest 可解析：任何失败都要写出合法 error 报告。
    let fallback_lang = OutputLang::default();

    // 先加载 executor config：后续 error 报告的 status_reason 才能跟随配置的语言（1.4）。
    let cfg = match executor_config::load(&manifest, &job_dir) {
        Ok(c) => c,
        Err(e) => return finish(&out_dir, report::error_report(&manifest, &fallback_lang, &e)),
    };
    // 1.4：job 模式语言 = 配置 > 缺省 en_US；**不读 env**。
    let lang = OutputLang::resolve_job(&cfg);
    // release 是 panic=abort：hook 在 abort 前尽力写出合法 error 报告（不能靠 Result::Err 假装兜底）。
    install_panic_report_hook(manifest.clone(), out_dir.clone(), lang.clone());

    if let Err(e) = manifest.validate() {
        return finish(&out_dir, report::error_report(&manifest, &lang, &e));
    }
    if let Err(e) = manifest.check_cli_kind(cli_kind) {
        return finish(&out_dir, report::error_report(&manifest, &lang, &e));
    }
    let artifacts = match artifact::load(&manifest, &job_dir) {
        Ok(a) => a,
        Err(e) => return finish(&out_dir, report::error_report(&manifest, &lang, &e)),
    };

    // D1：triage / verify 还没有分析实现——命令面存在，写 error + 临时码，禁止伪造产物。
    // verify 例外（协调者 2026-11-24 裁 A）：必须逐条覆盖输入指纹，故对每条输出 `inconclusive`
    // （未核验），整体仍 error——不伪造 resolved/unresolved。
    if manifest.kind != JobKind::Review {
        let r = match manifest.kind {
            JobKind::Verify => match verify_fingerprints(&artifacts) {
                Ok(fps) => report::kind_not_implemented_verify(&manifest, &lang, &fps),
                Err(e) => report::error_report(&manifest, &lang, &e),
            },
            _ => report::kind_not_implemented_report(&manifest, &lang),
        };
        return finish(&out_dir, r);
    }

    // 准备阶段已收到 SIGTERM 或预算已耗尽：直接写 partial（成果为空）。
    let remaining = budget.saturating_sub(started.elapsed());
    let early_signal = match sigterm.as_mut() {
        Some(s) => tokio::time::timeout(Duration::from_millis(0), s.recv()).await.is_ok(),
        None => false,
    };
    if remaining.is_zero() || early_signal {
        let sink = JobDirCellSink::new(out_dir.clone());
        let r = lifecycle::interrupted_report(&manifest, &lang, &cfg, &sink);
        return finish(&out_dir, r);
    }
    match run_review(&manifest, &job_dir, &out_dir, &cfg, &lang, &artifacts, remaining, &mut sigterm, over).await {
        Ok(r) => finish(&out_dir, r),
        Err(e) => finish(&out_dir, report::error_report(&manifest, &lang, &e)),
    }
}

async fn run_review(
    manifest: &JobManifest,
    job_dir: &Path,
    out_dir: &Path,
    cfg: &Config,
    lang: &OutputLang,
    artifacts: &artifact::Artifacts,
    remaining: Duration,
    sigterm: &mut Option<tokio::signal::unix::Signal>,
    over: Option<&crate::cli::BackendOverride<'_>>,
) -> Result<report::HibotReport, JobFailure> {
    let repo = io::resolve_repo(job_dir, &manifest.paths.repo)?;
    // 分析基准 = inputs.merge_base 的内容（diff 左端，可能 ≠ pr.base_sha；Q2）。
    let base = artifacts
        .text("merge_base")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| JobFailure::new("input_schema_mismatch", Some("merge_base".into())))?
        .to_string();
    // HEAD 必须与 manifest 一致（Appendix E head_sha_mismatch；完整 sha 或前缀均可）。
    let head = crate::evidence::git::head_sha_at(&repo)
        .map_err(|_| JobFailure::new("head_sha_mismatch", None))?;
    if !head.starts_with(&manifest.pr.head_sha) && !manifest.pr.head_sha.starts_with(&head) {
        return Err(JobFailure::new("head_sha_mismatch", None));
    }
    // 角色最小集（P2）：review = scanner + reviewer；refuter 可选。
    let roles = cfg.active_roles();
    if roles.scanners.is_empty() {
        return Err(JobFailure::new("missing_role", Some("scanner".into())));
    }
    if roles.reviewers.is_empty() {
        return Err(JobFailure::new("missing_role", Some("reviewer".into())));
    }
    let args = ReviewArgs {
        job_mode: true,
        base: Some(base),
        scope: "branch".into(),
        spec: vec![],
        focus: None,
        block_threshold: None,
        conf_floor: None,
        background: false,
        format: "json".into(),
        resume: None,
        dismissed: None,
        no_dismissed: true, // P7：五条台账路径全关
        incremental_from: None,
        tools: None,
        test_command: None,
        pack_only: false,
    };
    let registry = crate::cli::build_registry_with(&args, cfg, over).map_err(|e| {
        JobFailure::new("internal_panic", Some(e))
    })?;

    let sink = Arc::new(JobDirCellSink::new(out_dir.to_path_buf()));
    let run = Box::pin(crate::cli::run_review_dispatch(&args, &registry, cfg, &repo, Some(sink.as_ref())));
    let mut run = run;
    let outcome: Result<_, ()> = tokio::select! {
        r = &mut run => Ok(r),
        _ = tokio::time::sleep(remaining) => Err(()),
        _ = wait_sigterm(sigterm) => Err(()),
    };
    match outcome {
        Ok(Ok((internal, _code))) => Ok(report::review_report(manifest, &internal, cfg, lang)),
        Ok(Err(e)) => Err(JobFailure::new("internal_panic", Some(e.to_string()))),
        Err(()) => {
            // 停止派发：drop 掉运行 future（tokio 取消），再用已落地 cell 组 partial。
            drop(run);
            Ok(lifecycle::interrupted_report(manifest, lang, cfg, &sink))
        }
    }
}

/// verify 输入指纹集合（契约 §3.3 / Q6）：非法指纹 → `invalid_fingerprint_input`。
fn verify_fingerprints(artifacts: &artifact::Artifacts) -> Result<Vec<String>, JobFailure> {
    let text = artifacts
        .text("findings")
        .ok_or_else(|| JobFailure::new("input_schema_mismatch", Some("findings".into())))?;
    let v: serde_json::Value = serde_json::from_str(text)
        .map_err(|_| JobFailure::new("input_schema_mismatch", Some("findings".into())))?;
    let arr = v
        .as_array()
        .ok_or_else(|| JobFailure::new("input_schema_mismatch", Some("findings".into())))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let fp = item
            .get("fingerprint")
            .and_then(|x| x.as_str())
            .ok_or_else(|| JobFailure::new("invalid_fingerprint_input", None))?;
        if !crate::job::fingerprint::is_fingerprint(fp) {
            return Err(JobFailure::new("invalid_fingerprint_input", None));
        }
        out.push(fp.to_string());
    }
    Ok(out)
}

/// job 模式的环境隔离（P6 规则④ / 不变式⑦）：语言、密钥、并发全部来自 `paths.config`。
/// 进程级研发旋钮在 job 进程内清掉，杜绝任何下游代码（backend / 超时公式 / 诊断落盘）读 env。
fn seal_job_env() {
    // SAFETY: job 进程是单用途进程；启动即清，不与其它线程并发读写这些变量。
    unsafe {
        for k in [
            "REDTEAM_MAX_TOKENS", "REDTEAM_PER_TURN_SECS", "REDTEAM_DUMP_DIR", "REDTEAM_HOME",
            "LC_ALL", "LC_MESSAGES", "LANG",
        ] {
            std::env::remove_var(k);
        }
    }
}

/// 原子写报告并返回退出码（partial 也算已写出 → 0；error → 1）。
fn finish(out_dir: &Path, report: report::HibotReport) -> i32 {
    let path = out_dir.join("report.json");
    match io::atomic_write_json(&path, &report) {
        Ok(()) => {
            if report.status == JobStatus::Error { 1 } else { 0 }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn install_sigterm() -> Option<tokio::signal::unix::Signal> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        return signal(SignalKind::terminate()).ok();
    }
    #[allow(unreachable_code)]
    None
}

async fn wait_sigterm(sig: &mut Option<tokio::signal::unix::Signal>) {
    #[cfg(unix)]
    {
        if let Some(s) = sig {
            s.recv().await;
            return;
        }
    }
    std::future::pending::<()>().await
}

/// panic（release 下 `panic=abort`，hook 在 abort 前运行）→ 原子写一份合法 error 报告。
fn install_panic_report_hook(manifest: JobManifest, out_dir: std::path::PathBuf, lang: OutputLang) {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        if let Err(e) = write_panic_report(&manifest, &out_dir, &lang, &msg) {
            eprintln!("error: panic 报告写入失败: {e}");
        }
        prev(info);
    }));
}

/// hook 的可测主体：panic → 合法 error 报告（原子写）。
fn write_panic_report(manifest: &JobManifest, out_dir: &Path, lang: &OutputLang, msg: &str) -> Result<(), String> {
    let failure = JobFailure::new("internal_panic", Some(msg.to_string()));
    let r = report::error_report(manifest, lang, &failure);
    io::atomic_write_json(&out_dir.join("report.json"), &r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> JobManifest {
        JobManifest::parse(r#"{
          "schema":"hibot.audit.job.review/1","job_id":"J1","kind":"review",
          "repo":{"id":1,"full_name":"o/r"},
          "pr":{"number":7,"head_sha":"abcdef0","base_sha":"1234567"},
          "executor":{"name":"redteam","version":"0.1.0"},
          "limits":{"wall_clock_s":60},
          "paths":{"repo":"../repo","out":"out","config":"config/executor.redteam.json"},
          "inputs":{}
        }"#).unwrap()
    }

    #[test]
    fn panic_report_is_valid_error_with_sanitized_code() {
        let d = tempfile::tempdir().unwrap();
        write_panic_report(&manifest(), d.path(), &crate::config::Language::EnUs.output(), "boom\nstack").unwrap();
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(d.path().join("report.json")).unwrap()).unwrap();
        assert_eq!(v["status"], "error");
        assert_eq!(v["extensions"]["redteam"]["status_code"], "internal_panic", "机器码不拼诊断全文");
        assert!(v["status_reason"].as_str().unwrap().contains("boom"));
        assert_eq!(v["job_id"], "J1");
    }
}
