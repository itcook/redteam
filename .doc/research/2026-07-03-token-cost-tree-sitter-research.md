# redteam 慢且贵问题：code-review-graph / tree-sitter 调研与机制映射

2026-07-03。dogfood 阶段观测到的核心痛点：**慢且贵（token 用量巨大）**。本调研回答：
引入 tree-sitter 类结构分析工具能否在机制层面缓解。结论先行：**能显著缓解，但第一杠杆
不是 tree-sitter**。成本结构实证 + 三层杠杆 + 外部工具机制对照 + 建议路线如下。

调研素材：
- https://github.com/tirth8205/code-review-graph （v2.3.6，下称 CRG）——tree-sitter 建代码图 + PR 评审上下文
- https://github.com/tree-sitter/tree-sitter （v0.27.0）——增量解析引擎本体
- redteam 自身源码 + 真实 dogfood usage 数据（session 20260703-004708）

---

## 1. redteam 的钱花在哪（实证，非猜测）

### 1.1 账单形状

Session 20260703-004708（ship 循环 5 轮、3 模型 DeepSeek/Kimi/VibeCodingFast，审 feedback 优化批）：

| 轮 | input tokens | output tokens | 耗时 |
|---|---|---|---|
| r1 | 3,443,323 | 104,752 | 19.1 min |
| r2 | 2,725,883 | 99,764 | 10.2 min |
| r3 | 2,908,318 | 103,043 | 21.6 min |
| r4 | 3,549,992 | 158,059 | 9.2 min |
| r5 | 4,488,990 | 135,132 | 14.4 min |
| **合计** | **17,116,506** | **600,750** | **74.5 min** |

**输入:输出 = 28.5 : 1。** 成本几乎全部是"重复重发的上下文"，不是模型思考（输出）。
这个比例是全部结论的锚点：凡是不减输入重发的优化都是隔靴搔痒。

### 1.2 输入 token 的三个结构性来源

**① 二次方重发（单 cell 内）。** 每个 cell 是最多 12+1 轮的 tool-loop 对话
（`tool_loop.rs:118` DEFAULT_MAX_ITERS=12 + 强制收尾轮），每轮把全部历史重发一遍
（`run_tool_loop` 逐轮 push 后整个 `messages` 再发，`backends.rs` 两个 build_*_tools_body
都是全量渲染）。工具结果单次上限 64KB（`tools/mod.rs:79` READ_MAX_BYTES），
第 k 轮要重发前 k-1 轮的所有结果 → 单 cell 输入 ≈ (轮数/2) × 最终上下文体积，
对话越长每轮越贵。

**② 探索被 N×B×R 倍重复（跨 cell）。** `adversarial-code-bundle.md:17` 明令：
"The diffs below show only what changed — **read the surrounding files, callers, and
callees yourself** before judging." 于是同一批 callers/callees 被
3 模型 × B bundle × R 轮各自用 search_repo（盲 grep）+ read_file（64KB 全文）摸一遍。
这份工作是**确定性的**——二进制完全可以算一次、所有 cell 共享。refute 尾段
（`verify.rs::refute_one` → provider.run → 又是完整 tool-loop）同样如此。

**③ 零缓存感知。** 三个 provider 全是 `kind = "openai"` 兼容端点
（DeepSeek / api.moonshot.cn / vibecodingfast 中转）。现状：
- 请求体没有任何缓存注解；Anthropic 后端（`backends.rs::build_anthropic_tools_body`）
  没有 `cache_control` → 一旦配 Anthropic 直连，13 轮重发全按全价。
- DeepSeek 服务端自动上下文缓存**可能**已在账单侧打折（命中约 1/10 价），Kimi 平台
  也有缓存机制，中转站大概率没有——但 redteam 只解析 `prompt_tokens`/`completion_tokens`
  （`backends.rs::parse_openai_turn`），**usage 里的 cached 字段被丢弃**，
  17.1M 里有多少实付全价目前不可观测。

旁证：`bundle::group`（bundle/mod.rs:283）里已经有一个手写的行文本迷你"代码图"
（同目录/同词干/相对 import 并查集），注释自认 `use crate::`、裸 import、Go import
都不解析（bundle/mod.rs:227-229"超出确定性 MVP"）——说明"结构分析该内置"这个直觉
项目里早就有了，只是当时没趁手的引擎。

---

## 2. 外部工具机制拆解

### 2.1 code-review-graph：思路对、默认实现打脸自己（当素材库，不当依赖）

