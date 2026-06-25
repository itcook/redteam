use crate::config::Config;
use crate::consolidate::{apply_clusters, correlate, dedup};
use crate::domain::{Mode, ReviewReport, RunMeta, Severity, Stats, Usage, Verdict};
use crate::evidence::{collect, EvidenceOpts, Scope};
use crate::lens::select;
use crate::orchestrate::{run_fanout, OrchestrateOpts, PlannedModel};
use crate::output::Format;
use crate::provider::ReviewProvider;
use crate::verdict::{clamp_compliance, compute_verdict};

#[derive(Clone, Debug)]
pub struct ReviewArgs {
    pub mode: Option<String>,
    pub base: Option<String>,
    pub scope: String,
    pub spec: Vec<String>,
    pub focus: Option<String>,
    pub lenses: Option<String>,
    pub models: Option<String>,
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

/// 整条评审流水线：证据 → lens → fan-out → dedup → clamp → correlate → apply_clusters
/// → verdict → 组装 report → 退出码。
///
/// 退出码：0=approve、10=reject、2=对照集不完整（failed_models 非空）。
/// `--background` 在 Plan 1 未实现（留 Plan 2），此处直接报错。
/// 默认前台同步路径无需 `wait` 字段——`run_review` 本身就是同步等待模式。
/// MVP 多模型保护：`--models` 显式传入 >1 个不同 provider 名时返回 Err。
/// 多 provider 路由属于 Plan 2 的 ProviderRegistry；当前 MVP 只能诚实地跑单模型。
pub fn check_multi_model(models: &Option<String>) -> anyhow::Result<()> {
    if let Some(m) = models {
        let mut names: Vec<&str> = m.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        names.dedup();
        if names.len() > 1 {
            anyhow::bail!(
                "multi-model review needs ProviderRegistry (Plan 2); MVP supports a single model, \
use --models <one> or omit. Got {} distinct models: {}",
                names.len(),
                names.join(", ")
            );
        }
    }
    Ok(())
}

pub async fn run_review(
    args: ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
) -> anyhow::Result<(ReviewReport, i32)> {
    if args.background {
        anyhow::bail!("--background is not yet implemented (planned for Plan 2)");
    }
    // MVP 多模型保护（在 run_review 层也检查，使函数可测）
    check_multi_model(&args.models)?;
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

    // 3. 模型集 + 显式判定
    let explicit = args.models.is_some();
    let model_names: Vec<String> = match &args.models {
        Some(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
        None if !cfg.review.default_models.is_empty() => cfg.review.default_models.clone(),
        None => {
            // 默认：全部已配置 provider，按 priority 降序（候补链）
            let mut v: Vec<_> = cfg.providers.iter().collect();
            v.sort_by_key(|p| std::cmp::Reverse(p.1.priority));
            v.into_iter().map(|(k, _)| k.clone()).collect()
        }
    };
    let models: Vec<PlannedModel> = model_names
        .iter()
        .map(|n| PlannedModel {
            name: n.clone(),
            priority: cfg.providers.get(n).map(|p| p.priority).unwrap_or(0),
        })
        .collect();

    let oopts = OrchestrateOpts {
        models,
        explicit,
        no_fallback: false,
        max_concurrency: cfg.review.max_concurrency,
        retries: 2,
    };

    // 4. fan-out
    let focus = args.focus.clone().unwrap_or_default();
    let raw = run_fanout(provider, &input, &lenses, &focus, &oopts).await;

    // 5. consolidate：dedup → clamp → correlate → apply_clusters
    let mut findings = dedup(raw.findings);
    clamp_compliance(&mut findings);
    let correlate_on = cfg.review.correlate && !args.no_correlate;
    let lead_model = model_names.first().cloned().unwrap_or_default();
    let clusters = correlate(provider, &lead_model, &findings, correlate_on).await;
    apply_clusters(&mut findings, &clusters);

    // 6. verdict
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
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
    let code = match (verdict, !report.run.failed_models.is_empty()) {
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2,
        (Verdict::Approve, false) => 0,
    };
    Ok((report, code))
}
