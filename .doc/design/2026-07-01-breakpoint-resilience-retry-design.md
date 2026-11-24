# Breakpoint 弹性/重试设计（P1-C：错误分类 + turn 级重试 + 优雅降级）

> 来源：`.doc/research/2026-06-30-live-review-operability-lessons.md` 主题 C（R4.1/4.2/4.3）。
> 前置：可靠性内核 P0（`.doc/design/2026-06-30-breakpoint-reliability-core-design.md`）已交付
> HTTP/per-turn 超时、manifest+partial checkpoint、job deadline、心跳、prune wedge、`--resume`。
> 本 spec 是同一可靠性批次的续篇，延用 engine 层 unfreeze 授权。

## 1. 动机

19 轮真实 codex 对抗评审里，最痛的一类反复故障是「**reconnect 风暴杀整轮**」：
单次 HTTP turn 的瞬时抖动，让一整个多轮工具循环（已读文件、已累积上下文、已烧 token）
全部作废、20–34min 重来。根因有二：

1. **错误不分类**：现有 `ProviderError` 只有 `Unavailable(String)`/`Parse(String)` 两态；
   `call_with_retry` 对任何错误盲目指数退避、不读 `Retry-After`、不分可重试与否——
   400/401 也照烧 3 次退避才失败。
2. **重试粒度太粗**：重试包在整个 `provider.run()`（= 整个工具循环）外。turn 8 抖动
   会丢弃 turn 1–7 的全部已读上下文，从头重跑。

本 spec 补三层，自下而上：**分类 → turn 级重试 → 优雅降级**。

## 2. 范围

**做**：
- R4.1 `ProviderError` 分类 + `is_retryable()`/`retry_after()` + 四个 HTTP 调用点共享 classifier。
- R4.2 共享重试原语，下沉到 provider turn/call 级：code 路径每 turn 重试（带累积 messages）、
  doc 路径单次调用重试；删除 orchestrate 的 whole-loop `call_with_retry`。
- R4.3 钉死「模型循环中途失联 → cell 失败但 run 继续出部分报告」不变量（补测试）。

**不做（YAGNI / defer）**：
- 跨 cell「熔断已死模型」（一个 model 首次 Auth 即跳过其余 cell）——纯效率优化，
  R4.1 快速失败后收益已小，需 cell 间共享状态。见 §9 决策⑤。
- `Retry-After` 的 HTTP-date 形式解析（只解 integer-seconds，见 §9 决策④）。
- 熔断/断路器、请求排队限流、跨 job 全局速率控制——超出 P1-C。

## 3. R4.1 错误分类

### 3.1 枚举

`src/provider/mod.rs`：`ProviderError` 扩为：

```rust
#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    #[error("rate limited")]
    RateLimited { retry_after: Option<std::time::Duration> }, // 429 → 可重试，尊重 Retry-After
    // ponytail: Display 不内嵌 retry_after（thiserror 格式串放不下方法链）；需要时日志侧单独打
    #[error("server error: {0}")]
    Server(String),        // 5xx → 可重试
    #[error("timeout: {0}")]
    Timeout(String),       // 408 / reqwest timeout / per-turn 超时 → 可重试
    #[error("network error: {0}")]
    Network(String),       // 连接重置 / DNS / connect 失败 → 可重试
    #[error("auth error: {0}")]
    Auth(String),          // 401/403 → 不可重试（模型已死）
    #[error("bad request: {0}")]
    BadRequest(String),    // 400/413/422，含 context-too-long → 不可重试
    #[error("structured-output parse failed: {0}")]
    Parse(String),         // 我方 JSON 解析失败 → 不可重试（既有）
    #[error("provider unavailable: {0}")]
    Unavailable(String),   // 未知兜底 → 保守当可重试（既有，保留）
}
```

### 3.2 判定方法

```rust
impl ProviderError {
    /// 可重试类：瞬时/服务侧，重发同一请求有望成功。
    pub fn is_retryable(&self) -> bool {
        matches!(self,
            ProviderError::RateLimited { .. }
            | ProviderError::Server(_)
            | ProviderError::Timeout(_)
            | ProviderError::Network(_)
            | ProviderError::Unavailable(_))  // 未知保守可重试
    }
    /// 建议等待时长（仅 RateLimited 带 Retry-After 时非 None）。
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ProviderError::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}
```

不可重试：`Auth` / `BadRequest` / `Parse`。

### 3.3 共享 classifier

四个 HTTP 调用点现状（backends.rs 的 anthropic `turn` + openai `post`；direct.rs 的
`call_anthropic` + `call_openai`）都是同一模式：

