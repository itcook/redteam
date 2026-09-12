use crate::config::{ProviderKind, ThinkingLevel};
use crate::provider::parse::error_body_to_provider_error;
use crate::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage};
use crate::provider::ProviderError;
use crate::tools::{ToolCall, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};

const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";
/// 单轮输出上限（仅 anthropic 协议必填 max_tokens 时注入；openai body 不传、走模型默认）。
/// 默认值与 `REDTEAM_MAX_TOKENS` 解析统一在 `config::env_max_tokens`（与超时公式同链，评审 #7）。
fn max_tokens() -> u64 {
    crate::config::env_max_tokens()
}

/// Anthropic 每请求 `cache_control` 断点上限（API 硬限制）。
const ANTHROPIC_CACHE_BOUNDARIES_MAX: usize = 4;
/// 分块 user 消息里兑现的显式断点上限：留 1 个给共享头 system（三段界 = system + 证据末 + 角色尾前 = 3）。
const ANTHROPIC_EXPLICIT_BOUNDARIES_MAX: usize = ANTHROPIC_CACHE_BOUNDARIES_MAX - 1;

/// 把 provider 无关的 messages 渲染成 Anthropic Messages API 请求体。
/// system 提到顶层；tool_use / tool_result 用 content 块表达。
#[cfg(test)] // 批 F-2 后生产路径走 *_ext（带 thinking 参数）；本包装只给既有 body 测试保基线
pub(crate) fn build_anthropic_tools_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    max_tokens: u64,
) -> Value {
    build_anthropic_tools_body_ext(model, messages, tools, max_tokens, None)
}

