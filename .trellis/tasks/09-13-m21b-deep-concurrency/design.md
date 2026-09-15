# 批 C1 技术设计：门驱动并发派发与 reviewer 分派池轮转

> 输入：prd + plan 批 C1 节 + design v10 §2 / §6 规则 2-5〔D13〕/ §7 / §8.1。批 C 已合回（`eac0a60`）。
> 定案：2026-09-13 两轮裁定 + `/grilling` 十问（Q1–Q10，结论见 §9）。队列优先级 / 预算 / 必挖 / 保底规则、pack、prompt、
> `DeepMeta` 字段集**不重写**——本文只钉身份、派发形态、成员健康与二阶段契约。

## 1. 边界与文件

| 文件 | 动作 | 内容 |
|---|---|---|
| `src/orchestrate/queue.rs` | 修改 | id 定死；共识只经谓词；Skipped 复位 + `EnqueueOutcome::MergedSkipped` |
| `src/provider/registry.rs` | 修改 | `InflightGate::try_enter` + `ProviderRegistry::try_lease`（只加不改） |
| `src/orchestrate/deep.rs` | 修改 | `FuturesUnordered` 主循环 + 入队通道 `select!`；`run_cell` future（首轮 + 二阶段，持门位）；派发序组装；分派池轮转与踢出；cell_id |
| `src/cli/mod.rs` | 修改 | resume「Settled 以 cells 为准」；`RunMeta.models` 含实际参与 reviewer；0 格成员 warning |
| `src/main.rs` | 修改 | 兼任 scanner 的 reviewer 补发 thinkingLevel 无映射 warning |
| `Cargo.toml` | 修改 | dev-dependency `tokio` 开 `test-util`（虚拟时间测试） |

不碰：`review_shot.rs` 两轮单发与锚定门、`region::build`、prompt 模板、`DeepMeta` 字段集、scanner 扇出路径、`verify.rs`（批 D）。
与批 D 唯一碰撞点：`run_deep` 的 `sink` 参数（C1 不改其类型）。

## 2. 身份与 resume 完整性（C1-1）

### 2.1 队列项 id 定死

`QueueItem::new` 按首次 spec 算 `item_id`（形态不变 `file:origin:ranges`）；`merge_into` **删除** `it.id = item_id(&it.spec)`。
`restore` 原样读回存储的 id、resume 路径不新入队，所以任何 id 形态都兼容——保持现形态只为可读，不是迁移约束。

### 2.2 共识只经谓词（Q2）

`find_merge_idx` 仍按 `same_item ∨ it.id == incoming_id` 找合并目标——保住「一个区间只挖一次」与 id 唯一性。
**谓词命中优先**：先在全队列找 `same_item` 命中项，找不到才退 id 捷径（按位置取第一个命中会把与 B 同疑点、恰与 A 区间逐字相同的入队并进 A，B 的真共识落空）。区分合并原因：

```text
merge_into(idx, req, by_predicate: bool):
  区间 union、笔记 merge_notes、score 取大          // 两种合并都做
  if by_predicate:                                  // same_item 成立
      并入 req.families（去重）→ agreement = max(agreement, agreement_of(families))
  else:                                             // id 捷径合并
      不并家族；agreement = max(agreement, agreement_of(req.families))   // 保请求自身共识
```

**评审修（2026-09-14）**：id 捷径合并原定「agreement 不动」，会把上游已合并成多家族共识的入队疑点（必挖）降成 1、掉出必挖集被预算截掉——
agreement 同时决定必挖集与排序分，不只是批 D 免复核的判据。改为：家族仍不跨疑点并入，agreement 取「已有值」与「请求自身家族数」之大。
两条路径都取大 → agreement 单调不减（§3.5 `event ≤ landed` 的前提由构造保证）。

理由：上游 `merge_suspicions` 已按 `same_suspicion` 组内传递闭包，进队列的同文件 scan 项必然互不满足谓词，
id 捷径合并的全是「上游判定为不同疑点、恰好行区间逐字相同」的两条——可能是 quote 噪声下的同一问题，也可能是同一行上的两个问题。
计共识的错误方向是批 D 免复核错放；不计的错误方向是多复核一次——取后者。quote 噪声漏判若在数据里显出，修上游归一化。

### 2.3 Skipped 并入复位

