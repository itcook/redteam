# breakpoint —— 模型无关的对抗评审工具设计（spec）

> 状态：Phase 1 已实施（单发基线落地）；**Phase 2 执行核心重构设计待评审** → 进实现计划
> 日期：2026-06-25
> 关联研究：[`.doc/research/codex-adversarial-review-harness.md`](../research/codex-adversarial-review-harness.md)（codex 现状逆向分析）
>
> ★ **执行核心已于 Phase 2 重构**（工具循环 agent + code/doc 双流水线 + positioning/verify 尾段），权威设计见 **§19**。§5/§7.1/§8/§9/§14 的「单发 + lens 作扇出轴」描述为 Phase 1 历史基线，受 §19 取代的部分已就地加指针。

---

## 1. 背景与动机

现有 `until-codex-approve` + `codex:adversarial-review` 这套对抗评审有效，但有两个痛点：

1. **强依赖 codex**：脱不开 codex CLI + GPT/app-server JSON-RPC，OpenAI 不稳时整套不可用。
2. **耗时长**：单进程、单模型、串行跑一个高 effort 的整体评审，一轮 >10min 常见。

研究文档已划清边界：**真正有效的是「对抗 prompt + 输出 schema + verdict 二值收敛 + 外层核实循环」，这四样全模型无关**；唯一强耦合 codex 的只是「把 prompt 送进一个模型、拿回符合 schema 的 JSON」那一层。

本项目把这套**重建为一个自给自足的二进制**：自己直连各家 LLM、自己做并行 fan-out、自己强约束 schema，模型无关、低耗、可被外部（含未来的循环对抗 SKILL）调用。

---

## 2. 目标与非目标

### v1 目标
- **模型无关**：换模型 = 改配置；靠 provider 适配层 + 「prompt 内嵌 schema + 解析 + 重试」兜底，连零结构化输出能力的模型也能接。
- **高效低耗**：单进程 + 一把 tokio 运行时做并发 HTTP fan-out（不是多 OS 进程），CPU/内存占用平；真正独立的静态单二进制，零运行时依赖。
- **双形态评审**：支持 **Spec/Design 文档**与**实施代码**两类对抗评审（codex 偏代码，这里补齐文档侧）。
- **规范遵循视角**：新增「规范遵循」lens；除非违反明确标注的项目强约束，否则 severity 封顶 medium。
- **轻量编排**：`find → consolidate(dedup+correlate)`；同模型下多 lens 并发不同视角；支持多模型同时评审。（**Phase 2 修订**：code 线改为「文件包并发 + lens 作 rule 注入」，doc 线仍「多 lens 并发」，见 §19.3–§19.5。）
- **slash 插件形态**（非 skill），且**同时可用于 Claude Code 与 Codex**。
- **核心成品是二进制**，可外部调用（退出码 + JSON 输出）。

### 非目标（v1 不做）
- **循环对抗**（跨轮核实/驳回/修复/重审/升级刹车）——留给未来的外层 SKILL，二进制只做"单轮评审核心"。
- per-finding 对抗核实投票（skeptic voting）——v1 编排只到 consolidate，不含 verify 投票。（**Phase 2 修订**：见 §19.6，引入**有条件的定向 verify**——仅复核「会触发 reject 且为单模型孤证」的 finding，仍非全量投票、仍非跨轮循环。）
- 守护进程 / IPC broker。
- LLM 辅助 dedup（默认纯启发式；LLM 辅助留作未来 opt-in）。

---

## 3. 顶层架构

**核心引擎是一个自给自足的二进制 `brk`**：用 HTTP 直连各家 LLM、在进程内做并行 fan-out、强约束 schema、产出结构化报告。

```
┌─ slash 插件（薄壳，Claude Code + Codex 双宿主）
│   commands/skills 调 brk 并渲染结果
│
├─ brk（二进制，模型无关引擎）★核心成品
│   CLI → Config → Evidence → Prompt → Orchestrator(fan-out)
│        → Consolidate(dedup + correlate) → Verdict → Output
│   + Job/State（文件存储，支持后台作业与状态查询）
│
└─ 未来：循环对抗 SKILL（外层 Claude/Codex 编排）
    反复调 brk，做核实/驳回/修复/重审/升级刹车 —— 与今天 until-codex-approve 同构
```

**耦合收敛**：模型耦合**全部**关在 Provider 适配层（aisdk.rs 那一小块）。其余组件（证据收集 / prompt / dedup / consolidate / verdict / 输出 / job）全模型无关，是从 codex 划出的通用骨架。

---

## 4. 技术选型

- **语言：Rust**。理由：本工具重 I/O、轻 CPU，关键诉求是「真正独立的静态单二进制 + 低内存 + 快启动」。Rust 产物最精瘦、内存足迹最小。
- **Provider 适配层：aisdk.rs（`lazy-hq/aisdk`）**。仿 Vercel AI SDK 的 Rust 版，恰好只解决我们唯一要解决的那层（model-agnostic 的 prompt→结构化 JSON）：
  - `LanguageModelRequest::builder().model(..).system(..).prompt(..).schema::<T>().generate_text()`；
  - 原生 provider：OpenAI / Anthropic / Google + `provider-codegen openai-compatible` 生成任意 OpenAI 兼容端点 + 自定义 header/body/base_url（网关/自建/ollama 全覆盖）；
  - 结构化输出 `.schema::<T>()` + `schemars::JsonSchema` + `.into_schema::<T>()`。
- **fan-out / dedup / consolidate / verdict 自己写**（`tokio` + `futures::join_all`）——轻量编排不外包给框架。
- **AutoAgents 不用**（留备胎）：它解决的是自主多智能体 ReAct + tool + memory，我们的"subagent 不同视角"是无状态并发，用它等于背一身死重量，顶撞低耗。
- **TLS：reqwest 走 rustls**（纯 Rust），避免 OpenSSL 动态链接，保证纯静态二进制。

