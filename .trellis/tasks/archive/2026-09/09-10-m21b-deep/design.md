# 批 C 技术设计：深挖队列与 reviewer 运行时

> 输入：prd + plan 批 C 节（C-1…C-7）+ design v10 §2 / §6 / §7 / §8.4 / §12.3〔D13〕。
> 五态 / 优先级 / 必挖 / 保底 / graceful drain / 门驱动派发 **不重写**——本文只钉接线形状。
> 批 A/B 已合回（`eaf8bb5` / `27bf391`）。429 棘轮沿用 D13（2026-09-10 裁，followup）。

## 1. 边界与文件

| 文件 | 动作 | 内容 |
|---|---|---|
| `src/orchestrate/queue.rs` | 新增 | `QueueItem` + 五态 `enqueue` + 优先级/预算/必挖/drain + 单派发器（C-1/C-2/C-3） |
| `src/orchestrate/review_shot.rs` | 新增 | `run_review_shot` + `FindingEvent` + `FindingSink`（C-3） |
| `src/orchestrate/mod.rs` | 修改 | `run_code_review` 接线：H0 t=0 入队、scan 完成回调入队、barrier 等深挖 |
| `src/cli/mod.rs` | 修改 | `stage0_risk` 保留 `RiskRoute` 下传；barrier → reduce；`DeepMeta`；exit_code 第二参 |
| `src/domain/report.rs` | 修改 | `DeepMeta`；`RunMeta.deep` |
| `src/state/mod.rs` | 修改 | `Phase::Deep` / `Phase::Verify`（旧 `Refute` 可读） |
| `src/db.rs` | 修改 | `queue_item_put` / `queue_items_for_job`（表壳 v4 已建，C-5 开始写） |

不碰：`src/risk/` 规则、`src/pack/region.rs` 装配、`src/consolidate/verify.rs`（批 D 独占）、scanner 路径零 diff。
碰撞点（plan 波次 2）：独占 `orchestrate/`、`cli/mod.rs` 主流程、`state/`。

## 2. H0 如何进队列（阶段 0 不丢区列表）

`stage0_risk`（`cli/mod.rs:247`）今天丢掉 `RiskRoute.hotspots`，只写 `RiskMeta` 计数。C **不从 RunMeta 反推区间**。

```
route_from_git(...) -> RiskRoute          // 调一次（git grep/log 有成本）
  ├─ RiskMeta { hotspots, r, tier, budget_cells }  → 报告 / resume 沿用
  └─ RiskRoute 所有权留在 run_code_review
       └─ t=0：scanner 扇出已提交之后，hotspots 入队（scanner 先于 H0〔D13 Q2〕）
            origin = pack::region::Origin::Rule
            ranges = (start, end) 1-based 含端点
            notes = []（QueueItemSpec::new 构造校验）
            score = hotspot.score；主区 > 涟漪（kind 分层，同分再比 score）
```

`budget_cells` = `route.budget_cells_hint`（已含 Fixed/Auto/帽 48）。不再二次 `budget_cells(tier, cfg)`。

## 3. 队列项（对齐批 B `QueueItemSpec`）

```rust
// Origin 复用 pack::region::Origin（rule/scan/needs），导入改名 QueueOrigin
// 勿与 domain::finding::Origin（carried/new）混用（followup 批 B 接缝）
pub struct QueueItem {
    pub id: String,                 // 稳定：file + 并后 ranges + origin
    pub spec: QueueItemSpec,        // file / ranges / origin / notes
    pub score: f32,
    pub agreement: u32,             // 活值；异族 +1，同家族只合并
    pub state: Pending | InFlight | Settled,
    pub kind: Primary | Ripple | Suspicion | Needs,  // 优先级层
    pub dispatch_agreement: u32,    // 弹出时快照
    pub landed_agreement: Option<u32>,
}
```

合并谓词 = `consolidate::scan::same_suspicion`（`scan.rs:22`：同文件 ∧ 行重叠 ±LINE_GAP ∧ quote 去空白相等或不琐碎重叠）。hint 只辅助，不加 agreement（R4）。跨文件不合并。跨轮判例守卫本批空过 → 按无命中入队（M21c）。

优先级键（非 FIFO、confidence 不做门）：

1. 主区 H0
2. 涟漪 H0
3. 疑点 `confidence × agreement × 类别权重`
4. needs 中档（固定分，低于疑点层、高于被截项）

**类别权重本批全 1.0 是实现取舍，不是既有 spec。** 上位 design §6 写了「类别权重」因子、没有数字表；L0-L5 自标可靠性未验（R4 → M21c）。M21b 不做类别校准，用中性 1.0，排序实际由 `confidence × agreement` 与 H0 分层决定。权重表留 `const CATEGORY_WEIGHT: [f32; 6] = [1.0; 6]`，改数字不改公式。

必挖 = 主区 ∪ {agreement ≥ 2} 不截；`maxCells = 0` 时主区仍挖。保底 = ⌈maxCells/4⌉ 给疑点；主区吃穿时帽 = 主区 + 保底并披露。drain：在途跑完，未开跑标 `skipped(budget)` 进 `DeepMeta`，不静默。

