# redteam 评审引擎架构·决策日志（M21）

> 状态: WIP · 建于 2026-09-05（文件名取 design 同日期以对齐三联；D1 定于 09-04，D2-D5 定于 09-05）。
> 对应 design：`../design/2026-09-04-redteam-review-engine-architecture-design.md`（各节以〔Dn〕引本文）。
> 证据：`../research/2026-09-04-redteam-review-engine-architecture-research.md`。
> 架构级单份决策日志、分节记。每条：议题 / 候选与被否 / 定案 / 依据 / 代价与残余。

---

## D1 串行三角色 → 风险路由 DAG（2026-09-04）

**议题**：v1「scanner → reviewer → advisor」三角色串行能否兼顾快与好。

**候选与被否**
- 被否：固定串行三角色（各自独立提示词、共享工具）。
- 被否：advisor 作为固定必走轮（含 per-PR 摘要一轮）。

**定案**：快模型默认全景（分组并行）+ 确定性风险路由触发慢模型深挖 + 跨家族并行 + 定向 verify。矛盾从
「时间 vs 质量」挪到「钱 vs 质量」：同一墙钟内用 token 买召回。

**依据**（量化账见 research §11）
- 墙钟 = 串行调用相加。reviewer 深挖与单发**同质**：聚焦省的是「看不该看的」，不是「写论证的 token」；top-K
  若多于单发自发现，输出反涨。advisor 的 PR 级摘要是纯增一轮。合计 ≈ 12-15 min，劣于 DeepSeek 单发 5.3 min
  两三倍。
- PR181 实证：最复杂线路（ocr：LLM 分组 + agent 循环）最慢（30 min / 3 findings）。更复杂 ≠ 更快。
- **分治 ≠ 角色分层**：分治并行摊平墙钟；角色分层串行相加。「快」的五个变量（TPS / 并行 / 输出精简 / 调用次数 /
  增量+cache）角色分层全部中性或负。
- 「发现便宜、论证贵」洞察成立，正确容器是同一次调用内的输出结构或风险路由的触发条件，不是三个独立 agent。

**代价与残余**：非热区只有快模型深度；路由规则会漏（靠台账回流）。

**勘误（2026-09-05，D6 连带）**：上表 reviewer 行「~25K @27 ≈ 10-11 min」把**总 output 同质**当成了**墙钟同质**。
TPS 是单流约束，K 个疑点各一条流并行，墙钟 = max 不是 sum（rpiv F1.2）。单发的 30.9K 是一条拆不开的流，疑点索引
把它变成 K 个可调度单元，这是 scanner 除宽召回外的第二重价值。此勘误**不复活串行三角色**（三角色的错在「固定串行」
与「必走 advisor 轮」，结论不变），但修正推论：慢模型的广度成本是钱与有界墙钟，「典型 PR 不碰慢模型」的边界改由
预算控制（D6）。上行「非热区只有快模型深度」随之改为「预算外疑点休眠、下一轮激活」。

---

## D2 波次模型 → 深挖优先级队列（2026-09-05）

**议题**：v2 把深挖写成「波次 1（H0）/ 波次 2（H1）」两段；用户提出应为 producer/consumer 队列，H0 先入队。

**候选与被否**
- 被否：波次模型（等**全部**快扫完成才开第二波）。
- 被否：FIFO 队列（话多的快模型会把规则命中的高分项排到后面）。
- 被否：无预算上限（快模型标一切为 high/低置信时退化为「慢模型审一切」，「快」消失）。
- 被否：深挖 cell 可再入队（链条无界）。

**定案**：快扫 = 静态 fan-out、纯生产者；深挖 = 优先级队列 + worker 池；三种生产者（规则 H0 / 快扫触发项 /
快扫 needs）产同一类型项；reduce 为 barrier。六条规则见 design §6。

**依据**
- 尾延迟：第 k 组扫完即可触发其热区深挖，不等最慢组；墙钟永不更差、尾部通常更短。
- 限流：worker 池大小即并发上限，天然对网关 429/503 友好（ocr 35 请求撞限的教训）。
- 统一抽象：H0 与 H1 同型，不特判波次；needs 与触发项同队列，不设三套机制。
- 五个必须钉住的细节及原因：①优先级键 = 风险分（规则项先于噪音项）；②入队合并去重（五个组各报同一热点五次）；
  ③预算截断且截断项**不静默**（以快扫 finding 原样进 reduce、标 `skipped(budget)`）；④终止显式（生产者关闭 →
  排空 → 在途 0），因 agreement ≥ 2 跳 verify 需全集；⑤H0 无笔记。
- **H0 无笔记的取舍**：给 H0 带笔记需等覆盖它的快扫组完成，重新引入串行依赖；独立审反而是多样性轴（同区域两模型
  独立看，reduce 计 agreement）。快扫后来命中同区域按去重丢，其 finding 本身仍进 reduce。〔勘误 D13：改为并入记录（agreement 累加、
  hint 附记录）并沿用结案，不丢；agreement 成活值。〕

**代价与残余**：实现复杂度（mpsc + 池 + 终止条件）；`max_cells`/`workers` 默认值待实测（R9）。

---

## D3 分组：确定性引用图优先，LLM 语义分组 opt-in（2026-09-05）

**议题**：design §4 对分组规则含糊；候选有 LLM 按 diff 分、AST + caller/callee、爆炸半径。

**候选与被否**
- 被否：默认 LLM 语义分组（ocr 路线）——多一次调用、丢确定性（C5、M15 重确定性）；对本仓价值未证。
- 被否：AST 重解析——本仓 regex 引用索引 Rust recall 1.0，已够作基底。
- 澄清：爆炸半径不是分组——分组看 changed 与 changed 的边，半径看 changed 与 unchanged 的边；同图两向。

**定案**：changed files 间引用图（引用边 / import 边强、同目录边弱）→ 连通分量 → 超预算先切弱边 → 仍超不切强边
改降内容（M15 L4「宁降不拆」）→ 孤立文件按目录 bin-packing。LLM 语义分组 `semantic = off` 默认，只喂路径 + churn，
仅在孤立文件占比高时值得开。门默认值取 ocr 实测（4 / 200 / 10）。

**依据**
- M15 L4 已定「拆格仅在弱耦合缝，文件级 import/引用图，regex 可建」，本条是其落地。
- 分组切错代价可控：半径已填（另一组的 caller 仍作上下文出现），reduce 再 dedup，退化为冗余而非盲区。
- LLM 分组能抓「同 feature 无 import 边」（跨语言同改契约），但那正是孤立文件多的场景，先按目录打包也能成组；
  留 opt-in 等 dogfood 出现 feature 级聚合缺失再开。

**代价与残余**：跨语言/跨模块横切改动可能分到不同组，靠跨组指针与半径兜底（R8）。

---

## D4 装包单位：改动中心切片，默认待实验（2026-09-05）

**议题**：文件粒度装包噪音大（几十个 helper 的 util 文件只改一个）；提示词「仅关注改动」对注意力约束有多强。

**候选与被否**
- 被否：直接把切片设为默认——lost-in-the-middle 未验证，F3 数据不足以裁决。
- 被否：维持整文件为唯一形态——注意力稀释只能靠不发治。
- 被否：依赖提示词约束——它约束「报什么」不约束「注意力分给谁」。

**定案**：分组单位 = 文件，装包单位 = changed symbol。切片配方 = 核（改动 def 全文 + diff）+ 文件内一跳依赖全文 +
文件地图（全部签名 + 文件头/模块 doc/常量块）+ 其余折叠 + 文件外片段 + 禁真空兜底。配置轴 `pack.body = full | slice`，
**默认 full**，E2 实验定。报错位置噪音由确定性后滤（positioning `anchor_window`）治。diff + 改动 def 置尾。

**依据**
- F3「全文 > 符号」的失败模式是**真空**（锚定失败给空壳）与**缺依赖**（切掉了改动函数用到的同文件符号），不是
  「少即差」的证明；两者分别由禁真空与文件内一跳补上。
- 文件地图必留：「已有 `foo_safe` 该复用」类 finding 需要知道文件里有什么。
- 位置：diff 置尾同时满足前缀缓存（稳定在前）与 LITM 高位，不需二选一。
- input 侧 cache 命中 99%，切片的**成本**论据弱；**质量**论据（稀释）才是真的，而它需要数据。
- Baz 文件级裁剪省 86% 不漏 + AACR naive RAG 反伤强模型：相关加入有益、无关加入有害或中性；切片是提高相关密度。

**代价与残余**：切错 → 缺依赖类漏（兜底三层）；E2 前不变默认。

---

