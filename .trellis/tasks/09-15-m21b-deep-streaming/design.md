# 批 C2 技术设计：scanner 与深挖时间重叠

> 输入：prd + plan 批 C2 节 + design v11 §2 / §6 规则 2-6 / §7 / §8.4 / §12.3 / §17 R14〔D13, D15〕+ C1 design（归档 `09-13-m21b-deep-concurrency`）。
> C1 已合回（`80919ad`）。定案：2026-09-15 `/grilling` 十问（Q1–Q10，结论见 §13；权衡与被否见 decisions D15）。
> **不在本批**：选项规则（优先级键、相邻区间合并、文件多样性与同分 tie-break、scanner samples）与 scanner needs 入队——批 C3；
> verifier 流式段、真 sink、barrier 复核在途计数——批 D。

## 1. 边界与文件

| 文件 | 动作 | 内容 |
|---|---|---|
| `src/cli/mod.rs` | 修改 | `run_code_review`：scanner 扇出与 `run_deep` 同 task `join!`（先 poll 扫描）、入队通道、收场切 `Deep`、scanner 失败不再早退；`overlay_deep` 的 F3 口径；拆簇对账；`run_resume` 离线续跑（RS1） |
| `src/orchestrate/mod.rs` | 修改 | `run_scan_fanout` 加可选入队发送端：格完成（落盘之后）即逐条发该格疑点；`EnqueueReq` 构造带到达键 |
| `src/orchestrate/deep.rs` | 修改 | 在线额度与收场 drain；到达只写受影响快照项；等门挂起（G1）取代「在途为空才阻塞 lease」；收场按优先级序组装（D2）；resume 模式幂等入队 / 复位 Skipped；新观测与阻塞计时 |
| `src/orchestrate/queue.rs` | 修改 | `arrivals` 字段与幂等入队；`enqueue` 回报受影响项；`pop_dispatch_where` / `has_eligible`；`drain_with(budget, floor)`（`drain(K)` = 其特例）；`merge_into` 前提注释改写；优先级比较对 deep 可见 |
| `src/provider/registry.rs` | 修改 | `InflightGate` 等门者计数、首试让位、等门登记守卫（取消安全）；`try_lease` 随之让位 |
| `src/consolidate/scan.rs` | 修改 | 组内闭包成员（到达键）导出，供拆簇对账；`merge_suspicions` 行为不变 |
| `src/cli/second_pass.rs` / `src/pack/region.rs` | 修改 | 读守卫查每轮预取的 git 跟踪文件集；拒绝口径（未跟踪 / denylist / 符号链接逃逸）不变 |
| `src/domain/report.rs` | 修改 | `DeepMeta` 新字段（serde default） |
| `src/main.rs` | 修改 | `--help`：深挖与 scanner 时间重叠、scanner 失败时深挖照跑（退出码 3 不变） |

不碰：`cmp_priority` / `priority_score` 语义、`same_item` / `merge_notes` 谓词、region pack 内容与笔记渲染、prompt 模板、`verify.rs`、`run_deep` 的 `sink` 参数类型。

## 2. 首跑流式接线（C2-1）

### 2.1 形态

```text
run_code_review（配了 reviewer）:
  stage0_risk → plan_cells → (tx, rx) = mpsc::channel::<EnqueueReq>(256)
  scan_fut = async move {                        // tx 移入：扫描全返回即 drop = 通道关闭
      let r = run_scan_fanout(registry, &input, sink, Some(&tx)).await;
      sink.on_phase(Deep)                        // 收场切 Deep（Q10）
      drop(tx); r
  }
  deep_fut = run_deep(registry, DeepInput { route, scan_cells: &[], incoming: Some(rx), restore: None, … }, &())
  pin scan_fut；futures::poll!(scan_fut) 一次        // 显式先 poll 扫描：t=0 scanner 先占门或登记（G1，§7.3）
  (results, deep) = tokio::join!(scan_fut, deep_fut)
  report = finish_scan_report(results)（scan 报告口径不变：疑点并集仍按组 merge_suspicions）
  deep.meta.queue_split_clusters = split_clusters(&results.cells, &deep.items)   // §4
  overlay_deep(report, scan_code, deep, …)             // F3 口径见 §5
没配 reviewer：只跑 scan_fut（无通道），scan-only 路径与现状同。
```

