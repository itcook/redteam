# 轮次增量协议（incremental rounds）设计 — ship 成本重构支柱 1

2026-07-03 · grilling 会话定稿（Q1-Q8），同日 plan-eng-review + codex 双评审加固
（R1-R6，**已并入下文决议正文**）。分支 `feat/incremental-rounds`。
上游：`.doc/research/2026-07-03-cost-redesign-five-pillars.md`（五支柱重设计，本文 = 支柱 1）、
`.doc/research/2026-07-03-token-cost-tree-sitter-research.md`（成本实证：单 ship 会话
输入 17.1M / 输出 0.6M tokens = 28.5:1，R× 全量重审是最大单项乘数）。
**评审记录**（R1-R6 决策过程/代码证据/codex 十漏点处置/覆盖图/失败模式/任务分解）：
`.doc/reviews/2026-07-03-incremental-rounds-eng-review.md`。

**目标**：ship 会话总 input tokens 较同规模历史会话降 ≥50%（设计估算 ~2.5×，验收线取保守值）；
同时 approve 天然可达——自指尾流问题（20260702 会话 7 轮不可达的根源）随全量重审语义一并消解。

## 0. 核心论证：锚定不变式

> 任何使旧 finding 的 quote 锚定失效的代码改动，必然位于下一轮的评审范围内
> （它就是轮间 diff 的一部分）。

覆盖闭环：未动代码 → 旧 findings 重锚定成功、携带前轮判定；已动代码 → 即 delta，
被新一轮全对抗火力审。r1 全量深审建立基线，r2..rn 只审增量。

**成立前提（评审 R1/R2 定）**：① **rename 须显式处理**——纯 rename 使旧路径锚定失效而
delta 只含 rename 元数据，不迁移则真问题静默蒸发（决议 2/3 的 rename 映射与迁移即为此）；
② **基线必须是干净全量轮**——partial/半失败轮有覆盖缺口，不得作 carry 源（决议 7 健康基线守卫）。

**已知缺口与接受理由**：缺口 = 代码未动但语义被远处 fix 破坏（跨文件耦合）。接受依据：
① dogfood 实证——两个 session 全部「衍生」finding（661e3973 色码、5361b545 注记溢出、
manifest 前导逗号 nit）均落在 fix commit 自身代码内，零远程破坏案例；② r1 全量已对整个
变更集审过一次跨文件耦合；③ ship 每轮测试绿是独立防线（fuzz 配方 property 测试即回归资产）；
④ 逃生门 = 可选全量终扫（见决议 1）。

## 1. 决策记录（grilling Q1-Q8，评审加固 R1-R6 已并入对应条目）

1. **approve 语义 + 终扫可选**：approve = r1 全量清 + 之后每个 fix-delta 清 +
   无 open carry（`compute_verdict(carried ∪ new)`）。**不默认全量终扫**——成本回吐一整轮
   且重新引入不收敛风险；发版级信心 = 用户手动裸跑一次全量 `review`（零新机制，ship.md
   一句可选步骤）。
2. **零邻域 + rename 感知 delta + 权威 stamp**：
   - rN 评审输入 = `diff(上轮 stamped HEAD .. 当前 HEAD)`，自然包含轮间用户手动改动
     （谁改的都是新证据）。不做 diff 范围邻域扩张：同目录/同词干信号与语义波及弱相关
     （否决 1 跳）；引用级邻域等支柱 2 索引落地后作为**卷宗内容增强**再评估，永不改
     评审范围语义。模型 tool-loop 读 fix 周边的能力不受影响（"零邻域"≠蒙眼）。
   - **rename 映射（R1）**：delta 计算取 `git diff --name-status -M` 的旧→新路径映射，
     供决议 3 的 carry 迁移——锚定不变式在 rename 场景成立的前提。
   - **stamp 权威落点（R3，推翻原「job 表加列」提法）**：权威时刻 = **证据收集实际所用
     HEAD**（`changed_files` 跑的那一刻 `rev-parse HEAD`），写 **result JSON 的
     `RunMeta.head_sha`**（serde default 兼容 v2，零 SQL 迁移）——与 findings 同产物、
     同事务，前台裸 review 与后台 job 统一。对全模式生效，非仅增量。