## D5 三方 callee 正确性：本地源契约注入四层（2026-09-05）

**议题**：callee 为三方库时无文档/源码作上下文则调用方式错误发现不到；无 LSP，有也未必保证。

**候选与被否**
- 前提推翻：三方源码与契约**本地就有**（cargo registry / go mod cache / node `.d.ts` / site-packages）。
- 被否：依赖 LSP——LSP 给的也只是签名 + doc，静态抽取同样拿得到；行为级 oracle 只有编译器与测试。
- 被否：跳过三方 callee——关键模块的调用错误后果重。
- 被否：仅按需注入契约——每 callee ~100 token、全注入几 K，input 慷慨；按需只留给函数体（C 层）。

**定案**：四层。A 编译门（M15 D8 已有，类型级）；B 契约注入（S2-b 扩到 registry 源：签名 + doc + 短函数体，按
Cargo.lock 精确版本，默认全注入）；C needs 联动（`needs{kind: api}` → 深挖队列 → 带契约与函数体）；D 测试执行
（M20 R6 钩子位）。风险路由加「关键模块 + 外部 API 密集」结构信号。

**依据**
- B 层真正解决的是**版本漂移**：模型参数知识记某版本 API，代码用 Cargo.lock 那个版本，注入以后者为准。
- Rust 定位链确定性：`use` → crate → Cargo.lock 版本 → registry 路径 → `pub fn` + `///` 块；歧义带 crate 标签全给。
- S2-b 已证 callee 注入产量可观（单 fn 13 条、变更集 77 条）且 regex 做不到——扩根路径即可。

**代价与残余**：doc 未写的前置条件 + 无测试 + 模型不知 → 不可发现，与无该库领域知识的人类评审者同一残余，
接受为边界。非 Rust 生态定位链后置（R10）。

---

## D6 吸收 rpiv 交叉评审（2026-09-05）

**议题**：用户另取两份 LLM 方案交叉。`~/llm-code-review-architecture.md` 为共同祖先（v0，无增量，其独有主张已在 D3
与 research §6.3 裁过）；rpiv 研究（`.rpiv/artifacts/research/2026-09-04_10-26-56_llm-code-review-multi-agent.md`，含被
追问「快」后的 F1-F5 追加）与 v3 逐点对照，证据摘录见 research §13。

**采纳（8 项）**
1. **快扫输出改疑点索引**（<50 token、quote 必填、疑似即列）：finding 模式门槛高，「知道哪里可疑但写不出来」的信号
   蒸发；疑点模式召回高，筛与论证交慢模型。finding 只由深挖产出（C7）。
2. **全部疑点入队 + 预算随风险分 + 必挖集**：v3「只有 severity ≥ high ∧ conf < τ 触发」与 rpiv「全部疑点吃预算」在
   D1 勘误后合并——H0 规则项最高优先、必挖集（H0 ∪ 高置信 blocking）不截、其余按 `置信 × agreement × 严重度` 吃到
   `f(R, 规模)`。不用置信阈值切（边界疑点是漏报敏感带）。
3. **疑点台账 + 增量轮 delta 激活 + 收敛三维 + 活锁守卫**：预算截断从「漏」变「延迟到下一轮」，是预算敢保守的前提；
   carry 机器零改动复用；休眠疑点不进 verdict（verdict 纯函数是资产）。
4. **`[roles.X]` 角色层**：thinking_level 是角色属性（同模型两档消费），fallback 只在同 TPS 档（跨档回退 = 静默 4×
   反转），项目层剥离清单加 roles。
5. **正反向跨组索引 + `[delta]` 标注**：`extract_imports` 边算完即丢，提升为流水线产物零成本；反向索引是
   manifestation-site 信息。
6. **段序**：角色尾（输出契约 + 指令）移到证据与 diff 之后：共享前缀延伸到 diff，分化只在尾部；与 D4「diff 置尾」合并为
   「diff 次尾 + 短角色尾」。
7. **判例前移进快扫 prompt**（机械降级、不直接丢）：假阳从下游压制变上游不生成。
8. **coverage 五集合 + 角色分级**（快扫失败硬 Incomplete / 深挖截断带债 approve）；小项：ocr 跨文件重定位、
   `scanned_areas`、refute analysis 先于 verdict、deep cell 可选增量提交（价值是中途死亡持久化，非防截断）。

**未采纳（5 项）**
- 三阶段独立 orchestrator：单编排器 + 队列已是 rpiv F5「LLM 声明、编排器调度」形态；其四轴顾虑（CellRecord 装不下疑点、
  resume 语义、per-fanout 单模型计划、manifest/partial）成立但属实现债，记 R14。
- advisor 对每个疑点做 LLM 三元裁决：确定性排序 + 预算即可，rpiv F3 自己也转为机械排序。
- 疑点索引必须工具化：体量太小（几十条 × 50 token）截不了；deep cell 可选开。
- 扫描遍 0.5 min 估计：DeepSeek 单发实测 5.3 min，大头是读 145K 与想，不是写；分组并行后取 2-3 min。
- 三阶段各自扇出粒度作为架构轴：队列项统一了粒度，不设三套扇出。

**保留 v3 独有**：确定性风险路由 H0（rpiv 深挖目标全靠疑点；授权类 bug 所有快模型都弱，规则命中即深挖是漏报根源①
最确定的对症）；L4 三方契约；D4 切片轴。

**依据**：rpiv 文档贴代码写（ThinkingLevel 零消费、剥离清单、边被丢弃、harness 只测 dossier 召回），其实现级发现可
直接进 plan；其外部证据（BitsAI-CR / Multi-Review / agreeableness bias / End of Code Review 等）补入 research §13。

**代价与残余**：低风险 PR 墙钟 +1-2 min（预算内并行深挖）；台账激活依赖 delta 触及锚定文件；实现债 R14；
findings-recall oracle 缺口 R15。

---

## D7 配置与基准（grill，2026-09-05）

**议题**：用户提出配置改 YAML 并给出 providers / scanners / reviewers 形状；测试集换 aacr-bench。逐分支 grill 后定案。
事实基础：本仓 config 已是 providers / models / review 三段、项目层剥离 providers+models、secrets 按 (provider, model_id)
二维键、无 google kind、只有全局 max_concurrency、`model setup` 走 `toml::Value` 重序列化（注释已丢）；`serde_yaml`
已弃用、替代品碎片化；aacr-bench 事实见 research §14。

**A 配置**

| # | 定案 | 被否 | 依据 |
|---|---|---|---|
| A1 | TOML；providers 键名表 | YAML | 同形状行数持平（33 vs 33）；`toml_edit` 支撑后续交互式配置保注释；`serde_yaml` 弃用；YAML 1.1 隐式类型踩坑面 |
| A2 | 角色条目内联模型字段 + 可选 `name`；cell 命名向 config 对齐（scanner / reviewer / verifier） | `[models]` + `[roles]` 两层引用 | 典型 2-4 个模型，间接层不值；thinking 是角色属性在内联里天然成立；`name` 是身份串 |
| A3 | 列表 = 活跃集全并行（scanners 疑点并集、reviewers 分派池、verifiers 选择池）；**无候补** | `standby` 候补、FIFO、failover 链 | 用户判断：失败根因是网关不稳，候补用不上且加复杂度（YAGNI）；失败路径由 coverage `failed` 分级承接；同角色即同档，D6 采纳项 4 的 fallback 部分撤回 |
| A4 | verifier 保留、流式化（finding 落地即复核）、触发集 blocking + 判例变体、跳过 = 两异族 scanner + reviewer 三方一致、`enabled` 旋钮 + refute 率数据判决；`[[verifiers]]` 可选、缺省自动异族 | 砍掉复核；必配 verifiers；多 verifier 投票 | 复核墙钟 = 并行一次调用且可与深挖重叠，非大头；它是唯一以推翻为目标的环节与判例终审；去留交数据（铁律 1） |
| A5 | provider 级 `max_concurrency` = 在途上限，缺省继承 `review.max_concurrency` | RPM 令牌桶（R16） | 实测失败形态是并发密度触发的 429/503/524；持续超速由重试退避吸收 |
| A6 | 不新增 google 后端 | 顺手做 | 每个后端要重踩非流式 524 / SSE error / usage 低报 / finish_reason 缺失；R17 |
| A7 | secrets：`[providers.X] api_key` + `"<model_id>" = key`（值可内联表）；引号守卫、未知键警告 | `[models.<id>]` 独立表；provider 嵌套 `models` 子表 | 用户判断：一 key 多 provider 几乎不出现，provider 内嵌最简且无歧义 |
| A8 | 用户级唯一 `~/.redteam/`，`REDTEAM_HOME` 覆盖；项目 `.redteam/` 只状态与内容；评审旋钮走 CLI flag / env；`--test-command` | 保留项目层 config | provider 在途上限是 key 属性天然跨项目；项目层每一项要么是旋钮（PR 可改）要么是内容；剥离清单代码整段删除 |
| A9 | 不做迁移与兼容 | `config migrate` | 未发布仅 dogfood |