- 发送：scan 格 future 在 `sink.on_cell(record)` 之后，对锚定后输出里的每条疑点 `tx.send(EnqueueReq::scan(s, family, arrival)).await`；
  发送失败（接收端已 drop，如 `run_deep` 因变更集为空早退）忽略。交付粒度仍是逐 cell（D13 Q-B）：一格的疑点在该格完成时一起到达。
- 无死锁：深挖主循环的填门段是同步代码、没有 await；唯一的等待点是 `select!`，其中通道分支在 `!closed` 时恒启用——通道满时扫描侧
  `send` 挂起，深挖侧下一次进 `select!` 即收，容量只影响批次，不会互等。

### 2.2 到达键

- `EnqueueReq.arrival: Option<String>` = `{scan cell_id}#{i}`（i = 疑点在该格锚定后 `suspicions` 里的下标；落盘输出同序，resume 可复算）。H0 无到达键。
- `QueueItem.arrivals: Vec<String>`（`#[serde(default, skip_serializing_if = "Vec::is_empty")]`）：新建项记自身到达键，`merge_into` 追加请求的到达键。
- 幂等：`enqueue` 先查到达键是否已在某项 `arrivals` 中——在则直接返回 `Duplicate`（不改项、不计 `dedup_hits`）。用途：§4 拆簇对账、§6 resume 幂等入队。
- 实施（C2-1）：`DeepQueue::enqueue_affected(req) -> (EnqueueOutcome, Option<usize>)` 回报受影响项下标（`enqueue` 只取 outcome）；
  到达键构造统一为 `consolidate::scan::arrival_key(cell_id, i)`。

### 2.3 phase

`Scan`（现状：manifest 时打）→ 收场（`scan_fut` 末尾）打 `Deep` → 收口打 `Finalizing`。t=0 起挖由 `first_dispatch_ms` 体现（§10），不另设阶段。

## 3. 在线预算（C2-2，Q1 B）

### 3.1 扫描期额度

```text
K = budget_cells；F = if K == 0 { 0 } else { ⌈K/4⌉ }
dispatched          = 本调用已派发项数（含派发时 pack 构造失败、就地结案的项——与 C1「占 keep 位」同口径）
must(it)            = kind == Primary ∨ agreement ≥ 2
eligible(it)        = must(it) ∨ closed ∨ dispatched < K.saturating_sub(F)
填门：只在 q.has_eligible(eligible) 时取门位；取到后 q.pop_dispatch_where(eligible)——满足条件的最高优先 Pending（优先级序不变）
```

- 优先级序沿用 `cmp_priority`（种类 → 分数 → id）：额度耗尽时涟漪 / 非必挖疑点不可派，必挖疑点照派。
- 不在无可派项时取门位——避免持门空转（见 §7.2）。

### 3.2 收场 drain

```text
关闭时刻 = 通道收到 None（首跑）；incoming 为 None（测试 / resume）时开跑即关闭
on_close（只执行一次）:
  nonmust_susp = 本调用已派发、kind == Suspicion 且 dispatch_agreement < 2 的项数
  remaining    = K.saturating_sub(dispatched)
  floor        = F.saturating_sub(nonmust_susp)
  skipped_ids  = q.drain_with(remaining, floor)
      // 与 C1 drain 同形：keep = Pending 必挖 ∪ 前 floor 条非必挖疑点 ∪ 按优先级补到 kept ≥ remaining；余者 Skipped
  meta.budget_skipped += skipped_ids.len()；meta.close_budget_remaining = remaining；meta.close_ms；meta.scan_phase_dispatched = dispatched
  逐项写快照（只写 skipped_ids）
```

