use crate::domain::report::RateLimitMeta;
use crate::prompt::ScanPrompt;
use crate::provider::scan_shot::{run_scan_shot_with, ScanCall};
use crate::provider::tool_loop::ChatBackend;
use crate::provider::{AttemptOutcome, AttemptPolicy, LensCall, LensRequest, ProviderError, ReviewProvider};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// provider 级在途门（design §12.3〔D13〕）：**唯一一道闸**。名额 = 门宽（provider `maxConcurrency`，缺省继承 `review.max_concurrency`），
/// 同一 provider 下全部角色条目共享一把（scanner 与 M21b 的 reviewer / verifier 同门）。自管计数 + `Notify` 而非 tokio `Semaphore`：
/// 撞 429 时门宽只降不升，收窄后多出的**已发**名额要在释放时丢弃（`Semaphore::forget_permits` 只能忘掉可用名额）。
/// `peak` = 同时在途的 HTTP 请求峰值（RunMeta `provider_inflight_peak`，铁律 1）。
pub struct InflightGate {
    /// 配置门宽（不变；`stat().cap`）。
    cap_initial: u32,
    state: Mutex<GateState>,
    /// 名额释放 → `notify_one`：唤醒一个等门者。等门者先 `enable()` 注册再检条件（tokio 推荐模式），两次释放落在同一窗口也不丢唤醒。
    slot_free: Notify,
    /// 成功完成 → `notify_one`：每次完成只放一个 429 等待者重试（排队语义，K6）；在途归零 → `notify_waiters`（K8：全员 429 时不互相等死，
    /// 被广播唤醒者先退避再准入）。
    completed: Notify,
    peak: AtomicU32,
}

#[derive(Clone, Copy, Debug)]
struct GateState {
    /// 当前门宽（≤ cap_initial；429 收敛只降不升）。
    cap: u32,
    /// 已发出的名额数（收窄后可暂大于 cap：释放即丢弃，不唤醒）。
    held: u32,
    /// 在途 HTTP 请求数（`attempt_begin` .. `attempt_end`；429 等待者准入时预占）。
    active: u32,
    /// 本轮撞 429 / overloaded 次数。
    rate_limited: u32,
    /// 成功完成计数：429 等待者以「计数超过入睡时的值」判断等到的是新完成，而不是 `notify_one` 存下的旧许可；
    /// 与 `active` 同一临界区读写（评审修：seq0 不在锁外读）。
    completions: u64,
}

impl GateState {
    /// 收门（只降不升，下限 1）：`on_rate_limited` 与测试共用同一公式。
    fn shrink_to(&mut self, cap: u32) {
        self.cap = cap.max(1).min(self.cap);
    }
}

/// 每 provider 门的观测（RunMeta / warnings）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateStat {
    pub cap: u32,
    pub effective_cap: u32,
    pub rate_limited: u32,
    pub peak: u32,
    pub completions: u64,
}

impl InflightGate {
    fn new(cap: u32) -> Self {
        let cap = cap.max(1);
        InflightGate {
            cap_initial: cap,
            state: Mutex::new(GateState { cap, held: 0, active: 0, rate_limited: 0, completions: 0 }),
            slot_free: Notify::new(),
            completed: Notify::new(),
            peak: AtomicU32::new(0),
        }
    }
    /// 临界区只有几条整数运算，从不跨 await 持锁；poison 直接取内层（状态是纯计数，不会半更新）。
    fn lock(&self) -> MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn peak(&self) -> u32 {
        self.peak.load(Ordering::SeqCst)
    }
    /// 配置门宽（provider `maxConcurrency` 或继承的缺省）。
    pub fn configured_cap(&self) -> u32 {
        self.cap_initial
    }
    pub fn stat(&self) -> GateStat {
        let s = *self.lock();
        GateStat { cap: self.cap_initial, effective_cap: s.cap, rate_limited: s.rate_limited, peak: self.peak(), completions: s.completions }
    }
    /// 取名额：`held < cap` 即取（[`Self::try_enter`]），否则等一次释放再重检。`notified().enable()` 先于条件检查注册，
    /// 释放发生在两者之间也不丢唤醒。
    async fn enter(&self) -> GatePermit<'_> {
        loop {
            let notified = self.slot_free.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(permit) = self.try_enter() {
                return permit;
            }
            notified.await;
        }
    }
    /// 非阻塞取名额（M21b 批 C1 深挖派发循环）：`held < cap` 即取，否则 `None`。`cap` 是 429 收敛后的有效门宽，天然尊重收门。
    /// **不感知等门者**：同门若有 `enter()` 阻塞等待者，本调用可能抢在其前——C1 深挖内部没有阻塞等门者（一格持门位到底），
    /// 与 scanner 共门时的让位公平性归批 C2。
    fn try_enter(&self) -> Option<GatePermit<'_>> {
        let mut s = self.lock();
        if s.held < s.cap {
            s.held += 1;
            Some(GatePermit { gate: self })
        } else {
            None
        }
    }
    #[cfg(test)]
    fn shrink_to(&self, cap: u32) {
        self.lock().shrink_to(cap);
    }
}

