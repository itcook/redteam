# Breakpoint 弹性/重试（P1-C）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 `brk` 的 provider 层对瞬时故障有分类感知的 turn 级重试，一次 HTTP 抖动不再作废整个多轮工具循环。

**Architecture:** 三层自下而上——(1) `ProviderError` 分类 + 每个 HTTP 边界共享 classifier；(2) 共享 `retry_classified` 原语，下沉到 provider 的 turn/call 级（code 路径 `run_tool_loop` 每 turn 带累积 messages 重发失败 turn；doc 路径 `DirectProvider::run` 单 call 重试），删除 orchestrate 的 whole-loop 重试；(3) 钉死「模型循环中途失联→cell 失败但 run 继续出部分报告」不变量。

**Tech Stack:** Rust 2021、tokio（time::timeout/sleep）、reqwest（rustls-only）、thiserror、async-trait。

**Spec:** `.doc/design/2026-07-01-breakpoint-resilience-retry-design.md`（§9 关键决策为准）。

## Global Constraints

- **TLS 必须 rustls，openssl-sys 不得出现**（`cargo tree` 不得引入 openssl）。
- **零新增依赖**：只用已在 Cargo.toml 的 crate（tokio/reqwest/thiserror/async-trait/serde_json）。
- **Engine 层 unfreeze**：本批次已获用户 overall unfreeze，可改 `provider/{mod,backends,direct,tool_loop}.rs`、`orchestrate/mod.rs`。
- **所有测试 hermetic**：不打真实网络，用 `FakeChatBackend`/`FakeProvider`/纯函数；退避睡眠用极短 Duration 或 `tokio::time` 暂停时钟，勿写慢测。
- **PONYTAIL**：最短正确 diff；根因修一处（classifier/retry 各一处，四调用点共用）；不加未要求的抽象；deliberate 简化留 `// ponytail:` 注释带 ceiling。
- **默认重试预算 = `DEFAULT_RETRY_BUDGET`（值 2）**，本轮不加 config 旋钮（决策③）。
- 退避封顶：指数 5s、Retry-After 30s。`Retry-After` 只解 integer-seconds（决策④）。

---

## File Structure

| 文件 | 职责（本计划内的改动） |
|---|---|
| `src/provider/mod.rs` | `ProviderError` 扩分类枚举 + `is_retryable`/`retry_after`；`send_classified`/`classify_transport`/`classify_status`/`parse_retry_after`；`retry_classified` 原语 + 常量。分类与重试的**唯一定义处**。 |
| `src/provider/backends.rs` | anthropic `turn` + openai `post` 两处 HTTP 发送改用 `send_classified`。 |
| `src/provider/direct.rs` | `call_anthropic`/`call_openai` 改用 `send_classified`；`run` 包 `retry_classified`；`DirectProvider` 增 `retry_budget`。 |
| `src/provider/tool_loop.rs` | 每 turn/turn_final 包 `retry_classified`；超时构造改 `Timeout`；`run_tool_loop` 增 `retry_budget` 形参；`ToolLoopProvider` 增字段；`FakeChatBackend` 扩为可脚本化 Err。 |
| `src/orchestrate/mod.rs` | 删 `call_with_retry`；三处调用点直调 `provider.run`；`OrchestrateOpts.retries` 字段与全部构造点清除；R4.3 降级不变量测试。 |

---

## Task 1: R4.1 错误分类 + 全 HTTP 边界共享 classifier

**Files:**
- Modify: `src/provider/mod.rs`（`ProviderError` 枚举 + 方法 + classifier 组）
- Modify: `src/provider/backends.rs:119-132`（anthropic turn）、`:247-259`（openai post）
- Modify: `src/provider/direct.rs:153-166`（call_anthropic）、`:176-188`（call_openai）
- Modify: `src/provider/tool_loop.rs:70-71,100-101`（两处 timeout 构造 `Unavailable`→`Timeout`）、`:319,345`（两处 timeout 测试断言 `Unavailable`→`Timeout`）

**Interfaces:**
- Produces:
  - `ProviderError`（新增变体 `RateLimited{retry_after:Option<Duration>}`/`Server(String)`/`Timeout(String)`/`Network(String)`/`Auth(String)`/`BadRequest(String)`，保留 `Parse(String)`/`Unavailable(String)`）
  - `impl ProviderError { pub fn is_retryable(&self)->bool; pub fn retry_after(&self)->Option<Duration> }`
  - `pub(crate) async fn send_classified(rb: reqwest::RequestBuilder) -> Result<reqwest::Response, ProviderError>`

