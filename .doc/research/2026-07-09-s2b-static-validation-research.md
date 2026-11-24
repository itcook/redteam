# S2-b 静态验收（recall delta / callee 产量）+ 发现并修复 scip 引擎真 bug

> 状态: 已交付 · M15-S2-b §5 验收「静态段·零 LLM」。喂端到端决策。
> harness: scratchpad `s2b_static`（path-dep redteam，用 shipped `build_pack`/`build_pack_scip`/`ScipIndex`，零重实现漂移）。

## 方法

pack-字符串层对比（design §5 指定 `scip-strict` 纯 SCIP 臂）：真 diff → `build_pack`(regex) vs
`build_pack_scip(strict=true)`(纯 SCIP、无兜底)，抽渲染出的「## 引用点」`path:line` 做集合差 =
recall delta（`SCIP\regex`=SCIP 抓到 regex 漏的；`regex\SCIP`=regex 有 SCIP 无=FP+SCIP 盲区），
「## 被调契约」计 callee 产量。budget=1M（不触预算砍；per-name cap 10 两臂同施、公平）。

## 头条：静态探针在真实代码上发现 scip 引擎不可用 → 已修（commit dfc8ade）

**受控单符号探针**（只改 `build_pack` 体）修前：regex 给 4 个**真 caller**（cli.rs:385 + 3 测试），
SCIP 给 10 个**完全不相关**位置（bundle/config/db/consolidate…），**交集 0**——SCIP 把真 caller 全漏、返垃圾。

**根因（两叠加 bug）**：
1. **`symbol_at` 同行多 def 碰撞**：函数 def 与其参数（SCIP `local N`）常**同行** Definition
   （pack/mod.rs:91 上 `build_pack` + `local 27/28/29`=bundle/root/budget 三参数）；`def_at` HashMap
   后写覆盖 → `symbol_at(91)` 返回 `local 29`（参数）而非 `build_pack`。
2. **`local N` 文档内作用域、按串全局 key**：`local 29` 串在本仓 **26 个文件**各指不同物；`refs_of`
   按符号串合并 → `callers_of` 跨文件混入满仓垃圾。

**为何单元测试全绿没抓到**：fixture 符号（`"S foo()."`）无参数 local、无 `local N`、def 在第 0 行——
三个触发条件全缺。**位置对精度是 SCIP 立身点，却被真实代码的参数 local 击穿**。这正是静态验收的价值：
零成本抓到「机制交付🟢 + 全评审绿」下仍不可用的引擎。

**修**：`from_index` occ/si 循环跳过 `local ` 前缀符号（连接键与引用目标恒为 item，local 非目标）。
回归测试编码「item 与 local 同行 def + 跨文件同名 local」双坏场景。修后单符号 build_pack：**SCIP=regex 4、交集 4**。

## 修后 recall delta（zero-LLM）

| 语料 | regex 引用点 | SCIP(strict) | 交集 | SCIP\regex（增益） | regex\SCIP | callee 注入 |
|---|---|---|---|---|---|---|
| 单符号 `build_pack`（fn） | 4 | 4 | 4 | **0** | 0 | 13 |
| 单符号 `Bundle`（高 fan-in 类型） | 10 | 10 | 8 | 2 | 2（cap-10 取序差） | 1 |
| S2-b 变更集（5 文件） | 98 | 79 | 69 | 10（全 dossier 内部） | 29（多在改动文件测试区：部分 SCIP 正确丢 regex 同名 FP、部分 strict 跳未解析） |
77（1 dep） |

**结论**：修后**SCIP 引用 ≈ regex**。干净单符号完全一致；高 fan-in 类型 8/10 一致、差的 2 是 cap-10 取序
非真 recall/精度差。**SCIP 不是批量 recall 赢家**——regex+B 过滤在普通函数上已够好，SCIP 的引用增益温和且集中
（少量宏/derive/内部引用）。这与 S2-a 假阳探针互补印证：regex 主要问题是 FP（B 已治大半），非 recall 缺口。

## S2-b 真正的价值不在 recall，在两个 recall 测不到的杠杆

- **callee 签名注入**：产量可观（单 fn 13 条、变更集 77 条），**regex 根本做不到**——直接打「search 被调函数签名」的 tool_use。
- **契约分类器（A/B 方向性深度）**：A 桶 caller 给完整外围体，打「读 caller 查用法」的 tool_use。**静态测不到**（pub(crate) 挡住 bucket 分布测量；且其效应是 tool_use 非引用集）。