/// 已发名额：Drop 归还；门已收窄到 `held ≥ cap` 时不唤醒 = 名额丢弃（K6）。
pub struct GatePermit<'a> {
    gate: &'a InflightGate,
}

impl Drop for GatePermit<'_> {
    fn drop(&mut self) {
        let wake = {
            let mut s = self.gate.lock();
            s.held = s.held.saturating_sub(1);
            s.held < s.cap
        };
        if wake {
            self.gate.slot_free.notify_one();
        }
    }
}

/// 429 无他人在途 / 被在途归零广播唤醒时的探测退避基数与封顶；重试次数上限 = `max(RL_RETRY_FLOOR, 当时门宽 × 2)`
/// （K9：D13 写「门宽 × 2」，门收敛到 1 时只剩 2 次太薄，加下限）。
const RL_BACKOFF_BASE: Duration = Duration::from_millis(500);
const RL_BACKOFF_CAP: Duration = Duration::from_secs(30);
const RL_RETRY_FLOOR: u32 = 4;

/// 429 排队重试策略（design §12.3〔D13〕）：每次 `ScanLease::run` 新建一个，挂在 `retry_classified_with` 上。
/// 撞 429 → 该 provider 门宽 ← max(1, min(门宽 − 1, 此刻仍在跑的请求数))，本轮不回升（一波 429 内收敛到网关真实上限，
/// 误填 `maxConcurrency` 也自愈）；撞门者持名额不放，等本 provider 下一个**成功**完成（每次完成只唤醒一个）或 Retry-After 取晚者，
/// 再经**准入**（`active < cap` 才预占在途，收窄后的门真正约束重试；门满则回去等下一次完成）重试；无他人在途、或被在途归零广播
/// 唤醒时先退避探测再准入（评审修：不齐发）。429 次数按每次 HTTP 调用独立计（`nth` 由 `retry_classified_with` 传入），不吃 2 次抖动预算。
pub(crate) struct GateAttempts<'a> {
    gate: &'a InflightGate,
    backoff_base: Duration,
    /// 准入已预占 `active`：下一次 `attempt_begin` 不再加一。
    reserved: AtomicBool,
    /// 实际发生的 429 重试次数（观测：`ScanCall.rate_limit_retries` → `CellRecord.attempts`）。
    retries: AtomicU32,
    /// 429 排队等待总毫秒（观测：`ScanCall.rate_limit_wait_ms` → `CellRecord.queue_wait_ms`）。
    waited_ms: AtomicU64,
}

impl<'a> GateAttempts<'a> {
    pub(crate) fn new(gate: &'a InflightGate) -> Self {
        Self::with_backoff_base(gate, RL_BACKOFF_BASE)
    }
    pub(crate) fn with_backoff_base(gate: &'a InflightGate, backoff_base: Duration) -> Self {
        GateAttempts { gate, backoff_base, reserved: AtomicBool::new(false), retries: AtomicU32::new(0), waited_ms: AtomicU64::new(0) }
    }
    pub(crate) fn retries(&self) -> u32 {
        self.retries.load(Ordering::SeqCst)
    }
    pub(crate) fn waited_ms(&self) -> u64 {
        self.waited_ms.load(Ordering::SeqCst)
    }
    /// 第 n 次（1 起）429 重试的探测间隔：base × 2^(n−1)，封顶 30 s。
    fn backoff(&self, n: u32) -> Duration {
        self.backoff_base.saturating_mul(1u32 << n.saturating_sub(1).min(16)).min(RL_BACKOFF_CAP)
    }
    /// 等本 provider 下一个成功完成（`completions > seq0`）；返回 true = 因在途归零而退出（无人可等，调用方须退避）。
    async fn wait_new_completion(&self, seq0: u64) -> bool {
        loop {
            let notified = self.gate.completed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let s = self.gate.lock();
                if s.completions > seq0 {
                    return false;
                }
                if s.active == 0 {
                    return true;
                }
            }
            notified.await;
        }
    }
}

