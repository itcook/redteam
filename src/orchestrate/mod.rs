use crate::bundle::Bundle;
use crate::domain::{Finding, Origin, RawFinding, ReviewInput};
use crate::hash::fnv1a64_hex;
use crate::lens::Lens;
use crate::prompt::{build_bundle_prompt, build_doc_prompt};
use crate::provider::{LensRequest, ReviewProvider};
#[cfg(test)]
use crate::provider::ProviderError;
use crate::state::store::{CellInput, CellRecord};
use std::cmp::Reverse;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// 扇出持久化钩子：扇出前写输入快照（`manifest`，全集一次性），每 cell 完成即写结果（`on_cell`）。
/// `sink=None` 时全跳过——`run_review`/`run_code_review` 无 job 上下文的直调路径行为不变。
pub trait CellSink: Send + Sync {
    fn manifest(&self, inputs: &[CellInput]);
    fn on_cell(&self, rec: CellRecord);
    /// 阶段转换钩子（后台 job 用；默认 no-op，前台 sink=None / 其他 sink 零影响）。
    /// `note`：阶段注记（如 refute 的 "N targets"），进 phase_history 时间线。
    fn on_phase(&self, _phase: crate::state::job::Phase, _note: Option<String>) {}
    /// resolve 完成即报 models/lenses（后台 job stamp 进 state，running 期 status 可见；
    /// 此前 done 时才从 report 回填——r1 20260703 实证运行全程空数组）。默认 no-op。
    fn on_meta(&self, _models: &[String], _lenses: &[String]) {}
}

#[derive(Clone, Debug)]
pub struct PlannedModel {
    pub name: String,
    pub priority: i32,
}

#[derive(Clone, Debug)]
pub struct OrchestrateOpts {
    pub models: Vec<PlannedModel>,
    pub replicate: bool,
    pub no_fallback: bool,
    pub max_concurrency: usize,
}

#[derive(Default, Debug)]
pub struct RawResults {
    pub findings: Vec<Finding>,
    pub warnings: Vec<String>,
    pub failed_models: Vec<String>,
    pub usage: (u64, u64, u64), // (input, output, cached_input)
}

/// R8.1：quote 去除全部空白（保大小写）——token 序列即身份，免疫 reformat。
fn normalize_quote(q: &str) -> String {
    q.chars().filter(|c| !c.is_whitespace()).collect()
}

