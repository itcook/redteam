---
date: 2026-09-04T10:26:56+0100
author: S.Kai
commit: 88aaf16
branch: dev
repository: redteam
topic: "LLM 代码评审多 agent 架构：scanner/reviewer/advisor 三角色分层编排的第一性原理重设计"
tags: [research, codebase, orchestration, multi-agent, tps-tiering, recall]
status: ready
last_updated: 2026-09-04T12:53:54+0100
last_updated_by: S.Kai
last_updated_note: "Added follow-up research: 快机制澄清（thinking 外显化/疑点粒度 fanout）、K 值预算驱动定论、调度归属（LLM 声明/编排器完成）、并发条件用户确认排除"
---

# Research: LLM 代码评审多 agent 架构——scanner/reviewer/advisor 三角色分层编排的第一性原理重设计

## Research Question

基于 `~/llm-code-review-architecture.md`（PR181 四工具实战对照提炼的初步方案）进一步明确 TPS 分层：multi-agents 分三角色 scanner（扫描）/ reviewer（深挖）/ advisor（验证），各自独立提示词、工具共享。用户核心担忧：**workflow 非常依赖第一步扫描结果，快模型宽召回但仍可能漏 bug**。要求从第一性原理出发（如何又快又好地用 LLM 做代码评审），不怎么考虑现有实现，参考同类工具，整理出 agents 编排/工作流（含必要工具）、context 规划与管理等核心架构逻辑。

## Summary

1. **架构文档的核心结论全部经独立重审后成立且被外部证据强化**：input/output 经济学不对称律、扫描-深挖两遍法、TPS 分层、增量提交，与业界收敛形态（BitsAI-CR RuleChecker→ReviewFilter、Qodo 专门 agents+judge、Multi-Review 论文 ensemble 实证）同构。
2. **漏报问题的第一性拆解**：漏报分四根源（模型能力上限/注意力稀释/diff-scoped 盲区/分治切割损伤），现有全部后处理机制（dedup agreement→cross-verify→判例台账）只作用于「至少一个模型报了」的集合——**漏报是其补集，post-hoc 处理触不到，缓解必须发生在生成侧**。
3. **对核心担忧的结构性回答**：疑点并集（union of suspicions，把生成门槛从「完整六字段 finding」降到「文件+行号+一句话+置信度 <50 token」）+ 疑点台账跨轮激活（单轮漏报从「终局」变成「未激活的休眠项」，delta 触及相关文件即重扫）+ coverage 五集合契约（漏报从不可见变为可归因）——三者组合把「单点依赖第一步」改造成「逐轮修复的依赖」。
4. **编排拓扑定论**：三阶段独立 orchestrator（scanner 组粒度扇出→reviewer 疑点粒度扇出→advisor finding 粒度）以持久化中间产物（疑点台账）衔接，严格优于单 orchestrator 三级嵌套（恢复/观测/配置三轴全占优）。needs 二阶段（`src/orchestrate/mod.rs:210`）与两遍法控制流不能统一（扇出粒度/模型路由/合并语义/检查点四维不同），但 schema 层与证据抓取机器层完全共享。
5. **用户定调（Developer Context）**：倾向完全重构（不受渐进兼容约束）；漏报缓解机制直接上（不卡在 oracle 先行）；**角色基数不对称——scanner/reviewer 可多模型，advisor 单模型即可**（原 `--models` 全链复跑工作流在新架构中消亡）。
6. **已知最大缺口**：`eval/harness.py` 只测 dossier 召回不测 findings 召回——漏报缓解效果暂不可量化，需后补 oracle（历史 dogfood findings 集）。

## Detailed Findings

### 1. 第一性原理重审（外部证据交叉验证）

- **不对称律**：PR181 全部事故（524/600s 超时/max_tokens 截断烧 cell）在 output 侧，input 侧（145K 卷宗、Kimi 99% cache 命中）从未爆。业界侧证：augmentcode 指出 agent-first review 改变错误候选的单位成本——过滤发生在评论产生之前，这是两遍法经济学的独立表述（来源见「外部工具证据」节）。
- **两遍法**：Tencent 实测 LLM 过滤静态分析告警消除 94–98% 假阳且 recall 保持；BitsAI-CR（FSE 2025，ByteDance 12000 WAU）正是 RuleChecker（宽检）→ReviewFilter（验证）两阶段；Qodo 商业版 = 专门 agents 并行 + judge agent 合并去重过滤低置信。发现便宜/论证贵的分层是业界共识形态。
- **ensemble 互补性**：Multi-Review（arXiv 2509.01494）实测 Multi-Agg（多模型聚合）与 Self-Agg（同模型多次）均显著提升 F1（n∈{1,3,5,10} 单调改善）；架构文档 PR181 三方独有 findings 互补是同现象的工程实证。
- **TPS 分层**：HackenProof 审计管线按任务分模型档（opus/sonnet/haiku 各司 reentrancy/access-control/overflow）；实测 DeepSeek flash 103 tok/s vs GLM 27 tok/s（4 倍耗时差）支撑快扫慢挖。
- **验证者独立性**：zylos 综述与 arXiv 2510.11822（agreeableness bias）确认 Critic/advisor 必须独立会话、无生产者推理历史——redteam 的 `pick_cross_model`（`src/consolidate/verify.rs:149`）语义正确，advisor 单模型不损害这一原则。

### 2. 漏报四根源框架与缓解机制映射（核心担忧的对症）

| 漏报根源 | 机理 | 对症缓解 |
|---|---|---|
| ① 扫描模型能力上限 | 想不到这类 bug | 多模型疑点并集（「想到」集合差异远大于「论证成功」集合差异） |
| ② 注意力稀释 | 看过没报（单轮 output 集中/context 大） | 两遍法 + 疑点索引化（发现/论证分离） |
| ③ diff-scoped 盲区 | bug 在 diff 未触碰文件 | 邻接 full-file 扫描（触发式，dossier 邻接集） |
| ④ 分治切割损伤 | 同 bug 证据被切两组 | 跨组 import 索引（正反向）+ dedup 合并 |