## 4. `FindingEvent` 与流式旁路契约

定义在 `orchestrate/review_shot.rs`，**不**泄漏队列内部状态。

```rust
pub struct FindingEvent {
    pub finding: Finding,           // id / agreement / models 已按落地时刻改写
    pub agreement: u32,             // 落地时刻（跳过规则按此算，后涨不回溯〔D13〕）
    pub reviewer_family: String,    // verifier 异族选择
}

/// 批 D 合回前的背压：可选 sink，缺省 no-op。不用有界 mpsc（无消费者会堵住派发器），
/// 也不建无界 channel（无消费者会藏内存增长）。
pub trait FindingSink: Send + Sync {
    fn on_finding(&self, ev: FindingEvent);
}
impl FindingSink for () {
    fn on_finding(&self, _ev: FindingEvent) {}
}
```

`run_review_shot(..., sink: &dyn FindingSink)`。C 阶段传入 `()`；批 D 传入流消费者。barrier「复核在途 0」在 C = 恒真（没有 verifier 在途计数器）。C 不改 `verify.rs`，也不创建发送 channel。

### 4.1 旁路 vs 终判（权威与去重）

旁路是持久化旁路，**终态 `parse_review_output_with_stats` 是权威结果**（批 B / D10）。落盘与 `FindingEvent` 按下面走：

1. SSE `feed` → `take()` 得到 `RawFinding`。id = `orchestrate::finding_id`（quote 锚；空 quote 退化行号，与 `enrich` 同源）。每条：`anchor_review_findings` 单条（或批量后过滤）→ 写入 `store.append_partial` → `sink.on_finding`。已发射 id 进本 cell 的 `emitted: HashSet<String>`。
2. 请求结束（含 REPAIR 一轮之后）跑终判 parse。对权威 `Finding` 列表：id 已在 `emitted` → 不重发、不重写盘（旁路已是同一 id）；id 不在 → 补落盘 + `on_finding`（旁路漏了的闭合对象，例如非流式 backend）。权威里没有、旁路已发的：保留已落盘（死亡/截断价值），warning 记 `bypass_not_in_final`。
3. `IncrementalFindings::finalize()` **正式接口**，批 C 必调，幂等（`stream_json.rs:69`）。时机：
   - 正常结束 / REPAIR 成功后：parse 之后调（余下未闭合计 discard）。
   - 超时 / 取消 / cell 死亡：parse 可能失败，**仍调** finalize，已闭合对象已在步骤 1 落盘。
4. `discarded()` 回填 `ReviewParseStats` 新增观测字段 `stream_discarded`（旁路截断 + 坏形状）；并累加 `DeepMeta.stream_discarded`。与 `dropped_quote`（定位失败）分列。plain `parse_review_output` 仍丢计数——C 只走 `_with_stats`。

调用方契约（followup 批 B，写死）：parse 后必调 `anchor_review_findings`，回填 `dropped_quote`；**per-cell 构造 backend**（`backend_from_config().with_text_delta(...)`），共享 registry 实例不挂回调。

`run_review_shot` 观测对齐 `ScanCall`：`input/output/cached` tokens、`iterations`、`finish_reason`、`repair_note`、`rate_limit_retries` / `rate_limit_wait_ms`。

## 5. 派发 / barrier / 退化

- 单派发器：`registry.lease(reviewer_route_key)` 等到门位 → pop 最高 Pending → InFlight。无角色 workers。
- scan 来源项不等同组兄弟 cell；先到先挖，后到并入记录；记 `dispatch_agreement` / `landed_agreement`。
- barrier：scanner 全完 ∧ 队列无 Pending ∧ InFlight 0 ∧ 复核在途 0 → `consolidate::dedup` → 报告。verdict 从 findings 重建（`verdict.rs` 沿用）。
- 无 reviewer 条目 → 不建队列、不入 Deep，scan-only 报告与现路径同构（`Verdict::ScanOnly`，exit 0）。
- scanner cell 失败 → 仍 Incomplete / 3（§8.4 硬规则，本批不改）。
- verifier 失败 **不由 C 定义**（批 D：保守 upheld + warning）。

## 6. 退出码（用户裁，2026-09-10）

`exit_code(verdict, failed_models_nonempty)` 第二参本批启用，路径表：

| 路径 | verdict | 第二参 | exit | 披露 |
|---|---|---|---|---|
| 干净深挖完成 | Approve | false | 0 | — |
| reviewer cell 重试耗尽 | Approve | true | 2 | coverage `failed` + warning；已落盘 finding 保留 |
| 预算截断（drain skipped） | Approve | true | 2 | `DeepMeta.budget_skipped`；截断项不进 verdict |
| 上两路同时发生 | Approve | true | 2 | 两披露都在 |
| scanner cell 失败 | Incomplete | * | 3 | 现口径，C 不改 |
| 无 reviewer | ScanOnly | false | 0 | 退化 |
| verifier 失败 | （批 D） | — | — | C 不提前定义 |