**它是什么**：Python 包（tree-sitter-language-pack 解析 35 语言 → 节点/边存单文件
SQLite → git diff 映射图节点 → 双向 BFS 算 blast radius → 喂 LLM/MCP）。v2.3.6、
1378 个测试、GitHub Action/VS Code/MCP/daemon 一应俱全；但 196/325 commits 出自单一作者，
Beta 状态。

**最重要的发现——它自己 commit 的 eval 打脸默认路径**
（`evaluate/results/*_token_efficiency_2026-05-25.csv`）：

| repo | naive（裸读改动文件） | graph（默认 get_review_context） | ratio |
|---|---|---|---|
| flask | 72,069 | **426,628** | 0.2 |
| gin | 44,085 | **362,689** | 0.1 |
| fastapi | 6,045 | **195,653** | 0.0 |
| CRG 自身 | 10,858 | **215,154** | 0.1 |

即：**默认"评审上下文"比裸读改动文件贵 10-40 倍**。README 头条的"82x median 省 token"
是对着"读整仓"baseline 算的（README 自认 "an upper bound no real agent pays"）。
根因：blast radius 的递归 CTE **对边类型不加权不过滤**（CONTAINS/IMPORTS 与 CALLS
同权双向传播、深度 2、500 节点 SQL LIMIT 盲截）→ 扇出爆炸后全量 JSON 序列化。

**其余硬伤**（读代码坐实）：
- 宣传的三档置信度（EXTRACTED/INFERRED/AMBIGUOUS）**零代码实现**，所有边恒为 1.0；
- 跨文件调用默认裸名匹配，仅 Python(jedi)/Java Spring/TS import 三个生态有增强
  ——"35 语言覆盖"是解析覆盖，不是调用图精度覆盖；
- 安全关键词是裸子串匹配（`"sign" in name` 会命中 assign）；
- 宣传 incremental 实为文件级 sha256 跳过 + 整文件重解析，tree-sitter 增量解析 API 零使用；
- 搜索 MRR 0.35、flow 检测 recall 33%（文档自认）。

**值得抄的 5 个机制**：
1. **单文件 SQLite 图 + `WITH RECURSIVE` 遍历**——不常驻语言服务、跨会话存活。
   redteam 已有 `.redteam/proj.db`，图表有现成的家。
2. **content-hash 增量 + 反向依赖有界扩散**（2 跳、上限 500、truncated 标志）。
3. **diff hunk 行区间 ↔ 节点行区间重叠映射**（`git diff --unified=0` + 区间相交判定），
   语言无关、极简。
4. **detail_level 分级 + minimal-first 纪律**（minimal ~100 tokens 档位 + prompt 强制先
   minimal）——CRG 里真正省 token 的是这条路径，不是图本身。
5. **local-first CI 形态**（缓存 db → update||build → sticky comment → fail-on-risk）
  ——远期 M10+ 参考。

**不要抄**：无权双向 blast radius（反教材）、纸面置信度、子串安全启发式、
把它整个当依赖（Python 运行时，与单二进制冲突）。

### 2.2 tree-sitter：真正值得内置的底座（成本清单在列）

v0.27.0（edition 2024, MSRV 1.90）。核心事实：

- **形态**：C 运行时 + Rust FFI；`tree-sitter` crate 经 `cc` 编 C 源静态链入
  → 引入 C 工具链依赖（对 M4 交叉构建：cross/zig 均可处理，但要记入成本）。
- **能力面**：增量解析（`Tree::edit` + `parse(text, Some(&old_tree))`）；
  **错误容忍**（残缺/WIP 代码必得一棵树，ERROR/MISSING 节点可查询）——对审 diff 半成品
  代码是刚需性质的优点；Query = S 表达式 + captures + `#match?` 谓词，
  QueryCursor 有 match_limit 护栏；解析可中断（progress_callback）。
  注意 query 迭代器是 `streaming-iterator`，不是 std Iterator。
- **tags 系统**（`crates/tags`，即 Aider repo-map / GitHub code-nav 的底座）：
  按各语言 grammar 仓的 `queries/tags.scm` 抽 `@definition.*` / `@reference.*`
  （function/class/method/call…），产出 def/ref + 行区间 + docstring。
  **tags.scm 不随主仓分发，须逐语言 vendor**。
- **分发模式**：每语言一个 `tree-sitter-<lang>` crate 静态编入 = 官方主推，
  完全契合"静态单二进制"。动态加载两条路（libloading .so / wasmtime .wasm）
  都破坏单二进制目标，不考虑。
