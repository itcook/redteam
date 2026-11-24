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

    if let Err(e) = manifest.validate() {
        return finish(&out_dir, report::error_report(&manifest, &lang, &e));
    }
    if let Err(e) = manifest.check_cli_kind(cli_kind) {
        return finish(&out_dir, report::error_report(&manifest, &lang, &e));
    }
    if let Err(e) = artifact::load(&manifest, &job_dir) {
        return finish(&out_dir, report::error_report(&manifest, &lang, &e));
    }

    // D1：triage / verify 还没有分析实现——命令面存在，写 error + 临时码，禁止伪造产物。
    if manifest.kind != JobKind::Review {
        let r = report::kind_not_implemented_report(&manifest, &lang);
        return finish(&out_dir, r);
    }

    match run_review(&manifest, &job_dir, &out_dir, &cfg, &lang, over).await {
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
    over: Option<&crate::cli::BackendOverride<'_>>,
) -> Result<report::HibotReport, JobFailure> {
    let repo = io::resolve_repo(job_dir, &manifest.paths.repo)?;
    // 分析基准 = inputs.merge_base 的内容（diff 左端，可能 ≠ pr.base_sha；Q2）。
    let artifacts = artifact::load(manifest, job_dir)?;
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
    let budget = Duration::from_secs(manifest.limits.wall_clock_s);
    let run = Box::pin(crate::cli::run_review_dispatch(&args, &registry, cfg, &repo, Some(sink.as_ref())));
    let mut run = run;
    let outcome: Result<_, ()> = tokio::select! {
        r = &mut run => Ok(r),
        _ = tokio::time::sleep(budget) => Err(()),
        _ = wait_sigterm() => Err(()),
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

async fn wait_sigterm() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
            return;
        }
    }
    std::future::pending::<()>().await
}