两者都是 tool_use 杠杆但**只在端到端可测**。故 recall-delta 弱不代表 S2-b 无价值，代表 **S2-b 的 tool_use 收益若有、来自 callee+A-body 而非引用 recall**。

## 端到端决策建议

- **纯 recall 论据弱**：按 design §5「显著才端到端」，recall-delta 不显著 → **不值全量 3 模型×N=3 端到端**（成本纪律）。
- **但 callee+分类器未测**：建议**小廉价端到端**（单模型 DeepSeek、附四少数 bundle、`scip` vs `regex-pack`）专测
  「callee 注入 + A-body 是否降 tool_use」——隔离这两个杠杆，量 input 升/tool_use 降的赌注。显著再扩。
- D25 独立性门照旧（A-body 灌更多共享上下文）。

## 小端到端（N=1 · DeepSeek · 6f95c25 5 格 · scip vs regex，2026-07-09）

静态弱信号后跑小廉价端到端专测 callee+A-body 的 tool_use 效应。同 dev 二进制（含 fix dfc8ade）、
干净 worktree @ 6f95c25、`--models DeepSeek --no-dismissed`、两臂**串行**（D9）：ON=`REDTEAM_PACK=scip`
（job f961ff7088f8）、OFF=默认 regex（job 0e4a840f559d）。**预验 rust-analyzer scip 在该 worktree 能建
（2.4M）→ ON 臂真用 SCIP 非降级**（逐格 input 差异佐证：orchestrate ON 497K vs OFF 325K=+53% 注入了 A-body/callee）。

| 指标（5 格合计） | OFF(regex) | ON(scip) | Δ |
|---|---|---|---|
| tool_use 总 | 47 | 46 | **−2%** |
| read_files | 29 | 29 | 0% |
| search_repo | 15 | 15 | 0% |
| iterations | 45 | 41 | −9% |
| input(K) | 1850 | 1705 | −8% |

**tool_use 基本平**；逐格混杂（prompt scip 赢 TU 7→4/input−47%、verify scip 输 TU 4→9/input+65%、
orchestrate search_repo 10→6 但 input+53%）。**`.md` 模板格=天然对照组**（非 Rust 两臂无 pack 差异、
本应完全相同）却 TU 8→7 / **input 377K→85K（4.4×）**——**坐实 N=1 噪声底巨大**，−2% 完全淹没。

**判定：小端到端无 tool_use 可测收益、方向持平**，与静态 recall≈regex 一致。**不升级全量 N≥5×紧尺（gpt-5.5）**
——按附二教训 DeepSeek 41% CV 松尺 + N=1 噪声，边际信号不值那成本（成本纪律）。**S2-b 的 tool_use 收益在此
语料/尺度未证实**；真收益若有，或在（a）宏/derive/高 fan-in 重的 diff、（b）S1 式质量/独立性红利（未测），
非普通 tool_use。**park 端到端**：引擎已正确（bug 修）+ opt-in 可用、默认 regex 不翻；痛点/更好语料驱动再验。

## 残余读解剖（2026-07-09 追加 · 定案「设计层三错配」）

小端到端持平后做差分诊断：**不数 tool_use、看它读什么**。仪器 = `REDTEAM_TOOL_LOG`（commit 4fe9b9b，
execute 单点记参数 jsonl + tool-loop 起点 bundle marker）+ 项目本地 `.redteam/config.toml`
`max_concurrency=1` 顺序跑归属 cell；语料同小端到端（scip 臂、DeepSeek、6f95c25，job 5165df6635d0）；
分类脚本比对每次 read 的内容与该 cell pack 全文（行级 overlap，scratch `classify.py`+`packs_dump/`）。

**前置排除（本轮已证）**：64KB vs 1M pack 渲染逐字节相同（预算非瓶颈、T27 解禁无用）；两臂真分化
（scip 臂多 251/97/31 行独有内容+callee 段）；5 格中 2 格（prompt=锚定 bug、templates=.md）两臂 pack 全空
=天然对照组，其 TU 波动（8→7、7→4）纯噪声。

**解剖结果（~58 次调用全分类）**：「重读已 pack 内容（≥70% overlap）」**≈0**——模型没浪费调用重取
pack 给过的东西；残余读全部落在 pack **结构上不给**的四类：

1. **整读改动文件**（每格第一动作）：cli.rs 整读→pack 已有仅 19%、main.rs 40%、verify.rs 17%、
   orchestrate 分段扫全文件——**模型消费单位是文件，pack 给的是 def 粒度**（0 壳外围体+1 壳引用行）。
2. **跨 bundle 变更集读**：审 orchestrate 的格读 cli.rs/main.rs/state/job.rs、审 verify 的格读
   cli.rs/orchestrate/prompt 模板——同一 feature 切在 5 个 bundle，cell 隔离（D12）逼模型自拼全景。
