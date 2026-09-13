# M21b · 风险路由与深挖队列

> 状态: ⏳（plan `../plans/2026-09-10-redteam-m21b-risk-deep-queue-plan.md`：**六批 A / B / C / C1 / C2 / D + 出口**——2026-09-10 派生时为四批 A-D〔原五批含 aacr 接线批，同日删〕，2026-09-12 增批 C1 承接交付项 2/3 的门驱动并发与分派池轮转，2026-09-13 C1 拷问定案增批 C2 承接 design §2 的 scanner 与深挖时间重叠）· 依赖 M21a ✅ · design §3.3 / §6 / §7 / §8.1 / §12.3 / §13 · 决策 D1 勘误, D2, D6, D7, D13
> 出口：`redteam-next` 端到端产 finding 并流式复核；插件与 ship 仍用 dev `redteam`（切名在 M21c）

## 目标

finding 只由深挖产出。规则热区与 scanner 疑点进同一条优先级队列，reviewer 分派池按预算并行消费；finding 落地即由
verifier 流式复核；墙钟由 provider 在途门与预算共同封顶（无角色 worker 池〔D13〕）。无候补。

## 交付项

| # | 项 | design | 落点 |
|---|---|---|---|
| 1 | 风险评分器：路径/类别、结构、外部 API、历史四类信号 → H0 + R；无模式旋钮〔D11〕；规则集 = P0 v4 定案（涟漪二梯队、两类信号、常量化），`eval/risk_replay.py` 回放为回归门；可回流；历史信号台账经 `--dismissed` 指定（缺省读 `proj-next.db` 自身，见 followup 分库裁定），**台账口径（2026-09-10 批 A 核实，P0 §15.1）**：PR181 回放不带台账（带则被测作业 upheld 泄漏）；dev 旧 `proj.db` + `--before` 守卫是 oracle B 侧口径，两者与 P0 数字逐位一致 | §3.3 | `src/risk/`（新模块）, `src/cli` |
| 2 | 深挖优先级队列：键 = H0 最高 / `置信 × agreement × 严重度`；入队合并 = **入口五态**（无命中 / 排队中并入 / 在途只改记录 / 已结案沿用 / 跨轮判例守卫；谓词复用 `merge_suspicions`；design §6 规则 2〔D13〕）；agreement 活值（排队项并入即重算优先级与必挖资格）；预算 `review.reviewers.maxCells = "auto"` = `f(R, 规模)` + **必挖集不截**；graceful drain；终止条件；**分派池**（每项一个 reviewer；派发由 provider 门空位驱动，无角色 `workers`；scan 项不等同组兄弟 cell〔D13〕） | §6 | `src/orchestrate/queue.rs`（新）：tokio mpsc + 门驱动派发器 |
| 3 | provider 在途门接 reviewer / verifier：三角色同门（M21a 收口批已建 `InflightGate` 429 排队重试与 RunMeta 字段）；t=0 入队序 scanner 先于 H0；每角色在途峰值、门等待时长可读 | §12.3〔D13〕 | `src/provider/registry.rs`, `src/orchestrate` |
| 4 | reviewer cell：区域 pack（由构造即切片 + 爆炸半径 + 跨组索引 + spec）；H0 无笔记 / 疑点带笔记；输出完整 finding；终端不再生产；**流式持久化**（SSE 增量 JSON 解析，`findings` 对象闭合即落盘，无 `submit` 旋钮，D10）；**`templates/reviewer.md` 角色尾**（「确认、驳回或补充」、笔记非事实、H0「独立评审此区域」措辞）+ 模板哈希 | §7, §10.7 | `src/pack/mod.rs`, `src/prompt/templates/reviewer.md`, `src/orchestrate` |
| 5 | **verifier 流式段**：finding 落地即入复核流；触发集 blocking + 判例变体；跳过 = 两异族 scanner 标记且 reviewer 确认；verifier 选择（verifiers 活跃集〔拥有 `verifier` 角色的模型，D12 JSON〕或自动异族，优先 reviewers 档，无异族 → 同族 + warning）；`review.verifiers.enabled`（触发集消费 `--block-threshold` / `--conf-floor`，M21a 无消费者旗）；**`templates/verifier.md` 角色尾**（`analysis` 先于 verdict、「找出不成立的证据」）；refute 率入 RunMeta；**verifier 离线回归集**：proj.db dismissed（假阳）vs upheld（真）二分类准确率，改模板必跑 | §8.1, §10.7 | `src/consolidate/verify.rs` 改为流消费者, `src/prompt/templates/verifier.md`, `eval/`（回归脚本） |
| 6 | 三标配：reviewer pack 注入互斥（笔记 vs needs 证据段）、证据段预算帽、读守卫（复用 `second_pass.rs`） | 铁律 4 | `src/cli/second_pass.rs` 机器复用 |
| 7 | 预算外去向（M21c 台账前的过渡）：截断项标 `deep: skipped(budget)` 进 RunMeta 披露，不静默 | §6 规则 4 | RunMeta |
| 8 | resume 覆盖队列状态（R14 第二刀）：checkpoint 队列快照；Phase 枚举加 `Deep` / `Verify`（`Scan` 已在 M21a F-5；旧 `Fanout` / `Refute` 保留可读，serde lowercase 向后兼容） | — | `src/state/mod.rs`, `src/cli/mod.rs` resume |
| 9 | barrier 语义：scanner 全完 ∧ 队列空 ∧ 在途 0 ∧ 复核在途 0 → reduce（dedup → 台账 → 报告） | §2 | `src/orchestrate` |
| 10 | 观测：队列深度、入队原因分布（rule / scan / needs）、去重命中、预算截断数、项等待时长、每 cell 输出 token、复核触发数 / 跳过数 / refute 率、per-provider 每角色在途峰值、门等待时长、429 收敛门宽、派发时 / 落地时 agreement〔D13〕 | §6, §8.1 | RunMeta / phase_history |