/// R8.1：title lowercase + 空白折叠单空格。
fn normalize_title(t: &str) -> String {
    t.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// finding id 组成的单一真相源（R8.1 + M13 rename 迁移共用）：quote 锚定、行无关；
/// 空 quote 退化行号路径（不保证跨 fixup 稳定）。
pub fn finding_id(file: &str, quote: &str, title: &str, line_start: u32, line_end: u32) -> String {
    let nq = normalize_quote(quote);
    let nt = normalize_title(title);
    if nq.is_empty() {
        fnv1a64_hex(&[file, &line_start.to_string(), &line_end.to_string(), &nt])
    } else {
        fnv1a64_hex(&[file, &nq, &nt])
    }
}

pub fn enrich(raw: &RawFinding, fallback_lens: &str, model: &str) -> Finding {
    let id = finding_id(&raw.file, &raw.quote, &raw.title, raw.line_start, raw.line_end);
    let lens = raw.lens.as_deref().unwrap_or(fallback_lens).to_string();
    Finding {
        id,
        severity: raw.severity,
        title: raw.title.clone(),
        body: raw.body.clone(),
        file: raw.file.clone(),
        line_start: raw.line_start,
        line_end: raw.line_end,
        confidence: raw.confidence,
        recommendation: raw.recommendation.clone(),
        lenses: vec![lens],
        models: vec![model.to_string()],
        agreement: 1,
        hard_constraint: raw.hard_constraint,
        quote: raw.quote.clone(),
        previously_dismissed: false,
        cluster_id: None,
        verify: None,
        origin: Origin::New,
    }
}

/// 候补链场景 manifest 的 model 快照：实际命中模型运行前未知，仅取 priority 最高候选作快照。
fn top_priority_model(opts: &OrchestrateOpts) -> String {
    opts.models
        .iter()
        .max_by_key(|m| m.priority)
        .map(|m| m.name.clone())
        .unwrap_or_default()
}

/// 单个文件包的评审（候补链复用 run_one_lens 的语义）。
/// 单 cell 执行结果（成功/失败一体：`warn=None` 即成功）。运行学字段 2026-07-03 grill 定案。
struct CellRun {
    model: String, // 胜出 model；整链失败为空
    findings: Vec<Finding>,
    tokens: (u64, u64, u64), // (input, output, cached_input)
    duration_ms: u64,
    attempts: u32,
    /// 胜出 call 的 tool-loop 迭代数（失败候补烧掉的轮数不可观测——Err 无 LensCall）；
    /// 整链失败记 0＝未观测。
    iterations: u32,
    /// 胜出 call 的工具调用构成（延迟诊断仪器）；整链失败/单发路径为零值。
    tool_use: crate::tools::ToolUse,
    warn: Option<String>,
}

impl CellRun {
    /// 转持久化记录；`fallback_model`：胜出者为空（整链失败）时的 model 标注
    /// （replicate 格=该格 model；候补链=空串，warn 已含逐家原因）。
    fn to_record(&self, cell_id: String, fallback_model: &str) -> CellRecord {
        CellRecord {
            cell: cell_id,
            model: if self.model.is_empty() { fallback_model.to_string() } else { self.model.clone() },
            ok: self.warn.is_none(),
            findings: self.findings.clone(),
            warn: self.warn.clone(),
            duration_ms: self.duration_ms,
            input_tokens: self.tokens.0,
            output_tokens: self.tokens.1,
            cached_input_tokens: self.tokens.2,
            attempts: self.attempts,
            iterations: self.iterations,
            tool_use: self.tool_use.clone(),
        }
    }
}

async fn run_one_bundle(
    provider: &dyn ReviewProvider,
    label: &str,
    prompt: &str,
    opts: &OrchestrateOpts,
) -> CellRun {
    let t0 = std::time::Instant::now();
    let mut chain: Vec<&PlannedModel> = opts.models.iter().collect();
    if !opts.replicate {
        chain.sort_by_key(|m| Reverse(m.priority));
    }
    let chain: Vec<&PlannedModel> = if opts.replicate || opts.no_fallback {
        chain.into_iter().take(1).collect()
    } else {
        chain
    };

    let mut errs: Vec<String> = Vec::new();
    for (i, pm) in chain.iter().enumerate() {
        match provider.run(LensRequest { model: pm.name.clone(), prompt: prompt.to_string() }).await {
            Ok(call) => {
                let fs = call
                    .findings
                    .iter()
                    .map(|r| enrich(r, "code", &pm.name))
                    .collect();
                return CellRun {
                    model: pm.name.clone(), findings: fs,
                    tokens: (call.input_tokens, call.output_tokens, call.cached_input_tokens),
                    duration_ms: t0.elapsed().as_millis() as u64,
                    attempts: i as u32 + 1, iterations: call.iterations, tool_use: call.tool_use, warn: None,
                };
            }
            Err(e) => errs.push(format!("bundle [{label}] model {}: {e}", pm.name)),
        }
    }
    CellRun {
        model: String::new(), findings: vec![], tokens: (0, 0, 0),
        duration_ms: t0.elapsed().as_millis() as u64,
        attempts: chain.len() as u32, iterations: 0, tool_use: Default::default(), warn: Some(errs.join("; ")),
    }
}

/// code 线扇出，用 Semaphore 限 max_concurrency：
/// - replicate=true：每 (bundle, model) 独立格子全跑（对抗复制），失败记 failed_models（守 exit-2）。
/// - replicate=false：每 bundle 一条候补链（容错，首个成功即止），整链失败只记 warning。
pub async fn run_code_fanout(
    provider: &dyn ReviewProvider,
    target_label: &str,
    bundles: &[Bundle],
    rules: &[&Lens],
    focus: &str,
    incremental: &str,
    dossiers: &[String],
    opts: &OrchestrateOpts,
    sink: Option<&dyn CellSink>,
) -> RawResults {
    let sem = Arc::new(Semaphore::new(opts.max_concurrency.max(1)));
    // 每个 bundle 的 label/prompt 只算一次：manifest 与执行复用同一份。
    let cells: Vec<(String, String, String)> = bundles
        .iter()
        .enumerate()
        .map(|(i, bundle)| {
            let label = bundle.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(",");
            let others = crate::bundle::other_files_manifest(bundles, i);
            let dossier = dossiers.get(i).map(String::as_str).unwrap_or("");
            let prompt = build_bundle_prompt(target_label, bundle, rules, focus, &others, incremental, dossier);
            let cell_id = format!("bundle[{label}]");
            (cell_id, label, prompt)
        })
        .collect();

    if opts.replicate {
        // 全复制：bundle × model 笛卡尔积，每格只试该 model（不候补）
        // cell-id 带上 model（`bundle[label]@<model>`）：replicate 下同 cell 有多模型格，带 model 后缀区分。
        let manifest: Vec<CellInput> = cells
            .iter()
            .flat_map(|(cell, _label, prompt)| {
                opts.models.iter().map(move |pm| CellInput {
                    cell: format!("{cell}@{}", pm.name),
                    model: pm.name.clone(),
                    prompt: prompt.clone(),
                })
            })
            .collect();
        if let Some(s) = sink {
            s.manifest(&manifest);
        }

        let futures = cells.iter().flat_map(|(cell, label, prompt)| {
            let sem = sem.clone();
            opts.models.iter().map(move |pm| {
                let sem = sem.clone();
                let model_name = pm.name.clone();
                let cell_id = format!("{cell}@{}", pm.name);
                let single = OrchestrateOpts {
                    models: vec![pm.clone()],
                    replicate: true,
                    no_fallback: opts.no_fallback,
                    max_concurrency: opts.max_concurrency,
                };
                async move {
                    let _permit = sem.acquire().await.expect("semaphore not closed");
                    let run = run_one_bundle(provider, label, prompt, &single).await;
                    // R2.1 修复：on_cell 在 future 内部即时调用。
                    if let Some(s) = sink {
                        s.on_cell(run.to_record(cell_id.clone(), &model_name));
                    }
                    (model_name, run)
                }
            })
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (model_hint, run) in results {
            out.usage.0 += run.tokens.0;
            out.usage.1 += run.tokens.1; out.usage.2 += run.tokens.2;
            match run.warn {
                None => out.findings.extend(run.findings),
                Some(warn) => {
                    out.warnings.push(warn);
                    if !out.failed_models.contains(&model_hint) {
                        out.failed_models.push(model_hint);
                    }
                }
            }
        }
        out
    } else {
        // 候补：每 bundle 一条候补链（按 priority 降序、首个成功即止）
        let manifest: Vec<CellInput> = cells
            .iter()
            .map(|(cell, _label, prompt)| CellInput {
                cell: cell.clone(),
                model: top_priority_model(opts),
                prompt: prompt.clone(),
            })
            .collect();
        if let Some(s) = sink {
            s.manifest(&manifest);
        }

        let futures = cells.iter().map(|(cell, label, prompt)| {
            let sem = sem.clone();
            let cell_id = cell.clone();
            async move {
                let _permit = sem.acquire().await.expect("semaphore not closed");
                let run = run_one_bundle(provider, label, prompt, opts).await;
                // R2.1 修复：on_cell 在 future 内部即时调用。
                if let Some(s) = sink {
                    s.on_cell(run.to_record(cell_id.clone(), ""));
                }
                run
            }
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for run in results {
            out.usage.0 += run.tokens.0;
            out.usage.1 += run.tokens.1; out.usage.2 += run.tokens.2;
            match run.warn {
                None => out.findings.extend(run.findings),
                Some(warn) => out.warnings.push(warn),
            }
        }
        out
    }
}

/// 单篇文档评审（整篇文档为单一单元、无 bundle；候补链语义同 run_one_bundle）。
async fn run_one_doc(
    provider: &dyn ReviewProvider,
    prompt: &str,
    opts: &OrchestrateOpts,
) -> CellRun {
    let t0 = std::time::Instant::now();
    let mut chain: Vec<&PlannedModel> = opts.models.iter().collect();
    if !opts.replicate {
        chain.sort_by_key(|m| Reverse(m.priority));
    }
    let chain: Vec<&PlannedModel> = if opts.replicate || opts.no_fallback {
        chain.into_iter().take(1).collect()
    } else {
        chain
    };

    let mut errs: Vec<String> = Vec::new();
    for (i, pm) in chain.iter().enumerate() {
        match provider.run(LensRequest { model: pm.name.clone(), prompt: prompt.to_string() }).await {
            Ok(call) => {
                let fs = call.findings.iter().map(|r| enrich(r, "doc", &pm.name)).collect();
                return CellRun {
                    model: pm.name.clone(), findings: fs,
                    tokens: (call.input_tokens, call.output_tokens, call.cached_input_tokens),
                    duration_ms: t0.elapsed().as_millis() as u64,
                    attempts: i as u32 + 1, iterations: call.iterations, tool_use: call.tool_use, warn: None,
                };
            }
            Err(e) => errs.push(format!("doc model {}: {e}", pm.name)),
        }
    }
    CellRun {
        model: String::new(), findings: vec![], tokens: (0, 0, 0),
        duration_ms: t0.elapsed().as_millis() as u64,
        attempts: chain.len() as u32, iterations: 0, tool_use: Default::default(), warn: Some(errs.join("; ")),
    }
}

/// doc 线扇出（整篇文档为单一单元）：
/// - replicate=true：每 model 各跑一遍（对抗复制），失败记 failed_models（守 exit-2）；Semaphore 限并发。
/// - replicate=false：单条候补链（按 priority 降序、首个成功即止），整链失败只记 warning。
pub async fn run_doc_fanout(
    provider: &dyn ReviewProvider,
    input: &ReviewInput,
    rules: &[&Lens],
    focus: &str,
    opts: &OrchestrateOpts,
    sink: Option<&dyn CellSink>,
) -> RawResults {
    // 整篇文档为单一 cell：prompt 只渲染一次，manifest 与执行复用同一份。
    let prompt = build_doc_prompt(input, rules, focus);
    let cell_id = "doc".to_string();

    if opts.replicate {
        // cell-id 带上 model（`doc@<model>`）：replicate 下同 cell 有多模型格，带 model 后缀区分。
        let manifest: Vec<CellInput> = opts
            .models
            .iter()
            .map(|pm| CellInput {
                cell: format!("{cell_id}@{}", pm.name),
                model: pm.name.clone(),
                prompt: prompt.clone(),
            })
            .collect();
        if let Some(s) = sink {
            s.manifest(&manifest);
        }

        let sem = Arc::new(Semaphore::new(opts.max_concurrency.max(1)));
        let futures = opts.models.iter().map(|pm| {
            let sem = sem.clone();
            let model_name = pm.name.clone();
            let cell_id = format!("{cell_id}@{}", pm.name);
            let prompt = &prompt;
            let single = OrchestrateOpts {
                models: vec![pm.clone()],
                replicate: true,
                no_fallback: opts.no_fallback,
                max_concurrency: opts.max_concurrency,
            };
            async move {
                let _permit = sem.acquire().await.expect("semaphore not closed");
                let run = run_one_doc(provider, prompt, &single).await;
                // R2.1 修复：on_cell 在 future 内部即时调用。
                if let Some(s) = sink {
                    s.on_cell(run.to_record(cell_id.clone(), &model_name));
                }
                (model_name, run)
            }
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (model_hint, run) in results {
            out.usage.0 += run.tokens.0;
            out.usage.1 += run.tokens.1; out.usage.2 += run.tokens.2;
            match run.warn {
                None => out.findings.extend(run.findings),
                Some(warn) => {
                    out.warnings.push(warn);
                    if !out.failed_models.contains(&model_hint) {
                        out.failed_models.push(model_hint);
                    }
                }
            }
        }
        out
    } else {
        // 单一单元候补链：仅一个任务，无需信号量。
        let manifest = vec![CellInput {
            cell: cell_id.clone(),
            model: top_priority_model(opts),
            prompt: prompt.clone(),
        }];
        if let Some(s) = sink {
            s.manifest(&manifest);
        }

        let mut out = RawResults::default();
        let run = run_one_doc(provider, &prompt, opts).await;
        if let Some(s) = sink {
            s.on_cell(run.to_record(cell_id, ""));
        }
        out.usage.0 += run.tokens.0;
        out.usage.1 += run.tokens.1; out.usage.2 += run.tokens.2;
        match run.warn {
            None => out.findings.extend(run.findings),
            Some(warn) => out.warnings.push(warn),
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    use crate::lens::by_id;
    use crate::provider::fake::FakeProvider;
    use crate::domain::{Mode, ReviewInput, RawFinding};

    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    use crate::lens::by_id as lens_by_id;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn raw_q(t: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: t.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(), lens: Some("L1".into()),
        }
    }

    fn raw_at(file: &str, title: &str, sev: Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(), lens: Some("L1".into()),
        }
    }

    fn bundle(path: &str) -> Bundle {
        Bundle { files: vec![FileChange { path: path.into(), diff: "+x".into() }] }
    }

    #[tokio::test]
    async fn code_fanout_runs_every_bundle() {
        let p = FakeProvider::with("m", vec![raw_q("found")]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10 }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs"), bundle("src/c.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let r = run_code_fanout(&p, "wt", &bundles, &rules, "", "", &[], &opts, None).await;
        assert_eq!(r.findings.len(), 3, "one finding per bundle");
        // raw.lens=Some("L1") → enrich 用 L1（非 fallback）
        assert!(r.findings.iter().all(|f| f.lenses == vec!["L1".to_string()]));
    }

    // prompt 记录探针：断言每个 bundle 收到自己（而非别的 bundle）的卷宗段。
    struct PromptProbe {
        prompts: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl ReviewProvider for PromptProbe {
        async fn run(&self, req: LensRequest) -> Result<crate::provider::LensCall, ProviderError> {
            self.prompts.lock().unwrap().push(req.prompt);
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, iterations: 1, tool_use: Default::default() })
        }
    }

    #[tokio::test]
    async fn code_fanout_threads_per_bundle_dossier() {
        let p = PromptProbe { prompts: std::sync::Mutex::new(vec![]) };
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10 }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let rules = vec![lens_by_id("L1").unwrap()];
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs")];
        let dossiers = vec![
            "<dossier>\nDOSSIER-A\n</dossier>".to_string(),
            "<dossier>\nDOSSIER-B\n</dossier>".to_string(),
        ];
        let _ = run_code_fanout(&p, "wt", &bundles, &rules, "", "", &dossiers, &opts, None).await;
        let prompts = p.prompts.lock().unwrap();
        let pa = prompts.iter().find(|s| s.contains("### src/a.rs")).expect("bundle a 的 prompt 应被采集");
        let pb = prompts.iter().find(|s| s.contains("### src/b.rs")).expect("bundle b 的 prompt 应被采集");
        assert!(pa.contains("DOSSIER-A"), "bundle a 的 prompt 应带自己的卷宗 A");
        assert!(!pa.contains("DOSSIER-B"), "bundle a 的 prompt 不应带 bundle b 的卷宗（防反索引）");
        assert!(pb.contains("DOSSIER-B"), "bundle b 的 prompt 应带自己的卷宗 B");
        assert!(!pb.contains("DOSSIER-A"), "bundle b 的 prompt 不应带 bundle a 的卷宗（防反索引）");
    }

    // 并发探针：断言同时在飞的 provider 调用数不超过 max_concurrency。
    struct ConcProbe {
        inflight: AtomicUsize,
        max_seen: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ReviewProvider for ConcProbe {
        async fn run(&self, _req: LensRequest) -> Result<crate::provider::LensCall, ProviderError> {
            let cur = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(cur, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, iterations: 1, tool_use: Default::default() })
        }
    }

    #[tokio::test]
    async fn code_fanout_respects_max_concurrency() {
        let probe = Arc::new(ConcProbe { inflight: AtomicUsize::new(0), max_seen: AtomicUsize::new(0) });
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10 }],
            replicate: true, no_fallback: false, max_concurrency: 2,
        };
        let bundles: Vec<Bundle> = (0..6).map(|i| bundle(&format!("src/f{i}.rs"))).collect();
        let rules = vec![lens_by_id("L1").unwrap()];
        let probe_ref: &ConcProbe = &probe;
        let _ = run_code_fanout(probe_ref, "wt", &bundles, &rules, "", "", &[], &opts, None).await;
        assert!(probe.max_seen.load(Ordering::SeqCst) <= 2, "concurrency exceeded cap: {}", probe.max_seen.load(Ordering::SeqCst));
        assert!(probe.max_seen.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn code_fanout_explicit_unreachable_model_records_failed() {
        // 显式模型完全不可用 → 整链失败 → failed_models 记该模型（守 exit-2）
        let p = FakeProvider::with("present", vec![]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "absent".into(), priority: 10 }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let r = run_code_fanout(&p, "wt", &bundles, &rules, "", "", &[], &opts, None).await;
        assert!(r.findings.is_empty());
        assert!(r.failed_models.contains(&"absent".to_string()), "explicit unreachable model must be recorded");
        assert!(!r.warnings.is_empty());
    }

    #[tokio::test]
    async fn code_fanout_replicate_runs_every_model_per_bundle() {
        use crate::evidence::git::FileChange;
        use std::collections::BTreeMap;
        // 两个 model 各产 1 条 finding（不同 title），手动多 model script
        let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
        scripted.insert("alpha".into(), vec![raw_at("src/a.rs", "alpha-issue", Severity::High)]);
        scripted.insert("beta".into(), vec![raw_at("src/a.rs", "beta-issue", Severity::High)]);
        let provider = crate::provider::fake::FakeProvider { scripted, ..Default::default() };
        let bundles = vec![
            Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "+x".into() }] },
            Bundle { files: vec![FileChange { path: "src/b.rs".into(), diff: "+y".into() }] },
        ];
        let l1 = by_id("L1").unwrap();
        let rules = vec![l1];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true,
            no_fallback: false,
            max_concurrency: 8,
        };
        let out = run_code_fanout(&provider, "working tree", &bundles, &rules, "", "", &[], &opts, None).await;
        // 2 bundle × 2 model = 4 格，各 1 finding → 4 条
        assert_eq!(out.findings.len(), 4);
        let models: std::collections::BTreeSet<&str> =
            out.findings.iter().flat_map(|f| f.models.iter().map(|s| s.as_str())).collect();
        assert!(models.contains("alpha") && models.contains("beta"));
        assert!(out.failed_models.is_empty());
    }

    #[tokio::test]
    async fn code_fanout_replicate_records_failed_model() {
        use crate::evidence::git::FileChange;
        // 只有 alpha 有 script；beta 不可达 → 记 failed_models
        let provider = crate::provider::fake::FakeProvider::with(
            "alpha",
            vec![raw_at("src/a.rs", "alpha-issue", Severity::High)],
        );
        let bundles = vec![Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "+x".into() }] }];
        let l1 = by_id("L1").unwrap();
        let rules = vec![l1];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true,
            no_fallback: false,
            max_concurrency: 8,
        };
        let out = run_code_fanout(&provider, "working tree", &bundles, &rules, "", "", &[], &opts, None).await;
        assert_eq!(out.findings.len(), 1); // alpha 成功
        assert!(out.failed_models.contains(&"beta".to_string()));
    }

    fn doc_input() -> ReviewInput {
        ReviewInput {
            mode: Mode::Doc, target_label: "spec.md".into(),
            collection_guidance: String::new(), body: "DOCBODY".into(), spec_docs: vec![],
        }
    }

    #[tokio::test]
    async fn doc_fanout_replicate_runs_every_model() {
        use std::collections::BTreeMap;
        let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
        scripted.insert("alpha".into(), vec![raw_at("spec.md", "alpha-issue", Severity::High)]);
        scripted.insert("beta".into(), vec![raw_at("spec.md", "beta-issue", Severity::High)]);
        let provider = crate::provider::fake::FakeProvider { scripted, ..Default::default() };
        let rules = vec![by_id("D1").unwrap(), by_id("D2").unwrap()];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true, no_fallback: false, max_concurrency: 8,
        };
        let sink = RecSink { manifest_n: 0.into(), records: Vec::new().into() };
        let out = run_doc_fanout(&provider, &doc_input(), &rules, "", &opts, Some(&sink)).await;
        assert_eq!(out.findings.len(), 2, "one finding per model over the single doc");
        // final-review Important 回归：replicate 下同一 base cell（"doc"）的多 model 副本，
        // manifest + on_cell 的 cell-id 必须各自带上 model 才唯一——否则 resume 的差集判定
        // （join on cell）会把「任一 model 已完成」误判为「该 cell 全部完成」，见 cli.rs
        // resume_replicate_missing_model_copy_is_completed。
        let manifest_cells: std::collections::BTreeSet<String> =
            sink.records.lock().unwrap().iter().map(|r| r.cell.clone()).collect();
        assert_eq!(manifest_cells.len(), 2, "两个 model 副本的 cell-id 必须彼此唯一，不能坍缩成同一个 \"doc\"");
        assert!(manifest_cells.contains("doc@alpha") && manifest_cells.contains("doc@beta"));
        let models: std::collections::BTreeSet<&str> =
            out.findings.iter().flat_map(|f| f.models.iter().map(|s| s.as_str())).collect();
        assert!(models.contains("alpha") && models.contains("beta"));
        assert!(out.failed_models.is_empty());
    }

    #[tokio::test]
    async fn doc_fanout_replicate_records_failed_model() {
        let provider = FakeProvider::with("alpha", vec![raw_at("spec.md", "alpha-issue", Severity::High)]);
        let rules = vec![by_id("D1").unwrap()];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true, no_fallback: false, max_concurrency: 8,
        };
        let out = run_doc_fanout(&provider, &doc_input(), &rules, "", &opts, None).await;
        assert_eq!(out.findings.len(), 1);
        assert!(out.failed_models.contains(&"beta".to_string()));
    }

    #[tokio::test]
    async fn doc_fanout_fallback_chain_first_success_wins() {
        // 非 replicate 候补：claude 无脚本、gpt 有 → 候补命中 gpt，不记 failed_models。
        let provider = FakeProvider::with("gpt", vec![raw_at("spec.md", "found-by-gpt", Severity::High)]);
        let rules = vec![by_id("D1").unwrap()];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "claude".into(), priority: 30 },
                PlannedModel { name: "gpt".into(), priority: 20 },
            ],
            replicate: false, no_fallback: false, max_concurrency: 4,
        };
        let out = run_doc_fanout(&provider, &doc_input(), &rules, "", &opts, None).await;
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].models, vec!["gpt".to_string()]);
        assert!(out.failed_models.is_empty());
    }

    // R2.1：记录型 sink，断言扇出前写 manifest（全集）+ 每 cell 完成各回调一次。
    // 记完整 CellRecord（而非只记 cell id），供 T4 回归测试断言 model 字段。
    struct RecSink {
        manifest_n: std::sync::Mutex<usize>,
        records: std::sync::Mutex<Vec<CellRecord>>,
    }
    impl CellSink for RecSink {
        fn manifest(&self, inputs: &[CellInput]) {
            *self.manifest_n.lock().unwrap() = inputs.len();
        }
        fn on_cell(&self, rec: CellRecord) {
            self.records.lock().unwrap().push(rec);
        }
    }

    #[tokio::test]
    async fn fanout_writes_manifest_and_each_cell() {
        let p = FakeProvider::with("m", vec![raw_q("found")]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10 }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs"), bundle("src/c.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let sink = RecSink { manifest_n: 0.into(), records: Vec::new().into() };
        let _ = run_code_fanout(&p, "wt", &bundles, &rules, "", "", &[], &opts, Some(&sink)).await;
        assert_eq!(*sink.manifest_n.lock().unwrap(), 3, "manifest 全集=3 cell");
        assert_eq!(sink.records.lock().unwrap().len(), 3, "每 cell 各回调一次");
    }

    // T5 Important 回归：checkpoint 现在是流式的——on_cell 必须在每个 cell 自己的 future
    // 内部即时触发，而不是等 join_all 收齐全部结果后才批量回调（旧实现下进程中途被杀，
    // 已完成的 cell 也会因为还没来得及回调而在 proj.db 的 cell checkpoint 里全部丢失）。
    //
    // 用一慢一快两个 bundle 验证时序：慢 cell（src/slow.rs）的 provider 调用故意 sleep，
    // 快 cell（src/fast.rs）立即返回。若 on_cell 仍是旧的「join_all 之后统一回调」，快 cell
    // 的回调必然要等慢 cell 也跑完才会触发；改成流式后，快 cell 的回调应该在慢 cell 完成前
    // 就已经落地。
    struct SlowFastProbe {
        slow_done: std::sync::atomic::AtomicBool,
    }
    #[async_trait::async_trait]
    impl ReviewProvider for SlowFastProbe {
        async fn run(&self, req: LensRequest) -> Result<crate::provider::LensCall, ProviderError> {
            // 精确匹配「本包文件行」——清单段注入后其他包的 prompt 也含 slow.rs 路径
            if req.prompt.contains("Files in this bundle: src/slow.rs") {
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                self.slow_done.store(true, Ordering::SeqCst);
            }
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, iterations: 1, tool_use: Default::default() })
        }
    }

    struct StreamingProbeSink<'a> {
        slow_done: &'a std::sync::atomic::AtomicBool,
        // Some(true) 表示 fast cell 的 on_cell 触发时，slow cell 尚未完成（流式，符合预期）。
        // Some(false) 表示触发时 slow 已完成（说明退化回了旧的批量回调）。
        fast_saw_slow_incomplete: std::sync::Mutex<Option<bool>>,
        calls: AtomicUsize,
    }
    impl CellSink for StreamingProbeSink<'_> {
        fn manifest(&self, _inputs: &[CellInput]) {}
        fn on_cell(&self, rec: CellRecord) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if rec.cell.contains("fast.rs") {
                *self.fast_saw_slow_incomplete.lock().unwrap() =
                    Some(!self.slow_done.load(Ordering::SeqCst));
            }
        }
    }

    #[tokio::test]
    async fn on_cell_fires_per_cell_before_slow_sibling_completes() {
        let provider = SlowFastProbe { slow_done: std::sync::atomic::AtomicBool::new(false) };
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10 }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/slow.rs"), bundle("src/fast.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let sink = StreamingProbeSink {
            slow_done: &provider.slow_done,
            fast_saw_slow_incomplete: None.into(),
            calls: AtomicUsize::new(0),
        };
        let _ = run_code_fanout(&provider, "wt", &bundles, &rules, "", "", &[], &opts, Some(&sink)).await;
        assert_eq!(sink.calls.load(Ordering::SeqCst), 2, "on_cell 调用次数=cell 数（聚合前逐个触发）");
        let saw = *sink.fast_saw_slow_incomplete.lock().unwrap();
        assert_eq!(
            saw,
            Some(true),
            "快 cell 的 on_cell 必须在慢 cell 完成前触发——证明每 cell 完成即落盘（流式 checkpoint）"
        );
    }

    // R4.3 降级不变量：一个 model 在 tool loop 中途死亡（重试耗尽仍失败）
    // → 该 cell 失败记 failed_models，run 继续，另一个 model 的 findings 存活。
    #[tokio::test]
    async fn mid_loop_model_death_degrades_gracefully() {
        use crate::provider::registry::ProviderRegistry;
        use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend, ToolLoopProvider};
        use crate::provider::ProviderError;
        use crate::tools::ToolCall;
        use serde_json::json;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x").unwrap();
        let good_json = r#"{"findings":[{"severity":"high","title":"real","body":"b","file":"src/a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r"}]}"#;

        // good：一轮直接产最终 findings
        let good = ToolLoopProvider::new(
            Box::new(FakeChatBackend::new(vec![AssistantTurn {
                text: good_json.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None }])),
            dir.path().to_path_buf());
        // bad：先 tool_call（iter1），再一路 Timeout（iter2 重试耗尽仍失败）。
        // DEFAULT_RETRY_BUDGET=2 → iter2 需 3 次 Err（首发+2 重试）→ 备 1 Ok + 3 Err。
        let tc = AssistantTurn { text: String::new(),
            tool_calls: vec![ToolCall { id: "c".into(), name: "read_file".into(), args: json!({"path":"a.rs"}) }],
            input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None };
        let bad = ToolLoopProvider::new(
            Box::new(FakeChatBackend::new_results(vec![
                Ok(tc),
                Err(ProviderError::Timeout("dead".into())),
                Err(ProviderError::Timeout("dead".into())),
                Err(ProviderError::Timeout("dead".into())),
            ])),
            dir.path().to_path_buf());

        let mut reg = ProviderRegistry::new();
        reg.insert("good", Box::new(good));
        reg.insert("bad", Box::new(bad));

        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "good".into(), priority: 1 },
                PlannedModel { name: "bad".into(), priority: 1 },
            ],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let out = run_code_fanout(&reg, "wt", &bundles, &rules, "", "", &[], &opts, None).await;
        assert!(out.failed_models.contains(&"bad".to_string()),
            "坏 model 应记 failed_models: {:?}", out.failed_models);
        assert!(out.findings.iter().any(|f| f.title == "real"),
            "好 model 的 finding 应仍在报告里");
    }

    // R8.1：quote 锚定、行无关的 finding id。
    // 注：命名为 raw_full（而非 raw）以避免与本模块已有的 `fn raw(t: &str)` 撞名——
    // Rust 无重载，两者签名不同无法共用一个名字。
    fn raw_full(file: &str, ls: u32, le: u32, title: &str, quote: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(),
            file: file.into(), line_start: ls, line_end: le, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false, quote: quote.into(), lens: None,
        }
    }

    #[test]
    fn id_ignores_line_shift_when_quote_present() {
        let a = enrich(&raw_full("a.rs", 10, 12, "SQL injection", "let q = fmt!(sql);"), "L1", "m");
        let b = enrich(&raw_full("a.rs", 99, 101, "SQL injection", "let q = fmt!(sql);"), "L1", "m");
        assert_eq!(a.id, b.id, "行号位移不得改变 id");
    }

    #[test]
    fn id_ignores_whitespace_reformat() {
        let a = enrich(&raw_full("a.rs", 1, 2, "SQL injection", "let q =\n    fmt!( sql );"), "L1", "m");
        let b = enrich(&raw_full("a.rs", 1, 2, "SQL injection", "letq=fmt!(sql);"), "L1", "m");
        assert_eq!(a.id, b.id, "quote 去空白后等价 → 同 id");
    }

    #[test]
    fn id_title_normalization_case_and_ws() {
        let a = enrich(&raw_full("a.rs", 1, 1, "SQL  Injection", "x"), "L1", "m");
        let b = enrich(&raw_full("a.rs", 1, 1, "sql injection", "x"), "L1", "m");
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn id_composition_pinned() {
        // 组成 golden：钉死组成与归一（FNV 算法本身由 hash.rs golden 钉死）。
        let f = enrich(&raw_full("src/x.rs", 5, 7, "Weak  Hash", "md5( pw )"), "L1", "m");
        assert_eq!(f.id, crate::hash::fnv1a64_hex(&["src/x.rs", "md5(pw)", "weak hash"]));
    }

    #[test]
    fn id_falls_back_to_lines_without_quote() {
        let a = enrich(&raw_full("a.rs", 1, 2, "t", ""), "L1", "m");
        let b = enrich(&raw_full("a.rs", 3, 4, "t", ""), "L1", "m");
        assert_ne!(a.id, b.id, "无 quote 退化回行号路径");
        assert_eq!(a.id, crate::hash::fnv1a64_hex(&["a.rs", "1", "2", "t"]));
    }
}
