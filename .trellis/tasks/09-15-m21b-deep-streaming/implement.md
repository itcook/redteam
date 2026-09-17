# 批 C2 实施清单

> 顺序 C2-1 → C2-6。每 task 一 commit，简体中文 `type(scope): 摘要——细节`，绝不 push。
> 工作树 `~/hobby/.worktrees/redteam/m21b-deep-streaming`，分支 `feat/m21b-deep-streaming`（自 `feat/m21-review-engine` 切出）。
> **C2-1 必须先落**：到达键 / 按可派条件弹出 / `drain_with` / 门的等门者计数是 C2-3、C2-4、C2-5 的共同地基；零件先用单测钉住语义再接线。
> 定案见 design §13（`/grilling` Q1–Q10）与 decisions D15；选项规则与 scanner needs 入队不在本批（批 C3）。

## Checklist

### C2-1 队列、门与闭包零件（不接线）
- [x] `queue.rs`：`EnqueueReq.arrival: Option<String>`、`QueueItem.arrivals`（serde default、空不序列化）；`enqueue` 先查到达键 → `EnqueueOutcome::Duplicate`（不改项、不计 `dedup_hits`）；新建 / 并入回报受影响项（`enqueue_affected`）
- [x] `queue.rs`：`pop_dispatch_where(pred)` / `has_eligible(pred)`（优先级序不变）；`drain_with(budget, floor)`，`drain(K)` = `drain_with(K, ⌈K/4⌉)` 且现有 drain 用例零改动通过
- [x] `queue.rs`：`merge_into` 注释前提改写（逐 cell 到达、非传递残差、id 捷径不计共识）；Skipped 复位分支注释「首跑生产不可达」；优先级比较对 deep 可见（`pub(crate)`）
- [x] `registry.rs`：`GateState.waiters` + `WaiterGuard`（Drop −1）；`try_enter` = `held < cap ∧ waiters == 0`；`enter` 首试让位、登记后 `try_enter_as_waiter`；`try_lease` 随之让位
  （实施细化为票号 FIFO：只有队头可取、放门 / 连锁 / 取消队头用 `notify_waiters`——`notify_one` 加登记即检有插队窗口，见 design §7.1）
- [x] `consolidate/scan.rs`：`closure_members(cells)`（同一并查集，输出成员到达键）；`merge_suspicions` 在其上组装，输出逐字节不变
- [x] 测试：到达键幂等（重复到达 0 改动）；受影响项回报；`pop_dispatch_where` 跳过不可派项取次高；`drain_with` 与旧 `drain` 在 C1 用例上逐项一致、主区吃穿时帽 = 主区 + 保底；
  门首试在有等门者时让位、登记者被唤醒可取、等门者 FIFO；**取消安全**（被唤醒未取到即 drop → 下一位仍被唤醒、`waiters` 归零；tokio 不转交则 `WaiterGuard` 补 `notify_one`）；429 等待者不受影响；
  `closure_members` 与 `merge_suspicions` 同簇
- [x] 变异检查：`try_enter` 不看 `waiters` / `WaiterGuard` 不回退 / `drain_with` floor 不扣 / 幂等键不查——各自用例转红
  （另加票号 FIFO 四处：不查队头 / 放门 `notify_one` / 取后不连锁 / 取消队头不唤醒——首轮后三处假绿，补逐 future waker 标记用例后转红）
- [x] Commit: `feat(queue,registry): C2 零件——到达键幂等入队与受影响项回报、按可派条件弹出、drain_with(预算, 保底)、门等门者计数与首试让位（取消安全）、组内闭包成员导出（D15）`

### C2-2 读守卫预取（Q9 IO2）
- [x] `second_pass.rs`：`TrackedFiles::load(root)`（一次 `git ls-files -z`）+ `need_read_allowed_in(&tracked, root, rel)`；原 `need_read_allowed` 保留给其他调用点
  （实施：`need_read_allowed_in(&tracked, rel)`；原函数改为现取集合的便捷入口、唯一实现，见 design §9）