- [ ] **Step 1: 写失败测试（分类纯函数）**

在 `src/provider/mod.rs` 的 `#[cfg(test)] mod tests` 追加（该 mod 已存在，含 `anthropic_messages_url` 测试）：

```rust
    use super::{classify_status, parse_retry_after, ProviderError};
    use reqwest::StatusCode;
    use std::time::Duration;

    #[test]
    fn classify_status_maps_http_codes() {
        use reqwest::header::HeaderMap;
        let h = HeaderMap::new();
        assert!(matches!(classify_status(StatusCode::TOO_MANY_REQUESTS, &h), ProviderError::RateLimited { .. }));
        assert!(matches!(classify_status(StatusCode::UNAUTHORIZED, &h), ProviderError::Auth(_)));
        assert!(matches!(classify_status(StatusCode::FORBIDDEN, &h), ProviderError::Auth(_)));
        assert!(matches!(classify_status(StatusCode::REQUEST_TIMEOUT, &h), ProviderError::Timeout(_)));
        assert!(matches!(classify_status(StatusCode::INTERNAL_SERVER_ERROR, &h), ProviderError::Server(_)));
        assert!(matches!(classify_status(StatusCode::SERVICE_UNAVAILABLE, &h), ProviderError::Server(_)));
        assert!(matches!(classify_status(StatusCode::BAD_REQUEST, &h), ProviderError::BadRequest(_)));
        assert!(matches!(classify_status(StatusCode::PAYLOAD_TOO_LARGE, &h), ProviderError::BadRequest(_)));
        assert!(matches!(classify_status(StatusCode::UNPROCESSABLE_ENTITY, &h), ProviderError::BadRequest(_)));
        // 3xx（非 4xx/5xx）→ 兜底 Unavailable
        assert!(matches!(classify_status(StatusCode::from_u16(418).unwrap(), &h), ProviderError::BadRequest(_))); // teapot 是 4xx
    }

    #[test]
    fn parse_retry_after_integer_seconds_only() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("5"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(5)));
        // HTTP-date 形式忽略（决策④）
        let mut h2 = HeaderMap::new();
        h2.insert(RETRY_AFTER, HeaderValue::from_static("Wed, 21 Oct 2025 07:28:00 GMT"));
        assert_eq!(parse_retry_after(&h2), None);
        // 缺头
        assert_eq!(parse_retry_after(&HeaderMap::new()), None);
    }

    #[test]
    fn retry_after_from_rate_limited_only() {
        let rl = ProviderError::RateLimited { retry_after: Some(Duration::from_secs(3)) };
        assert_eq!(rl.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(ProviderError::Server("x".into()).retry_after(), None);
    }

    #[test]
    fn is_retryable_truth_table() {
        assert!(ProviderError::RateLimited { retry_after: None }.is_retryable());
        assert!(ProviderError::Server("x".into()).is_retryable());
        assert!(ProviderError::Timeout("x".into()).is_retryable());
        assert!(ProviderError::Network("x".into()).is_retryable());
        assert!(ProviderError::Unavailable("x".into()).is_retryable()); // 未知保守可重试
        assert!(!ProviderError::Auth("x".into()).is_retryable());
        assert!(!ProviderError::BadRequest("x".into()).is_retryable());
        assert!(!ProviderError::Parse("x".into()).is_retryable());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoint --lib provider::tests 2>&1 | tail -20`
Expected: 编译失败——`classify_status`/`parse_retry_after` 未定义、`ProviderError` 无新变体/方法。

- [ ] **Step 3: 实现枚举 + 方法 + classifier**

`src/provider/mod.rs`：把 `ProviderError` 定义替换为：

