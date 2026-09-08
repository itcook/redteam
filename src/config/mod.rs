//! 用户级唯一配置（design v8 §12〔D7 A1-A9 / D11〕）：`$REDTEAM_HOME/config.toml`（缺省 `~/.redteam/`）+ 同目录
//! `secrets.toml`。形状：`[providers.X]` 键名表 + `[[scanners]]` / `[[reviewers]]` / `[[verifiers]]` 内联条目 + `[review.*]` 子表。
//! 无项目层、无候补链、无 `--models`（M21a 批 A）。

pub mod secrets;

use crate::domain::Severity;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// 各变体的 `#[serde(alias = ...)]` 是别名的唯一真相源；`model::validate_setup` 的报错文案
/// 手工抄了一份人读版本（serde alias 列表运行时不可反射，无法程序化生成）——改这里的别名
/// 记得同步改那边的文案（batch4 polish 交叉引用，2026-07-27）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
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

    /// 规范 kebab-case 名（与 serde `rename_all` 一致，恒返别名的标准形）。
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Openai => "openai",
            ProviderKind::OpenaiCompatible => "openai-compatible",
            ProviderKind::Google => "google",
        }
    }
}

/// `[providers.X]`：通道层。`max_concurrency` = 该 provider 同时在途请求上限（缺省继承
/// `review.max_concurrency`）；在途锁在 M21a 批 F 接线，本批只读入。
#[derive(Clone, Debug, Deserialize)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub max_concurrency: Option<u32>,
}

/// 思考档位（角色属性：同一模型可在 scanner 位 low、reviewer 位 high）。backend 消费在批 F。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThinkingLevel { Off, Low, Medium, High }

impl ThinkingLevel {
    pub fn parse(s: &str) -> Option<ThinkingLevel> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
        }
    }
}

/// 角色（design §2 词汇）。批 B 的 domain 直接复用本枚举，勿另定义。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, serde::Serialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Role { Scanner, Reviewer, Verifier }

impl Role {
    pub const ALL: [Role; 3] = [Role::Scanner, Role::Reviewer, Role::Verifier];
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "scanner" => Some(Role::Scanner),
            "reviewer" => Some(Role::Reviewer),
            "verifier" => Some(Role::Verifier),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Scanner => "scanner",
            Role::Reviewer => "reviewer",
            Role::Verifier => "verifier",
        }
    }
    /// config.toml 里的列表键名（`[[scanners]]` …）。
    pub fn list_key(&self) -> &'static str {
        match self {
            Role::Scanner => "scanners",
            Role::Reviewer => "reviewers",
            Role::Verifier => "verifiers",
        }
    }
}

/// 角色条目（`[[scanners]]` / `[[reviewers]]` / `[[verifiers]]`）：模型固有属性与角色属性同写。
/// 同一模型在两个角色各写一次，接受重复。`priority` 退场（无候补链，D7 A3）。
#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct RoleEntry {
    /// 可选身份串；缺省 `provider/model_id`。报告 / 台账 / 异族判定用它。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub provider: String,
    pub model_id: String,
    /// 覆盖 provider.kind。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProviderKind>,
    /// 模型级 key env（覆盖 provider.api_key_env）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// 家族；缺省取 model_id 首段（deepseek / gemini / glm / kimi）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// context window（token），pack 预算护墙用；缺省按 128K 保守估。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// 输出上限（max_tokens 请求字段）：> REDTEAM_MAX_TOKENS env > 32768 默认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_max_tokens: Option<u64>,
    /// 思考型模型（thinking 计入 output 额度——预算与截断判定参考）。
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<ThinkingLevel>,
    /// 单轮超时（秒）：> 缺省 `output_max_tokens ÷ 20 TPS + 60s` > REDTEAM_PER_TURN_SECS env > 150s。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_turn_timeout_secs: Option<u64>,
}

impl RoleEntry {
    /// 身份串：`name` 缺省 `provider/model_id`。
    pub fn display_name(&self) -> String {
        self.name.clone().unwrap_or_else(|| format!("{}/{}", self.provider, self.model_id))
    }
    /// 家族：`family` 缺省 model_id 按 `-` / `_` / `.` 切的首段（小写）。
    pub fn family(&self) -> String {
        self.family.clone().unwrap_or_else(|| {
            self.model_id
                .split(['-', '_', '.'])
                .next()
                .unwrap_or(&self.model_id)
                .to_lowercase()
        })
    }
}

