use crate::config::{ProviderConfig, ProviderKind};
use crate::provider::parse::error_body_to_provider_error;
use crate::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage};
use crate::provider::ProviderError;
use crate::tools::{ToolCall, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};

const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";
/// 单轮输出上限（仅 anthropic 协议必填 max_tokens 时注入；openai body 不传、走模型默认）。
/// 思考型模型的 thinking tokens 也计入此额度，8192 旧值在大 bundle 评审下会被截断
/// （finish_reason=max_tokens，findings JSON 解析失败，2026-09-03 dogfood 实证）。
/// env `REDTEAM_MAX_TOKENS` 可覆盖，对齐 REDTEAM_HTTP_TIMEOUT_SECS/per_turn 旋钮先例。
const MAX_TOKENS: u64 = 32768;

fn max_tokens() -> u64 {
    match std::env::var("REDTEAM_MAX_TOKENS") {
        Ok(s) => match s.parse::<u64>() {
            Ok(v) => v,
            Err(_) => {
                // F4：静默回落会让运维误以为旋钮已生效，复发时被迫活体复现调试。
                eprintln!("warning: REDTEAM_MAX_TOKENS={s:?} 非法（需正整数），回落默认 {MAX_TOKENS}");
                MAX_TOKENS
            }
        },
        Err(_) => MAX_TOKENS,
    }
}

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
    let mut body = serde_json::Map::new();
    body.insert("model".into(), json!(model));
    body.insert("max_tokens".into(), json!(max_tokens));
    // system 为空时省略该键，与 OpenAI 的 tool_calls 省略策略保持对称。
    if !system.is_empty() {
        body.insert("system".into(), json!(system));
    }
    body.insert("tools".into(), json!(tool_decls));
    // L1 缓存：最后一个 message 的最后一个 content block 打 ephemeral 断点——前缀顺序
    // tools→system→messages，断点在 messages 末尾即缓存整个前缀（tools+system+全部历史）。
    // tool-loop append-only 确定性渲染，前缀天然稳定：每轮把断点移到新末尾，命中前一轮写的前缀、
    // 只付增量写入，13 轮对话输入成本理论折减 ~80-90%（5min TTL 覆盖 cell 内轮间隔）。
    // 当前三 provider 全 openai kind，此 body 走不到；为将来配 Anthropic 直连即生效而备。
    if let Some(last) = msgs.last_mut() {
        let is_string = last.get("content").map(Value::is_string).unwrap_or(false);
        if is_string {
            // User 消息 content 是裸 string → 转带 cache_control 的 text block 数组
            let text = last["content"].as_str().unwrap_or("").to_string();
            last["content"] = json!([{ "type": "text", "text": text, "cache_control": { "type": "ephemeral" } }]);
        } else if let Some(Value::Array(blocks)) = last.get_mut("content") {
            if let Some(obj) = blocks.last_mut().and_then(Value::as_object_mut) {
                obj.insert("cache_control".into(), json!({ "type": "ephemeral" }));
            }
        }
    }
    body.insert("messages".into(), json!(msgs));
    // 流式：与 OpenAI backend 同理——非 stream 长思考期连接静默，被隧道/网关当空闲 reset、
    // 或撞 Cloudflare 524（~100s 无首字节，2026-09-03 GLM/Kimi dogfood 实证）。stream 让服务器
    // 随 token 增量发、reqwest 随到达消费 → 连接持续活跃。Anthropic 协议无 stream_options；
    // usage 由 message_start（input/cache 分列）/message_delta（output）事件携带。
    body.insert("stream".into(), json!(true));
    Value::Object(body)
}

/// body 是否星 SSE 形态：任意行**行首** `data:`（trim 后）。行首锚定比子串 `contains`
/// 强得多——JSON 值里的 `"data": `、YAML 键、代码引文都不会裸置行首，免疫 F1 误路由。
pub(crate) fn looks_like_sse(body: &str) -> bool {
    body.lines().any(|l| l.trim_start().starts_with("data:"))
}

/// SSE 帧内 error 事件探测（D1 延续）：只扫行首 `data:` 行里 `type=="error"` 的事件，
/// 复用 `error_body_to_provider_error` 语义分类（Anthropic error 形状 `{"type":"error","error":{...}}`
/// 与 OpenAI 形状共享顶层 `error` 对象）。非 SSE 整体错误体由 turn 层「整体 JSON 优先」
/// 分支消化（F1）。正常流（无 error 事件）→ None 不干预。
fn sse_error_event(body: &str) -> Option<ProviderError> {
    body.lines()
        .filter_map(|l| l.trim_start().strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("error"))
        .find_map(|v| error_body_to_provider_error(&v))
}

