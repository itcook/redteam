# M21b 批C2：scanner 与深挖时间重叠——流式入队/在线预算与保底预留/门让位与唤醒/scanner 失败口径

## Goal

把批 C / C1 的「scan 全完 → 深挖」改成 design §2 的时间重叠形态：H0 在 t=0 入队即可派发，scanner 格完成即把疑点经 C1 预留的入队通道送进队列，scanner 全部返回 = 关闭入口。墙钟从「scan 段 + 深挖段」变成 `max(最慢 scanner cell, 队列排空)`（复核尾部归批 D）。

重叠带来的是**语义**变化，不只是派发形态：预算要在项持续到达时在线计算、疑点保底要为后到者预留、scanner 与 reviewer 同门时 scanner 要先入门、reviewer 已开挖后 scanner 失败要有口径。**开工前须 grill**（议程见文末）；本 prd 是拷问输入，定案并入本文与 design.md。

## Background（已核实，2026-09-15 · HEAD `d23e715`）

- **前提**：C1 已合回（`80919ad`）；集成分支四绿（lib 683 / cli 17 / e2e 17 / packaging 7；clippy 7 = 基线；`make verify` 绿）。
- **主流程仍是串行两段**（`src/cli/mod.rs` `run_code_review`）：`run_scan_fanout(...).await` 全部返回 → 任一 scanner 格失败或无 reviewer 即返回 scan-only 报告（exit 3 Incomplete）→ 否则 `run_deep`，生产两处都传 `incoming: None`（首跑与 resume）。
- **入队与预算在派发前一次定死**（`src/orchestrate/deep.rs`）：`enqueue_h0` → `enqueue_scan`（对全部 scan 格跑 `merge_suspicions`：按组并查集、`same_suspicion` 传递闭包、代表取最高 confidence）→ `drain(budget_cells)`。经通道到达的项走入口五态但**不过预算**（`on_arrival`，每次全量写快照）。
- **`drain` 口径**（`queue.rs`）：必挖（主区 H0 ∨ agreement ≥ 2）全留 → 按优先级留 ⌈maxCells/4⌉ 条疑点作保底 → 按优先级补到预算 → 余者 Skipped；`maxCells = 0` 只留必挖。
- **scanner 扇出**（`src/orchestrate/mod.rs` `run_scan_fanout`）：大格先发排序后 `join_all`；每格阻塞 `registry.lease()` → `lease.run` → `sink.on_cell(record)`；结果按 plan 序回收。scanner 输出里的 **needs 从未入队**（`Origin::Needs` 只出现在测试里；design §6 把 scanner needs 列为第三种生产者）。
- **门**（`src/provider/registry.rs` `InflightGate`）：自管 `held / cap` + `Notify`；scanner 走阻塞 `enter()`，深挖主循环走非阻塞 `try_enter()`，后者只比 `held < cap`、**不感知等待者**。design §12.3：「门 = 单把 FIFO，不设调度器、不设预留；最长链优先 = scanner 格先于 H0 项入门（t=0 入队序）」。
- **C1 主循环的唤醒源只有两路**：在途格完成 / 通道到达（`select!`）；在途为空且门全满时才阻塞等各成员门位。
- **C1 定案里两条前提会被流式入队拿掉**：
  - 「共识只经谓词」（C1 design §2.2，`merge_into` 注释）的理由是「上游已组内传递闭包，进队列的同文件 scan 项互不满足谓词」。逐格到达后，合并变成队列侧逐条取首个 `same_item` 命中：非传递、依赖到达序（A、C 先到互不匹配，B 后到两边都匹配 → B 只并入 A，A 与 C 永远两项）。
  - 「报告逐字节一致」（C1 design §3.4）的前提是「无生产在途入队 → 派发序 = 静态优先级序」。
- **design §6 规则 2 半实现**：原文「区间扩到 enclosing def；同文件重叠/相邻区间合并为一项；跨模型同疑点以 quote 轻锚合并」，scan / needs 项的 `same_item` 只实现了 quote 锚那一半。
- **C1 收口留给本批的三条**（followup「批 C1 收口带出的账」）：
  - #5 Skipped 经通道并入复位后 `budget_skipped` 不回减 → 假债（与 C1 design §2.3「不回减」裁定冲突，流式入队后即真）。
  - #6 外部持门者放门时主循环不被唤醒（探针：门 2、外部先占 1、4 项 × 100 ms → 300 ms，及时补位约 210 ms）。
  - #11 填门循环里 `region::build` 每项同步起一次 `git ls-files`（读守卫，`src/cli/second_pass.rs`）并读文件，二阶段 `need_section` 同；阻塞期间同 task 其余在途格的流读取与计时器一起停。