```rust
.send().await
.and_then(|r| r.error_for_status())          // 塌掉状态码
.map_err(|e| ProviderError::Unavailable(e.to_string()))?   // 丢 Retry-After
.json().await.map_err(|e| ProviderError::Unavailable(e.to_string()))?
```

替换为 `src/provider/mod.rs` 里一个共享 helper，四处共用（一处根治）：

```rust
/// 发请求 + 按 HTTP 语义分类。成功返回 Response（调用方再 .json()）。
pub(crate) async fn send_classified(rb: reqwest::RequestBuilder) -> Result<reqwest::Response, ProviderError> {
    let resp = rb.send().await.map_err(classify_transport)?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    Err(classify_status(status, resp.headers()))
}

/// reqwest 传输层错误（非 HTTP 状态）：timeout/connect/其他。
fn classify_transport(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout(e.to_string())
    } else if e.is_connect() {
        ProviderError::Network(e.to_string())
    } else {
        ProviderError::Network(e.to_string())  // 重置/请求构造等，保守 Network（可重试）
    }
}

/// HTTP 状态码 + 头 → 分类。
fn classify_status(status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> ProviderError {
    use reqwest::StatusCode as S;
    match status {
        S::TOO_MANY_REQUESTS => ProviderError::RateLimited { retry_after: parse_retry_after(headers) },
        S::UNAUTHORIZED | S::FORBIDDEN => ProviderError::Auth(format!("http {status}")),
        S::REQUEST_TIMEOUT => ProviderError::Timeout(format!("http {status}")),
        s if s.is_server_error() => ProviderError::Server(format!("http {status}")),
        s if s.is_client_error() => ProviderError::BadRequest(format!("http {status}")), // 400/413/422 等
        s => ProviderError::Unavailable(format!("http {status}")),
    }
}

/// 只解 integer-seconds 形式的 Retry-After（HTTP-date 形式忽略，见 §9④）。
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    headers.get(reqwest::header::RETRY_AFTER)?
        .to_str().ok()?
        .trim().parse::<u64>().ok()
        .map(std::time::Duration::from_secs)
}
```

调用点改写（backends.rs anthropic 示例）：

```rust
let resp: Value = super::send_classified(
        self.client.post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
    ).await?
    .json().await.map_err(|e| ProviderError::Parse(e.to_string()))?;  // 成功响应体解析失败 → Parse（不可重试，是形状问题非瞬时）
```

其余三处同构改写。`tool_loop.rs` 两处 `Unavailable("turn timeout after Ns")` 改
`Timeout("turn timeout after Ns")`（让 per-turn 超时进入可重试路径）。

## 4. R4.2 turn 级重试

### 4.1 共享原语

`src/provider/mod.rs`：

```rust
pub(crate) const DEFAULT_RETRY_BUDGET: u32 = 2;      // 首发 + 2 重试 = 至多 3 次
const RETRY_BACKOFF_BASE_MS: u64 = 200;
const RETRY_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(5);
const RETRY_AFTER_CAP: std::time::Duration = std::time::Duration::from_secs(30); // 尊重但封顶

/// 通用重试：op 产出 Result<T, ProviderError>；可重试错→退避后重试，不可重试→立即返回。
/// 退避：RateLimited 有 retry_after 则用之（封顶 RETRY_AFTER_CAP），否则指数（封顶 CAP）。
pub(crate) async fn retry_classified<T, F, Fut>(budget: u32, mut op: F) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ProviderError>>,
{
    let mut attempt = 0u32;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                if attempt >= budget || !e.is_retryable() {
                    return Err(e);
                }
                let wait = e.retry_after()
                    .map(|d| d.min(RETRY_AFTER_CAP))
                    .unwrap_or_else(|| std::time::Duration::from_millis(
                        RETRY_BACKOFF_BASE_MS << (attempt + 1)).min(RETRY_BACKOFF_CAP));
                attempt += 1;
                tokio::time::sleep(wait).await;
            }
        }
    }
}
```

### 4.2 code 路径（run_tool_loop 内）

`run_tool_loop` 里每个 `backend.turn(&messages,&tools)` / `backend.turn_final(&messages)`
用 `retry_classified` 包一层，**闭包每次借用同一份累积 `messages`**——瞬时抖动只重发这一 turn，
turn 1..n-1 的已读上下文保住。per-turn `tokio::time::timeout` 留在闭包**内**（每次尝试各自受
150s 约束，超时→`Timeout`→可重试）。

