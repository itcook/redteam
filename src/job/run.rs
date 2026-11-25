//! job 模式入口：kind 分派、错误报告可写性、review 执行与终态落盘（1.5 / 1.7 / 1.8）。
//!
//! 退出码（P13，「写出报告即 0」的本地调试便利，hibot 不读）：
//! - `0`：`ok` / `partial` 报告已写出；
//! - `1`：`error` 报告已写出；
//! - `2`：job.json 定位不到 / 不可解析（**不写报告**，runner 按 no-report=error）。
//!
//! # 2.0 数据流（Phase 2）
//!
//! ```text
//! job.json ──manifest::load──► JobManifest ──validate──► kind / limits / paths
//! inputs.diff_patch ──sha256 已校验──► split_diff_by_file ──► FileChange[]
//! inputs.merge_base ──读内容──► base（diff 左端；**可能 ≠ pr.base_sha**，禁止断言相等）
//! paths.repo ──► root（只读检出；HEAD 必须 = pr.head_sha）
//!         └──► run_code_review(args, registry, cfg, changes, root, head, sink, carried)
//!              （**不经** run_review_dispatch：那条路会重新采集 git diff）
//!
//! 收口：findings ──► 逐字重锚（quote = 检出原文切片）──► fingerprint ──► dismissed 匹配
//!       ──► hibot 信封（extensions.redteam.*）+ status 判定
//! ```
//!
//! 硬规则：`diff_patch` 是输入真相源，git 只用于证据收集（风险路由 / 分组 / 装包）；
//! 两边都可得时只做一致性披露，不改行为。

