use crate::bundle::{group, select_files};
use crate::config::Config;
use crate::consolidate::{apply_clusters, correlate, dedup, dedup_doc};
use crate::domain::{Mode, ReviewReport, RunMeta, Severity, Stats, Usage, Verdict};
use crate::evidence::{collect, detect_mode, EvidenceOpts, Scope};
use crate::evidence::git::FileChange;
use crate::lens::select;
use crate::orchestrate::{run_code_fanout, run_doc_fanout, CellSink, OrchestrateOpts, PlannedModel};
use crate::output::Format;
use crate::positioning::{anchor, anchor_doc};
use crate::provider::ReviewProvider;
use crate::verdict::{clamp_compliance, compute_verdict};
use std::path::Path;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct ReviewArgs {
    pub mode: Option<String>,
    pub base: Option<String>,
    pub scope: String,
    pub spec: Vec<String>,
    pub focus: Option<String>,
    pub lenses: Option<String>,
    pub models: Option<String>,
    pub models_all: bool,
    pub no_correlate: bool,
    pub block_threshold: Option<String>,
    pub conf_floor: Option<f32>,
    // `--wait` 是默认前台同步路径（与 `--background` 对称）。
    // main 层处理该 CLI flag；run_review 不持有它——前台路径就是默认路径，无需单独字段。
    pub background: bool,
    pub format: String,
    pub targets: Vec<String>,
}

fn parse_sev(s: &str) -> Severity {
    match s {
        "low" => Severity::Low,
        "high" => Severity::High,
        "critical" => Severity::Critical,
        _ => Severity::Medium,
    }
}

pub fn parse_format(s: &str) -> Format {
    if s == "md" {
        Format::Md
    } else {
        Format::Json
    }
}

/// 退出码映射（统一 run_review/run_code_review）：reject→10、approve+对照集不完整→2、干净 approve→0。
pub fn exit_code(verdict: Verdict, failed_models_nonempty: bool) -> i32 {
    match (verdict, failed_models_nonempty) {
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2,
        (Verdict::Approve, false) => 0,
        (Verdict::Incomplete, _) => 2,
    }
}

/// 统一 model 选取（S3-a）。返回 (planned_models, replicate)。
/// 显式列表（`--models-all` / `--models` / 非空 `default_models` 任一）→ replicate=true。
/// 三者全空 → 所有已配置 provider 按 priority 降序候补，replicate=false。
/// `--models-all` 优先于 `--models`。
pub fn resolve_models(
    models_flag: Option<&str>,
    models_all: bool,
    cfg: &Config,
) -> (Vec<PlannedModel>, bool) {
    let all_by_priority = || -> Vec<String> {
        let mut v: Vec<_> = cfg.providers.iter().collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.1.priority));
        v.into_iter().map(|(k, _)| k.clone()).collect()
    };
    let (names, replicate): (Vec<String>, bool) = if models_all {
        (all_by_priority(), true)
    } else if let Some(s) = models_flag {
        (
            s.split(',').map(|x| x.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            true,
        )
    } else if !cfg.review.default_models.is_empty() {
        (cfg.review.default_models.clone(), true)
    } else {
        (all_by_priority(), false)
    };
    let models = names
        .iter()
        .map(|n| PlannedModel {
            name: n.clone(),
            priority: cfg.providers.get(n).map(|p| p.priority).unwrap_or(0),
        })
        .collect();
    (models, replicate)
}