/// merged 视图：条目 + 所属 provider 的覆盖链成品，registry/backend 构造的唯一入口。
#[derive(Clone, Debug)]
pub struct EffectiveModel {
    /// display_name（route_key / 报告名）。
    pub name: String,
    /// 请求体里的 model id。
    pub id: String,
    pub provider: String,
    pub role: Role,
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub context_window: u32,
    pub output_max_tokens: Option<u64>,
    pub per_turn_timeout_secs: Option<u64>,
    pub reasoning: bool,
    pub thinking_level: Option<ThinkingLevel>,
    pub family: String,
    /// provider 在途上限（缺省继承 review.max_concurrency，由调用方决定）。
    pub max_concurrency: Option<u32>,
}

impl EffectiveModel {
    /// 单轮超时缺省公式（design §12.1 注：`output_max_tokens ÷ 最低 TPS + 余量`；TPS 取 20、余量 60s）。
    pub fn per_turn_secs_default(&self) -> Option<u64> {
        self.per_turn_timeout_secs
            .or_else(|| self.output_max_tokens.map(|t| t / 20 + 60))
    }
}

/// 评审工具档位（design 2026-07-13 §2.4，E8 裁决 A）：off=单发单轮无工具；two-stage
/// （默认）=单发 + needs 声明触发的二阶段补查；on=对话式工具（诊断/对照档）。
/// M21b 定工具分层前保留不动（批 A K5）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolsMode { Off, TwoStage, On }

impl ToolsMode {
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

/// `[review.reviewers] max_cells`：`"auto"`（= f(风险分, 规模)，D11 三档 4 / 12 / 32 帽 48）或整数固定上限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaxCells { Auto, Fixed(u32) }

impl<'de> Deserialize<'de> for MaxCells {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw { N(u32), S(String) }
        match Raw::deserialize(d)? {
            Raw::N(n) => Ok(MaxCells::Fixed(n)),
            Raw::S(s) if s == "auto" => Ok(MaxCells::Auto),
            Raw::S(s) => Err(serde::de::Error::custom(format!(
                "max_cells 取 \"auto\" 或整数，得到 {s:?}"
            ))),
        }
    }
}

/// `[review.pack] body`：scanner 组 pack 的装包形态；缺省 `slice`〔D8〕，pack 侧实现在批 E。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackBody { Full, Slice }