/// 累积 Anthropic 流式（SSE）响应为一轮 AssistantTurn：`text_delta` 拼接、按 content_block
/// index 聚合 tool_use（id/name 由 block_start 首现即取、`input_json_delta.partial_json` 逐片
/// 拼接后整体 from_str）、usage 取 message_start（input/cache 分列，归一加回合成总输入）与
/// message_delta（stop_reason/output 末现优先）。`thinking_delta` 不计入输出。
/// 非 SSE 回退：端点忽略 `stream` 直接返完整 JSON 对象时按非流式解析。
/// 与 OpenAI 版同假设：一 `data:` 行一 JSON 事件。
pub(crate) fn parse_anthropic_stream(body: &str) -> Option<AssistantTurn> {
    // 回退：非 SSE（端点无视 stream）→ 当完整 JSON 对象解析。行首锚定（F1）而非子串。
    if !looks_like_sse(body) {
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            return parse_anthropic_turn(&v);
        }
    }
    let mut text = String::new();
    // content_block index -> (tool id, tool name, partial_json 拼接)
    let mut frags: std::collections::BTreeMap<u64, (String, String, String)> =
        std::collections::BTreeMap::new();
    let mut finish_reason: Option<String> = None;
    let mut input_tokens = 0u64;
    let mut cache_read = 0u64;
    let mut cache_creation = 0u64;
    let mut output_tokens = 0u64;
    let mut saw_event = false;
    for line in body.lines() {
        let data = match line.trim().strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue,
        };
        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue, // ping/心跳/空白——跳过
        };
        saw_event = true;
        match chunk.get("type").and_then(Value::as_str) {
            // 官方流仅一个 message_start，携带完整 input/cache 分列 usage
            Some("message_start") => {
                let u = &chunk["message"]["usage"];
                input_tokens = u["input_tokens"].as_u64().unwrap_or(0);
                cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                cache_creation = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
            }
            Some("content_block_start") => {
                // tool_use 块：id/name 首现即取（block_start 携带，input 恒为空对象）
                if chunk["content_block"]["type"].as_str() == Some("tool_use") {
                    let e = frags.entry(chunk["index"].as_u64().unwrap_or(0)).or_default();
                    if let Some(id) = chunk["content_block"]["id"].as_str() {
                        if !id.is_empty() {
                            e.0 = id.to_string();
                        }
                    }
                    if let Some(n) = chunk["content_block"]["name"].as_str() {
                        if !n.is_empty() {
                            e.1 = n.to_string();
                        }
                    }
                }
            }
            Some("content_block_delta") => {
                let idx = chunk["index"].as_u64().unwrap_or(0);
                let d = &chunk["delta"];
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => {
                        if let Some(t) = d.get("text").and_then(Value::as_str) {
                            text.push_str(t);
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(p) = d.get("partial_json").and_then(Value::as_str) {
                            frags.entry(idx).or_default().2.push_str(p);
                        }
                    }
                    _ => {} // thinking_delta 等不计入输出
                }
            }
            // 末现优先：stop_reason 与累积 output_tokens 官方由 message_delta 携带
            Some("message_delta") => {
                if let Some(fr) = chunk["delta"]["stop_reason"].as_str() {
                    finish_reason = Some(fr.to_string());
                }
                if let Some(o) = chunk["usage"]["output_tokens"].as_u64() {
                    output_tokens = o;
                }
            }
            _ => {} // message_stop/ping/error（turn 层先行探测）等
        }
    }
    if !saw_event {
        return None; // 无一事件解析成功 → 让上层判 Parse/Unavailable
    }
    let mut malformed_tool_args = false;
    let tool_calls: Vec<ToolCall> = frags
        .into_values()
        .filter(|(_, name, _)| !name.is_empty())
        .map(|(id, name, args)| {
            // 空 args = 无参调用合法落 {}；非空但解析失败 = 流/输出不完整（F2）——
            // 不静默吞成 {} 让工具带空参跑垃圾，置 malformed 传导给 turn 层按截断处理。
            let args = if args.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&args).unwrap_or_else(|_| {
                    malformed_tool_args = true;
                    json!({})
                })
            };
            ToolCall { id, name, args }
        })
        .collect();
    // 截断/畸形传导（F2）：turn 层见 finish_reason=None → Unavailable 可重试；
    // 宁可重试不烧格（与 error_body_to_provider_error 未知类型归 Unavailable 同哲学）。
    let finish_reason = if malformed_tool_args { None } else { finish_reason };
    // 与 parse_anthropic_turn 同归一：input_tokens 是「未缓存新输入」，加回 cache 分列合成「总输入」
    let input_tokens = input_tokens + cache_read + cache_creation;
    Some(AssistantTurn {
        text,
        tool_calls,
        input_tokens,
        output_tokens,
        cached_input_tokens: cache_read,
        finish_reason,
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
    // Anthropic 归一：usage.input_tokens 是「未缓存新输入」，cache_read/cache_creation 分列不含在内。
    // 加回合成「总输入」，与 OpenAI/DeepSeek 的 prompt_tokens（本含 cached）语义对齐。
    let u = &resp["usage"];
    let cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
    let cache_creation = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
    let input_tokens = u["input_tokens"].as_u64().unwrap_or(0) + cache_read + cache_creation;
    let output_tokens = u["output_tokens"].as_u64().unwrap_or(0);
    let cached_input_tokens = cache_read;
    let finish_reason = resp["stop_reason"].as_str().map(String::from);
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens, cached_input_tokens, finish_reason })
}

