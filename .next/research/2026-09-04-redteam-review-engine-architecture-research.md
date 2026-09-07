# M21 评审引擎架构·证据汇编（web 深研 + 参考工具抽取 + 已有实验 + 串行三角色否决账）

> 状态: WIP · 2026-09-04（v2：补入 4 个参考仓库子代理抽取 §10；新增串行三角色墙钟否决账 §11；§12 映射改指风险路由 DAG）
> · 2026-09-05 v3：§12 映射对齐 design v3 节号（队列 §6 / 切片 §10 / 三方契约 §9.4）；推理归位 decisions D1-D5。
> · 2026-09-05 v4：新增 §13 rpiv 交叉评审补充证据；§12 映射对齐 design v4（疑点契约 §4 / 预算与台账 §6, §8 / 角色层 §12 /
> 跨组索引 §10.6）〔D6〕；§11 加 D1 勘误注。
> · 2026-09-05 v5：新增 §14 aacr-bench 事实与配置生态事实（grill〔D7〕，供 M21c / M21e 重启基准时直接用）。
> · 2026-09-07 v6：P0 E1 / E3 数据另立 `../private/research/2026-09-07-redteam-p0-validation-gate-research.md`（私有区，含 PR181
>   细节；节号沿用 §15 / §16），本文 §15 / §16 只留指针；纯数字摘要见 `../roadmap/P0-validation-gate.md`。
> 喂 `../design/2026-09-04-redteam-review-engine-architecture-design.md`（架构定论 v2）。
> 方法: 9 角度 WebSearch（单模型召回天花板 / 多模型互补 / 两遍法快慢分层 / 前缀缓存 / 跨文件图 /
> 假阳压制 / 增量提交 vs 大 JSON / 融合角色 / 工具隔离）+ 4 参考仓库子代理抽取（open-code-review 本地源码 /
> pr-agent / reviewdog+tree-sitter / code-review-graph+awesome-code-review）+ 本仓已有第一手实验
> （s2a / s2b / no-tools-default / dossier-recall / pr181 三方对照）交叉核验。
> 曾名 `2026-09-04-redteam-tri-agent-architecture-research.md`（同日改名：三角色串行被否，主题升为评审引擎架构）。

---

## 0. 一句话结论

单 LLM 评审存在**召回天花板（~61%）**；解药是 **① 跨家族多样性** + **② 确定性地板**，不是更贵的模型。
**但「扫描→深挖→验证」三角色串行对「快」是负账**（§11）：角色分层 ≠ 分治，串行调用次数相加、只有并行才摊平墙钟。
正确容器 = **快模型默认全景（分组并行）+ 确定性风险路由触发慢模型定向深挖 + 跨家族并行 + 定向 verify**。

---

## 1. 单模型召回天花板（最关键实证）

**1.1 《A Single LLM Is an Incomplete Code Reviewer》（Zenodo v2, 2026-07-13）**
- 语料：33 个代码/混合制品；294 个确认缺陷（人工校准答案键）；15 个模型版本 × 8 家 provider。
- **单个模型召回上限 ~61%**，典型模型仅抓约一半。
- **56.8%（167/294）确认缺陷恰好只有一个模型找到**；跨家族重叠中位 Jaccard ~0.29。
- 覆盖率（artifact 内）：1 评审 47% → 加 1 个（跨家族）72%（**最大单步增益**）→ 递减。
- 作者明确：支持「加第二个独立评审」，**非**「加某个特定 provider」；不建议超 2-3 个继续堆。
- 作者自评：结论方向性，非通用 benchmark（单机构研究，分母偏宽）。

**1.2 《Wisdom and Delusion of LLM Ensembles》（EASE 2026, arXiv 2510.21513）**
- 10 个 LLM × 5 家族：ensemble 理论上限可比最强单模型**高 83%**，小模型也能产出最大模型绝无可能产生的解法。
- **流行陷阱**：共识选择策略会放大「共同但错误」的输出，把多样性退化回冗余。
- **多样性策略**：兑现 ensemble 上限的 95%，且**两模型小集合即成本有效**。
- 机制：LLM 有「自我增强偏差」，倾向接受与自己推理模式相似的（错误）结论；跨家族验证者缓解。

**1.3 方向不对称**
- 《Cross-Model LLM Code Review》（arXiv 2607.21656）：Claude 审 Codex 草稿 pass 71.6%→89.7%；
  Codex 审 Claude 草稿 pass **跌** 91.4%→82.8%。「哪两个模型、谁审谁」是真实决策。

**1.4 工业背书**
- GitHub Copilot CLI "Rubber Duck"：Claude + GPT-5.4 跨家族互审，闭合 Sonnet↔Opus 差距 74.7%（SWE-Bench Pro）。
- 本仓 PR181 三方对照：redteam 独有「配置开关启用即回归」、rpiv 独有「分类器 fork 腐蚀 operability」、
  Kimi 独有「callback 队列退化上限失效」——**多模型/多工具互补是真实收益，非冗余**。

---

## 2. 两遍法 / 快慢分层

