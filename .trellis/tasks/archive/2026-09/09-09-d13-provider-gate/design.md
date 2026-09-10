# design · D13 provider 门唯一闸 + 429 排队重试

> 需求与 K 编号见 `prd.md`；架构真相源 design v10 §12.3（路由与限流）/ §4（scanner 逐 cell 交付、只受 provider 门限流）/ §6 规则 7；决策 D13 Q1-Q5。行号以 HEAD 84dee09 为准。

## 1. 配置层（R1）

```rust
pub struct ScannersCfg  { pub samples: u32 }                       // src/config/mod.rs:261-272
pub struct ReviewersCfg { pub max_cells: MaxCells, pub reenqueue: bool }
pub struct VerifiersCfg { pub enabled: bool }
fn default_concurrency() -> usize { 10 }                           // :372
```
- `validate`（`:669-682`）列表：`review.maxConcurrency` / `review.scanners.samples` / `review.pack.anchorWindow` / `review.grouping.minFiles` / `review.grouping.maxFilesPerGroup`。
- `shape_guard`（`:778-788`）：review 段循环里先 `if so.contains_key("workers")` 推专属 warning，再把去掉 `workers` 的 map 交给通用 `check`（K7）。`REVIEW_SECTIONS`：`("scanners", &["samples"])`、`("reviewers", &["maxCells", "reenqueue"])`、`("verifiers", &["enabled"])`。
- `from_json_warned`（`:494-514`）在 `validate` 后：遍历 providers 的 `max_concurrency > 128` 与 `review.max_concurrency > 128` → warning（K2）。

## 2. provider 门（R3）

### 2.1 数据结构（`src/provider/registry.rs`，替换 `:11-48`）
```rust
pub struct InflightGate {
    state: std::sync::Mutex<GateState>,
    slot_free: tokio::sync::Notify,     // 名额释放 → notify_one：唤醒一个等门者
    completed: tokio::sync::Notify,     // 非 429 请求结束 → notify_one：唤醒一个 429 等待者；active 归零 → notify_waiters
    completions: AtomicU64,             // 非 429 结束计数（等待者比较 seq）
    peak: AtomicU32,                    // 同时在途 HTTP 请求峰值（RunMeta provider_inflight_peak，语义不变）
}
struct GateState { cap_initial: u32, cap: u32, held: u32, active: u32, rate_limited: u32 }
pub struct GatePermit<'a> { gate: &'a InflightGate }               // Drop：held -= 1；held < cap 才 slot_free.notify_one()
pub(crate) struct GateAttempts<'a> { gate: &'a InflightGate, retries: AtomicU32, backoff_base: Duration }  // 每次 ScanLease::run 新建
pub struct GateStat { pub cap: u32, pub effective_cap: u32, pub rate_limited: u32, pub peak: u32 }
```
临界区都是几条整数运算，用 `std::sync::Mutex`（不跨 await 持锁）。

### 2.2 取门 / 放门
```
enter():  loop { { s = lock; if s.held < s.cap { s.held += 1; return GatePermit } }  slot_free.notified().await }
drop():   s = lock; s.held -= 1; let wake = s.held < s.cap; drop(s); if wake { slot_free.notify_one() }
```
`notify_one` 无等待者时存一次许可 → 下一个 `notified()` 立即返回、循环重检 `held < cap`；不会丢唤醒，最多一次虚假唤醒。门收窄到 `cap < held` 时，释放不唤醒 = 多余名额丢弃（K6）。

### 2.3 `AttemptPolicy` 挂点（`src/provider/mod.rs`）
```rust
#[async_trait] pub(crate) trait AttemptPolicy: Send + Sync {
    fn attempt_begin(&self);
    fn attempt_end(&self, rate_limited: bool);
    async fn on_rate_limited(&self, retry_after: Option<Duration>) -> bool;   // true = 已等完请重试；false = 放弃
}
pub(crate) async fn retry_classified_with<T,F,Fut>(budget, policy: Option<&dyn AttemptPolicy>, op) -> Result<T, ProviderError>
```
循环体：`begin` → `op().await` → `rl = matches!(e.inner(), RateLimited{..})` → `end(rl)` → Ok 返回 / `rl && policy` 走 `on_rate_limited`（true `continue` 不计 attempt；false 返错）/ 其余按旧退避与 `budget`。`retry_classified` 保留为 `retry_classified_with(budget, None, op)`；`timed_turn(_final)` 转调 `_with(.., None)`；`scan_shot::run_scan_shot` 转调 `run_scan_shot_with(.., None)`。`single_shot` / `tool_loop` 调用点不动（K11）。

