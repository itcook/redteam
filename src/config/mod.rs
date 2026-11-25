//! 用户级唯一配置（config v2，2026-11-25）：`$REDTEAM_HOME/config.json`（缺省 `~/.redteam/`）+ 同目录 `secrets.json`。
//!
//! v2 形状（破坏性，硬切无别名）：
//! - `providers.<name>`：通道层（`api` / `baseUrl` / 凭据）+ `models[]`（模型固有属性）。
//! - `settings`：怎么用——`language`、`review`（阈值/工具/角色池）、`triage.models[]`、`verify.models[]`。
//! - 角色池条目 `{provider,id,reasoningEffort?}`；顺序即轮转顺序，长度 1 合法。
//!
//! 严格 JSON、键 camelCase、无注释。v1 形状在 `shape_guard` 里报错并给逐字段迁移提示。

pub mod secrets;

use crate::domain::Severity;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 通道 API（v2 三值，**不接受别名**）。`openai-responses` 仅预留：配置期即拒绝。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderApi {
    OpenaiCompletions,
    AnthropicMessages,
    OpenaiResponses,
}

impl ProviderApi {
    pub fn parse(s: &str) -> Option<ProviderApi> {
        match s {
            "openai-completions" => Some(ProviderApi::OpenaiCompletions),
            "anthropic-messages" => Some(ProviderApi::AnthropicMessages),
            "openai-responses" => Some(ProviderApi::OpenaiResponses),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderApi::OpenaiCompletions => "openai-completions",
            ProviderApi::AnthropicMessages => "anthropic-messages",
            ProviderApi::OpenaiResponses => "openai-responses",
        }
    }
    /// 尚未实现的 API（配置期拒绝）。
    pub fn implemented(&self) -> bool {
        !matches!(self, ProviderApi::OpenaiResponses)
    }
}

/// 推理强度（角色属性：同一模型可在 scanner 位 low、reviewer 位 high）。backend 消费在 `provider::backends`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

impl ReasoningEffort {
    pub const ALL: [ReasoningEffort; 7] = [
        ReasoningEffort::Minimal,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
        ReasoningEffort::Xhigh,
        ReasoningEffort::Max,
        ReasoningEffort::Ultra,
    ];
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

/// 角色（v2 扩展 `triage`：triage 池独立 role_key，避免与 reviewer 同键覆盖）。
///
/// 五角色分三条流程：
/// - `scanner` / `reviewer` / `refuter` —— 评审管线（review）。
/// - `verifier` —— hibot `verify` kind 的修复验证（Phase 4；v2 起**不再回退 reviewer**）。
/// - `triage` —— hibot `triage` kind（2026-11-24 起不复用 reviewer）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Scanner,
    Reviewer,
    Refuter,
    Verifier,
    Triage,
}

impl Role {
    pub const ALL: [Role; 5] = [Role::Scanner, Role::Reviewer, Role::Refuter, Role::Verifier, Role::Triage];
    /// 评审管线（review）的封闭角色集：`CellOutput` / `coverage` / phase 只认这三个。
    pub const REVIEW_PIPELINE: [Role; 3] = [Role::Scanner, Role::Reviewer, Role::Refuter];
    pub fn parse(s: &str) -> Option<Role> {
        match s.trim() {
            "scanner" => Some(Role::Scanner),
            "reviewer" => Some(Role::Reviewer),
            "refuter" => Some(Role::Refuter),
            "verifier" => Some(Role::Verifier),
            "triage" => Some(Role::Triage),
            _ => None,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::Scanner => "scanner",
            Role::Reviewer => "reviewer",
            Role::Refuter => "refuter",
            Role::Verifier => "verifier",
            Role::Triage => "triage",
        }
    }
}

/// `reasoningEffort` 的模型级翻译表（`models[].thinkingLevelMap`）：我方档位 → provider 实际强度值。
/// 键缺失与 `null` 同义：省略该档位的整组推理参数（不保证上游默认推理关闭）；整键缺省 → 回退内置 family 表。
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingLevelMap {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub medium: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xhigh: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultra: Option<String>,
}

impl ThinkingLevelMap {
    /// `None` = 该档位抑制（null 或缺键）；`Some(v)` = 翻译为 `v`。
    pub fn value(&self, level: ReasoningEffort) -> Option<&str> {
        let slot = match level {
            ReasoningEffort::Minimal => &self.minimal,
            ReasoningEffort::Low => &self.low,
            ReasoningEffort::Medium => &self.medium,
            ReasoningEffort::High => &self.high,
            ReasoningEffort::Xhigh => &self.xhigh,
            ReasoningEffort::Max => &self.max,
            ReasoningEffort::Ultra => &self.ultra,
        };
        slot.as_deref()
    }
}

/// 模型（`providers.<name>.models[]`）：固有属性只写一次；角色由 `settings.*.models[]` 引用。
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelConfig {
    /// 模型 id（请求体里的 model；身份串 = `provider/id`）。
    pub id: String,
    /// 人读显示名（`model list` / 报告用）；**不是**身份串。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// 本地内联密钥（Q2 裁决；job 模式出现即 `secret_field`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// 模型级 key env（覆盖 provider.apiKeyEnv）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    /// 家族；缺省取 id 首段（deepseek / gemini / glm / kimi）。服务 agreement 归族与 reasoning 参数形状。
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
    /// 推理档位翻译表；缺省走内置 family 表。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<ThinkingLevelMap>,
}

/// `providers.<name>`：通道层。`maxConcurrency` = 该 provider 在途上限（缺省继承 `settings.review.maxConcurrency`）。
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    /// 通道 API（必填）。
    pub api: ProviderApi,
    /// 端点（必填；不再按 api 猜默认）。
    pub base_url: String,
    /// 本地内联密钥（Q2 裁决；job 模式出现即 `secret_field`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    #[serde(default)]
    pub models: Vec<ModelConfig>,
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
    pub api: ProviderApi,
    pub base_url: Option<String>,
    pub context_window: u32,
    pub max_output_tokens: Option<u64>,
    pub per_turn_timeout_secs: Option<u64>,
    pub reasoning: bool,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub thinking_level_map: Option<ThinkingLevelMap>,
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
                eprintln!("warning: REDTEAM_MAX_TOKENS={s:?} 非法（需正整数），回落默认 {DEFAULT_MAX_TOKENS}");
                DEFAULT_MAX_TOKENS
            }
        },
        Err(_) => DEFAULT_MAX_TOKENS,
    }
}

/// `REDTEAM_PER_TURN_SECS` env 解析（`per_turn_secs` 与 `provider::per_turn_deadline` 共用，评审 S6）。
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
    /// backend 实际发送的 max_tokens：模型 `maxOutputTokens` > `REDTEAM_MAX_TOKENS` env > 32768。
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

/// 评审工具档位（design 2026-07-13 §2.4）：off / two-stage（默认）/ on。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ToolsMode {
    Off,
    TwoStage,
    On,
}

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
fn default_tools() -> ToolsMode {
    ToolsMode::TwoStage
}

/// `settings.review.reviewers.maxCells`：`"auto"`（f(风险分, 规模)，D11 三档 4 / 12 / 32 帽 48）或整数固定上限。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaxCells {
    Auto,
    Fixed(u32),
}

impl<'de> Deserialize<'de> for MaxCells {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            N(u32),
            S(String),
        }
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

/// `"auto" | N(≥1)`：手写 JSON Schema（schemars 的 untagged 表达不了这个枚举）。
impl JsonSchema for MaxCells {
    fn schema_name() -> String {
        "MaxCells".into()
    }
    fn json_schema(_: &mut schemars::gen::SchemaGenerator) -> schemars::schema::Schema {
        use schemars::schema::*;
        Schema::Object(SchemaObject {
            subschemas: Some(Box::new(SubschemaValidation {
                one_of: Some(vec![
                    Schema::Object(SchemaObject {
                        instance_type: Some(InstanceType::String.into()),
                        enum_values: Some(vec![serde_json::json!("auto")]),
                        ..Default::default()
                    }),
                    Schema::Object(SchemaObject {
                        instance_type: Some(InstanceType::Integer.into()),
                        number: Some(Box::new(NumberValidation { minimum: Some(1.0), ..Default::default() })),
                        ..Default::default()
                    }),
                ]),
                ..Default::default()
            })),
            ..Default::default()
        })
    }
}

/// `settings.review.pack.body`：scanner 组 pack 装包形态；缺省 `slice`〔D8〕。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PackBody {
    Full,
    Slice,
}

/// 角色池（`settings.review.roles.<role>` / `settings.triage` / `settings.verify`）。
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RolePool {
    /// 有序轮转池；长度 1 合法。空池 = 角色缺席。
    #[serde(default)]
    pub models: Vec<RoleModelRef>,
}

/// 角色池条目：`{provider, id, reasoningEffort?}`。
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleModelRef {
    pub provider: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// `settings.review.scanners`：同模型多 lens 采样并集（E3：方差对冲旋钮）。
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScannersCfg {
    #[serde(default = "d_one")]
    pub samples: u32,
}
impl Default for ScannersCfg {
    fn default() -> Self {
        ScannersCfg { samples: d_one() }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewersCfg {
    #[serde(default = "d_max_cells")]
    pub max_cells: MaxCells,
    #[serde(default)]
    pub reenqueue: bool,
}
impl Default for ReviewersCfg {
    fn default() -> Self {
        ReviewersCfg { max_cells: d_max_cells(), reenqueue: false }
    }
}

/// `settings.review.refuters`：评审内 finding 验真（refute）开关。**缺省关**。
///
/// 池空 + `enabled=true` 的分叉（Q6 裁决）：
/// - 本地：保留自动异族回退（reviewer → scanner；无外族才同族 + warning）。
/// - job：跳过复核，报告 `extensions.redteam.refute_skipped = "no_refuter"`。
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RefutersCfg {
    #[serde(default)]
    pub enabled: bool,
}

/// `settings.review.grouping`：ocr 双阈值门起步值（4 / 200 / 10，D3）；`semantic` 默认关（R8）。
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
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
        GroupingCfg {
            min_files: d_min_files(),
            bundle_line_threshold: d_bundle_lines(),
            max_files_per_group: d_max_files(),
            semantic: false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackCfg {
    #[serde(default = "d_pack_body")]
    pub body: PackBody,
    #[serde(default = "d_anchor_window")]
    pub anchor_window: u32,
    /// finding 判定半径；**与 `anchorWindow` 是两回事**；0 = 连观测都关。
    #[serde(default = "d_finding_window")]
    pub finding_window: u32,
    /// 窗外 finding 是否**降级**（移出 findings、只进披露）。默认 false = 只观测不改产出。
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

fn d_one() -> u32 {
    1
}
fn d_max_cells() -> MaxCells {
    MaxCells::Auto
}
fn d_min_files() -> u32 {
    4
}
fn d_bundle_lines() -> u32 {
    200
}
fn d_max_files() -> u32 {
    10
}
fn d_pack_body() -> PackBody {
    PackBody::Slice
}
fn d_anchor_window() -> u32 {
    30
}
fn d_finding_window() -> u32 {
    30
}
fn d_slice_min_bytes() -> u64 {
    8192
}

/// `settings.review.roles`：评审三角色的模型池。
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRoles {
    #[serde(default)]
    pub scanner: RolePool,
    #[serde(default)]
    pub reviewer: RolePool,
    #[serde(default)]
    pub refuter: RolePool,
}

#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewConfig {
    #[serde(default = "default_block")]
    pub block_threshold: Severity,
    #[serde(default = "default_floor")]
    pub conf_floor: f32,
    /// 全局在途上限缺省（10，D13）；provider 级 `maxConcurrency` 缺省继承它。**唯一一道闸**（无角色级 workers）。
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
    pub refuters: RefutersCfg,
    #[serde(default)]
    pub grouping: GroupingCfg,
    #[serde(default)]
    pub pack: PackCfg,
    /// 评审三角色的模型池。
    #[serde(default)]
    pub roles: ReviewRoles,
}
fn default_block() -> Severity {
    Severity::Medium
}
fn default_floor() -> f32 {
    0.5
}
fn default_concurrency() -> usize {
    10
}
/// `maxConcurrency` 合理上限（D13）：超过只 warning 不拒。
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
            refuters: RefutersCfg::default(),
            grouping: GroupingCfg::default(),
            pack: PackCfg::default(),
            roles: ReviewRoles::default(),
        }
    }
}

/// `settings`：怎么用（语言、管线旋钮、角色池）。
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// 输出语言（`zh_CN` | `en_US`）；缺省：本地 locale / job `en_US`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub triage: RolePool,
    #[serde(default)]
    pub verify: RolePool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderConfig>,
}