#[async_trait]
impl AttemptPolicy for GateAttempts<'_> {
    fn attempt_begin(&self) {
        let cur = {
            let mut s = self.gate.lock();
            if !self.reserved.swap(false, Ordering::SeqCst) {
                s.active += 1;
            }
            s.active
        };
        self.gate.peak.fetch_max(cur, Ordering::SeqCst);
    }

    fn attempt_end(&self, outcome: AttemptOutcome) {
        let zero = {
            let mut s = self.gate.lock();
            s.active = s.active.saturating_sub(1);
            if outcome == AttemptOutcome::Ok {
                s.completions += 1;
            }
            s.active == 0
        };
        // 只有成功才是「网关放出一个槽」的证据（评审修：Timeout / Server 等失败不放行等待者）。
        if outcome == AttemptOutcome::Ok {
            self.gate.completed.notify_one();
        }
        if zero {
            self.gate.completed.notify_waiters();
        }
    }

    async fn on_rate_limited(&self, nth: u32, retry_after: Option<Duration>) -> bool {
        let t0 = Instant::now();
        let (mut others, limit, mut seq0) = {
            let mut s = self.gate.lock();
            s.rate_limited += 1;
            // 本请求已 attempt_end，此刻 active 只剩别人 = 网关仍在服务的请求数（同一波 429 逐个到达时，最后一个看到的就是真实上限）。
            let others = s.active;
            let target = s.cap.saturating_sub(1).min(others);
            s.shrink_to(target);
            (others, (s.cap * 2).max(RL_RETRY_FLOOR), s.completions)
        };
        if nth > limit {
            return false;
        }
        let mut retry_after = retry_after.map(|d| d.min(RL_BACKOFF_CAP));
        loop {
            if others == 0 {
                // 没人在跑，等完成事件无意义：按 Retry-After 或退避探测（K8）。
                tokio::time::sleep(retry_after.unwrap_or_else(|| self.backoff(nth))).await;
            } else {
                // 等下一个成功完成，与 Retry-After 取晚者；被在途归零广播唤醒 = 无人可等 → 同样退避（评审修：不零延迟齐发）。
                let (woke_by_zero, _) = tokio::join!(self.wait_new_completion(seq0), tokio::time::sleep(retry_after.unwrap_or(Duration::ZERO)));
                if woke_by_zero {
                    tokio::time::sleep(retry_after.unwrap_or_else(|| self.backoff(nth))).await;
                }
            }
            retry_after = None; // Retry-After 只等一次
            // 准入：收窄后的门真正约束重试——`active < cap` 才预占在途；门满则回去等下一次完成（每次完成只放一个）。
            let mut s = self.gate.lock();
            if s.active < s.cap {
                s.active += 1;
                self.reserved.store(true, Ordering::SeqCst);
                drop(s);
                self.retries.fetch_add(1, Ordering::SeqCst);
                self.waited_ms.fetch_add(t0.elapsed().as_millis() as u64, Ordering::SeqCst);
                return true;
            }
            others = s.active;
            seq0 = s.completions;
        }
    }
}

/// scanner 条目（批 F-2 prd K2）：route_key → backend + 单轮超时 + 重试预算 + 所属 provider（取在途门）。
struct ScanEntry {
    backend: Box<dyn ChatBackend>,
    per_turn: Duration,
    retry_budget: u32,
    provider: String,
}

