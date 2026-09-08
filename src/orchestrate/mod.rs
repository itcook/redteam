use crate::bundle::Bundle;
use crate::domain::{Finding, NeedEntry, Origin, RawFinding};
use crate::hash::fnv1a64_hex;
use crate::prompt::build_bundle_prompt;
use crate::provider::{LensRequest, ReviewProvider};
#[cfg(test)]
use crate::provider::ProviderError;
use crate::state::{CellInput, CellRecord};
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
    fn on_phase(&self, _phase: crate::state::Phase, _note: Option<String>) {}
    /// resolve 完成即报 models（后台 job stamp 进 state，running 期 status 可见；
    /// 此前 done 时才从 report 回填——r1 20260703 实证运行全程空数组）。默认 no-op。
    fn on_meta(&self, _models: &[String]) {}
}

#[derive(Clone, Debug)]
pub struct PlannedModel {
    pub name: String,
    pub priority: i32,
    /// 模型家族（角色条目 `EffectiveModel.family`），随 cell 记录落库；测试 / 未知为空串。
    pub family: String,
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
    /// 二阶段补查触发观测（design §2.3，E8 裁决 E）：(触发格数, needs 总条数)——
    /// 第一趟声明的 needs（cap 前），模板未列 needs 的旧模型输出恒 (0, 0)。
    pub needs_stats: (u32, u32),
}

/// 二阶段补查 prompt 构造器（design §2.3）：`(cell_idx, cap 后的 needs, 第一趟 findings) ->
/// (Some(第二趟 prompt) 或 None＝本格不补跑, 拒读 warnings)`。第二元恒返回（T46①：needs 读守卫
/// 拒读事件即便最终判定不补跑——evidence 为空——也须把拒读理由带出去落 `run.warnings`，不能
/// 随 None 分支一起被默默吞掉）。doc fanout 单 cell，`cell_idx` 恒 0。
pub type SecondPrompt<'a> =
    Option<&'a (dyn Fn(usize, &[NeedEntry], &[Finding]) -> (Option<String>, Vec<String>) + Sync)>;

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

pub fn enrich(raw: &RawFinding, model: &str) -> Finding {
    let id = finding_id(&raw.file, &raw.quote, &raw.title, raw.line_start, raw.line_end);
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
    family: String, // 胜出 model 的家族（PlannedModel.family）；整链失败为空
    findings: Vec<Finding>,
    tokens: (u64, u64, u64), // (input, output, cached_input)
    duration_ms: u64,
    attempts: u32,
    /// 胜出 call 的 tool-loop 迭代数（失败候补烧掉的轮数不可观测——Err 无 LensCall）；
    /// 整链失败记 0＝未观测。
    iterations: u32,
    /// 胜出 call 的工具调用构成（延迟诊断仪器）；整链失败/单发路径为零值。
    tool_use: crate::tools::ToolUse,
    /// 胜出 call 声明的证据缺口（design §2.3）；整链失败/二阶段已消化后为空。
    needs: Vec<NeedEntry>,
    warn: Option<String>,
}