3. **携带 = 重锚定；销案 = resolve 标记**：
   - 上轮合并集逐条对当前树重锚定（复用 positioning）：quote 可定位 → 携带（行号更新，
     id 行无关；severity/agreement/verify/dismissed 状态原样，refuted/dismissed 本就
     不 blocking，携带无害）；定位失败 → 消灭进 `carried_dropped`（使锚失效的改动必在
     本轮 delta 内，正被审）。
   - **rename 迁移（R1）**：重锚前按决议 2 的 rename 映射把 `finding.file` 从旧路径迁到
     新路径，并迁移/重算 id——否则 resolved/dismissed/disputed 的 id 碰撞机制在
     rename 后全失效。
   - **多命中就近选（R4）**：carry 集记上轮 anchor 后 `line_start`，重锚定同文件多处命中
     时选最接近旧行号者（carry 专用 locate 变体；单一命中零影响）。
   - **销案**：ship 裁决「属实→修」后调 `redteam resolve <id> --from <result.json>
     --round rN`（与 dismiss add 完全对称：快照四件套、落 proj.db **独立表**——
     已修复≠误报，不混 dismissed、不进豁免/预筛）；resolved id 不参与携带。
   - 防假修复保险：resolved 且 quote 仍锚定成功的 finding → 注入下轮 delta 评审 prompt
     「顺带验证」（零额外 LLM 调用，搭现有评审的车）。
4. **增量轮 prompt 框架**：模板新增仅增量模式渲染的 `<incremental_round>` 段，rules/
   lenses/对抗立场/output contract 一字不动。内容四件：
   - Target 口径 = "fix delta on top of a previously-reviewed changeset"；
   - **双层 manifest**：全分支文件清单（复用现有 4KB 帽）+ 标注哪些在本轮 delta 内
     ——压视野塌缩型「无证据实现」误报；
   - **claimed-resolved 清单**（id+title+quote+file）+ 指令「验证是否真被解决；未解决则
     **用原 quote 原样再报**」→ 同 quote+title → 同 id → 与 resolved 记录碰撞 → 报告
     聚合为 `resolution_disputed`（与 dismissed_reappeared 对称）→ ship 按修复未生效
     重开重修。假修复由此获得显式回路；
   - **不给 hunk→finding 映射**（fix 意图不注入）：清单足以驱动验证，保留模型对 hunk
     真实效果的解读自由度；主任务钉死为对抗审 diff 本身，销案验证是顺带义务，防橡皮图章化。
5. **数据契约（schema_version 2→3）**：
   - finding 级：`origin: "carried" | "new"`；
   - run 级：`incremental: true`、`delta_base: <sha>`、`head_sha: <sha>`（决议 2 stamp，
     全模式）、`resolved: [ids]`、`resolution_disputed: [ids]`、`carried_dropped: [ids]`
     （锚定失效消灭者，审计用）；
   - `stats` 维持合并集口径（verdict 依据），另设 `stats.new` 子块；exit code 语义不动；
   - **refute 只打 `origin=new`**：携带项出生轮已过孤证 refute + 判例预筛；台账精确 id
     豁免每轮照常重跑（免费）；放弃"新增判例的变体命中旧携带项"的模糊匹配（接受小洞：
     其前提场景应在裁决内循环处理）；
   - **re-raise 不涨 agreement**：新 finding 与携带项同 id → 去重保留携带项、丢新副本、
     models 并集记录、agreement 不变。理由：跨轮重看非独立证据；agreement 保持"单轮内
     多模型互证"原语义，防孤证 blocking 的 refute 触发条件静默漂移。顽固性由"仍在携带集"
     这一事实本身表达；
   - M12 记分卡 `raised` 口径改 **new-only**，jq 须 **`(.origin // "new") == "new"`**
     兼容 v2 老报告（R6，缺省视 new）；
   - ~~status --format table 补 carried/new 计数列~~ **砍（R5）**：status 是运行时进度
     视图（只读 JobState），carried/new 是终态语义（住 result JSON），无数据源且关注点
     不同；消费方读 result JSON / rN.md。
