use crate::bundle::{group, select_files};
use crate::config::Config;
use crate::consolidate::{apply_clusters, correlate, dedup, dedup_doc, verify_findings};
use crate::domain::{Finding, Mode, ReviewReport, RunMeta, Severity, Stats, Usage, Verdict};
use crate::evidence::{collect, detect_mode, EvidenceOpts, Scope};
use crate::evidence::git::FileChange;
use crate::lens::select;
use crate::orchestrate::{enrich, run_code_fanout, run_doc_fanout, CellSink, OrchestrateOpts, PlannedModel};
use crate::output::Format;
use crate::positioning::{anchor, anchor_doc};
use crate::provider::{LensRequest, ReviewProvider};
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
    // 前台同步路径是默认路径；`--background` 由 main 层解析并写入本字段。
    pub background: bool,
    pub format: String,
    pub targets: Vec<String>,
    /// R2.3：`--resume <job_id>`。main.rs 据此路由到 `run_resume`（前台同步，忽略 background）；
    /// 其余字段（mode/targets/models/…）在 resume 路径下不生效——run_resume 会重新
    /// `read_args_raw(job_id)` 取原始作业的 ReviewArgs。
    pub resume: Option<String>,
    /// R8.2：`--dismissed <目录|csv>`，经 `project::parse_dismissed` 解析。`serde(default)`
    /// 保证旧 job 落盘的 args.json（无此字段）仍可反序列化（resume 兼容）。
    #[serde(default)]
    pub dismissed: Option<String>,
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

/// R8.2：命中标记 + 派生索引（排序去重）。单点应用于共用尾段，三路径（code/doc/resume）同享。
pub(crate) fn mark_dismissed(
    findings: &mut [Finding],
    dismissed: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut reappeared = Vec::new();
    if dismissed.is_empty() {
        return reappeared;
    }
    for f in findings.iter_mut() {
        if dismissed.contains(&f.id) {
            f.previously_dismissed = true;
            reappeared.push(f.id.clone());
        }
    }
    reappeared.sort();
    reappeared.dedup();
    reappeared
}

/// 扇出之后的共用尾段上下文——`run_code_review`/`run_review`/`run_resume` 三处扇出前逻辑
/// 各不相同（bundle×model / lens×model / 直接重放 manifest prompt），但扇出后（anchor → dedup
/// → [doc: correlate] → verify → verdict → 组报告）完全一致，抽出复用（DRY，T7 授权解冻）。
struct TailCtx<'a> {
    mode: Mode,
    /// code 模式下 anchor 用的项目根；doc 模式忽略。
    root: Option<&'a Path>,
    /// doc 模式下 anchor_doc 用的受审文档目标；code 模式忽略。
    targets: &'a [String],
    provider: &'a dyn ReviewProvider,
    model_names: Vec<String>,
    lens_ids: Vec<String>,
    block: Severity,
    floor: f32,
    correlate_on: bool,
    max_concurrency: usize,
    job_id: String,
    duration_ms: u64,
    usage: (u64, u64),
    warnings: Vec<String>,
    failed_models: Vec<String>,
    dismissed: &'a std::collections::HashSet<String>,
    /// dir 形态台账全文（已截断）——refute pass 判例注入，主题变体跨轮一致抑制。
    dismissed_notes: &'a str,
}