3. **类型/契约深读**：search `struct JobState`/`LensCall`/`RawFinding` + 读 domain/finding.rs——
   callee 1 跳**签名行**不够，模型要类型**完整定义**；search 18 次中仅 2 次词在 pack。
4. **refute 核实 pass 零 pack**：捕到无 bundle marker 的 tool-loop（refute prompt 不带 pack）14 次调用
   从零重读——每 finding 一趟的整个消费者在 pack 机制之外。

**定案**：实施层（引擎/预算/分化/呈现/信任）**全部排除**——pack 被正常使用，但只覆盖模型所需的一小角。
根因 = **设计层三错配**：(a) **粒度**（符号级 pack vs 文件级消费）(b) **范围**（per-bundle radius vs
per-changeset 评审）(c) **覆盖**（refute pass 无 pack）。这回溯解释全部旧数据：S1 给 def 体 −8~10%、
S2-b 给更准引用行+签名行 ≈0——**模型缺的不是行级精度，是成块原文**；SCIP 语义精度在回答模型没问的问题。

**S2-c 最终判定：不做**——其全部件（双树/剪枝/传递排序/per-model 预算）都在错配的符号级精度轴上加码。

**数据支持的新杠杆（全 regex 级、不需 SCIP，按 ROI 排）**：
① **0 壳升级「改动文件全文」**（预算内整文件、大文件回落现状）——对准错配(a)，杀「整读改动文件」类
   （每格 1-2 次+后续同文件分段读）；即 T29 强化 + latency thread「1c/★宿主供全文」的数据印证。
② **refute pass 带上下文**（refute prompt 附 finding.file 相关段）——对准错配(c)，每 finding 乘数。
③ **变更集全景**（cell 附其他 bundle 的 diff 一览）——对准错配(b)，与 D12 独立性有张力、需 grill。
④ 修 prompt/mod.rs 空 pack 锚定 bug（test-mod hunk 锚不到外围 def，T42）。
**验证方法改用解剖法**（跑一次看「整读改动文件」调用是否消失）——比 A/B 计数便宜且不与行为噪声搏斗。

## S2-b' 第一批验收（解剖法 · 2026-07-09 · jobs 9765cd342281/e6b2e349d907）

三件（①0 壳整文件+③256K 帽=24798fd+6b257ca、②refute ±60 行快照=e3363d6+fd002e1）SDD 双评审交付后解剖法验收：

- **验收中追加抓到第四个真凶并修（7d23240）**：`render_dossier_section` 散文里 M14 时代旧指令
  「Excerpts are a starting point — **read the full file at any site you must judge**」在 0 壳升级全文后
  仍**明确命令模型整读**——改为 0 壳信任声明（工作树逐字快照、重读同内容、工具只花在 context 外）+
  1 壳读指令限定 OTHER site；测试钉住防漂移。
- **pack 核实**：全格含全文块零回落；prompt/mod.rs pack 0B→21KB（T42 被①吞实证）、templates 0→7KB。
- **量化（vs S2-b 基线，同语料同模型 DeepSeek）**：per-ok-格 tool_use **~9.3 → 6.2 / 6.0（−34%，跨两跑一致）**
  ——全线最稳的量化信号（此前任何增量都 <噪声）；Σiter 41→28→21；Σinput 1705K→1498K→1315K
  （多给前置、轮次降、**净省**——「input 升换 tool_use 降」的赌注实际连 input 都降了）。
- **质门（整读类消失）未全过**：评审格整读本 bundle 文件 6→5→4，减而未零。**旗舰 cli+main 格两跑归零**
  （基线=第一动作整读 4 文件）；小格（verify/prompt/templates）各残 1-2 次——全文在 context + 明示
  勿重读仍读 = **行为地板正式坐实**（实施层已无嫌疑可排）。
- **refute 快照段**：机制正确、行为未动（~13-16 调用/趟不变）；skeptic 的读是**跨文件取证**
  （callers/types/相关模块）非重读 claimed 位置——±60 行快照对不上其行为模式；ledger 升级路径
  （附整 bundle pack）预期同样有限（feature-chasing 不是快照能满足的）→ 缓、不再追。
- 跑 #2 2/5 格 DeepSeek parse 失败（老毛病）。**成本纪律：验收停在两跑，不再加 N。**

