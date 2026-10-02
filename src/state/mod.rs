//! 作业运行时存取（design 2026-07-03 #8：state 层入库 `.redteam/proj.db`）。
//! 曾为「系统 temp + workspace bucket 分桶 + TTL prune」的临时文件层——sqlite 单库后
//! 整层退役：评审结果是项目资产持久存储、跨项目状态查询 YAGNI、REDTEAM_STATE_DIR 退役。
//!
//! M17-c 夹层合并（design 2026-07-24-redteam-m17c-state-store-design）：原 job.rs（类型群+
//! 4 自由函数）+ store.rs（CellInput/CellRecord+9 自由函数）收敛进本文件单一 `JobStore` 类型——
//! 连接一次复用（`db: Mutex<ProjDb>`，方法级锁、不跨 await）、错误 anyhow 直通、写路径收口为
//! 「create_job / update_if_active / #[cfg(test)] put_state_for_test」三形态、result 列级原子写。
//! 并发四件套语义（守卫 CAS/首个终态封盘/pid 身份锚/惰性收尸/panic hook）原样保留。

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Mutex;

// ── 类型区（原 job.rs，serde 属性逐字节不动）───────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Pending,
    Running,
    Done,
    Failed,
    Cancelled,
    #[serde(rename = "timed_out")]
    TimedOut,
}

impl JobStatus {
    /// 终态：不可再转移，落定后永不被覆盖（E12 首个终态封盘）。
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            JobStatus::Done | JobStatus::Failed | JobStatus::Cancelled | JobStatus::TimedOut
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Collecting, // worker 起手，收集证据，fanout 前
    Fanout,     // 旧 finding 扇出（F-4 后不再打；旧作业 state 可读）
    Scan,       // 批 F-5：scanner 静态扇出（cell 并发执行中，progress.done 推进）
    Deep,       // M21b 批 C：深挖队列消费中
    // M21b 批 C 占位、批 D 做了流式复核却没接上打点（全仓零产品写入，M21c 批 C 的状态机盘点查出来）。
    // 现在打在**复核尾段**：深挖队列排空、就剩复核还在跑的那一段——`verify_tail_ms` 量的正是它。
    // 复核与深挖重叠的部分仍算 Deep，这里标的是「深挖该收了，在等复核」。
    Verify,
    Finalizing, // 尾段：合并 / 报告（progress/last_active 静止）
    Refute,     // 旧 verifier 子阶段（serde 仍可读；新作业打 Verify）
}

/// phase 时间线一格（2026-07-03 grill 定案：finalizing 黑盒可观测）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PhaseStamp {
    pub phase: Phase,
    pub at: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub done: u32,
    pub total: u32,
}

/// 深挖开跑时落的队列头（M21b C2 评审 #1 / #2 / #5）：resume 靠它区分「首跑是 scan-only」与「深挖开过但被 kill」，
/// 并沿用首跑的预算——被 kill 的作业没有原报告，按当时配置 / 工作树重算 K 会把首跑已选中的项截掉。
/// 扇出前写（此时 job 仍 Running）；旧作业缺字段 → `None`，resume 退回「有队列快照才续挖」的口径。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeepHeader {
    /// 本作业深挖预算 K（`budget_cells`）。
    pub budget_cells: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobState {
    pub job_id: String,
    pub status: JobStatus,
    pub started_at: u64,
    #[serde(default)]
    pub progress: Progress,
    #[serde(default)]
    pub models: Vec<String>,
    /// v4：本作业活跃的角色集（design §2）；F-2 起填，旧 state 缺字段回空。枚举而非裸串：写错编译期即报。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub roles: Vec<crate::domain::Role>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pid: Option<u32>,
    /// pid 身份锚：`pid` 对应进程的启动时刻（Unix 秒），防 pid 复用误判（E12-Q3）。
    /// 父进程 spawn worker 后记录；旧状态缺字段 default None → 降级纯存活探测。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub pid_start: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub result_path: Option<String>,
    #[serde(default)]
    pub last_active: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub deadline_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub phase: Option<Phase>,
    /// phase 转换时间线（append-only；旧 state 缺字段 default 空）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub phase_history: Vec<PhaseStamp>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub workspace: Option<String>,
    /// 深挖队列头（配了 reviewer 的作业在扇出前写；scan-only 作业恒 `None`）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub deep: Option<DeepHeader>,
}

impl JobState {
    pub fn new(job_id: &str, started_at: u64) -> Self {
        JobState {
            job_id: job_id.to_string(),
            status: JobStatus::Pending,
            started_at,
            progress: Progress::default(),
            models: vec![],
            roles: vec![],
            pid: None,
            pid_start: None,
            exit_code: None,
            error: None,
            result_path: None,
            last_active: started_at,
            deadline_at: None,
            phase: None,
            phase_history: vec![],
            workspace: None,
            deep: None,
        }
    }