```text
merge_into 命中 Skipped:
  （按 2.2 合并）
  if is_must(it):      state = Pending; enqueued_ms = now; → MergedPending
  else:                保持 Skipped;                        → MergedSkipped
```

`meta.budget_skipped` 不回减（drain 那一刻的披露口径）。`note_enq`：`MergedSkipped` 计入 `dedup_hits`。

### 2.4 reviewer cell_id 与「Settled 以 cells 为准」（Q3）

- cell_id = `review[{item.id}]@{rev.name}`，例 `review[src/deep.rs:rule:120-180]@agc/glm-5.3`。id 已定死在首次 spec，长度有界。
  同项重跑 → `cell_put` upsert 覆盖自身（与 scanner「重试者胜」同语义）。
- `run_resume` 在 `restore` 之前：对快照里 `state == Settled` 的项，若 `final_records` 中不存在 cell 名以 `review[{item.id}]@` 开头的**成功**记录 → 改为 Pending 重挖，并 warning 报数。
  只有失败格也重挖（实施时细化，与 scanner resume「失败格重跑」同口径）：`overlay_deep` 的债只看本轮 `reviewer_failed`，不重挖则首跑失败格的债在 resume 报告里被抹成干净通过。
- **评审修（2026-09-14）——对账双向以成功格为准**：有成功格的项一律按 Settled（快照是 InFlight / Pending / Skipped 都算，kill 落在
  「格已落盘、快照未写」之间或快照写失败只 warning 时出现），不重挖；对账改过的项写回快照。原接受的残余（kill 窗口内重挖、换模型留两条格）随之消除；
  更要紧的是旧口径重挖失败时同名 upsert 会把成功格覆盖成失败格，finding 失去载体，下一次 resume 永久丢失。
- **diff 重建不了时截断数不丢（评审修 #3）**：resume 的正常续跑、变更集为空早退、diff 重建报错三条路径的深挖 meta 统一从快照起算
  （`deep::restored_meta`：`budget_skipped` = 快照 Skipped 数、`queue_depth_peak` = 快照项数）。原实现后两条是空 meta，往轮沿用格又让报告走正常收口
  → 首跑被截掉的项既不计债也不披露，报告 Approve / exit 0。
- **重挖失败不抹掉往轮载体**：resume 的 `on_cell` 落盘时，若新格失败且无 finding、而同名往轮格带 finding（如二阶段失败保留的首轮结果），
  失败格照写（债与重挖资格不变）但带上往轮 finding。

## 3. 并发派发（C1-2）

### 3.1 门位非阻塞试取

```rust
impl InflightGate {
    fn try_enter(&self) -> Option<GatePermit<'_>> {
        let mut s = self.lock();
        (s.held < s.cap).then(|| { s.held += 1; GatePermit { gate: self } })
    }
}
impl ProviderRegistry {
    pub fn try_lease(&self, route_key: &str) -> Result<Option<ScanLease<'_>>, ProviderError>;
}
```

`s.cap` 是 429 收敛后的有效门宽；`GatePermit` Drop 语义不变。`try_enter` **不感知等待者**——C1 内部没有阻塞等门者（二阶段持门位，见 3.3），
所以不产生饿死；共享门时 scanner 先入门的公平性归 C2。

### 3.2 主循环

```text
running  = FuturesUnordered::new()
closed   = input.incoming.is_none()
seq      = 0                                   // 派发序
loop {
  // ① 填门位（池非空）
  while q.pending_count() > 0 && !pool.is_empty() {
      match pool.next_lease(registry)? {        // 游标轮转，§4
          Some((lease, rev)) => {
              item = q.pop_dispatch(); snap_one(item); seq += 1
              构造 pack / prompt（同步）→ running.push(run_cell(seq, lease, rev, item, prompt, …))
          }
          None => break,                         // 门全满
      }
  }
  // ② barrier
  if running.is_empty() && q.pending_count() == 0 && closed { break }
  if running.is_empty() && closed && (pool.is_empty() || 兜底阻塞 lease 失败) { break }  // 池空收场，剩余 Pending
  // ③ 等事件
  select! {
      Some(done) = running.next(), if !running.is_empty() => on_done(done),
      msg = rx.recv(), if !closed => match msg {
          Some(req) => { note_enq(q.enqueue(req)); snap_all; peak = max(peak, q.items().len()) }
          None      => closed = true,
      },
  }
}
收场：按 seq 升序组装报告（§3.4）
```

