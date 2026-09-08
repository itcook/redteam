pub mod backends;
pub mod fake;
pub mod parse;
pub mod registry;
pub mod single_shot;
pub mod tool_loop;

use crate::domain::RawFinding;
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct LensRequest { pub route_key: String, pub prompt: String }

/// iterations：本次调用实际消耗的 tool-loop 逻辑迭代数（计 FINALIZE 强制收尾轮、
/// 不计 turn 级 retry 重试）。成功恒 >=1（单发单收即 1）；0 保留给「失败/未观测」。
#[derive(Clone, Debug)]
pub struct LensCall { pub findings: Vec<RawFinding>, pub input_tokens: u64, pub output_tokens: u64, pub cached_input_tokens: u64, pub iterations: u32, pub tool_use: crate::tools::ToolUse, pub needs: Vec<crate::domain::NeedEntry> }

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
    /// T25：整链失败 token 入账——多轮 tool_loop/single_shot 在最终报错前已真实烧掉的
    /// usage，原先随 `?`/Err 一起被丢弃（`orchestrate::run_one_bundle` 候补链全灭时
    /// `CellRun.tokens` 恒 (0,0,0)，与真实花费不符）。只在 usage 非零时才包一层
    /// （见 `attach_usage`），零使用量的错误路径（如首轮就 Auth 失败）字节级不变，
    /// 现有 `matches!(err, ProviderError::X(_))` 断言不受影响。
    #[error("{source}")]
    WithUsage {
        #[source]
        source: Box<ProviderError>,
        /// (input, output, cached_input) —— 与 `LensCall`/`CellRun.tokens` 同口径。
        usage: (u64, u64, u64),
    },
}