/// 整条评审流水线：证据 → lens → fan-out → dedup → clamp → correlate → apply_clusters
/// → verdict → 组装 report → 退出码。
///
/// 退出码：0=approve、10=reject、2=对照集不完整（failed_models 非空）。
/// `--background` 在 Plan 1 未实现（留 Plan 2），此处直接报错。
/// 默认前台同步路径无需 `wait` 字段——`run_review` 本身就是同步等待模式。
pub async fn run_review(
    args: ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    if args.background {
        anyhow::bail!("--background is not yet implemented (planned for Plan 2)");
    }
    let t0 = std::time::Instant::now();

    // 1. mode + 证据
    let mode = args.mode.as_deref().map(|m| match m {
        "code" => Mode::Code,
        _ => Mode::Doc,
    });
    let scope = if args.scope == "branch" {
        Scope::Branch
    } else {
        Scope::WorkingTree
    };
    let eopts = EvidenceOpts {
        mode,
        base: args.base.clone(),
        scope,
        spec: args.spec.clone(),
        targets: args.targets.clone(),
    };
    let input = collect(&eopts)?;

    // 2. lens
    let lens_ids: Vec<String> = args
        .lenses
        .as_deref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| match input.mode {
            Mode::Code => cfg.review.lenses.code.clone(),
            Mode::Doc => cfg.review.lenses.doc.clone(),
        });
    let lenses = select(input.mode, &lens_ids)?;

    // 3. 模型集 + replicate 判定（统一 resolve_models）
    let (models, replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let oopts = OrchestrateOpts {
        models,
        replicate,
        no_fallback: false,
        max_concurrency: cfg.review.max_concurrency,
        retries: 2,
    };

    // 4. fan-out（doc：全 D rule 一包 × model 工具循环）
    let focus = args.focus.clone().unwrap_or_default();
    // job 上下文时由 run_job 传 JobCellSink；直调路径（无 job）传 None，行为不变。
    let raw = run_doc_fanout(provider, &input, &lenses, &focus, &oopts, sink).await;

    // 5. positioning（doc anchor：空引用=遗漏保留、编造丢弃；按受审文档内容锚定）
    let docs = crate::evidence::doc::collect_doc_files(&args.targets)?;
    let anchored = anchor_doc(raw.findings, &docs);

    // 6. consolidate：dedup_doc（非行号相似度）→ clamp → correlate(桩) → verify(三层，doc refute)
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
    let mut findings = dedup_doc(anchored);
    clamp_compliance(&mut findings);
    let correlate_on = cfg.review.correlate && !args.no_correlate;
    let lead_model = model_names.first().cloned().unwrap_or_default();
    let clusters = correlate(provider, &lead_model, &findings, correlate_on).await;
    apply_clusters(&mut findings, &clusters);
    let findings = crate::consolidate::verify_findings(
        findings, provider, &model_names, block, cfg.review.max_concurrency, Mode::Doc,
    ).await;

    // 7. verdict
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let verdict = compute_verdict(&findings, block, floor);

    // 7. 组装报告
    let stats = Stats::from_findings(&findings);
    let report = ReviewReport {
        schema_version: "1".into(),
        mode: input.mode,
        verdict,
        stats,
        findings,
        clusters,
        next_steps: vec![],
        run: RunMeta {
            job_id: String::new(),
            models: model_names,
            lenses: lens_ids,
            duration_ms: t0.elapsed().as_millis() as u64,
            usage: Usage {
                input_tokens: raw.usage.0,
                output_tokens: raw.usage.1,
            },
            warnings: raw.warnings,
            failed_models: raw.failed_models,
        },
    };

    // 8. 退出码：reject 优先 → 10；否则 approve 但对照集不完整 → 2；干净 approve → 0。
    let code = exit_code(verdict, !report.run.failed_models.is_empty());
    Ok((report, code))
}