**2.1 同模型去相关采样的召回增益（关键反直觉）**
- 《Code Review With a 12B Model》：单遍 12B（gemma）真实 bug 召回 ~0.15；**同一个** 12B 模型跑成去相关
  DAG（多 lens / 重采样 / heat up / 并集）→ **~0.71（4.7×，纯采样预算，非模型升级）**。小模型每遍约便宜 8 倍。
- **能力墙真实**：authorization/scoping、跨函数不变量、data-flow 类缺陷任何采样都难找（33 个真实 bug 中
  3 个任何遍都找不到、13 个 <5% 遍找到）。→ 靠模型选择解决不了，需跨文件静态分析或真实测试。

**2.2 强弱模型分工（koda asymmetric model collaboration, Issue #256）**
- Gemini Flash Lite 当「初级工程师」只**提问**（高量低质）→ Claude Opus 当「高级架构师」只**回答/判断**。
  结论：两次遍就够；弱遍 33% 噪音率可容忍（强模型天然忽略琐碎）。

**2.3 精度的作用（B 面）**
- 《GenAI Code Review Tools: 2025 Field Test》：240 植入缺陷；CodeRabbit 召回 64%、Claude 4.5 基线 61%（最贵）、
  Copilot 54%、Qodo 49%、CodeGuru 41%；平均幻觉 18%。
- 召回按类别剧烈分化：SQL/命令注入 >80%，**授权缺陷 <30%**，业务逻辑 bug ~21%。
- **约 2K token 的项目意图 brief 翻倍逻辑 bug 召回（24%→51%）**——上下文杠杆 > 模型杠杆。
- 《Agentic code review in production》：假阳而非裸能力决定系统存活——20 评论/PR 时 5% 假阳率 = 每 PR 一个
  假旗，几周内团队停止读 bot。**评审是「打断通道」，精度常比召回更重要**。

**2.4 AACR-Bench（context 的隧道效应）**
- agent 模式 precision 0.08→~0.15，但 **recall 掉到 ~8.7%**（聚焦检索的 contextual tunnel vision）。
  **naive RAG 有时反伤害强推理模型**。

---

## 3. 前缀缓存 / 上下文构建（input 侧经济）

**3.1 机制**：prompt cache 依赖**精确前缀匹配**；前缀内第一个变动 token 之后全重算。volatile 值（timestamp/UUID）
插早 → 静默废掉后面全部缓存。**反面实例：open-code-review 主模板第一个占位符即 `{{current_system_date_time}}`**（§10.1）。

**3.2 工具信号**：prefixlint（静态分析 cache-hostile 模式）；AgentCheaper（动态 harness state 后移 → 命中率
91.66%→97.67%，paid uncached 30,082→7,817）；Claude Code 顺序 system → project context → conversation。

**3.3 成本**：cached 输入约 fresh 的 10%；input:output ~36:1 时 input 价主导。DeepSeek 85-99%+ 命中 ≈ 50× 成本降。

**3.4 本仓实证**：PR181 Kimi 166.5K input / **99% cache 命中**。

**3.5 不变量**：PinnedPrefix / AppendLog / TurnScratch 三区；前缀哈希检测漂移。

---

## 4. 跨文件盲区 / 图 / blast-radius

**4.1 问题**：reviewer 的 context window 是 diff，但 blast radius 在 diff 外。

**4.2 爆炸半径裁剪（Baz 研究，30 真实 PR）**：tree-sitter 建依赖图 → scope 到 Hop 0-2 →
**输入文件减中位 ~87-90%、prompt tokens ~86%、成本 7×、时间 74s→16s，且 bug 检测未降（81.5% vs 77.8%）、
假阳减半（14→6）**。经验债：①别含库边（1 文件→812 文件）；②Hop 距离仓库相关；③**scope 在 prompt 进模型前做**。

**4.3 本仓 SCIP 实证（诚实数据）**：
- `s2b-static-validation-research.md`：修后 SCIP 引用 ≈ regex。**SCIP 不是批量 recall 赢家**；真正价值在
  **callee 签名注入**（单 fn 13 条、变更集 77 条，regex 做不到）+ 契约分类器。
- `dossier-recall-eval.md`：穷人版（词边界 grep + 缩进/括号）Rust 全场景 **recall = 1.000**（431/431）；
  查准才是问题。tree-sitter 不立项。

**4.4 two-pass 交叉上下文**：Pass 1 单调用产 PR 级总结、Pass 2 注入文件级评审。
**v2 定论：不做 per-PR 摘要轮**（§11 否决账：固定必走轮=净增墙钟）；改为**仓库级简介入稳定前缀**（跨 PR
稳定、cache 近免费），吃到 §2.3 那 2K brief 翻倍逻辑 bug 召回的红利。

---

## 5. 假阳压制 / 历史记忆

**5.1 核心问题**：LLM 评审把每个 PR 当全新，反复报已驳回/已修复的发现（agentic-code-reviewer issue：16 轮中
12-16 轮重复报 4 个已驳回 finding）。

**5.2 机制**：结构化历史记忆（finding→reason→status 喂 FP filter，纯散文总结丢特异性）；规则提取自
accept/dismiss（Cursor Bugbot resolution 52%→~78-80%）；Arnica 90 天窗 + 70% 置信门；diffray **应用前先对历史
PR 验证规则**（40-60% → 8-13% FP）；cubic 51% 降假阳无 recall 损失（架构而非 prompt）；Entelligence 对抗环证伪。

