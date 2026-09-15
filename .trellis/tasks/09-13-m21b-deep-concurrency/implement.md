# 批 C1 实施清单

> 顺序 C1-1 → C1-5。每 task 一 commit，简体中文 `type(scope): 摘要——细节`，绝不 push。
> 工作树 `~/hobby/.worktrees/redteam/m21b-deep-concurrency`，分支 `feat/m21b-deep-concurrency`（自 `feat/m21-review-engine` 切出）。
> **C1-1 必须先落**：id 不定死就开并发，在途并入会让 `mark_settled` 找不到项、barrier 永不满足。
> 定案见 design §9（两轮裁定 + `/grilling` Q1–Q10）。

## Checklist

### C1-1 身份与 resume 完整性
- [x] `queue.rs`：`merge_into` 删除 `it.id = item_id(&it.spec)`；id 只在 `new` 时算
- [x] `queue.rs`：`find_merge_idx` 返回合并原因（谓词 / id 捷径，**谓词命中优先**）；`merge_into` 只在谓词合并时并入家族、重算 agreement，id 捷径合并只并笔记与区间
- [x] `queue.rs`：`EnqueueOutcome::MergedSkipped`；Skipped 并入后 `is_must` → Pending（重置 `enqueued_ms`）+ `MergedPending`，否则保持 + `MergedSkipped`
- [x] `deep.rs`：`note_enq` 把 `MergedSkipped` 计入 `dedup_hits`；reviewer cell_id 改 `review[{item.id}]@{rev.name}`
- [x] `cli/mod.rs` `run_resume`：restore 前对 Settled 项查 `review[{item.id}]@` 前缀的**成功** cell，缺失 → Pending + warning；注释写明「kill 窗口内换模型重跑留两条 cell」的接受口径
- [x] 测试：在途并入后派发时 id 仍命中；id 对 ranges 扩展稳定；id 捷径合并不抬 agreement / 谓词合并照抬；谓词优先于 id 捷径；Skipped 异族并入复位、同家族 `MergedSkipped`；**批 C 覆盖路径 resume 两次不丢 finding**（先红后绿）；快照 Settled 无成功格 → 重挖（先红后绿；「只认成功格」与「谓词优先」经变异检查确认非空转）
- [x] Commit: `fix(queue,resume): 队列项与 cell 身份定死——id 不随 ranges 重算、共识只经谓词、Skipped 仅成必挖才复位、cell_id 按队列项且 Settled 以成功格为准（批 C1 前置，修批 C resume 覆盖丢 finding）`

### C1-2 并发派发循环与二阶段契约
- [x] `Cargo.toml`：dev-dependency `tokio = { version = "1", features = ["test-util", "macros", "rt"] }`（虚拟时间）
- [x] `registry.rs`：`InflightGate::try_enter` + `ProviderRegistry::try_lease`（尊重 429 收门后的有效 cap；scanner 路径零 diff）
- [x] `deep.rs`：`DeepInput.incoming: Option<mpsc::Receiver<EnqueueReq>>`（生产 `None`）；到达项走五态 + 快照 + 入队原因 + `queue_depth_peak` 取运行中最大值；C1 里不过预算（注释写明归 C2）
- [x] `deep.rs`：主循环 `FuturesUnordered` + `select!`（完成 / 到达）；barrier = 在途 0 ∧ Pending 0 ∧ 通道关闭；空转兜底阻塞 lease
- [x] `deep.rs`：`run_cell` future = 首轮 + 二阶段，**全程持同一 lease**；第二轮新造 per-cell backend + inc、复用同一 policy；两轮结束统一回读重试 / 等待；`second_stage_round` 的 `&mut warnings` 改本地带回
- [x] `deep.rs`：二阶段契约——triggered 只计真发出；全被拒 → skipped 不计债；第二轮失败或建 pack 内部错误 → 保留首轮 finding + warning + 计债；第二轮烧掉的 token 入账
- [x] `deep.rs`：`on_done` 在完成时回填 landed / `snap_cell` / `mark_settled` / `snap_one`；结果按派发 `seq` 暂存，收场按 seq 组装 findings → dedup、cells、meta 四数组（失败格也各一条）
- [x] `review_shot.rs`：`FindingEvent.suspicion_agreement` 注释补 `event ≤ landed` 不变式与安全方向（仅注释）
- [x] 测试：在途峰值 = 门宽；乱序不错配；慢格不堵；二阶段持门位（门宽 1 时第二格等首格两轮结束）；429 注入收敛无死锁；**通道并入分叉**（落盘 = 落地 = `landed_agreement`，事件严格 <）；通道未关不收场；**打乱完成时延两次运行报告逐字节一致**（计时字段 `duration_ms` / `queue_wait_ms` / `item_wait_ms` 置零后比较）；meta 四数组等长对齐；二阶段失败 → 首轮 finding + exit 2 + token 入账；全被拒 → skipped + exit 0
- [x] 变异检查：串行派发 / 按完成序组装 / 删落地回填 / 忽略通道 / 二阶段失败不计债 / 失败格缺 meta 位 / 二阶段门宽放宽——各自对应用例均转红
- [x] Commit: `feat(orchestrate): 门驱动并发派发——FuturesUnordered + 入队通道、一格持门位到底、报告按派发序组装、二阶段失败计债（design §6 规则 5〔D13〕）`