/// 直连 Anthropic 的 ChatBackend（rustls-only）。
pub struct AnthropicBackend {
    model: String,
    base_url: Option<String>,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicBackend {
    pub fn new(model: String, base_url: Option<String>, api_key: String) -> Self {
        AnthropicBackend { model, base_url, api_key, client: crate::provider::default_http_client() }
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
        let url = super::anthropic_messages_url(base);
        let body = build_anthropic_tools_body(&self.model, messages, tools, max_tokens());
        let resp = super::send_classified(
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body),
        )
        .await?;
        // 读 SSE 全文（随到达消费）：reqwest 持续读 socket → 连接持续活跃，网关/隧道不当空闲
        // reset、不撞 524（与 OpenAiBackend::post 同构）。中途断流 → Unavailable（可重试）。
        let sse = resp
            .text()
            .await
            .map_err(|e| ProviderError::Unavailable(format!("stream body read failed for {url}: {e}")))?;
        if sse.trim().is_empty() {
            // 空体（keep-alive/上游放弃）→ 可重试，勿误判 Parse 烧格
            return Err(ProviderError::Unavailable(format!("empty stream body for {url}")));
        }
        // 三层分流（F1：整体 JSON 优先，免疫「正文含 data: 子串的 JSON 被误路由到 SSE
        // 解析器 → saw_event=false → 误判不可重试 Parse 烧 cell」）：
        // 层1 整体 JSON（端点无视 stream 的完整响应、200-内-error 体、含 data: 子串的 JSON）
        // 层2 SSE 流（行首 data: 锚定）
        // 层3 兜底 envelope（HTML 网关页等非法 JSON → Unavailable 带 preview 可重试）
        if let Ok(v) = serde_json::from_str::<Value>(sse.trim()) {
            if let Some(e) = error_body_to_provider_error(&v) {
                return Err(e);
            }
            return parse_anthropic_turn(&v)
                .ok_or_else(|| ProviderError::Parse("anthropic response missing content".into()));
        }
        if looks_like_sse(&sse) {
            // SSE 帧：先探帧内 error 事件（D1 的流式版），再聚合事件流。
            if let Some(e) = sse_error_event(&sse) {
                return Err(e);
            }
            let t = parse_anthropic_stream(&sse)
                .ok_or_else(|| ProviderError::Parse("anthropic stream missing content".into()))?;
            // F2 完整性门：finish_reason None = message_delta 未达（流中途断/网关放弃）
            // 或 tool args 畸形（parse 内传导）→ 按传输层不完整走可重试 Unavailable，
            // 不把半截产出当完整轮烧成 Parse——repair 轮救不了断流，重试才救得了。
            if t.finish_reason.is_none() {
                return Err(ProviderError::Unavailable(format!(
                    "truncated anthropic stream for {url}: no finish_reason"
                )));
            }
            return Ok(t);
        }
        Err(super::parse_envelope_json(&sse, &url).unwrap_err())
    }
}

/// 把 messages 渲染成 OpenAI 兼容 Chat Completions 请求体。
/// system/user 直接成消息；assistant tool_calls 用 function 形状；tool 结果用 role:"tool"。
pub(crate) fn build_openai_tools_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    json_mode: bool,
) -> Value {
    let mut msgs: Vec<Value> = Vec::new();
    for m in messages {
        match m {
            ChatMessage::System(s) => msgs.push(json!({ "role": "system", "content": s })),
            ChatMessage::User(u) => msgs.push(json!({ "role": "user", "content": u })),
            ChatMessage::Assistant { text, tool_calls } => {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|c| json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.args.to_string() }
                    }))
                    .collect();
                let mut obj = serde_json::Map::new();
                obj.insert("role".into(), json!("assistant"));
                obj.insert(
                    "content".into(),
                    if text.is_empty() { Value::Null } else { json!(text) },
                );
                // OpenAI rejects an empty tool_calls array; omit the key entirely when there are none
                if !calls.is_empty() {
                    obj.insert("tool_calls".into(), json!(calls));
                }
                msgs.push(Value::Object(obj));
            }
            ChatMessage::ToolResults(results) => {
                for r in results {
                    msgs.push(json!({
                        "role": "tool", "tool_call_id": r.id, "content": r.content
                    }));
                }
            }
        }
    }
    let mut body = serde_json::Map::new();
    body.insert("model".into(), json!(model));
    body.insert("messages".into(), json!(msgs));
    // 探索轮带 tools；收尾轮（tools 空）省略该键。
    if !tools.is_empty() {
        let tool_decls: Vec<Value> = tools
            .iter()
            .map(|t| json!({
                "type": "function",
                "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters
                }
            }))
            .collect();
        body.insert("tools".into(), json!(tool_decls));
    }
    // response_format=json_object 仅在强制收尾轮加（不带 tools 时），保证最终 content 是
    // 合法 JSON；探索轮不加，避免抑制 function calling（dogfood 反转教训）。
    if json_mode {
        body.insert("response_format".into(), json!({ "type": "json_object" }));
    }
    // 流式：服务器随 token 增量发、reqwest 随到达消费 socket → 连接持续活跃，隧道/网关不当
    // 空闲 reset（非 stream 长思考期连接静默 = 被 reset/超时的根因）。include_usage 让末块带 usage。
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({ "include_usage": true }));
    // 显式 max_tokens：不传时兼容网关会注入自己的默认上限（api.aigocode.team 实测不够，
    // 大 bundle 评审被截成 finish_reason=max_tokens，findings JSON 解析失败——2026-09-03
    // PR181 dogfood 实证）。与 anthropic 路径同一旋钮（REDTEAM_MAX_TOKENS，默认 32768）。
    body.insert("max_tokens".into(), json!(max_tokens()));
    Value::Object(body)
}