#[derive(Clone, Debug, Deserialize)]
pub struct ScannersCfg {
    #[serde(default = "d_workers_4")]
    pub workers: u32,
    /// 同模型多 lens 采样并集（E3：方差对冲旋钮）。
    #[serde(default = "d_one")]
    pub samples: u32,
}
impl Default for ScannersCfg {
    fn default() -> Self { ScannersCfg { workers: d_workers_4(), samples: d_one() } }
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReviewersCfg {
    #[serde(default = "d_workers_2")]
    pub workers: u32,
    #[serde(default = "d_max_cells")]
    pub max_cells: MaxCells,
    #[serde(default)]
    pub reenqueue: bool,
}
impl Default for ReviewersCfg {
    fn default() -> Self { ReviewersCfg { workers: d_workers_2(), max_cells: d_max_cells(), reenqueue: false } }
}

#[derive(Clone, Debug, Deserialize)]
pub struct VerifiersCfg {
    #[serde(default = "d_true")]
    pub enabled: bool,
    #[serde(default = "d_workers_2")]
    pub workers: u32,
}
impl Default for VerifiersCfg {
    fn default() -> Self { VerifiersCfg { enabled: true, workers: d_workers_2() } }
}

/// `[review.grouping]`：ocr 双阈值门起步值（4 / 200 / 10，D3）；`semantic` 默认关（R8）。
#[derive(Clone, Debug, Deserialize)]
pub struct GroupingCfg {
    #[serde(default = "d_min_files")]
    pub min_files: u32,
    #[serde(default = "d_bundle_lines")]
    pub bundle_line_threshold: u32,
    #[serde(default = "d_max_files")]
    pub max_files_per_group: u32,
    #[serde(default)]
    pub semantic: bool,
}
impl Default for GroupingCfg {
    fn default() -> Self {
        GroupingCfg { min_files: d_min_files(), bundle_line_threshold: d_bundle_lines(),
            max_files_per_group: d_max_files(), semantic: false }
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct PackCfg {
    #[serde(default = "d_pack_body")]
    pub body: PackBody,
    #[serde(default = "d_anchor_window")]
    pub anchor_window: u32,
    /// 文件 ≤ 此字节数不切片直接全文（design §10.2）。
    #[serde(default = "d_slice_min_bytes")]
    pub slice_min_bytes: u64,
}
impl Default for PackCfg {
    fn default() -> Self {
        PackCfg { body: d_pack_body(), anchor_window: d_anchor_window(), slice_min_bytes: d_slice_min_bytes() }
    }
}

fn d_one() -> u32 { 1 }
fn d_true() -> bool { true }
fn d_workers_4() -> u32 { 4 }
fn d_workers_2() -> u32 { 2 }
fn d_max_cells() -> MaxCells { MaxCells::Auto }
fn d_min_files() -> u32 { 4 }
fn d_bundle_lines() -> u32 { 200 }
fn d_max_files() -> u32 { 10 }
fn d_pack_body() -> PackBody { PackBody::Slice }
fn d_anchor_window() -> u32 { 30 }
fn d_slice_min_bytes() -> u64 { 8192 }

#[derive(Clone, Debug, Deserialize)]
pub struct ReviewConfig {
    #[serde(default = "default_block")]
    pub block_threshold: Severity,
    #[serde(default = "default_floor")]
    pub conf_floor: f32,
    /// 全局在途上限；provider 级缺省继承。
    #[serde(default = "default_concurrency")]
    pub max_concurrency: usize,
    /// 工具档位：off | two-stage（默认）| on。
    #[serde(default = "default_tools")]
    pub tools: ToolsMode,
    #[serde(default)]
    pub scanners: ScannersCfg,
    #[serde(default)]
    pub reviewers: ReviewersCfg,
    #[serde(default)]
    pub verifiers: VerifiersCfg,
    #[serde(default)]
    pub grouping: GroupingCfg,
    #[serde(default)]
    pub pack: PackCfg,
}
fn default_block() -> Severity { Severity::Medium }
fn default_floor() -> f32 { 0.5 }
fn default_concurrency() -> usize { 8 }
impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig {
            block_threshold: default_block(),
            conf_floor: default_floor(),
            max_concurrency: default_concurrency(),
            tools: default_tools(),
            scanners: ScannersCfg::default(),
            reviewers: ReviewersCfg::default(),
            verifiers: VerifiersCfg::default(),
            grouping: GroupingCfg::default(),
            pack: PackCfg::default(),
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub scanners: Vec<RoleEntry>,
    #[serde(default)]
    pub reviewers: Vec<RoleEntry>,
    #[serde(default)]
    pub verifiers: Vec<RoleEntry>,
    #[serde(default)]
    pub review: ReviewConfig,
}

/// 三角色活跃集（merged 视图）。列表即活跃集：全部并跑，无候补（D7 A3）。
#[derive(Clone, Debug, Default)]
pub struct RoleSets {
    pub scanners: Vec<EffectiveModel>,
    pub reviewers: Vec<EffectiveModel>,
    pub verifiers: Vec<EffectiveModel>,
}

impl RoleSets {
    pub fn get(&self, role: Role) -> &[EffectiveModel] {
        match role {
            Role::Scanner => &self.scanners,
            Role::Reviewer => &self.reviewers,
            Role::Verifier => &self.verifiers,
        }
    }
    /// 全部条目（scanner → reviewer → verifier 序）。
    pub fn all(&self) -> impl Iterator<Item = &EffectiveModel> {
        self.scanners.iter().chain(self.reviewers.iter()).chain(self.verifiers.iter())
    }
    /// display_name → 请求 id（人读报告显示 name、请求体用 id）。
    pub fn id_of(&self, name: &str) -> Option<&str> {
        self.all().find(|m| m.name == name).map(|m| m.id.as_str())
    }
}

impl Config {
    /// 真实加载：单一路径 `config_paths()[0]`；旧路径存在时 stderr 一行提示（不读）；
    /// secrets 未知键 warning 走 stderr——与「解析失败也走 stderr」风格一致。
    pub fn load() -> anyhow::Result<Config> {
        for p in legacy_config_paths_present() {
            eprintln!(
                "notice: 配置已改读 {}（REDTEAM_HOME 可覆盖）；旧文件 {} 未读，请手动迁移（redteam model setup --help）",
                redteam_home().display(),
                p.display()
            );
        }
        let text = config_paths()
            .into_iter()
            .next()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let (cfg, warnings) = Config::from_layers_warned(&[&text])?;
        for w in &warnings {
            eprintln!("warning: {w}");
        }
        let secrets = secrets::load_secrets();
        for w in secrets.warnings.iter().chain(cfg.secret_key_warnings(&secrets).iter()) {
            eprintln!("warning: {w}");
        }
        Ok(cfg)
    }

    /// 多段 TOML 文本按 toml::Value 深合并（右侧覆盖左侧）后反序列化 + 校验；返回 warnings
    /// （已知旧键 / 无定义键）。生产只有一层，多层保留给测试组合。
    pub fn from_layers_warned(layers: &[&str]) -> anyhow::Result<(Config, Vec<String>)> {
        let mut merged = toml::Value::Table(Default::default());
        for layer in layers {
            let v: toml::Value = toml::from_str(layer)?;
            merge(&mut merged, v);
        }
        legacy_shape_guard(&merged)?;
        let mut warnings = Vec::new();
        if merged.get("review").and_then(|r| r.get("risk")).is_some() {
            warnings.push("[review.risk] 无定义（D11：无模式旋钮），已忽略".to_string());
        }
        let cfg: Config = merged.try_into()?;
        cfg.validate()?;
        Ok((cfg, warnings))
    }

    pub fn from_layers(layers: &[&str]) -> anyhow::Result<Config> {
        Ok(Config::from_layers_warned(layers)?.0)
    }

    pub fn entries(&self, role: Role) -> &[RoleEntry] {
        match role {
            Role::Scanner => &self.scanners,
            Role::Reviewer => &self.reviewers,
            Role::Verifier => &self.verifiers,
        }
    }

    /// 所有条目 `(role, entry)`。
    pub fn all_entries(&self) -> impl Iterator<Item = (Role, &RoleEntry)> {
        Role::ALL.iter().flat_map(move |r| self.entries(*r).iter().map(move |e| (*r, e)))
    }

    /// merged 视图（条目 + provider 覆盖链）。provider 引用缺失 → None（validate() 已聚合报错，这里防御）。
    pub fn effective(&self, role: Role, entry: &RoleEntry) -> Option<EffectiveModel> {
        let p = self.providers.get(&entry.provider)?;
        Some(EffectiveModel {
            name: entry.display_name(),
            id: entry.model_id.clone(),
            provider: entry.provider.clone(),
            role,
            kind: entry.kind.unwrap_or(p.kind),
            base_url: p.base_url.clone(),
            context_window: entry.context_window.unwrap_or(128_000),
            output_max_tokens: entry.output_max_tokens,
            per_turn_timeout_secs: entry.per_turn_timeout_secs,
            reasoning: entry.reasoning,
            thinking_level: entry.thinking_level,
            family: entry.family(),
            max_concurrency: p.max_concurrency,
        })
    }

    /// 按 display_name 取某角色的 merged 视图。
    pub fn effective_entry(&self, role: Role, name: &str) -> Option<EffectiveModel> {
        self.entries(role)
            .iter()
            .find(|e| e.display_name() == name)
            .and_then(|e| self.effective(role, e))
    }

    /// 三角色活跃集。
    pub fn active_roles(&self) -> RoleSets {
        let pick = |role: Role| -> Vec<EffectiveModel> {
            self.entries(role).iter().filter_map(|e| self.effective(role, e)).collect()
        };
        RoleSets { scanners: pick(Role::Scanner), reviewers: pick(Role::Reviewer), verifiers: pick(Role::Verifier) }
    }

    /// key 解析链（design §12.2）：条目 `api_key_env`(env) → secrets[provider]."<model_id>" →
    /// provider `api_key_env`(env) → secrets[provider].api_key。
    pub fn resolve_key(&self, entry: &RoleEntry) -> Option<String> {
        self.resolve_key_with(entry, &secrets::load_secrets())
    }

    pub fn resolve_key_with(&self, entry: &RoleEntry, secrets: &secrets::Secrets) -> Option<String> {
        if let Some(var) = &entry.api_key_env {
            if let Ok(v) = std::env::var(var) {
                return Some(v);
            }
        }
        let ps = secrets.providers.get(&entry.provider);
        if let Some(k) = ps.and_then(|p| p.models.get(&entry.model_id)) {
            return Some(k.api_key.clone());
        }
        if let Some(p) = self.providers.get(&entry.provider) {
            if let Some(var) = &p.api_key_env {
                if let Ok(v) = std::env::var(var) {
                    return Some(v);
                }
            }
        }
        ps.and_then(|p| p.api_key.clone())
    }

    /// provider 维度 key（`model list --remote` / setup 落盘后 probe 用）：provider `api_key_env`(env) →
    /// secrets[provider].api_key → 该 provider 下任一 model 级 key。
    pub fn resolve_provider_key(&self, provider: &str) -> Option<String> {
        if let Some(p) = self.providers.get(provider) {
            if let Some(var) = &p.api_key_env {
                if let Ok(v) = std::env::var(var) {
                    return Some(v);
                }
            }
        }
        let secrets = secrets::load_secrets();
        let ps = secrets.providers.get(provider)?;
        ps.api_key.clone().or_else(|| ps.models.values().next().map(|e| e.api_key.clone()))
    }

    /// secrets 守卫②：provider 下除 `api_key` 外的键若不匹配 config 该 provider 任一条目的 model_id → warning。
    pub fn secret_key_warnings(&self, secrets: &secrets::Secrets) -> Vec<String> {
        let mut out = Vec::new();
        for (prov, ps) in &secrets.providers {
            for mid in ps.models.keys() {
                let known = self
                    .all_entries()
                    .any(|(_, e)| &e.provider == prov && &e.model_id == mid);
                if !known {
                    out.push(format!(
                        "secrets.toml [providers.{prov}] 的键 {mid:?} 在 config.toml 该 provider 下没有对应 model_id 条目（拼写？）"
                    ));
                }
            }
        }
        out
    }

    /// 结构校验（load 后必调）：条目引用的 provider 必须存在；同角色 display_name 不得重复。
    /// 全部问题聚合成一条错误，不挤牙膏。`scanners` 为空不在此报错（review 时报「未配置 [[scanners]]」）。
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut errs = Vec::new();
        for role in Role::ALL {
            let mut seen = std::collections::BTreeSet::new();
            for e in self.entries(role) {
                if !self.providers.contains_key(&e.provider) {
                    errs.push(format!(
                        "[[{}]] {} 引用了不存在的 provider {:?}（已配置：{}）",
                        role.list_key(),
                        e.display_name(),
                        e.provider,
                        if self.providers.is_empty() { "无".to_string() } else {
                            self.providers.keys().cloned().collect::<Vec<_>>().join(", ")
                        }
                    ));
                }
                if !seen.insert(e.display_name()) {
                    errs.push(format!("[[{}]] 里 name {:?} 重复", role.list_key(), e.display_name()));
                }
            }
        }
        if !errs.is_empty() {
            anyhow::bail!("配置校验失败：\n{}", errs.join("\n"));
        }
        Ok(())
    }
}

/// 旧形状指引：`[models.*]` / `review.default_models` / 条目 `priority` / `output_limit` /
/// provider 上的 model 级字段 → 指向性错误，不静默丢配置。
fn legacy_shape_guard(merged: &toml::Value) -> anyhow::Result<()> {
    let mut found = Vec::new();
    if merged.get("models").is_some() {
        found.push("顶层 [models.*]（改为 [[scanners]] / [[reviewers]] / [[verifiers]] 内联条目，字段 id → model_id、output_limit → output_max_tokens）".to_string());
    }
    if merged.get("review").and_then(|r| r.get("default_models")).is_some() {
        found.push("[review] default_models（列表即活跃集，无需指定）".to_string());
    }
    for key in ["scanners", "reviewers", "verifiers"] {
        if let Some(toml::Value::Array(items)) = merged.get(key) {
            for (i, it) in items.iter().enumerate() {
                if let Some(t) = it.as_table() {
                    for f in ["priority", "output_limit", "id"] {
                        if t.contains_key(f) {
                            found.push(format!("[[{key}]] 第 {} 条的 {f:?}（priority 退场；output_limit → output_max_tokens；id → model_id）", i + 1));
                        }
                    }
                }
            }
        }
    }
    if let Some(toml::Value::Table(ps)) = merged.get("providers") {
        for (pname, pv) in ps {
            if let Some(t) = pv.as_table() {
                for f in ["model", "context_window_tokens", "priority", "per_turn_timeout_secs", "output_limit"] {
                    if t.contains_key(f) {
                        found.push(format!("[providers.{pname}] 的 {f:?}（属于角色条目字段）"));
                    }
                }
            }
        }
    }
    if !found.is_empty() {
        anyhow::bail!(
            "配置形状已升级（用户级唯一 + 角色列表，design §12.1；见 redteam --help）：\n{}",
            found.join("\n")
        );
    }
    Ok(())
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

/// 配置根：`$REDTEAM_HOME` 否则 `~/.redteam`。
pub fn redteam_home() -> PathBuf {
    std::env::var_os("REDTEAM_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            home.join(".redteam")
        })
}

/// 配置路径（单一层；Vec 形状保留给 `model list` 的位置展示）。
pub fn config_paths() -> Vec<PathBuf> {
    vec![redteam_home().join("config.toml")]
}

/// 旧路径探测（只提示不读）：`~/.config/redteam/config.toml`（09-04 前用户层）与 `./.redteam/config.toml`（项目层）。
pub fn legacy_config_paths_present() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        let p = home.join(".config").join("redteam").join("config.toml");
        if p.is_file() {
            out.push(p);
        }
    }
    let proj = PathBuf::from(".redteam").join("config.toml");
    if proj.is_file() {
        out.push(proj);
    }
    out
}