/// 按 route_key 路由的注册表（通用基建）。
/// - `providers`：旧 `ReviewProvider` 表（finding 契约；M21b reviewer / verifier 沿用）。
/// - `scan`：scanner 条目表（批 F-2）——`scan()` 先过所属 provider 的在途门再走 `run_scan_shot`。
/// - `warnings`：建表时的告警（thinking 无映射等），调用方并入 RunMeta.warnings。
#[derive(Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Box<dyn ReviewProvider>>,
    scan: BTreeMap<String, ScanEntry>,
    gates: BTreeMap<String, Arc<InflightGate>>,
    warnings: Vec<String>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn insert(&mut self, name: impl Into<String>, provider: Box<dyn ReviewProvider>) {
        self.providers.insert(name.into(), provider);
    }
    pub fn len(&self) -> usize {
        self.providers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }

    /// 注册 scanner 条目；同 provider 的在途门按首次 `cap` 建立并复用（同 provider 多条目共享）。
    pub fn insert_scan(
        &mut self,
        route_key: impl Into<String>,
        provider: &str,
        cap: u32,
        backend: Box<dyn ChatBackend>,
        per_turn: Duration,
        retry_budget: u32,
    ) {
        self.gates.entry(provider.to_string()).or_insert_with(|| Arc::new(InflightGate::new(cap)));
        self.scan.insert(route_key.into(), ScanEntry { backend, per_turn, retry_budget, provider: provider.to_string() });
    }
    pub fn scan_len(&self) -> usize {
        self.scan.len()
    }
    pub fn has_scan(&self, route_key: &str) -> bool {
        self.scan.contains_key(route_key)
    }
    pub fn push_warning(&mut self, w: impl Into<String>) {
        self.warnings.push(w.into());
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
    /// 每 provider 的在途峰值（RunMeta `provider_inflight_peak`）。
    pub fn inflight_peaks(&self) -> BTreeMap<String, u32> {
        self.gates.iter().map(|(p, g)| (p.clone(), g.peak())).collect()
    }
    /// route_key → 所属 provider。
    pub fn provider_of(&self, route_key: &str) -> Option<&str> {
        self.scan.get(route_key).map(|e| e.provider.as_str())
    }
    /// provider 配置门宽（`maxConcurrency` 或继承的缺省）；未建门 → None。
    pub fn cap_of(&self, provider: &str) -> Option<u32> {
        self.gates.get(provider).map(|g| g.configured_cap())
    }
    /// 每 provider 门的观测快照。
    pub fn gate_stats(&self) -> BTreeMap<String, GateStat> {
        self.gates.iter().map(|(p, g)| (p.clone(), g.stat())).collect()
    }
    /// 排队告警（扇出与 resume 共用，D13 按 provider 计）：路由到同一 provider 的格数 > 其**有效**门宽即排队，墙钟 = 排队 + 最长格。
    /// 门未收窄时指向唯一旋钮 `providers.<p>.maxConcurrency`；已因 429 收窄则说明是收窄所致（不再劝「调大」，与 429 告警不打架）。
    /// 未注册条目不计（它们进 failed）。跑完再算，用的是收口时的有效门宽。
    pub fn queue_warnings<'a>(&self, route_keys: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        let mut per: BTreeMap<&str, usize> = BTreeMap::new();
        for k in route_keys {
            if let Some(p) = self.provider_of(k) {
                *per.entry(p).or_default() += 1;
            }
        }
        per.into_iter()
            .filter_map(|(p, n)| {
                let st = self.gates.get(p)?.stat();
                let eff = st.effective_cap as usize;
                if n <= eff {
                    return None;
                }
                Some(if st.effective_cap < st.cap {
                    format!(
                        "scanner cells {n} on provider {p} > effective maxConcurrency {eff} (configured {}, shrunk after 429): {} cell(s) queued (wall = queue wait + slowest cell)",
                        st.cap, n - eff
                    )
                } else {
                    format!(
                        "scanner cells {n} on provider {p} > maxConcurrency {eff}: {} cell(s) queued (wall = queue wait + slowest cell; raise providers.{p}.maxConcurrency)",
                        n - eff
                    )
                })
            })
            .collect()
    }
    /// RunMeta `provider_rate_limit`：只含本轮撞过 429 的 provider（空时报告不产键）。
    pub fn rate_limit_meta(&self) -> BTreeMap<String, RateLimitMeta> {
        self.gates
            .iter()
            .filter_map(|(p, g)| {
                let s = g.stat();
                (s.rate_limited > 0).then(|| (p.clone(), RateLimitMeta { hits: s.rate_limited, cap: s.cap, effective_cap: s.effective_cap }))
            })
            .collect()
    }
    /// run.warnings：每个撞过 429 的 provider 一条「429 × N，门宽 A → B」，指向唯一的旋钮（同一判定源 = `rate_limit_meta`）。
    pub fn gate_warnings(&self) -> Vec<String> {
        self.rate_limit_meta()
            .into_iter()
            .map(|(p, m)| {
                format!(
                    "provider {p}: 429 x {}, maxConcurrency {} -> {} (gate shrank to the gateway's real limit this run; set providers.{p}.maxConcurrency <= {})",
                    m.hits, m.cap, m.effective_cap, m.effective_cap
                )
            })
            .collect()
    }

    /// 条目与其所属 provider 的在途门（`lease` / `try_lease` 共用）。未注册 → `Unavailable`（该格入 coverage failed）。
    fn entry_and_gate(&self, route_key: &str) -> Result<(&ScanEntry, &InflightGate), ProviderError> {
        let Some(entry) = self.scan.get(route_key) else {
            return Err(ProviderError::Unavailable(format!("scanner not in registry: {route_key}")));
        };
        let gate = self.gates.get(&entry.provider).ok_or_else(|| ProviderError::Unavailable(format!("no inflight gate for provider {}", entry.provider)))?;
        Ok((entry, gate))
    }

    /// 过 provider 在途门再拿到条目（唯一闸，D13：无角色级 workers）。
    pub async fn lease(&self, route_key: &str) -> Result<ScanLease<'_>, ProviderError> {
        let (entry, gate) = self.entry_and_gate(route_key)?;
        let permit = gate.enter().await;
        Ok(ScanLease { entry, gate, _permit: permit })
    }

    /// 非阻塞版 [`Self::lease`]（深挖门驱动派发：门有空位才弹出队列项，D13）：门满 → `Ok(None)`；未注册 → `Unavailable`（同 `lease`）。
    pub fn try_lease(&self, route_key: &str) -> Result<Option<ScanLease<'_>>, ProviderError> {
        let (entry, gate) = self.entry_and_gate(route_key)?;
        Ok(gate.try_enter().map(|permit| ScanLease { entry, gate, _permit: permit }))
    }

    /// scanner 调用：过 provider 在途门 → 两轮单发（`lease` + `run`）。
    pub async fn scan(&self, route_key: &str, prompt: &ScanPrompt) -> Result<ScanCall, ProviderError> {
        self.lease(route_key).await?.run(prompt).await
    }
}