**B 基准**

| # | 定案 | 被否 | 依据 |
|---|---|---|---|
| B1 | aacr-bench 全在 fork `itcook/aacr-bench`（数据、评估器、`reviewers/redteam.py`、子集）；本仓只 `eval/README` + `AACR_BENCH_DIR` | vendor 进 `eval/`；submodule | 用户判断：测试数据不在本仓维护；reviewer 硬编码在 pipeline 需改其代码，fork 是自然落点 |
| B2 | `smoke` 5 正；`gate` 规模重启时按 spike 定 | 30+10 / 三语言全 114 | 成本；15 个 PR 约 60 条标注，<10pp 差异 N=1 不可分，故 gate N=3 |
| B3 | mock judge 内循环取 Line-P/R（前三阶段确定性、零成本）；出口 GLM judge（家族与角色不重合）；可调 | 全程 LLM judge | 匹配四阶段只有 semantic 是 LLM |
| B4 | **aacr 暂退役**；主场 PR181（连续性、网关真实度）+ dogfood（多轮、台账、ship）；重启点 M21c 翻转前 `smoke`、M21e 收官 `gate` | 现阶段跑 aacr | 用户判断：早期里程碑需分钟级信号，40 PR 四小时拖慢迭代；翻转与收官是绝对召回必须被量的两个时刻 |
| B5 | ocr 参照 = deepseek-v4-pro 直连 chat-completions 流式，版本钉；Claude D 参照后置 | AGC 网关跑 ocr | PR181 三跑只有 chat-completions 流式未撞 524 |
| B6 | 负样本 N=1；N=3 只在关键出口；估算按 pack 字节 ÷ 3.5 | — | 网关 usage 低报（GLM 7K vs 145K） |
| B7 | P0 E1/E3 回 PR181 基座 + proj.db 历史 upheld 作小 oracle；`--pack-only` 预算仪表 M21a 顺手 | E1/E3 改 aacr 基座 | 随 B4 |

**连带**：D6 采纳项 4（`[roles.X]` + 同档 fallback）中 fallback 撤回、角色层改为顶层角色列表；design v5 §12 重写；
`priority` / `--models` / `--models-all` / 项目层配置 / `XDG_CONFIG_HOME` 退场。

**代价与残余**：无候补下 provider 不稳直接体现为 coverage `failed`；PR181 + dogfood 无标注、测不出绝对召回，缺口在
M21c 翻转前由 aacr smoke 补；RPM / google / diffsym 语言 / 交互式配置进 R16-R19。

---

## D8 scanner 组 pack 默认 `slice`（D4 修订，2026-09-05）

**议题**：D4 把 `pack.body` 默认留 `full`、等 E2 出数据。用户直觉 full 必有噪音与 token 浪费，提议 slice 优先、full 留作
召回受损时的回退测试。

**候选与被否**
- 被否：维持 `full` 默认等 E2——合成 util 文件实验不如真 PR 的 A/B，且 reviewer 侧本来就已切片，悬而不决只剩 scanner 一处。
- 被否：删掉 `full`——回退需要它作对照与逃生口。

**定案**：`[review.pack] body` 默认 `slice`，只管 scanner 的组 pack；reviewer 区域 pack 由构造即切片。保险四条：`full` 留旋钮；
切片三层（禁真空兜底 / 文件内一跳 / 文件地图）同批交付；小文件（≤ `slice_min_bytes`，默认 8K）直接全文；回退信号写死
（M21a 出口 PR181 A/B 中 slice 疑点命中低于 full，或 dogfood 中 scanner `needs{kind: evidence}` 触发率持续偏高 → 先二跳
再 full）。E2 改为 M21a 出口的 slice vs full A/B，合成 LITM 实验降为可选。

**依据**
- reviewer cell 在 v5 §7 已是「热区切片 + 半径」，`body` 轴事实上只剩 scanner 一个消费者。
- scanner 的职责是广度：改动 def 全文 + 一跳 + 全部签名足以「怀疑」，被折叠的函数体它知道存在，缺证据走 `needs`，由
  reviewer 拿全文论证——这是设计里给它的出口。
- full 的代价在 scanner 上被乘法放大：组 pack × scanner 条目数 × samples。
- F3 的失败模式（真空、缺依赖）各有对症层；slice 优先是「配方修好了 F3」的赌注，保险即上述四条。

**代价与残余**：若配方没修好 F3，M21a 出口 A/B 才会暴露；回退路径已定。切片装包从 M21d 提前到 M21a，M21a 范围再增。

---

## D9 提示词按角色分离（2026-09-05）

**议题**：用户指出 design / roadmap 未明确「提示词按 scanner / reviewer / verifier 分离与优化」。此前散落在 §4 / §7 / §8.1 /
§10.3，且 M6-b 提示词 harness 被并入 M20 backlog。

**候选与被否**
- 被否：单一模板 + mode 开关——分化段若在证据前会截断共享前缀（rpiv 段序翻转论据）；三种输出契约混在一个模板里与
  parse 字段级对齐做不到；三种立场（广度怀疑 / 深度建设 / 单点推翻）互相污染。
- 被否：提示词评测继续放 M20——三角色各有零新数据的离线回归集可用，不用等 harness 立项。

**定案**：design §10.7。共享头（字节稳定：协议、规则、立场、CI 跳过清单、免疫段、brief、spec）+ 三个角色尾（唯一分化段，
含各自输出契约与指令）；模板文件 `shared-head.md` + `scanner.md` / `reviewer.md` / `verifier.md`；注入段全过
`neutralize_structure`；思考档位由条目给、prompt 不写散文；模板哈希入 RunMeta；离线回归集：verifier 用 proj.db
dismissed（假阳）vs upheld（真）做二分类，scanner 用 PR181 疑点 oracle，reviewer 用确认率 + 其后 refute 率。

**依据**
- ReviewBench：结构化提示单独把某模型 F1 0.13→0.32，提示架构本身是性能面（research §10.4）。
- PR-Agent 一命令一提示词的先例（research §10.2）；Critic 必须独立会话、独立立场（research §13.2）。
- 段序翻转要求分化只在尾部（D6 采纳项 6），角色尾正是那个尾。
- proj.db 的 dismissed / upheld 记录天然是 verifier 的标注集，零成本。

**代价与残余**：三套模板维护；M6-b 的一部分提前进 M21a / M21b；scanner 与 reviewer 的回归集依赖 PR181 oracle，绝对值仍待
aacr 重启。

---

## D10 收尾四项 + 不新开项目（2026-09-05）

**议题**：用户问增量提交、项目状态 / 台账 / 内容数据、输入缓存优化是否还需讨论、放 M21 还是 followup；并担心重构面太大、
旧文档与代码成为噪音，是否新开项目。

**定案**

| # | 定案 | 被否 | 依据 |
|---|---|---|---|
| 1 增量提交 | **客户端流式持久化**：SSE 流上对 `findings` 数组增量 JSON 解析，对象闭合即落盘；无 `submit` 旋钮，R13 消 | 工具循环 `submit_finding`；`final \| incremental` 旋钮 | 响应已是流式（附录坑：非流式 = 死刑）；不改模型契约、不重开工具循环（C4）、零额外 token；FINALIZE / REPAIR 仍管最终解析 |
| 2 状态数据 | **proj.db schema v4 一次重置**（M21a）：cell 带角色、suspicions、queue_snapshot、coverage 五集合、runs meta；不写迁移；**重置前导出**旧库 dismissed / upheld findings 为 `eval/` 固定集 | 三个里程碑各迁一次 | 未发布仅 dogfood；分三次迁移 = 三份迁移代码与三次风险；旧库的 dismissed / upheld 是 verifier 离线回归集的全部数据，不能丢 |
| 3 输入缓存 | **结构性部分在 M21a**：段序翻转 + anthropic kind 三段界 `cache_control` 断点（共享头末 / 证据段末 / 角色尾前）+ RunMeta 按 cell 记命中率；**非结构性进 R21**（provider 专属 context caching、跨 run 预热、磁盘缓存调参） | 全部后置；或全部进 M21 | 现有断点实现因三 provider 皆 openai kind 从未跑到，而用户的 GLM / Kimi 正是 anthropic kind，断点是免费收益；provider 专属 API 各家不同，先看命中率数据再定 |
| 4 分支形态 | **分支硬切，无 `engine` 旗**：`feat/m21-review-engine` 直接删除旧编排（fan-out、候补链、`--models` 四分支、项目层配置）；对照基线 = `dev` 分支二进制并排跑同一 PR；切名前分支产物 `redteam-next`，插件与 ship 循环继续用 dev 的 `redteam`，M21c 切名 | `engine = legacy \| v2` 旗贯穿 M21a-c（原铁律 6） | 两条编排路径并存才是噪音与陈旧假设的温床（M15 翻转当天四个集成 bug 的 precedent）；对照能力由 dev 二进制提供，不需要在同一二进制里养两条路 |
| PS 新开项目 | **不新开**，在本仓分支硬切 | 新 repo 重来 | 新 repo 丢掉的恰是最贵的：provider 层那串坑（524 / SSE error / usage 低报 / finish_reason）、路径注入修复与威胁模型 B、E11 注入加固、regex 引用索引（recall 1.0）、pack 梯子、dedup / carry、判例台账、exit code 契约、ship 协议、四百多个测试；design §15「其余原样」即这份清单。文档噪音靠 `CLAUDE.md` 常用节加「M21 阅读入口」压住，不搬 `.doc/`（互链会断） |

