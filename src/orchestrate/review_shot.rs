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

fn emit_raw(
    raws: Vec<crate::domain::RawFinding>,
    model: &str,
    family: &str,
    agreement: u32,
    emitted: &mut HashSet<String>,
    sink: &dyn FindingSink,
    landed: &mut Vec<Finding>,
) {
    for raw in raws {
        let mut f = enrich(&raw, model);
        f.agreement = agreement;
        f.models = vec![model.to_string()];
        if emitted.insert(f.id.clone()) {
            sink.on_finding(FindingEvent {
                finding: f.clone(),
                agreement,
                reviewer_family: family.to_string(),
            });
            landed.push(f);
        }
    }
}

/// reviewer 两轮单发。`inc` 由调用方挂到 per-cell backend 的 `with_text_delta` 后再传入。
pub(crate) async fn run_review_shot_with(
    backend: &dyn ChatBackend,
    prompt: &ScanPrompt,
    per_turn: std::time::Duration,
    retry_budget: u32,
    policy: Option<&dyn AttemptPolicy>,
    model: &str,
    family: &str,
    agreement: u32,
    root: &Path,
    inc: Arc<Mutex<IncrementalFindings>>,
    sink: &dyn FindingSink,
) -> Result<ReviewCall, ProviderError> {
    let mut messages = vec![
        ChatMessage::System(prompt.system.clone()),
        ChatMessage::UserBlocks(prompt.blocks.clone()),
    ];
    let mut emitted = HashSet::new();
    let mut landed: Vec<Finding> = Vec::new();

    let turn = match timed_turn_with(backend, &messages, &[], per_turn, retry_budget, policy).await {
        Ok(t) => t,
        Err(e) => {
            drain_bypass(&inc, model, family, agreement, &mut emitted, sink, &mut landed);
            return Err(e);
        }
    };
    drain_bypass(&inc, model, family, agreement, &mut emitted, sink, &mut landed);

    let (mut in_tok, mut out_tok, mut cached_tok) = (turn.input_tokens, turn.output_tokens, turn.cached_input_tokens);
    let mut reasoning = turn.reasoning_tokens;

    if let Ok(call) = finish_parse(
        &turn.text, model, family, agreement, root, &mut emitted, sink, &mut landed, &inc, 1,
        turn.finish_reason.clone(), None, in_tok, out_tok, cached_tok, reasoning,
    ) {
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
    let repair = timed_turn_final_with(backend, &messages, per_turn, retry_budget, policy)
        .await
        .map_err(|e| {
            drain_bypass(&inc, model, family, agreement, &mut emitted, sink, &mut landed);
            e.attach_usage((in_tok, out_tok, cached_tok))
        })?;
    drain_bypass(&inc, model, family, agreement, &mut emitted, sink, &mut landed);
    in_tok += repair.input_tokens;
    out_tok += repair.output_tokens;
    cached_tok += repair.cached_input_tokens;
    reasoning = sum_opt(reasoning, repair.reasoning_tokens);

    match finish_parse(
        &repair.text, model, family, agreement, root, &mut emitted, sink, &mut landed, &inc, 2,
        repair.finish_reason.clone(), Some(repair_note), in_tok, out_tok, cached_tok, reasoning,
    ) {
        Ok(call) => Ok(call),
        Err(_) => {
            inc.lock().unwrap().finalize();
            Err(diagnostic_parse_error(&repair).attach_usage((in_tok, out_tok, cached_tok)))
        }
    }
}

fn drain_bypass(
    inc: &Arc<Mutex<IncrementalFindings>>,
    model: &str,
    family: &str,
    agreement: u32,
    emitted: &mut HashSet<String>,
    sink: &dyn FindingSink,
    landed: &mut Vec<Finding>,
) {
    let raws = inc.lock().unwrap().take();
    emit_raw(raws, model, family, agreement, emitted, sink, landed);
}

fn finish_parse(
    text: &str,
    model: &str,
    family: &str,
    agreement: u32,
    root: &Path,
    emitted: &mut HashSet<String>,
    sink: &dyn FindingSink,
    landed: &mut Vec<Finding>,
    inc: &Arc<Mutex<IncrementalFindings>>,
    iterations: u32,
    finish_reason: Option<String>,
    repair_note: Option<String>,
    in_tok: u64,
    out_tok: u64,
    cached_tok: u64,
    reasoning: Option<u64>,
) -> Result<ReviewCall, ProviderError> {
    let (out, mut stats) = parse_review_output_with_stats(text, model)?;
    let CellOutput::Review { findings, needs } = out else {
        return Err(ProviderError::Parse("review parse did not yield Review".into()));
    };
    let (kept, dropped, dropped_unreadable) = anchor_review_findings(findings, root);
    stats.dropped_quote = dropped + dropped_unreadable;

    let mut final_findings = Vec::new();
    for mut f in kept {
        f.agreement = agreement;
        if !emitted.contains(&f.id) {
            emitted.insert(f.id.clone());
            sink.on_finding(FindingEvent {
                finding: f.clone(),
                agreement,
                reviewer_family: family.to_string(),
            });
            landed.push(f.clone());
        }
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
        input_tokens: in_tok,
        output_tokens: out_tok,
        cached_input_tokens: cached_tok,
        iterations,
        finish_reason,
        reasoning_tokens: reasoning,
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
        let call = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, "m", "alpha", 2, dir.path(), inc, &rec,
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
        let call = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, "m", "alpha", 1, dir.path(), inc, &(),
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
        let err = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, "m", "alpha", 1, dir.path(), inc.clone(), &rec,
        )
        .await
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("Parse") || msg.contains("parse"), "{msg}");
        inc.lock().unwrap().finalize();
        assert!(!rec.0.lock().unwrap().is_empty(), "已闭合对象已发射");
        assert!(inc.lock().unwrap().discarded() >= 1);
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
