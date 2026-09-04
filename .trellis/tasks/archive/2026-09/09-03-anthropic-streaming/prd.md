# AnthropicBackend 流式支持：修复大 bundle 长思考 524

## 背景

- `2d9945d`（feat(provider): OpenAI backend 改流式 SSE——治连接 reset/超时根源）只改了
  `OpenAIBackend`；`AnthropicBackend` 仍是 non-streaming：POST 后整读 JSON
  （`src/provider/backends.rs` `AnthropicBackend::turn`）。
- 2026-09-03 dogfood 实证（见 `research/2026-09-03-524-spike.md`）：PR 181 评审
  （40 文件、+1750/-69 bundle），GLM/Kimi 配 `kind="anthropic"` → 大 bundle 长思考期
  连接静默 → api.aigocode.team（Cloudflare 前置）~100s 无首字节 → **http 524**，
  双模型全挂、input/output tokens 全 0、作业退码 2（降级 approve，verdict 不可采信）。
- 轻量 `model check`（"hi" 级请求 3s 返回）全绿 → 该故障**只在长推理场景复现**，
  预检覆盖不到。

## 目标

`AnthropicBackend` 对齐 `OpenAIBackend` 已验证的流式方案：请求体带 `stream:true`、
SSE 增量消费聚合为 `AssistantTurn`、保留非 SSE 回退与 error 体语义分类。
修复后 GLM/Kimi 以 `kind="anthropic"` 跑大 bundle 评审不再 524。

## 方案

1. **请求体**：`build_anthropic_tools_body` 增加 `"stream": true`。
   Anthropic 协议无 `stream_options`；usage 由事件流携带
   （`message_start` 带input、`message_delta` 带output）。
2. **收发重构**：`AnthropicBackend::turn` 对齐 `OpenAiBackend::post` 模式：
   - `send_classified` 分类 HTTP 状态；
   - `resp.text()` 随到达消费（reqwest 持续读 socket → 连接活跃，网关不当空闲 reset）；
   - 空体 → `Unavailable`（可重试，勿误判 Parse 烧格）；
   - 非 SSE 回退：`!sse.contains("data:")` 时保留现有 `error_body_to_provider_error`
     探测 + `parse_anthropic_turn` 整体 JSON 解析（部分网关无视 stream）。
3. **新增 `parse_anthropic_stream(&sse) -> Option<AssistantTurn>`**，聚合 Anthropic SSE 事件：
   - `message_start` → `usage.input_tokens`、`cache_read_input_tokens`；
   - `content_block_start`（type=tool_use）→ 按 index 登记 tool id+name；
   - `content_block_delta` → `text_delta` 追加 text；`input_json_delta.partial_json`
     追加该块 tool arguments（最终 `serde_json::from_str`，失败落 `json!({})`）；
     `thinking_delta` 忽略；
   - `message_delta` → `stop_reason` + `usage.output_tokens`；
   - `event: error`（SSE 帧内）→ 按语义分类（对齐 D1 的 200-内-error 处理）。
   - `AssistantTurn` 字段映射：`input_tokens`/`output_tokens`/`cached_input_tokens`
     ← `cache_read_input_tokens`。
4. **测试**（对齐 `parse_openai_stream` 既有测试模式）：
   - SSE 样本聚合（text + tool_use 分片 + usage）；
   - 非 SSE 回退（完整 JSON 直接解析）；
   - SSE 内 error 事件分类；
   - 空体 → Unavailable。

## 验收

- 单测：新增 SSE 聚合/回退/error 用例全绿；`cargo test` 全量绿。
- 实测：GLM/Kimi 配置改回 `kind="anthropic"` 后，重跑 PR 181 同规模 bundle 评审
  双模型出报告（token 计数非 0、无 524）。

## 边界与非目标

- 不动 `OpenAIBackend`、`tool_loop`、retry 烧格策略本身。
- 不做逐事件超时/心跳监控（reqwest 消费即保活，够用）。
- `ChatBackend` trait 的 `turn_final` 在 Anthropic 侧的默认实现保持现状
  （实现时确认 trait 定义，若 Anthropic 走默认 `turn` 路径则天然获得流式）。
- 临时绕过（GLM/Kimi `kind` 改 `openai`，config.toml.bak 已备份）在代码修复合入后回退。

## 关联

- 参照 commit：`2d9945d`（OpenAI 流式改造，+152/-13 于 backends.rs）
- spike 实证：`research/2026-09-03-524-spike.md`
