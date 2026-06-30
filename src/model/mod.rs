pub mod probe;

use crate::config::{Config, ProviderConfig, ProviderKind};

pub const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
pub const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";

/// provider 有效 base URL：显式 base_url 优先，否则按 kind 取默认。
pub fn effective_base(pc: &ProviderConfig) -> String {
    if let Some(b) = &pc.base_url {
        return b.clone();
    }
    match pc.kind {
        ProviderKind::Anthropic => ANTHROPIC_DEFAULT_BASE.to_string(),
        _ => OPENAI_DEFAULT_BASE.to_string(),
    }
}

/// 端点探测（可注入）：reachable=端点可达；list_models=查 /v1/models。
#[async_trait::async_trait]
pub trait Prober: Send + Sync {
    async fn reachable(&self, base_url: &str) -> bool;
    async fn list_models(&self, base_url: &str, key: Option<&str>) -> anyhow::Result<Vec<String>>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Availability {
    pub provider: String,
    pub key_present: bool,
    pub reachable: bool,
}
impl Availability {
    pub fn available(&self) -> bool {
        self.key_present && self.reachable
    }
    pub fn status_str(&self) -> &'static str {
        if self.available() {
            "✓ available"
        } else if !self.key_present {
            "✗ missing key"
        } else {
            "✗ unreachable"
        }
    }
}

/// 对所有已配置 provider 做可用性判定（key 就位 × 端点可达）。
pub async fn check_all(cfg: &Config, prober: &dyn Prober) -> Vec<Availability> {
    let mut out = Vec::new();
    for (name, pc) in &cfg.providers {
        let key_present = cfg.resolve_key(name).is_some();
        let reachable = prober.reachable(&effective_base(pc)).await;
        out.push(Availability { provider: name.clone(), key_present, reachable });
    }
    out
}

/// 退出码：非空且全部可用→0；否则→1（供脚本/循环 SKILL 评审前预检）。
pub fn check_exit_code(avs: &[Availability]) -> i32 {
    if !avs.is_empty() && avs.iter().all(|a| a.available()) {
        0
    } else {
        1
    }
}

/// 测试探测替身（pub 供后续 task 复用）。
pub struct FakeProber {
    pub reachable: bool,
    pub models: Vec<String>,
}
#[async_trait::async_trait]
impl Prober for FakeProber {
    async fn reachable(&self, _base_url: &str) -> bool {
        self.reachable
    }
    async fn list_models(&self, _base_url: &str, _key: Option<&str>) -> anyhow::Result<Vec<String>> {
        Ok(self.models.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn cfg_with(providers: &str) -> Config {
        Config::from_layers(&[providers]).unwrap()
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn check_all_truth_table_and_exit_code() {
        let _g = ENV_LOCK.lock().unwrap();
        // alpha 有 env key、beta 无 key
        unsafe { std::env::set_var("ALPHA_KEY", "k"); std::env::remove_var("BETA_KEY"); }
        let cfg = cfg_with(
            "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\napi_key_env=\"ALPHA_KEY\"\npriority=10\n\
             [providers.beta]\nkind=\"openai\"\nmodel=\"b\"\napi_key_env=\"BETA_KEY\"\npriority=5\n",
        );
        // 端点可达
        let reach = FakeProber { reachable: true, models: vec![] };
        let avs = check_all(&cfg, &reach).await;
        let alpha = avs.iter().find(|a| a.provider == "alpha").unwrap();
        let beta = avs.iter().find(|a| a.provider == "beta").unwrap();
        assert!(alpha.available(), "alpha: key+reachable");
        assert!(!beta.available() && !beta.key_present, "beta: missing key");
        assert_eq!(check_exit_code(&avs), 1, "有不可用→非 0");

        // 端点不可达 → 即便有 key 也不可用
        let unreach = FakeProber { reachable: false, models: vec![] };
        let avs2 = check_all(&cfg, &unreach).await;
        assert!(avs2.iter().all(|a| !a.available()));
        assert_eq!(check_exit_code(&avs2), 1);
        unsafe { std::env::remove_var("ALPHA_KEY"); }
    }

    #[test]
    fn exit_code_empty_is_nonzero() {
        assert_eq!(check_exit_code(&[]), 1, "无 provider→非 0（预检无可用 model）");
    }

    #[test]
    fn effective_base_defaults_by_kind() {
        let pc: ProviderConfig = toml::from_str("kind=\"anthropic\"\nmodel=\"m\"").unwrap();
        assert_eq!(effective_base(&pc), ANTHROPIC_DEFAULT_BASE);
        let pc2: ProviderConfig = toml::from_str("kind=\"openai\"\nmodel=\"m\"").unwrap();
        assert_eq!(effective_base(&pc2), OPENAI_DEFAULT_BASE);
        let pc3: ProviderConfig = toml::from_str("kind=\"openai-compatible\"\nmodel=\"m\"\nbase_url=\"http://x/v1\"").unwrap();
        assert_eq!(effective_base(&pc3), "http://x/v1");
    }
}
