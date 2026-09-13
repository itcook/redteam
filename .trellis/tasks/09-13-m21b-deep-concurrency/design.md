# 批 C1 技术设计：门驱动并发派发与 reviewer 分派池轮转

> 输入：prd + plan 批 C1 节（C1-1…C1-5）+ design v10 §6 规则 5〔D13〕/ §7 / §8.1。
> 批 C 已合回（`eac0a60`）。队列语义、pack、prompt、观测字段集**不重写**——本文只钉派发形态与两处裁定的落点。
> 用户裁定（2026-09-13）：Skipped 并入「仅成为必挖才复位」；事件侧疑点共识「接受滞后，钉住 event ≤ landed」。

## 1. 边界与文件

| 文件 | 动作 | 内容 |
|---|---|---|
| `src/orchestrate/queue.rs` | 修改 | id 定死（C1-1）；`merge_into` Skipped 复位规则 + `EnqueueOutcome::MergedSkipped` |
| `src/provider/registry.rs` | 修改 | `InflightGate::try_enter` + `ProviderRegistry::try_lease`（非阻塞取门位） |
| `src/orchestrate/deep.rs` | 修改 | 主循环改 `FuturesUnordered`；cell 执行（首轮 + 二阶段）整体搬进 `run_cell` future；reviewer 轮转 |
| `src/cli/mod.rs` | 修改 | `coverage.selected[Reviewer]` / `RunMeta.models` 含全部实际参与条目；未分到格的条目 warning |

不碰：`review_shot.rs` 的两轮单发与锚定门、`region::build`、prompt 模板、`DeepMeta` 字段集、scanner 路径、`verify.rs`（批 D）。
碰撞点（plan 波次 3）：与批 D 唯一重叠是 `run_deep` 的 `sink` 参数——C1 不改其类型（仍 `&dyn FindingSink`）。

## 2. C1-1 队列项身份与 spec 解耦

**id 定死**：`QueueItem::new` 按首次 spec 算 `item_id(&spec)`（形态不变：`file:origin:ranges`），`merge_into` **删除** `it.id = item_id(&it.spec)` 那一行。`spec.ranges` 可继续 union、id 不跟着变。

- 为什么不换单调序号：现形态已写进 `queue_items` 快照的 `item_id` 列与 JSON，保持形态 = 旧快照 `restore` 零迁移。
- 查重仍用 `find_merge_idx` 的 `same_item(spec) || it.id == incoming_id`：id 不再追随 ranges 后，`it.id == incoming_id` 只命中「与首次 spec 完全相同」的入队，扩展后的区间靠 `same_item` 的重叠谓词命中——语义不变。

**Skipped 并入复位**（用户裁）：

```text
merge_into(Skipped 项):
  并入家族、重算 agreement（agreement_of(families)，单调不减）
  if is_must(it):          // Primary ∨ agreement ≥ 2
      state = Pending      // 必挖不截（design §6 规则 3）
      enqueued_ms = now    // 重置，免 item_wait_ms 虚高
      → MergedPending
  else:
      保持 Skipped          // 不突破 drain 定下的预算
      → MergedSkipped      // 新枚举，note_enq 计入 dedup_hits
```

`budget_skipped` 口径：复位的项**不**回减 `meta.budget_skipped`（它记的是 drain 那一刻的截断数，披露口径不变）；复位项被挖时正常进 cells。

## 3. C1-2 并发派发循环

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

`s.cap` 是 429 收敛后的有效门宽（D13 只降不升），`try_enter` 天然尊重；`GatePermit` 的 Drop 语义（归还、收窄时不唤醒）不变。未注册 route_key → `Err`（同 `lease`）。

### 3.2 主循环与 future 边界

```text
running = FuturesUnordered::new()
loop {
  // ① 填门位：按轮转游标对 reviewer 条目依次 try_lease，拿到即 pop_dispatch + 组 future
  while q.pending_count() > 0 {
      match next_lease(registry, reviewers, &mut cursor)? {   // 轮转见 §4
          Some((lease, rev)) => {
              item = q.pop_dispatch(); snap_one(item)
              meta.item_wait_ms.push(派发时刻 − enqueued_ms)   // 派发时定格（第 2 档口径不变）
              pack / prompt 在此同步构造（region::build 是同步的）→ move 进 future
              running.push(run_cell(lease, rev, item, prompt, cell_id, ...))
          }
          None => break,                                        // 门全满
      }
  }
  // ② barrier：在途 0 ∧ Pending 0 → 结束（复核在途批 D 前恒 0）
  if running.is_empty() { if q.pending_count() == 0 { break } else { 兜底：阻塞 lease 一次 } }
  // ③ 等任一格完成（完成即释放门位），回主循环处理
  done = running.next().await
  landed = live_agreement(&q, &done.item) → 回填 done 的 findings.suspicion_agreement
  meta 累加（needs 触发/跳过、stream_discarded、reviewer_output_tokens、dispatch/landed_agreement）
  snap_cell(rec) → cells.push → q.mark_settled(id, landed) → snap_one
  warnings.extend(done.warnings)
}
```

**future 内（`run_cell`）= 一格的完整执行**：per-cell backend 构造 → `run_review_shot_with` → 读 `policy.retries()/waited_ms()` → drop lease → **needs 二阶段补查**（`second_stage_round` 内部自己 `registry.lease().await` 重新排队）→ 返回 `CellDone { item, cell_id, t0, result, rl_retries, rl_wait_ms, needs_triggered, needs_skipped, warnings }`。

**future 外（主循环）= 全部共享可变状态**：队列读写、`meta`、`cells`、`findings`、`warnings`、`on_cell` / `snapshot` 回调。

