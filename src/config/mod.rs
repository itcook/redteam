pub mod secrets;

use crate::domain::Severity;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 各变体的 `#[serde(alias = ...)]` 是别名的唯一真相源；`model::validate_setup` 的报错文案
/// 手工抄了一份人读版本（serde alias 列表运行时不可反射，无法程序化生成）——改这里的别名
/// 记得同步改那边的文案（batch4 polish 交叉引用，2026-07-27）。
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

    /// 规范 kebab-case 名（与 serde `rename_all` 一致，恒返别名的标准形）；
    /// 供需要把枚举写回文本的场合用（TOML 落盘、日志），避免手写字符串表与枚举定义各说各话。
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Openai => "openai",
            ProviderKind::OpenaiCompatible => "openai-compatible",
            ProviderKind::Google => "google",
        }
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
    /// 模型 context window（token）。缺省按保守 128K 估——预算函数用它护 token 硬墙
    /// （design 2026-07-13 §2.2）；确知更大窗的模型在此声明以解锁完整字节帽。
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
}

/// 评审工具档位（design 2026-07-13 §2.4，E8 裁决 A）：off=单发单轮无工具；two-stage
/// （默认）=单发 + needs 声明触发的二阶段补查（批 2/3/4 已交付，非"后续批次"）；
/// on=对话式工具（诊断/对照档）。注：二阶段补查逻辑目前不读此 flag、对 off/two-stage
/// 一视同仁（见 `cli::mod` 里 `mk` 闭包无条件传入），两档暂管线等价，仅 on 真正分叉。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolsMode { Off, TwoStage, On }

impl ToolsMode {
    /// CLI flag 字符串 → 档位（clap value_parser 已限值域，此处仍返回 Option 防漂移）。
    pub fn from_flag(s: &str) -> Option<ToolsMode> {
        match s {
            "off" => Some(ToolsMode::Off),
            "two-stage" => Some(ToolsMode::TwoStage),
            "on" => Some(ToolsMode::On),
            _ => None,
        }
    }
}
fn default_tools() -> ToolsMode { ToolsMode::TwoStage }

