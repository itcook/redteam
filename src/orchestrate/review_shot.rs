//! reviewer 单发通路（M21b 批 C-3）：旁路落盘 + 终判权威 + FindingSink。
//! 与 `scan_shot` 同构两轮（turn → parse → 失败则 REPAIR + turn_final）。
//! 批 D 合回前 `sink = &()` no-op，不建 mpsc。

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::domain::{CellOutput, Finding};
use crate::orchestrate::enrich;
use crate::positioning::anchor_review_findings;
use crate::prompt::ScanPrompt;
use crate::provider::parse::{
    diagnostic_parse_error, parse_review_output_with_stats, ReviewParseStats, REPAIR_PROMPT_REVIEW,
};
use crate::provider::stream_json::IncrementalFindings;
use crate::provider::tool_loop::{ChatBackend, ChatMessage};
use crate::provider::{timed_turn_final_with, timed_turn_with, AttemptPolicy, ProviderError};

/// 与批 D 的唯一事件（不泄漏 QueueItem）。
#[derive(Clone, Debug)]
pub struct FindingEvent {
    pub finding: Finding,
    pub agreement: u32,
    pub reviewer_family: String,
}

/// 缺省 no-op，避免无消费者堵住派发器。
pub trait FindingSink: Send + Sync {
    fn on_finding(&self, ev: FindingEvent);
}

impl FindingSink for () {
    fn on_finding(&self, _ev: FindingEvent) {}
}

/// 发射上下文：旁路与终判共用的身份 / 去重面（一处收拢，免两个函数各带 5-6 个同源参数）。
pub(crate) struct EmitCtx<'a> {
    model: &'a str,
    family: &'a str,
    agreement: u32,
    root: &'a Path,
    sink: &'a dyn FindingSink,
    /// 已发射 finding.id——旁路发过的，终判不补发。
    emitted: HashSet<String>,
}

impl<'a> EmitCtx<'a> {
    pub(crate) fn new(
        model: &'a str,
        family: &'a str,
        agreement: u32,
        root: &'a Path,
        sink: &'a dyn FindingSink,
    ) -> Self {
        EmitCtx { model, family, agreement, root, sink, emitted: HashSet::new() }
    }

    /// 首次见到该 id 才发射（旁路 / 终判同一去重面）。
    fn emit(&mut self, f: &Finding) {
        if !self.emitted.insert(f.id.clone()) {
            return;
        }
        self.sink.on_finding(FindingEvent {
            finding: f.clone(),
            agreement: self.agreement,
            reviewer_family: self.family.to_string(),
        });
    }
}

/// 两轮累计用量（参数面收拢）。
#[derive(Clone, Copy, Default)]
struct TurnUsage {
    input: u64,
    output: u64,
    cached: u64,
    reasoning: Option<u64>,
}

/// 一次 reviewer 调用成品（对齐 ScanCall + stream_discarded）。
#[derive(Clone, Debug)]
pub struct ReviewCall {
    pub output: CellOutput,
    pub stats: ReviewParseStats,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub iterations: u32,
    pub finish_reason: Option<String>,
    pub reasoning_tokens: Option<u64>,
    pub repair_note: Option<String>,
    pub rate_limit_retries: u32,
    pub rate_limit_wait_ms: u64,
}

fn sum_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0) + y.unwrap_or(0)),
    }
}

fn emit_raw(raws: Vec<crate::domain::RawFinding>, ctx: &mut EmitCtx) {
    for raw in raws {
        let mut f = enrich(&raw, ctx.model);
        f.agreement = ctx.agreement;
        f.models = vec![ctx.model.to_string()];
        ctx.emit(&f);
    }
}

/// 轮间旁路：发已闭合对象，**不** finalize——后续轮次还要继续 feed 同一个解析器。
fn drain_bypass(inc: &Arc<Mutex<IncrementalFindings>>, ctx: &mut EmitCtx) {
    let raws = inc.lock().unwrap().take();
    emit_raw(raws, ctx);
}

/// 错误路径收口（超时 / 网络失败 / 取消）：先发已闭合对象（不丢），再 `finalize`
/// ——未闭合片段计入 `discarded` 并清缓冲，不静默丢。PRD「必调 finalize」的失败半边。
fn close_bypass(inc: &Arc<Mutex<IncrementalFindings>>, ctx: &mut EmitCtx) {
    drain_bypass(inc, ctx);
    inc.lock().unwrap().finalize();
}