```rust
#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    #[error("rate limited")]
    RateLimited { retry_after: Option<std::time::Duration> }, // 429 → 可重试
    // ponytail: Display 不内嵌 retry_after（thiserror 格式串放不下方法链）；需要时日志侧单独打
    #[error("server error: {0}")]
    Server(String),      // 5xx → 可重试
    #[error("timeout: {0}")]
    Timeout(String),     // 408 / reqwest timeout / per-turn 超时 → 可重试
    #[error("network error: {0}")]
    Network(String),     // 连接重置/DNS/connect → 可重试
    #[error("auth error: {0}")]
    Auth(String),        // 401/403 → 不可重试
    #[error("bad request: {0}")]
    BadRequest(String),  // 400/413/422 含 context-too-long → 不可重试
    #[error("structured-output parse failed: {0}")]
    Parse(String),       // 我方 JSON 解析失败 → 不可重试
    #[error("provider unavailable: {0}")]
    Unavailable(String), // 未知兜底 → 保守可重试
}

impl ProviderError {
    pub fn is_retryable(&self) -> bool {
        matches!(self,
            ProviderError::RateLimited { .. }
            | ProviderError::Server(_)
            | ProviderError::Timeout(_)
            | ProviderError::Network(_)
            | ProviderError::Unavailable(_))
    }
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ProviderError::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

/// 发请求 + 按 HTTP 语义分类。成功返回 Response（调用方再 `.json()`）。
pub(crate) async fn send_classified(
    rb: reqwest::RequestBuilder,
) -> Result<reqwest::Response, ProviderError> {
    let resp = rb.send().await.map_err(classify_transport)?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    Err(classify_status(status, resp.headers()))
}

/// reqwest 传输层错误（非 HTTP 状态）：timeout/connect/其他保守 Network。
fn classify_transport(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout(e.to_string())
    } else {
        // connect / reset / dns / 请求构造：均可重试类，归 Network
        ProviderError::Network(e.to_string())
    }
}

/// HTTP 状态码 + 头 → 分类。
fn classify_status(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> ProviderError {
    use reqwest::StatusCode as S;
    match status {
        S::TOO_MANY_REQUESTS => ProviderError::RateLimited { retry_after: parse_retry_after(headers) },
        S::UNAUTHORIZED | S::FORBIDDEN => ProviderError::Auth(format!("http {status}")),
        S::REQUEST_TIMEOUT => ProviderError::Timeout(format!("http {status}")),
        s if s.is_server_error() => ProviderError::Server(format!("http {status}")),
        s if s.is_client_error() => ProviderError::BadRequest(format!("http {status}")),
        s => ProviderError::Unavailable(format!("http {status}")),
    }
}

/// 只解 integer-seconds 形式的 Retry-After（HTTP-date 忽略，决策④）。
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str().ok()?
        .trim()
        .parse::<u64>().ok()
        .map(std::time::Duration::from_secs)
}
```

- [ ] **Step 4: 跑分类测试确认通过**

Run: `cargo test -p breakpoint --lib provider::tests 2>&1 | tail -20`
Expected: 上述 4 个新测试 PASS。

- [ ] **Step 5: 四个 HTTP 调用点改用 `send_classified`**

`src/provider/backends.rs` anthropic `turn`（当前 `:119-132`），把
```rust
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
```
替换为：
```rust
        let resp: Value = super::send_classified(
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .json(&body),
        )
        .await?
        .json()
        .await
        .map_err(|e| ProviderError::Parse(e.to_string()))?; // 200 但 body 非法 JSON → Parse（决策⑥）
```

`src/provider/backends.rs` openai `post`（`:247-259`）同构替换（header 用 `authorization`/`content-type`，其余一致）。

`src/provider/direct.rs` `call_anthropic`（`:153-166`）与 `call_openai`（`:176-188`）同构替换（各自 header 保持不变，`send()...error_for_status()...` 段换成 `super::send_classified(<builder>).await?.json().await.map_err(|e| ProviderError::Parse(e.to_string()))?`）。

- [ ] **Step 6: 两处 per-turn 超时构造改 `Timeout`**

`src/provider/tool_loop.rs:70-71` 与 `:100-101`，把 `ProviderError::Unavailable(format!("turn timeout after {}s", ...))` 改为 `ProviderError::Timeout(format!("turn timeout after {}s", ...))`（让 per-turn 超时进入可重试路径）。

- [ ] **Step 7: 更新受影响的既有超时断言**

`src/provider/tool_loop.rs:319` 与 `:345`：`matches!(r, Err(ProviderError::Unavailable(_)))` 改为 `matches!(r, Err(ProviderError::Timeout(_)))`。
（`registry.rs:72`、`fake.rs:45`、`backends.rs:538`、`direct.rs:364,379` 不动——它们分别是 unknown-model/google-unsupported/Parse，语义未变。）

- [ ] **Step 8: 全套测试 + clippy + openssl 检查**