```rust
// 探索轮（示意）：
let turn = retry_classified(retry_budget, || async {
    match tokio::time::timeout(per_turn, backend.turn(&messages, &tools)).await {
        Ok(r) => r,
        Err(_) => Err(ProviderError::Timeout(format!("turn timeout after {}s", per_turn.as_secs()))),
    }
}).await?;
```

`run_tool_loop` 签名增 `retry_budget: u32`；`ToolLoopProvider` 增 `retry_budget` 字段
（构造默认 `DEFAULT_RETRY_BUDGET`），`run()` 透传。收尾轮 `turn_final` 同构包裹。

### 4.3 doc 路径（DirectProvider::run）

单发无循环，turn≡call：`DirectProvider::run` 里 `call_anthropic`/`call_openai` 外包
同一 `retry_classified`。`DirectProvider` 增 `retry_budget` 字段（构造默认）。

```rust
let (text, it, ot) = retry_classified(self.retry_budget, || async {
    match self.kind {
        ProviderKind::Anthropic => self.call_anthropic(&req).await,
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => self.call_openai(&req).await,
        ProviderKind::Google => Err(ProviderError::Unavailable("google not yet supported".into())),
    }
}).await?;
let out = parse_lens_output(&text)?;  // Parse 不进重试（形状问题）
```

### 4.4 删除 orchestrate whole-loop 重试

`src/orchestrate/mod.rs`：删 `call_with_retry`，`run_one_lens`/`run_one_*` 直接
`provider.run(req).await`。`OrchestrateOpts.retries` 字段移除（或保留但不再被消费——
优先移除以免误导；调用点 `retries: 2/0` 一并清）。重试预算改由 provider 构造时设定。

**语义变化**：删除后，orchestrate 不再重试。候补链（非 replicate）与 failed_models
（replicate）逻辑不变——单次 `provider.run` 失败即候补下一个 / 记 failed。provider 内部
已在 turn/call 级重试过，故到达 orchestrate 的 Err 已是「重试后仍失败」的终态。

## 5. R4.3 优雅降级

**已结构性满足**：replicate 下每 (lens/bundle × model) 是独立 cell（`run_fanout` 的
join_all），失败即 `out.failed_models.push` + warning，run 继续、其余 cell 照常汇入报告
（`orchestrate/mod.rs:229-243`）。R4.1 让 Auth/400 **1 次快速失败**（而非烧满退避）已吃掉
大半「死模型拖慢每个 cell」的浪费。

**本 spec 只补一条测试**钉死不变量：某 model 在工具循环**中途** turn 失败（重试耗尽仍失败）
→ 该 cell 返回 Err → run 继续、`failed_models` 含该 model、其余 model 的 findings 仍在报告里。
用 `FakeChatBackend` 脚本化「先 tool_call、再持续 Err」的 backend 构造。

**不做**：跨 cell 熔断（见 §2、§9⑤）。

## 6. 测试策略

全部 hermetic（不打网络），用 `FakeChatBackend` / `FakeProvider` / 纯函数。

**R4.1 分类（纯函数，`provider/mod.rs` tests）**：
- `classify_status`：429→RateLimited、401/403→Auth、500/503→Server、408→Timeout、
  400/413/422→BadRequest、418→Unavailable。
- `parse_retry_after`：`"5"`→Some(5s)；`"Wed, 21 Oct..."`（HTTP-date）→None；缺头→None。
- `is_retryable()`：RateLimited/Server/Timeout/Network/Unavailable=true；Auth/BadRequest/Parse=false。
- `retry_after()`：仅 RateLimited{Some} 返回 Some。

**R4.2 重试原语（`retry_classified`）**：
- 可重试错第 N 次成功 → 返回 Ok，op 被调用 N 次。
- 不可重试错（Auth）→ **立即返回、op 只调 1 次**（不烧退避）。
- 预算耗尽 → 返回最后一次 Err，op 调 `budget+1` 次。
- RateLimited{retry_after:Some(小值)} → sleep 用该值（用 `tokio::time` 暂停时钟断言，
  或仅断言调用次数/短真实值避免慢测）。

**R4.2 turn 级续跑（`run_tool_loop`，扩展 `FakeChatBackend`）**：
- 脚本 turn 序列 `[tool_call, Err(Timeout), final_json]`，budget≥1：
  Err 被 turn 级重试吞掉，最终产出 findings；**且重试那一 turn 收到的 messages 仍含
  turn 1 的 ToolResults**（证明累积上下文未丢）——这是 R4.2 的核心断言。