- `drain(max_cells)` 改为 `drain_with(max_cells, F)` 的特例：C1 路径（开跑即关闭、dispatched = 0）结果逐项不变。
- 「主区吃穿时帽 = 主区 + 保底」自然成立：主区占满 K → remaining 0，floor = F → 仍留 F 条疑点。
- Skipped 只在此刻产生；首跑关闭后无到达，`merge_into` 的 Skipped 复位分支首跑不可达（保留，注释写明；resume 复位走 §6 显式路径）。

## 4. 合并残差对账（Q2 M1）

- 到达照走入口五态，谓词不变（`same_item` + `merge_notes`）。`merge_into` 注释的前提改写为：逐 cell 到达时队列侧逐条比对已有项笔记，
  不是组内传递闭包；非传递漏合并只少算共识、多复核（D15 实测 ≤ 1/19 簇）；id 捷径仍不计共识。
- `consolidate::scan::closure_members(cells) -> Vec<Vec<String>>`：与 `merge_suspicions` 同一并查集（按组、`same_suspicion`），输出每簇成员到达键；
  `merge_suspicions` 改为在其上组装、行为与输出逐字节不变。实施（C2-1）：两者共用私有 `suspicion_clusters(cells)`。
- `split_clusters(cells, items)`：成员到达键映射到所在项 id，一簇成员分落 ≥ 2 项即计 1。`DeepRun` 带回最终队列项（`items`）供 cli 调用。

## 5. scanner 失败口径（Q5 F3）

- `run_code_review` 删除 `scan_failed` 早退（保留「无 reviewer」早退）：scanner 格失败时深挖照跑，失败格的疑点永不到达，关闭时刻 = 全部格返回。
- `overlay_deep`：scan 收口已为 Incomplete（`scan_code == 3`）时——findings / stats / deep 观测 / reviewer 格照常并入；`report.verdict` 保持 `Incomplete`、
  退出码 3（优先于 reject 10 与带债 2）；`compute_verdict` 为 Reject 时追加 warning
  `scan incomplete: N blocking finding(s) present — a complete review would reject`；带债 warning 去掉「approve with exit 2」字样。
- 「深挖根本没发生」的早退分支（0 格、无债、无 finding）不变。
- `--help` 退出码 3 文案补一句：深挖照跑，findings 仍在报告（blocking 条数见 run.warnings）。

## 6. resume 离线续跑（Q6 RS1）

```text
run_resume:
  补跑缺失 scanner 格（现状）；replayed = 本次补跑前有缺失格（不论补跑成败）
  if 配了 reviewer（去掉 `still_missing == 0` 与 `!queue_snap.is_empty()` 两个前提）:
     diff 重建失败 → 现状分支（已挖 finding 进报告、未完计债）
     C1 对账（Settled 以成功格为准）
     route = queue_snap 为空 ? stage0_risk(当前 diff).route : None
     run_deep(DeepInput {
         restore: Some(&queue_snap)（空快照 = 新队列），route,
         scan_cells: 全部成功 scanner 记录,         // 按到达键幂等入队（§2.2）；旧快照无到达键 → 谓词合并兜底，可能多出少量项
         reopen_skipped: replayed,                 // 复位 Skipped → Pending（enqueued_ms 重置）
         incoming: None,                           // 开跑即关闭 → §3.2 收场 drain
         …})
     restore 模式下 dispatched 初值 = Settled 项数；nonmust_susp 初值 = Settled、Suspicion、dispatch_agreement < 2 的项数
  verdict：scanner 仍缺失 → Incomplete（F3 同口径，findings 照进）
  budget_cells：原报告 risk 值；无原报告 → 重算 route 的 hint；再无 → 4（现状兜底）
```

