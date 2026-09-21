# redteam 评审引擎·风险路由 DAG 架构（M21 · v12）

> 状态: **锁定**（**键名勘误 2026-09-10**：正文 TOML / snake_case 配置残留统一为 §12.1 JSON camelCase，无语义变化，不升版；
> v8，2026-09-07，P0 验证门出口〔D11〕：E1 风险规则 v4 成立、E3 疑点契约成立；§3.3 / §4 / §6 / §12.1 / §16 / §17 按 D11 改；
> 无 `[review.risk]` / `--risk`；v9 2026-09-08〔D12〕§12 改 JSON + provider → models → roles；**v10 2026-09-09〔D13〕并发形态**：删角色 `workers`、无 `--workers`，
> 唯一闸 = provider 在途门（单把 FIFO + scanner 先于 H0 的入队序 + 429 排队重试，`review.maxConcurrency` 缺省 10），队列入口五态并入 + 跨轮沿用结案带守卫，
> scanner 逐 cell 交付、reviewer 逐 finding 落盘定性为持久化旁路；§0 / §2 / §4 / §5 / §6 / §7 / §8.1 / §12 / §13 / §14 / §15 / §17 对应改；
> **v11 2026-09-15〔D15〕scanner 与深挖时间重叠语义**：在线预算（scanner 未全返回时非必挖项派发数受 maxCells − 保底约束、关闭入口时对剩余预算 drain 一次、
> Skipped 只在此刻判定）、逐 cell 到达的队列侧合并残差入观测、scanner 失败深挖照跑仍硬 Incomplete、门 FIFO 让位与放门唤醒、resume 离线续跑；
> §6 规则 2 / 3 / 6、§8.4、§12.3、§17 R14 对应改；
> **v12 2026-09-20〔D16〕深挖选项稳定性**：结构定项（项身份 = 同文件 ∧ 同稳定块：代码取 enclosing def、文档取标题节、其余退行带）、
> 共识两轴（项内 `peak_agreement` 定必挖、区域级 `agreement` 进排序）、分层排序键接风险档与每文件保底一格、reviewer 笔记去自评置信、
> scanner needs 接线（仅变更集内）；§6 规则 1 / 2 / 3 与可观测面、§7 对应改）· 派生 plan：M21a `../plans/2026-09-07-redteam-m21a-scan-foundation-plan.md`（2026-09-08）；
> M21b `../plans/2026-09-10-redteam-m21b-risk-deep-queue-plan.md`（2026-09-10）；M21c-e 待产出。
> v5（2026-09-05 grill〔D7〕：角色命名 scanner / reviewer / verifier；配置节 §12 重写为用户级唯一 TOML；
> 无候补；verifier 流式化 + 数据判决；provider 在途上限；aacr-bench 退役至 M21c / M21e 重启）。
> v6（2026-09-05）：scanner 组 pack 默认 `slice`〔D8〕；§10.7 提示词按角色分离〔D9〕；E2 改为 M21a 出口 slice vs full A/B。
> v7（2026-09-05）〔D10〕：增量提交改为流式持久化（无 `submit` 旋钮）；proj.db schema v4 一次重置 + 台账导出；cache 断点进
> M21a、其余 R21；分支硬切替代 `engine` 旗，对照基线 = `dev` 二进制。
> 定论依据: `../research/2026-09-04-redteam-review-engine-architecture-research.md`（§1-10 证据、§11 否决账、§12 映射、
> §13 rpiv 补充、§14 aacr-bench 事实）。
> 决策记录（WHY/权衡/被否）: `../decisions/2026-09-04-redteam-review-engine-architecture-decisions.md`（D1-D13，各节以〔Dn〕引）。
> 里程碑: roadmap **M21**（`../roadmap/`，随分支入库；锁定后登记主 checkout `.doc/roadmap/`）。派生 plan: 待产出。
> 曾名 `2026-09-04-redteam-tri-agent-architecture-design.md`（同日改名）。

---

## 0. 一句话定义

**scanner 疑点扫描（分组并行）+ 确定性风险路由 + 深挖优先级队列（reviewer 定向消费，预算随风险分）+ 疑点台账跨轮
激活 + 跨家族并行 + verifier 流式复核。**
不消灭「快 vs 好」的矛盾，而是把它挪到第三个轴：同一墙钟内**用 token 买召回**。慢模型的广度成本是**钱与有界墙钟**
（provider 在途门与预算封顶），不是线性时间〔D1 勘误〕。

**词汇约定**〔D7，D12 键名勘误 2026-09-10〕：三个**角色** = `scanner` / `reviewer` / `verifier`（配置 = 模型下
`roles.scanner / reviewer / verifier`，§12.1）；三个**阶段** = 快扫 / 深挖 / 复核；cell 按角色命名（scanner cell / reviewer cell / verifier cell）。

---

## 1. 约束（C1-C7）

- **C1 输入/输出经济不对称**：input（cache 命中后 ≈ 免费）可慷慨；output（昂贵 + 限速）必须节俭。
  推论：**发现**一个 finding 便宜，**论证**一个 finding 贵，**复核**最便宜。
- **C2 单模型召回天花板（~61%）**：超半数缺陷恰好只有一个模型找到。召回上限靠**跨家族多样性** +
  **确定性地板**，不靠更贵模型。
- **C3 前缀字节稳定性 = prompt cache 命中**：组装顺序是成本问题。volatile 值绝不置顶；角色分化段只在尾部。
- **C4 工具 = 上下文的替代品**：全景 pack + 无工具 = 质量未降 + 轮次 39→5 + input −91%（no-tools-default F1）。
- **C5 确定性先于概率性**：rules/AST/图能可证明覆盖的，绝不交给 LLM；排序/过滤/分派是机械工作，不花 output 买零信息。
- **C6 角色分层 ≠ 分治**：墙钟 = 串行调用相加；只有并行摊平墙钟。**TPS 是单流约束不是总量约束**：K 个独立工作单元
  并行，墙钟 = max 不是 sum。慢模型只以**并行、预算门控**的形态进入，永不作为串行必走级。〔D1, D6〕
- **C7 疑点是一等公民**：低门槛生成（<50 token）、可持久化、可合并、可跨轮激活、可判例预筛。finding 只由深挖论证
  产出；单轮未挖到的疑点是休眠项不是终局。〔D6〕

---

## 2. 编排形状（静态 fan-out + 动态队列 + 流式复核 + 台账回流）〔D2, D6, D7〕

```
阶段0（同步，零 token，一次解析）
  diff 解析 → 噪声过滤 → changed symbols
     ├─ 引用图（changed 之间）→ 连通分量 → 弱缝切 / 宁降不拆 → G1..Gn
     │     └─ 边集保留 → 正反向跨组索引（§10.6）
     └─ 风险规则 → H0 + PR 风险分 R ─────────────────┐
                                                      ▼
  [scanner cells G1..Gn 并行]  ──疑点 / needs──▶  [深挖优先级队列]
   （纯生产者，疑点模式低门槛；                    H0 最高优先 · 键 = 置信 × agreement × 严重度
     多个活跃 scanner 并行 → 疑点并集）            入队合并去重 · 预算 = f(R, 规模) · 必挖集不截
                                                      ▼
                                               [reviewer 分派池]（按 provider 门空位派发 · 终端不再生产）
                                                      │  预算外 → 疑点台账（休眠）
                                                      │  finding 落地即发射 ──▶ [verifier 流式复核]
                                                      ▼                          （blocking / 判例变体才触发）
  ═══ barrier：scanner 全完 ∧ 队列空 ∧ 在途 0 ∧ 复核在途 0 ═══
                                                      ▼
  reduce：dedup → 台账（判例 + 疑点）→ 报告 → 增量轮（delta 激活休眠疑点）
```