use crate::cli::ReviewArgs;
use crate::config::{Config, OutputLang};
use crate::job::lifecycle::{self, JobDirCellSink};
use crate::job::manifest::{JobKind, JobManifest};
use crate::job::report;
use crate::job::state;
use crate::job::status::{JobFailure, JobStatus};
use crate::job::{artifact, executor_config, io};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// 跑一个 job，返回进程退出码。
pub async fn run(cli_kind: JobKind, job_dir_arg: &Path) -> i32 {
    // 只在真实 CLI 入口清环境：job 进程单用途，清掉研发旋钮无副作用；
    // 进程内测试并发调用 `run_with` 时不动全局 env（避免线程间 set/remove 竞争）。
    seal_job_env();
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
    let started = std::time::Instant::now();
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
    // Phase 5：心跳从“manifest 可解析”起手（首帧 preparing，覆盖 config/artifact 准备期）。
    // out_dir 已定；此后所有失败路径都走 finish → shutdown（补 running → finishing，不跳段）。
    let heartbeat = state::Heartbeat::start(&out_dir, &manifest.job_id);
    // 先加载 executor config：后续 error 报告的 status_reason 才能跟随配置的语言（1.4）。
    let cfg = match executor_config::load(&manifest, &job_dir) {
        Ok(c) => c,
        Err(e) => return finish(&out_dir, report::error_report(&manifest, &fallback_lang, &e), &heartbeat),
    };
    // 1.4：job 模式语言 = 配置 > 缺省 en_US；**不读 env**。
    let lang = OutputLang::resolve_job(&cfg);
    // release 是 panic=abort：hook 在 abort 前尽力写出合法 error 报告（不能靠 Result::Err 假装兜底）。
    install_panic_report_hook(manifest.clone(), out_dir.clone(), lang.clone());

    if let Err(e) = manifest.validate() {
        return finish(&out_dir, report::error_report(&manifest, &lang, &e), &heartbeat);
    }
    if let Err(e) = manifest.check_cli_kind(cli_kind) {
        return finish(&out_dir, report::error_report(&manifest, &lang, &e), &heartbeat);
    }
    let artifacts = match artifact::load(&manifest, &job_dir) {
        Ok(a) => a,
        Err(e) => return finish(&out_dir, report::error_report(&manifest, &lang, &e), &heartbeat),
    };

    // Phase 3：triage 有分析实现；verify 仍是 Phase 1 的 stub（协调者 2026-11-24 裁 A：
    // 必须逐条覆盖输入指纹，故对每条输出 `inconclusive`，整体仍 error——不伪造 resolved/unresolved）。
    match manifest.kind {
        JobKind::Triage => {
            let r = run_triage(
                &manifest, &job_dir, &out_dir, &cfg, &lang, &artifacts, started, &mut sigterm, &heartbeat, over,
            )
            .await;
            return finish(&out_dir, r, &heartbeat);
        }
        JobKind::Verify => {
            let r = match verify_fingerprints(&artifacts) {
                Ok(fps) => {
                    // Phase 5：分析未实现，但输入项数可知；progress.unit 固定 items。
                    heartbeat.set_progress_total("items", fps.len() as u64);
                    report::kind_not_implemented_verify(&manifest, &lang, &fps)
                }
                Err(e) => report::error_report(&manifest, &lang, &e),
            };
            heartbeat.set_phase(state::Phase::Running);
            return finish(&out_dir, r, &heartbeat);
        }
        JobKind::Review => {}
    }

    // 2.4：dismissed.csv 是 review 的必需输入——坏格式 → error（不静默当成空台账）。
    let dismissed = match artifacts.get("dismissed") {
        Some(a) => match crate::job::dismissed::parse(&a.bytes) {
            Ok(d) => d,
            Err(e) => return finish(&out_dir, report::error_report(&manifest, &lang, &e), &heartbeat),
        },
        None => {
            let e = JobFailure::new("input_missing", Some("dismissed".into()));
            return finish(&out_dir, report::error_report(&manifest, &lang, &e), &heartbeat);
        }
    };
    // 准备阶段已收到 SIGTERM 或预算已耗尽：直接写 partial（成果为空）。
    let deadline = match effective_deadline(started, manifest.limits.wall_clock_s, None) {
        Ok(d) => d,
        Err(e) => return finish(&out_dir, report::error_report(&manifest, &lang, &e), &heartbeat),
    };
    let early_signal = match sigterm.as_mut() {
        Some(s) => tokio::time::timeout(Duration::from_millis(0), s.recv()).await.is_ok(),
        None => false,
    };
    if deadline.saturating_duration_since(std::time::Instant::now()).is_zero() || early_signal {
        let sink = JobDirCellSink::with_heartbeat(out_dir.clone(), Arc::clone(&heartbeat));
        let refute_skipped = (cfg.settings.review.refuters.enabled && cfg.active_roles().refuters.is_empty())
            .then_some("no_refuter");
        let base_extras = report::ReviewExtras { refute_skipped, ..Default::default() };
        let r = lifecycle::interrupted_report(&manifest, &lang, &cfg, &sink, None, &dismissed, &base_extras);
        return finish(&out_dir, r, &heartbeat);
    }
    match run_review(
        &manifest, &job_dir, &out_dir, &cfg, &lang, &artifacts, &dismissed, started, &mut sigterm, &heartbeat, over,
    )
    .await
    {
        Ok(r) => finish(&out_dir, r, &heartbeat),
        Err(e) => finish(&out_dir, report::error_report(&manifest, &lang, &e), &heartbeat),
    }
}

/// 2.3：`prior_report.summary.routing.wall_clock_s` 是**建议**预算——只能收紧，不能放宽。
/// 2.3：绝对截止时刻 = min(manifest 硬上限, `prior_report` 建议预算)。
/// 用**绝对时刻**而不是「剩余秒数」：准备阶段（完整性检查 / registry 构造）耗时不会被重复计入。
fn effective_deadline(
    started: std::time::Instant,
    hard_cap_s: u64,
    suggested_s: Option<u64>,
) -> Result<std::time::Instant, JobFailure> {
    // 先算一次有效秒数，再一次 `checked_add`：两条分支都不做“溢出就换个数”的兜底。
    let secs = suggested_s.map_or(hard_cap_s, |s| hard_cap_s.min(s));
    started.checked_add(Duration::from_secs(secs)).ok_or_else(|| {
        JobFailure::new(
            "job_manifest_unsupported",
            Some("limits.wall_clock_s overflows Instant".into()),
        )
    })
}
/// 收口后的 counted 集合（stats 与 verdict 共用）：dismissed 不计，enforce 时窗外不计。
fn counted_findings(findings: &[crate::domain::Finding], window_enforced: bool) -> Vec<crate::domain::Finding> {
    findings
        .iter()
        .filter(|f| !f.previously_dismissed && !(window_enforced && f.outside_window))
        .cloned()
        .collect()
}

