//! `out/state.json` 运行态心跳（契约 §3.6 / 计划 5.1–5.3）。
//!
//! - **只服务 job 模式**：本地 review 不写（调用点在 [`crate::job::run`]）。
//! - `phase` 是 job 视角的三值 `preparing → running → finishing`；**永不写终态**——终态唯一真相是
//!   `report.json.status`。首帧在 [`Heartbeat::start`] 里同步落盘（schema 要求 `phase` 必填）。
//! - `llm` 段与 [`crate::provider::registry::ProviderRegistry::snapshot`] 共用 `snapshot_of_gates`，
//!   每帧从 `Arc<InflightGate>` 现读（唯一计数来源 = `active`；429 退避只进 `retrying`）。
//! - 覆写走 [`crate::job::io::atomic_write`]：`state.json.tmp` → `rename`。写失败（文件/目录被删）
//!   **静默**——心跳缺失/过期合法，停更不影响 job 正确性。
//! - [`Heartbeat::shutdown`] 之后不再写帧（`stopped` 闸 + abort 周期任务）。

use crate::job::io;
use crate::provider::registry::{snapshot_of_gates, InflightGate, LlmSnapshot};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `audit-state.schema.json` 的 `schema` 常量。
pub const SCHEMA: &str = "hibot.audit.state/1";
/// 覆写周期：契约上限 5s，取 2s 给写盘与调度抖动留余量。
pub const TICK: Duration = Duration::from_secs(2);

/// job 视角的阶段（附录 F）。**没有终态值**：进程退出即停写。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Running,
    Finishing,
}

impl Phase {
    fn as_u8(self) -> u8 {
        match self {
            Phase::Preparing => 0,
            Phase::Running => 1,
            Phase::Finishing => 2,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            0 => Phase::Preparing,
            1 => Phase::Running,
            _ => Phase::Finishing,
        }
    }
}

#[derive(Clone, Debug)]
struct Progress {
    unit: &'static str,
    done: u64,
    total: u64,
}

struct Inner {
    out_dir: PathBuf,
    job_id: String,
    phase: AtomicU8,
    /// `Arc` 共享的 provider 门表：每帧现读，不复制计数。
    gates: Mutex<Option<BTreeMap<String, Arc<InflightGate>>>>,
    progress: Mutex<Option<Progress>>,
    /// `shutdown` 后置位：周期任务即使还没被 abort 也不再落盘。
    stopped: AtomicBool,
    /// 串行化同步写帧与周期写帧（`atomic_write` 的 tmp 名固定，不能并发写）。
    write_lock: Mutex<()>,
    /// 递增计数（测试/排障用；不落盘）。
    frames: AtomicU64,
}