**S2-b' 结论**：粒度错配修正**有效且是全线最大单笔收益**（−34% per-cell tool_use、轮次/input 同降）；
残余 = 行为地板（对抗核实习惯）+ 跨 bundle feature 拼图（范围错配，唯一未修的错配）+ refute 取证
（快照形态解不了）。**pack 线在此语料的天花板已画清**：够本的杠杆已收割完，范围错配（变更集全景）
是仅存的、需 grill 的候选——门槛=痛点或大仓语料。

## S2-b'' 激进 A/B：无工具评审（2026-07-10，用户立项，jobs 85d3fb56191c/cd85797a7733）

**问题**：若 context 已足够，砍掉 search/read 工具，评审质量降不降？（若不降 → 轮次塌到 1 = 北极星字面达成。）

**前提核查（静态，先行）**：①变更集 7 文件全文合计 195KB < 256K 预算（可全给）；②带工具跑的 findings
锚点 **4/4 在变更集内**（3 own-pack + 1 跨 bundle）、0 在变更集外；③两轮验收读需求分类：58% 跨 bundle
（变更集 pack 可覆盖）+ 22% 仪式重读（已有）+ **17% 变更集外**（state/domain 类型佐证——实验待测其价值）。
→ 前提「可满足」：变更集全景 pack 覆盖 83% 读需求 + 100% finding 锚点。

**仪器（commit d5429c5，dev 态 env 门）**：`REDTEAM_PACK_SCOPE=changeset`（每格 0 壳追加兄弟 bundle 文件
全文，空 diff 自家优先）+ `REDTEAM_NO_TOOLS`（空工具集 + 无工具 system；backend 对空 tools 已有干净处理）。

**双臂（同变更集 pack、同 scip 引擎、DeepSeek、串行、5/5 ok）**：

| | Arm A（工具） | Arm B（**无工具**） |
|---|---|---|
| findings（raw） | 7（2h/3m/2l） | 3（1h/1m/1l） |
| refute 存活 | 4（0h/2m/2l——**2 条 high 全被 refute 毙**） | 2（**1 high 存活 conf 0.9** + 1 low） |
| verdict | reject | reject |
| quote 真伪 | — | **3/3 逐字在源文件（零幻觉）** |
| Σiterations | 39 | **5（恒 1 轮/格）** |
| Σinput | 2997K | **280K（−90.7%）** |
| 挂钟 | ~24min | **~8min** |

**质量判读**：两臂同报核心 bug（空证据假绿灯 cli.rs:443 双臂同锚同被 refute）；B 的 high（classify_refute
信 title 前缀）refute 存活、A 的 2 条 high 反而全灭；A 多出的 4 条（scope guard 被毙/测试覆盖 nit/
partial_report/2 low nit）**全部锚在变更集内**——A 的额外产出没有一条需要工具才能锚定，差异是注意力/轮数
而非访问权。**N=1 单模型小仓语料 caveat 照旧。**

**判定：无工具臂质量未见下降、延迟与成本数量级改善（轮次 39→5、input −91%）。**

**意外副产品（Arm A 自身）**：「变更集 pack + 工具」比 per-bundle+工具**更贵**（TU 39 vs 31/18、input
2997K vs ~1500K）——更多上下文给模型更多可追线头、工具放大成本。**上下文与工具是替代品非互补品**：
大上下文的收益只有砍掉工具才兑现；两者都给=最差组合。

**含义与门**：这可能是 M15 线最大的产品发现——「上下文足够时，工具是纯开销」。翻产品默认前需：
①多模型复验（gpt-5.5 紧尺）②非本仓/更大变更集语料③设计「变更集外证据」缺口的处置（B 天然做不了
17% 类佐证读——本轮未见质量损伤，但语料太窄）④refute 是否也无工具化（本轮 B 全程无工具、含 refute，
其 refute 判定与 A 方向一致）。候选形态=「无工具默认 + 证据不足时二阶段带工具补查」。

## 粒度 A/B：S2-b 符号 pack + 无工具（S 臂，2026-07-11，用户立项，jobs 43aa5fd272fd/111f07f5522a）

**问题**：E7 粒度结论推自「有工具下的读行为」，行为≠必要性（行为地板：全文在手仍整读）。
若「S2-b 符号级 0 壳 + per-bundle + 无工具」质量守住 → 爆炸半径 pack 复活且严格更省；若掉 →
全文默认坐实。**决定规则先声明**：S 臂与 B 臂差粒度+范围两个变量，掉了归因不可分（只裁产品问题）、
守住则双错配结论均过度推断。