- `running` 空、Pending > 0、池非空时，各门 `held = 0 < cap`，`try_lease` 必成功；为防理论不可达的空转，走一次阻塞 `lease` 兜底。
- 池空（§4.2）：不再派发；通道未关则 `select!` 只剩到达分支，继续收项直到关闭；收场时剩余 Pending 保持 Pending + warning 报数 + 计债。
- **C1 里经通道到达的项不过预算**（在线预算、疑点保底预留归 C2）；生产传 `None`，不暴露。

### 3.3 `run_cell` future（首轮 + 二阶段，持门位）

future 内：per-cell backend（`backend_factory`）→ `run_review_shot_with` → **不释放 lease** → 若 needs 合格：建第二轮 pack、
**为第二轮新造 per-cell backend + inc**、复用同一 lease 的 per_turn / retry_budget / **同一 policy** → 两轮结束读 `policy.retries()/waited_ms()` → drop lease →
返回 `CellDone { seq, item, cell_id, t0, rev, outcome, round2, rl_retries, rl_wait_ms, warnings }`。

future 外（`on_done`，完成时刻）：`landed = live_agreement` → 回填落盘 finding 的 `suspicion_agreement` → `snap_cell` → `mark_settled(id, landed)` → `snap_one`；
结果按 `seq` 暂存待收场组装。`meta` / `cells` / `findings` / `warnings` 只在主循环写。

`&DeepInput`、`&CrossIndex`、`&PackOpts`、`&ProviderRegistry`、`&dyn FindingSink` 活过 `run_deep`，被多个 future 以共享引用借用；同 task 并发不要求 `Send` / `'static`。

### 3.4 报告按派发序组装（Q6）

收场时按 `seq` 升序：findings 依次 extend（再 extend prior）→ `dedup`；cells 依次 push；meta 四数组依次 push：

| 数组 | 成功格 | 失败格 |
|---|---|---|
| `item_wait_ms` | 派发时刻 − `enqueued_ms` | 同 |
| `dispatch_agreement` | `item.dispatch_agreement` | 同 |
| `landed_agreement` | 完成时 `live_agreement` | 失败时 `live_agreement` |
| `reviewer_output_tokens` | 两轮合计 output | 烧掉的 output |

C1 无生产在途入队 → 派发序 = 静态优先级序 → 同一输入报告逐字节一致；单 reviewer + 门宽 1 时派发序 = 完成序，与批 C 同构。
「逐字节一致」不含计时字段（`duration_ms` / `queue_wait_ms` / `item_wait_ms` 本就随墙钟变）。每格 warnings 也按派发序进报告；「无 per-cell backend」按 reviewer 去重、排在各格 warnings 之前。
派发时 pack 构造失败的项不成格：warning + 结案，不占派发序、不进 meta 四数组（沿用批 C 口径）。

### 3.5 事件侧疑点共识滞后（裁定：接受）

`EmitCtx.suspicion_agreement` 为派发快照（future 内读不到队列）；落盘由 `on_done` 按落地值回填。`families` 只 push 不 remove →
agreement 单调不减 → **`event ≤ landed`**。批 D「≥ 2 免复核」读较小的事件值，偏差只会多复核、不会漏复核。`FindingEvent` 字段注释写明。

## 4. 分派池（C1-3）

### 4.1 轮转

`pool` = `reviewers` 中未被踢出的成员；`cursor` 记上次成功派发者。`next_lease` 从 `cursor + 1` 起逐个 `try_lease`，第一个拿到的用，`cursor` 置为它；全 `None` → 门全满。
同 provider 成员共享门宽，轮转只决定空位归属；跨 provider 各门独立，一门满另一门接。每项只交一个成员（不双审）。纯空闲轮转，无权重（design「无 priority、顺序无语义」）。
游标初值指向末位（首派落 0 号成员，单成员与批 C 同构）；只有真成格才移游标、计格数（派发时 pack 失败不算）。

### 4.2 成员健康（Q7）

- `on_done` 见首轮或二阶段返回的错误**穿透 `attach_usage` 包装**为 `ProviderError::Auth` → 将该成员移出 `pool`，warning `"deep: reviewer {name} auth failed — removed from pool"`。
- 已失败的那一格保持失败（coverage failed + 计债），**不改派**——不是候补（D7 A3）。
- 已在途于该成员的其他格照常跑完（多半也 401 失败），不取消。
- 超时 / `Server` / `RateLimited` / `Network` / `Unavailable` / `BadRequest` / `Parse` 不踢。
- 成员未注册（`try_lease` 返回 `Err`）→ 就地出局 + warning（实施补充）：不挡其余成员、本身不计债；池因此变空且剩余 Pending → 按池空计债。
- 限制（注释写明）：只在「队列项数 > 并发容量」时起作用；小队列 t=0 全部派出，首个 401 回来前坏成员已领走它那份。