/// 共用尾段：anchor → dedup → [doc: correlate/apply_clusters] → verify → verdict → 组装 report
/// → 退出码。三处调用方（run_code_review/run_review/run_resume）行为 1:1 迁移，不改变既有语义。
async fn assemble_report(findings: Vec<Finding>, ctx: TailCtx<'_>) -> anyhow::Result<(ReviewReport, i32)> {
    let anchored = match ctx.mode {
        Mode::Code => anchor(findings, ctx.root.expect("code tail requires root")),
        Mode::Doc => {
            let docs = crate::evidence::doc::collect_doc_files(ctx.targets)?;
            anchor_doc(findings, &docs)
        }
    };
    let mut findings = match ctx.mode {
        Mode::Code => dedup(anchored),
        Mode::Doc => dedup_doc(anchored),
    };
    clamp_compliance(&mut findings);
    let clusters = if ctx.mode == Mode::Doc {
        let lead_model = ctx.model_names.first().cloned().unwrap_or_default();
        let clusters = correlate(ctx.provider, &lead_model, &findings, ctx.correlate_on).await;
        apply_clusters(&mut findings, &clusters);
        clusters
    } else {
        vec![]
    };
    let (findings, refute_warnings) = verify_findings(
        findings, ctx.provider, &ctx.model_names, ctx.block, ctx.max_concurrency, ctx.mode,
        ctx.dismissed_notes,
    ).await;
    let mut run_warnings = ctx.warnings;
    run_warnings.extend(refute_warnings);

    let mut findings = findings;
    let dismissed_reappeared = mark_dismissed(&mut findings, ctx.dismissed);

    let verdict = compute_verdict(&findings, ctx.block, ctx.floor);
    let stats = Stats::from_findings(&findings);
    let report = ReviewReport {
        schema_version: "2".into(),
        mode: ctx.mode,
        verdict,
        stats,
        findings,
        clusters,
        next_steps: vec![],
        run: RunMeta {
            job_id: ctx.job_id,
            models: ctx.model_names,
            lenses: ctx.lens_ids,
            duration_ms: ctx.duration_ms,
            usage: Usage { input_tokens: ctx.usage.0, output_tokens: ctx.usage.1 },
            warnings: run_warnings,
            failed_models: ctx.failed_models,
            dismissed_reappeared,
        },
    };
    // 退出码：reject 优先 → 10；否则 approve 但对照集不完整 → 2；干净 approve → 0。
    let code = exit_code(verdict, !report.run.failed_models.is_empty());
    Ok((report, code))
}

