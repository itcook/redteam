pub mod backends;
pub mod fake;
pub mod parse;
pub mod registry;
pub mod scan_shot;
pub mod single_shot;
pub mod stream_json;
pub mod tool_loop;

use crate::domain::RawFinding;
use async_trait::async_trait;

#[derive(Clone, Debug)]
pub struct LensRequest { pub route_key: String, pub prompt: String }

/// iterations：本次调用实际消耗的 tool-loop 逻辑迭代数（计 FINALIZE 强制收尾轮、
/// 不计 turn 级 retry 重试）。成功恒 >=1（单发单收即 1）；0 保留给「失败/未观测」。
#[derive(Clone, Debug)]
pub struct LensCall {
    pub findings: Vec<RawFinding>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    /// 逻辑迭代数：探索轮 + 收尾轮 + 修复轮（**不含**二阶段补查，那条单独记）。
    pub iterations: u32,
    /// 其中**修复轮**的数量（0 或 1：解析失败后至多补发一次 `REPAIR_PROMPT`）。
    ///
    /// 从 `iterations` 里单独标出来（schema 批 B-4，2026-10-04）：切名门的判据是「修复轮为 0」，
    /// 而 `iterations` 把探索轮、收尾轮、修复轮揉在一个数里，判据没法自动化。
    /// PR181 那轮 24 格 `attempts>1` 一度被误判成修复轮，实际全是二阶段补查——
    /// 靠 `needs_two_stage_triggered` 恰好等于 24 才辨清。
    pub repair_rounds: u32,
    pub tool_use: crate::tools::ToolUse,
    pub needs: Vec<crate::domain::NeedEntry>,
}

#[derive(thiserror::Error, Debug)]
pub enum ProviderError {
    #[error("rate limited")]
    RateLimited { retry_after: Option<std::time::Duration> }, // 429 → 可重试
    // ponytail: Display 不内嵌 retry_after（thiserror 格式串放不下方法链）；需要时日志侧单独打
    #[error("server error: {0}")]
    Server(String),      // 5xx → 可重试
    #[error("timeout: {0}")]
    Timeout(String),     // 408 / reqwest timeout / per-turn 超时 → 可重试
    #[error("network error: {0}")]
    Network(String),     // 连接重置/DNS/connect → 可重试
    #[error("auth error: {0}")]
    Auth(String),        // 401/403 → 不可重试
    #[error("bad request: {0}")]
    BadRequest(String),  // 400/413/422 含 context-too-long → 不可重试
    #[error("structured-output parse failed: {0}")]
    Parse(String),       // 我方 JSON 解析失败 → 不可重试
    #[error("provider unavailable: {0}")]
    Unavailable(String), // 未知兜底 → 保守可重试
    /// T25：整链失败 token 入账——多轮 tool_loop/single_shot 在最终报错前已真实烧掉的
    /// usage，原先随 `?`/Err 一起被丢弃（`orchestrate::run_one_bundle` 候补链全灭时
    /// `CellRun.tokens` 恒 (0,0,0)，与真实花费不符）。只在 usage 非零时才包一层
    /// （见 `attach_usage`），零使用量的错误路径（如首轮就 Auth 失败）字节级不变，
    /// 现有 `matches!(err, ProviderError::X(_))` 断言不受影响。
    #[error("{source}")]
    WithUsage {
        #[source]
        source: Box<ProviderError>,
        /// (input, output, cached_input) —— 与 `LensCall`/`CellRun.tokens` 同口径。
        usage: (u64, u64, u64),
    },
}