> 风险记录：aisdk 较年轻（context7 评分 81、medium 声誉），有 API 漂移风险；对 hobby + 练 Rust 可接受，provider 面靠"OpenAI 兼容 + 自定义 base_url"基本全覆盖。
>
> **⚠️ 实施更新（2026-06-25，Plan 1 执行期）**：aisdk 0.5.2 经 `reqwest-eventsource → reqwest 0.12(native-tls)` 传递拉入 **OpenSSL**，且不暴露 TLS-backend feature 开关，要满足"rustls-only / 真正独立静态二进制"硬约束只能 vendor 整个 fork。经评估，**弃用 aisdk，改为自写薄 `DirectProvider`（reqwest 0.13 rustls 直连 Anthropic + OpenAI 兼容 REST）**——依赖更干净、TLS 完全可控，`ReviewProvider` trait 边界使该替换局部化。`parse_lens_output`（prompt 内嵌 schema + 解析兜底）保留。下文 §14 的"aisdk `.schema::<T>()` 原生路径"相应改为"OpenAI 兼容端点用 response_format、其余走 prompt+parse 兜底"。遗留：reqwest 0.13 rustls 默认 aws-lc-rs 后端引入 cmake C 构建（静态、运行时仍独立），可选切 ring 后端。

---

## 5. 组件与数据流

> ⚠️ **Phase 2 修订**：下面是 Phase 1 单发流水线（已实施基线）。Phase 2 把执行核心改为「工具循环 agent + code/doc 双流水线 + positioning/verify 尾段」，权威设计见 §19。

单向流水线，每组件单一职责、输入输出是明确数据结构、可独立单测。

```
brk review [...flags]
  [1] CLI/entry      解析子命令与 flag，定位 mode（显式或自动判定）
  [2] Config         provider/key/model/lens 配置（TOML + env），决定 fan-out 矩阵
  [3] Evidence       产出统一 ReviewInput
       ├ code: git diff（cached+worktree+untracked / base...HEAD）
       │        2-file & 256KB 阈值内联，否则文件清单 + "自己去看"指引
       └ doc : 读目标文档（+可选关联 spec 文档）
  [4] Prompt         按 mode 选模板 + 注入当前 lens + {{占位符}}插值
  [5] Orchestrator   构建 (lens × model) 矩阵 → tokio join_all 并发 fan-out
       └ Provider(aisdk): run(model, prompt, schema) -> Vec<Finding>
            原生 structured output 优先；不支持则 prompt 内嵌 schema + 解析 + 重试
  [6] Consolidate    (a) 确定性 dedup（零 token）
                     (b) LLM correlate（有条件，≥2 findings）→ clusters
  [7] Verdict        确定性规则 → approve | reject
  [8] Output         schema JSON → stdout（供外部/SKILL）+ 可选人读 markdown
  [9] Job/State      后台作业生命周期与状态（贯穿，见 §6）
```

数据类型流：`ReviewInput → Vec<Finding>（多 lens×model）→ ConsolidatedFindings + Clusters → ReviewReport`。

---

## 6. 状态与并发模型

**模型 A：无常驻进程 + 状态落盘文件**（不引入守护进程，空闲零成本）。所有落盘文件均为 **ephemeral（随用随清）**，brk 不当持久档案馆。

- 每次 `brk` 调用都是短命进程。
- `brk review --background` → fork 一个 detached worker（re-exec 自身带隐藏 `worker` 子命令 + setsid），父进程立刻打印 `job-id` 退出；worker 边跑边写状态。
- `brk status` / `brk result` 是另外的短命进程读盘。
- "多消费者" = 多个短命进程通过磁盘文件协调。

**文件存储布局**（**临时目录**，按 workspace 路径 sha256 分桶，搬 codex 思路）：
```
$BRK_STATE_DIR（默认 $TMPDIR/breakpoint，回落 /tmp/breakpoint）/
  <workspace-sha256>/
    jobs/<job-id>.json          # 状态文件：status/mode/started_at/progress/result 指针/models/lenses
    jobs/<job-id>.log           # 追加式 NDJSON 进度事件
    jobs/<job-id>.result.json   # 异步投递缓冲（仅后台作业；前台 --wait 不落）
```
- **临时目录而非持久目录**：OS 的 /tmp 清理当兜底，brk 再做机会式 TTL prune。从根上避免膨胀，**无需 `purge`/retention 命令**。
- **原子写**：写临时文件 + rename。
- **job 状态机**：`pending → running → done | failed | cancelled`，progress 记录"几分之几 lens 完成"。

**生命周期（随用随清）**：
- **状态文件**（`.json`/`.log`）：只为支撑后台作业期间的 `status` 轮询；作业结束且 result 被取用后、或超过 `state_ttl`（默认 24h）兜底 → 自动删。
- **result.json**：仅作后台作业的**一次性投递缓冲**——`result <id>` 取一次，留一个 grace TTL 供重取/换 `--format md` 重渲染，到期自动清；前台 `--wait` 报告直接打 stdout，**不落 result 文件**。
- 持久"评审历史"不是 brk 的职责——由未来循环 SKILL 把每轮记录写进项目自己的 docs 目录（同 codex `reviews/round-NN.md`）。
- **机会式 prune**：每次 `brk` 启动时顺手清掉本 workspace 内超 `state_ttl` 的过期文件。

子命令：`status [id]`、`result <id>`、`cancel <id>`（详见 §12）。

---

## 7. lens 视角设计

fan-out 的灵魂：把 codex「一次扫全部攻击面」拆成几个更窄更利的 lens 并发，各自钻得更深。lens 集合**可配置**，下面是默认集。

### 7.1 code 模式 lens（默认）

> ⚠️ **Phase 2 修订**：code 模式的扇出轴改为**文件包**，lens 降级为「按文件类型注入的 rule」（不再是独立 subagent pass）。下表的 lens 定义转为 rule 定义复用，覆盖内容不变；扇出机制见 §19.3–§19.5。