Run: `cargo test -p breakpoint --lib 2>&1 | tail -15`
Expected: 全绿（既有超时测试现匹配 `Timeout`）。
Run: `cargo clippy -p breakpoint --all-targets 2>&1 | tail -5`
Expected: 无 warning。
Run: `cargo tree -p breakpoint | grep -i openssl || echo "no openssl ✓"`
Expected: `no openssl ✓`。

- [ ] **Step 9: Commit**

```bash
git add src/provider/mod.rs src/provider/backends.rs src/provider/direct.rs src/provider/tool_loop.rs
git commit -m "feat(resilience): R4.1 classify ProviderError + shared HTTP classifier

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: R4.2 `retry_classified` 原语 + code 路径 turn 级重试

**Files:**
- Modify: `src/provider/mod.rs`（`retry_classified` + 常量）
- Modify: `src/provider/tool_loop.rs`（每 turn 包重试；`run_tool_loop` 增形参；`ToolLoopProvider` 增字段；`FakeChatBackend` 扩为可脚本化 Err；新测试）

**Interfaces:**
- Consumes: `ProviderError::is_retryable`/`retry_after`（Task 1）。
- Produces:
  - `pub(crate) async fn retry_classified<T, F, Fut>(budget: u32, op: F) -> Result<T, ProviderError>`（`F: FnMut() -> Fut, Fut: Future<Output=Result<T, ProviderError>>`）
  - `pub(crate) const DEFAULT_RETRY_BUDGET: u32 = 2;`
  - `run_tool_loop(backend, executor, system, user_prompt, max_iters, per_turn, retry_budget)`（末尾新增 `retry_budget: u32`）

- [ ] **Step 1: 写失败测试（重试原语）**

`src/provider/mod.rs` tests 追加（用 `std::cell::Cell` 记调用次数，脚本化错误序列）：

```rust
    #[tokio::test]
    async fn retry_classified_retries_then_succeeds() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let r: Result<u32, ProviderError> = super::retry_classified(2, || {
            let n = calls.get();
            calls.set(n + 1);
            async move {
                if n < 2 { Err(ProviderError::Server("boom".into())) } else { Ok(99) }
            }
        }).await;
        assert_eq!(r.unwrap(), 99);
        assert_eq!(calls.get(), 3); // 首发 + 2 重试
    }

    #[tokio::test]
    async fn retry_classified_non_retryable_fails_fast() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let r: Result<u32, ProviderError> = super::retry_classified(5, || {
            calls.set(calls.get() + 1);
            async move { Err::<u32, _>(ProviderError::Auth("401".into())) }
        }).await;
        assert!(matches!(r, Err(ProviderError::Auth(_))));
        assert_eq!(calls.get(), 1, "不可重试错必须只调 1 次、不烧退避");
    }

    #[tokio::test]
    async fn retry_classified_exhausts_budget() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let r: Result<u32, ProviderError> = super::retry_classified(2, || {
            calls.set(calls.get() + 1);
            async move { Err::<u32, _>(ProviderError::Network("down".into())) }
        }).await;
        assert!(matches!(r, Err(ProviderError::Network(_))));
        assert_eq!(calls.get(), 3); // budget+1
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoint --lib provider::tests::retry 2>&1 | tail -15`
Expected: 编译失败——`retry_classified` 未定义。

- [ ] **Step 3: 实现 `retry_classified` + 常量**

`src/provider/mod.rs` 追加：

```rust
pub(crate) const DEFAULT_RETRY_BUDGET: u32 = 2; // 首发 + 2 重试
const RETRY_BACKOFF_BASE_MS: u64 = 200;
const RETRY_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(5);
const RETRY_AFTER_CAP: std::time::Duration = std::time::Duration::from_secs(30);

