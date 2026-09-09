use crate::prompt::ScanPrompt;
use crate::provider::scan_shot::{run_scan_shot, ScanCall};
use crate::provider::tool_loop::ChatBackend;
use crate::provider::{LensCall, LensRequest, ProviderError, ReviewProvider};
use async_trait::async_trait;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// provider 级在途门（design §12.3 / D7 A5）：`Semaphore` = 在途上限（provider `maxConcurrency`，缺省继承 `review.max_concurrency`），
/// 同一 provider 下的全部角色条目共享一把（scanner 与 M21b 的 reviewer 同门）；`peak` 记本次进程内观测到的在途峰值（RunMeta
/// `provider_inflight_peak`，铁律 1）。
pub struct InflightGate {
    sem: Semaphore,
    current: AtomicU32,
    peak: AtomicU32,
}

impl InflightGate {
    fn new(cap: u32) -> Self {
        InflightGate { sem: Semaphore::new(cap.max(1) as usize), current: AtomicU32::new(0), peak: AtomicU32::new(0) }
    }
    pub fn peak(&self) -> u32 {
        self.peak.load(Ordering::SeqCst)
    }
    /// 取名额（不计在途：租约可能还在等角色 worker 许可，评审修 7 后门先于 worker）。
    async fn enter(&self) -> Result<tokio::sync::SemaphorePermit<'_>, ProviderError> {
        self.sem.acquire().await.map_err(|_| ProviderError::Unavailable("inflight gate closed".into()))
    }
    /// 真正发请求时计在途（峰值 = 同时在途的 HTTP 请求数，不含等 worker 的租约）。
    fn activate(&self) -> ActiveGuard<'_> {
        let cur = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(cur, Ordering::SeqCst);
        ActiveGuard { gate: self }
    }
}

/// 在途计数守卫：Drop 即减一（panic / 提前返回都安全）。
struct ActiveGuard<'a> {
    gate: &'a InflightGate,
}

impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.gate.current.fetch_sub(1, Ordering::SeqCst);
    }
}

/// scanner 条目（批 F-2 prd K2）：route_key → backend + 单轮超时 + 重试预算 + 所属 provider（取在途门）。
struct ScanEntry {
    backend: Box<dyn ChatBackend>,
    per_turn: std::time::Duration,
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
        per_turn: std::time::Duration,
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

    /// 先过 provider 在途门再拿到条目（评审修 7）：扇出层在门后才取角色 `workers` 许可——饱和 / 限流的 provider 不占 worker 槽，
    /// 其他 provider 的 cell 不被饿死。未注册 → `Unavailable`（该格入 coverage failed）。
    pub async fn lease(&self, route_key: &str) -> Result<ScanLease<'_>, ProviderError> {
        let Some(entry) = self.scan.get(route_key) else {
            return Err(ProviderError::Unavailable(format!("scanner not in registry: {route_key}")));
        };
        let gate = self.gates.get(&entry.provider).ok_or_else(|| ProviderError::Unavailable(format!("no inflight gate for provider {}", entry.provider)))?;
        let permit = gate.enter().await?;
        Ok(ScanLease { entry, gate, _permit: permit })
    }

    /// scanner 调用：过 provider 在途门 → 两轮单发（`lease` + `run`）。
    pub async fn scan(&self, route_key: &str, prompt: &ScanPrompt) -> Result<ScanCall, ProviderError> {
        self.lease(route_key).await?.run(prompt).await
    }
}

/// 已过在途门的 scanner 条目租约：持有期间占该 provider 一个名额；`run` 走两轮单发并计在途峰值。
pub struct ScanLease<'a> {
    entry: &'a ScanEntry,
    gate: &'a InflightGate,
    _permit: tokio::sync::SemaphorePermit<'a>,
}

impl ScanLease<'_> {
    pub async fn run(&self, prompt: &ScanPrompt) -> Result<ScanCall, ProviderError> {
        let _active = self.gate.activate();
        run_scan_shot(self.entry.backend.as_ref(), prompt, self.entry.per_turn, self.entry.retry_budget).await
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

    /// 批 F-2：scan 表按 route_key 路由；未注册 → Unavailable（不碰旧表）。
    #[tokio::test]
    async fn scan_routes_and_unknown_is_unavailable() {
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 4, Box::new(FakeChatBackend::repeating(scan_turn())), Duration::from_secs(5), 0);
        assert!(reg.has_scan("p/a") && !reg.has_scan("p/b") && reg.scan_len() == 1 && reg.is_empty());
        let call = reg.scan("p/a", &prompt()).await.unwrap();
        assert_eq!(call.iterations, 1);
        let e = reg.scan("p/b", &prompt()).await.unwrap_err();
        assert!(matches!(e, ProviderError::Unavailable(ref m) if m.contains("scanner not in registry")), "{e}");
        assert_eq!(reg.inflight_peaks().get("p"), Some(&1));
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
}