/// 从 OpenAI 兼容响应抽取这一轮：message.content（可空）+ tool_calls（arguments 是 JSON 字符串）。
pub(crate) fn parse_openai_turn(resp: &Value) -> Option<AssistantTurn> {
    let msg = resp.get("choices")?.as_array()?.first()?.get("message")?;
    let text = msg.get("content").and_then(Value::as_str).unwrap_or("").to_string();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for c in calls {
            let f = match c.get("function") {
                Some(f) => f,
                None => continue,
            };
            let args_str = f.get("arguments").and_then(Value::as_str).unwrap_or("{}");
            let args = serde_json::from_str::<Value>(args_str).unwrap_or_else(|_| json!({}));
            tool_calls.push(ToolCall {
                id: c.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: f.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                args,
            });
        }
    }
    let u = &resp["usage"];
    let input_tokens = u["prompt_tokens"].as_u64().unwrap_or(0);
    let output_tokens = u["completion_tokens"].as_u64().unwrap_or(0);
    // 缓存命中量 fallback 链（同协议多方言）：OpenAI 嵌套 → DeepSeek top-level → Kimi/通用 top-level。
    // 全不中记 0（可观测缺失：cached=0 且 input 大 = 未命中或字段未知，不假装省钱）。prompt_tokens
    // 本含 cached，input_tokens 已是「总输入」，无需加法（区别于 Anthropic）。
    let cached_input_tokens = u["prompt_tokens_details"]["cached_tokens"].as_u64()
        .or_else(|| u["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| u["cached_tokens"].as_u64())
        .unwrap_or(0);
    let finish_reason = resp["choices"][0]["finish_reason"].as_str().map(String::from);
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens, cached_input_tokens, finish_reason })
}

/// 累积 OpenAI 流式（SSE）响应为一轮 AssistantTurn：拼 `delta.content`、按 index 合并
/// `delta.tool_calls` 分片（id/name 首现即取、arguments 逐片拼接）、取末现 finish_reason 与 usage。
/// 非 SSE 回退：端点忽略 `stream` 直接返完整 JSON 对象时按非流式解析。
/// 假设 OpenAI 风格「一 `data:` 行一 JSON chunk」（多行 data 字段不支持，各家实测未用）。
pub(crate) fn parse_openai_stream(body: &str) -> Option<AssistantTurn> {
    // 回退：非 SSE（端点无视 stream）→ 当完整 JSON 对象解析。行首锚定（F1）而非子串。
    if !looks_like_sse(body) {
        if let Ok(v) = serde_json::from_str::<Value>(body) {
            return parse_openai_turn(&v);
        }
    }
    let mut text = String::new();
    // tool_call index -> (id, name, arguments 拼接)
    let mut frags: std::collections::BTreeMap<i64, (String, String, String)> = std::collections::BTreeMap::new();
    let mut finish_reason: Option<String> = None;
    let mut usage = Value::Null;
    let mut saw_chunk = false;
    for line in body.lines() {
        let data = match line.trim().strip_prefix("data:") {
            Some(d) => d.trim(),
            None => continue,
        };
        if data == "[DONE]" {
            break;
        }
        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue, // 心跳/空白/半行——跳过
        };
        saw_chunk = true;
        if let Some(u) = chunk.get("usage") {
            if !u.is_null() {
                usage = u.clone(); // 末块（include_usage）带 usage
            }
        }
        let choice = match chunk.get("choices").and_then(Value::as_array).and_then(|a| a.first()) {
            Some(c) => c,
            None => continue, // usage-only 末块无 choices
        };
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            finish_reason = Some(fr.to_string());
        }
        let delta = match choice.get("delta") {
            Some(d) => d,
            None => continue,
        };
        if let Some(c) = delta.get("content").and_then(Value::as_str) {
            text.push_str(c); // 思考型模型 native 格式的 <think> 亦在此、由 parse_lens_output 剥离
        }
        if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
            for tc in tcs {
                let idx = tc.get("index").and_then(Value::as_i64).unwrap_or(0);
                let e = frags.entry(idx).or_default();
                if let Some(id) = tc.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        e.0 = id.to_string();
                    }
                }
                if let Some(f) = tc.get("function") {
                    if let Some(n) = f.get("name").and_then(Value::as_str) {
                        if !n.is_empty() {
                            e.1 = n.to_string();
                        }
                    }
                    if let Some(a) = f.get("arguments").and_then(Value::as_str) {
                        e.2.push_str(a);
                    }
                }
            }
        }
    }
    if !saw_chunk {
        return None; // 无一 chunk 解析成功 → 让上层判 Parse/Unavailable
    }
    let tool_calls: Vec<ToolCall> = frags
        .into_values()
        .filter(|(_, name, _)| !name.is_empty())
        .map(|(id, name, args)| ToolCall {
            id,
            name,
            args: serde_json::from_str(&args).unwrap_or_else(|_| json!({})),
        })
        .collect();
    let input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
    let output_tokens = usage["completion_tokens"].as_u64().unwrap_or(0);
    let cached_input_tokens = usage["prompt_tokens_details"]["cached_tokens"].as_u64()
        .or_else(|| usage["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| usage["cached_tokens"].as_u64())
        .unwrap_or(0);
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens, cached_input_tokens, finish_reason })
}

