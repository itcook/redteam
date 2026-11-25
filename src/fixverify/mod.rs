//! Phase 4：hibot `verify` kind（fix 验证）。
//!
//! **流程隔离（4.0）**：本模块与 `consolidate::refute` 零共享——不 import 它、不复用 `refuter.md`、
//! 不复用它的解析与判定表。只借 provider 传输层（`timed_turn_final` / `ChatMessage`）与
//! `prompt::neutralize_structure`（计划 4.2 明确要求）。
//!
//! 判定模型（契约 §3.3 / Q7）：
//! - `resolved` / `unresolved` / `regressed`：都要有证据；`quote`（若有）必须逐字命中提供材料；
//! - `inconclusive`：模型失败 / 无法定位 / 证据不足——**不确认修好也不确认没修**，绝不能塞进
//!   `unresolved`；出现即要求报告 `status ∈ {partial,error}` + `status_reason`。
//!
//! 覆盖不变量（4.1 / 4.6）：输出指纹集合 == 输入指纹集合；输入集合为空时输出空数组合法。

pub mod budget;
pub mod evidence;
pub mod input;
pub mod prompt;

use crate::config::{Config, EffectiveModel, OutputLang};
use crate::job::state::Heartbeat;
use crate::job::status::JobFailure;
use crate::provider::registry::ProviderRegistry;
use crate::provider::tool_loop::ChatMessage;
use futures::StreamExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// 四态（hibot B6 新增 `inconclusive`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FixVerifyStatus {
    Resolved,
    Unresolved,
    Regressed,
    Inconclusive,
}

impl FixVerifyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            FixVerifyStatus::Resolved => "resolved",
            FixVerifyStatus::Unresolved => "unresolved",
            FixVerifyStatus::Regressed => "regressed",
            FixVerifyStatus::Inconclusive => "inconclusive",
        }
    }
    /// 解析模型输出（大小写不敏感；其余值一律拒绝）。
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "resolved" => Some(FixVerifyStatus::Resolved),
            "unresolved" => Some(FixVerifyStatus::Unresolved),
            "regressed" => Some(FixVerifyStatus::Regressed),
            "inconclusive" => Some(FixVerifyStatus::Inconclusive),
            _ => None,
        }
    }
    pub fn is_inconclusive(self) -> bool {
        self == FixVerifyStatus::Inconclusive
    }
}

/// 报告里的一条 `verification`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verification {
    pub finding_fingerprint: String,
    pub status: FixVerifyStatus,
    pub evidence: String,
    pub quote: Option<String>,
}

impl Verification {
    fn inconclusive(fingerprint: &str, evidence: String) -> Self {
        Self {
            finding_fingerprint: fingerprint.to_string(),
            status: FixVerifyStatus::Inconclusive,
            evidence,
            quote: None,
        }
    }
}

fn msg(lang: &OutputLang, zh_text: &str, en_text: &str) -> String {
    if lang.value.starts_with("zh") {
        zh_text.to_string()
    } else {
        en_text.to_string()
    }
}

/// verifier 池 → `(身份串, route key)`（顺序即轮转序；空池由调用方报 `missing_role:verifier`）。
pub fn verifier_routes(cfg: &Config) -> Vec<(String, String)> {
    cfg.active_roles()
        .verifiers
        .iter()
        .map(|em: &EffectiveModel| {
            let key = crate::provider::registry::role_key(crate::config::Role::Verifier, &em.name);
            (em.name.clone(), key)
        })
        .collect()
}

/// 一次 verify 的全部输入（repo 已由调用方完成完整性校验）。
pub struct FixVerifyRequest<'a> {
    pub repo: &'a Path,
    pub lang: &'a OutputLang,
    pub findings: &'a [input::InputFinding],
    pub fixes: &'a [input::FileFix],
    /// 上次评审报告里同指纹的佐证（可选）。
    pub prior: Option<&'a input::PriorEvidence>,
    /// verifier 轮转池：`(身份串, route key)`。
    pub routes: Vec<(String, String)>,
    /// 派发窗口 `min(Σ provider 帽, n)`：只是「同时活着的 future 数」，provider 门仍是硬上限。
    pub max_parallel: usize,
    /// manifest 墙钟（秒）：只用于规模预算的放大系数。
    pub wall_clock_s: u64,
    /// 绝对截止时刻（manifest 墙钟）。
    pub deadline: Instant,
    /// 输入里重复的指纹条数（报告披露用；集合语义只核一次）。
    pub duplicates: usize,
}