- **深挖选项方差**（followup，C1 dogfood 两轮同 diff findings 2 vs 8）：scanner 候选集跨轮只重合 39%；confidence 排序 + 预算截断把漂移变成门；`render_notes` 把 confidence 写进 reviewer 笔记；`cmp_priority` 同分按 id 字典序、`.next/…` 恒排在 `src/…` 前；单文档 27 条疑点各占一项。followup 建议并入本批拷问；验收口径 = 同 diff 两跑的选中区域与 findings 重合度。
- **真实配置**（`d23e715` 定）：scanner = deepseek-flash（deepseek 门 16）+ qwen3.8-flash（qwen 门 20）；reviewer = glm-5.3（agc 门 6，兼 verifier 角色，批 D 前不消费）；grok-4.6 无角色。**scanner 与 reviewer 不同门**——门公平与 #6 在真 dogfood 里触发不到，只能构造；批 D 的 verifier 与 reviewer 同在 agc，#6 对 D 更实。
- **墙钟基线**（目标 diff 同批 C：`f1d3ca3` 状态、`--base 674583e`，budget_cells = 12）：
  - C1 dogfood（混合池 glm + grok）：agc 门 20 总 11.2 min（scanner 段 4.0 / 深挖段 7.1）；门 4 总 19.7 min（3.6 / 16.1）。私有记录 `.next/private/m21b/2026-09-14-batch-c1-dogfood.md`。
  - **现配置补跑**（2026-09-15，HEAD `d23e715` 构建；glm 单成员 · agc 门 6）：总 **20.6 min**（scanner 段 6.2 = qwen g1 374 s / 深挖段 14.3）；
    门 6 仍撞 429 × 2 → 收到 4，12/12 成功、零断流；队列 22 项截断 10；findings 6（格内 7）；12 格里 4 格是 roadmap 文档行，与 C1 ① / ② 选中项各重合 4/12。
    scanner 格完成时刻 58 / 197 / 255 / 374 s。私有记录 `.next/private/m21b/2026-09-15-batch-c1-baseline-current-config.md`（原始报告与重放脚本在同目录 `raw/`）。
- **批 D 碰撞面**：D 未开工（任务 planning、无分支），本批先行。碰撞点 = `run_deep` 的 `sink`（仍 `&()`）与 barrier「复核在途 0」；「流式旁路跨瞬时重试不复位」归 D 接真 sink 前修。
- **退化零差基线** `src/orchestrate/testdata/deep_serial_baseline.json` 钉「单成员门宽 1 的派发形态」：本批有意改变时审过 diff 再更新（C1 design §8）。

## Requirements（拷问前草案；「待裁」由 grill 定）

- **C2-1 流式入队接线**
  - H0 在 t=0 入队并可立即派发；scanner 扇出与深挖并发执行，scan 格完成即把该格疑点经通道入队（入口五态）；scanner 全部返回 = 发送端全部 drop = 通道关闭。
  - scan 报告的疑点并集仍按组 `merge_suspicions` 生成（报告口径不变）。
  - 待裁：队列侧逐条合并的非传递 / 到达序相关是否接受（或到达时桥接合并）；与报告疑点并集的口径差如何披露；共识计数前提的更新。
  - 待裁：scanner needs 是否本批入队。
- **C2-2 在线预算**
  - 「入队完一次性 drain」改为在线：派发计数对预算；必挖不截（含后到才成必挖的项）；疑点保底 ⌈maxCells/4⌉ 为后到疑点预留（H0 / 涟漪不得吃掉）；预算耗尽后到达的非必挖项 → `skipped(budget)`。
  - 待裁：scanner 全完时未用满的保底如何释放；「到期」时点；`budget_skipped` 的披露口径与计债口径拆分（#5）。
- **C2-3 门公平与唤醒**
  - 共享 provider 时 scanner 先入门：`InflightGate` 感知等待者，非阻塞试取在有等待者时让位。
  - 外部持门者放门能唤醒深挖主循环（#6），按通用机制设计（批 D verifier 同门复用）。
  - 待裁：让位规则与 design §12.3「单把 FIFO、不设调度器」的对齐。