- 修掉 C1 现存缺口：扫描期被 kill（快照为空）且配了 reviewer 的作业，resume 不再出 scan-only / 退出码 0，而是重建 H0 + 疑点入队后深挖。
- 首跑已在信息完整时收场、kill 在深挖期：不补跑 scanner → 不复位 Skipped；drain 以剩余预算重跑。首跑留下的非必挖项数 ≤ K − 已结案数，
  预期原样保留、不新增 Skipped——以测试钉住（resume ①④ 用例断言 Pending 集合不缩）。

## 7. 门公平与唤醒（Q7 G1）

### 7.1 `InflightGate`

```rust
struct GateState { cap: u32, held: u32, active: u32, rate_limited: u32, completions: u64, waiters: u32 }

fn try_enter(&self) -> Option<GatePermit<'_>>          // 首试：held < cap ∧ waiters == 0（有人排队就让位）
async fn enter(&self) -> GatePermit<'_> {
    if let Some(p) = self.try_enter() { return p; }
    let _w = WaiterGuard::register(self);                // waiters += 1；Drop 时 −1（取消安全）
    loop {
        let notified = self.slot_free.notified(); tokio::pin!(notified); notified.as_mut().enable();
        if let Some(p) = self.try_enter_as_waiter() { return p; }   // held < cap：已登记者不让自己
        notified.await;
    }
}
```

- 等门者之间按 `Notify::notify_one` 的先来后到唤醒；放门逻辑（`GatePermit::drop`）不变。
- 取消安全：等门者被唤醒后未取到就被 drop 时，唤醒必须转交下一位——实现时以测试核实 tokio `Notified` 的转交行为，不成立则由
  `WaiterGuard::drop` 在 `held < cap` 时补一次 `notify_one`。

**实施细化（C2-1，2026-09-17）——票号 FIFO**：上面「`notify_one` 唤醒 + 已登记者 `held < cap` 即取」有插队窗口：放门后唤醒已发给队头、
队头尚未被 poll 时，新登记的等门者（如深挖等门挂起刚建的阻塞 lease）登记即检、`held < cap` 成立就把门位拿走，队头醒来取不到、重排队尾——
扫描与深挖同 task `join!`，谁先被 poll 不是契约，§11「放门不插队」会不稳定。改为：
- `GateState.waiting: VecDeque<票号>`（等门者计数 = 其长度）；`WaiterGuard::register` 发票号入队尾。
- `try_enter`：`held < cap ∧ waiting 空`；`try_enter_as_waiter(票号)`：`held < cap ∧ 票号是队头` → 出队取门位。
- 放门 `GatePermit::drop`：有空位且有人排队 → `notify_waiters`（全员醒、只有队头能取）。`notify_one` 不够：非队头者被唤醒后重新登记，
  Notify 内部唤醒次序会与票号次序漂移，只叫醒一个可能叫醒非队头者、真队头永远等不到。
- 连锁：队头取后若仍有空位且还有人排队，再 `notify_waiters`（同一刻放出多个名额时后面的人不必等下一次放门）。
- 取消：`WaiterGuard::drop` 把票号移出队列；它若是队头且有空位，`notify_waiters`（其余等门者可能已被唤醒过、重检后又睡下，
  放门那次唤醒已被消费；tokio 的转交只对 `notify_one` 的未收通知生效，对 `notify_waiters` 不转交）。
- 用例以逐 future 独立 waker 标记断言「谁被唤醒」；「放门改回 `notify_one` / 不连锁 / 取消不唤醒 / 不查队头 / `try_enter` 不看排队 /
  守卫不出队」六处变异各自转红。
- 429 等待者持名额、走 `completed` 通知，不计入 `waiters`，行为不变。
- scanner-only 路径没有非阻塞调用者，`enter` 的首试让位只影响「门上已有排队者时新来的 scanner」——即 FIFO 本义。