### 2.4 `GateAttempts` 实现
```
attempt_begin:      s.active += 1; peak.fetch_max(s.active)
attempt_end(rl):    s.active -= 1; zero = s.active == 0; drop(s);
                    if !rl { completions += 1; completed.notify_one() }
                    if zero { completed.notify_waiters() }
on_rate_limited(ra):
    n = retries.fetch_add(1) + 1
    { s = lock; s.rate_limited += 1; others = s.active;
      new_cap = max(1, min(s.cap - 1, others)); if new_cap < s.cap { s.cap = new_cap }
      limit = max(4, s.cap * 2); cap_now = s.cap }                       // K5 / K9
    if n > limit { return false }
    if others == 0 { sleep(ra.unwrap_or(backoff(n))).await; return true }  // K8：没人在跑，等完成事件无意义
    seq0 = completions.load()
    wait_completion = async { loop { let nt = completed.notified();
        { s = lock; if completions.load() > seq0 || s.active == 0 { break } }   // active==0 → 其余人也都退出了
        nt.await } }
    join!(wait_completion, sleep(ra.unwrap_or(0)))   // 与 Retry-After 取晚者
    true
backoff(n) = min(backoff_base × 2^(n-1), 30 s)，backoff_base 缺省 500 ms（测试注入 1 ms）
```
`active == 0` 兜底后仍返回 true 让请求立刻重试：此时门上没有在途请求，重试就是探测；再撞则 `others == 0` 走 sleep 退避，由 `limit` 封顶。

### 2.5 registry 接口
```rust
pub fn provider_of(&self, route_key: &str) -> Option<&str>
pub fn cap_of(&self, provider: &str) -> Option<u32>                 // cap_initial
pub fn gate_stats(&self) -> BTreeMap<String, GateStat>
pub fn rate_limit_meta(&self) -> BTreeMap<String, RateLimitMeta>    // 只含 rate_limited > 0
pub fn gate_warnings(&self) -> Vec<String>
pub fn inflight_peaks(&self) -> BTreeMap<String, u32>               // 不变
```
`ScanLease { entry, gate, _permit: GatePermit }`；`run`：`let pol = GateAttempts::new(self.gate); run_scan_shot_with(backend, prompt, per_turn, retry_budget, Some(&pol))`。

## 3. 扇出（R2，`src/orchestrate/mod.rs`）
- `ScanFanoutInput` 去 `workers`（`:105`）；`:222` 信号量与 `:235` clone 删；future 体：`lease` → `queue_wait_ms = t0.elapsed()` → `lease.run(prompt)`。
- 告警（替换 `:265-272`）：`per_provider: BTreeMap<&str, usize>` 由 `plan` 经 `registry.provider_of` 聚合；每 provider `cap = registry.cap_of(p)`；`n > cap` 推告警（K4）。随后 `out.warnings.extend(registry.gate_warnings())`。
- 注释 `:94-95` / `:175-177` / `:223-225` 改「并发 = provider 在途门（唯一闸，D13）」。

## 4. 报告与 cli（R2 / R4）
- `src/domain/report.rs`：`RateLimitMeta { hits, cap, effective_cap }`（`Clone, Debug, PartialEq, Serialize, Deserialize`）；`RunMeta.provider_rate_limit`（skip 空）紧随 `provider_inflight_peak`。
- `src/cli/mod.rs`：`:288` 删 `workers:`；`ScanFinish.rate_limit`；`:321` / `:940` 各加 `rate_limit: registry.rate_limit_meta()`；`:460` 后加 `provider_rate_limit: f.rate_limit`；resume 路径 `join_all` 后 `warnings.extend(registry.gate_warnings())`。
- md 渲染不动：429 收敛信息已通过 run.warnings 进 md。

## 5. 兼容 / 回滚
- 旧配置带 `workers` 键：解析成功 + 专属 warning；行为 = 只受 provider 门。
- 旧 JSON 报告无 `provider_rate_limit`：serde default 空表；新报告空时不产键 → v4 消费者（`eval/scan_score.py`、ship）零噪音。
- `queue_wait_ms` 字段名 / 位置不变，语义收窄为门等待；resume / 旧记录仍 0。
- 回滚点：两个代码 commit 各自可 revert；配置删键回滚会让专属 warning 消失、缺省回 4。

## 5.1 评审修后的 `GateAttempts`（4aabec3，覆盖 §2.4）
```
on_rate_limited(nth, ra):
    { lock; rate_limited += 1; others = active; shrink_to(min(cap−1, others)); limit = max(4, cap×2); seq0 = completions }
    if nth > limit { return false }
    loop {
        if others == 0 { sleep(ra 或 backoff(nth)) }
        else { join!(等 completions > seq0 或 active == 0, sleep(ra)); 若因 active == 0 醒 → sleep(ra 或 backoff(nth)) }
        ra = None
        { lock; if active < cap { active += 1（预占，attempt_begin 不再加）; return true } ; others = active; seq0 = completions }
    }
attempt_end(outcome): active −= 1; outcome == Ok → completions += 1、notify_one；active == 0 → notify_waiters
retry_classified_with：AttemptGuard RAII（丢弃按 Failed）；nth 本调用内计
```

## 6. 取舍
- 不用 `tokio::sync::Semaphore` + `forget_permits`：它只能忘掉**可用**名额，无法把已发出的名额在释放时丢弃；自管计数 + Notify 二十行以内。
- 不做 AIMD 加法回升：本轮回升需要「成功 N 次」的判据，网关限流形态未实测，先只降（RunMeta 可见），R9 数据回来再议。
- 429 等待者用 `notify_one` 而非 `notify_waiters`：每次完成只放一个重试，是「排队」的定义；`active == 0` 才广播。