/// 取消后仍可收敛的已完成部分。
#[derive(Clone, Default)]
pub struct Progress {
    done: Vec<Verification>,
    usage: (u64, u64, u64),
    calls: u32,
    failures: u32,
    quote_rejected: u32,
    /// 逐条模型调用耗时（毫秒）：只记录，不设 KPI。
    latency_total_ms: u64,
    latency_max_ms: u64,
    /// provider 门 429 排队重试（`AttemptPolicy` 计数；与心跳 `llm.retrying` 同源）。
    rl_retries: u32,
    rl_wait_ms: u64,
    /// 证据降级原因（`symbol_blast_radius_skipped:*` 等），集合语义去重。
    warnings: std::collections::BTreeSet<String>,
}

/// 报告层需要的全部素材。
#[derive(Clone, Debug)]
pub struct Outcome {
    pub verifications: Vec<Verification>,
    pub usage: (u64, u64, u64),
    /// 降级原因（`partial`）。空 = `ok`。
    pub reasons: Vec<JobFailure>,
    pub calls: u32,
    pub failures: u32,
    pub quote_rejected: u32,
    pub duplicates: usize,
    /// `(总耗时, 单条最大耗时)` 毫秒：只记录（用户裁：不设墙钟 KPI）。
    pub latency: (u64, u64),
    /// `(429 排队重试次数, 等待毫秒)`：来自 provider 门的 `AttemptPolicy`。
    pub rate_limit: (u32, u64),
    /// 证据降级原因（去重排序）；报告披露用，不参与 status。
    pub warnings: Vec<String>,
    /// 首个 verifier 身份（`provider/id`），报告披露用。
    pub verifier: String,
}

/// 覆盖不变量（4.1）：输出指纹集合必须与输入**集合**相等（少一条、多一条都是 bug）。
pub fn same_fingerprint_set(findings: &[input::InputFinding], out: &[Verification]) -> bool {
    let want: std::collections::BTreeSet<&str> = findings.iter().map(|f| f.fingerprint.as_str()).collect();
    let got: std::collections::BTreeSet<&str> = out.iter().map(|v| v.finding_fingerprint.as_str()).collect();
    want == got && out.len() == want.len()
}

