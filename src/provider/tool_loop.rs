use crate::provider::direct::parse_lens_output;
use crate::provider::{LensCall, LensRequest, ProviderError, ReviewProvider};
use crate::tools::{tool_specs, ToolCall, ToolExecutor, ToolResult, ToolSpec};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::path::PathBuf;
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

    /// 强制收尾轮：**不**提供 tools，要求模型仅基于已收集上下文产出最终 findings JSON。
    /// 默认实现＝不带 tools 调 `turn`（无工具可调→模型自然产出最终文本）。OpenAI 兼容
    /// backend override 以额外带 `response_format=json_object`（探索轮不带、仅收尾带——
    /// 中间轮带 json_object 会抑制部分 provider 的 function calling，见 dogfood 反转教训）。
    async fn turn_final(
        &self,
        messages: &[ChatMessage],
    ) -> Result<AssistantTurn, ProviderError> {
        self.turn(messages, &[]).await
    }
}

/// 多轮工具循环：发→若 tool_calls 则执行回灌→直至最终文本→解析为 findings。
/// usage 跨轮累加；超过 max_iters 仍未收敛则 Parse 错误。
/// `per_turn` 为单轮最长等待时间；超时返回 `ProviderError::Timeout`。
pub async fn run_tool_loop(
    backend: &dyn ChatBackend,
    executor: &ToolExecutor,
    system: &str,
    user_prompt: &str,
    max_iters: u32,
    per_turn: std::time::Duration,
    retry_budget: u32,
) -> Result<LensCall, ProviderError> {
    let tools = tool_specs();
    let mut messages = vec![
        ChatMessage::System(system.to_string()),
        ChatMessage::User(user_prompt.to_string()),
    ];
    let mut in_tok = 0u64;
    let mut out_tok = 0u64;

    for _ in 0..max_iters {
        let turn = crate::provider::retry_classified(retry_budget, || async {
            match tokio::time::timeout(per_turn, backend.turn(&messages, &tools)).await {
                Ok(r) => r,
                Err(_) => Err(ProviderError::Timeout(
                    format!("turn timeout after {}s", per_turn.as_secs()))),
            }
        }).await?;
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

    // 探索预算耗尽仍未自行收敛：发一轮强制收尾——不带 tools +（OpenAI）json_object，
    // 要求模型基于已收集上下文仅产出最终 findings JSON。这把「function-calling 探索」与
    // 「structured-output 收尾」分阶段，避免中间轮强制 JSON 抑制工具调用（dogfood 反转教训）。
    messages.push(ChatMessage::User(FINALIZE_PROMPT.to_string()));
    let turn = crate::provider::retry_classified(retry_budget, || async {
        match tokio::time::timeout(per_turn, backend.turn_final(&messages)).await {
            Ok(r) => r,
            Err(_) => Err(ProviderError::Timeout(
                format!("turn timeout after {}s", per_turn.as_secs()))),
        }
    }).await?;
    in_tok += turn.input_tokens;
    out_tok += turn.output_tokens;
    let out = parse_lens_output(&turn.text)?;
    Ok(LensCall {
        findings: out.findings,
        input_tokens: in_tok,
        output_tokens: out_tok,
    })
}

const DEFAULT_MAX_ITERS: u32 = 12;
const FINALIZE_PROMPT: &str = "You have used your tool-call budget. Based ONLY on what you have \
     already gathered, output the final review now as a single JSON object with a \"findings\" \
     array matching the requested schema. Do not call any more tools — respond with JSON only.";
const DEFAULT_SYSTEM: &str =
    "You are an adversarial code reviewer. Use the provided tools to read and search the \
     codebase as needed, then return ONLY JSON matching the requested schema (an object with a \
     \"findings\" array).";
const PER_TURN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(150);

/// 把一个 ChatBackend 与一个 ToolExecutor 组合成可直接喂给编排层的 ReviewProvider。
pub struct ToolLoopProvider {
    backend: Box<dyn ChatBackend>,
    executor: ToolExecutor,
    system: String,
    max_iters: u32,
    per_turn: std::time::Duration,
    retry_budget: u32,
}

impl ToolLoopProvider {
    pub fn new(backend: Box<dyn ChatBackend>, root: PathBuf) -> Self {
        ToolLoopProvider {
            backend,
            executor: ToolExecutor::new(root),
            system: DEFAULT_SYSTEM.to_string(),
            max_iters: DEFAULT_MAX_ITERS,
            per_turn: PER_TURN_DEADLINE,
            retry_budget: crate::provider::DEFAULT_RETRY_BUDGET,
        }
    }
}

#[async_trait]
impl ReviewProvider for ToolLoopProvider {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        run_tool_loop(
            self.backend.as_ref(),
            &self.executor,
            &self.system,
            &req.prompt,
            self.max_iters,
            self.per_turn,
            self.retry_budget,
        )
        .await
    }
}