| lens | 覆盖（源自 codex 7 大攻击面，重组） | severity |
|---|---|---|
| **L1 安全与边界** | 鉴权/越权/租户隔离/信任边界/输入校验 | 按影响 |
| **L2 数据与状态** | 数据丢失/损坏/重复/不可逆状态/回滚安全/幂等 | 按影响 |
| **L3 并发与失败** | 竞态/顺序/重入/重试/部分失败/超时/降级依赖/空态null | 按影响 |
| **L4 演进与可观测** | 版本漂移/schema 漂移/迁移风险/兼容回归/可观测盲区 | 按影响 |
| **L0 spec 一致性**（有 spec 才启用） | 实现是否**偏离设计意图**（drift）——是**正确性** | 按影响 |
| **L5 规范遵循**（新，可关） | 编码约定/命名/结构/团队标准 | **≤ medium**（除非强约束） |

### 7.2 doc 模式 lens（默认，codex 没有）

"对抗一篇 Spec/Design 文档" = 尽力证明"这设计还不能开工"：

| lens | 找什么 | severity |
|---|---|---|
| **D1 内部一致性** | 段落矛盾、互斥需求、自相冲突约束 | 按影响 |
| **D2 完备性与歧义** | 缺失分支/未定义错误处理/欠规约/会坑实现的歧义 | 按影响 |
| **D3 隐藏假设与风险** | 未言明假设/未验证依赖；设计层面有无正面回应 安全/数据/失败/迁移 | 按影响 |
| **D4 可测性与成功标准** | 成功标准可否度量、验收条件是否定义 | 按影响 |
| **D5 规范遵循**（新，可关） | 是否遵循团队文档规范/模板 | **≤ medium**（除非强约束） |

### 7.3 关键区分：L0 ≠ L5
- **L0「spec 一致性」**：代码有没有做 spec 要求的事；做错=高危，**按影响定级**。
- **L5「规范遵循」**：风格/约定/团队标准层面；即你要的"规范遵循视角"，**默认封顶 medium**，仅违反**明确标注为项目强约束**的规范才允许更高。

### 7.4 severity 封顶双保险（学 codex 双重 schema 强制）
1. **prompt 侧**：L5/D5 模板明确要求 ≤ medium，除非命中 `hard_constraint`。
2. **Synthesizer 侧确定性兜底**：落盘前，若 finding 来自规范遵循 lens 且未标 `hard_constraint`，把 severity 钳到 ≤ medium。

---

## 8. fan-out 矩阵与多模型

> ⚠️ **Phase 2 修订**：仅 **doc 线**仍是 `lens × model`；**code 线**改为 `文件包 × model`（见 §19.3/§19.5）。本节的多模型语义、priority 候补（§8.1）、replicate 全复制、统一 consolidate 出单一 result —— **两线共用，不变**。

fan-out = **lens × model** 的笛卡尔积，全部并发（`join_all`）。

- **默认（未传 `--models`）单模型**：选定 lens 集 × 1 个默认 model（= `default_models[0]` 或最高 priority），失败按 priority 候补（§8.1）。
- **`--models a[,b,...]` 显式指定模型集**：≥2 触发多模型同时评审、默认策略 **replicate（全复制）**——同一组 lens 在每个 model 上各跑一遍，consolidate 据此捕捉**跨模型一致性**（两个 model 都报 → confidence 上调、`agreement` 增大；只一个报 → 标记待核实）；=1（如 `--models a`）是其退化特例。显式指定一律**不候补**（§8.1）。
- 备选 distribute（lens 分摊到不同 model，省钱但失交叉验证）——不作默认。
- **多模型 = 单一结果（关键，钉死）**：所有 (lens×model) 的 findings 汇入**同一个 consolidate**，产出**一份** result、**一个** verdict。每模型的归因不丢——靠 finding 的 `models`/`agreement` provenance 保留（两模型都报 → `agreement≥2`、confidence 上调；只一报 → `agreement=1` 待核实）。**不存在"每模型各出一份 result"的并行分离模式**：那等价于分开多次 `brk review --models <单个>`，而 `--models a,b` 的全部价值正是合并交叉验证 + 单一收敛信号（循环 SKILL 要的）。想要各模型独立对比，分开跑即可。

并发度由 tokio 控制；可配 `max_concurrency` 上限避免触发 provider 限流。

### 8.1 失败处理与 priority 候补

**候补与否的判据是「是否经 `--models` 显式指定」，不是单/多模型**——`--models a`（显式单模型）是多模型的退化特例，"用户点名了"就同样不候补。intra-model 重试（指数退避，§14）耗尽后的"硬失败"：

- **未经 `--models` 指定（默认路径，brk 自行选模型）→ 按 priority 候补（要韧性）**：主模型 = `default_models[0]` 或最高 priority。某 lens 调用硬失败 → 在**其余已配置 provider**里按 `priority` 降序依次试同一 lens，命中即止；finding 的 `models` provenance 如实记录最终出它的模型。priority 相同则按配置出现顺序。`--no-fallback` 可在默认路径下也强制关掉候补。
- **经 `--models` 显式指定（含单个 `--models a`）→ 仅记录，不候补（忠实显式选择）**：用户点名的模型集是对照基准，**替换会污染结果/跨模型一致性**。故 cell 硬失败只 **记 warning + 该格降级为空 findings**，run 继续出部分结果。
  - **退出码**：仍以评审结论为准（approve=0 / reject=10）；但若**某个被点名 model 的全部 cell 失败**（整模型不可达），在报告 `run` 段标注 `failed_models`，并以运行错误码（2）退出，便于脚本/SKILL 察觉对照集不完整。
- 单 lens 调用失败永不静默吞掉——无论候补成功与否，都在 `run.warnings` 留痕。

---

## 9. Consolidate（dedup + correlate 合并为一阶段）