- **关键观察**：redteam 现有 replicate findings 并集只提升已发现项的置信度（`src/consolidate/dedup.rs:66` merge_into，agreement=models.len()），对「没人发现」无能为力。完整 finding 的六字段契约论证门槛高——快模型「知道哪里可疑但写不出来」的信号在 findings 并集里蒸发；疑点索引把门槛降到 <50 token/条，任一模型列出即入池。
- **疑点台账跨轮激活**：疑点与 finding 同构（file+quote+title+行号自然键），carry 机器（`src/consolidate/carry.rs` anchor_near 重锚、rename 迁移）零改动适用。重扫触发规则 = `delta ∩ 疑点锚定文件 ≠ ∅`（claimed-resolved 假修复保险 `src/cli/incremental.rs:72-79` 的推广——旧状态按条件注入新轮 prompt、搭车验证、零额外调用）。召回率随轮数单调改善。
- **coverage 契约**（ocr 五集合 selected/completed/reused/failed/waived，`~/codes/open-code-review/internal/session/manifest.go:265-268`）：redteam 零件 80% 已在——`CellSink.manifest`（`src/orchestrate/mod.rs:16`）扇出前写全量清单、`on_cell` 流式落盘、增量 preflight（`src/cli/incremental.rs:215-226`）已做 selected-vs-completed 校验。缺五集合语义显式化与 verdict 的 partial 档联动。
- **多角色覆盖分级**：scanner 覆盖不完整 → 硬 Incomplete（top-K 选择集建立在部分扫描上=结构性选择偏差，无法事后校正）；reviewer/advisor 覆盖不完整 → Approve-with-debt（疑点已台账化，增量轮天然重试，降 warning+RunMeta 披露不烧整轮）。
- **eval 缺口（负面发现）**：`eval/harness.py` docstring 自述「穷人版卷宗查全率评测」——度量 dossier 层（changed symbols 引用点抓取率，`rg -wF` + lsif ground truth），非 findings 层。用户已决策机制直接上；oracle（历史 dogfood 已确认 findings 做 ground truth，复用 harness 的 materialize+分桶框架）作为后续补齐项。

### 3. 三角色编排拓扑

- **结论：三阶段独立 orchestrator + 持久化中间产物衔接**，不扩展 OrchestrateOpts/Phase 做单 orchestrator 三级嵌套。理由四轴：
  - CellRecord 装不下疑点（`src/state/mod.rs:142` findings: Vec<Finding> 是 proj.db 持久化契约），嵌套需 schema 迁移成联合体；
  - resume（`src/cli/mod.rs:506`）语义「manifest 差集+存档 prompt 重放」被迫理解阶段转移；
  - 模型计划无法分级（per-fanout 单一模型计划 `src/orchestrate/mod.rs:33-39`，TPS 分层需阶段边界换 opts）；
  - 三阶段各自天然 manifest/partial 对：scanner cell 完成即落疑点（快模型的钱被杀也不丢）、reviewer 疑点完成即落 finding、advisor 已有 refute usage 记账（`src/consolidate/verify.rs` 独立 Semaphore）。
- 漏斗形状：scanner 组粒度高并发（快模型便宜）→ reviewer 疑点粒度低-中并发（慢模型贵）→ advisor finding 粒度低并发/串行（verify 便宜一个量级，单模型）。
- Phase 枚举扩展 `Collecting→Scan→Review→Finalizing→Refute`（`src/state/mod.rs:41-46` 加变体，serde lowercase 向后兼容旧 state）；phase_history 免费获得漏斗时间线。
- **needs 与两遍法的关系**：控制流层不统一——①扇出粒度 1:1（cell 级重跑 `maybe_second_pass`，`src/orchestrate/mod.rs:210`）vs 1:N（疑点级扇出）；②模型路由同链锁死（rerun 闭包传同一 opts，`src/orchestrate/mod.rs:328/:379`）vs 跨档；③合并语义整体替换（:237-249）vs 并集；④检查点扣押 vs 流式落盘。数据层统一——NeedEntry（`src/domain/finding.rs:50-55`）升级为疑点 schema（加 hint/confidence/kind 或独立 Suspicion 内嵌 evidence_requests），LensOutput（:59-64）加 suspicions 数组（serde default 零漂移）；`second_pass_prompt`（`src/cli/second_pass.rs:132`）的读守卫（:45 git-tracked∧非 denylist）、±30 行窗、64KB 段帽等证据抓取机器整体复用为深挖遍定向装配。needs 保留为小 PR 快路径。
- **分派点在 orchestrate 层不在 provider 层**：扫描遍走 single_shot 形态（无工具单发，`src/provider/single_shot.rs:28`），深挖遍「确定性证据预取+single_shot」或 tools=on 皆可；`parse_lens_output` 随 struct 走零改动。
- **角色基数（用户定调）**：scanner 可多模型并跑（疑点并集）、reviewer 可多（疑点分派/并行深挖）、**advisor 单模型**——裁决不需多数派，独立单裁决者+判例即可，且单 advisor 避免裁决冲突。`--models` 全链复跑工作流消亡。

### 4. 输出协议：增量提交

- **submit_finding + submit_suspicion 工具化 + grace round**。现状事故链（终态大 JSON→max_tokens 截断→parse fail→repair 烧双倍→烧 cell）的三层兜底（early-repair/FINALIZE/FINALIZE-repair，`src/provider/tool_loop.rs:107-190`）全部为一次性大 JSON 服务；工具化后结构性退役：
  - FINALIZE → ocr 式 grace round（预算耗尽→收尾指令+工具集收缩 {submit_finding, finish}，单轮 output 有界；`~/codes/open-code-review/internal/llmloop/loop.go:479-540`）；
  - REPAIR → 参数级自愈（单条 tool call 校验失败以 error result 回灌，模型修单条重发，无「再吐一遍全部」）；
  - `finish_reason` 降级为观测信号（传输层截断检测留 backends；per-turn length 截断入 CellRecord/警告）。
- **诚实的限定**：工具化不减少 findings 总 output token（body 在 tool args 里同样是输出），消灭的是单轮截断形态、cell 中途死亡时已提交 findings 的持久性、parse 失败爆炸半径。真正砍 output 总量的是两遍法（疑点索引）——submit_suspicion 是两遍法安全的地基，两者配套。
- **ocr 的坑要避开**：`code_comment.go:130-132` 对缺 path/content 条目静默丢弃——宽召回架构里这是漏报源；redteam 应逐条校验（severity 枚举/quote 非空/行号≥1）+ error 回灌修正重发。
- needs 不必工具化（终态微 JSON `{"needs":[...]}` <1KB 截断免疫，single_shot 二阶段契约零改动）；**疑点索引必须工具化**（新的批量输出，否则两遍法自己重演终态 JSON 事故）。
- 生产热路径事实：当前默认 tools=two-stage 走 SingleShotProvider（`src/main.rs:427-432`），工具环是诊断档——完全重构下 submit_finding/submit_suspicion 随三阶段转正。

### 5. Context 管理与 prompt cache