> **坑（必须搬进 future）**：批 C 的二阶段在主循环的 `match` 里 await。并发后若留在主循环，主循环 await 二阶段期间不 poll `running`，**所有其他在途格停摆**。所以二阶段整体进 `run_cell`；它原来的 `&mut warnings` 改为 future 本地 `Vec`，经 `CellDone.warnings` 带回。

`&dyn FindingSink`、`&DeepInput`、`&CrossIndex`、`&PackOpts`、`&ProviderRegistry` 都活过 `run_deep`，被多个 future 以共享引用借用——`FuturesUnordered` 同 task 内并发，不要求 `Send` / `'static`。

### 3.3 barrier 与兜底

- `running` 空时所有 `GatePermit` 已 drop → 各门 `held = 0 < cap` → `try_lease` 必成功（除未注册条目）。所以「在途 0 ∧ Pending > 0 ∧ 一格都派不出」只可能是未注册 reviewer：沿用批 C 口径 `warnings` + `reviewer_failed = true` + break。
- 防御性兜底：若 `running` 空、Pending > 0、`try_lease` 全返回 `None`（理论不可达），改用阻塞 `lease` 取第一条目一次，避免空转死循环。
- 整体超时退路不新增（后台 job 已有 40 min deadline；单格超时 `per_turn` 不变）。

## 4. C1-3 分派池轮转

- **游标轮转**：`cursor` 记上次成功派发的条目下标；`next_lease` 从 `cursor + 1` 起对 `reviewers` 逐个 `try_lease`，第一个拿到的就用，并把 `cursor` 置为它。全部 `None` → 门全满。
- **同 provider 共享门**：两条目同在 `agc` 时共享一个门宽，轮转只决定「这个空位给谁」，不加吞吐（prd Background）。跨 provider 时各门独立，一个满了另一个继续吃单。
- **每项只由一个模型消费**（不双审）：`pop_dispatch` 一次只弹一项，交给拿到门位的那一个条目。
- **cell_id**：`review[{n}]@{rev.name}`，`n` = 主循环派发序（单线程主循环自增，天然唯一）。
- **观测**：`coverage.selected[Reviewer]` 仍按实际 cells 列（批 C 已如此）；`RunMeta.models` 补齐实际参与的 reviewer 条目；**配了但一格未分到**的条目 → warning `"deep: reviewer {name} configured but received 0 cells"`（dogfood 轮 2 grok-4.6 空转且无声的那种情况不再静默）。

## 5. 事件侧疑点共识滞后（用户裁：接受）

- `EmitCtx.suspicion_agreement` 仍取派发时快照（future 内读不到队列）；落盘 finding 在主循环按 `live_agreement` 回填（批 C `86706e8` 已如此）。
- **不变式 `event ≤ landed`**：`agreement_of(families)`、`families` 只 push 不 remove → 队列项 agreement 单调不减 → 派发时快照 ≤ 落地值。批 D 跳过规则「≥ 2 免复核」读较小的事件值，偏差方向只会是**多复核**、不会**漏复核**。
- 钉住方式：分叉用例（§6）同时断言 `FindingEvent.suspicion_agreement ≤ 落盘 finding.suspicion_agreement`，且在途并入场景下严格 `<`。
- 文档：`FindingEvent` 字段注释补一句不变式与安全方向（给批 D 读者）。

## 6. 测试形状

| 用例 | 构造 | 断言 |
|---|---|---|
| id 稳定 | 入队 → pop（InFlight）→ 并入扩区间 | `mark_settled(派发时 id)` 命中；id 未变 |
| Skipped 复位 | drain 截断单家族项 → 异族并入（→ 2） | `MergedPending` 且 Pending；同家族并入 → `MergedSkipped` 且仍 Skipped |
| 旧快照兼容 | 批 C 形态 id 的快照 JSON restore | 状态与 id 原样 |
| 在途上限 | Fake 慢 backend（`tokio::time::sleep`）、门宽 N、K > N 格 | `gate_stats().peak == N` |
| 乱序完成 | 各格不同时延 | 每个 cell 的 findings 对应自己的 item.file |
| 慢 consumer | 一格极慢、其余正常 | 其余格先落地，总墙钟 ≈ 慢格 |
| 二阶段不阻塞 | 一格触发 needs 补查（带时延）、其余正常 | 其余格在补查期间完成 |
| 分叉用例 | 在途期间向同项并入异族（测试钩子驱动） | 落盘 = 落地值 = `landed_agreement`；事件值 < 落盘值 |
| 轮转 | 两条目、门宽各 1（不同 provider） | 两格并行、分落不同模型 |
| 一条满另一条接 | 两条目、A 门宽 0 有效（占满） | 全部落到 B |
| 未分到 warning | 两条目、队列只 1 项 | 有 warning 指名未分到者 |
| 墙钟门 | K=12 格 × 固定 50 ms、门宽和 W=4 | 总时长 ≈ ceil(12/4)×50 ms（容差放宽） |
| 退化零差 | 单 reviewer、门宽 1 | 与批 C 串行报告同构（cell 序、findings、meta） |
| 429 注入 | Fake 首发 429 | 门收窄后仍全部完成、无死锁 |

分叉用例需要「在途期间入队」的驱动点：批 C1 本身不引入在途入队的生产路径（入队仍全在循环前），测试经 `#[cfg(test)]` 钩子在 future 挂起时调 `q.enqueue`。生产路径的在途入队由批 D 复核回灌 / M21c 增量轮引入时复用本节断言。

## 7. 兼容与回滚

- schema 不变（v4）；`queue_items` 形态不变；`EnqueueOutcome` 新增变体仅内部使用（不序列化）。
- 单 reviewer + 门宽 1 退化与批 C 同构——任何回归先看此用例。
- 回滚 = 不合回子分支；合回后 revert 该 merge。