> ⚠️ **Phase 2 修订**：consolidate 之后新增**尾段两步**——positioning（确定性引用锚定 + 幻觉过滤）与 targeted-verify（只复核 verdict-blocking 的孤证），见 §19.6。本节 dedup/correlate 不变，且 positioning 提供的准行号会反过来提升此处的 line-overlap 去重质量。

dedup 与 correlate 是同一阶段内**两个档次**的操作：

### 9.1 (a) 确定性 dedup（零 token）
折叠"无异议的同一条"：
1. 先按 file 分桶（不同文件永不合并）。
2. 桶内：行区间重叠或间距 ≤ N 行（默认 3）才进候选。
3. 候选再比标题/正文相似度（lowercased token 集合 Jaccard，标题阈值默认 0.5 + 正文兜底）。纯 Rust，无 embedding、无额外依赖。
4. 合并簇：取最高 severity、最大 confidence；多 model 命中 → confidence 上调并标 `agreement`；union provenance（lens+model）；保留最具体 recommendation。
5. doc 模式无行号 finding：退化为同文件内按标题/正文相似度合并。
6. **刻意偏向 under-merge**：行重叠 **且** 标题相似同时成立才合并。宁留近重复，不误合两个不同 bug。

### 9.2 (b) LLM correlate（有条件，仅 ≥2 findings）
不是复述，是**关联同质异质问题**——这是 fan-out 切成多 lens 后唯一会丢失的东西：
- 把"相关但不同"的 finding 聚成 cluster：识别同根因 / 互相叠加成更大风险（如 L1 漏鉴权 + L2 未校验写入 → 合成 exploit-chain）。
- 可对 cluster 产出一条 **meta-finding**（带自己的 severity）。
- **成本守低耗**：0/1 条 finding 无可关联 → **跳过，零额外调用**；`--no-correlate` 强制关。默认成本 = lens×model 次 + [若 ≥2 findings 则 +1]。

> 已被删掉：原"纯合并总结"式的无价值 summary —— 它的位置被 cluster 关联取代。

---

## 10. Verdict（确定性、零 token）

```
reject   当存在 finding 满足: severity ≥ block_threshold 且 confidence ≥ conf_floor
approve  否则
```
- `block_threshold` 默认 **medium**、`conf_floor` 默认 **0.5**，均可配。
- **默认 medium 的理由**：规范遵循 lens 天然产 low/med，若 low 也阻断则 verdict 几乎永远 reject、失去意义。纯 low（多为规范/风格）**照样报出**但不阻断。
- verdict 是 findings（含 correlate 产出的 meta-finding）的**纯函数**，不受 LLM 心情影响 —— 给未来循环 SKILL 的稳定退出信号。correlate 想影响 verdict，只能通过产出带 severity 的 meta-finding，verdict 再对全集重算。
- 取值 **`approve` | `reject`**。

---

## 11. 输出 schema

> ⚠️ **Phase 2 修订**：finding 新增 `quote`（逐字引用，positioning 用）与可选 `verify`（定向核实结果），见 §19.7；其余字段不变。

每个 lens 调用只需吐 **codex 形状的 finding**（severity/title/body/file/line_start/line_end/confidence/recommendation + 规范 lens 可选 `hard_constraint`）；`id`/`lenses`/`models`/`agreement`/`cluster_id` 由本地在 consolidate 时填充。

```jsonc
{
  "schema_version": "1",
  "mode": "code" | "doc",
  "verdict": "approve" | "reject",
  "stats": { "critical":0,"high":0,"medium":0,"low":0,"total":0 },
  "findings": [{
    "id": "stable-hash(file+line+title)",   // ★循环 SKILL 跨轮追踪/驳回清单靠它
    "severity": "critical|high|medium|low",
    "title": "...", "body": "...",
    "file": "...", "line_start": 1, "line_end": 1,   // doc 模式可为 0/段锚点
    "confidence": 0.0,
    "recommendation": "...",
    "lenses": ["L2"],                        // ★provenance
    "models": ["claude","gpt"],              // ★跨模型一致
    "agreement": 2,                          // ★几路调用都报了它
    "hard_constraint": false,                // ★规范 lens 标强约束才 true
    "cluster_id": "c1"                       // ★挂到哪个关联簇（可空）
  }],
  "clusters": [{                             // ★correlate 产物；correlate 跳过时为空
    "id": "c1", "theme": "...",
    "finding_ids": ["..."],
    "relation": "root-cause|compounding",
    "note": "为何相关/如何叠加",
    "severity": "high"                       // cluster meta-finding 的级别（可空）
  }],
  "next_steps": ["..."],                     // 可选
  "run": {                                   // ★status/result + 成本透明
    "job_id": "...", "started_at": "...", "finished_at": "...",
    "models": ["..."], "lenses": ["..."], "duration_ms": 0,
    "usage": { "input_tokens": 0, "output_tokens": 0 },
    "warnings": ["..."],                     // ★lens×model 硬失败/候补留痕（§8.1）
    "failed_models": ["..."]                 // ★多模型下整模型不可达（对照集不完整）
  }
}
```

相对 codex 的关键新增：`id`（稳定哈希，供循环 SKILL 维护驳回清单）、`lenses/models/agreement`（fan-out provenance）、`clusters`（关联）、`stats`、`hard_constraint`、`run.usage`（成本透明）。

输出：`--format json`（默认，供外部/SKILL）；`--format md`（人读，按 severity 分组、显 provenance）。

---

## 12. CLI 面与退出码