impl ProviderError {
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::RateLimited { .. }
            | ProviderError::Server(_)
            | ProviderError::Timeout(_)
            | ProviderError::Network(_)
            | ProviderError::Unavailable(_) => true,
            ProviderError::WithUsage { source, .. } => source.is_retryable(),
            _ => false,
        }
    }
    pub fn retry_after(&self) -> Option<std::time::Duration> {
        match self {
            ProviderError::RateLimited { retry_after } => *retry_after,
            ProviderError::WithUsage { source, .. } => source.retry_after(),
            _ => None,
        }
    }
    /// 本错误携带的已烧 usage；非 `WithUsage` 恒 (0,0,0)（未观测/无花费两种情形不区分，
    /// 调用方原本就以 0 处理）。
    pub fn burned_usage(&self) -> (u64, u64, u64) {
        match self {
            ProviderError::WithUsage { usage, .. } => *usage,
            _ => (0, 0, 0),
        }
    }
    /// 见穿 `WithUsage` 包装取内层错误的引用——`verify.rs` 等按变体分派的调用点用它
    /// 兼容新包装，不必对每个匹配点都补一层 `WithUsage` 分支。
    pub fn inner(&self) -> &ProviderError {
        match self {
            ProviderError::WithUsage { source, .. } => source.inner(),
            other => other,
        }
    }
    /// 把已烧 usage 附着到错误上；usage 全零时原样返回（不引入包装，保持既有错误路径
    /// 字节级不变）——多轮调用中途失败、且之前的轮次真花费了 token 时才需要它。
    pub fn attach_usage(self, usage: (u64, u64, u64)) -> ProviderError {
        if usage == (0, 0, 0) {
            self
        } else {
            ProviderError::WithUsage { source: Box::new(self), usage }
        }
    }
}

/// 发请求 + 按 HTTP 语义分类。成功返回 Response（调用方再 `.json()`）。
pub(crate) async fn send_classified(
    rb: reqwest::RequestBuilder,
) -> Result<reqwest::Response, ProviderError> {
    let resp = rb.send().await.map_err(classify_transport)?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    Err(classify_status(status, resp.headers()))
}

/// 把 2xx 响应体读为 JSON。**envelope 层**解码失败（空 body / 纯 keep-alive 空白 /
/// 非 JSON 网关页——典型上游抖动或推理模型长思考后放弃）分类为 `Unavailable`（可重试），
/// 而非 `Parse`。`Parse` 专指我方对**模型内容**的形状解析失败（不可重试）。
/// 这是本层与 `retry_classified` 的接缝：误判为 Parse 会绕过重试，一次抖动烧掉整个 cell。
/// 现由 Anthropic backend 的非 SSE 回退路径直接消费（SSE 主路径不需要整体 JSON 解码）。
pub(crate) fn parse_envelope_json(text: &str, url: &str) -> Result<serde_json::Value, ProviderError> {
    serde_json::from_str(text).map_err(|e| {
        let preview: String = text.chars().take(200).collect();
        ProviderError::Unavailable(format!(
            "envelope decode failed for {url}: {e}; body[0..200]={preview:?}"
        ))
    })
}

/// reqwest 传输层错误（非 HTTP 状态）：timeout/connect/其他保守 Network。
fn classify_transport(e: reqwest::Error) -> ProviderError {
    if e.is_timeout() {
        ProviderError::Timeout(e.to_string())
    } else {
        // connect / reset / dns / 请求构造：均可重试类，归 Network
        ProviderError::Network(e.to_string())
    }
}

/// HTTP 状态码 + 头 → 分类。
fn classify_status(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
) -> ProviderError {
    use reqwest::StatusCode as S;
    match status {
        S::TOO_MANY_REQUESTS => ProviderError::RateLimited { retry_after: parse_retry_after(headers) },
        // 529 = Anthropic 系 overloaded：并发密度限流的另一种写法，与 SSE `overloaded_error` 事件同归 RateLimited
        // → 进 provider 门的 429 排队重试（D13），不按 5xx 抖动退避烧格。
        s if s.as_u16() == 529 => ProviderError::RateLimited { retry_after: parse_retry_after(headers) },
        S::UNAUTHORIZED | S::FORBIDDEN => ProviderError::Auth(format!("http {status}")),
        S::REQUEST_TIMEOUT => ProviderError::Timeout(format!("http {status}")),
        s if s.is_server_error() => ProviderError::Server(format!("http {status}")),
        s if s.is_client_error() => ProviderError::BadRequest(format!("http {status}")),
        _ => ProviderError::Unavailable(format!("http {status}")),
    }
}

/// 只解 integer-seconds 形式的 Retry-After（HTTP-date 忽略，决策④）。
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<std::time::Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str().ok()?
        .trim()
        .parse::<u64>().ok()
        .map(std::time::Duration::from_secs)
}