/// 整条评审流水线：证据 → lens → fan-out → 共用尾段（assemble_report）。
///
/// 退出码：0=approve、10=reject、2=对照集不完整（failed_models 非空）。
/// `--background` 由 main.rs 在 dispatch 之前分叉（spawn worker）；到达这里即路由 bug。
pub async fn run_review(
    args: ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    if args.background {
        anyhow::bail!("--background forks in main.rs before dispatch; reaching here is a routing bug");
    }
    // Fix 3（final-review）：--dismissed 解析上提到证据收集/fan-out 之前 fail-fast——坏值不再
    // 烧完 LLM 调用才报错。
    let dismissed = match args.dismissed.as_deref() {
        Some(v) => crate::project::parse_dismissed(v)?,
        None => Default::default(),
    };
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
    };

    // 4. fan-out（doc：全 D rule 一包 × model 工具循环）
    let focus = args.focus.clone().unwrap_or_default();
    // job 上下文时由 run_job 传 JobCellSink；直调路径（无 job）传 None，行为不变。
    let (dismissed_notes, notes_warnings) = args.dismissed.as_deref().map(crate::project::load_dismissed_notes).unwrap_or_default();
    let mut raw = run_doc_fanout(provider, &input, &lenses, &focus, &dismissed_notes, &oopts, sink).await;
    raw.warnings.extend(notes_warnings);

    // 5-8. 共用尾段：anchor_doc → dedup_doc → correlate/apply_clusters → verify → verdict → 组报告
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let correlate_on = cfg.review.correlate && !args.no_correlate;
    let ctx = TailCtx {
        mode: input.mode,
        root: None,
        targets: &args.targets,
        provider,
        model_names,
        lens_ids,
        block,
        floor,
        correlate_on,
        max_concurrency: cfg.review.max_concurrency,
        job_id: String::new(),
        duration_ms: t0.elapsed().as_millis() as u64,
        usage: raw.usage,
        warnings: raw.warnings,
        failed_models: raw.failed_models,
        dismissed: &dismissed,
        dismissed_notes: &dismissed_notes,
    };
    if let Some(s) = sink { s.on_phase(crate::state::job::Phase::Finalizing); }
    assemble_report(raw.findings, ctx).await
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
        anyhow::bail!("--background forks in main.rs before dispatch; reaching here is a routing bug");
    }
    // Fix 3（final-review）：--dismissed 解析上提到 fan-out 之前 fail-fast——坏值不再烧完 LLM
    // 调用才报错。
    let dismissed = match args.dismissed.as_deref() {
        Some(v) => crate::project::parse_dismissed(v)?,
        None => Default::default(),
    };
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
    };

    // 4. fan-out（bundle × model）
    let focus = args.focus.clone().unwrap_or_default();
    let target_label = if args.scope == "branch" {
        format!("branch {}...HEAD", args.base.as_deref().unwrap_or("?"))
    } else {
        "working tree".to_string()
    };
    // job 上下文时由 run_job 传 JobCellSink；直调路径（无 job）传 None，行为不变。
    let (dismissed_notes, notes_warnings) = args.dismissed.as_deref().map(crate::project::load_dismissed_notes).unwrap_or_default();
    let mut raw = run_code_fanout(provider, &target_label, &bundles, &rules, &focus, &dismissed_notes, &oopts, sink).await;
    raw.warnings.extend(notes_warnings);

    // 5-8. 共用尾段：anchor → dedup → verify → verdict → 组报告（code 无 correlate/clusters）
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let ctx = TailCtx {
        mode: Mode::Code,
        root: Some(root),
        targets: &args.targets,
        provider,
        model_names,
        lens_ids,
        block,
        floor,
        correlate_on: false,
        max_concurrency: cfg.review.max_concurrency,
        job_id: String::new(),
        duration_ms: t0.elapsed().as_millis() as u64,
        usage: raw.usage,
        warnings: raw.warnings,
        failed_models: raw.failed_models,
        dismissed: &dismissed,
        dismissed_notes: &dismissed_notes,
    };
    if let Some(s) = sink { s.on_phase(crate::state::job::Phase::Finalizing); }
    assemble_report(raw.findings, ctx).await
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
/// （+刷 last_active/progress.done）。Bug B 修复：现在 on_cell 由并发 fan-out future 直接触发
/// （见 orchestrate/mod.rs），read_state→改→write_state 的 read-modify-write 段必须串行化，
/// 否则并发 cell 各自读到同一旧值、+1 后互相覆盖，丢更新。
/// ponytail：全局互斥锁最省事且正确——cell 完成是低频事件，锁粒度不值当细化。
struct JobCellSink {
    ws: std::path::PathBuf,
    job_id: String,
    lock: std::sync::Mutex<()>,
}

impl CellSink for JobCellSink {
    fn manifest(&self, inputs: &[crate::state::store::CellInput]) {
        let _g = self.lock.lock().unwrap();
        let _ = crate::state::store::write_manifest(&self.ws, &self.job_id, inputs);
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.progress.total = inputs.len() as u32;
            st.phase = Some(crate::state::job::Phase::Fanout);
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }

    fn on_cell(&self, rec: crate::state::store::CellRecord) {
        let _g = self.lock.lock().unwrap();
        let _ = crate::state::store::append_partial(&self.ws, &self.job_id, &rec);
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.last_active = now_unix();
            st.progress.done += 1;
            let _ = crate::state::job::write_state(&self.ws, &st);
        }
    }

    fn on_phase(&self, phase: crate::state::job::Phase) {
        let _g = self.lock.lock().unwrap();
        if let Some(mut st) = crate::state::job::read_state(&self.ws, &self.job_id) {
            st.phase = Some(phase);
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
        schema_version: "2".into(),
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
            dismissed_reappeared: vec![],
        },
    };
    let code = exit_code(Verdict::Incomplete, false);
    (report, code)
}

/// 空壳 ReviewArgs：仅供 `run_resume` 在 `read_args_raw` 缺失时兜底构造 incomplete 报告——
/// 正常路径不会走到（resume 只对经 `--background` 落过 args.json 的作业有意义）。
fn empty_review_args() -> ReviewArgs {
    ReviewArgs {
        mode: None, base: None, scope: "working-tree".into(), spec: vec![], focus: None,
        lenses: None, models: None, models_all: false, no_correlate: false,
        block_threshold: None, conf_floor: None, background: false, format: "json".into(),
        targets: vec![], resume: None, dismissed: None,
    }
}