- **体积**（常识判断，未实测）：每语言编入约 0.5-5MB。当前 redteam 二进制 5.0M
  （M4 刚从 10M 瘦到 4.1M），内置 8-12 语言预计膨胀到 15-50M。
  → **必须选极小语言集起步**（rust/python/typescript/javascript/go + markdown），
  没 grammar 的语言退化到现状（tool-loop 兜底），行为无损。
- **维护负担**：grammar crate 各自版本、ABI 13-15 硬边界，升级 tree-sitter 可能连带
  升一批 grammar；grammar 质量参差，tags.scm 覆盖度逐语言验证。
- **天花板**：语法级、单文件、无类型信息、无跨文件语义。tags 的 `@reference.call`
  是裸名——跨文件调用边 = 裸名匹配 + import 图限定的**启发式**（CRG 的精度教训直接适用：
  Rust trait 动态分发/宏展开下会漏/错，需以"上下文提示"定位而非"精确调用图"自居）。
- doc 模式：tree-sitter-markdown 存在（block+inline 双 parser，集成略繁）；但 doc 侧
  痛点是"检索无 bundle 边界"（M6 定案），结构包收益有限，**不进第一期**。

周边一句话：ast-grep（tree-sitter 的结构搜索封装）偏 lint/改写场景，不如直接用
tags+query；rust-analyzer/SCIP/stack-graphs 是"语义级精确"档，体积/复杂度与
模型无关多语言定位冲突，不取。

---

## 3. 三层杠杆（按 ROI 排序）

### L1 缓存感知——先拿账单上的钱（不动产品行为，几乎零风险）
**→ 已实施（2026-07-03 dev d579948）**：cached_input_tokens 观测全链路（含 OpenAI 方言
fallback 链：details.cached_tokens → prompt_cache_hit_tokens → cached_tokens）、
input_tokens 归一为总输入（Anthropic 侧加回 cache_read+creation，对齐 OpenAI 口径）、
Anthropic cache_control 滚动断点（前瞻，当前三 provider 走不到）。以下为原设计文字。

1. **观测先行**：解析 OpenAI 兼容 usage 里的缓存字段（OpenAI `prompt_tokens_details.cached_tokens`、
   DeepSeek `prompt_cache_hit_tokens`/`prompt_cache_miss_tokens` 等），
   记入 CellRecord/usage 与 M12 记分卡。回答"17.1M 里实付全价多少"。
2. **Anthropic 后端补 `cache_control`**（一旦配直连即生效）：多轮工具循环的标准
   增量缓存模式——每轮在最新 block 打 ephemeral 断点，前缀读 0.1×、增量写 1.25×、
   5min TTL 完全覆盖 cell 内轮间隔。13 轮对话输入成本理论折减 ~80-90%。
   注意最低可缓存前缀（Opus 系 4096/Sonnet 系 1024 tokens）与 20-block lookback。
3. 顺手核查各 provider 缓存生效条件（前缀稳定性 redteam 天然满足：对话 append-only、
   模板渲染确定性）。

**上限与边界**：L1 只降账单（和部分 prefill 延迟），**不减少 token 重发量本身**，
对不支持缓存的中转站无效。它是地板不是天花板。

### L2 预计算结构上下文包——tree-sitter 的落点（乘法级，本调研的核心建议）

把模板里"你自己去读 callers/callees"的确定性部分收进二进制，算一次、
注入同 bundle 的全部 cell（3 模型 × R 轮全部受益）：

每 bundle 生成一个 `<structural_context>`（硬预算 ≤8K tokens，超限截断并标注）：
1. **changed file 大纲**：tags 抽 def 列表（签名行 + 行区间），代替模型第一轮
   list_files/全文 read 摸方位；
2. **enclosing scope 全文**：diff hunk 行区间 → 所在函数/方法完整体
   （行区间相交判定 + tree-sitter 节点边界），代替 64KB 整文件读；
3. **1 跳 callers/callees 摘要**：changed symbol 的跨文件引用点
   （tags 裸名索引 + import 边限定 + 同名多义时列全并标注 heuristic），
   给"位置 + 签名 + ±3 行"，不给全文；
4. 工具描述与模板改口径："先用结构包定位，仅在其不足时 read_file 精读"
   ——即 M6"渐进检索"的 code 侧对应物，且核实义务不减（精查非少查）。