### C1-3 分派池轮转与成员健康
- [x] `deep.rs`：`pool` + 游标轮转 `next_lease`；删除 `reviewers.first()` 单点；每项只交一个成员
- [x] `deep.rs`：错误穿透 `attach_usage` 判 `Auth` → 移出池 + warning；已失败格不改派；其余错误类不踢；池空 → 停派、通道未关继续收、剩余 Pending + warning 报数 + 计债
- [x] `cli/mod.rs`：`RunMeta.models` 含实际参与 reviewer；配了但 0 格成员 warning
- [x] `main.rs`：兼任 scanner 的 reviewer 被 `has_scan` 跳过注册时仍 push `thinking_warning`（抽成 `backends::role_thinking_warnings` 纯函数：`build_registry` 读真实密钥不宜直测；文案带角色名）
- [x] 测试：两成员不同 provider 门宽各 1 → 两格并行分落不同模型；一成员 Auth（队列 > 容量）→ 移出、后续全落另一成员；唯一成员 Auth → 剩余 Pending + exit 2；0 格成员 warning；兼任 warning 出现在 `RunMeta.warnings`；单成员行为不变
- [x] 补测：同 provider 共享门时两成员交替领项；二阶段第二轮 401 同样出局；未注册成员就地出局不挡其余成员、全员未注册 = 池空计债
- [x] 变异检查：不踢 / Auth 不穿透包装 / 忽略第二轮 Auth / 游标不前移 / 丢 0 格披露 / 池空不计债 / 空队列也报 0 格——各自对应用例均转红
- [x] Commit: `feat(orchestrate): reviewer 分派池空闲轮转与成员健康——鉴权失败移出池不改派、池空停派保留 Pending、未分到格不再静默（design §6 规则 5、D7 A3）`

### C1-4 墙钟门与退化零差
- [x] 虚拟时间墙钟门：K=12 × 50 ms、门宽和 W=4 → 虚拟总时长 ≈ 3 × 50 ms（容差放宽）——单成员门宽 4 与跨 provider 两成员 2 + 2 两种形态都测，后者两门各吃 6 格
- [x] 确认 `item_wait_ms` 并发下为队列等待（派发时定格）：门宽 2、4 格 × 100 ms（真实时间——入队时刻取系统时钟，虚拟时间推不动）→ 先派两格 < 60 ms、后两格 ≥ 90 ms；`DeepMeta` 字段集不变（`git diff eac0a60 -- src/domain/report.rs` 为空）
- [x] 退化零差：单 reviewer + 门宽 1 与批 C 串行报告同构（cell 序、findings、meta）——基线由批 C 代码（`git archive eac0a60` 导出到 scratchpad、跑同一场景）采得，存 `src/orchestrate/testdata/deep_serial_baseline.json`；比对面含 findings / cells / meta / warnings / 债 / 落盘序 / 队列快照序，归一化计时字段与模板哈希，cell 名按派发序映射回批 C 形态
- [x] 变异检查：丢第二轮终判 / 去派发快照 / 丢 prior findings（基线比对转红）；完成时刻采样 wait（wait 用例转红）；串行派发（墙钟门与 wait 用例转红）
- [x] Commit: `test(deep): 并发墙钟门（虚拟时间）与单成员退化零差——对照批 C 串行基线`

