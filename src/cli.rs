use crate::bundle::{group, select_files};
use crate::config::Config;
use crate::consolidate::{apply_clusters, correlate, dedup};
use crate::domain::{Mode, ReviewReport, RunMeta, Severity, Stats, Usage, Verdict};
use crate::evidence::{collect, EvidenceOpts, Scope};
use crate::evidence::git::FileChange;
use crate::lens::select;
use crate::orchestrate::{run_code_fanout, run_fanout, OrchestrateOpts, PlannedModel};
use crate::output::Format;
use crate::positioning::anchor;
use crate::provider::ReviewProvider;
use crate::verdict::{clamp_compliance, compute_verdict};
use std::path::Path;

#[derive(Clone, Debug)]
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

/// code 专属流水线（S2-2b）：bundle × model 工具循环 + positioning 尾段。
/// 退出码同 run_review：0=approve、10=reject、2=对照集不完整。
pub async fn run_code_review(
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    changes: Vec<FileChange>,
    root: &Path,
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
    let raw = run_code_fanout(provider, &target_label, &bundles, &rules, &focus, &oopts).await;

    // 5. positioning（dedup 之前：准行号反哺 line-overlap 去重）
    let anchored = anchor(raw.findings, root);

    // 6. consolidate：dedup → clamp（correlate 留到 S3）
    let mut findings = dedup(anchored);
    clamp_compliance(&mut findings);

    // 7. verdict
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
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

    let code = match (verdict, !report.run.failed_models.is_empty()) {
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2,
        (Verdict::Approve, false) => 0,
    };
    Ok((report, code))
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
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        // 真 quote（锚定到第 2 行）+ 编造 quote（丢弃）
        let provider = FakeProvider::with("m", vec![
            raw("src/a.rs", "db.write(token);", Severity::High),
            raw("src/a.rs", "this is fabricated", Severity::Critical),
        ]);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
        let (report, code) = run_code_review(&args(), &provider, &cfg, changes, dir.path()).await.unwrap();
        assert_eq!(report.findings.len(), 1, "fabricated dropped");
        assert_eq!(report.findings[0].file, "src/a.rs");
        assert_eq!(report.findings[0].line_start, 2);
        assert_eq!(report.verdict, Verdict::Reject); // High ≥ medium block
        assert_eq!(code, 10);
    }

    #[tokio::test]
    async fn code_review_empty_changes_approves() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        let provider = FakeProvider::with("m", vec![]);
        let (report, code) = run_code_review(&args(), &provider, &cfg, vec![], dir.path()).await.unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(code, 0);
    }
}
