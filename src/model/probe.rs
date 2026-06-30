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

#[async_trait::async_trait]
impl Prober for HttpProber {
    async fn list_models(&self, base_url: &str, key: Option<&str>) -> anyhow::Result<Vec<String>> {
        let url = format!("{}/models", base_url.trim_end_matches('/'));
        let mut req = self.client.get(&url);
        if let Some(k) = key {
            req = req.bearer_auth(k);
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
