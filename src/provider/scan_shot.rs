//! scanner 单发通路（M21a 批 F-2，prd K3）：`[System(共享头), UserBlocks(参考 / 证据 / diff / 角色尾)]` → 首发 `turn`
//! （不用 `turn_final`，与旧单发同一铁律：首发不带 json 收尾形态）→ `parse_scan_output_with_stats` → 失败则补一轮
//! `REPAIR_PROMPT_SCAN` + `turn_final`（ONE-shot，不循环）→ 再解析 / `diagnostic_parse_error`。
//!
//! 与 `single_shot::run_single_shot` 的分叉点：system 是 `ScanPrompt.system`（共享头，字节稳定、cache 区），不是 `NO_TOOLS_SYSTEM`；
//! user 是分块（anthropic 三段界 `cache_control`、openai 拼接）；契约是疑点（`ScanOutput`）而非 findings。
//! usage（input / output / cached）跨轮累加；`reasoning_tokens` 取各轮 `Some` 之和（全 None → None，anthropic 无分列）。

use crate::domain::ScanOutput;
use crate::prompt::ScanPrompt;
use crate::provider::parse::{diagnostic_parse_error, dump_parse_failure, parse_scan_output_with_stats, ScanParseStats, REPAIR_PROMPT_SCAN};
use crate::provider::tool_loop::{ChatBackend, ChatMessage};
use crate::provider::{timed_turn_final_with, timed_turn_with, AttemptPolicy, ProviderError};

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
    /// 首发解析失败、经 REPAIR 成功时的诊断（finish_reason / 文本长度 / 文本头）；首发即成功为 None。
    /// 出口验收观察：dogfood 某格三跑全走 REPAIR 而直发同 prompt 三次首发皆合法——不落原因无从定位。
    pub repair_note: Option<String>,
    /// D13：provider 门的 429 排队重试次数（registry `ScanLease::run` 回填；→ `CellRecord.attempts` = 轮次 + 此值）。
    pub rate_limit_retries: u32,
    /// D13：429 排队等待总毫秒（registry 回填；→ `CellRecord.queue_wait_ms` 与门等待合计）。
    pub rate_limit_wait_ms: u64,
}

fn sum_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0) + y.unwrap_or(0)),
    }
}