/// 输出语言枚举（serde 严格枚举：非法值 → 配置解析失败）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
pub enum Language {
    #[serde(rename = "zh_CN")]
    ZhCn,
    #[serde(rename = "en_US")]
    EnUs,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::ZhCn => "zh_CN",
            Language::EnUs => "en_US",
        }
    }
    /// prompt 里的语言名（中英双语名对模型的约束更强，沿用 dogfood 20260703 的结论）。
    pub fn prompt_name(self) -> &'static str {
        match self {
            Language::ZhCn => "Simplified Chinese (简体中文)",
            Language::EnUs => "English",
        }
    }
    pub fn output(self) -> OutputLang {
        OutputLang { value: self.as_str().to_string(), prompt_name: self.prompt_name().to_string(), source: LangSource::Config }
    }
}

/// 语言取值的来源（`extensions.redteam.language.source`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LangSource {
    Config,
    Locale,
    Default,
}

impl LangSource {
    pub fn as_str(self) -> &'static str {
        match self {
            LangSource::Config => "config",
            LangSource::Locale => "locale",
            LangSource::Default => "default",
        }
    }
}

/// 解析一次、传给全部 prompt builder 与 status_reason 的输出语言（计划 P15：不再各处读 env）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLang {
    pub value: String,
    pub prompt_name: String,
    pub source: LangSource,
}

impl Default for OutputLang {
    fn default() -> Self {
        OutputLang { value: "en_US".into(), prompt_name: "English".into(), source: LangSource::Default }
    }
}

impl OutputLang {
    /// 本地/手工路径：配置 > locale 探测 > English。
    pub fn resolve_local(cfg: &Config) -> Self {
        if let Some(l) = cfg.settings.language {
            return l.output();
        }
        Self::from_env().unwrap_or_default()
    }

    /// job 路径：配置 > 缺省 `en_US`；**不读 env**（P15 顺序 3 / P6 可复现性）。
    pub fn resolve_job(cfg: &Config) -> Self {
        match cfg.settings.language {
            Some(l) => l.output(),
            None => OutputLang { source: LangSource::Default, ..Language::EnUs.output() },
        }
    }

    /// locale 探测（本地模式专用）：`LC_ALL` → `LC_MESSAGES` → `LANG`。
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("LC_ALL")
            .ok()
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|v| !v.is_empty()))
            .or_else(|| std::env::var("LANG").ok().filter(|v| !v.is_empty()))?;
        let lang_part = raw.split('.').next().unwrap_or("").trim();
        if lang_part.is_empty() || lang_part == "C" || lang_part == "POSIX" {
            return None;
        }
        let code = lang_part.split('_').next().unwrap_or("").to_lowercase();
        let prompt_name = match code.as_str() {
            "zh" => "Simplified Chinese (简体中文)",
            "ja" => "Japanese (日本語)",
            "ko" => "Korean (한국어)",
            "fr" => "French (français)",
            "de" => "German (Deutsch)",
            "es" => "Spanish (español)",
            _ => "English",
        };
        let value = match code.as_str() {
            "zh" => "zh_CN",
            "ja" => "ja",
            "ko" => "ko",
            "fr" => "fr",
            "de" => "de",
            "es" => "es",
            _ => "en_US",
        };
        Some(OutputLang { value: value.to_string(), prompt_name: prompt_name.to_string(), source: LangSource::Locale })
    }
}

/// 角色活跃集（merged 视图）。顺序 = 池内声明顺序。
#[derive(Clone, Debug, Default)]
pub struct RoleSets {
    pub scanners: Vec<EffectiveModel>,
    pub reviewers: Vec<EffectiveModel>,
    pub refuters: Vec<EffectiveModel>,
    pub verifiers: Vec<EffectiveModel>,
    pub triage: Vec<EffectiveModel>,
}

impl RoleSets {
    pub fn get(&self, role: Role) -> &[EffectiveModel] {
        match role {
            Role::Scanner => &self.scanners,
            Role::Reviewer => &self.reviewers,
            Role::Refuter => &self.refuters,
            Role::Verifier => &self.verifiers,
            Role::Triage => &self.triage,
        }
    }
    /// 全部条目（scanner → reviewer → refuter → verifier → triage 序）。
    pub fn all(&self) -> impl Iterator<Item = &EffectiveModel> {
        self.scanners
            .iter()
            .chain(self.reviewers.iter())
            .chain(self.refuters.iter())
            .chain(self.verifiers.iter())
            .chain(self.triage.iter())
    }
    /// 身份串 → 请求 id。
    pub fn id_of(&self, name: &str) -> Option<&str> {
        self.all().find(|m| m.name == name).map(|m| m.id.as_str())
    }
}

/// env 取值：未设或空白视为未设（`export X_API_KEY=` 占位不算有 key）。
fn env_nonempty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.trim().is_empty())
}

/// 身份串 = `provider/id`（route_key / 报告名 / 台账 model 字段）；`displayName` 只作显示〔D12 定案 3〕。
pub fn identity(provider: &str, id: &str) -> String {
    format!("{provider}/{id}")
}

/// 语义问题（`config check` 的机器码 + JSON Pointer；`Config::validate` 复用同一批判断）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticProblem {
    pub code: &'static str,
    pub pointer: String,
    pub message: String,
}

impl Config {
    /// 真实加载：`$REDTEAM_HOME/config.json`；文件不存在 = 空配置；其他 io 错误传播（不吞成空配置）。
    pub fn load() -> anyhow::Result<Config> {
        let (cfg, messages) = Config::load_with_messages()?;
        for m in &messages {
            eprintln!("{m}");
        }
        Ok(cfg)
    }

    /// 同 `load`，但 notice / warning 行返回给调用方而非打印。
    pub fn load_with_messages() -> anyhow::Result<(Config, Vec<String>)> {
        Self::load_with_messages_opts(true)
    }

    /// 不读 `secrets.json` 的加载：`--pack-only` 只装包不调 provider。
    pub fn load_no_secrets() -> anyhow::Result<(Config, Vec<String>)> {
        Self::load_with_messages_opts(false)
    }

    fn load_with_messages_opts(read_secrets: bool) -> anyhow::Result<(Config, Vec<String>)> {
        let mut messages = Vec::new();
        let path = config_path()?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => anyhow::bail!("读取 {} 失败：{e}", path.display()),
        };
        let (cfg, warnings) = Config::from_json_warned(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
        messages.extend(warnings.iter().map(|w| format!("warning: {w}")));
        if read_secrets {
            let secrets = secrets::load_secrets();
            messages.extend(secrets.warnings.iter().chain(cfg.secret_key_warnings(&secrets).iter()).map(|w| format!("warning: {w}")));
        }
        Ok((cfg, messages))
    }

