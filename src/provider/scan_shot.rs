//! scanner 单发通路（M21a 批 F-2，prd K3）：`[System(共享头), UserBlocks(参考 / 证据 / diff / 角色尾)]` → 首发 `turn`
//! （不用 `turn_final`，与旧单发同一铁律：首发不带 json 收尾形态）→ `parse_scan_output_with_stats` → 失败则补一轮
//! `REPAIR_PROMPT_SCAN` + `turn_final`（ONE-shot，不循环）→ 再解析 / `diagnostic_parse_error`。
//!
//! 与 `single_shot::run_single_shot` 的分叉点：system 是 `ScanPrompt.system`（共享头，字节稳定、cache 区），不是 `NO_TOOLS_SYSTEM`；
//! user 是分块（anthropic 三段界 `cache_control`、openai 拼接）；契约是疑点（`ScanOutput`）而非 findings。
//! usage（input / output / cached）跨轮累加；`reasoning_tokens` 取各轮 `Some` 之和（全 None → None，anthropic 无分列）。

use crate::domain::ScanOutput;
use crate::prompt::ScanPrompt;
use crate::provider::parse::{diagnostic_parse_error, parse_scan_output_with_stats, ScanParseStats, REPAIR_PROMPT_SCAN};
use crate::provider::tool_loop::{ChatBackend, ChatMessage};
use crate::provider::{timed_turn, timed_turn_final, ProviderError};

/// 一次 scanner 调用的成品：规整后的疑点输出 + parse 丢弃统计 + usage / 迭代数 / 结束原因 / 推理 token（观测）。
#[derive(Clone, Debug)]
pub struct ScanCall {
    pub output: ScanOutput,
    pub stats: ScanParseStats,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    /// 1 = 首发即解析成功；2 = 经一次 REPAIR 轮。
    pub iterations: u32,
    /// 最后一轮的 finish_reason（openai）/ stop_reason（anthropic）。
    pub finish_reason: Option<String>,
    /// 各轮 `reasoning_tokens` 之和；provider 不分列（anthropic）则 None。零推理 + 短输出的观测口径（prd K5）。
    pub reasoning_tokens: Option<u64>,
}

fn sum_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0) + y.unwrap_or(0)),
    }
}