**仪器**（commit 478cc02，dev 态 env 门）：`REDTEAM_PACK_BODY=symbol`——body_section 拆
impl(symbol_mode)+env 薄壳，regex/scip 两引擎共用；symbol=S2-b 旧形状（旧段题/旧块头/非支持语言
无损降级）。发射前验 pack（manifest_json 直查）：5 格分区同 S2-b''；Rust 三格=符号 0 壳+SCIP 1 壳+
callee、零全文块；cell4（hunk 在 mod tests、锚不到 def=T42 根因重新裸露）/cell5（markdown）0 壳全空。

**跑法**：同语料（worktree@6f95c25、`--base 6f95c25~1`）、DeepSeek 串行、
`REDTEAM_PACK=scip REDTEAM_PACK_BODY=symbol REDTEAM_NO_TOOLS=1`，两跑。

| | Arm B（全文 changeset+无工具，N=1） | Arm S 跑1 | Arm S 跑2 |
|---|---|---|---|
| findings raw | 3（1h/1m/1l） | 6（2h/4m） | 6（1c/2h/2m/1l） |
| 语义存活 | 2（1 high 0.9 + 1 low） | **0**（名义 2 全是 refute parse-fail 假象，refute 文本意图均 REFUTED） | 1 low 0.9（另 1 medium 0.7=parse-fail、意图不明截断） |
| verdict | reject | 记录 reject / **语义 approve** | 记录 reject / 语义 approve（low<threshold） |
| quote 逐字 | 3/3 | 5/6（非逐字=裸奔格拼接引文） | 5/6（非逐字=verify.rs critical） |
| Σinput（含 refute） | 280K | ~48K | ~50K |
| 挂钟 | ~8min | 4.7min | 4.1min |

**质量判读（两跑合并）**：
- 核心 bug 家族（空证据守卫）跑1 同报同 refute ✓；零幻觉级 quote 但逐字率 10/12 < B 3/3；
- **B 的存活 high 面（classify_refute 信 title 前缀/对抗输出可绕）两跑均未存活**：跑1 整面漏报
  （报同函数相邻面 first-finding-only、被 refute 毙）；跑2 以 critical 命中同面（"trivially bypassed
  by adversarial output"）但 quote 非逐字且被 refute；
- finding 形态一致偏投机：raw 6/6 > B 3、refute 毙杀率高、语义存活 0-1 < B 2；两跑 finding 集重叠低；
- 覆盖洞实证：cell4 两跑零产出、cell5 产全场唯一拼接引文 finding（refuted）——「diff 不落 def」处=
  信息真空（符号 pack 固有降级路径，非粒度原则本身失败）；
- **语义 verdict 两跑均倾 approve、与 A/B reject 不同向**——记录 reject 全靠 refute parse-fail
  假象撑住。

**成本**：input 比 B 再 −82%（48-50K vs 280K）、挂钟减半——符号 pack 省钱主张完全成立。

**判定：质量未守住——全文 0 壳（S2-b'/b'' 路线）维持默认方向，爆炸半径不以 pack 粒度身份复活。**
符号 pack 就此获得实测新定位：**超预算降级档**（成本 1/6、失效模式已知=覆盖洞+groundedness 降），
回落梯子「全文→def 体→签名」的中间层从推测升为有数据背书。

**caveats**：B 臂 N=1 vs S 臂 N=2 不对称；单模型 DeepSeek；同一小仓语料；**refute structured-output
parse 失败（跑1×2、跑2×1）实质性污染存活记录**——parse 失败按原 confidence 保留会把语义 approve
记成 reject，两跑均 outcome-determining，登 T43。

## 批 1 验收四臂（2026-07-13，分支 feat/no-tools-default-batch01，jobs 9a582301b02e/82d32570e38e/3c104e405c10/ea35411743ac）

同语料（worktree@6f95c25）、DeepSeek 串行、新默认（tools=two-stage 无工具 + changeset 全景 + refute 快照 + T43 全套）：

| 臂 | cells | verdict | raw→语义存活 | quote 逐字 | Σinput(+refute) | 挂钟 |
|---|---|---|---|---|---|---|
| 主（新默认） | 5 | approve | 3→0 | 3/3 | 314K+8K | 6.3min |
| 单格（REDTEAM_SINGLE_CELL） | 1（prompt 219KB） | reject | 2→1（0.7 存活） | 2/2 | **74K（主臂 23%）** | **2.0min** |
| 纯 0 壳（REDTEAM_PACK_SHELLS=0） | 5 | reject | 4→2（0.8+0.9 存活） | 3/4 | 281K+7K（主臂 89%） | 5.5min |
| doc smoke（本仓 design 文档） | 1 | reject | 4→2 | n/a | 24K | 1.3min |