/// 直连 OpenAI 兼容端点的 ChatBackend。
pub struct OpenAiBackend {
    model: String,
    base_url: Option<String>,
    api_key: String,
    client: reqwest::Client,
}

impl OpenAiBackend {
    pub fn new(model: String, base_url: Option<String>, api_key: String) -> Self {
        OpenAiBackend { model, base_url, api_key, client: crate::provider::default_http_client() }
    }

    /// 共享 HTTP 发送：POST {base}/chat/completions（流式），累积 SSE 解析这一轮输出。
    /// 流式消费的意义：reqwest 随 token 到达读 socket → 连接持续有数据流，隧道/网关不当空闲
    /// reset（非 stream 长思考期连接静默 = 超时/reset 根因）。
    async fn post(&self, body: &Value) -> Result<AssistantTurn, ProviderError> {
        let base = self.base_url.as_deref().unwrap_or(OPENAI_DEFAULT_BASE);
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let resp = super::send_classified(
            self.client
                .post(&url)
                .header("authorization", format!("Bearer {}", self.api_key))
                .header("content-type", "application/json")
                .json(body),
        )
        .await?;
        // 读 SSE 全文（随到达消费）。中途断流 → Unavailable（可重试），与 decode_json 一致。
        let sse = resp
            .text()
            .await
            .map_err(|e| ProviderError::Unavailable(format!("stream body read failed for {url}: {e}")))?;
        if sse.trim().is_empty() {
            // 空体（keep-alive/上游放弃）→ 可重试，勿误判 Parse 烧格
            return Err(ProviderError::Unavailable(format!("empty stream body for {url}")));
        }
        // 三层分流（F1 镜像 anthropic 侧）：层1 整体 JSON 优先——error 体、无视 stream 的
        // 网关、以及正文含 data: 子串的 JSON（旧 `!sse.contains("data:")` 子串判定会把它误
        // 路由到 SSE 解析器烧 cell）都在此层消化；层2 行首 data: 锚定的 SSE；层3 envelope 兜底。
        if let Ok(v) = serde_json::from_str::<Value>(sse.trim()) {
            // D1：网关把错误塞进 HTTP 200 body——在 parse 前探 error 形状，避免误判 Parse 烧格。
            if let Some(e) = error_body_to_provider_error(&v) {
                return Err(e);
            }
            return parse_openai_turn(&v)
                .ok_or_else(|| ProviderError::Parse("openai response missing choices".into()));
        }
        if looks_like_sse(&sse) {
            return parse_openai_stream(&sse)
                .ok_or_else(|| ProviderError::Parse("openai stream missing choices".into()));
        }
        Err(super::parse_envelope_json(&sse, &url).unwrap_err())
    }
}

#[async_trait]
impl ChatBackend for OpenAiBackend {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError> {
        // 探索轮：带 tools、不强制 JSON（保留 function calling）。
        let body = build_openai_tools_body(&self.model, messages, tools, false);
        self.post(&body).await
    }

    async fn turn_final(
        &self,
        messages: &[ChatMessage],
    ) -> Result<AssistantTurn, ProviderError> {
        // 收尾轮：不带 tools + response_format=json_object，强制产出最终 findings JSON。
        let body = build_openai_tools_body(&self.model, messages, &[], true);
        self.post(&body).await
    }
}