/// 首发 + 4 重试（bin 侧 build_registry 装 scan / reviewer / verifier 条目都用它，批 F-2）。
/// M21b 出口验收金丝雀：原 2 次重试 + 0.4 / 0.8 s 退避，一阵持续数秒的网关 503 一次打掉 11 格（每格 5–6 s 就放弃）；
/// 抬到 4 并配 `backoff_for` 的瞬断长退避，能扛约一分钟的抖动。超时另封顶 `TIMEOUT_RETRY_CAP`。
pub const DEFAULT_RETRY_BUDGET: u32 = 4;
/// 超时类重试封顶：每次超时已经烧掉一整个 per-turn（最长 150 s+），不跟着预算放大。
const TIMEOUT_RETRY_CAP: u32 = 2;
/// 单轮默认最长等待；tool_loop 与 single_shot 共用（原各私有一份，Task 2 评审 Minor 收敛）。
pub(crate) const PER_TURN_DEADLINE: std::time::Duration = std::time::Duration::from_secs(150);
/// 单轮最长等待时间，env `REDTEAM_PER_TURN_SECS` 可覆盖——大上下文 bundle（40+ 文件）
/// 配深思考模型（glm/kimi 等）单轮思考+生成可超 150s 被掐成 `turn timeout`（2026-09-03
/// dogfood 实证，非网络不可达而是慢）。校准旋钮，对齐 `REDTEAM_HTTP_TIMEOUT_SECS`（9f7540b）
/// 先例。HTTP 整请求超时由 `backend_from_config` 按该模型 per_turn 自动抬（`http_timeout_for`，评审 M2），
/// 不必再手动同步 `REDTEAM_HTTP_TIMEOUT_SECS`；env 解析与 `config::EffectiveModel::per_turn_secs` 共用 `config::env_per_turn_secs`（评审 S6）。
pub fn per_turn_deadline() -> std::time::Duration {
    crate::config::env_per_turn_secs().map(std::time::Duration::from_secs).unwrap_or(PER_TURN_DEADLINE)
}
const RETRY_BACKOFF_BASE_MS: u64 = 200;
const RETRY_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(5);
const RETRY_AFTER_CAP: std::time::Duration = std::time::Duration::from_secs(30);
/// 网关瞬断（5xx / 断网 / 不可用）的退避：2 → 4 → 8 → 16 s，封顶 30 s。
const TRANSIENT_BACKOFF_BASE_MS: u64 = 2_000;
const TRANSIENT_BACKOFF_CAP: std::time::Duration = std::time::Duration::from_secs(30);

/// 第 `attempt` 次失败（0 起）后等多久再发。带 `Retry-After` 的（**只有 429 / 529**——`classify_status`
/// 不给 5xx 挂这个头，收口评审 #3）听服务端的并封顶；
/// 网关瞬断走长退避——短退避在一阵故障里几秒就把预算烧光；超时等其余错误维持原短退避（超时本身已等够久）。
fn backoff_for(e: &ProviderError, attempt: u32) -> std::time::Duration {
    if let Some(d) = e.retry_after() {
        return d.min(RETRY_AFTER_CAP);
    }
    match e.inner() {
        ProviderError::Server(_) | ProviderError::Network(_) | ProviderError::Unavailable(_) => {
            std::time::Duration::from_millis(TRANSIENT_BACKOFF_BASE_MS.saturating_mul(1 << attempt.min(16))).min(TRANSIENT_BACKOFF_CAP)
        }
        _ => std::time::Duration::from_millis(RETRY_BACKOFF_BASE_MS << (attempt + 1).min(16)).min(RETRY_BACKOFF_CAP),
    }
}

/// 429 排队重试的挂点（design §12.3〔D13〕）：`retry_classified_with` 在每次真正发请求前后回调（provider 门据此计在途峰值、
/// 发「请求完成」事件），撞 429 / overloaded 时把等待与去留交给策略——不吃 2 次抖动预算。无策略 = 旧行为（429 按 Retry-After /
/// 指数退避，计入预算）。实现者：`registry::GateAttempts`（provider 在途门）；`single_shot` / `tool_loop` 暂不挂（M21b 接线时再挂）。
/// 一次 HTTP 尝试的结局（`AttemptPolicy::attempt_end`）：只有 `Ok` 是「网关放出一个槽」的证据；`RateLimited` 与其它失败都不算完成。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AttemptOutcome {
    Ok,
    RateLimited,
    Failed,
}

#[async_trait]
pub(crate) trait AttemptPolicy: Send + Sync {
    /// 一次 HTTP 尝试开始（在途 + 1）。
    fn attempt_begin(&self);
    /// 一次 HTTP 尝试结束（含 future 被丢弃时由守卫按 `Failed` 补记）。
    fn attempt_end(&self, outcome: AttemptOutcome);
    /// 撞 429 后：`nth` = 本次 `retry_classified_with` 调用内第几次撞 429（1 起；每次 HTTP 调用独立计数）。
    /// 返回 true = 已按策略等完、请重试；false = 放弃（超 429 重试上限），错误原样返回给调用方。
    async fn on_rate_limited(&self, nth: u32, retry_after: Option<std::time::Duration>) -> bool;
}