/// reviewer 两轮单发。`inc` 由调用方挂到 per-cell backend 的 `with_text_delta` 后再传入。
pub(crate) async fn run_review_shot_with(
    backend: &dyn ChatBackend,
    prompt: &ScanPrompt,
    per_turn: std::time::Duration,
    retry_budget: u32,
    policy: Option<&dyn AttemptPolicy>,
    ctx: &mut EmitCtx<'_>,
    inc: Arc<Mutex<IncrementalFindings>>,
) -> Result<ReviewCall, ProviderError> {
    let mut messages = vec![
        ChatMessage::System(prompt.system.clone()),
        ChatMessage::UserBlocks(prompt.blocks.clone()),
    ];

    let turn = match timed_turn_with(backend, &messages, &[], per_turn, retry_budget, policy).await {
        Ok(t) => t,
        Err(e) => {
            close_bypass(&inc, ctx);
            return Err(e);
        }
    };
    drain_bypass(&inc, ctx);

    let mut usage = TurnUsage {
        input: turn.input_tokens,
        output: turn.output_tokens,
        cached: turn.cached_input_tokens,
        reasoning: turn.reasoning_tokens,
    };

    if let Ok(call) = finish_parse(&turn.text, ctx, &inc, 1, turn.finish_reason.clone(), None, usage) {
        return Ok(call);
    }

    let head: String = turn.text.trim().chars().take(80).collect();
    let repair_note = format!(
        "first turn parse failed (finish={}, text_len={}, head={:?}); repaired",
        turn.finish_reason.as_deref().unwrap_or("none"),
        turn.text.len(),
        head
    );
    if !turn.text.trim().is_empty() {
        messages.push(ChatMessage::Assistant { text: turn.text.clone(), tool_calls: vec![] });
    }
    messages.push(ChatMessage::User(REPAIR_PROMPT_REVIEW.to_string()));
    let repair = match timed_turn_final_with(backend, &messages, per_turn, retry_budget, policy).await {
        Ok(r) => r,
        Err(e) => {
            close_bypass(&inc, ctx);
            return Err(e.attach_usage((usage.input, usage.output, usage.cached)));
        }
    };
    drain_bypass(&inc, ctx);
    usage.input += repair.input_tokens;
    usage.output += repair.output_tokens;
    usage.cached += repair.cached_input_tokens;
    usage.reasoning = sum_opt(usage.reasoning, repair.reasoning_tokens);

    match finish_parse(
        &repair.text, ctx, &inc, 2, repair.finish_reason.clone(), Some(repair_note), usage,
    ) {
        Ok(call) => Ok(call),
        Err(_) => {
            inc.lock().unwrap().finalize();
            Err(diagnostic_parse_error(&repair).attach_usage((usage.input, usage.output, usage.cached)))
        }
    }
}