/// 按 ProviderConfig.kind 造对应 backend（S2-2b 编排层入口）。Google 暂不支持。
pub fn backend_from_config(
    pc: &ProviderConfig,
    api_key: String,
) -> Result<Box<dyn ChatBackend>, ProviderError> {
    match pc.kind {
        ProviderKind::Anthropic => Ok(Box::new(AnthropicBackend::new(
            pc.model.clone(),
            pc.base_url.clone(),
            api_key,
        ))),
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => Ok(Box::new(OpenAiBackend::new(
            pc.model.clone(),
            pc.base_url.clone(),
            api_key,
        ))),
        ProviderKind::Google => Err(ProviderError::Unavailable("google not yet supported".into())),
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
        assert_eq!(b["tools"][0]["name"], "read_files");
        assert!(b["tools"][0]["input_schema"].is_object());
        // system 不进 messages；user 进。L1 缓存：末条 message 的 content 转为带 ephemeral
        // 断点的 text block 数组（裸 string User 消息形态）。
        assert_eq!(b["messages"][0]["role"], "user");
        assert_eq!(b["messages"][0]["content"][0]["text"], "review");
        assert_eq!(b["messages"][0]["content"][0]["cache_control"]["type"], "ephemeral");
    }

    // L1 缓存：多轮场景末条是 tool_result（block 数组）→ 其最后一个 block 打 ephemeral 断点，
    // 非尾 message 不受影响。
    #[test]
    fn anthropic_body_caches_last_content_block_on_toolresult_tail() {
        let msgs = vec![
            ChatMessage::User("review".into()),
            ChatMessage::ToolResults(vec![crate::tools::ToolResult { id: "tu1".into(), content: "file body".into() }]),
        ];
        let b = build_anthropic_tools_body("claude-x", &msgs, &specs(), MAX_TOKENS);
        let tail = &b["messages"][1]["content"];
        let n = tail.as_array().unwrap().len();
        assert_eq!(tail[n - 1]["cache_control"]["type"], "ephemeral", "尾 block 打断点: {b}");
        assert_eq!(tail[n - 1]["content"], "file body", "断点不破坏原 tool_result 内容: {b}");
        // 非尾 message（User）不受影响，仍是裸 string
        assert_eq!(b["messages"][0]["content"], "review", "非尾 message 不打断点: {b}");
    }

    // I1 回归：system 为空时 Anthropic 请求体中不应含 "system" 键。
    #[test]
    fn anthropic_body_omits_system_when_empty() {
        let msgs = vec![ChatMessage::User("review".into())];
        let b = build_anthropic_tools_body("claude-x", &msgs, &specs(), MAX_TOKENS);
        assert!(b.get("system").is_none(), "empty system must be omitted, got: {b}");
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

    // 观测回归：stop_reason 落进 AssistantTurn.finish_reason，供上层区分截断 vs 形状违约。
    #[test]
    fn parse_anthropic_turn_captures_finish_reason() {
        let resp = json!({
            "content": [{ "type": "text", "text": "partial" }],
            "stop_reason": "max_tokens",
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        });
        let t = parse_anthropic_turn(&resp).unwrap();
        assert_eq!(t.finish_reason, Some("max_tokens".to_string()));
    }

    #[test]
    fn parse_anthropic_turn_missing_stop_reason_is_none() {
        let resp = json!({
            "content": [{ "type": "text", "text": "hi" }],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        });
        assert_eq!(parse_anthropic_turn(&resp).unwrap().finish_reason, None);
    }

    #[test]
    fn openai_body_text_only_assistant_omits_tool_calls() {
        let msgs = vec![ChatMessage::Assistant { text: "hello".into(), tool_calls: vec![] }];
        let b = build_openai_tools_body("m", &msgs, &specs(), false);
        let asst = &b["messages"][0];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"], "hello");
        assert!(asst.get("tool_calls").is_none(), "empty tool_calls must be omitted, got: {asst}");
    }

    // 两阶段收尾回归：收尾轮（tools 空 + json_mode）省略 tools 键并带 response_format=json_object；
    // 探索轮（带 tools + json_mode=false）不带 response_format（避免抑制 function calling）。
    #[test]
    fn openai_final_body_drops_tools_and_adds_json_object() {
        let msgs = vec![ChatMessage::User("finalize".into())];
        let fin = build_openai_tools_body("gpt-x", &msgs, &[], true);
        assert!(fin.get("tools").is_none(), "收尾轮不应带 tools, got: {fin}");
        assert_eq!(fin["response_format"]["type"], "json_object");

        let explore = build_openai_tools_body("gpt-x", &msgs, &specs(), false);
        assert!(explore.get("response_format").is_none(), "探索轮不应带 response_format");
        assert!(explore.get("tools").is_some(), "探索轮应带 tools");
        // 两轮都流式（保连接活跃）+ 带 include_usage
        assert_eq!(fin["stream"], true, "收尾轮应流式");
        assert_eq!(explore["stream"], true, "探索轮应流式");
        assert_eq!(explore["stream_options"]["include_usage"], true);
    }

    #[test]
    fn parse_openai_stream_accumulates_content_toolcalls_usage() {
        // 内容分片 + tool_call 分片（id/name 首现、arguments 逐片）+ 末块 usage + [DONE]
        let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read_files\",\"arguments\":\"{\\\"pa\"}}]}}]}\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"ths\\\":[]}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\
data: {\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":20,\"prompt_tokens_details\":{\"cached_tokens\":80}}}\n\
data: [DONE]\n";
        let t = parse_openai_stream(sse).unwrap();
        assert_eq!(t.text, "hello");
        assert_eq!(t.tool_calls.len(), 1);
        assert_eq!(t.tool_calls[0].id, "call_1");
        assert_eq!(t.tool_calls[0].name, "read_files");
        assert_eq!(t.tool_calls[0].args["paths"], json!([]));
        assert_eq!(t.input_tokens, 100);
        assert_eq!(t.output_tokens, 20);
        assert_eq!(t.cached_input_tokens, 80);
        assert_eq!(t.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn anthropic_body_requests_stream() {
        let msgs = vec![ChatMessage::User("hi".into())];
        let b = build_anthropic_tools_body("claude-x", &msgs, &specs(), MAX_TOKENS);
        assert_eq!(b["stream"], true, "anthropic body 应流式（保连接活跃）");
    }

    #[test]
    fn anthropic_stream_accumulates_text_tooluse_usage() {
        // 官方事件序：message_start(input/cache 分列) → text 分片 → tool_use 块 + partial_json 分片
        // → thinking_delta(忽略) → message_delta(stop_reason + output) → message_stop
        let sse = "\
event: message_start\n\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"cache_read_input_tokens\":80,\"cache_creation_input_tokens\":10}}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"hel\"}}\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\
\n\
event: content_block_start\n\
data: {\"type\":\"content_block_start\",\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read_files\",\"input\":{}}}\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"pa\"}}\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"ths\\\":[]}\"}}\n\
data: {\"type\":\"content_block_delta\",\"index\":1,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"noise\"}}\n\
\n\
event: message_delta\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":42}}\n\
\n\
event: message_stop\n\
data: {\"type\":\"message_stop\"}\n";
        let t = parse_anthropic_stream(sse).unwrap();
        assert_eq!(t.text, "hello");
        assert_eq!(t.tool_calls.len(), 1);
        assert_eq!(t.tool_calls[0].id, "toolu_1");
        assert_eq!(t.tool_calls[0].name, "read_files");
        assert_eq!(t.tool_calls[0].args["paths"], json!([]));
        assert_eq!(t.input_tokens, 110, "input 需加回 cache 分列（20+80+10）");
        assert_eq!(t.output_tokens, 42);
        assert_eq!(t.cached_input_tokens, 80);
        assert_eq!(t.finish_reason.as_deref(), Some("tool_use"));
    }

    #[test]
    fn anthropic_stream_falls_back_to_nonstream_json() {
        // 端点无视 stream 直接返完整 JSON 对象 → 按非流式解析
        let body = "{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}],\"stop_reason\":\"end_turn\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}";
        let t = parse_anthropic_stream(body).unwrap();
        assert_eq!(t.text, "hi");
        assert_eq!(t.finish_reason.as_deref(), Some("end_turn"));
        assert_eq!(t.input_tokens, 5);
    }

    #[test]
    fn anthropic_sse_error_event_classified() {
        // SSE 帧 error 事件 → 语义分类（overloaded → RateLimited），不 Parse 烧格
        let sse = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n";
        assert!(matches!(sse_error_event(sse), Some(ProviderError::RateLimited { .. })));
        // 正常流（无 error 事件）→ None 不干预
        let ok = "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}";
        assert!(sse_error_event(ok).is_none());
        // 非 SSE 整体 JSON（含 error 体）不归此函数——turn 层「整体 JSON 优先」分支管（F1）
        let whole_json = "{\"type\":\"error\",\"error\":{\"type\":\"timeout\",\"message\":\"upstream\"}}";
        assert!(sse_error_event(whole_json).is_none());
    }

    #[test]
    fn json_body_with_data_substring_routes_to_json_not_sse() {
        // F1 核心：正文里含 "data:" 子串（YAML 键/代码引文/findings 引文）的完整 JSON
        // 响应不得被子串判定误路由进 SSE 解析器——行首锚定 + 整体 JSON 优先双保险。
        let json_with_sub = "{\"content\":[{\"type\":\"text\",\"text\":\"see data: below in yaml\"}],\"stop_reason\":\"end_turn\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}";
        assert!(!looks_like_sse(json_with_sub), "子串不在行首 → 不应判为 SSE");
        let t = parse_anthropic_stream(json_with_sub).unwrap();
        assert_eq!(t.text, "see data: below in yaml");
        let t2 = parse_openai_stream(
            "{\"choices\":[{\"message\":{\"content\":\"key data: here\"},\"finish_reason\":\"stop\"}]}",
        )
        .unwrap();
        assert_eq!(t2.text, "key data: here");
        // 真 SSE：行首 data: 锚定
        assert!(looks_like_sse("data: {\"type\":\"message_start\"}\n\n"));
        assert!(looks_like_sse("event: x\n  data: {\"a\":1}\n"), "行首空白容忍");
    }

    #[test]
    fn anthropic_stream_malformed_tool_args_flags_truncation() {
        // F2：流「完整」（message_delta 到达）但 tool args 拼接非合法 JSON → 不静默落 {}
        // 跑垃圾，而是置 finish_reason=None 传导给 turn 层按截断走可重试。
        let sse = "\
data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"read_files\",\"input\":{}}}\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"pat\"}}\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":7}}\n";
        let t = parse_anthropic_stream(sse).unwrap();
        assert_eq!(t.tool_calls.len(), 1);
        assert_eq!(t.tool_calls[0].args, json!({}), "畸形 args 仍落空对象（不跑垃圾靠门拦截）");
        assert!(t.finish_reason.is_none(), "畸形 args 必须传导为 None 供 turn 层判截断");
    }

    #[test]
    fn parse_openai_stream_falls_back_to_nonstream_json() {
        // 端点无视 stream 直接返完整 JSON 对象 → 按非流式解析
        let json = "{\"choices\":[{\"message\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}";
        let t = parse_openai_stream(json).unwrap();
        assert_eq!(t.text, "hi");
        assert_eq!(t.finish_reason.as_deref(), Some("stop"));
    }

    #[test]
    fn openai_body_has_messages_and_function_tools() {
        let msgs = vec![
            ChatMessage::System("sys".into()),
            ChatMessage::User("review".into()),
        ];
        let b = build_openai_tools_body("gpt-x", &msgs, &specs(), false);
        assert_eq!(b["model"], "gpt-x");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["role"], "user");
        // tools 用 {type:function, function:{name, parameters}}
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["function"]["name"], "read_files");
        assert!(b["tools"][0]["function"]["parameters"].is_object());
    }

    #[test]
    fn openai_body_renders_assistant_toolcall_and_tool_message() {
        let msgs = vec![
            ChatMessage::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    args: json!({ "path": "a.rs" }),
                }],
            },
            ChatMessage::ToolResults(vec![crate::tools::ToolResult {
                id: "call_1".into(),
                content: "file body".into(),
            }]),
        ];
        let b = build_openai_tools_body("m", &msgs, &specs(), false);
        let asst = &b["messages"][0];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["tool_calls"][0]["id"], "call_1");
        assert_eq!(asst["tool_calls"][0]["type"], "function");
        assert_eq!(asst["tool_calls"][0]["function"]["name"], "read_file");
        // arguments 是 JSON 字符串
        assert_eq!(asst["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a.rs\"}");
        // tool 结果消息
        let tr = &b["messages"][1];
        assert_eq!(tr["role"], "tool");
        assert_eq!(tr["tool_call_id"], "call_1");
        assert_eq!(tr["content"], "file body");
    }

    #[test]
    fn parse_openai_tool_call_turn() {
        let resp = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_7",
                        "type": "function",
                        "function": { "name": "search_repo", "arguments": "{\"pattern\":\"x\"}" }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 6 }
        });
        let t = parse_openai_turn(&resp).unwrap();
        assert_eq!(t.text, "");
        assert_eq!(t.tool_calls.len(), 1);
        assert_eq!(t.tool_calls[0].id, "call_7");
        assert_eq!(t.tool_calls[0].name, "search_repo");
        assert_eq!(t.tool_calls[0].args["pattern"], "x");
        assert_eq!((t.input_tokens, t.output_tokens), (5, 6));
    }

    #[test]
    fn parse_openai_final_text_turn() {
        let resp = json!({
            "choices": [{ "message": { "content": "{\"findings\":[]}" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let t = parse_openai_turn(&resp).unwrap();
        assert!(t.tool_calls.is_empty());
        assert_eq!(t.text, "{\"findings\":[]}");
    }

    #[test]
    fn parse_openai_missing_choices_is_none() {
        assert!(parse_openai_turn(&json!({ "id": "x" })).is_none());
    }

    // 观测回归：choices[0].finish_reason 落进 AssistantTurn.finish_reason（截断诊断关键信号）。
    #[test]
    fn parse_openai_turn_captures_finish_reason() {
        let resp = json!({
            "choices": [{ "message": { "content": "{\"findings\":" }, "finish_reason": "length" }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let t = parse_openai_turn(&resp).unwrap();
        assert_eq!(t.finish_reason, Some("length".to_string()));
    }

    #[test]
    fn parse_openai_turn_missing_finish_reason_is_none() {
        let resp = json!({
            "choices": [{ "message": { "content": "hi" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        assert_eq!(parse_openai_turn(&resp).unwrap().finish_reason, None);
    }

    #[test]
    fn parse_openai_cached_tokens_fallback_chain() {
        // OpenAI 标准嵌套路径
        let oa = json!({ "choices": [{ "message": { "content": "{}" } }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 5,
                       "prompt_tokens_details": { "cached_tokens": 40 } } });
        assert_eq!(parse_openai_turn(&oa).unwrap().cached_input_tokens, 40);
        // DeepSeek top-level（prompt_tokens 本含 cached，input_tokens 不加法）
        let ds = json!({ "choices": [{ "message": { "content": "{}" } }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 5, "prompt_cache_hit_tokens": 70 } });
        let t = parse_openai_turn(&ds).unwrap();
        assert_eq!((t.input_tokens, t.cached_input_tokens), (100, 70));
        // Kimi/通用 top-level cached_tokens 兜底
        let km = json!({ "choices": [{ "message": { "content": "{}" } }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 5, "cached_tokens": 25 } });
        assert_eq!(parse_openai_turn(&km).unwrap().cached_input_tokens, 25);
        // 无缓存字段 → 记 0（可观测缺失，不假装省钱）
        let none = json!({ "choices": [{ "message": { "content": "{}" } }],
            "usage": { "prompt_tokens": 100, "completion_tokens": 5 } });
        assert_eq!(parse_openai_turn(&none).unwrap().cached_input_tokens, 0);
    }

    #[test]
    fn parse_anthropic_cached_normalizes_total_input() {
        // Anthropic：input_tokens 是「未缓存新输入」，cache_read/creation 分列——归一为总输入。
        let resp = json!({ "content": [{ "type": "text", "text": "hi" }],
            "usage": { "input_tokens": 30, "output_tokens": 20,
                       "cache_read_input_tokens": 60, "cache_creation_input_tokens": 10 } });
        let t = parse_anthropic_turn(&resp).unwrap();
        // 总输入 = 30 + 60 + 10 = 100；命中读 = 60
        assert_eq!((t.input_tokens, t.cached_input_tokens, t.output_tokens), (100, 60, 20));
    }

    #[test]
    fn from_config_maps_kind_to_backend() {
        let anth = ProviderConfig {
            kind: ProviderKind::Anthropic, model: "m".into(),
            base_url: None, api_key_env: None, priority: 0, context_window_tokens: None,
            per_turn_timeout_secs: None,
        };
        assert!(backend_from_config(&anth, "k".into()).is_ok());
        let goog = ProviderConfig {
            kind: ProviderKind::Google, model: "m".into(),
            base_url: None, api_key_env: None, priority: 0, context_window_tokens: None,
            per_turn_timeout_secs: None,
        };
        assert!(matches!(
            backend_from_config(&goog, "k".into()),
            Err(ProviderError::Unavailable(_))
        ));
    }
}