/// 流式旁路的尝试复位（M21b 批 D-1，followup「接真 sink 前须处理」）：`timed_turn_with` 的重试复用同一
/// backend 与同一 `IncrementalFindings`——断流那次的半截 JSON 会与重试输出拼进同一解析器，
/// sink 是 no-op 时无害，接真 sink（批 D 的 verifier）后就会发出重试前的草稿对象。
///
/// 本包装在**每次尝试开始**复位解析状态（已闭合对象与丢弃计数保留），其余回调纯委托给内层策略。
/// 只在有内层策略时安装：`policy = None` 的路径（单测 / 无门）不能凭空多出一个 `on_rate_limited` 实现，
/// 那会把 `retry_classified_with` 里「无策略 → 走通用退避并受 budget 约束」的分支改掉。
pub(crate) struct ResetOnAttempt<'a> {
    inner: &'a dyn AttemptPolicy,
    inc: std::sync::Arc<std::sync::Mutex<crate::provider::stream_json::IncrementalFindings>>,
}

impl<'a> ResetOnAttempt<'a> {
    pub(crate) fn new(
        inner: &'a dyn AttemptPolicy,
        inc: std::sync::Arc<std::sync::Mutex<crate::provider::stream_json::IncrementalFindings>>,
    ) -> Self {
        ResetOnAttempt { inner, inc }
    }
}

#[async_trait]
impl AttemptPolicy for ResetOnAttempt<'_> {
    fn attempt_begin(&self) {
        if let Ok(mut inc) = self.inc.lock() {
            inc.reset_for_attempt();
        }
        self.inner.attempt_begin();
    }
    fn attempt_end(&self, outcome: AttemptOutcome) {
        self.inner.attempt_end(outcome);
    }
    async fn on_rate_limited(&self, nth: u32, retry_after: Option<std::time::Duration>) -> bool {
        self.inner.on_rate_limited(nth, retry_after).await
    }
}

/// 尝试括号的 RAII 守卫：future 中途被丢弃（job deadline 的 `select!` 取消）时按 `Failed` 收尾，在途计数不泄漏。
struct AttemptGuard<'a> {
    policy: &'a dyn AttemptPolicy,
    ended: bool,
}

impl<'a> AttemptGuard<'a> {
    fn begin(policy: &'a dyn AttemptPolicy) -> Self {
        policy.attempt_begin();
        AttemptGuard { policy, ended: false }
    }
    fn end(mut self, outcome: AttemptOutcome) {
        self.ended = true;
        self.policy.attempt_end(outcome);
    }
}

impl Drop for AttemptGuard<'_> {
    fn drop(&mut self) {
        if !self.ended {
            self.policy.attempt_end(AttemptOutcome::Failed);
        }
    }
}

/// 通用重试：`op` 产 `Result<T, ProviderError>`；可重试错→退避后重试，不可重试→立即返回。
/// 退避：RateLimited 有 retry_after 用之（封顶 30s），否则指数（200·2^(n+1) ms，
/// 首试 400ms，封顶 5s；n=attempt 从 0 计，故首次退避即左移一位而非 200ms）。
/// 尝试挂点（D13）：有 `policy` 时 429 走 `on_rate_limited`（排队等本 provider 下一完成 / Retry-After，独立计数）、
/// 不计入 `budget`；其余可重试错照旧退避。`None` = 旧行为。
pub(crate) async fn retry_classified_with<T, F, Fut>(budget: u32, policy: Option<&dyn AttemptPolicy>, mut op: F) -> Result<T, ProviderError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, ProviderError>>,
{
    let mut attempt = 0u32;      // 总重试次数，决定退避档
    let mut timeouts = 0u32;     // 超时单独计（收口评审 #6：共用计数器会让「先 5xx 后超时」的序列一次都不重试超时）
    let mut rate_limited_n = 0u32;
    loop {
        let guard = policy.map(AttemptGuard::begin);
        let r = op().await;
        let outcome = match &r {
            Ok(_) => AttemptOutcome::Ok,
            Err(e) if matches!(e.inner(), ProviderError::RateLimited { .. }) => AttemptOutcome::RateLimited,
            Err(_) => AttemptOutcome::Failed,
        };
        if let Some(g) = guard {
            g.end(outcome);
        }
        match r {
            Ok(v) => return Ok(v),
            Err(e) => {
                if outcome == AttemptOutcome::RateLimited {
                    if let Some(p) = policy {
                        rate_limited_n += 1;
                        if p.on_rate_limited(rate_limited_n, e.retry_after()).await {
                            continue;
                        }
                        return Err(e);
                    }
                }
                let is_timeout = matches!(e.inner(), ProviderError::Timeout(_));
                let exhausted = attempt >= budget || (is_timeout && timeouts >= TIMEOUT_RETRY_CAP);
                if exhausted || !e.is_retryable() {
                    return Err(e);
                }
                let wait = backoff_for(&e, attempt);
                attempt += 1;
                if is_timeout {
                    timeouts += 1;
                }
                // 单测构建里退避缩千倍：大量用例用假后端喂 5xx，真等 2 s 起步会把全量拖慢；时长本身由 `backoff_for` 的纯函数用例钉住
                tokio::time::sleep(if cfg!(test) { wait / 1000 } else { wait }).await;
            }
        }
    }
}