- **C2-4 scanner 失败口径**
  - 待裁：reviewer 已开挖或已产出 finding 时 scanner 格失败——深挖是否继续、finding 是否进报告、退出码（现 3 = Incomplete）、coverage 分级。
- **C2-5 墙钟分段与收口**
  - RunMeta 墙钟分段可读（scan 段 / 队列排空 / 复核尾部，复核在批 D 前恒 0）。
  - 待裁：#11 同步 IO 本批修（`spawn_blocking` / 按 bundle 预取 tracked 文件集）还是留。
  - 待裁：深挖选项方差四层里哪些并入本批。

## Acceptance Criteria（草案，grill 后定稿）

- [ ] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify` 全绿
- [ ] C2-1：H0 在 scanner 全完前被派发并完成（虚拟时间可观测）；scan 格完成即入队，后到的同疑点并入在途项只改记录；通道关闭前不收场；scan 报告疑点并集与改动前一致
- [ ] C2-2：必挖从不被截（`maxCells = 0` 时 H0 仍被挖、后到才成必挖的项被挖）；保底格留给后到疑点；截断项 100% 出现在 RunMeta；无假债
- [ ] C2-3：构造「scanner 与 reviewer 同 provider、门宽 < 格数」→ scanner 先入门可观测；外部放门后派发及时补位（#6 探针转绿）
- [ ] C2-4：按 grill 定的口径，覆盖「scanner 失败 × reviewer 已产出 finding」的报告 / 退出码 / coverage 用例
- [ ] 并发 kill → resume：多项 InFlight、部分格已落盘时 finding 不丢不双计（followup 用例缺口）
- [ ] dogfood 同目标 diff 对照现配置 C1 基线：墙钟分段可读；记入 `.next/private/m21b/`
- [ ] 收口：code-review 审核 → 修 → `--no-ff` 合回（M21 期间不跑 ship 评审）

## Out of Scope

- 批 D：verifier 流式段、真 sink、barrier 的复核在途计数、旁路跨瞬时重试复位
- 台账与增量轮（M21c）；三方契约注入（M21d）；第二家族 scanner（M21e）
- 同组 hold（design §6 规则 5：由派发时 / 落地时 agreement 数据在 M21e 定）
- 429 棘轮公式、断流视同过载收门（followup 候选，另议）
- 模型权重 / 候补 / failover（D7 A3）

## 开工前 grill 议程

1. 在线预算：保底预留的对象与释放时点；预算「到期」的定义；H0 / 涟漪 / 疑点 / needs 在在线口径下的配额关系
2. `budget_skipped`：披露口径（截断时刻）与计债口径（收场时仍未挖）拆分（#5）
3. scanner 失败口径（C2-4）
4. 门公平：等待者让位 vs design §12.3「不设调度器」；外部放门唤醒源（#6）；是否一并服务批 D 同门
5. 流式合并语义：非传递与到达序相关、共识前提（C1 §2.2）、队列项与报告疑点并集的口径差
6. 报告确定性（C1 §3.4）在流式入队后的口径
7. 深挖选项方差四层：confidence 排序键与笔记、design §6 规则 2 相邻区间合并（及是否计共识）、文件多样性与同分 tie-break、scanner samples ≥ 2——本批并入哪些、验收口径
8. scanner needs 入队（design §6 第三种生产者，批 C 未接、账里无记录）
9. 同步 IO（#11）：本批修还是留
10. 验收构造：同门场景、墙钟分段字段、多 InFlight kill/resume

## Technical Notes

- 分支 `feat/m21b-deep-streaming`，工作树 `~/hobby/.worktrees/redteam/m21b-deep-streaming`（自 `feat/m21-review-engine` 切出）。
- `.next/private/` 不随新工作树：C1 及更早的私有记录从集成工作树读；本批私有记录写在本工作树 `.next/private/m21b/`，收口时拷回集成工作树（C1 先例）。
- 实现形态（scanner 扇出与 `run_deep` 同 task 并发、还是 spawn 后 registry `Arc` 化）留 design 定；C1 的同 task `FuturesUnordered` 借用形态不要求 `Send` / `'static`。