```
brk review     [--mode doc|code]              # 省略则自动判定（有 --base/diff→code；目标是文档→doc）
               [--base <ref>] [--scope working-tree|branch]
               [--spec <path>...]             # code: 喂 L0；doc: 受审文档
               [--focus <text>] [--lenses L1,L2,...] [--models claude,gpt]
               [--no-correlate]
               [--block-threshold medium] [--conf-floor 0.5]
               [--wait | --background]        # 省略则按体量自动选
               [--format json|md]
brk status     [job-id]                       # 无 id 列全部；有 id 看单条进度
brk result     <job-id> [--format json|md]
brk cancel     <job-id>
brk worker     ...                            # 隐藏：detached worker 入口
brk model setup  [--provider <n>] [--kind <k>] [--base_url <url>] [--model <id>] [--key <secret>] [--priority <n>]
                                              # 配置 provider/model（缺省字段交互补全，--key 隐藏输入）（见 §13.1）
brk model list   [--remote <provider>]        # 列已配置 model + 可用性 + 配置文件位置；--remote 查端点 /v1/models（见 §13.2）
brk model check                               # 轻量可用性检测（key 就位 + 端点可达）（见 §13.3）
```

> `codex-skills/` 的生成**不在 brk 职责内**——它是 build/dev 工具链的事，由 `scripts/gen-codex-skills`（bash）+ `make gen` 完成（见 §15），runtime 二进制不含此命令。

**退出码**（服务"可供外部调用"，让脚本/循环 SKILL 不解析 JSON 也能 gate）：
`0 = approve`，`10 = reject`，`1/2 = 运行错误`。三态分明（reject ≠ error）。
- `brk model check` 的退出码：全部已配置 model 可用 → 0；有 model 不可用（缺 key/端点不可达）→ 非 0，便于脚本预检。

---

## 13. 配置

TOML，分层覆盖：内置默认 → 用户 `~/.config/breakpoint/config.toml` → 项目 `.breakpoint.toml` → 环境变量 → CLI flag（右优先）。

```toml
[providers.claude]
kind        = "anthropic"            # → aisdk provider 种类
model       = "claude-..."
api_key_env = "ANTHROPIC_API_KEY"    # ★只存"读哪个 env"，绝不在文件里存密钥
priority    = 30                     # ★数值大=优先级高；单模型候补按它降序（见 §8.1）

[providers.gpt]
kind        = "openai"
model       = "gpt-..."
api_key_env = "OPENAI_API_KEY"
priority    = 20

[providers.local]                    # 任意 OpenAI 兼容端点（网关/自建/ollama）
kind        = "openai-compatible"
base_url    = "http://localhost:11434/v1"
model       = "qwen2.5-coder"
priority    = 10

[review]
default_models  = ["claude"]         # 多模型: ["claude","gpt"]
block_threshold = "medium"
conf_floor      = 0.5
correlate       = true
max_concurrency = 8
state_ttl       = "24h"              # 临时状态/result 文件的随用随清兜底 TTL（见 §6）

[review.lenses]
code = ["L1","L2","L3","L4","L0","L5"]
doc  = ["D1","D2","D3","D4","D5"]

[lens.L5]
max_severity = "medium"              # 规范遵循 severity 钳制（双保险配置侧）
```

- **模型无关的全部表面就是 `[providers.*]`**：加 model = 加一段 block（`kind` 映射 aisdk provider）；多模型 = `default_models`/`--models` 列多个。

> 配置实质只有"模型/provider"一类，故全部命令收在 `brk model` 下，无独立 `config` 命名空间。

### 13.1 `brk model setup`（配置 provider/model）
- **flag 驱动 + 缺省交互补全**：`brk model setup --provider <name> [--kind <kind>] [--base_url <url>] [--model <id>] [--key <secret>] [--priority <n>]`。
  - `--key` 省略时 **隐藏输入**提示（不进 shell 历史 / 进程参数）；显式 `--key` 仅为脚本化便利。
  - `--kind` 可省：给了 `--base_url` 默认按 `openai-compatible`，否则按 provider 名推断（claude→anthropic、gpt→openai）。
- 写入分工，**保持主配置不含密钥**：
  - `kind`/`base_url`/`model`/`priority`/`api_key_env` → 写进主配置 `config.toml` 的 `[providers.*]`。
  - **API Key 两条去处**（向导/flag 让用户选）：①（默认/推荐）env：只打印 `export <VAR>=...` 供加进 shell profile，主配置仅记 `api_key_env`，CI 友好；②secrets 文件 `~/.config/breakpoint/secrets.toml`（**chmod 600**），与主配置物理隔离。
  - **读取优先级 env > secrets**；主配置 `config.toml` 永不含明文密钥——评审工具自己先做到密钥卫生。
- 完成后自动跑一次 `brk model check`。

### 13.2 `brk model list`（列已配置 model）
- 列出**已配置**的 provider/model + 对每个的**可用性**（✓ 可用 / ✗ 缺 key / ✗ 不可达），**并打印配置文件位置**（分层来源）。
- `--remote <provider>`：对 OpenAI 兼容端点查 `/v1/models`，列出该端点**实际提供**的模型（辅助填 `model` 字段）。

### 13.3 `brk model check`（轻量可用性检测）
- 对所有已配置 model 做轻量检测：key 是否就位（env/secrets）+ 端点是否可达。供脚本/循环 SKILL 评审前预检；退出码见 §12（全可用→0，有不可用→非 0）。

---

## 14. Provider 适配层（耦合收敛点）

> ⚠️ **Phase 2 修订**：接口从「单发 `run`」升级为**带工具的多轮循环**（模型可调 read/search/list 代码库，brk 执行回灌），见 §19.2。本节的「prompt 内嵌 schema + 解析 + 重试」**保留为弱模型 + 极小 diff 的单发兜底**；指数退避重试、按模式分流的失败处理（§8.1）不变。

唯一与"具体模型"打交道的地方。接口约一行：`run(model, prompt, schema) -> Vec<Finding>`。

