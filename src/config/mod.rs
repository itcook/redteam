//! 用户级唯一配置（design v9 §12〔D7 A8 / D12〕）：`$REDTEAM_HOME/config.json`（缺省 `~/.redteam/`）+ 同目录 `secrets.json`。
//! 严格 JSON、键 camelCase、无注释。形状 = providers → models（按 id 做键）→ roles：模型固有字段只写一次，角色属性挂模型下；
//! 某角色的活跃集 = 拥有该角色键的全部模型（列表即活跃集、无候补，D7 A3）。无项目层、无 `--models`（M21a 批 A 重做）。

pub mod secrets;

use crate::domain::Severity;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 各变体的 `#[serde(alias = ...)]` 是别名的唯一真相源；`model::validate_setup` 的报错文案
/// 手工抄了一份人读版本（serde alias 列表运行时不可反射）——改这里的别名记得同步改那边的文案。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
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
    pub fn parse(s: &str) -> Option<ProviderKind> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Openai => "openai",
            ProviderKind::OpenaiCompatible => "openai-compatible",
            ProviderKind::Google => "google",
        }
    }
}

/// 推理强度（角色属性：同一模型可在 scanner 位 low、reviewer 位 high）。backend 消费在批 F。
/// 值原样透传给上游；`off` / `none` 不再建模——只有极少数模型支持关推理，需要时由模型默认行为决定。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningEffort { Minimal, Low, Medium, High, Xhigh, Max, Ultra }

impl ReasoningEffort {
    pub fn parse(s: &str) -> Option<ReasoningEffort> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ReasoningEffort::Minimal => "minimal",
            ReasoningEffort::Low => "low",
            ReasoningEffort::Medium => "medium",
            ReasoningEffort::High => "high",
            ReasoningEffort::Xhigh => "xhigh",
            ReasoningEffort::Max => "max",
            ReasoningEffort::Ultra => "ultra",
        }
    }
}

/// 角色（design §2 词汇）。批 B 的 domain 直接复用本枚举。作 `roles` 的 map 键时序列化为小写串。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Scanner, Reviewer, Verifier }

impl Role {
    pub const ALL: [Role; 3] = [Role::Scanner, Role::Reviewer, Role::Verifier];
    pub fn parse(s: &str) -> Option<Role> {
        match s.trim() {
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
}

/// 角色属性（`models.<id>.roles.<role>`）。将来的角色级旋钮只加在这里。
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleAttrs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// 模型（`providers.<name>.models.<id>`）：固有属性只写一次；`roles` 键即该模型参与的角色。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    /// 人读显示名（`model list` / 将来报告用）；**不是**身份串——身份串固定为 `provider/id`〔D12 定案 3〕。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 覆盖 provider.kind；两层都缺 → validate 报错。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProviderKind>,
    /// 模型级 key env（覆盖 provider.apiKeyEnv）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// 家族；缺省取 id 首段（deepseek / gemini / glm / kimi）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// context window（token），pack 预算护墙；缺省保守 128K；下限 `MIN_CONTEXT_WINDOW`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    /// 输出上限（max_tokens，≥ 1）：> REDTEAM_MAX_TOKENS env > 32768 默认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
    /// 单轮超时（秒，≥ 1）：> REDTEAM_PER_TURN_SECS > 公式（见 `EffectiveModel::per_turn_secs`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_turn_timeout_secs: Option<u64>,
    /// 思考型模型（thinking token 计入输出额度——预算与截断判定参考）。
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub roles: BTreeMap<Role, RoleAttrs>,
}

/// `providers.<name>`：通道层。`maxConcurrency` = 该 provider 在途上限（缺省继承 `review.maxConcurrency`；在途锁批 F 接线）。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<ProviderKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelConfig>,
}

/// context window 下限：预算公式 `(ctx − 16 000) × 3 − 24 000` 在 24 000 以下饱和为 0，评审会在空卷宗上静默跑。
pub const MIN_CONTEXT_WINDOW: u32 = 32_000;

/// merged 视图：模型 + provider 覆盖链 + 角色属性成品，registry / backend 构造的唯一入口。
#[derive(Clone, Debug)]
pub struct EffectiveModel {
    /// 身份串 `provider/id`（route_key / 报告名 / 台账 model 字段）。
    pub name: String,
    /// 人读显示名（可选，只作显示）。
    pub display_name: Option<String>,
    /// 请求体里的 model id。
    pub id: String,
    pub provider: String,
    pub role: Role,
    pub kind: ProviderKind,
    pub base_url: Option<String>,
    pub context_window: u32,
    pub max_output_tokens: Option<u64>,
    pub per_turn_timeout_secs: Option<u64>,
    pub reasoning: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub family: String,
    pub max_concurrency: Option<u32>,
}

/// 单轮输出上限默认值（思考型模型的 thinking tokens 也计入；8192 旧值在大 bundle 下会被截断，2026-09-03 dogfood 实证）。
pub const DEFAULT_MAX_TOKENS: u64 = 32_768;

/// `REDTEAM_MAX_TOKENS` env 解析——backend 请求体与超时公式共用同一函数（评审 #7）：非法 / 0 → warning + 默认。
pub fn env_max_tokens() -> u64 {
    match std::env::var("REDTEAM_MAX_TOKENS") {
        Ok(s) => match s.trim().parse::<u64>() {
            Ok(v) if v > 0 => v,
            _ => {
                // 静默回落会让运维误以为旋钮已生效，复发时被迫活体复现调试。
                eprintln!("warning: REDTEAM_MAX_TOKENS={s:?} 非法（需正整数），回落默认 {DEFAULT_MAX_TOKENS}");
                DEFAULT_MAX_TOKENS
            }
        },
        Err(_) => DEFAULT_MAX_TOKENS,
    }
}

/// `REDTEAM_PER_TURN_SECS` env 解析（`per_turn_secs` 与 `provider::per_turn_deadline` 共用，评审 S6）：非法 / 0 → warning + None（按模型 / 公式）。
pub fn env_per_turn_secs() -> Option<u64> {
    match std::env::var("REDTEAM_PER_TURN_SECS") {
        Ok(s) => match s.trim().parse::<u64>() {
            Ok(v) if v > 0 => Some(v),
            _ => {
                eprintln!("warning: REDTEAM_PER_TURN_SECS={s:?} 非法（需正整数秒），忽略——按模型 perTurnTimeoutSecs / 公式");
                None
            }
        },
        Err(_) => None,
    }
}

impl EffectiveModel {
    /// backend 实际发送的 max_tokens：模型 `maxOutputTokens` > `REDTEAM_MAX_TOKENS` env > 32768（与 backends.rs 共用 `env_max_tokens`）。
    pub fn max_tokens_effective(&self) -> u64 {
        self.max_output_tokens.unwrap_or_else(env_max_tokens)
    }
    /// 单轮超时（秒）〔D12 #6〕：模型 `perTurnTimeoutSecs` > `REDTEAM_PER_TURN_SECS` > `max_tokens_effective ÷ 20 TPS + 60s`。
    pub fn per_turn_secs(&self) -> u64 {
        if let Some(s) = self.per_turn_timeout_secs {
            return s;
        }
        if let Some(s) = env_per_turn_secs() {
            return s;
        }
        self.max_tokens_effective() / 20 + 60
    }
}

/// 评审工具档位（design 2026-07-13 §2.4）：off / two-stage（默认）/ on。M21b 定工具分层前保留不动。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
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

/// `review.reviewers.maxCells`：`"auto"`（f(风险分, 规模)，D11 三档 4 / 12 / 32 帽 48）或整数固定上限。
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
            Raw::S(s) => Err(serde::de::Error::custom(format!("maxCells 取 \"auto\" 或整数，得到 {s:?}"))),
        }
    }
}
impl Serialize for MaxCells {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            MaxCells::Auto => s.serialize_str("auto"),
            MaxCells::Fixed(n) => s.serialize_u32(*n),
        }
    }
}

/// `review.pack.body`：scanner 组 pack 装包形态；缺省 `slice`〔D8〕，pack 侧实现在批 E。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackBody { Full, Slice }

