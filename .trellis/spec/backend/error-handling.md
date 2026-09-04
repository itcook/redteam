# Error Handling

> How errors are handled in this project — provider 层（LLM HTTP 收发）错误分类与流式收发约定。
> 来源：09-03-anthropic-streaming 任务沉淀（含 dogfood 对抗评审修正）。

---

## Provider 收发两条铁律（2026-09-03 定）

### 1. 流式优先：所有 backend 请求体带 `stream: true`

非 stream 长思考期连接静默 → 隧道/网关当空闲 reset、或撞 Cloudflare 524（~100s 无首字节）。
流式让服务器随 token 增量发、reqwest 随到达消费 socket → 连接持续活跃。
`resp.text()` 一次读全 SSE 流即可（消费期间连接活跃），无需逐事件处理。

### 2. 响应体三层分流（免疫 `data:` 子串误路由）

判定顺序（`AnthropicBackend::turn` / `OpenAiBackend::post` 镜像实现）：

1. **整体 JSON 优先**：`from_str::<Value>(body.trim())` 成功 → 非 SSE（端点无视 stream 的
   完整响应 / 200-内-error 体 / 正文含 `data:` 子串的 JSON——YAML 键、代码引文常见）。
   先探 `error_body_to_provider_error`，再走非流式 parse。
2. **行首锚定 SSE**：`looks_like_sse`（任意行 trim 后以 `data:` 开头）→ 先探 SSE 帧 error
   事件，再聚合事件流。**禁止**用 `contains("data:")` 子串判定——会把含该子串的 JSON 误
   路由进 SSE 解析器 → `saw_event=false` → 误判不可重试 `Parse` 烧 cell。
3. **envelope 兜底**：既非合法 JSON 又无 SSE 行（HTML 网关页等）→
   `parse_envelope_json` → `Unavailable`（可重试）+ body 前 200 字节 preview。

## 错误分类语义（误判代价不对称：可重试误判为 Parse = 一次抖动烧掉整个 cell）

| 分类 | 触发 | 可重试 |
|---|---|---|
| `Unavailable` | 空体 / envelope 非法 JSON / **流截断**（SSE 无 finish_reason / message_delta 未达） | ✓ |
| `RateLimited` / `Timeout` / `Server` / `Network` | 429+Retry-After / 408+reqwest timeout / 5xx / 传输层 | ✓ |
| `Auth` / `BadRequest` | 401/403 / 400（含 max_tokens 超模型上限） | ✗ |
| `Parse` | 模型**内容**形状解析失败（envelope 已 OK 但无期望字段 / repair 后仍畸形 JSON） | ✗ |

- **200-内-error 体**：网关会把 `{"error":{...}}` 塞进 HTTP 200——parse 前必探
  `error_body_to_provider_error`（只认顶层 `error` 对象键，覆盖 OpenAI/Anthropic 双形状，
  子串匹配宽容方言）。SSE 帧内 `type=="error"` 事件同样处理。
- **流截断 vs 模型畸形**：anthropic 侧 finish_reason 完整性门——`finish_reason == None`
  → `Unavailable` 可重试（proxy 早断/网关放弃），repair 轮救不了断流、重试才救得了。
  tool args 非空解析失败 → 传导为 `finish_reason = None`（不静默落 `{}` 让工具带空参跑垃圾）。
- **未知 error 类型保守可重试**：宁多重试一次，不把陌生错误当永久失败。

## 超时/限额旋钮（优先级：per-provider config > env > 默认）

| 旋钮 | 层 | 默认 | 说明 |
|---|---|---|---|
| `providers.X.per_turn_timeout_secs` | 单轮（config） | — | 慢速模型按 `max_tokens ÷ 模型最低 tok/s + prefill 余量` 配（Kimi 30tok/s×32K→1200s） |
| `REDTEAM_PER_TURN_SECS` | 单轮（env 全局） | 150 | 粗调通道；速度差 2 倍的模型用同一个值必然顾此失彼 |
| `REDTEAM_HTTP_TIMEOUT_SECS` | HTTP 整请求 | 300 | 与 per-turn 联动：必须 ≥ per-turn，否则先掐 |
| `REDTEAM_MAX_TOKENS` | 输出上限（anthropic 必填/openai 显式） | 32768 | thinking tokens 计入额度；Claude 3/3.5 直连需按模型调低 |

非法值一律 eprintln 警告后回落默认（不静默——运维误以为旋钮生效会复发且难诊断）。