/// 测试替身：按序吐预置 turns（可脚本化 Err），并记录每轮收到的 messages 快照。
pub struct FakeChatBackend {
    turns: Mutex<VecDeque<Result<AssistantTurn, ProviderError>>>,
    seen: Mutex<Vec<Vec<ChatMessage>>>,
}

impl FakeChatBackend {
    pub fn new(turns: Vec<AssistantTurn>) -> Self {
        FakeChatBackend {
            turns: Mutex::new(turns.into_iter().map(Ok).collect()),
            seen: Mutex::new(Vec::new()),
        }
    }
    pub fn new_results(turns: Vec<Result<AssistantTurn, ProviderError>>) -> Self {
        FakeChatBackend { turns: Mutex::new(turns.into()), seen: Mutex::new(Vec::new()) }
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
            .unwrap_or_else(|| Err(ProviderError::Unavailable("no scripted turn".into())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{LensRequest, ProviderError, ReviewProvider};
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
        let call = run_tool_loop(&backend, &ex, "sys", "review", 8, std::time::Duration::from_secs(60), 0).await.unwrap();
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
        let call = run_tool_loop(&backend, &ex, "sys", "review", 8, std::time::Duration::from_secs(60), 0).await.unwrap();
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
    async fn tool_loop_provider_is_drop_in_review_provider() {
        let (_d, ex_root) = {
            let dir = tempfile::tempdir().unwrap();
            std::fs::File::create(dir.path().join("a.rs"))
                .unwrap()
                .write_all(b"needle here")
                .unwrap();
            let root = dir.path().to_path_buf();
            (dir, root)
        };
        let backend = FakeChatBackend::new(vec![
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            final_turn(FINDINGS_JSON),
        ]);
        let provider = ToolLoopProvider::new(Box::new(backend), ex_root);
        // 经 trait 对象调用，证明 drop-in
        let p: &dyn ReviewProvider = &provider;
        let call = p
            .run(LensRequest { model: "any".into(), prompt: "review".into() })
            .await
            .unwrap();
        assert_eq!(call.findings.len(), 1);
        assert_eq!((call.input_tokens, call.output_tokens), (3, 4));
    }

    #[tokio::test]
    async fn exhausted_exploration_forces_finalize() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        // 探索预算 2 轮都只发 tool_call（不自行收敛）；耗尽后强制收尾轮拿到 findings。
        let backend = FakeChatBackend::new(vec![
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            final_turn(FINDINGS_JSON), // turn_final（默认实现走 turn）消费这个
        ]);
        let call = run_tool_loop(&backend, &ex, "sys", "review", 2, std::time::Duration::from_secs(60), 0).await.unwrap();
        assert_eq!(call.findings.len(), 1, "强制收尾应产出 findings 而非报错");
        // usage 跨探索 2 轮 + 收尾 1 轮累加：(2+2+1, 3+3+1)
        assert_eq!((call.input_tokens, call.output_tokens), (5, 7));
        // 收尾轮 messages 必须含 FINALIZE 指令
        let last = backend.messages_at(backend.turn_count() - 1);
        assert!(
            last.iter().any(|m| matches!(m, ChatMessage::User(u) if u.contains("JSON only"))),
            "finalize prompt not present: {last:?}"
        );
    }

    struct HangBackend;
    #[async_trait]
    impl ChatBackend for HangBackend {
        async fn turn(&self, _m: &[ChatMessage], _t: &[ToolSpec]) -> Result<AssistantTurn, ProviderError> {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            unreachable!("should be cut by per-turn timeout")
        }
    }

    #[tokio::test]
    async fn per_turn_timeout_returns_err_not_hang() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        let r = run_tool_loop(&HangBackend, &ex, "sys", "review", 12,
                              std::time::Duration::from_millis(30), 0).await;
        assert!(matches!(r, Err(ProviderError::Timeout(_))), "got: {r:?}");
    }

    // 收尾轮 timeout 覆盖：探索轮正常返回 tool_call、耗尽 max_iters 后进收尾轮 turn_final 挂起。
    struct HangFinalBackend;
    #[async_trait]
    impl ChatBackend for HangFinalBackend {
        async fn turn(&self, _m: &[ChatMessage], _t: &[ToolSpec]) -> Result<AssistantTurn, ProviderError> {
            Ok(AssistantTurn {
                text: String::new(),
                tool_calls: vec![ToolCall { id: "c1".into(), name: "read_file".into(), args: json!({"path":"a.rs"}) }],
                input_tokens: 1, output_tokens: 1,
            })
        }
        async fn turn_final(&self, _m: &[ChatMessage]) -> Result<AssistantTurn, ProviderError> {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            unreachable!("finalize turn should be cut by per-turn timeout")
        }
    }

    #[tokio::test]
    async fn per_turn_timeout_covers_finalize_round() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        // max_iters=1：探索轮 1 次 tool_call → 进收尾轮 turn_final（挂起）→ 收尾轮 timeout 切断
        let r = run_tool_loop(&HangFinalBackend, &ex, "sys", "review", 1,
                              std::time::Duration::from_millis(30), 0).await;
        assert!(matches!(r, Err(ProviderError::Timeout(_))), "finalize timeout: {r:?}");
    }