- **现状段序已满足稳定性降序**（实测逐段标注 `src/prompt/templates/adversarial-code-bundle.md`）：编译期常量（role/rules/finding_bar，L1-52）→ run 级稳定（spec_docs L25-27、focus）→ 轮变（incremental_round L73）→ 变更集级（bundle L75-86、dossier L88）。「spec 在 diff 之前破坏 cache」的担忧不成立——spec 恰在 diff 前且 run 级稳定，正确。
- 工具环 append-only 历史（`src/provider/tool_loop.rs:88-91` 只 push 不重排）对 cache 已是最优——每轮请求是前轮的前缀扩展；redteam 预算前置治理（pack 帽 768K + spec 帽 64K min(budget/2)）换掉 ocr 式运行中压缩（中途重写 messages=前缀全 miss），cache 维度净赢。
- **三角色共享同一卷宗前缀、只分化尾部指令**（一次 pack 三角色复用）：pack 是 hermetic 纯函数（`src/pack/mod.rs` 零 LLM）复用零成本；各建卷宗=最大段（dossier 全文）三份全 miss。**需要一次段序翻转**：现状角色协议段（`<output_contract>` 等）在头部、证据在尾部；三角色 contract 必然分化（疑点索引 schema vs findings schema），分化段在证据前会把共享前缀截断在分化点——把 output_contract+角色专属指令移到 dossier 之后（或收进各角色 System 尾部），形成 `[共享: role/rules/stance + spec + bundle + dossier] → [角色尾: contract+指令]`。这不是美学重排而是成本结构（跨请求 cache 命中是快模型扫描遍便宜化的前提）。
- FINALIZE 轮撤 tools 声明本身破坏 Anthropic 缓存前缀（tools→system→messages 顺序，`src/provider/backends.rs:75-86`）——随 FINALIZE 退役自动消失；Anthropic cache_control 断点实现当前三 provider 全 openai kind 走不到（dogfood 实测 50% 命中基线见 precedents）。

### 6. TPS 角色路由

- **per-role binding 定论**：`[roles.scanner/digger/verifier]`，role 携带「模型名 + 角色级 thinking_level 覆盖 + 同档 fallback 列表 + 可选 replicate 列表」。决定性论据：**thinking_level 是角色属性非模型属性**（同一物理模型可能在 scanner 位跑 low、digger 位跑 high；挂在 `[models.X]` 上则无法同时两档消费——现 schema `src/config/mod.rs:52,84` 先行零消费的债只有角色化才能还）。
- 候补链角色化后退化为**同角色内 fallback**，且 TPS 档位成为候补合法性约束（DeepSeek flash 失败回退 GLM = 4 倍耗时静默 inversion，分层经济学报废）。
- **replicate 新语义（用户定调）**：原「全链复跑」消亡；scanner 位多模型并跑=疑点并集（对症漏报①）；reviewer 可多（疑点分派）；advisor 单模型。`resolve_models`（`src/cli/mod.rs:132-163`）四分支与 `--models`/`--models-all` CLI 面在完全重构中重新设计。
- 配置层形状（要点，完整草案待 design）：通道层 `[providers.X]`（kind/base_url/api_key_env）与模型层 `[models.X]`（context_window/output_limit/per_turn_timeout_secs/reasoning/thinking_level/priority——模型固有属性）不变；新增角色层 `[roles.X]`（model 引用+fallback+replicate+thinking_level 覆盖）。validate 聚合报错引用存在性；项目层安全剥离清单（`src/config/mod.rs:222-231`）必须加 "roles"（防项目配置注入评审旋钮，dogfood ccbedddcdd01 F2 教训）。
- registry route_key 机制（`src/provider/registry.rs:31-35`）已支持任意粒度路由，缺的只是编排层按角色发不同 route_key；build_registry（`src/main.rs:384-435`）注册集改为所有角色引用模型 ∪ 显式 override。

### 7. 验证层与判例

- **advisor = refute 趟角色化 + 两个真新增**：同构面全继承（单对象进三值出、pick_cross_model 独立选法、source_snippet 512KB/64KB/±60 行三级帽、判例注入、嗅探兜底、失败保守 Upheld）。真新增：①裁决对象从 finding 扩到疑点（三元动作：升级进深挖队列/降级低优先池/丢弃）——**top-K 选择权从扫描遍自评置信度移交独立裁决者**，消除单模型自评偏差（扫出者高估自己的疑点）；②ThinkingLevel 档位化的第一个真实消费者（高置信疑点 high 档、低置信 low 快筛、判例命中降级快筛确认）。
- **疑点格式约束**：quote 必填——保住 matches_precedent 的 quote 信号（`src/consolidate/verify.rs:138`，复用 `dedup.rs:23` quote_overlap 口径）与跨模型疑点合并锚定；纯文字疑点（无 quote）的合并 id 稳定性弱于 finding_id（`src/orchestrate/mod.rs:67-77`），需要 quote 轻锚。
- **判例预筛前移**：现状 matches_precedent 只在 refute_targets 调用（`src/consolidate/verify.rs:206-223`）——判例变体走完完整深挖产出 finding 后才被压，预算白烧。前移形态=「机械降级 + advisor 快筛确认」而非直接丢弃（机械匹配对换措辞天然半盲，直接丢弃会把假阴变成新漏报面——matches_precedent doc :134-137 自述只做候选不做终审）。附带：扫描遍 prompt 注入同 file 活跃判例摘要（现仅 refute 模板见判例 `adversarial-refute.md:13-16`），假阳骚扰从下游压制变上游不生成。
- ocr review-filter 的可抄细节：`analysis` 字段强制先于 `comment_ids`（先推理后承诺、不可撤回）——redteam refute prompt 可借鉴。

### 8. 分组与跨组索引

