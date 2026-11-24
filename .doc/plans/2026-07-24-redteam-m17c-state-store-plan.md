# M17-c state 夹层合并 · 实施计划

> 状态: 已交付(2026-07-27 合 dev 06d589e) · 对应 design `../design/2026-07-24-redteam-m17c-state-store-design.md`（六裁决闭集）；决策 `../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §C；里程碑=roadmap M17-c。
> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development，逐 task 派发。

**Goal:** state 夹层收敛 JobStore（连接复用/错误分级/写收口/D5 堵洞/result 原子化），并发四件套语义原样。

**Architecture:** 先建新 API 并迁移（Task 1-2，含断言零改迁移），再改造调用方三生命周期（Task 3），再修 bug 补披露（Task 4 D5+result / Task 5 warnings 通道），背景冒烟收口（Task 6）。

**分支:** `feat/m17c-state-store`（从 dev 切出；纯本地，永不 push）。
**并行注意:** 与 M17-b（provider 层）文件面不重叠可并行；若 b 先合 dev，本分支收口前 rebase/merge dev 一次。

## Global Constraints（每 task 隐含）

- **并发四件套语义原样**（守卫 CAS/首个终态封盘/pid 身份锚/惰性收尸/panic hook）——迁移的并发测试**断言一条不改**。
- 契约冻结（design §6）：JobState serde 面、proj.db DDL、report schema、exit_code、现有 5 e2e 零改动过。
- 每 task 收尾：`cargo test` 全绿 + `cargo clippy --all-targets` 零新增 + commit。
- 注释/commit 简体中文；只 commit 绝不 push。范围闭集：design §8 排除项一行不碰。

---

### Task 1: JobStore 类型 + 方法全集（state/ 收敛单 mod.rs）

**Files:** Modify `src/state/mod.rs`（吸收 job.rs+store.rs 全部内容）；Delete `src/state/job.rs`、`src/state/store.rs`；Modify `src/db.rs`（result_put_json/result_put_md 两个单列 upsert 方法 + 单测）

**Interfaces:** design §2 的 JobStore 全签名；类型区原样搬移（JobState/JobStatus/Phase/PhaseStamp/Progress/CellInput/CellRecord，serde 属性逐字节不动）。

**Steps:**
- [ ] 1.1 TDD：先写 JobStore 新 API 测试——create_job 首建 Pending / update_if_active 应用与封盘（对照旧 update_active_applies_and_freezes_on_terminal 断言）/ 读系列 roundtrip / **复用不变式**（单 store 全生命周期方法序：create→update×N→read→append_partial×N→write_result→read_result 一条连接跑通）/ **Arc<JobStore> 10 线程进程内 lost-update**（对照旧 concurrent_progress，两测并存意图注明）/ result 单列 upsert 两线程各写一列互不丢。
- [ ] 1.2 实现：JobStore { db: Mutex<ProjDb> }；13 方法（写三形态收口 + 读/manifest/partial/args/result 平移，anyhow 直通）；`#[cfg(test)] put_state_for_test`；db.rs 两个单列 upsert（`INSERT INTO results(job_id,json,md) VALUES(?,?,'') ON CONFLICT(job_id) DO UPDATE SET json=excluded.json` 式，md 侧对称——先读现 DDL 确认列与约束名）。write_result/write_result_md 内部改走单列方法；双列 result_put 若无余生产调用方则删。
- [ ] 1.3 **旧模块测试断言零改迁移**：job.rs/store.rs 两个 tests mod 全部测试迁入新 mod.rs tests，fixture `write_state(ws,…)`→`store.put_state_for_test(…)`/`JobStore::open`，**断言行逐字保留**；迁移清单（旧名→新位置）入报告。
- [ ] 1.4 本 task 末暂留兼容自由函数薄壳（write_state/read_state/…转发 JobStore 一次性 open）让全仓编译过——**Task 3 整体删壳**（壳存在仅限本分支内两个 commit 之间，不合 dev）。
- [ ] 1.5 全绿 + clippy + commit：`refactor(state): JobStore 合并夹层——连接复用+写收口+result 单列 upsert（C-Q1/Q2/Q4）`

### Task 2: 调用方改造——一次性命令 + cli 库内消费面

**Files:** Modify `src/main.rs`（status/result/cancel 等命令入口构造 store）、`src/cli/mod.rs`+`src/cli/incremental.rs`+`src/cli/tail.rs`（state:: 自由函数调用点→store 方法；函数签名按需增 `store: &JobStore` 参）、`src/orchestrate/mod.rs`（CellSink trait 若签名涉 state 类型则只改 import 路径）

**Steps:**
- [ ] 2.1 `grep -rn "state::job::\|state::store::" src/ --include="*.rs"` 列全调用点（含测试 fixture），按文件分组改造：一次性命令=命令 match 臂顶构造一次 store；cli 库函数=签名传 &JobStore（run_job/reap_if_dead/mark_failed_panic/partial_report/write_partial_result 等——**只改取用方式，函数不搬家**，M17-a 已裁原地）。
- [ ] 2.2 错误分级同步落地（design §4）：④一次性命令 `?` 直冒（原 `let _=` 在命令路径的删掉）；③终态写必响（eprintln + 前台 Result）；②热路径暂保 `let _=`（Task 5 换披露计数，本 task 不动行为）。
- [ ] 2.3 全绿（含迁移测试）+ clippy + commit：`refactor(state): 调用方三生命周期改造——一次性命令与库函数走 JobStore（C-Q4）`