### 7.2 深挖主循环

```text
loop {
  ① 填门：while pool 非空 ∧ q.has_eligible():
        lease = parked.take() 或 pool.next_lease()（非阻塞、让位口径）；None → break
        pop_dispatch_where → 建 pack / prompt → running.push(run_cell)
  ② 收场 / barrier：通道关闭且未 drain → §3.2；在途 0 ∧ 可派 0 ∧ closed → break（池空收场同 C1）
  ③ 等门挂起：q.has_eligible() ∧ 本轮未取到门位 ∧ pool 非空 ∧ waits 未建 → 为各活跃成员建阻塞 lease（进 FIFO）
     waits 跨事件保持（通道到达、格完成都不重建，不丢排队位置）；可派项消失（额度耗尽且无必挖）→ 撤销 waits
  ④ select! { running.next() | recv (!closed) | waits.next() (waits 存在) }
     waits 命中 → parked = Some((lease, member))，其余 waits 撤销；无可派项时立即归还（不持门空转）
}
```

- 取代 C1「在途为空才阻塞 lease」的特例；#6「外部持门者放门不唤醒」随之解决：放门 → FIFO 唤醒深挖的等门 future → 主循环醒来补位。
- 同 provider 多成员共享一门时 waits 同门多登记，先中者用、其余撤销；成员出局（`try_lease` / `lease` 报错）沿用 C1 就地出局口径。

### 7.3 t=0 先后

不依赖 `tokio::join!` 的分支轮询顺序（宏内有轮转公平，首轮顺序不作契约）：`join!` 之前显式 `futures::poll!` 扫描 future 一次——
扫描侧 `join_all` 逐格首 poll，各格 `enter()` 先占门或登记为等门者；此后深挖首次填门，遇等门者即让位。测试钉住。

## 8. 报告组装序（Q8 D2）

- 收场时 `landed` 不再按派发序 `seq` 迭代，而按最终队列项优先级排序：由 cell 名反解项 id（`review_cell_prefix` 同源），取 `q` 中该项的最终状态比 `cmp_priority`
  （种类 → 分数 → id，全序；同一调用同项至多一格）。findings（→ dedup）、cells、meta 四数组、每格 warnings 依此序喂入；`prior_findings` 仍追加在后。
- 保证注释改写：「同一选中集合 + 同样的格输出 → 报告逐字节一致（`duration_ms` / `queue_wait_ms` / `item_wait_ms` 等计时字段除外）」；
  选中集合随到达时序变化是 B / M1 的已知代价（C3）。
- 退化零差：单成员门宽 1、`incoming: None` 时派发序 = 优先级序，基线文件不变。

## 9. 同步 IO（Q9 IO2）

- 快照：`enqueue` 回报受影响项（新建 / 并入目标；`Duplicate` 不写），`on_arrival` 只写该项；H0 入队逐项写；`drain_with` 回报 Skipped 项；派发 / 结案沿用单项写。
- 读守卫：`TrackedFiles::load(root)`（一次 `git ls-files -z`）在 `run_deep` 开跑时建，`need_read_allowed_in(&tracked, root, rel)` 取代逐项子进程；
  `region::build` 与二阶段 `need_section` 接收该集合。denylist 与符号链接逃逸检查不变；原函数保留给其他调用点。
  实施（C2-2）：
  - 取集合 = `git rev-parse --show-prefix` + `git ls-files -z --full-name -- :/`（整仓跟踪文件；去掉外部 `GIT_LITERAL_PATHSPECS`，否则 `:/` 被当字面路径、列表为空）；
    非 git 仓 → 全拒「非 git-tracked」，git 不可用 → 全拒「git 不可用（fail-closed）」，文案与原实现相同。
  - 判定 `need_read_allowed_in(&tracked, rel)`（`root` 已在取集合时用掉）：与 git 字面 pathspec 逐条同口径——空串 / 绝对路径拒；相对 root 前缀解析
    `.` `..` 重复斜杠（`..` 越出顶层拒）；命中文件本身或含跟踪文件的目录放行；以 `/` `/.` `/..` 结尾只按目录匹配；大小写敏感。
    rel 不再作为 git 参数，参数 / pathspec 注入无从下手。
  - 原 `need_read_allowed(root, rel)` 改为「现取集合 + 集合判定」的便捷入口，唯一实现；既有注入 / fail-closed 用例原样验证新口径。
  - `need_section(ex, tracked, need, warnings)`；`region::build(item, bundle, root, tracked, idx, budget, opts, needs_evidence)`。
  - 等价性以原子进程实现为对照：33 个路径 × 仓库顶层 / 子目录 / 非 git 目录三种 root 逐条同结果、同拒绝文案（首轮抓到文件尾斜杠一处口径差后修）。