**连带**：铁律 6 改写；M21a 验收「legacy 路径零行为变化」改为「dev 二进制基线归档」；M21c「默认翻转」改为「切名」；
R13 消、R14 扩、R21 增；design §7 / §10.3 / §11 / §12.1 / §15 / §16 / §17 对应改。

**代价与残余**：M21a、M21b 期间 `redteam-next` 审不出 finding，dogfood 靠 dev 二进制；两个二进制名要在 Makefile / 插件
放置里区分；流式持久化只能保住已闭合的对象，未闭合的仍随 cell 死亡丢失。

---

## D11 P0 出口裁决（grill，2026-09-07）

**议题**：P0 验证门（E1 风险规则回放 + E3 疑点契约 A/B，数据见 `../private/research/2026-09-07-redteam-p0-validation-gate-research.md`
§15 / §16，纯数字摘要见 `../roadmap/P0-validation-gate.md`）之后，design 锁定前的必答项：R5（涟漪去留 / 两类信号 / θ 与权重落点 /
预算三档）、R11 `c_hi`、R6 `--risk all` 等价性、R14 实现债路线。

| # | 定案 | 被否 | 依据 |
|---|---|---|---|
| 1 R5-a | **涟漪卫星区保留，作 H0 第二梯队**：主区（有改动行）必挖且排最前；涟漪区（改动符号在本文件 ≤ 4 处引用点的 enclosing def）排主区之后、不进必挖集、预算截断先截它；不做跨文件涟漪 | 不进规则（靠疑点 / dossier 碰）；与主区同级 | PR181 主裁决集靠它 +1（T2，finding 引用未改动的 fence 代码）、oracle B +2；代价 H0 +30-40% 且新增区多为空——放第二梯队让可达性只花预算不挤主区 |
| 2 R5-b | **两类独立信号规则写进 §3.3**：入 H0 需 score ≥ θ 且命中 ≥ 2 类信号（路径/类别、结构、外部 API、历史），关键词类单独不成区 | 只靠 θ 调 | v3→v4 唯一让 held-out 集守住的改动：大 diff 上 H0 占区 45-52% → 10-24%，PR181 主裁决集 4/4 不变；θ 单调不可兼得（2.0 时主裁决集 1/4） |
| 3 R5-c | **不设 `[review.risk]`、不加 `--risk`**，规则只有一种行为；θ=1.5 与权重表为 Rust `risk` 模块常量，改动必须过 `eval/risk_replay.py` 回放 PR181（私有 fixture）+ 本仓 oracle B（入库 fixture）并把数字写进 research；快扫档将来走独立途径（如 `redteam triage`） | `mode = off\|auto\|all` 三档；权重进配置 | 用户判断：快速验证期不加分支特性；数字只在一个语料上标定过，开旋钮等于把标定责任推给用户，`--help` 还要解释十几个数字；§12.1 孤儿键 `threshold = 0.3` 无定义、随节删除 |
| 4 R5-d | **预算**：R = Σ_{H0} score ÷ (1 + ln(1 + 代码改动行))，切点 0.5 / 3.5；`max_cells = "auto"` 起步 low 4 / mid 12 / high 32，硬帽 48；必挖集 = 主区不截，主区超帽时只做主区并在 RunMeta 披露；`max_cells` 整数固定上限保留 | high 24（保守）/ 48（激进） | 22 个有代码 diff 的 R 三分位 \|H0\| 中位 0 / 5 / 23，起步值 = 中位 + 疑点余量；起步值，M21b 出口按截断数 / 台账落入数回校 |
| 5 R11 | **必挖集 = 主区 ∪ {agreement ≥ 2 的疑点}**（跨 scanner 或跨 samples 经 quote 轻锚 + hint 归一合并）；confidence 只进优先级公式不做门；删「severity ≥ block」（疑点无 severity）；**预算分池保底**：每轮为 scanner 疑点保留 ⌈max_cells/4⌉ 格（1 / 3 / 8），主区吃穿时帽 = \|主区\| + 保底 | `c_hi` 置信阈值；按 category 必挖 | E3：命中项 confidence 0.25-0.50、全体均值 0.36，`c_hi ≥ 0.5` 只留 1/8 命中——flash 级自评置信与命中无关；用户担心「agreement=1 且非热区的真 bug」被饿死（E3 有两例）→ 保底格保证最坏情况 top-K 疑点仍深挖，其余走台账休眠 + delta 激活 + 披露；category 自标可靠性未验，M21c 看台账数据再议 |
| 6 R6 | **销案**：对照 = dev 旧二进制与新二进制并排跑 PR181，比 P0 oracle（22 条）命中数，不追求 finding 逐条相同（提示词已按角色分离）；M21b / M21c / M21e 三处 `--risk all` 验收改写；慢模型全景档不设 | 保留 `--risk all` 为对照档 / 全景档 | 随 3 无载体；D10 已定对照基线 = dev 二进制；M21e 收官对照是 dev 二进制最后一次上场，与仪器拆除条目一致 |
| 7 R14 | **cell 记录加 `role` + 按角色分叉的输出枚举**（scanner：疑点 + needs + scanned_areas；reviewer：findings + needs；verifier：判决），一 cell 一条 JSON 记录；**单独建表只有疑点台账**（锚点、hint、状态、最近作业、激活次数）**与队列快照**；coverage 五集合进作业元数据；resume = 读快照重跑未完项，scanner cell 缺失才重跑（清单差集语义保留） | findings / suspicions / needs 各一张表；一条记录两个可空列表 | 产品从不按列查 finding、整条读；枚举让记录类型在类型上说清，避免两列表同时非空的糊涂态（M15 翻转当天集成 bug 的温床）；台账须跨作业按锚点查改状态，必须成表；拆多表 = 多份迁移面与测试面而 proj.db 是小库 |

**E3 连带（实现级，数据支撑，随 D11 记）**
- M21a scanner 请求显式钉推理档（`thinking_level` 首个消费者），`samples` 作方差对冲；零推理 + 短输出记 RunMeta 观测信号，不重跑（铁律 7）——E3 疑点臂 2/3 跑几乎零推理（11-16 s）。
- 疑点契约：quote 逐字硬约束 + 不在文件即丢弃（E3 未定位 5/32 vs finding 契约 1/11）；category 保留 L0-L5；hint 归一化后置 M21c 看台账数据。
- 规则初版已知边界写进 §3.3：P0 用了四轮（r2 / r3 为度量修正、r4 由 held-out 驱动）；held-out oracle B 区间级 0.59 / 文件级 0.85；漏项主因是改动函数之外的交互点。

**连带**：design v8 锁定——§3.3 重写为规则 v4 定案、§4 补 quote 丢弃与推理档、§6 行 1 / 3 改写、§12.1 删 `[review.risk]`、§16 记结果、§17 R5 / R6 / R11 / R14 标已裁；
roadmap P0 ✅、followup 四行归位、M21b / M21c / M21e / M21a 的 `--risk` 与 `engine` 提法清除；主 checkout `.doc/roadmap/summary.md` 登记 M21。

**代价与残余**：默认配置（单 scanner、samples=1）下必挖集 = 纯规则主区，疑点全部走预算 + 保底；大 diff 首轮只保证 top-K 疑点深挖；
规则权重不可用户调，跨语言 / 跨仓库校准留 R18；`--risk off` 型快扫无入口，需要时另立命令。

---

