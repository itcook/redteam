use crate::domain::{Finding, RawFinding, ReviewInput};
use crate::hash::fnv1a64_hex;
use crate::lens::Lens;
use crate::prompt::build_prompt;
use crate::provider::{LensRequest, ProviderError, ReviewProvider};
use std::cmp::Reverse;

#[derive(Clone, Debug)]
pub struct PlannedModel {
    pub name: String,
    pub priority: i32,
}

#[derive(Clone, Debug)]
pub struct OrchestrateOpts {
    pub models: Vec<PlannedModel>,
    pub explicit: bool,
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

pub fn enrich(raw: &RawFinding, lens_id: &str, model: &str) -> Finding {
    let id = fnv1a64_hex(&[
        &raw.file,
        &raw.line_start.to_string(),
        &raw.line_end.to_string(),
        &raw.title.to_lowercase(),
    ]);
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
        lenses: vec![lens_id.to_string()],
        models: vec![model.to_string()],
        agreement: 1,
        hard_constraint: raw.hard_constraint,
        cluster_id: None,
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
    input: &ReviewInput,
    lens: &Lens,
    focus: &str,
    opts: &OrchestrateOpts,
) -> Result<(Vec<Finding>, (u64, u64)), String> {
    let prompt = build_prompt(input, lens, focus);

    // 默认路径：按 priority 降序排列，逐个候补
    let mut chain: Vec<&PlannedModel> = opts.models.iter().collect();
    if !opts.explicit {
        chain.sort_by_key(|m| Reverse(m.priority));
    }

    // 显式或 no_fallback 时只取第一个
    let chain: Vec<&PlannedModel> = if opts.explicit || opts.no_fallback {
        chain.into_iter().take(1).collect()
    } else {
        chain
    };

    let mut errs: Vec<String> = Vec::new();

    for pm in &chain {
        match call_with_retry(provider, &pm.name, &prompt, opts.retries).await {
            Ok(call) => {
                let fs = call
                    .findings
                    .iter()
                    .map(|r| enrich(r, lens.id, &pm.name))
                    .collect();
                return Ok((fs, (call.input_tokens, call.output_tokens)));
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
) -> RawResults {
    // 收集所有任务的 future，然后用 join_all 并发执行。
    // 由于 provider、input、focus、opts 都是借用且生命周期覆盖所有 future，
    // 不需要 Arc——直接在 async block 中使用引用即可。
    //
    // 每个 future 的类型：
    //   impl Future<Output = (/*model_hint*/ String, Result<(Vec<Finding>, (u64,u64)), String>)>

    if opts.explicit {
        // 显式：每个 (lens, model) 独立格子，失败记 warning + failed_models
        let futures: Vec<_> = lenses
            .iter()
            .flat_map(|lens| {
                opts.models.iter().map(move |pm| {
                    let model_name = pm.name.clone();
                    let single_opts = OrchestrateOpts {
                        models: vec![pm.clone()],
                        explicit: true,
                        no_fallback: opts.no_fallback,
                        max_concurrency: opts.max_concurrency,
                        retries: opts.retries,
                    };
                    async move {
                        let res =
                            run_one_lens(provider, input, lens, focus, &single_opts).await;
                        (model_name, res)
                    }
                })
            })
            .collect();

        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (model_hint, res) in results {
            match res {
                Ok((fs, (it, ot))) => {
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
        let futures: Vec<_> = lenses
            .iter()
            .map(|lens| async move {
                run_one_lens(provider, input, lens, focus, opts).await
            })
            .collect();

        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for res in results {
            match res {
                Ok((fs, (it, ot))) => {
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
            explicit: false,
            no_fallback: false,
            max_concurrency: 4,
            retries: 0,
        };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts).await;
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].models, vec!["gpt".to_string()]);
    }

    #[tokio::test]
    async fn explicit_models_record_failure_no_fallback() {
        // 显式指定 claude（不可用）→ 不候补，记 warning + failed_models
        let p = FakeProvider::with("gpt", vec![raw("x")]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "claude".into(), priority: 30 }],
            explicit: true,
            no_fallback: false,
            max_concurrency: 4,
            retries: 0,
        };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts).await;
        assert!(r.findings.is_empty());
        assert!(r.failed_models.contains(&"claude".to_string()));
        assert!(!r.warnings.is_empty());
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
            explicit: false,
            no_fallback: false,
            max_concurrency: 4,
            retries: 0,
        };
        let r = run_fanout(&p, &input(), &[by_id("L1").unwrap()], "", &opts).await;
        assert!(r.findings.is_empty(), "default path full-chain failure must produce no findings");
        assert!(r.failed_models.is_empty(), "default path full-chain failure must not write failed_models");
        assert!(!r.warnings.is_empty(), "default path full-chain failure must produce a warning");
        // warning should contain failure info for both models (accumulated)
        let warn = &r.warnings[0];
        assert!(warn.contains("claude"), "warning must contain claude failure: {warn}");
        assert!(warn.contains("gpt"), "warning must contain gpt failure: {warn}");
    }
}