/// 已过在途门的 scanner 条目租约：持有期间占该 provider 一个名额（撞 429 排队期间也不放）；`run` 走两轮单发，
/// 每次 HTTP 尝试经 `GateAttempts` 计在途峰值并处理 429；429 重试次数与等待时长回填 `ScanCall`（观测口径）。
pub struct ScanLease<'a> {
    entry: &'a ScanEntry,
    gate: &'a InflightGate,
    _permit: GatePermit<'a>,
}

impl ScanLease<'_> {
    pub fn backend(&self) -> &dyn ChatBackend {
        self.entry.backend.as_ref()
    }
    pub fn per_turn(&self) -> Duration {
        self.entry.per_turn
    }
    pub fn retry_budget(&self) -> u32 {
        self.entry.retry_budget
    }
    pub(crate) fn gate_attempts(&self) -> GateAttempts<'_> {
        GateAttempts::new(self.gate)
    }

    pub async fn run(&self, prompt: &ScanPrompt) -> Result<ScanCall, ProviderError> {
        let policy = GateAttempts::new(self.gate);
        run_scan_shot_with(self.entry.backend.as_ref(), prompt, self.entry.per_turn, self.entry.retry_budget, Some(&policy))
            .await
            .map(|mut call| {
                call.rate_limit_retries = policy.retries();
                call.rate_limit_wait_ms = policy.waited_ms();
                call
            })
    }
}

#[async_trait]
impl ReviewProvider for ProviderRegistry {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        match self.providers.get(&req.route_key) {
            Some(p) => p.run(req).await,
            None => Err(ProviderError::Unavailable(format!(
                "model not in registry: {}",
                req.route_key
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RawFinding, Severity};
    use crate::prompt::PromptBlock;
    use crate::provider::fake::FakeProvider;
    use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend};
    use std::time::Duration;

    fn rf(title: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false, quote: "q".into(),
        }
    }

    /// 非阻塞取门位：门满回 `None` 不阻塞；429 收门后按有效门宽判；未注册 → `Unavailable`；归还后可再取。
    #[tokio::test]
    async fn try_lease_respects_effective_cap_without_blocking() {
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 2, Box::new(FakeChatBackend::new(vec![])), Duration::from_secs(5), 0);
        let l1 = reg.try_lease("p/a").unwrap().expect("空门可取");
        let l2 = reg.try_lease("p/a").unwrap().expect("门宽 2");
        assert!(reg.try_lease("p/a").unwrap().is_none(), "门满返回 None");
        drop(l1);
        reg.gates["p"].shrink_to(1);
        assert!(reg.try_lease("p/a").unwrap().is_none(), "收门到 1 后 held 1 已满");
        drop(l2);
        assert!(reg.try_lease("p/a").unwrap().is_some(), "归还后可取");
        assert!(matches!(reg.try_lease("p/none"), Err(ProviderError::Unavailable(_))));
    }