- **LLM 语义分组挂在 `group()` 与 `merge_bundles()` 之间的缝隙**（`semantic_regroup`）：redteam 与 ocr 的根本差异——ocr 从零分区（LLM 买全部分区信息），redteam 已有免费先验（union-find 三信号：同目录/同词干/相对 import 互引，`src/bundle/mod.rs:262`），LLM 只买边际信息（先验分错的部分：`extract_imports` 只认相对 import，TS 裸模块路径/Rust use crate/Go 包路径不解析，`src/bundle/mod.rs:206-208` 注释自认 MVP 缺口）。**触发阈值可免费度量：数 union-find 分组边界上的跨组 import 边数**（边已算出只是分组后丢弃）——跨组边多=先验切错=调用有信息可买；为零=白花钱。失败回退地板是确定性先验分区（质量高于 ocr 的 per-file 回退）。
- ocr 双阈值哲学的后半句要补：churn 注意力上限（`BUNDLE_LINE_THRESHOLD` 问「组内能否共享一次评审注意力」）在 redteam 无对应物（pack 预算管 token 不管注意力）——semantic_regroup 输出侧加 churn 检查。
- **跨组关联索引：零件已存在但被丢弃**。`extract_imports`（`src/bundle/mod.rs:208`）已产出有向边 A→B，只在 group() 内消费一次后丢弃；索引=把边集提升为流水线级产物：(a) 边计算前提到 group() 之前、两处消费；(b) 渲染进现成 `{{OTHER_CHANGED_FILES}}` 槽（`src/prompt/mod.rs:146`）升级为 `path → imports [...]` 形状（该槽 per-group 变化、位于尾部不威胁前缀稳定）；(c) **反向索引**（谁 import 我——BTreeMap 反转）注入「组外引用本组文件的源文件清单」，这是 manifest 无法表达的 manifestation-site 信息（A 文件 bug 在 B 文件暴露）；(d) 增量轮 `[delta]` 标注（边的目标在 delta 内=需联动审视 vs 未变=前轮已审）——把 `<incremental_round>` 段 "delta interacts with them"（`src/prompt/mod.rs:246`）从模型猜测变成给定事实。
- body_section 回落链（全文→def 体→骨架→hunk 窗→omit，`src/pack/mod.rs:26`）结构无需重组（预算驱动逐文件降档与组粒度正交）；旋钮：组内填充顺序改按「与组内其他文件 import 边数」排序（枢纽文件保全文）；omit 清单升级为「降质+留指针」（被省略文件的指针在跨组索引里，且 omit 条目天然是 needs 声明提示）。
- E9 单格全审默认（`merge_bundles` 压扁，`src/cli/mod.rs:200-205`）与三角色化的关系：E9 判掉的是「N 倍重复喂同一全景」；三阶段共享同一份 pack 卷宗（见 §5），不违反 E9。

### 9. 增量轮与疑点台账

- 现状五步流水（preflight 四守卫→delta fan-out→carry/merge→refute(new-only)→verdict，`src/cli/incremental.rs:20`）+ claimed-resolved 假修复保险（:72-79，零额外调用的状态回流）。
- **疑点台账化**（proj.db 新表，镜像 DismissCard/ResolveCard 模式）+ 条件重扫（delta∩锚定文件≠∅ 触发，注入措辞照抄 claimed 的搭车定位防稀释 delta 主任务）。两个守卫扩展：preflight 的 prior.verdict≠Incomplete 检查覆盖疑点台账（扫描不完整的上轮疑点不可信）；**休眠疑点绝不进 verdict**（verdict 纯函数只看 findings 的纯洁性是资产，疑点不 blocking 只进 RunMeta 披露）。
- SecondPrompt 闭包（`src/cli/incremental.rs:110`）已是完整两角色流水的雏形（第一趟草稿+证据请求→构造器定向装配→第二趟 CONFIRM-or-REVISE），示范了正确的状态传递原语：**状态=带机器可校验契约的渲染段 + 字节稳定段透传**（保 cache 前缀）。三角色沿用此原语而非发明消息历史。缺三样（三角色要补的）：reviewer→scanner 精炼重扫通道（needs 路由到「换焦点重扫」而非「读文件」）、轮内反驳通道（现判例只跨轮回流）、增量合并语义（替换→advisor 只对存活 top-K 补充论证）。
- **收敛重定义**（三正交维度）：无 blocking findings（verdict 纯函数不动，exit_code 五档契约不污染）∧ 疑点台账无 delta 邻接未处理项 ∧ 激活计数无超限项。活锁守卫：低置信疑点被 delta 反复激活→深挖不晋升→记激活次数，超 N 次强制升档 advisor 或停车交人（镜像 ship.md re-upheld→AskUser 通道）。

### 10. 机制引入决策表（ocr 对照逐项裁决）

| 机制 | 裁决 | 要点 |
|---|---|---|
| per-role 绑定 | **立即** | thinking_level 只有角色化才可消费；route_key registry 已就绪 |
| 确定性跨文件重定位 | **立即（零成本）** | ocr `resolver.go:100` RelocateAcrossFiles 唯一命中语义（多命中/零命中拒绝）——redteam anchor 只查自身文件，decl/impl 分离场景纯字符串匹配即可救 |
| LLM 兜底 re-location | 有条件 | 仅 severity≥Medium∧conf≥floor 的 anchor 失败项触发、限一次、产出必过 locate_quote 确定性复校验（ocr 回滚守门 relocation.go:86-89）；anchor 失败=最贵的漏报点（发现成本已付） |
| scanner 多模型 replicate | 随两遍法 | 疑点并集对症漏报① |
| PLAN_TASK 独立阶段 | **不引入** | 两遍法下疑点索引信息量严格大于 plan checklist（评审中产物 vs 评审前产物）；偷三样：双阈值触发思想（何时值得两遍法）、scanned_areas 覆盖确认字段并入 scanner schema（零调用获得注意力对账）、round-2 剥离教训（上轮 findings 重注入不当 coverage ceiling） |
| MEMORY_COMPRESSION | **不引入** | input 侧机制违反不对称律（LLM 摘要是 output 支出+cache 前缀击穿）；redteam pack 帽+12 轮预算结构性不会滚到需压缩；findings 增量提交已接管其核心价值。重启条件记录：深挖做成长循环（>30 轮或逼近窗口）时先做确定性截断（tool results 可重 derive） |
| MaxToolRequestTimes/WaitTimeMs | **不引入** | redteam max_iters=12+FINALIZE 更严且带优雅收尾；WaitTimeMs 系 ocr 死配置（全仓无消费点）。可偷：空轮计数器（3 连续空轮提前收尾，~15 行防 doomloop） |
| tree-sitter | **缓** | diffsym 现状三语言（rs/go/ts，Rust 119/119 符号锚定已验证，`src/diffsym/mod.rs:13-49`）失败模式良性（错配=给错上下文段非崩溃，只影响降级路径质量）。先做：补 Python 条目（最大缺口）、block_end 加字符串/注释感知词法（~40 行/语言消灭括号盲区）。触发条件：>2 个非三语言主语言、或 omit/骨架质量可归因到 def 错配、或**实施跨组引用索引**（tree-sitter queries 是可靠 reference 提取的自然工具，def 边界升级搭车） |

### 11. 外部工具证据（web 调研汇总）

