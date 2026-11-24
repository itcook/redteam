//! triage 三轴的模型单发通路：`[System, UserBlocks]` → 严格 JSON →（失败）一次 repair 轮。
//!
//! 与 `provider::scan_shot` 的分叉点：输出契约是 triage 行/声明（各自 `Deserialize`），
//! 不是 `ScanOutput`；复用同一套 provider 在途门、超时/重试与 JSON 宽容提取。

use crate::prompt::PromptBlock;
use crate::provider::parse::{diagnostic_parse_error, extract_json_object};
use crate::provider::registry::ScanLease;
use crate::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage};
use crate::provider::{timed_turn_final_with, timed_turn_with, ProviderError};
use serde::de::DeserializeOwned;
use std::sync::{Arc, Mutex};

/// 修一轮的指令（与具体轴无关；契约文本由各轴 prompt 给出）。
const REPAIR_PROMPT_TRIAGE: &str =
    "Your previous reply did not match the required JSON object. Reply again with ONLY the JSON object \
     described in the task, no prose, no code fences, no comments. Keep all human-readable strings in the \
     same output language requested earlier.";

#[derive(Clone, Debug)]
pub struct ShotCall<T> {
    pub output: T,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    /// 1 = 首发即解析成功；2 = 经一次 repair 轮。
    pub iterations: u32,
    /// 首发失败、repair 成功时的诊断（进 axes 披露）。
    pub repair_note: Option<String>,
}

/// 单格诊断：外层 timeout 会 cancel 掉内部重试链，用它记录“最后一次看见的”调用状态。
#[derive(Default)]
struct ShotDiag {
    attempts: u32,
    in_flight_started: Option<tokio::time::Instant>,
    last_call_ms: Option<u64>,
    last_error: Option<String>,
    last_finish: Option<String>,
    last_text_len: Option<usize>,
}

impl ShotDiag {
    fn start(&mut self) {
        self.attempts += 1;
        self.in_flight_started = Some(tokio::time::Instant::now());
    }

    fn finish(&mut self, r: &Result<AssistantTurn, crate::provider::ProviderError>) {
        let started = self.in_flight_started.take();
        self.last_call_ms = started.map(|t| t.elapsed().as_millis() as u64);
        match r {
            Ok(t) => {
                self.last_error = None;
                self.last_finish = t.finish_reason.clone();
                self.last_text_len = Some(t.text.len());
            }
            Err(e) => {
                self.last_error = Some(e.to_string());
            }
        }
    }

    fn summary(&self) -> String {
        let in_flight = self.in_flight_started.map(|t| t.elapsed().as_millis() as u64);
        format!(
            "attempts={} last_call_ms={:?} in_flight_ms={:?} last_finish={:?} last_text_len={:?} last_error={:?}",
            self.attempts, self.last_call_ms, in_flight, self.last_finish, self.last_text_len, self.last_error
        )
    }
}

/// 记录每次 backend 调用的结果；不改变请求/响应。
struct DiagBackend<'a> {
    inner: &'a dyn ChatBackend,
    diag: Arc<Mutex<ShotDiag>>,
}

#[async_trait::async_trait]
impl ChatBackend for DiagBackend<'_> {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[crate::tools::ToolSpec],
    ) -> Result<AssistantTurn, crate::provider::ProviderError> {
        self.diag.lock().unwrap().start();
        let r = self.inner.turn(messages, tools).await;
        self.diag.lock().unwrap().finish(&r);
        r
    }

    async fn turn_final(&self, messages: &[ChatMessage]) -> Result<AssistantTurn, crate::provider::ProviderError> {
        self.diag.lock().unwrap().start();
        let r = self.inner.turn_final(messages).await;
        self.diag.lock().unwrap().finish(&r);
        r
    }

    fn on_text_delta(&self) -> Option<crate::provider::backends::DeltaCb> {
        self.inner.on_text_delta()
    }
}

/// 单发 + 一次 repair。`label` 只用于诊断（如 `triage-security`）。
pub async fn run<T: DeserializeOwned>(
    lease: &ScanLease<'_>,
    system: String,
    blocks: Vec<PromptBlock>,
    label: &str,
    cell_timeout: std::time::Duration,
) -> Result<ShotCall<T>, ProviderError> {
    // 墙钟优化：`cell_timeout` 是**整格**预算（含重试与 repair），外层 envelope 封顶；
    // provider 单轮上限仍由 `lease.per_turn()` 管，不动重试语义。
    let cell_deadline = tokio::time::Instant::now() + cell_timeout;
    let diag = Arc::new(Mutex::new(ShotDiag::default()));
    let backend = DiagBackend { inner: lease.backend(), diag: diag.clone() };
    let mut messages = vec![ChatMessage::System(system), ChatMessage::UserBlocks(blocks)];
    let policy = lease.gate_attempts();
    let turn = match tokio::time::timeout_at(
        cell_deadline,
        timed_turn_with(
            &backend,
            &messages,
            &[],
            lease.per_turn(),
            lease.retry_budget(),
            Some(&policy),
        ),
    )
    .await
    {
        Ok(r) => r?,
        Err(_) => {
            let d = diag.lock().unwrap().summary();
            return Err(ProviderError::Timeout(format!(
                "triage cell `{label}` exceeded its {cell_timeout:?} budget ({d})"
            )))
        }
    };
    let (mut in_tok, mut out_tok, mut cached) = (turn.input_tokens, turn.output_tokens, turn.cached_input_tokens);

    if let Some(v) = extract_json_object(&turn.text) {
        if let Ok(output) = serde_json::from_value::<T>(v) {
            return Ok(ShotCall {
                output,
                input_tokens: in_tok,
                output_tokens: out_tok,
                cached_input_tokens: cached,
                iterations: 1,
                repair_note: None,
            });
        }
    }

    let head: String = turn.text.trim().chars().take(80).collect();
    let repair_note = format!(
        "{label}: first turn parse failed (finish={}, text_len={}, head={head:?}); repaired",
        turn.finish_reason.as_deref().unwrap_or("none"),
        turn.text.len()
    );
    if !turn.text.trim().is_empty() {
        messages.push(ChatMessage::Assistant { text: turn.text.clone(), tool_calls: vec![] });
    }
    messages.push(ChatMessage::User(REPAIR_PROMPT_TRIAGE.to_string()));
    let repair = match tokio::time::timeout_at(
        cell_deadline,
        timed_turn_final_with(
            &backend,
            &messages,
            lease.per_turn(),
            lease.retry_budget(),
            Some(&policy),
        ),
    )
    .await
    {
        Ok(r) => r.map_err(|e| e.attach_usage((in_tok, out_tok, cached)))?,
        Err(_) => {
            let d = diag.lock().unwrap().summary();
            return Err(ProviderError::Timeout(format!(
                "triage cell `{label}` exceeded its {cell_timeout:?} budget during repair (first_turn: {repair_note}; {d})"
            ))
            .attach_usage((in_tok, out_tok, cached)))
        }
    };
    in_tok += repair.input_tokens;
    out_tok += repair.output_tokens;
    cached += repair.cached_input_tokens;

    match extract_json_object(&repair.text).and_then(|v| serde_json::from_value::<T>(v).ok()) {
        Some(output) => Ok(ShotCall {
            output,
            input_tokens: in_tok,
            output_tokens: out_tok,
            cached_input_tokens: cached,
            iterations: 2,
            repair_note: Some(repair_note),
        }),
        None => Err(diagnostic_parse_error(&repair).attach_usage((in_tok, out_tok, cached))),
    }
}