**墙钟 = max(最慢 scanner cell, 队列排空) + 最后一条复核；队列排空 ≈ ceil(K / reviewer 各 provider 门宽之和) × 单项时长，K 由预算封顶〔D13〕。**
复核与深挖尾部重叠，不再是独立阶段。

原则：scanner 是静态 fan-out、纯生产者；reviewer 是动态队列消费者池；verifier 是 finding 流的消费者；barrier 只为
dedup 与报告。LLM 在每个节点只做「声明」（疑点 / needs / finding / 判决），所有「完成」（排序、预算、分派、合并、截断）
在编排器，零 token、可复现、故障域隔离。**无候补机制**：cell 重试耗尽即失败，进 coverage `failed`（§8.4）。

---

## 3. 阶段 0：一次解析，三路产出

### 3.1 解析（共享基底）
diff 解析 → 噪声过滤（锁文件/生成物剔除，测试半权）→ changed symbols（hunk 内 def + enclosing def，diffsym 已有）
→ 每文件 churn。分组与风险评分都读此结果，互不依赖，可并行算。

### 3.2 分组（确定性引用图）〔D3〕

**单位 = 文件**（决定谁与谁同 cell）。边只建在 changed files 之间：

| 边 | 判定 | 权重 |
|---|---|---|
| 引用边 | A 的 changed symbol 出现在 B 的 hunk 或全文（regex 引用索引，Rust recall 1.0） | 强，按引用次数计 |
| import 边 | A import/use B（`extract_imports`，需补 TS 裸模块 / Rust `use crate` / Go 包路径） | 强 |
| 同目录边 | 同目录 / 同顶层模块 | 弱 |

算法：
1. 文件数 < `review.grouping.minFiles` 或 churn < `bundleLineThreshold` → 单包直达，不分组。
2. 连通分量 = 初始分组。
3. 分量超 `maxFilesPerGroup` 或超预算 → **先切弱边**（同目录边、单次引用边）。
4. 仍超 → **不切强边，改降内容**（M15 梯子：全文 → 改动 def 体 → 骨架），即「宁降不拆」；组内填充顺序按
   「与组内其他文件的 import 边数」降序（枢纽文件保全文）。
5. 孤立文件（无边）→ 按目录 bin-packing 到预算。
6. LLM 语义分组 `review.grouping.semantic = false` 默认；只喂路径 + churn 元数据；失败回退步骤 5。
   触发是否值得可免费度量：分组边界上的跨组 import 边数（多 = 先验切错、有信息可买；零 = 白花钱）。
7. **边集不丢弃**，提升为流水线产物 → §10.6 正反向跨组索引。

**分组 ≠ 爆炸半径**：分组看 changed 与 changed 的边；爆炸半径看 changed 与 unchanged 的边，为每个 cell 填 caller
body 与 callee 签名。同一张引用图、两个方向。分组切错退化为冗余而非盲区，因半径已填、reduce 再 dedup。

### 3.3 风险路由 → H0 与风险分 R

**输入**：§3.1 解析 + 引用索引 + 台账（proj.db）+ git log。
**输出**：热区 `H0 = [{file, ranges|symbol, signals[], score}]`（t=0 入深挖队列，最高优先）+ **PR 级风险分 R**
（驱动 §6 预算函数）。M15「比例分诊」的落地形态。

| 信号类 | 内容 | 来源 | 对齐 |
|---|---|---|---|
| 路径/类别 | auth·security·crypto·payment·config 目录；权限检查、raw SQL、exec/eval、unsafe、序列化边界、外部输入解析 | 路径 glob + 关键词规则 | 快模型最弱类别（授权 <30%、逻辑 ~21%）恰是规则最易命中处 |
| 结构 | 签名面变更（M15 A 桶）、高 fan-in 符号被改、删除、公开面拓宽、diff 规模/churn | regex/SCIP 现有能力 | 涟漪与破坏面 |
| 外部 API | 关键模块中 hunk 内外部 crate 符号调用密集（≥ k） | `use` 解析 + 改动行 | 三方误用（§9.4），reviewer cell 带契约 |
| 历史 | 该文件有 upheld 记录、在 fix 类 commit 高频、台账有休眠疑点锚定于此 | 台账 + git log | 缺陷热点 |

**规则 v4 定案〔D11，P0 E1 标定，明细见 P0 研究 §15.4〕**
- 成区：hunk → enclosing def 块（局部 const 不算 def；无 def → hunk ± 10 行 padded，只与 padded 相邻合并；相邻函数各自成区）。
  **主区** = 含改动行的区；**涟漪区** = 改动 / 新增 / 删除的声明名与 hunk 外围符号在本文件引用点（≤ 4 处，不跨文件）的 enclosing def。
- 信号四类（路径/类别 · 结构 · 外部 API · 历史）与权重、乘子（声明区 ×0.5、测试含 `#[cfg(test)]` 之后 ×0.3、文档 ×0.2）为代码常量；
  **入 H0 需 score ≥ θ=1.5 且命中 ≥ 2 类信号**，关键词类单独不成区。
- 梯队：主区 = 必挖集、队列最前；涟漪区随预算、截断先截。R = Σ_{H0} score ÷ (1 + ln(1 + 代码改动行))，切点 0.5 / 3.5 分低 / 中 / 高三档（§6 预算）。
- 无模式旋钮：不设 `review.risk`、不加 `--risk`；θ 与权重改动必须过 `eval/risk_replay.py` 回放 PR181 + 本仓 oracle B 并写进 research。
  快扫档将来走独立途径。
- 已知边界：PR181 主裁决集区间级 4/4、并集空热区 0.47；held-out oracle B 区间级 0.59 / 文件级 0.85，漏项主因是改动函数之外的交互点。
- 规则集可回流：台账 dismissed/upheld 统计驱动规则增删（与假阳压制同一反馈环）。

---

## 4. scanner cells（静态 fan-out，纯生产者，疑点模式）〔D6〕

每组一个 cell，并行，只受各 provider 在途门限流（无角色级 `workers`〔D13〕）。**逐 cell 交付**：cell 返回后其全部疑点经 §6 入口并入队列；
不做逐疑点流式（思考模型的可见 JSON 只在每格尾部 10-20 s 内产出，且只有最后一格在关键路径上，收益不值〔D13〕）。输入 = 该组 pack（§10）+ 同文件活跃
判例摘要（判例前移）。**多个活跃 scanner 条目对同一组各跑一次，疑点并集**（§5）。