**判读**：
- **主臂门=过**：对照 S2-b'' B 臂（280K/3 raw/1 high 存活/reject）——input 同量级（+15%，模板+快照涨量）、raw 同、quote 零幻觉、**零 warnings（T43 期 parse 失败零出现或 repair 救回）**。verdict approve vs B reject：B 存活的 title-prefix 面本跑对应物被 refute 毙——该面两轮人工裁决均为「加固级非实锤」，新 refute（快照+散文修）杀伤更准非退化。
- **单格臂（E8-B 数据点 #1）**：input −77%、挂钟 −68%、quote 逐字，且抓到 ledger 已确认真问题（prompt 测试 env 竞态）并存活 refute——**质量未见下降+成本大降**，方向强烈支持批 4 收缩 fan-out。caveat：N=1、与主臂 finding 集零重叠（DeepSeek 方差）。
- **纯 0 壳臂（E8-E 第一层数据点 #1）**：1 壳/callee 仅占 pack ~10% 体积；砍掉后 raw 4>3、存活 2>0、面不重叠——**砍 1 壳质量未见下降**，SCIP 线趋向安乐死；needs 预防针假说（触发率指标）待批 2 补测后终判。瑕疵：1 条 low 存活 quote 非逐字。
- **doc smoke 门=过**：无工具 doc 评审不炸、findings 可读；且审出 design 真 gap——「tools=off 档在 needs 世界的语义未定义」（conf 0.85 存活）→ 已回补 design §2.4。
- **异语料 smoke（job 1018dd5f121c，ripgrep@241b87b「ignore: GIT_CONFIG_GLOBAL/SYSTEM 支持」真 feature commit）门=过且超预期**：不炸、0 warnings、verdict reject 可读、input 190K、3.4min；5 findings（1 high 0.9 存活）面相当中肯（缺 ~ 展开 / GIT_CONFIG_GLOBAL 存在但缺 excludesFile 的 fallback 语义 / 缺 GIT_CONFIG_SYSTEM 测试）——异语料上无工具评审直接产出像样结果，非仅「不炸」。瑕疵：1 条 high quote 非逐字（跨语料逐字率 4/5）。

## 批 2/3 验收四臂（2026-07-13，分支 feat/no-tools-default-batch23，jobs ffc12db732bb/020560251351/58dc69d09680/66a190db56e4）

| 臂 | verdict | findings | needs (cells/declared) | Σinput | 读数 |
|---|---|---|---|---|---|
| 主臂（needs+refute 升档全套） | reject | 3→3 全存活 | **0/0** | 317K | quote 3/3 逐字、0 warnings；refute 升档后不再滥杀（对照批 1 主臂 3→0 全灭），「多 finding 静默丢弃」面第三次出现终于存活 conf 0.85 |
| 纯 0 壳臂（E8-E 终判） | approve | 1→0 | **0/0** | 285K | **1 壳砍掉后 needs 触发率未上升——「1 壳=needs 预防针」假说证伪** |
| ripgrep 12-commit 大变更集 | reject | 7（2 high 0.95 存活） | 0/0 | 651K | **梯子全套真实触发**（骨架块/def 体回落/hunk 窗/已省略段全出现）；8 格各 ~272KB 无撞墙、0 warnings、6.3min |
| doc 二阶段冒烟 | approve | 10→0 | **1/5** | 50K | **二阶段端到端首证**：真实声明 5 needs→补查趟→替换语义走完、0 warnings |

**判定**：
- 验收四项全过：needs 观测面落盘（含零值恒写）✓、二阶段真实跑通（doc 臂）✓、梯子大变更集实测 ✓、E8-E 终判数据 ✓。
- **E8-E 判决落锤：SCIP/1 壳安乐死**——第一层（批 1 纯 0 壳臂质量未降）+ needs 指标（本轮 1 壳砍掉 needs 不升）双证据齐。执行（scip 引擎降级/拆除）归 M12/M16 拆除清单，pack 默认线不再依赖 SCIP。
- refute 升档副作用（方向好）：锚点全文下 refute 存活率显著上升（主臂 3/3 vs 批 1 0/3）——判得更准而非更狠；需多语料确认非方差。
- Q1（fan-out 收缩）成本论据再+1：大变更集 8 格全景重复喂=651K，单格约 1/8。
- caveats：N=1/单模型照旧；code 侧 needs 零触发与 B 臂旧证据一致（上下文足够），行为面（模型是否爱用 needs）待异模型观察。

## 批 4 验收三臂（2026-07-14，分支 feat/no-tools-default-batch4，jobs a04f0628a828/b69e86bf8267/9c8bfe0220f7）