/// 2.2 投影：job 模式的 `ReviewArgs`（**唯一构造点**——生产与门槛测试共用同一入口）。
///
/// 四个布尔恒 false（`background` / `resume` / `incremental_from` / `pack_only`）、`scope=branch`、
/// `base=merge_base`；阈值字段保持 `None`（配置说了算，prior_report 不得改）。
fn review_args(base: &str, prior: &crate::job::prior::PriorRouting) -> ReviewArgs {
    ReviewArgs {
        job_mode: true,
        base: Some(base.to_string()),
        scope: "branch".into(),
        // 2.3：只调镜头（focus）与规范注入。
        spec: prior.specs.clone(),
        focus: prior.focus.clone(),
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
    }
}

// 参数各不同源（manifest / 路径 / 配置 / 语言 / 输入 / 预算 / 计时 / 信号 / 测试注入）：收拢 struct 不减少调用方负担。
#[allow(clippy::too_many_arguments)]
async fn run_review(
    manifest: &JobManifest,
    job_dir: &Path,
    out_dir: &Path,
    cfg: &Config,
    lang: &OutputLang,
    artifacts: &artifact::Artifacts,
    dismissed: &crate::job::dismissed::Dismissed,
    started: std::time::Instant,
    sigterm: &mut Option<tokio::signal::unix::Signal>,
    heartbeat: &Arc<state::Heartbeat>,
    over: Option<&crate::cli::BackendOverride<'_>>,
) -> Result<report::HibotReport, JobFailure> {
    let repo = io::resolve_repo(job_dir, &manifest.paths.repo)?;
    // 分析基准 = inputs.merge_base 的内容（diff 左端，可能 ≠ pr.base_sha；Q2）。
    // 形态先校验：契约写的是「40 位 sha（纯文本）」——短 sha / 大写不属于该形态。
    let base = artifacts
        .text("merge_base")
        .map(str::trim)
        .filter(|s| crate::job::repo::is_full_sha(s))
        .ok_or_else(|| JobFailure::new("input_schema_mismatch", Some("merge_base".into())))?
        .to_string();
    // 角色最小集（P2）：review = scanner + reviewer；refuter 可选。放在 prior 之前——
    // spec 段的共享预算与生产 `budget_and_spec` 同一处计算（`spec_budget_bytes`）。
    let roles = cfg.active_roles();
    if roles.scanners.is_empty() {
        return Err(JobFailure::new("missing_role", Some("scanner".into())));
    }
    if roles.reviewers.is_empty() {
        return Err(JobFailure::new("missing_role", Some("reviewer".into())));
    }
    // 2.3 prior_report（可选）：**只**读 summary.routing。历史 findings / 计数 / tier 一律不消费。
    let prior = match artifacts.get("prior_report") {
        Some(a) => {
            let text = a
                .text()
                .ok_or_else(|| JobFailure::new("input_schema_mismatch", Some("prior_report".into())))?;
            crate::job::prior::parse(text, &repo, crate::cli::spec_budget_bytes(&roles))?
        }
        None => crate::job::prior::PriorRouting::default(),
    };
    // 建议预算只能收紧，不能突破 manifest 硬上限；用**绝对截止时刻**，准备阶段耗时只计一次。
    let deadline = effective_deadline(started, manifest.limits.wall_clock_s, prior.wall_clock_s)?;
    // 2.6 完整性：非 shallow、HEAD == pr.head_sha（解析成完整 commit 再比）、base/merge_base
    // 对象可得、merge-base 可算。缺必需对象 → error（不是披露后仍报 ok）。
    let head = crate::job::repo::ensure_complete(&repo, &manifest.pr, &base)?;
    // 2.1 输入真相源 = inputs.diff_patch（sha256 已由 artifact 层校验）；git 只做证据收集。
    let patch = artifacts
        .text("diff_patch")
        .ok_or_else(|| JobFailure::new("input_schema_mismatch", Some("diff_patch".into())))?;
    let changes = crate::job::projection::from_patch(patch)?;
    let input_consistency = crate::job::projection::consistency(&repo, &base, &changes);
    // 2.3：specs 声明了但读不到的数目 → `partial` + `routing_specs_unreadable:<n>`（点名，不静默）。
    let mut extra_partial = Vec::new();
    if prior.specs_unreadable > 0 {
        extra_partial.push(JobFailure::new("routing_specs_unreadable", Some(prior.specs_unreadable.to_string())));
    }
    // P2：job 模式 + 验真开启 + 未声明 refuter → 跳过验真；报告必须显式披露。
    let refute_skipped = (cfg.settings.review.refuters.enabled && roles.refuters.is_empty()).then_some("no_refuter");
    // 正常与中断两条收口路径共享同一份运行事实（否则 SIGTERM 报告会丢 input_consistency / prior_routing）。
    let base_extras = report::ReviewExtras {
        input_consistency: Some(input_consistency.clone()),
        prior_routing: (!prior.disclosure.is_null()).then(|| prior.disclosure.clone()),
        refute_skipped,
        extra_partial: extra_partial.clone(),
        ..Default::default()
    };
    let args = review_args(&base, &prior);
    // backend 构造失败来自 executor config（baseUrl/kind/密钥形态），是**配置问题**而非内部错误；
    // 机器码用 config_invalid（无 pointer 时只给基础码），原始报错进 status_reason。
    let registry = crate::cli::build_registry_with(&args, cfg, over)
        .map_err(|e| JobFailure::with_reason("config_invalid", None, e))?;
    // Phase 5：registry 建好即绑定心跳（preparing 阶段也如实披露 provider 帽）。
    heartbeat.attach_llm(registry.gate_handles());
    // 预算已耗尽（含 registry 构造耗时）→ 不进模型派发，直接落 partial。
    if deadline.saturating_duration_since(std::time::Instant::now()).is_zero() {
        let text = JobFailure::new("wall_clock_exhausted", None).status_text(lang);
        let usage = report::UsageOut {
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            wall_clock_ms: started.elapsed().as_millis() as u64,
        };
        return Ok(report::partial_report(manifest, lang, &text, vec![], usage, cfg, 0, &base_extras));
    }
    // 进入模型派发：job 视角从 preparing 推进到 running（此后不再回退）。
    heartbeat.set_phase(state::Phase::Running);
    let sink = Arc::new(JobDirCellSink::with_heartbeat(out_dir.to_path_buf(), Arc::clone(heartbeat)));
    // 2.1 / 2.2：变更集来自已校验的 `inputs.diff_patch`，**不**再走 `run_review_dispatch`（它会重新 `git diff`）。
    // `head` 是完整性检查解析出的完整 commit（逐字透传给风险路由的 stamp）。
    let run = Box::pin(crate::cli::run_code_review(
        &args,
        &registry,
        cfg,
        changes,
        &repo,
        Some(head),
        Some(sink.as_ref()),
        vec![],
    ));
    let mut run = run;
    let outcome: Result<_, ()> = tokio::select! {
        r = &mut run => Ok(r),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(()),
        _ = wait_sigterm(sigterm) => Err(()),
    };
    match outcome {
        Ok(Ok((mut internal, _code))) => {
            // 分析结束、进入收尾（重锚/去重/报告投影）：finishing 覆盖这段，报告写入期间心跳继续。
            heartbeat.set_phase(state::Phase::Finishing);
            // 2.5：重锚 → 用检出原文修正 quote/行号；失败即丢弃并计数（不静默）。
            // 指纹在 `map_finding` 里按修正后的 quote 生成；dismissed 匹配接在指纹之后（2.4）。
            let verified = crate::job::quote::verify(std::mem::take(&mut internal.findings), &repo);
            let dropped = verified.dropped + crate::job::quote::upstream_dropped(&internal.cells);
            internal.findings = verified.findings;
            // 2.4：指纹 → dismissed（顺序固定：重锚/修正 quote 之后）。命中只标审计、不写 refuted。
            let ledger = crate::job::dismissed::apply(&mut internal.findings, dismissed);
            // 统计与 verdict 共用同一份 counted 集合（与 overlay_deep 的口径一致）：
            // - 被丢弃的 finding 已不在数组里；
            // - dismissed 命中的不计任何计数（协调者 Q-A 第 2 条）；
            // - `finding_window_enforced` 时窗外的也不计。
            let counted = counted_findings(&internal.findings, internal.run.finding_window_enforced);
            internal.stats = crate::domain::Stats::from_findings(&counted);
            internal.run.doc_findings = counted
                .iter()
                .filter(|f| crate::risk::signals::is_doc_path(&f.file))
                .count() as u32;
            // 内部 verdict 也要认这个事实（报告不携带 verdict，但内部 blocking 口径必须一致）。
            // 只重算 Approve/Reject 这两种「纯由 findings 决定」的结论：
            // - Incomplete 来自 coverage/budget（扫描/深挖不完整）；
            // - ScanOnly 来自 scan-only 形态；
            // 两者都不能被一次重算抹掉。重算沿用 overlay_deep 的 counted 口径（窗外 finding 在 enforce 下不计）。
            if matches!(internal.verdict, crate::domain::Verdict::Approve | crate::domain::Verdict::Reject) {
                let (block, floor) = crate::cli::verdict_thresholds(&args, cfg);
                internal.verdict = crate::verdict::compute_verdict(&counted, block, floor);
            }
            if dropped > 0 {
                extra_partial.push(JobFailure::new("quotes_dropped", Some(dropped.to_string())));
            }
            Ok(report::review_report(
                manifest,
                &internal,
                cfg,
                lang,
                &report::ReviewExtras {
                    dropped_unverified_quotes: dropped,
                    ledger,
                    extra_partial,
                    ..base_extras
                },
            ))
        }
        Ok(Err(e)) => {
            heartbeat.set_phase(state::Phase::Finishing);
            Err(JobFailure::new("internal_panic", Some(e.to_string())))
        }
        Err(()) => {
            // 停止派发：drop 掉运行 future（tokio 取消），再用已落地 cell 组 partial。
            drop(run);
            heartbeat.set_phase(state::Phase::Finishing);
            Ok(lifecycle::interrupted_report(manifest, lang, cfg, &sink, Some(&repo), dismissed, &base_extras))
        }
    }
}

