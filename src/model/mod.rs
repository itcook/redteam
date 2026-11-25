pub mod probe;

use crate::config::{Config, ProviderApi};

pub const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
pub const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";

/// 有效 base URL：显式 baseUrl 优先，否则按 api 取默认。
pub fn effective_base(base_url: Option<&str>, kind: ProviderApi) -> String {
    if let Some(b) = base_url {
        return b.to_string();
    }
    match kind {
        ProviderApi::AnthropicMessages => ANTHROPIC_DEFAULT_BASE.to_string(),
        _ => OPENAI_DEFAULT_BASE.to_string(),
    }
}

/// 端点探测（可注入）：`list_models` 带 key 查 `{base}/models`——
/// 成功(2xx)即认证通过 + 端点正确，返回模型 id 列表；失败(401/404/连不上)→Err。
/// 认证头随 `kind` 分派（Anthropic 用 `x-api-key`+`anthropic-version`，其余 bearer；
/// 见 `probe::auth_headers`），故调用方须传入 provider 的 `ProviderApi`。
#[async_trait::async_trait]
pub trait Prober: Send + Sync {
    async fn list_models(&self, base_url: &str, key: Option<&str>, kind: ProviderApi) -> anyhow::Result<Vec<String>>;
}

#[derive(Clone, Debug, PartialEq)]
pub struct Availability {
    pub provider: String,
    pub key_present: bool,
    /// 带 key 探测 `{base}/models` 成功（2xx）：认证通过 + 端点正确。
    pub reachable: bool,
    /// 配置的 model 是否在 /models 返回列表中：
    /// Some(true)=在；Some(false)=不在（拼错或下线，或 /models 不全）；None=列表不可得（探测失败或返回空）。
    pub model_listed: Option<bool>,
}
impl Availability {
    /// 可用 = key 就位 + 带 key 端点认证通过。`model_listed` 仅软提示、不参与判定
    /// （部分网关 /models 不返回全部模型，缺列不代表不可用）。
    pub fn available(&self) -> bool {
        self.key_present && self.reachable
    }
    pub fn status_str(&self) -> &'static str {
        if !self.key_present {
            "✗ missing key"
        } else if !self.reachable {
            "✗ unreachable / auth failed"
        } else if self.model_listed == Some(false) {
            "⚠ reachable, model not in /models"
        } else {
            "✓ available"
        }
    }
}

/// 对所有已配置 provider 做可用性判定：带 key 探测 `{base}/models`（认证 + 端点），
/// 并查配置的 model 是否在返回列表中。取代仅 GET base 的弱探测（会对 key/端点不匹配假阳性）。
pub async fn check_all(cfg: &Config, prober: &dyn Prober) -> Vec<Availability> {
    let mut out = Vec::new();
    // 按活跃集迭代（scanner → reviewer → refuter → verifier；探测用模型有效 kind：模型层 > provider 层）；
    // 同一模型兼多角色各探一次（接受重复）。
    for em in cfg.active_roles().all() {
        let name = format!("{}:{}", em.role.as_str(), em.name);
        let key = cfg.resolve_key(&em.provider, &em.id);
        let key_present = key.is_some();
        let (reachable, model_listed) =
            match prober.list_models(&effective_base(em.base_url.as_deref(), em.api), key.as_deref(), em.api).await {
                Ok(ids) => {
                    let listed = if ids.is_empty() {
                        None
                    } else {
                        Some(ids.iter().any(|m| m == &em.id))
                    };
                    (true, listed)
                }
                Err(_) => (false, None),
            };
        out.push(Availability { provider: name.clone(), key_present, reachable, model_listed });
    }
    out
}

/// 退出码：至少一个 scanner 且全部可用→0；否则→1（供脚本/循环 SKILL 评审前预检；只配 reviewer / refuter / verifier 时 review 会因
/// 无 scanner 失败，预检同步报 1，评审 S9）。
pub fn check_exit_code(avs: &[Availability]) -> i32 {
    let has_scanner = avs.iter().any(|a| a.provider.starts_with("scanner:"));
    if has_scanner && avs.iter().all(|a| a.available()) {
        0
    } else {
        1
    }
}

