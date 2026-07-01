use crate::bundle::Bundle;
use crate::domain::{Finding, RawFinding, ReviewInput};
use crate::hash::fnv1a64_hex;
use crate::lens::Lens;
use crate::prompt::{build_bundle_prompt, build_doc_prompt, build_prompt};
use crate::provider::{LensRequest, ProviderError, ReviewProvider};
use crate::state::store::{CellInput, CellRecord};
use std::cmp::Reverse;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// 扇出持久化钩子：扇出前写输入快照（`manifest`，全集一次性），每 cell 完成即写结果（`on_cell`）。
/// `sink=None` 时全跳过——`run_review`/`run_code_review` 无 job 上下文的直调路径行为不变。
pub trait CellSink: Send + Sync {
    fn manifest(&self, inputs: &[CellInput]);
    fn on_cell(&self, rec: CellRecord);
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
    pub retries: u32,
}

#[derive(Default, Debug)]
pub struct RawResults {
    pub findings: Vec<Finding>,
    pub warnings: Vec<String>,
    pub failed_models: Vec<String>,
    pub usage: (u64, u64),
}

pub fn enrich(raw: &RawFinding, fallback_lens: &str, model: &str) -> Finding {
    let id = fnv1a64_hex(&[
        &raw.file,
        &raw.line_start.to_string(),
        &raw.line_end.to_string(),
        &raw.title.to_lowercase(),
    ]);
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
        cluster_id: None,
        verify: None,
    }
}

async fn call_with_retry(
    provider: &dyn ReviewProvider,
    model: &str,
    prompt: &str,
    retries: u32,
) -> Result<crate::provider::LensCall, ProviderError> {
    let mut attempt = 0u32;
    loop {
        let req = LensRequest {
            model: model.to_string(),
            prompt: prompt.to_string(),
        };
        match provider.run(req).await {
            Ok(c) => return Ok(c),
            Err(e) => {
                if attempt >= retries {
                    return Err(e);
                }
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(
                    200 * (1u64 << attempt),
                ))
                .await;
            }
        }
    }
}

/// 对单个 (lens, 候补链) 执行调用。
/// 默认路径：按 priority 降序逐个候补，首个成功即止。
/// 显式路径：只试 models[0]，失败则记录，不候补。
/// 返回 Ok((findings, usage)) 或 Err(warn_message)。
/// warn_message 包含链中所有失败模型的累积错误信息。
async fn run_one_lens(
    provider: &dyn ReviewProvider,
    prompt: &str,
    lens: &Lens,
    opts: &OrchestrateOpts,
) -> Result<(String, Vec<Finding>, (u64, u64)), String> {
    // 默认路径：按 priority 降序排列，逐个候补
    let mut chain: Vec<&PlannedModel> = opts.models.iter().collect();
    if !opts.replicate {
        chain.sort_by_key(|m| Reverse(m.priority));
    }

    // 显式或 no_fallback 时只取第一个
    let chain: Vec<&PlannedModel> = if opts.replicate || opts.no_fallback {
        chain.into_iter().take(1).collect()
    } else {
        chain
    };

    let mut errs: Vec<String> = Vec::new();

    for pm in &chain {
        match call_with_retry(provider, &pm.name, prompt, opts.retries).await {
            Ok(call) => {
                let fs = call
                    .findings
                    .iter()
                    .map(|r| enrich(r, lens.id, &pm.name))
                    .collect();
                return Ok((pm.name.clone(), fs, (call.input_tokens, call.output_tokens)));
            }
            Err(e) => {
                errs.push(format!("lens {} model {}: {}", lens.id, pm.name, e));
            }
        }
    }

    Err(errs.join("; "))
}