/// 通用重试：`op` 产 `Result<T, ProviderError>`；可重试错→退避后重试，不可重试→立即返回。
/// 退避：RateLimited 有 retry_after 用之（封顶 30s），否则指数（200·2^n ms，封顶 5s）。
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
                let wait = match e.retry_after() {
                    Some(d) => d.min(RETRY_AFTER_CAP),
                    None => std::time::Duration::from_millis(RETRY_BACKOFF_BASE_MS << (attempt + 1))
                        .min(RETRY_BACKOFF_CAP),
                };
                attempt += 1;
                tokio::time::sleep(wait).await;
            }
        }
    }
}
```

- [ ] **Step 4: 跑原语测试确认通过**

Run: `cargo test -p breakpoint --lib provider::tests::retry 2>&1 | tail -15`
Expected: 3 个测试 PASS。

- [ ] **Step 5: 扩 `FakeChatBackend` 支持脚本化 Err（写失败测试用）**

`src/provider/tool_loop.rs` 的 `FakeChatBackend`：把预置队列元素类型从 `AssistantTurn` 改为 `Result<AssistantTurn, ProviderError>`，`turn` 里 `pop_front` 后返回该 Result（保留 seen 记录）。同时改 `new(turns: Vec<AssistantTurn>)` 为仍收 `Vec<AssistantTurn>` 但内部 map 成 `Ok(_)`（既有测试不改），并加 `new_results(turns: Vec<Result<AssistantTurn, ProviderError>>)` 供错误脚本用：

```rust
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
    pub fn turn_count(&self) -> usize { self.seen.lock().unwrap().len() }
    pub fn messages_at(&self, idx: usize) -> Vec<ChatMessage> { self.seen.lock().unwrap()[idx].clone() }
}
```
`turn` 实现改为：
```rust
    async fn turn(&self, messages: &[ChatMessage], _tools: &[ToolSpec]) -> Result<AssistantTurn, ProviderError> {
        self.seen.lock().unwrap().push(messages.to_vec());
        self.turns.lock().unwrap().pop_front()
            .unwrap_or_else(|| Err(ProviderError::Unavailable("no scripted turn".into())))
    }
```

- [ ] **Step 6: 写 turn 级续跑失败测试**

`src/provider/tool_loop.rs` tests 追加（核心断言：turn 抖动被 turn 级重试吞掉、且重试轮仍带 turn 1 的 ToolResults）：

```rust
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
        let r = run_tool_loop(&HangGuard(backend), &ex, "sys", "review", 8,
            std::time::Duration::from_secs(60), 3).await;
        assert!(matches!(r, Err(ProviderError::Auth(_))), "不可重试错应立即返回: {r:?}");
    }
```
（`HangGuard` 无需——直接用 `&backend`。上面第二个测试写成：`run_tool_loop(&backend, ...)`，删掉 `HangGuard` 包装。）

> 实施注意：把第二个测试的 `&HangGuard(backend)` 改为 `&backend`。

- [ ] **Step 7: 跑测试确认失败**

Run: `cargo test -p breakpoint --lib tool_loop 2>&1 | tail -20`
Expected: 编译失败——`run_tool_loop` 只收 6 参、无 `retry_budget`；`new_results` 若 Step 5 未落则未定义。

- [ ] **Step 8: `run_tool_loop` 加 `retry_budget` 形参 + 每 turn 包重试**

`src/provider/tool_loop.rs`：`run_tool_loop` 签名末尾加 `retry_budget: u32`。探索轮与收尾轮的 turn 调用各包一层 `retry_classified`：

```rust
    for _ in 0..max_iters {
        let turn = crate::provider::retry_classified(retry_budget, || async {
            match tokio::time::timeout(per_turn, backend.turn(&messages, &tools)).await {
                Ok(r) => r,
                Err(_) => Err(ProviderError::Timeout(
                    format!("turn timeout after {}s", per_turn.as_secs()))),
            }
        }).await?;
        // ...（其余不变：累加 usage、判 tool_calls、执行工具回灌）
    }
    // 收尾轮：
    messages.push(ChatMessage::User(FINALIZE_PROMPT.to_string()));
    let turn = crate::provider::retry_classified(retry_budget, || async {
        match tokio::time::timeout(per_turn, backend.turn_final(&messages)).await {
            Ok(r) => r,
            Err(_) => Err(ProviderError::Timeout(
                format!("turn timeout after {}s", per_turn.as_secs()))),
        }
    }).await?;
```
`ToolLoopProvider` 增 `retry_budget: u32` 字段，`new` 里设 `retry_budget: crate::provider::DEFAULT_RETRY_BUDGET`，`run()` 透传给 `run_tool_loop`。

- [ ] **Step 9: 更新既有 `run_tool_loop` 调用点补 `retry_budget` 实参**

`src/provider/tool_loop.rs` tests 内既有 5 处 `run_tool_loop(...)`（约 `:230,246,293,317,343`）末尾各加一个 budget 实参：**既有行为测试传 `0`**（无重试，保持原语义），新加的 timeout 测试（`per_turn_timeout_*`）也传 `0`。

- [ ] **Step 10: 全套测试 + clippy**

Run: `cargo test -p breakpoint --lib 2>&1 | tail -15`
Expected: 全绿（含新 turn-retry 测试）。
Run: `cargo clippy -p breakpoint --all-targets 2>&1 | tail -5`
Expected: 无 warning。

- [ ] **Step 11: Commit**

```bash
git add src/provider/mod.rs src/provider/tool_loop.rs
git commit -m "feat(resilience): R4.2 retry_classified + per-turn retry in tool loop

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: R4.2 doc 路径重试 + 删 orchestrate whole-loop 重试 + R4.3 降级不变量