- [x] `region.rs` / 二阶段 `need_section`：接收 `&TrackedFiles`；`run_deep` 开跑时建一次
- [x] 测试：未跟踪 / denylist / 符号链接逃逸与改动前同样拒绝（沿用 region 与 second_pass 既有用例 + 集合版各一）；`run_deep` 全程只建一次集合
  （等价对照：原子进程实现 × 33 路径 × 三种 root 逐条同结果；变异：去目录前缀 / 尾斜杠不按目录 / `..` 不回退 / 逐项取集合——各自转红）
- [x] Commit: `perf(pack,deep): 读守卫改查每轮预取的 git 跟踪文件集——逐项子进程去掉，拒绝口径不变（D15 Q9）`

### C2-3 深挖主循环：在线额度、收场 drain、等门挂起、组装序、观测
- [x] `deep.rs`：`eligible = must ∨ closed ∨ dispatched < K − F`；填门只在 `has_eligible` 时取门位；派发时 pack 失败结案也计 `dispatched`
- [x] `deep.rs`：`on_close` 只执行一次（通道收到 None；`incoming: None` 开跑即关闭）→ `drain_with(K − dispatched, F − 已派发非必挖疑点)`，写 Skipped 项快照
- [x] `deep.rs`：等门挂起——有可派项而各门无位时为活跃成员建阻塞 lease，跨事件保持、可派项消失即撤销；命中 → `parked`；取代 C1「在途为空才阻塞 lease」
- [x] `deep.rs`：`on_arrival` 只写受影响项快照；H0 入队逐项写（scan 格回放 `enqueue_scan_cells` 同形逐条入队、逐项写，取代 `merge_suspicions` 预合并 + 全量快照）
- [x] `deep.rs`：收场按最终项 `cmp_priority` 排序 `landed` 后组装 findings / cells / meta 四数组 / 每格 warnings；保证注释改写（`Landed` 带项 id，不反解 cell 名）
- [x] `deep.rs`：restore 模式参数——`scan_cells` 幂等入队、`reopen_skipped`、`dispatched` / 非必挖疑点初值取 Settled 项；`DeepRun.items` 带回最终队列项
  （实施细化：初值取「往轮派发过」= Settled ∪ 快照 InFlight，往轮在途项不参与收场 drain（`DeepQueue::drain_among`）；cli 对账「Settled 无成功格」改记 InFlight——
  必挖顶出 K 时只数 Settled 会截掉往轮在途项，见 design §6）
- [x] `report.rs`：`DeepMeta` 增 `close_ms` / `first_dispatch_ms` / `drain_done_ms` / `verify_tail_ms` / `scan_phase_dispatched` / `close_budget_remaining` / `queue_split_clusters` / `fill_block_max_ms` / `fill_block_total_ms`（serde default）；填门段与到达处理段计时
- [x] 测试（虚拟时间，经 `incoming` 通道喂到达）：扫描期额度（宽门 K = 8 首批 10 条非必挖 → 收场前 ≤ 6）；必挖不截（`maxCells = 0` + 后到抬 agreement 2）；Skipped 只在收场、`budget_skipped` 无回减；
  外部持门者放门及时补位（#6 探针：≈ 210 ms）；共门不插队（测试内以 `registry.lease` 模拟等门 scanner）；报告确定性（固定选中集合、打乱时延两次逐字节一致）；
  快照写入计数（每次到达 ≤ 1 项、重复到达 0 项）；新观测字段可读；**退化零差基线不变**
  （另：等门跨到达事件保持排队位；resume 往轮在途占位 run_deep / cli 各一；restore 幂等回放 scan 格与复位 Skipped；`DeepMeta` 旧 JSON 兼容。
  报告确定性改为「到达序与时延都反过来、门 1 → 派发序不同」；全量并行首跑抓到到达 / 关闭与格完成同一虚拟时刻时 `select!` 随机分支致收场读数抖动 → 用例错开时刻、保证除外项加收场读数。
  退化基线：新字段先断言确定性读数再归零，基线文件不动）