## D12 配置格式与形状：JSON + provider → models → roles（2026-09-08，取代 D7 A1 / A2）

**议题**：批 A 按 D7 A2 落地（be87c44）后用户看到实际配置：同一模型在两个角色各写一遍全部字段、`[[scanners]]` 表数组不直观、
角色 / 模型引用不清；批 A ship 评审与 code-review（high）的必修项 #2 #3 #4（原位更新整条替换、覆写共享 provider 字段、写前不校验）
也都源于「内联条目 + 表数组」。用户重提格式（TOML 不喜欢；问 JSON）并给出 JSON 示例。

**候选与被否**
- 被否：保留 TOML 只改形状（两层 `[models]` + `[roles]`）——能解形状问题，但用户明确不用 TOML。
- 被否：YAML——D7 A1 的三条理由仍成立（无成熟保注释编辑器、`serde_yaml` 弃用且替代碎片化、隐式类型脚枪）。
- 被否：JSON 允许 `//` 注释（`json_comments` 剥离）——用户选不要注释：少一个 crate、少一条「写回丢注释」的解释。

**定案**
1. **格式**：`$REDTEAM_HOME/config.json` + `secrets.json`（缺省 `~/.redteam/`），严格 JSON（`serde_json`，已是依赖），键 camelCase，
   无注释；`toml` 依赖退出。JSON 正好绕开 D7 A1 的三条顾虑。
2. **形状**（模型固有字段只写一次，角色属性挂模型下）：
   `providers.<name>{ kind?, baseUrl?, apiKeyEnv?, maxConcurrency?, models.<id>{ displayName?, kind?, apiKeyEnv?, family?, contextWindow?,
   maxOutputTokens?, perTurnTimeoutSecs?, reasoning?, roles.<scanner|reviewer|verifier>{ thinkingLevel? } } }` + `review{ … }`。
   某角色的活跃集 = 拥有该角色键的全部模型（列表即活跃集、无候补，D7 A3 不变）；顺序按 provider 名、model id 字典序（无语义，不为保序引依赖）。
3. **身份**：route_key / 报告名 = `provider/id`（两者都是必选、请求体里传的也是 id）；`displayName` 可选、只作人读显示（2026-09-08 重做评审后
   由 `name` 改名，防被当身份串）；`family` 缺省 id 首段。`kind` = 模型层 > provider 层，两者都缺 → 校验报错。
4. **secrets.json**：`{"providers": {"<name>": {"apiKey": "…", "models": {"<id>": "…"}}}}`；解析链：模型 `apiKeyEnv`(env) → secrets.models[id]
   → provider `apiKeyEnv`(env) → secrets.apiKey；守卫：secrets 里的 provider 名 / model id 在 config 无对应 → warning；chmod 600。
5. **`model setup`**：`--provider X --model <id> --role scanner[,reviewer,verifier] [--name --kind --base-url --max-concurrency --context-window
   --max-output-tokens --reasoning --thinking-level --key --key-store env|secrets --model-key]`；provider / model / role 三层各自**只合并显式给出的字段**；
   写前对合并结果做完整解析 + 校验，失败拒写；现有文件解析失败拒写不覆写（`write_secret` 同策略）。
6. **并入 code-review（high）项**：HOME 与 REDTEAM_HOME 皆缺 → 报错，不退化为相对路径（#1，安全）；单轮超时 = 模型 `perTurnTimeoutSecs` >
   `REDTEAM_PER_TURN_SECS` > `实际生效 max_tokens ÷ 20 + 60` > 150s（#6）；`contextWindow` 下限（#7）；纯注释的旧 `.redteam/config.toml`
   不刷提示（#9）；secrets 未知 provider 名告警（#10）；`infer_test_command` 不把 `test:=` / npm 占位脚本当测试命令（#11）；读配置区分
   不存在与权限错误（#13）；`--resume` 帮助文案改正（#14）；setup 落盘后探测走刚写模型的解析链（#5）。ship.md 透传 `--models`（#12）记 followup，M21c 一并改。

**依据**：用户判断（格式）；按 id 做键的对象天然消灭表数组与原位更新歧义；显式字段合并消灭跨条目覆写；`serde_json` 是本仓最稳的依赖。

**代价与残余**：无注释——`--help` 与 `redteam model list` 是在场文档；批 A 首版（TOML）整体重做约半天，在 `feat/m21a-config-json`；
手改 JSON 易错（尾逗号），报错须带行列；design §12 重写、roadmap M21a 交付项 1 与 plan 批 A 措辞同步。

---

## D13 并发形态：删角色 `workers`、唯一闸 = provider 门、队列入口五态、429 排队重试（grill，2026-09-09）

**议题**：M21a 出口验收发现 `review.scanners.workers`（缺省 4）< PR181 7 格造成 100-130 s 排队，而 provider `maxConcurrency` 从不是约束门；
用户提出 workers 不该是用户级配置键，且自适应规则要看同 provider 上 reviewer / verifier 的占用。grill 走到根问题：角色级并行数到底解决什么。
中途用户校正两处前提（agreement 是否要等两家 scanner；是否逐疑点 / 逐 finding 边跑边入队），先核文档再定。

**事实核定（grill 中查证）**
- 角色信号量是跨 provider 全局一把：deepseek + qwen 双 scanner 14 格被同一个 4 卡住，两个独立 provider 互相拖累。
- 设计里 scanner → 队列一直是逐 cell 交付（D2「第 k 组扫完即可触发」），从未写逐疑点；reviewer → verifier 逐 finding 是 D10 客户端增量解析；
  深挖队列是优先级队列，D2 明确否掉 FIFO（本次说的 FIFO 是 provider 门的排队序，两者不同）。
- 模型在一次请求内不能反复调工具再交付：工具调用结束该轮，循环由客户端驱动、每轮重付 TTFT 与整段推理（思考模型分钟级）；provider 托管工具
  要求公网端点，不适用本地仓库。故「边挖边交」只能是客户端解析输出流，D10 已定，不需要工具。
- PR181 每格输出 10-20K token，疑点 JSON 总计约 1-2K，可见 JSON 只在每格尾部 10-20 s 产出 → scanner 逐疑点流式上限收益 10-20 s 且只在最后一格。
- 用户当前配置 scanner（deepseek / qwen）与 reviewer / verifier（agc）不共享 provider；唯一共享是 reviewer 对 verifier。
- 现有 429 路径：可重试、预算首发 + 2、退避 Retry-After（整数秒、封顶 30 s）或 400 / 800 ms，重试期间持门名额；占网关槽位的是分钟级生成请求，
  1.2 s 内的重试必再撞，兜的是抖动不是超发。