**5.3 告诫**：规则编码盲点；抑制规则衰减需 TTL；「丢弃」语义歧义需分类。

---

## 6. 增量提交 vs 单大 JSON（output 侧）

**6.1 本仓实证**：8192 时代完整重演「max_tokens 截断 → JSON 半截 → parse fail → repair 烧双倍 → 烧 cell」；
PR181 两次撞顶。单轮超时公式：`≥ max_tokens ÷ 模型最低 TPS + prefill 余量`。

**6.2 两条路**：抬上限（Decepticon 93e5b32：LiteLLM 默认 4096 截断 tool call，修法=显式 16384）或结构改增量提交
（ocr `code_comment`，§10.1）。

**6.3 v2 定论**：**按分组/热区把每 call 输出天然有界**，单发 30K 撞顶在结构上消失，**无需另做增量提交协议**。
max_tokens 显式 + 流式照旧。

---

## 7. 融合层角色

**7.1 外部形态**：arch-review-assistant Synthesizer（检测重叠、severity-ranked、Approve/Reject 回流 prompt）；
adr-reviewer synthesis（汇总 + fact-check）；Zylos multi-model convergence（并行 + 投票 + validator）。

**7.2 v2 定论**：融合层 = **确定性 reduce（dedup/排序，代码）+ 定向 verify（只对孤证 blocking）**，
**不设独立 advisor agent、不设固定必走轮**（§11）。verify 持独立正交上下文（finding + ±60 行快照），不重建 bundle。

---

## 8. 工具设计 / 隔离

**8.1 工具 = 上下文的替代品**（本仓最强第一手证据，`no-tools-default-design.md` F1）：全景 pack + 无工具 =
质量未降 + 轮次 39→5 + input −91%；全景 pack + 工具 = 最贵。F4：全文在手仍用工具，红利只能靠砍工具兑现。

**8.2 分层授予**：reviewer read-only（Read/Grep/Glob，拒 Write/Edit；colmena：不能"好心"修复弄脏 diff）；
隔离结构性非概率性。加工具放大失败暴露面：ocr PR181 35 请求撞 429/503，多轮在慢思考模型上是**乘法非加法**。

---

## 9. 编排模式

**9.1 共识**：确定性代码做脚手架（dedup/sort/route/loop），LLM 只在真判断处调用；节点做一事；边带结构化证据；
按风险路由（router 读 severity/confidence）；收敛环；模型匹配节点。

**9.2 形状**：diamond（fan-out → barrier → reduce → route → verify → synthesize）。ADK-Rust deferred fan-in；
petrarca gate（Continue/Stop/Restart/GoTo + max_cycles）；Jido FanOutNode + backpressure。

**9.3 反面**：《The Agent DAG is Dead》——拓扑代替 agent 智能。折中：DAG 对「已知模式 + 确定性分治」对。
**本管线属此类；但勿让拓扑承担本属 agent 的判断，勿让固定阶段变成必走轮**（§11）。v2 形状见 design §2。

---

## 10. 参考工具设计抽取（4 子代理，2026-09-04）

### 10.1 open-code-review（本地源码 `~/codes/open-code-review`）

- **分组**：`GROUPING_MIN_FILES=4`、`GROUPING_BUNDLE_LINE_THRESHOLD=200`、`MAX_REVIEW_ROUNDS=2`、
  `PLAN_MODE_LINE_THRESHOLD=50`（`internal/config/template/task_template.json:38-42`）。`GroupingPlan`
  （`template.go:129-137`）：文件数 < MIN → 本地不调 LLM；churn < BUNDLE_LINE → `bundle_all`，否则 `per_file`；
  **LLM 分组只喂路径+churn 元数据、不含 diff**（`grouping.go:63-103`），失败回退 per-file；`maxFilesPerGroup=10`；
  `enforceGroupTokenBudget` 超限降 per-file。
- **增量提交**：`code_comment` 工具，参数 `comments[]{content, existing_code, suggestion_code, thinking, category,
  severity, path}`（`internal/model/review.go:7-21`）；**`existing_code` 为锚点，工具用滑动窗口在 diff 里反解行号**
  （模型不报行号）；线程安全 `CommentCollector` 累加；`MAX_COMPLETION_TOKENS=16384` 固定。
- **工具循环**：`MAX_TOOL_REQUEST_TIMES=100`（review）/ 60（scan）；工具 `file_read/file_find/file_read_diff/
  code_search/code_comment/task_done`；预算尽 → **grace round**（`loop.go:479-520`）：工具裁到
  `code_comment+task_done`，一次 final call「补报未上报 findings」；硬停：3 轮空结果 / 压缩超阈。
- **rules**：custom(`--rule`) > project(`.opencodereview/rule.json`) > global > embedded；doublestar glob
  **首匹配胜**；注入 `{{system_rule}}`；另有 `ReviewFilterTask` 二轮 LLM 假阳过滤（`--no-filter` 关）；
  `CanonicalConfig` SHA256 保可复现。