#[async_trait]
pub trait ReviewProvider: Send + Sync {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError>;
}

/// 单轮 timeout+retry 包装（带 tools，对应 `ChatBackend::turn`）：超时归 `ProviderError::Timeout`，
/// 外层套 `retry_classified_with`。tool_loop 与 single_shot 共享，避免各自手写同一手写块（B2 抽取）。
pub(crate) async fn timed_turn(
    backend: &dyn crate::provider::tool_loop::ChatBackend,
    messages: &[crate::provider::tool_loop::ChatMessage],
    tools: &[crate::tools::ToolSpec],
    per_turn: std::time::Duration,
    retry_budget: u32,
) -> Result<crate::provider::tool_loop::AssistantTurn, ProviderError> {
    timed_turn_with(backend, messages, tools, per_turn, retry_budget, None).await
}

/// `timed_turn` + 尝试挂点（D13：provider 门的 429 排队重试）。
pub(crate) async fn timed_turn_with(
    backend: &dyn crate::provider::tool_loop::ChatBackend,
    messages: &[crate::provider::tool_loop::ChatMessage],
    tools: &[crate::tools::ToolSpec],
    per_turn: std::time::Duration,
    retry_budget: u32,
    policy: Option<&dyn AttemptPolicy>,
) -> Result<crate::provider::tool_loop::AssistantTurn, ProviderError> {
    retry_classified_with(retry_budget, policy, || async {
        match tokio::time::timeout(per_turn, backend.turn(messages, tools)).await {
            Ok(r) => r,
            Err(_) => Err(ProviderError::Timeout(
                format!("turn timeout after {}s", per_turn.as_secs()))),
        }
    }).await
}

/// 同上，收尾轮版本（对应 `ChatBackend::turn_final`，不带 tools）。
pub(crate) async fn timed_turn_final(
    backend: &dyn crate::provider::tool_loop::ChatBackend,
    messages: &[crate::provider::tool_loop::ChatMessage],
    per_turn: std::time::Duration,
    retry_budget: u32,
) -> Result<crate::provider::tool_loop::AssistantTurn, ProviderError> {
    timed_turn_final_with(backend, messages, per_turn, retry_budget, None).await
}

/// `timed_turn_final` + 尝试挂点（D13）。
pub(crate) async fn timed_turn_final_with(
    backend: &dyn crate::provider::tool_loop::ChatBackend,
    messages: &[crate::provider::tool_loop::ChatMessage],
    per_turn: std::time::Duration,
    retry_budget: u32,
    policy: Option<&dyn AttemptPolicy>,
) -> Result<crate::provider::tool_loop::AssistantTurn, ProviderError> {
    retry_classified_with(retry_budget, policy, || async {
        match tokio::time::timeout(per_turn, backend.turn_final(messages)).await {
            Ok(r) => r,
            Err(_) => Err(ProviderError::Timeout(
                format!("turn timeout after {}s", per_turn.as_secs()))),
        }
    }).await
}