/// R2.3：resume 差集补跑。读 `manifest`（全集 M）+ `partial`（已完成 ok:true 的 cell 集）→
/// `待跑 = M − 已完成`；对待跑 cell **直接用 manifest 里渲染好的 prompt 重放**
/// `provider.run(LensRequest{model,prompt})`——不重收集证据、不重 build prompt（确定性）；
/// append_partial 落盘后，`read_partial` 取全部 ok:true 的 findings（已完成 + 新补）走 mode
/// 对应的共用尾段（`assemble_report`）组出完整报告。全集补齐→verdict 正常；仍有缺失→`Incomplete`。
///
/// ponytail：候补链 cell（未指定 `--models`）的 `CellInput.model` 只是 manifest 写入时「最高
/// priority 候选」的近似快照（见设计 §6.5 ⚠️）——resume 按此近似重放。若该模型正是触发 resume
/// 的卡死模型，这次重放会再失败，该 cell 仍缺，可再次 `--resume`（渐进收敛，不会更糟）。精确解
/// 是重放完整候补链或把 manifest.model 升级为 `Vec<String>` 快照，本 task 不做（显式/replicate
/// cell 的 manifest.model 已是精确值，dogfood 常见场景不受影响）。
pub async fn run_resume(
    ws: &Path,
    job_id: &str,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    cwd: &Path,
) -> (ReviewReport, i32) {
    let t0 = std::time::Instant::now();
    let manifest = crate::state::store::read_manifest(ws, job_id);
    // 空 manifest = job 在 fan-out 写 manifest 前就崩溃：没有任何 work-list，尾段会把 0 findings
    // 组装成干净 Approve（假阳性——评审工具静默给未经审查的通过）。护栏：直接出 Incomplete。
    if manifest.is_empty() {
        return partial_report(
            ws, job_id, &empty_review_args(),
            "resume: empty manifest (job crashed before fan-out); nothing to resume".into(),
        );
    }
    let args: ReviewArgs = match crate::state::store::read_args_raw(ws, job_id)
        .and_then(|j| serde_json::from_str(&j).ok())
    {
        Some(a) => a,
        None => {
            return partial_report(
                ws, job_id, &empty_review_args(),
                "resume: missing job args (job never ran via --background)".into(),
            );
        }
    };
    let mode = match args.mode.as_deref() {
        Some("code") => Mode::Code,
        _ => Mode::Doc,
    };
    let fallback_lens = match mode { Mode::Code => "code", Mode::Doc => "doc" };

    let done: std::collections::BTreeSet<String> = crate::state::store::read_partial(ws, job_id)
        .into_iter()
        .filter(|r| r.ok)
        .map(|r| r.cell)
        .collect();
    let pending: Vec<&crate::state::store::CellInput> =
        manifest.iter().filter(|c| !done.contains(&c.cell)).collect();

    let mut usage = (0u64, 0u64);
    let mut warnings = Vec::new();
    for cell in &pending {
        let req = LensRequest { model: cell.model.clone(), prompt: cell.prompt.clone() };
        let rec = match provider.run(req).await {
            Ok(call) => {
                usage.0 += call.input_tokens;
                usage.1 += call.output_tokens;
                let fs: Vec<Finding> =
                    call.findings.iter().map(|r| enrich(r, fallback_lens, &cell.model)).collect();
                crate::state::store::CellRecord::ok(cell.cell.clone(), cell.model.clone(), fs)
            }
            Err(e) => {
                warnings.push(format!("resume cell [{}] model {}: {e}", cell.cell, cell.model));
                crate::state::store::CellRecord {
                    cell: cell.cell.clone(), model: cell.model.clone(), ok: false,
                    findings: vec![], warn: Some(e.to_string()),
                }
            }
        };
        let _ = crate::state::store::append_partial(ws, job_id, &rec);
    }

    // 合并出报告：全部 ok:true 的 findings（已完成 + 新补）。
    let final_records = crate::state::store::read_partial(ws, job_id);
    let ok_cells: std::collections::BTreeSet<&str> =
        final_records.iter().filter(|r| r.ok).map(|r| r.cell.as_str()).collect();
    let still_missing = manifest.iter().filter(|c| !ok_cells.contains(c.cell.as_str())).count();
    let findings: Vec<Finding> =
        final_records.into_iter().filter(|r| r.ok).flat_map(|r| r.findings).collect();

    let (models, _replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let lens_ids: Vec<String> = args
        .lenses
        .as_deref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| match mode {
            Mode::Code => cfg.review.lenses.code.clone(),
            Mode::Doc => cfg.review.lenses.doc.clone(),
        });
    let block = args.block_threshold.as_deref().map(parse_sev).unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let correlate_on = cfg.review.correlate && !args.no_correlate;
    // resume 因重读 args.json 自然继承原 --dismissed；run_resume 不返回 Result（异步状态机式），
    // 故此处不能用 `?`——解析失败按既有约定折成 partial_report（同 empty-manifest/missing-args）。
    let dismissed = match args.dismissed.as_deref().map(crate::project::parse_dismissed) {
        Some(Ok(d)) => d,
        Some(Err(e)) => {
            return partial_report(ws, job_id, &args, format!("resume: bad --dismissed: {e}"));
        }
        None => Default::default(),
    };
    let (dismissed_notes, notes_warnings) =
        args.dismissed.as_deref().map(crate::project::load_dismissed_notes).unwrap_or_default();
    warnings.extend(notes_warnings);

    let ctx = TailCtx {
        mode,
        root: if mode == Mode::Code { Some(cwd) } else { None },
        targets: &args.targets,
        provider,
        model_names,
        lens_ids,
        block,
        floor,
        correlate_on,
        max_concurrency: cfg.review.max_concurrency,
        job_id: job_id.to_string(),
        duration_ms: t0.elapsed().as_millis() as u64,
        usage,
        warnings,
        failed_models: vec![],
        dismissed: &dismissed,
        dismissed_notes: &dismissed_notes,
    };
    match assemble_report(findings, ctx).await {
        Ok((mut report, mut code)) => {
            if still_missing > 0 {
                report.verdict = Verdict::Incomplete;
                report.run.warnings.push(format!("resume: {still_missing} cell(s) still missing after replay"));
                code = exit_code(Verdict::Incomplete, false);
            }
            (report, code)
        }
        Err(e) => partial_report(ws, job_id, &args, format!("resume tail failed: {e}")),
    }
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
    st.phase = Some(crate::state::job::Phase::Collecting);
    let _ = write_state(ws, &st);

    let sink = JobCellSink { ws: ws.to_path_buf(), job_id: job_id.to_string(), lock: std::sync::Mutex::new(()) };
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
            // Bug C 修复：以刚读到的最新状态（含 sink 在运行期写入的 progress/last_active）
            // 为底稿叠加终态字段，而非复用函数入口处那份陈旧 st（只设过 status=Running），
            // 否则会把 sink 写的 progress/last_active 覆盖回 0/started_at。
            let latest = read_state(ws, job_id);
            let cancelled = matches!(latest.as_ref().map(|s| s.status), Some(JobStatus::Cancelled));
            let mut st = latest.unwrap_or(st);
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
            // Bug C 修复：同上，叠加终态前先取最新状态做底稿。
            let mut st = read_state(ws, job_id).unwrap_or(st);
            write_partial_result(ws, job_id, &mut st, JobStatus::Failed, &report, code, e.to_string());
            code
        }
        Outcome::TimedOut => {
            let reason = "timed out: deadline exceeded".to_string();
            let (report, code) = partial_report(ws, job_id, args, reason.clone());
            // Bug C 修复：同上，叠加终态前先取最新状态做底稿。
            let mut st = read_state(ws, job_id).unwrap_or(st);
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
            quote: "q".into(), cluster_id: None, verify: None, previously_dismissed: false,
        }
    }

    fn doc_args(target: &str) -> ReviewArgs {
        ReviewArgs {
            mode: Some("doc".into()), base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, lenses: None, models: None, models_all: false,
            no_correlate: false, block_threshold: None, conf_floor: None,
            background: true, format: "json".into(), targets: vec![target.into()],
            resume: None, dismissed: None,
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
        // Bug C 回归：run_job 收尾 write_state 必须以 sink 运行期写的最新 progress 为底稿
        // 叠加终态字段，而不是复用函数入口那份陈旧 st（否则 done/total 会被重置回 0/0）。
        assert_eq!(st.progress.done, 1, "收尾不应吞掉 sink 写入的 progress");
        assert_eq!(st.progress.total, 1, "收尾不应吞掉 sink 写入的 progress");
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

    /// Bug B 回归：`JobCellSink::on_cell` 的 read_state→改→write_state 段必须串行化。
    /// 无锁时并发 N 次调用会各自读到同一旧 progress、各自 +1、后写覆盖先写，最终
    /// `progress.done < N`；加全局锁后必须精确等于 N。用真实 OS 线程（而非 tokio 任务，
    /// 后者在单线程 runtime 下不会真正并发）制造竞争。
    #[test]
    fn job_cell_sink_on_cell_concurrent_no_lost_update() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        crate::state::job::write_state(&ws, &crate::state::job::JobState::new("jc1", 0)).unwrap();
        let sink = Arc::new(JobCellSink {
            ws: ws.clone(),
            job_id: "jc1".to_string(),
            lock: std::sync::Mutex::new(()),
        });

        let n = 20usize;
        let handles: Vec<_> = (0..n)
            .map(|i| {
                let sink = sink.clone();
                std::thread::spawn(move || {
                    sink.on_cell(crate::state::store::CellRecord::ok(
                        format!("cell{i}"),
                        "m".to_string(),
                        vec![],
                    ));
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let st = read_state(&ws, "jc1").unwrap();
        assert_eq!(
            st.progress.done, n as u32,
            "并发 on_cell 不应丢更新（无锁会 < {n}）"
        );
    }
}

#[cfg(test)]
mod resume_tests {
    use super::*;
    use crate::domain::{Finding, RawFinding, Severity};
    use crate::state::store::CellInput;

    /// 记录每次 `run` 调用的 prompt，脚本化返回一条低 severity finding（避免触发 verify 的
    /// 二次 provider 调用，测试只关心 resume 对缺失 cell 的调用次数）。
    #[derive(Default)]
    struct RecordingProvider {
        prompts: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl crate::provider::ReviewProvider for RecordingProvider {
        async fn run(
            &self,
            req: crate::provider::LensRequest,
        ) -> Result<crate::provider::LensCall, crate::provider::ProviderError> {
            self.prompts.lock().unwrap().push(req.prompt.clone());
            Ok(crate::provider::LensCall {
                findings: vec![RawFinding {
                    severity: Severity::Low,
                    title: format!("issue-{}", req.prompt),
                    body: "b".into(),
                    file: "spec.md".into(),
                    line_start: 1,
                    line_end: 1,
                    confidence: 0.9,
                    recommendation: "r".into(),
                    hard_constraint: false,
                    quote: String::new(), // 空引用=遗漏类，anchor_doc 无条件保留
                    lens: None,
                }],
                input_tokens: 1,
                output_tokens: 1,
            })
        }
    }

    fn omission_finding(title: &str) -> Finding {
        Finding {
            id: title.into(), severity: Severity::Low, title: title.into(), body: "b".into(),
            file: "spec.md".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec![], models: vec!["m".into()], agreement: 1,
            hard_constraint: false, quote: String::new(), cluster_id: None, verify: None,
            previously_dismissed: false,
        }
    }

    /// T7 核心测试：manifest 3 cell、partial 已完成 1（c1）→ resume 只应对缺失的 c2/c3 调用
    /// provider，最终报告合并出全部 3 个 cell 的 findings。
    #[tokio::test]
    async fn resume_only_runs_missing_cells() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // doc 模式尾段需要至少一个可读的受审文档（omission finding 本身不依赖其内容）。
        let doc = dir.path().join("spec.md");
        std::fs::write(&doc, "whatever\n").unwrap();
        let dp = doc.to_string_lossy().to_string();

        let inputs = vec![
            CellInput { cell: "c1".into(), model: "m".into(), prompt: "P1".into() },
            CellInput { cell: "c2".into(), model: "m".into(), prompt: "P2".into() },
            CellInput { cell: "c3".into(), model: "m".into(), prompt: "P3".into() },
        ];
        crate::state::store::write_manifest(ws, "j", &inputs).unwrap();
        crate::state::store::append_partial(
            ws, "j",
            &crate::state::store::CellRecord::ok("c1", "m", vec![omission_finding("issue-c1")]),
        ).unwrap();

        let args = ReviewArgs {
            mode: Some("doc".into()), base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, lenses: None, models: Some("m".into()), models_all: false,
            no_correlate: true, block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), targets: vec![dp], resume: None, dismissed: None,
        };
        crate::state::store::write_args_raw(ws, "j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let cfg = Config::default();

        let (report, _code) = run_resume(ws, "j", &provider, &cfg, dir.path()).await;

        let calls = provider.prompts.lock().unwrap();
        assert_eq!(calls.len(), 2, "只应对缺失的 2 个 cell 调用 provider");
        assert!(calls.contains(&"P2".to_string()), "应补跑 c2");
        assert!(calls.contains(&"P3".to_string()), "应补跑 c3");
        assert!(!calls.contains(&"P1".to_string()), "已完成的 c1 不应重跑");
        drop(calls);

        assert_eq!(report.findings.len(), 3, "合并后应含 3 个 cell 的 findings（1 已完成 + 2 新补）");
    }

    /// final-review Important 回归：replicate 模式（同 cell 多 model 副本）manifest 的
    /// cell-id 必须带上 model（如 `doc@alpha`/`doc@beta`），否则 resume 的差集判定（join on
    /// cell）会把「同 cell 任一 model 已完成」误判为「该 cell 全部完成」，缺失的 model 副本
    /// 永不补跑，还会静默拼出一份只含部分 model 的「完整」报告。
    /// 这里手工构造 manifest 模拟 run_doc_fanout 修复后的输出（2 行，cell 各自带 @model），
    /// partial 只完成 doc@alpha → 断言 doc@beta 必须被补跑，且合并结果含两个 model 的 finding。
    #[tokio::test]
    async fn resume_replicate_missing_model_copy_is_completed() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let doc = dir.path().join("spec.md");
        std::fs::write(&doc, "whatever\n").unwrap();
        let dp = doc.to_string_lossy().to_string();

        let inputs = vec![
            CellInput { cell: "doc@alpha".into(), model: "alpha".into(), prompt: "P-alpha".into() },
            CellInput { cell: "doc@beta".into(), model: "beta".into(), prompt: "P-beta".into() },
        ];
        crate::state::store::write_manifest(ws, "j", &inputs).unwrap();
        crate::state::store::append_partial(
            ws, "j",
            &crate::state::store::CellRecord::ok("doc@alpha", "alpha", vec![omission_finding("issue-alpha")]),
        ).unwrap();

        let args = ReviewArgs {
            mode: Some("doc".into()), base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, lenses: None, models: Some("alpha,beta".into()), models_all: false,
            no_correlate: true, block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), targets: vec![dp], resume: None, dismissed: None,
        };
        crate::state::store::write_args_raw(ws, "j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let cfg = Config::default();

        let (report, _code) = run_resume(ws, "j", &provider, &cfg, dir.path()).await;

        let calls = provider.prompts.lock().unwrap();
        assert_eq!(calls.len(), 1, "只应对缺失的 doc@beta 调用 provider（doc@alpha 已完成不得重跑）");
        assert!(calls.contains(&"P-beta".to_string()), "beta 副本必须被补跑，不能被 alpha 的完成状态静默坍缩");
        drop(calls);

        assert_eq!(
            report.findings.len(), 2,
            "合并后应含 alpha+beta 两个 model 副本的 findings，不能只含 alpha 就报完整"
        );
        assert_ne!(
            report.verdict, crate::domain::Verdict::Incomplete,
            "两个 model 副本都已补齐，不应再是 Incomplete"
        );
    }

    /// 假阳性防护：manifest 从未写出（job 在 fan-out 前崩溃）→ resume 不得静默出 Approve，
    /// 必须 Incomplete + 非零退出，且不调 provider。
    #[tokio::test]
    async fn resume_empty_manifest_is_incomplete_not_approve() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        // 只写 args，从不 write_manifest（模拟 fan-out 前崩溃）。
        let args = ReviewArgs {
            mode: Some("doc".into()), base: None, scope: "working-tree".into(), spec: vec![],
            focus: None, lenses: None, models: Some("m".into()), models_all: false,
            no_correlate: true, block_threshold: None, conf_floor: None, background: true,
            format: "json".into(), targets: vec![], resume: None, dismissed: None,
        };
        crate::state::store::write_args_raw(ws, "j", &serde_json::to_string(&args).unwrap()).unwrap();

        let provider = RecordingProvider::default();
        let (report, code) = run_resume(ws, "j", &provider, &Config::default(), dir.path()).await;

        assert_eq!(report.verdict, crate::domain::Verdict::Incomplete, "空 manifest 不得静默 Approve");
        assert_ne!(code, 0, "退出码非 0");
        assert_eq!(provider.prompts.lock().unwrap().len(), 0, "无 cell 可跑，不应调 provider");
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
            resume: None, dismissed: None,
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
            resume: None, dismissed: None,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn jobcellsink_manifest_sets_phase_fanout() {
        use crate::state::job::{JobState, Phase, write_state, read_state};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        write_state(&ws, &JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { ws: ws.clone(), job_id: "j".into(), lock: std::sync::Mutex::new(()) };
        use crate::orchestrate::CellSink;
        sink.manifest(&[crate::state::store::CellInput {
            cell: "c".into(), model: "m".into(), prompt: "p".into() }]);
        assert_eq!(read_state(&ws, "j").unwrap().phase, Some(Phase::Fanout));
    }

    #[tokio::test]
    async fn jobcellsink_on_phase_writes_phase() {
        use crate::state::job::{JobState, Phase, write_state, read_state};
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        write_state(&ws, &JobState::new("j", 0)).unwrap();
        let sink = JobCellSink { ws: ws.clone(), job_id: "j".into(), lock: std::sync::Mutex::new(()) };
        use crate::orchestrate::CellSink;
        sink.on_phase(Phase::Finalizing);
        assert_eq!(read_state(&ws, "j").unwrap().phase, Some(Phase::Finalizing));
    }

    #[test]
    fn dismissed_survives_line_shift_and_is_marked_exempt() {
        use crate::orchestrate::enrich;
        use std::collections::HashSet;
        let raw = |ls: u32, le: u32| crate::domain::RawFinding {
            severity: Severity::High, title: "Weak hash".into(), body: "b".into(),
            file: "src/x.rs".into(), line_start: ls, line_end: le, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false,
            quote: "md5(pw)".into(), lens: None,
        };
        let before = enrich(&raw(5, 7), "L1", "m");          // 第一轮：驳回它
        let dismissed: HashSet<String> = [before.id.clone()].into();
        let mut after = vec![enrich(&raw(50, 52), "L1", "m")]; // 修复位移后再现
        let reappeared = mark_dismissed(&mut after, &dismissed);
        assert_eq!(reappeared, vec![before.id]);
        assert!(after[0].previously_dismissed);
        assert_eq!(compute_verdict(&after, Severity::Medium, 0.5), Verdict::Approve);
    }
}