**输出契约（疑点索引，非 finding）**
```
suspicion: { file, line_start, line_end, hint(一句话), category?, confidence, quote }   每条 <50 token，quote 必填
needs:     [{ path, lines?, reason, kind: evidence | api }]                              cap 5
scanned_areas: [{ file, ranges }]                                                        覆盖自报（零调用注意力对账）
```
- **门槛：疑似即列**，不要求论证、不管假阳；假阳交深挖与复核。快模型「知道哪里可疑但写不出来」的信号不蒸发。
- **scanner 不直接产 finding**；全部疑点入深挖队列（§6），needs 项同队列。
- **判例前移**：prompt 注入同文件活跃判例摘要。机械匹配命中的疑点在 §6 入口按**跨轮沿用结案**处理，守卫 = 锚定可重定位（carry 机器 anchor_near）
  ∧ 该区域自判例以来无 delta：过守卫 → dismissed 即豁免不入队、upheld 即 carry 不重挖；不过 → 正常入队、带判例笔记〔D13，替代 v9「降级候选」；
  无守卫的漏报路径 = fix 引入新 bug 被旧判例盖住〕。
- 可选：同模型多 lens 采样并集 `review.scanners.samples = 1`（换召回，不换墙钟）。
- `quote` 是跨模型疑点合并与判例预筛的锚（`quote_overlap` 口径复用）；**quote 逐字硬约束，不在文件即丢弃**（E3：疑点模式守约弱于 finding 模式）；
  `category` 取 L0-L5；`hint` 归一化后置 M21c（R4，〔D11〕）。
- scanner 请求显式钉推理档（角色属性 `thinkingLevel` 首个消费者）；零推理 + 短输出记 RunMeta 观测，不重跑（E3：疑点模式 2/3 跑几乎零推理）。
- 提示词 = 共享头 + scanner 角色尾（§10.7）。

---

## 5. 多样性轴：多个活跃 scanner 并行（疑点并集）〔D7〕

- `scanner` 角色模型集即活跃集（D12 roles）：每条对每组各跑一次，**并行**、疑点并集、`agreement` = 列出该疑点的**家族数**（入 §6
  优先级键与 §8.1 跳过规则）。
- 第二条优选异家族快档；30 tok/s 档在疑点输出体量下仍落在快扫窗口内。
- 代价：input × 条目数（cache 不跨模型）；墙钟不增。无候补语义：列表里没有备胎，只有并跑者。
- **agreement 是活值**〔D13〕：在 §6 入口随后到的同疑点并入而累加；排队中的项每次并入后重算优先级与必挖资格；已派发的项只改记录不回头；
  §8.1 跳过规则按 finding 落地时刻的值算，后涨不回溯。三个消费者（优先级键 / 必挖集 / 跳过规则）不变。

---

## 6. 深挖队列（优先级 · 合并 · 预算 · 台账 · 终止）〔D2, D6, D7〕

**队列项** `{file, ranges[], block, score, origin: rule | scan | needs, notes: suspicion[]}`。
**生产者三种，同一类型**：规则命中（H0，t=0 入队）· scanner 疑点 · scanner needs（C3 起接线，**只接变更集内**的 `kind = evidence`；
diff 外只计 `needs_out_of_diff`——不在变更集的文件装不出证据（§7 的区域 pack 只会输出一行省略说明），入队即白烧一格；
装包归属另议〔D16〕）。

| # | 规则 | 内容 |
|---|---|---|
| 1 | 优先级 | **分层键**〔D16〕，自高而低：每文件保底一格 → 种类（主区 H0 > 涟漪 > 疑点 > needs〔D11〕）→ 必挖轴 `peak_agreement` → 所在风险区档（按 θ 粗分四档）→ 项内疑点条数 → 代码优先于文档 → 自评 confidence（**只做末位裂解**）→ id。**非 FIFO；不用置信阈值切**；类别权重保留 1.0 不启用（标注自身在漂：两轮 L0 占比 59% vs 21%）。原乘积式 `confidence × agreement × 类别权重` 因权重恒 1、agreement 只取 1/2，实际退化成「按 confidence 排」，而置信跨轮不可比（两轮中位 0.35 vs 0.45）、经预算截断后等于把它当门用。 |
| 2 | 入队合并 | **结构定项、谓词定共识**〔D16〕：项身份 = 同文件 ∧ 同**稳定块**——代码取 enclosing def（与 §3.3 H0 成区同一零件）、markdown 取标题节、其余退固定行带；块键取声明 / 标题行的去空白哈希，上游插删行与缩进调整都不改身份，项区间随之取块范围。`same_suspicion`（同文件 ∧ 行重叠 ∧ quote 相等或不琐碎重叠，谓词仍复用 M21a `merge_suspicions`）降为**项内聚簇**依据，产出两轴：`peak_agreement` = 最大簇的家族数（定必挖）、`agreement` = 块被几个家族碰过（进排序）。无块信息（旧快照 / 内容不可读）退回 C1/C2 的 quote 谓词，行为逐字不变。**入口五态**〔D13〕：无命中 → 新项 agreement = 1；命中排队中项 → 并入（异族 agreement + 1、notes 追加、区间取并、重算 score，≥ 2 即标必挖）；命中在途项 → 只改记录（agreement + 1、hint 附记录），运行中的请求不注入、不重挖；命中本轮已结案 → 同上并沿用结案；命中跨轮判例 → §4 守卫（过 → 沿用结案不入队；不过 → 按无命中入队、带判例笔记）。同家族重复只合并不加 agreement。入口是编排器确定性代码，零 token；派发前再核一次在途 / 已结案。逐 cell 到达时队列侧逐条比对已有项笔记——不是组内传递闭包，漏合并只会少算共识、多复核；收场以报告侧闭包对账，拆簇数入 RunMeta〔D15〕。 |
| 3 | 预算 | `review.reviewers.maxCells = "auto"`（按 R 三档：low 4 / mid 12 / high 32，硬帽 48，起步值）或整数固定〔D11〕。**必挖集** = 主区 H0 ∪ {`peak_agreement` ≥ 2 的疑点}〔D16〕不受截断（按区域级 agreement 判会把必挖集从每轮 4 条撑到 7–9 块、PR181 规模 18/23，`maxCells` 形同虚设）（confidence 只进优先级不做门；保护的是仍在排队的项，已派发项已在挖〔D13〕）；**保底**：每轮为 scanner 疑点保留 ⌈maxCells/4⌉ 格，主区吃穿时帽 = 主区 + 保底并在 RunMeta 披露；涟漪区与其余疑点按优先级吃剩余预算。到期 **graceful drain**：在途跑完，未开跑者落台账。**在线口径**〔D15〕：scanner 未全部返回时，非必挖项仅在「本轮已派发 < maxCells − 保底」时可派，必挖项不受限；关闭入口即「到期」——对剩余预算（maxCells − 已派发）按上述规则 drain 一次（保底扣除扫描期已派发的非必挖疑点），Skipped 只在此刻判定；resume 补跑过 scanner 则复位 Skipped 重选（§17 R14）。 |
| 4 | 预算外去向 | **疑点台账**（§8.3）休眠；报告披露为低置信线索；**不进 verdict**。截断不静默。 |
| 5 | 分派 | 活跃 reviewer 条目构成**分派池**：每项只由一个模型消费，按空闲轮转；多条目买吞吐与分散限流，不双审。派发由 provider 门驱动：某 reviewer 条目所在 provider 有空位时才弹出最高优先项交它，无角色级池大小〔D13〕。scan 来源项**不等**同组其他家族 cell 返回（先到先挖，后到并入记录；是否加同组 hold 由派发时 / 落地时 agreement 数据在 M21e 定〔D13〕）。 |
| 6 | 终止 | 全部 scanner cell 返回（成功或失败）→ 关闭入口（收场 drain〔D15〕）→ 队列排空 → 在途归零 → 复核在途归零 → reduce barrier。 |
| 7 | 并发 | 无角色级 `workers`〔D13〕；有效并行 = 各 reviewer 条目所在 provider 的在途门（§12.3）。 |
| 8 | 终端 | reviewer cell 不再生产。`reenqueue = false` 默认。 |