- **上下文顺序**（`agent.go:1269-1290`）：`{{current_system_date_time}} {{system_rule}} {{change_files}} {{diffs}}
  {{requirement_background}} {{plan_guidance}} {{confirmed_comments}}`；diff 包 `<file path=…>` XML、按路径稳定排序；
  session 级 cache affinity key；压缩三段 frozen/compress/active。**反面教材：时间戳占位符排模板最前**，volatile
  值置顶废掉整段前缀缓存（§3.1）。
- **模型分层**：**无快/慢双模型**；深度靠 Plan Phase 门控（单文件 ≥50 行或组合 ≥100 行才触发 plan）；scan 走
  enumerate → PLAN → main loop → DEDUP → PROJECT_SUMMARY，`BATCH_STRATEGY=by-language`、`BATCH_SIZE=50`。
- **启示**：双阈值分组哲学（文件数问「值不值得调 LLM」、行数问「能否共享一次注意力」）直接移植为分组并行的门；
  `existing_code` 锚点反解行号与本仓 positioning 同源；grace round 是有界循环的好收尾（仅诊断档参考）；
  PR181 实测其多轮路线 35 请求撞 429/503、anthropic/responses client 非流式撞 524——**载体不取 agent 循环**。

### 10.2 PR-Agent（qodo-ai/pr-agent）

- **一命令一提示词**：`/review /describe /improve /ask /custom /update_changelog`，各对应 `pr_<tool>_prompts.toml`；
  **共享同一 model 配置**，非一命令一模型。
- **模板**：`system`+`user` 两键；固定段（角色 / 只审 `+` 新代码 / `__new hunk__` 格式 / 反幻觉「明确才报」/
  Pydantic schema）+ Jinja 条件段 `require_security_review / require_can_be_split_review / require_todo_scan /
  require_risk_assessment / require_merge_recommendation / require_score / require_tests`；注入点
  `extra_instructions / repo_context(AGENTS.md) / skills_context(SKILL.md)`。
- **diff-only**：不给全文，并明示模型「看不见全库」抑幻觉；超长：token 预算截断进 `remaining_files_list`
  （`MAX_REVIEW_COVERAGE_FILES=50`）；`enable_large_pr_chunking` → `asyncio.gather` 并行分块 → `merge_review_chunks`。
- **无工具、单次 `chat_completion`**；上下文预组装（AGENTS.md / SKILL.md / ticket 抽取）。
- **模型配置**：`model / model_weak / model_reasoning / fallback_models / reasoning_effort`——「任务难度→模型档位」映射。
- **评测**：ReviewBench 结构化提示单独把某模型 F1 0.13→0.32（**提示架构本身是性能面**）；c-CRAB 23.1%、
  2.8 评论/PR、人工抽查 94% 有用；Qodo 自报 64.3% F1 但标准配置 47.9%。
- **启示**：`model_weak/model_reasoning` 是本设计「cell 类型→模型档」路由的直接先例；固定段+条件段+注入位的
  模板结构值得抄；「单发无工具 + 预组装上下文」与本仓 no-tools-default 同路；分块并行+合并是标配。

### 10.3 reviewdog + tree-sitter

- **reviewdog**：Parse（errorformat `%f/%l/%c/%m`、RDJSON/RDJSONL、SARIF 2.1、checkstyle）→ **Filter**
  （`added` 默认只报新增/修改行；`diff_context` / `file` / `nofilter`）→ Report（`github-pr-review /
  github-pr-check / github-annotations / gitlab-mr-discussion / gerrit-change-review / bitbucket-code-report /
  gitea-pr-review / local`）；`-fail-level=[any,info,warning,error]`；`-guess` 从分支猜 PR；无内置 LLM，社区模式
  「LLM 脚本 → rdjsonl → reviewdog」。
- **tree-sitter**：增量解析、具体语法树、字节偏移+行列、错误容错、query 语言可答「谁调用 X」；实践：
  code-review-graph / better-code-review-graph / src-mcp / tree-sitter-mcp / code-context-graph / wdyt。
- **启示**：diff-aware filter（只报改动行）是零成本精度杠杆，可作 reduce 后确定性后滤（本仓 positioning 已锚定
  改动，可选加严）；reviewdog 是裸终端外的 CI 收口面（M20 备选，非本里程碑）；tree-sitter=前端精度层，本仓
  regex 已达 recall 1.0，维持不立项。

### 10.4 code-review-graph + awesome-code-review

- **CRG**：tree-sitter AST 派生图（节点 fn/class/import；边 call/inherit/test-coverage，三级置信
  EXTRACTED/INFERRED/AMBIGUOUS）+ Leiden 社区检测 + SQLite + FTS5 混合检索；影响分析 BFS
  （`CRG_MAX_IMPACT_DEPTH=2`、`MAX_IMPACT_NODES=500`、`MAX_BFS_DEPTH=15`）、执行流追踪；~30 个 MCP 工具
  （`get_minimal_context` ~100 token、`get_review_context`、`get_impact_radius`、`query_graph`…），
  **检索式引导、不灌整图**；自报 token 中位降 65×。**诚实边界：影响分析 F1 0.69、关键词 MRR 0.35、流检测
  召回 33%**——图是降噪器非正确性保证。