**候选与被否**
| # | 定案 | 被否 | 依据 |
|---|---|---|---|
| Q1 角色 `workers` 的用途 | 只有「共享 provider 时的角色间分配」一个正当用途；不做节流帽、不设 `--workers` | A 保护 provider（与 `maxConcurrency` 重复）；C 单次节流旗 | 角色帽在 PR181 只起副作用；单次节流的真实需求（两个进程打同一 provider）是跨进程的，旗解决不了；用户：减心智模型 + YAGNI |
| Q2 门上仲裁 | C：单把 FIFO Semaphore + 入队序（t=0 scanner 格先于 H0 项）；无调度器、无数字；观测每 provider 每角色在途峰值与门等待 | A 静态预留数；B 下游优先（verifier > reviewer > scanner） | 墙钟下界 = 最长链（最后一个 scanner 格 → 其 reviewer 项 → 复核），任何预留改不了；门饱和时 FIFO 无空转，预留只换顺序不换总量（60 格 / 32 项 / 门 16 仿真：两臂都贴 17.25 min 下界）；预留让 reviewer 用不完整信息提前吃 `max_cells` 预算，质量更差；下游优先会让 H0 项饿死 scanner，最长链最晚开工；最长链优先 = scanner ≥ reviewer ≥ verifier，恰是 FIFO 自然序。用户最初担心「reviewer / verifier 单元更长却排队」——按链长而非单元时长看，排在后面是对的 |
| Q3 键删几个 | 三个一起删（scanners / reviewers / verifiers） | 只删 scanners | 另两个无消费者、C 后无数字可填；形状守卫对未知键只 warning，老配置不炸；示例配置一次改齐 |
| Q-B 交付粒度 | 按文档现状定死：scanner 逐 cell 交付、队列项按区域、reviewer 逐 finding 落盘定性为持久化旁路（墙钟视同逐 cell）；不加 scanner 逐疑点流式 | scanner 逐疑点流式 | 收益上限 10-20 s 且只在最后一格；D10 价值是格死亡时已闭合对象不丢，非延迟 |
| Q-A 同轮并入 + 跨轮沿用结案 | 队列入口五态（无命中 / 排队中并入 / 在途只改记录 / 本轮已结案沿用 / 跨轮判例守卫）；守卫 = 锚定可重定位 ∧ 区域自判例以来无 delta；agreement 成活值，三消费者不变，跳过规则按落地时刻 | v9「已处理或在途直接丢（set 语义）」；v9「判例命中降级候选」；无守卫直接沿用 | 丢会让必挖集与跳过规则中途看不到 agreement；无守卫的漏报路径 = fix 引入新 bug 被旧判例盖住；守卫用 carry 机器 anchor_near，零新件；入口是编排器确定性代码，scanner 单发看不到别家、reviewer 永远见不到重复 |
| Q4 同组 hold | A 不等同组兄弟 cell（先到先挖，后到并入记录）；记派发时 / 落地时 agreement | B 等同组兄弟 cell 攒齐两家笔记 | 用户配置下常态是先到项已在途，agreement 2 只落记录；hold 代价 = 最后一组尾巴多等两家返回时差；数据先行，M21e 看两字段再定 |
| Q5 缺省与 429 | `review.maxConcurrency` 8 → 10；429 排队重试：门宽 ← max(1, min(门宽 − 1, 此刻仍在跑且已被接受数))、本轮不回升；撞 429 者持名额等本 provider 下一完成（每次完成只唤醒一个）或 Retry-After 取晚者；独立计数上限 = 当时有效门宽 × 2；门收窄后多余名额释放时丢弃；`maxConcurrency > 128` warning；RunMeta / warnings 记 429 次数与门宽 A → B | A 只改数字；缺省 16（样本只有 deepseek 一家）；C 删全局缺省强制显式；「缩 1」（对误填 65536 无效） | 抬缺省的理由「有兜底就可激进」只在有真排队重试时成立；按在途接受数收门一波内收敛到网关真实上限，错配 `maxConcurrency` 自愈；单唤醒防 95 个等待者一起重试；机制来源无关，显式与继承同门；R16 令牌桶的最小替身，与 A5「失败形态是并发密度」对上 |

**依据（用户判断）**：并发与评审内容相关、不是用户偏好，不该是配置键；规则合理则 `--workers` 也不要（减心智模型 + YAGNI）；预留问题在
「reviewer 无消费物则不排队、有积压才排」的前提下只剩共享 provider 一种情形，按链长论证接受不预留。

**代价与残余**
- 没有任何单次节流手段；两个进程同打一个 provider 仍会超发，靠 429 排队重试兜（跨进程门是 R16 / M20）。
- 门本轮只降不升：一次偶发 429 会让该 provider 本轮少一个槽；可接受，RunMeta 可见。
- reviewer 常态只看一家笔记；agreement 对派发的影响只在积压时体现；两字段数据决定 M21e 是否加 hold。
- `InflightGate` 从 tokio Semaphore 换自管计数 + Notify；M21a 收口批只动 scanner 路径，队列入口五态、agreement 两字段随 M21b。
- design v10：§0 / §2 / §4 / §5 / §6 / §7 / §8.1 / §12 / §12.3 / §13 / §14 / §15 / §17 对应改；D2 加勘误；roadmap followup / M21a / M21b / M21c / M21e / README 同步。

## D14 分组策略：小分量与孤立文件同池装箱（2026-09-10，M21b 前置）

- **背景**：PR181 分组 7 组超「≤5」目标；4 组是 ≤3 文件小强分量（同目录文档对、schema+测试对）各自直出，组数随小分量数线性涨，每组一次 scanner 调用。
- **裁定（用户，2026-09-10 m21b-risk 脑暴，二选一）**：选项 1——小分量（文件数 ≤ 3 且估算字节 ≤ 预算/4）作**原子单元**与孤立文件同池装箱；弃选项 2（组数门槛改「≤ 强分量数 + 1」，等价效果但规则绕、需二次定义强分量）。
- **不变量**：小分量原子进池不拆散——强边不跨组保持（哨兵测试覆盖）；budget=None 无预算信息不判（保守分量直出）。
- **零配置旋钮**：阈值常量 `SMALL_COMPONENT_MAX_FILES=3` / `SMALL_COMPONENT_BUDGET_DIV=4`，不加 `[review.grouping]` 键。
- **观测**：`GroupingStats.pooled_small_components: Option<usize>`（serde default，旧报告 None ≠ 零）。
- **标定**：PR181 budget=768K，四小分量 est 21–29K（占 3–4%）；回放 7→4 组、cross_import=0 保持、cross_reference=2 / cross_same_dir=6 / over_budget=2 不变；数据 `.next/private/m21b/pr181-packonly-pooling.txt`。
- **代价（接受）**：不相关文件同格（小分量场景上下文增益本就低）；池格按 10 文件帽切缝新增弱边跨组（进 cross_same_dir 口径）。

## D15 scanner 与深挖时间重叠：在线预算、合并残差、失败口径、FIFO 让位、resume 离线续跑（grill，2026-09-15，M21b 批 C2）

**议题**：批 C / C1 仍是 scan 全完 → 深挖（墙钟 = 两段之和），design §2 是 `max(最慢 scanner cell, 队列排空)`。时间重叠改变的是语义：
预算要在项持续到达时在线算、保底怎么预留与释放、逐 cell 到达后合并不再是组内闭包、reviewer 已开挖后 scanner 失败怎么收、共享门时谁先入门、
resume 怎么接。开工前 `/grilling` 十问（Trellis `09-15-m21b-deep-streaming`，技术设计见其 `design.md`）。

**事实核定（grill 中查证）**
- 现状：`run_code_review` 等 scan 全返回；任一 scanner 失败即返回 scan-only（exit 3）；`run_deep` 在派发前 `enqueue_h0` → `enqueue_scan`
  （按组 `merge_suspicions` 闭包、代表取最高 confidence）→ 一次性 `drain`；生产 `incoming: None`。
- C1 现配置基线（2026-09-15，同批 C / C1 目标 diff，glm 单成员 · agc 门 6）：总 20.6 min（scanner 段 6.2 / 深挖段 14.3），门 6 仍 429 × 2 → 4、
  零断流；scanner 格完成 58 / 197 / 255 / 374 s。
- 策略重放（同选中项、实测格时长，只改可派时刻；模拟器 C1 形态门 6 / 门 4 = 17.7 / 21.6 min，实测 20.6）：等收场 15.8 / 18.3；纯贪心 13.7 / 16.8；
  贪心 + 收场预留 13.7 / 16.8；贪心 + 组完成才入队 14.4 / 17.6。
- 同轮 deepseek 两格（58 / 197 s）恰 10 条疑点 = K − 2 个 H0：宽门下纯贪心会在 qwen 返回前把非必挖预算全给 deepseek——偏向是系统性的
  （偏快家族、偏小组），不只是噪声。
- 合并残差实测（按 scanner 格完成序重放，队列侧逐条合并 vs 组内闭包）：基线 33 疑点 / 2 多家族簇，拆 0；PR181 双家族 r1 111 / 19，拆 1（少算共识 1 条）；
  r2 107 / 19，拆 0；必挖项数三份均不变。`merge_notes` 丢弃相似笔记，链式先后到达也形不成闭包。
- design §8.4「scanner cell 失败 → verdict 硬 Incomplete」；ship.md「部分报告 `verdict: incomplete` → 用部分 findings 照常裁决、不算 approve」。
- resume 只在「scan 补齐 ∧ 有 reviewer ∧ 队列快照非空」时续挖，隐含前提「快照只在 scan 全完、drain 后才写」——C2 下失效；且现存缺口：扫描期被 kill
  （快照为空）的作业 resume 出 scan-only、退出码 0。
- 共门插队：放门 `notify_one` 唤醒的 scanner 尚未被 poll，深挖主循环同步 `try_lease` 抢走门位；主循环唤醒源只有「格完成 / 通道到达」
  （C1 评审 #6 探针 300 ms vs 及时补位 210 ms）。现配置 scanner 与 reviewer 不同门；批 D verifier 与 reviewer 同在 agc。
- 同步 IO：读守卫 `git ls-files` 本仓中位 9.4 ms / 次；`region::build` 每项同步走仓搜索；`on_arrival` 每次到达全量写快照（到达 × 项数次 SQLite 写）。
- scanner needs 从未入队（批 C plan C-3 原文有、followup 无账）；`NeedEntry {path, lines?, reason, kind}` 不指向触发疑点、path 可在 diff 外。