    #[tokio::test]
    async fn routes_by_model_name() {
        let mut reg = ProviderRegistry::new();
        reg.insert("alpha", Box::new(FakeProvider::with("alpha", vec![rf("from-alpha")])));
        reg.insert("beta", Box::new(FakeProvider::with("beta", vec![rf("from-beta")])));
        assert_eq!(reg.len(), 2);
        let a = reg.run(LensRequest { route_key: "alpha".into(), prompt: "p".into() }).await.unwrap();
        assert_eq!(a.findings[0].title, "from-alpha");
        let b = reg.run(LensRequest { route_key: "beta".into(), prompt: "p".into() }).await.unwrap();
        assert_eq!(b.findings[0].title, "from-beta");
    }

    #[tokio::test]
    async fn unknown_model_is_unavailable() {
        let reg = ProviderRegistry::new();
        let e = reg.run(LensRequest { route_key: "ghost".into(), prompt: "p".into() }).await;
        assert!(matches!(e, Err(ProviderError::Unavailable(_))));
    }

    const SCAN_JSON: &str = r#"{"suspicions":[],"scanned_areas":[]}"#;

    fn scan_turn() -> AssistantTurn {
        AssistantTurn { text: SCAN_JSON.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: None, reasoning_tokens: None }
    }

    fn prompt() -> ScanPrompt {
        ScanPrompt { system: "h".into(), blocks: vec![PromptBlock { text: "b".into(), cache_boundary: false }] }
    }

    fn rate_limited() -> ProviderError {
        ProviderError::RateLimited { retry_after: None }
    }

