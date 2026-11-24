# M11 · doc 评审守卫化（方向性 drift 指令）设计

2026-07-02 · grilling 会话定稿（Q1-Q6）。上游：roadmap M11（2026-07-01 定位决议：doc 评审 =
纯「文档交付守卫」，非设计/决策技能）；实证输入：M7 spec dogfood 三条方向性误报（含 CRITICAL）。

## 0. 关键事实更正（本次勘探发现，推翻旧 roadmap 判断）

- `main.rs:240-256` 对**所有** model 无条件装 `ToolLoopProvider`——doc 模式与 code 同享真
  工具执行（read_file/search_repo/list_files）。`registry.rs:8`「doc 路径填 DirectProvider」
  是**过时错误注释**（DirectProvider 生产装配为零，仅剩单测引用）。
- 故 doc↔code drift **不缺引擎能力**；M7 dogfood 的误报是模型**误判文档时间性**（把待实施
  spec 当 as-built 审，"代码里还没有"被当缺陷），纯 prompt 缺口。
- **M11 因此收敛为纯 prompt 批次**：双模板注入方向性指令 + 注释修正 + 断言测试 + dogfood。
- 守卫视角强化（roadmap M11-①）的结论：D1-D5 本就是守卫型底子，M7 已加穷尽性指令，本次
  补方向性指令后无其他待打磨项——①②合并为本 spec 一个批次交付。

## 1. 决策记录（Q1-Q5）

1. **时间性判定：模型自推断，零用户信号**。不加 CLI flag/frontmatter 标注；模型从文档自身
   措辞（"将/应/计划" vs "已/现有"）逐条推断。混合体文档（既有已实现又有待建）靠 finding 级
   粒度自然处理。
2. **finding 级粒度 + 通用指令注入，不新增 lens**。方向性误报是横切问题（D2/D3/… 检索代码
   时都会犯），新增 drift lens 只会把判断局限一处；指令注入模板对全部 D1-D5 生效。
3. **产出物 / 前置 二分**（防矫枉过正的核心边界）：
   - 文档把 X 描述为**待建产出** → 代码没有 X **不是缺陷**（豁免）。
   - 文档**声称 X 已存在/已实现** → 代码验证，缺失或实质不符 → **报 drift**。
   - 文档**依赖既有前置** X（"依赖现有模块 A 的函数 B"）→ 代码无 X → **报缺陷**（隐含假设
     失效，D3 域，绝不豁免）。
   - 文档说待建 X、代码**已有冲突实现** → **报 drift**（文档过时或实施已偏离）。
4. **双注入**：`adversarial-doc-bundle.md`（D1-D5 主评审）+ `adversarial-refute-doc.md`
   （targeted-verify）。实证依据：M7 那条 CRITICAL 误报在 refute 阶段被 **upheld**——第二道
   防线同样缺时间性判断，不注入则误报会穿透 verify。
5. **severity 不额外定规则**：drift 类 finding 按现有 severity 语义走实际阻塞程度（YAGNI）。

## 2. 指令文本（基线草案，实施时可打磨措辞、语义不可动）

### 2.1 `adversarial-doc-bundle.md` — 新增 `<temporal_stance>` 节（置于 `<tools>` 之后）

```
<temporal_stance>
Documents differ in temporal nature: some describe work TO BE built (specs, designs,
plans), others describe an EXISTING system (READMEs, API docs), and many mix both.
Before reporting any finding that rests on a mismatch between this document and code
you retrieved, classify — per claim, from the document's own wording — how the
document presents that content:
- Planned deliverable (the document says it WILL be built): its absence from the code
  is NOT a defect. Never report "the code does not have X" when X is what the document
  sets out to build.
- Claimed as existing (the document asserts X already exists or is implemented):
  verify in the code; if absent or materially different, report the drift.
- Stated dependency or precondition (the document RELIES on existing module/function/
  behavior X): if X does not exist in the code, that IS a defect — the document's
  hidden assumption fails and implementation will hit a wall.
- Planned deliverable that conflicts with something already in the code: report the
  drift — the document is stale or implementation has diverged.
</temporal_stance>
```

### 2.2 `adversarial-refute-doc.md` — 在其 `<tools>`/判定指引后追加精简版

```
Temporal check: if the finding faults the code for lacking something, first classify
from the document's own wording whether that thing is a planned deliverable (to be
built — then the finding is REFUTED: absence of planned work is not a defect), a
claim of existing implementation, or a stated dependency/precondition (absence of
either IS a real defect — uphold).
```

## 3. 顺手项

- `src/provider/registry.rs:8` 过时注释修正（"doc 路径填 DirectProvider" → 如实：生产装配
  统一 ToolLoopProvider，DirectProvider 为无工具单发的备选实现，当前无生产使用方）。
  与 --background bail 文案同类的误导性注释，M7 dogfood 中模板可信度已受其害。

## 4. 测试与验收

- **自动化（hermetic）**：`src/prompt/mod.rs` 加断言——doc-bundle 含 `<temporal_stance>`
  且 refute-doc 含 `Temporal check`（同 T4 exhaustiveness 模式，防未来漂移）。
- **live dogfood 三靶**（prompt 有效性唯一判据）：
  1. **回归靶（消误报，天然 A/B）**：重跑 M7 lgtm design doc 的 doc 评审——原 3 条方向性
     误报（CRITICAL "--background 未实现"、HIGH "--dismissed/init 不存在"、HIGH "字段缺失"）
     应消失/降级/被 refute 推翻。
  2. **防矫枉过正（不漏真 drift）**：造靶验证两类必报——①文档明说"依赖既有函数 X"而代码
     无 X（前置失效）；②文档声称"已实现 Y"而代码无 Y（声称已实现的 drift）。
  3. **真缺陷不受影响**：D1-D5 原能报的矛盾/缺失分支/不可测标准仍报出（靶 2 文档内自带）。

## 5. 执行方式（Q6）

**内联执行**（本会话直接改，不走 subagent-driven）：改动面 = 2 模板 + 1 注释 + 1 断言测试 +
dogfood，核心风险在 prompt 措辞精准度而非代码正确性，需与用户直接迭代。**可选**：改完派一个
fresh agent 独立审 prompt 措辞质量（唯一核心风险面）。dogfood 前须 `make build` 重编二进制。

## 6. 非目标

- 不新增 lens、不加 CLI flag（--doc-status 之类）、不动引擎/管线代码（注释修正与 anchoring
  软换行修复除外——dogfood 中实锤 `locate_quote` 对跨软换行引用误杀真 finding，2026-07-02
  用户批准修法 A：词级序列匹配退路，`positioning/mod.rs::locate_words`）。
- severity 不为 drift 单开规则。
- 设计/决策方法论（脑暴/grill/反向推导）维持出界（2026-07-01 决议）。
- id 跨轮稳定性（M7 已记 roadmap 的模型输出漂移边界）不在本批——那是引导模型引用独特
  quote/稳定 title 的另一项 prompt 工作，与方向性指令正交，避免一次改两变量污染 A/B。