/// `model setup` 落盘：深合并写 `[providers.<provider>]`（显式给出的通道字段更新）+ 追加角色条目
/// （同 role 下 provider+model_id 已存在则原位更新，不重复追加）。**绝不写明文 key**。
pub fn write_entry(role: Role, entry: &RoleEntry, provider: &ProviderConfig) -> anyhow::Result<()> {
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
    let pt = providers_t
        .entry(entry.provider.clone())
        .or_insert_with(|| toml::Value::Table(Default::default()));
    if let Some(t) = pt.as_table_mut() {
        t.insert("kind".into(), toml::Value::String(provider.kind.as_str().to_string()));
        if let Some(b) = &provider.base_url {
            t.insert("base_url".into(), toml::Value::String(b.clone()));
        }
        if let Some(e) = &provider.api_key_env {
            t.insert("api_key_env".into(), toml::Value::String(e.clone()));
        }
        if let Some(m) = provider.max_concurrency {
            t.insert("max_concurrency".into(), toml::Value::Integer(m as i64));
        }
    }

    let entry_v = toml::Value::try_from(entry)?;
    let list = root_t
        .entry(role.list_key().to_string())
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let arr = list
        .as_array_mut()
        .ok_or_else(|| anyhow::anyhow!("[[{}]] is not an array", role.list_key()))?;
    let same = |v: &toml::Value| {
        v.get("provider").and_then(|x| x.as_str()) == Some(entry.provider.as_str())
            && v.get("model_id").and_then(|x| x.as_str()) == Some(entry.model_id.as_str())
    };
    match arr.iter_mut().find(|v| same(v)) {
        Some(slot) => *slot = entry_v,
        None => arr.push(entry_v),
    }

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, toml::to_string_pretty(&root)?)?;
    Ok(())
}