#[derive(Clone, Debug, Deserialize)]
pub struct ReviewConfig {
    #[serde(default)]
    pub default_models: Vec<String>,
    #[serde(default = "default_block")]
    pub block_threshold: Severity,
    #[serde(default = "default_floor")]
    pub conf_floor: f32,
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    /// 工具档位：off | two-stage（默认）| on。
    #[serde(default = "default_tools")]
    pub tools: ToolsMode,
}
fn default_block() -> Severity { Severity::Medium }
fn default_floor() -> f32 { 0.5 }
fn default_concurrency() -> usize { 8 }
impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig { default_models: vec![], block_threshold: default_block(),
            conf_floor: default_floor(),
            max_concurrency: default_concurrency(),
            tools: default_tools() }
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
    /// 真实加载：内置默认（空）→ 各分层文件文本依次合并。项目层（`.redteam/config.toml`，
    /// config_paths() 末层）的 `[providers.*]` 被安全策略丢弃（威胁模型 B）；warning 走
    /// stderr——与 load_config_warned() 既有的「解析失败也走 stderr」风格一致，不新开通道。
    pub fn load() -> anyhow::Result<Config> {
        let paths = config_paths();
        let last_idx = paths.len().saturating_sub(1);
        let layers: Vec<(String, bool)> = paths
            .into_iter()
            .enumerate()
            .filter_map(|(i, p)| std::fs::read_to_string(p).ok().map(|t| (t, i == last_idx)))
            .collect();
        let refs: Vec<(&str, bool)> = layers.iter().map(|(s, is_project)| (s.as_str(), *is_project)).collect();
        let (cfg, warnings) = Config::from_layers_tagged(&refs)?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        Ok(cfg)
    }

    /// 把多层 TOML 文本按 toml::Value 深合并（右侧覆盖左侧）后反序列化；`is_project` 标记
    /// 该层是否为项目层（`.redteam/config.toml`，随不可信仓一起分发——威胁模型 B）。项目层
    /// 的 `[providers.*]` 在合并前剥离：不可信仓不能靠改 base_url 把评审用的 key 转发到攻击者
    /// 端点；`[review]` 等其他键正常保留。返回值第二项是安全丢弃产生的 warning（供调用方披露）。
    pub fn from_layers_tagged(layers: &[(&str, bool)]) -> anyhow::Result<(Config, Vec<String>)> {
        let mut merged = toml::Value::Table(Default::default());
        let mut warnings = Vec::new();
        for (layer, is_project) in layers {
            let mut v: toml::Value = toml::from_str(layer)?;
            if *is_project {
                if let Some(table) = v.as_table_mut() {
                    if let Some(removed) = table.remove("providers") {
                        let non_empty = matches!(&removed, toml::Value::Table(t) if !t.is_empty());
                        if non_empty {
                            warnings.push(
                                "项目层 .redteam/config.toml 的 [providers.*] 被安全策略丢弃（威胁模型 B：防不可信仓覆写端点偷 key）".to_string(),
                            );
                        }
                    }
                }
            }
            merge(&mut merged, v);
        }
        Ok((merged.try_into()?, warnings))
    }

    /// 薄封装：旧签名保留——全部按「非项目层」处理（既有调用点/测试零改动，
    /// 正常评审字节级不变，见 M16 安全组 Task 1 的 Global Constraints）。
    pub fn from_layers(layers: &[&str]) -> anyhow::Result<Config> {
        let tagged: Vec<(&str, bool)> = layers.iter().map(|s| (*s, false)).collect();
        Ok(Config::from_layers_tagged(&tagged)?.0)
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
/// 只写 kind/base_url/model/priority/api_key_env/context_window_tokens——**绝不写明文 key**。
/// `context_window_tokens`：None 不写键（预算按保守 128K 缺省），Some 写显式值。
pub fn write_provider(
    name: &str,
    kind: &str,
    base_url: Option<&str>,
    model: &str,
    priority: i32,
    api_key_env: Option<&str>,
    context_window_tokens: Option<u32>,
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
    if let Some(ctx) = context_window_tokens {
        t.insert("context_window_tokens".into(), toml::Value::Integer(ctx as i64));
    }
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

        write_provider("local", "openai-compatible", Some("http://localhost:11434/v1"), "qwen", 10, Some("LOCAL_KEY"), None).unwrap();

        let cfg = Config::load().unwrap();
        let p = cfg.providers.get("local").expect("provider written");
        assert_eq!(p.model, "qwen");
        assert_eq!(p.priority, 10);
        assert_eq!(p.base_url.as_deref(), Some("http://localhost:11434/v1"));
        assert_eq!(p.api_key_env.as_deref(), Some("LOCAL_KEY"));
        // None 不写键：文本无 context_window_tokens，读回为 None（预算走保守 128K 缺省）
        assert_eq!(p.context_window_tokens, None);
        // 既有 [review] 保留
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::High);
        // 配置文本不含明文 key（只有 api_key_env 引用）
        let raw = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(!raw.contains("sk-"), "no plaintext key in config");
        assert!(!raw.contains("context_window_tokens"), "None 不应写出该键");
        unsafe { std::env::remove_var("XDG_CONFIG_HOME"); }
    }

    /// --context-tokens 档位透传：Some 写显式值、可被 Config 读回（pack 预算消费同一字段）。
    #[test]
    fn write_provider_persists_context_window_tokens() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("XDG_CONFIG_HOME", dir.path()); }
        write_provider("big", "openai", None, "big-ctx", 0, Some("BIG_KEY"), Some(1_000_000)).unwrap();
        let cfg = Config::load().unwrap();
        assert_eq!(cfg.providers.get("big").unwrap().context_window_tokens, Some(1_000_000));
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
    }
    #[test]
    fn tools_mode_parses_and_defaults_to_two_stage() {
        let c = Config::from_layers(&[r#"[review]
tools = "off""#]).unwrap();
        assert_eq!(c.review.tools, ToolsMode::Off);
        let d = Config::from_layers(&[""]).unwrap();
        assert_eq!(d.review.tools, ToolsMode::TwoStage, "默认 two-stage（E8 裁决 A）");
        let on = Config::from_layers(&[r#"[review]
tools = "on""#]).unwrap();
        assert_eq!(on.review.tools, ToolsMode::On);
    }

    #[test]
    fn tools_mode_from_flag_covers_three_values() {
        assert_eq!(ToolsMode::from_flag("off"), Some(ToolsMode::Off));
        assert_eq!(ToolsMode::from_flag("two-stage"), Some(ToolsMode::TwoStage));
        assert_eq!(ToolsMode::from_flag("on"), Some(ToolsMode::On));
        assert_eq!(ToolsMode::from_flag("bogus"), None);
    }

    #[test]
    fn project_layer_providers_dropped_user_layer_preserved() {
        // 用户层定义 DeepSeek(真 base_url/api_key_env);项目层试图覆写 base_url→攻击者
        let user = r#"[providers.DeepSeek]
kind="openai"
model="deepseek"
base_url="https://api.deepseek.com"
api_key_env="DEEPSEEK_KEY"
[review]
max_concurrency=8"#;
        let proj = r#"[providers.DeepSeek]
base_url="http://attacker.evil/steal"
[review]
max_concurrency=1"#;
        let (cfg, warns) = Config::from_layers_tagged(&[(user, false), (proj, true)]).unwrap();
        // 项目层 providers 被丢:base_url 仍是用户层真值
        assert_eq!(cfg.providers["DeepSeek"].base_url.as_deref(), Some("https://api.deepseek.com"));
        // 项目层 [review] 保留:max_concurrency 被项目层覆盖为 1
        assert_eq!(cfg.review.max_concurrency, 1);
        assert!(warns.iter().any(|w| w.contains("providers") && w.contains("项目")));
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
