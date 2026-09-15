# M21b 批C2：scanner 与深挖时间重叠——流式入队/在线预算与保底预留/门让位与唤醒/scanner 失败口径

## Goal

把批 C / C1 的「scan 全完 → 深挖」改成 design §2 的时间重叠形态：H0 在 t=0 入队即可派发，scanner 格完成即把疑点经 C1 预留的入队通道送进队列，scanner 全部返回 = 关闭入口。墙钟从「scan 段 + 深挖段」变成 `max(最慢 scanner cell, 队列排空)`（复核尾部归批 D）。

重叠带来的是**语义**变化，不只是派发形态：预算在项持续到达时在线算、保底为后到者预留、共门时 scanner 先入门、reviewer 已开挖后 scanner 失败的收场、resume 怎么接。
**定案**：2026-09-15 `/grilling` 十问（Q1–Q10），技术设计见 `design.md`（§13 定案记录），权衡与被否见 decisions **D15**，design 升 v11。

## Background（已核实，2026-09-15 · HEAD `d23e715`）

- **前提**：C1 已合回（`80919ad`）；集成分支四绿（lib 683 / cli 17 / e2e 17 / packaging 7；clippy 7 = 基线；`make verify` 绿）。
- **主流程仍是串行两段**（`src/cli/mod.rs` `run_code_review`）：`run_scan_fanout(...).await` 全部返回 → 任一 scanner 格失败或无 reviewer 即返回 scan-only 报告（exit 3 Incomplete）→ 否则 `run_deep`，生产两处都传 `incoming: None`（首跑与 resume）。
- **入队与预算在派发前一次定死**（`src/orchestrate/deep.rs`）：`enqueue_h0` → `enqueue_scan`（对全部 scan 格跑 `merge_suspicions`：按组并查集、`same_suspicion` 传递闭包、代表取最高 confidence）→ `drain(budget_cells)`。经通道到达的项走入口五态但**不过预算**（`on_arrival`，每次全量写快照）。
- **`drain` 口径**（`queue.rs`）：必挖（主区 H0 ∨ agreement ≥ 2）全留 → 按优先级留 ⌈maxCells/4⌉ 条疑点作保底 → 按优先级补到预算 → 余者 Skipped；`maxCells = 0` 只留必挖。
- **scanner 扇出**（`src/orchestrate/mod.rs` `run_scan_fanout`）：大格先发排序后 `join_all`；每格阻塞 `registry.lease()` → `lease.run` → `sink.on_cell(record)`；结果按 plan 序回收。scanner 输出里的 **needs 从未入队**（`Origin::Needs` 只出现在测试里；design §6 把 scanner needs 列为第三种生产者）→ 归批 C3。
- **门**（`src/provider/registry.rs` `InflightGate`）：自管 `held / cap` + `Notify`；scanner 走阻塞 `enter()`，深挖主循环走非阻塞 `try_enter()`，后者只比 `held < cap`、**不感知等待者**——放门唤醒的 scanner 尚未被 poll，深挖同步 `try_lease` 就把门位拿走。
- **C1 主循环唤醒源只有两路**：在途格完成 / 通道到达（`select!`）；外部持门者放门不唤醒（C1 评审 #6，探针 300 ms vs 及时补位 210 ms）。
- **C1 定案里两条前提会被流式入队拿掉**：「共识只经谓词」的理由「上游已组内传递闭包」（`merge_into` 注释）；「报告逐字节一致」的前提「派发序 = 静态优先级序」（C1 design §3.4）。
- **resume 的隐含前提失效 + 现存缺口**：只在「scan 补齐 ∧ 有 reviewer ∧ 快照非空」时续挖，隐含「快照只在 scan 全完、drain 后才写」；配 reviewer 的作业在扫描期被 kill（快照为空）→ resume 出 scan-only、**退出码 0**。
- **同步 IO**：读守卫 `git ls-files` 本仓中位 9.4 ms / 次；`region::build` 每项同步走仓搜索；快照每次到达全量写（到达 × 项数次 SQLite 写）。C2 下深挖与 scanner 同 task，阻塞同时停住两边读流。
- **真实配置**（agc 门宽保持 6，2026-09-15 定）：scanner = deepseek-flash（门 16）+ qwen3.8-flash（门 20）；reviewer = glm-5.3（agc 门 6，兼 verifier 角色，批 D 前不消费）；grok-4.6 无角色。scanner 与 reviewer 不同门——共门场景只能构造；批 D verifier 与 reviewer 同在 agc。
- **墙钟基线**（目标 diff 同批 C：`f1d3ca3` 状态、`--base 674583e`，budget_cells = 12）：
  - C1 dogfood（混合池 glm + grok）：agc 门 20 总 11.2 min（scanner 段 4.0 / 深挖段 7.1）；门 4 总 19.7 min（3.6 / 16.1）。私有记录 `.next/private/m21b/2026-09-14-batch-c1-dogfood.md`（集成工作树）。
  - **现配置补跑**（2026-09-15，HEAD `d23e715` 构建；glm 单成员 · agc 门 6）：总 **20.6 min**（scanner 段 6.2 = qwen g1 374 s / 深挖段 14.3）；门 6 仍撞 429 × 2 → 收到 4，12/12 成功、零断流；队列 22 项截断 10；findings 6（格内 7）；12 格里 4 格是 roadmap 文档行，与 C1 ① / ② 选中项各重合 4/12。scanner 格完成时刻 58 / 197 / 255 / 374 s。私有记录 `.next/private/m21b/2026-09-15-batch-c1-baseline-current-config.md`（原始报告与脚本在同目录 `raw/`）。
  - **策略重放**（同选中项、实测格时长、只改可派时刻）：C1 形态门 6 / 4 = 17.7 / 21.6 min（实测 20.6）；等收场 15.8 / 18.3；纯贪心与**贪心 + 收场预留** 13.7 / 16.8；贪心 + 组完成才入队 14.4 / 17.6。
  - **合并残差实测**：逐格到达的队列侧合并 vs 组内闭包——基线拆 0 / 2 簇；PR181 双家族 r1 拆 1 / 19、r2 拆 0 / 19；必挖项数均不变。