/// 测试专用：跨 config 子模块串行化 REDTEAM_HOME 环境变量写入，防并行测试污染。
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// design §12.1 样例（去掉 D11 删除的 `[review.risk]`）。
    const SAMPLE: &str = r#"
[providers.deepseek]
kind = "openai"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"
max_concurrency = 20

[providers.agct]
kind = "openai"
base_url = "https://api.aigocode.team/v1"
max_concurrency = 20

[[scanners]]
name = "flash"
provider = "deepseek"
model_id = "deepseek-v4-flash"
context_window = 1_000_000
output_max_tokens = 384_000
reasoning = true
thinking_level = "low"

[[scanners]]
name = "gemini-flash"
provider = "agct"
model_id = "gemini-3.8-flash"
kind = "openai"
api_key_env = "AGCT_GEMINI_API_KEY"
family = "gemini"
context_window = 1_048_576
output_max_tokens = 65_536
reasoning = true
thinking_level = "low"

[[reviewers]]
name = "pro"
provider = "deepseek"
model_id = "deepseek-v4-pro"
context_window = 1_000_000
output_max_tokens = 384_000
reasoning = true
thinking_level = "high"
per_turn_timeout_secs = 1200

[review]
block_threshold = "medium"
conf_floor = 0.5
max_concurrency = 8