6. **停机规则重构表**：
   | 规则 | 处置 | 理由 |
   |---|---|---|
   | verdict=approve（合并集）| 保留，**主出口** | 自指尾流结构性消失，approve 天然可达 |
   | 尾流守卫 | **退役**（增量模式）| 增量轮所有新 finding 定义上全是衍生 →「连续 2 轮全衍生且不增」会在健康收敛中误触发抢跑 |
   | 原始/衍生标注 + git blame 取证 | **退役** | 被 origin 字段 + delta 范围语义吸收；内循环净减负 |
   | 停滞守卫 / 升级刹车链 / 修复受阻 / max-rounds / cancelled | 保留不动 | 语义与增量正交 |
   | **新增：销案争议刹车** | `resolution_disputed` 同 id **第二次**出现 → 停交人 | 修复者与评审者死锁不该机器无限循环；对称"每 id 至多一次复裁"精神 |
7. **CLI 原语 + 适用范围**：
   - `redteam review --incremental-from <job-id>`（显式；ship 逐轮已记 job id，透传即可。
     不做隐式自动找上轮——决定论、可单测、裸终端可用）；
   - **前置守卫**，违反**显式报错拒跑**（不静默降级全量——成本意外）：
     ①谱系守卫：`merge-base --is-ancestor <stamped-head> HEAD`（rebase/reset 即锚定
     不变式失效，报错建议全量）；
     ②同族守卫：mode/scope 与上轮 job 一致；
     ③**健康基线守卫（R2）**：上轮 `verdict==incomplete`（partial——未跑 bundle 的
     findings 压根不存在）、`failed_models` 非空（半失败——对抗火力不足）、或 manifest
     未全 ok → 拒。锚定不变式只对「上轮全量审干净」的基线成立；
     ④空 delta（HEAD 未动）→ 显式报错，交 ship 走停滞守卫；
   - `redteam resolve <id> --from <result.json> --round rN` 子命令，dismiss add 对称；
   - 裸 `redteam review` 行为一字不动（增量纯增益路径）；`--help` 同步（在场文档原则）；
   - **v1 仅 code 模式**：doc 模式 id 稳定性弱（M7 实证 quote 概括化 → 跨轮 id 漂移，
     锚定携带地基不存在），传 `--incremental-from` 显式报错。doc 成本走 M6 渐进检索线。
8. **验收三件套**：
   1. **单跳回放**（验携带/销案/delta 机械正确性，有真值，成本 <1M tokens）：
      历史 session 20260703-004708 的 r1 findings 已在 proj.db（不重跑 r1）；按 r1.md
      裁决段补打 resolve/dismiss；对历史真实 r1→r2 delta（fix commit 在 git 历史）跑一轮
      增量。验收线：历史 r2 属实项（含衍生 661e3973/5361b545）100% 被捕获或携带；
      `carried_dropped` 逐 id 抽查确属 delta 内改动；input_tokens 对照历史 r2 的 2.7M。
   2. **合成靶 ×2**（新机械触发路径）：假修复靶（fix 不触 quote 且不解决 → 期望
      resolution_disputed）；争议刹车靶（同 id 二次 disputed → 期望停机交人）。
   3. **live 收编**（验收主体）：下一真实 ship 会话全程增量，new-only 记分卡对照历史
      两 session：总 input ≥50% 降、正常收敛（approve 或合理停机，无携带死锁）、
      驳回率不显著恶化。指标口径注记：L1（d579948）已把 input_tokens 归一为「总输入
      （含 cached）」且只降账单不减重发量——本验收量的重发量本身，与缓存命中率正交，
      历史基线（OpenAI 系口径）直接可比。
   明确**不做全会话回放**（单跳已覆盖机械正确性最险部分，剩余风险 live 常态暴露）。

## 2. 实施要点（供 plan 拆批参考，非完整 plan）