不把 reviewer 失败打成 Incomplete/3（插件会当成没跑完）。签名不收敛：第二参 = 「对照集不完整 ∨ 深挖带债」。followup「exit_code 第二参恒 false」行本批关闭。

## 7. Phase、快照持久化、resume

- 枚举加 `Deep` / `Verify`（serde lowercase）。旧 JSON `"refute"` 仍反序列化为 `Refute`；新作业不打 `Refute` / `Fanout`。
- 新时间线：Collecting → Scan → Deep → Finalizing（`Verify` 由批 D 插入 Deep 与 Finalizing 之间）。

### 7.1 `queue_items` API（C-5 在 `db.rs` 加，表壳已在）

```sql
-- 已建：PRIMARY KEY (job_id, item_id)
-- state TEXT = pending | inflight | settled | skipped
-- json  = QueueItem serde（含 spec/score/agreement/kind/dispatch_agreement/landed_agreement）
```

```rust
fn queue_item_put(job_id: &str, item: &QueueItem) -> Result<()>   // INSERT OR REPLACE
fn queue_items_for_job(job_id: &str) -> Result<Vec<QueueItem>>
```

写点（每次状态转换，同一把 job 连接，不另开事务跨 finding 落盘）：

| 事件 | state | 失败 |
|---|---|---|
| `enqueue` 新项 / 并入 | `pending` | warning，内存队列仍有效；resume 可能丢这项 → 重跑扫描入队补 |
| pop 派发 | `inflight` | 同上 |
| cell 成功结案 | `settled` | 见下「原子性」 |
| drain 未开跑 | `skipped` | warning |

**原子性**：finding 落盘走现有 `cells` / `append_partial`（权威产出）；`queue_items.settled` 是 resume 索引，不是 finding 源。结案顺序 = 先写 cell 记录，再 `queue_item_put(settled)`。resume 以 cells 里已有 finding 为准，快照 Settled 且 cells 缺条 → 视为未完成重跑（宁双跑，去重 by finding.id）。快照写失败 **不** 把 job 打成失败——只 warning「queue snapshot failed」，本轮继续；代价是 kill 后可能重跑 InFlight/Pending。

### 7.2 resume

- 读 `queue_items_for_job`：**零行** → 旧作业 / 无深挖快照 → 现 scan-only resume，不进 Deep。
- Settled：沿用 cells 已落盘 finding（不双计）；Pending/InFlight：重跑 `run_review_shot`；Skipped：保持 skipped，进 `DeepMeta`。
- 未闭合对象：重跑路径重新 `finalize`；旧旁路已落盘的 id 去重。
- registry / `RunMeta.models` 按快照角色条目重建（followup：args.json 模型集不再权威）。

## 8. 顺手项（followup 已归 M21b，本批做）

- Anthropic thinking×tools：`build_anthropic_tools_body_ext` 在 tools 非空时守卫（reviewer 工具循环否则 400）。
- needs 二阶段：`--tools <off|two-stage|on>` 首个消费者，复用 `second_pass.rs`，触发/跳过计数入 `DeepMeta`。
- `AttemptPolicy`：旧退避提成 `BudgetBackoff`，`retry_classified` 单路径；reviewer / 现 scanner 调用点一并改（K11 仍不动 `single_shot` / 无门测试路径语义）。

## 9. `DeepMeta` 形状

`RunMeta.deep: Option<DeepMeta>`（serde default / skip None，旧报告不产键）。

```rust
pub struct DeepMeta {
    pub queue_depth_peak: u32,
    pub enqueue_reasons: BTreeMap<String, u32>, // "rule" | "scan" | "needs"
    pub dedup_hits: u32,                        // enqueue 并入次数（非新项）
    pub budget_skipped: u32,                    // drain 未开跑项数
    pub item_wait_ms: Vec<u64>,                 // 每项入队→派发
    pub dispatch_agreement: Vec<u32>,           // 每项弹出时
    pub landed_agreement: Vec<u32>,             // 每项结案时（与上方面同序或可按 item 对齐）
    pub reviewer_output_tokens: Vec<u64>,       // 每 reviewer cell；有界口径 < maxOutputTokens/2
    pub needs_two_stage_triggered: u32,
    pub needs_two_stage_skipped: u32,
    pub stream_discarded: u32,                  // 旁路 finalize + 坏形状
}
```

**不搬进 DeepMeta**（沿用现字段）：`provider_inflight_peak`、`provider_rate_limit`、`CellRecord.queue_wait_ms`（门等待 + 429 排队）。`RunMeta.refute_rate` 仍由批 D 填。

## 10. 兼容与回滚

- scanner 路径：无 reviewer 配置时零行为差（报告形态、exit 0）。
- 旧 resume / `queue_items` 零行：scan-only。
- 回滚 = 不合回 `feat/m21b-deep`；合回后 revert 该 `--no-ff` merge。schema 仍 v4（只开始写表壳）。