- [x] 变异检查：额度不扣 F / 收场 drain 执行两次 / waits 每事件重建丢排队位 / 按派发序组装 / 到达全量写快照——各自用例转红
  （另六处：退回 C1「在途为空才等门」/ 往轮在途不计入已派发 / 往轮在途参与 drain / 对账改回 Pending / `reopen_skipped` 不生效 / `scan_cells` 不入队。
  首轮「往轮在途不计入已派发」假绿——用例里 Pending 全是往轮在途、drain 无可截对象 → 补一条从未派发的涟漪后转红；「按派发序组装」以去掉排序（完成序）代替）
- [x] Commit: `feat(deep): 在线预算与收场 drain、等门挂起入 FIFO（修外部放门不唤醒）、收场按优先级序组装、快照只写受影响项——C2 深挖主循环（design §6 规则 3 / 6、§12.3〔D15〕）`

### C2-4 首跑流式接线与 scanner 失败口径
- [x] `orchestrate/mod.rs`：`run_scan_fanout` 加 `Option<&mpsc::Sender<EnqueueReq>>`；格落盘后逐条发锚定后疑点（到达键 `{cell_id}#{i}`），发送失败忽略
- [x] `cli/mod.rs`：配 reviewer 时建通道；扫描 future 末尾打 `Deep` 并 drop 发送端；`futures::poll!` 先 poll 扫描一次再 `join!`；scan 报告照旧由 `finish_scan_report` 出
  （首次 poll 即完成时直接 `deep.await`）
- [x] `cli/mod.rs`：删 `scan_failed` 早退；`overlay_deep` 在 scan Incomplete 时保留 verdict / exit 3、并入 findings 与观测、blocking 数 warning、带债 warning 去掉「approve with exit 2」
  （判 `report.verdict == Incomplete`；`verdict::blocking_count` 与 `compute_verdict` 同谓词）
- [x] `cli/mod.rs`：`split_clusters(&results.cells, &deep.items)` 写 `queue_split_clusters`（函数放 `consolidate::scan`）
- [x] `main.rs`：`--help` 退出码 3 补「深挖照跑，findings 仍在报告」；review 总述补「scanner 与深挖时间重叠」
- [x] 测试（Fake e2e）：scanner 格慢、H0 快 → H0 在 scanner 返回前完成；共门 cap 1 时 t=0 scanner 先入门；scanner 一格失败 + reviewer 产 blocking → Incomplete / exit 3 + findings + warning；
  A、C、B 到达序构造 → `queue_split_clusters == 1` 且 scan 报告疑点并集不变；phase 时间线 Collecting → Scan → Deep → Finalizing；无 reviewer 配置 scan-only 行为不变
  （虚拟时间精确断言：H0 格 < 1000 ms 落地、scanner 1000、疑点格 1010，`first_dispatch_ms` / `close_ms` / `drain_done_ms` = 1 / 1000 / 1010，阶段 Scan@0 → Deep@1000 → Finalizing@1010；
  H0 夹具须过门槛——敏感路径 + 签名变更 + 含控制流的 git 仓。另：扇出单测两条（只发锚定后疑点 / 失败格不发 / 接收端关闭忽略；落盘先于入队）、
  `split_clusters` 单测、`blocking_count` 单测、带债 + scanner 失败仍 3 且不提 exit 2、作业级阶段时间线（`run_job`））
- [x] 变异检查（8 处，全部转红）：去掉显式先 poll（深挖先 poll）/ 首次 poll 即完成时丢深挖 / `Deep` 打在扫描前 / 扫描不入队 / 去掉 F3 分支 / 不写拆簇数 /
  先发后落盘 / 拆簇阈值 ≥ 1。首轮「先发后落盘」假绿（接收端在外部 poll 看不出先后）→ 改在落盘钩子里 `try_recv` 查后转红
- [x] Commit: `feat(cli,orchestrate): scanner 与深挖时间重叠接线——格完成即入队、显式先 poll 扫描、收场切 Deep、scanner 失败深挖照跑仍硬 Incomplete、拆簇对账（design §2 / §8.4〔D15〕）`