- **原生 structured output 优先**：provider 支持 JSON Schema（OpenAI/兼容端点）时用 aisdk `.schema::<T>()`。
- **prompt 内嵌 schema + 解析 + 重试 兜底**（**真·模型无关的关键**）：provider 无原生结构化输出（如 Anthropic）或解析失败时，把 schema 文本嵌进 prompt、要求只回 JSON、本地解析、失败带错误信息重试 N 次（默认 2）。
- 这条兜底是无论如何都要写的（aisdk 与 AutoAgents 对 Claude 都无原生 structured output），写了它连零结构化输出能力的模型也能接。
- 错误/限流：**intra-model 指数退避重试**（默认 2 次）。重试耗尽的硬失败后续处理按模式分流——单模型按 priority 候补、多模型仅记录——详见 §8.1。

---

## 15. 跨宿主插件打包（Claude Code + Codex）

**关键事实（实地查证）**：Codex CLI 故意把 Claude 的插件体系"搬"得很彻底——
- 清单发现路径 `[".codex-plugin/plugin.json", ".claude-plugin/plugin.json"]`，**fallback 读 Claude 的清单**；
- 安装模型相同：`plugin marketplace add owner/repo` → `plugin install/add name@marketplace`；
- 唯一运行时差异：Claude slash 命令 = `commands/*.md`；Codex slash 命令 = `skills/<name>/SKILL.md`；
- 而 Codex **内置 `render_command_skill`**，把 Claude command 机械转成 Codex skill —— 即同步问题用 codegen 解决，不手维护两份。
- **宿主在用户机器上不做构建**，git 仓里是什么布局就读什么。

**结论：一个仓（它本身即 marketplace）+ 仓内 codegen**。`commands/` 是唯一手写真相源，`codex-skills/` 由 `scripts/gen-codex-skills`（bash，`make gen`）生成并提交。

```
breakpoint/                       ← 一个 git 仓 = marketplace
  .claude-plugin/
    marketplace.json               # Claude 市场入口（Codex 大概率亦读，待实现期确认常量）
  plugins/breakpoint/
    .claude-plugin/plugin.json     # 清单 —— Codex fallback 读它 ✓（一份通吃）
    .codex-plugin/plugin.json      # 仅 ~3 行 {name,version,skills:"codex-skills"}（亦可生成）
    commands/                      # ★唯一真相源（手写，Claude slash 命令）
      review.md  status.md  result.md
    codex-skills/                  # ★由 commands/ 生成（Codex slash 命令）
      review/SKILL.md  status/SKILL.md  result/SKILL.md
    bin/brk-darwin-arm64           # 预编译二进制（先出 darwin-arm64，跨平台后补）
  scripts/gen-codex-skills         # bash 脚本，镜像 codex render_command_skill；不入 brk runtime
  Makefile                         # make gen → 重生成 codex-skills/；CI diff 防漂移
```

- **避免 Claude 把 Codex skill 当重复 skill 加载**：用 `.codex-plugin/plugin.json` 的 `skills` 字段指到 **`codex-skills/`**（非默认 `skills/`），Claude 只读 `commands/` 与默认 `skills/`，不碰 `codex-skills/` → 一个文件夹、零碰撞。
- **slash 命令**：`/breakpoint:review`、`/breakpoint:status`、`/breakpoint:result`。`review` 为计费重操作 → `disable-model-invocation`（仅用户触发）；`status`/`result` 为轻读，可允模型触发。
- **无用户侧 `brk install`、无第二个仓**。安装 = 各宿主原生 marketplace 命令。
  - Claude：`/plugin marketplace add 你/breakpoint` → `/plugin install breakpoint`
  - Codex：`codex plugin marketplace add 你/breakpoint` → `codex plugin add breakpoint@breakpoint`

---

## 16. 从 codex 复用清单（模型无关，搬思路/改造）

- **对抗 prompt 模板灵魂**（`<role>/<operating_stance>/<attack_surface>/<review_method>/<finding_bar>/<structured_output_contract>/<grounding_rules>/<calibration_rules>/<final_check>`）—— 按 lens 参数化后复用。
- **输出 schema**（verdict/summary/findings/...）—— 作为我们扩展 schema 的底。
- **git 证据收集** + 256KB/2-file 内联阈值（`collectReviewContext`）。
- **job 生命周期/前后台/status-result/工作区 sha256 分桶存储**。
- 外层核实-驳回-修复-重审-升级刹车循环 —— 留给未来 SKILL 层。

---

## 17. 未来：循环对抗 SKILL 层（v1 之后）

二进制只做单轮评审核心。循环对抗放在外层 SKILL（Claude/Codex 编排），反复调 `brk`：
- 读 `verdict`（或退出码）决定是否继续；
- 对 `needs`-修复的 finding 逐条对抗性核实（属实→修；误报→记 dismissed）；
- 用 finding 的稳定 `id` 维护跨轮"累计驳回清单"，回喂下一轮 `--focus`；
- 升级刹车：同一已驳回 id 反复再现且仍判误报 → 停，交用户裁决。
- 与今天的 `until-codex-approve` 同构，只是底层换成模型无关、并行的 `brk`。

---

## 18. 实现期待办 / 未决项（实现时落实）

1. **确认 Codex 远程 marketplace.json 的发现常量**（是否读 `.claude-plugin/marketplace.json`，还是需 `.codex-plugin/marketplace.json`）。
2. **确认 `render_command_skill` 的具体转换规则 + Codex skill 的参数传递约定**（`$ARGUMENTS` 在 SKILL 里如何映射），据此实现 `scripts/gen-codex-skills`（bash）。
3. **aisdk 版本钉定 + provider 能力探测表**（哪些走原生 schema、哪些走兜底）。
4. **自动 mode 判定的精确规则**（`--base`/diff 在场→code；目标路径为文档→doc；歧义时报错要求显式）。
5. **git 证据收集移植**：working-tree 与 branch 两路、内联阈值、untracked 处理。
6. **进度事件粒度**（每完成一个 lens×model 写一条 NDJSON）与 `status` 的渲染。
7. **跨平台二进制**（v1 仅 darwin-arm64；后补 linux/win）。
8. 默认 lens 集是否需要精简（成本 vs 覆盖）的实测校准。