单格默认 + lens 去实体化后的新默认全套：

| 臂 | verdict | findings | Σinput | 对照 | 读数 |
|---|---|---|---|---|---|
| 本仓语料 | approve | 1→0 | **74.6K** | 批 2/3 主臂 327K → **−77%** | 1.5min、0 warnings、quote 逐字；classify_refute 系列面持续命中被 refute 毙 |
| ripgrep 12-commit 大变更集 | reject | 3（1 medium 0.9 存活=非 Unix /etc/gitconfig 硬编码真问题面） | **89K** | 批 2/3 八格 651K → **−86%** | 1 cell/274KB prompt（帽处）、def 回落+省略段在场（骨架档未触发=预算未压穿，非异常）、2.3min、0 warnings |
| doc 冒烟 | reject | 5→2 | 36K | — | 不炸、可读、0 warnings、needs 本轮零触发（机制上轮已首证） |

**判定：批 4 验收全过。** 单格收缩的成本收益全兑现（小仓 −77%/大仓 −86%）、质量面照常（真问题存活/refute 系列面持续命中）、lens 去实体化零可见副作用（standards 假 high 观察项：三臂均未出现 naming nit 报 high 存活——机械钳制删除后散文+refute 防线首轮实证够用）。

**E8 九裁决至此全部落地**（A 默认翻转/B fan-out 收缩/C needs 契约/D 梯子/E SCIP 安乐死/F refute 升档/G T43/H doc 同批/I lens 去实体化）。北极星账（对照 S2-b'' 前的 M14 时代 arm A：input 2997K/39 轮/~24min）：**新默认单跑 input ~75K（−97.5%）、恒 1-2 轮、~1.5min（−94%）**——延迟/效率/成本三线全兑现，质量由 refute 闸门+验收多轮对照守住。

## 中大型真实语料验收（2026-07-14，hiapi backend channel-rate-limit 分支，job 14280e5fe759）

**首个真实战场 + 中大型探针一次跑完**（design §批 4 验收后的核心机制真实规模确认）。语料=hiapi 私有 Go 后端 feature 分支相对 dev 的 12 commit / 22 文件（15 Go/5 md/1 jsx/1 json）/ +2817−53，改动文件全文合计 **889KB**、diff ~147KB。`--base dev --scope branch --spec design.md --models DeepSeek,VibeCodingFast`（无 MiniMax，避 per-turn 帽）。

| 指标 | 值 |
|---|---|
| verdict | reject（1 high / 1 medium-refuted / 1 low） |
| Σinput | 175.6K（2 模型 ~87K/各）+refute 13.6K | 
| 挂钟 | 2.2min · needs 0 触发 · 0 warnings · 0 failed |
| pack 形状 | 单格 231KB prompt：8 全文块 + 3 def 体回落 + 0 骨架 + 0 hunk 窗 + 省略清单（大文件 tasks_facade.go 改动符号体 173-463 等进省略坐标） |

**核心结论——单格+梯子在真实大型 Go 变更集上质量守住**：
- **889KB → 231KB 重度降级**，两模型全程干净、无超时/parse 失败；
- **头号 high（agreement=2，DeepSeek+gpt-5.5 独立同报）= 真跨文件一致性 bug**：`tasks_facade.go:383` 限流收口无条件改写 429、不判 selErr 错误类型（DB/selector/配置故障被掩盖成容量受限），对照 `relay.go` 守了 `errNoCandidateChannel` 的不一致。agreement≥2 跳孤证 refute、直接存活；
- **降级韧性关键实证**：该 high 所在文件的**完整符号体已进省略坐标**，模型仍找到——因 **diff 是 mandate 永不砍**，改动本身一直在 prompt。**降级砍上下文丰度、砍不到改动本身**（验证「diff 恒渲染」设计在大变更集的价值）；
- **needs 0 触发**：重度降级后 pack 仍够用，模型没要更多文件——梯子的降级表示保住了评审所需。