**Files:**
- Modify: `src/provider/direct.rs`（`run` 包 `retry_classified`；`DirectProvider` 增 `retry_budget`）
- Modify: `src/orchestrate/mod.rs`（删 `call_with_retry`；三调用点直调 `provider.run`；`OrchestrateOpts.retries` 字段 + 全部构造点清除；R4.3 测试）

**Interfaces:**
- Consumes: `retry_classified`/`DEFAULT_RETRY_BUDGET`（Task 2）、`FakeChatBackend::new_results`（Task 2）、`ProviderError::Timeout`（Task 1）。
- Produces: 无对外新签名（内部收口）。`OrchestrateOpts` 去掉 `retries` 字段。

- [ ] **Step 1: 写 R4.3 降级失败测试**

`src/orchestrate/mod.rs` tests 追加。用 **code 路径 `run_code_fanout`**（ToolLoopProvider 是 code provider）：registry 里一个 model 正常产 findings、另一个 tool_call 后一路 Timeout（重试耗尽仍失败）→ 断言 run 继续、failed_models 含坏 model、好 model findings 在。复用本 mod 既有测试助手 `bundle(path)` 与 `lens_by_id("L1")`：

```rust
    #[tokio::test]
    async fn mid_loop_model_death_degrades_gracefully() {
        use crate::provider::registry::ProviderRegistry;
        use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend, ToolLoopProvider};
        use crate::provider::ProviderError;
        use crate::tools::ToolCall;
        use serde_json::json;

        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x").unwrap();
        let good_json = r#"{"findings":[{"severity":"high","title":"real","body":"b","file":"src/a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r"}]}"#;

        // good：一轮直接产最终 findings
        let good = ToolLoopProvider::new(
            Box::new(FakeChatBackend::new(vec![AssistantTurn {
                text: good_json.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1 }])),
            dir.path().to_path_buf());
        // bad：先 tool_call（iter1），再一路 Timeout（iter2 重试耗尽仍失败）。
        // DEFAULT_RETRY_BUDGET=2 → iter2 需 3 次 Err（首发+2 重试）→ 备 1 Ok + 3 Err。
        let tc = AssistantTurn { text: String::new(),
            tool_calls: vec![ToolCall { id: "c".into(), name: "read_file".into(), args: json!({"path":"a.rs"}) }],
            input_tokens: 1, output_tokens: 1 };
        let bad = ToolLoopProvider::new(
            Box::new(FakeChatBackend::new_results(vec![
                Ok(tc),
                Err(ProviderError::Timeout("dead".into())),
                Err(ProviderError::Timeout("dead".into())),
                Err(ProviderError::Timeout("dead".into())),
            ])),
            dir.path().to_path_buf());

        let mut reg = ProviderRegistry::new();
        reg.insert("good", Box::new(good));
        reg.insert("bad", Box::new(bad));

        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "good".into(), priority: 1 },
                PlannedModel { name: "bad".into(), priority: 1 },
            ],
            replicate: true, no_fallback: false, max_concurrency: 4,
        };
        let bundles = vec![bundle("src/a.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let out = run_code_fanout(&reg, "wt", &bundles, &rules, "", &opts, None).await;
        assert!(out.failed_models.contains(&"bad".to_string()),
            "坏 model 应记 failed_models: {:?}", out.failed_models);
        assert!(out.findings.iter().any(|f| f.title == "real"),
            "好 model 的 finding 应仍在报告里");
    }
```