/// 整个 (lens × model) 矩阵的 fan-out。
///
/// 默认路径（explicit=false）：每个 lens 一条候补链，按 priority 降序尝试，首个成功即止。
/// 显式路径（explicit=true）：每个 (lens, model) 独立格子，失败记 warning + failed_models，不候补。
pub async fn run_fanout(
    provider: &dyn ReviewProvider,
    input: &ReviewInput,
    lenses: &[&Lens],
    focus: &str,
    opts: &OrchestrateOpts,
    sink: Option<&dyn CellSink>,
) -> RawResults {
    // 收集所有任务的 future，然后用 join_all 并发执行。
    // 由于 provider、input、focus、opts 都是借用且生命周期覆盖所有 future，
    // 不需要 Arc——直接在 async block 中使用引用即可。
    //
    // 每个 future 的类型：
    //   impl Future<Output = (/*model_hint*/ String, Result<(Vec<Finding>, (u64,u64)), String>)>

    // 每个 lens 的 prompt 只渲染一次：manifest 与执行复用同一份。
    let cells: Vec<(String, String)> = lenses
        .iter()
        .map(|lens| (format!("lens:{}", lens.id), build_prompt(input, lens, focus)))
        .collect();

    if opts.replicate {
        // 显式：每个 (lens, model) 独立格子，失败记 warning + failed_models
        let manifest: Vec<CellInput> = cells
            .iter()
            .flat_map(|(cell, prompt)| {
                opts.models.iter().map(move |pm| CellInput {
                    cell: cell.clone(),
                    model: pm.name.clone(),
                    prompt: prompt.clone(),
                })
            })
            .collect();
        if let Some(s) = sink {
            s.manifest(&manifest);
        }

        let futures: Vec<_> = lenses
            .iter()
            .zip(cells.iter())
            .flat_map(|(lens, (cell, prompt))| {
                opts.models.iter().map(move |pm| {
                    let model_name = pm.name.clone();
                    let cell_id = cell.clone();
                    let single_opts = OrchestrateOpts {
                        models: vec![pm.clone()],
                        replicate: true,
                        no_fallback: opts.no_fallback,
                        max_concurrency: opts.max_concurrency,
                        retries: opts.retries,
                    };
                    async move {
                        let res = run_one_lens(provider, prompt, lens, &single_opts).await;
                        // R2.1 修复：on_cell 在 future 内部即时调用，cell 一完成即落盘——
                        // 崩溃只丢在飞的那个 cell（而非等 join_all 全部结束后批量落盘丢光全部）。
                        if let Some(s) = sink {
                            match &res {
                                Ok((_m, fs, _)) => {
                                    s.on_cell(CellRecord::ok(cell_id.clone(), model_name.clone(), fs.clone()));
                                }
                                Err(warn) => {
                                    s.on_cell(CellRecord {
                                        cell: cell_id.clone(),
                                        model: model_name.clone(),
                                        ok: false,
                                        findings: vec![],
                                        warn: Some(warn.clone()),
                                    });
                                }
                            }
                        }
                        (model_name, res)
                    }
                })
            })
            .collect();

        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (model_hint, res) in results {
            match res {
                Ok((_m, fs, (it, ot))) => {
                    out.findings.extend(fs);
                    out.usage.0 += it;
                    out.usage.1 += ot;
                }
                Err(warn) => {
                    out.warnings.push(warn);
                    if !out.failed_models.contains(&model_hint) {
                        out.failed_models.push(model_hint);
                    }
                }
            }
        }
        out
    } else {
        // 默认：每个 lens 一条候补链
        // 整条候补链失败只记 warnings，不写 failed_models（避免误导 Task 13 退出码）
        // manifest 的 model 记高优先候选（候补链本身无固定 model，此处只是快照近似值）。
        let manifest: Vec<CellInput> = cells
            .iter()
            .map(|(cell, prompt)| CellInput {
                cell: cell.clone(),
                model: top_priority_model(opts),
                prompt: prompt.clone(),
            })
            .collect();
        if let Some(s) = sink {
            s.manifest(&manifest);
        }

        let futures: Vec<_> = lenses
            .iter()
            .zip(cells.iter())
            .map(|(lens, (cell, prompt))| {
                let cell_id = cell.clone();
                async move {
                    let res = run_one_lens(provider, prompt, lens, opts).await;
                    // R2.1 修复：on_cell 在 future 内部即时调用（见上一分支同款注释）。
                    if let Some(s) = sink {
                        match &res {
                            Ok((model, fs, _)) => {
                                s.on_cell(CellRecord::ok(cell_id.clone(), model.clone(), fs.clone()));
                            }
                            Err(warn) => {
                                s.on_cell(CellRecord {
                                    cell: cell_id.clone(),
                                    model: String::new(),
                                    ok: false,
                                    findings: vec![],
                                    warn: Some(warn.clone()),
                                });
                            }
                        }
                    }
                    res
                }
            })
            .collect();

        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for res in results {
            match res {
                Ok((_model, fs, (it, ot))) => {
                    out.findings.extend(fs);
                    out.usage.0 += it;
                    out.usage.1 += ot;
                }
                Err(warn) => {
                    // 默认路径：整条候补链失败，只记 warning，不写 failed_models
                    out.warnings.push(warn);
                }
            }
        }
        out
    }
}