impl CellRun {
    /// 转持久化记录；`fallback_model`：胜出者为空（整链失败）时的 model 标注
    /// （replicate 格=该格 model；候补链=空串，warn 已含逐家原因）。
    fn to_record(&self, cell_id: String, fallback_model: &str) -> CellRecord {
        let model = if self.model.is_empty() { fallback_model.to_string() } else { self.model.clone() };
        // 旧 finding 路径（F-4 删）落为 reviewer 形态输出：findings + 剩余 needs 声明。统一走构造函数，
        // 角色只来自 output tag（不手填）。
        let mut rec = CellRecord::ok(
            cell_id, model,
            crate::domain::CellOutput::Review { findings: self.findings.clone(), needs: self.needs.clone() },
        ).with_family(self.family.clone());
        rec.ok = self.warn.is_none();
        rec.warn = self.warn.clone();
        rec.duration_ms = self.duration_ms;
        rec.input_tokens = self.tokens.0;
        rec.output_tokens = self.tokens.1;
        rec.cached_input_tokens = self.tokens.2;
        rec.attempts = self.attempts;
        rec.iterations = self.iterations;
        rec.tool_use = self.tool_use.clone();
        rec
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
    // T25：候补链逐个失败时，每次失败携带的已烧 usage（`ProviderError::attach_usage` 挂的）
    // 累加进这里——链条全灭时不再谎报 (0,0,0)，真实入账各次尝试烧掉的 token。
    let mut burned = (0u64, 0u64, 0u64);
    for (i, pm) in chain.iter().enumerate() {
        match provider.run(LensRequest { route_key: pm.name.clone(), prompt: prompt.to_string() }).await {
            Ok(call) => {
                let fs = call
                    .findings
                    .iter()
                    .map(|r| enrich(r, &pm.name))
                    .collect();
                return CellRun {
                    model: pm.name.clone(), family: pm.family.clone(), findings: fs,
                    tokens: (call.input_tokens, call.output_tokens, call.cached_input_tokens),
                    duration_ms: t0.elapsed().as_millis() as u64,
                    attempts: i as u32 + 1, iterations: call.iterations, tool_use: call.tool_use,
                    needs: call.needs, warn: None,
                };
            }
            Err(e) => {
                let u = e.burned_usage();
                burned = (burned.0 + u.0, burned.1 + u.1, burned.2 + u.2);
                errs.push(format!("bundle [{label}] model {}: {e}", pm.name));
            }
        }
    }
    CellRun {
        model: String::new(), family: String::new(), findings: vec![], tokens: burned,
        duration_ms: t0.elapsed().as_millis() as u64,
        attempts: chain.len() as u32, iterations: 0, tool_use: Default::default(),
        needs: vec![], warn: Some(errs.join("; ")),
    }
}

/// 二阶段补查（design §2.3）：第一趟 needs 非空 → cap 5（超出 truncate + warning）→
/// 构造器给第二趟 prompt → `rerun` 闭包重新执行一次（调用方传入自己的执行体，
/// 与 run_one_bundle 的 cap/替换逻辑解耦复用）。
/// 第二趟成功＝整体替换（tokens/duration 累加、iterations 求和、attempts 取 max、needs 清空）；
/// 失败＝保留第一趟全部产出 + warning。恒 ≤2 趟/格。
/// 纯值返回（cell future 并发展开，彼此无法共享 `&mut` 状态）——warnings/stats 随返回值带出，
/// 由调用方在 join 后的汇总循环里累加（照 usage 累加的现有模式）。
async fn maybe_second_pass<Fut>(
    mk: SecondPrompt<'_>,
    idx: usize,
    mut run: CellRun,
    warn_label: &str,
    rerun: impl FnOnce(String) -> Fut,
) -> (CellRun, Vec<String>, (u32, u32))
where
    Fut: std::future::Future<Output = CellRun>,
{
    let mut warnings = Vec::new();
    let mut stats = (0u32, 0u32);
    let Some(mk) = mk else { return (run, warnings, stats) };
    if run.warn.is_some() || run.needs.is_empty() {
        return (run, warnings, stats);
    }
    stats.0 += 1;
    stats.1 += run.needs.len() as u32;
    if run.needs.len() > 5 {
        warnings.push(format!("cell {warn_label}: {} needs declared, capped at 5", run.needs.len()));
        run.needs.truncate(5);
    }
    let (p2, deny_warnings) = mk(idx, &run.needs, &run.findings);
    warnings.extend(deny_warnings); // T46①：拒读 warnings 无条件带出（即便 p2 随后是 None）
    let Some(p2) = p2 else { return (run, warnings, stats) };
    let run2 = rerun(p2).await;
    match run2.warn {
        None => {
            let merged = CellRun {
                model: run2.model,
                family: run2.family,
                findings: run2.findings, // 整体替换，非合并
                tokens: (run.tokens.0 + run2.tokens.0, run.tokens.1 + run2.tokens.1, run.tokens.2 + run2.tokens.2),
                duration_ms: run.duration_ms + run2.duration_ms,
                attempts: run.attempts.max(run2.attempts),
                iterations: run.iterations + run2.iterations,
                tool_use: run.tool_use,
                needs: vec![],
                warn: None,
            };
            (merged, warnings, stats)
        }
        Some(w) => {
            warnings.push(format!("cell {warn_label} second pass failed: {w}"));
            run.needs.clear();
            (run, warnings, stats)
        }
    }
}

/// code 线扇出，用 Semaphore 限 max_concurrency：
/// - replicate=true：每 (bundle, model) 独立格子全跑（对抗复制），失败记 failed_models（守 exit-2）。
/// - replicate=false：每 bundle 一条候补链（容错，首个成功即止），整链失败只记 warning。
///
/// `spec_docs`：`--spec` 参照段（Task 2），原样透传给每个 bundle 的 prompt（`build_bundle_prompt`
/// 的 `{{SPEC_DOCS}}` 槽，供 `[L0] Spec Consistency` 实现-对照-设计）；空串渲染为占位文案。
/// `others`：每组一份的跨组索引串（`bundle::render_cross_indices` 预渲染，与 `dossiers` 等长等序），
/// 填 `{{OTHER_CHANGED_FILES}}` 槽；缺位按空串（单组占位）。
#[allow(clippy::too_many_arguments)] // 与 second_pass_prompt 同源的参数面；收拢 struct 是过度设计
pub async fn run_code_fanout(
    provider: &dyn ReviewProvider,
    target_label: &str,
    bundles: &[Bundle],
    focus: &str,
    incremental: &str,
    dossiers: &[String],
    opts: &OrchestrateOpts,
    sink: Option<&dyn CellSink>,
    second_prompt: SecondPrompt<'_>,
    spec_docs: &str,
    others: &[String],
) -> RawResults {
    let sem = Arc::new(Semaphore::new(opts.max_concurrency.max(1)));
    // 每个 bundle 的 label/prompt 只算一次：manifest 与执行复用同一份。
    let cells: Vec<(String, String, String)> = bundles
        .iter()
        .enumerate()
        .map(|(i, bundle)| {
            let label = bundle.label();
            let others = others.get(i).map(String::as_str).unwrap_or("");
            let dossier = dossiers.get(i).map(String::as_str).unwrap_or("");
            let prompt = build_bundle_prompt(target_label, bundle, focus, others, incremental, dossier, spec_docs);
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

        let futures = cells.iter().enumerate().flat_map(|(idx, (cell, label, prompt))| {
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
                    let warn_label = format!("bundle[{label}]");
                    let (run, p2_warnings, p2_stats) = maybe_second_pass(
                        second_prompt, idx, run, &warn_label,
                        |p2| async move { run_one_bundle(provider, label, &p2, &single).await },
                    ).await;
                    // R2.1 修复：on_cell 在 future 内部即时调用（二阶段结束后的最终结果，仅一次）。
                    if let Some(s) = sink {
                        s.on_cell(run.to_record(cell_id.clone(), &model_name));
                    }
                    (model_name, run, p2_warnings, p2_stats)
                }
            })
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (model_hint, run, p2_warnings, p2_stats) in results {
            out.usage.0 += run.tokens.0;
            out.usage.1 += run.tokens.1; out.usage.2 += run.tokens.2;
            out.warnings.extend(p2_warnings);
            out.needs_stats.0 += p2_stats.0; out.needs_stats.1 += p2_stats.1;
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

        let futures = cells.iter().enumerate().map(|(idx, (cell, label, prompt))| {
            let sem = sem.clone();
            let cell_id = cell.clone();
            async move {
                let _permit = sem.acquire().await.expect("semaphore not closed");
                let run = run_one_bundle(provider, label, prompt, opts).await;
                let warn_label = format!("bundle[{label}]");
                let (run, p2_warnings, p2_stats) = maybe_second_pass(
                    second_prompt, idx, run, &warn_label,
                    |p2| async move { run_one_bundle(provider, label, &p2, opts).await },
                ).await;
                // R2.1 修复：on_cell 在 future 内部即时调用（二阶段结束后的最终结果，仅一次）。
                if let Some(s) = sink {
                    s.on_cell(run.to_record(cell_id.clone(), ""));
                }
                (run, p2_warnings, p2_stats)
            }
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (run, p2_warnings, p2_stats) in results {
            out.usage.0 += run.tokens.0;
            out.usage.1 += run.tokens.1; out.usage.2 += run.tokens.2;
            out.warnings.extend(p2_warnings);
            out.needs_stats.0 += p2_stats.0; out.needs_stats.1 += p2_stats.1;
            match run.warn {
                None => out.findings.extend(run.findings),
                Some(warn) => out.warnings.push(warn),
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    use crate::provider::fake::FakeProvider;
    use crate::domain::RawFinding;

    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn raw_q(t: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: t.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(),
        }
    }

    fn raw_at(file: &str, title: &str, sev: Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(),
        }
    }

    fn bundle(path: &str) -> Bundle {
        Bundle::from_files(vec![FileChange { path: path.into(), diff: "+x".into() }])
    }

    // ---- 二阶段补查测试助手（design §2.3）----

    /// brief 里称 `rawf`——等价 raw_q，起个短名跟 brief 测试代码对齐。
    fn rawf(t: &str) -> RawFinding {
        raw_q(t)
    }

    fn need(path: &str, reason: &str) -> NeedEntry {
        NeedEntry { path: path.into(), lines: None, reason: reason.into(), kind: crate::domain::NeedKind::Evidence }
    }

    fn lens_call_with(findings: Vec<RawFinding>, needs: Vec<NeedEntry>) -> crate::provider::LensCall {
        crate::provider::LensCall {
            findings, input_tokens: 0, output_tokens: 0, cached_input_tokens: 0,
            iterations: 1, tool_use: Default::default(), needs,
        }
    }

    fn bundles_one() -> Vec<Bundle> {
        vec![bundle("src/x.rs")]
    }

    fn dossiers_one() -> Vec<String> {
        vec![String::new()]
    }

    fn opts_single() -> OrchestrateOpts {
        OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10, family: String::new() }],
            replicate: false, no_fallback: false, max_concurrency: 4,
        }
    }

    /// 序列 provider：每次 `run` 弹出队首一个预设 LensCall（不看 model/prompt）；
    /// 队列耗尽 → Err。用于测二阶段「同 model 两次调用返回不同结果」（现有 FakeProvider
    /// 按 model 建表，同 model 只能回同一份脚本，撑不住序列语义）。
    struct SeqProvider {
        calls: std::sync::Mutex<std::collections::VecDeque<crate::provider::LensCall>>,
    }
    impl SeqProvider {
        fn new(calls: Vec<crate::provider::LensCall>) -> Self {
            SeqProvider { calls: std::sync::Mutex::new(calls.into_iter().collect()) }
        }
    }
    #[async_trait::async_trait]
    impl ReviewProvider for SeqProvider {
        async fn run(&self, _req: LensRequest) -> Result<crate::provider::LensCall, ProviderError> {
            self.calls
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| ProviderError::Unavailable("SeqProvider exhausted".into()))
        }
    }

    #[tokio::test]
    async fn code_fanout_second_pass_replaces_first_on_needs() {
        // 第一趟：1 finding + 1 need；第二趟：2 findings 无 needs → 最终 2 findings（替换非合并）
        let first = lens_call_with(vec![rawf("draft finding")], vec![need("src/x.rs", "verify")]);
        let second = lens_call_with(vec![rawf("confirmed A"), rawf("confirmed B")], vec![]);
        let provider = SeqProvider::new(vec![first, second]);
        let mk = |_idx: usize, needs: &[NeedEntry], drafts: &[Finding]| {
            assert_eq!(needs.len(), 1);
            assert_eq!(drafts.len(), 1, "草稿传入构造器");
            (Some("second pass prompt".to_string()), vec![])
        };
        let out = run_code_fanout(&provider, "t", &bundles_one(), "", "", &dossiers_one(), &opts_single(), None, Some(&mk), "", &[]).await;
        assert_eq!(out.findings.len(), 2, "第二趟整体替换第一趟");
        assert_eq!(out.needs_stats, (1, 1));
    }

    #[tokio::test]
    async fn code_fanout_second_pass_failure_keeps_first() {
        let first = lens_call_with(vec![rawf("draft finding")], vec![need("src/x.rs", "verify")]);
        let provider = SeqProvider::new(vec![first]); // 第二趟无 script → Err
        let mk = |_i: usize, _n: &[NeedEntry], _d: &[Finding]| (Some("p2".to_string()), vec![]);
        let out = run_code_fanout(&provider, "t", &bundles_one(), "", "", &dossiers_one(), &opts_single(), None, Some(&mk), "", &[]).await;
        assert_eq!(out.findings.len(), 1, "第二趟失败保留第一趟");
        assert!(out.warnings.iter().any(|w| w.contains("second pass failed")));
    }

    #[tokio::test]
    async fn code_fanout_needs_capped_at_five() {
        let six: Vec<NeedEntry> = (0..6).map(|i| need(&format!("src/f{i}.rs"), "r")).collect();
        let first = lens_call_with(vec![], six);
        let second = lens_call_with(vec![], vec![]);
        let provider = SeqProvider::new(vec![first, second]);
        let mk = |_i: usize, n: &[NeedEntry], _d: &[Finding]| { assert_eq!(n.len(), 5, "cap 5"); (Some("p2".into()), vec![]) };
        let out = run_code_fanout(&provider, "t", &bundles_one(), "", "", &dossiers_one(), &opts_single(), None, Some(&mk), "", &[]).await;
        assert!(out.warnings.iter().any(|w| w.contains("capped at 5")));
    }

    #[tokio::test]
    async fn code_fanout_runs_every_bundle() {
        let p = FakeProvider::with("m", vec![raw_q("found")]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10, family: String::new() }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs"), bundle("src/c.rs")];
        let r = run_code_fanout(&p, "wt", &bundles, "", "", &[], &opts, None, None, "", &[]).await;
        assert_eq!(r.findings.len(), 3, "one finding per bundle");
    }

    // prompt 记录探针：断言每个 bundle 收到自己（而非别的 bundle）的卷宗段。
    struct PromptProbe {
        prompts: std::sync::Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl ReviewProvider for PromptProbe {
        async fn run(&self, req: LensRequest) -> Result<crate::provider::LensCall, ProviderError> {
            self.prompts.lock().unwrap().push(req.prompt);
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, iterations: 1, tool_use: Default::default(), needs: vec![] })
        }
    }