/// 角色节（design §12.1〔D13〕）：无角色级 `workers`——并发只看 provider `maxConcurrency`（唯一闸，§12.3）。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScannersCfg {
    /// 同模型多 lens 采样并集（E3：方差对冲旋钮）。
    #[serde(default = "d_one")]
    pub samples: u32,
}
impl Default for ScannersCfg {
    fn default() -> Self { ScannersCfg { samples: d_one() } }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewersCfg {
    #[serde(default = "d_max_cells")]
    pub max_cells: MaxCells,
    #[serde(default)]
    pub reenqueue: bool,
}
impl Default for ReviewersCfg {
    fn default() -> Self { ReviewersCfg { max_cells: d_max_cells(), reenqueue: false } }
}

/// `review.verifiers`：流式复核开关。**缺省关**——批 D-4 的离线基线按预登记阈值裁定
/// （误驳 ≤ 5% ∧ 驳回 ≥ 30% 才默认开）：异族 deepseek-flash 驳回 31.2% / 误驳 12.0%、
/// 同族 glm-5.3 驳回 18.8% / 误驳 8.0%，两臂的误驳都超门槛 → 默认关，要开显式写 `"enabled": true`。
/// （数字为收口修掉「角色尾未 render」后的重跑值；修前那版见 research 的对照表。）
/// 数字与归因见 `.next/research/2026-09-21-redteam-verifier-offline-baseline-research.md`。
/// 缺省关 = `bool` 的 `Default`，故直接 derive：serde 缺省与 `Default` 只此一个来源，不会各说各话
/// （`verifiers_enabled_defaults_off` 钉住两条路径同值）。
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiersCfg {
    #[serde(default)]
    pub enabled: bool,
}

/// `review.grouping`：ocr 双阈值门起步值（4 / 200 / 10，D3）；`semantic` 默认关（R8）。
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
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
        GroupingCfg { min_files: d_min_files(), bundle_line_threshold: d_bundle_lines(), max_files_per_group: d_max_files(), semantic: false }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackCfg {
    #[serde(default = "d_pack_body")]
    pub body: PackBody,
    #[serde(default = "d_anchor_window")]
    pub anchor_window: u32,
    /// finding 判定半径（design §10.4，M21d 批 B-2）：锚点离最近改动行超过 N 行即「窗外」。
    /// **与 `anchorWindow` 是两回事**——那个是装包的禁真空窗半径，调参方向相反（装包半径大一点更安全，
    /// 判定半径大一点更宽松），共用一个键会让调任一方都要跑两套回归〔批 B 开工裁定 1〕。
    /// 0 = 连观测都关。
    #[serde(default = "d_finding_window")]
    pub finding_window: u32,
    /// 窗外 finding 是否**降级**（移出 findings、只进披露）。默认 false = 只观测不改产出
    /// 〔批 B 开工裁定 2：先拿真实分布再定阈值；直接丢 = 静默少报〕。
    #[serde(default)]
    pub finding_window_enforce: bool,
    /// 文件 ≤ 此字节数不切片直接全文（design §10.2）。
    #[serde(default = "d_slice_min_bytes")]
    pub slice_min_bytes: u64,
}
impl Default for PackCfg {
    fn default() -> Self {
        PackCfg {
            body: d_pack_body(),
            anchor_window: d_anchor_window(),
            finding_window: d_finding_window(),
            finding_window_enforce: false,
            slice_min_bytes: d_slice_min_bytes(),
        }
    }
}

fn d_one() -> u32 { 1 }
fn d_max_cells() -> MaxCells { MaxCells::Auto }
fn d_min_files() -> u32 { 4 }
fn d_bundle_lines() -> u32 { 200 }
fn d_max_files() -> u32 { 10 }
fn d_pack_body() -> PackBody { PackBody::Slice }
fn d_anchor_window() -> u32 { 30 }
fn d_finding_window() -> u32 { 30 }
fn d_slice_min_bytes() -> u64 { 8192 }

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewConfig {
    #[serde(default = "default_block")]
    pub block_threshold: Severity,
    #[serde(default = "default_floor")]
    pub conf_floor: f32,
    /// 全局在途上限缺省（10，D13）；provider 级 `maxConcurrency` 缺省继承它。provider 在途门是唯一一道闸（design §12.3）：
    /// 无角色级 workers；> `MAX_CONCURRENCY_SANE` 只 warning（像是把 maxOutputTokens 填错了字段）。
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
fn default_concurrency() -> usize { 10 }
/// `maxConcurrency` 合理上限（D13）：超过只 warning 不拒——撞 429 时门会自动收敛，但多半是填错字段。
pub const MAX_CONCURRENCY_SANE: u32 = 128;
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
    #[serde(default)]
    pub review: ReviewConfig,
}

/// 三角色活跃集（merged 视图）。顺序 = provider 名、model id 字典序（无语义，D12 K3）。
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
    /// 身份串 → 请求 id。
    pub fn id_of(&self, name: &str) -> Option<&str> {
        self.all().find(|m| m.name == name).map(|m| m.id.as_str())
    }
}

/// env 取值：未设或空白视为未设（`export X_API_KEY=` 占位不算有 key，修补批 X-4）。
fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
}

/// 身份串 = `provider/id`（route_key / 报告名 / 台账 model 字段）；`displayName` 只作显示〔D12 定案 3〕。
pub fn identity(provider: &str, id: &str) -> String {
    format!("{provider}/{id}")
}

impl Config {
    /// 真实加载：`$REDTEAM_HOME/config.json`；文件不存在 = 空配置；其他 io 错误传播（不吞成空配置，#13）；
    /// 新配置尚不存在而旧路径存在 → stderr 各一行提示（不读；新配置已在场即视为已迁移、不再刷提示——dev 二进制与本二进制
    /// 并存期旧文件会长期存在）；secrets 守卫 warning 走 stderr。
    pub fn load() -> anyhow::Result<Config> {
        let (cfg, messages) = Config::load_with_messages()?;
        for m in &messages {
            eprintln!("{m}");
        }
        Ok(cfg)
    }

    /// 同 `load`，但 notice / warning 行返回给调用方而非打印（`model setup` 写前预检用，避免重复刷屏）。
    pub fn load_with_messages() -> anyhow::Result<(Config, Vec<String>)> {
        Self::load_with_messages_opts(true)
    }

    /// 不读 `secrets.json` 的加载：`--pack-only` 只装包不调 provider（design §6 / prd R-C4.1「不读密钥文件」，修补批 X-5）。
    pub fn load_no_secrets() -> anyhow::Result<(Config, Vec<String>)> {
        Self::load_with_messages_opts(false)
    }