**「实现对照设计」真实价值实证（承 E10）——⚠️ 机制归因更正（2026-07-16）**：design.md 内容真进了模型推理——F2 引「设计 D8 fail-open」、F3 引「设计不鼓励动态切换」；**F2 refute 尤其漂亮**：DeepSeek 报 medium，gpt-5.5 结合设计 D8 判定为设计允许行为、驳回（跨模型+设计对照压掉「像 bug 实为设计选择」）。
**归因更正**：此处最初记作「`--spec` 真实价值实证」是**误归因**。经查（2026-07-16，dev@8dcaf6f）：code 模式 `run_code_review`（`--base` 走的唯一路径）**从不消费 `args.spec`**——`{{SPEC_DOCS}}` 只在 `build_doc_prompt` 渲染、spec-guidance 只在 doc 路径 `collect()`；本次 `--spec design.md` 被 code 路径**静默忽略**。design 真进模型的机制=**design.md 是分支改动文件之一**（22 文件含 5 md，design-then-implement 把设计随实现提交上分支 → 落进 `--scope branch` diff → 作改动文件进 pack）。
→ 结论仍成立且宝贵：「code 评审**对照**设计」（评审对象仍是代码、spec 仅参照、有界）确有价值，**区别于** E10 砍掉的「文档**作为评审对象**需无界代码地基」；但当前这价值靠「设计恰在 diff 里」偶得、非 `--spec` 保障。**M16 待办**：把 `--spec` 接进 code pack（作有界参照段），使此能力从偶得转为可靠、脱离「设计必须在 diff 里」的依赖（用户 2026-07-16 定，优先于安全组）。见 plan `2026-07-16-m16-spec-into-code-pack-plan.md`。

**判定**：中大型真实验收**通过**——核心机制经真实规模确认成立，非小仓推断。cached 1152（0%）=冷跑真实成本上限。M16 前清理与安全组可带此底气推进。

## 关联
- 承 design `2026-07-09-…-s2b-design.md` §5 / decisions E6；S2-b' design `2026-07-09-…-s2b-prime-design.md` / E7；S2-a 假阳探针 `2026-07-08-s2a-false-positive-probe`（本探针的精度补集）。
- bug 修复 commit `dfc8ade`（dev）；回归测试 `scip::tests::from_index_skips_local_symbols_...`。
- harness 留 scratchpad `s2b_static/`。

## --spec 接入 code pack feature 验收（2026-07-16，dogfood 自评审 gpt-5.6-sol）

**决定性证明 --spec 真生效（区别于 hiapi 那次"设计在 diff 里"的误归因）**：让 feature 用 `--spec <自己的 plan>` 审自己的实现——plan 在 `.doc/`（gitignored）**不在 diff 里**，设计只能靠 --spec 进模型。
- **金丝雀铁证**：另造 canary spec 塞入荒谬规则「ZEBRA-7：provider 模块注释必须英文」→ 审 3 行中文注释 diff → 模型报 `[medium conf=1.0] ZEBRA-7: Newly added provider comments violate the mandatory English-only rule`（refute=upheld）。**规则号+规则语逐字进推理 ⇒ spec 逐字注入生效**，无歧义。
- **注意坑（本次事故）**：`make build` 产物在 `target/aarch64-apple-darwin/release/`，`cargo build --release` 在 `target/release/`——两路径不同；头两跑拿了旧二进制（无 spec 接线），主趟 input 差值 −87/−5883（spec 未进），排查半天才发现是二进制拿错。用对二进制后金丝雀立即命中。

**自评审抓 bug（模型审自己 90 秒前写的代码，5 findings，reject）**——3 个建议本批修、2 个留债：
| finding | 定性 | 处置 |
|---|---|---|
| read_to_string 无界读（巨文件耗内存/FIFO 挂死） | 真 bug | ✅ 修 2ba62fc：metadata 拒非常规文件 + take(cap+1) 哨兵有界读 |
| render_spec_section 未中和 `</spec_docs>`（spec 内容伪造段边界注入 rules） | 真安全 finding（=安全组③同款二阶注入向量） | ✅ 修 2ba62fc：同 render_dossier_section 手法转义 |
| SPEC_BUDGET 64K 固定帽吃穿小窗模型输入窗（40K-token→48KB 预算<64KB） | 真编排边缘 | ✅ 修 2ba62fc：render_spec_section_capped + 调用点夹紧 min(64K,预算/2) |
| 增量路径静默丢 --spec（三处 `""`） | =T44，模型独立重发现+抓 --help 文档矛盾 | 留债 T44 |
| resume 从可变路径重读 spec、与首趟缓存 prompt 漂移 | 真观察（"保留"裁决的反面） | 留债（内容持久化，成本中） |

**D4 守卫经 refute 正确工作实证**：high「doc 扩展内 KEY=VALUE 密钥绕过」被 refute 压到 conf=0.0（note：opt-in spec-file policy 非 universal 凭据检测器）——终审定性的"已知天花板、威胁模型内可接受"被 refute 独立确认。

**判定**：--spec feature 验收**通过**——金丝雀铁证注入生效、L0 端到端命中、自评审抓的可修项本批全修、留债明确（T44+resume）。承 E10「实现对照设计=有界、非 E10 砍掉的文档评审」方向落地。