可观测面（cell/phase 已有框架）：队列深度、入队原因分布、去重命中数、预算截断数、台账落入数、项等待时长、派发时 / 落地时 agreement〔D13〕；
收场时刻、首次派发时刻、深挖排空时刻、复核尾部、扫描期派发数、收场剩余预算、拆簇数、填门循环阻塞时长〔D15〕；
疑点到达数、疑点项数（两者之比 = 结构化合并的压缩比）、候选 / 选中文件数、单文件最大占格〔D16〕。
实现落点：tokio mpsc + 单派发器（按各 reviewer provider 门空位弹出最高优先项，不设 worker 数〔D13〕），scanner cell 为 sender、reviewer cell 为 receiver，复用现有 cell 抽象；reviewer
cell 的记录形状与 resume 语义见 §17 R14〔D11〕。

---

## 7. reviewer cell（慢模型，终端）

- **H0 项（origin = rule）**：无笔记，**独立**审。覆盖它的 scanner 组尚未完成；同一区域两模型独立看，reduce 里
  agreement 计数即多样性轴。
- **疑点项（origin = scan | needs）**：带疑点作「一位评审者的笔记」注入，指令「确认、驳回或补充」，**不是**「验证以下」
  （防锚定）。笔记形如 `file:lines — hint — quote`，**不带自评置信**〔D16〕：括注理由即为防锚定，摆一个 0.82 与此自相矛盾；
  E3 也实证 flash 级自评与命中无关。needs 项无笔记，走与 H0 同款的区域独立审。在途期间后到的同疑点只并入记录（§6 入口第三态），不注入运行中的请求；落地的 finding 继承记录当时的 agreement〔D13〕。
- **输入 pack**：热区切片（§10.2）+ 爆炸半径邻居（caller body、callee 签名）+ 跨组索引（§10.6）+ 三方契约（§9.4 B 层，
  `kind = api` 时含函数体）+ spec 段（`--spec` 已有）。确定性代码预装配，**不拿工具自主探查**。
  区域 pack **由构造即切片**，不受 `review.pack.body` 控制（该轴只管 scanner 的组 pack）。
- **输出**：完整 finding（危害链、修复建议）。**finding 只由此产出**（C7）。每 cell 输出天然有界。思考档由角色属性
  `thinkingLevel` 给（典型 high）。
- **流式持久化**〔D10〕：响应 SSE 流上对 `findings` 数组做增量 JSON 解析，每个对象闭合即落盘，cell 中途死亡时已产出项不丢。
  不改模型契约、不引入工具循环、无 `submit` 旋钮；FINALIZE / REPAIR 仍管最终整体解析，流式持久化只是旁路。**定性为持久化旁路**〔D13〕：
  思考模型的可见 JSON 在尾部数秒内闭合，逐 finding 与逐 cell 在墙钟上等价；价值在格中途死亡时已闭合对象不丢。模型在一次请求内不能边调工具边交付，
  客户端工具循环每轮重付 TTFT 与整段推理，故不以工具循环换粒度。
- finding 落地即发射给 verifier（§8.1），不等 barrier。
- 终端：不入队新项（§6 规则 8）。
- 提示词 = 共享头 + reviewer 角色尾（§10.7）。

---

## 8. 复核 · reduce · 台账 · 增量轮 · coverage

### 8.1 verifier 流式复核（怀疑者立场）〔D7〕
- **触发集**：`severity ≥ block ∧ confidence ≥ floor` 的 finding（blocking）、以及命中判例变体的 finding。低危不触发。
- **跳过**：该 finding 的区域被**两个不同家族的 scanner** 独立标记（疑点 agreement ≥ 2）且由 reviewer 确认——三方一致才
  免验；carried 项（出生轮已复核）；精确 id 已在台账者。
- **流式**：finding 一落地即复核，与深挖尾部重叠；跳过规则所需的 agreement 与判例匹配均可单条计算，不需 barrier。跳过规则按 finding **落地时刻**的
  agreement 算，后涨不回溯〔D13〕。
- **verifier 选择**：verifiers 活跃集（拥有 `verifier` 角色的模型）非空则从中选；缺省自动：**家族 ≠ 该 finding 的 reviewer 家族**（`family` 缺省取
  `model_id` 首段，条目可覆盖），优先 reviewers 档，其次 scanners 档；一个异族都没有 → 同族复核并在 RunMeta 与报告
  warning「无异族 verifier」。
- **动作**：finding + ±60 行快照（骨架中档）+ 相关判例 + spec 段 → `REFUTED | UPHELD` 加一句理由；prompt 字段序
  **analysis 先于 verdict**。refuted → confidence 归零、永不 blocking；upheld → 原样；调用失败 → 保守 upheld + warning。
- **去留由数据决定**：`review.verifiers.enabled = true`；RunMeta 记每轮 blocking finding 的 refute 率；dogfood 数轮后
  refute 率接近零则关默认（铁律 1）。
- 提示词 = 共享头 + verifier 角色尾（§10.7）；离线回归集 = proj.db 已 dismissed（已知假阳）与 upheld（已知真）findings。

### 8.2 reduce（已有零件，零改动）
dedup（title jaccard + quote 行重叠）→ 判例台账 → 报告 → carry 重锚定进增量轮。层② positioning 砍编造照旧。

### 8.3 疑点台账（新）〔D6〕
- proj.db 新表，镜像 DismissCard / ResolveCard 模式；条目 = 疑点（quote 锚 + hint + confidence + origin + 落入原因）。
- 增量轮激活规则：`delta ∩ 疑点锚定文件 ≠ ∅` → 重新入队，优先级提升一档；注入措辞照抄 claimed-resolved 卡的搭车定位，
  不稀释 delta 主任务。carry 机器（anchor_near 重锚、rename 迁移）零改动复用。
- **休眠疑点绝不进 verdict**（verdict 纯函数只看 findings），只进报告披露 + RunMeta。
- preflight 守卫扩展：上轮 verdict = Incomplete 时台账疑点不可信。