    fn load_with_messages_opts(read_secrets: bool) -> anyhow::Result<(Config, Vec<String>)> {
        let mut messages = Vec::new();
        let path = config_path()?;
        // 旧路径迁移 notice **不在这里打**（2026-10-04 对账收窄）：配置每条命令都要加载，而
        // dev 二进制的 `~/.config/redteam/*.toml` 与首版 TOML 在并存期长期存在——只用 env 存 key
        // 的用户于是每次调用都看见同样两行。迁移是一次性动作，提示属于「查配置」那两条命令：
        // `model list` / `model check` 调 `legacy_migration_notices()` 打。
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => anyhow::bail!("读取 {} 失败：{e}", path.display()),
        };
        let (cfg, warnings) = Config::from_json_warned(&text)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        messages.extend(warnings.iter().map(|w| format!("warning: {w}")));
        if read_secrets {
            let secrets = secrets::load_secrets();
            messages.extend(secrets.warnings.iter().chain(cfg.secret_key_warnings(&secrets).iter()).map(|w| format!("warning: {w}")));
        }
        Ok((cfg, messages))
    }

    /// 解析 + 旧形状指引 + 校验；空白文本 = 空配置。返回 warnings（无定义键、无角色模型）。
    pub fn from_json_warned(text: &str) -> anyhow::Result<(Config, Vec<String>)> {
        let mut warnings = Vec::new();
        if text.trim().is_empty() {
            return Ok((Config::default(), warnings));
        }
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| anyhow::anyhow!("JSON 解析失败：{e}"))?;
        warnings.extend(shape_guard(&v)?);
        let cfg: Config = serde_json::from_value(v.clone()).map_err(|e| {
            let at = locate_shape_error(&v).map(|p| format!("{p}：")).unwrap_or_default();
            anyhow::anyhow!("配置形状错误：{at}{e}")
        })?;
        cfg.validate()?;
        for (pname, p) in &cfg.providers {
            if let Some(c) = p.max_concurrency.filter(|c| *c > MAX_CONCURRENCY_SANE) {
                warnings.push(format!("providers.{pname}.maxConcurrency = {c} 像是填错字段（maxOutputTokens？）：它是该 provider 的同时在途请求数上限；撞 429 时门会自动收敛，仍建议改回"));
            }
            for (id, m) in &p.models {
                if m.roles.is_empty() {
                    warnings.push(format!("providers.{pname}.models.{id} 未声明任何 roles，不会参与评审"));
                }
            }
        }
        if cfg.review.max_concurrency > MAX_CONCURRENCY_SANE as usize {
            warnings.push(format!("review.maxConcurrency = {} 像是填错字段（maxOutputTokens？）：它是 provider 在途上限的缺省；撞 429 时门会自动收敛，仍建议改回", cfg.review.max_concurrency));
        }
        Ok((cfg, warnings))
    }

    pub fn from_json(text: &str) -> anyhow::Result<Config> {
        Ok(Config::from_json_warned(text)?.0)
    }

    /// 按 (provider, id) 取模型。
    pub fn model(&self, provider: &str, id: &str) -> Option<(&ProviderConfig, &ModelConfig)> {
        let p = self.providers.get(provider)?;
        let m = p.models.get(id)?;
        Some((p, m))
    }

    /// 按身份串反查 (provider, id)。
    pub fn find(&self, name: &str) -> Option<(String, String)> {
        for (pname, p) in &self.providers {
            for id in p.models.keys() {
                if identity(pname, id) == name {
                    return Some((pname.clone(), id.clone()));
                }
            }
        }
        None
    }

    /// merged 视图；kind 两层都缺 → None（validate 已拦，这里防御）。
    pub fn effective(&self, provider: &str, id: &str, role: Role) -> Option<EffectiveModel> {
        let (p, m) = self.model(provider, id)?;
        let kind = m.kind.or(p.kind)?;
        Some(EffectiveModel {
            name: identity(provider, id),
            display_name: m.display_name.clone(),
            id: id.to_string(),
            provider: provider.to_string(),
            role,
            kind,
            base_url: p.base_url.clone(),
            context_window: m.context_window.unwrap_or(128_000),
            max_output_tokens: m.max_output_tokens,
            per_turn_timeout_secs: m.per_turn_timeout_secs,
            reasoning: m.reasoning,
            reasoning_effort: m.roles.get(&role).and_then(|r| r.reasoning_effort),
            family: m.family.clone().unwrap_or_else(|| {
                id.split(['-', '_', '.']).next().unwrap_or(id).to_lowercase()
            }),
            max_concurrency: p.max_concurrency,
        })
    }

    /// 三角色活跃集：每角色 = 拥有该角色键的全部模型。
    pub fn active_roles(&self) -> RoleSets {
        let mut sets = RoleSets::default();
        for (pname, p) in &self.providers {
            for (id, m) in &p.models {
                for role in m.roles.keys() {
                    if let Some(em) = self.effective(pname, id, *role) {
                        match role {
                            Role::Scanner => sets.scanners.push(em),
                            Role::Reviewer => sets.reviewers.push(em),
                            Role::Verifier => sets.verifiers.push(em),
                        }
                    }
                }
            }
        }
        sets
    }

    /// key 解析链（§12.2）：模型 `apiKeyEnv`(env) → secrets.models[id] → provider `apiKeyEnv`(env) → secrets.apiKey。
    pub fn resolve_key(&self, provider: &str, id: &str) -> Option<String> {
        self.resolve_key_with(provider, id, &secrets::load_secrets())
    }

    pub fn resolve_key_with(&self, provider: &str, id: &str, secrets: &secrets::Secrets) -> Option<String> {
        let (p, m) = self.model(provider, id)?;
        if let Some(var) = &m.api_key_env {
            if let Some(v) = env_nonempty(var) {
                return Some(v);
            }
        }
        let ps = secrets.providers.get(provider);
        if let Some(k) = ps.and_then(|s| s.models.get(id)) {
            return Some(k.clone());
        }
        if let Some(var) = &p.api_key_env {
            if let Some(v) = env_nonempty(var) {
                return Some(v);
            }
        }
        ps.and_then(|s| s.api_key.clone())
    }

    /// provider 维度 key（`model list --remote` 用）：provider `apiKeyEnv`(env) → secrets.apiKey；不回退到任一模型级 key（#5）。
    pub fn resolve_provider_key(&self, provider: &str) -> Option<String> {
        if let Some(p) = self.providers.get(provider) {
            if let Some(var) = &p.api_key_env {
                if let Some(v) = env_nonempty(var) {
                    return Some(v);
                }
            }
        }
        secrets::load_secrets().providers.get(provider).and_then(|s| s.api_key.clone())
    }

    /// secrets 守卫（#10）：secrets 里的 provider 名不在 config、或 model id 不在该 provider 下 → warning。
    pub fn secret_key_warnings(&self, secrets: &secrets::Secrets) -> Vec<String> {
        let mut out = Vec::new();
        for (prov, ps) in &secrets.providers {
            match self.providers.get(prov) {
                None => out.push(format!("secrets.json providers.{prov} 在 config.json 里没有对应 provider（拼写 / 大小写？）")),
                Some(p) => {
                    for mid in ps.models.keys() {
                        if !p.models.contains_key(mid) {
                            out.push(format!("secrets.json providers.{prov}.models.{mid:?} 在 config.json 该 provider 下没有对应模型（拼写？）"));
                        }
                    }
                }
            }
        }
        out
    }

    /// 结构校验（load 后必调）：kind 可解析；contextWindow 下限；maxOutputTokens / perTurnTimeoutSecs / maxConcurrency ≥ 1（评审 #3）；
    /// review 数值 ≥ 1。全部问题聚合成一条错误。
    /// 无 scanner 不在此报错（review 时报「未配置 scanner」）。
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut errs = Vec::new();
        for (pname, p) in &self.providers {
            if pname.is_empty() || pname.contains('/') {
                errs.push(format!("provider 名 {pname:?} 非法：须非空且不含 '/'（身份串 provider/id 须可拆）"));
            }
            if p.max_concurrency == Some(0) {
                errs.push(format!("providers.{pname}.maxConcurrency 必须 ≥ 1"));
            }
            for (id, m) in &p.models {
                if id.is_empty() {
                    errs.push(format!("providers.{pname}.models 含空 model id"));
                }
                if m.kind.or(p.kind).is_none() {
                    errs.push(format!("providers.{pname}.models.{id}: 缺 kind（模型层或 provider 层至少一处：openai | anthropic）"));
                }
                if let Some(cw) = m.context_window {
                    if cw < MIN_CONTEXT_WINDOW {
                        errs.push(format!("providers.{pname}.models.{id}: contextWindow {cw} 低于下限 {MIN_CONTEXT_WINDOW}（单位是 token，128K 请写 128000）"));
                    }
                }
                if m.max_output_tokens == Some(0) {
                    errs.push(format!("providers.{pname}.models.{id}: maxOutputTokens 必须 ≥ 1"));
                }
                if m.per_turn_timeout_secs == Some(0) {
                    errs.push(format!("providers.{pname}.models.{id}: perTurnTimeoutSecs 必须 ≥ 1"));
                }
            }
        }
        let r = &self.review;
        for (k, v) in [
            ("review.maxConcurrency", r.max_concurrency as u64),
            ("review.scanners.samples", r.scanners.samples as u64),
            ("review.pack.anchorWindow", r.pack.anchor_window as u64),
            // `findingWindow` **不在这张表里**：它的 0 是合法的「关掉观测」（装包半径的 0 才非法）。
            // 塞进来会让文档承诺的退路设不上——两个键语义相反，连校验都不该共用（批 B 裁定 1 的同一条理由）。
            ("review.grouping.minFiles", r.grouping.min_files as u64),
            ("review.grouping.maxFilesPerGroup", r.grouping.max_files_per_group as u64),
        ] {
            if v == 0 {
                errs.push(format!("{k} 必须 ≥ 1"));
            }
        }
        if r.reviewers.max_cells == MaxCells::Fixed(0) {
            errs.push("review.reviewers.maxCells 必须 ≥ 1（或 \"auto\"）".to_string());
        }
        if !(0.0..=1.0).contains(&r.conf_floor) {
            errs.push(format!("review.confFloor {} 须在 0.0..=1.0（评审 S2）", r.conf_floor));
        }
        if !errs.is_empty() {
            anyhow::bail!("配置校验失败：\n{}", errs.join("\n"));
        }
        Ok(())
    }
}