- **awesome-code-review**：主体传统人工评审（Gerrit/Phabricator、Fagan/Porter 论文），几乎无 LLM。LLM 谱系补检索：
  规则派（SonarCloud/CodeQL/Semgrep）/ 单次全量派（Copilot/CodeRabbit）/ 多 agent 派（qwen-code `/review`
  5 agent+linter 预扫、diffray 11 agent）/ RAG 派（Greptile/Qodo）/ 图派（CRG/CodeQL）/ 人机派（Reviewable/Cursor）。
- **基准**：LangChain ReviewBench 裸 harness 最强约恢复 30% 基线缺陷、结构化提示 0.13→0.32；Factory Review
  Benchmark GPT-5.2 60.5% / Opus 4.6 59.8% F1；ReviewBenchLite 117 生产缺陷；全行业约 20 点召回带宽内。
- **启示**：图的价值落点 = blast-radius 裁剪上下文 + callee 签名（与 §4.3 一致），不是引用召回；多 agent 派的
  实证卖点是**降噪**非深度——与 §11 否决账互证。

---

## 11. 串行三角色的墙钟否决账（2026-09-04 讨论定）

墙钟 = Σ（串行调用的 prefill + output ÷ TPS）。PR181 规模逐角色：

| 角色 | 墙钟贡献 | 判定 |
|---|---|---|
| Scanner（快模型疑点索引 ~5K @103 tok/s） | +1-2 min | 收益取决于深挖是否因此省时 |
| Reviewer 深挖（top-K 完整论证 ~25K @27 tok/s） | ≈ 10-11 min | **与单发同质**：聚焦省的是「看不该看的」，不是「写论证的 token」；若 top-K 多于单发自发现，输出反涨 |
| Advisor（dedup/排序=代码≈0；refute 孤证 ≈1 min；**PR 级摘要=纯增一轮**） | +1-3 min | 固定必走轮 = 净增 |
| **合计** | **≈ 12-15 min** | 劣于 DeepSeek 单发 5.3 min 两三倍；仅略优于 GLM 单发 19 min |

历史印证：PR181 最复杂线路（ocr：LLM 分组 + agent 循环）**最慢**（30 min / 3 findings）vs redteam 单发 20 min
同模型同质。**更复杂 ≠ 更快。**

结构判定：
- **分治（map-reduce）≠ 角色分层**。分治 = 同一 diff 切组**并行**、墙钟摊平（真降速）；角色分层 = 同一 diff
  按职责切三段**串行**、墙钟相加（中性到负）。
- 「快」的五个变量：**TPS**（PR181：4× TPS → 4× 墙钟）、**并行 fan-out**、**输出精简**、**调用次数**、
  **增量轮 + cache**。角色分层对五者全部中性或负；它服务的是「好」。
- 「发现便宜、论证贵」洞察本身成立，正确容器是**同一次调用内的输出结构**（先疑点后论证）或**风险路由的触发
  条件**，不是三个独立 agent。

结论：放弃固定三角色串行。「好」改为**按风险触发**：默认快模型全景（分组并行）、规则命中区才慢模型深挖、
verify 只对 blocking 孤证、多样性靠**并行**不加墙钟。→ design v2（决策记录 decisions D1；v3 后深挖改为优先级
队列，见 decisions D2）。

**勘误（2026-09-05）**：上表 reviewer 行把总 output 同质当成了墙钟同质；K 个疑点并行时墙钟 = max 非 sum（rpiv
F1.2，§13.1）。结论不变（固定串行三角色与必走 advisor 轮仍否），推论修正见 decisions D1 勘误与 D6。

---

## 12. 证据 → 设计映射（design v4 各节；推理见 decisions D1-D6）