/// 推断 provider api（`model setup` 新建 provider 用）：显式 > 有 base_url(openai-completions) >
/// 按名推断 > 兜底 openai-completions。v2 无别名；显式值由 `validate_setup` 先把关。
pub fn derive_api(provider: &str, explicit_api: Option<&str>, base_url: Option<&str>) -> ProviderApi {
    if let Some(k) = explicit_api.and_then(ProviderApi::parse) {
        return k;
    }
    if base_url.is_some() {
        return ProviderApi::OpenaiCompletions;
    }
    let p = provider.to_lowercase();
    if p.contains("claude") || p.contains("anthropic") {
        ProviderApi::AnthropicMessages
    } else {
        ProviderApi::OpenaiCompletions
    }
}

/// 推断默认 api_key_env：始终按 provider 名生成唯一 env（`<PROVIDER_UPPER>_API_KEY`，
/// 非字母数字一律转 `_`）。避免同 kind 多 provider（如两个 openai 兼容端点）共用同一
/// env 名、env 模式下无法区分各自 key。provider 名恰为 `openai`/`anthropic` 时自然得
/// 标准名 `OPENAI_API_KEY`/`ANTHROPIC_API_KEY`。
pub fn derive_api_key_env(provider: &str) -> String {
    let norm: String = provider
        .to_uppercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{norm}_API_KEY")
}

/// `model setup` 写入前守卫：api 必须是 v2 三值之一（无别名），base_url 若提供必须是
/// http(s) URI 且有 host。任一不合法返回 Err（调用方提示并拒绝写入）。
pub fn validate_setup(api: &str, base_url: Option<&str>) -> Result<(), String> {
    if ProviderApi::parse(api).is_none() {
        return Err(format!(
            "非法 provider api '{api}'。v2 只接受：openai-completions / anthropic-messages / openai-responses（最后一个仅预留）"
        ));
    }
    if let Some(b) = base_url {
        match reqwest::Url::parse(b) {
            Ok(u) if matches!(u.scheme(), "http" | "https") && u.has_host() => {}
            _ => return Err(format!("非法 base_url '{b}'，需形如 https://host[/path]")),
        }
    }
    Ok(())
}

/// 渲染活跃集（每行 `[role] provider/id  (displayName)  [kind]  status`；无 displayName 不打印括号）+ 配置文件位置。
pub fn render_model_list(cfg: &Config, avs: &[Availability], config_path: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for em in cfg.active_roles().all() {
        let key = format!("{}:{}", em.role.as_str(), em.name);
        let status = avs
            .iter()
            .find(|a| a.provider == key)
            .map(|a| a.status_str())
            .unwrap_or("?");
        let display = em.display_name.as_deref().map(|d| format!("  ({d})")).unwrap_or_default();
        lines.push(format!("[{}] {}/{}{display}  [{}]  {status}", em.role.as_str(), em.provider, em.id, em.api.as_str()));
    }
    if lines.is_empty() {
        lines.push("(no model in any role pool — `redteam model setup --provider X --model <id> --role scanner …`)".into());
    }
    let mut out = lines.join("\n");
    out.push_str(&format!("\n\nconfig: {config_path}  (user-level only; REDTEAM_HOME overrides)"));
    out
}

