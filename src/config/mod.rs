pub mod secrets;

use crate::domain::Severity;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[serde(alias = "claude")]
    Anthropic,
    #[serde(alias = "gpt")]
    Openai,
    #[serde(alias = "openai_compatible", alias = "compatible")]
    OpenaiCompatible,
    #[serde(alias = "gemini")]
    Google,
}

impl ProviderKind {
    /// 从字符串解析（复用 serde 的 rename + 别名：claude/gpt/gemini 等）；非法返回 None。
    /// 供 `model setup` 写入前守卫——拒绝写出后续无法反序列化的坏配置。
    pub fn parse(s: &str) -> Option<ProviderKind> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    pub model: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LensSets {
    #[serde(default = "default_code_lenses")]
    pub code: Vec<String>,
    #[serde(default = "default_doc_lenses")]
    pub doc: Vec<String>,
}
fn default_code_lenses() -> Vec<String> {
    ["L1", "L2", "L3", "L4", "L0", "L5"].iter().map(|s| s.to_string()).collect()
}
fn default_doc_lenses() -> Vec<String> {
    ["D1", "D2", "D3", "D4", "D5"].iter().map(|s| s.to_string()).collect()
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReviewConfig {
    #[serde(default)]
    pub default_models: Vec<String>,
    #[serde(default = "default_block")]
    pub block_threshold: Severity,
    #[serde(default = "default_floor")]
    pub conf_floor: f32,
    #[serde(default = "default_true")]
    pub correlate: bool,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    #[serde(default = "default_ttl")]
    pub state_ttl: String,
    #[serde(default)]
    pub lenses: LensSets,
}
fn default_block() -> Severity { Severity::Medium }
fn default_floor() -> f32 { 0.5 }
fn default_true() -> bool { true }
fn default_concurrency() -> usize { 8 }
fn default_ttl() -> String { "24h".into() }
impl Default for LensSets {
    fn default() -> Self { LensSets { code: default_code_lenses(), doc: default_doc_lenses() } }
}
impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig { default_models: vec![], block_threshold: default_block(),
            conf_floor: default_floor(), correlate: default_true(),
            max_concurrency: default_concurrency(), state_ttl: default_ttl(),
            lenses: LensSets::default() }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub review: ReviewConfig,
}

impl Config {
    /// 真实加载：内置默认（空）→ 各分层文件文本依次合并。
    pub fn load() -> anyhow::Result<Config> {
        let texts: Vec<String> = config_paths()
            .into_iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .collect();
        let refs: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
        Config::from_layers(&refs)
    }

    /// 把多层 TOML 文本按 toml::Value 深合并（右侧覆盖左侧）后反序列化。
    pub fn from_layers(layers: &[&str]) -> anyhow::Result<Config> {
        let mut merged = toml::Value::Table(Default::default());
        for layer in layers {
            let v: toml::Value = toml::from_str(layer)?;
            merge(&mut merged, v);
        }
        Ok(merged.try_into()?)
    }

    /// key 解析：env(api_key_env) 优先，回落 secrets.toml[provider]。
    pub fn resolve_key(&self, provider: &str) -> Option<String> {
        if let Some(pc) = self.providers.get(provider) {
            if let Some(var) = &pc.api_key_env {
                if let Ok(v) = std::env::var(var) {
                    return Some(v);
                }
            }
        }
        secrets::load_secrets().get(provider).cloned()
    }
}

fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(b), toml::Value::Table(o)) => {
            for (k, v) in o {
                merge(b.entry(k).or_insert(toml::Value::Table(Default::default())), v);
            }
        }
        (b, o) => *b = o,
    }
}

/// 分层路径（左→右优先级递增）：用户 → 项目。内置默认由 serde default 提供。
pub fn config_paths() -> Vec<PathBuf> {
    let mut v = vec![secrets::dirs_config().join("redteam").join("config.toml")];
    v.push(PathBuf::from(".redteam").join("config.toml"));
    v
}