| 证据 | 设计杠杆 |
|---|---|
| 单模型召回 ~61%、56.8% 单模型独有、第二家族 47%→72%（§1） | 多样性轴：第二家族**并行**快扫（design §5），用钱不用时间 |
| 12B 去相关采样 0.15→0.71（§2.1） | 快扫主干可选多 lens 采样并集（§4）；不靠「换贵模型」 |
| 授权 <30% / 逻辑 ~21% 召回；2K 意图简介翻倍（§2.3） | 风险路由路径规则对齐快模型盲区（§3.3）；仓库简介入稳定前缀（§10.5） |
| blast-radius Hop 0-2 省 86% 不漏；CRG F1 0.69（§4, §10.4） | 深挖 cell 定向 pack = 切片+邻居（§7, §10.2）；图只做裁剪与 callee 签名（§9） |
| 工具=上下文替代品（§8）；ocr 多轮撞 429/503/524（§10.1） | 单发无工具主干；工具分层授予（§11）；不取 agent 循环；worker 池限流（§6）〔D2〕 |
| 假阳=打断通道；共识投票=流行陷阱（§2.3, §1.2） | verify 分歧导向、跨族 refute 只对孤证 blocking（§8） |
| 确定性 AST 可证明覆盖；regex recall 1.0（§4.3） | 确定性地板 L1-L3（§9）承担「不漏」；分组用引用图连通分量（§3.2）〔D3〕 |
| 单发 30K 撞顶（§6） | 按分组/热区把每 call 输出天然有界（§4/§7），无需增量提交协议 |
| §11 墙钟账 | 静态 fan-out + 深挖优先级队列、无固定 advisor 轮、慢模型永不必走（§2, §6, C6）〔D1, D2〕 |
| PR-Agent `model_weak/model_reasoning`；ocr 双阈值（§10.1-2） | cell 类型→模型档路由表（§12）；分组门默认值（§3.2） |
| ocr 时间戳置顶（§10.1） | 前缀不变量反面教材（§10.5） |
| F3 全文>符号的失败模式=真空+缺依赖（§4.3 关联 no-tools F3）；Baz 文件级裁剪不漏、AACR naive RAG 反伤（§2.4, §4.2） | 改动中心切片配方（§10.2）、`pack.body` 轴 scanner 默认 `slice`、`full` 留旋钮、M21a 出口 A/B（§16）〔D4, D8〕 |
| S2-b callee 注入产量 13/fn、77/变更集 regex 做不到（§4.3） | L4-B 三方契约注入扩到 registry 源、`needs{kind: api}`（§9.4）〔D5〕 |
| reviewdog `added` filter mode（§10.3） | positioning `anchor_window` 确定性后滤（§10.4） |
| rpiv F1.2：TPS 单流约束，K 疑点并行墙钟 = max（§13.1） | 快扫改疑点索引（§4）、全部入队 + 预算随风险分 + 必挖集（§6）、C6/C7〔D1 勘误, D6〕 |
| rpiv 漏报四根源 + 缓解必须在生成侧（§13.1） | 疑点并集（§5）、疑点台账跨轮激活（§8.2）、H0 不经扫描直接深挖（§3.3）——三重对症（§13 账） |
| BitsAI-CR RuleChecker→ReviewFilter；Multi-Review Self/Multi-Agg 单调改善（§13.2） | 两遍法与 replicate 的业界同构证据（§4, §5） |
| agreeableness bias / Critic 独立性（§13.2） | verify 换模型族、无生产者历史（§8.1）；advisor 单模型即可 |
| ocr coverage 五集合；`extract_imports` 边被丢弃（§13.1） | coverage 分级（§8.4）；正反向跨组索引（§10.6）〔D6〕 |
| ThinkingLevel schema 零消费；跨档 fallback 反转（§13.1） | `[roles]` 角色层、同档 fallback（§12）〔D6〕 |
| 段序翻转：分化段在证据前截断共享前缀（§13.1） | 角色尾置尾、diff 次尾（§10.3）〔D6〕 |
| ReviewBench 结构化提示 F1 0.13→0.32（§10.4）；PR-Agent 一命令一提示词（§10.2）；Critic 独立立场（§13.2） | 提示词共享头 + 三角色尾、立场表、离线回归集、模板哈希（§10.7）〔D9〕 |
| 本仓 provider 全流式 + 8192 时代截断事故链（§3.4, §6.1）；M15 翻转当天四个集成 bug（§13.1 precedent） | 流式持久化替代增量提交工具化、无 `submit` 旋钮（§7）；分支硬切无 `engine` 旗、对照 = dev 二进制（§16）〔D10〕 |

---

## 13. rpiv 交叉评审补充证据（2026-09-05，〔D6〕）

来源：`.rpiv/artifacts/research/2026-09-04_10-26-56_llm-code-review-multi-agent.md`（贴代码写、含被追问「快」后的 F1-F5
追加）。`~/llm-code-review-architecture.md` 为共同祖先（v0），无增量。

### 13.1 结构性论点
- **漏报四根源**：①模型能力上限（想不到）→ 多模型疑点并集；②注意力稀释（看过没报）→ 两遍法 + 疑点索引化；③diff-scoped
  盲区 → 邻接 full-file / 跨组索引；④分治切割损伤 → 跨组 import 索引 + dedup。关键观察：dedup/agreement/cross-verify/判例
  全部只作用于「至少一个模型报了」的集合，**漏报是其补集，post-hoc 触不到，缓解必须在生成侧**。
- **F1.2 疑点粒度解锁并行**：TPS 是单流约束非总量约束，wall-clock = max(各流) 非 sum；单发 30.9K 是拆不开的串行单流，
  深挖遍 K 个独立 ~1K 流并发，同 token 总量拆 8 路 ≈ 近 8 倍提速。**findings 是终态一次性产出中途拆不开；只有疑点索引
  才能成为可调度的独立工作单元**。→ D1 勘误。
- **F1.1 thinking 外显化**：GLM 30.9K 里只有 ~6-10K 是 6 个 findings 的论证，大头是 thinking（读卷宗、隐式筛选）按
  27 tok/s 计费；scanner 把这部分提到 103 tok/s。（方向成立；其 0.5 min 估计偏乐观，DeepSeek 实测 5.3 min 大头是读+想。）
- **F3 预算驱动 K**：定预算不定 K；不用置信阈值切（边界疑点是漏报敏感带）；台账兜底使「K 保守的代价从漏降为延迟到
  下一轮」。**F4**：K > 并发时 Semaphore 排队 + 预算到期 graceful drain（在途跑完、未开跑者落台账）。