**为什么是乘法**：轮数 ↓（预期 8-12 → 3-6）既砍二次方重发的高次项，又砍串行轮次
延迟（10-20min/轮的大头）；每轮工具结果更小；N 模型 × R 轮共享同一份预计算。
配合 L1 缓存，两者正交叠乘。

**CRG 反教材内化为设计红线**：结构包必须 minimal-first、按边类型取舍
（CALLS/INHERITS 才算影响，CONTAINS/IMPORTS 不进包）、风险相关排序后截断、
**永远带体积硬预算**。CRG 默认路径 200-426K token 的事故就是没做这四条。

**索引实现**：tags → `.redteam/proj.db` 新表（files/symbols/refs + file_hash），
content-hash 增量（抄 CRG）；ship 循环轮间只重解析变更文件，正好吃到
"同一变更集反复审"的场景红利。**不需要** CRG 那种全仓图/社区检测/embeddings。

**验收方式**（沿 M6 batch1 的 A/B 方法论）：同一靶 A/B——input_tokens 与轮均耗时
显著降 + 既有四判据不回退（真 finding 不丢、误报不增、id 稳定、reappeared 零）。

### L3 后续可选（各自独立决策，不进第一期）

- **结构化工具**：`outline(path)` / `read_symbol(name)` / `callers_of(name)`
  替代部分盲 grep——收益与 L2 重叠，先看 L2 的 A/B 再决定。
- **bundle 分组升级**：`extract_imports` 换 tree-sitter import query，
  `use crate::`/Go/裸 TS import 可解析 → 分包更准，跨 bundle"无证据实现"类误报
  （现靠 other_files_manifest 压）从源头减少。小工程量，顺手。
- **结构锚定 id**：finding id 从 `hash(file+quote+title)` 升级可选
  `hash(file+enclosing_symbol+quote)`——M7 已知的 dismissed id 跨轮漂移问题的
  潜在改善（quote 在符号内位移不再影响锚定）。
- **跨轮增量评审**（round r+1 只全审上轮修复涉及的 bundle）：动对抗语义
  （修复可能引入远处回归），若做必须 grilling + 显式 opt-in，不当默认。
- **AST-gating**（纯格式/注释改动跳过评审）：真实场景占比存疑，YAGNI 观察。

### 明确不解决什么（诚实边界）

- 多模型对抗 fan-out 与 refute 尾段是产品核心价值，不砍；
- 输出 token 仅占 3.5%，非问题；
- 结构包是加速器不是替代——深语义 bug 仍需模型自行探索，核实义务不变；
- tags 裸名匹配在 Rust 宏/trait 对象、Python 鸭子类型下有精度天花板，
  结构包中标注 heuristic，不冒充精确调用图（CRG 纸面置信度的反面教训）。

---

## 4. 建议路线（供排期参考，未立项）

| 阶段 | 内容 | 性质 |
|---|---|---|
| P0 | usage 缓存字段解析入库/记分卡；Anthropic cache_control | 纯观测 + API 旗标，行为零变化 |
| P1 | 结构上下文包 MVP：内置 rs/py/ts/js/go grammar + tags 索引入 proj.db + 三件套注入（大纲/enclosing scope/1 跳引用）+ 模板口径改 | 引擎级，A/B 验收 |
| P2 | 视 P1 数据决定：结构工具、bundle 分组升级、结构锚定 id | 各自独立小项 |

风险清单：二进制体积（选最小语言集，必要时 feature-gate）；C 工具链入构建链
（cross 可处理）；grammar ABI 维护（锁定版本、升级窗口化）；doc 模式不受益（预期内）。

---

## 附：本次调研的证据位置

- redteam usage 实证：`.redteam/rounds/20260703-004708/r*.result.json` 的 `run.usage`
- 二次方重发：`src/provider/tool_loop.rs:51-116`、`src/provider/backends.rs:15-66,138-201`
- 探索指令：`src/prompt/templates/adversarial-code-bundle.md:17`
- 迷你代码图现状：`src/bundle/mod.rs:227-338`
- 无缓存注解：`src/provider/direct.rs:78-96`、`backends.rs` 全文
- CRG 拆解与 eval 数字：subagent 报告（blast radius `graph.py:640-748`、
  token_efficiency CSV、置信度纸面化 grep 结论等）
- tree-sitter 集成面：subagent 报告（`lib/binding_rust/lib.rs` API 位点、
  `crates/tags`、ABI `docs/src/using-parsers/7-abi-versions.md`）