---

## 19. Phase 2 架构修订（执行核心：工具循环 + 双流水线）

> **取代**：§5 单发流水线、§7.1 code lens 作扇出轴、§8 的 `lens×model`（仅 code 部分）、§9 consolidate 尾段、§14 单发 provider。**不变**：§6 状态/job、§7.2 doc lens 集、§7.4 severity 双保险、§8.1 失败/候补、§10 verdict、§11 输出、§13 配置/密钥、§15 跨宿主。
> **触发**：Phase 1 单发基线落地后复盘，参照成熟工具 **alibaba/open-code-review（OCR）**（阿里内部两年、几万开发者、百万缺陷，已开源）重审执行核心。

### 19.1 动机：单发 + lens-as-fanout 的两个错配

1. **视角扇出不提效，反伤效。** 不论分不分视角，LLM 评审都要「遍历」涉及代码；按视角扇出会让**每个视角重读全部代码（N× 成本）**，且模型发现问题后还要把它归到某个视角。真正的效率/精度杠杆是**缩小每个 subagent 的上下文**——扇出轴应是**代码本身（按文件/文件包切）**，视角降级为**按文件类型注入的 rule**。
2. **单发给不了「自己去看」的手。** §5 的「2-file/256KB 内联否则甩文件清单 + 自己去看」甩了清单却没给工具——单发 provider 无法读取/检索代码库。必须上**工具循环**。
3. **文档 ≠ 代码（反直觉结论）。** lens 扇出**对代码错、对文档对**：文档通常单文件、塞得进上下文，N 次重读便宜，强制不同视角确实逼出不同的洞；文档无「文件包」可切，对抗性（多模型/多视角找漏洞、找矛盾）价值更高。

**OCR 关键事实（佐证）**：① 执行模型是带工具的 agent 循环（read 全文件 / search 代码库 / 看其它改动文件）；② 扇出轴是**文件包**（相关文件聚为一 review unit，每包隔离上下文 sub-agent、并发），**不按视角**；③「视角」在它那是按文件类型匹配注入的 rule；④ **确定性工程 × agent** 混合（文件筛选/分包/规则匹配/评论定位/反思由代码保证，动态判断交 agent）；⑤ 精度优先、故意低召回，token 仅 Claude Code 的 ~1/9。

**我们与 OCR 的有意分歧**：OCR 精度优先 / 单模型 / 低召回；我们是**对抗 + 多模型 + 高召回**。借它的**结构**（文件包分解、工具循环、确定性定位/反思），不照搬它的精度/召回**取舍**。

### 19.2 执行模型：工具循环 agent（取代单发）

`ReviewProvider` 从「单发 `run(prompt, schema) -> Vec<Finding>`」升级为**带工具的多轮循环**：模型可调 `read_file` / `search_repo` / `list_files`，brk 执行并把结果回灌，循环至模型产出最终 findings。

- **模型无关仍成立**：Anthropic tools / OpenAI function-calling 各写一个 adapter；零工具/零结构化能力的弱模型**回落到 Phase-1 的 prompt-embed-schema 单发兜底**（保留，用于弱模型 + 极小 diff）。
- **工具实现，零外部依赖**：`search_repo` = 链入 **ripgrep 内核 crate**（`ignore` 做 .gitignore 感知遍历 + `grep-searcher`/`grep-regex` 做搜索），静态进二进制；`read_file` = fs 读（带行号）；`list_files` = `ignore` 遍历。**绝不 shell 调外部 `rg`/`git grep`**，守住「真正独立的静态二进制」硬约束。
- **确定性工程 × agent**（学 OCR）：工具调度、文件筛选、分包、锚定、verdict 由确定性代码保证；只有「读什么 / 怎么判断」交给模型。

### 19.3 双流水线（共脊柱）

| 维度 | code 线 | doc 线 |
|---|---|---|
| 扇出轴 | **文件包 × 模型** | **lens × 模型** |
| 上下文 | 每包只含其文件（隔离、并发） | 整篇文档进上下文 |
| 工具 | read/search 代码库 | read 它引用的 spec/代码（验可行性/一致性） |
| 视角 | lens → 按文件类型注入的 rule | lens 即扇出轴（D1–D5 不变） |
| 锚定 | 行级（file:line） | 章节/引文 |
| 去重相似度 | file + 行重叠 + 标题 | 主题/引文 |

**共脊柱**：provider/工具循环引擎、consolidate、positioning、targeted-verify、verdict、output、config。两线只在「分解轴 + 锚定 + 去重相似度」分叉。

### 19.4 lens 的再定位
- **code**：lens → **rule**（按文件类型/路径匹配注入的关注清单，不再是独立 subagent pass）。Phase 1 的 L1–L5 注册表转成 rule 定义复用；L5 规范遵循 rule 仍 severity 封顶 medium（§7.4 双保险不变）。
- **doc**：lens **保留为扇出轴**，D1–D5 原样。

### 19.5 文件分包（code 专属）
- 借 OCR「smart file bundling」：相关文件聚为一个 review unit（轻启发式：目录邻近 / 同名多语言资源 / 显式 import 关系），每包一个隔离上下文 sub-agent，按 `max_concurrency` 并发。
- **确定性文件筛选**（哪些该评、哪些过滤）由代码做，不交模型。
- 超大 changeset 靠「分包 + 隔离 + 并发」稳定，**取代** §5 的「内联否则甩清单」；内联只保留为极小 diff 省一次往返的优化。

### 19.6 尾段四步（共脊柱：positioning + targeted-verify）

```
fanout → consolidate(dedup + agreement) → positioning → targeted-verify → verdict → output
```