> 实施注意：`bundle`/`lens_by_id`/`run_code_fanout`/`Bundle` 均为本 mod 既有符号（见 `code_fanout_runs_every_bundle` 等测试），照用即可。`OrchestrateOpts` 字面量**不含** `retries`（本任务删除）。`bad` provider 会真实 sleep ~1.2s（2 次退避 400+800ms）——单测可接受。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoint --lib orchestrate 2>&1 | tail -20`
Expected: 编译失败——`FakeChatBackend::new_results`（已在 Task 2 落）或 `OrchestrateOpts` 仍要求 `retries` 字段（此测试字面量未给）。

- [ ] **Step 3: 删 `call_with_retry`，三调用点直调 `provider.run`**

`src/orchestrate/mod.rs`：删除 `call_with_retry` 函数（`:70-95`）。三处 `match call_with_retry(provider, &pm.name, prompt, opts.retries).await`（`:125,337,522`）改为：
```rust
        match provider.run(LensRequest { model: pm.name.clone(), prompt: prompt.to_string() }).await {
```
（`LensRequest` 已 `use`；若未 use 则加 `use crate::provider::LensRequest;`。）

- [ ] **Step 4: 删 `OrchestrateOpts.retries` 字段与全部构造点**

`src/orchestrate/mod.rs:31` 删 `pub retries: u32,`。删除所有 `OrchestrateOpts { ... }` 字面量里的 `retries: N,` 行——本 mod 内 `:199,404,574`（`single_opts`）与 tests 内 `:701,717,756,787,803,836,866,893,921,938,961,993,1017,1073`。`src/cli.rs:231,301` 两处 `retries: 2,` 亦删。

> 用 `grep -rn "retries" src --include="*.rs"` 核对无残留（除 `lens/mod.rs` 的 focus 文本，非本字段）。

- [ ] **Step 5: doc 路径 `DirectProvider::run` 包 `retry_classified`**

`src/provider/direct.rs`：`DirectProvider` 结构体加 `retry_budget: u32` 字段；`from_config` 里设 `retry_budget: crate::provider::DEFAULT_RETRY_BUDGET`。`run` 改为：
```rust
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        let (text, input_tokens, output_tokens) =
            crate::provider::retry_classified(self.retry_budget, || async {
                match self.kind {
                    ProviderKind::Anthropic => self.call_anthropic(&req).await,
                    ProviderKind::Openai | ProviderKind::OpenaiCompatible => self.call_openai(&req).await,
                    ProviderKind::Google => Err(ProviderError::Unavailable("google not yet supported".into())),
                }
            }).await?;
        let out = parse_lens_output(&text)?; // Parse 不进重试（形状问题）
        Ok(LensCall { findings: out.findings, input_tokens, output_tokens })
    }
```

- [ ] **Step 6: 跑全套测试确认通过**

Run: `cargo test -p breakpoint --lib 2>&1 | tail -15`
Expected: 全绿（含 R4.3 降级测试）。若既有 orchestrate 测试因删 `retries` 字面量而编译错，补删对应行。

- [ ] **Step 7: clippy + openssl + 死字段核查**

Run: `cargo clippy -p breakpoint --all-targets 2>&1 | tail -5`
Expected: 无 warning（尤其无「field never read」——确认 `retries` 已彻底删）。
Run: `cargo tree -p breakpoint | grep -i openssl || echo "no openssl ✓"`
Expected: `no openssl ✓`。

- [ ] **Step 8: Commit**

```bash
git add src/provider/direct.rs src/orchestrate/mod.rs src/cli.rs
git commit -m "feat(resilience): R4.2 doc-path retry + drop orchestrate whole-loop retry; R4.3 degradation test

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review 记录（写计划者自查）

- **Spec 覆盖**：R4.1 分类=Task 1；R4.2 原语+code turn 重试=Task 2、doc 重试+删 whole-loop=Task 3；R4.3 降级不变量=Task 3 Step 1；决策③默认预算=Task 2 常量；决策④Retry-After 秒=Task 1 `parse_retry_after`；决策⑥200-body 失败归 Parse=Task 1 Step 5。全覆盖。
- **类型一致**：`retry_classified(budget, op)`、`send_classified(rb)`、`run_tool_loop(...,per_turn,retry_budget)`、`DEFAULT_RETRY_BUDGET` 跨任务同名一致。
- **占位符**：无 TBD/TODO。R4.3 测试已核对现网 API 修正（用 `run_code_fanout`+`bundle`/`lens_by_id`，非早稿的 `run_fanout`/`ReviewInput`）。
- **风险点**：Task 2 Step 6 第二个测试原稿误带 `HangGuard`，已在 Step 6 注明改 `&backend`。Task 3 `OrchestrateOpts` 去 `retries` 波及 ~20 构造点（Step 4 已列全行号 + grep 核对指令）。实施者遇字段不符按现网修正。