- 不可重试 Err（Auth）在探索轮 → `run_tool_loop` 立即返回 Err，不进收尾轮。

**R4.3 降级（`run_fanout`，replicate）**：
- 两 model，其一 backend 中途 turn 持续 Err（重试后仍失败）→ 返回的 RawResults：
  失败 model 进 `failed_models`、成功 model 的 findings 在 `findings` 里、run 不 panic。

**回归**：既有 `per_turn_timeout_returns_err_not_hang` / `per_turn_timeout_covers_finalize_round`
断言从 `Unavailable(_)` 改为 `Timeout(_)`（超时归类变更）。既有 direct.rs
`bad_response_shape_maps_to_parse_error` 不变。

## 7. 触及文件

| 文件 | 改动 |
|---|---|
| `src/provider/mod.rs` | `ProviderError` 扩枚举 + `is_retryable`/`retry_after`；`send_classified`/`classify_transport`/`classify_status`/`parse_retry_after`；`retry_classified` 原语 + 常量 |
| `src/provider/backends.rs` | anthropic `turn` + openai `post` 两处改用 `send_classified` |
| `src/provider/direct.rs` | `call_anthropic`/`call_openai` 改用 `send_classified`；`run` 包 `retry_classified`；`retry_budget` 字段 |
| `src/provider/tool_loop.rs` | 每 turn/turn_final 包 `retry_classified`；超时改 `Timeout`；`run_tool_loop` 增 `retry_budget` 参数；`ToolLoopProvider` 增字段；`FakeChatBackend` 扩为可脚本化 Err |
| `src/orchestrate/mod.rs` | 删 `call_with_retry`；`run_one_*` 直调 `provider.run`；`OrchestrateOpts.retries` 移除；R4.3 降级测试 |
| `src/main.rs` / provider 构造点 | provider 构造用 `DEFAULT_RETRY_BUDGET` 默认（本轮不加 config 旋钮，见 §9③；`ToolLoopProvider::new`/`DirectProvider::from_config` 内部设默认，构造点通常无需改动） |

## 8. Engine 层 unfreeze

R4.1/4.2 必然改动常态冻结的 engine 层（`provider/backends.rs`、`tool_loop.rs`、
`provider/mod.rs`、`direct.rs`）。延用可靠性批次的 overall unfreeze 授权——本 spec 是
同一批次（同一经验文档 P1）的续篇，不另开授权。

## 9. 关键决策

① **重试放 provider turn/call 级共享原语**（而非只改 code 路径、doc 留 orchestrate）：
一套 `retry_classified` 策略，code（每 turn）与 doc（单 call）复用，orchestrate 不再重试。
根因：turn 级续跑是 R4.2 的硬要求，共享原语避免两套退避逻辑漂移。

② **`ProviderError` 保留 `Unavailable` 兜底**（而非全塌成分类枚举）：只加 HTTP 分类需要的
变体，`Unavailable` 留作未知兜底并**保守判为可重试**。避免动到 registry/google-unsupported
等一堆既有构造点，缩小 diff。

③ **默认重试预算 2**（首发+2=3 次）：与既有 `opts.retries=2` 对齐。**与 deadline 交互**：
最坏 2 retry × 150s per-turn = 单 turn 至多 ~450s；job 40min deadline 兜底。故 budget 保守取
2 不放大；退避封顶 5s（指数）/ 30s（Retry-After），避免单次抖动吃掉过多墙钟。

④ **`Retry-After` 只解 integer-seconds**：HTTP-date 形式（`"Wed, 21 Oct 2025..."`）忽略、
回落指数退避。主流 LLM 网关的 429 用秒数；HTTP-date 罕见，不值得引 chrono。
`// ponytail: integer-seconds only; add HTTP-date parse if a provider needs it`。

⑤ **不做跨 cell 熔断**（一个 model 首次 Auth 即跳过其余 cell）：R4.1 快速失败后，死模型
每 cell 只烧 1 次而非 3 次退避，收益已小；跨 cell 熔断需 join_all 内共享状态（AtomicBool/
DashSet），复杂度不匹配剩余收益。defer，实测证明必要再加。

⑥ **成功响应体 `.json()` 解析失败 → `Parse`**（不可重试）：HTTP 200 但 body 不是合法 JSON
是形状问题非瞬时，重发无益。与既有 `parse_lens_output` 的 Parse 语义一致。

## 10. 非目标

- 断路器 / 全局限流 / 请求排队。
- 跨 job 速率协调。
- `Retry-After` HTTP-date。
- 跨 cell 死模型熔断（§9⑤）。
- provider 侧流式/SSE。