- 阻塞计时：填门段（`region::build` + prompt 构建 + 快照写）与到达处理段各自以 `std::time::Instant` 计时，记 `fill_block_max_ms` / `fill_block_total_ms`。
- 挪 `spawn_blocking` 不在本批；触发条件记 followup（PR181 出口验收单次 ≥ 500 ms 或累计 ≥ 墙钟 1%）。

## 10. `DeepMeta` 新字段（均 serde default）

| 字段 | 含义 |
|---|---|
| `close_ms` | 收场时刻（自 `run_deep` 开跑，毫秒；resume 恒 0） |
| `first_dispatch_ms` | 首次派发时刻（无派发 0） |
| `drain_done_ms` | 最后一格落地时刻 |
| `verify_tail_ms` | 复核尾部（批 D 前恒 0） |
| `scan_phase_dispatched` | 收场前派出的格数 |
| `close_budget_remaining` | 收场时剩余预算（`K − dispatched`，可为 0） |
| `queue_split_clusters` | 组内闭包簇被队列拆开的簇数（§4） |
| `fill_block_max_ms` / `fill_block_total_ms` | 主循环同步段单次最长 / 累计阻塞 |

`run_deep` 与扫描同时开跑，`close_ms` 即 scanner 段墙钟；`drain_done_ms` 即深挖排空；`verify_tail_ms` 批 D 填。

## 11. 测试形状

虚拟时间：`#[tokio::test(start_paused = true)]` + Fake `with_delay`；共门构造 = scanner 条目与 reviewer 条目路由到同一 provider。