/// 形状守卫（解析前按 `Value` 走一遍已知层级）：旧形状 / snake_case / 改名键 / 非法角色 → **错误**并指向 D12 形状（不静默丢配置）；
/// 已知层级下的无定义键 → **warning**（serde 默认忽略未知字段，评审 #4；不开 `deny_unknown_fields` 以保向前兼容）。
/// provider 名 / model id 是用户键，不检查。新增配置字段时同步下面的已知键表（单测 `review_risk_and_roleless_model_warn` 兜底）。
fn shape_guard(v: &serde_json::Value) -> anyhow::Result<Vec<String>> {
    const ROOT: &[&str] = &["providers", "review"];
    const PROVIDER: &[&str] = &["kind", "baseUrl", "apiKeyEnv", "maxConcurrency", "models"];
    const MODEL: &[&str] = &["displayName", "kind", "apiKeyEnv", "family", "contextWindow", "maxOutputTokens", "perTurnTimeoutSecs", "reasoning", "roles"];
    const ROLE_ATTRS: &[&str] = &["reasoningEffort"];
    const REVIEW: &[&str] = &["blockThreshold", "confFloor", "maxConcurrency", "tools", "scanners", "reviewers", "verifiers", "grouping", "pack"];
    /// review 已删键：`risk`（D11 无模式旋钮）。
    const REVIEW_REMOVED: &[(&str, &str)] = &[("risk", "D11：无模式旋钮")];
    /// (节名, 已知键, 是否角色节)；角色节的已删键 `workers`（D13）在 shape_guard 里按缺省值拼文案。
    const REVIEW_SECTIONS: &[(&str, &[&str], bool)] = &[
        ("scanners", &["samples"], true),
        ("reviewers", &["maxCells", "reenqueue"], true),
        ("verifiers", &["enabled"], true),
        ("grouping", &["minFiles", "bundleLineThreshold", "maxFilesPerGroup", "semantic"], false),
        ("pack", &["body", "anchorWindow", "findingWindow", "findingWindowEnforce", "sliceMinBytes"], false),
    ];
    /// 已知层级的键检查：snake_case → 错误（指引 camelCase 拼写）；`removed`（键 → 为什么没了 / 去哪调）命中 → 专属 warning；
    /// 其余未知 → 通用 warning。已删键统一走 `removed` 表，不另开分支（评审修：同一文件只留一种「已删键」手法）。
    fn check(errors: &mut Vec<String>, warnings: &mut Vec<String>, path: &str, obj: &serde_json::Map<String, serde_json::Value>, known: &[&str], hint: &str) {
        check_keys(errors, warnings, path, obj, known, hint, &[]);
    }
    fn check_keys(errors: &mut Vec<String>, warnings: &mut Vec<String>, path: &str, obj: &serde_json::Map<String, serde_json::Value>, known: &[&str], hint: &str, removed: &[(&str, &str)]) {
        for k in obj.keys() {
            if known.contains(&k.as_str()) {
                continue;
            }
            if let Some((_, why)) = removed.iter().find(|(rk, _)| rk == k) {
                warnings.push(format!("{path}.{k} 已删除（{why}），已忽略"));
                continue;
            }
            if k.contains('_') {
                errors.push(format!("{path}.{k}：键名为 camelCase（{hint}）"));
            } else {
                warnings.push(format!("{path}.{k} 无定义，已忽略（已知键：{}）", known.join(" / ")));
            }
        }
    }
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let Some(top) = v.as_object() else {
        anyhow::bail!("config.json 顶层须是对象（{{\"providers\": …}}）");
    };
    for k in ["scanners", "reviewers", "verifiers", "models"] {
        if top.contains_key(k) {
            errors.push(format!("顶层 {k:?}：角色列表已改为 providers.<name>.models.<id>.roles.<role>"));
        }
    }
    check(&mut errors, &mut warnings, "(顶层)", top, ROOT, "providers / review");
    if let Some(provs) = top.get("providers").and_then(|p| p.as_object()) {
        for (pname, pv) in provs {
            let Some(po) = pv.as_object() else {
                errors.push(format!("providers.{pname} 须是对象"));
                continue;
            };
            check(&mut errors, &mut warnings, &format!("providers.{pname}"), po, PROVIDER, "baseUrl / apiKeyEnv / maxConcurrency");
            match po.get("models") {
                Some(serde_json::Value::Array(_)) => errors.push(format!("providers.{pname}.models 是数组：应为按 model id 做键的对象")),
                Some(serde_json::Value::Object(ms)) => {
                    for (id, mv) in ms {
                        let Some(mo) = mv.as_object() else {
                            errors.push(format!("providers.{pname}.models.{id} 须是对象"));
                            continue;
                        };
                        let mpath = format!("providers.{pname}.models.{id}");
                        if mo.contains_key("name") {
                            errors.push(format!("{mpath}.name 已改名 displayName（身份串固定为 provider/id，不可配置）"));
                        }
                        check(&mut errors, &mut warnings, &mpath, mo, MODEL, "contextWindow / maxOutputTokens / perTurnTimeoutSecs / apiKeyEnv / displayName");
                        if let Some(roles) = mo.get("roles") {
                            let Some(ro) = roles.as_object() else {
                                errors.push(format!("{mpath}.roles 须是对象（键为 scanner / reviewer / verifier）"));
                                continue;
                            };
                            for (role, rv) in ro {
                                if Role::parse(role).is_none() {
                                    errors.push(format!("{mpath}.roles.{role}：角色取 scanner | reviewer | verifier"));
                                    continue;
                                }
                                if let Some(ra) = rv.as_object() {
                                    check(&mut errors, &mut warnings, &format!("{mpath}.roles.{role}"), ra, ROLE_ATTRS, "reasoningEffort");
                                }
                            }
                        }
                    }
                }
                Some(_) => errors.push(format!("providers.{pname}.models 须是对象")),
                None => {}
            }
        }
    }
    if let Some(r) = top.get("review").and_then(|r| r.as_object()) {
        check_keys(&mut errors, &mut warnings, "review", r, REVIEW, "blockThreshold / confFloor / maxConcurrency", REVIEW_REMOVED);
        // D13：角色级 workers 已删——专属提示（并发只看 provider 门、去哪调），不叠加通用「无定义」。
        let workers_why = format!("D13：并发只看 provider maxConcurrency，缺省 review.maxConcurrency = {}", default_concurrency());
        let workers_removed = [("workers", workers_why.as_str())];
        for (sec, known, role_section) in REVIEW_SECTIONS {
            if let Some(so) = r.get(*sec).and_then(|s| s.as_object()) {
                let removed: &[(&str, &str)] = if *role_section { &workers_removed } else { &[] };
                check_keys(&mut errors, &mut warnings, &format!("review.{sec}"), so, known, "如 maxCells / minFiles / sliceMinBytes", removed);
            }
        }
    }
    if !errors.is_empty() {
        anyhow::bail!(
            "配置形状已升级（JSON，providers → models → roles，design §12.1〔D12〕；见 redteam --help）：\n{}",
            errors.join("\n")
        );
    }
    Ok(warnings)
}

/// `from_value` 的错误不带字段路径（serde_json 只在语法错误带行列）：按 model / provider / review 段逐个试解析，
/// 定位第一处失败的路径（评审 S3）。
fn locate_shape_error(v: &serde_json::Value) -> Option<String> {
    let top = v.as_object()?;
    if let Some(provs) = top.get("providers").and_then(|p| p.as_object()) {
        for (pname, pv) in provs {
            if let Some(ms) = pv.get("models").and_then(|m| m.as_object()) {
                for (id, mv) in ms {
                    if serde_json::from_value::<ModelConfig>(mv.clone()).is_err() {
                        return Some(format!("providers.{pname}.models.{id}"));
                    }
                }
            }
            let mut po = pv.clone();
            if let Some(o) = po.as_object_mut() {
                o.remove("models");
            }
            if serde_json::from_value::<ProviderConfig>(po).is_err() {
                return Some(format!("providers.{pname}"));
            }
        }
    }
    if let Some(r) = top.get("review") {
        if serde_json::from_value::<ReviewConfig>(r.clone()).is_err() {
            return Some("review".into());
        }
    }
    None
}

/// 配置根：`$REDTEAM_HOME`（非空）否则 `$HOME/.redteam`；两者皆缺 → 错误（#1：不退化为相对路径，防把项目层文件当用户配置读）。
pub fn redteam_home() -> anyhow::Result<PathBuf> {
    if let Some(v) = std::env::var_os("REDTEAM_HOME").filter(|v| !v.is_empty()) {
        let p = PathBuf::from(v);
        if !p.is_absolute() {
            anyhow::bail!("REDTEAM_HOME 须为绝对路径（当前 {}）——相对路径会随 CWD 把仓内文件当用户配置读（#1，评审 S8）", p.display());
        }
        return Ok(p);
    }
    if let Some(h) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(h).join(".redteam"));
    }
    anyhow::bail!("REDTEAM_HOME 与 HOME 皆未设置，无法定位配置目录（~/.redteam/）")
}

pub fn config_path() -> anyhow::Result<PathBuf> {
    Ok(redteam_home()?.join("config.json"))
}

/// 文件是否只有空白与 `#` 注释（旧 init 写过的纯注释 TOML 模板不算旧配置，#9）。
fn comment_only(p: &Path) -> bool {
    match std::fs::read_to_string(p) {
        Ok(t) => t.lines().all(|l| {
            let s = l.trim();
            s.is_empty() || s.starts_with('#')
        }),
        Err(_) => false,
    }
}

/// 旧路径迁移 notice（`model list` / `model check` 专用，2026-10-04 对账从 `Config::load` 收窄而来）。
///
/// 新文件已在场即视为已迁移、不提示。放在这两条命令而不是每次加载：迁移是一次性动作，
/// 而 dev 二进制的旧文件在并存期长期存在——挂在加载链上等于对只用 env 存 key 的用户永久刷屏。
pub fn legacy_migration_notices() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(path) = config_path() {
        if !path.is_file() {
            for p in legacy_config_paths_present() {
                out.push(format!(
                    "notice: 配置已改读 {}（REDTEAM_HOME 可覆盖，严格 JSON）；旧文件 {} 未读，请手动迁移（redteam model setup --help）",
                    path.display(),
                    p.display()
                ));
            }
        }
    }
    if let Ok(spath) = secrets::secrets_path() {
        if !spath.is_file() {
            for p in legacy_secrets_paths_present() {
                out.push(format!(
                    "notice: 密钥已改读 {}（严格 JSON）；旧文件 {} 未读——用 `redteam model setup … --key-store secrets [--model-key]` 重写或手动迁移",
                    spath.display(),
                    p.display()
                ));
            }
        }
    }
    out
}

/// 旧路径探测（只提示不读）：`$XDG_CONFIG_HOME/redteam/config.toml`、`~/.config/redteam/config.toml`（09-04 前用户层）、
/// `./.redteam/config.toml`（项目层）、`$REDTEAM_HOME/config.toml`（批 A 首版 TOML）。纯注释文件不算；canonical 去重。
pub fn legacy_config_paths_present() -> Vec<PathBuf> {
    legacy_paths_present("config.toml")
}

/// dev 版 / 首版 `secrets.toml`：key 不会被读，只提示（评审 S7）。
pub fn legacy_secrets_paths_present() -> Vec<PathBuf> {
    legacy_paths_present("secrets.toml")
}

fn legacy_paths_present(file: &str) -> Vec<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        cands.push(PathBuf::from(x).join("redteam").join(file));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from) {
        cands.push(home.join(".config").join("redteam").join(file));
    }
    cands.push(PathBuf::from(".redteam").join(file));
    if let Ok(h) = redteam_home() {
        cands.push(h.join(file));
    }
    let mut out: Vec<PathBuf> = Vec::new();
    for p in cands {
        if !p.is_file() || comment_only(&p) {
            continue;
        }
        let canon = p.canonicalize().ok();
        if out.iter().any(|o| o.canonicalize().ok() == canon) {
            continue;
        }
        out.push(p);
    }
    out
}

