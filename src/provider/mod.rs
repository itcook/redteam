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
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    #[error("structured-output parse failed: {0}")]
    Parse(String),
}

#[async_trait]
pub trait ReviewProvider: Send + Sync {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError>;
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
}