**候选与被否**
| # | 定案 | 被否 | 依据 |
|---|---|---|---|
| Q1 在线预算 | B 贪心 + 收场预留：scanner 未全返回时非必挖项仅在「本轮已派发 < K − F」时可派（F = ⌈K/4⌉），必挖不受限；关闭入口时对剩余预算按 C1 drain 一次选定（保底扣除扫描期已派发的非必挖疑点），Skipped 只在此刻判定 | A 纯贪心（只受 K）；C 等收场（扫描期只派必挖） | 现配置 B 与 A 同速（门位腾出时 scanner 已返回，预留零代价）；宽门下 A 把「凭先到部分信息选项」的系统性偏向放大到整个非必挖预算，B 封顶 K − F；C 放弃扫描期门位（重放 15.8 vs 13.7 min）；复用 F 常量、零旋钮；Skipped 只在收场判定，C1 评审 #5 假债（复位不回减）无从发生 |
| Q2 到达与合并 | M1 逐 cell 到达、队列逐条合并（design 规则 5 字面）；收场以报告侧闭包对账，拆簇数入 RunMeta；`merge_into` 前提注释改写 | M2 到达时桥接合并（在线并查集）；M3 组内全返回才入队 | 残差实测 ≤ 1/19 簇且必挖项数不变；M2 要两项合一退役 id、改快照、处理在途 / 结案组合，复杂度远超残差；M3 墙钟 13.7 → 14.4 min，且即 D13 Q4 已否的同组 hold（M21e 凭数据定，拆簇数正是那份数据） |
| Q3 方差四层 | S3 整体另立批 C3（深挖选项稳定性），C2 合回后、出口验收前；C2 不改选项规则 | S1 全并入；S2 只并规则 2 相邻区间合并 + 同分 tie-break；S4 推迟 M21c / M21e | B + M1 已让选中集合随到达时序变，再叠选项规则改动则 C2 dogfood 对照 C1 基线无法归因（铁律 1）；方差验收要同 diff 两跑量重合度，与 C2 墙钟验收不同；规则 2 合并的共识口径、def 扩展、id 形态与 M1 交织，排序键要改 design 规则 1，需自己的拷问 |
| Q4 scanner needs 入队 | N2 归 C3；缺口记 followup | N1 放 C2；N3 归 M21d；N4 从 design 删除 | 改变候选集，归因理由同 S3；needs 项语义未定义（无触发疑点链接、path 可在 diff 外、装包退 `bundles[0]`），需先设计；`Api` 类若占多数可再拆给 M21d |
| Q5 scanner 失败 | F3 深挖照跑；verdict 硬 Incomplete / 3（优先于 reject 10 与带债 2）；findings 照常进报告与统计，warning 报 blocking 条数 | F1 立即停、丢深挖成果；F2 reject 优先于 incomplete；F4 停派新项、保留已挖 | §8.4 字面、五档退出码契约不变；与 ship 部分报告先例同口径；scanner 失败多为 provider 故障、resume 未必即时可成，照跑让用户当下拿到能挖的 findings，resume 沿用不浪费 |
| Q6 resume | RS1 离线续跑：补跑缺失 scanner → 全部成功 scanner 记录的疑点幂等并入快照队列 → 补跑过 scanner 则复位 Skipped 按剩余预算重选，否则沿用 → 深挖；空快照 + reviewer 同路径（H0 按当前 diff 重算），修现存「扫描期 kill → scan-only exit 0」 | RS2 流式续跑（复用扫描期逻辑）；RS3 只续快照 | 恢复路径求正确不求快，只复用幂等入队与 C1 drain；RS2 要从快照恢复已派发数、是否收场等状态，边界多；RS3 下补跑出的疑点永不深挖而 verdict 翻成完整 = 假绿灯 |
| Q7 共门先后 | G1 §12.3 字面 FIFO：门上有等门者时非阻塞试取让位；有可派项无门位时深挖自身排入 FIFO（兼修 #6）；t=0 poll 序 scanner 先；机制通用于 verifier | G2 角色优先级（scanner > verifier > reviewer）；G3 只修唤醒不管公平 | G2 即调度器，违 §12.3 / D13 Q2；G3 过不了出口验收 #5（入队序 scanner 先可观测）；公平与唤醒同一机制——排进 FIFO 既不插队又能被放门唤醒 |
| Q8 报告组装序 | D2 收场按队列项最终优先级（`cmp_priority`）组装 findings / cells / meta 四数组；保证改为「同选中集合 + 同格输出 → 逐字节一致（计时字段除外）」 | D1 维持派发序 | 派发序在 C2 随到达 / 放门时刻变，报告文本逐次漂移；优先级序保 C1 本意（高优先项作 dedup 基准），退化零差基线不变；选中集合本身的漂移照实写进保证条件（归 C3） |
| Q9 同步 IO | IO2 到达只写受影响快照项 + git 跟踪文件集每轮预取；填门循环单次最长 / 累计阻塞入 RunMeta；`spawn_blocking` 待数据（followup 挂触发条件） | IO1 全修（含 `region::build` / `need_section` 挪 `spawn_blocking`）；IO3 全留债只观测 | 快照写是 C2 到达路径新增的平方级放大、改法小；预取省每项一次子进程；挪 `spawn_blocking` 要把 bundle、跨组索引、装包参数改共享所有权，而大仓走仓的真实代价未量——先量再动 |
| Q10 验收 | Fake + 虚拟时间覆盖 Q1-Q9 各条；RunMeta 新观测（收场 / 首次派发 / 排空时刻、复核尾部、扫描期派发数、收场剩余预算、拆簇数、填门阻塞）；dogfood 一轮同目标 diff、配置与基线一致；`phase` 收场前 Scan、后 Deep | — | 不硬卡单点门槛（同 M21a 出口裁决口径），对照基线 20.6 min 与重放预测 13.7 / 16.8 min 看分段 |

**依据（用户判断，2026-09-15）**：十问均按推荐定；另定 agc `maxConcurrency` 保持 6（门 6 撞 429 由自动收门兜底、首波排队可接受）；
批序 C2 → C3 → D → 出口验收（D 原与 C1 并行、未开工，改排 C3 之后）。

**代价与残余**
- 扫描期选项仍凭先到信息（封顶 K − F），选中集合随到达时序变——稳定性归 C3，C2 dogfood 的 findings 仍会不稳。
- M1 非传递漏合并（实测 ≤ 1/19 簇）；拆簇数攒给 M21e 的同组 hold 决策。
- scanner 失败时退出码看不出 reject，须读报告（warning 报 blocking 条数）。
- `region::build` 走仓搜索仍在单 task 同步段，大仓代价待 PR181 出口验收数据。
- design v11：§6 规则 2 / 3 / 6 与可观测面、§8.4、§12.3、§17 R14 对应改；plan 批 C2 节改写并增批 C3；roadmap M21b / README、followup 同步。

---

## D16 深挖选项稳定性：结构定项、共识两轴、分层排序与文件多样性、needs 入队（grill，2026-09-20，M21b 批 C3）

**议题**：同一份 diff 两跑，深挖挖的应当基本是同一批区域；实测不是——选中项逐项重合度 25%、findings（`file:line`）19%，
而单文件能独占近半预算。C2 把墙钟砍掉一半（20.6 → 10.7 min）之后，方差成了下一个拦路的东西：不可复现 = 不可回归、不可归因，
与 §9 确定性地板冲突。开工前 `/grilling` 十二问（Trellis `09-20-m21b-deep-stability`，技术设计见其 `design.md`）。

**事实核定（grill 中查证）**
- 改前基线对（同二进制 `fffd595`、同 diff `674583e...f1d3ca3`、门 6、samples 1，2026-09-20 两轮）：候选集（scanner 疑点位置）32%、
  **选中项逐项 25%**、按文件 57%、findings 19%；单文件最大占格 6/13、6/12；格内 findings 11 / 15，代码格占比 7-of-13、10-of-12。
- findings 按 id 的跨轮重合度恒 0%——id 哈希含 title，措辞一变即不同 id。跨轮只能按 `file:line` 比。
- 排序现状：`priority_score = max(confidence) × agreement × 类别权重`，而 `CATEGORY_WEIGHT = [1.0; 6]` 是空操作、`agreement` 只取 1/2
  → 实际等价于「按 confidence 排」。而 confidence 跨轮整体平移（两轮 min/中位/max = 0.15/0.35/0.55 与 0.20/0.45/0.82），E3 早证实
  flash 级自评与命中无关；经预算截断，它等于被当成门用。
- §6 规则 2 的「区间扩到 enclosing def、同文件重叠 / 相邻合并」从未实现，`same_item` 只做了 quote 锚那一半。零件现成：
  `risk/regions.rs` 的 `enclosing_decl` / `decl_block_end`（H0 成区就是「hunk → enclosing def 块，无 def → ±10 行」）。
