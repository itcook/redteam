pub mod backends;
pub mod direct;
pub mod fake;
pub mod registry;
pub mod tool_loop;

use crate::domain::RawFinding;
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct LensRequest { pub model: String, pub prompt: String }

#[derive(Clone, Debug)]
pub struct LensCall { pub findings: Vec<RawFinding>, pub input_tokens: u64, pub output_tokens: u64 }

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

#[async_trait]
pub trait ReviewProvider: Send + Sync {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError>;
}

/// 带超时的共享 HTTP client（connect 10s / 整请求 120s）；构建失败回落默认 client。
pub(crate) fn default_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Anthropic Messages 端点 URL：规范化 base 的 `/v1`——无论 base 含不含 `/v1`，最终都拼成
/// `{host}/v1/messages`。backends（工具循环）与 direct（doc 单发）**共用**，避免重复导致不同步
/// （dogfood：网关 base 配 `.../v1` 时旧逻辑拼出 `/v1/v1/messages` 404；曾只修 backends 漏 direct）。
pub(crate) fn anthropic_messages_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}

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
}