### 4.3 观测

`coverage.selected[Reviewer]` 按实际 cells；`RunMeta.models` 补齐实际参与的 reviewer（本轮格 ∪ resume 沿用格，`model_ids` 同序追加）；配了但 0 格的成员 → warning `"deep: reviewer {name} configured but received 0 cells"`。
0 格披露只在本轮至少派出一格时给（空队列 / resume 全 Settled 不报）；已出局成员由出局原因披露、不重复报。
池观测 warnings 按成员序收场给（不随完成序）：出局原因 → 池空剩余数 → 0 格成员 → 无 per-cell backend → 各格 warnings（派发序）。

## 5. 二阶段补查契约（Q4 / Q5）

| 情况 | triggered / skipped | 结局 | 债 |
|---|---|---|---|
| 有 needs 但不合格（tools off / 带笔记互斥） | skipped +1 | 首轮结果 | 否 |
| 合格，但全部 needs 被读守卫拒 / 不可读 → 无证据，第二轮不发 | skipped +1 | 首轮结果；拒绝原因经 warnings | 否 |
| 第二轮建 pack 内部错误 | triggered +1 | **保留首轮 finding** + warning；格按失败落盘 | **是** |
| 第二轮发出，HTTP / parse 失败 | triggered +1 | **保留首轮 finding** + warning；第二轮烧掉的 token 入账；格按失败落盘 | **是** |
| 第二轮成功 | triggered +1 | 第二轮输出为终判（CONFIRM / REVISE） | 否 |

债经 `DeepRun` 的债标志进 `overlay_deep`（与 reviewer cell 失败同路：Approve + exit 2）。
**评审修（2026-09-14）**：二阶段失败的格落成 `ok = false`、`output` 仍携带首轮 finding、warn 写失败原因——进 `coverage.failed`，
resume 按「Settled 以成功格为准」重挖。原实现落成 ok 形态，债只活在本次调用的 `reviewer_failed` 里，resume 报告会把它抹成干净通过。
第二轮发出后失败：`iterations` 记下限 +1（失败轮内是否走过 REPAIR 不可观测），`attempts` 随之计入。二阶段成功：cell 的解析 / 锚定丢弃计数取终判轮
（首轮草稿被丢的不算这一格的产出丢失），旁路丢弃两轮累加。

**reviewer cell warn（评审第 2 档）**：与 scanner 格同口径披露丢弃——二阶段失败原因、`parse dropped`、`needs dropped / truncated`、
`N findings dropped: quote not anchored`、`stream discarded N`。原实现只写旁路丢弃，「模型零发现」与「finding 全被锚定门静默丢掉」在报告里分不出来。首轮声明 needs = 模型自认判断不完整，静默沿用首轮会成单区域假绿灯。

## 6. 兼任角色 warning

`build_registry` 中 reviewer 因 `has_scan` 被 `continue` 跳过注册时，仍执行 `thinking_warning(em)` 并 `push_warning`——兼任时两角色只有 `thinking_level` 可能不同，
per-cell backend 已按 reviewer 自己的 `EffectiveModel` 构造，缺的只是这条披露。
实施：抽成 `backends::role_thinking_warnings(&RoleSets)`（scanner → reviewer 序、同文案去重），`build_registry` 建表前统一 push；文案带角色名以区分兼任两角色的不同档位。

## 7. 测试形状

虚拟时间：`#[tokio::test(start_paused = true)]` + Fake `with_delay`。