- **批序**（2026-09-15 定）：C2 → C3（深挖选项稳定性）→ D（verifier 流式段）→ 出口验收。批 D 碰撞面 = `run_deep` 的 `sink`（仍 `&()`）与 barrier「复核在途 0」；「流式旁路跨瞬时重试不复位」归 D 接真 sink 前修。
- **退化零差基线** `src/orchestrate/testdata/deep_serial_baseline.json` 钉「单成员门宽 1 的派发形态」：本批预期不变，若有 diff 审过再更新（C1 design §8）。

## Requirements（定案）

- **C2-1 流式接线（design §2）**
  - 配 reviewer 时建入队通道；scanner 格落盘后即逐条发该格锚定后疑点（到达键 `{cell_id}#{i}`）；scanner 全部返回 = drop 发送端 = 关闭入口；
    `join!` 之前显式 poll 扫描一次，保证 t=0 scanner 先占门或登记。
  - H0 在 t=0 入队并可立即派发；scan 报告的疑点并集仍按组 `merge_suspicions` 生成（报告口径不变）。
  - `phase`：收场前 `Scan`、收场时切 `Deep`、收口 `Finalizing`。
- **C2-2 在线预算（Q1 B，design §3）**
  - 扫描期非必挖项仅在「本调用已派发 < K − F」时可派（F = ⌈K/4⌉，K = 0 时 F = 0），必挖项不受限；不在无可派项时取门位。
  - 关闭入口时对剩余预算 drain 一次：`drain_with(K − 已派发, F − 已派发非必挖疑点)`，与 C1 drain 同形；Skipped 只在此刻判定——C1 评审 #5 假债随之消解。
- **C2-3 合并残差（Q2 M1，design §4）**
  - 入口五态与谓词不变；`merge_into` 注释前提改写。
  - 收场以组内闭包成员（到达键）对账队列项，拆簇数写 `DeepMeta.queue_split_clusters`（给 M21e 同组 hold 决策攒数据）。
- **C2-4 scanner 失败口径（Q5 F3，design §5）**
  - 删 scanner 失败早退：深挖照跑；verdict 硬 Incomplete、退出码 3（优先于 reject 10 与带债 2）；findings / stats / 观测照常进报告；
    verdict 本会 reject 时 warning 报 blocking 条数；`--help` 同步。
- **C2-5 resume 离线续跑（Q6 RS1，design §6）**
  - 补跑缺失 scanner 格 → 全部成功 scanner 记录的疑点按到达键幂等并入快照队列 → 本次补跑过 scanner 则复位 Skipped 按剩余预算重选，否则沿用 → 深挖。
  - 快照为空但配了 reviewer 走同一路径（H0 按当前 diff 重算）——修「扫描期 kill 后 resume 出 scan-only / 退出码 0」。scanner 仍缺失 → Incomplete（F3 口径）。
- **C2-6 门公平与唤醒（Q7 G1，design §7）**
  - `InflightGate` 记等门者：非阻塞首试在有等门者时让位；登记者被唤醒可取；取消安全（计数回退、唤醒转交）。
  - 深挖有可派项而各门无位时自身排入 FIFO（跨事件保持排队位、可派项消失即撤销）——外部持门者放门后及时补位（修 #6）；机制对批 D verifier 通用。