    /// 解析 + 旧形状指引 + 校验；空白文本 = 空配置。返回 warnings（无定义键、模型未被引用）。
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
                warnings.push(format!(
                    "providers.{pname}.maxConcurrency = {c} 像是填错字段（maxOutputTokens？）：它是该 provider 的同时在途请求数上限；撞 429 时门会自动收敛，仍建议改回"
                ));
            }
            let unused: Vec<&str> = p.models.iter().map(|m| m.id.as_str()).filter(|id| !cfg.model_is_used(pname, id)).collect();
            for id in unused {
                warnings.push(format!("providers.{pname}.models[{id}] 未被任何角色池引用，不会参与任何作业"));
            }
        }
        if cfg.settings.review.max_concurrency > MAX_CONCURRENCY_SANE as usize {
            warnings.push(format!(
                "settings.review.maxConcurrency = {} 像是填错字段（maxOutputTokens？）：它是 provider 在途上限的缺省；撞 429 时门会自动收敛，仍建议改回",
                cfg.settings.review.max_concurrency
            ));
        }
        if cfg.settings.review.refuters.enabled && cfg.settings.review.roles.refuter.models.is_empty() {
            warnings.push("settings.review.refuters.enabled = true 但 roles.refuter.models 为空：job 模式将跳过复核（本地模式走自动异族回退）".into());
        }
        Ok((cfg, warnings))
    }

    pub fn from_json(text: &str) -> anyhow::Result<Config> {
        Ok(Config::from_json_warned(text)?.0)
    }

    /// 按 (provider, id) 取模型。
    pub fn model(&self, provider: &str, id: &str) -> Option<(&ProviderConfig, &ModelConfig)> {
        let p = self.providers.get(provider)?;
        let m = p.models.iter().find(|m| m.id == id)?;
        Some((p, m))
    }

    /// 按身份串反查 (provider, id)。
    pub fn find(&self, name: &str) -> Option<(String, String)> {
        for (pname, p) in &self.providers {
            for m in &p.models {
                if identity(pname, &m.id) == name {
                    return Some((pname.clone(), m.id.clone()));
                }
            }
        }
        None
    }

    /// 该 provider 下的模型是否被任一角色池引用（`model_unused` WARN 用）。
    pub fn model_is_used(&self, provider: &str, id: &str) -> bool {
        let used = |pool: &RolePool| pool.models.iter().any(|r| r.provider == provider && r.id == id);
        used(&self.settings.review.roles.scanner)
            || used(&self.settings.review.roles.reviewer)
            || used(&self.settings.review.roles.refuter)
            || used(&self.settings.triage)
            || used(&self.settings.verify)
    }

    /// merged 视图。未实现的 api 也照常返回（由 backend 构造报错 → reviewer 硬失败 / refuter 软跳过，
    /// 保持可观测；`validate` 仍会在配置加载期先拒）。
    pub fn effective(
        &self,
        provider: &str,
        id: &str,
        role: Role,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Option<EffectiveModel> {
        let (p, m) = self.model(provider, id)?;
        Some(EffectiveModel {
            name: identity(provider, id),
            display_name: m.display_name.clone(),
            id: id.to_string(),
            provider: provider.to_string(),
            role,
            api: p.api,
            base_url: Some(p.base_url.clone()),
            context_window: m.context_window.unwrap_or(128_000),
            max_output_tokens: m.max_output_tokens,
            per_turn_timeout_secs: m.per_turn_timeout_secs,
            reasoning: m.reasoning,
            reasoning_effort,
            thinking_level_map: m.thinking_level_map.clone(),
            family: m.family.clone().unwrap_or_else(|| {
                id.split(['-', '_', '.']).next().unwrap_or(id).to_lowercase()
            }),
            max_concurrency: p.max_concurrency,
        })
    }

    fn pool_models(&self, pool: &RolePool, role: Role) -> Vec<EffectiveModel> {
        pool.models
            .iter()
            .filter_map(|r| self.effective(&r.provider, &r.id, role, r.reasoning_effort))
            .collect()
    }

    /// 角色活跃集：每角色 = settings 池里的模型（顺序即轮转序）。
    pub fn active_roles(&self) -> RoleSets {
        RoleSets {
            scanners: self.pool_models(&self.settings.review.roles.scanner, Role::Scanner),
            reviewers: self.pool_models(&self.settings.review.roles.reviewer, Role::Reviewer),
            refuters: self.pool_models(&self.settings.review.roles.refuter, Role::Refuter),
            verifiers: self.pool_models(&self.settings.verify, Role::Verifier),
            triage: self.pool_models(&self.settings.triage, Role::Triage),
        }
    }

    /// triage 池（按轴轮转的候选；顺序 = 声明顺序）。
    pub fn triage_models(&self) -> Vec<EffectiveModel> {
        self.pool_models(&self.settings.triage, Role::Triage)
    }

    /// P2（用户 2026-11-24 裁）+ Q6（2026-11-25 裁）：job 模式池空 → 不验真；本地模式保留自动异族回退。
    pub fn refute_enabled(&self, job_mode: bool, roles: &RoleSets) -> bool {
        self.settings.review.refuters.enabled && !(job_mode && roles.refuters.is_empty())
    }

    /// key 解析链（Q2 裁决）：模型内联 → 模型 env → secrets.models → provider 内联 → provider env → secrets.apiKey。
    pub fn resolve_key(&self, provider: &str, id: &str) -> Option<String> {
        self.resolve_key_with(provider, id, &secrets::load_secrets())
    }

    pub fn resolve_key_with(&self, provider: &str, id: &str, secrets: &secrets::Secrets) -> Option<String> {
        let (p, m) = self.model(provider, id)?;
        if let Some(k) = m.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
            return Some(k.clone());
        }
        if let Some(var) = &m.api_key_env {
            if let Some(v) = env_nonempty(var) {
                return Some(v);
            }
        }
        let ps = secrets.providers.get(provider);
        if let Some(k) = ps.and_then(|s| s.models.get(id)) {
            return Some(k.clone());
        }
        if let Some(k) = p.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
            return Some(k.clone());
        }
        if let Some(var) = &p.api_key_env {
            if let Some(v) = env_nonempty(var) {
                return Some(v);
            }
        }
        ps.and_then(|s| s.api_key.clone())
    }

    /// job 模式专用：**只读 env**（模型 apiKeyEnv → provider apiKeyEnv），不读内联 / secrets。
    /// job 配置在 `check_text` 已拒绝 apiKey 键；这里再保证解析路径本身不会碰内联槽（defense in depth）。
    pub fn resolve_key_env_only(&self, provider: &str, id: &str) -> Option<String> {
        let (p, m) = self.model(provider, id)?;
        if let Some(var) = &m.api_key_env {
            if let Some(v) = env_nonempty(var) {
                return Some(v);
            }
        }
        if let Some(var) = &p.api_key_env {
            if let Some(v) = env_nonempty(var) {
                return Some(v);
            }
        }
        None
    }

    /// provider 维度 key（`model list --remote` 用）：provider 内联 → provider env → secrets.apiKey；不回退模型级 key。
    pub fn resolve_provider_key(&self, provider: &str) -> Option<String> {
        if let Some(p) = self.providers.get(provider) {
            if let Some(k) = p.api_key.as_ref().filter(|k| !k.trim().is_empty()) {
                return Some(k.clone());
            }
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
                        if !p.models.iter().any(|m| &m.id == mid) {
                            out.push(format!("secrets.json providers.{prov}.models.{mid:?} 在 config.json 该 provider 下没有对应模型（拼写？）"));
                        }
                    }
                }
            }
        }
        out
    }

    /// 语义问题（未知模型引用 / 池内重复 / 未实现 api）。`config check` 用 pointer 逐条上报，validate 聚合成一条错误。
    pub fn semantic_problems(&self) -> Vec<SemanticProblem> {
        let mut out = Vec::new();
        for (pname, p) in &self.providers {
            if !p.api.implemented() {
                out.push(SemanticProblem {
                    code: "api_not_implemented",
                    pointer: format!("/providers/{}/api", ptr_escape(pname)),
                    message: format!("api {} 尚未实现（预留值）；当前可用：openai-completions / anthropic-messages", p.api.as_str()),
                });
            }
            let mut seen: Vec<&str> = Vec::new();
            for (idx, m) in p.models.iter().enumerate() {
                if seen.contains(&m.id.as_str()) {
                    out.push(SemanticProblem {
                        code: "duplicate_model",
                        pointer: format!("/providers/{}/models/{idx}", ptr_escape(pname)),
                        message: format!("model id {:?} 在同一 provider 内重复", m.id),
                    });
                }
                seen.push(&m.id);
            }
        }
        let pools: [(&str, &RolePool); 5] = [
            ("/settings/review/roles/scanner", &self.settings.review.roles.scanner),
            ("/settings/review/roles/reviewer", &self.settings.review.roles.reviewer),
            ("/settings/review/roles/refuter", &self.settings.review.roles.refuter),
            ("/settings/triage", &self.settings.triage),
            ("/settings/verify", &self.settings.verify),
        ];
        for (base, pool) in pools {
            let mut seen: Vec<(&str, &str)> = Vec::new();
            for (idx, r) in pool.models.iter().enumerate() {
                let path = format!("{base}/models/{idx}");
                if r.provider.trim().is_empty() || r.id.trim().is_empty() {
                    out.push(SemanticProblem {
                        code: "invalid_value",
                        pointer: path,
                        message: "provider / id 不能为空".into(),
                    });
                    continue;
                }
                if seen.contains(&(r.provider.as_str(), r.id.as_str())) {
                    out.push(SemanticProblem {
                        code: "duplicate_model",
                        pointer: path.clone(),
                        message: format!("{}/{} 在同一角色池内重复", r.provider, r.id),
                    });
                }
                seen.push((&r.provider, &r.id));
                if self.model(&r.provider, &r.id).is_none() {
                    out.push(SemanticProblem {
                        code: "unknown_model",
                        pointer: path,
                        message: format!("引用了不存在的模型 {}/{}（检查 providers 下的 provider 与 models[].id）", r.provider, r.id),
                    });
                }
            }
        }
        out
    }

    /// 结构校验（不含语义问题；供 `check_text` 单行上报）。
    pub fn structural_errors(&self) -> Vec<String> {
        let mut errs: Vec<String> = Vec::new();
        for (pname, p) in &self.providers {
            if pname.is_empty() || pname.contains('/') {
                errs.push(format!("provider 名 {pname:?} 非法：须非空且不含 '/'（身份串 provider/id 须可拆）"));
            }
            if p.base_url.trim().is_empty() {
                errs.push(format!("providers.{pname}.baseUrl 必填（v2 不再按 api 猜默认端点）"));
            }
            if p.max_concurrency == Some(0) {
                errs.push(format!("providers.{pname}.maxConcurrency 必须 ≥ 1"));
            }
            for m in &p.models {
                if m.id.trim().is_empty() {
                    errs.push(format!("providers.{pname}.models[] 含空 model id"));
                }
                if let Some(cw) = m.context_window {
                    if cw < MIN_CONTEXT_WINDOW {
                        errs.push(format!(
                            "providers.{pname}.models[{}]: contextWindow {cw} 低于下限 {MIN_CONTEXT_WINDOW}（单位是 token，128K 请写 128000）",
                            m.id
                        ));
                    }
                }
                if m.max_output_tokens == Some(0) {
                    errs.push(format!("providers.{pname}.models[{}]: maxOutputTokens 必须 ≥ 1", m.id));
                }
                if m.per_turn_timeout_secs == Some(0) {
                    errs.push(format!("providers.{pname}.models[{}]: perTurnTimeoutSecs 必须 ≥ 1", m.id));
                }
            }
        }
        let r = &self.settings.review;
        for (k, v) in [
            ("settings.review.maxConcurrency", r.max_concurrency as u64),
            ("settings.review.scanners.samples", r.scanners.samples as u64),
            ("settings.review.pack.anchorWindow", r.pack.anchor_window as u64),
            ("settings.review.grouping.minFiles", r.grouping.min_files as u64),
            ("settings.review.grouping.maxFilesPerGroup", r.grouping.max_files_per_group as u64),
        ] {
            if v == 0 {
                errs.push(format!("{k} 必须 ≥ 1"));
            }
        }
        if r.reviewers.max_cells == MaxCells::Fixed(0) {
            errs.push("settings.review.reviewers.maxCells 必须 ≥ 1（或 \"auto\"）".to_string());
        }
        if !(0.0..=1.0).contains(&r.conf_floor) {
            errs.push(format!("settings.review.confFloor {} 须在 0.0..=1.0（评审 S2）", r.conf_floor));
        }
        errs
    }

    /// 结构校验（load 后必调）：结构 + 语义（未知模型 / 重复 / 未实现 api）。
    /// **不**校验 review/triage/verify 三池齐全（最小集只在 `config check --kind`）。
    pub fn validate(&self) -> anyhow::Result<()> {
        let mut errs = self.structural_errors();
        for p in self.semantic_problems() {
            errs.push(p.message);
        }
        if !errs.is_empty() {
            anyhow::bail!("配置校验失败：\n{}", errs.join("\n"));
        }
        Ok(())
    }
}

// ─────────────────────────── shape guard / strict ───────────────────────────

const ROOT: &[&str] = &["settings", "providers"];
const SETTINGS: &[&str] = &["language", "review", "triage", "verify"];
const REVIEW: &[&str] = &[
    "blockThreshold", "confFloor", "maxConcurrency", "tools", "scanners", "reviewers", "refuters", "grouping", "pack", "roles",
];
const ROLES: &[&str] = &["scanner", "reviewer", "refuter"];
const ROLE_POOL: &[&str] = &["models"];
const ROLE_REF: &[&str] = &["provider", "id", "reasoningEffort"];
const PROVIDER: &[&str] = &["api", "baseUrl", "apiKey", "apiKeyEnv", "maxConcurrency", "models"];
const MODEL: &[&str] = &[
    "id", "displayName", "apiKey", "apiKeyEnv", "family", "contextWindow", "maxOutputTokens", "perTurnTimeoutSecs", "reasoning", "thinkingLevelMap",
];
const THINKING: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max", "ultra"];
/// review 已删键：`risk`（D11 无模式旋钮）；`verifiers` 已更名 `refuters`。
const REVIEW_REMOVED: &[(&str, &str)] = &[("risk", "D11：无模式旋钮")];
/// (节名, 已知键, 是否角色旋钮节)；角色旋钮节的已删键 `workers`（D13）按缺省值拼文案。
const REVIEW_SECTIONS: &[(&str, &[&str], bool)] = &[
    ("scanners", &["samples"], true),
    ("reviewers", &["maxCells", "reenqueue"], true),
    ("refuters", &["enabled"], true),
    ("grouping", &["minFiles", "bundleLineThreshold", "maxFilesPerGroup", "semantic"], false),
    ("pack", &["body", "anchorWindow", "findingWindow", "findingWindowEnforce", "sliceMinBytes"], false),
];

/// v1 → v2 的 `kind` 值迁移提示（含别名；v2 解析期不接受别名）。
fn legacy_api_hint(v: &str) -> Option<&'static str> {
    match v {
        "openai" | "gpt" | "openai-compatible" | "openai_compatible" | "compatible" => Some("openai-completions"),
        "anthropic" | "claude" => Some("anthropic-messages"),
        _ => None,
    }
}

/// 模型级 v1 旧键 → 迁移提示（数组 / 对象两种形状共用）。
fn legacy_model_keys(errors: &mut Vec<String>, mpath: &str, mo: &serde_json::Map<String, serde_json::Value>) {
    if mo.contains_key("name") {
        errors.push(format!("{mpath}.name 已改名 displayName（身份串固定为 provider/id，不可配置）"));
    }
    if let Some(kind) = mo.get("kind") {
        let eff = kind.as_str().unwrap_or("<kind>");
        errors.push(format!(
            "{mpath}.kind 已删除：v2 的 api 只在 provider 层（本模型有效 api = model.kind.or(provider.kind) = {eff:?}）；同一 provider 混多种 api 需拆 provider"
        ));
    }
    if mo.contains_key("roles") {
        errors.push(format!("{mpath}.roles 已删除：角色改为 settings.review.roles.<role>.models[] 条目（verifier → settings.verify.models[]）"));
    }
}

fn push_legacy(out: &mut Vec<ConfigIssue>, ptr: impl Into<String>, msg: impl Into<String>) {
    out.push(ConfigIssue { code: "legacy_shape", pointer: ptr.into(), message: msg.into() });
}