/// 3.1–3.7：triage 作业。输入校验与 review 同源（`diff_patch` 是真相源、repo 只读、
/// merge_base 是 diff 左端），失败仍写合法报告；墙钟/SIGTERM 用已完成的轴收敛成 `partial`。
#[allow(clippy::too_many_arguments)]
async fn run_triage(
    manifest: &JobManifest,
    job_dir: &Path,
    _out_dir: &Path,
    cfg: &Config,
    lang: &OutputLang,
    artifacts: &artifact::Artifacts,
    started: std::time::Instant,
    sigterm: &mut Option<tokio::signal::unix::Signal>,
    heartbeat: &state::Heartbeat,
    over: Option<&crate::cli::BackendOverride<'_>>,
) -> report::HibotReport {
    let repo = match io::resolve_repo(job_dir, &manifest.paths.repo) {
        Ok(r) => r,
        Err(e) => return report::error_report(manifest, lang, &e),
    };
    let base = match artifacts
        .text("merge_base")
        .map(str::trim)
        .filter(|s| crate::job::repo::is_full_sha(s))
    {
        Some(s) => s.to_string(),
        None => return report::error_report(manifest, lang, &JobFailure::new("input_schema_mismatch", Some("merge_base".into()))),
    };
    let pr = match artifacts.text("pr_context").map(str::to_string).map(|t| crate::triage::input::parse(&t)) {
        Some(Ok(pr)) => pr,
        Some(Err(e)) => return report::error_report(manifest, lang, &e),
        None => return report::error_report(manifest, lang, &JobFailure::new("input_schema_mismatch", Some("pr_context".into()))),
    };
    let patch = match artifacts.text("diff_patch") {
        Some(t) => t,
        None => return report::error_report(manifest, lang, &JobFailure::new("input_schema_mismatch", Some("diff_patch".into()))),
    };
    let changes = match crate::job::projection::from_patch(patch) {
        Ok(c) => c,
        Err(e) => return report::error_report(manifest, lang, &e),
    };
    // 2.6 同源完整性：非 shallow、head == pr.head_sha、base/merge_base 对象可得、merge-base 可算。
    let _head = match crate::job::repo::ensure_complete(&repo, &manifest.pr, &base) {
        Ok(h) => h,
        Err(e) => return report::error_report(manifest, lang, &e),
    };
    let routes = crate::triage::triage_routes(cfg);
    if routes.is_empty() {
        return report::error_report(manifest, lang, &JobFailure::new("missing_role", Some("triage".into())));
    }
    let route_keys: Vec<String> = routes.iter().map(|(_, k)| k.clone()).collect();
    let route_name = routes[0].0.name.clone();
    // backend 构造失败 = 配置问题（与 review 同一口径）。
    let registry = match crate::cli::build_registry_for_triage(&triage_args(&base), cfg, over) {
        Ok(r) => r,
        Err(e) => return report::error_report(manifest, lang, &JobFailure::with_reason("config_invalid", None, e)),
    };
    // Phase 5：triage 与 review 同用一张 provider 门表（`llm` 段）；阶段 preparing → running → finishing。
    heartbeat.attach_llm(registry.gate_handles());
    let deadline = match effective_deadline(started, manifest.limits.wall_clock_s, None) {
        Ok(d) => d,
        Err(e) => return report::error_report(manifest, lang, &e),
    };
    // 墙钟优化在 `triage::run` 内做：I0 串行预算 + 波次预算（含等门）。
    let req = crate::triage::TriageRequest {
        repo: &repo,
        pr: &pr,
        changes: &changes,
        lang,
        manifest_cap_s: manifest.limits.wall_clock_s,
        deadline,
        route_keys,
        route_name,
    };
    let progress = std::sync::Arc::new(std::sync::Mutex::new(crate::triage::Progress::default()));
    // Phase 5：三 kind 都走心跳；进入模型派发即 running（triage 的 manifest 没有 item 清单，不伪造 progress）。
    heartbeat.set_phase(state::Phase::Running);
    let mut fut = Box::pin(crate::triage::run(&registry, &req, progress.clone()));
    let outcome: Result<_, ()> = tokio::select! {
        out = &mut fut => Ok(out),
        _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => Err(()),
        _ = wait_sigterm(sigterm) => Err(()),
    };
    match outcome {
        Ok(out) => {
            let mut r = report::triage_report(manifest, lang, cfg, &out, &crate::triage::template_hash());
            r.usage.wall_clock_ms = started.elapsed().as_millis() as u64;
            r
        }
        Err(()) => {
            // 停止派发：drop 掉运行 future（tokio 取消），再用已完成轴收敛成 partial。
            drop(fut);
            let snapshot = progress.lock().unwrap();
            let mut out = crate::triage::snapshot_finalize(&req, &snapshot, false);
            out.reasons.push(JobFailure::new("wall_clock_exhausted", None));
            let mut r = report::triage_report(manifest, lang, cfg, &out, &crate::triage::template_hash());
            r.usage.wall_clock_ms = started.elapsed().as_millis() as u64;
            r
        }
    }
}