- **C2-7 报告组装序（Q8 D2，design §8）**：收场按最终队列项优先级（`cmp_priority`）组装 findings / cells / meta 四数组；保证改为「同一选中集合 + 同样格输出 → 逐字节一致（计时字段除外）」。
- **C2-8 同步 IO（Q9 IO2，design §9）**：快照只写受影响项；git 跟踪文件集每轮预取一次、读守卫查集合（拒绝口径不变）；填门段与到达处理段阻塞计时入 `DeepMeta`；`spawn_blocking` 不在本批（followup 挂触发条件）。
- **观测（design §10）**：`DeepMeta` 增 `close_ms` / `first_dispatch_ms` / `drain_done_ms` / `verify_tail_ms`（批 D 前 0）/ `scan_phase_dispatched` / `close_budget_remaining` / `queue_split_clusters` / `fill_block_max_ms` / `fill_block_total_ms`，均 serde default。

## Acceptance Criteria

- [ ] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify` 全绿
- [ ] 时间重叠：H0 在 scanner 全部返回前派出并完成；通道关闭前不收场；t=0 共门时 scanner 先入门
- [ ] 在线预算：宽门构造下扫描期非必挖派发 ≤ K − F；必挖从不被截（`maxCells = 0` 时 H0 照挖、后到才成必挖的项照挖）；收场按剩余预算 drain；Skipped 只在收场产生、`budget_skipped` = 收场时 Skipped 数、无假债
- [ ] 合并残差：A~B~C 非传递构造（A、C、B 到达）→ `queue_split_clusters == 1`；scan 报告疑点并集与改动前一致
- [ ] scanner 失败：深挖进行中 scanner 格失败 → 深挖跑完、Incomplete / exit 3、findings 在报告、warning 报 blocking 数
- [ ] resume：① 扫描期 kill → 补跑 + 幂等入队 + drain + 挖；② 首跑 F3 带缺口收场 → 补跑后 Skipped 复位重选；③ 空快照 + reviewer → 深挖（先红后绿）；④ 多项 InFlight 并发 kill → finding 不丢不双计
- [ ] 门公平：放门时深挖不插队；外部持门者放门后及时补位（#6 探针转绿）；等门者被唤醒后取消 → 下一位仍被唤醒
- [ ] 报告确定性：选中集合固定、打乱各格完成时延两次 → 报告逐字节一致（计时字段除外）；meta 四数组等长对齐
- [ ] 同步 IO：每次到达快照写 ≤ 1 项（重复到达 0 项）；读守卫拒绝口径不变；`fill_block_*` 可读
- [ ] 退化零差基线（单成员门宽 1）不变，或审过 diff 后更新
- [ ] dogfood 一轮：同目标 diff（`f1d3ca3`、`--base 674583e`），配置与基线一致（agc 门 6，跑前跑后 sha 校验）；对照基线 20.6 min 与重放预测（门 6 13.7 / 门 4 16.8 min）看分段（不硬卡单点门槛）；记入 `.next/private/m21b/`
- [ ] 收口：code-review 审核 → 修 → `--no-ff` 合回（M21 期间不跑 ship 评审）；本批私有记录拷回集成工作树

## Out of Scope

- **批 C3（深挖选项稳定性，C2 合回后、D 前）**：confidence 排序键与 reviewer 笔记里的 confidence、design 规则 2「区间扩到 enclosing def / 同文件相邻区间合并」、文件多样性与同分 tie-break、scanner samples ≥ 2、scanner needs 入队
- 批 D：verifier 流式段、真 sink、barrier 的复核在途计数、旁路跨瞬时重试复位
- `region::build` / `need_section` 挪 `spawn_blocking`（等 PR181 出口验收数据，followup 挂触发条件）
- 台账与增量轮（M21c）；三方契约注入（M21d）；第二家族 scanner（M21e）；同组 hold（M21e 凭拆簇数与派发 / 落地 agreement 定）
- 429 棘轮公式、断流视同过载收门（followup 候选，另议）；模型权重 / 候补 / failover（D7 A3）

## Technical Notes

- 分支 `feat/m21b-deep-streaming`，工作树 `~/hobby/.worktrees/redteam/m21b-deep-streaming`（自 `feat/m21-review-engine` 切出）。
- `.next/private/` 不随新工作树：C1 及更早的私有记录从集成工作树读；本批私有记录写在本工作树 `.next/private/m21b/`，收口时拷回集成工作树（C1 先例）。
- 实现形态：scanner 扇出与 `run_deep` 同 task 并发（沿用 C1「不要求 `Send` / `'static`」的借用形态），不 spawn、registry 不 `Arc` 化。
- 定案过程：2026-09-15 C1 现配置基线补跑 + 四策略重放 + 合并残差实测 → `/grilling` Q1–Q10（均按推荐）；另定 agc `maxConcurrency` 保持 6、批序 C2 → C3 → D。