## 不做

台账与增量轮激活（M21c）· 三方契约注入（M21d，reviewer pack 预留槽位）· 默认翻转（M21c）· 候补 / failover（不做）·
aacr 接线（`reviewers/redteam.py` 等随重启点 M21c smoke 一并做，D7 B4：aacr 暂退役、重启节点待定）。

## 验收

- PR181：findings oracle 命中 ≥ GLM 单发 6 条；墙钟分段可读——scan 段对齐 M21a 实测（大格先发 189-251 s）+ 深挖 ceil(K / reviewer 门宽和) + 复核尾部 ≤ 1 min；总账参考 design §13（低 5-8 / 高 7-10 min），超标先看最长格与门宽（M21a 出口裁决：不硬卡单点门槛）。
- 对照（R6 销案〔D11〕）：dev 旧二进制与 `redteam-next` 并排跑 PR181，比 P0 oracle 22 条的命中数；预算截断数 / 台账落入数 / 保底格使用数入 RunMeta，供三档起步值回校。
- 预算截断项 100% 出现在 RunMeta；必挖集从不被截（构造用例：`maxCells = 0` 时 H0 仍被挖）。
- verifier：blocking finding 全部有复核记录或跳过理由；构造「scanners / reviewers 全 deepseek」配置 → 同族复核 + 「无异族
  verifier」warning；refute 率在 RunMeta 可读；复核尾部对总墙钟贡献 ≤ 1 min。
- provider 在途门：`maxConcurrency = 1` 时同 provider 请求串行可观测；构造 reviewer 与 scanner 同 provider 且门宽 < cell 数 → 入队序可观测（scanner 先）；PR181 全程无 429 / 503，或有 429 时门宽收敛、cell 零失败（D13）。
- 后台 job 中途 kill → resume 从队列快照续跑，已落盘 finding（流式持久化）不丢、不双计；未闭合对象丢弃且计入 warning。
- 每个 reviewer cell 输出 < `maxOutputTokens` 的 1/2（有界性验证，FINALIZE / REPAIR 三层不触发）。
- `reviewers/redteam.py` 等 aacr 接线不在本里程碑（D7 B4 暂退役，随 M21c smoke 重启点一并做）。
- verifier 离线回归集：对 proj.db 历史 dismissed / upheld findings 的二分类准确率有基线数字并入 research；reviewer 回归 =
  PR181 上 H0 / 疑点 → finding 确认率与其后 refute 率可读。

## 风险

- 慢模型 per-turn 死亡只丢未闭合的 finding 对象（流式持久化兜底），不再有旋钮可调；死亡率高时调 `perTurnTimeoutSecs`。
- 预算函数形状依赖 E1；先三档常数起步，随 dogfood 调。
- 队列合并的 hint 归一化若过松会误合并不同疑点：以 quote 为主锚、hint 只作辅助（R4）。
- 无候补下 provider 抖动直接体现为 coverage `failed`：先把在途上限与重试退避调对，再看失败率。