- **F5 调度归属**：LLM 只做声明（疑点/needs/finding），编排器完成排序/预算/分派（可复现、零信息量不花 output、故障域
  隔离）；needs 二阶段已是此原语雏形。
- **段序翻转**：现状角色协议段在头部、证据在尾部；三角色 contract 分化在证据前会把共享前缀截断在分化点 →
  `[共享: role/rules/stance + spec + bundle + dossier] → [角色尾: contract + 指令]`。
- **thinking_level 是角色属性非模型属性**；候补链角色化后 TPS 档位成为候补合法性约束（flash 失败回退 GLM = 4 倍耗时
  静默 inversion）。
- **判例预筛前移**：现状 matches_precedent 只在 refute_targets 调用，判例变体走完完整深挖才被压，预算白烧；前移 =
  机械降级 + 快筛确认而非直接丢弃。
- **跨组索引零件已存在但被丢弃**：`extract_imports` 有向边只在 group() 内消费一次；正向进 `{{OTHER_CHANGED_FILES}}`、
  反向（谁 import 我）= manifestation-site、增量轮 `[delta]` 标注。
- **多角色覆盖分级**：scanner 不完整 → 硬 Incomplete（top-K 建立在部分扫描上 = 结构性选择偏差）；reviewer/advisor
  不完整 → Approve-with-debt。
- **收敛三维**：无 blocking ∧ 疑点台账无 delta 邻接未处理项 ∧ 激活计数无超限；活锁守卫 = 激活超 N 强制升档或交人。
- **ocr 可抄细节**：`analysis` 字段强制先于 `comment_ids`（先推理后承诺）；RelocateAcrossFiles 唯一命中语义；
  `code_comment.go:130-132` 对缺字段条目静默丢弃是漏报源（本仓应逐条校验 + error 回灌）。
- **实现级发现（进 plan）**：ThinkingLevel schema 零消费（`config/mod.rs:52,84`）；项目层剥离清单须加 roles（`:222-231`）；
  CellRecord 装不下疑点（`state/mod.rs:142`）；`eval/harness.py` 只测 dossier 召回非 findings 召回；needs 二阶段与两遍法
  控制流四维不同（扇出粒度 / 模型路由 / 合并语义 / 检查点）但 schema 与证据抓取机器可共享。

### 13.2 外部证据（本文 §1-9 未收录者）
- **BitsAI-CR**（FSE 2025，ByteDance 12000 WAU）：Context Preparation → RuleChecker（宽检）→ ReviewFilter（验证）→
  Aggregation + data flywheel；75% precision；**Outdated Rate（开发者实际采纳率）**作自动评估指标——采纳率反馈环是判例
  台账缺的闭环。
- **Tencent**：LLM 过滤静态分析告警消除 94-98% 假阳且 recall 保持。
- **Multi-Review**（arXiv 2509.01494）：Multi-Agg（多模型聚合）与 Self-Agg（同模型多次）均显著提升 F1，n∈{1,3,5,10}
  单调改善；聚合器双目标 = 提升 recall（多评审一致项高置信）+ 提升 precision（过滤零星单发项）。
- **agreeableness bias**（arXiv 2510.11822）：RLHF 编码「顺从验证」伪相关——advisor 单模型也必须换模型族于生产者。
- **Critic 独立性**（zylos 综述）：Critic 必须独立会话无 Builder 历史；生产案例：独立 Critic 抓住了全部测试通过的性能 bug。
- **The End of Code Review**（arXiv 2606.13175）：LLM 漏训练分布外缺陷且可能静默 approve；缓解 = ensemble + 校准的
  不确定性报告（允许 abstain）→ 支持疑点并集 + coverage 契约。
- **Uber AI review at scale**：correctness 高采纳、style nits 低采纳——类别分层依据（scanner 分域待证）。
- **Qodo 商业版**：专门 agents 并行 → judge agent 合并去重过滤低置信；Rule Miner 从 PR 历史提炼团队规则。**Greptile**：
  全仓 AST 依赖图 → 改动节点 → 受影响 callers 注入。**CodeRabbit**：context 引擎 + 验证层。**HackenProof**：按任务分
  模型档（opus/sonnet/haiku 各司一类漏洞）。

### 13.3 与本文结论的关系
F1.2 修正 §11 的 reviewer 行（墙钟非总量）；漏报四根源与 §1-2 互证并补「生成侧缓解」框架；其余为同构证据。取舍见
decisions D6；未采纳项（三独立 orchestrator / LLM 三元裁决 / 疑点必须工具化 / 0.5 min 扫描估计）亦记于 D6。

---

## 14. aacr-bench 与配置生态事实（2026-09-05 grill，〔D7〕）

### 14.1 aacr-bench（github.com/alibaba/aacr-bench，fork `itcook/aacr-bench` @ `~/codes/aacr-bench`）
- **规模**：仓 15M（dataset 1.5M：`positive_samples.json` 1.1M + `negative_samples.json` 0.5M；evaluation 248K）。
  351 实例 = 200 正样本 PR + 151 负样本（无缺陷 PR，喂噪音率）；50 仓、10 语言；Apache-2.0；亦在 HuggingFace
  `Alibaba-Aone/aacr-bench`；论文 arXiv 2601.19494。