/// 两阶段执行：**prepare**（同步）装配证据 / prompt / 规模预算，**dispatch**（异步、受限并发）只发请求。
///
/// 并发窗口 `k = min(Σ provider 帽, n)` 只是「同时活着的 future 数」；每个 provider 的真实上限仍由
/// `InflightGate` 强制（`registry.lease`），**任何 provider 都不会被撑破**。墙钟到期或 SIGTERM 时整个
/// stream 被 drop：在飞请求取消、permit 归还，已完成条目保留，缺口由 [`finalize`] 补 `inconclusive`
/// ——覆盖不变量不因取消而破。
pub async fn run(
    registry: &ProviderRegistry,
    req: &FixVerifyRequest<'_>,
    heartbeat: &Heartbeat,
    progress: Arc<Mutex<Progress>>,
) -> Outcome {
    let factor = budget::time_factor(req.wall_clock_s);
    // ── Phase 1 prepare（同步：读文件、装配证据、量规模；async 阶段不再碰 fs）──
    let mut prepared = Vec::with_capacity(req.findings.len());
    for (idx, finding) in req.findings.iter().enumerate() {
        if Instant::now() >= req.deadline {
            break;
        }
        let ev = evidence::build(req.repo, req.lang, finding, req.fixes);
        if !ev.located {
            // 无法定位 → inconclusive（不是 unresolved：我们并不知道缺陷还在），不发请求。
            let text = ev.note.unwrap_or_else(|| {
                msg(req.lang, "无法定位该 finding 的代码，未能核验", "the finding could not be located; nothing was verified")
            });
            progress.lock().unwrap().done.push(Verification::inconclusive(&finding.fingerprint, text));
            heartbeat.bump_done();
            continue;
        }
        {
            let mut p = progress.lock().unwrap();
            for w in &ev.warnings {
                p.warnings.insert(w.clone());
            }
        }
        let prior = req.prior.and_then(|p| p.get(&finding.fingerprint));
        let prompt = prompt::build(&prompt::FixVerifyPromptInput { lang: req.lang, finding, evidence: &ev, prior });
        let size_kb = budget::kb(prompt.system.len() + prompt.user.len());
        prepared.push(Prepared {
            idx,
            key: req.routes[idx % req.routes.len()].1.clone(),
            prompt,
            budget: budget::scaled_item_timeout(size_kb, factor),
            file_content: ev.file_content,
        });
    }
    // ── Phase 2 dispatch（异步：只发请求，受 provider 门约束）──
    let k = req.max_parallel.max(1).min(prepared.len().max(1));
    let mut stream = futures::stream::iter(prepared)
        .map(|item| dispatch_one(registry, req, item))
        .buffer_unordered(k);
    while let Some(item) = stream.next().await {
        {
            let mut p = progress.lock().unwrap();
            p.done.push(item.verification);
            p.usage.0 += item.usage.0;
            p.usage.1 += item.usage.1;
            p.usage.2 += item.usage.2;
            p.calls += item.calls;
            p.failures += u32::from(item.failed);
            p.quote_rejected += u32::from(item.quote_rejected);
            p.latency_total_ms += item.latency_ms;
            p.latency_max_ms = p.latency_max_ms.max(item.latency_ms);
            p.rl_retries += item.rl_retries;
            p.rl_wait_ms += item.rl_wait_ms;
        }
        heartbeat.bump_done();
    }
    let snapshot = progress.lock().unwrap().clone();
    finalize(req, &snapshot, true)
}

/// 取消（SIGTERM / 墙钟）后的收敛：已完成的条目保留，其余按 `inconclusive` 补齐。
pub fn snapshot_finalize(req: &FixVerifyRequest<'_>, progress: &Progress) -> Outcome {
    finalize(req, progress, false)
}

fn finalize(req: &FixVerifyRequest<'_>, progress: &Progress, completed: bool) -> Outcome {
    let mut verifications = Vec::with_capacity(req.findings.len());
    let mut inconclusive = 0usize;
    for f in req.findings {
        let v = match progress.done.iter().find(|v| v.finding_fingerprint == f.fingerprint) {
            Some(v) => v.clone(),
            None => Verification::inconclusive(
                &f.fingerprint,
                msg(
                    req.lang,
                    "墙钟预算或终止信号中断了核验，这一条未完成",
                    "the check was interrupted by the wall clock or a termination signal; this finding was not verified",
                ),
            ),
        };
        if v.status.is_inconclusive() {
            inconclusive += 1;
        }
        verifications.push(v);
    }
    debug_assert!(
        same_fingerprint_set(req.findings, &verifications),
        "verify 覆盖不变量被破坏：输出指纹集合与输入不一致"
    );
    let mut reasons = Vec::new();
    if !completed {
        reasons.push(JobFailure::new("wall_clock_exhausted", None));
    }
    if inconclusive > 0 {
        reasons.push(JobFailure::new("verifications_inconclusive", Some(inconclusive.to_string())));
    }
    Outcome {
        verifications,
        usage: progress.usage,
        reasons,
        calls: progress.calls,
        failures: progress.failures,
        quote_rejected: progress.quote_rejected,
        duplicates: req.duplicates,
        latency: (progress.latency_total_ms, progress.latency_max_ms),
        rate_limit: (progress.rl_retries, progress.rl_wait_ms),
        warnings: progress.warnings.iter().cloned().collect(),
        verifier: req.routes.first().map(|(name, _)| name.clone()).unwrap_or_default(),
    }
}