impl ProviderError {
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::RateLimited { .. }
            | ProviderError::Server(_)
            | ProviderError::Timeout(_)
            | ProviderError::Network(_)
            | ProviderError::Unavailable(_) => true,
            ProviderError::WithUsage { source, .. } => source.is_retryable(),
            _ => false,
        }
    }
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ProviderError::RateLimited { retry_after } => *retry_after,
            ProviderError::WithUsage { source, .. } => source.retry_after(),
            _ => None,
        }
    }
    /// 本错误携带的已烧 usage；非 `WithUsage` 恒 (0,0,0)（未观测/无花费两种情形不区分，
    /// 调用方原本就以 0 处理）。
    pub fn burned_usage(&self) -> (u64, u64, u64) {
        match self {
            ProviderError::WithUsage { usage, .. } => *usage,
            _ => (0, 0, 0),
        }
    }
    /// 见穿 `WithUsage` 包装取内层错误的引用——`verify.rs` 等按变体分派的调用点用它
    /// 兼容新包装，不必对每个匹配点都补一层 `WithUsage` 分支。
    pub fn inner(&self) -> &ProviderError {
        match self {
            ProviderError::WithUsage { source, .. } => source.inner(),
            other => other,
        }
    }
    /// 把已烧 usage 附着到错误上；usage 全零时原样返回（不引入包装，保持既有错误路径
    /// 字节级不变）——多轮调用中途失败、且之前的轮次真花费了 token 时才需要它。
    pub fn attach_usage(self, usage: (u64, u64, u64)) -> ProviderError {
        if usage == (0, 0, 0) {
            self
        } else {
            ProviderError::WithUsage { source: Box::new(self), usage }
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

/// 把 2xx 响应体读为 JSON。**envelope 层**解码失败（空 body / 纯 keep-alive 空白 /
/// 非 JSON 网关页——典型上游抖动或推理模型长思考后放弃）分类为 `Unavailable`（可重试），
/// 而非 `Parse`。`Parse` 专指我方对**模型内容**的形状解析失败（不可重试）。
/// 这是本层与 `retry_classified` 的接缝：误判为 Parse 会绕过重试，一次抖动烧掉整个 cell。
/// 现由 Anthropic backend 的非 SSE 回退路径直接消费（SSE 主路径不需要整体 JSON 解码）。
pub(crate) fn parse_envelope_json(text: &str, url: &str) -> Result<serde_json::Value, ProviderError> {
    serde_json::from_str(text).map_err(|e| {
        let preview: String = text.chars().take(200).collect();
        ProviderError::Unavailable(format!(
            "envelope decode failed for {url}: {e}; body[0..200]={preview:?}"
        ))
    })
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
        _ => ProviderError::Unavailable(format!("http {status}")),
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

pub(crate) const DEFAULT_RETRY_BUDGET: u32 = 2; // 首发 + 2 重试
/// 单轮默认最长等待；tool_loop 与 single_shot 共用（原各私有一份，Task 2 评审 Minor 收敛）。
pub(crate) const PER_TURN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(150);
/// 单轮最长等待时间，env `REDTEAM_PER_TURN_SECS` 可覆盖——大上下文 bundle（40+ 文件）
/// 配深思考模型（glm/kimi 等）单轮思考+生成可超 150s 被掐成 `turn timeout`（2026-09-03
/// dogfood 实证，非网络不可达而是慢）。校准旋钮，对齐 `REDTEAM_HTTP_TIMEOUT_SECS`（9f7540b）
/// 先例。HTTP 整请求超时由 `backend_from_config` 按该模型 per_turn 自动抬（`http_timeout_for`，评审 M2），
/// 不必再手动同步 `REDTEAM_HTTP_TIMEOUT_SECS`；env 解析与 `config::EffectiveModel::per_turn_secs` 共用 `config::env_per_turn_secs`（评审 S6）。
pub fn per_turn_deadline() -> std::time::Duration {
    crate::config::env_per_turn_secs().map(std::time::Duration::from_secs).unwrap_or(PER_TURN_DEADLINE)
}
const RETRY_BACKOFF_BASE_MS: u64 = 200;
const RETRY_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(5);
const RETRY_AFTER_CAP: std::time::Duration = std::time::Duration::from_secs(30);

/// 通用重试：`op` 产 `Result<T, ProviderError>`；可重试错→退避后重试，不可重试→立即返回。
/// 退避：RateLimited 有 retry_after 用之（封顶 30s），否则指数（200·2^(n+1) ms，
/// 首试 400ms，封顶 5s；n=attempt 从 0 计，故首次退避即左移一位而非 200ms）。
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

#[async_trait]
pub trait ReviewProvider: Send + Sync {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError>;
}

/// 单轮 timeout+retry 包装（带 tools，对应 `ChatBackend::turn`）：超时归 `ProviderError::Timeout`，
/// 外层套 `retry_classified`。tool_loop 与 single_shot 共享，避免各自手写同一手写块（B2 抽取）。
pub(crate) async fn timed_turn(
    backend: &dyn crate::provider::tool_loop::ChatBackend,
    messages: &[crate::provider::tool_loop::ChatMessage],
    tools: &[crate::tools::ToolSpec],
    per_turn: std::time::Duration,
    retry_budget: u32,
) -> Result<crate::provider::tool_loop::AssistantTurn, ProviderError> {
    retry_classified(retry_budget, || async {
        match tokio::time::timeout(per_turn, backend.turn(messages, tools)).await {
            Ok(r) => r,
            Err(_) => Err(ProviderError::Timeout(
                format!("turn timeout after {}s", per_turn.as_secs()))),
        }
    }).await
}

/// 同上，收尾轮版本（对应 `ChatBackend::turn_final`，不带 tools）。
pub(crate) async fn timed_turn_final(
    backend: &dyn crate::provider::tool_loop::ChatBackend,
    messages: &[crate::provider::tool_loop::ChatMessage],
    per_turn: std::time::Duration,
    retry_budget: u32,
) -> Result<crate::provider::tool_loop::AssistantTurn, ProviderError> {
    retry_classified(retry_budget, || async {
        match tokio::time::timeout(per_turn, backend.turn_final(messages)).await {
            Ok(r) => r,
            Err(_) => Err(ProviderError::Timeout(
                format!("turn timeout after {}s", per_turn.as_secs()))),
        }
    }).await
}

/// `REDTEAM_HTTP_TIMEOUT_SECS`（整请求超时，缺省 300s）：思考型模型在大上下文 bundle 上单轮响应可 >120s（旧值），
/// 非网络不可达（ping 通）而是慢——留 env 供按网络粗调；非法 / 0 → 缺省。
pub(crate) fn http_timeout_env_secs() -> u64 {
    std::env::var("REDTEAM_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(300)
}

/// 整请求超时须罩住单轮超时（评审 M2：否则 reqwest 在 300s 先切断响应体，报 Unavailable 而非 Timeout，慢模型永远完不成一轮）：
/// env 值与 `per_turn + 60s` 取大。
pub(crate) fn http_timeout_for(per_turn_secs: u64) -> u64 {
    http_timeout_env_secs().max(per_turn_secs.saturating_add(60))
}

/// 带超时的 HTTP client（connect 10s / 整请求 `secs`）；构建失败回落默认 client。
pub(crate) fn http_client_with_timeout(secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(secs))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 缺省 client：整请求超时取 env / 300s（`backend_from_config` 会按模型 per_turn 重建）。
pub(crate) fn default_http_client() -> reqwest::Client {
    http_client_with_timeout(http_timeout_env_secs())
}

/// Anthropic Messages 端点 URL：规范化 base 的 `/v1`——无论 base 含不含 `/v1`，最终都拼成
/// `{host}/v1/messages`（dogfood：网关 base 配 `.../v1` 时旧逻辑拼出 `/v1/v1/messages` 404）。
pub(crate) fn anthropic_messages_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}
// 注：openai 侧**有意不做** anthropic 同款 /v1 规范化——openai 兼容生态无统一规范
// （DeepSeek 官方 base 裸路径正统、部分网关必须带 /v1、部分必须不带），自动补写对
// 「必须不带」的端点是破坏性的。base_url = 完整前缀，由配置负责（用户 2026-07-16 定）。

#[cfg(test)]
mod tests {
    use super::anthropic_messages_url;

    #[test]
    fn anthropic_messages_url_normalizes_v1() {
        // base 不含 /v1（默认 Anthropic）→ 拼 /v1/messages
        assert_eq!(anthropic_messages_url("https://api.anthropic.com"), "https://api.anthropic.com/v1/messages");
        // base 已含 /v1（OpenAI 风格网关，如 aigocode）→ 只拼 /messages，不双 /v1
        assert_eq!(anthropic_messages_url("https://team.aigocode.com/v1"), "https://team.aigocode.com/v1/messages");
        // trailing slash 容错
        assert_eq!(anthropic_messages_url("https://h/v1/"), "https://h/v1/messages");
        assert_eq!(anthropic_messages_url("https://h/"), "https://h/v1/messages");
    }

    use super::{classify_status, parse_envelope_json, parse_retry_after, ProviderError};
    use reqwest::StatusCode;
    use std::time::Duration;

    #[test]
    fn envelope_empty_body_is_retryable_unavailable() {
        // 200 + 纯空白 body（上游推理模型 keep-alive 后放弃、网关抖动）→ 可重试，
        // 绝不能是不可重试的 Parse（那会绕过 retry_classified，一次抖动烧掉整个 cell）。
        for body in ["", "   \n  ", "\n\n"] {
            let err = parse_envelope_json(body, "https://api.deepseek.com/x").unwrap_err();
            assert!(matches!(err, ProviderError::Unavailable(_)), "body {body:?} → {err:?}");
            assert!(err.is_retryable(), "envelope 解码失败必须可重试");
        }
    }

    #[test]
    fn envelope_html_body_carries_preview() {
        let err = parse_envelope_json("<html>502 Bad Gateway</html>", "u").unwrap_err();
        let ProviderError::Unavailable(m) = err else { panic!("应为 Unavailable") };
        assert!(m.contains("502 Bad Gateway"), "错误须携带 body 预览以便诊断: {m}");
    }

    #[test]
    fn envelope_valid_json_decodes() {
        let v = parse_envelope_json(r#"{"ok":true}"#, "u").unwrap();
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn envelope_leading_whitespace_before_json_is_ok() {
        // DeepSeek 非流式在长思考期间以空行 keep-alive；有 JSON 随后仍应正常解码。
        let v = parse_envelope_json("\n\n{\"ok\":1}", "u").unwrap();
        assert_eq!(v["ok"], 1);
    }

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
}