/// `REDTEAM_HTTP_TIMEOUT_SECS`（整请求超时，缺省 300s）：思考型模型在大上下文 bundle 上单轮响应可 >120s（旧值），
/// 非网络不可达（ping 通）而是慢——留 env 供按网络粗调；非法 / 0 → 缺省。
pub(crate) fn http_timeout_env_secs() -> u64 {
    std::env::var("REDTEAM_HTTP_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(300)
}

/// 整请求超时须罩住单轮超时（评审 M2：否则 reqwest 在 300s 先切断响应体，报 Unavailable 而非 Timeout，慢模型永远完不成一轮）：
/// env 值与 `per_turn + 60s` 取大。
pub(crate) fn http_timeout_for(per_turn_secs: u64) -> u64 {
    http_timeout_env_secs().max(per_turn_secs.saturating_add(60))
}

/// 带超时的 HTTP client（connect 10s / 整请求 `secs`）；构建失败回落默认 client。
pub(crate) fn http_client_with_timeout(secs: u64) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(secs))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 缺省 client：整请求超时取 env / 300s（`backend_from_config` 会按模型 per_turn 重建）。
pub(crate) fn default_http_client() -> reqwest::Client {
    http_client_with_timeout(http_timeout_env_secs())
}

/// Anthropic Messages 端点 URL：规范化 base 的 `/v1`——无论 base 含不含 `/v1`，最终都拼成
/// `{host}/v1/messages`（dogfood：网关 base 配 `.../v1` 时旧逻辑拼出 `/v1/v1/messages` 404）。
pub(crate) fn anthropic_messages_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") {
        format!("{base}/messages")
    } else {
        format!("{base}/v1/messages")
    }
}
// 注：openai 侧**有意不做** anthropic 同款 /v1 规范化——openai 兼容生态无统一规范
// （DeepSeek 官方 base 裸路径正统、部分网关必须带 /v1、部分必须不带），自动补写对
// 「必须不带」的端点是破坏性的。base_url = 完整前缀，由配置负责（用户 2026-07-16 定）。

#[cfg(test)]
mod tests {
    use super::anthropic_messages_url;

    #[test]
    fn anthropic_messages_url_normalizes_v1() {
        // base 不含 /v1（默认 Anthropic）→ 拼 /v1/messages
        assert_eq!(anthropic_messages_url("https://api.anthropic.com"), "https://api.anthropic.com/v1/messages");
        // base 已含 /v1（OpenAI 风格网关，如 aigocode）→ 只拼 /messages，不双 /v1
        assert_eq!(anthropic_messages_url("https://team.aigocode.com/v1"), "https://team.aigocode.com/v1/messages");
        // trailing slash 容错
        assert_eq!(anthropic_messages_url("https://h/v1/"), "https://h/v1/messages");
        assert_eq!(anthropic_messages_url("https://h/"), "https://h/v1/messages");
    }

    use super::{classify_status, parse_envelope_json, parse_retry_after, ProviderError};
    use reqwest::StatusCode;
    use std::time::Duration;

    #[test]
    fn envelope_empty_body_is_retryable_unavailable() {
        // 200 + 纯空白 body（上游推理模型 keep-alive 后放弃、网关抖动）→ 可重试，
        // 绝不能是不可重试的 Parse（那会绕过 retry_classified，一次抖动烧掉整个 cell）。
        for body in ["", "   \n  ", "\n\n"] {
            let err = parse_envelope_json(body, "https://api.deepseek.com/x").unwrap_err();
            assert!(matches!(err, ProviderError::Unavailable(_)), "body {body:?} → {err:?}");
            assert!(err.is_retryable(), "envelope 解码失败必须可重试");
        }
    }

    #[test]
    fn envelope_html_body_carries_preview() {
        let err = parse_envelope_json("<html>502 Bad Gateway</html>", "u").unwrap_err();
        let ProviderError::Unavailable(m) = err else { panic!("应为 Unavailable") };
        assert!(m.contains("502 Bad Gateway"), "错误须携带 body 预览以便诊断: {m}");
    }