### 8.4 收敛三维、活锁守卫、coverage 分级
- 收敛 = 无 blocking findings ∧ 疑点台账无 delta 邻接未处理项 ∧ 无激活超限项；exit_code 五档契约不污染。
- 活锁守卫：低置信疑点被 delta 反复激活而深挖不晋升 → 记激活次数，超 N 次强制入必挖集或停车交人。
- coverage 五集合（per cell 类型）：selected / completed / reused / failed / waived，不相交并 = 全集。**无候补**，
  cell 重试耗尽即入 `failed`。分级：**scanner cell 失败 → verdict 硬 Incomplete**（疑点集有结构性选择偏差）；
  **reviewer cell 失败 / 预算截断 → 带债 approve**（台账化、RunMeta 披露、warning）；verifier cell 失败 → 保守 upheld。
- scanner cell 失败不中止深挖〔D15〕：已入队项照常挖完，findings 照常进报告与统计；verdict 与退出码仍为 Incomplete / 3（优先于 reject 与带债），
  其中 blocking 条数进 warning——与 ship「incomplete 带部分 findings 照常裁决、不算 approve」同口径。

---

## 9. 确定性地板（「不漏」的保证）

扫描承担广度，不漏由确定性组件保证（C5）。

- **L1 规则/AST**：规则先跑，LLM 只看规则决定不了的；prompt 显式列出 CI 已覆盖类别令模型跳过。
- **L2 引用召回与锚定**：regex 词边界 + 缩进/括号（Rust recall 1.0）+ 过滤 B + per-name cap 10。positioning 加
  **跨文件重定位**（changed files 内唯一命中，多/零命中拒绝）。tree-sitter 不立项。
- **L3 blast-radius 裁剪**：Hop 0-2；排库边；Hop 距离按仓库配置；**在 prompt 进模型前做**。图价值 = 裁剪 +
  callee 签名，**不做引用召回引擎**。

### 9.4 L4 三方 callee 正确性（四层）〔D5〕

前提：三方库源码与契约**本地就有**（Rust `~/.cargo/registry/src/`、Go mod cache、Node `.d.ts`、Python site-packages），
不需要 LSP。

| 层 | 机制 | 抓什么 | 状态 |
|---|---|---|---|
| A 编译门 | 索引构建即真类型检查 | 参数个数/类型/生命周期 | M15 D8 已有 |
| B 契约注入 | S2-b callee 注入扩到 registry 源：签名 + doc + 短函数体，按 Cargo.lock **精确版本** | 语义误用、版本漂移 | 同一机制换根路径；默认全注入 |
| C needs 联动 | scanner `needs{kind: api}` → 队列 → reviewer cell 带契约 + 函数体 | 模型自知不知的部分 | needs 已有，加 kind |
| D 测试执行 | 跑 PR 测试或定向测试 | 行为级 | M20 R6 执行判，钩子位 |

B 层 Rust 定位链：`use` → crate → Cargo.lock 版本 → registry 路径 → `pub fn name` + `///` 块；歧义带 crate 标签 cap。
**接受的残余**：doc 未写的前置条件 + 无测试 + 模型不知 → 不可发现。

---

## 10. 上下文：装包单位、顺序与跨组索引〔D4, D6, D8, D9〕

### 10.1 单位
**分组单位 = 文件**（§3.2）；**装包单位 = 改动中心切片**（changed symbol，非文件）。

### 10.2 改动中心切片（`review.pack.body = "slice"`）

| 层 | 内容 | 形态 |
|---|---|---|
| 核 | 改动 def 的新全文 + diff | 全文 |
| 文件内一跳 | 改动 def 调用的、以及调用改动 def 的同文件符号 | 全文 |
| 文件地图 | 文件内全部签名 + 文件头 / 模块 doc / 常量块 | 骨架 |
| 其余 | 无关符号的函数体 | 折叠 |
| 文件外 | caller 调用点 ±N、callee 签名 + doc | 片段 |
| 兜底 | 锚定失败 → hunk ±N → 全文 | 禁真空 |

**默认 `slice`**〔D8〕。该轴只管 scanner 的组 pack（reviewer 区域 pack 由构造即切片）；`full` 保留为旋钮。
- **小文件直接全文**：文件 ≤ `review.pack.sliceMinBytes`（默认 8K）不切，此时 slice 与 full 等价。
- **三层同批**：禁真空兜底、文件内一跳、文件地图缺一不可，否则重演 F3。
- **回退信号**（任一出现先试二跳、再试 `full`）：M21a 出口 PR181 A/B 中 slice 的疑点 oracle 命中低于 full；dogfood 中 scanner
  `needs{kind: evidence}` 触发率持续偏高（反复索要被折叠的函数体）。

### 10.3 顺序与位置（稳定性降序，字节级；角色分化只在尾部）
```
[字节稳定 → cache 区]   系统协议 / 评审规则 / 对抗立场 / 仓库简介 / 仓库 spec
[轮间稳定]              仓库级摘要、公共符号表
[组内/热区变化]          文件地图、邻居、三方契约、跨组索引、同文件判例摘要
[次尾]                  diff + 改动 def（提问对象，注意力高位）
[尾]                    角色尾：输出契约 + 角色指令（短；各角色唯一分化段）
```
- **cache 断点**〔D10〕：anthropic kind 在三个段界（共享头末、证据段末、角色尾前）放 `cache_control`；openai kind 靠字节稳定
  前缀自动命中。RunMeta 按 cell 记命中率。非结构性优化（provider 专属 context caching、跨 run 预热）见 R21。

### 10.4 「仅关注改动」的约束强度
提示词约束**弱**。报错位置噪音 → 确定性后滤 `review.pack.anchorWindow = ±N`；注意力稀释 → 只能靠不发（§10.2）。

### 10.5 仓库简介与反面教材
- **仓库简介入稳定前缀**：项目 `.redteam/brief.md`（约 2K token，**内容非配置**，按不可信证据注入）或复用 `--spec`。
  **不做 per-PR 摘要轮**（C6）。
- 反面教材：ocr 模板 `{{current_system_date_time}}` 置顶。

### 10.6 跨组索引（分组副产品，零 token）〔D6〕
正向 `path → imports`、反向 `path ← imported-by`（manifestation-site 信息）、增量轮 `[delta]` 标注；渲染进
`{{OTHER_CHANGED_FILES}}` 槽；omit 清单「降质 + 留指针」。

### 10.7 提示词：共享头 + 三个角色尾〔D9〕

**结构**：一份共享头（字节稳定，cache 区）+ 三个角色尾（各角色唯一分化段，§10.3）。今天的 `adversarial-code-bundle.md` /
`adversarial-refute.md` 拆为 `templates/shared-head.md` + `scanner.md` / `reviewer.md` / `verifier.md`；渲染 = 共享头 + 证据段 +
角色尾。每个角色尾的输出契约与 parse 类型字段级对齐（铁律 5）。

| 角色 | 立场 | 输入 | 输出契约 | 关键措辞 |
|---|---|---|---|---|
| scanner | 怀疑者·广度：疑似即列，不论证，不管假阳 | 组 pack（slice）+ 同文件活跃判例摘要 | `suspicion[]` + `needs[]` + `scanned_areas` | 「列出一切可疑之处，不需要证明」；判例摘要「以下曾被驳回，若仍报请降低置信」；缺证据「声明 needs，不要猜」 |
| reviewer | 建设者·深度：论证或驳回 | 区域切片 + 半径 + 跨组索引 + 契约 + spec + 笔记 | `finding[]`（危害链、建议） | 「确认、驳回或补充」；笔记是「一位评审者的笔记」而非事实（防锚定）；H0 无笔记时「独立评审此区域」 |
| verifier | 怀疑者·单点：只推翻 | finding + ±60 快照 + 判例 + spec | `analysis` → `REFUTED \| UPHELD` + 理由 | 「找出这条 finding 不成立的证据」；analysis 先于 verdict，不可撤回 |