    #[tokio::test]
    async fn code_fanout_threads_per_bundle_dossier() {
        let p = PromptProbe { prompts: std::sync::Mutex::new(vec![]) };
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10, family: String::new() }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs")];
        let dossiers = vec![
            "<dossier>\nDOSSIER-A\n</dossier>".to_string(),
            "<dossier>\nDOSSIER-B\n</dossier>".to_string(),
        ];
        let _ = run_code_fanout(&p, "wt", &bundles, "", "", &dossiers, &opts, None, None, "", &[]).await;
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
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, iterations: 1, tool_use: Default::default(), needs: vec![] })
        }
    }

    #[tokio::test]
    async fn code_fanout_respects_max_concurrency() {
        let probe = Arc::new(ConcProbe { inflight: AtomicUsize::new(0), max_seen: AtomicUsize::new(0) });
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10, family: String::new() }],
            replicate: true, no_fallback: false, max_concurrency: 2,
        };
        let bundles: Vec<Bundle> = (0..6).map(|i| bundle(&format!("src/f{i}.rs"))).collect();
        let probe_ref: &ConcProbe = &probe;
        let _ = run_code_fanout(probe_ref, "wt", &bundles, "", "", &[], &opts, None, None, "", &[]).await;
        assert!(probe.max_seen.load(Ordering::SeqCst) <= 2, "concurrency exceeded cap: {}", probe.max_seen.load(Ordering::SeqCst));
        assert!(probe.max_seen.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn code_fanout_explicit_unreachable_model_records_failed() {
        // 显式模型完全不可用 → 整链失败 → failed_models 记该模型（守 exit-2）
        let p = FakeProvider::with("present", vec![]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "absent".into(), priority: 10, family: String::new() }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs")];
        let r = run_code_fanout(&p, "wt", &bundles, "", "", &[], &opts, None, None, "", &[]).await;
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
            Bundle::from_files(vec![FileChange { path: "src/a.rs".into(), diff: "+x".into() }]),
            Bundle::from_files(vec![FileChange { path: "src/b.rs".into(), diff: "+y".into() }]),
        ];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10, family: String::new() },
                PlannedModel { name: "beta".into(), priority: 5, family: String::new() },
            ],
            replicate: true,
            no_fallback: false,
            max_concurrency: 8,
        };
        let out = run_code_fanout(&provider, "working tree", &bundles, "", "", &[], &opts, None, None, "", &[]).await;
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
        let bundles = vec![Bundle::from_files(vec![FileChange { path: "src/a.rs".into(), diff: "+x".into() }])];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10, family: String::new() },
                PlannedModel { name: "beta".into(), priority: 5, family: String::new() },
            ],
            replicate: true,
            no_fallback: false,
            max_concurrency: 8,
        };
        let out = run_code_fanout(&provider, "working tree", &bundles, "", "", &[], &opts, None, None, "", &[]).await;
        assert_eq!(out.findings.len(), 1); // alpha 成功
        assert!(out.failed_models.contains(&"beta".to_string()));
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
            models: vec![PlannedModel { name: "m".into(), priority: 10, family: String::new() }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs"), bundle("src/c.rs")];
        let sink = RecSink { manifest_n: 0.into(), records: Vec::new().into() };
        let _ = run_code_fanout(&p, "wt", &bundles, "", "", &[], &opts, Some(&sink), None, "", &[]).await;
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
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, iterations: 1, tool_use: Default::default(), needs: vec![] })
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
            models: vec![PlannedModel { name: "m".into(), priority: 10, family: String::new() }],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/slow.rs"), bundle("src/fast.rs")];
        let sink = StreamingProbeSink {
            slow_done: &provider.slow_done,
            fast_saw_slow_incomplete: None.into(),
            calls: AtomicUsize::new(0),
        };
        let _ = run_code_fanout(&provider, "wt", &bundles, "", "", &[], &opts, Some(&sink), None, "", &[]).await;
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
                PlannedModel { name: "good".into(), priority: 1, family: String::new() },
                PlannedModel { name: "bad".into(), priority: 1, family: String::new() },
            ],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs")];
        let out = run_code_fanout(&reg, "wt", &bundles, "", "", &[], &opts, None, None, "", &[]).await;
        assert!(out.failed_models.contains(&"bad".to_string()),
            "坏 model 应记 failed_models: {:?}", out.failed_models);
        assert!(out.findings.iter().any(|f| f.title == "real"),
            "好 model 的 finding 应仍在报告里");
    }

    /// T25 测试助手：按 route_key 查表返回一个携带非零 usage 的失败——模拟「本次尝试真烧了
    /// token，但最终仍报错」（如 tool_loop 内多轮后 FINALIZE 超时）。查表 miss 时兜底一个零
    /// usage 的 Unavailable（不影响本测试，仅防越界 panic）。
    struct FailingWithUsage {
        usage_by_model: std::collections::BTreeMap<String, (u64, u64, u64)>,
    }
    #[async_trait::async_trait]
    impl ReviewProvider for FailingWithUsage {
        async fn run(&self, req: LensRequest) -> Result<crate::provider::LensCall, ProviderError> {
            let usage = self.usage_by_model.get(&req.route_key).copied().unwrap_or((0, 0, 0));
            Err(ProviderError::Timeout(format!("dead: {}", req.route_key)).attach_usage(usage))
        }
    }

    // T25：候补链全灭时，各次失败尝试已烧的 usage 必须累计回填进失败 CellRecord——
    // 不能像修复前那样恒记 (0,0,0)（design 表 2-6）。
    #[tokio::test]
    async fn full_chain_failure_backfills_accumulated_usage() {
        let mut usage_by_model = std::collections::BTreeMap::new();
        usage_by_model.insert("first".to_string(), (100, 20, 5));
        usage_by_model.insert("second".to_string(), (50, 10, 0));
        let provider = FailingWithUsage { usage_by_model };

        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "first".into(), priority: 2, family: String::new() },
                PlannedModel { name: "second".into(), priority: 1, family: String::new() },
            ],
            replicate: false, no_fallback: false, max_concurrency: 4,
        };
        let run = run_one_bundle(&provider, "wt", "prompt", &opts).await;
        assert!(run.warn.is_some(), "候补链全灭必须落 warn");
        assert_eq!(run.tokens, (150, 30, 5), "两次失败尝试的 usage 应累加，而非恒零");

        let rec = run.to_record("cell-1".into(), "");
        assert!(rec.input_tokens > 0, "失败 CellRecord.input_tokens 必须非零: {}", rec.input_tokens);
        assert_eq!(rec.input_tokens, 150);
        assert_eq!(rec.output_tokens, 30);
        assert_eq!(rec.cached_input_tokens, 5);
        assert!(!rec.ok, "候补链全灭应记为失败");
    }

    // R8.1：quote 锚定、行无关的 finding id。
    // 注：命名为 raw_full（而非 raw）以避免与本模块已有的 `fn raw(t: &str)` 撞名——
    // Rust 无重载，两者签名不同无法共用一个名字。
    fn raw_full(file: &str, ls: u32, le: u32, title: &str, quote: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(),
            file: file.into(), line_start: ls, line_end: le, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false, quote: quote.into(),
        }
    }

    #[test]
    fn id_ignores_line_shift_when_quote_present() {
        let a = enrich(&raw_full("a.rs", 10, 12, "SQL injection", "let q = fmt!(sql);"), "m");
        let b = enrich(&raw_full("a.rs", 99, 101, "SQL injection", "let q = fmt!(sql);"), "m");
        assert_eq!(a.id, b.id, "行号位移不得改变 id");
    }

    #[test]
    fn id_ignores_whitespace_reformat() {
        let a = enrich(&raw_full("a.rs", 1, 2, "SQL injection", "let q =\n    fmt!( sql );"), "m");
        let b = enrich(&raw_full("a.rs", 1, 2, "SQL injection", "letq=fmt!(sql);"), "m");
        assert_eq!(a.id, b.id, "quote 去空白后等价 → 同 id");
    }

    #[test]
    fn id_title_normalization_case_and_ws() {
        let a = enrich(&raw_full("a.rs", 1, 1, "SQL  Injection", "x"), "m");
        let b = enrich(&raw_full("a.rs", 1, 1, "sql injection", "x"), "m");
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn id_composition_pinned() {
        // 组成 golden：钉死组成与归一（FNV 算法本身由 hash.rs golden 钉死）。
        let f = enrich(&raw_full("src/x.rs", 5, 7, "Weak  Hash", "md5( pw )"), "m");
        assert_eq!(f.id, crate::hash::fnv1a64_hex(&["src/x.rs", "md5(pw)", "weak hash"]));
    }

    #[test]
    fn id_falls_back_to_lines_without_quote() {
        let a = enrich(&raw_full("a.rs", 1, 2, "t", ""), "m");
        let b = enrich(&raw_full("a.rs", 3, 4, "t", ""), "m");
        assert_ne!(a.id, b.id, "无 quote 退化回行号路径");
        assert_eq!(a.id, crate::hash::fnv1a64_hex(&["a.rs", "1", "2", "t"]));
    }
}