/// 测试探测替身：`models=Some(ids)` 模拟 /models 成功返回；`None` 模拟认证/连接失败(Err)。
pub struct FakeProber {
    pub models: Option<Vec<String>>,
}
#[async_trait::async_trait]
impl Prober for FakeProber {
    async fn list_models(&self, _base_url: &str, _key: Option<&str>, _kind: ProviderApi) -> anyhow::Result<Vec<String>> {
        match &self.models {
            Some(m) => Ok(m.clone()),
            None => Err(anyhow::anyhow!("simulated auth/connection failure")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ENV_LOCK;

    fn cfg_with(providers: &str) -> Config {
        Config::from_json(providers).unwrap()
    }

    #[tokio::test]
    #[allow(clippy::await_holding_lock)]
    async fn check_all_authed_probe_truth_table() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("ALPHA_KEY", "k"); std::env::remove_var("BETA_KEY"); }
        let cfg = cfg_with(
            r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"alpha_p","id":"a"}]},"reviewer":{"models":[{"provider":"beta_p","id":"b"}]}}}},"providers":{"alpha_p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"a","displayName":"alpha","apiKeyEnv":"ALPHA_KEY"}]},"beta_p":{"api":"openai-completions","apiKeyEnv":"BETA_KEY","baseUrl":"https://x","models":[{"id":"b","displayName":"beta"}]}}}"#,
        );
        // 认证成功，/models 含 alpha 的 model "a"
        let ok = FakeProber { models: Some(vec!["a".into(), "z".into()]) };
        let avs = check_all(&cfg, &ok).await;
        let alpha = avs.iter().find(|a| a.provider == "scanner:alpha_p/a").unwrap();
        let beta = avs.iter().find(|a| a.provider == "reviewer:beta_p/b").unwrap();
        assert!(alpha.available() && alpha.model_listed == Some(true), "alpha: key+认证+model 在列表");
        assert!(!beta.available() && !beta.key_present, "beta: 缺 key");
        assert_eq!(check_exit_code(&avs), 1, "beta 缺 key→非 0");

        // 认证失败（401/连不上）→ 即便 key 在也不可用（修复 base 错配的假阳性）
        let cfg_a = cfg_with(
            r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"alpha_p","id":"a"}]}}}},"providers":{"alpha_p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"a","displayName":"alpha","apiKeyEnv":"ALPHA_KEY"}]}}}"#,
        );
        let fail = FakeProber { models: None };
        let avs2 = check_all(&cfg_a, &fail).await;
        assert!(!avs2[0].available() && avs2[0].key_present, "有 key 但认证失败→不可用");
        assert_eq!(avs2[0].status_str(), "✗ unreachable / auth failed");
        assert_eq!(check_exit_code(&avs2), 1);

        // 认证成功但配置 model 不在列表 → ⚠ 软提示，仍 available（不 block 退出码）
        let mism = FakeProber { models: Some(vec!["other".into()]) };
        let avs3 = check_all(&cfg_a, &mism).await;
        assert!(avs3[0].available() && avs3[0].model_listed == Some(false));
        assert_eq!(avs3[0].status_str(), "⚠ reachable, model not in /models");
        assert_eq!(check_exit_code(&avs3), 0, "model 软提示不影响退出码");

        unsafe { std::env::remove_var("ALPHA_KEY"); }
    }

    #[test]
    fn exit_code_empty_is_nonzero() {
        assert_eq!(check_exit_code(&[]), 1, "无 provider→非 0（预检无可用 model）");
        let rev_only = [Availability { provider: "reviewer:p/m".into(), key_present: true, reachable: true, model_listed: Some(true) }];
        assert_eq!(check_exit_code(&rev_only), 1, "无 scanner → 1（评审 S9）");
    }

    #[test]
    fn derive_api_rules() {
        assert_eq!(derive_api("anything", Some("anthropic-messages"), None), ProviderApi::AnthropicMessages); // 显式优先
        assert_eq!(derive_api("x", None, Some("http://h/v1")), ProviderApi::OpenaiCompletions); // base_url
        assert_eq!(derive_api("claude", None, None), ProviderApi::AnthropicMessages); // 名推断
        assert_eq!(derive_api("gpt", None, None), ProviderApi::OpenaiCompletions);
        assert_eq!(derive_api("mystery", None, None), ProviderApi::OpenaiCompletions); // 兜底
        // v2 无别名：非法显式值落回推断链（不 panic）
        assert_eq!(derive_api("mystery", Some("bogus"), None), ProviderApi::OpenaiCompletions);
        assert_eq!(derive_api("mystery", Some("claude"), None), ProviderApi::OpenaiCompletions, "别名不再接受");
    }

    #[test]
    fn derive_api_key_env_per_provider_unique() {
        assert_eq!(derive_api_key_env("DeepSeek"), "DEEPSEEK_API_KEY");
        assert_eq!(derive_api_key_env("Kimi"), "KIMI_API_KEY");
        assert_eq!(derive_api_key_env("openai"), "OPENAI_API_KEY");
        assert_eq!(derive_api_key_env("anthropic"), "ANTHROPIC_API_KEY");
        assert_eq!(derive_api_key_env("my-proxy.v2"), "MY_PROXY_V2_API_KEY");
        // 核心修复：不同 provider 名 → 不同 env（消除同 kind 共用 OPENAI_API_KEY 的冲突）
        assert_ne!(derive_api_key_env("DeepSeek"), derive_api_key_env("Kimi"));
    }

    #[test]
    fn validate_setup_guards_api_and_url() {
        // 合法 api（v2 三值，无别名）
        assert!(validate_setup("anthropic-messages", None).is_ok());
        assert!(validate_setup("openai-completions", Some("https://h/v1")).is_ok());
        assert!(validate_setup("openai-responses", None).is_ok(), "预留值可声明（config check 再拒）");
        // 旧词表 → 拒绝
        for old in ["anthropic", "claude", "openai", "openai-compatible", "google", "foobar"] {
            assert!(validate_setup(old, None).is_err(), "{old}");
        }
        // 非法 base_url → 拒绝
        assert!(validate_setup("openai-completions", Some("not-a-url")).is_err());
        assert!(validate_setup("openai-completions", Some("ftp://h")).is_err()); // 非 http(s)
        assert!(validate_setup("openai-completions", Some("https://")).is_err()); // 无 host
    }

    #[test]
    fn effective_base_defaults_by_api() {
        assert_eq!(effective_base(None, ProviderApi::AnthropicMessages), ANTHROPIC_DEFAULT_BASE);
        assert_eq!(effective_base(None, ProviderApi::OpenaiCompletions), OPENAI_DEFAULT_BASE);
        assert_eq!(effective_base(None, ProviderApi::OpenaiCompletions), OPENAI_DEFAULT_BASE);
        assert_eq!(effective_base(Some("http://x/v1"), ProviderApi::OpenaiCompletions), "http://x/v1");
        assert_eq!(effective_base(Some("https://gw/v1"), ProviderApi::AnthropicMessages), "https://gw/v1", "显式 baseUrl 优先于 api 默认");
    }

    #[test]
    fn render_model_list_one_line_per_provider_with_status() {
        let cfg = cfg_with(
            r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"alpha_p","id":"a"}]},"reviewer":{"models":[{"provider":"beta_p","id":"b"}]}}}},"providers":{"alpha_p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"a","displayName":"alpha"}]},"beta_p":{"api":"anthropic-messages","baseUrl":"https://x","models":[{"id":"b"}]}}}"#,
        );
        let avs = vec![
            Availability { provider: "scanner:alpha_p/a".into(), key_present: true, reachable: true, model_listed: Some(true) },
            Availability { provider: "reviewer:beta_p/b".into(), key_present: false, reachable: true, model_listed: None },
        ];
        let out = render_model_list(&cfg, &avs, "/u/config.json");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.iter().any(|l| l.starts_with("[scanner] alpha_p/a  (alpha)  [openai-completions]") && l.contains("available")), "{out}");
        assert!(lines.iter().any(|l| l.starts_with("[reviewer] beta_p/b  [anthropic-messages]") && l.contains("missing key")), "无 name 不重复打印 provider/id：{out}");
        assert!(out.contains("/u/config.json"), "config 位置打印");
    }
}