- **positioning（定位，确定性、零额外 LLM）**：finding schema 强制带 `quote`（逐字引用出问题的代码/文档原文）；确定性解析 `quote → 真实 file:line / 章节`，**绕开 diff↔文件行号换算的漂移**。引用在源中找不到 → 判幻觉，丢弃/标记（**免费 reflection-lite**）。准行号反过来让 dedup 的 line-overlap 才有意义（提升去重质量）。code 解析到行，doc 解析到章节/引文。
- **targeted-verify（reflection 的我们版，唯一可能花钱的一步）**：不做 OCR 式全量自检（贵 + 杀召回）。三层从便宜到贵：
  1. **多模型 agreement ≥ 2** → 已被多模型背书，不验；
  2. **引用存在性** → 已在 positioning 砍掉编造；
  3. **定向 refute pass** → 只对**「severity ≥ block_threshold ∧ agreement 低（孤证）」**的 finding spawn skeptic 反驳。理由有界：verdict 确定性（severity ≥ 阈值 ∧ confidence ≥ 下限），**单模型幻觉一个 critical 就能把 approve 翻成 reject**——定向 verify 专守 verdict 完整性。不 block verdict 的低 severity finding **全保留输出、不验**（守高召回）。
- **明确不要**：OCR 全量 reflection；独立 LLM positioning 模块；对 doc「遗漏类」finding 强行 positioning（它本质是召回判断，无法确定性证伪 → 走第 3 层让另一模型判真伪；「矛盾类」可确定性验：引两段判是否真矛盾）。

### 19.7 schema 增量（在 §11 基础上）
- finding 新增 `quote`（逐字引用，positioning 用）；可选 `verify`（定向核实结果：`{ checked, verdict: confirmed|refuted|uncertain, by }`）。
- 现有 `models`/`agreement` 继续作交叉校验信号，喂第 1 层 verify 与 confidence。

### 19.8 取代关系一览
| Phase 1（§） | Phase 2 取代为 |
|---|---|
| §5 单发流水线 | 工具循环 + code/doc 双流水线 |
| §7.1 code lens 作扇出轴 | 文件包作扇出轴；lens → 按文件类型注入的 rule |
| §8 `lens×model` 矩阵（仅 code 部分） | `bundle×model`（doc 部分仍 `lens×model`） |
| §9 consolidate 尾段 | 增 positioning + targeted-verify 四步尾段 |
| §14 单发 `run` | 工具循环 `run`（弱模型 + 极小 diff 保留单发兜底） |

**不变（Phase 1 继续有效）**：§6 状态/job、§7.2 doc lens、§7.4 severity 双保险、§8.1 失败/候补、§10 verdict、§11 输出、§13 配置/密钥、§15 跨宿主。

### 19.9 分期建议（PLAN 层决策）
非设计、属计划排序。建议：①**先把「工具循环 + code 文件包流水线 + 尾段 positioning」单模型跑通**（OCR 验证过的高价值区）→ ② 加**多模型**（召回/交叉）+ **targeted-verify** → ③ **doc 流水线**（我们的差异化）。其中 doc 与多模型可调序。

> 已落地为完整路线：[`.doc/plans/2026-06-26-breakpoints-phase2-roadmap.md`](../plans/2026-06-26-breakpoints-phase2-roadmap.md)（S2–S6 五阶段，含运营面/打包与 Phase-1 遗留 Minor 并入映射）。

---

## 附：已定决策速查

| 决策点 | 结论 |
|---|---|
| 引擎架构 | 二进制直连 provider API，进程内并发 fan-out |
| 语言 | Rust |
| Provider 框架 | aisdk.rs + 自写编排（AutoAgents 备胎） |
| 状态/并发 | 模型 A（无守护进程）+ 临时目录文件存储；随用随清(state_ttl 24h)，无 purge/retention |
| 评审形态 | code + doc 双模式 |
| 规范遵循 | 独立 lens（L5/D5），severity ≤ medium 双保险 |
| 编排 | find → consolidate(dedup 确定性 + correlate 有条件 LLM) |
| verdict | approve \| reject（确定性，默认 block_threshold=medium） |
| summary 字段 | 删除（被 clusters 关联取代） |
| 多模型 | 默认单模型；`--models a,b` replicate 全复制 → **统一 consolidate 出单一 result**（非每模型各一份），归因靠 `models`/`agreement` |
| 失败处理 | 未传 `--models`(默认)按 `priority` 候补（韧性）；显式 `--models`(含单个 `a`)仅记录 warning/failed_models（不候补） |
| 密钥 | 默认走 env，或独立 secrets.toml(chmod 600)；主配置永不含明文；优先级 env > secrets |
| 配置体验 | `brk model setup/list/check` 统管模型配置（无独立 config 命名空间）；codegen 走 bash 脚本不入 brk |
| 形态 | 二进制 + slash 插件（非 skill） |
| 跨宿主 | 一个仓 + codegen；commands/ 真相源 → 生成 codex-skills/ |
| 循环对抗 | v1 不做，留外层 SKILL |
| **— 以下为 Phase 2 修订（见 §19）—** | |
| 执行模型 (P2) | 工具循环 agent（read/search/list 代码库）取代单发；弱模型 + 极小 diff 保留单发兜底 |
| search 工具 (P2) | 链入 ripgrep 内核 crate（`ignore`+`grep-*`），静态进二进制，零外部命令依赖 |
| 扇出轴 (P2) | code：文件包 × 模型（lens → 按文件类型注入的 rule）；doc：lens × 模型 |
| 流水线 (P2) | code/doc 双线，共脊柱（provider/工具循环/consolidate/positioning/verify/verdict/output/config） |
| positioning (P2) | finding 带 `quote`，确定性解析到 file:line/章节；引用缺失即判幻觉（免费 reflection-lite）；提升 dedup |
| reflection (P2) | 不做全量自检；用「多模型 agreement + 引用存在性 + 定向 refute」替代，定向 verify 只打 verdict-blocking 的孤证 |
| 分期 (P2) | 建议 ① code 工具循环单模型 → ② 多模型 + verify → ③ doc 线（doc/多模型可调序，待确认） |