### C1-5 收口
- [ ] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify`
- [x] `make build` → dogfood 两轮（base `674583e`，目标与批 C 同为 `f1d3ca3`，临时 detached 工作树）：① 11.2 min（深挖段 7.1 min，429 × 2 门 20→7，grok 2 格断流）② 19.7 min（门 4，全绿）；配置已恢复并 cmp 校验；记入 `.next/private/m21b/2026-09-14-batch-c1-dogfood.md`，账目进 followup
  - ① 当前配置（reviewers = glm-5.3 + grok-4.6，同在 agc 门宽 20）——墙钟对照串行基线 18.3 / 50.7 min；统计每模型每格产出（grok 去留的决策数据）
  - ② 争门轮：跑前 `cp ~/.redteam/config.json ~/.redteam/config.json.bak-<ts>`，`providers.agc.maxConcurrency = 4`，跑完用备份恢复并校验
  - 记入 `.next/private/m21b/`
- [ ] 三路 code-review 或 dogfood 自评审 → 修 → `--no-ff` 合回 `feat/m21-review-engine`
  - [x] `/code-review`（15 条）→ 分四档；第 1 档已修：#1 id 捷径保请求自身共识（`4e70292`）、#2 二阶段失败按失败格落盘 + #4 对账双向以成功格为准 + 重挖失败保载体（`96b1ad5`）、#3 diff 重建不了时截断数不丢
  - [x] 第 2 档：#9 `--help` 退出码 2 覆盖二阶段失败 / 池空未挖、补多 reviewer 轮转与鉴权出局，债 warning 文案同步；#7 「re-digging」只在真会重挖时披露；
    #8 二阶段第二轮失败轮次记下限 +1；#10 真实时间用例时延 300 ms、阈值 150 / 280 ms；reviewer cell warn 补丢弃计数（parse / needs / 锚定 / 旁路，与 scanner 同口径），二阶段成功时统计取终判轮
  - [ ] 第 3 档（#12 cell 名 helper、#13 删 dispatch 死代码、#14 Pool 冗余状态、#15 registry 复用，可选）
  - [ ] 留 C2 记账：#5 复位后 budget_skipped 不回减、#6 外部放门不唤醒、#11 单 task 内同步 IO 停摆
- [ ] followup：「批 C 核查留债」前两条与 resume 覆盖 bug 标已解；与批 D 的 sink 接线现状（若 D 先合回则对齐）
- [ ] Commit: 评审修（如有）+ 归档 / journal

## 验证命令

```bash
cargo test
cargo clippy --all-targets        # 基线 7
make verify
# 在途峰值：assert registry.gate_stats()["p"].peak == 门宽
# dogfood：make build && ./plugins/redteam/bin/darwin/aarch64/redteam-next review --base 674583e --format json
```

## 风险文件与回滚

- 高风险：`deep.rs` 主循环（future 边界划错 → 共享可变状态跨 await；`select!` 分支条件写错 → 通道未关提前收场或死等）；`queue.rs` 合并原因拆分（agreement 口径）；`run_resume` Settled 校验（误判 → 大面积重挖）。
- `registry.rs` 只加不改；scanner 路径零 diff。
- 回滚 = 不合回；合回后 revert 该 merge。退化零差用例是第一道回归哨。

## start 前核对

- [x] 批 C 已合回 HEAD（`eac0a60`）并归档（`5f53262`）
- [x] 两轮裁定 + `/grilling` Q1–Q10 已落 design §9
- [x] prd.md / design.md / implement.md 已按定案重写
- [x] `implement.jsonl` / `check.jsonl` 各 ≥1 条真条目