| 用例 | 构造 | 断言 |
|---|---|---|
| id 定死 | 入队 → pop → 通道并入扩区间 | `mark_settled(派发时 id)` 命中；id 不变 |
| 共识只经谓词 | 同区间不同 quote 两条（不同家族） | 合并为一项、agreement 仍 1；quote 重叠两条 → agreement 2 |
| Skipped 复位 | drain 截单家族项 → 异族 / 同家族分别并入 | `MergedPending`+Pending / `MergedSkipped`+Skipped |
| resume 两次不丢 | 复现批 C 覆盖路径（首跑 X 落盘、Y 在途 kill → resume ×2） | 第二次 resume 后 Fx 仍在报告与 cells |
| Settled 以成功格为准 | 快照 Settled：一项无格、一项只有失败格、一项有成功格 | 前两项重挖、第三项不动；重挖成功 exit 0 |
| 在途峰值 | 门宽 N、K > N、每格时延 | `gate_stats().peak == N` |
| 乱序不错配 / 慢格不堵 | 各格不同时延 | findings 对应各自 item；总时长 ≈ 最慢格 |
| 二阶段持门位 | 门宽 1、首格触发 needs | 首格两轮间门位不释放，第二格在首格结束后才开始 |
| 通道并入分叉 | 首轮时延中经通道并入异族 | 落盘 = 落地值 = `landed_agreement`；事件值严格 < |
| 通道未关不收场 | 队列清空后通道仍开、稍后再送一项 | 该项被挖；关闭后才收场 |
| 报告确定性 | 同输入、两次打乱完成时延 | 报告 JSON 逐字节一致；meta 四数组等长对齐 |
| 二阶段失败计债 | 第二轮 Fake 返回 Err | 首轮 finding 在报告；exit 2；token 入账 |
| 全被拒不计债 | needs 全指向未入库路径 | skipped +1；exit 0 |
| 轮转分落 | 两成员不同 provider、门宽各 1 | 两格并行分落不同模型 |
| Auth 踢出 | 成员 A 首格 401、队列 > 容量 | A 移出池、后续项全落 B、warning 点名 |
| 池空 | 唯一成员 401 | 剩余项 Pending、exit 2、warning 报数 |
| 0 格成员 | 两成员、队列 1 项 | warning 点名未分到者 |
| 墙钟门 | K=12 × 50 ms、W=4 | 虚拟时长 ≈ 3 × 50 ms |
| 退化零差 | 单 reviewer、门宽 1 | 与批 C 串行报告同构（基线由批 C 代码跑同一场景采得，见 §8） |
| 429 注入 | Fake 首发 429 | 收门后全部完成、无死锁 |

## 8. 兼容与回滚

- schema 不变（v4）；`queue_items` 形态不变；`EnqueueOutcome::MergedSkipped` 仅内部使用。
- cell_id 形态从 `review[{n}]@` 变为 `review[{item.id}]@`：无生产代码解析该格式；旧作业的 `review[n]@` cell 在 resume 时按 §2.4 找不到 → Settled 项会被重挖一次（一次性成本，不丢数据）。
- 回滚 = 不合回；合回后 revert 该 merge。退化零差用例是第一道回归哨。
- 退化零差基线 `src/orchestrate/testdata/deep_serial_baseline.json`：`git archive eac0a60` 导出批 C 源码，在其 `deep.rs` 测试里跑与 C1 同一份场景
  （`degenerate_serial_scenario`，两边只差 `incoming` 字段与 cell 名映射）写出 JSON。后续批次**有意**改变串行形态（如 C2 / D 改 prompt 链路或 meta 口径）时，
  审过 diff 再更新该文件——它钉的是「单成员门宽 1 的派发形态」，不是永远冻结的输出。

## 9. 定案记录（2026-09-13）

| # | 问题 | 决定 |
|---|---|---|
| — | Skipped 并入 | 仅成必挖才复位；否则 `MergedSkipped` |
| — | 事件侧滞后 | 接受，钉 `event ≤ landed` |
| Q1 | scanner 与深挖重叠 | 另立批 C2 |
| Q2 | 同区间不同 quote | 合并一次挖掘，只 `same_item` 计共识 |
| Q3 | resume 覆盖 cell | cell_id 按队列项 + 「Settled 以 cells 为准」 |
| Q4 | 二阶段门位 | 一格持门位到底 |
| Q5 | 二阶段失败 | 保留首轮 finding + warning + 计债；全被拒不计债 |
| Q6 | 乱序完成 | 报告按派发序组装，meta 对齐 |
| Q7 | 坏成员 | 首次 Auth 踢出池，不改派，池空停派 |
| Q8 | 轮转策略 | 纯空闲轮转 |
| Q9 | dogfood | 当前配置 + `agc.maxConcurrency=4` 争门轮 |
| Q10 | C2 接口 | C1 加可选入队通道，barrier 含通道关闭 |