pub async fn run_scan_shot(
    backend: &dyn ChatBackend,
    prompt: &ScanPrompt,
    per_turn: std::time::Duration,
    retry_budget: u32,
) -> Result<ScanCall, ProviderError> {
    let mut messages = vec![ChatMessage::System(prompt.system.clone()), ChatMessage::UserBlocks(prompt.blocks.clone())];

    let turn = timed_turn(backend, &messages, &[], per_turn, retry_budget).await?;
    let (mut in_tok, mut out_tok, mut cached_tok) = (turn.input_tokens, turn.output_tokens, turn.cached_input_tokens);
    let mut reasoning = turn.reasoning_tokens;

    if let Ok((output, stats)) = parse_scan_output_with_stats(&turn.text) {
        return Ok(ScanCall {
            output, stats, input_tokens: in_tok, output_tokens: out_tok, cached_input_tokens: cached_tok,
            iterations: 1, finish_reason: turn.finish_reason, reasoning_tokens: reasoning,
        });
    }

    // 首发 parse 失败：把模型的原话作为 assistant 轮回灌（模型能看见自己写了什么；anthropic 要求角色交替），再补
    // REPAIR_PROMPT_SCAN + turn_final。本轮若失败（超时 / 网络），把首发已烧的 usage 附着到错误上（T25 口径）。
    if !turn.text.trim().is_empty() {
        messages.push(ChatMessage::Assistant { text: turn.text.clone(), tool_calls: vec![] });
    }
    messages.push(ChatMessage::User(REPAIR_PROMPT_SCAN.to_string()));
    let repair = timed_turn_final(backend, &messages, per_turn, retry_budget)
        .await
        .map_err(|e| e.attach_usage((in_tok, out_tok, cached_tok)))?;
    in_tok += repair.input_tokens;
    out_tok += repair.output_tokens;
    cached_tok += repair.cached_input_tokens;
    reasoning = sum_opt(reasoning, repair.reasoning_tokens);

    match parse_scan_output_with_stats(&repair.text) {
        Ok((output, stats)) => Ok(ScanCall {
            output, stats, input_tokens: in_tok, output_tokens: out_tok, cached_input_tokens: cached_tok,
            iterations: 2, finish_reason: repair.finish_reason, reasoning_tokens: reasoning,
        }),
        Err(_) => Err(diagnostic_parse_error(&repair).attach_usage((in_tok, out_tok, cached_tok))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prompt::PromptBlock;
    use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend};
    use std::time::Duration;

    const SCAN_JSON: &str = r#"{"suspicions":[{"file":"src/a.rs","line_start":3,"line_end":3,"hint":"h","confidence":0.7,"quote":"let t = secret();"}],"needs":[{"path":"src/b.rs","reason":"r"}],"scanned_areas":[{"file":"src/a.rs","ranges":["1-9"]}]}"#;

    fn turn(text: &str, reasoning: Option<u64>) -> AssistantTurn {
        AssistantTurn { text: text.into(), tool_calls: vec![], input_tokens: 10, output_tokens: 4, cached_input_tokens: 2, finish_reason: Some("stop".into()), reasoning_tokens: reasoning }
    }

    fn prompt() -> ScanPrompt {
        ScanPrompt {
            system: "<role>head</role>".into(),
            blocks: vec![
                PromptBlock { text: "<reference>r</reference>".into(), cache_boundary: false },
                PromptBlock { text: "<evidence>e</evidence>".into(), cache_boundary: true },
                PromptBlock { text: "<diff>d</diff>".into(), cache_boundary: true },
                PromptBlock { text: "<scanner>t</scanner>".into(), cache_boundary: false },
            ],
        }
    }

    /// 首发即成功：system = 共享头（非 NO_TOOLS_SYSTEM）、user 是分块、不调 turn_final、iterations 1。
    #[tokio::test]
    async fn scan_shot_first_turn_uses_shared_head_blocks_and_never_turn_final() {
        let b = FakeChatBackend::new(vec![turn(SCAN_JSON, Some(0))]);
        let call = run_scan_shot(&b, &prompt(), Duration::from_secs(5), 0).await.unwrap();
        assert_eq!(call.iterations, 1);
        assert_eq!(call.output.suspicions.len(), 1);
        assert_eq!(call.output.needs.len(), 1);
        assert_eq!((call.input_tokens, call.output_tokens, call.cached_input_tokens), (10, 4, 2));
        assert_eq!(call.reasoning_tokens, Some(0));
        assert_eq!(call.finish_reason.as_deref(), Some("stop"));
        assert_eq!(b.final_call_count(), 0, "首发不走 turn_final");
        let seen = b.messages_at(0);
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], ChatMessage::System("<role>head</role>".into()));
        assert!(matches!(&seen[1], ChatMessage::UserBlocks(bl) if bl.len() == 4 && bl[1].cache_boundary && bl[2].cache_boundary));
    }

    /// 首发违约 → 回灌 assistant 原话 + REPAIR_PROMPT_SCAN → turn_final → 成功 iterations 2；usage 与 reasoning 跨轮累加。
    #[tokio::test]
    async fn scan_shot_repairs_with_scan_prompt_and_accumulates_usage() {
        let b = FakeChatBackend::new(vec![turn("sorry, here is prose", Some(3)), turn(SCAN_JSON, Some(5))]);
        let call = run_scan_shot(&b, &prompt(), Duration::from_secs(5), 0).await.unwrap();
        assert_eq!(call.iterations, 2);
        assert_eq!((call.input_tokens, call.output_tokens, call.cached_input_tokens), (20, 8, 4));
        assert_eq!(call.reasoning_tokens, Some(8));
        assert_eq!(b.final_call_count(), 1);
        let seen = b.messages_at(1);
        assert_eq!(seen.len(), 4, "system + blocks + assistant 原话 + repair: {seen:?}");
        assert!(matches!(&seen[2], ChatMessage::Assistant { text, .. } if text == "sorry, here is prose"));
        assert_eq!(seen[3], ChatMessage::User(REPAIR_PROMPT_SCAN.to_string()));
    }

    /// 两轮都违约 → diagnostic_parse_error（含 finish_reason）且 usage 附着；空首发不回灌空 assistant。
    #[tokio::test]
    async fn scan_shot_second_failure_is_parse_error_with_usage() {
        let b = FakeChatBackend::new(vec![turn("", None), AssistantTurn { finish_reason: Some("length".into()), ..turn("{\"nope\":1}", None) }]);
        let err = run_scan_shot(&b, &prompt(), Duration::from_secs(5), 0).await.unwrap_err();
        assert!(matches!(err.inner(), ProviderError::Parse(_)), "{err}");
        assert_eq!(err.burned_usage(), (20, 8, 4));
        assert!(err.to_string().contains("length"), "finish_reason 进诊断: {err}");
        let seen = b.messages_at(1);
        assert_eq!(seen.len(), 3, "空首发不回灌 assistant: {seen:?}");
    }

    /// 首发网络错误直接冒泡（无 usage 可附着）。
    #[tokio::test]
    async fn scan_shot_first_turn_error_propagates() {
        let b = FakeChatBackend::new_results(vec![Err(ProviderError::Auth("no key".into()))]);
        let err = run_scan_shot(&b, &prompt(), Duration::from_secs(5), 0).await.unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
    }
}
