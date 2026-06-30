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

/// 推断 provider kind：显式 > 有 base_url(openai-compatible) > 按名推断 > 兜底 openai-compatible。
pub fn derive_kind(provider: &str, explicit_kind: Option<&str>, base_url: Option<&str>) -> String {
    if let Some(k) = explicit_kind {
        return k.to_string();
    }
    if base_url.is_some() {
        return "openai-compatible".to_string();
    }
    let p = provider.to_lowercase();
    if p.contains("claude") || p.contains("anthropic") {
        "anthropic".to_string()
    } else if p.contains("gpt") || p.contains("openai") {
        "openai".to_string()
    } else {
        "openai-compatible".to_string()
    }
}

/// 推断默认 api_key_env：anthropic→ANTHROPIC_API_KEY / openai→OPENAI_API_KEY / 否则 <PROVIDER_UPPER>_API_KEY。
pub fn derive_api_key_env(provider: &str, kind: &str) -> String {
    match kind {
        "anthropic" => "ANTHROPIC_API_KEY".to_string(),
        "openai" => "OPENAI_API_KEY".to_string(),
        _ => format!("{}_API_KEY", provider.to_uppercase().replace('-', "_")),
    }
}

/// 渲染已配置 provider 列表（每行 name/kind/model/status）+ 配置文件位置。
pub fn render_model_list(cfg: &Config, avs: &[Availability], config_paths: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (name, pc) in &cfg.providers {
        let status = avs
            .iter()
            .find(|a| &a.provider == name)
            .map(|a| a.status_str())
            .unwrap_or("?");
        let kind = format!("{:?}", pc.kind).to_lowercase();
        lines.push(format!("{name}  [{kind}]  {}  {status}", pc.model));
    }
    lines.sort();
    let mut out = lines.join("\n");
    out.push_str("\n\nconfig files (low→high priority):");
    for p in config_paths {
        out.push_str(&format!("\n  {p}"));
    }
    out
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
    fn derive_kind_rules() {
        assert_eq!(derive_kind("anything", Some("anthropic"), None), "anthropic"); // 显式优先
        assert_eq!(derive_kind("x", None, Some("http://h/v1")), "openai-compatible"); // base_url
        assert_eq!(derive_kind("claude", None, None), "anthropic"); // 名推断
        assert_eq!(derive_kind("gpt", None, None), "openai");
        assert_eq!(derive_kind("mystery", None, None), "openai-compatible"); // 兜底
    }

    #[test]
    fn derive_api_key_env_rules() {
        assert_eq!(derive_api_key_env("claude", "anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(derive_api_key_env("gpt", "openai"), "OPENAI_API_KEY");
        assert_eq!(derive_api_key_env("local", "openai-compatible"), "LOCAL_API_KEY");
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

    #[test]
    fn render_model_list_one_line_per_provider_with_status() {
        let cfg = cfg_with(
            "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
             [providers.beta]\nkind=\"anthropic\"\nmodel=\"b\"\npriority=5\n",
        );
        let avs = vec![
            Availability { provider: "alpha".into(), key_present: true, reachable: true },
            Availability { provider: "beta".into(), key_present: false, reachable: true },
        ];
        let out = render_model_list(&cfg, &avs, &["/u/config.toml".into()]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.iter().any(|l| l.contains("alpha") && l.contains("a") && l.contains("available")));
        assert!(lines.iter().any(|l| l.contains("beta") && l.contains("missing key")));
        assert!(out.contains("/u/config.toml"), "config 位置打印");
    }
}