| 用例 | 构造 | 断言 |
|---|---|---|
| H0 t=0 开挖 | scanner 格时延 1000、H0 格 100 | H0 格在 scanner 返回前完成；`first_dispatch_ms` ≈ 0；通道关闭前不收场 |
| 扫描期额度 | 宽门、K = 8（F = 2）、首格 10 条非必挖疑点先到 | 收场前派出 ≤ 6；关闭后剩余按 drain 补齐；`scan_phase_dispatched` 与 `close_budget_remaining` 对得上 |
| 必挖不截 | `maxCells = 0`；另一格后到把某项抬到 agreement 2 | H0 与后到必挖项都被挖；非必挖全 Skipped |
| Skipped 只在收场 | 构造「额度耗尽后到达」 | 收场前无 Skipped；`budget_skipped` = 收场时 Skipped 数；无复位回减 |
| 拆簇对账 | 两格三条疑点 A~B、B~C、A≁C，按 A、C、B 顺序到达 | `queue_split_clusters == 1`；scan 报告疑点并集与改动前一致 |
| scanner 失败 | 一格 scanner 失败、reviewer 已产出 blocking finding | 深挖跑完；verdict Incomplete、exit 3；findings 在报告；warning 报 blocking 数 |
| resume ① 扫描期 kill | 快照含 H0 在途、部分 scanner 格落盘 | 补跑缺失格 → 疑点幂等入队 → drain → 挖；已落盘 finding 不丢不双计 |
| resume ② F3 缺口 | 首跑带 scanner 失败收场（有 Skipped） | 补跑后 Skipped 复位参与重选 |
| resume ③ 空快照 | 配 reviewer、快照为空、scanner 部分落盘（先写红用例） | 深挖发生，不再 scan-only / exit 0 |
| resume ④ 多项在途 kill | 并发多项 InFlight、部分格已落盘 | finding 不丢不双计（followup 用例缺口） |
| 共门 t=0 先后 | 共门 cap 1：1 个 scanner 格 + 1 个 H0 | scanner 先入门 |
| 放门不插队 | 共门 cap 1：scanner 等门时深挖格完成放门 | 下一门位归 scanner |
| 外部放门唤醒（#6） | 门 2、外部先占 1、4 项 × 100 ms | 及时补位（≈ 210 ms，不再 300 ms） |
| 取消安全 | 等门者被唤醒后未取到即 drop | 下一位仍被唤醒；`waiters` 归零 |
| 报告确定性 | 选中集合固定、打乱各格完成时延两次 | 报告 JSON 逐字节一致（计时字段除外）；meta 四数组等长对齐 |
| 快照只写受影响项 | 计数 sink 记录写入次数 | 每次到达写 ≤ 1 项；幂等重复到达写 0 项 |
| 读守卫等价 | 未跟踪 / denylist / 符号链接逃逸路径 | 与改动前同样拒绝；`fill_block_*` 可读 |
| 退化零差 | 单成员门宽 1、`incoming: None` | 与 `deep_serial_baseline.json` 同构 |

## 12. 兼容与回滚

- schema v4 不变；`queue_items` JSON 增 `arrivals`、`DeepMeta` 增字段，均 serde default（旧快照 / 旧报告可读）。
- 行为变化（有意）：scanner 失败的首跑与 resume 报告多出 findings（仍 exit 3）；扫描期被 kill 的作业 resume 会深挖（原 scan-only exit 0）；
  门首试在有等门者时让位；报告组装序由派发序改优先级序（单成员门宽 1 时二者相同）。
- 退化零差基线 `src/orchestrate/testdata/deep_serial_baseline.json`：预期不变；若有 diff，审过再更新（C1 §8 口径）。
- 回滚 = 不合回；合回后 revert 该 merge。

## 13. 定案记录（2026-09-15）

| # | 问题 | 决定 |
|---|---|---|
| Q1 | 在线预算 | B：扫描期非必挖受 K − F 额度，必挖不限；关闭入口时剩余预算 drain 一次；Skipped 只在收场判定 |
| Q2 | 到达与合并 | M1：逐 cell 到达、队列逐条合并；收场闭包对账，拆簇数入 RunMeta |
| Q3 | 方差四层 | S3：另立批 C3（C2 后、D 前）；C2 不改选项规则 |
| Q4 | scanner needs 入队 | N2：归 C3 |
| Q5 | scanner 失败 | F3：深挖照跑；硬 Incomplete / 3；findings 进报告，warning 报 blocking 数 |
| Q6 | resume | RS1：离线续跑；补跑过 scanner 则复位 Skipped 重选；空快照同路径（修 C1 缺口） |
| Q7 | 共门先后 | G1：FIFO 让位 + 深挖排入 FIFO（兼修 #6）；t=0 poll 序；通用于 verifier |
| Q8 | 报告组装序 | D2：收场按最终优先级序；保证改写 |
| Q9 | 同步 IO | IO2：快照只写受影响项 + 跟踪文件集预取 + 阻塞计时；`spawn_blocking` 待数据 |
| Q10 | 验收 | §11 测试 + §10 观测；dogfood 一轮同目标 diff、配置与基线一致（agc 门 6）；phase 收场前 Scan、后 Deep |
| — | 配置 | agc `maxConcurrency` 保持 6 |
| — | 批序 | C2 → C3 → D → 出口验收 |