/// 把 [providers.<name>] 写进用户配置（config_paths()[0]），toml 深合并保留其他内容。
/// 只写 kind/base_url/model/priority/api_key_env——**绝不写明文 key**。
pub fn write_provider(
    name: &str,
    kind: &str,
    base_url: Option<&str>,
    model: &str,
    priority: i32,
    api_key_env: Option<&str>,
) -> anyhow::Result<()> {
    let path = config_paths()
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no config path"))?;
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut root: toml::Value =
        toml::from_str(&existing).unwrap_or_else(|_| toml::Value::Table(Default::default()));
    let root_t = root
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("config root is not a table"))?;
    let providers = root_t
        .entry("providers".to_string())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    let providers_t = providers
        .as_table_mut()
        .ok_or_else(|| anyhow::anyhow!("[providers] is not a table"))?;

    let mut t = toml::value::Table::new();
    t.insert("kind".into(), toml::Value::String(kind.to_string()));
    t.insert("model".into(), toml::Value::String(model.to_string()));
    if let Some(b) = base_url {
        t.insert("base_url".into(), toml::Value::String(b.to_string()));
    }
    if let Some(e) = api_key_env {
        t.insert("api_key_env".into(), toml::Value::String(e.to_string()));
    }
    t.insert("priority".into(), toml::Value::Integer(priority as i64));
    providers_t.insert(name.to_string(), toml::Value::Table(t));

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, toml::to_string_pretty(&root)?)?;
    Ok(())
}

/// 测试专用：跨 config 子模块串行化 XDG_CONFIG_HOME 环境变量写入，防并行测试污染。
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_provider_persists_block_without_key() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()); }
        // 先放一段已有 [review]，验证保留
        let cfg_path = config_paths()[0].clone();
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, "[review]\nblock_threshold=\"high\"\n").unwrap();

        write_provider("local", "openai-compatible", Some("http://localhost:11434/v1"), "qwen", 10, Some("LOCAL_KEY")).unwrap();

        let cfg = Config::load().unwrap();
        let p = cfg.providers.get("local").expect("provider written");
        assert_eq!(p.model, "qwen");
        assert_eq!(p.priority, 10);
        assert_eq!(p.base_url.as_deref(), Some("http://localhost:11434/v1"));
        assert_eq!(p.api_key_env.as_deref(), Some("LOCAL_KEY"));
        // 既有 [review] 保留
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::High);
        // 配置文本不含明文 key（只有 api_key_env 引用）
        let raw = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(!raw.contains("sk-"), "no plaintext key in config");
        unsafe { std::env::remove_var("XDG_CONFIG_HOME"); }
    }

    #[test]
    fn project_overrides_user() {
        let user = r#"[review]
default_models=["claude"]
block_threshold="low"
"#;
        let project = r#"[review]
block_threshold="high"
"#;
        let cfg = Config::from_layers(&[user, project]).unwrap();
        // 项目层覆盖用户层
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::High);
        // 未覆盖字段保留
        assert_eq!(cfg.review.default_models, vec!["claude".to_string()]);
    }
    #[test]
    fn defaults_apply_when_absent() {
        let cfg = Config::from_layers(&[]).unwrap();
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::Medium);
        assert_eq!(cfg.review.conf_floor, 0.5);
        assert!(cfg.review.correlate);
    }
    #[test]
    fn key_resolves_env_over_secrets() {
        std::env::set_var("REDTEAM_TEST_KEY", "from-env");
        let toml = r#"[providers.x]
kind="openai"
model="m"
api_key_env="REDTEAM_TEST_KEY"
priority=10
"#;
        let cfg = Config::from_layers(&[toml]).unwrap();
        assert_eq!(cfg.resolve_key("x").as_deref(), Some("from-env"));
        std::env::remove_var("REDTEAM_TEST_KEY");
    }
}