/// code 专属流水线（S2-2b）：bundle × model 工具循环 + positioning 尾段。
/// 退出码同 run_review：0=approve、10=reject、2=对照集不完整。
pub async fn run_code_review(
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    changes: Vec<FileChange>,
    root: &Path,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    if args.background {
        anyhow::bail!("--background is not yet implemented (planned for Plan 2)");
    }
    let t0 = std::time::Instant::now();

    // 1. 确定性筛选 + 分包
    let bundles = group(select_files(changes), Some(root));

    // 2. rules = code lenses（lens → 注入 rule）
    let lens_ids: Vec<String> = args
        .lenses
        .as_deref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| cfg.review.lenses.code.clone());
    let rules = select(Mode::Code, &lens_ids)?;

    // 3. 模型集 + replicate 判定（统一 resolve_models）
    let (models, replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let oopts = OrchestrateOpts {
        models,
        replicate,
        no_fallback: false,
        max_concurrency: cfg.review.max_concurrency,
        retries: 2,
    };

    // 4. fan-out（bundle × model）
    let focus = args.focus.clone().unwrap_or_default();
    let target_label = if args.scope == "branch" {
        format!("branch {}...HEAD", args.base.as_deref().unwrap_or("?"))
    } else {
        "working tree".to_string()
    };
    // job 上下文时由 run_job 传 JobCellSink；直调路径（无 job）传 None，行为不变。
    let raw = run_code_fanout(provider, &target_label, &bundles, &rules, &focus, &oopts, sink).await;

    // 5. positioning（dedup 之前：准行号反哺 line-overlap 去重）
    let anchored = anchor(raw.findings, root);

    // 6. consolidate：dedup → clamp → verify（三层 targeted-verify，孤证定向 refute）
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
    let mut findings = dedup(anchored);
    clamp_compliance(&mut findings);
    let findings = crate::consolidate::verify_findings(
        findings, provider, &model_names, block, cfg.review.max_concurrency, Mode::Code,
    ).await;

    // 7. verdict
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let verdict = compute_verdict(&findings, block, floor);

    // 8. 组装报告
    let stats = Stats::from_findings(&findings);
    let report = ReviewReport {
        schema_version: "1".into(),
        mode: Mode::Code,
        verdict,
        stats,
        findings,
        clusters: vec![],
        next_steps: vec![],
        run: RunMeta {
            job_id: String::new(),
            models: model_names,
            lenses: lens_ids,
            duration_ms: t0.elapsed().as_millis() as u64,
            usage: Usage {
                input_tokens: raw.usage.0,
                output_tokens: raw.usage.1,
            },
            warnings: raw.warnings,
            failed_models: raw.failed_models,
        },
    };

    let code = exit_code(verdict, !report.run.failed_models.is_empty());
    Ok((report, code))
}