- **Qodo（PR-Agent 商业版）**：专门 agents 并行（correctness/standards/architecture/risk）→ judge agent 合并去重过滤低置信（docs.qodo.ai/code-review）；Rule Miner 从 PR 历史持续提炼团队规则（对照 redteam 判例台账的方向）。PR-Agent 开源版：token-aware 压缩（语言内按 token 降序装箱+剩余文件清单硬停、hunk 删优先、±3 行上下文扩展）——pack 预算治理同路。
- **BitsAI-CR（FSE 2025）**：Context Preparation→RuleChecker→ReviewFilter→Comment Aggregation 四步 + data flywheel；75% precision、Outdated Rate（开发者实际采纳率）作为自动评估指标——**采纳率反馈环是 redteam 判例台账缺的闭环**。
- **Multi-Review（arXiv 2509.01494）**：Multi-Agg/Self-Agg 均显著提升 F1；聚合器双目标——提升 recall（多评审一致标记的高置信项）+ 提升 precision（过滤零星单发项）。
- **Greptile**：全仓 AST 依赖图持续索引；PR 触发→提取改动 AST 节点→符号图查受影响 callers→diff+caller context 注入——跨文件破坏检测正是 redteam 跨组反向索引要解的问题。
- **CodeRabbit**：context 引擎（去重/压缩/排序/过滤，子任务 context 隔离不污染主线程）+ 验证层（建议对 code/团队规范/repo 配置校验后才达 PR）；语义块→关系图→cohort 排布。
- **Critic 独立性（zylos 综述）**：Critic 必须独立会话无 Builder 历史（继承推理上下文=丧失独立性）；2026 生产案例：Critic 独立性抓住了全部测试通过的性能 bug。
- **agreeableness bias（arXiv 2510.11822）**：RLHF 偏好模型编码「顺从用户验证」的伪相关——advisor 单模型也必须换模型族于生产者（pick_cross_model 语义保留）。
- **The End of Code Review（arXiv 2606.13175）**：LLM 会漏训练分布外缺陷且可能静默 approve——主要缓解=ensemble review（多独立 agent 共识）+ 校准的不确定性报告（允许 abstain）。支持疑点并集+coverage 契约。
- **Uber AI review at scale**：correctness bugs 高采纳、style nits 低采纳——类别分层设计（scanner 按缺陷类别分域的依据）；precision-recall dashboard 分阶段 rollout。
- reviewdog：注释聚合/过滤/dedup 的平台层分工（linter 生态）；pr-agent 开源版为单工具单 prompt 扁平结构（describe/review/improve 分离，JSON 配置可定制）。

## Code References

### redteam（本仓，相对 repo root）

- `src/orchestrate/mod.rs:15-24` — CellSink 四钩子（manifest/on_cell/on_phase/on_meta），流式 checkpoint
- `src/orchestrate/mod.rs:26-39` — PlannedModel/OrchestrateOpts（单模型计划+replicate，角色化改造对象）
- `src/orchestrate/mod.rs:151-201` — run_one_bundle 候补链（priority 降序首个成功即止）
- `src/orchestrate/mod.rs:210-264` — maybe_second_pass（needs 二阶段：cap5/整体替换/失败保第一趟）
- `src/orchestrate/mod.rs:246-257` — prompt 每 bundle 只算一次，manifest 与执行复用
- `src/orchestrate/mod.rs:67-87` — finding_id（quote 锚定去空白归一）与 enrich
- `src/provider/tool_loop.rs:56-190` — run_tool_loop（FINALIZE+REPAIR 三层兜底，增量提交改造对象）
- `src/provider/tool_loop.rs:29-31` — AssistantTurn.finish_reason（截断检测，降级为观测信号）
- `src/provider/single_shot.rs:28` — run_single_shot（off/two-stage 生产路径，扫描遍现成形态）
- `src/provider/parse.rs:10-117` — parse_lens_output + REPAIR_PROMPT + diagnostic_parse_error
- `src/provider/registry.rs:31-35` — route_key→provider 扁平表（角色路由已支持）
- `src/provider/backends.rs:287-299,573-585` — output_limit 消费点；:75-86 Anthropic cache_control 断点
- `src/config/mod.rs:35-39` — ProviderConfig（通道层三字段）
- `src/config/mod.rs:52,79-91` — ThinkingLevel schema（零消费）+ ModelDef 全套旋钮
- `src/config/mod.rs:222-231` — 项目层剥离清单（须加 "roles"）
- `src/cli/mod.rs:132-163` — resolve_models 四分支（--models/--models-all/default/候补）
- `src/cli/mod.rs:200-205` — merge_bundles E9 单格压扁
- `src/cli/mod.rs:254,257` — 全量入口两调用点
- `src/cli/incremental.rs:20` — run_incremental_review 五步流水
- `src/cli/incremental.rs:71-79` — delta 分组入口 + claimed-resolved 假修复保险
- `src/cli/incremental.rs:87` — 增量轮 resolve_models
- `src/cli/incremental.rs:100-107` — spec 段帽夹紧
- `src/cli/incremental.rs:110-112` — SecondPrompt mk 闭包（两角色雏形）
- `src/cli/incremental.rs:169-226` — preflight 四守卫（:215-226 selected-vs-completed 校验）
- `src/cli/second_pass.rs:45` — need_read_allowed（git-tracked∧非 denylist 读守卫）
- `src/cli/second_pass.rs:132` — second_pass_prompt（证据段装配：整文件前插/±30 行窗/64KB 帽/draft_block）
- `src/consolidate/verify.rs:33` — source_snippet（512KB/64KB 骨架/±60 行三级帽）
- `src/consolidate/verify.rs:138` — matches_precedent（机械预筛，召回优先精判交 refute）
- `src/consolidate/verify.rs:149-155` — pick_cross_model（交叉模型选法）
- `src/consolidate/verify.rs:206-223` — refute_targets（孤证 blocking/判例变体；carried 排除）
- `src/consolidate/dedup.rs:23,37,43` — quote_overlap/is_same_bug（跨 seam 单一真相源）
- `src/consolidate/dedup.rs:66` — merge_into（agreement=models.len()）
- `src/consolidate/carry.rs:26,54` — rename 迁移 + carry（anchor_near 就近重锚）
- `src/state/mod.rs:41-46,92,142` — Phase 枚举/phase_history/CellRecord（疑点放不进）
- `src/bundle/mod.rs:137` — BUNDLE_MAX_FILES=8
- `src/bundle/mod.rs:208` — extract_imports（TS/JS/Py/Rust mod 相对 import，边被丢弃）
- `src/bundle/mod.rs:262` — group（union-find 三信号：同目录/同词干/import 互引）
- `src/bundle/mod.rs:65,121` — other_files_manifest / two_layer_manifest（[delta] 标注+双帽）
- `src/pack/mod.rs:26` — body_section 回落链（全文→def 体→骨架→hunk 窗→omit）
- `src/pack/mod.rs:170-229` — build_pack 预算预扣与近→远填充
- `src/prompt/mod.rs:41,146,165` — build_refute_prompt / OTHER_CHANGED_FILES 槽 / render_precedents
- `src/prompt/mod.rs:214-253` — incremental_round 段（四件套+6000 字符帽）
- `src/prompt/templates/adversarial-code-bundle.md:67-71` — needs 输出契约
- `src/diffsym/mod.rs:13-49` — 三语言正则表（Rust 119/119 验证）
- `src/tools/mod.rs:64-100` — tool_specs（read_files/search_repo/list_files）
- `src/main.rs:384-435,427-432` — build_registry / ToolsMode 分派（默认 two-stage→SingleShot）
- `eval/harness.py:3` — docstring 自述 dossier 召回评测（非 findings 召回）