- **共享头内容**：系统协议、评审规则、对抗立场、CI 已覆盖类别跳过清单（L1）、不可信输入免疫段（威胁模型 B）、仓库简介、spec。
  角色尾不重复共享头。
- **注入段全部过 `neutralize_structure`**：笔记、判例摘要、跨组索引、brief、三方契约。
- **思考档位由角色属性 `thinkingLevel` 给**，prompt 不写「仔细思考」类散文。
- **模板哈希入 RunMeta**：共享头与每个角色尾各一 hash，A/B 归因用。
- **离线回归集**（M6-b 提示词 harness 的复活形态，零新数据；改模板必跑对应集）：
  - verifier：proj.db 已 dismissed findings = 已知假阳、upheld = 已知真 → 二分类准确率；
  - scanner：PR181 疑点 oracle 命中 + 噪音比（E3 同源）；
  - reviewer：H0 / 疑点 → finding 的确认率，及其后 verifier 的 refute 率（refute 率高 = reviewer 模板在编故事）。

---

## 11. 工具（分层授予，非共享）

| cell | 工具 | 说明 |
|---|---|---|
| scanner | 无 | 纯靠 pack + 确定性地板；缺证据走 `needs` |
| reviewer | 无（默认）/ `needs` 二阶段补查 | two-stage 语义沿用；拒 Write/Edit；产出经流式持久化落盘 |
| verifier | Read 快照（`source_snippet`） | 不重建 bundle |

`tools = on` 保留作诊断/对照档。

---

## 12. 配置（用户级唯一 JSON）〔D7, D12〕

`$REDTEAM_HOME/config.json` + `secrets.json`（缺省 `~/.redteam/`，D7 A8 用户级唯一、无项目层）；严格 JSON、键 camelCase、无注释。
形状 = provider → models（按 id 做键）→ roles：模型固有字段只写一次，角色属性挂模型下；某角色的活跃集 = 拥有该角色键的全部模型
（列表即活跃集、无候补〔D7 A3〕）。

### 12.1 config.json
```json
{
  "providers": {
    "deepseek": {
      "kind": "openai",
      "baseUrl": "https://api.deepseek.com",
      "apiKeyEnv": "DEEPSEEK_API_KEY",
      "maxConcurrency": 20,
      "models": {
        "deepseek-v4-flash": {
          "displayName": "DeepSeek V4 Flash",
          "contextWindow": 1000000,
          "maxOutputTokens": 384000,
          "reasoning": true,
          "roles": { "scanner": { "thinkingLevel": "low" } }
        },
        "deepseek-v4-pro": {
          "contextWindow": 1000000,
          "maxOutputTokens": 384000,
          "reasoning": true,
          "perTurnTimeoutSecs": 1200,
          "roles": { "reviewer": { "thinkingLevel": "high" } }
        }
      }
    },
    "agct": {
      "baseUrl": "https://api.aigocode.team/v1",
      "maxConcurrency": 20,
      "models": {
        "gemini-3.8-flash": {
          "kind": "openai",
          "apiKeyEnv": "AGCT_GEMINI_API_KEY",
          "family": "gemini",
          "contextWindow": 1048576,
          "maxOutputTokens": 65536,
          "reasoning": true,
          "roles": { "scanner": { "thinkingLevel": "low" }, "verifier": { "thinkingLevel": "high" } }
        }
      }
    }
  },
  "review": {
    "blockThreshold": "medium",
    "confFloor": 0.5,
    "maxConcurrency": 10,
    "scanners":  { "samples": 1 },
    "reviewers": { "maxCells": "auto", "reenqueue": false },
    "verifiers": { "enabled": true },
    "grouping":  { "minFiles": 4, "bundleLineThreshold": 200, "maxFilesPerGroup": 10, "semantic": false },
    "pack":      { "body": "slice", "anchorWindow": 30, "sliceMinBytes": 8192 }
  }
}
```

字段：provider 层 `kind`（模型层可覆盖，两层都缺 → 校验报错）/ `baseUrl` / `apiKeyEnv` / `maxConcurrency`（该 provider 在途上限，缺省继承
`review.maxConcurrency` = 10；> 128 校验 warning「像是填错字段」〔D13〕）；模型层 `name`（人读显示，缺省 `provider/id`）/ `kind` / `apiKeyEnv` / `family`（缺省 id 首段）/ `contextWindow`
（缺省保守 128K，有下限）/ `maxOutputTokens` / `perTurnTimeoutSecs` / `reasoning`（思考 token 计入输出额度）；角色层 `thinkingLevel`
（`off | low | medium | high`，角色属性：同一模型可 scanner 位 low、reviewer 位 high）。route_key / 报告名 = `provider/id`；活跃集顺序按
provider 名、model id 字典序（无语义）。`verifiers` 无任何模型声明 → 自动异族（§8.1）。无 `priority`、无 `--models`（多模型即多条目）。无角色级 `workers`、无 `--workers`〔D13〕。

### 12.2 secrets.json
```json
{
  "providers": {
    "deepseek": { "apiKey": "sk-…" },
    "agct": { "apiKey": "sk-gateway", "models": { "gemini-3.8-flash": "sk-model-specific" } }
  }
}
```
- 解析链：模型 `apiKeyEnv`(env) → `secrets.providers.X.models[id]` → provider `apiKeyEnv`(env) → `secrets.providers.X.apiKey`。
- 守卫：secrets 里的 provider 名或 model id 在 config 无对应 → warning（拼写）；文件 chmod 600；`apiKey` 只出现在 secrets.json。
- `redteam model setup --key-store secrets [--model-key]` 按此形状写入；provider / model / role 三层只合并显式给出的字段，写前完整校验，
  现有文件解析失败拒写不覆写〔D12〕。

### 12.3 路由与限流〔D13〕
- 编排层按角色发 route_key（`provider/id`；registry 已支持任意粒度）。**唯一一道闸 = provider 在途门**（`maxConcurrency`，缺省继承
  `review.maxConcurrency` = 10）：同一 provider 下全部角色条目共享一把；无角色级 `workers`、无 `--workers`（角色帽在 PR181 实测只起副作用：
  缺省 4 < 7 格排队 100-130 s，而跨 provider 全局一把还把两个独立 scanner provider 卡在同一个数上；单次节流的真实需求是跨进程的，旗解决不了）。
- 门 = 单把 FIFO，不设调度器、不设预留：门饱和时预留只换顺序不换总量，还让 reviewer 用不完整信息提前吃预算；最长链优先 = scanner 格先于 H0 项
  入门（t=0 入队序），reviewer / verifier 项天然后到。真正缩尾巴的杠杆是拆大 scanner 格（followup）。
  FIFO 对非阻塞取门同样成立〔D15〕：门上已有等门者时非阻塞试取让位、不插队；深挖有可派项而各门无位时自身排入同一 FIFO 等门（放门即唤醒，
  外部持门者——scanner、verifier——释放后及时补位）；t=0 由 poll 序保证 scanner 先登记。机制按「任意角色等门者」通用，无角色优先级。