/// 心跳句柄。`Arc` 可克隆给 `JobDirCellSink`（progress）与 `run_review`（llm）。
pub struct Heartbeat {
    inner: Arc<Inner>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Heartbeat {
    /// 启动心跳：**同步写首帧 `preparing`**（返回时文件已存在），再挂 2s 周期任务。
    pub fn start(out_dir: &Path, job_id: &str) -> Arc<Self> {
        let hb = Arc::new(Self {
            inner: Arc::new(Inner {
                out_dir: out_dir.to_path_buf(),
                job_id: job_id.to_string(),
                phase: AtomicU8::new(Phase::Preparing.as_u8()),
                gates: Mutex::new(None),
                progress: Mutex::new(None),
                stopped: AtomicBool::new(false),
                write_lock: Mutex::new(()),
                frames: AtomicU64::new(0),
            }),
            task: Mutex::new(None),
        });
        hb.write_frame();
        let inner = Arc::clone(&hb.inner);
        let task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // interval 的第一次 tick 立即完成；首帧已在 start 里同步写，跳过。
            tick.tick().await;
            loop {
                tick.tick().await;
                write_frame(&inner);
            }
        });
        *hb.task.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
        hb
    }

    /// 阶段单调前进（不跳段、不回退）；前进时立即落一帧。`preparing → running → finishing`。
    pub fn set_phase(&self, phase: Phase) {
        if self.inner.stopped.load(Ordering::SeqCst) {
            return;
        }
        let cur = Phase::from_u8(self.inner.phase.load(Ordering::SeqCst));
        if phase > cur {
            self.inner.phase.store(phase.as_u8(), Ordering::SeqCst);
            self.write_frame();
        }
    }

    /// 绑定 provider 门表（registry 构造成功后调用）。未绑定（preparing / triage / verify）→ 不写 `llm` 段。
    pub fn attach_llm(&self, gates: BTreeMap<String, Arc<InflightGate>>) {
        *self.inner.gates.lock().unwrap_or_else(|e| e.into_inner()) = Some(gates);
        self.write_frame();
    }

    /// 设置进度总量（review：scan cells 数；triage/verify 未实现分析，不设）。
    pub fn set_progress_total(&self, unit: &'static str, total: u64) {
        let mut p = self.inner.progress.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = p.as_mut() {
            p.unit = unit;
            p.total = total;
            p.done = p.done.min(total);
        } else {
            *p = Some(Progress { unit, done: 0, total });
        }
    }

    /// 完成一格（`on_cell`）：done 饱和到 total（深挖 cell 不把 review 的 scan 进度推过 100%）。
    pub fn bump_done(&self) {
        let mut p = self.inner.progress.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = p.as_mut() {
            p.done = (p.done + 1).min(p.total);
        }
    }

    /// 收尾：确保走到 `finishing`（若还在 `preparing` 先补一帧 `running`，保持不跳段），写最后一帧，
    /// 然后永久停写。幂等；之后调用任何写方法都不再落盘。
    pub fn shutdown(&self) {
        if self.inner.stopped.load(Ordering::SeqCst) {
            return;
        }
        // 在写锁内完成最后两帧并置 stopped：周期任务即使已通过入口检查，拿到锁后也会被二次闸拦下，
        // 不会在 `finishing` 之后再补一帧（5.3「退出后不再写」）。
        {
            let _write = self.inner.write_lock.lock().unwrap_or_else(|e| e.into_inner());
            if self.inner.stopped.load(Ordering::SeqCst) {
                return;
            }
            if Phase::from_u8(self.inner.phase.load(Ordering::SeqCst)) == Phase::Preparing {
                // 不跳段：preparing → running → finishing。
                self.inner.phase.store(Phase::Running.as_u8(), Ordering::SeqCst);
                write_frame_locked(&self.inner);
            }
            if Phase::from_u8(self.inner.phase.load(Ordering::SeqCst)) != Phase::Finishing {
                self.inner.phase.store(Phase::Finishing.as_u8(), Ordering::SeqCst);
                write_frame_locked(&self.inner);
            }
            // 先写后停：最后一帧 = finishing（非终态值）；之后任何路径都不再落盘。
            self.inner.stopped.store(true, Ordering::SeqCst);
        }
        if let Some(h) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.abort();
        }
    }

    /// 已落盘帧数（测试断言「退出后停写」）。
    pub fn frames(&self) -> u64 {
        self.inner.frames.load(Ordering::SeqCst)
    }

    fn write_frame(&self) {
        write_frame(&self.inner);
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        if let Some(h) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
            h.abort();
        }
    }
}

fn write_frame(inner: &Inner) {
    let _write = inner.write_lock.lock().unwrap_or_else(|e| e.into_inner());
    // 锁内二次检查：`shutdown` 在持锁时置 stopped，已过入口检查的周期帧在这里被拦下。
    if inner.stopped.load(Ordering::SeqCst) {
        return;
    }
    write_frame_locked(inner);
}

/// 写一帧；调用方必须已持 `write_lock`（`shutdown` 的最后一帧与周期帧共用）。
fn write_frame_locked(inner: &Inner) {
    // 心跳只读门计数，不持门锁写文件：先在锁内取快照，再释放。
    let llm: Option<LlmSnapshot> = inner
        .gates
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(snapshot_of_gates);
    let progress = inner.progress.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let extensions = llm.as_ref().map(|s| concurrency_extension(s));
    let frame = StateFile {
        schema: SCHEMA,
        job_id: &inner.job_id,
        phase: Phase::from_u8(inner.phase.load(Ordering::SeqCst)),
        at: rfc3339_now(),
        llm,
        progress: progress.map(|p| ProgressOut { unit: Some(p.unit), done: p.done, total: p.total }),
        extensions,
    };
    // 写失败静默：心跳是可选旁路，文件被删/目录不可写都不影响 job 正确性。
    let _ = io::atomic_write_json(&inner.out_dir.join("state.json"), &frame);
    inner.frames.fetch_add(1, Ordering::SeqCst);
}

/// `extensions.redteam.concurrency`：如实披露并发口径（P12：provider 子闸唯一，无 job 总闸）。
fn concurrency_extension(snapshot: &LlmSnapshot) -> serde_json::Value {
    let caps: BTreeMap<&str, u32> = snapshot
        .providers
        .iter()
        .map(|p| (p.name.as_str(), p.max_concurrency))
        .collect();
    serde_json::json!({
        "redteam": {
            "concurrency": {
                "mode": "per-provider",
                "provider_caps": caps,
            }
        }
    })
}

#[derive(serde::Serialize)]
struct StateFile<'a> {
    schema: &'static str,
    job_id: &'a str,
    phase: Phase,
    at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    llm: Option<LlmSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<ProgressOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions: Option<serde_json::Value>,
}