[review.scanners]
workers = 4
samples = 1

[review.reviewers]
workers = 2
max_cells = "auto"
reenqueue = false

[review.verifiers]
enabled = true
workers = 2

[review.grouping]
min_files = 4
bundle_line_threshold = 200
max_files_per_group = 10
semantic = false

[review.pack]
body = "slice"
anchor_window = 30
"#;

    #[test]
    fn sample_config_parses_whole() {
        let cfg = Config::from_layers(&[SAMPLE]).unwrap();
        assert_eq!(cfg.scanners.len(), 2);
        assert_eq!(cfg.reviewers.len(), 1);
        assert!(cfg.verifiers.is_empty());
        assert_eq!(cfg.providers["deepseek"].max_concurrency, Some(20));
        let roles = cfg.active_roles();
        let flash = &roles.scanners[0];
        assert_eq!(flash.name, "flash");
        assert_eq!(flash.family, "deepseek");
        assert_eq!(flash.thinking_level, Some(ThinkingLevel::Low));
        assert_eq!(flash.context_window, 1_000_000);
        let gem = &roles.scanners[1];
        assert_eq!(gem.family, "gemini");
        assert_eq!(gem.kind, ProviderKind::Openai);
        let pro = &roles.reviewers[0];
        assert_eq!(pro.per_turn_timeout_secs, Some(1200));
        assert_eq!(pro.role, Role::Reviewer);
        assert_eq!(cfg.review.reviewers.max_cells, MaxCells::Auto);
        assert_eq!(cfg.review.pack.body, PackBody::Slice);
        assert_eq!(cfg.review.grouping.max_files_per_group, 10);
    }

    #[test]
    fn defaults_name_and_family() {
        let cfg = Config::from_layers(&[r#"[providers.p]
kind="openai"
[[scanners]]
provider="p"
model_id="glm-5.3"
"#]).unwrap();
        let e = &cfg.scanners[0];
        assert_eq!(e.display_name(), "p/glm-5.3");
        assert_eq!(e.family(), "glm");
        let em = cfg.effective_entry(Role::Scanner, "p/glm-5.3").unwrap();
        assert_eq!(em.context_window, 128_000, "未声明窗口 → 保守 128K");
        assert_eq!(em.per_turn_secs_default(), None);
        assert_eq!(cfg.review.pack.body, PackBody::Slice, "D8：slice 默认");
        assert_eq!(cfg.review.pack.slice_min_bytes, 8192);
        assert_eq!(cfg.review.scanners.workers, 4);
    }

    #[test]
    fn per_turn_default_formula_from_output_max_tokens() {
        let cfg = Config::from_layers(&[r#"[providers.p]
kind="openai"
[[reviewers]]
provider="p"
model_id="m"
output_max_tokens=32768
"#]).unwrap();
        let em = &cfg.active_roles().reviewers[0];
        assert_eq!(em.per_turn_secs_default(), Some(32768 / 20 + 60));
    }

    #[test]
    fn missing_provider_errors_aggregate() {
        let err = Config::from_layers(&[r#"[providers.p]
kind="openai"
[[scanners]]
provider="nope"
model_id="a"
[[reviewers]]
provider="nope2"
model_id="b"
"#]).unwrap_err().to_string();
        assert!(err.contains("nope") && err.contains("nope2"), "两条一次说清: {err}");
        assert!(err.contains("已配置：p"));
    }

    #[test]
    fn duplicate_display_name_in_role_errors() {
        let err = Config::from_layers(&[r#"[providers.p]
kind="openai"
[[scanners]]
name="x"
provider="p"
model_id="a"
[[scanners]]
name="x"
provider="p"
model_id="b"
"#]).unwrap_err().to_string();
        assert!(err.contains("重复"), "{err}");
    }

    #[test]
    fn max_cells_two_shapes() {
        let a = Config::from_layers(&["[review.reviewers]\nmax_cells = \"auto\"\n"]).unwrap();
        assert_eq!(a.review.reviewers.max_cells, MaxCells::Auto);
        let n = Config::from_layers(&["[review.reviewers]\nmax_cells = 12\n"]).unwrap();
        assert_eq!(n.review.reviewers.max_cells, MaxCells::Fixed(12));
        let bad = Config::from_layers(&["[review.reviewers]\nmax_cells = \"lots\"\n"]);
        assert!(bad.is_err());
    }

    #[test]
    fn legacy_keys_point_to_new_shape() {
        let e1 = Config::from_layers(&["[models.x]\nprovider=\"p\"\nid=\"m\"\n"]).unwrap_err().to_string();
        assert!(e1.contains("[models.*]") && e1.contains("§12.1"), "{e1}");
        let e2 = Config::from_layers(&["[review]\ndefault_models=[\"a\"]\n"]).unwrap_err().to_string();
        assert!(e2.contains("default_models"), "{e2}");
        let e3 = Config::from_layers(&["[providers.p]\nkind=\"openai\"\n[[scanners]]\nprovider=\"p\"\nmodel_id=\"m\"\npriority=3\n"])
            .unwrap_err().to_string();
        assert!(e3.contains("priority"), "{e3}");
        let e4 = Config::from_layers(&["[providers.p]\nkind=\"openai\"\nmodel=\"m\"\n"]).unwrap_err().to_string();
        assert!(e4.contains("[providers.p]"), "{e4}");
    }

    #[test]
    fn review_risk_is_unknown_and_warned() {
        let (cfg, warns) = Config::from_layers_warned(&["[review.risk]\nmode=\"auto\"\n"]).unwrap();
        assert!(warns.iter().any(|w| w.contains("review.risk")), "{warns:?}");
        assert_eq!(cfg.review.max_concurrency, 8);
    }

    #[test]
    fn defaults_apply_when_absent() {
        let cfg = Config::from_layers(&[]).unwrap();
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::Medium);
        assert_eq!(cfg.review.conf_floor, 0.5);
        assert_eq!(cfg.review.reviewers.max_cells, MaxCells::Auto);
        assert!(cfg.active_roles().scanners.is_empty());
    }

    #[test]
    fn tools_mode_parses_and_defaults_to_two_stage() {
        let c = Config::from_layers(&["[review]\ntools = \"off\""]).unwrap();
        assert_eq!(c.review.tools, ToolsMode::Off);
        let d = Config::from_layers(&[""]).unwrap();
        assert_eq!(d.review.tools, ToolsMode::TwoStage, "默认 two-stage（E8 裁决 A）");
        assert_eq!(ToolsMode::from_flag("bogus"), None);
    }

    #[test]
    fn key_resolution_chain_four_levels() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe {
            std::env::set_var("REDTEAM_TEST_KEY", "from-entry-env");
            std::env::set_var("REDTEAM_TEST_PKEY", "from-provider-env");
        }
        let cfg = Config::from_layers(&[r#"[providers.xp]
kind="openai"
api_key_env="REDTEAM_TEST_PKEY"
[[scanners]]
name="e"
provider="xp"
model_id="m"
api_key_env="REDTEAM_TEST_KEY"
[[scanners]]
name="n"
provider="xp"
model_id="m2"
"#]).unwrap();
        let sec = secrets::Secrets::parse(r#"[providers.xp]
api_key = "prov-secret"
"m2" = "model-secret"
"#);
        let e = &cfg.scanners[0];
        let n = &cfg.scanners[1];
        // ① 条目 env
        assert_eq!(cfg.resolve_key_with(e, &sec).as_deref(), Some("from-entry-env"));
        // ② secrets model 级（n 无条目 env）
        assert_eq!(cfg.resolve_key_with(n, &sec).as_deref(), Some("model-secret"));
        // ③ provider env（secrets 无该 model）
        let sec2 = secrets::Secrets::parse("[providers.xp]\napi_key = \"prov-secret\"\n");
        assert_eq!(cfg.resolve_key_with(n, &sec2).as_deref(), Some("from-provider-env"));
        // ④ secrets provider 级
        unsafe { std::env::remove_var("REDTEAM_TEST_PKEY"); }
        assert_eq!(cfg.resolve_key_with(n, &sec2).as_deref(), Some("prov-secret"));
        assert_eq!(cfg.resolve_key_with(n, &secrets::Secrets::default()), None);
        unsafe { std::env::remove_var("REDTEAM_TEST_KEY"); }
    }

    #[test]
    fn unknown_secret_model_key_warns() {
        let cfg = Config::from_layers(&[r#"[providers.xp]
kind="openai"
[[scanners]]
provider="xp"
model_id="m"
"#]).unwrap();
        let sec = secrets::Secrets::parse("[providers.xp]\n\"m\" = \"k\"\n\"typo-model\" = \"k2\"\n");
        let w = cfg.secret_key_warnings(&sec);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("typo-model"));
    }

    #[test]
    fn write_entry_persists_and_updates_in_place() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("REDTEAM_HOME", dir.path()); }
        let cfg_path = config_paths()[0].clone();
        assert!(cfg_path.starts_with(dir.path()));
        std::fs::create_dir_all(cfg_path.parent().unwrap()).unwrap();
        std::fs::write(&cfg_path, "[review]\nblock_threshold=\"high\"\n").unwrap();
        let prov = ProviderConfig { kind: ProviderKind::OpenaiCompatible, base_url: Some("http://localhost:11434/v1".into()),
            api_key_env: Some("LOCAL_KEY".into()), max_concurrency: None };
        let entry = RoleEntry { name: None, provider: "ollama".into(), model_id: "qwen".into(), kind: None, api_key_env: None,
            family: None, context_window: None, output_max_tokens: None, reasoning: false, thinking_level: None, per_turn_timeout_secs: None };
        write_entry(Role::Scanner, &entry, &prov).unwrap();
        let cfg = Config::load().unwrap();
        assert_eq!(cfg.scanners.len(), 1);
        let em = cfg.effective_entry(Role::Scanner, "ollama/qwen").unwrap();
        assert_eq!(em.base_url.as_deref(), Some("http://localhost:11434/v1"));
        assert_eq!(em.context_window, 128_000);
        assert_eq!(cfg.review.block_threshold, crate::domain::Severity::High, "既有 [review] 保留");
        // 同 provider/model_id 再写 → 原位更新不重复
        let entry2 = RoleEntry { context_window: Some(1_000_000), thinking_level: Some(ThinkingLevel::High), ..entry.clone() };
        write_entry(Role::Scanner, &entry2, &prov).unwrap();
        let cfg2 = Config::load().unwrap();
        assert_eq!(cfg2.scanners.len(), 1);
        assert_eq!(cfg2.effective_entry(Role::Scanner, "ollama/qwen").unwrap().context_window, 1_000_000);
        let raw = std::fs::read_to_string(&cfg_path).unwrap();
        assert!(!raw.contains("sk-"), "no plaintext key in config");
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn home_and_paths_follow_env() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("REDTEAM_HOME", "/tmp/rt-home-x"); }
        assert_eq!(config_paths(), vec![PathBuf::from("/tmp/rt-home-x/config.toml")]);
        assert_eq!(secrets::secrets_path(), PathBuf::from("/tmp/rt-home-x/secrets.toml"));
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
        assert!(redteam_home().ends_with(".redteam"));
    }

    #[test]
    fn load_ignores_legacy_project_file_and_notices() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("REDTEAM_HOME", dir.path()); }
        // 用户级新文件 + 一个「项目层」旧文件（相对路径 .redteam/config.toml 只在 cwd 下探测，这里直接测探测函数）
        std::fs::write(dir.path().join("config.toml"), "[review]\nmax_concurrency=3\n").unwrap();
        let cfg = Config::load().unwrap();
        assert_eq!(cfg.review.max_concurrency, 3);
        let legacy = legacy_config_paths_present();
        assert!(legacy.iter().all(|p| !p.starts_with(dir.path())), "新目录不算旧路径");
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }
}