/// prepare 阶段的产物：dispatch 只需要「发给谁 + 发什么 + 给多久」。
struct Prepared {
    idx: usize,
    key: String,
    prompt: prompt::FixVerifyPrompt,
    /// 规模预算（尚未与模型 per-turn / 剩余墙钟取 min）。
    budget: std::time::Duration,
    /// post-fix 检出原文（quote 严格重锚用；删除/不可读 → None）。
    file_content: Option<String>,
}

struct ItemRun {
    verification: Verification,
    usage: (u64, u64, u64),
    /// 本轮真正发出的模型调用数（1 = 正常；2 = 多了一轮 repair）。
    calls: u32,
    failed: bool,
    quote_rejected: bool,
    latency_ms: u64,
    rl_retries: u32,
    rl_wait_ms: u64,
}

/// 未发请求就定论的条目（无法定位等）。
fn decided(finding: &input::InputFinding, evidence: String) -> ItemRun {
    ItemRun {
        verification: Verification::inconclusive(&finding.fingerprint, evidence),
        usage: (0, 0, 0),
        calls: 0,
        failed: false,
        quote_rejected: false,
        latency_ms: 0,
        rl_retries: 0,
        rl_wait_ms: 0,
    }
}

/// 一条 finding 的派发：过 provider 门 → 单轮调用（挂 `AttemptPolicy`）→ 解析 → 四态校验。
///
/// 解析失败只补**一次** repair 轮（把上一轮原文挂回 assistant 位 + 明确"只要 JSON"）：hibot 侧
/// 任一 `inconclusive` 要阻塞一整轮，一次 repair 比丢掉这条判决便宜得多。
async fn dispatch_one(registry: &ProviderRegistry, req: &FixVerifyRequest<'_>, item: Prepared) -> ItemRun {
    let finding = &req.findings[item.idx];
    let lease = match registry.lease(&item.key).await {
        Ok(l) => l,
        Err(e) => {
            return ItemRun {
                failed: true,
                ..decided(
                    finding,
                    msg(req.lang, &format!("模型不可用，未能核验：{e}"), &format!("the model is unavailable; nothing was verified: {e}")),
                )
            }
        }
    };
    let policy = lease.gate_attempts();
    let mut messages = vec![
        ChatMessage::System(item.prompt.system.clone()),
        ChatMessage::User(item.prompt.user.clone()),
    ];
    let mut usage = (0u64, 0u64, 0u64);
    let mut calls = 0u32;
    let mut latency_ms = 0u64;
    let mut judgment: Option<prompt::Judgment> = None;
    let mut parse_err: Option<String> = None;
    // 两轮上限：首轮 + 一次 repair。
    for attempt in 0..2 {
        let remaining = req.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break; // 缺口留给 finalize 补 inconclusive
        }
        // 单条预算 = min(规模预算, 模型 per-turn, 剩余墙钟)。
        let per_turn = item
            .budget
            .min(lease.per_turn())
            .min(remaining.max(std::time::Duration::from_secs(1)));
        let t0 = Instant::now();
        let turn = crate::provider::timed_turn_with(
            lease.backend(),
            &messages,
            &[],
            per_turn,
            lease.retry_budget(),
            Some(&policy),
        )
        .await;
        latency_ms += t0.elapsed().as_millis() as u64;
        match turn {
            Ok(t) => {
                calls += 1;
                usage.0 += t.input_tokens;
                usage.1 += t.output_tokens;
                usage.2 += t.cached_input_tokens;
                match prompt::parse(&t.text) {
                    Ok(j) => {
                        judgment = Some(j);
                        break;
                    }
                    Err(e) => {
                        parse_err = Some(e);
                        if attempt == 1 {
                            break;
                        }
                        messages.push(ChatMessage::Assistant { text: t.text, tool_calls: Vec::new() });
                        messages.push(ChatMessage::User(prompt::REPAIR_PROMPT.to_string()));
                    }
                }
            }
            Err(e) => {
                calls += 1;
                return ItemRun {
                    calls,
                    latency_ms,
                    failed: true,
                    rl_retries: policy.retries(),
                    rl_wait_ms: policy.waited_ms(),
                    ..decided(
                        finding,
                        msg(req.lang, &format!("模型调用失败，未能核验：{e}"), &format!("the model call failed; nothing was verified: {e}")),
                    )
                };
            }
        }
    }
    let Some(judgment) = judgment else {
        let (text, failed) = match parse_err {
            Some(e) => (
                msg(req.lang, &format!("模型输出无法解析，未能核验：{e}"), &format!("the model output could not be parsed; nothing was verified: {e}")),
                true,
            ),
            None => (
                msg(req.lang, "墙钟预算耗尽，这一条未完成核验", "the wall clock was exhausted before this finding was checked"),
                false,
            ),
        };
        return ItemRun {
            usage,
            calls,
            latency_ms,
            failed,
            rl_retries: policy.retries(),
            rl_wait_ms: policy.waited_ms(),
            ..decided(finding, text)
        };
    };
    // 证据不足（模型没给证据）→ inconclusive：契约要求 evidence 非空，不能替模型编。
    if judgment.evidence.trim().is_empty() {
        return ItemRun {
            usage,
            calls,
            latency_ms,
            rl_retries: policy.retries(),
            rl_wait_ms: policy.waited_ms(),
            ..decided(
                finding,
                msg(req.lang, "模型未给出证据，未能核验", "the model returned no evidence; nothing was verified"),
            )
        };
    }
    // quote 必须逐字存在：先查材料，再查**整个 post-fix 文件**（严格字面重锚）；都不命中才降级。
    let quote = match judgment.quote.as_deref() {
        Some(q) => match evidence::reanchor(q, &item.prompt.material, item.file_content.as_deref()) {
            Some(v) => Some(v),
            None => {
                return ItemRun {
                    usage,
                    calls,
                    latency_ms,
                    quote_rejected: true,
                    rl_retries: policy.retries(),
                    rl_wait_ms: policy.waited_ms(),
                    ..decided(
                        finding,
                        msg(
                            req.lang,
                            &format!("模型给出的 quote 未能在提供的材料或检出中逐字命中（`{}`），未能核验", one_line(q)),
                            &format!("the model's quote was not found verbatim in the material or the checkout (`{}`); nothing was verified", one_line(q)),
                        ),
                    )
                }
            }
        },
        None => None,
    };
    ItemRun {
        verification: Verification {
            finding_fingerprint: finding.fingerprint.clone(),
            status: judgment.status,
            evidence: judgment.evidence,
            quote,
        },
        usage,
        calls,
        failed: false,
        quote_rejected: false,
        latency_ms,
        rl_retries: policy.retries(),
        rl_wait_ms: policy.waited_ms(),
    }
}