### open-code-review（外部仓 ~/codes/open-code-review，标注 `ocr:`）

- `ocr: internal/session/manifest.go:265-268` — Coverage 五集合（selected/completed/reused/failed/waived 不相交并）
- `ocr: internal/agent/agent.go:1693-1749` — executeGroupPlanPhase（受限工具集+失败软降级+round2 剥离）
- `ocr: internal/agent/agent.go:1346` — executeGroupSubtask（plan→多轮→每轮 review-filter）
- `ocr: internal/diff/resolver.go:62-76,100-141` — ResolveComment / RelocateAcrossFiles（唯一命中语义）
- `ocr: internal/diff/relocation.go:57-100` — ReLocateComment（LLM 兜底+确定性复校验回滚）
- `ocr: internal/tool/code_comment.go:19-63,130-132` — 增量落袋+白名单校验+静默丢弃坑
- `ocr: internal/llmloop/loop.go:333-338,479-540,562-705` — cache 亲和 SessionTaskKey / grace round / code_comment 热路径（pool 异步）
- `ocr: internal/llmloop/compression.go:100-124,219-295` — 三区压缩（60/80% 双阈值，失败不截断）
- `ocr: internal/llmloop/loop.go:340-341` — MaxToolRequestTimes 消费点+连续空轮 3 停
- `ocr: internal/config/template/template.go:43-58,125-135` — ScanTemplate 分立 / GroupingPlan 双阈值
- `ocr: internal/agent/grouping.go:53,208,268` — groupDiffs（元数据分组+全防线回退）/ buildFileList / parseGroupingResponse
- `ocr: internal/scan/agent.go:316,513,821,862,935` — full-file scan 流水线（batch/plan/summary/dedup）

## Integration Points

### Inbound References

- `src/cli/mod.rs:254,257`（全量）与 `src/cli/incremental.rs:71,114`（增量）→ run_code_fanout：两组件编排入口，三阶段化的两个改写点
- `src/main.rs:427-432` ToolsMode → ToolLoopProvider/SingleShotProvider 分派：输出协议改造的消费开关
- `src/cli/tail.rs:79,138-139` TailCtx → verify_findings：model_names 流入 refute（advisor 角色化的接线点）
- ship 循环（`plugins/redteam/commands/ship.md:79,153-165`）消费 RunMeta 字段（resolution_disputed 等）——收敛三维度的新字段出口

### Outbound Dependencies

- provider 层：LensRequest.route_key（`src/provider/mod.rs:12`）→ registry——角色路由的既有通道
- pack/diffsym：`src/pack/mod.rs` 与 `src/consolidate/verify.rs:65-71` 双消费 diffsym——tree-sitter 升级的影响面
- 判例库：db.rs DismissCard/ResolveCard → verify/预筛/豁免三消费点——预筛前移的依赖
- proj.db：CellInput/CellRecord/checkpoint 模式（疑点台账镜像此模式）

### Infrastructure Wiring

- CellSink 四钩子（`src/orchestrate/mod.rs:15-24`）→ 后台 job state（phase_history 时间线，`src/main.rs:1228` status 表消费）
- Semaphore max_concurrency（`src/orchestrate/mod.rs`）——三阶段各自并发档
- 增量轮 preflight 四守卫（`src/cli/incremental.rs:169-252`）——疑点台账守卫扩展点
- 安全层：from_layers_tagged 剥离清单（`src/config/mod.rs:222-231`）+ neutralize_structure（`src/prompt/mod.rs:265-300`）+ 不可信输入免疫段（DEFAULT_SYSTEM，威胁模型 B）——新增疑点清单/跨组索引注入必须同款防护

## Architecture Insights

1. **漏报的经济学**：发现便宜、论证贵、验证最便宜——三个价位对应三个角色的预算档；漏报只能在上游（生成侧）缓解，下游（后处理）只对已发现集有效。
2. **疑点是「低门槛建制化」的原语**：把「模型隐约觉得不对」从一次性注意力现象变成可持久化、可合并、可跨轮激活、可判例预筛、可裁决的一等公民——台账化把单轮漏报从终局变成休眠态。
3. **状态传递原语**：带机器可校验契约的渲染段 + 字节稳定段透传（claimed 卡的 EXACT title+quote 匹配、draft 的 CONFIRM-or-REVISE、inc_section/spec_section 前缀保 cache）——不发明消息历史。
4. **拓扑原则**：跨模型、带判例、需独立检查点的角色必须独立阶段（ocr 的 review-filter 内嵌组会话是反例）；同模型同会话的廉价 filter 才配内嵌。
5. **cache 三律**：段序稳定性降序、append-only 历史、跨请求共享前缀只分化尾部——三角色若各建卷宗或分化段在证据前，扫描遍的 input 优势蒸发。
6. **先分析后承诺**：裁决类 prompt 的字段顺序 load-bearing（analysis 先于 verdict，不可撤回）。
7. **机械预筛的安全边界**：只做降级候选不做终审丢弃——半盲的机械匹配直接丢弃会把假阳压制变成新漏报面。
8. **TOD 旋钮的角色归属**：TPS/output_limit/timeout 是模型属性，thinking_level 是角色属性（同一模型不同角色不同档）——配置分层（通道/模型/角色）各管各的。

## Precedents & Lessons

8 组架构演化判例分析（证据源：commit bodies + `.trellis/tasks/archive/2026-09/` + `.remember/` 日常注记；本仓无 `.rpiv/artifacts/`）。

### Precedent: needs 二阶段编排（最接近 scanner→reviewer 的两遍机制）
**Commit(s)**: `ce0ca6b`,`3e754a3`,`e62a7cd`（2026-07-13）
**Blast radius**: 6 files（domain/prompt/orchestrate/provider）+ orchestrate +248 行
**Follow-up fixes**: `9fa4924`（增量轮缺 inc_section+needs 双重注入互斥）、`c4500a9`（证据段纳入预算纪律）、`e25c26d`（git-tracked 读守卫堵任意读，E11-Q4）
**Takeaway**: 新增编排 pass 第一天就要带：预算纪律、注入互斥、读守卫——三者全是事后补的。