/// triage 的 `ReviewArgs`：job 模式、`base=merge_base`、无业务 flag（P1/P13）。
fn triage_args(base: &str) -> ReviewArgs {
    ReviewArgs {
        base: Some(base.to_string()),
        scope: "branch".into(),
        spec: Vec::new(),
        focus: None,
        block_threshold: None,
        conf_floor: None,
        background: false,
        format: "json".into(),
        resume: None,
        dismissed: None,
        no_dismissed: true,
        incremental_from: None,
        tools: None,
        test_command: None,
        pack_only: false,
        job_mode: true,
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
/// 报告写入期间心跳继续（`finishing` 覆盖整个收尾段）；报告落盘（或写失败）后再停写。
fn finish(out_dir: &Path, report: report::HibotReport, heartbeat: &state::Heartbeat) -> i32 {
    // 所有出口（含配置/输入错误、triage/verify、准备期超时）都在写报告前进入 finishing；
    // 正常 review 已在收尾链起点提前进入（幂等）。
    heartbeat.set_phase(state::Phase::Finishing);
    let path = out_dir.join("report.json");
    let code = match io::atomic_write_json(&path, &report) {
        Ok(()) => {
            if report.status == JobStatus::Error { 1 } else { 0 }
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    };
    // 报告已写（或写失败）：停写并留下最后一帧 finishing。
    heartbeat.shutdown();
    code
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

    /// 2.3：建议预算只能收紧，不能放宽；截止时刻是绝对的（准备阶段耗时不重复计入）。
    #[test]
    fn prior_budget_can_only_tighten() {
        let started = std::time::Instant::now();
        // 建议比硬上限大 → 不放宽。
        assert_eq!(
            effective_deadline(started, 60, Some(900)).unwrap(),
            started + Duration::from_secs(60)
        );
        // 建议更小 → 取建议。
        assert_eq!(
            effective_deadline(started, 60, Some(30)).unwrap(),
            started + Duration::from_secs(30)
        );
        // 建议与硬上限同时收紧：取更小的那个。
        assert_eq!(
            effective_deadline(started, 15, Some(900)).unwrap(),
            started + Duration::from_secs(15)
        );
        // 无建议 → 硬上限。
        assert_eq!(
            effective_deadline(started, 60, None).unwrap(),
            started + Duration::from_secs(60)
        );
    }

    /// 极大 `wall_clock_s` 不得 panic：走 manifest 错误路径。
    #[test]
    fn huge_wall_clock_overflows_into_a_manifest_error() {
        let e = effective_deadline(std::time::Instant::now(), u64::MAX, None).unwrap_err();
        assert_eq!(e.code, "job_manifest_unsupported");
    }

    /// 截止时刻已过 → 预算耗尽（调用方据此禁止进入模型派发）。
    #[test]
    fn past_deadline_means_exhausted() {
        let now = std::time::Instant::now();
        let past = now - Duration::from_secs(1);
        assert!(past.saturating_duration_since(now).is_zero());
        let future = now + Duration::from_secs(1);
        assert!(!future.saturating_duration_since(now).is_zero());
    }


    /// 2.5/2.4：counted 集合的口径——窗内 dismissed、窗外 enforced、普通有效项三者混合。
    #[test]
    fn counted_set_excludes_dismissed_and_enforced_outside_window() {
        use crate::domain::{Finding, Origin, Severity};
        let mk = |sev, dismissed: bool, outside: bool| Finding {
            id: "x".into(), severity: sev, title: "t".into(), body: "b".into(), file: "src/a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(), models: vec![],
            agreement: 1, suspicion_agreement: 0, hard_constraint: false, cluster_id: None,
            quote: "q".into(), refute: None, previously_dismissed: dismissed,
            origin: Origin::New, outside_window: outside,
        };
        let fs = vec![
            mk(Severity::Critical, true, false),  // 窗内但被驳回
            mk(Severity::High, false, true),      // 窗外高危（enforce 下不计）
            mk(Severity::Medium, false, false),   // 普通有效项
        ];
        assert_eq!(counted_findings(&fs, true).len(), 1);
        assert_eq!(counted_findings(&fs, false).len(), 2, "未 enforce 时窗外项照常计");
        let counted = counted_findings(&fs, true);
        assert_eq!(crate::domain::Stats::from_findings(&counted).total, 1);
        let v = crate::verdict::compute_verdict(&counted, Severity::High, 0.5);
        assert_ne!(v, crate::domain::Verdict::Reject, "驳回/窗外项不得把结论推到 reject: {v:?}");
    }

    /// 2.3 出口验收：走**生产投影入口**，prior_report 前后生效的阻断阈值逐字节相同（非默认配置）。
    #[test]
    fn prior_routing_cannot_change_blocking_thresholds() {
        let cfg: Config = serde_json::from_str(
            r#"{"settings":{"review":{"blockThreshold":"low","confFloor":0.31}},"providers":{}}"#,
        )
        .unwrap();
        let base = "0".repeat(40);
        let before = crate::cli::verdict_thresholds(
            &review_args(&base, &crate::job::prior::PriorRouting::default()),
            &cfg,
        );
        assert_eq!(before, (crate::domain::Severity::Low, 0.31), "基线是非默认阈值");

        let mut prior = crate::job::prior::PriorRouting::default();
        prior.focus = Some("auth".into());
        prior.specs = vec!["/abs/spec.md".into()];
        let args = review_args(&base, &prior);
        assert_eq!(args.spec, vec!["/abs/spec.md".to_string()], "投影确实生效");
        assert!(args.focus.is_some());
        assert_eq!(crate::cli::verdict_thresholds(&args, &cfg), before);
    }

    /// 2.3：截断后的截止时刻不会因准备工作耗时被“补回去”（与旧 remaining 语义的差异）。
    #[test]
    fn deadline_is_absolute_not_recomputed_from_elapsed() {
        let started = std::time::Instant::now();
        let d = effective_deadline(started, 600, Some(10)).unwrap();
        assert_eq!(d, started + Duration::from_secs(10));
        // 同一 deadline 在 3 秒后仍是起点 + 10s，而不是「剩余 7s + 当前」。
        assert_eq!(d.saturating_duration_since(started + Duration::from_secs(3)), Duration::from_secs(7));
    }

}