/// v1 形状 → **逐字段**迁移诊断（`config check` 用；每条带准确 JSON Pointer）。
/// 与 `shape_guard` 的聚合文案同源，但拆成一条一键，避免 hibot 只看到根指针。
fn legacy_field_issues(v: &serde_json::Value) -> Vec<ConfigIssue> {
    let mut out: Vec<ConfigIssue> = Vec::new();
    let Some(top) = v.as_object() else { return out };
    if top.contains_key("language") {
        push_legacy(&mut out, "/language", "language 已移入 settings.language");
    }
    if let Some(review) = top.get("review").and_then(|r| r.as_object()) {
        push_legacy(
            &mut out,
            "/review",
            "review 已整节移入 settings.review（角色池在 settings.review.roles.<role>.models[]）",
        );
        for k in review.keys() {
            let target = match k.as_str() {
                "blockThreshold" | "confFloor" | "maxConcurrency" | "tools" => format!("settings.review.{k}"),
                "scanners" => "settings.review.scanners".into(),
                "reviewers" => "settings.review.reviewers".into(),
                "refuters" => "settings.review.refuters".into(),
                "grouping" => "settings.review.grouping".into(),
                "pack" => "settings.review.pack".into(),
                "verifiers" => "settings.review.refuters / settings.review.roles.refuter.models".into(),
                "risk" => "已删除（D11：无模式旋钮）".into(),
                _ => "settings.review（该键在 v2 无定义）".into(),
            };
            push_legacy(&mut out, format!("/review/{}", ptr_escape(k)), format!("review.{k} → {target}"));
        }
        for (section, keys, _) in REVIEW_SECTIONS {
            if let Some(so) = review.get(*section).and_then(|s| s.as_object()) {
                for k in so.keys() {
                    let target = if keys.contains(&k.as_str()) {
                        format!("settings.review.{section}.{k}")
                    } else {
                        format!("settings.review.{section}（该键在 v2 无定义）")
                    };
                    push_legacy(
                        &mut out,
                        format!("/review/{section}/{}", ptr_escape(k)),
                        format!("review.{section}.{k} → {target}"),
                    );
                }
            }
        }
    }
    if let Some(provs) = top.get("providers").and_then(|p| p.as_object()) {
        for (pname, pv) in provs {
            let Some(po) = pv.as_object() else { continue };
            let ppath = format!("/providers/{}", ptr_escape(pname));
            if let Some(kind) = po.get("kind").and_then(|k| k.as_str()) {
                let target: String = legacy_api_hint(kind)
                    .map(|api| format!("providers.{pname}.api = {api:?}"))
                    .unwrap_or_else(|| "api 三值：openai-completions | anthropic-messages | openai-responses（最后一个仅预留）；google 已移除".into());
                push_legacy(&mut out, format!("{ppath}/kind"), format!("kind 已改名为 api（v2 无别名）：{target}"));
            } else if po.contains_key("kind") {
                push_legacy(&mut out, format!("{ppath}/kind"), "kind 已改名为 api（v2 三值，无别名）");
            }
            match po.get("models") {
                Some(serde_json::Value::Object(ms)) => {
                    let ids: Vec<&str> = ms.keys().map(String::as_str).collect();
                    push_legacy(
                        &mut out,
                        format!("{ppath}/models"),
                        format!(
                            "models 是对象（v1）：v2 改为数组，元素带 id（如 [{{\"id\": {:?}, …}}]；原有模型：{}）",
                            ids.first().unwrap_or(&"<id>"),
                            ids.join(" / ")
                        ),
                    );
                    for (mid, mv) in ms {
                        if let Some(mo) = mv.as_object() {
                            legacy_model_field_issues(&mut out, &format!("{ppath}/models/{}", ptr_escape(mid)), mo);
                        }
                    }
                }
                Some(serde_json::Value::Array(ms)) => {
                    for (idx, mv) in ms.iter().enumerate() {
                        if let Some(mo) = mv.as_object() {
                            legacy_model_field_issues(&mut out, &format!("{ppath}/models/{idx}"), mo);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn legacy_model_field_issues(out: &mut Vec<ConfigIssue>, mpath: &str, mo: &serde_json::Map<String, serde_json::Value>) {
    if mo.contains_key("name") {
        push_legacy(out, format!("{mpath}/name"), "name 已改名 displayName（身份串固定为 provider/id，不可配置）");
    }
    if let Some(kind) = mo.get("kind") {
        let eff = kind.as_str().unwrap_or("<kind>");
        push_legacy(
            out,
            format!("{mpath}/kind"),
            format!("kind 已删除：api 只在 provider 层（本模型有效 api = model.kind.or(provider.kind) = {eff:?}）；同一 provider 混多种 api 需拆 provider"),
        );
    }
    if let Some(roles) = mo.get("roles") {
        push_legacy(
            out,
            format!("{mpath}/roles"),
            "roles 已删除：角色改为 settings.review.roles.<role>.models[] 条目（verifier → settings.verify.models[]）",
        );
        if let Some(ro) = roles.as_object() {
            for role in ro.keys() {
                let target = match role.as_str() {
                    "scanner" | "reviewer" | "refuter" => format!("settings.review.roles.{role}.models[]"),
                    "verifier" => "settings.verify.models[]".into(),
                    _ => "scanner | reviewer | refuter | verifier | triage".into(),
                };
                push_legacy(out, format!("{mpath}/roles/{}", ptr_escape(role)), format!("roles.{role} → {target}"));
            }
        }
    }
}

/// 形状守卫（解析前按 `Value` 走一遍）：
/// - v1 形状 / 旧键 / 旧别名 / snake_case → **错误**并指向 v2 写法（不静默丢配置）；
/// - 已知层级下的无定义键 → **warning**（serde 默认忽略未知字段；job 模式的 `strict_issues` 会升级为 error）。
fn shape_guard(v: &serde_json::Value) -> anyhow::Result<Vec<String>> {
    fn check(errors: &mut Vec<String>, warnings: &mut Vec<String>, path: &str, obj: &serde_json::Map<String, serde_json::Value>, known: &[&str], hint: &str) {
        check_keys(errors, warnings, path, obj, known, hint, &[]);
    }
    fn check_keys(
        errors: &mut Vec<String>,
        warnings: &mut Vec<String>,
        path: &str,
        obj: &serde_json::Map<String, serde_json::Value>,
        known: &[&str],
        hint: &str,
        removed: &[(&str, &str)],
    ) {
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
        anyhow::bail!("config.json 顶层须是对象（{{\"settings\": …, \"providers\": …}}）");
    };

    // v1 顶层键 → v2 专属迁移提示（比「未知键」更可操作）。
    if top.contains_key("language") {
        errors.push("language 已移入 settings.language（v2：设置节统一承载语言 / 角色池 / 管线旋钮）".into());
    }
    if top.contains_key("review") {
        errors.push("review 已整节移入 settings.review（角色池在 settings.review.roles.<role>.models[]）".into());
    }
    for k in ["scanners", "reviewers", "refuters", "verifiers", "models"] {
        if top.contains_key(k) {
            errors.push(format!("顶层 {k:?}：v2 的角色池在 settings.review.roles.<role>.models[] / settings.triage.models[] / settings.verify.models[]"));
        }
    }
    check(&mut errors, &mut warnings, "(顶层)", top, ROOT, "settings / providers");

    if let Some(st) = top.get("settings").and_then(|s| s.as_object()) {
        check(&mut errors, &mut warnings, "settings", st, SETTINGS, "language / review / triage / verify");
        for sec in ["triage", "verify"] {
            if let Some(so) = st.get(sec) {
                if !so.is_object() {
                    errors.push(format!("settings.{sec} 须是对象（{{\"models\": [...]}}）"));
                } else if let Some(so) = so.as_object() {
                    check(&mut errors, &mut warnings, &format!("settings.{sec}"), so, ROLE_POOL, "models");
                    if so.contains_key("models") {
                        check_role_refs(&mut errors, &mut warnings, &format!("settings.{sec}.models"), so.get("models"));
                    }
                }
            }
        }
        if let Some(r) = st.get("review") {
            let Some(r) = r.as_object() else {
                errors.push("settings.review 须是对象".into());
                if !errors.is_empty() {
                    anyhow::bail!("{}", v2_shape_error(&errors));
                }
                return Ok(warnings);
            };
            if r.contains_key("verifiers") {
                errors.push("settings.review.verifiers 已更名 settings.review.refuters（0.8 术语分家：verify 留给 hibot 的修复验证 kind，评审内验真叫 refute）".into());
            }
            check_keys(&mut errors, &mut warnings, "settings.review", r, REVIEW, "blockThreshold / confFloor / maxConcurrency", REVIEW_REMOVED);
            let workers_why = format!("D13：并发只看 provider maxConcurrency，缺省 settings.review.maxConcurrency = {}", default_concurrency());
            let workers_removed = [("workers", workers_why.as_str())];
            for (sec, known, role_section) in REVIEW_SECTIONS {
                if let Some(so) = r.get(*sec).and_then(|s| s.as_object()) {
                    let removed: &[(&str, &str)] = if *role_section { &workers_removed } else { &[] };
                    check_keys(&mut errors, &mut warnings, &format!("settings.review.{sec}"), so, known, "如 maxCells / minFiles / sliceMinBytes", removed);
                }
            }
            if let Some(roles) = r.get("roles") {
                let Some(ro) = roles.as_object() else {
                    errors.push("settings.review.roles 须是对象（scanner / reviewer / refuter）".into());
                    if !errors.is_empty() {
                        anyhow::bail!("{}", v2_shape_error(&errors));
                    }
                    return Ok(warnings);
                };
                check(&mut errors, &mut warnings, "settings.review.roles", ro, ROLES, "scanner / reviewer / refuter");
                for role in ROLES {
                    if let Some(pool) = ro.get(*role) {
                        let Some(po) = pool.as_object() else {
                            errors.push(format!("settings.review.roles.{role} 须是对象（{{\"models\": [...]}}）"));
                            continue;
                        };
                        check(&mut errors, &mut warnings, &format!("settings.review.roles.{role}"), po, ROLE_POOL, "models");
                        if po.contains_key("models") {
                            check_role_refs(&mut errors, &mut warnings, &format!("settings.review.roles.{role}.models"), po.get("models"));
                        }
                    }
                }
            }
        }
    }

    if let Some(provs) = top.get("providers").and_then(|p| p.as_object()) {
        for (pname, pv) in provs {
            let Some(po) = pv.as_object() else {
                errors.push(format!("providers.{pname} 须是对象"));
                continue;
            };
            let ppath = format!("providers.{pname}");
            if let Some(kind) = po.get("kind").and_then(|k| k.as_str()) {
                let hint = legacy_api_hint(kind)
                    .map(|api| format!("providers.{pname}.api = {api:?}"))
                    .unwrap_or_else(|| "api 三值：openai-completions | anthropic-messages | openai-responses（最后一个仅预留）".into());
                errors.push(format!("{ppath}.kind 已改名为 api（v2 值收缩，无别名）：{hint}；google 已移除"));
            } else if po.contains_key("kind") {
                errors.push(format!("{ppath}.kind 已改名为 api（v2 三值：openai-completions | anthropic-messages | openai-responses）"));
            }
            check(&mut errors, &mut warnings, &ppath, po, PROVIDER, "api / baseUrl / apiKeyEnv / maxConcurrency");
            match po.get("models") {
                Some(serde_json::Value::Object(ms)) => {
                    let ids: Vec<&str> = ms.keys().map(String::as_str).collect();
                    errors.push(format!(
                        "{ppath}.models 是对象（v1 形状）：v2 改为数组，元素带 id（如 [{{\"id\": {:?}, …}}]；原有模型：{}）",
                        ids.first().unwrap_or(&"<id>"),
                        ids.join(" / ")
                    ));
                    // 即使形状是 v1 的对象，也要把模型级旧键逐条点名（否则用户只看到「改成数组」，
                    // 不知道 roles / kind 也已删除）。
                    for (mid, mv) in ms {
                        if let Some(mo) = mv.as_object() {
                            legacy_model_keys(&mut errors, &format!("{ppath}.models.{mid}"), mo);
                        }
                    }
                }
                Some(serde_json::Value::Array(ms)) => {
                    for (idx, mv) in ms.iter().enumerate() {
                        let Some(mo) = mv.as_object() else {
                            errors.push(format!("{ppath}.models[{idx}] 须是对象"));
                            continue;
                        };
                        let mpath = format!("{ppath}.models[{idx}]");
                        legacy_model_keys(&mut errors, &mpath, mo);
                        check(&mut errors, &mut warnings, &mpath, mo, MODEL, "id / displayName / apiKeyEnv / contextWindow / maxOutputTokens");
                        if let Some(tm) = mo.get("thinkingLevelMap").and_then(|t| t.as_object()) {
                            check(&mut errors, &mut warnings, &format!("{mpath}.thinkingLevelMap"), tm, THINKING, "minimal … ultra（值为字符串或 null）");
                        }
                    }
                }
                Some(_) => errors.push(format!("{ppath}.models 须是数组（v2）")),
                None => {}
            }
        }
    }

    if !errors.is_empty() {
        anyhow::bail!("{}", v2_shape_error(&errors));
    }
    Ok(warnings)
}

/// 角色池条目的形状：必须是数组，元素是 `{provider,id,reasoningEffort?}` 对象。
fn check_role_refs(
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
    path: &str,
    value: Option<&serde_json::Value>,
) {
    let Some(value) = value else { return };
    let Some(items) = value.as_array() else {
        errors.push(format!("{path} 须是数组（条目 {{provider,id,reasoningEffort?}}）"));
        return;
    };
    for (idx, item) in items.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            errors.push(format!("{path}[{idx}] 须是对象（{{provider,id,reasoningEffort?}}）"));
            continue;
        };
        for k in obj.keys() {
            if !ROLE_REF.contains(&k.as_str()) {
                if k.contains('_') {
                    errors.push(format!("{path}[{idx}].{k}：键名为 camelCase（provider / id / reasoningEffort）"));
                } else {
                    warnings.push(format!("{path}[{idx}].{k} 无定义，已忽略（已知键：provider / id / reasoningEffort）"));
                }
            }
        }
    }
}

fn v2_shape_error(errors: &[String]) -> String {
    format!("配置形状已升级（config v2；见 redteam --help 与 .doc/design/2026-11-25-redteam-config-v2-design.md）：\n{}", errors.join("\n"))
}

/// 严格校验问题（job 模式 / `config check`）：位置用 **JSON Pointer**。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigIssue {
    /// 机器码：`unknown_key` / `secret_field` / `legacy_shape` / `invalid_value` / `api_not_implemented` /
    /// `unknown_model` / `duplicate_model`。
    pub code: &'static str,
    pub pointer: String,
    pub message: String,
}

fn ptr_escape(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

fn ptr_join(base: &str, key: &str) -> String {
    format!("{base}/{}", ptr_escape(key))
}

/// 密钥类字段名（规范化后精确匹配，`apiKeyEnv` 不命中）。
fn is_secret_key(k: &str) -> bool {
    let n = k.to_ascii_lowercase().replace(['_', '-'], "");
    matches!(n.as_str(), "apikey" | "token" | "secret" | "password" | "accesstoken" | "privatekey" | "clientsecret")
}

fn strict_unknown(out: &mut Vec<ConfigIssue>, base: &str, obj: &serde_json::Map<String, serde_json::Value>, known: &[&str]) {
    strict_unknown_legacy(out, base, obj, known, &[]);
}

/// 同 `strict_unknown`，但 `legacy` 里的键交给 `shape_guard` 的迁移提示，不再叠一条 `unknown_key`。
fn strict_unknown_legacy(
    out: &mut Vec<ConfigIssue>,
    base: &str,
    obj: &serde_json::Map<String, serde_json::Value>,
    known: &[&str],
    legacy: &[&str],
) {
    for k in obj.keys() {
        if !known.contains(&k.as_str()) {
            // 密钥字段由 `strict_secrets` 单独报 `secret_field`；v1 旧键由 `shape_guard` 给迁移提示。
            if is_secret_key(k) || legacy.contains(&k.as_str()) {
                continue;
            }
            out.push(ConfigIssue {
                code: "unknown_key",
                pointer: ptr_join(base, k),
                message: format!("unknown key (known: {})", known.join(" / ")),
            });
        }
    }
}

fn strict_secrets(v: &serde_json::Value, base: &str, out: &mut Vec<ConfigIssue>) {
    match v {
        serde_json::Value::Object(o) => {
            for (k, child) in o {
                let p = ptr_join(base, k);
                if is_secret_key(k) {
                    out.push(ConfigIssue {
                        code: "secret_field",
                        pointer: p.clone(),
                        message: "executor config must not contain secrets; supply credentials via env vars".into(),
                    });
                }
                strict_secrets(child, &p, out);
            }
        }
        serde_json::Value::Array(a) => {
            for (i, child) in a.iter().enumerate() {
                strict_secrets(child, &format!("{base}/{i}"), out);
            }
        }
        _ => {}
    }
}

/// 严格扫描：未知键 / 密钥字段（形状与类型错误由 `check_text` 的 serde + validate 补齐）。
pub fn strict_issues(v: &serde_json::Value) -> Vec<ConfigIssue> {
    let mut out = Vec::new();
    strict_secrets(v, "", &mut out);
    let Some(top) = v.as_object() else {
        out.push(ConfigIssue { code: "invalid_value", pointer: String::new(), message: "config root must be an object".into() });
        return out;
    };
    strict_unknown_legacy(&mut out, "", top, ROOT, &["language", "review"]);
    if let Some(st) = top.get("settings").and_then(|s| s.as_object()) {
        strict_unknown(&mut out, "/settings", st, SETTINGS);
        if let Some(r) = st.get("review").and_then(|r| r.as_object()) {
            strict_unknown_legacy(&mut out, "/settings/review", r, REVIEW, &["verifiers"]);
            for (sec, known, _) in REVIEW_SECTIONS {
                if let Some(so) = r.get(*sec).and_then(|s| s.as_object()) {
                    strict_unknown(&mut out, &ptr_join("/settings/review", sec), so, known);
                }
            }
            if let Some(ro) = r.get("roles").and_then(|r| r.as_object()) {
                strict_unknown(&mut out, "/settings/review/roles", ro, ROLES);
                for role in ROLES {
                    if let Some(po) = ro.get(*role).and_then(|p| p.as_object()) {
                        strict_unknown(&mut out, &ptr_join("/settings/review/roles", role), po, ROLE_POOL);
                        strict_role_refs(&mut out, &format!("/settings/review/roles/{role}/models"), po.get("models"));
                    }
                }
            }
        }
        for sec in ["triage", "verify"] {
            if let Some(po) = st.get(sec).and_then(|p| p.as_object()) {
                strict_unknown(&mut out, &format!("/settings/{sec}"), po, ROLE_POOL);
                strict_role_refs(&mut out, &format!("/settings/{sec}/models"), po.get("models"));
            }
        }
    }
    if let Some(provs) = top.get("providers").and_then(|p| p.as_object()) {
        for (pname, pv) in provs {
            let Some(po) = pv.as_object() else { continue };
            let base = format!("/providers/{}", ptr_escape(pname));
            strict_unknown_legacy(&mut out, &base, po, PROVIDER, &["kind"]);
            if let Some(ms) = po.get("models").and_then(|m| m.as_array()) {
                for (idx, mv) in ms.iter().enumerate() {
                    let Some(mo) = mv.as_object() else { continue };
                    let mbase = format!("{base}/models/{idx}");
                    strict_unknown_legacy(&mut out, &mbase, mo, MODEL, &["kind", "roles", "name"]);
                    if let Some(tm) = mo.get("thinkingLevelMap").and_then(|t| t.as_object()) {
                        strict_unknown(&mut out, &format!("{mbase}/thinkingLevelMap"), tm, THINKING);
                    }
                }
            }
        }
    }
    out
}

fn strict_role_refs(out: &mut Vec<ConfigIssue>, base: &str, value: Option<&serde_json::Value>) {
    let Some(items) = value.and_then(|v| v.as_array()) else { return };
    for (idx, item) in items.iter().enumerate() {
        if let Some(obj) = item.as_object() {
            strict_unknown(out, &format!("{base}/{idx}"), obj, ROLE_REF);
        }
    }
}

/// 严格校验（job 模式 / `config check`）：形状 + 未知键 + 密钥字段 + 语义，返回全部问题。
/// 返回 `(Some(config), issues)`：issues 非空即不合法；`None` = 连 Config 都构不出来。
pub fn check_text(text: &str) -> (Option<Config>, Vec<ConfigIssue>) {
    let mut issues = Vec::new();
    let v: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            issues.push(ConfigIssue { code: "invalid_value", pointer: String::new(), message: format!("JSON 解析失败：{e}") });
            return (None, issues);
        }
    };
    issues.extend(strict_issues(&v));
    let field_issues = legacy_field_issues(&v);
    let legacy = if field_issues.is_empty() {
        match shape_guard(&v) {
            Ok(_) => None,
            Err(e) => {
                // 非 v1-键类形状错误（类型 / 未知角色等）：仍聚合一条，单行输出。
                issues.push(ConfigIssue { code: "legacy_shape", pointer: String::new(), message: e.to_string().replace('\n', " ") });
                Some(())
            }
        }
    } else {
        issues.extend(field_issues);
        Some(())
    };
    match serde_json::from_value::<Config>(v.clone()) {
        Ok(cfg) => {
            let structural = cfg.structural_errors();
            if !structural.is_empty() {
                // 单行（hibot 一行一条诊断）；逐条语义问题随后单独上报。
                issues.push(ConfigIssue {
                    code: "invalid_value",
                    pointer: String::new(),
                    message: structural.join(" "),
                });
            }
            for p in cfg.semantic_problems() {
                issues.push(ConfigIssue { code: p.code, pointer: p.pointer, message: p.message });
            }
            (Some(cfg), issues)
        }
        Err(e) => {
            // v1 形状已由 legacy_shape 解释；再报一条 serde 形状错误只会掩盖迁移提示。
            if legacy.is_none() {
                let pointer = locate_shape_error(&v).unwrap_or_default();
                issues.push(ConfigIssue { code: "invalid_value", pointer, message: format!("配置形状错误：{e}") });
            }
            (None, issues)
        }
    }
}

/// 严格解析（任何 issue 都是错）。job 模式用它——未知键 = error（D4）。
pub fn from_json_strict(text: &str) -> Result<Config, Vec<ConfigIssue>> {
    match check_text(text) {
        (Some(cfg), issues) if issues.is_empty() => Ok(cfg),
        (_, issues) => Err(issues),
    }
}

/// 生成 executor config 的 JSON Schema（P17 / 1.9）。
///
/// 身份固定：`$id: urn:redteam:config/2`、`title: redteam executor config`、`schemaVersion: 2`；
/// **不嵌 crate 版本**（发布版本变化不该让 schema 漂移）。draft-07 由 schemars 决定。
pub fn config_schema_json() -> serde_json::Value {
    let mut v = serde_json::to_value(schemars::schema_for!(Config)).expect("schema serialize");
    {
        let o = v.as_object_mut().expect("schema object");
        o.insert("$id".into(), serde_json::json!("urn:redteam:config/2"));
        o.insert("title".into(), serde_json::json!("redteam executor config"));
        o.insert("schemaVersion".into(), serde_json::json!(2));
        // executor 契约不含明文密钥：schema 里隐藏本地内联槽（本地反序列化仍支持 apiKey，
        // 由 `check_text` 的 `secret_field` 在 job 模式拒绝）。同时把结构体对象收紧为
        // `additionalProperties:false`——job 模式对未知键就是 ERROR，schema 应与之一致。
        o.insert("additionalProperties".into(), serde_json::json!(false));
    }
    if let Some(defs) = v.get_mut("definitions").and_then(|d| d.as_object_mut()) {
        for (name, def) in defs.iter_mut() {
            if let Some(props) = def.get_mut("properties").and_then(|p| p.as_object_mut()) {
                if name == "ProviderConfig" || name == "ModelConfig" {
                    props.remove("apiKey");
                }
                if def.get("additionalProperties").is_none() {
                    def.as_object_mut().unwrap().insert("additionalProperties".into(), serde_json::json!(false));
                }
            }
        }
    }
    v
}

/// 稳定文本形态（`dist/config.schema.json` 生成物与 `make verify` 对拍用）。
pub fn render_config_schema() -> String {
    let mut s = serde_json::to_string_pretty(&config_schema_json()).expect("schema");
    s.push('\n');
    s
}

/// `from_value` 的错误不带字段路径：按 settings / provider 段逐个试解析，定位第一处失败的路径。
fn locate_shape_error(v: &serde_json::Value) -> Option<String> {
    let top = v.as_object()?;
    if let Some(st) = top.get("settings") {
        if let Some(r) = st.get("review") {
            if serde_json::from_value::<ReviewConfig>(r.clone()).is_err() {
                return Some("/settings/review".into());
            }
        }
        if serde_json::from_value::<Settings>(st.clone()).is_err() {
            return Some("/settings".into());
        }
    }
    if let Some(provs) = top.get("providers").and_then(|p| p.as_object()) {
        for (pname, pv) in provs {
            if let Some(ms) = pv.get("models").and_then(|m| m.as_array()) {
                for (idx, mv) in ms.iter().enumerate() {
                    if serde_json::from_value::<ModelConfig>(mv.clone()).is_err() {
                        return Some(format!("/providers/{}/models/{idx}", ptr_escape(pname)));
                    }
                }
            }
            let mut po = pv.clone();
            if let Some(o) = po.as_object_mut() {
                o.remove("models");
            }
            if serde_json::from_value::<ProviderConfig>(po).is_err() {
                return Some(format!("/providers/{}", ptr_escape(pname)));
            }
        }
    }
    None
}

/// 配置根：`$REDTEAM_HOME`（非空）否则 `$HOME/.redteam`；两者皆缺 → 错误（#1：不退化为相对路径）。
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
    anyhow::bail!("无法定位配置根：请设置 REDTEAM_HOME（绝对路径）或 HOME")
}

pub fn config_path() -> anyhow::Result<PathBuf> {
    Ok(redteam_home()?.join("config.json"))
}

fn comment_only(p: &Path) -> bool {
    match std::fs::read_to_string(p) {
        Ok(t) => t.lines().all(|l| l.trim().is_empty() || l.trim_start().starts_with('#')),
        Err(_) => false,
    }
}

/// 旧路径迁移 notice（`model list` / `model check` 专用）。
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

/// 旧路径探测（只提示不读）：`$XDG_CONFIG_HOME/redteam/config.toml`、`~/.config/redteam/config.toml`、
/// `./.redteam/config.toml`、`$REDTEAM_HOME/config.toml`。纯注释文件不算；canonical 去重。
pub fn legacy_config_paths_present() -> Vec<PathBuf> {
    legacy_paths_present("config.toml")
}

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

/// `model setup` 的显式字段补丁：三层各自只合并给出的字段（D12 定案 5）。
#[derive(Clone, Debug)]
pub struct SetupPatch {
    pub provider: String,
    pub model_id: String,
    pub roles: Vec<Role>,
    pub display_name: Option<String>,
    /// 显式 `--api`：新建 provider 写 provider 层；已存在 provider 时覆盖 provider.api（v2 无模型级 api）。
    pub api: Option<ProviderApi>,
    pub base_url: Option<String>,
    /// 显式 `--api-key-env`：落点规则同 `api`。
    pub api_key_env: Option<String>,
    pub max_concurrency: Option<u32>,
    pub context_window: Option<u32>,
    pub max_output_tokens: Option<u64>,
    pub per_turn_timeout_secs: Option<u64>,
    pub reasoning: Option<bool>,
    pub reasoning_effort: Option<ReasoningEffort>,
    /// 新建 provider 且未显式 `--api` 时的推断值（已存在的 provider 不动）。
    pub api_if_new: ProviderApi,
    /// 新建 provider 且未显式 `--api-key-env` 时的推断值。
    pub api_key_env_if_new: Option<String>,
}

impl Default for SetupPatch {
    fn default() -> Self {
        SetupPatch {
            provider: String::new(),
            model_id: String::new(),
            roles: Vec::new(),
            display_name: None,
            api: None,
            base_url: None,
            api_key_env: None,
            max_concurrency: None,
            context_window: None,
            max_output_tokens: None,
            per_turn_timeout_secs: None,
            reasoning: None,
            reasoning_effort: None,
            api_if_new: ProviderApi::OpenaiCompletions,
            api_key_env_if_new: None,
        }
    }
}

/// `model setup` 的默认端点（Q5 裁决）：显式 > 已知供应商默认 > 报错。
pub fn default_base_for(api: ProviderApi, provider: &str) -> Option<&'static str> {
    match api {
        ProviderApi::AnthropicMessages => Some("https://api.anthropic.com"),
        ProviderApi::OpenaiCompletions => {
            let p = provider.to_ascii_lowercase();
            if p == "openai" || p.starts_with("openai-") || p == "gpt" || p.starts_with("gpt-") {
                Some("https://api.openai.com/v1")
            } else {
                None
            }
        }
        ProviderApi::OpenaiResponses => None,
    }
}

/// 三层显式合并 → 完整解析 + 校验 → 原子落盘（pretty JSON）。现有文件解析失败拒写不覆写。
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

    // provider 层：api / baseUrl / 凭据 / 并发。
    let api = patch.api.unwrap_or(patch.api_if_new);
    if !is_new_provider {
        if let Some(explicit) = patch.api {
            if let Some(current) = po.get("api").and_then(|v| v.as_str()) {
                if current != explicit.as_str() {
                    anyhow::bail!(
                        "providers.{} 当前 api = {current}：v2 的 api 是 provider 级共享通道，单模型无法覆盖；\
                         如确需更换通道，请拆成多个 provider（每个 api 一个）后改角色池引用，不要静默改掉其他模型",
                        patch.provider
                    );
                }
            }
        }
    }
    if is_new_provider || patch.api.is_some() {
        po.insert("api".into(), serde_json::json!(api.as_str()));
    }
    if let Some(b) = &patch.base_url {
        po.insert("baseUrl".into(), serde_json::json!(b));
    } else if is_new_provider && !po.contains_key("baseUrl") {
        match default_base_for(api, &patch.provider) {
            Some(base) => {
                po.insert("baseUrl".into(), serde_json::json!(base));
            }
            None => {
                anyhow::bail!(
                    "providers.{}: v2 的 baseUrl 必填，且 api {} 没有可推断的默认端点；请显式给 --base-url",
                    patch.provider,
                    api.as_str()
                );
            }
        }
    }
    // 新建 provider：env 名写 provider 层；已存在 provider：显式 --api-key-env 写模型层（不动共享 provider 层）。
    let model_api_key_env = if is_new_provider {
        if let Some(e) = patch.api_key_env.as_ref().or(patch.api_key_env_if_new.as_ref()) {
            po.insert("apiKeyEnv".into(), serde_json::json!(e));
        }
        None
    } else {
        patch.api_key_env.clone()
    };
    if let Some(c) = patch.max_concurrency {
        po.insert("maxConcurrency".into(), serde_json::json!(c));
    }

    // 模型层：models 是数组，按 id upsert。
    let models = po.entry("models").or_insert_with(|| serde_json::json!([]));
    let models_a = models.as_array_mut().ok_or_else(|| anyhow::anyhow!("providers.{}.models 不是数组（v2）", patch.provider))?;
    let entry = models_a.iter_mut().find(|m| m.get("id").and_then(|v| v.as_str()) == Some(patch.model_id.as_str()));
    let mv = match entry {
        Some(mv) => mv,
        None => {
            models_a.push(serde_json::json!({"id": patch.model_id}));
            models_a.last_mut().unwrap()
        }
    };
    let mo = mv.as_object_mut().ok_or_else(|| anyhow::anyhow!("models[] 元素不是对象"))?;
    if let Some(n) = &patch.display_name {
        mo.insert("displayName".into(), serde_json::json!(n));
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

    // 角色层：settings 池 upsert（scanner/reviewer/refuter → settings.review.roles.<role>.models；
    // verifier → settings.verify.models；triage → settings.triage.models）。
    for role in &patch.roles {
        let settings = root_o.entry("settings").or_insert_with(|| serde_json::json!({}));
        let so = settings.as_object_mut().ok_or_else(|| anyhow::anyhow!("settings 不是对象"))?;
        let pool_path: (&str, Option<&str>) = match role {
            Role::Scanner => ("review", Some("scanner")),
            Role::Reviewer => ("review", Some("reviewer")),
            Role::Refuter => ("review", Some("refuter")),
            Role::Verifier => ("verify", None),
            Role::Triage => ("triage", None),
        };
        let pool = if let Some(r) = pool_path.1 {
            let review = so.entry("review").or_insert_with(|| serde_json::json!({}));
            let ro = review.as_object_mut().ok_or_else(|| anyhow::anyhow!("settings.review 不是对象"))?;
            let roles = ro.entry("roles").or_insert_with(|| serde_json::json!({}));
            let ro = roles.as_object_mut().ok_or_else(|| anyhow::anyhow!("settings.review.roles 不是对象"))?;
            ro.entry(r).or_insert_with(|| serde_json::json!({"models": []}))
        } else {
            so.entry(pool_path.0).or_insert_with(|| serde_json::json!({"models": []}))
        };
        let po = pool.as_object_mut().ok_or_else(|| anyhow::anyhow!("角色池不是对象"))?;
        let arr = po.entry("models").or_insert_with(|| serde_json::json!([]));
        let arr = arr.as_array_mut().ok_or_else(|| anyhow::anyhow!("角色池 models 不是数组"))?;
        let existing = arr.iter_mut().find(|m| {
            m.get("provider").and_then(|v| v.as_str()) == Some(patch.provider.as_str())
                && m.get("id").and_then(|v| v.as_str()) == Some(patch.model_id.as_str())
        });
        let item = match existing {
            Some(item) => item,
            None => {
                arr.push(serde_json::json!({"provider": patch.provider, "id": patch.model_id}));
                arr.last_mut().unwrap()
            }
        };
        if let (Some(effort), Some(io)) = (patch.reasoning_effort, item.as_object_mut()) {
            io.insert("reasoningEffort".into(), serde_json::json!(effort.as_str()));
        }
    }

    // 写前完整校验（#4）
    let text = serde_json::to_string_pretty(&root)?;
    Config::from_json(&text).map_err(|e| anyhow::anyhow!("合并后配置无效，未写入：{e}"))?;
    // 含内联密钥的配置保持 0600：否则一次 `model setup` 会把原本受限的文件换成默认权限。
    let sensitive = has_secret_key(&root);
    write_atomic(&path, &(text + "\n"), sensitive.then_some(0o600))?;
    Ok(path)
}

/// 配置树里是否含明文密钥槽（`apiKey`，递归）。
fn has_secret_key(v: &serde_json::Value) -> bool {
    match v {
        serde_json::Value::Object(o) => o.iter().any(|(k, child)| k == "apiKey" || has_secret_key(child)),
        serde_json::Value::Array(a) => a.iter().any(has_secret_key),
        _ => false,
    }
}

/// 原子落盘（评审 #6）：同目录临时文件 + rename；`mode` 给定时临时文件即以该权限创建。
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

    const SAMPLE: &str = r#"{
      "settings": {
        "language": "en_US",
        "review": {
          "blockThreshold": "medium", "confFloor": 0.5, "maxConcurrency": 10,
          "scanners": { "samples": 1 },
          "reviewers": { "maxCells": "auto", "reenqueue": false },
          "refuters": { "enabled": true },
          "grouping": { "minFiles": 4, "bundleLineThreshold": 200, "maxFilesPerGroup": 10, "semantic": false },
          "pack": { "body": "slice", "anchorWindow": 30, "findingWindow": 30, "findingWindowEnforce": false, "sliceMinBytes": 8192 },
          "roles": {
            "scanner":  { "models": [{"provider":"deepseek","id":"flash","reasoningEffort":"low"}] },
            "reviewer": { "models": [{"provider":"deepseek","id":"pro","reasoningEffort":"high"}] },
            "refuter":  { "models": [{"provider":"agct","id":"gemini","reasoningEffort":"high"}] }
          }
        },
        "triage": { "models": [{"provider":"deepseek","id":"pro","reasoningEffort":"high"}] },
        "verify": { "models": [{"provider":"deepseek","id":"pro","reasoningEffort":"high"}] }
      },
      "providers": {
        "deepseek": {
          "api": "openai-completions", "baseUrl": "https://api.deepseek.com", "apiKeyEnv": "DEEPSEEK_API_KEY", "maxConcurrency": 20,
          "models": [
            { "id": "flash", "displayName": "Flash", "contextWindow": 1000000, "maxOutputTokens": 384000, "reasoning": true },
            { "id": "pro", "contextWindow": 1000000, "maxOutputTokens": 384000, "perTurnTimeoutSecs": 1200, "reasoning": true }
          ]
        },
        "agct": {
          "api": "anthropic-messages", "baseUrl": "https://api.aigocode.team/v1", "maxConcurrency": 20,
          "models": [
            { "id": "gemini", "apiKeyEnv": "AGCT_GEMINI_API_KEY", "family": "gemini", "contextWindow": 1048576, "reasoning": true }
          ]
        }
      }
    }"#;

    fn cfg(text: &str) -> Config {
        Config::from_json(text).unwrap()
    }

    #[test]
    fn sample_parses_and_role_pools_are_ordered() {
        let c = cfg(SAMPLE);
        let roles = c.active_roles();
        assert_eq!(roles.scanners.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["deepseek/flash"]);
        assert_eq!(roles.reviewers[0].name, "deepseek/pro");
        assert_eq!(roles.refuters[0].name, "agct/gemini");
        assert_eq!(roles.triage[0].name, "deepseek/pro");
        assert_eq!(roles.verifiers[0].name, "deepseek/pro");
        assert_eq!(roles.reviewers[0].reasoning_effort, Some(ReasoningEffort::High));
        assert_eq!(roles.scanners[0].reasoning_effort, Some(ReasoningEffort::Low));
        // 模型固有属性 / family 缺省
        let (_, flash) = c.model("deepseek", "flash").unwrap();
        assert_eq!(flash.display_name.as_deref(), Some("Flash"));
        let (_, gem) = c.model("agct", "gemini").unwrap();
        assert_eq!(gem.family.as_deref(), Some("gemini"));
        assert_eq!(roles.refuters[0].base_url.as_deref(), Some("https://api.aigocode.team/v1"));
        assert_eq!(roles.reviewers[0].max_concurrency, Some(20));
    }

    #[test]
    fn defaults_match_v1_values() {
        let c = Config::default();
        let r = &c.settings.review;
        assert_eq!(r.block_threshold, Severity::Medium);
        assert_eq!(r.conf_floor, 0.5);
        assert_eq!(r.max_concurrency, 10);
        assert_eq!(r.tools, ToolsMode::TwoStage);
        assert_eq!(r.scanners.samples, 1);
        assert_eq!(r.reviewers.max_cells, MaxCells::Auto);
        assert!(!r.reviewers.reenqueue);
        assert!(!r.refuters.enabled, "refuters 缺省关");
        assert_eq!(r.grouping.min_files, 4);
        assert_eq!(r.grouping.bundle_line_threshold, 200);
        assert_eq!(r.grouping.max_files_per_group, 10);
        assert_eq!(r.pack.body, PackBody::Slice);
        assert_eq!((r.pack.anchor_window, r.pack.finding_window, r.pack.slice_min_bytes), (30, 30, 8192));
        assert!(!r.pack.finding_window_enforce);
    }

    #[test]
    fn validate_aggregates_structural_errors() {
        let e = Config::from_json(
            r#"{"settings":{"review":{"confFloor":1.5,"reviewers":{"maxCells":0},"pack":{"anchorWindow":0}}},"providers":{"p":{"api":"openai-completions","baseUrl":"","models":[{"id":"m","contextWindow":1000,"maxOutputTokens":0,"perTurnTimeoutSecs":0}]}}}"#,
        )
        .unwrap_err()
        .to_string();
        for k in ["baseUrl 必填", "contextWindow 1000", "maxOutputTokens 必须 ≥ 1", "perTurnTimeoutSecs 必须 ≥ 1", "auto", "confFloor 1.5", "anchorWindow 必须 ≥ 1"] {
            assert!(e.contains(k), "缺 {k}: {e}");
        }
    }

    #[test]
    fn semantic_problems_have_pointers_and_codes() {
        let c: Config = serde_json::from_str(
            r#"{"settings":{"review":{"roles":{"reviewer":{"models":[
                    {"provider":"p","id":"nope"},
                    {"provider":"p","id":"m"},
                    {"provider":"p","id":"m"}
                ]}}},"verify":{"models":[{"provider":"p","id":"m"}]}},
               "providers":{"p":{"api":"openai-responses","baseUrl":"https://x","models":[{"id":"m"},{"id":"m"}]}}}"#,
        )
        .unwrap();
        let ps = c.semantic_problems();
        assert!(ps.iter().any(|p| p.code == "api_not_implemented" && p.pointer == "/providers/p/api"), "{ps:?}");
        assert!(ps.iter().any(|p| p.code == "duplicate_model" && p.pointer == "/providers/p/models/1"), "{ps:?}");
        assert!(ps.iter().any(|p| p.code == "unknown_model" && p.pointer == "/settings/review/roles/reviewer/models/0"), "{ps:?}");
        assert!(ps.iter().any(|p| p.code == "duplicate_model" && p.pointer == "/settings/review/roles/reviewer/models/2"), "{ps:?}");
    }

    #[test]
    fn key_resolution_chain_six_levels() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("M_ENV", "model-env");
            std::env::set_var("P_ENV", "provider-env");
        }
        let mut c = cfg(
            r#"{"settings":{"review":{"roles":{"reviewer":{"models":[{"provider":"p","id":"m"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"m"}]}}}"#,
        );
        // 6 → provider secrets
        let mut secrets = secrets::Secrets::default();
        secrets.providers.insert("p".into(), secrets::ProviderSecrets { api_key: Some("secret-provider".into()), models: BTreeMap::new() });
        assert_eq!(c.resolve_key_with("p", "m", &secrets).as_deref(), Some("secret-provider"));
        // 5 → provider env
        c.providers.get_mut("p").unwrap().api_key_env = Some("P_ENV".into());
        assert_eq!(c.resolve_key_with("p", "m", &secrets).as_deref(), Some("provider-env"));
        // 4 → provider inline
        c.providers.get_mut("p").unwrap().api_key = Some("provider-inline".into());
        assert_eq!(c.resolve_key_with("p", "m", &secrets).as_deref(), Some("provider-inline"));
        // 3 → secrets.models
        let mut secrets = secrets::Secrets::default();
        let mut ps = secrets::ProviderSecrets::default();
        ps.models.insert("m".into(), "secret-model".into());
        secrets.providers.insert("p".into(), ps);
        assert_eq!(c.resolve_key_with("p", "m", &secrets).as_deref(), Some("secret-model"));
        // 2 → model env
        c.providers.get_mut("p").unwrap().models[0].api_key_env = Some("M_ENV".into());
        assert_eq!(c.resolve_key_with("p", "m", &secrets).as_deref(), Some("model-env"));
        // 1 → model inline（空白视为未设）
        c.providers.get_mut("p").unwrap().models[0].api_key = Some("model-inline".into());
        assert_eq!(c.resolve_key_with("p", "m", &secrets).as_deref(), Some("model-inline"));
        c.providers.get_mut("p").unwrap().models[0].api_key = Some("   ".into());
        assert_eq!(c.resolve_key_with("p", "m", &secrets).as_deref(), Some("model-env"));
        unsafe {
            std::env::remove_var("M_ENV");
            std::env::remove_var("P_ENV");
        }
    }

    #[test]
    fn thinking_level_map_parses_null_and_strings() {
        let c = cfg(
            r#"{"settings":{"review":{"roles":{"reviewer":{"models":[{"provider":"p","id":"m","reasoningEffort":"high"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"m","thinkingLevelMap":{"high":"max","low":null}}]}}}"#,
        );
        let m = &c.active_roles().reviewers[0];
        let map = m.thinking_level_map.as_ref().unwrap();
        assert_eq!(map.value(ReasoningEffort::High), Some("max"));
        assert_eq!(map.value(ReasoningEffort::Low), None, "显式 null → 抑制");
        assert_eq!(map.value(ReasoningEffort::Medium), None, "缺键 → 抑制");
    }

    #[test]
    fn shape_guard_gives_per_field_migration_hints() {
        let cases: &[(&str, &[&str])] = &[
            (
                r#"{"language":"en_US"}"#,
                &["settings.language"],
            ),
            (
                r#"{"review":{"blockThreshold":"high"}}"#,
                &["settings.review"],
            ),
            (
                r#"{"providers":{"p":{"kind":"openai","baseUrl":"https://x","models":[]}}}"#,
                &["api", "openai-completions"],
            ),
            (
                r#"{"providers":{"p":{"kind":"google","baseUrl":"https://x","models":[]}}}"#,
                &["google 已移除"],
            ),
            (
                r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":{"m":{}}}}}"#,
                &["数组", "id"],
            ),
            (
                r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":{"m":{"roles":{"verifier":{}}}}}}}"#,
                &["roles 已删除", "settings.verify.models"],
            ),
            (
                r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"m","kind":"anthropic"}]}}}"#,
                &["kind 已删除", "拆 provider"],
            ),
            (
                r#"{"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"m","roles":{"scanner":{}}}]}}}"#,
                &["roles 已删除", "settings.review.roles"],
            ),
            (
                r#"{"settings":{"review":{"reviewers":{"max_cells":4}}}}"#,
                &["camelCase", "max_cells"],
            ),
        ];
        for (json, needles) in cases {
            let e = Config::from_json(json).unwrap_err().to_string();
            for n in *needles {
                assert!(e.contains(n), "json={json}\n缺 {n}:\n{e}");
            }
        }
    }

    #[test]
    fn v1_full_shape_hints_every_key() {
        let v1 = r#"{
          "language": "en_US",
          "providers": {"p": {"kind": "openai-compatible", "baseUrl": "https://x", "apiKeyEnv": "K", "maxConcurrency": 2,
            "models": {"m": {"displayName": "M", "family": "deepseek", "contextWindow": 128000, "maxOutputTokens": 4096,
              "perTurnTimeoutSecs": 60, "reasoning": true, "roles": {"scanner": {"reasoningEffort": "low"}, "verifier": {}}}}}},
          "review": {"blockThreshold": "medium", "confFloor": 0.5, "maxConcurrency": 10, "tools": "two-stage",
            "scanners": {"samples": 1}, "reviewers": {"maxCells": "auto", "reenqueue": false}, "refuters": {"enabled": true},
            "grouping": {"minFiles": 4, "bundleLineThreshold": 200, "maxFilesPerGroup": 10, "semantic": false},
            "pack": {"body": "slice", "anchorWindow": 30, "findingWindow": 30, "findingWindowEnforce": false, "sliceMinBytes": 8192}}
        }"#;
        let (cfg, issues) = check_text(v1);
        assert!(cfg.is_none());
        let legacy: Vec<&ConfigIssue> = issues.iter().filter(|i| i.code == "legacy_shape").collect();
        let ptrs: Vec<&str> = legacy.iter().map(|i| i.pointer.as_str()).collect();
        for ptr in [
            "/language",
            "/review",
            "/review/blockThreshold",
            "/review/scanners/samples",
            "/review/reviewers/maxCells",
            "/review/reviewers/reenqueue",
            "/review/refuters/enabled",
            "/review/grouping/minFiles",
            "/review/pack/sliceMinBytes",
            "/providers/p/kind",
            "/providers/p/models",
            "/providers/p/models/m/roles",
            "/providers/p/models/m/roles/scanner",
            "/providers/p/models/m/roles/verifier",
        ] {
            assert!(ptrs.contains(&ptr), "缺 pointer {ptr}: {ptrs:?}");
        }
        // 逐字段目标路径出现，且不叠 unknown_key（同一批旧键交给 legacy_shape）。
        let blob = legacy.iter().map(|i| i.message.as_str()).collect::<Vec<_>>().join("\n");
        for needle in ["settings.review.refuters.enabled", "settings.review.reviewers.maxCells", "openai-completions", "settings.verify.models[]"] {
            assert!(blob.contains(needle), "缺 {needle}:\n{blob}");
        }
        assert!(!issues.iter().any(|i| i.code == "unknown_key"), "{issues:?}");
        // CLI 诊断必须单行。
        assert!(legacy.iter().all(|i| !i.message.contains('\n')), "{legacy:?}");
    }

    #[test]
    fn kind_aliases_each_get_a_concrete_target() {
        for (alias, needle) in [
            ("gpt", "openai-completions"),
            ("openai_compatible", "openai-completions"),
            ("compatible", "openai-completions"),
            ("claude", "anthropic-messages"),
            ("gemini", "google 已移除"),
        ] {
            let json = format!(
                r#"{{"providers":{{"p":{{"kind":"{alias}","baseUrl":"https://x","models":[]}}}}}}"#
            );
            let e = Config::from_json(&json).unwrap_err().to_string();
            assert!(e.contains(needle), "alias={alias} 缺 {needle}:\n{e}");
        }
    }

    #[test]
    fn removed_keys_still_warn_but_do_not_fail() {
        let (c, w) = Config::from_json_warned(
            r#"{"settings":{"review":{"risk":{"mode":"x"},"reviewers":{"workers":2},"scanners":{"samples":1}}},"providers":{}}"#,
        )
        .unwrap();
        assert!(c.settings.review.scanners.samples == 1);
        assert!(w.iter().any(|x| x.contains("risk 已删除")), "{w:?}");
        assert!(w.iter().any(|x| x.contains("workers 已删除")), "{w:?}");
    }

    #[test]
    fn strict_scan_flags_unknown_keys_and_secrets() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"settings":{"review":{"unknown":1},"lang":"x"},"providers":{"p":{"api":"openai-completions","apiKey":"sk-x","baseUrl":"https://x","models":[{"id":"m","extra":1}]}}}"#,
        )
        .unwrap();
        let issues = strict_issues(&v);
        let has = |code: &str, ptr: &str| issues.iter().any(|i| i.code == code && i.pointer == ptr);
        assert!(has("unknown_key", "/settings/review/unknown"), "{issues:?}");
        assert!(has("unknown_key", "/settings/lang"), "{issues:?}");
        assert!(has("unknown_key", "/providers/p/models/0/extra"), "{issues:?}");
        assert!(has("secret_field", "/providers/p/apiKey"), "{issues:?}");
        assert!(issues.iter().all(|i| !i.message.contains("sk-x")), "诊断不得输出密钥值");
    }

    #[test]
    fn local_config_allows_inline_api_key_job_strict_does_not() {
        let text = r#"{"settings":{"review":{"roles":{"reviewer":{"models":[{"provider":"p","id":"m"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","apiKey":"sk-local","models":[{"id":"m"}]}}}"#;
        assert!(Config::from_json(text).is_ok(), "本地允许内联 apiKey（Q2）");
        let (_, issues) = check_text(text);
        assert!(issues.iter().any(|i| i.code == "secret_field"), "{issues:?}");
        // 空串同样拒绝
        let empty = text.replace("sk-local", "");
        assert!(check_text(&empty).1.iter().any(|i| i.code == "secret_field"), "空串同样拒绝");
        let null_key = text.replace("\"apiKey\":\"sk-local\"", "\"apiKey\":null");
        assert!(check_text(&null_key).1.iter().any(|i| i.code == "secret_field"), "null 同样拒绝");
        let model_level = r#"{"settings":{"review":{"roles":{"reviewer":{"models":[{"provider":"p","id":"m"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"m","apiKey":"sk-model"}]}}}"#;
        let issues = check_text(model_level).1;
        assert!(issues.iter().any(|i| i.code == "secret_field" && i.pointer == "/providers/p/models/0/apiKey"), "{issues:?}");
        assert!(issues.iter().all(|i| !i.message.contains("sk-model")), "不得回显密钥值");
    }

    #[test]
    fn refute_enabled_split_local_falls_back_job_skips() {
        let c = cfg(
            r#"{"settings":{"review":{"refuters":{"enabled":true},"roles":{"reviewer":{"models":[{"provider":"p","id":"r"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"r"}]}}}"#,
        );
        let roles = c.active_roles();
        assert!(c.refute_enabled(false, &roles), "本地池空仍走自动异族回退");
        assert!(!c.refute_enabled(true, &roles), "job 模式池空 → 跳过");
        let mut c2 = c.clone();
        c2.settings.review.refuters.enabled = false;
        assert!(!c2.refute_enabled(false, &roles));
    }

    #[test]
    fn model_unused_warns_and_secret_warnings_still_work() {
        let (_c, w) = Config::from_json_warned(
            r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"p","id":"s"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"s"},{"id":"idle"}]}}}"#,
        )
        .unwrap();
        assert!(w.iter().any(|x| x.contains("idle") && x.contains("未被任何角色池引用")), "{w:?}");
        let c = cfg(
            r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"p","id":"s"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"s"}]}}}"#,
        );
        let mut s = secrets::Secrets::default();
        s.providers.insert("other".into(), secrets::ProviderSecrets::default());
        let mut ps = secrets::ProviderSecrets::default();
        ps.models.insert("ghost".into(), "k".into());
        s.providers.insert("p".into(), ps);
        let ws = c.secret_key_warnings(&s);
        assert!(ws.iter().any(|w| w.contains("other")), "{ws:?}");
        assert!(ws.iter().any(|w| w.contains("ghost")), "{ws:?}");
    }

    #[test]
    fn output_language_resolution_order() {
        let mut c = Config::default();
        assert_eq!(OutputLang::resolve_job(&c).value, "en_US");
        assert_eq!(OutputLang::resolve_job(&c).source, LangSource::Default);
        c.settings.language = Some(Language::ZhCn);
        assert_eq!(OutputLang::resolve_job(&c).value, "zh_CN");
        assert_eq!(OutputLang::resolve_job(&c).source, LangSource::Config);
    }

    #[test]
    fn locate_shape_error_points_at_settings_review() {
        let err = Config::from_json(r#"{"settings":{"review":{"confFloor":"high"}}}"#).unwrap_err().to_string();
        assert!(err.contains("/settings/review"), "{err}");
    }

    #[test]
    fn legacy_paths_still_detected() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        let rh = d.path().join("rh");
        std::fs::create_dir_all(&rh).unwrap();
        std::fs::write(rh.join("config.toml"), "[x]\n").unwrap();
        std::fs::write(rh.join("secrets.toml"), "[x]\n").unwrap();
        let old_rh = std::env::var_os("REDTEAM_HOME");
        unsafe { std::env::set_var("REDTEAM_HOME", &rh); }
        let cs = legacy_config_paths_present();
        let ss = legacy_secrets_paths_present();
        assert!(cs.iter().any(|p| p == &rh.join("config.toml")), "{cs:?}");
        assert!(ss.iter().any(|p| p == &rh.join("secrets.toml")), "{ss:?}");
        let notices = legacy_migration_notices();
        assert!(notices.iter().any(|n| n.contains("config.toml")), "{notices:?}");
        unsafe {
            match old_rh { Some(h) => std::env::set_var("REDTEAM_HOME", h), None => std::env::remove_var("REDTEAM_HOME") }
        }
    }

    fn patch(provider: &str, id: &str, roles: &[Role]) -> SetupPatch {
        SetupPatch {
            provider: provider.into(),
            model_id: id.into(),
            roles: roles.to_vec(),
            api_if_new: ProviderApi::OpenaiCompletions,
            ..Default::default()
        }
    }

    #[test]
    fn write_setup_writes_v2_and_merges_explicit_fields_only() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("REDTEAM_HOME", d.path()); }
        let mut p = patch("deepseek", "flash", &[Role::Scanner, Role::Reviewer]);
        p.base_url = Some("https://api.deepseek.com".into());
        p.api_key_env = Some("DS_KEY".into());
        p.reasoning_effort = Some(ReasoningEffort::High);
        write_setup(&p).unwrap();

        let mut p2 = patch("deepseek", "flash", &[Role::Verifier, Role::Triage]);
        p2.max_output_tokens = Some(4096);
        write_setup(&p2).unwrap();

        let c = cfg(&std::fs::read_to_string(d.path().join("config.json")).unwrap());
        let pc = c.providers.get("deepseek").unwrap();
        assert_eq!(pc.api, ProviderApi::OpenaiCompletions);
        assert_eq!(pc.base_url, "https://api.deepseek.com");
        assert_eq!(pc.api_key_env.as_deref(), Some("DS_KEY"));
        assert_eq!(pc.models.len(), 1, "同 id upsert，不重复");
        assert_eq!(pc.models[0].id, "flash");
        assert_eq!(pc.models[0].max_output_tokens, Some(4096));
        assert!(c.settings.review.roles.scanner.models.iter().any(|r| r.id == "flash"));
        assert!(c.settings.review.roles.reviewer.models.iter().any(|r| r.id == "flash"));
        assert!(c.settings.verify.models.iter().any(|r| r.id == "flash"));
        assert!(c.settings.triage.models.iter().any(|r| r.id == "flash"));
        assert_eq!(c.settings.review.roles.scanner.models[0].reasoning_effort, Some(ReasoningEffort::High));
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn write_setup_requires_base_url_without_known_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("REDTEAM_HOME", d.path()); }
        let mut p = patch("my-gateway", "m", &[Role::Scanner]);
        p.api_if_new = ProviderApi::OpenaiCompletions;
        let e = write_setup(&p).unwrap_err().to_string();
        assert!(e.contains("baseUrl") && e.contains("--base-url"), "{e}");
        // 已知供应商可推断
        let mut ok = patch("openai", "gpt-5", &[Role::Scanner]);
        ok.api_if_new = ProviderApi::OpenaiCompletions;
        assert!(write_setup(&ok).is_ok());
        let c = cfg(&std::fs::read_to_string(d.path().join("config.json")).unwrap());
        assert_eq!(c.providers["openai"].base_url, "https://api.openai.com/v1");
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn write_setup_refuses_v1_existing_config() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("REDTEAM_HOME", d.path()); }
        std::fs::write(d.path().join("config.json"), r#"{"language":"en_US","providers":{}}"#).unwrap();
        let mut p = patch("p", "m", &[Role::Scanner]);
        p.base_url = Some("https://x".into());
        let e = write_setup(&p).unwrap_err().to_string();
        assert!(e.contains("合并后配置无效") && e.contains("settings.language"), "{e}");
        let raw = std::fs::read_to_string(d.path().join("config.json")).unwrap();
        assert!(raw.contains("\"language\""), "拒写不覆写");
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn schema_hides_inline_api_key_and_denies_unknown() {
        let v = config_schema_json();
        assert!(v["definitions"]["ProviderConfig"]["properties"].get("apiKey").is_none(), "executor schema 不得暴露 provider.apiKey");
        assert!(v["definitions"]["ModelConfig"]["properties"].get("apiKey").is_none(), "executor schema 不得暴露 model.apiKey");
        assert_eq!(v["additionalProperties"], false);
        assert_eq!(v["definitions"]["ProviderConfig"]["additionalProperties"], false);
        assert_eq!(v["definitions"]["ModelConfig"]["additionalProperties"], false);
        assert_eq!(v["definitions"]["ProviderConfig"]["required"], serde_json::json!(["api", "baseUrl"]));
        assert_eq!(v["definitions"]["ModelConfig"]["required"], serde_json::json!(["id"]));
    }

    #[test]
    fn job_env_only_never_reads_inline_or_secrets() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::set_var("P_ONLY_ENV", "env-value"); std::env::remove_var("M_ONLY_ENV"); }
        let c = cfg(
            r#"{"settings":{"review":{"roles":{"reviewer":{"models":[{"provider":"p","id":"m","apiKeyEnv":"M_ONLY_ENV"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","apiKey":"provider-inline","models":[{"id":"m","apiKey":"model-inline"}]}}}"#,
        );
        let mut secrets = secrets::Secrets::default();
        let mut ps = secrets::ProviderSecrets::default();
        ps.models.insert("m".into(), "secret-model".into());
        ps.api_key = Some("secret-provider".into());
        secrets.providers.insert("p".into(), ps);
        // 本地链读到内联
        assert_eq!(c.resolve_key_with("p", "m", &secrets).as_deref(), Some("model-inline"));
        // job 链：模型 env 未设 → provider env 未设 → None（不碰内联/secrets）
        assert_eq!(c.resolve_key_env_only("p", "m"), None);
        c.providers.get("p").unwrap();
        let mut c2 = c.clone();
        c2.providers.get_mut("p").unwrap().api_key_env = Some("P_ONLY_ENV".into());
        assert_eq!(c2.resolve_key_env_only("p", "m").as_deref(), Some("env-value"));
        unsafe { std::env::remove_var("P_ONLY_ENV"); }
    }

    #[test]
    fn write_setup_preserves_0600_and_inline_key() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("REDTEAM_HOME", d.path()); }
        let path = d.path().join("config.json");
        let text = r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"p","id":"m"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","apiKey":"sk-local","models":[{"id":"m"}]}}}"#;
        std::fs::write(&path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let mut p = patch("p", "m2", &[Role::Reviewer]);
        p.api_key_env = Some("P2_KEY".into());
        write_setup(&p).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(v["providers"]["p"]["apiKey"], "sk-local", "内联密钥不得被写盘抹掉");
        assert!(v["providers"]["p"]["models"].as_array().unwrap().iter().any(|m| m["id"] == "m2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "含内联密钥的配置重写后必须仍是 0600");
        }
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn write_setup_refuses_cross_api_on_existing_provider() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("REDTEAM_HOME", d.path()); }
        let path = d.path().join("config.json");
        std::fs::write(&path, r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"p","id":"m"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"m"}]}}}"#).unwrap();
        let mut p = patch("p", "m", &[Role::Scanner]);
        p.api = Some(ProviderApi::AnthropicMessages);
        let e = write_setup(&p).unwrap_err().to_string();
        assert!(e.contains("共享通道") && e.contains("拆成多个 provider"), "{e}");
        // 未变 api 的显式写法允许
        let mut same = patch("p", "m", &[Role::Scanner]);
        same.api = Some(ProviderApi::OpenaiCompletions);
        assert!(write_setup(&same).is_ok());
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn per_turn_priority_model_env_default() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        unsafe { std::env::remove_var("REDTEAM_PER_TURN_SECS"); }
        let mut em = EffectiveModel {
            name: "p/m".into(), display_name: None, id: "m".into(), provider: "p".into(), role: Role::Reviewer,
            api: ProviderApi::OpenaiCompletions, base_url: Some("https://x".into()), context_window: 128_000,
            max_output_tokens: Some(1000), per_turn_timeout_secs: None, reasoning: false, reasoning_effort: None,
            thinking_level_map: None, family: "m".into(), max_concurrency: None,
        };
        assert_eq!(em.per_turn_secs(), 1000 / 20 + 60, "公式");
        unsafe { std::env::set_var("REDTEAM_PER_TURN_SECS", "77"); }
        assert_eq!(em.per_turn_secs(), 77, "env");
        em.per_turn_timeout_secs = Some(99);
        assert_eq!(em.per_turn_secs(), 99, "模型字段优先");
        unsafe { std::env::remove_var("REDTEAM_PER_TURN_SECS"); }
    }

    #[test]
    fn home_resolution_and_relative_home_errors() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let old_rh = std::env::var_os("REDTEAM_HOME");
        unsafe { std::env::set_var("REDTEAM_HOME", "/abs/rh"); }
        assert_eq!(redteam_home().unwrap(), PathBuf::from("/abs/rh"));
        unsafe { std::env::set_var("REDTEAM_HOME", "relative"); }
        assert!(redteam_home().unwrap_err().to_string().contains("绝对路径"));
        unsafe {
            match old_rh { Some(h) => std::env::set_var("REDTEAM_HOME", h), None => std::env::remove_var("REDTEAM_HOME") }
        }
    }

    #[test]
    fn load_no_secrets_skips_invalid_secrets_file() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let d = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("REDTEAM_HOME", d.path()); }
        std::fs::write(d.path().join("config.json"), r#"{"providers":{},"settings":{}}"#).unwrap();
        std::fs::write(d.path().join("secrets.json"), "{ not json").unwrap();
        let (_, msgs) = Config::load_no_secrets().unwrap();
        assert!(!msgs.iter().any(|m| m.contains("secrets.json")), "{msgs:?}");
        let (_, msgs2) = Config::load_with_messages().unwrap();
        assert!(msgs2.iter().any(|m| m.contains("secrets.json")), "{msgs2:?}");
        unsafe { std::env::remove_var("REDTEAM_HOME"); }
    }

    #[test]
    fn default_base_for_known_vendors_only() {
        assert_eq!(default_base_for(ProviderApi::AnthropicMessages, "any"), Some("https://api.anthropic.com"));
        assert_eq!(default_base_for(ProviderApi::OpenaiCompletions, "openai"), Some("https://api.openai.com/v1"));
        assert_eq!(default_base_for(ProviderApi::OpenaiCompletions, "deepseek"), None);
        assert_eq!(default_base_for(ProviderApi::OpenaiResponses, "openai"), None);
    }
}