/// `model setup` 的显式字段补丁：三层各自只合并给出的字段（D12 定案 5，评审 #2 #3）。
#[derive(Clone, Debug)]
pub struct SetupPatch {
    pub provider: String,
    pub model_id: String,
    pub roles: Vec<Role>,
    pub display_name: Option<String>,
    /// 显式 `--kind`：新建 provider → provider 层；provider 已存在 → 模型层（不动共享的 provider 层，评审 #5）。
    pub kind: Option<ProviderKind>,
    pub base_url: Option<String>,
    /// 显式 `--api-key-env`：落点规则同 `kind`。
    pub api_key_env: Option<String>,
    pub max_concurrency: Option<u32>,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u64>,
    pub per_turn_timeout_secs: Option<u64>,
    pub reasoning: Option<bool>,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 新建 provider 且未显式 `--kind` 时的推断值（已存在的 provider 不动）。
    pub kind_if_new: ProviderKind,
    /// 新建 provider 且未显式 `--api-key-env` 时的推断值。
    pub api_key_env_if_new: Option<String>,
}

impl Default for SetupPatch {
    fn default() -> Self {
        SetupPatch {
            provider: String::new(), model_id: String::new(), roles: Vec::new(), display_name: None, kind: None, base_url: None,
            api_key_env: None, max_concurrency: None, context_window: None, max_output_tokens: None, per_turn_timeout_secs: None,
            reasoning: None, reasoning_effort: None, kind_if_new: ProviderKind::OpenaiCompatible, api_key_env_if_new: None,
        }
    }
}

/// 三层显式合并 → 完整解析 + 校验 → 原子落盘（pretty JSON）。现有文件解析失败拒写不覆写（评审 #2/#4/#8 同策略）。
pub fn write_setup(patch: &SetupPatch) -> anyhow::Result<PathBuf> {
    let path = config_path()?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => anyhow::bail!("读取 {} 失败：{e}", path.display()),
    };
    let mut root: serde_json::Value = if existing.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(&existing)
            .map_err(|e| anyhow::anyhow!("{} 解析失败（{e}）：不覆写现有配置，请先修复该文件再 setup", path.display()))?
    };
    let root_o = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("config.json 顶层不是对象"))?;
    let provs = root_o.entry("providers").or_insert_with(|| serde_json::json!({}));
    let provs_o = provs.as_object_mut().ok_or_else(|| anyhow::anyhow!("providers 不是对象"))?;
    let is_new_provider = !provs_o.contains_key(&patch.provider);
    let pv = provs_o.entry(patch.provider.clone()).or_insert_with(|| serde_json::json!({}));
    let po = pv.as_object_mut().ok_or_else(|| anyhow::anyhow!("providers.{} 不是对象", patch.provider))?;
    // provider 层：只写显式的 baseUrl / maxConcurrency；kind / apiKeyEnv 新建 provider 时写这里（显式或推断），
    // provider 已存在时改写模型层（不动共享的 provider 层，评审 #3 / #5）。
    let (model_kind, model_api_key_env) = if is_new_provider {
        po.insert("kind".into(), serde_json::json!(patch.kind.unwrap_or(patch.kind_if_new).as_str()));
        if let Some(e) = patch.api_key_env.as_ref().or(patch.api_key_env_if_new.as_ref()) {
            po.insert("apiKeyEnv".into(), serde_json::json!(e));
        }
        (None, None)
    } else {
        (patch.kind, patch.api_key_env.clone())
    };
    if let Some(b) = &patch.base_url {
        po.insert("baseUrl".into(), serde_json::json!(b));
    }
    if let Some(c) = patch.max_concurrency {
        po.insert("maxConcurrency".into(), serde_json::json!(c));
    }
    // 模型层：只写显式
    let models = po.entry("models").or_insert_with(|| serde_json::json!({}));
    let models_o = models.as_object_mut().ok_or_else(|| anyhow::anyhow!("providers.{}.models 不是对象", patch.provider))?;
    let mv = models_o.entry(patch.model_id.clone()).or_insert_with(|| serde_json::json!({}));
    let mo = mv.as_object_mut().ok_or_else(|| anyhow::anyhow!("models.{} 不是对象", patch.model_id))?;
    if let Some(n) = &patch.display_name {
        mo.insert("displayName".into(), serde_json::json!(n));
    }
    if let Some(k) = model_kind {
        mo.insert("kind".into(), serde_json::json!(k.as_str()));
    }
    if let Some(e) = &model_api_key_env {
        mo.insert("apiKeyEnv".into(), serde_json::json!(e));
    }
    if let Some(s) = patch.per_turn_timeout_secs {
        mo.insert("perTurnTimeoutSecs".into(), serde_json::json!(s));
    }
    if let Some(c) = patch.context_window {
        mo.insert("contextWindow".into(), serde_json::json!(c));
    }
    if let Some(t) = patch.max_output_tokens {
        mo.insert("maxOutputTokens".into(), serde_json::json!(t));
    }
    if let Some(r) = patch.reasoning {
        mo.insert("reasoning".into(), serde_json::json!(r));
    }
    // 角色层：不存在建 {}；reasoningEffort 显式才写
    let roles = mo.entry("roles").or_insert_with(|| serde_json::json!({}));
    let roles_o = roles.as_object_mut().ok_or_else(|| anyhow::anyhow!("models.{}.roles 不是对象", patch.model_id))?;
    for role in &patch.roles {
        let rv = roles_o.entry(role.as_str().to_string()).or_insert_with(|| serde_json::json!({}));
        if let Some(effort) = patch.reasoning_effort {
            if let Some(ro) = rv.as_object_mut() {
                ro.insert("reasoningEffort".into(), serde_json::json!(effort.as_str()));
            }
        }
    }
    // 写前完整校验（#4）
    let text = serde_json::to_string_pretty(&root)?;
    Config::from_json(&text).map_err(|e| anyhow::anyhow!("合并后配置无效，未写入：{e}"))?;
    write_atomic(&path, &(text + "\n"), None)?;
    Ok(path)
}

/// 原子落盘（评审 #6）：同目录临时文件 + rename；`mode` 给定时临时文件即以该权限创建（secrets 不经 umask 明文窗口）。
pub(crate) fn write_atomic(path: &std::path::Path, text: &str, mode: Option<u32>) -> anyhow::Result<()> {
    let dir = path.parent().ok_or_else(|| anyhow::anyhow!("{} 无父目录", path.display()))?;
    std::fs::create_dir_all(dir)?;
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("config");
    let tmp = dir.join(format!(".{stem}.tmp-{}", std::process::id()));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if let Some(m) = mode {
                opts.mode(m);
            }
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(m) = mode {
            // 临时文件若已存在（上次中断残留），open 的 mode 不生效——补一次
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(m))?;
        }
    }
    #[cfg(not(unix))]
    let _ = mode;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 测试专用：跨 config 子模块串行化 REDTEAM_HOME / HOME 等环境变量写入，防并行测试污染。