### C2-5 resume 离线续跑（Q6 RS1）
- [ ] 先写红用例：配 reviewer、队列快照为空、scanner 部分落盘的作业 resume → 现状出 scan-only / exit 0
- [ ] `cli/mod.rs` `run_resume`：去掉 `still_missing == 0` 与 `!queue_snap.is_empty()` 两前提；`replayed` = 补跑前有缺失格；空快照时 `stage0_risk` 重算 route；
  `run_deep` restore 模式传全部成功 scanner 记录（幂等入队）+ `reopen_skipped: replayed`；scanner 仍缺失 → verdict Incomplete（F3 口径）
- [ ] 测试：① 扫描期 kill（H0 在途 + 部分 scanner 落盘）→ 补跑 → 入队 → drain → 挖、finding 不丢不双计；② 首跑 F3 带 Skipped 收场 → 补跑后 Skipped 复位重选；③ 空快照（上面的红用例转绿）；
  ④ 多项 InFlight 并发 kill、部分格已落盘 → 不丢不双计；深挖期 kill、无需补跑 → Skipped 沿用、Pending 集合不缩；旧快照（无到达键）可 resume
- [ ] Commit: `fix(resume): 离线续跑——补跑 scanner 后疑点幂等入队、补跑过则复位 Skipped 重选、空快照同路径重建 H0（修扫描期 kill 后 resume 出 scan-only / exit 0）（design §17 R14〔D15〕）`

### C2-6 收口
- [ ] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify`
- [ ] `make build` → dogfood 一轮：临时 detached 工作树检出 `f1d3ca3`、`redteam-next review --base 674583e --format json`；配置与基线一致（agc 门 6，跑前跑后 sha256 校验）
  - 用 `.next/private/m21b/raw/analyze_report.py` 汇总、`replay_policies.py` 对照；比基线 20.6 min 与重放预测（门 6 13.7 / 门 4 16.8 min）、各段墙钟、`scan_phase_dispatched`、429 / 断流
  - 记入本工作树 `.next/private/m21b/`；账目进 followup
- [ ] code-review 审核（技能只传 effort，起来后 SendMessage 钉范围：只评 `feat/m21-review-engine...HEAD`）→ 分档修 → `--no-ff` 合回 `feat/m21-review-engine`
- [ ] 本批私有记录（`2026-09-15-batch-c1-baseline-current-config.md`、`raw/`、C2 dogfood 记录）拷回集成工作树 `.next/private/m21b/`
- [ ] 归档任务 + journal

## 验证命令

```bash
cargo test
cargo clippy --all-targets        # 基线 7
make verify
# 共门构造：scanner 与 reviewer 条目路由同一 provider，gate_stats 峰值与入门先后可断言
# dogfood：make build && <临时工作树> ./…/redteam-next review --base 674583e --format json
```

## 风险文件与回滚

- 高风险：`deep.rs` 主循环（额度与收场 drain 的一次性、等门挂起的撤销时机——撤销晚了持门空转、撤销早了丢排队位；`select!` 分支条件）；
  `registry.rs` 等门者计数（取消路径漏减 → 深挖永久让位 = 饿死；首试让位误伤 scanner-only 路径）；`run_resume` 前提放宽（误入深挖路径 → 旧作业行为变化）。
- 退化零差基线与 C1 全部 deep / registry / resume 用例是第一道回归哨；registry 改动须证明 scanner-only 路径行为不变（既有 gate / 429 / e2e 用例）。
- 回滚 = 不合回；合回后 revert 该 merge。

## start 前核对

- [x] 批 C1 已合回（`80919ad`）并归档（`7d8a06e`）
- [x] C1 现配置基线已补跑并记账（`0ee99ad`；私有明细 `.next/private/m21b/2026-09-15-batch-c1-baseline-current-config.md`）
- [x] `/grilling` Q1–Q10 定案已落 design §13 与 decisions D15；design 升 v11
- [x] prd.md / design.md / implement.md 已按定案写
- [x] `implement.jsonl` / `check.jsonl` 各 ≥ 1 条真条目