    /// 设当前 phase 并 append 时间线（所有 phase 赋值点统一走这里，防漏 stamp）。
    pub fn set_phase(&mut self, phase: Phase, note: Option<String>) {
        self.phase = Some(phase);
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.phase_history.push(PhaseStamp { phase, at, note });
    }
}

// ── manifest / partial 数据结构（原 store.rs，serde 属性逐字节不动）──────────────

/// manifest 每行：输入快照（含渲染后 prompt 全文）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellInput {
    pub cell: String,
    pub model: String,
    /// 扁平 prompt：scan cell 留空（全文在 `scan`，不双存——每 cell 可达 MB 级，评审帽外项）；旧路径 / 旧作业为渲染后全文。
    pub prompt: String,
    /// scanner cell 的完整请求（system + 分块 + cache 断点，批 F-2 prd K7）：resume 差集重放的唯一输入——扁平串会丢 system
    /// 与断点。旧路径 / 旧作业为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan: Option<crate::prompt::ScanPrompt>,
    /// 渲染 `scan` 时的模板哈希（`prompt::scan_template_hash`）：resume 重放盖此值而非当前二进制的（评审修 6，A/B 归因不失真）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_hash: Option<String>,
}

/// partial 每 cell：单个 cell 的执行结果（db 同键覆盖：重试成功覆盖失败记录）。
/// v4（D11-7 R14）：输出按角色分叉为 [`CellOutput`]（scanner 疑点 / reviewer findings / verifier 判决），一 cell 一条记录；
/// 角色只有一个真相源——`output` 的枚举 tag，[`CellRecord::role`] 与 `cells.role` 列都由它派生（不另存字段，评审裁定 2026-09-08）。
/// 旧 v3 记录（`findings` 平铺、无 output）不再可读——由 `db::USER_VERSION` 守卫保证运行时读不到。
/// 运行学字段（duration/tokens/attempts，2026-07-03 grill 定案）serde default——缺字段零值。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellRecord {
    pub cell: String,
    pub model: String,
    /// 模型家族（agreement 计家族数、verifier 异族选择的口径，design §5 / §8.1）：取角色条目 `EffectiveModel.family`
    /// （批 A：`family` 缺省 model_id 首段小写），由调用方经 [`CellRecord::with_family`] 填；未知为空串。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub family: String,
    pub ok: bool,
    pub output: crate::domain::CellOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    /// 排队等待毫秒（provider 在途门，唯一闸，D13），含在 `duration_ms` 内；扇出路径填，resume / 旧记录 0
    /// （M21a 出口验收：PR181 7 格 / 角色 workers 4 时 100-140 s 排队与生成时长不可分——角色帽随 D13 删除）。
    #[serde(default)]
    pub queue_wait_ms: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// 命中缓存读的输入量（input_tokens 子集，L1 缓存观测）；缺失/未知记 0。
    #[serde(default)]
    pub cached_input_tokens: u64,
    /// 候补链实际尝试的 model 数（成功=第几家成功；整链失败=链长）。
    #[serde(default)]
    pub attempts: u32,
    /// 胜出 call 的 tool-loop 迭代数（计 FINALIZE、不计 turn 级重试；单发单收=1）。
    /// 0＝失败/未观测（旧记录、整链失败、resume 格）。
    #[serde(default)]
    pub iterations: u32,
    /// **单轮**最大输出 token（M21b 出口验收 D3）：`output_tokens` 是多轮之和（首轮 + repair / 二阶段），
    /// 而输出上限 `max_tokens` 是逐轮生效的——判「离撞帽还有多远」只能看单轮。anthropic 口径下含思考 token。
    /// 0 = 未观测（旧记录、scanner 格暂不填、失败格）。`record_json` 整条存，加字段无需迁移。
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub max_turn_output_tokens: u64,
    /// 工具调用构成（延迟诊断仪器：模型有没有用 read_files 批量、每轮读多少、撞顶时新读 vs 空转）。
    /// serde default——旧记录/单发路径/整链失败为零值。
    #[serde(default)]
    pub tool_use: crate::tools::ToolUse,
    /// 渲染该 cell 所用模板（共享头 / 角色尾）的哈希（D-4 填；前缀稳定回归与 cache 归因用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_hash: Option<String>,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

impl CellRecord {
    /// 便捷构造：ok=true, warn=None, family 空（运行学字段与 family 由调用方按需回填）。
    pub fn ok(cell: impl Into<String>, model: impl Into<String>, output: crate::domain::CellOutput) -> Self {
        Self {
            cell: cell.into(), model: model.into(), family: String::new(), ok: true, output, warn: None,
            duration_ms: 0, queue_wait_ms: 0, input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, attempts: 0, iterations: 0,
            max_turn_output_tokens: 0, tool_use: Default::default(), template_hash: None,
        }
    }

    /// 回填家族（来源：角色条目 `EffectiveModel.family`）。
    pub fn with_family(mut self, family: impl Into<String>) -> Self {
        self.family = family.into();
        self
    }