#[derive(serde::Serialize)]
struct ProgressOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    unit: Option<&'static str>,
    done: u64,
    total: u64,
}

fn rfc3339_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    rfc3339_secs(secs)
}

/// Unix 秒 → `YYYY-MM-DDTHH:MM:SSZ`（UTC）。schema 只要求 RFC3339 前缀形态；不引 chrono。
fn rfc3339_secs(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        sod / 3600,
        (sod % 3600) / 60,
        sod % 60
    )
}

/// Howard Hinnant 的 civil_from_days（1970-01-01 起的天数 → 年月日）。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::registry::ProviderRegistry;
    use crate::provider::tool_loop::FakeChatBackend;

    fn read_state(dir: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(dir.join("state.json")).expect("state.json 存在")).expect("合法 JSON")
    }

    fn registry_with_two_providers() -> ProviderRegistry {
        let mut reg = ProviderRegistry::new();
        let turn = crate::provider::tool_loop::AssistantTurn {
            text: "{}".into(),
            tool_calls: vec![],
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            finish_reason: None,
            reasoning_tokens: None,
        };
        reg.insert_scan("p/a", "p", 4, Box::new(FakeChatBackend::repeating(turn.clone())), Duration::from_secs(5), 0);
        reg.insert_scan("q/b", "q", 3, Box::new(FakeChatBackend::repeating(turn)), Duration::from_secs(5), 0);
        reg
    }

    /// 首帧在 `start` 返回前已存在且为 `preparing`；schema/job_id/at 合法；无 `.tmp` 残留。
    #[tokio::test]
    async fn first_frame_is_preparing_and_atomic() {
        let d = tempfile::tempdir().unwrap();
        let hb = Heartbeat::start(d.path(), "J1");
        let v = read_state(d.path());
        assert_eq!(v["schema"], SCHEMA);
        assert_eq!(v["job_id"], "J1");
        assert_eq!(v["phase"], "preparing", "首帧必须是 preparing");
        assert!(v["at"].as_str().unwrap().starts_with("20"), "{}", v["at"]);
        assert!(!d.path().join("state.json.tmp").exists(), "rename 后不留 tmp");
        assert_eq!(v.get("llm"), None, "未建 registry → 不伪造 llm 段");
        hb.shutdown();
    }

    /// 阶段单调前进、不跳段；shutdown 后最后一帧是 finishing（非终态）且不再写。
    #[tokio::test]
    async fn phase_monotonic_and_shutdown_stops_writing() {
        let d = tempfile::tempdir().unwrap();
        let hb = Heartbeat::start(d.path(), "J2");
        hb.set_phase(Phase::Running);
        assert_eq!(read_state(d.path())["phase"], "running");
        hb.set_phase(Phase::Preparing); // 回退被忽略
        assert_eq!(read_state(d.path())["phase"], "running");
        // 从 preparing 直接 shutdown 也要先补 running 再到 finishing（不跳段）。
        let d2 = tempfile::tempdir().unwrap();
        let hb2 = Heartbeat::start(d2.path(), "J3");
        let frames_before = hb2.frames();
        hb2.shutdown();
        assert!(hb2.frames() >= frames_before + 2, "preparing→running→finishing 各一帧");
        assert_eq!(read_state(d2.path())["phase"], "finishing");
        assert!(!["done", "fail"].contains(&read_state(d2.path())["phase"].as_str().unwrap()));
        // 停写：2s 周期 > 多次 tick 后也不再有新帧（对已 shutdown 的 hb2 断言）。
        let frames = hb2.frames();
        tokio::time::sleep(TICK + Duration::from_millis(300)).await;
        assert_eq!(hb2.frames(), frames, "shutdown 后不再写");
        assert_eq!(read_state(d2.path())["phase"], "finishing", "最后一帧保持不变（未被覆盖）");
        hb.shutdown();
    }

    /// `at` 严格递增且采样间隔 ≤ 5s（真实时间；周期 2s）。
    #[tokio::test]
    async fn at_advances_within_five_seconds() {
        let d = tempfile::tempdir().unwrap();
        let _hb = Heartbeat::start(d.path(), "J4");
        let mut stamps = Vec::new();
        for _ in 0..3 {
            stamps.push(read_state(d.path())["at"].as_str().unwrap().to_string());
            tokio::time::sleep(Duration::from_millis(2_100)).await;
        }
        for w in stamps.windows(2) {
            assert!(w[1] > w[0], "at 必须递增: {:?}", stamps);
        }
        // 解析回秒比较间隔（格式固定，字符串可直接切片到秒）。
        let secs: Vec<u64> = stamps.iter().map(|s| hms_to_secs(s)).collect();
        assert!(secs.windows(2).all(|w| w[1] - w[0] <= 5 && w[1] > w[0]), "{stamps:?}");
    }

    fn hms_to_secs(s: &str) -> u64 {
        let (h, m, sec) = (&s[11..13], &s[14..16], &s[17..19]);
        h.parse::<u64>().unwrap() * 3600 + m.parse::<u64>().unwrap() * 60 + sec.parse::<u64>().unwrap()
    }

    /// 心跳 `llm` 与 registry 快照逐字一致（同一函数）；provider 名是配置 key；429 退避只进 retrying。
    #[tokio::test]
    async fn llm_section_matches_registry_snapshot_and_discloses_concurrency() {
        let d = tempfile::tempdir().unwrap();
        let hb = Heartbeat::start(d.path(), "J5");
        let reg = registry_with_two_providers();
        hb.attach_llm(reg.gate_handles());
        let mut v = read_state(d.path());
        assert_eq!(v["llm"], serde_json::to_value(reg.snapshot().unwrap()).unwrap(), "心跳与快照逐字一致");
        assert_eq!(v["extensions"]["redteam"]["concurrency"]["mode"], "per-provider");
        assert_eq!(v["extensions"]["redteam"]["concurrency"]["provider_caps"]["p"], 4);
        assert_eq!(v["extensions"]["redteam"]["concurrency"]["provider_caps"]["q"], 3);
        assert_eq!(v["llm"]["max_concurrency"], 7, "Σ provider 帽");

        // 两条请求在途 → in_flight==2（唯一来源 = active）。
        let pa = reg.try_lease("p/a").unwrap().unwrap();
        let qb = reg.try_lease("q/b").unwrap().unwrap();
        use crate::provider::AttemptPolicy;
        pa.gate_attempts().attempt_begin();
        qb.gate_attempts().attempt_begin();
        hb.set_phase(Phase::Running);
        v = read_state(d.path());
        assert_eq!(v["llm"]["in_flight"], 2);
        assert_eq!(v["llm"]["retrying"], 0);
        // 429 退避中：retrying>0 且不计入 in_flight。
        pa.gate_attempts().attempt_end(crate::provider::AttemptOutcome::RateLimited);
        let policy = pa.gate_attempts();
        let mut fut = Box::pin(policy.on_rate_limited(1, None));
        assert!(futures::poll!(&mut fut).is_pending());
        // 另一次在途请求仍在：in_flight==1，retrying==1。
        hb.set_phase(Phase::Running); // 已是 running，不等于不写；直接用周期帧验证：手动触发一帧
        hb.attach_llm(reg.gate_handles());
        v = read_state(d.path());
        assert_eq!((v["llm"]["in_flight"].as_u64(), v["llm"]["retrying"].as_u64()), (Some(1), Some(1)));
        drop(fut);
        qb.gate_attempts().attempt_end(crate::provider::AttemptOutcome::Ok);
        hb.attach_llm(reg.gate_handles());
        v = read_state(d.path());
        assert_eq!((v["llm"]["in_flight"].as_u64(), v["llm"]["retrying"].as_u64()), (Some(0), Some(0)));
        hb.shutdown();
    }

    /// 文件被删后不 panic；下一帧重建（写失败静默）。
    #[tokio::test]
    async fn deleted_state_file_is_recreated_without_panic() {
        let d = tempfile::tempdir().unwrap();
        let hb = Heartbeat::start(d.path(), "J6");
        std::fs::remove_file(d.path().join("state.json")).unwrap();
        hb.attach_llm(registry_with_two_providers().gate_handles());
        assert!(d.path().join("state.json").exists(), "同步写帧重建文件");
        hb.shutdown();
    }

    /// progress：review 的 cells 总数；done 饱和到 total。
    #[tokio::test]
    async fn progress_tracks_cells_and_saturates() {
        let d = tempfile::tempdir().unwrap();
        let hb = Heartbeat::start(d.path(), "J7");
        hb.set_progress_total("cells", 2);
        hb.bump_done();
        hb.bump_done();
        hb.bump_done();
        hb.attach_llm(BTreeMap::new()); // 触发一帧（gates 空 → 无 llm）
        let v = read_state(d.path());
        assert_eq!(v["progress"], serde_json::json!({"unit": "cells", "done": 2, "total": 2}));
        hb.shutdown();
    }

    /// RFC3339 固定样例（含闰年边界）。
    #[test]
    fn rfc3339_formats_utc() {
        assert_eq!(rfc3339_secs(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_secs(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(rfc3339_secs(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn phase_ordering_is_total() {
        assert!(Phase::Preparing < Phase::Running && Phase::Running < Phase::Finishing);
        assert_eq!(Phase::from_u8(Phase::Finishing.as_u8()), Phase::Finishing);
    }
}