- **429 排队重试**（R16 最小替身）：某 provider 撞 429 / overloaded → 该 provider 门宽 ← max(1, min(门宽 − 1, 此刻仍在跑且已被接受的请求数))，
  本轮不回升——一波 429 内收敛到网关真实上限，错配 `maxConcurrency`（如误填 maxOutputTokens）也能自愈；撞 429 的请求持名额不放，等本 provider
  下一个请求完成（每次完成只唤醒一个等待者）或 Retry-After 取晚者再重试；429 重试独立计数，上限 = 当时有效门宽 × 2，不吃 2 次抖动预算；门收窄后
  多出的名额在释放时丢弃不归还（`InflightGate` 自管计数 + Notify）。RunMeta 与 run.warnings 记「provider X：429 × N，门宽 A → B」；每 provider
  每角色在途峰值、每 cell 门等待时长（`queue_wait_ms`）可读；路由到某 provider 的 cell 数 > 其门宽 → 排队告警指向 `maxConcurrency`。
- **收口批评审修补记**（2026-09-09 code-review，Trellis `09-09-d13-provider-gate` 4aabec3）：撞 429 者的重试须经**准入**（`active <`
  有效门宽才预占在途；门满则回去等下一次完成，每次成功完成只放一个），被在途归零广播唤醒者先退避再准入——不齐发；只有**成功**
  完成才唤醒等待者（Timeout / 5xx 不算）；429 重试按每次 HTTP 调用独立计；HTTP 529 与 SSE `overloaded_error` 同归 RateLimited
  （503 仍按 5xx，见 followup）；`CellRecord.attempts` = 轮次 + 429 重试、`queue_wait_ms` = 门等待 + 429 排队等待；排队告警按收口时的
  有效门宽，已收窄时说明是收窄所致、不劝「调大」；尝试括号 RAII，取消不泄漏在途。延后的设计层争议（速率型 429 棘轮、时间预算放弃、
  策略非 Option、503 归类）见 followup「D13 收口批评审延后项」。
- 同角色即同档：不设 `tier` 字段。
- 单轮超时：模型 `perTurnTimeoutSecs` > `REDTEAM_PER_TURN_SECS` > `实际生效 max_tokens ÷ 20 + 60s` > 150s〔D12〕。
- 实测 TPS：DeepSeek v4 flash ~103、GLM 5.3 ~27（含思考）、Kimi K3 ~30。流式 + 显式 max_tokens 硬要求。

---

## 13. 账（PR181 规模，40 文件 +1750/−69）

| | 现状 GLM 单发 | 现状 DeepSeek 单发 | v5 低风险 PR | v5 高风险 PR |
|---|---|---|---|---|
| 快扫（分组并行，疑点输出） | 19 min（单流 30.9K） | 5.3 min | 2-3 min | 2-3 min |
| 深挖（并行，预算内） | - | - | 必挖集 ≈ 1 轮 ≈ 2-4 min | ceil(K / reviewer 门宽和) 轮 ≈ 4-6 min |
| 复核（流式，与深挖尾部重叠） | 含 | 含 | 尾部 +0.3-1 min | 尾部 +0.3-1 min |
| **合计** | 19 | 5.3 | **5-8** | **7-10** |

**「好」的来源**：跨家族疑点并集 · 路由对齐快模型盲区 · 确定性地板 L1-L4 · 疑点台账 · 稳定前缀里的仓库简介 ·
verifier 怀疑者复核（唯一以推翻为目标的环节）。
**对「太依赖第一步扫描」的三重对症**：H0 规则项不经扫描直接深挖；多家族疑点并集；台账让单轮漏报变休眠。

---

## 14. 边界（接受的代价）

- **预算外疑点休眠而非丢**，但当轮不进 verdict；delta 永不触及其锚定文件则永不激活。
- **路由规则会漏**。只能靠疑点并集与台账兜。
- **无候补**：provider 不稳时 cell 失败直接进 coverage，靠 provider 在途门、429 排队重试与重试退避降低发生率，不靠换模型重跑。
- **切片若切错 → 缺依赖类漏**。三层兜底 + 小文件全文 + 回退信号（§10.2）。
- **小 PR 净变慢**（阶段固定开销）：`minFiles` 以下单包直达 + needs 快路径旁路。
- **三方残余**（§9.4）。
- **成本上升**。input × scanner 条目数 + 预算内深挖 + 契约注入；换的是时间。

---

## 15. 与现状 delta 与落地序

**新增**：风险评分器（H0 + R）· 深挖队列 + 门驱动派发器 + 预算函数 · reviewer cell 类型 · verifier 流式段 · 配置节
（用户级唯一、角色列表、provider 在途上限、secrets 新形状、`REDTEAM_HOME`）· 分组算法 + 正反向跨组索引 · 疑点 schema +
疑点台账表 + 增量轮激活 + 收敛三维 · coverage 五集合 + 角色分级 · L4-B 三方契约注入 · 切片装包（scanner 默认 slice）· 提示词共享头 + 三角色尾 + 模板哈希 + 离线回归集 · 段序角色尾 ·
判例前移 · 跨文件重定位 · `scanned_areas` · refute 字段序 · `--pack-only` 预算仪表 · `--test-command` · 流式持久化 · proj.db schema v4（一次重置，先导出台账）· cache 断点（anthropic kind 三段界）· 429 排队重试与门收敛〔D13〕。
**移植**：ocr 双阈值门。
**其余原样**：pack、dedup、carry、判例台账、流式、per-model 超时、no-tools 默认、needs 二阶段。
**消亡**：`--models` / `--models-all` / `priority` 候补链 / 项目级 config.toml 与剥离清单 / `XDG_CONFIG_HOME` 路径 /
scanner 直接产 finding / `engine` 旗（对照基线 = `dev` 二进制）/ `submit` 旋钮 / 角色级 `workers` 与 `--workers`〔D13〕。

落地序：
1. 配置节 + 分组（引用图 + 正反向索引）+ 切片装包（scanner 默认 slice）+ scanner 疑点契约 + 提示词共享头 / scanner 尾拆分 + proj.db v4 一次重置（先导出台账）+ cache 断点（墙钟与召回地基）
2. 风险评分器 + 深挖队列 + reviewer cell + 预算函数（finding 只由深挖产出）+ verifier 流式段
3. 疑点台账 + 增量轮激活 + 收敛三维 + coverage 分级 + 默认翻转（翻转前 aacr smoke）
4. 段序角色尾 + 判例前移 + refute 字段序 + 跨文件重定位 + `scanned_areas`（小项打包）
5. L4-B 三方契约 + `needs{kind: api}`
6. 第二家族 scanner（疑点并集）
7. 仓库简介入稳定前缀
8. 切片调优（二跳 / `sliceMinBytes`，按回退信号）