    /// 记录角色 = `output` 的枚举 tag（唯一真相源）。
    pub fn role(&self) -> crate::domain::Role {
        self.output.role()
    }

    /// 失败格：ok=false + 该角色的空输出占位（类型仍明确），warn 带原因。
    pub fn failed(cell: impl Into<String>, model: impl Into<String>, role: crate::domain::Role, warn: impl Into<String>) -> Self {
        let mut rec = Self::ok(cell, model, crate::domain::CellOutput::empty(role));
        rec.ok = false;
        rec.warn = Some(warn.into());
        rec
    }
}

// ── JobStore：state 夹层合并核心（design §2）────────────────────────────────────

/// 作业运行时存取的唯一入口：连接一次复用（rusqlite `Connection` Send 不 Sync → `Mutex` 包一层），
/// 方法全同步、方法级锁、不跨 await——与旧「各开连接+SQLite 写锁排队」语义等价
/// （`busy_timeout(5s)` 跨进程兜底不变）。
pub struct JobStore {
    db: Mutex<crate::db::ProjDb>,
}

impl JobStore {
    /// 打开项目库（一次 `ProjDb::open`，此后同一 `JobStore` 实例内复用）。
    pub fn open(ws: &Path) -> anyhow::Result<Self> {
        Ok(Self { db: Mutex::new(crate::db::ProjDb::open(ws)?) })
    }

    // ── 写（全收口，仅此三形态）────────────────────────────────────────────

    /// 唯一整状态写：Pending 首建（job_id 不存在时的起手状态）。
    pub fn create_job(&self, id: &str, started_at: u64) -> anyhow::Result<()> {
        self.overwrite_state(&JobState::new(id, started_at))
    }

    /// 守卫式状态更新（E12 并发组核心）：db 事务内取现状 → 反序列化 → **若已终态则整个更新放弃**
    /// （首个终态封盘：任一终态落定后，progress/终态/任何字段都不再被写）→ 否则 `mutate` 改后写回。
    /// 返回是否实际写入（`false`=被终态封盘拦下 / job 不存在 / 反序列化失败）。
    /// 所有 job 状态写点（进度、phase、终态、cancel、收尸、panic 兜底）统一走这里，杜绝非原子 RMW。
    pub fn update_if_active(&self, id: &str, mutate: impl FnOnce(&mut JobState)) -> anyhow::Result<bool> {
        let mut db = self.db.lock().unwrap();
        let mut wrote = false;
        let mut mutate = Some(mutate);
        db.job_update_guarded(id, |cur| {
            let json = cur?; // job 不存在/state 未写：正常静默放弃，非异常
            let mut st: JobState = match serde_json::from_str(&json) {
                Ok(st) => st,
                Err(e) => {
                    // schema 演进导致的反序列化失败与「终态封盘」同为放弃写，但语义不同——
                    // 前者是数据/代码不一致的异常信号，不打印会与正常封盘混淆、难以排查。
                    eprintln!("warn: job {id} state_json 反序列化失败，更新被跳过: {e}");
                    return None;
                }
            };
            if st.status.is_terminal() {
                return None; // 首个终态封盘（正常分支，不告警）
            }
            (mutate.take().unwrap())(&mut st);
            let out = serde_json::to_string(&st).ok()?;
            wrote = true;
            Some(out)
        })?;
        Ok(wrote)
    }

    /// 测试专用：任意 `JobState` 无条件整行覆写（不判活）——预置并发/收尸/D5 等场景的起手态。
    /// 生产写路径只有 create_job/update_if_active 两形态（design §2「写三形态收口」）。
    #[cfg(test)]
    pub fn put_state_for_test(&self, st: &JobState) -> anyhow::Result<()> {
        self.overwrite_state(st)
    }

    /// 内部：无条件整状态覆写，`create_job`/`put_state_for_test` 共用；本模块尾部 M17-c
    /// 过渡兼容壳（Task 3 随壳删除）也借它实现旧 `write_state` 自由函数语义。
    fn overwrite_state(&self, st: &JobState) -> anyhow::Result<()> {
        let json = serde_json::to_string(st)?;
        self.db.lock().unwrap().job_put_state(&st.job_id, &json)
    }

    // ── 读（语义平移）──────────────────────────────────────────────────────

    /// 读状态；不存在或解析失败 → None。
    pub fn read_state(&self, id: &str) -> Option<JobState> {
        self.db.lock().unwrap().job_get_state(id).ok().flatten().and_then(|j| serde_json::from_str(&j).ok())
    }

    /// 列出本项目全部 job 状态（跨项目聚合已随 workspace bucket 退役，YAGNI 定案）。
    pub fn list_states(&self) -> Vec<JobState> {
        self.db
            .lock()
            .unwrap()
            .job_all_states()
            .ok()
            .map(|rows| rows.iter().filter_map(|j| serde_json::from_str(j).ok()).collect())
            .unwrap_or_default()
    }

    // ── manifest（语义平移）───────────────────────────────────────────────