/// quote 进 `evidence` 句子里时压成一行并截断（报告字段可读性，不改判决）。
fn one_line(s: &str) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 80 {
        flat
    } else {
        format!("{}…", flat.chars().take(80).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Language;

    fn finding(fp: &str) -> input::InputFinding {
        input::InputFinding {
            fingerprint: fp.to_string(),
            severity: "high".into(),
            confidence: 0.9,
            file: "a.rs".into(),
            line_start: 1,
            line_end: 1,
            quote: "q".into(),
            title: "t".into(),
            body: "b".into(),
            recommendation: "r".into(),
        }
    }

    fn request<'a>(
        findings: &'a [input::InputFinding],
        lang: &'a OutputLang,
        fixes: &'a [input::FileFix],
    ) -> FixVerifyRequest<'a> {
        FixVerifyRequest {
            repo: Path::new("."),
            lang,
            findings,
            fixes,
            prior: None,
            routes: vec![("p/v".into(), "verifier:p/v".into())],
            max_parallel: 4,
            wall_clock_s: 1200,
            deadline: Instant::now() + std::time::Duration::from_secs(60),
            duplicates: 0,
        }
    }

    fn v(fp: &str, status: FixVerifyStatus) -> Verification {
        Verification { finding_fingerprint: fp.into(), status, evidence: "e".into(), quote: None }
    }

    #[test]
    fn coverage_detects_missing_and_extra_verifications() {
        let findings = vec![finding("fp1"), finding("fp2")];
        let lang = Language::EnUs.output();
        let req = request(&findings, &lang, &[]);

        // 覆盖不变量本身：少一条 / 多一条都必须被判失败（finalize 的补齐不能掩盖它）。
        assert!(!same_fingerprint_set(&findings, &[v("fp1", FixVerifyStatus::Resolved)]), "少一条必须判失败");
        assert!(
            !same_fingerprint_set(
                &findings,
                &[v("fp1", FixVerifyStatus::Resolved), v("fp2", FixVerifyStatus::Resolved), v("fp3", FixVerifyStatus::Resolved)]
            ),
            "多一条必须判失败"
        );
        assert!(same_fingerprint_set(
            &findings,
            &[v("fp1", FixVerifyStatus::Resolved), v("fp2", FixVerifyStatus::Resolved)]
        ));

        let mut p = Progress::default();
        p.done.push(v("fp1", FixVerifyStatus::Resolved));
        let out = finalize(&req, &p, true);
        assert_eq!(out.verifications.len(), 2, "缺口补 inconclusive");
        assert_eq!(out.verifications[1].status, FixVerifyStatus::Inconclusive);
        assert_eq!(out.reasons.len(), 1);
        assert_eq!(out.reasons[0].code, "verifications_inconclusive");
        assert_eq!(out.reasons[0].detail.as_deref(), Some("1"));

        let mut p = Progress::default();
        p.done.push(v("fp1", FixVerifyStatus::Resolved));
        p.done.push(v("fp2", FixVerifyStatus::Resolved));
        p.done.push(v("fp3", FixVerifyStatus::Resolved));
        let out = finalize(&req, &p, true);
        assert_eq!(out.verifications.len(), 2, "输出被输入集合裁剪（集合语义）");
        assert!(same_fingerprint_set(&findings, &out.verifications));
        assert!(out.reasons.is_empty(), "多一条不算 partial：报告只覆盖输入集合");
    }

    #[test]
    fn empty_input_is_ok_with_no_verifications_and_no_fabricated_verdict() {
        let findings: Vec<input::InputFinding> = Vec::new();
        let lang = Language::EnUs.output();
        let req = request(&findings, &lang, &[]);
        let out = finalize(&req, &Progress::default(), true);
        assert!(out.verifications.is_empty());
        assert!(out.reasons.is_empty(), "空输入必须 ok（Q6/B5）");
        assert!(same_fingerprint_set(&findings, &out.verifications));
    }

    #[test]
    fn cancellation_marks_the_rest_inconclusive_and_records_both_reasons() {
        let findings = vec![finding("fp1")];
        let lang = Language::ZhCn.output();
        let req = request(&findings, &lang, &[]);
        let out = finalize(&req, &Progress::default(), false);
        assert_eq!(out.verifications[0].status, FixVerifyStatus::Inconclusive);
        assert!(out.verifications[0].evidence.contains("中断"), "证据随 language: {}", out.verifications[0].evidence);
        let codes: Vec<&str> = out.reasons.iter().map(|r| r.code.as_str()).collect();
        assert!(codes.contains(&"wall_clock_exhausted") && codes.contains(&"verifications_inconclusive"));
    }

    #[test]
    fn status_parse_is_closed_and_case_insensitive() {
        assert_eq!(FixVerifyStatus::parse("Resolved"), Some(FixVerifyStatus::Resolved));
        assert_eq!(FixVerifyStatus::parse(" inconclusive "), Some(FixVerifyStatus::Inconclusive));
        assert_eq!(FixVerifyStatus::parse("fixed"), None);
        assert_eq!(FixVerifyStatus::as_str(FixVerifyStatus::Regressed), "regressed");
        assert_eq!(
            serde_json::to_string(&FixVerifyStatus::Inconclusive).unwrap(),
            "\"inconclusive\"",
            "报告里的枚举拼写"
        );
    }
}