/// 统一 mode 探测 + 评审分发（前台 main 与后台 worker 共用）。
/// code：git changed_files → run_code_review；doc：run_review。
pub async fn run_review_dispatch(
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    cwd: &Path,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    // run_review_dispatch 永远同步执行——--background 分叉发生在 main.rs，先于此处；
    // 故两分支都强制 background=false（否则 run_code_review/run_review 会 bail）。
    let mut sync_args = args.clone();
    sync_args.background = false;
    let scope_enum = if sync_args.scope == "branch" { Scope::Branch } else { Scope::WorkingTree };
    let mode_opt = sync_args.mode.as_deref().map(|m| if m == "code" { Mode::Code } else { Mode::Doc });
    let eopts = EvidenceOpts {
        mode: mode_opt,
        base: sync_args.base.clone(),
        scope: scope_enum,
        spec: sync_args.spec.clone(),
        targets: sync_args.targets.clone(),
    };
    let detected = detect_mode(&eopts)?;
    match detected {
        Mode::Code => {
            let changes = crate::evidence::git::changed_files(sync_args.base.as_deref(), scope_enum == Scope::Branch)?;
            run_code_review(&sync_args, provider, cfg, changes, cwd, sink).await
        }
        Mode::Doc => run_review(sync_args, provider, cfg, sink).await,
    }
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// R2.1 job checkpoint sink：扇出前落 manifest（+刷 progress.total），每 cell 完成落 partial
/// （+刷 last_active/progress.done）。cell 完成是低频事件——ponytail：每次操作直接
/// read_state+write_state，不额外加锁/缓存。
struct JobCellSink {
    ws: std::path::PathBuf,
    job_id: String,
}

impl CellSink for JobCellSink {
    fn manifest(&self, inputs: &[crate::state::store::CellInput]) {
        let _ = crate::state::store::write_manifest(&self.ws, &self.job_id, inputs);
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.progress.total = inputs.len() as u32;
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }

    fn on_cell(&self, rec: crate::state::store::CellRecord) {
        let _ = crate::state::store::append_partial(&self.ws, &self.job_id, &rec);
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.last_active = now_unix();
            st.progress.done += 1;
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }
}

/// R2.2：失败/超时时从 `<id>.partial.ndjson` 里已 checkpoint 的 findings 组一份 `incomplete`
/// 报告——绝不因为 dispatch 出错/超时就静默丢掉已经跑完的 cell。M 取 manifest 行数（若尚未
/// 写出 manifest 就已失败，退化用已完成数兜底，不出现 "N/0"）。
fn partial_report(ws: &Path, job_id: &str, args: &ReviewArgs, reason: String) -> (ReviewReport, i32) {
    let manifest = crate::state::store::read_manifest(ws, job_id);
    let done_records: Vec<_> = crate::state::store::read_partial(ws, job_id)
        .into_iter()
        .filter(|r| r.ok)
        .collect();
    let done = done_records.len();
    let total = manifest.len().max(done);
    let findings: Vec<_> = done_records.into_iter().flat_map(|r| r.findings).collect();
    let mode = match args.mode.as_deref() {
        Some("code") => Mode::Code,
        _ => Mode::Doc,
    };
    let stats = Stats::from_findings(&findings);
    let report = ReviewReport {
        schema_version: "1".into(),
        mode,
        verdict: Verdict::Incomplete,
        stats,
        findings,
        clusters: vec![],
        next_steps: vec![],
        run: RunMeta {
            job_id: job_id.to_string(),
            models: vec![],
            lenses: vec![],
            duration_ms: 0,
            usage: Usage::default(),
            warnings: vec![format!("partial: {done}/{total} cells"), reason],
            failed_models: vec![],
        },
    };
    let code = exit_code(Verdict::Incomplete, false);
    (report, code)
}

/// 落终态部分报告：渲染 json+md、写 result 文件、把 state 转终态并记录错误原因。
fn write_partial_result(
    ws: &Path,
    job_id: &str,
    st: &mut crate::state::job::JobState,
    status: crate::state::job::JobStatus,
    report: &ReviewReport,
    code: i32,
    error: String,
) {
    let json = crate::output::render(report, crate::output::Format::Json);
    let md = crate::output::render(report, crate::output::Format::Md);
    let _ = crate::state::store::write_result(ws, job_id, &json);
    let _ = crate::state::store::write_result_md(ws, job_id, &md);
    st.status = status;
    st.error = Some(error);
    st.exit_code = Some(code);
    st.result_path = Some(crate::state::store::job_result(ws, job_id).to_string_lossy().into_owned());
    let _ = crate::state::job::write_state(ws, st);
}

/// 状态机包裹评审：running → (评审) → done+result.json / failed+error。返回退出码。
/// 已被 cancel（状态 Cancelled）时不覆盖为 Done，保留 cancelled 终态。
/// `st.deadline_at`（worker 起手设，见 main.rs）到点仍未完成 → TimedOut + 部分报告（R1.2）。
pub async fn run_job(
    ws: &Path,
    job_id: &str,
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    cwd: &Path,
    started_at: u64,
) -> i32 {
    use crate::state::job::{read_state, write_state, JobState, JobStatus};
    use crate::state::store::{job_result, write_result};

    let mut st = read_state(ws, job_id).unwrap_or_else(|| JobState::new(job_id, started_at));
    // I2：写 Running 前检查是否已被 cancel，若是则直接返回，保留 Cancelled 终态。
    if st.status == JobStatus::Cancelled {
        return st.exit_code.unwrap_or(0);
    }
    st.status = JobStatus::Running;
    let _ = write_state(ws, &st);

    let sink = JobCellSink { ws: ws.to_path_buf(), job_id: job_id.to_string() };
    let dispatch = run_review_dispatch(args, provider, cfg, cwd, Some(&sink));

    // Outcome 只在本函数内构造/匹配一次，Box 间接层不值当——沿用 main.rs Cmd 枚举同款 allow。
    #[allow(clippy::large_enum_variant)]
    enum Outcome {
        Done(anyhow::Result<(ReviewReport, i32)>),
        TimedOut,
    }

    let outcome = match st.deadline_at {
        Some(deadline_secs) => {
            let remaining = deadline_secs.saturating_sub(now_unix());
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(remaining);
            tokio::select! {
                r = dispatch => Outcome::Done(r),
                _ = tokio::time::sleep_until(deadline) => Outcome::TimedOut,
            }
        }
        None => Outcome::Done(dispatch.await),
    };

    match outcome {
        Outcome::Done(Ok((report, code))) => {
            // 同渲染 json + md 落盘（result 命令按 format 直读，避免反序列化 report 图）。
            let json = crate::output::render(&report, crate::output::Format::Json);
            let md = crate::output::render(&report, crate::output::Format::Md);
            let _ = write_result(ws, job_id, &json);
            let _ = crate::state::store::write_result_md(ws, job_id, &md);
            // 若期间已被 cancel，保留 cancelled；否则 done。
            let latest = read_state(ws, job_id);
            let cancelled = matches!(latest.as_ref().map(|s| s.status), Some(JobStatus::Cancelled));
            st.status = if cancelled { JobStatus::Cancelled } else { JobStatus::Done };
            st.exit_code = Some(code);
            st.mode = Some(report.mode);
            st.models = report.run.models.clone();
            st.lenses = report.run.lenses.clone();
            st.result_path = Some(job_result(ws, job_id).to_string_lossy().into_owned());
            let _ = write_state(ws, &st);
            code
        }
        Outcome::Done(Err(e)) => {
            let (report, code) = partial_report(ws, job_id, args, e.to_string());
            write_partial_result(ws, job_id, &mut st, JobStatus::Failed, &report, code, e.to_string());
            code
        }
        Outcome::TimedOut => {
            let reason = "timed out: deadline exceeded".to_string();
            let (report, code) = partial_report(ws, job_id, args, reason.clone());
            write_partial_result(ws, job_id, &mut st, JobStatus::TimedOut, &report, code, reason);
            code
        }
    }
}

#[cfg(test)]
mod job_tests {
    use super::*;
    use crate::state::job::{read_state, JobStatus};
    use crate::state::store::read_result;
    use crate::provider::fake::FakeProvider;
    use crate::domain::{Finding, RawFinding, Severity};
    use std::collections::BTreeMap;

    fn checkpointed_finding(title: &str) -> Finding {
        Finding {
            id: "f".into(), severity: Severity::Medium, title: title.into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            lenses: vec![], models: vec!["alpha".into()], agreement: 1, hard_constraint: false,
            quote: "q".into(), cluster_id: None, verify: None,
        }
    }

    fn doc_args(target: &str) -> ReviewArgs {
        ReviewArgs {
            mode: Some("doc".into()), base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, lenses: None, models: None, models_all: false,
            no_correlate: false, block_threshold: None, conf_floor: None,
            background: true, format: "json".into(), targets: vec![target.into()],
        }
    }
    fn cfg() -> Config {
        Config::from_layers(&[
            "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
             [review]\ndefault_models=[\"alpha\"]\n",
        ]).unwrap()
    }

    #[tokio::test]
    async fn run_job_done_writes_result_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("spec.md");
        std::fs::write(&doc, "intro\nthe wrong claim here\nend\n").unwrap();
        let dp = doc.to_string_lossy().to_string();
        // alpha 报一条锚得到的 finding
        let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
        scripted.insert("alpha".into(), vec![RawFinding {
            severity: Severity::High, title: "t".into(), body: "b".into(), file: dp.clone(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "the wrong claim here".into(), lens: None,
        }]);
        let provider = FakeProvider { scripted };
        let ws = dir.path().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        crate::state::job::write_state(&ws, &crate::state::job::JobState::new("j1", 500)).unwrap();

        let code = run_job(&ws, "j1", &doc_args(&dp), &provider, &cfg(), dir.path(), 500).await;

        let st = read_state(&ws, "j1").unwrap();
        assert_eq!(st.status, JobStatus::Done);
        assert_eq!(st.exit_code, Some(code));
        assert!(st.result_path.is_some());
        // result.json 落盘且是合法 JSON
        let res = read_result(&ws, "j1").unwrap();
        assert!(res.contains("\"verdict\""));
    }

    #[tokio::test]
    async fn run_job_failed_records_error() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        crate::state::job::write_state(ws, &crate::state::job::JobState::new("j2", 0)).unwrap();
        // targets 指向不存在文件 → collect_doc 失败 → run_review_dispatch Err → failed
        let provider = FakeProvider { scripted: BTreeMap::new() };
        let code = run_job(ws, "j2", &doc_args("/nonexistent/zzz.md"), &provider, &cfg(), dir.path(), 0).await;
        assert_eq!(code, 2);
        let st = read_state(ws, "j2").unwrap();
        assert_eq!(st.status, JobStatus::Failed);
        assert!(st.error.is_some());
    }

    /// R2.2：dispatch 失败但已有 checkpoint（partial 里一条 ok:true 记录）→ result.json 不能
    /// 静默丢——应写 verdict=incomplete，findings 并入已 checkpoint 的记录，warnings 含 "partial:"。
    #[tokio::test]
    async fn run_job_failure_writes_incomplete_partial_report() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        crate::state::job::write_state(ws, &crate::state::job::JobState::new("j2b", 0)).unwrap();
        // 预置一条 partial（模拟崩溃前已完成 1 cell）
        crate::state::store::append_partial(
            ws, "j2b",
            &crate::state::store::CellRecord::ok("bundle[a]", "alpha", vec![checkpointed_finding("checkpointed")]),
        ).unwrap();

        // targets 指向不存在文件 → collect 失败 → run_review_dispatch Err，但已有 partial 记录
        let provider = FakeProvider { scripted: BTreeMap::new() };
        let code = run_job(ws, "j2b", &doc_args("/nonexistent/zzz.md"), &provider, &cfg(), dir.path(), 0).await;

        assert_eq!(code, 2);
        let st = read_state(ws, "j2b").unwrap();
        assert_eq!(st.status, JobStatus::Failed);
        assert!(st.error.is_some());
        let res = read_result(ws, "j2b").unwrap();
        assert!(res.contains("\"incomplete\""), "verdict 应为 incomplete: {res}");
        assert!(res.contains("partial:"), "warnings 应含 partial 标记: {res}");
        assert!(res.contains("checkpointed"), "已 checkpoint 的 finding 不应丢失: {res}");
    }

    /// 永不完成的 provider——保证 dispatch 分支不可能赢得 deadline race（而非靠真实短
    /// sleep+timeout 的时序假设）。用于 R1.2 deadline 超时测试，0 依赖、0 wall-clock 等待。
    struct HangProvider;

    #[async_trait::async_trait]
    impl crate::provider::ReviewProvider for HangProvider {
        async fn run(&self, _req: crate::provider::LensRequest) -> Result<crate::provider::LensCall, crate::provider::ProviderError> {
            std::future::pending::<()>().await;
            unreachable!("HangProvider never resolves")
        }
    }

    /// R1.2：`deadline_at` 已过期 + dispatch 永不完成 → run_job 走超时分支，写 TimedOut +
    /// 部分报告（不静默丢 deadline 前已 checkpoint 的 findings）。
    #[tokio::test]
    async fn run_job_timeout_writes_incomplete_partial_report() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let doc = dir.path().join("spec.md");
        std::fs::write(&doc, "intro\nsome content\nend\n").unwrap();
        let dp = doc.to_string_lossy().to_string();

        let mut st = crate::state::job::JobState::new("j-to", 0);
        st.deadline_at = Some(0); // 已过期（远小于 now）→ select 立即走超时分支
        crate::state::job::write_state(ws, &st).unwrap();
        // 预置一条 partial（模拟 deadline 前已完成 1 cell）
        crate::state::store::append_partial(
            ws, "j-to",
            &crate::state::store::CellRecord::ok("bundle[a]", "alpha", vec![checkpointed_finding("before-deadline")]),
        ).unwrap();

        let provider = HangProvider;
        let code = run_job(ws, "j-to", &doc_args(&dp), &provider, &cfg(), dir.path(), 0).await;

        assert_eq!(code, 2);
        let st = read_state(ws, "j-to").unwrap();
        assert_eq!(st.status, JobStatus::TimedOut);
        let res = read_result(ws, "j-to").unwrap();
        assert!(res.contains("\"incomplete\""), "verdict 应为 incomplete: {res}");
        assert!(res.contains("partial:"), "warnings 应含 partial 标记: {res}");
        assert!(res.contains("before-deadline"), "deadline 前已 checkpoint 的 finding 不应丢失: {res}");
    }

    /// I2：pre-cancel 场景——run_job 读到 Cancelled 状态时应直接返回，不覆盖为 Running/Done，
    /// 且不写 result.json。
    #[tokio::test]
    async fn run_job_honours_pre_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();

        // 先写一个 Cancelled 状态（模拟 cancel 命令先于 worker 启动）
        let mut pre = crate::state::job::JobState::new("j3", 0);
        pre.status = JobStatus::Cancelled;
        pre.exit_code = Some(0);
        crate::state::job::write_state(ws, &pre).unwrap();

        let provider = FakeProvider { scripted: BTreeMap::new() };
        let _code = run_job(ws, "j3", &doc_args("/any.md"), &provider, &cfg(), dir.path(), 0).await;

        // 状态仍应是 Cancelled
        let st = read_state(ws, "j3").unwrap();
        assert_eq!(st.status, JobStatus::Cancelled, "pre-cancel 状态不应被覆盖");
        // 不应写 result.json
        assert!(
            crate::state::store::read_result(ws, "j3").is_none(),
            "Cancelled 提前返回不应留下 result.json"
        );
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn cfg(providers: &str, defaults: &str) -> Config {
        let toml = format!("{providers}\n[review]\ndefault_models=[{defaults}]\n");
        Config::from_layers(&[&toml]).unwrap()
    }
    fn provs() -> &'static str {
        "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
         [providers.beta]\nkind=\"openai\"\nmodel=\"b\"\npriority=5\n"
    }

    #[test]
    fn explicit_models_flag_replicates() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(Some("alpha,beta"), false, &c);
        assert!(rep);
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn models_all_replicates_all_providers_by_priority() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(None, true, &c);
        assert!(rep);
        // 按 priority 降序：alpha(10) 在 beta(5) 前
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn nonempty_default_models_replicates() {
        let c = cfg(provs(), "\"beta\",\"alpha\"");
        let (m, rep) = resolve_models(None, false, &c);
        assert!(rep);
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["beta", "alpha"]);
    }

    #[test]
    fn empty_everything_falls_back_to_candidate_chain() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(None, false, &c);
        assert!(!rep, "no explicit list → candidate chain (fallback)");
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn models_all_overrides_models_flag() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(Some("alpha"), true, &c);
        assert!(rep);
        assert_eq!(m.len(), 2, "--models-all takes precedence over --models");
    }
}

