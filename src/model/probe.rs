use crate::config::ProviderKind;
use crate::model::Prober;
use std::time::Duration;

/// 真 HTTP 探测（reqwest，rustls）。薄缝——不单测（无真实端点）。
pub struct HttpProber {
    client: reqwest::Client,
}
impl Default for HttpProber {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_default();
        HttpProber { client }
    }
}
impl HttpProber {
    pub fn new() -> Self {
        Self::default()
    }
}

/// 按 kind 选认证头（纯函数，可测）：Anthropic 用 `x-api-key`+`anthropic-version`
/// （其 `/v1/models` 端点不认 bearer），其余（openai / openai-compatible / google 兜底）
/// 沿用 `authorization: Bearer <key>`。`key=None` 时不产生任何头（未配置 key 的探测——
/// 上游多半直接 401，这里如实不带头、把认证失败留给响应状态分类）。
pub(crate) fn auth_headers(kind: ProviderKind, key: Option<&str>) -> Vec<(&'static str, String)> {
    let Some(k) = key else { return vec![] };
    match kind {
        ProviderKind::Anthropic => vec![
            ("x-api-key", k.to_string()),
            ("anthropic-version", "2023-06-01".to_string()),
        ],
        _ => vec![("authorization", format!("Bearer {k}"))],
    }
}

/// `{base}/models` 端点路径拼接：Anthropic 需规范化到 `{base}/v1/models`（复用
/// `anthropic_messages_url` 的同款 /v1 规范手法——base 含不含 `/v1` 都能拼对，不重复
/// `/v1/v1`）；其余 provider 沿用现有 `{base}/models`（openai 兼容生态无统一规范，
/// base_url 由配置负责，见 `provider::mod` 对应注释）。
fn models_url(base_url: &str, kind: ProviderKind) -> String {
    let base = base_url.trim_end_matches('/');
    match kind {
        ProviderKind::Anthropic => {
            if base.ends_with("/v1") {
                format!("{base}/models")
            } else {
                format!("{base}/v1/models")
            }
        }
        _ => format!("{base}/models"),
    }
}

#[async_trait::async_trait]
impl Prober for HttpProber {
    async fn list_models(&self, base_url: &str, key: Option<&str>, kind: ProviderKind) -> anyhow::Result<Vec<String>> {
        let url = models_url(base_url, kind);
        let mut req = self.client.get(&url);
        for (name, value) in auth_headers(kind, key) {
            req = req.header(name, value);
        }
        let resp = req.send().await?.error_for_status()?;
        let v: serde_json::Value = resp.json().await?;
        let ids = v
            .get("data")
            .and_then(|d| d.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_headers_anthropic_uses_x_api_key_and_version() {
        let h = auth_headers(ProviderKind::Anthropic, Some("sk-ant-x"));
        assert_eq!(h, vec![
            ("x-api-key", "sk-ant-x".to_string()),
            ("anthropic-version", "2023-06-01".to_string()),
        ]);
    }

    #[test]
    fn auth_headers_non_anthropic_uses_bearer() {
        for kind in [ProviderKind::Openai, ProviderKind::OpenaiCompatible, ProviderKind::Google] {
            let h = auth_headers(kind, Some("sk-x"));
            assert_eq!(h, vec![("authorization", "Bearer sk-x".to_string())], "kind={kind:?}");
        }
    }

    #[test]
    fn auth_headers_no_key_is_empty() {
        assert_eq!(auth_headers(ProviderKind::Anthropic, None), vec![]);
        assert_eq!(auth_headers(ProviderKind::Openai, None), vec![]);
    }

    #[test]
    fn models_url_normalizes_v1_for_anthropic_only() {
        assert_eq!(models_url("https://api.anthropic.com", ProviderKind::Anthropic), "https://api.anthropic.com/v1/models");
        assert_eq!(models_url("https://h/v1", ProviderKind::Anthropic), "https://h/v1/models");
        assert_eq!(models_url("https://h/v1/", ProviderKind::Anthropic), "https://h/v1/models");
        // 非 anthropic 不做 /v1 规范化（沿用现有 {base}/models）
        assert_eq!(models_url("https://api.openai.com/v1", ProviderKind::Openai), "https://api.openai.com/v1/models");
        assert_eq!(models_url("http://localhost:11434/v1", ProviderKind::OpenaiCompatible), "http://localhost:11434/v1/models");
    }
}