/// 同上 + 思考档位参数对（批 F-2 `thinking_params`）；`None` / 空 → 与不带参数版逐字节相同。
pub(crate) fn build_anthropic_tools_body_ext(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    max_tokens: u64,
    thinking: Option<&[(&'static str, Value)]>,
) -> Value {
    let mut system = String::new();
    let mut msgs: Vec<Value> = Vec::new();
    // 显式断点计数（批 D-4 三段界）：有显式断点时 system 也打断点、末块不再自动加（Anthropic 每请求最多 4 个断点）。
    // 运行时守卫（合并后修补）：显式断点只兑现前 ANTHROPIC_EXPLICIT_BOUNDARIES_MAX 个（system 占第 4 个），多余的静默
    // 降为普通块——一次会话若出现多条 UserBlocks（或一条带 ≥ 4 个断点），release 下不会发出第 5 个断点被 API 400 拒掉整格。
    let mut explicit_boundaries = 0usize;
    // 末条 message 是否带「已兑现」的显式断点：是则不再自动加尾断点（断点已显式给出）；否则（普通消息 / 断点被守卫吞掉的分块）
    // 仍给尾块移动断点，tool-loop 逐轮增量缓存不因分块会话失效。
    let mut last_msg_honored = false;
    for m in messages {
        last_msg_honored = false;
        match m {
            ChatMessage::System(s) => system = s.clone(),
            ChatMessage::User(u) => msgs.push(json!({ "role": "user", "content": u })),
            ChatMessage::UserBlocks(blocks) => {
                let content: Vec<Value> = blocks
                    .iter()
                    .map(|b| {
                        let mut o = json!({ "type": "text", "text": b.text });
                        if b.cache_boundary && explicit_boundaries < ANTHROPIC_EXPLICIT_BOUNDARIES_MAX {
                            o["cache_control"] = json!({ "type": "ephemeral" });
                            explicit_boundaries += 1;
                            last_msg_honored = true;
                        }
                        o
                    })
                    .collect();
                msgs.push(json!({ "role": "user", "content": content }));
            }
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
    // thinking 与 tools 共存：官方文档「Thinking works alongside tool use」，两条约束都不落在本 body——
    // ① tool_choice 强制工具（any / tool）才与 manual thinking 冲突，我们不设该键（默认 auto）；
    // ② thinking block 须随 tool_result 原样回传（见 followup：ChatMessage 无 thinking 字段，M21c 债）。
    // 曾有一版「tools 非空即丢弃思考参数」的守卫（批 C 顺手项），前提是错的：真正会 400 的是
    // 模型版本（Claude 4.7+ 拒 type:"enabled"）与被改动的 thinking block，两者都不是「有没有 tools」。
    for (k, v) in thinking.unwrap_or(&[]) {
        body.insert((*k).into(), v.clone());
    }
    // system 为空时省略该键，与 OpenAI 的 tool_calls 省略策略保持对称。
    if !system.is_empty() {
        // 三段界之首（批 D-4）：分块请求把共享头 system 也打断点；旧裸 string 形态不变（既有 body 逐字节不变）。
        if explicit_boundaries > 0 {
            body.insert("system".into(), json!([{ "type": "text", "text": system, "cache_control": { "type": "ephemeral" } }]));
        } else {
            body.insert("system".into(), json!(system));
        }
    }
    body.insert("tools".into(), json!(tool_decls));
    // L1 缓存：最后一个 message 的最后一个 content block 打 ephemeral 断点——前缀顺序
    // tools→system→messages，断点在 messages 末尾即缓存整个前缀（tools+system+全部历史）。
    // tool-loop append-only 确定性渲染，前缀天然稳定：每轮把断点移到新末尾，命中前一轮写的前缀、
    // 只付增量写入，13 轮对话输入成本理论折减 ~80-90%（5min TTL 覆盖 cell 内轮间隔）。
    // 当前三 provider 全 openai kind，此 body 走不到；为将来配 Anthropic 直连即生效而备。
    // 末块自动断点：末条 message 本身是分块（断点已显式给出）则不再加；其后追加的 Assistant / ToolResults / REPAIR 轮仍拿到
    // 移动尾断点（tool-loop 逐轮增量缓存不因分块会话失效），总数守 Anthropic 上限 4（评审 #12）。
    let placed = explicit_boundaries + usize::from(!system.is_empty() && explicit_boundaries > 0);
    debug_assert!(placed <= ANTHROPIC_CACHE_BOUNDARIES_MAX, "anthropic cache_control 断点上限 4，已放 {placed}");
    let auto_tail = !last_msg_honored && placed < ANTHROPIC_CACHE_BOUNDARIES_MAX;
    if let Some(last) = msgs.last_mut().filter(|_| auto_tail) {
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

/// 文本 delta 回调（M21b 批 B）。
pub(crate) type DeltaCb = Box<dyn FnMut(&str) + Send>;
/// 文本 delta 回调槽类型（M21b 批 B）：take / put-back 形态。
pub(crate) type TextDeltaSlot = std::sync::Mutex<Option<DeltaCb>>;

/// 回调放回 guard（取消安全，安全评审 #6）：turn 的读取 future 在 await 中被取消（per_turn 超时包装正是取消点）时，
/// 裸 put-back 永不执行、槽停在 None，同实例后续轮（FINALIZE/REPAIR）旁路静默熄火。guard 持槽的 `Arc` 克隆与回调本体，
/// 块尾正常 drop 或取消 drop 都执行放回（槽空才放——并发误用同实例时后完成者不抹先放回者）。
struct DeltaGuard {
    slot: std::sync::Arc<TextDeltaSlot>,
    cb: Option<DeltaCb>,
}

impl Drop for DeltaGuard {
    fn drop(&mut self) {
        let mut slot = self.slot.lock().unwrap();
        if slot.is_none() {
            *slot = self.cb.take();
        }
    }
}

/// SSE `data:` 行 → 文本 delta 提取（anthropic 方言）：`content_block_delta` 事件的 `text_delta.text`。
/// 与 `parse_anthropic_stream` 的文本拼接同字段路径——旁路喂出的序即最终聚合序。非文本事件 / 心跳 → None。
fn anthropic_text_delta(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    if v.get("type").and_then(Value::as_str) != Some("content_block_delta") {
        return None;
    }
    let d = &v["delta"];
    if d.get("type").and_then(Value::as_str) != Some("text_delta") {
        return None;
    }
    d.get("text").and_then(Value::as_str).map(str::to_string)
}

/// SSE `data:` 行 → 文本 delta 提取（openai 方言）：`choices[0].delta.content`。
/// 与 `parse_openai_stream` 的文本拼接同字段路径。
fn openai_text_delta(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    v.get("choices")?.as_array()?.first()?.get("delta")?.get("content")?.as_str().map(str::to_string)
}

/// 流式读 SSE body（M21b 批 B，D10-1 持久化旁路）：chunk 到达 → 拼行缓冲 → 完整 `data:` 行经方言提取器
/// 喂 `on_delta`（回调只读）→ 原始字节全量聚合返回。聚合结果与 `resp.text()` 等价——
/// **无回调（缺省 None）时行为逐字节不变**（cache 三段界与 PR181 对拍基线的锚）。
/// 中途断流 → 同 `text()` 的 reqwest 错误语义（调用方映射 Unavailable）。
async fn read_sse_with_delta(
    resp: reqwest::Response,
    mut on_delta: Option<&mut (dyn FnMut(&str) + Send + '_)>,
    extract: fn(&str) -> Option<String>,
) -> Result<String, reqwest::Error> {
    use futures::StreamExt;
    let mut buf: Vec<u8> = Vec::new();
    let mut line: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        line.extend_from_slice(&chunk);
        while let Some(pos) = line.iter().position(|&b| b == b'\n') {
            let rest = line.split_off(pos + 1);
            let l = std::mem::replace(&mut line, rest);
            feed_sse_line(&l[..l.len().saturating_sub(1)], &mut on_delta, extract);
        }
        buf.extend_from_slice(&chunk);
    }
    if !line.is_empty() {
        feed_sse_line(&line, &mut on_delta, extract);
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 一行 SSE：`data:` 前缀 → 方言提取器 → 喂回调（有回调且有文本增量时）。心跳 / 非 JSON 行静默跳过。
fn feed_sse_line(
    line: &[u8],
    on_delta: &mut Option<&mut (dyn FnMut(&str) + Send + '_)>,
    extract: fn(&str) -> Option<String>,
) {
    let l = String::from_utf8_lossy(line);
    let l = l.trim_end_matches('\r');
    // trim_start 容忍前导空白（与终态解析 `line.trim().strip_prefix("data:")` 同口径——旁路不得少喂）
    if let (Some(data), Some(cb)) = (l.trim_start().strip_prefix("data:"), on_delta.as_deref_mut()) {
        // 无回调（缺省）时 extract 也不跑——读取路径只剩字节聚合，行为逐字节不变
        if let Some(t) = extract(data.trim()) {
            cb(&t);
        }
    }
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
            // 末现优先：stop_reason 与累积 output_tokens 官方由 message_delta 携带；input / cache 分列官方也在此重复
            // 累计值，而部分网关（agc 2026-09-09 实测：GLM anthropic 形态）message_start 报 input 0、只在 message_delta 给
            // 真值 → 字段在场即覆盖，缺席保留 message_start 的值（M21a 出口验收 4：GLM 格 input 恒 0、cache 命中不可读）。
            Some("message_delta") => {
                if let Some(fr) = chunk["delta"]["stop_reason"].as_str() {
                    finish_reason = Some(fr.to_string());
                }
                let u = &chunk["usage"];
                if let Some(o) = u["output_tokens"].as_u64() {
                    output_tokens = o;
                }
                if let Some(i) = u["input_tokens"].as_u64() {
                    input_tokens = i;
                }
                if let Some(c) = u["cache_read_input_tokens"].as_u64() {
                    cache_read = c;
                }
                if let Some(c) = u["cache_creation_input_tokens"].as_u64() {
                    cache_creation = c;
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
        reasoning_tokens: None,
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
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens, cached_input_tokens, finish_reason, reasoning_tokens: None })
}

/// 直连 Anthropic 的 ChatBackend（rustls-only）。
pub struct AnthropicBackend {
    model: String,
    base_url: Option<String>,
    api_key: String,
    /// model 级输出上限（[models.X].output_limit）；None → env/默认链。
    output_limit: Option<u64>,
    /// 思考档位参数对（角色条目 `thinkingLevel` → `thinking_params`，批 F-2）；None = 不发。
    thinking: Option<Vec<(&'static str, Value)>>,
    /// 文本 delta 旁路回调槽（M21b 批 B，D10-1；take / put-back——跨 await 持 `Box`）；None = 行为逐字节不变。
    on_text_delta: std::sync::Arc<TextDeltaSlot>,
    client: reqwest::Client,
}

impl AnthropicBackend {
    pub fn new(model: String, base_url: Option<String>, api_key: String) -> Self {
        AnthropicBackend { model, base_url, api_key, output_limit: None, thinking: None, on_text_delta: std::sync::Arc::new(TextDeltaSlot::new(None)), client: crate::provider::default_http_client() }
    }

    /// 思考档位参数对注入（`thinking_params` 产物；None = 不发）。
    pub fn with_thinking(mut self, thinking: Option<Vec<(&'static str, Value)>>) -> Self {
        self.thinking = thinking;
        self
    }

    /// 模型 maxOutputTokens 注入（优先于 REDTEAM_MAX_TOKENS env 与默认）。
    pub fn with_max_tokens(mut self, limit: Option<u64>) -> Self {
        self.output_limit = limit;
        self
    }

    /// 整请求超时（秒）：`backend_from_config` 按该模型 per_turn 抬到罩住单轮（评审 M2）。
    pub fn with_http_timeout(mut self, secs: u64) -> Self {
        self.client = crate::provider::http_client_with_timeout(secs);
        self
    }

    /// 文本 delta 旁路回调注入（M21b 批 B）：`IncrementalFindings::feed` 的闭包形态。
    pub fn with_text_delta(self, cb: Box<dyn FnMut(&str) + Send>) -> Self {
        *self.on_text_delta.lock().unwrap() = Some(cb);
        self
    }

}

#[async_trait]
impl ChatBackend for AnthropicBackend {
    fn on_text_delta(&self) -> Option<Box<dyn FnMut(&str) + Send>> {
        self.on_text_delta.lock().unwrap().take()
    }

    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError> {
        let base = self.base_url.as_deref().unwrap_or(ANTHROPIC_DEFAULT_BASE);
        let url = super::anthropic_messages_url(base);
        let body = build_anthropic_tools_body_ext(&self.model, messages, tools, self.output_limit.unwrap_or_else(max_tokens), self.thinking.as_deref());
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
        let sse = {
            let mut guard = DeltaGuard { slot: self.on_text_delta.clone(), cb: None };
            guard.cb = self.on_text_delta();
            let r = read_sse_with_delta(resp, guard.cb.as_deref_mut(), anthropic_text_delta).await;
            r // guard drop（正常或取消）放回回调
        }
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
#[cfg(test)] // 批 F-2 后生产路径走 *_ext（带 thinking 参数）；本包装只给既有 body 测试保基线
pub(crate) fn build_openai_tools_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    json_mode: bool,
    output_limit: Option<u64>,
) -> Value {
    build_openai_tools_body_ext(model, messages, tools, json_mode, output_limit, None)
}

/// 同上 + 思考档位参数对（批 F-2 `thinking_params`）；`None` / 空 → 与不带参数版逐字节相同。
pub(crate) fn build_openai_tools_body_ext(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    json_mode: bool,
    output_limit: Option<u64>,
    thinking: Option<&[(&'static str, Value)]>,
) -> Value {
    let mut msgs: Vec<Value> = Vec::new();
    for m in messages {
        match m {
            ChatMessage::System(s) => msgs.push(json!({ "role": "system", "content": s })),
            ChatMessage::User(u) => msgs.push(json!({ "role": "user", "content": u })),
            ChatMessage::UserBlocks(blocks) => msgs.push(json!({ "role": "user", "content": crate::prompt::flatten_blocks(blocks) })),
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
    body.insert("max_tokens".into(), json!(output_limit.unwrap_or_else(max_tokens)));
    for (k, v) in thinking.unwrap_or(&[]) {
        body.insert((*k).into(), v.clone());
    }
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
    // 推理 token（批 F-2 观测）：OpenAI / DeepSeek / Kimi 都放 completion_tokens_details.reasoning_tokens；缺失 None。
    let reasoning_tokens = u["completion_tokens_details"]["reasoning_tokens"].as_u64();
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens, cached_input_tokens, finish_reason, reasoning_tokens })
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
    let reasoning_tokens = usage["completion_tokens_details"]["reasoning_tokens"].as_u64();
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens, cached_input_tokens, finish_reason, reasoning_tokens })
}

/// 直连 OpenAI 兼容端点的 ChatBackend。
pub struct OpenAiBackend {
    model: String,
    base_url: Option<String>,
    api_key: String,
    /// model 级输出上限（[models.X].output_limit）；None → env/默认链。
    output_limit: Option<u64>,
    /// 思考档位参数对（角色条目 `thinkingLevel` → `thinking_params`，批 F-2）；None = 不发。
    thinking: Option<Vec<(&'static str, Value)>>,
    /// 文本 delta 旁路回调槽（M21b 批 B，D10-1；take / put-back——跨 await 持 `Box`）；None = 行为逐字节不变。
    on_text_delta: std::sync::Arc<TextDeltaSlot>,
    client: reqwest::Client,
}

impl OpenAiBackend {
    pub fn new(model: String, base_url: Option<String>, api_key: String) -> Self {
        OpenAiBackend { model, base_url, api_key, output_limit: None, thinking: None, on_text_delta: std::sync::Arc::new(TextDeltaSlot::new(None)), client: crate::provider::default_http_client() }
    }

    /// 思考档位参数对注入（`thinking_params` 产物；None = 不发）。
    pub fn with_thinking(mut self, thinking: Option<Vec<(&'static str, Value)>>) -> Self {
        self.thinking = thinking;
        self
    }

    /// 模型 maxOutputTokens 注入（优先于 REDTEAM_MAX_TOKENS env 与默认）。
    pub fn with_max_tokens(mut self, limit: Option<u64>) -> Self {
        self.output_limit = limit;
        self
    }

    /// 整请求超时（秒）：`backend_from_config` 按该模型 per_turn 抬到罩住单轮（评审 M2）。
    pub fn with_http_timeout(mut self, secs: u64) -> Self {
        self.client = crate::provider::http_client_with_timeout(secs);
        self
    }

    /// 文本 delta 旁路回调注入（M21b 批 B）：`IncrementalFindings::feed` 的闭包形态。
    pub fn with_text_delta(self, cb: Box<dyn FnMut(&str) + Send>) -> Self {
        *self.on_text_delta.lock().unwrap() = Some(cb);
        self
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
        // 读 SSE 全文（随到达消费；有回调时逐片喂——M21b 批 B 旁路）。中途断流 → Unavailable（可重试），与 decode_json 一致。
        let sse = {
            let mut guard = DeltaGuard { slot: self.on_text_delta.clone(), cb: None };
            guard.cb = self.on_text_delta();
            let r = read_sse_with_delta(resp, guard.cb.as_deref_mut(), openai_text_delta).await;
            r // guard drop（正常或取消）放回回调
        }
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
    fn on_text_delta(&self) -> Option<Box<dyn FnMut(&str) + Send>> {
        self.on_text_delta.lock().unwrap().take()
    }

    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError> {
        // 探索轮：带 tools、不强制 JSON（保留 function calling）。
        let body = build_openai_tools_body_ext(&self.model, messages, tools, false, self.output_limit, self.thinking.as_deref());
        self.post(&body).await
    }

    async fn turn_final(
        &self,
        messages: &[ChatMessage],
    ) -> Result<AssistantTurn, ProviderError> {
        // 收尾轮：不带 tools + response_format=json_object，强制产出最终 findings JSON。
        let body = build_openai_tools_body_ext(&self.model, messages, &[], true, self.output_limit, self.thinking.as_deref());
        self.post(&body).await
    }
}

/// 按 EffectiveModel（角色条目+provider 覆盖链成品）造对应 backend（编排层入口）。
/// kind 已在 config::effective 里 merge（条目.kind > provider.kind）。
/// Google 暂不支持（占位规划）。max_tokens：模型 maxOutputTokens 注入 backend
/// （> REDTEAM_MAX_TOKENS env > 32768 默认，见 AnthropicBackend/OpenAiBackend 构造）。
pub fn backend_from_config(
    em: &crate::config::EffectiveModel,
    api_key: String,
) -> Result<Box<dyn ChatBackend>, ProviderError> {
    backend_from_config_inner(em, api_key, None)
}

/// 带流式旁路回调的 **per-cell 独占实例**（M21b 批 C，批 B 接缝契约）：registry 里的共享 backend
/// 不挂回调——挂了并发 cell 会互相抢 delta——所以 reviewer 每格另造一个，回调喂本格的
/// `IncrementalFindings`。构造失败时调用方退回共享 backend（只是没有旁路，不致命）。
pub fn backend_from_config_with_delta(
    em: &crate::config::EffectiveModel,
    api_key: String,
    on_text_delta: Box<dyn FnMut(&str) + Send>,
) -> Result<Box<dyn ChatBackend>, ProviderError> {
    backend_from_config_inner(em, api_key, Some(on_text_delta))
}

/// 文本 delta 旁路回调（`IncrementalFindings::feed` 的闭包形态）。
pub type TextDeltaCb = Box<dyn FnMut(&str) + Send>;

fn backend_from_config_inner(
    em: &crate::config::EffectiveModel,
    api_key: String,
    on_text_delta: Option<TextDeltaCb>,
) -> Result<Box<dyn ChatBackend>, ProviderError> {
    // 整请求超时罩住单轮（评审 M2）：慢模型 per_turn 1200s 时 reqwest 不再在 300s 先切断。
    let http_secs = crate::provider::http_timeout_for(em.per_turn_secs());
    // 批 F-2：角色条目 thinkingLevel 首个消费者——按 (kind, family) 查表；无映射 → 不发（warning 由 `thinking_warning` 给调用方）。
    let thinking = em.thinking_level.and_then(|l| thinking_params(em.kind, &em.family, &em.id, l, em.max_tokens_effective()));
    match em.kind {
        ProviderKind::Anthropic => {
            let b = AnthropicBackend::new(em.id.clone(), em.base_url.clone(), api_key)
                .with_max_tokens(em.max_output_tokens)
                .with_http_timeout(http_secs)
                .with_thinking(thinking);
            Ok(Box::new(match on_text_delta {
                Some(cb) => b.with_text_delta(cb),
                None => b,
            }))
        }
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => {
            let b = OpenAiBackend::new(em.id.clone(), em.base_url.clone(), api_key)
                .with_max_tokens(em.max_output_tokens)
                .with_http_timeout(http_secs)
                .with_thinking(thinking);
            Ok(Box::new(match on_text_delta {
                Some(cb) => b.with_text_delta(cb),
                None => b,
            }))
        }
        ProviderKind::Google => Err(ProviderError::Unavailable("google not yet supported".into())),
    }
}

/// 配置了 thinkingLevel 但 (kind, family) 无参数映射 → 一条 warning（进 RunMeta.warnings，铁律 1 可观测）；否则 None。
pub fn thinking_warning(em: &crate::config::EffectiveModel) -> Option<String> {
    let level = em.thinking_level?;
    if thinking_params(em.kind, &em.family, &em.id, level, em.max_tokens_effective()).is_some() {
        return None;
    }
    Some(format!(
        "thinkingLevel {} 对 {}（kind {:?}，family {}）无参数映射，未发送；已知映射：anthropic+claude / openai+deepseek·glm·kimi·gpt-5·o 系·gemini·qwen",
        level.as_str(), em.name, em.kind, em.family
    ))
}

/// 家族串（`EffectiveModel.family`：配置 `family` 或 model id 首段小写，如 `qwen3` / `glm` / `deepseek` / `gpt`）+ model id → 映射表键。
/// `gpt` 家族只有推理型（gpt-5 系 / o 系）接受 `reasoning_effort`，gpt-4 系发了即 400（评审修 3）——按 id 区分，分不清的归
/// `gpt-legacy`（无映射 + warning）。
fn family_class(family: &str, id: &str) -> &'static str {
    let f = family.trim().to_ascii_lowercase();
    let i = id.trim().to_ascii_lowercase();
    let o_series = |s: &str| s.len() > 1 && s.starts_with('o') && s[1..].starts_with(|c: char| c.is_ascii_digit());
    if f.starts_with("deepseek") { "deepseek" }
    else if f.starts_with("glm") || f.starts_with("zhipu") || f == "zai" || f == "z.ai" { "glm" }
    else if f.starts_with("kimi") || f.starts_with("moonshot") { "kimi" }
    else if f == "openai" || o_series(&f) || o_series(&i) || f.starts_with("gpt-5") || f == "gpt5" || i.starts_with("gpt-5") || i.starts_with("gpt5") { "openai" }
    else if f.starts_with("gpt") { "gpt-legacy" }
    else if f.starts_with("gemini") { "gemini" }
    else if f.starts_with("qwen") { "qwen" }
    else if f.starts_with("claude") || f == "anthropic" { "claude" }
    else { "unknown" }
}
/// 思考档位 → 请求体参数对（批 F-2，ThinkingLevel 首个消费者；design §4「scanner 请求显式钉推理档」）。
/// 按 (kind, family, id) 查表——各家参数名互斥（Kimi 同发 `thinking` 与 `reasoning_effort` 即 400；GLM-5.x 不可关闭而 GLM-4.x 可关；
/// gpt-4 系不认 `reasoning_effort`；Anthropic 要求 `1024 ≤ budget_tokens < max_tokens`）。`None` = 无映射（调用方不发 + warning）；
/// `Some(vec![])` = 该档位就是「不发任何参数」。
/// 官方文档核实 2026-09-09：DeepSeek（`thinking{type}` + `reasoning_effort low|high|max`）、Z.ai GLM-5.3（openai 形态同上，不可 disable；
/// anthropic 形态文档无字段说明 → 无映射）、Moonshot（只认 `thinking{type}`）、Anthropic（`thinking{enabled, budget_tokens}`）、
/// OpenAI 推理型 / Gemini（`reasoning_effort low|medium|high`）、DashScope Qwen（`enable_thinking`）。
pub(crate) fn thinking_params(
    kind: ProviderKind,
    family: &str,
    id: &str,
    level: ThinkingLevel,
    max_tokens: u64,
) -> Option<Vec<(&'static str, Value)>> {
    use ThinkingLevel::*;
    let class = family_class(family, id);
    let lid = id.trim().to_ascii_lowercase();
    match kind {
        ProviderKind::Anthropic => match class {
            "claude" => {
                let budget: u64 = match level { Off => return Some(vec![]), Low => 2048, Medium => 8192, High => 16384 };
                let budget = budget.min(max_tokens.saturating_sub(1024));
                if budget < 1024 {
                    return None;
                }
                Some(vec![("thinking", json!({ "type": "enabled", "budget_tokens": budget }))])
            }
            _ => None,
        },
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => match class {
            "deepseek" => Some(match level {
                Off => vec![("thinking", json!({ "type": "disabled" }))],
                Low => vec![("thinking", json!({ "type": "enabled" })), ("reasoning_effort", json!("low"))],
                Medium => vec![("thinking", json!({ "type": "enabled" })), ("reasoning_effort", json!("high"))],
                High => vec![("thinking", json!({ "type": "enabled" })), ("reasoning_effort", json!("max"))],
            }),
            "glm" => {
                // GLM-5.x：不可关闭（Off → enabled + low，文档迁移口径）；GLM-4.x 及未知版本：Off 真关（评审修 8）。
                let v5 = lid.starts_with("glm-5") || lid.starts_with("glm5") || family.trim().to_ascii_lowercase().starts_with("glm-5");
                Some(match level {
                    Off if !v5 => vec![("thinking", json!({ "type": "disabled" }))],
                    Off | Low => vec![("thinking", json!({ "type": "enabled" })), ("reasoning_effort", json!("low"))],
                    Medium => vec![("thinking", json!({ "type": "enabled" })), ("reasoning_effort", json!("high"))],
                    High => vec![("thinking", json!({ "type": "enabled" })), ("reasoning_effort", json!("max"))],
                })
            }
            "kimi" => Some(vec![("thinking", json!({ "type": if level == Off { "disabled" } else { "enabled" } }))]),
            "openai" | "gemini" => Some(match level {
                Off => vec![],
                Low => vec![("reasoning_effort", json!("low"))],
                Medium => vec![("reasoning_effort", json!("medium"))],
                High => vec![("reasoning_effort", json!("high"))],
            }),
            "qwen" => Some(vec![("enable_thinking", json!(level != Off))]),
            _ => None,
        },
        ProviderKind::Google => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_MAX_TOKENS as MAX_TOKENS;
    use crate::tools::tool_specs;

    /// 评审 M2：整请求超时随 per_turn 抬；env 更大时取 env。

    // ----------------------------- 流式旁路（M21b 批 B-4）-----------------------------

    #[test]
    fn anthropic_text_delta_extracts_only_text_events() {
        assert_eq!(anthropic_text_delta(r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hel"}}"#).as_deref(), Some("hel"));
        assert_eq!(anthropic_text_delta(r#"{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#), None, "工具分片不喂");
        assert_eq!(anthropic_text_delta(r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"x"}}"#), None, "思考不喂");
        assert_eq!(anthropic_text_delta(r#"{"type":"message_start","message":{}}"#), None);
        assert_eq!(anthropic_text_delta("event: ping"), None, "非 JSON 跳过");
    }

    #[test]
    fn openai_text_delta_extracts_choices_content() {
        assert_eq!(openai_text_delta(r#"{"choices":[{"delta":{"content":"wor"}}]}"#).as_deref(), Some("wor"));
        assert_eq!(openai_text_delta(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"x"}}]}}]}"#), None);
        assert_eq!(openai_text_delta(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#), None);
        assert_eq!(openai_text_delta(r#"{"usage":{"total_tokens":1}}"#), None, "usage-only 末块无 choices");
        assert_eq!(openai_text_delta("[DONE]"), None);
    }

    /// 无回调时 feed_sse_line 不动任何状态（读取路径只剩字节聚合——逐字节不变锚的行为件）。
    #[test]
    fn feed_sse_line_without_callback_is_noop() {
        let mut cb: Option<&mut (dyn FnMut(&str) + Send)> = None;
        let fed = std::sync::Mutex::new(Vec::<String>::new());
        // 有回调时按序喂
        {
            let mut sink = |t: &str| fed.lock().unwrap().push(t.to_string());
            let mut c: Option<&mut (dyn FnMut(&str) + Send)> = Some(&mut sink);
            feed_sse_line(b"data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"a\"}}", &mut c, anthropic_text_delta);
            feed_sse_line(b"event: ping", &mut c, anthropic_text_delta);
            feed_sse_line(b"data: [DONE]", &mut c, anthropic_text_delta);
            feed_sse_line(b"data: {\"choices\":[{\"delta\":{\"content\":\"b\"}}]}", &mut c, openai_text_delta);
        }
        assert_eq!(*fed.lock().unwrap(), vec!["a".to_string(), "b".to_string()]);
        // 无回调：同输入静默
        feed_sse_line(b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}", &mut cb, openai_text_delta);
        assert!(cb.is_none());
    }

    #[test]
    fn http_timeout_covers_per_turn() {
        let _g = crate::config::ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::remove_var("REDTEAM_HTTP_TIMEOUT_SECS"); }
        assert_eq!(crate::provider::http_timeout_for(100), 300, "短轮：缺省 300 罩得住");
        assert_eq!(crate::provider::http_timeout_for(1200), 1260, "长轮：per_turn + 60");
        unsafe { std::env::set_var("REDTEAM_HTTP_TIMEOUT_SECS", "2000"); }
        assert_eq!(crate::provider::http_timeout_for(1200), 2000, "env 更大时取 env");
        unsafe { std::env::set_var("REDTEAM_HTTP_TIMEOUT_SECS", "0"); }
        assert_eq!(crate::provider::http_timeout_env_secs(), 300, "0 / 非法 → 缺省");
        unsafe { std::env::remove_var("REDTEAM_HTTP_TIMEOUT_SECS"); }
    }

    fn specs() -> Vec<ToolSpec> {
        tool_specs()
    }

    /// 09-04 分离后 backend_from_config 吃 EffectiveModel——测试构造辅助。
    fn eff(kind: crate::config::ProviderKind) -> crate::config::EffectiveModel {
        crate::config::EffectiveModel {
            name: "t".into(), display_name: None, id: "m".into(), provider: "p".into(), role: crate::config::Role::Scanner, kind,
            base_url: None, context_window: 128_000, max_output_tokens: None,
            per_turn_timeout_secs: None, reasoning: false, thinking_level: None,
            family: "m".into(), max_concurrency: None,
        }
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

    /// 批 D-4：分块 user 消息 → anthropic 三段界 cache_control（system + 证据末 + 角色尾前），末块不再自动加断点；
    /// openai 拼接后与裸 User 逐字节等价。
    #[test]
    fn user_blocks_three_cache_boundaries_and_openai_flatten() {
        use crate::prompt::PromptBlock;
        let blocks = vec![
            PromptBlock { text: "<reference>r</reference>".into(), cache_boundary: false },
            PromptBlock { text: "<evidence>e</evidence>".into(), cache_boundary: true },
            PromptBlock { text: "<diff>d</diff>".into(), cache_boundary: true },
            PromptBlock { text: "<scanner>t</scanner>".into(), cache_boundary: false },
        ];
        let msgs = vec![ChatMessage::System("head".into()), ChatMessage::UserBlocks(blocks.clone())];
        let b = build_anthropic_tools_body("claude-x", &msgs, &[], MAX_TOKENS);
        assert_eq!(b["system"][0]["text"], "head");
        assert_eq!(b["system"][0]["cache_control"]["type"], "ephemeral", "共享头断点: {b}");
        let content = b["messages"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 4);
        assert!(content[0].get("cache_control").is_none() && content[3].get("cache_control").is_none(), "末块不自动加断点: {b}");
        assert_eq!(content[1]["cache_control"]["type"], "ephemeral");
        assert_eq!(content[2]["cache_control"]["type"], "ephemeral");
        assert_eq!(b.to_string().matches("cache_control").count(), 3, "恰三段界: {b}");
        // 分块之后再追加 tool_result 尾：尾块仍拿到移动断点，总数 4 ≤ 上限（评审 #12）
        let mut multi = msgs.clone();
        multi.push(ChatMessage::ToolResults(vec![crate::tools::ToolResult { id: "t1".into(), content: "file".into() }]));
        let b2 = build_anthropic_tools_body("claude-x", &multi, &[], MAX_TOKENS);
        assert_eq!(b2["messages"][1]["content"][0]["cache_control"]["type"], "ephemeral", "尾 tool_result 仍打断点: {b2}");
        assert_eq!(b2.to_string().matches("cache_control").count(), 4, "{b2}");
        let flat = crate::prompt::flatten_blocks(&blocks);
        let a = build_openai_tools_body("m", &[ChatMessage::System("head".into()), ChatMessage::UserBlocks(blocks)], &[], true, None);
        let c = build_openai_tools_body("m", &[ChatMessage::System("head".into()), ChatMessage::User(flat)], &[], true, None);
        assert_eq!(a, c, "openai 分块与拼接逐字节相同");
    }

    /// 合并后修补：显式断点运行时封顶——多条 UserBlocks（或一条带 ≥ 4 断点）只兑现前 3 个，连同 system 恒 ≤ 4；
    /// 断点被吞掉的末条分块仍拿到移动尾断点（不因守卫丢掉逐轮增量缓存）。
    #[test]
    fn user_blocks_explicit_boundaries_capped_at_api_limit() {
        use crate::prompt::PromptBlock;
        let two = |tag: &str| vec![
            PromptBlock { text: format!("<{tag}>a</{tag}>"), cache_boundary: true },
            PromptBlock { text: format!("<{tag}>b</{tag}>"), cache_boundary: true },
        ];
        // 两条分块各 2 断点：第一条全兑现、第二条只兑现 1 个（3 + system = 4），末块不再自动加。
        let msgs = vec![ChatMessage::System("head".into()), ChatMessage::UserBlocks(two("x")), ChatMessage::UserBlocks(two("y"))];
        let b = build_anthropic_tools_body("claude-x", &msgs, &[], MAX_TOKENS);
        assert_eq!(b.to_string().matches("cache_control").count(), 4, "封顶 4: {b}");
        let m1 = b["messages"][1]["content"].as_array().unwrap();
        assert!(m1[0].get("cache_control").is_some() && m1[1].get("cache_control").is_none(), "第二条只兑现首个: {b}");
        // 三条分块：第三条断点全被吞掉 → 不自动补尾断点（已满 4）；总数仍 4。
        let mut three = msgs.clone();
        three.push(ChatMessage::UserBlocks(two("z")));
        let b3 = build_anthropic_tools_body("claude-x", &three, &[], MAX_TOKENS);
        assert_eq!(b3.to_string().matches("cache_control").count(), 4, "{b3}");
        // 一条带 5 断点的分块：兑现前 3 个 + system = 4。
        let five: Vec<PromptBlock> = (0..5).map(|k| PromptBlock { text: format!("b{k}"), cache_boundary: true }).collect();
        let b5 = build_anthropic_tools_body("claude-x", &[ChatMessage::System("head".into()), ChatMessage::UserBlocks(five)], &[], MAX_TOKENS);
        assert_eq!(b5.to_string().matches("cache_control").count(), 4, "{b5}");
        let c5 = b5["messages"][0]["content"].as_array().unwrap();
        assert!(c5[2].get("cache_control").is_some() && c5[3].get("cache_control").is_none() && c5[4].get("cache_control").is_none(), "{b5}");
        // 无 system、末条是断点被吞掉的分块：尾块拿到移动断点，总数 4。
        let no_sys = vec![ChatMessage::UserBlocks(two("x")), ChatMessage::UserBlocks(two("y"))];
        let b0 = build_anthropic_tools_body("claude-x", &no_sys, &[], MAX_TOKENS);
        assert!(b0.get("system").is_none());
        assert_eq!(b0.to_string().matches("cache_control").count(), 3, "{b0}");
        let swallowed = vec![ChatMessage::UserBlocks(two("x")), ChatMessage::UserBlocks(two("y")), ChatMessage::UserBlocks(two("z"))];
        let bs = build_anthropic_tools_body("claude-x", &swallowed, &[], MAX_TOKENS);
        let last = bs["messages"][2]["content"].as_array().unwrap();
        assert_eq!(bs.to_string().matches("cache_control").count(), 4, "{bs}");
        assert!(last[1].get("cache_control").is_some(), "断点被吞掉的末条分块拿到尾断点: {bs}");
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
        let b = build_openai_tools_body("m", &msgs, &specs(), false, None);
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
        let fin = build_openai_tools_body("gpt-x", &msgs, &[], true, None);
        assert!(fin.get("tools").is_none(), "收尾轮不应带 tools, got: {fin}");
        assert_eq!(fin["response_format"]["type"], "json_object");

        let explore = build_openai_tools_body("gpt-x", &msgs, &specs(), false, None);
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
    fn anthropic_stream_takes_input_and_cache_from_message_delta_when_present() {
        // 网关形态（agc GLM anthropic，2026-09-09 实测）：message_start 报 input 0，真值只在 message_delta.usage；
        // 官方形态 message_delta 也重复累计 input / cache 分列——在场即覆盖，缺席保留 message_start（出口验收 4）。
        let gateway = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":0,\"output_tokens\":0}}}\n\
data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":19,\"output_tokens\":32,\"cache_read_input_tokens\":7}}\n\
data: {\"type\":\"message_stop\"}\n";
        let t = parse_anthropic_stream(gateway).unwrap();
        assert_eq!((t.input_tokens, t.cached_input_tokens, t.output_tokens), (26, 7, 32), "delta 在场即覆盖：19 + cache 7 合成总输入");
        let official_only_start = "\
data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":20,\"cache_read_input_tokens\":80}}}\n\
data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":5}}\n";
        let t = parse_anthropic_stream(official_only_start).unwrap();
        assert_eq!((t.input_tokens, t.cached_input_tokens), (100, 80), "delta 缺席保留 message_start 分列");
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
        let b = build_openai_tools_body("gpt-x", &msgs, &specs(), false, None);
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
        let b = build_openai_tools_body("m", &msgs, &specs(), false, None);
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
        let anth = eff(ProviderKind::Anthropic);
        assert!(backend_from_config(&anth, "k".into()).is_ok());
        let goog = eff(ProviderKind::Google);
        assert!(matches!(
            backend_from_config(&goog, "k".into()),
            Err(ProviderError::Unavailable(_))
        ));
    }

    /// 批 F-2：思考档位映射表——每个 (kind, family) 一断言；无映射 None；anthropic budget 夹到 max_tokens − 1024。
    #[test]
    fn thinking_params_table_by_kind_and_family() {
        use crate::config::ProviderKind::*;
        use ThinkingLevel::*;
        let get = |kind, fam: &str, level, max| thinking_params(kind, fam, fam, level, max);
        let get_id = |kind, fam: &str, id: &str, level, max| thinking_params(kind, fam, id, level, max);
        let pairs = |v: Option<Vec<(&'static str, Value)>>| v.map(|v| serde_json::to_string(&serde_json::Map::from_iter(v.into_iter().map(|(k, x)| (k.to_string(), x)))).unwrap());
        assert_eq!(pairs(get(Openai, "deepseek", Low, 32768)).unwrap(), r#"{"reasoning_effort":"low","thinking":{"type":"enabled"}}"#);
        assert_eq!(pairs(get(Openai, "deepseek", Off, 32768)).unwrap(), r#"{"thinking":{"type":"disabled"}}"#);
        assert_eq!(pairs(get(Openai, "deepseek", High, 32768)).unwrap(), r#"{"reasoning_effort":"max","thinking":{"type":"enabled"}}"#);
        assert_eq!(pairs(get_id(OpenaiCompatible, "glm", "glm-5.3", Off, 32768)).unwrap(), r#"{"reasoning_effort":"low","thinking":{"type":"enabled"}}"#, "GLM-5.3 不可关：Off → enabled+low");
        assert_eq!(pairs(get_id(Openai, "glm", "glm-4.6", Off, 32768)).unwrap(), r#"{"thinking":{"type":"disabled"}}"#, "GLM-4.x Off 真关（评审修 8）");
        assert!(get_id(Openai, "gpt", "gpt-4.1", Low, 32768).is_none(), "gpt-4 系不认 reasoning_effort → 无映射（评审修 3）");
        assert_eq!(pairs(get_id(Openai, "gpt", "gpt-5.6", Low, 32768)).unwrap(), r#"{"reasoning_effort":"low"}"#);
        assert_eq!(pairs(get_id(Openai, "o3", "o3-mini", Low, 32768)).unwrap(), r#"{"reasoning_effort":"low"}"#);
        assert_eq!(pairs(get(Openai, "glm", Medium, 32768)).unwrap(), r#"{"reasoning_effort":"high","thinking":{"type":"enabled"}}"#);
        assert_eq!(pairs(get(Openai, "kimi", High, 32768)).unwrap(), r#"{"thinking":{"type":"enabled"}}"#, "Kimi 不发 reasoning_effort");
        assert_eq!(pairs(get(Openai, "moonshot", Off, 32768)).unwrap(), r#"{"thinking":{"type":"disabled"}}"#);
        assert_eq!(pairs(get(Openai, "openai", Medium, 32768)).unwrap(), r#"{"reasoning_effort":"medium"}"#, "显式 family=openai 视为推理型");
        assert_eq!(pairs(get(Openai, "o3", Low, 32768)).unwrap(), r#"{"reasoning_effort":"low"}"#);
        assert_eq!(pairs(get_id(Openai, "gpt", "gpt-5", Off, 32768)).unwrap(), "{}", "openai Off = 不发");
        assert_eq!(pairs(get(Openai, "gemini", High, 32768)).unwrap(), r#"{"reasoning_effort":"high"}"#);
        assert_eq!(pairs(get(Openai, "qwen3", Low, 32768)).unwrap(), r#"{"enable_thinking":true}"#, "family 取 id 首段 qwen3 也命中");
        assert_eq!(pairs(get(Openai, "qwen", Off, 32768)).unwrap(), r#"{"enable_thinking":false}"#);
        assert_eq!(pairs(get(Anthropic, "claude", Medium, 32768)).unwrap(), r#"{"thinking":{"budget_tokens":8192,"type":"enabled"}}"#);
        assert_eq!(pairs(get(Anthropic, "claude", High, 4096)).unwrap(), r#"{"thinking":{"budget_tokens":3072,"type":"enabled"}}"#, "budget 夹到 max_tokens − 1024");
        assert_eq!(pairs(get(Anthropic, "claude", Off, 32768)).unwrap(), "{}");
        assert!(get(Anthropic, "claude", Low, 2000).is_none(), "夹后 < 1024 → 无法满足 API 约束 → 无映射");
        assert!(get(Anthropic, "glm", Low, 32768).is_none(), "GLM anthropic 形态文档无字段 → 无映射");
        assert!(get(Openai, "grok", Low, 32768).is_none() && get(Google, "gemini", Low, 32768).is_none());
    }

    /// thinking × tools 共存（2026-09-12 按官方文档更正：「Thinking works alongside tool use」）。
    /// 曾有一版守卫在 tools 非空时丢弃思考参数，前提是错的——Anthropic 的两条约束是
    /// ① tool_choice 强制工具（我们不设该键）② thinking block 原样回传，都与「有没有 tools」无关。
    /// 真正的 400 源是模型版本（4.7+ 拒 type:"enabled"）与被改动的 thinking block，见 followup。
    #[test]
    fn anthropic_thinking_survives_tools_in_body() {
        let msgs = vec![ChatMessage::System("sys".into()), ChatMessage::User("go".into())];
        let tool = crate::tools::ToolSpec { name: "read", description: "d", parameters: serde_json::json!({}) };
        let th = thinking_params(ProviderKind::Anthropic, "claude", "claude-x", ThinkingLevel::Low, MAX_TOKENS).unwrap();
        let with_tools = build_anthropic_tools_body_ext("claude-x", &msgs, &[tool], MAX_TOKENS, Some(&th));
        assert_eq!(with_tools["thinking"]["type"], "enabled", "工具在场不该丢用户配的思考档");
        assert!(with_tools["thinking"]["budget_tokens"].as_u64().unwrap() >= 1024);
        assert_eq!(with_tools["tools"].as_array().unwrap().len(), 1);
        assert!(with_tools.get("tool_choice").is_none(), "不设 tool_choice → 默认 auto，与 thinking 相容");
        assert!(with_tools.get("temperature").is_none() && with_tools.get("top_p").is_none());
        let no_tools = build_anthropic_tools_body_ext("claude-x", &msgs, &[], MAX_TOKENS, Some(&th));
        assert_eq!(no_tools["thinking"]["type"], "enabled");
    }

    /// 批 F-2：参数对进请求体；`None` 与不带参数版逐字节相同（既有 body 测试的基线不受影响）。
    #[test]
    fn bodies_carry_thinking_pairs_and_none_is_bytewise_identical() {
        let msgs = vec![ChatMessage::System("sys".into()), ChatMessage::User("review".into())];
        let base_a = build_anthropic_tools_body("claude-x", &msgs, &[], MAX_TOKENS);
        assert_eq!(base_a, build_anthropic_tools_body_ext("claude-x", &msgs, &[], MAX_TOKENS, None));
        assert_eq!(base_a, build_anthropic_tools_body_ext("claude-x", &msgs, &[], MAX_TOKENS, Some(&[])));
        let th = thinking_params(ProviderKind::Anthropic, "claude", "claude-x", ThinkingLevel::Low, MAX_TOKENS).unwrap();
        let a = build_anthropic_tools_body_ext("claude-x", &msgs, &[], MAX_TOKENS, Some(&th));
        assert_eq!(a["thinking"]["budget_tokens"], 2048);
        assert_eq!(a["thinking"]["type"], "enabled");
        let base_o = build_openai_tools_body("m", &msgs, &[], false, None);
        assert_eq!(base_o, build_openai_tools_body_ext("m", &msgs, &[], false, None, None));
        let th = thinking_params(ProviderKind::Openai, "deepseek", "deepseek-v4-flash", ThinkingLevel::Low, MAX_TOKENS).unwrap();
        let o = build_openai_tools_body_ext("m", &msgs, &[], false, None, Some(&th));
        assert_eq!(o["thinking"]["type"], "enabled");
        assert_eq!(o["reasoning_effort"], "low");
        assert!(o.get("stream").is_some() && o.get("max_tokens").is_some(), "既有键不受影响: {o}");
    }

    /// 批 F-2：backend_from_config 注入档位；无映射 family 给 warning、有映射不给；未配档位不给。
    #[test]
    fn backend_from_config_injects_thinking_and_warns_when_unmapped() {
        let mut em = eff(ProviderKind::Openai);
        em.family = "deepseek".into();
        em.thinking_level = Some(ThinkingLevel::Low);
        assert!(backend_from_config(&em, "k".into()).is_ok());
        assert!(thinking_warning(&em).is_none());
        em.family = "grok".into();
        let w = thinking_warning(&em).expect("grok 无映射");
        assert!(w.contains("thinkingLevel low") && w.contains("grok"), "{w}");
        em.thinking_level = None;
        assert!(thinking_warning(&em).is_none(), "未配档位不告警");
    }

    /// 批 F-2：openai 响应的 reasoning_tokens 进 AssistantTurn（非流式 + 流式）；anthropic 恒 None。
    #[test]
    fn openai_parse_reads_reasoning_tokens() {
        let v = json!({"choices":[{"message":{"content":"{}"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":10,"completion_tokens":5,"completion_tokens_details":{"reasoning_tokens":3}}});
        assert_eq!(parse_openai_turn(&v).unwrap().reasoning_tokens, Some(3));
        let v2 = json!({"choices":[{"message":{"content":"{}"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5}});
        assert_eq!(parse_openai_turn(&v2).unwrap().reasoning_tokens, None);
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"{}\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"choices\":[],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2,\"completion_tokens_details\":{\"reasoning_tokens\":0}}}\n\ndata: [DONE]\n";
        assert_eq!(parse_openai_stream(sse).unwrap().reasoning_tokens, Some(0));
        let a = json!({"content":[{"type":"text","text":"{}"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}});
        assert_eq!(parse_anthropic_turn(&a).unwrap().reasoning_tokens, None);
    }
}