### Precedent: 无工具默认化架构翻转（最大转向）
**Commit(s)**: `d5429c5`→`3739c19`→`4f52ac9`→批2-4→`c0f8a34`（tools 三态诊断位）
**Follow-up fixes**: `a2bb169`（同日 4 集成 bug：预算双重预留致 0 壳静默塌缩、散文指挥不存在的工具）；E11 注入加固系列
**Takeaway**: 架构翻转跨层留陈旧假设；每个翻转配「旧记账下必失败」回归测试。

### Precedent: 单格全审转正（E9，fan-out 收缩）
**Commit(s)**: `20c3727`（2026-07-14）；复活点记录于 `656ed23`/`fee48e8`
**Takeaway**: 两跑 input −77%、挂钟 1/3——共享上下文喂一次；三角色共享 pack 不违反 E9，各角色重喂全景才是被判决的形态。

### Precedent: SCIP 语义引擎安乐死（建了又拆）
**Commit(s)**: 建 `e013562`+`b3be2b1`+`16323ed`；拆 `3653e54`（2026-07-15，−510−386 行）
**Takeaway**: 更精巧的基底没赢过「全文+降级梯子」；tree-sitter 引入须绑明确用途（跨组引用索引），不为 def 边界单独引重依赖。

### Precedent: 输出契约与 parse fail 事故链
**Commit(s)**: `891ea6b`（three-layer verify）；修复 `d60625e`（模板少要字段致 r2 6/7 解析灭）、`f42cabd`（T43 假 reject，前缀嗅探救回）、`44a11f0`、`60d9db0`
**Takeaway**: 新角色输出契约必须与 parse 类型字段级对齐；parse 失败默认语义保守显式（不静默翻转 verdict）。

### Precedent: 截断/超时事故链（provider 层）
**Commit(s)**: `e1194be`（two-phase finalize）；修复 `3a7a234`（T24 finish_reason 捕获+ONE-shot 纠错）、`2d9945d`（SSE 流式）、`2e797dc`（**Anthropic 漏同款修复 2 个月→524**）、`9f7540b`（超时默认 300s）
**Takeaway**: 修复按 backend 收敛单点否则平行实现漂移；三角色=三份 finalize/parse 面，finish_reason 门+修复轮+流式必须 per-backend 单点。

### Precedent: 增量轮/dedup/carry 身份 seam
**Commit(s)**: `e252164`,`e48e359`,`5ccc6cb`,`eebdc4b`；修复 `0f314dd`（carried 不重复 refute）、`49ae758`（D4 跨轮 quote 漂移）
**Takeaway**: 跨轮/跨角色去重最脆——is_same_bug 单一真相源，agreement 永不双计。

### Precedent: 重构方法论（最近两次）
**Commit(s)**: `1ae876b`+`84e0136`（provider/model 分离+狗粮 8 修）；`fc0750b`（cli 拆五模块，零语义 diff 机械迁移先行）；`d579948`（L1 cache 感知，狗粮 50% 命中）
**Takeaway**: 先机械拆零语义，再狗粮+对抗评审内循环；cache_control 已有 50% 命中基线。

### Composite Lessons

- **实证驱动铁律**（E7-E11/E9 全部 env 仪器 A/B→判决→转正→拆仪器）：三角色分层也应先上观测仪器（coverage+needs_stats 式触发率指标），SCIP 510 行建了又拆是前车之鉴——用户已选「机制直接上」，观测仪表至少随行。
- 新增编排 pass 三大标配（注入互斥/证据段预算帽/读守卫）+ 输出契约字段级对齐 + 截断修复 per-backend 单点 + 共享上下文喂一次 + 跨 seam 判定单一真相源 + cache 前缀稳定性=钱。

## Historical Context (from `.rpiv/artifacts/`)

- 本仓无 `.rpiv/artifacts/` 目录（首份）；历史证据源为 `.trellis/tasks/archive/2026-09/`（3 个归档任务含 524-spike 研究与 anthropic-streaming）、commit bodies、`.remember/` 日常注记。
- `~/llm-code-review-architecture.md` — 本次研究的输入：PR181 四工具对照架构方案（仓外文件）。

## Developer Context

**Q（`src/orchestrate/mod.rs:266` run_code_fanout / 三阶段化形态）: 完全重构还是渐进演进（roles opt-in + 零配置兼容）？**
A: 本次仅调研不决策，但用户倾向**完全重构**——不受渐进兼容约束；研究推荐按三阶段独立 orchestrator 全新组织，现有零件（provider/pack/consolidate 证据机器）作为可搬运组件盘点。

**Q（`eval/harness.py:3` 仅测 dossier 召回）: 漏报缓解落地顺序——oracle 先行 / coverage 先行 / 机制直接上？**
A: **机制直接上**——第一性论证已足够强，不卡在度量先行；eval oracle 后补（历史 dogfood findings 集），研究文档将其记为 known limitation 而非阻塞项。

**Q（`src/cli/mod.rs:132-163` resolve_models 四分支）: replicate 收缩为 scanner 位并跑、`--models a,b` 语义变化是否接受？**
A: **如果要重构，`--models` 这种形式就不存在了**——原架构多模型用于「查更多+交叉验证」的工作流在新架构已不适用。角色基数定调：**scanner、reviewer 可以多个，advisor 建议单个**。（交叉验证职能由 advisor 的 pick_cross_model 语义在角色层继承。）

## Related Research

- （首份，暂无。后续 design/blueprint 应以本文档为输入。）

## Open Questions

1. **findings-recall oracle 搭建**：以历史 dogfood 已确认 findings 为 ground truth 的最小评测集（复用 harness materialize+分桶框架换 oracle 层）——机制已直接上，度量后补以验证漏报缓解真实效果。
2. **疑点 schema 字段集**：quote 必填（保判例预筛+合并锚定）已定方向；hint 归一化规则、confidence 刻度、evidence_requests 内嵌形状待 design。
3. **advisor 单模型的选择策略与 fallback**：用户定调单 advisor；与生产者模型族交叉（pick_cross_model 保留）+ 同档 fallback 的具体配置待 design。
4. **top-K 的 K 值与联合排序**：置信度×列出模型数联合排序的 K 校准需 dogfood 数据；「宁多挖」下限策略待定。
5. **段序翻转的模板重构**：output_contract 移到 dossier 后的具体模板结构（System 尾部追加 vs user 尾段）与 E9 单格默认的共存形态。
6. **完全重构的迁移策略**：一次性替换 vs 新旧并行跑对照期（dogfood 内循环形态）；E9/SCIP 判例提示观测仪表须随行。
7. **疑点并集的 dedup 锚定**：跨模型疑点合并的 id 稳定性（quote 轻锚 + hint 归一化）实现细节。
8. **scanner 分域扫描**（Uber 证据：correctness 高采纳/style 低采纳）：scanner 按缺陷类别（安全/并发/边界/错误处理）分 prompt 分域是否值得——单模型全科 vs 分域 checklist 的召回差异未证。