#[cfg(test)]
pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    /// design §12.1 样例。
    const SAMPLE: &str = r#"{
  "providers": {
    "deepseek": {
      "kind": "openai", "baseUrl": "https://api.deepseek.com", "apiKeyEnv": "DEEPSEEK_API_KEY", "maxConcurrency": 20,
      "models": {
        "deepseek-v4-flash": { "displayName": "DeepSeek V4 Flash", "contextWindow": 1000000, "maxOutputTokens": 384000, "reasoning": true,
                               "roles": { "scanner": { "reasoningEffort": "low" } } },
        "deepseek-v4-pro":   { "contextWindow": 1000000, "maxOutputTokens": 384000, "reasoning": true, "perTurnTimeoutSecs": 1200,
                               "roles": { "reviewer": { "reasoningEffort": "high" } } }
      }
    },
    "agct": {
      "baseUrl": "https://api.aigocode.team/v1", "maxConcurrency": 20,
      "models": {
        "gemini-3.8-flash": { "kind": "openai", "apiKeyEnv": "AGCT_GEMINI_API_KEY", "family": "gemini",
                              "contextWindow": 1048576, "maxOutputTokens": 65536, "reasoning": true,
                              "roles": { "scanner": { "reasoningEffort": "low" }, "verifier": { "reasoningEffort": "high" } } }
      }
    }
  },
  "review": {
    "blockThreshold": "medium", "confFloor": 0.5, "maxConcurrency": 10,
    "scanners": { "samples": 1 },
    "reviewers": { "maxCells": "auto", "reenqueue": false },
    "verifiers": { "enabled": true },
    "grouping": { "minFiles": 4, "bundleLineThreshold": 200, "maxFilesPerGroup": 10, "semantic": false },
    "pack": { "body": "slice", "anchorWindow": 30, "findingWindow": 30, "findingWindowEnforce": false, "sliceMinBytes": 8192 }
  }
}"#;

    /// 批 D-4：`review.verifiers.enabled` **缺省关**。这是本批唯一的产品面开关，
    /// 离线基线两臂的误驳率（12% / 20%）都远超预登记的 5% 门槛——翻默认值必须是有意动作，
    /// 且「写了空 `verifiers` 段」与「整段缺失」必须同一个值（serde default 与 `Default` 不许各说各话）。
    #[test]
    fn verifiers_enabled_defaults_off() {
        assert!(!ReviewConfig::default().verifiers.enabled, "整段缺失 → 关");
        let empty: Config = serde_json::from_str(r#"{"review":{"verifiers":{}}}"#).unwrap();
        assert!(!empty.review.verifiers.enabled, "空 verifiers 段 → 同样关");
        let on: Config = serde_json::from_str(r#"{"review":{"verifiers":{"enabled":true}}}"#).unwrap();
        assert!(on.review.verifiers.enabled, "显式 true 照样开");
    }

    #[test]
    fn sample_config_parses_and_active_roles_ordered() {
        let cfg = Config::from_json(SAMPLE).unwrap();
        let roles = cfg.active_roles();
        // 顺序：provider 名字典序（agct < deepseek），再 id
        assert_eq!(roles.scanners.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["agct/gemini-3.8-flash", "deepseek/deepseek-v4-flash"]);
        assert_eq!(roles.reviewers.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["deepseek/deepseek-v4-pro"]);
        assert_eq!(roles.verifiers.len(), 1);
        let gem = &roles.scanners[0];
        assert_eq!(gem.kind, ProviderKind::Openai, "模型层 kind 覆盖 provider 层缺失");
        assert_eq!(gem.family, "gemini");
        assert_eq!(gem.reasoning_effort, Some(ReasoningEffort::Low));
        assert_eq!(roles.verifiers[0].reasoning_effort, Some(ReasoningEffort::High), "同一模型不同角色不同档");
        let pro = &roles.reviewers[0];
        assert_eq!(pro.per_turn_timeout_secs, Some(1200));
        assert_eq!(pro.role, Role::Reviewer);
        assert_eq!(cfg.review.reviewers.max_cells, MaxCells::Auto);
        assert_eq!(cfg.review.pack.body, PackBody::Slice);
        assert_eq!(cfg.providers["deepseek"].max_concurrency, Some(20));
        assert_eq!(roles.id_of("deepseek/deepseek-v4-flash"), Some("deepseek-v4-flash"));
        assert_eq!(roles.scanners[1].display_name.as_deref(), Some("DeepSeek V4 Flash"), "displayName 只作显示");
        assert_eq!(cfg.find("deepseek/deepseek-v4-flash"), Some(("deepseek".into(), "deepseek-v4-flash".into())));
        assert_eq!(cfg.find("DeepSeek V4 Flash"), None, "显示名不是身份串（D12 定案 3）");
    }

    #[test]
    fn defaults_identity_family_window() {
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"glm-5.3":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let em = &cfg.active_roles().scanners[0];
        assert_eq!(em.name, "p/glm-5.3");
        assert_eq!(em.family, "glm");
        assert_eq!(em.context_window, 128_000);
        assert_eq!(em.reasoning_effort, None);
        assert_eq!(cfg.review.pack.body, PackBody::Slice, "D8：slice 默认");
        assert_eq!(cfg.review.max_concurrency, 10, "D13：缺省 10，provider 门是唯一闸");
        assert!(Config::from_json("  \n").unwrap().providers.is_empty(), "空白 = 空配置");
    }

    /// `findingWindow` 的 0 是**合法的「关掉观测」**（装包半径 `anchorWindow` 的 0 才非法）——
    /// 两个键语义相反，连校验都不该共用（2026-09-29 `/code-review` 抓到：文档承诺的退路设不上）。
    #[test]
    fn finding_window_zero_is_allowed_but_anchor_window_zero_is_not() {
        let c = Config::from_json(r#"{"review":{"pack":{"findingWindow":0}}}"#).unwrap();
        assert_eq!(c.review.pack.finding_window, 0, "0 = 关掉窗口后滤，要设得上");
        let err = Config::from_json(r#"{"review":{"pack":{"anchorWindow":0}}}"#).unwrap_err().to_string();
        assert!(err.contains("review.pack.anchorWindow 必须 ≥ 1"), "装包半径的 0 仍非法: {err}");
    }

    #[test]
    fn validate_aggregates_kind_window_and_zero_knobs() {
        let err = Config::from_json(r#"{"providers":{
            "p":{"models":{"a":{"roles":{"scanner":{}}}}},
            "q":{"kind":"openai","maxConcurrency":0,"models":{"b":{"contextWindow":128,"roles":{"scanner":{}}},
                 "c":{"maxOutputTokens":0,"perTurnTimeoutSecs":0,"roles":{"scanner":{}}}}}}}"#)
            .unwrap_err().to_string();
        assert!(err.contains("providers.p.models.a: 缺 kind"), "{err}");
        assert!(err.contains("contextWindow 128 低于下限"), "{err}");
        assert!(err.contains("models.c: maxOutputTokens 必须 ≥ 1") && err.contains("models.c: perTurnTimeoutSecs 必须 ≥ 1"), "评审 #3: {err}");
        assert!(err.contains("providers.q.maxConcurrency 必须 ≥ 1"), "{err}");
        let cf = Config::from_json(r#"{"review":{"confFloor":5}}"#).unwrap_err().to_string();
        assert!(cf.contains("review.confFloor"), "配置层 confFloor 越界（评审 S2）: {cf}");
        let nm = Config::from_json(r#"{"providers":{"a/b":{"kind":"openai","models":{"":{"roles":{"scanner":{}}}}}}}"#).unwrap_err().to_string();
        assert!(nm.contains("provider 名") && nm.contains("空 model id"), "空名 / 含 '/' 的 provider 名拒绝（评审 N6）: {nm}");
        let zz = Config::from_json(r#"{"review":{"pack":{"anchorWindow":0},"grouping":{"minFiles":0,"maxFilesPerGroup":0},"reviewers":{"maxCells":0}}}"#).unwrap_err().to_string();
        for k in ["review.pack.anchorWindow", "review.grouping.minFiles", "review.grouping.maxFilesPerGroup", "review.reviewers.maxCells"] {
            assert!(zz.contains(k), "{k} 零值须拦（修补批 X-4）: {zz}");
        }
        assert!(Config::from_json(r#"{"review":{"pack":{"sliceMinBytes":0},"reviewers":{"maxCells":"auto"}}}"#).is_ok(), "sliceMinBytes 0 与 auto 合法");
        let z = Config::from_json(r#"{"review":{"scanners":{"samples":0}}}"#).unwrap_err().to_string();
        assert!(z.contains("review.scanners.samples"), "{z}");
    }

    /// D13：三个角色 `workers` 键已删——旧配置解析成功、专属 warning（去哪调）、不叠加通用「无定义」；maxConcurrency > 128 只警不拒。
    #[test]
    fn removed_workers_keys_and_huge_max_concurrency_warn() {
        let (cfg, w) = Config::from_json_warned(r#"{"providers":{"p":{"kind":"openai","maxConcurrency":65536,"models":{"m":{"roles":{"scanner":{}}}}}},
            "review":{"maxConcurrency":384000,"scanners":{"workers":4,"samples":1},"reviewers":{"workers":2},"verifiers":{"workers":2,"enabled":true}}}"#).unwrap();
        for sec in ["scanners", "reviewers", "verifiers"] {
            assert!(w.iter().any(|x| x.contains(&format!("review.{sec}.workers 已删除（D13")) && x.contains("review.maxConcurrency = 10")), "{sec}: {w:?}");
        }
        assert!(!w.iter().any(|x| x.contains("workers 无定义")), "专属提示不叠加通用 warning: {w:?}");
        assert!(w.iter().any(|x| x.contains("providers.p.maxConcurrency = 65536 像是填错字段")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("review.maxConcurrency = 384000 像是填错字段")), "{w:?}");
        assert_eq!((cfg.providers["p"].max_concurrency, cfg.review.max_concurrency, cfg.review.scanners.samples), (Some(65536), 384000, 1), "只警不拒");
        let (_, w128) = Config::from_json_warned(r#"{"review":{"maxConcurrency":128}}"#).unwrap();
        assert!(w128.iter().all(|x| !x.contains("像是填错")), "128 是边界，不警: {w128:?}");
    }

    #[test]
    fn legacy_shapes_point_to_d12() {
        let e1 = Config::from_json(r#"{"scanners":[{"provider":"p","model_id":"m"}]}"#).unwrap_err().to_string();
        assert!(e1.contains("顶层 \"scanners\"") && e1.contains("D12"), "{e1}");
        let e2 = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":[{"id":"m"}]}}}"#).unwrap_err().to_string();
        assert!(e2.contains("models 是数组"), "{e2}");
        let e3 = Config::from_json(r#"{"providers":{"p":{"kind":"openai","base_url":"x","models":{}}}}"#).unwrap_err().to_string();
        assert!(e3.contains("camelCase") && e3.contains("base_url"), "{e3}");
        let e4 = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"context_window":1}}}}}"#).unwrap_err().to_string();
        assert!(e4.contains("context_window"), "{e4}");
        let e5 = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{"reasoning_effort":"high"}}}}}},"review":{"reviewers":{"max_cells":4}}}"#).unwrap_err().to_string();
        assert!(e5.contains("roles.scanner.reasoning_effort") && e5.contains("review.reviewers.max_cells"), "嵌套段 snake_case 也拦（评审 #4）: {e5}");
        let e6 = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"name":"M","roles":{"scanner":{}}}}}}}"#).unwrap_err().to_string();
        assert!(e6.contains("name 已改名 displayName"), "{e6}");
        let e7 = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"judge":{}}}}}}}"#).unwrap_err().to_string();
        assert!(e7.contains("roles.judge") && e7.contains("scanner | reviewer | verifier"), "{e7}");
        let e8 = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{"m":{"contextWindow":"1M","roles":{"scanner":{}}}}}}}"#).unwrap_err().to_string();
        assert!(e8.contains("providers.p.models.m") && e8.contains("invalid type"), "形状错误带字段路径（评审 S3）: {e8}");
        let e9 = Config::from_json(r#"{"review":{"tools":"maybe"}}"#).unwrap_err().to_string();
        assert!(e9.contains("review：") || e9.contains("review:"), "{e9}");
        let bad = Config::from_json(r#"{"providers":{"p":{"kind":"openai",}}}"#).unwrap_err().to_string();
        assert!(bad.contains("line"), "解析错误须带行列: {bad}");
    }

    #[test]
    fn review_risk_and_roleless_model_warn() {
        let (cfg, warns) = Config::from_json_warned(r#"{"providers":{"p":{"kind":"openai","models":{"m":{}}}},"review":{"risk":{"mode":"auto"}}}"#).unwrap();
        assert!(warns.iter().any(|w| w.contains("review.risk")), "{warns:?}");
        assert!(warns.iter().any(|w| w.contains("未声明任何 roles")), "{warns:?}");
        assert!(cfg.active_roles().scanners.is_empty());
        // 已知层级下的无定义键 → warning 不静默（评审 #4）
        let (_, w2) = Config::from_json_warned(r#"{"providers":{"p":{"kind":"openai","extra":1,"models":{"m":{"contextWindows":1,"roles":{"scanner":{"depth":2}}}}}},"review":{"grouping":{"minFile":1}},"foo":{}}"#).unwrap();
        for k in ["providers.p.extra", "providers.p.models.m.contextWindows", "providers.p.models.m.roles.scanner.depth", "review.grouping.minFile", "(顶层).foo"] {
            assert!(w2.iter().any(|w| w.contains(k) && w.contains("无定义")), "{k}: {w2:?}");
        }
    }

    #[test]
    fn max_cells_two_shapes_and_tools_mode() {
        let a = Config::from_json(r#"{"review":{"reviewers":{"maxCells":"auto"}}}"#).unwrap();
        assert_eq!(a.review.reviewers.max_cells, MaxCells::Auto);
        let n = Config::from_json(r#"{"review":{"reviewers":{"maxCells":12}}}"#).unwrap();
        assert_eq!(n.review.reviewers.max_cells, MaxCells::Fixed(12));
        assert!(Config::from_json(r#"{"review":{"reviewers":{"maxCells":"lots"}}}"#).is_err());
        let c = Config::from_json(r#"{"review":{"tools":"off"}}"#).unwrap();
        assert_eq!(c.review.tools, ToolsMode::Off);
        assert_eq!(Config::from_json("{}").unwrap().review.tools, ToolsMode::TwoStage);
        assert_eq!(ToolsMode::from_flag("bogus"), None);
    }

    #[test]
    fn key_resolution_chain_four_levels() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe {
            std::env::set_var("REDTEAM_TEST_KEY", "from-model-env");
            std::env::set_var("REDTEAM_TEST_PKEY", "from-provider-env");
        }
        let cfg = Config::from_json(r#"{"providers":{"xp":{"kind":"openai","apiKeyEnv":"REDTEAM_TEST_PKEY","models":{
            "m":{"apiKeyEnv":"REDTEAM_TEST_KEY","roles":{"scanner":{}}},"m2":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let sec = secrets::Secrets::parse(r#"{"providers":{"xp":{"apiKey":"prov-secret","models":{"m2":"model-secret"}}}}"#);
        assert_eq!(cfg.resolve_key_with("xp", "m", &sec).as_deref(), Some("from-model-env"));
        unsafe { std::env::set_var("REDTEAM_TEST_KEY", "  "); }
        assert_eq!(cfg.resolve_key_with("xp", "m", &sec).as_deref(), Some("from-provider-env"), "空白 env 视为未设，继续下一级（修补批 X-4）");
        unsafe { std::env::set_var("REDTEAM_TEST_KEY", "from-model-env"); }
        assert_eq!(cfg.resolve_key_with("xp", "m2", &sec).as_deref(), Some("model-secret"));
        let sec2 = secrets::Secrets::parse(r#"{"providers":{"xp":{"apiKey":"prov-secret"}}}"#);
        assert_eq!(cfg.resolve_key_with("xp", "m2", &sec2).as_deref(), Some("from-provider-env"));
        unsafe { std::env::remove_var("REDTEAM_TEST_PKEY"); }
        assert_eq!(cfg.resolve_key_with("xp", "m2", &sec2).as_deref(), Some("prov-secret"));
        assert_eq!(cfg.resolve_key_with("xp", "m2", &secrets::Secrets::default()), None);
        assert_eq!(cfg.resolve_key_with("nope", "m", &sec), None);
        unsafe { std::env::remove_var("REDTEAM_TEST_KEY"); }
    }

    #[test]
    fn secret_warnings_unknown_provider_and_model() {
        let cfg = Config::from_json(r#"{"providers":{"xp":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let sec = secrets::Secrets::parse(r#"{"providers":{"xp":{"models":{"m":"k","typo":"k2"}},"XP":{"apiKey":"k3"}}}"#);
        let w = cfg.secret_key_warnings(&sec);
        assert_eq!(w.len(), 2, "{w:?}");
        assert!(w.iter().any(|x| x.contains("typo")) && w.iter().any(|x| x.contains("providers.XP")), "{w:?}");
    }

    #[test]
    fn per_turn_priority_four_levels() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe {
            std::env::remove_var("REDTEAM_PER_TURN_SECS");
            std::env::remove_var("REDTEAM_MAX_TOKENS");
        }
        let cfg = Config::from_json(r#"{"providers":{"p":{"kind":"openai","models":{
            "a":{"perTurnTimeoutSecs":77,"maxOutputTokens":384000,"roles":{"scanner":{}}},
            "b":{"maxOutputTokens":384000,"roles":{"scanner":{}}},
            "c":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        let r = cfg.active_roles();
        let (a, b, c) = (&r.scanners[0], &r.scanners[1], &r.scanners[2]);
        assert_eq!(a.per_turn_secs(), 77, "模型显式值最高");
        assert_eq!(b.per_turn_secs(), 384_000 / 20 + 60, "公式用 maxOutputTokens");
        assert_eq!(c.per_turn_secs(), 32_768 / 20 + 60, "未声明 → 32768 缺省进公式");
        unsafe { std::env::set_var("REDTEAM_MAX_TOKENS", "8192"); }
        assert_eq!(c.per_turn_secs(), 8192 / 20 + 60, "公式用实际生效的 max_tokens");
        unsafe { std::env::set_var("REDTEAM_PER_TURN_SECS", "600"); }
        assert_eq!(b.per_turn_secs(), 600, "env 覆盖公式（#6）");
        assert_eq!(a.per_turn_secs(), 77, "env 不覆盖模型显式值");
        unsafe {
            std::env::remove_var("REDTEAM_PER_TURN_SECS");
            std::env::remove_var("REDTEAM_MAX_TOKENS");
        }
    }

    #[test]
    fn home_resolution_and_missing_home_errors() {
        let _g = ENV_LOCK.lock().unwrap();
        let saved_home = std::env::var_os("HOME");
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("REDTEAM_HOME", ".redteam"); }
        assert!(redteam_home().unwrap_err().to_string().contains("绝对路径"), "相对 REDTEAM_HOME 拒绝（评审 S8）");
        unsafe { std::env::set_var("REDTEAM_HOME", "/tmp/rt-home-x"); }
        assert_eq!(config_path().unwrap(), PathBuf::from("/tmp/rt-home-x/config.json"));
        assert_eq!(secrets::secrets_path().unwrap(), PathBuf::from("/tmp/rt-home-x/secrets.json"));
        unsafe { std::env::remove_var("REDTEAM_HOME"); std::env::set_var("HOME", "/tmp/rt-home-h"); }
        assert_eq!(redteam_home().unwrap(), PathBuf::from("/tmp/rt-home-h/.redteam"), "无 REDTEAM_HOME → HOME/.redteam（不依赖真实 HOME）");
        unsafe { std::env::remove_var("HOME"); }
        let err = redteam_home().unwrap_err().to_string();
        assert!(err.contains("REDTEAM_HOME 与 HOME"), "皆缺 → 报错而非相对路径（#1）: {err}");
        assert!(Config::load().is_err());
        unsafe {
            if let Some(h) = saved_home { std::env::set_var("HOME", h); }
        }
    }

    /// 修补批 X-5：`--pack-only` 路径不读 secrets.json（坏 secrets 不产 warning，配置照读）。
    #[test]
    fn load_no_secrets_skips_secrets_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("REDTEAM_HOME", dir.path()); }
        std::fs::write(dir.path().join("config.json"), r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#).unwrap();
        std::fs::write(dir.path().join("secrets.json"), "{\"providers\": {").unwrap();
        let (_, with) = Config::load_with_messages().unwrap();
        assert!(with.iter().any(|m| m.contains("secrets.json")), "正常加载报 secrets 解析失败: {with:?}");
        let (cfg, without) = Config::load_no_secrets().unwrap();
        assert!(without.iter().all(|m| !m.contains("secrets.json")), "不读 secrets: {without:?}");
        assert_eq!(cfg.active_roles().scanners.len(), 1);
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn load_propagates_io_errors_and_ignores_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("REDTEAM_HOME", dir.path()); }
        assert!(Config::load().unwrap().providers.is_empty(), "文件不存在 = 空配置");
        std::fs::create_dir_all(dir.path().join("config.json")).unwrap(); // 目录冒充文件 → EISDIR
        let err = Config::load().unwrap_err().to_string();
        assert!(err.contains("读取"), "io 错误不得吞成空配置（#13）: {err}");
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    /// 旧路径迁移 notice **不再挂在每次配置加载上**（2026-10-04 对账收窄）。
    ///
    /// 债的原话：「只用 env 存 key 的用户永远看到」——dev 二进制的旧文件在并存期长期存在，
    /// 而配置每条命令都加载，于是每次调用都刷同样两行。迁移是一次性动作，
    /// 提示该属于「查配置」的那两条命令（`model list` / `model check` 调
    /// `legacy_migration_notices()`）。
    ///
    /// 摘掉那两条时全量测试**照绿**——原行为没有任何用例盯着，这条补上。
    #[test]
    fn legacy_notices_moved_off_the_load_path() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        // 新配置不在场 + 旧 TOML 在场 = 该提示的那种局面
        std::fs::write(home.join("config.toml"), "[review]\n").unwrap();
        std::fs::write(home.join("secrets.toml"), "[providers.p]\nx=\"y\"\n").unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe {
            std::env::set_var("REDTEAM_HOME", &home);
            std::env::remove_var("XDG_CONFIG_HOME");
        }

        // 只断言本用例造的那两个文件：真机上 `~/.config/redteam/` 往往也在（dev 二进制的旧配置），
        // 那正是这条债描述的现实局面——数总数会把用例绑在开发机环境上。
        let notices = legacy_migration_notices();
        let mine = |suffix: &str| {
            notices.iter().any(|m| m.contains(&home.join(suffix).display().to_string()))
        };
        assert!(mine("config.toml"), "旧 config.toml 该进 notice: {notices:?}");
        assert!(mine("secrets.toml"), "旧 secrets.toml 该进 notice: {notices:?}");
        assert!(notices.iter().any(|m| m.contains("配置已改读")), "{notices:?}");
        assert!(notices.iter().any(|m| m.contains("密钥已改读")), "{notices:?}");

        let (_, messages) = Config::load_with_messages().unwrap();
        assert!(
            !messages.iter().any(|m| m.contains("已改读")),
            "加载路径不该再带迁移 notice（每条命令都会刷）: {messages:?}"
        );
    }

    #[test]
    fn legacy_detection_skips_comment_only_and_dedups() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let xdg = dir.path().join("xdg");
        std::fs::create_dir_all(xdg.join("redteam")).unwrap();
        std::fs::write(xdg.join("redteam").join("config.toml"), "# only comments\n\n# more\n").unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", &xdg);
            std::env::set_var("REDTEAM_HOME", dir.path().join("home"));
        }
        assert!(legacy_config_paths_present().iter().all(|p| !p.starts_with(&xdg)), "纯注释模板不提示（#9）");
        std::fs::write(xdg.join("redteam").join("config.toml"), "[providers.p]\nkind=\"openai\"\n").unwrap();
        assert!(legacy_config_paths_present().iter().any(|p| p.starts_with(&xdg)), "真实旧配置提示");
        std::fs::create_dir_all(dir.path().join("home")).unwrap();
        std::fs::write(dir.path().join("home").join("config.toml"), "[review]\n").unwrap();
        assert!(legacy_config_paths_present().iter().any(|p| p.ends_with("home/config.toml")), "首版 TOML 也提示");
        std::fs::write(dir.path().join("home").join("secrets.toml"), "[providers.p]\nx=\"y\"\n").unwrap();
        assert!(legacy_secrets_paths_present().iter().any(|p| p.ends_with("home/secrets.toml")), "旧 secrets.toml 也提示（评审 S7）");
        assert!(legacy_secrets_paths_present().iter().all(|p| !p.ends_with("config.toml")));
        unsafe {
            std::env::remove_var("XDG_CONFIG_HOME");
            std::env::remove_var("REDTEAM_HOME");
        }
    }

    fn patch(provider: &str, id: &str, roles: &[Role]) -> SetupPatch {
        SetupPatch {
            provider: provider.into(), model_id: id.into(), roles: roles.to_vec(),
            kind_if_new: ProviderKind::OpenaiCompatible, api_key_env_if_new: Some("P_API_KEY".into()),
            ..Default::default()
        }
    }

    #[test]
    fn write_setup_merges_explicit_fields_only_and_keeps_handwritten() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("REDTEAM_HOME", dir.path()); }
        // 手写：provider anthropic + 模型级字段
        std::fs::write(dir.path().join("config.json"), r#"{"providers":{"agc":{"kind":"anthropic","apiKeyEnv":"MY_KEY","models":{
            "glm-5.3":{"displayName":"GLM","family":"fam","perTurnTimeoutSecs":999,"apiKeyEnv":"GLM_KEY","roles":{"scanner":{"reasoningEffort":"low"}}}}}},
            "review":{"blockThreshold":"high"}}"#).unwrap();
        // 已存在 provider：不带 --kind 且带 base_url 的 setup 不得改 kind / apiKeyEnv（#3）；模型层只加 contextWindow；追加 reviewer 角色
        let mut p = patch("agc", "glm-5.3", &[Role::Reviewer]);
        p.base_url = Some("https://gw/v1".into());
        p.context_window = Some(200_000);
        p.reasoning_effort = Some(ReasoningEffort::High);
        write_setup(&p).unwrap();
        let cfg = Config::load().unwrap();
        let prov = &cfg.providers["agc"];
        assert_eq!(prov.kind, Some(ProviderKind::Anthropic), "已存在 provider 的 kind 不被覆写");
        assert_eq!(prov.api_key_env.as_deref(), Some("MY_KEY"));
        assert_eq!(prov.base_url.as_deref(), Some("https://gw/v1"), "显式字段写入");
        let m = &prov.models["glm-5.3"];
        assert_eq!(m.display_name.as_deref(), Some("GLM"), "手写 displayName 保留（#2）");
        assert_eq!(m.family.as_deref(), Some("fam"));
        assert_eq!(m.per_turn_timeout_secs, Some(999));
        assert_eq!(m.api_key_env.as_deref(), Some("GLM_KEY"));
        assert_eq!(m.context_window, Some(200_000));
        assert_eq!(m.roles[&Role::Scanner].reasoning_effort, Some(ReasoningEffort::Low), "原角色属性不动");
        assert_eq!(m.roles[&Role::Reviewer].reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(cfg.review.block_threshold, Severity::High, "review 段保留");
        // 新 provider：推断 kind / apiKeyEnv 补齐
        write_setup(&patch("newp", "m1", &[Role::Scanner])).unwrap();
        let cfg2 = Config::load().unwrap();
        assert_eq!(cfg2.providers["newp"].kind, Some(ProviderKind::OpenaiCompatible));
        assert_eq!(cfg2.providers["newp"].api_key_env.as_deref(), Some("P_API_KEY"));
        assert!(cfg2.providers["newp"].models["m1"].roles.contains_key(&Role::Scanner));
        // 已存在 provider 上显式 --kind / --api-key-env / --per-turn-timeout-secs → 写模型层，provider 层不动（评审 #5）
        let mut p2 = patch("agc", "k2", &[Role::Scanner]);
        p2.kind = Some(ProviderKind::Openai);
        p2.api_key_env = Some("K2_KEY".into());
        p2.per_turn_timeout_secs = Some(300);
        write_setup(&p2).unwrap();
        let cfg3 = Config::load().unwrap();
        assert_eq!(cfg3.providers["agc"].kind, Some(ProviderKind::Anthropic));
        assert_eq!(cfg3.providers["agc"].api_key_env.as_deref(), Some("MY_KEY"));
        let k2 = &cfg3.providers["agc"].models["k2"];
        assert_eq!((k2.kind, k2.api_key_env.as_deref(), k2.per_turn_timeout_secs), (Some(ProviderKind::Openai), Some("K2_KEY"), Some(300)));
        let raw = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(!raw.contains("sk-"), "config 永不含明文 key");
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains(".tmp-")), "原子写：无临时文件残留");
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn write_setup_refuses_invalid_merge_and_unparseable_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化
        unsafe { std::env::set_var("REDTEAM_HOME", dir.path()); }
        let good = r#"{"providers":{"p":{"kind":"openai","models":{"a":{"roles":{"scanner":{}}}}}}}"#;
        std::fs::write(dir.path().join("config.json"), good).unwrap();
        // 校验失败（contextWindow 过小）→ 拒写、文件不变（#4）
        let mut p = patch("p", "a", &[Role::Scanner]);
        p.context_window = Some(128);
        let err = write_setup(&p).unwrap_err().to_string();
        assert!(err.contains("未写入") && err.contains("contextWindow"), "{err}");
        assert_eq!(std::fs::read_to_string(dir.path().join("config.json")).unwrap(), good);
        // maxOutputTokens 0 → 拒写、文件不变（评审 #3）
        let mut q = patch("p", "b", &[Role::Scanner]);
        q.max_output_tokens = Some(0);
        assert!(write_setup(&q).unwrap_err().to_string().contains("maxOutputTokens 必须 ≥ 1"));
        assert_eq!(std::fs::read_to_string(dir.path().join("config.json")).unwrap(), good);
        // 现有文件解析失败 → 拒写（#2/#8 同策略）
        std::fs::write(dir.path().join("config.json"), "{\"providers\": {").unwrap();
        let err2 = write_setup(&patch("p", "a", &[Role::Scanner])).unwrap_err().to_string();
        assert!(err2.contains("不覆写"), "{err2}");
        assert_eq!(std::fs::read_to_string(dir.path().join("config.json")).unwrap(), "{\"providers\": {");
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }
}