    #[tokio::test]
    async fn transient_turn_error_retried_keeps_accumulated_context() {
        let (_d, ex) = tmp_executor_with("a.rs", "needle here");
        // turn 序列：tool_call → 瞬时 Timeout（可重试）→ final_json
        let backend = FakeChatBackend::new_results(vec![
            Ok(toolcall_turn("read_file", json!({ "path": "a.rs" }))),
            Err(ProviderError::Timeout("blip".into())),
            Ok(final_turn(FINDINGS_JSON)),
        ]);
        // retry_budget=1 足以吞掉 1 次瞬时错
        let call = run_tool_loop(&backend, &ex, "sys", "review", 8,
            std::time::Duration::from_secs(60), 1).await.unwrap();
        assert_eq!(call.findings.len(), 1, "瞬时错应被 turn 级重试吞掉、最终产 findings");
        // 第 3 次（重试的那一 turn）收到的 messages 必须仍含 turn 1 的 ToolResults —— 累积上下文未丢
        let retried = backend.messages_at(2);
        assert!(retried.iter().any(|m| matches!(m,
            ChatMessage::ToolResults(rs) if rs.iter().any(|r| r.content.contains("needle here")))),
            "重试轮丢了已累积上下文: {retried:?}");
    }

    #[tokio::test]
    async fn non_retryable_turn_error_returns_immediately() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        let backend = FakeChatBackend::new_results(vec![Err(ProviderError::Auth("401".into()))]);
        let r = run_tool_loop(&backend, &ex, "sys", "review", 8,
            std::time::Duration::from_secs(60), 3).await;
        assert!(matches!(r, Err(ProviderError::Auth(_))), "不可重试错应立即返回: {r:?}");
    }
}