/// 两轮单发 + 尝试挂点（D13：registry 传 provider 门的 `GateAttempts`——429 排队重试 + 在途峰值；`None` = 无门，仅测试）。
pub(crate) async fn run_scan_shot_with(
    backend: &dyn ChatBackend,
    prompt: &ScanPrompt,
    per_turn: std::time::Duration,
    retry_budget: u32,
    policy: Option<&dyn AttemptPolicy>,
    // dump 标签用的模型身份串（收口评审 #7）
    model: &str,
) -> Result<ScanCall, ProviderError> {
    let mut messages = vec![ChatMessage::System(prompt.system.clone()), ChatMessage::UserBlocks(prompt.blocks.clone())];

    let turn = timed_turn_with(backend, &messages, &[], per_turn, retry_budget, policy).await?;
    let (mut in_tok, mut out_tok, mut cached_tok) = (turn.input_tokens, turn.output_tokens, turn.cached_input_tokens);
    let mut reasoning = turn.reasoning_tokens;

    if let Ok((output, stats)) = parse_scan_output_with_stats(&turn.text) {
        return Ok(ScanCall {
            output, stats, input_tokens: in_tok, output_tokens: out_tok, cached_input_tokens: cached_tok,
            iterations: 1, finish_reason: turn.finish_reason, reasoning_tokens: reasoning, repair_note: None,
            rate_limit_retries: 0, rate_limit_wait_ms: 0,
        });
    }

    // 首发 parse 失败：把模型的原话作为 assistant 轮回灌（模型能看见自己写了什么；anthropic 要求角色交替），再补
    // REPAIR_PROMPT_SCAN + turn_final。本轮若失败（超时 / 网络），把首发已烧的 usage 附着到错误上（T25 口径）。
    dump_parse_failure(&format!("scan {model} t1"), &turn.text);
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
    messages.push(ChatMessage::User(REPAIR_PROMPT_SCAN.to_string()));
    let repair = timed_turn_final_with(backend, &messages, per_turn, retry_budget, policy)
        .await
        .map_err(|e| e.attach_usage((in_tok, out_tok, cached_tok)))?;
    in_tok += repair.input_tokens;
    out_tok += repair.output_tokens;
    cached_tok += repair.cached_input_tokens;
    reasoning = sum_opt(reasoning, repair.reasoning_tokens);

    match parse_scan_output_with_stats(&repair.text) {
        Ok((output, stats)) => Ok(ScanCall {
            output, stats, input_tokens: in_tok, output_tokens: out_tok, cached_input_tokens: cached_tok,
            iterations: 2, finish_reason: repair.finish_reason, reasoning_tokens: reasoning, repair_note: Some(repair_note),
            rate_limit_retries: 0, rate_limit_wait_ms: 0,
        }),
        Err(_) => {
            dump_parse_failure(&format!("scan {model} t2"), &repair.text);
            Err(diagnostic_parse_error(&repair).attach_usage((in_tok, out_tok, cached_tok)))
        }
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
    /// 收口评审 #7：scan 侧落盘标签要带模型身份——多家族并发时只靠纳秒前缀无法归属产物。
    // ENV_LOCK 是**测试专用**的 env 串行器（仓内约定，见 config::ENV_LOCK 的用法）：
    // 跨 await 持它是刻意的——保护窗口必须覆盖到读 env 的那段异步调用，否则别的用例的 set_var 仍构成竞争。
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn scan_dump_labels_carry_the_model() {
        let _g = crate::config::ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dump = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化（仓内既定约定）
        unsafe { std::env::set_var("REDTEAM_DUMP_DIR", dump.path()); }
        let b = FakeChatBackend::new(vec![turn("not json", None), turn("still not json", None)]);
        let err = run_scan_shot_with(&b, &prompt(), Duration::from_secs(5), 0, None, "qwen/qwen3.8-flash").await;
        unsafe { std::env::remove_var("REDTEAM_DUMP_DIR"); }

        assert!(err.is_err());
        let mut names: Vec<String> = std::fs::read_dir(dump.path()).unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.contains("qwen"))
            .collect();
        names.sort();
        assert_eq!(names.len(), 2, "首轮 + 修复轮各一份: {names:?}");
        assert!(names[0].ends_with("-scan_qwen_qwen3.8-flash_t1.txt"), "{names:?}");
        assert!(names[1].ends_with("-scan_qwen_qwen3.8-flash_t2.txt"), "{names:?}");
    }

    #[tokio::test]
    async fn scan_shot_first_turn_uses_shared_head_blocks_and_never_turn_final() {
        let b = FakeChatBackend::new(vec![turn(SCAN_JSON, Some(0))]);
        let call = run_scan_shot_with(&b, &prompt(), Duration::from_secs(5), 0, None, "p/m").await.unwrap();
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
        let call = run_scan_shot_with(&b, &prompt(), Duration::from_secs(5), 0, None, "p/m").await.unwrap();
        assert_eq!(call.iterations, 2);
        assert_eq!((call.input_tokens, call.output_tokens, call.cached_input_tokens), (20, 8, 4));
        assert_eq!(call.reasoning_tokens, Some(8));
        let note = call.repair_note.as_deref().expect("repair 路径须落诊断");
        assert!(note.contains("first turn parse failed (finish=stop, text_len=20, head=\"sorry, here is prose\")"), "{note}");
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
        let err = run_scan_shot_with(&b, &prompt(), Duration::from_secs(5), 0, None, "p/m").await.unwrap_err();
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
        let err = run_scan_shot_with(&b, &prompt(), Duration::from_secs(5), 0, None, "p/m").await.unwrap_err();
        assert!(matches!(err, ProviderError::Auth(_)));
    }
}