    /// 写全部 manifest（一次性整列覆盖）。
    pub fn write_manifest(&self, id: &str, inputs: &[CellInput]) -> anyhow::Result<()> {
        let json = serde_json::to_string(inputs)?;
        self.db.lock().unwrap().job_put_manifest(id, &json)
    }

    /// 读 manifest；缺失/损坏 → 空（调用方按「空 manifest」护栏处理）。
    pub fn read_manifest(&self, id: &str) -> Vec<CellInput> {
        self.db
            .lock()
            .unwrap()
            .job_get_manifest(id)
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str(&j).ok())
            .unwrap_or_default()
    }

    // ── partial（cell checkpoint，语义平移）───────────────────────────────

    /// 单 cell 结果落库（同 (job,cell) 覆盖——重试成功覆盖失败行，读取端零合并逻辑）。
    pub fn append_partial(&self, id: &str, record: &CellRecord) -> anyhow::Result<()> {
        let json = serde_json::to_string(record)?;
        self.db.lock().unwrap().cell_put(id, &record.cell, record.role().as_str(), &json)
    }

    /// 读全部 cell 记录；坏行跳过（坏一条不毁全部）。
    pub fn read_partial(&self, id: &str) -> Vec<CellRecord> {
        self.db
            .lock()
            .unwrap()
            .cell_all(id)
            .ok()
            .map(|rows| rows.iter().filter_map(|j| serde_json::from_str(j).ok()).collect())
            .unwrap_or_default()
    }

    /// 写一条队列快照；失败返回 Err，调用方 warning 不败 job。
    pub fn put_queue_item(&self, job_id: &str, item: &crate::orchestrate::queue::QueueItem) -> anyhow::Result<()> {
        let json = serde_json::to_string(item)?;
        let state = match item.state {
            crate::orchestrate::queue::ItemState::Pending => "pending",
            crate::orchestrate::queue::ItemState::InFlight => "inflight",
            crate::orchestrate::queue::ItemState::Settled => "settled",
            crate::orchestrate::queue::ItemState::Skipped => "skipped",
        };
        self.db.lock().unwrap().queue_item_put(job_id, &item.id, state, &json)
    }

    /// 读作业队列快照。空 = 旧作业 / 无深挖。
    pub fn read_queue_items(&self, job_id: &str) -> Vec<crate::orchestrate::queue::QueueItem> {
        self.db
            .lock()
            .unwrap()
            .queue_items_for_job(job_id)
            .ok()
            .map(|rows| rows.into_iter().filter_map(|(_, _, json)| serde_json::from_str(&json).ok()).collect())
            .unwrap_or_default()
    }

    // ── args / result（语义平移；result 改走单列原子 upsert，M17-c §5）──────

    pub fn write_args_raw(&self, id: &str, json: &str) -> anyhow::Result<()> {
        self.db.lock().unwrap().job_put_args(id, json)
    }

    pub fn read_args_raw(&self, id: &str) -> Option<String> {
        self.db.lock().unwrap().job_get_args(id).ok().flatten()
    }

    /// 评审结果持久存储（json 列；永不过期，评审史资产）。单列 upsert 原子写——
    /// 不再「读 md 现值再写两列」，双事务窗口消失（design §5）。
    pub fn write_result(&self, id: &str, json: &str) -> anyhow::Result<()> {
        self.db.lock().unwrap().result_put_json(id, json)
    }

    pub fn read_result(&self, id: &str) -> Option<String> {
        self.db.lock().unwrap().result_get(id).ok().flatten().map(|(j, _)| j).filter(|s| !s.is_empty())
    }

    /// 同上，md 列对称。
    pub fn write_result_md(&self, id: &str, md: &str) -> anyhow::Result<()> {
        self.db.lock().unwrap().result_put_md(id, md)
    }

    pub fn read_result_md(&self, id: &str) -> Option<String> {
        self.db.lock().unwrap().result_get(id).ok().flatten().map(|(_, m)| m).filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── 纯 serde/类型测试（无 db 参与，原样迁移自 job.rs/store.rs）─────────────

    #[test]
    fn jobstate_new_fields_default_and_roundtrip() {
        let mut s = JobState::new("j", 100);
        s.last_active = 150;
        s.deadline_at = Some(2500);
        s.status = JobStatus::TimedOut;
        let j = serde_json::to_string(&s).unwrap();
        let back: JobState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.last_active, 150);
        assert_eq!(back.deadline_at, Some(2500));
        assert_eq!(back.status, JobStatus::TimedOut);
        // 旧文件（无新字段）可读
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[]}"#
        ).unwrap();
        assert_eq!(old.last_active, 0);
        assert_eq!(old.deadline_at, None);
    }

    #[test]
    fn jobstate_ignores_legacy_lenses_key() {
        // E8-I：旧 proj.db 记录仍带 "lenses" 键——serde 静默忽略，resume/status 不炸。
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[],"lenses":[]}"#
        ).unwrap();
        assert_eq!(old.job_id, "x");
        assert_eq!(old.status, JobStatus::Running);
    }

    #[test]
    fn phase_serializes_lowercase_and_roundtrips() {
        let mut s = JobState::new("j", 100);
        s.phase = Some(Phase::Fanout);
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"phase\":\"fanout\""), "phase 应小写序列化: {j}");
        let back: JobState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.phase, Some(Phase::Fanout));
        // 批 F-5：Scan 阶段小写序列化 round-trip；旧 fanout 串仍可读
        assert_eq!(serde_json::to_string(&Phase::Scan).unwrap(), "\"scan\"");
        assert_eq!(serde_json::from_str::<Phase>("\"scan\"").unwrap(), Phase::Scan);
        assert_eq!(serde_json::to_string(&Phase::Deep).unwrap(), "\"deep\"");
        assert_eq!(serde_json::from_str::<Phase>("\"verify\"").unwrap(), Phase::Verify);
        assert_eq!(serde_json::from_str::<Phase>("\"refute\"").unwrap(), Phase::Refute, "旧 refute 仍可读");
    }

    #[test]
    fn phase_and_workspace_absent_default_none() {
        // 旧文件（无 phase/workspace 键）→ None
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[]}"#
        ).unwrap();
        assert_eq!(old.phase, None);
        assert_eq!(old.workspace, None);
    }

    #[test]
    fn deadline_at_none_omits_key() {
        // §3.1 一致性：deadline_at=None 序列化不含键（与 pid 等一致）
        let s = JobState::new("j", 100); // deadline_at=None
        let j = serde_json::to_string(&s).unwrap();
        assert!(!j.contains("deadline_at"), "deadline_at=None 应省略键: {j}");
        let mut s2 = JobState::new("j", 100);
        s2.deadline_at = Some(2500);
        assert!(serde_json::to_string(&s2).unwrap().contains("\"deadline_at\":2500"));
    }

    #[test]
    fn workspace_roundtrips() {
        let mut s = JobState::new("j", 100);
        s.workspace = Some("/Users/x/repo".into());
        let back: JobState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.workspace.as_deref(), Some("/Users/x/repo"));
    }

    #[test]
    fn set_phase_appends_history_with_note() {
        let mut st = JobState::new("j", 0);
        st.set_phase(Phase::Fanout, None);
        st.set_phase(Phase::Refute, Some("9 targets".into()));
        assert_eq!(st.phase, Some(Phase::Refute));
        assert_eq!(st.phase_history.len(), 2);
        assert_eq!(st.phase_history[1].note.as_deref(), Some("9 targets"));
        assert!(st.phase_history[1].at >= st.phase_history[0].at);
        // 旧 state（无 phase_history 字段）反序列化零值兼容
        let old = r#"{"job_id":"x","status":"running","started_at":1}"#;
        let back: JobState = serde_json::from_str(old).unwrap();
        assert!(back.phase_history.is_empty());
    }

    #[test]
    fn pid_start_roundtrips_and_legacy_defaults_none() {
        let mut s = JobState::new("j", 0);
        s.pid_start = Some(1_700_000_000);
        let back: JobState = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back.pid_start, Some(1_700_000_000));
        // 旧状态（无 pid_start 键）→ None
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[]}"#
        ).unwrap();
        assert_eq!(old.pid_start, None);
        // pid_start=None 省略键
        assert!(!serde_json::to_string(&JobState::new("j", 0)).unwrap().contains("pid_start"));
    }

    #[test]
    fn jobstate_roles_roundtrips_and_legacy_defaults_empty() {
        // v4：roles 有值 → 序列化带键；默认空 → 省略；旧 state（无 roles 键）→ 空。
        let mut s = JobState::new("j", 0);
        assert!(!serde_json::to_string(&s).unwrap().contains("roles"));
        s.roles = vec![crate::domain::Role::Scanner, crate::domain::Role::Reviewer];
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains("\"roles\":[\"scanner\",\"reviewer\"]"), "{j}");
        let back: JobState = serde_json::from_str(&j).unwrap();
        assert_eq!(back.roles, s.roles);
        let old: JobState = serde_json::from_str(
            r#"{"job_id":"x","status":"running","started_at":1,"progress":{"done":0,"total":0},"models":[]}"#
        ).unwrap();
        assert!(old.roles.is_empty());
    }

    #[test]
    fn is_terminal_covers_four() {
        assert!(!JobStatus::Pending.is_terminal());
        assert!(!JobStatus::Running.is_terminal());
        assert!(JobStatus::Done.is_terminal());
        assert!(JobStatus::Failed.is_terminal());
        assert!(JobStatus::Cancelled.is_terminal());
        assert!(JobStatus::TimedOut.is_terminal());
    }

    #[test]
    fn cell_record_v4_minimal_json_defaults_runtime_fields() {
        // v4 最小形状（output 带 tag 必有）：运行学字段缺失 → default 零值；family / template_hash 缺失 → 空 / None。
        let j = r#"{"cell":"c","model":"m","ok":true,"output":{"role":"reviewer","findings":[]}}"#;
        let rec: CellRecord = serde_json::from_str(j).unwrap();
        assert_eq!(
            (rec.duration_ms, rec.input_tokens, rec.output_tokens, rec.attempts, rec.iterations),
            (0, 0, 0, 0, 0)
        );
        assert_eq!(rec.role(), crate::domain::Role::Reviewer);
        assert_eq!(rec.family, "");
        assert_eq!(rec.template_hash, None);
        // 顶层不再有 role 键；顶层多余的 role 键被 serde 忽略、不会与 output tag 打架
        assert!(!serde_json::to_string(&rec).unwrap().contains("\"role\":\"reviewer\",\"model\""));
        let j2 = r#"{"cell":"c","role":"scanner","model":"m","ok":true,"output":{"role":"reviewer","findings":[]}}"#;
        assert_eq!(serde_json::from_str::<CellRecord>(j2).unwrap().role(), crate::domain::Role::Reviewer);
    }

    #[test]
    fn cell_record_v3_shape_without_output_is_rejected() {
        // 旧 v3 记录（findings 平铺、无 output）与缺 tag 的 output 都不可读——记录类型不靠猜（D11-7）。
        for old in [
            r#"{"cell":"c","model":"m","ok":true,"findings":[]}"#,
            r#"{"cell":"c","model":"m","ok":true,"output":{"findings":[]}}"#,
        ] {
            assert!(serde_json::from_str::<CellRecord>(old).is_err(), "{old}");
        }
    }

    #[test]
    fn cell_record_v4_roundtrip_all_three_roles() {
        use crate::domain::{CellOutput, Role, ScanOutput, Suspicion};
        let scan = CellRecord::ok("scan[g1]@deepseek/deepseek-v4-flash#1", "deepseek/deepseek-v4-flash", CellOutput::Scan(ScanOutput {
            suspicions: vec![Suspicion { file: "a.rs".into(), line_start: 1, line_end: 2, hint: "h".into(),
                category: None, confidence: 0.3, quote: "q".into() }],
            needs: vec![], scanned_areas: vec![],
        })).with_family("deepseek");
        assert_eq!(scan.role(), Role::Scanner);
        assert_eq!(scan.family, "deepseek");
        let review = CellRecord::ok("bundle[a]@m", "agc/glm-5.3", CellOutput::review(vec![]));
        assert_eq!((review.role(), review.family.as_str()), (Role::Reviewer, ""), "未回填 family 为空串且不序列化");
        assert!(!serde_json::to_string(&review).unwrap().contains("family"));
        let mut verify = CellRecord::failed("verify[f1]@kimi", "kimi", Role::Verifier, "timeout").with_family("kimi");
        verify.template_hash = Some("deadbeef".into());
        assert_eq!((verify.role(), verify.family.as_str(), verify.ok), (Role::Verifier, "kimi", false));
        for rec in [scan, review, verify] {
            let j = serde_json::to_string(&rec).unwrap();
            let v: serde_json::Value = serde_json::from_str(&j).unwrap();
            assert!(v.get("role").is_none(), "顶层无 role 字段（唯一真相源是 output tag）: {j}");
            assert_eq!(v["output"]["role"], rec.role().as_str());
            let back: CellRecord = serde_json::from_str(&j).unwrap();
            assert_eq!(back, rec);
        }
    }

    // ── JobStore 集成测试（迁移自 job.rs/store.rs：fixture 换 store API，断言逐字保留）───

    #[test]
    fn state_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).unwrap();
        let st = JobState {
            job_id: "job1".into(), status: JobStatus::Running,
            started_at: 1000, progress: Progress { done: 1, total: 3 },
            models: vec!["alpha".into()], roles: vec![],
            pid: Some(4242), pid_start: None, exit_code: None, error: None, result_path: None,
            last_active: 1000, deadline_at: None, phase: None, phase_history: vec![], workspace: None,
            deep: Some(DeepHeader { budget_cells: 12 }),
        };
        store.put_state_for_test(&st).unwrap();
        let back = store.read_state("job1").unwrap();
        assert_eq!(back.status, JobStatus::Running);
        assert_eq!(back.progress.total, 3);
        assert_eq!(back.pid, Some(4242));
        assert_eq!(back.deep, Some(DeepHeader { budget_cells: 12 }), "深挖队列头随 state 落盘");
    }

    #[test]
    fn list_states_returns_all_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).unwrap();
        for id in ["a", "b", "c"] {
            store.put_state_for_test(&JobState::new(id, 0)).unwrap();
        }
        let mut ids: Vec<String> = store.list_states().into_iter().map(|s| s.job_id).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn read_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).unwrap();
        assert!(store.read_state("nope").is_none());
    }

    #[test]
    fn update_active_applies_and_freezes_on_terminal() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).unwrap();
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        store.put_state_for_test(&st).unwrap();
        // 非终态 → 应用
        let wrote = store.update_if_active("j", |s| s.status = JobStatus::Done).unwrap();
        assert!(wrote);
        assert_eq!(store.read_state("j").unwrap().status, JobStatus::Done);
        // 已终态 → 拒写（首个终态封盘），status 不动
        let wrote2 = store.update_if_active("j", |s| s.status = JobStatus::Cancelled).unwrap();
        assert!(!wrote2);
        assert_eq!(store.read_state("j").unwrap().status, JobStatus::Done);
    }

    #[test]
    fn update_active_missing_job_returns_false() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).unwrap();
        assert!(!store.update_if_active("nope", |_s| {}).unwrap());
    }

    #[test]
    fn concurrent_progress_no_lost_update() {
        // 10 线程各自独立开连接（各自 JobStore::open）+1，靠守卫串行化 → done 精确 10
        // （无 RMW 丢更新）。锚「跨进程/独立连接」场景，断言逐字保留自旧测。
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let ws: Arc<std::path::PathBuf> = Arc::new(dir.path().to_path_buf());
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        JobStore::open(&ws).unwrap().put_state_for_test(&st).unwrap();
        let mut hs = vec![];
        for _ in 0..10 {
            let ws = Arc::clone(&ws);
            hs.push(std::thread::spawn(move || {
                JobStore::open(&ws).unwrap().update_if_active("j", |s| s.progress.done += 1).unwrap();
            }));
        }
        for h in hs { h.join().unwrap(); }
        assert_eq!(JobStore::open(&ws).unwrap().read_state("j").unwrap().progress.done, 10);
    }

    /// 新增（design §7.3 复用不变式）：Arc<JobStore> 单实例（单连接）被 10 线程共享，
    /// 方法级 Mutex 互斥串行化——与上一测意图不同：上测锚跨进程/独立连接靠 db 守卫串行化，
    /// 本测锚进程内共享连接靠 Mutex 串行化，两者并存互证。
    #[test]
    fn arc_jobstore_shared_connection_no_lost_update() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(JobStore::open(dir.path()).unwrap());
        let mut st = JobState::new("j", 0);
        st.status = JobStatus::Running;
        store.put_state_for_test(&st).unwrap();
        let mut hs = vec![];
        for _ in 0..10 {
            let store = Arc::clone(&store);
            hs.push(std::thread::spawn(move || {
                store.update_if_active("j", |s| s.progress.done += 1).unwrap();
            }));
        }
        for h in hs { h.join().unwrap(); }
        assert_eq!(store.read_state("j").unwrap().progress.done, 10);
    }

    #[test]
    fn manifest_roundtrip_and_missing_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JobStore::open(tmp.path()).unwrap();
        let inputs = vec![CellInput { template_hash: None, cell: "c1".into(), model: "m".into(), prompt: "p".into(), scan: None }];
        store.write_manifest("j1", &inputs).unwrap();
        let back = store.read_manifest("j1");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].cell, "c1");
        assert!(store.read_manifest("nope").is_empty());
    }

    fn rec(cell: &str, ok: bool) -> CellRecord {
        if ok {
            CellRecord::ok(cell, "m", crate::domain::CellOutput::review(vec![]))
        } else {
            CellRecord::failed(cell, "m", crate::domain::Role::Reviewer, "boom")
        }
    }

    #[test]
    fn queue_snapshot_roundtrip_empty_is_old_job() {
        use crate::orchestrate::queue::{EnqueueReq, ItemKind};
        use crate::pack::region::{Origin as QueueOrigin, QueueItemSpec};
        let tmp = tempfile::tempdir().unwrap();
        let store = JobStore::open(tmp.path()).unwrap();
        assert!(store.read_queue_items("j").is_empty(), "零行 = 旧作业");
        let spec = QueueItemSpec::new("a.rs".into(), vec![(1, 2)], QueueOrigin::Rule, vec![]).unwrap();
        let mut q = crate::orchestrate::queue::DeepQueue::new();
        q.enqueue(EnqueueReq { spec, score: 1.0, kind: ItemKind::Primary, families: vec![], arrival: None, activated: false, force_dig: false });
        let item = q.items()[0].clone();
        store.put_queue_item("j", &item).unwrap();
        let back = store.read_queue_items("j");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].id, item.id);
        assert_eq!(back[0].state, item.state);
    }

    #[test]
    fn partial_same_cell_overwrites_retry_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JobStore::open(tmp.path()).unwrap();
        store.append_partial("j1", &rec("c1", false)).unwrap();
        store.append_partial("j1", &rec("c1", true)).unwrap();
        store.append_partial("j1", &rec("c2", true)).unwrap();
        let all = store.read_partial("j1");
        assert_eq!(all.len(), 2, "同 cell 覆盖不追加");
        assert!(all.iter().find(|r| r.cell == "c1").unwrap().ok, "重试成功覆盖失败");
    }

    /// v4：read_partial 读回 role / output 枚举；db `cells.role` 列与记录一致（三角色各一）。
    #[test]
    fn partial_roundtrips_role_and_output_and_role_column_matches() {
        use crate::domain::{CellOutput, Role, ScanOutput};
        let tmp = tempfile::tempdir().unwrap();
        let store = JobStore::open(tmp.path()).unwrap();
        store.append_partial("j1", &CellRecord::ok("s", "deepseek-v4-flash", CellOutput::Scan(ScanOutput::default()))).unwrap();
        store.append_partial("j1", &CellRecord::ok("r", "glm-5.3", CellOutput::review(vec![]))).unwrap();
        store.append_partial("j1", &CellRecord::failed("v", "kimi_k3", Role::Verifier, "x")).unwrap();
        let all = store.read_partial("j1");
        let by = |c: &str| all.iter().find(|r| r.cell == c).unwrap().clone();
        assert_eq!(by("s").role(), Role::Scanner);
        assert!(matches!(by("s").output, CellOutput::Scan(_)));
        assert_eq!(by("r").role(), Role::Reviewer);
        assert_eq!(by("v").role(), Role::Verifier);
        assert!(!by("v").ok);
        let db = store.db.lock().unwrap();
        for (c, role) in [("s", "scanner"), ("r", "reviewer"), ("v", "verifier")] {
            assert_eq!(db.cell_role("j1", c).unwrap().as_deref(), Some(role), "cells.role 列 = 记录 role");
        }
    }

    #[test]
    fn args_and_result_roundtrip_persistent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = JobStore::open(tmp.path()).unwrap();
        store.write_args_raw("j1", r#"{"a":1}"#).unwrap();
        assert_eq!(store.read_args_raw("j1").unwrap(), r#"{"a":1}"#);
        store.write_result("j1", "{\"v\":1}").unwrap();
        store.write_result_md("j1", "# md").unwrap();
        // json 与 md 互不覆盖
        assert_eq!(store.read_result("j1").unwrap(), "{\"v\":1}");
        assert_eq!(store.read_result_md("j1").unwrap(), "# md");
        // 结果永不过期：重复读不消费
        assert!(store.read_result("j1").is_some());
        assert!(store.read_result("nope").is_none());
    }

    // ── 新增（design §7.1/§7.3/§7.4）：JobStore 新 API 专属测试 ───────────────

    #[test]
    fn create_job_first_build_is_pending() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).unwrap();
        store.create_job("j1", 1000).unwrap();
        let st = store.read_state("j1").unwrap();
        assert_eq!(st.status, JobStatus::Pending);
        assert_eq!(st.started_at, 1000);
    }

    /// 复用不变式（design §7.3）：单 store 全生命周期方法序一条连接跑通——
    /// create→update×N→read→append_partial×N→write_result→read_result。
    #[test]
    fn single_store_full_lifecycle_reuses_one_connection() {
        let dir = tempfile::tempdir().unwrap();
        let store = JobStore::open(dir.path()).unwrap();
        store.create_job("j1", 100).unwrap();
        for _ in 0..3 {
            store.update_if_active("j1", |s| s.progress.done += 1).unwrap();
        }
        let st = store.read_state("j1").unwrap();
        assert_eq!(st.progress.done, 3);
        for i in 0..3 {
            store.append_partial("j1", &CellRecord::ok(format!("c{i}"), "m", crate::domain::CellOutput::review(vec![]))).unwrap();
        }
        assert_eq!(store.read_partial("j1").len(), 3);
        store.write_result("j1", r#"{"v":1}"#).unwrap();
        assert_eq!(store.read_result("j1").unwrap(), r#"{"v":1}"#);
    }

    /// 披露测（design §7.4）：result 单列 upsert——两个独立连接各只写一列，交替多次，
    /// 单列 upsert 不读另一列现值，纵使连接独立仍互不覆盖（对照旧「读 md 再写两列」的丢失窗口）。
    #[test]
    fn result_columns_written_by_separate_connections_dont_clobber() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path().to_path_buf();
        let a = JobStore::open(&ws).unwrap();
        let b = JobStore::open(&ws).unwrap();
        let ha = std::thread::spawn(move || {
            for i in 0..50 { a.write_result("j", &format!("{{\"n\":{i}}}")).unwrap(); }
        });
        let hb = std::thread::spawn(move || {
            for i in 0..50 { b.write_result_md("j", &format!("# md {i}")).unwrap(); }
        });
        ha.join().unwrap();
        hb.join().unwrap();
        let json = JobStore::open(&ws).unwrap().read_result("j").unwrap();
        let md = JobStore::open(&ws).unwrap().read_result_md("j").unwrap();
        assert!(json.starts_with("{\"n\":"), "json 列应落定某次写入，非空/未被 md 写覆盖: {json}");
        assert!(md.starts_with("# md "), "md 列应落定某次写入，非空/未被 json 写覆盖: {md}");
    }
}
