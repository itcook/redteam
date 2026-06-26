use crate::provider::direct::parse_lens_output;
use crate::provider::{LensCall, ProviderError};
use crate::tools::{tool_specs, ToolCall, ToolExecutor, ToolResult, ToolSpec};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Mutex;

/// provider 无关的会话消息。各 backend 自行渲染到本家协议。
#[derive(Clone, Debug, PartialEq)]
pub enum ChatMessage {
    System(String),
    User(String),
    Assistant { text: String, tool_calls: Vec<ToolCall> },
    ToolResults(Vec<ToolResult>),
}

/// 一轮模型输出：要么带 tool_calls（需执行后续轮），要么是最终文本。
#[derive(Clone, Debug)]
pub struct AssistantTurn {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// 单轮往返抽象：给定历史 + 工具声明，返回模型这一轮的输出。
#[async_trait]
pub trait ChatBackend: Send + Sync {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError>;
}

/// 多轮工具循环：发→若 tool_calls 则执行回灌→直至最终文本→解析为 findings。
/// usage 跨轮累加；超过 max_iters 仍未收敛则 Parse 错误。
pub async fn run_tool_loop(
    backend: &dyn ChatBackend,
    executor: &ToolExecutor,
    system: &str,
    user_prompt: &str,
    max_iters: u32,
) -> Result<LensCall, ProviderError> {
    let tools = tool_specs();
    let mut messages = vec![
        ChatMessage::System(system.to_string()),
        ChatMessage::User(user_prompt.to_string()),
    ];
    let mut in_tok = 0u64;
    let mut out_tok = 0u64;

    for _ in 0..max_iters {
        let turn = backend.turn(&messages, &tools).await?;
        in_tok += turn.input_tokens;
        out_tok += turn.output_tokens;

        if turn.tool_calls.is_empty() {
            let out = parse_lens_output(&turn.text)?;
            return Ok(LensCall {
                findings: out.findings,
                input_tokens: in_tok,
                output_tokens: out_tok,
            });
        }

        let results: Vec<ToolResult> =
            turn.tool_calls.iter().map(|c| executor.execute(c)).collect();
        messages.push(ChatMessage::Assistant {
            text: turn.text,
            tool_calls: turn.tool_calls,
        });
        messages.push(ChatMessage::ToolResults(results));
    }

    Err(ProviderError::Parse(format!(
        "tool loop exceeded {max_iters} iterations without final output"
    )))
}

/// 测试替身：按序吐预置 turns，并记录每轮收到的 messages 快照。
pub struct FakeChatBackend {
    turns: Mutex<VecDeque<AssistantTurn>>,
    seen: Mutex<Vec<Vec<ChatMessage>>>,
}

impl FakeChatBackend {
    pub fn new(turns: Vec<AssistantTurn>) -> Self {
        FakeChatBackend {
            turns: Mutex::new(turns.into()),
            seen: Mutex::new(Vec::new()),
        }
    }
    pub fn turn_count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    pub fn messages_at(&self, idx: usize) -> Vec<ChatMessage> {
        self.seen.lock().unwrap()[idx].clone()
    }
}

#[async_trait]
impl ChatBackend for FakeChatBackend {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError> {
        self.seen.lock().unwrap().push(messages.to_vec());
        self.turns
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::Unavailable("no scripted turn".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    const FINDINGS_JSON: &str = r#"{"findings":[{"severity":"high","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r"}]}"#;

    fn final_turn(text: &str) -> AssistantTurn {
        AssistantTurn { text: text.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1 }
    }

    fn toolcall_turn(name: &str, args: serde_json::Value) -> AssistantTurn {
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![ToolCall { id: "c1".into(), name: name.into(), args }],
            input_tokens: 2,
            output_tokens: 3,
        }
    }

    fn tmp_executor_with(rel: &str, body: &str) -> (tempfile::TempDir, ToolExecutor) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(rel);
        std::fs::File::create(&p).unwrap().write_all(body.as_bytes()).unwrap();
        let ex = ToolExecutor::new(dir.path());
        (dir, ex)
    }

    #[tokio::test]
    async fn final_only_returns_parsed_findings() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        let backend = FakeChatBackend::new(vec![final_turn(FINDINGS_JSON)]);
        let call = run_tool_loop(&backend, &ex, "sys", "review", 8).await.unwrap();
        assert_eq!(call.findings.len(), 1);
        assert_eq!(call.findings[0].title, "t");
        // 单轮 usage
        assert_eq!((call.input_tokens, call.output_tokens), (1, 1));
        // 没触发工具执行
        assert_eq!(backend.turn_count(), 1);
    }

    #[tokio::test]
    async fn tool_call_then_final_feeds_results_back() {
        let (_d, ex) = tmp_executor_with("a.rs", "needle here");
        let backend = FakeChatBackend::new(vec![
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            final_turn(FINDINGS_JSON),
        ]);
        let call = run_tool_loop(&backend, &ex, "sys", "review", 8).await.unwrap();
        assert_eq!(call.findings.len(), 1);
        // 两轮 usage 累加：(2+1, 3+1)
        assert_eq!((call.input_tokens, call.output_tokens), (3, 4));
        // 第二轮收到的 messages 必须含 ToolResults，且内容是真读到的文件文本
        let second = backend.messages_at(1);
        let fed = second.iter().any(|m| matches!(
            m, ChatMessage::ToolResults(rs) if rs.iter().any(|r| r.content.contains("needle here"))
        ));
        assert!(fed, "tool result not fed back: {second:?}");
    }

    #[tokio::test]
    async fn exceeding_max_iters_errors() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        // 永远只发 tool_call，从不收敛
        let backend = FakeChatBackend::new(vec![
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
        ]);
        let err = run_tool_loop(&backend, &ex, "sys", "review", 2).await.unwrap_err();
        assert!(matches!(err, ProviderError::Parse(_)), "got: {err:?}");
    }
}