#[cfg(test)]
mod code_review_tests {
    use super::*;
    use crate::domain::{RawFinding, Verdict};
    use crate::evidence::git::FileChange;
    use crate::provider::fake::FakeProvider;
    use std::io::Write;

    fn write(dir: &std::path::Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn exit_code_maps_all_cases() {
        use crate::domain::Verdict;
        assert_eq!(exit_code(Verdict::Reject, false), 10);
        assert_eq!(exit_code(Verdict::Reject, true), 10);   // reject 优先
        assert_eq!(exit_code(Verdict::Approve, true), 2);   // approve 但对照集不完整
        assert_eq!(exit_code(Verdict::Approve, false), 0);  // 干净 approve
        assert_eq!(exit_code(Verdict::Incomplete, false), 2); // 部分结果＝对照集不完整
        assert_eq!(exit_code(Verdict::Incomplete, true), 2);
    }

    #[tokio::test]
    async fn lone_blocking_refuted_does_not_reject() {
        use crate::domain::{Severity, Verdict, VerifyVerdict};
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        // 单 model alpha：review 报一条 High 孤证(单 model → agreement=1)；refute 退化自审 alpha。
        // 注：FakeProvider 是 prompt-blind 的——对同一 model 的任何调用（无论 review 还是 refute
        // prompt）都返回同一脚本，故 refute 调用返回非空 → refuted → confidence 0 → 不 reject。
        // 交叉 model 选择本身已在 verify.rs 单测验证；本测试只验「证伪翻转 verdict」全链路。
        let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha"]
"#]).unwrap();
        let provider = crate::provider::fake::FakeProvider::with(
            "alpha", vec![raw("src/a.rs", "db.write(token);", Severity::High)],
        );
        let my_args = ReviewArgs {
            mode: Some("code".into()), base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, lenses: Some("L1".into()), models: None, models_all: false,
            no_correlate: true, block_threshold: None, conf_floor: None,
            background: false, format: "json".into(), targets: vec![],
        };
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
        let (report, code) = run_code_review(&my_args, &provider, &cfg, changes, dir.path(), None).await.unwrap();
        let f = &report.findings[0];
        assert_eq!(f.verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(f.confidence, 0.0);
        assert_eq!(report.verdict, Verdict::Approve, "孤证被自审 refute 证伪 → 不翻 verdict");
        assert!(code == 0 || code == 2, "approve → 0 或 2（不 10）");
    }

    fn args() -> ReviewArgs {
        ReviewArgs {
            mode: Some("code".into()), base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, lenses: Some("L1".into()), models: Some("m".into()),
            models_all: false,
            no_correlate: true, block_threshold: None, conf_floor: None,
            background: false, format: "json".into(), targets: vec![],
        }
    }

    fn raw(file: &str, quote: &str, sev: crate::domain::Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: "issue".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()),
        }
    }