### Task 3: worker 生命周期 Arc<JobStore> 贯穿 + 删兼容壳

**Files:** Modify `src/main.rs`（Cmd::Worker 入口构造 Arc<JobStore> 穿 run_job；Cmd::Review spawn 分支同）、`src/cli/mod.rs`（JobCellSink 持 Arc<JobStore> 替 ws: PathBuf；run_job 签名）、`src/state/mod.rs`（**删 Task 1.4 的兼容壳**）

**Steps:**
- [ ] 3.1 JobCellSink { ws, job_id } → { store: Arc<JobStore>, job_id }；on_cell/on_phase/on_meta 改方法调用；run_job/worker 收尾链全走同一 Arc。
- [ ] 3.2 删兼容自由函数壳；`grep -rn "state::job::\|state::store::" src/` 仅剩类型引用（JobState/Phase/CellRecord 等）为过。
- [ ] 3.3 全绿 + clippy + commit：`refactor(state): worker 全生命周期单连接贯穿，兼容壳删除（C-Q1/Q4）`

### Task 4: D5 修复 + 回归

**Files:** Modify `src/main.rs`（Cmd::Worker deadline 裸 RMW → store.update_if_active 闭包内判）、测试落 `src/cli/mod.rs` 或 `src/state/mod.rs` tests（与既有 pre_cancel 测试同居）

**Steps:**
- [ ] 4.1 TDD：D5 回归先行——预置 Cancelled(exit=7) → 执行「worker 起手 deadline 写」等价逻辑（store.update_if_active + deadline 闭包）→ 断言 status 仍 Cancelled、deadline_at 仍 None → FAIL 於旧裸 RMW（若直测 main.rs 不可行，把 deadline 设置抽成 `pub(crate) fn stamp_deadline(store,&id,started)` 小函数测它，main.rs 调用）。
- [ ] 4.2 修：main.rs:545-551 改 `store.update_if_active(&job_id, |st| if st.deadline_at.is_none() {...})`；顺带确认 read started_at 的相邻逻辑不受影响。
- [ ] 4.3 D5 挂进 `.doc/reviews/2026-07-22-rubber-duck-walkthrough-review.md` P1 表（一行：D5 | main.rs:545 | deadline 裸 RMW 覆终态，M16 终审漏，M17-c 修）。
- [ ] 4.4 全绿 + clippy + commit：`fix(state): D5 worker deadline 裸 RMW 堵洞——首个终态封盘第三例绕过（C-Q2）`

### Task 5: 检查点失败披露通道（warnings 计数）

**Files:** Modify `src/cli/mod.rs`（JobCellSink 加 AtomicU32 计数；on_* 写失败 eprintln+计数；run_job 收尾读计数塞 warnings）

**Steps:**
- [ ] 5.1 TDD：单元级——构造写必败场景（把 store 指向已删除/只读路径的 db，或 cfg(test) 注入）驱动 on_cell 失败 → 计数>0；run_job 收尾拼 warnings 断言含「检查点落盘失败 N 次」条目。若注入不可行，至少测计数器→warnings 的拼装函数。
- [ ] 5.2 实现：②级披露落地（design §4 表）；warning 文案简体中文、含 job_id 与次数。
- [ ] 5.3 全绿 + clippy + commit：`feat(state): 检查点落盘失败进 run.warnings——R2.1 卖点可观测（C-Q3）`

### Task 6: 背景 job 冒烟 e2e

**Files:** Modify `tests/review_e2e.rs`（或新 `tests/background_e2e.rs`，以既有骨架复用度定）

**Steps:**
- [ ] 6.1 首选真 spawn：临时仓 + fake provider 配置落 config → `redteam review --background`（经二进制或 main 逻辑）→ 轮询 status 观察 Pending/Running→Done → result 取回断言 verdict。若真 spawn fixture 太贵（二进制路径/配置注入困难），兜底=进程内驱动 run_job 全链路（构造 job + manifest → run_job → 断言终态+result 落库），至少锚「一条连接跑完整 job 生命周期」。
- [ ] 6.2 可选加测（fixture 顺利才做，T47③ 半吞）：panic 注入版——fake provider 或 REDTEAM_TEST_PANIC 钩子让 worker panic → 断言 Failed(error 含 "panic")。做不动不阻塞，M18 仍挂账。
- [ ] 6.3 全绿 + clippy + commit：`test(e2e): 背景 job 冒烟——worker 单连接全生命周期端到端锚（C-Q5）`

---

## 收尾

- 若 M17-b 已合 dev：merge dev 进本分支跑一次全量再终审。
- 整分支 opus 终审（review-package MERGE_BASE=分支起点；重点=并发四件套等价性、Mutex 不跨 await、删壳完备）→ 修 findings → 合 dev（--no-ff，纯本地）。
- roadmap M17-c 置 ✅、done.md 记账、本 plan 状态头改「已交付」、D5 评审档补行核对、T45/T47③ 账目按 6.1/6.2 实际落地情况刷新。