## Follow-up Research 2026-09-04T12:53:54+0100

**触发**：用户对三角色架构的「快」提出质疑——「reviewer 深挖和之前 redteam 实现没啥大区别，scanner 和 advisor 的加入反而会拖慢整个评审」。经讨论修正/深化了五项结论（本节追加，不修改原文；原 Open Questions #4 被被本节部分解决，#6 的部署前提被被用户确认排除）。

### F1. 「快」的三重机制澄清（对质疑的第一性回应）

1. **scanner 是替换不是追加**：PR181 实测 GLM 直出 30.9K output、19 min，其中只有 ~6-10K 是 6 个 findings 的完整论证，大头是 **thinking tokens——模型读卷宗、做隐式筛选、决定报什么**。即：现状慢模型直出本身就是「模型内部的两遍法」，扫描这步藏在 thinking 里按 27 tok/s 计费（thinking 计入 output 额度）。scanner 把这部分隐式扫描工作**外显化并提到 103 tok/s**（疑点索引 2K ≈ 20s）。扫描遍不是新增成本，是把 19 min 里本来就存在的隐式扫描时间提速 4 倍。
2. **疑点粒度 fanout 解锁并行（被原研究低估的杠杆）**：TPS 是**单流**约束而非总量约束——wall-clock = max(各流) 而非 sum。现状 30.9K 是串行单流（bundle 粒度 cell，无法拆分）；深挖遍是 K 个独立 ~1K 流并发跑，wall-clock = 最慢单疑点。同 token 总量拆 8 路并发 ≈ 近 8 倍提速。**findings 是终态一次性产出、中途拆不开；只有 scanner 的疑点索引才能成为可调度的独立工作单元**——这是 scanner 的第二重价值（宽召回之外）：解锁并行粒度。此为原文 §3「漏斗形状」的深化：快的主因不是砍阶段而是换粒度。
3. **advisor 非新增串行阶段**：现状已有 refute 趟（`src/consolidate/verify.rs:206-223` 三层筛选，仅孤证 blocking；架构文档实测单 finding 几十秒）——advisor 是其角色化。真正新增的疑点三元裁决可只对低置信疑点跑或用机械排序（置信度×agreement）零 LLM 替代。

质疑中成立的三点（保留为设计约束）：深挖遍单位工作不变（慢模型论证 TPS 不变）；阶段切换固定开销；**小 PR 净变慢**（<10 文件单遍直达，与「单包直达」同理——needs 快路径的第二重存在理由）。

### F2. 延迟模型量化（PR181 数字）

| 配置 | wall-clock | 质量 |
|---|---|---|
| GLM 直出（现状） | 19 min（串行单流 30.9K） | 6 findings |
| flash 直出（现状） | 5.3 min | 3 findings（质量打折） |
| 三角色，并发充足（≥4-8 路） | ~0.5 + 2-5 + 0.5 ≈ **3-6 min** | ≥ GLM 直出 |
| 三角色，并发=1（退化） | 7-13 min（疑点串行） | 同上 |

**成立条件三项**：并发额度（≥4-8 路，用户已确认 provider 并发不低——风险解除）；K/预算纪律（F3）；小 PR 旁路。条件不满足时用户质疑成立。

### F3. K 值定论：预算驱动而非常数（部分解决原 OQ #4）

- **定预算不定 K**：深挖遍给预算帽（token 或 wall-clock，如 10K 深挖 output / 8 min），疑点按 `置信度 × 列出模型数(agreement) × 严重度类别` 联合排序，从队头吃到预算为止。K 是预算的函数，不是常数。校准的对象是可解释的预算量，而非随 PR 规模漂移的 K。
- **不用置信度阈值切**：阈值两侧边界疑点被武断切掉，而边界恰是漏报敏感带；预算制下排名靠前的边界疑点仍会被挖到。
- **台账兜底闭环**：预算内未挖疑点不丢弃——落疑点台账 + 报告低置信线索披露 + 增量轮 delta 触及锚定文件重扫。**K 保守的代价从「漏」降为「延迟到下一轮」**——这是 K 敢保守的前提，与原 §2/§9 的台账机制闭环。

### F4. K 大于并发：排队 + graceful drain

- Semaphore 排队消化（现状 `run_code_fanout` 已有 max_concurrency）：wall-clock ≈ ceil(K/并发) × 单疑点时长 + 慢尾。
- **预算到期 graceful drain**：正在跑的疑点跑完（杀掉=白烧已生成 token），队列未开跑的取消、直接落台账。
- **并发按角色分级**：scanner 组粒度高并发（快模型便宜）、reviewer 疑点粒度中并发（慢模型流贵，需防挤占 rate limit）、advisor finding 粒度低并发。

### F5. 调度归属定论：LLM 产工作单元，编排器调度（零 token 确定性代码）

- LLM 在每个节点只做「声明」，所有「完成」在编排器：scanner 产疑点（含 evidence_requests）→ 编排器排序/预算/分派 → reviewer 论证；reviewer 可选声明 needs → 编排器装配 → 补跑。**needs 二阶段（`src/orchestrate/mod.rs:210` SecondPrompt 闭包）已是此原语的雏形**——LLM 声明证据缺口、确定性代码守卫+装配、定向补跑；三角色化将其泛化为角色间状态传递的统一模式。
- 理由三条：可复现（同疑点集→同分派；对照 ocr 工具循环探查路径随机的教训）；零信息量（排序/过滤/分派是机械工作，花 output 买零信息）；故障域隔离（编排器失败不烧 token，LLM 失败烧 cell）。
- reviewer 的 cell 输入 = 一条疑点 + **确定性代码预装配的定向上下文**（周边代码 ± 窗、spec 相关段、跨组指针——`src/cli/second_pass.rs:132` 证据段机器按疑点粒度重入），不拿工具自主探查（tools=on 留作可选档）。LLM 之间无直接对话，全部经持久化中间产物衔接——三阶段独立 orchestrator 的又一论据（呼应原 §3）。

### Discussion Outcomes（Developer Context 追加）

**Q（F2 成立条件）: 实际部署的并发额度是否构成三角色「快」的瓶颈？**
A: 用户确认——「一般的 provider 并发都不算太低，所以并发不是问题」。并发风险解除；剩余纪律项为预算帽（F3）与小 PR 旁路。

**Q（F3/F5）: 预算驱动 + 台账兜底 + 编排器调度的深挖遍形态是否认可？**
A: 用户要求将讨论结论修补进本文档（即本节）——方向认可。