每个新编排 pass 三大标配：注入互斥、证据段预算帽、读守卫；输出契约与 parse 字段级对齐；截断修复 per-backend 单点；
观测仪表随行。

---

## 16. 前置验证与基准〔D7〕

| # | 实验 | 方法 | 裁决 |
|---|---|---|---|
| E1 | PR181 回放（零 LLM） | 风险评分器脚本跑 40 文件 → H0 + R；GLM 6 条 vs DeepSeek 3 条差集 + proj.db 历史 dogfood upheld findings 作小 oracle；看落热区比例 | 落中 → §3.3 规则初版成立；未落 → 先修规则 |
| E2 | slice vs full A/B（M21a 出口，PR181 + dogfood） | v2 scanner 两种 `pack.body` 各 N=3：疑点 oracle 命中、噪音比、输入 token、cache 命中率 | slice 命中 ≥ full → 维持默认；否则按 §10.2 回退信号处理。合成 util 文件 LITM 实验降为可选 |
| E3 | 疑点模式 vs finding 模式召回（快模型） | PR181 同快模型两种契约各 N=3；oracle = GLM 6 ∪ rpiv 8 ∪ Kimi 3 ∪ ocr 3 去重 | 验证 §4 契约；校准 `c_hi`（R11） |

结果（2026-09-07〔D11〕）：E1 规则 v4 成立（主裁决集区间级 4/4、空热区 0.47；四轮）；E3 疑点契约成立（人工主题命中三跑并集 5:1）；`c_hi` 数据不支持 → R11 改 agreement 口径。
数字见 `../roadmap/P0-validation-gate.md`「结果摘要」，明细 `../private/research/2026-09-07-redteam-p0-validation-gate-research.md`。E2 移至 M21a 出口。

**测试主场**：PR181（连续性对照、网关真实度）+ 本仓 dogfood（增量轮、台账、ship）。
**对照基线 = `dev` 分支二进制**〔D10〕：分支硬切、无 `engine` 旗，A/B 时两个二进制并排跑同一 PR；切名前分支产物名 `redteam-next`，
插件与 ship 循环继续用 dev 的 `redteam`，M21c 切回。
**aacr-bench 暂退役**（fork `itcook/aacr-bench` 承载数据、评估器、`reviewers/redteam.py`、子集 JSONL；事实见
research §14）。重启点：**M21c 默认翻转前跑 `smoke`（5 正，legacy / v2 各 N=1，mock judge 取 Line-P/R）；M21e 收官跑
`gate`（规模按 `--pack-only` spike 定，legacy / v2 / ocr 三方，GLM judge，`--eval-rounds 3`，负样本 N=1）**。
ocr 参照 = deepseek-v4-pro 直连 chat-completions 流式，版本钉入元数据。

---

## 17. 残留未决（grill 前列出）

| # | 未决 | 方向 |
|---|---|---|
| R2 | reviewer cell 是否保留 `needs` 二阶段 | 沿用 two-stage；触发率实测后定 |
| R3 | 分组阈值本仓校准 | ocr 默认起步（4/200/10），dogfood 调 |
| R4 | 疑点 schema 细节 | **部分已裁〔D11〕**：category L0-L5、quote 逐字 + 丢弃；`hint` 归一化与合并 id 稳定性 → M21c 台账数据 |
| R5 | 风险规则初版 + 预算函数形状 | **已裁〔D11〕**：规则 v4、涟漪二梯队、两类信号、常量化、三档 4 / 12 / 32 帽 48（§3.3 / §6） |
| R6 | `--risk all` 对照档等价性 | **销案〔D11〕**：无 `--risk`；对照 = dev 二进制并排跑 PR181 比 oracle 命中 |
| R7 | slice 回退信号阈值（`needs` 触发率、oracle 命中差）与 `sliceMinBytes` | M21a A/B 与 dogfood 定 |
| R8 | LLM 语义分组何时值得开 | 跨组 import 边数阈值，dogfood 定 |
| R9 | provider `maxConcurrency` 缺省值（10 起步；角色 `workers` 已删〔D13〕） | 网关限流实测定；429 排队重试的收敛门宽是数据来源 |
| R10 | L4-B 非 Rust 生态定位链 | Rust 先行 |
| R11 | 必挖集阈值 `c_hi` | **已裁〔D11〕**：置信不做门；必挖 = 主区 ∪ agreement ≥ 2；疑点保底 ⌈maxCells/4⌉ 格 |
| R12 | 台账激活上限 N 与停车通道形态 | 镜像 ship AskUser |
| R13 | （已消：流式持久化无旋钮〔D10〕） | — |
| R14 | 实现债 | **已裁〔D11〕**：cell 记录 `role` + 角色分叉输出枚举；单独表 = 疑点台账 + 队列快照；coverage 进作业元数据；resume 读快照重跑未完项；schema v4 一次重置〔D10〕。**resume 离线续跑〔D15〕**：补跑缺失 scanner cell → 全部成功 scanner 记录的疑点幂等并入快照队列（按到达键去重）→ 本次补跑过 scanner 则复位 Skipped、按剩余预算重新 drain，否则沿用 → 深挖；快照为空但配了 reviewer 走同一路径（H0 按当前 diff 重算）；scanner 仍缺失时深挖照跑、verdict Incomplete（§8.4） |
| R15 | findings-recall 度量 | aacr 重启后接管；此前 PR181 三方并集 + proj.db upheld 小 oracle |
| R16 | provider RPM 令牌桶 | 网关按 RPM 计费时再上；并发密度限流已由 429 排队重试（§12.3〔D13〕）兜 |
| R17 | google 原生后端 | M20 provider 拓展 |
| R18 | diffsym 补语言（Python 等） | 语料需要时 |
| R19 | 交互式配置（verifiers 选项 Auto / New Model / Same As Scanner / Same As Reviewer；`toml_edit` 保注释） | M20 |
| R20 | aacr `gate` 规模与 `--pack-only` spike | M21e 前 |
| R21 | 非结构性缓存优化（provider 专属 context caching、跨 run 预热、DeepSeek 磁盘缓存调参） | M20；结构性部分已在 M21a |

（R1 第二家族默认已随 §5 定为「列表即活跃集」，不再是开关。）

---

## 18. 关联

- research：`../research/2026-09-04-redteam-review-engine-architecture-research.md`（§13 rpiv、§14 aacr-bench）。
- decisions：`../decisions/2026-09-04-redteam-review-engine-architecture-decisions.md`（D1-D13）。
- 交叉评审输入：`.rpiv/artifacts/research/2026-09-04_10-26-56_llm-code-review-multi-agent.md`；`~/llm-code-review-architecture.md`。
- 基准 fork：`~/codes/aacr-bench`（`itcook/aacr-bench`）。
- 前置设计：`2026-07-06-redteam-context-management-design.md` · `2026-07-13-redteam-no-tools-default-design.md` ·
  `2026-07-03-redteam-incremental-rounds-design.md` · `2026-07-03-dismissed-ledger-machinization-design.md`。
- 评审记录：`../private/reviews/2026-09-04-pr181-three-way-redteam-ocr-rpiv.md`。
- roadmap：工作树 `.roadmap/`（M21，锁定后登记 `.doc/roadmap/`）。