    /// 批 F-2：scan 表按 route_key 路由；未注册 → Unavailable（不碰旧表）。
    #[tokio::test]
    async fn scan_routes_and_unknown_is_unavailable() {
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 4, Box::new(FakeChatBackend::repeating(scan_turn())), Duration::from_secs(5), 0);
        assert!(reg.has_scan("p/a") && !reg.has_scan("p/b") && reg.scan_len() == 1 && reg.is_empty());
        let call = reg.scan("p/a", &prompt()).await.unwrap();
        assert_eq!((call.iterations, call.rate_limit_retries, call.rate_limit_wait_ms), (1, 0, 0));
        let e = reg.scan("p/b", &prompt()).await.unwrap_err();
        assert!(matches!(e, ProviderError::Unavailable(ref m) if m.contains("scanner not in registry")), "{e}");
        assert_eq!(reg.inflight_peaks().get("p"), Some(&1));
        assert_eq!((reg.provider_of("p/a"), reg.provider_of("p/b"), reg.cap_of("p"), reg.cap_of("q")), (Some("p"), None, Some(4), None));
        assert!(reg.rate_limit_meta().is_empty() && reg.gate_warnings().is_empty(), "无 429 不产键不告警");
        // 排队告警按 provider 计：5 格 / 门宽 4 → 1 格排队；未注册条目不计；≤ 门宽不告警。
        let w = reg.queue_warnings(["p/a", "p/a", "p/a", "p/a", "p/a", "ghost/x"]);
        assert_eq!(w.len(), 1);
        assert!(w[0].starts_with("scanner cells 5 on provider p > maxConcurrency 4: 1 cell(s) queued") && w[0].contains("raise providers.p.maxConcurrency"), "{w:?}");
        assert!(reg.queue_warnings(["p/a", "p/a"]).is_empty());
    }

    /// 批 F-2：同 provider 两条目共享一把在途门——帽 2、8 个并发调用 → 两个 backend 的峰值之和 ≤ 2；异 provider 各自独立。
    #[tokio::test]
    async fn scan_gate_is_shared_per_provider_and_caps_inflight() {
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 2, Box::new(FakeChatBackend::repeating(scan_turn()).with_delay(Duration::from_millis(40))), Duration::from_secs(5), 0);
        reg.insert_scan("p/b", "p", 99, Box::new(FakeChatBackend::repeating(scan_turn()).with_delay(Duration::from_millis(40))), Duration::from_secs(5), 0);
        reg.insert_scan("q/c", "q", 3, Box::new(FakeChatBackend::repeating(scan_turn()).with_delay(Duration::from_millis(40))), Duration::from_secs(5), 0);
        let pr = prompt();
        let futs = (0..8).map(|i| {
            let key = if i % 2 == 0 { "p/a" } else { "p/b" };
            let pr = &pr;
            let reg = &reg;
            async move { reg.scan(key, pr).await.map(|_| ()) }
        });
        let qs = (0..5).map(|_| { let pr = &pr; let reg = &reg; async move { reg.scan("q/c", pr).await.map(|_| ()) } });
        let (rp, rq) = tokio::join!(futures::future::join_all(futs), futures::future::join_all(qs));
        assert!(rp.iter().all(Result::is_ok) && rq.iter().all(Result::is_ok));
        let peaks = reg.inflight_peaks();
        assert_eq!(peaks.get("p"), Some(&2), "同 provider 共享帽 2（首次 cap 为准）: {peaks:?}");
        assert_eq!(peaks.get("q"), Some(&3), "{peaks:?}");
    }

    /// D13 ①：门宽 3；p/b 两格慢 OK 先在途（200 ms），p/a 首发 429 → 门宽收敛到在途接受数 2 → 等到一次完成再准入重试 → OK；
    /// 三格全成功，RunMeta / warnings 记 429 × 1、3 → 2；ScanCall 回填 429 重试次数与等待时长。
    #[tokio::test]
    async fn gate_shrinks_to_accepted_count_on_429_and_waits_for_a_completion() {
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 3, Box::new(FakeChatBackend::new_results(vec![Err(rate_limited()), Ok(scan_turn())])), Duration::from_secs(5), 0);
        reg.insert_scan("p/b", "p", 3, Box::new(FakeChatBackend::repeating(scan_turn()).with_delay(Duration::from_millis(200))), Duration::from_secs(5), 0);
        let pr = prompt();
        let slow = || { let pr = &pr; let reg = &reg; async move { reg.scan("p/b", pr).await.map(|_| ()) } };
        let t0 = std::time::Instant::now();
        let rl = async {
            tokio::time::sleep(Duration::from_millis(30)).await; // 让两个慢格先在途
            (reg.scan("p/a", &pr).await.map(|c| (c.rate_limit_retries, c.rate_limit_wait_ms)), t0.elapsed())
        };
        let (a, b, (ra, took)) = tokio::join!(slow(), slow(), rl);
        assert!(a.is_ok() && b.is_ok(), "{a:?} {b:?}");
        let (retries, waited) = ra.expect("撞 429 的格最终成功");
        assert_eq!(retries, 1);
        assert!(waited >= 150 && took >= Duration::from_millis(190), "撞 429 者等到一次完成才重试，而非立即重试: waited {waited} ms, took {took:?}");
        let st = reg.gate_stats()["p"];
        assert_eq!((st.cap, st.effective_cap, st.rate_limited, st.peak, st.completions), (3, 2, 1, 3, 3), "{st:?}");
        assert_eq!(reg.rate_limit_meta()["p"], RateLimitMeta { hits: 1, cap: 3, effective_cap: 2 });
        let w = reg.gate_warnings();
        assert!(w.len() == 1 && w[0].contains("provider p: 429 x 1, maxConcurrency 3 -> 2") && w[0].contains("providers.p.maxConcurrency <= 2"), "{w:?}");
        // 收窄后的排队告警口径：3 格 > 有效门宽 2 → 说明是收窄所致，不劝「调大」。
        let q = reg.queue_warnings(["p/a", "p/b", "p/b"]);
        assert!(q.len() == 1 && q[0].contains("> effective maxConcurrency 2 (configured 3, shrunk after 429)") && !q[0].contains("raise"), "{q:?}");
    }

    /// D13 ②：独自撞 429（无他人在途）→ 退避探测；超过 max(4, 门宽 × 2) 次即放弃，错误原样返回，抖动预算不参与；门宽收敛到 1。
    #[tokio::test]
    async fn gate_alone_on_429_backs_off_and_gives_up_after_bound() {
        let gate = InflightGate::new(1);
        let policy = GateAttempts::with_backoff_base(&gate, Duration::from_millis(1));
        let calls = AtomicU32::new(0);
        let r: Result<(), ProviderError> = crate::provider::retry_classified_with(2, Some(&policy), || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(rate_limited()) }
        })
        .await;
        assert!(matches!(r, Err(ProviderError::RateLimited { .. })), "{r:?}");
        assert_eq!(calls.load(Ordering::SeqCst), 5, "首发 + 4 次 429 重试（下限 4）");
        assert_eq!(policy.retries(), 4, "放弃的那次不算重试");
        let st = gate.stat();
        assert_eq!((st.cap, st.effective_cap, st.rate_limited, st.peak, st.completions), (1, 1, 5, 1, 0), "{st:?}");
        assert_eq!(gate.lock().active, 0, "放弃后不留预占");
        // 无策略 = 旧行为：429 吃抖动预算（首发 + 2）。
        let calls2 = AtomicU32::new(0);
        let r2: Result<(), ProviderError> = crate::provider::retry_classified_with(2, None, || {
            calls2.fetch_add(1, Ordering::SeqCst);
            async { Err(ProviderError::RateLimited { retry_after: Some(Duration::from_millis(1)) }) }
        })
        .await;
        assert!(r2.is_err() && calls2.load(Ordering::SeqCst) == 3);
    }

    /// D13 ③：门宽 3 取满后收窄到 1——释放两个名额都不放人（多余名额丢弃），释放第三个才进。
    #[tokio::test]
    async fn gate_drops_surplus_permits_after_shrink() {
        let gate = InflightGate::new(3);
        let a = gate.enter().await;
        let b = gate.enter().await;
        let c = gate.enter().await;
        gate.shrink_to(1);
        let mut waiter = Box::pin(gate.enter());
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut waiter).await.is_err(), "满门等待");
        drop(a);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut waiter).await.is_err(), "held 3 → 2 仍 > cap 1：名额丢弃");
        drop(b);
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut waiter).await.is_err(), "held 2 → 1 = cap：仍不放");
        drop(c);
        let _d = tokio::time::timeout(Duration::from_millis(200), &mut waiter).await.expect("held 1 → 0 < cap 1：放入");
        assert_eq!(gate.stat().effective_cap, 1);
    }

    /// D13 评审修（高危 + 中危）：在途归零广播后 429 等待者不齐发——先退避再准入，准入受收窄门宽约束（每次成功完成只放一个）；
    /// 非 429 失败（Timeout 等）不算完成、不放行等待者；取消（守卫 Drop）按失败收尾不泄漏 active。
    #[tokio::test]
    async fn gate_waiters_admitted_one_per_completion_under_shrunk_cap() {
        let gate = InflightGate::new(3);
        let p1 = GateAttempts::with_backoff_base(&gate, Duration::from_millis(1));
        let p2 = GateAttempts::with_backoff_base(&gate, Duration::from_millis(1));
        let p3 = GateAttempts::with_backoff_base(&gate, Duration::from_millis(1));
        p1.attempt_begin(); // P1 在途（慢）
        p2.attempt_begin();
        p2.attempt_end(AttemptOutcome::RateLimited); // P2 撞 429：others = 1 → cap 1
        p3.attempt_begin();
        p3.attempt_end(AttemptOutcome::RateLimited); // P3 撞 429：others = 1 → cap 1
        let mut w2 = Box::pin(p2.on_rate_limited(1, None));
        let mut w3 = Box::pin(p3.on_rate_limited(1, None));
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut w2).await.is_err(), "有人在途：等完成");
        assert!(tokio::time::timeout(Duration::from_millis(20), &mut w3).await.is_err());
        assert_eq!(gate.stat().effective_cap, 1, "收门发生在 on_rate_limited 首次 poll 时");
        p1.attempt_end(AttemptOutcome::Failed); // Timeout 类失败：不算完成，但在途归零 → 广播
        assert_eq!(gate.stat().completions, 0, "失败不算完成");
        // 两个等待者都被广播唤醒 → 各自退避 1 ms → 准入只放一个（cap 1）
        assert_eq!(tokio::time::timeout(Duration::from_millis(200), &mut w2).await.ok(), Some(true), "先被 poll 的准入");
        assert!(tokio::time::timeout(Duration::from_millis(30), &mut w3).await.is_err(), "门满（active 1 = cap 1）：回去等下一次完成");
        assert_eq!(gate.lock().active, 1, "准入预占在途");
        p2.attempt_begin(); // 预占不再加一
        assert_eq!(gate.lock().active, 1);
        p2.attempt_end(AttemptOutcome::Ok); // 一次成功完成 → 只放一个
        assert_eq!(tokio::time::timeout(Duration::from_millis(200), &mut w3).await.ok(), Some(true));
        assert_eq!((gate.stat().completions, gate.lock().active, p2.retries(), p3.retries()), (1, 1, 1, 1));
    }
}