    #[tokio::test]
    async fn code_review_anchors_real_drops_fabricated() {
        use crate::domain::Severity;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        // 两个 model 都报真 finding → dedup 合并成 agreement=2 → 跳过层① verify，
        // 保持本测试「anchor 砍编造 + High→Reject」的原意（不被 S3-b verify 自审误伤）。
        // 编造 quote 只 m 报，anchor 因 quote 不在文件而丢弃。
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m","m2"]
"#]).unwrap();
        let mut scripted = std::collections::BTreeMap::new();
        scripted.insert("m".to_string(), vec![
            raw("src/a.rs", "db.write(token);", Severity::High),
            raw("src/a.rs", "this is fabricated", Severity::Critical),
        ]);
        scripted.insert("m2".to_string(), vec![raw("src/a.rs", "db.write(token);", Severity::High)]);
        let provider = FakeProvider { scripted };
        let my_args = ReviewArgs { models: None, ..args() };
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
        let (report, code) = run_code_review(&my_args, &provider, &cfg, changes, dir.path(), None).await.unwrap();
        assert_eq!(report.findings.len(), 1, "fabricated dropped, real merged across models");
        assert_eq!(report.findings[0].file, "src/a.rs");
        assert_eq!(report.findings[0].line_start, 2);
        assert_eq!(report.findings[0].agreement, 2, "two models agree → skips verify (layer ①)");
        assert_eq!(report.verdict, Verdict::Reject); // High agreement≥2，verify 跳过 → Reject
        assert_eq!(code, 10);
    }

    #[tokio::test]
    async fn code_review_empty_changes_approves() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        let provider = FakeProvider::with("m", vec![]);
        let (report, code) = run_code_review(&args(), &provider, &cfg, vec![], dir.path(), None).await.unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(code, 0);
    }
}