fn finish_parse(
    text: &str,
    ctx: &mut EmitCtx,
    inc: &Arc<Mutex<IncrementalFindings>>,
    iterations: u32,
    finish_reason: Option<String>,
    repair_note: Option<String>,
    usage: TurnUsage,
) -> Result<ReviewCall, ProviderError> {
    let (out, mut stats) = parse_review_output_with_stats(text, ctx.model)?;
    let CellOutput::Review { findings, needs } = out else {
        return Err(ProviderError::Parse("review parse did not yield Review".into()));
    };
    let (kept, dropped, dropped_unreadable) = anchor_review_findings(findings, ctx.root);
    stats.dropped_quote = dropped + dropped_unreadable;

    let mut final_findings = Vec::new();
    for mut f in kept {
        f.agreement = ctx.agreement;
        ctx.emit(&f);
        final_findings.push(f);
    }

    {
        let mut g = inc.lock().unwrap();
        g.finalize();
        stats.stream_discarded = g.discarded();
    }

    Ok(ReviewCall {
        output: CellOutput::Review { findings: final_findings, needs },
        stats,
        input_tokens: usage.input,
        output_tokens: usage.output,
        cached_input_tokens: usage.cached,
        iterations,
        finish_reason,
        reasoning_tokens: usage.reasoning,
        repair_note,
        rate_limit_retries: 0,
        rate_limit_wait_ms: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::finding_id;
    use crate::prompt::{PromptBlock, ScanPrompt};
    use crate::provider::parse::parse_review_output_with_stats;
    use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend};
    use std::time::Duration;

    const TWO: &str = r#"{"findings":[
        {"severity":"low","title":"t1","body":"b1","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r1","quote":"fn a() {"},
        {"severity":"medium","title":"t2","body":"b2","file":"a.rs","line_start":2,"line_end":2,"confidence":0.6,"recommendation":"r2","quote":"fn b() {"}
    ],"needs":[]}"#;

    fn turn(text: &str) -> AssistantTurn {
        AssistantTurn {
            text: text.into(),
            tool_calls: vec![],
            input_tokens: 10,
            output_tokens: 4,
            cached_input_tokens: 0,
            finish_reason: Some("stop".into()),
            reasoning_tokens: None,
        }
    }

    fn prompt() -> ScanPrompt {
        ScanPrompt {
            system: "<role>head</role>".into(),
            blocks: vec![PromptBlock { text: "<reviewer>t</reviewer>".into(), cache_boundary: false }],
        }
    }

    struct Rec(Mutex<Vec<FindingEvent>>);
    impl FindingSink for Rec {
        fn on_finding(&self, ev: FindingEvent) {
            self.0.lock().unwrap().push(ev);
        }
    }

    fn tmp_file() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn a() {\nfn b() {\n").unwrap();
        d
    }

    #[tokio::test]
    async fn stream_then_final_does_not_double_emit() {
        let dir = tmp_file();
        let rec = Rec(Mutex::new(Vec::new()));
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let inc2 = inc.clone();
        let backend = FakeChatBackend::new(vec![turn(TWO)])
            .with_delta_chunk(11)
            .with_text_delta(Box::new(move |s: &str| inc2.lock().unwrap().feed(s)));
        let mut ctx = EmitCtx::new("m", "alpha", 2, dir.path(), &rec);
        let call = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc,
        )
        .await
        .unwrap();
        let events = rec.0.lock().unwrap();
        assert_eq!(events.len(), 2, "两条各发一次");
        assert!(events.iter().all(|e| e.agreement == 2 && e.reviewer_family == "alpha"));
        let CellOutput::Review { findings, .. } = &call.output else { panic!() };
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].agreement, 2);
        assert_eq!(call.stats.stream_discarded, 0);
        assert_eq!(call.iterations, 1);
        let ids: HashSet<_> = events.iter().map(|e| e.finding.id.clone()).collect();
        assert_eq!(ids.len(), 2);
    }

    #[tokio::test]
    async fn unit_sink_does_not_block() {
        let dir = tmp_file();
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let backend = FakeChatBackend::new(vec![turn(TWO)]);
        let mut ctx = EmitCtx::new("m", "alpha", 1, dir.path(), &());
        let call = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc,
        )
        .await
        .unwrap();
        let CellOutput::Review { findings, .. } = call.output else { panic!() };
        assert_eq!(findings.len(), 2);
    }

    #[tokio::test]
    async fn truncated_finalizes_and_keeps_closed() {
        let dir = tmp_file();
        let rec = Rec(Mutex::new(Vec::new()));
        let truncated = &TWO[..TWO.len() - 40];
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let inc2 = inc.clone();
        let backend = FakeChatBackend::new(vec![turn(truncated), turn(truncated)])
            .with_delta_chunk(8)
            .with_text_delta(Box::new(move |s: &str| inc2.lock().unwrap().feed(s)));
        let mut ctx = EmitCtx::new("m", "alpha", 1, dir.path(), &rec);
        let err = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc.clone(),
        )
        .await
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("Parse") || msg.contains("parse"), "{msg}");
        assert!(!rec.0.lock().unwrap().is_empty(), "已闭合对象已发射");
        assert!(inc.lock().unwrap().discarded() >= 1, "两轮都 parse 失败——终态路径已 finalize");
    }

    /// AC：超时 / 网络失败路径也必调 `finalize`——未闭合片段计入 `discarded`，不静默丢。
    #[tokio::test]
    async fn error_path_finalizes_bypass() {
        let dir = tmp_file();
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        // 流到一半（对象未闭合）即超时。
        inc.lock().unwrap().feed(r#"{"findings":[{"severity":"low","title":"half"#);
        let backend = FakeChatBackend::new_results(vec![Err(ProviderError::Timeout("probe".into()))]);
        let mut ctx = EmitCtx::new("m", "alpha", 1, dir.path(), &());
        let err = run_review_shot_with(
            &backend, &prompt(), Duration::from_millis(50), 0, None, &mut ctx, inc.clone(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ProviderError::Timeout(_)), "{err:?}");
        assert_eq!(inc.lock().unwrap().discarded(), 1, "未闭合片段经 finalize 计数");
    }

    /// REPAIR 轮失败（第二轮 turn 直接 Err）同样收口——错误半边两条路径都不漏 finalize。
    #[tokio::test]
    async fn repair_turn_error_finalizes_bypass() {
        let dir = tmp_file();
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let inc2 = inc.clone();
        let backend = FakeChatBackend::new_results(vec![
            Ok(turn(r#"{"findings":[{"severity":"low","title":"half"#)),
            Err(ProviderError::Timeout("repair".into())),
        ])
        .with_delta_chunk(8)
        .with_text_delta(Box::new(move |s: &str| inc2.lock().unwrap().feed(s)));
        let mut ctx = EmitCtx::new("m", "alpha", 1, dir.path(), &());
        let err = run_review_shot_with(
            &backend, &prompt(), Duration::from_millis(50), 0, None, &mut ctx, inc.clone(),
        )
        .await
        .unwrap_err();
        // REPAIR 轮的错误带首轮用量回传（WithUsage 包一层），断言内层成因。
        let msg = format!("{err:?}");
        assert!(msg.contains("Timeout"), "{msg}");
        assert_eq!(inc.lock().unwrap().discarded(), 1, "REPAIR 轮失败也 finalize");
    }

    #[test]
    fn finding_id_matches_enrich() {
        let (out, _) = parse_review_output_with_stats(TWO, "m").unwrap();
        let CellOutput::Review { findings, .. } = out else { panic!() };
        let id = finding_id(
            &findings[0].file, &findings[0].quote, &findings[0].title,
            findings[0].line_start, findings[0].line_end,
        );
        assert_eq!(id, findings[0].id);
    }
}
