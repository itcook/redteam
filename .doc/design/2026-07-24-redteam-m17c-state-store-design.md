# M17-c state 夹层合并 · 设计

> 状态: 锁定 · 2026-07-24 grill 六裁决全数拍板；WHY 见 `../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §C；实施计划 `../plans/2026-07-24-redteam-m17c-state-store-plan.md`；里程碑=roadmap M17-c。

## 1. 目标

state/{job,store}.rs 旧门面收敛为单一 `JobStore` 类型：连接一次复用、错误直通分级披露、
写路径类型收口（顺带堵 D5 新洞）、result 写原子化。**贯穿铁律：并发四件套（守卫 CAS/
首个终态封盘/pid 身份锚/惰性收尸/panic hook）语义原样**——419 项测试断言零改迁移为证。

## 2. JobStore（state/ 收敛单 mod.rs）

```rust
pub struct JobStore { db: Mutex<crate::db::ProjDb> }   // Connection Send 不 Sync → Mutex
impl JobStore {
    pub fn open(ws: &Path) -> anyhow::Result<Self>;    // 一次 ProjDb::open，此后复用
    // ── 写（全收口，仅此三形态）──
    pub fn create_job(&self, id: &str, started_at: u64) -> anyhow::Result<()>;  // 唯一整状态写：Pending 首建
    pub fn update_if_active(&self, id: &str, mutate: impl FnOnce(&mut JobState)) -> anyhow::Result<bool>;
    #[cfg(test)] pub fn put_state_for_test(&self, st: &JobState) -> anyhow::Result<()>;
    // ── 读（语义平移）──
    pub fn read_state(&self, id) -> Option<JobState>;
    pub fn list_states(&self) -> Vec<JobState>;
    // ── manifest/partial/args/result（语义平移，签名 anyhow 直通）──
    write_manifest / read_manifest / append_partial / read_partial /
    write_args_raw / read_args_raw / write_result / read_result / write_result_md / read_result_md
}
```

- 类型区（JobState/JobStatus/Phase/PhaseStamp/Progress/CellInput/CellRecord）原样保留。
- 自由函数**全删不留壳**；`pub write_state` 从生产面消灭。
- db.rs ProjDb 不动（除 §5 result 单列方法）。
- Mutex 锁粒度=方法级，方法全同步不跨 await 持锁；与现状「各开连接+SQLite 写锁排队」
  语义等价（busy_timeout 5s 跨进程兜底不变）。

## 3. D5 修复（grill 新抓，走读+M16 终审双漏）

`main.rs:545-551` Cmd::Worker 起手 deadline_at 裸 RMW（read_state→改→write_state 整状态
回写）——cancel 落在读写间被覆回非终态，首个终态封盘第三例绕过。修：

```rust
let _ = store.update_if_active(&job_id, |st| {
    if st.deadline_at.is_none() { st.deadline_at = Some(started + JOB_DEADLINE_SECS); }
});
```

回归测试：预置 Cancelled → worker 起手 deadline 写 → 断言仍 Cancelled 且 deadline 未写。
D5 挂进走读评审档 P1 表。**不动**：reap_if_dead/run_job 的「读→判断→守卫写」模式
（读写间隙由守卫写时重读重判兜底，非洞）。

## 4. 错误披露四级（按调用场景）

| 级 | 场景 | 行为 |
|---|---|---|
| ① | JobStore 签名 | anyhow::Result 直通（io::Error 包裹层删除） |
| ② | worker 热路径（JobCellSink on_cell/on_phase/on_meta、进度写） | 吞而有痕：eprintln + **AtomicU32 失败计数 → run_job 收尾塞 report.run.warnings**（detached worker stderr=null，warnings 才可见）。不中断评审（resume 差集重放兜底） |
| ③ | 终态写（mark_failed_panic/cancel/收尸/run_job 终态） | 必响：eprintln + 前台路径向上传 Result；panic hook 无通道则 eprintln 尽力 |
| ④ | 一次性命令（status/result/dismiss…） | 直接 `?` 向上冒 |

## 5. 连接复用 + result 原子化

- **三生命周期**：worker 进程 = Cmd::Worker 入口 `Arc<JobStore>` 穿 run_job+JobCellSink
  （sink 从持 ws: PathBuf 改持 Arc<JobStore>）+收尾；一次性命令 = 入口构造一次；
  台账消费面（load_ledger/resolve 卡/incremental_preflight 直用 ProjDb）**本批不动**。
- **result 原子化**：db 层新增 `result_put_json(id, json)` / `result_put_md(id, md)`——
  单 SQL `INSERT ... ON CONFLICT DO UPDATE SET 己列=excluded.己列`，「读另一列再写两列」
  双事务窗口整个消失。store 侧 write_result/write_result_md 改用；双列 result_put 若
  grep 无余生产调用则删。

## 6. 契约冻结面

- 零变化承诺：并发四件套语义、JobState serde 面（含全部旧字段兼容 default）、proj.db
  schema（除新增两个单列 upsert 方法，DDL 不动）、report schema、exit_code、e2e 现有
  5 条零改动通过。
- 预期行为变化仅三处（都是修 bug/补披露）：D5（deadline 写不再覆终态）；检查点写失败
  从全静默变 warnings 可见；result json/md 并发互丢窗口消失。

## 7. 测试网

1. **断言零改迁移**（等价性锚）：并发测试群（concurrent_progress_no_lost_update /
   run_job_pre_cancel / run_job_pre_done / concurrent_completion / cancel 三态 /
   reap 四测 / panic 两测）断言一条不改，fixture 从 write_state(ws,…) 换 store API。
2. **D5 回归**（§3）。
3. **复用不变式**：单 store 全生命周期方法序跑通；Arc<JobStore> 10 线程进程内并发
   lost-update（与跨进程旧测并存：旧测锚跨进程守卫、新测锚进程内 Mutex）。
4. **披露测**：JobCellSink 失败计数>0 → 收尾 warnings 含条目（单元级）；result 两线程
   各写一列 → 两列都在。
5. **背景 job 冒烟 e2e（进本批）**：现状 e2e 全 background=false、真 worker 链路零覆盖
   （T45/T47③ 旧债），而本批重构恰是 worker 共享连接。真 spawn worker + fake provider
   优先；fixture 贵则 run_job 进程内驱动兜底（至少锚「一条连接跑完整 job 生命周期」）。
   T47③ panic 注入版=可选加测（fixture 顺利则做，不阻塞）。

验收门：全量 cargo test + clippy 零新增 + make verify。

## 8. 范围闭集

本 design §2-§7 即全部范围。点名排除（decisions §C-Q6）：exit Incomplete(M18)/台账层
连接治理/db.rs DDL 拆分与 schema 迁移(M20)/JobState 字段清理/Phase 增强/job 状态机函数
搬家（M17-a 已裁原地，本批只改调用方式）。