    #[test]
    fn envelope_valid_json_decodes() {
        let v = parse_envelope_json(r#"{"ok":true}"#, "u").unwrap();
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn envelope_leading_whitespace_before_json_is_ok() {
        // DeepSeek 非流式在长思考期间以空行 keep-alive；有 JSON 随后仍应正常解码。
        let v = parse_envelope_json("\n\n{\"ok\":1}", "u").unwrap();
        assert_eq!(v["ok"], 1);
    }

    #[test]
    fn classify_status_maps_http_codes() {
        use reqwest::header::HeaderMap;
        let h = HeaderMap::new();
        assert!(matches!(classify_status(StatusCode::TOO_MANY_REQUESTS, &h), ProviderError::RateLimited { .. }));
        assert!(matches!(classify_status(StatusCode::from_u16(529).unwrap(), &h), ProviderError::RateLimited { .. }), "529 overloaded 进 429 排队重试（D13）");
        assert!(matches!(classify_status(StatusCode::UNAUTHORIZED, &h), ProviderError::Auth(_)));
        assert!(matches!(classify_status(StatusCode::FORBIDDEN, &h), ProviderError::Auth(_)));
        assert!(matches!(classify_status(StatusCode::REQUEST_TIMEOUT, &h), ProviderError::Timeout(_)));
        assert!(matches!(classify_status(StatusCode::INTERNAL_SERVER_ERROR, &h), ProviderError::Server(_)));
        assert!(matches!(classify_status(StatusCode::SERVICE_UNAVAILABLE, &h), ProviderError::Server(_)));
        assert!(matches!(classify_status(StatusCode::BAD_REQUEST, &h), ProviderError::BadRequest(_)));
        assert!(matches!(classify_status(StatusCode::PAYLOAD_TOO_LARGE, &h), ProviderError::BadRequest(_)));
        assert!(matches!(classify_status(StatusCode::UNPROCESSABLE_ENTITY, &h), ProviderError::BadRequest(_)));
        // 3xx（非 4xx/5xx）→ 兜底 Unavailable
        assert!(matches!(classify_status(StatusCode::from_u16(418).unwrap(), &h), ProviderError::BadRequest(_))); // teapot 是 4xx
    }

    #[test]
    fn parse_retry_after_integer_seconds_only() {
        use reqwest::header::{HeaderMap, HeaderValue, RETRY_AFTER};
        let mut h = HeaderMap::new();
        h.insert(RETRY_AFTER, HeaderValue::from_static("5"));
        assert_eq!(parse_retry_after(&h), Some(Duration::from_secs(5)));
        // HTTP-date 形式忽略（决策④）
        let mut h2 = HeaderMap::new();
        h2.insert(RETRY_AFTER, HeaderValue::from_static("Wed, 21 Oct 2025 07:28:00 GMT"));
        assert_eq!(parse_retry_after(&h2), None);
        // 缺头
        assert_eq!(parse_retry_after(&HeaderMap::new()), None);
    }

    #[test]
    fn retry_after_from_rate_limited_only() {
        let rl = ProviderError::RateLimited { retry_after: Some(Duration::from_secs(3)) };
        assert_eq!(rl.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(ProviderError::Server("x".into()).retry_after(), None);
    }

    #[test]
    fn is_retryable_truth_table() {
        assert!(ProviderError::RateLimited { retry_after: None }.is_retryable());
        assert!(ProviderError::Server("x".into()).is_retryable());
        assert!(ProviderError::Timeout("x".into()).is_retryable());
        assert!(ProviderError::Network("x".into()).is_retryable());
        assert!(ProviderError::Unavailable("x".into()).is_retryable()); // 未知保守可重试
        assert!(!ProviderError::Auth("x".into()).is_retryable());
        assert!(!ProviderError::BadRequest("x".into()).is_retryable());
        assert!(!ProviderError::Parse("x".into()).is_retryable());
    }

    #[tokio::test]
    async fn retry_classified_retries_then_succeeds() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let r: Result<u32, ProviderError> = super::retry_classified_with(2, None, || {
            let n = calls.get();
            calls.set(n + 1);
            async move {
                if n < 2 { Err(ProviderError::Server("boom".into())) } else { Ok(99) }
            }
        }).await;
        assert_eq!(r.unwrap(), 99);
        assert_eq!(calls.get(), 3); // 首发 + 2 重试
    }

    #[tokio::test]
    async fn retry_classified_non_retryable_fails_fast() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let r: Result<u32, ProviderError> = super::retry_classified_with(5, None, || {
            calls.set(calls.get() + 1);
            async move { Err::<u32, _>(ProviderError::Auth("401".into())) }
        }).await;
        assert!(matches!(r, Err(ProviderError::Auth(_))));
        assert_eq!(calls.get(), 1, "不可重试错必须只调 1 次、不烧退避");
    }

    #[tokio::test]
    async fn retry_classified_exhausts_budget() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let r: Result<u32, ProviderError> = super::retry_classified_with(2, None, || {
            calls.set(calls.get() + 1);
            async move { Err::<u32, _>(ProviderError::Network("down".into())) }
        }).await;
        assert!(matches!(r, Err(ProviderError::Network(_))));
        assert_eq!(calls.get(), 3); // budget+1
    }