- **语言分布**：C++ 64 / TS 55 / Java 48 / Go 40 / Python 36 / C 35 / JS 20 / Rust 19 / PHP 18 / C# 16。
  diffsym 已支持的 rs / go / ts 共 114 个正样本。
- **PR 规模**：change_line_count p10 24 / p50 160 / p90 695 / max 971。top 仓：ClickHouse 23、FreeCAD 21、
  elasticsearch 21、gemini-cli 14、immich 14、cline 13、keycloak 12、typescript-go 11。Rust 例：astral-sh/uv、
  juspay/hyperswitch。
- **标注**：2145 条；category Code Defect 1022 / Maintainability 905 / Performance 144 / Security 74；context Diff 1017 /
  File 744 / Repo 384；人写 548、AI 增强 1597；每 PR 中位 4 条、最多 33。字段：`path, side, from_line, to_line, note,
  category, context, is_ai_comment, source_model`。实例字段：`githubPrUrl, source_commit, target_commit,
  project_main_language, change_line_count, category, comments`。
- **数据形态**：只存 PR URL 与两端 commit，被测仓运行时克隆到 `repo/<owner__name>`（缓存，`--repo-dir`），每样本前后
  `git reset --hard && git clean -fdx`（会清掉 `.redteam/` 状态目录，恰合单次 PR 无台账语义）。
- **评估器**（`evaluation/`）：`python -m pipeline run --stage {all|review|eval} --reviewer {ocr|claude|codex} --dataset
  data/<key>.jsonl --run-id <id>`；converter `python -m converters.aacr_bench --limit N --seed N --validate` 产可复现
  随机子集，或 `--dataset` 传手筛 JSONL；`--concurrency N` 按仓分组并发（同仓串行）；`--timeout-minutes` 默认 30；
  `--preview` 只克隆不调模型。输出 `results/<bench>/<reviewer>/<run_id>/<safe_id>.json` + `metrics/…/metrics_*.json`。
- **匹配四阶段** path → side → line(k, `--line-k` 默认 1) → semantic；semantic 走 LLM judge（`JUDGE_BASE_URL /
  JUDGE_API_KEY / JUDGE_MODEL`，openai SDK 客户端，需 OpenAI 兼容端点），`JUDGE_USE_MOCK=true` 时前三阶段照算、
  semantic 走本地相似度（**Line-P / Line-R 因此是零 judge 成本的确定性指标**）；`--eval-rounds N` 平均 N 次。
- **指标**（`docs/metrics.md`）：`positive_match_rate = positive_matches / total_generated`（Precision）、
  `positive_recall_rate = positive_matches / positive_expected_nums`（Recall）、line 级同构两项、
  `unmatched_rate = unmatched_count / total_generated`（Noise）。无 F1（ocr README 自算）。缺结果文件的实例进
  `missing_instances` 不计分母；空输出计入并拉低召回。
- **reviewer 接入**：硬编码——`pipeline.py:129-140` 三个 `if/elif` + argparse `choices=["ocr","claude","codex"]`
  （:366）；reviewer 类在 `evaluation/reviewers/{ocr,claude,codex}.py`。输入 = 本地 checkout + base/head commit
  （ocr：`ocr review --from base --to head --format json`；claude / codex 经 MCP finding server）；输出各异，ocr 为
  `review.comments[]{start_line, end_line, content}`。加 redteam = 新增 `reviewers/redteam.py` + 改上述两处。
- **ocr 的公开基准**即 aacr-bench（README 表：F1 / Precision / Recall / Avg Time / Avg Token）。

### 14.2 配置生态事实
- 本仓 `src/config/mod.rs`：`providers: BTreeMap`、`models: BTreeMap`（provider / id / kind 覆盖 / api_key_env 覆盖 /
  context_window / output_limit / reasoning / thinking_level / per_turn_timeout_secs / priority）、`review`（default_models /
  block_threshold 默认 medium / conf_floor 默认 0.5 / max_concurrency / tools）；层序 `~/.config/redteam/config.toml` →
  `.redteam/config.toml`，项目层整段剥离 providers 与 models；secrets `[provider."model_id"] api_key`，解析链模型 env >
  secrets[provider][model] > provider env > secrets[provider] 任一；`model setup` 写配置走 `toml::Value` 重序列化。
- 后端 kind 仅 openai / anthropic（openai-compatible 为别名）；无 google。
- YAML 生态：`serde_yaml 0.9.34+deprecated`；替代 `yaml_serde 0.10`（YAML 组织）、`serde_norway 0.9`、`serde_yaml_ng 0.10`、
  `serde_yaml_neo 0.11`；`toml_edit 0.25` 健康。
- 同形状行数：用户 YAML 33 行；TOML 逐字翻 35、providers 键名表 33（不计空行注释）。

---

## 15. P0 E1 · PR181 风险规则回放数据 → 见 `../private/research/2026-09-07-redteam-p0-validation-gate-research.md` §15

## 16. P0 E3 · 疑点契约 vs finding 契约召回 → 同上 §16（入库侧纯数字摘要：`../roadmap/P0-validation-gate.md`「结果摘要」）