/// 候补链场景 manifest 的 model 快照：取最高 priority 候选（无固定实际值，见 run_fanout 注释）。
fn top_priority_model(opts: &OrchestrateOpts) -> String {
    opts.models
        .iter()
        .max_by_key(|m| m.priority)
        .map(|m| m.name.clone())
        .unwrap_or_default()
}

/// 单个文件包的评审（候补链复用 run_one_lens 的语义）。
async fn run_one_bundle(
    provider: &dyn ReviewProvider,
    label: &str,
    prompt: &str,
    opts: &OrchestrateOpts,
) -> Result<(String, Vec<Finding>, (u64, u64)), String> {
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
    for pm in &chain {
        match call_with_retry(provider, &pm.name, prompt, opts.retries).await {
            Ok(call) => {
                let fs = call
                    .findings
                    .iter()
                    .map(|r| enrich(r, "code", &pm.name))
                    .collect();
                return Ok((pm.name.clone(), fs, (call.input_tokens, call.output_tokens)));
            }
            Err(e) => errs.push(format!("bundle [{label}] model {}: {e}", pm.name)),
        }
    }
    Err(errs.join("; "))
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
    opts: &OrchestrateOpts,
    sink: Option<&dyn CellSink>,
) -> RawResults {
    let sem = Arc::new(Semaphore::new(opts.max_concurrency.max(1)));
    // 每个 bundle 的 label/prompt 只算一次：manifest 与执行复用同一份。
    let cells: Vec<(String, String, String)> = bundles
        .iter()
        .map(|bundle| {
            let label = bundle.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(",");
            let prompt = build_bundle_prompt(target_label, bundle, rules, focus);
            let cell_id = format!("bundle[{label}]");
            (cell_id, label, prompt)
        })
        .collect();

    if opts.replicate {
        // 全复制：bundle × model 笛卡尔积，每格只试该 model（不候补）
        let manifest: Vec<CellInput> = cells
            .iter()
            .flat_map(|(cell, _label, prompt)| {
                opts.models.iter().map(move |pm| CellInput {
                    cell: cell.clone(),
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
                let cell_id = cell.clone();
                let single = OrchestrateOpts {
                    models: vec![pm.clone()],
                    replicate: true,
                    no_fallback: opts.no_fallback,
                    max_concurrency: opts.max_concurrency,
                    retries: opts.retries,
                };
                async move {
                    let _permit = sem.acquire().await.expect("semaphore not closed");
                    let res = run_one_bundle(provider, label, prompt, &single).await;
                    // R2.1 修复：on_cell 在 future 内部即时调用（见 run_fanout 同款注释）。
                    if let Some(s) = sink {
                        match &res {
                            Ok((_m, fs, _)) => {
                                s.on_cell(CellRecord::ok(cell_id.clone(), model_name.clone(), fs.clone()));
                            }
                            Err(warn) => {
                                s.on_cell(CellRecord {
                                    cell: cell_id.clone(),
                                    model: model_name.clone(),
                                    ok: false,
                                    findings: vec![],
                                    warn: Some(warn.clone()),
                                });
                            }
                        }
                    }
                    (model_name, res)
                }
            })
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (model_hint, res) in results {
            match res {
                Ok((_m, fs, (it, ot))) => {
                    out.findings.extend(fs);
                    out.usage.0 += it;
                    out.usage.1 += ot;
                }
                Err(warn) => {
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
                let res = run_one_bundle(provider, label, prompt, opts).await;
                // R2.1 修复：on_cell 在 future 内部即时调用（见 run_fanout 同款注释）。
                if let Some(s) = sink {
                    match &res {
                        Ok((model, fs, _)) => {
                            s.on_cell(CellRecord::ok(cell_id.clone(), model.clone(), fs.clone()));
                        }
                        Err(warn) => {
                            s.on_cell(CellRecord {
                                cell: cell_id.clone(),
                                model: String::new(),
                                ok: false,
                                findings: vec![],
                                warn: Some(warn.clone()),
                            });
                        }
                    }
                }
                res
            }
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for res in results {
            match res {
                Ok((_model, fs, (it, ot))) => {
                    out.findings.extend(fs);
                    out.usage.0 += it;
                    out.usage.1 += ot;
                }
                Err(warn) => out.warnings.push(warn),
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
) -> Result<(String, Vec<Finding>, (u64, u64)), String> {
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
    for pm in &chain {
        match call_with_retry(provider, &pm.name, prompt, opts.retries).await {
            Ok(call) => {
                let fs = call.findings.iter().map(|r| enrich(r, "doc", &pm.name)).collect();
                return Ok((pm.name.clone(), fs, (call.input_tokens, call.output_tokens)));
            }
            Err(e) => errs.push(format!("doc model {}: {e}", pm.name)),
        }
    }
    Err(errs.join("; "))
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
        let manifest: Vec<CellInput> = opts
            .models
            .iter()
            .map(|pm| CellInput {
                cell: cell_id.clone(),
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
            let cell_id = cell_id.clone();
            let prompt = &prompt;
            let single = OrchestrateOpts {
                models: vec![pm.clone()],
                replicate: true,
                no_fallback: opts.no_fallback,
                max_concurrency: opts.max_concurrency,
                retries: opts.retries,
            };
            async move {
                let _permit = sem.acquire().await.expect("semaphore not closed");
                let res = run_one_doc(provider, prompt, &single).await;
                // R2.1 修复：on_cell 在 future 内部即时调用（见 run_fanout 同款注释）。
                if let Some(s) = sink {
                    match &res {
                        Ok((_m, fs, _)) => {
                            s.on_cell(CellRecord::ok(cell_id.clone(), model_name.clone(), fs.clone()));
                        }
                        Err(warn) => {
                            s.on_cell(CellRecord {
                                cell: cell_id.clone(),
                                model: model_name.clone(),
                                ok: false,
                                findings: vec![],
                                warn: Some(warn.clone()),
                            });
                        }
                    }
                }
                (model_name, res)
            }
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (model_hint, res) in results {
            match res {
                Ok((_m, fs, (it, ot))) => {
                    out.findings.extend(fs);
                    out.usage.0 += it;
                    out.usage.1 += ot;
                }
                Err(warn) => {
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
        match run_one_doc(provider, &prompt, opts).await {
            Ok((model, fs, (it, ot))) => {
                if let Some(s) = sink {
                    s.on_cell(CellRecord::ok(cell_id, model.clone(), fs.clone()));
                }
                out.findings.extend(fs);
                out.usage.0 += it;
                out.usage.1 += ot;
            }
            Err(warn) => {
                if let Some(s) = sink {
                    s.on_cell(CellRecord {
                        cell: cell_id,
                        model: String::new(),
                        ok: false,
                        findings: vec![],
                        warn: Some(warn.clone()),
                    });
                }
                out.warnings.push(warn)
            }
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

    fn input() -> ReviewInput {
        ReviewInput {
            mode: Mode::Code,
            target_label: "t".into(),
            collection_guidance: "".into(),
            body: "d".into(),
            spec_docs: vec![],
        }
    }

    fn raw(t: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High,
            title: t.into(),
            body: "b".into(),
            file: "a.rs".into(),
            line_start: 1,
            line_end: 1,
            confidence: 0.9,
            recommendation: "r".into(),
            hard_constraint: false,
            quote: "let x = 1;".into(),
            lens: None,
        }
    }

    #[tokio::test]
    async fn default_path_falls_back_to_next_priority() {
        // claude 不可用（无脚本），gpt 可用且 priority 更低 → 候补命中 gpt
        let p = FakeProvider::with("gpt", vec![raw("found-by-gpt")]);
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "claude".into(), priority: 30 },
                PlannedModel { name: "gpt".into(), priority: 20 },
            ],
            replicate: false,
            no_fallback: false,
            max_concurrency: 4,
            retries: 0,
        };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts, None).await;
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].models, vec!["gpt".to_string()]);
    }

    #[tokio::test]
    async fn explicit_models_record_failure_no_fallback() {
        // 显式指定 claude（不可用）→ 不候补，记 warning + failed_models
        let p = FakeProvider::with("gpt", vec![raw("x")]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "claude".into(), priority: 30 }],
            replicate: true,
            no_fallback: false,
            max_concurrency: 4,
            retries: 0,
        };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts, None).await;
        assert!(r.findings.is_empty());
        assert!(r.failed_models.contains(&"claude".to_string()));
        assert!(!r.warnings.is_empty());
    }

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
            replicate: true, no_fallback: false, max_concurrency: 4, retries: 0,
        };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs"), bundle("src/c.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let r = run_code_fanout(&p, "wt", &bundles, &rules, "", &opts, None).await;
        assert_eq!(r.findings.len(), 3, "one finding per bundle");
        // raw.lens=Some("L1") → enrich 用 L1（非 fallback）
        assert!(r.findings.iter().all(|f| f.lenses == vec!["L1".to_string()]));
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
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0 })
        }
    }

    #[tokio::test]
    async fn code_fanout_respects_max_concurrency() {
        let probe = Arc::new(ConcProbe { inflight: AtomicUsize::new(0), max_seen: AtomicUsize::new(0) });
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10 }],
            replicate: true, no_fallback: false, max_concurrency: 2, retries: 0,
        };
        let bundles: Vec<Bundle> = (0..6).map(|i| bundle(&format!("src/f{i}.rs"))).collect();
        let rules = vec![lens_by_id("L1").unwrap()];
        let probe_ref: &ConcProbe = &probe;
        let _ = run_code_fanout(probe_ref, "wt", &bundles, &rules, "", &opts, None).await;
        assert!(probe.max_seen.load(Ordering::SeqCst) <= 2, "concurrency exceeded cap: {}", probe.max_seen.load(Ordering::SeqCst));
        assert!(probe.max_seen.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn code_fanout_explicit_unreachable_model_records_failed() {
        // 显式模型完全不可用 → 整链失败 → failed_models 记该模型（守 exit-2）
        let p = FakeProvider::with("present", vec![]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "absent".into(), priority: 10 }],
            replicate: true, no_fallback: false, max_concurrency: 4, retries: 0,
        };
        let bundles = vec![bundle("src/a.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let r = run_code_fanout(&p, "wt", &bundles, &rules, "", &opts, None).await;
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
        let provider = crate::provider::fake::FakeProvider { scripted };
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
            retries: 0,
        };
        let out = run_code_fanout(&provider, "working tree", &bundles, &rules, "", &opts, None).await;
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
            retries: 0,
        };
        let out = run_code_fanout(&provider, "working tree", &bundles, &rules, "", &opts, None).await;
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
        let provider = crate::provider::fake::FakeProvider { scripted };
        let rules = vec![by_id("D1").unwrap(), by_id("D2").unwrap()];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true, no_fallback: false, max_concurrency: 8, retries: 0,
        };
        let out = run_doc_fanout(&provider, &doc_input(), &rules, "", &opts, None).await;
        assert_eq!(out.findings.len(), 2, "one finding per model over the single doc");
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
            replicate: true, no_fallback: false, max_concurrency: 8, retries: 0,
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
            replicate: false, no_fallback: false, max_concurrency: 4, retries: 0,
        };
        let out = run_doc_fanout(&provider, &doc_input(), &rules, "", &opts, None).await;
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].models, vec!["gpt".to_string()]);
        assert!(out.failed_models.is_empty());
    }

    #[tokio::test]
    async fn default_path_full_chain_failure_no_failed_models() {
        // 回归测试（Finding #1 / Minor #3）：
        // 默认路径，opts.models 顺序与 priority 不一致（gpt priority=20 排前，claude priority=30 排后）
        // provider 对两个 model 都无脚本 → 整条候补链失败
        // 期望：failed_models 为空，warnings 非空且包含两个模型的失败信息
        let p = FakeProvider::with("nobody", vec![raw("unreachable")]);
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "gpt".into(), priority: 20 },
                PlannedModel { name: "claude".into(), priority: 30 },
            ],
            replicate: false,
            no_fallback: false,
            max_concurrency: 4,
            retries: 0,
        };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts, None).await;
        assert!(r.findings.is_empty(), "default path full-chain failure must produce no findings");
        assert!(r.failed_models.is_empty(), "default path full-chain failure must not write failed_models");
        assert!(!r.warnings.is_empty(), "default path full-chain failure must produce a warning");
        // warning should contain failure info for both models (accumulated)
        let warn = &r.warnings[0];
        assert!(warn.contains("claude"), "warning must contain claude failure: {warn}");
        assert!(warn.contains("gpt"), "warning must contain gpt failure: {warn}");
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
            replicate: true, no_fallback: false, max_concurrency: 4, retries: 0,
        };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs"), bundle("src/c.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let sink = RecSink { manifest_n: 0.into(), records: Vec::new().into() };
        let _ = run_code_fanout(&p, "wt", &bundles, &rules, "", &opts, Some(&sink)).await;
        assert_eq!(*sink.manifest_n.lock().unwrap(), 3, "manifest 全集=3 cell");
        assert_eq!(sink.records.lock().unwrap().len(), 3, "每 cell 各回调一次");
    }

    // T4 回归：候补链（replicate=false）+ provider 成功但零 findings
    // → CellRecord.model 必须是实际命中的候选模型名，不能退化为空串。
    #[tokio::test]
    async fn fallback_cell_records_hit_model_even_with_zero_findings() {
        // claude 无脚本（候补失败），gpt 有脚本但返回空 findings（成功、零发现）
        let p = FakeProvider::with("gpt", vec![]);
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "claude".into(), priority: 30 },
                PlannedModel { name: "gpt".into(), priority: 20 },
            ],
            replicate: false,
            no_fallback: false,
            max_concurrency: 4,
            retries: 0,
        };
        let sink = RecSink { manifest_n: 0.into(), records: Vec::new().into() };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts, Some(&sink)).await;
        assert!(r.findings.is_empty(), "gpt 命中但零 findings");
        let recs = sink.records.lock().unwrap();
        assert_eq!(recs.len(), 1);
        assert!(recs[0].ok, "cell 应标记成功");
        assert_eq!(recs[0].model, "gpt", "model 必须是实际命中的候选，不能是空串");
    }

    // T5 Important 回归：checkpoint 现在是流式的——on_cell 必须在每个 cell 自己的 future
    // 内部即时触发，而不是等 join_all 收齐全部结果后才批量回调（旧实现下进程中途被杀，
    // 已完成的 cell 也会因为还没来得及回调而在 partial.ndjson 里全部丢失）。
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
            if req.prompt.contains("slow.rs") {
                tokio::time::sleep(std::time::Duration::from_millis(60)).await;
                self.slow_done.store(true, Ordering::SeqCst);
            }
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0 })
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
            replicate: true, no_fallback: false, max_concurrency: 4, retries: 0,
        };
        let bundles = vec![bundle("src/slow.rs"), bundle("src/fast.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let sink = StreamingProbeSink {
            slow_done: &provider.slow_done,
            fast_saw_slow_incomplete: None.into(),
            calls: AtomicUsize::new(0),
        };
        let _ = run_code_fanout(&provider, "wt", &bundles, &rules, "", &opts, Some(&sink)).await;
        assert_eq!(sink.calls.load(Ordering::SeqCst), 2, "on_cell 调用次数=cell 数（聚合前逐个触发）");
        let saw = *sink.fast_saw_slow_incomplete.lock().unwrap();
        assert_eq!(
            saw,
            Some(true),
            "快 cell 的 on_cell 必须在慢 cell 完成前触发——证明每 cell 完成即落盘（流式 checkpoint）"
        );
    }
}