    /// 出口验收（503 成片）：瞬断走 2 s 起步翻倍、封顶 30 s；超时等其余错误维持短退避；Retry-After 优先。
    #[test]
    fn backoff_for_transient_is_long_and_capped() {
        use std::time::Duration as D;
        let s = ProviderError::Server("503".into());
        let got: Vec<D> = (0..6).map(|a| super::backoff_for(&s, a)).collect();
        assert_eq!(got, [D::from_secs(2), D::from_secs(4), D::from_secs(8), D::from_secs(16), D::from_secs(30), D::from_secs(30)]);
        assert_eq!(super::backoff_for(&ProviderError::Network("x".into()), 0), D::from_secs(2));
        assert_eq!(super::backoff_for(&ProviderError::Unavailable("x".into()), 1), D::from_secs(4));
        let wrapped = ProviderError::Server("503".into()).attach_usage((1, 1, 0));
        assert_eq!(super::backoff_for(&wrapped, 0), D::from_secs(2), "带 usage 的包装也按内层分类");
        assert_eq!(super::backoff_for(&ProviderError::Timeout("t".into()), 0), D::from_millis(400), "超时维持原短退避");
        assert_eq!(super::backoff_for(&ProviderError::Timeout("t".into()), 9), D::from_secs(5));
        let ra = ProviderError::RateLimited { retry_after: Some(D::from_secs(90)) };
        assert_eq!(super::backoff_for(&ra, 0), D::from_secs(30), "Retry-After 优先且封顶");
        // 预算 4 下累计等待 ≈ 30 s（再加 4 次请求本身），能扛住一阵持续数秒到半分钟的 503
        assert_eq!((0..super::DEFAULT_RETRY_BUDGET).map(|a| super::backoff_for(&s, a)).sum::<D>(), D::from_secs(30));
    }

    /// 混合错误序列（收口评审 #6）：先两次 5xx 再连续超时——超时自己的额度不该被 5xx 用掉。
    /// 共用计数器时 `attempt` 已是 2，`min(budget, TIMEOUT_RETRY_CAP)` = 2 → 超时一次都不重试。
    #[tokio::test]
    async fn timeout_budget_is_not_consumed_by_earlier_transient_errors() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let r: Result<u32, ProviderError> = super::retry_classified_with(super::DEFAULT_RETRY_BUDGET, None, || {
            let n = calls.get();
            calls.set(n + 1);
            async move {
                Err::<u32, _>(if n < 2 { ProviderError::Server("503".into()) } else { ProviderError::Timeout("turn".into()) })
            }
        }).await;
        assert!(matches!(r, Err(ProviderError::Timeout(_))));
        // 2 次 5xx（各重试一次）+ 超时首发 + 超时重试 2 次 = 5 次；总重试数仍受 budget 4 约束
        assert_eq!(calls.get(), 5, "超时拿到自己的 TIMEOUT_RETRY_CAP 次重试，而不是被前面的 5xx 吃掉");
    }

    /// 超时不随预算放大：预算 4 时仍只重试 2 次（每次超时已烧掉一整个 per-turn）；5xx 用满 4 次。
    #[tokio::test]
    async fn timeout_retries_are_capped_but_transient_uses_full_budget() {
        use std::cell::Cell;
        let calls = Cell::new(0u32);
        let r: Result<u32, ProviderError> = super::retry_classified_with(super::DEFAULT_RETRY_BUDGET, None, || {
            calls.set(calls.get() + 1);
            async move { Err::<u32, _>(ProviderError::Timeout("turn".into())) }
        }).await;
        assert!(matches!(r, Err(ProviderError::Timeout(_))));
        assert_eq!(calls.get(), 1 + super::TIMEOUT_RETRY_CAP);
        calls.set(0);
        let r: Result<u32, ProviderError> = super::retry_classified_with(super::DEFAULT_RETRY_BUDGET, None, || {
            calls.set(calls.get() + 1);
            async move { Err::<u32, _>(ProviderError::Server("503".into())) }
        }).await;
        assert!(r.is_err());
        assert_eq!(calls.get(), 1 + super::DEFAULT_RETRY_BUDGET);
    }
}