- **引擎**（大头）：`RunMeta` 加 `head_sha`（serde default）+ cli 在 `changed_files`
  处 rev-parse 回填；resolve 独立表（复用 dismiss 表范式：upsert/history/busy_timeout）；
  `review --incremental-from` 流水线 = 载上轮 result（head_sha/findings/mode）→ 四守卫
  → `git diff <sha>..HEAD` + `--name-status -M` rename 映射 → 现有 select_files/group
  复用 → fan-out 审 delta → 上轮合并集 rename 迁移 + 重锚定（positioning 复用 + carry
  专用就近变体，丢弃 id 收进 carried_dropped）→ 合并去重（同 id 保留 carried）→
  refute(new only) → verdict(合并集) → 报告（origin/聚合字段/schema v3）。
- **prompt**：`adversarial-code-bundle.md` 增 `<incremental_round>` 条件段；
  `other_files_manifest` 双层化。
- **ship.md**：起评审传 `--incremental-from <上轮 job>`（r1 照旧全量）；内循环「属实→修」
  后补 resolve 调用；停机节按决议 6 重写（删尾流守卫 + 衍生标注/blame 段，加销案争议刹车）；
  可选终扫一句。codex 镜像 `make gen` 重生成。
- **复用清单与任务分解**（T1-T7）见评审记录 §6/§7。

## 3. 测试与验收

- **hermetic 单测**：四守卫（谱系/同族/健康基线×3/空 delta）；重锚定携带与消灭（quote
  移位携带、quote 消失进 carried_dropped）；rename carry file/id 迁移（纯 rename 内容
  不变 → 存活指新路径、id 一致）；carry 重锚多命中就近选（同 quote 两处选最近旧行）；
  resolved 排除携带；同 id 去重不涨 agreement；resolution_disputed 聚合；refute targets
  只含 new；stamp = 证据 HEAD 前后台一致；`<incremental_round>` 段仅增量渲染断言
  （T4 模式）；verdict 合并集回归；feedback jq v2 兼容（无 origin 字段不掉不混）。
- **live**：决议 8 三件套。
- 完整覆盖 diagram 与失败模式表见评审记录 §4/§5。

## 4. 非目标

- doc 模式增量（id 稳定性前置未解）；邻域扩张（等支柱 2，且届时是卷宗增强非范围扩张）；
  默认终扫；全会话回放；跨轮 agreement 加权；卷宗/结构上下文包（支柱 2，另行立项）。
- **whitespace-only 语义变化**（quote 行被改但 normalize_ws 折叠后仍锚中，R4 P3 尾巴）：
  代码空白多数非语义、误判概率低，live 观察真出现再定，不预先堵。
- **ship.md 停机看 exit code 的加固**（verdict=approve 但 exit=2 半失败）：既有协议问题
  非增量引入；R2 已堵住「半失败作 carry 基线」的一半，停机条件本身归独立顺手项
  （`.doc/TODO.md`），不进本批（一批不混两个协议变更）。
- 不动：裸 review 语义、compute_verdict 函数签名、exit code 契约、台账/判例机制、
  refute 语义前缀契约。

## 5. 执行方式建议

规模 = 引擎（store/carry/merge/CLI）+ prompt + ship.md + 三件套验收，跨 storage/engine/
protocol 三层——先出 plan 文档拆批（house 流程 design→plan），实施走 subagent-driven：
①stamp+resolve 存储 ②carry/merge 引擎（含 rename 迁移）③CLI+四守卫 ④prompt+ship.md
⑤验收。每批 hermetic 绿再进下批，live 三件套收尾。

**并行化**：批①-④共享 `src/cli.rs` 与 `report.rs`，文件级高耦合 → **主线顺序**；唯一
独立支线 = feedback.md jq 兼容（plugins/ 壳层）。单 worktree 顺序推进即可。

**交付门槛**：3 个 P1 加固（rename 迁移 R1 / 健康基线守卫 R2 / stamp 权威 R3）分别落在
批②③①，未落地不得进 live 验收。
