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

/// 端点探测（可注入）：`list_models` 带 key 查 `{base}/models`——
/// 成功(2xx)即认证通过 + 端点正确，返回模型 id 列表；失败(401/404/连不上)→Err。
/// 认证头随 `kind` 分派（Anthropic 用 `x-api-key`+`anthropic-version`，其余 bearer；
/// 见 `probe::auth_headers`），故调用方须传入 provider 的 `ProviderKind`。
#[async_trait::async_trait]
pub trait Prober: Send + Sync {
    async fn list_models(&self, base_url: &str, key: Option<&str>, kind: ProviderKind) -> anyhow::Result<Vec<String>>;
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
    // 按三角色条目迭代（探测用 merged：条目.kind > provider.kind）；同一模型在两个角色各探一次（接受重复）。
    for (role, entry) in cfg.all_entries() {
        let Some(em) = cfg.effective(role, entry) else { continue };
        let name = format!("{}:{}", role.as_str(), em.name);
        let key = cfg.resolve_key(entry);
        let key_present = key.is_some();
        let pc = crate::config::ProviderConfig {
            kind: em.kind,
            base_url: em.base_url.clone(),
            api_key_env: None,
            max_concurrency: None,
        };
        let (reachable, model_listed) =
            match prober.list_models(&effective_base(&pc), key.as_deref(), pc.kind).await {
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

/// 退出码：非空且全部可用→0；否则→1（供脚本/循环 SKILL 评审前预检）。
pub fn check_exit_code(avs: &[Availability]) -> i32 {
    if !avs.is_empty() && avs.iter().all(|a| a.available()) {
        0
    } else {
        1
    }
}

/// 推断 provider kind：显式 > 有 base_url(openai-compatible) > 按名推断 > 兜底 openai-compatible。
/// 显式 kind 若非法（`ProviderKind::parse` 失败）视同未提供、落回后续推断——CLI 层
/// （`model setup`）在调用本函数前已用 `validate_setup` 对显式输入把关，故此处到达
/// 「非法显式值」分支时调用方本已提前拒绝；本函数独立单测时对此保守兜底、不 panic。
pub fn derive_kind(provider: &str, explicit_kind: Option<&str>, base_url: Option<&str>) -> ProviderKind {
    if let Some(k) = explicit_kind.and_then(ProviderKind::parse) {
        return k;
    }
    if base_url.is_some() {
        return ProviderKind::OpenaiCompatible;
    }
    let p = provider.to_lowercase();
    if p.contains("claude") || p.contains("anthropic") {
        ProviderKind::Anthropic
    } else if p.contains("gpt") || p.contains("openai") {
        ProviderKind::Openai
    } else {
        ProviderKind::OpenaiCompatible
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

/// `model setup` 写入前守卫：kind 必须是合法 ProviderKind（含别名），base_url 若提供必须是
/// http(s) URI 且有 host。任一不合法返回 Err（调用方提示并拒绝写入），避免污染配置导致后续
/// 整份 config 解析失败（如曾经的 kind="claude" → check 无输出）。
pub fn validate_setup(kind: &str, base_url: Option<&str>) -> Result<(), String> {
    if ProviderKind::parse(kind).is_none() {
        // 别名文案与 `ProviderKind` 各变体的 `#[serde(alias = ...)]` 手动保持同步——
        // serde 的 alias 列表运行时不可反射，无法程序化生成；改一处务必顺手改另一处
        // （batch4 polish 评估：converge 成本高于价值，改留交叉引用注释）。
        return Err(format!(
            "非法 provider kind '{kind}'。合法值：anthropic(别名 claude) / openai(别名 gpt) / \
             openai-compatible(别名 openai_compatible/compatible) / google(别名 gemini)"
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

/// 渲染三角色条目列表（每行 role:name/kind/model_id/status/@provider）+ 配置文件位置。
pub fn render_model_list(cfg: &Config, avs: &[Availability], config_paths: &[String]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (role, entry) in cfg.all_entries() {
        let Some(em) = cfg.effective(role, entry) else { continue };
        let key = format!("{}:{}", role.as_str(), em.name);
        let status = avs
            .iter()
            .find(|a| a.provider == key)
            .map(|a| a.status_str())
            .unwrap_or("?");
        let kind = format!("{:?}", em.kind).to_lowercase();
        lines.push(format!("[{}] {}  [{kind}]  {}  {status}  @{}", role.as_str(), em.name, em.id, em.provider));
    }
    if lines.is_empty() {
        lines.push("(no [[scanners]] / [[reviewers]] / [[verifiers]] configured — `redteam model setup --role scanner …`)".into());
    }
    let mut out = lines.join("\n");
    out.push_str("\n\nconfig (user-level only; REDTEAM_HOME overrides):");
    for p in config_paths {
        out.push_str(&format!("\n  {p}"));
    }
    out
}

/// 测试探测替身：`models=Some(ids)` 模拟 /models 成功返回；`None` 模拟认证/连接失败(Err)。
pub struct FakeProber {
    pub models: Option<Vec<String>>,
}
#[async_trait::async_trait]
impl Prober for FakeProber {
    async fn list_models(&self, _base_url: &str, _key: Option<&str>, _kind: ProviderKind) -> anyhow::Result<Vec<String>> {
        match &self.models {
            Some(m) => Ok(m.clone()),
            None => Err(anyhow::anyhow!("simulated auth/connection failure")),
        }
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
    async fn check_all_authed_probe_truth_table() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("ALPHA_KEY", "k"); std::env::remove_var("BETA_KEY"); }
        let cfg = cfg_with(
            "[providers.alpha_p]\nkind=\"openai\"\n[[scanners]]\nname=\"alpha\"\nprovider=\"alpha_p\"\nmodel_id=\"a\"\napi_key_env=\"ALPHA_KEY\"\n\
             [providers.beta_p]\nkind=\"openai\"\napi_key_env=\"BETA_KEY\"\n\n[[reviewers]]\nname=\"beta\"\nprovider=\"beta_p\"\nmodel_id=\"b\"\n",
        );
        // 认证成功，/models 含 alpha 的 model "a"
        let ok = FakeProber { models: Some(vec!["a".into(), "z".into()]) };
        let avs = check_all(&cfg, &ok).await;
        let alpha = avs.iter().find(|a| a.provider == "scanner:alpha").unwrap();
        let beta = avs.iter().find(|a| a.provider == "reviewer:beta").unwrap();
        assert!(alpha.available() && alpha.model_listed == Some(true), "alpha: key+认证+model 在列表");
        assert!(!beta.available() && !beta.key_present, "beta: 缺 key");
        assert_eq!(check_exit_code(&avs), 1, "beta 缺 key→非 0");

        // 认证失败（401/连不上）→ 即便 key 在也不可用（修复 base 错配的假阳性）
        let cfg_a = cfg_with(
            "[providers.alpha_p]\nkind=\"openai\"\n[[scanners]]\nname=\"alpha\"\nprovider=\"alpha_p\"\nmodel_id=\"a\"\napi_key_env=\"ALPHA_KEY\"\n",
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
    }

    #[test]
    fn derive_kind_rules() {
        assert_eq!(derive_kind("anything", Some("anthropic"), None), ProviderKind::Anthropic); // 显式优先
        assert_eq!(derive_kind("x", None, Some("http://h/v1")), ProviderKind::OpenaiCompatible); // base_url
        assert_eq!(derive_kind("claude", None, None), ProviderKind::Anthropic); // 名推断
        assert_eq!(derive_kind("gpt", None, None), ProviderKind::Openai);
        assert_eq!(derive_kind("mystery", None, None), ProviderKind::OpenaiCompatible); // 兜底
        // 显式别名按 ProviderKind::parse 规范化；非法显式值落回推断链（不 panic）
        assert_eq!(derive_kind("anything", Some("claude"), None), ProviderKind::Anthropic);
        assert_eq!(derive_kind("mystery", Some("bogus"), None), ProviderKind::OpenaiCompatible);
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
    fn validate_setup_guards_kind_and_url() {
        // 合法 kind（含别名）
        assert!(validate_setup("anthropic", None).is_ok());
        assert!(validate_setup("claude", None).is_ok());
        assert!(validate_setup("openai-compatible", Some("https://h/v1")).is_ok());
        assert!(validate_setup("claude", Some("https://team.aigocode.com/v1")).is_ok());
        // 非法 kind → 拒绝
        assert!(validate_setup("foobar", None).is_err());
        // 非法 base_url → 拒绝
        assert!(validate_setup("openai", Some("not-a-url")).is_err());
        assert!(validate_setup("openai", Some("ftp://h")).is_err()); // 非 http(s)
        assert!(validate_setup("openai", Some("https://")).is_err()); // 无 host
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
            "[providers.alpha_p]\nkind=\"openai\"\n\n[[scanners]]\nname=\"alpha\"\nprovider=\"alpha_p\"\nmodel_id=\"a\"\n\
             [providers.beta_p]\nkind=\"anthropic\"\n\n[[reviewers]]\nname=\"beta\"\nprovider=\"beta_p\"\nmodel_id=\"b\"\n",
        );
        let avs = vec![
            Availability { provider: "scanner:alpha".into(), key_present: true, reachable: true, model_listed: Some(true) },
            Availability { provider: "reviewer:beta".into(), key_present: false, reachable: true, model_listed: None },
        ];
        let out = render_model_list(&cfg, &avs, &["/u/config.toml".into()]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.iter().any(|l| l.contains("alpha") && l.contains("a") && l.contains("available")));
        assert!(lines.iter().any(|l| l.contains("beta") && l.contains("missing key")));
        assert!(out.contains("/u/config.toml"), "config 位置打印");
    }
}