- 按**行跨度**合并会把身份搞更不稳：C1 离线模拟「相邻 W 行合并 + 代码优先 + 按文件轮转」把重合度从 25% 压到 21% / 15% / 13%（W = 0 / 10 / 30）；
  按文件粒度算出的 57% 则是「合并到极致」的上界。两数一起说明：合并有效，但身份必须锚在结构上。
- 区域级共识会撑爆必挖集：现状每轮 `agreement ≥ 2` 的疑点只 4 条；按「区域里出现过的家族并集」算，两轮分别 7/9、7/10 个文件 ≥ 2 家族，
  PR181 规模 18/23。必挖不受截断 → `maxCells` 形同虚设。
- `samples ≥ 2` 不抬共识（followup 旧括注有误）：`agreement` = 去重家族数，D7 明确「同家族多 samples 计一次」；samples 只扩候选覆盖，
  成本是 scanner token 翻倍（现占全轮 48% in / 40% out），墙钟基本不变。配置无 temperature 旋钮。
- needs 是最稳的信号：两轮唯一路径 7 vs 8、Jaccard 67%，全部 `kind = evidence` 且全部带 lines；但约六成指向变更集之外，
  而不在变更集的文件装不出证据：`region::build` 在 bundle 里找不到该文件时只输出一行「队列项文件不在变更集或工作树缺失」的省略说明，
  整格就是白烧（**2026-09-21 评审更正**：原记「`pick_bundle` 退回 `bundles[0]` 会产出文件名 A、证据 B 的错配包」不成立——
  `bundles[0]` 只决定跨文件上下文取哪一组，该文件自身的切片根本不会渲染）。
- 类别标注本身在漂：L0 占比两轮 59% vs 21%。

**候选与被否**
| # | 定案 | 被否 | 依据 |
|---|---|---|---|
| Q1 目标函数 | A 主目标稳定性，产出只设护栏（格内 findings ≥ 11 且代码格占比 ≥ 68%，不要求变多） | B 主目标 findings 数；C 两者并重 | 单轮 findings 实测在 6 / 9 / 11 / 12 之间跳，噪声大于信号、当不了主指标；「这轮没挖到的下轮也未必挖」= 覆盖靠运气 |
| Q2 候选集是否进范围 | C 不进；另跑一次性 `samples = 2` 测量轮，数据进 followup 供后批决策 | A 产品化 samples ≥ 2；B 降温 | 候选集 32% 是方差上游，但源头手段成本高（token 翻倍）、无温度旋钮；先把「分配」层治干净，再拿数据谈源头 |
| Q3 项身份 | B enclosing def（文档退标题节、其余退固定行带），复用 `risk/regions.rs` 零件、逻辑不动 | A 报告行区间；C 行跨度聚类 | 结构跨轮恒定，报告行每轮都动；C1 模拟已证跨度合并越合越不稳 |
| Q4 合并后共识 | C 两轴分离：`peak_agreement`（项内最大簇家族数）定必挖、区域级 `agreement` 进排序 | A 全用区域级；B 全用项内 | A 撑爆必挖集（7–9 块 / 轮）；B 丢掉「这块被几个家族碰过」这个真实的排序信号 |
| Q5 排序键 | 分层键（保底 → 种类 → peak → 风险档 → 烟量 → 代码优先 → confidence 末位 → id），类别权重维持 1.0 不启用 | 继续乘积式；启用类别权重 | 乘积式已退化成 confidence 排序；分层后每维含义独立可查。风险档锚在既有 θ = 1.5 上，不另立阈值 |
| Q6 文件多样性 | D 每文件保底一格 + `max_cells_per_file` 可观测 | A 不管；B per-file 硬帽；C 按文件轮转 | 保底只让出第一轮、热点文件的第 2/3 项仍按价值排；硬帽与轮转都会在「问题真的都在一个文件」时伤覆盖 |
| Q7 笔记 confidence | A 去掉数字，基线重采 | 保留；改成分档词 | §7 明写笔记注入是防锚定设计，摆个 0.82 与此自相矛盾；E3 实证自评与命中无关 |
| Q8 scanner needs | B 只接变更集内；diff 外只计数，装包归属记账 C4 / M21d | A 全接；C 继续不接 | needs 路径 Jaccard 67% 是全轮最稳信号，值得接；但 diff 外的文件装不出证据（`region::build` 只出一行省略说明），入队即白烧一格——见「代价与残余」的更正 |
| Q9 验收门槛 | B 三量：逐项重合度 ≥ 40% + 单文件最大占格 ≤ 总格数 1/3 + 护栏；落 30%–50% 边界带加跑第三轮取中位数 | A 只看重合度；C 加权综合分 | 单指标会被候选集波动带偏；辅助门槛直接对应多样性机制且不受候选集影响；边界带避免一轮定生死 |
| Q10 合并谓词 | B 结构定项、谓词定共识；取消 id 捷径 | A 保留 C1 的「共识只经谓词、id 捷径不计共识」 | 块键已覆盖「同区间」的情形；共识改由项内聚簇算，`by_predicate` 这条特例失去存在理由 |
| Q11 观测 | B 五个标量（`suspicions_arrived` / `items_from_suspicions` / `files_candidate` / `files_selected` / `max_cells_per_file`）+ `enqueue_reasons` 增 `needs` / `needs_out_of_diff` | A 不加；C 加逐格数组 | 三量判定要能直接读数；逐格数组可由 `DeepRun.items` + scan 报告离线复算，不占 schema |
| Q12 拆分与边界 | C3-1 结构化成区与合并 → C3-2 排序与多样性 → C3-3 needs 与观测 → C3-4 收口；六条明确不做 | 一次性改完 | 三处语义（谓词 / 必挖轴 / 排序键）同时动，先红后绿逐条钉才看得出是哪条在起作用 |

**实施细化（design 未及，实现时定）**
- markdown 标题节按「下一个标题」切，不按「同级或更高级」：后者让父节前言的区间包住子节，两项重叠、同一段挖两遍。
- 退路块按固定行带量化（带宽 21）而非以报告行为心的 ±10：键若跟着行走，同段差一行的两条疑点仍各成一项，等于没治。
- `QueueItem` 增 `member_families`（与 `members` 同下标）：`Suspicion` 不带家族、项级 `families` 是并集还原不到条，聚簇算不出家族数。
- needs 的 `lines` 缺失时取文件起头（由块计算兜住），不用「整文件区间」：后者会把 `u32::MAX` 带进 `union_ranges` 与 pack 的切片算术。
- id 捷径只保留给「任一侧无块信息」的旧路径：有块时同文件同区间必同键，捷径复活只会把两个不同结构的项错并。

**代价与残余**
- 单项变大（reviewer 拿到整个 def 而非三行）：护栏盯 findings 数；实测 findings 反升（17 / 21 / 16，门槛 11、改前 11 / 15），未触发「缩块」这条处置。
  代价落在墙钟：reviewer 输出 token 146k → 230k / 288k / 243k，收场后尾段 3.3 / 6.0 → 6.0 / 18.4 / 11.2 min。
- **代码格占比护栏未达，2026-09-20 定案「认这个交换」（Q1 护栏的口径问题，非实现问题）**：实测 58% / 67% / 75%（均值 66.7%）低于门槛 68%，
  但它要保护的量反向而行——代码格产出 9 / 12 → **11 / 11 / 11**、报告口径代码 findings 7 / 9 → 10 / 9 / 10、代码格数 7 / 10 → 7 / 8 / 9；
  占比下降只因文档格自己变高产（0.63 → 1.75 条/格，产出最高的两格是 needs 带进来的文档项）。
  被否的两条处置：① needs 只接代码文件（砍掉整批产出最高的两格，与 Q8 依据打架）；② 文档项在分层键里再降一档
  （文档洪水在候选集里——三轮文档位置占 57% / 20% / 72%，samples 2 轮 62%，属 Q2 划出的范围；降档会压掉正在落地的 21 条文档 finding）。
  **阈值不回改**：本批按「未达」记账；下批起护栏换绝对量（代码格 findings ≥ 9 且代码格 ≥ 7），且阈值在跑数之前定。
- 候选集本身只重合 32%，排序改动的上限受此约束——源头手段（samples / 降温）仍欠账，测量轮数据进 followup。
- 变更集外 needs 的装包归属与读守卫口径 → C4 / M21d。
- 类别权重仍空置（标注自身在漂），启用与否攒给 M21e。
- design v11：§6 规则 1 / 2 / 3 与可观测面、§7 对应改；plan 批 C3 节与 roadmap / followup 同步。
