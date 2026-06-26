use crate::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage};
use crate::provider::ProviderError;
use crate::tools::{ToolCall, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};

const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
const MAX_TOKENS: u64 = 8192;

/// 把 provider 无关的 messages 渲染成 Anthropic Messages API 请求体。
/// system 提到顶层；tool_use / tool_result 用 content 块表达。
pub(crate) fn build_anthropic_tools_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    max_tokens: u64,
) -> Value {
    let mut system = String::new();
    let mut msgs: Vec<Value> = Vec::new();
    for m in messages {
        match m {
            ChatMessage::System(s) => system = s.clone(),
            ChatMessage::User(u) => msgs.push(json!({ "role": "user", "content": u })),
            ChatMessage::Assistant { text, tool_calls } => {
                let mut blocks: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for c in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use", "id": c.id, "name": c.name, "input": c.args
                    }));
                }
                msgs.push(json!({ "role": "assistant", "content": blocks }));
            }
            ChatMessage::ToolResults(results) => {
                let blocks: Vec<Value> = results
                    .iter()
                    .map(|r| {
                        json!({
                            "type": "tool_result", "tool_use_id": r.id, "content": r.content
                        })
                    })
                    .collect();
                msgs.push(json!({ "role": "user", "content": blocks }));
            }
        }
    }
    let tool_decls: Vec<Value> = tools
        .iter()
        .map(|t| json!({ "name": t.name, "description": t.description, "input_schema": t.parameters }))
        .collect();
    json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "tools": tool_decls,
        "messages": msgs,
    })
}

/// 从 Anthropic 响应抽取这一轮：text 块拼接 + tool_use 块 → ToolCall；usage 取不到则 0。
pub(crate) fn parse_anthropic_turn(resp: &Value) -> Option<AssistantTurn> {
    let blocks = resp.get("content")?.as_array()?;
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                tool_calls.push(ToolCall {
                    id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                    name: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    args: b.get("input").cloned().unwrap_or_else(|| json!({})),
                });
            }
            _ => {}
        }
    }
    let input_tokens = resp["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = resp["usage"]["output_tokens"].as_u64().unwrap_or(0);
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens })
}

/// 直连 Anthropic 的 ChatBackend（rustls-only，沿用 direct.rs 的 HTTP 形状）。
pub struct AnthropicBackend {
    model: String,
    base_url: Option<String>,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicBackend {
    pub fn new(model: String, base_url: Option<String>, api_key: String) -> Self {
        AnthropicBackend { model, base_url, api_key, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl ChatBackend for AnthropicBackend {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError> {
        let base = self.base_url.as_deref().unwrap_or(ANTHROPIC_DEFAULT_BASE);
        let url = format!("{}/v1/messages", base.trim_end_matches('/'));
        let body = build_anthropic_tools_body(&self.model, messages, tools, MAX_TOKENS);
        let resp: Value = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?
            .json()
            .await
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
        parse_anthropic_turn(&resp)
            .ok_or_else(|| ProviderError::Parse("anthropic response missing content".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tool_specs;

    fn specs() -> Vec<ToolSpec> {
        tool_specs()
    }

    #[test]
    fn anthropic_body_has_system_tools_and_messages() {
        let msgs = vec![
            ChatMessage::System("sys".into()),
            ChatMessage::User("review".into()),
        ];
        let b = build_anthropic_tools_body("claude-x", &msgs, &specs(), MAX_TOKENS);
        assert_eq!(b["model"], "claude-x");
        assert_eq!(b["system"], "sys");
        // tools 用 input_schema（Anthropic 协议字段名）
        assert_eq!(b["tools"][0]["name"], "read_file");
        assert!(b["tools"][0]["input_schema"].is_object());
        // system 不进 messages；user 进
        assert_eq!(b["messages"][0]["role"], "user");
        assert_eq!(b["messages"][0]["content"], "review");
    }

    #[test]
    fn anthropic_body_renders_assistant_toolcall_and_toolresult() {
        let msgs = vec![
            ChatMessage::User("review".into()),
            ChatMessage::Assistant {
                text: "looking".into(),
                tool_calls: vec![ToolCall {
                    id: "tu1".into(),
                    name: "read_file".into(),
                    args: json!({ "path": "a.rs" }),
                }],
            },
            ChatMessage::ToolResults(vec![crate::tools::ToolResult {
                id: "tu1".into(),
                content: "file body".into(),
            }]),
        ];
        let b = build_anthropic_tools_body("m", &msgs, &specs(), MAX_TOKENS);
        // assistant 消息含 tool_use 块
        let asst = &b["messages"][1];
        assert_eq!(asst["role"], "assistant");
        let tu = asst["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "tool_use")
            .unwrap();
        assert_eq!(tu["id"], "tu1");
        assert_eq!(tu["name"], "read_file");
        assert_eq!(tu["input"]["path"], "a.rs");
        // tool_result 作为 user 消息块，tool_use_id 对应
        let tr = &b["messages"][2];
        assert_eq!(tr["role"], "user");
        assert_eq!(tr["content"][0]["type"], "tool_result");
        assert_eq!(tr["content"][0]["tool_use_id"], "tu1");
        assert_eq!(tr["content"][0]["content"], "file body");
    }

    #[test]
    fn parse_anthropic_tool_use_turn() {
        let resp = json!({
            "content": [
                { "type": "text", "text": "let me look" },
                { "type": "tool_use", "id": "tu9", "name": "read_file", "input": { "path": "a.rs" } }
            ],
            "usage": { "input_tokens": 10, "output_tokens": 20 }
        });
        let t = parse_anthropic_turn(&resp).unwrap();
        assert_eq!(t.text, "let me look");
        assert_eq!(t.tool_calls.len(), 1);
        assert_eq!(t.tool_calls[0].id, "tu9");
        assert_eq!(t.tool_calls[0].name, "read_file");
        assert_eq!(t.tool_calls[0].args["path"], "a.rs");
        assert_eq!((t.input_tokens, t.output_tokens), (10, 20));
    }

    #[test]
    fn parse_anthropic_final_text_turn() {
        let resp = json!({
            "content": [{ "type": "text", "text": "{\"findings\":[]}" }],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        });
        let t = parse_anthropic_turn(&resp).unwrap();
        assert!(t.tool_calls.is_empty());
        assert_eq!(t.text, "{\"findings\":[]}");
    }

    #[test]
    fn parse_anthropic_missing_content_is_none() {
        assert!(parse_anthropic_turn(&json!({ "id": "x" })).is_none());
    }
}
