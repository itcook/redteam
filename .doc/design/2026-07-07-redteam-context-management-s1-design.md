# redteam 上下文管理 · S1「regex 地板 pack」设计（M15-S1）

> 状态: 锁定 · M15-S1 里程碑设计（定论/结果，读如 spec）。
> 上位架构（权威框架）= `2026-07-06-redteam-context-management-design.md`（§1/§3/§4/§6）；本文只解 S1-MVP 的**中层落地**决策，不重述架构。
> 决策/权衡/被否 = 上位决策日志 `../decisions/2026-07-06-redteam-context-management-decisions.md` + 本文 **§6 差异账**。
> 里程碑: roadmap **M15-S1**（`../roadmap/followup.md`）。派生 plan: `../plans/2026-07-07-redteam-context-management-s1-plan.md`（writing-plans 产出）。

## 0. 范围与北极星

- S1 = blast-radius 引擎的 **regex 地板**：把 M14 卷宗演进成「预算治理的 **pack**」。窄路径 = **单语言 Rust + flat 预算 + 一基准变更集（6f95c25）**。
- 北极星（可证伪）= **残余 tool_use（模型主动 read 调用）→ 0**；仪器 384fda6 per-cell 已量。
- **MVP 边界**：缓「广度/精度/维护」（见 §6 差异账），**不缓正确性门**（独立性 D25 / KPI / 对抗内核）。
- **不在 S1**（后续 Stage）：SCIP 语义索引 + 双树/build-gate/3 桶/剪枝/方向性（S2）、per-model 真预算与规划器分叉 + 候补退役（S2）、多语言/跨缝 regex-B（S3）、provisioning 打磨（S4）。

## 1. 数据流与模块

现状（M14）：`select_files → group()→bundles → build_dossiers(bundles)→Vec<String> → run_code_fanout(bundles, dossiers) → build_bundle_prompt(…, dossier)→{{DOSSIER}}`；prompt **model 无关**，扇给多模型。**关键：现有 `Vec<String>` 平行数组 + `{{DOSSIER}}` 槽本身就是「每 bundle 一段上下文、扇多模型」的 cell 机制**——pack 只是该槽里更好的字符串。

S1 目标（最省落法：只换字符串内容，**不引入 `Cell` struct**）：

- 新模块 `src/pack/mod.rs`：`pub fn build_pack(bundle: &Bundle, root: &Path, budget: usize) -> String`——产一个 bundle 的 **3 段 pack 正文**（§2/§3/§4；不含 `<dossier>` 包裹）。**复用** `dossier::{symbols_in_file, innermost_enclosing_def, block_end, reference_points, RefHit, SymRefs}`——引擎不重写。
- 集成接缝 = cli：`build_dossiers` → `build_packs(bundles, root, budget) -> (Vec<String>, PackMeta)`（每 bundle 调 `build_pack`、`render_dossier_section` 包裹）。**`run_code_fanout` / `build_bundle_prompt` 签名一行不改**，pack 字符串照旧填 `{{DOSSIER}}` 槽。**并发 / 候补 / 收集 / manifest 全不变（候补 S1 保留）**。
- **A/B 双开关**：`REDTEAM_NO_DOSSIER`（空臂，沿用）+ 新 `REDTEAM_PACK=0`（退回 dossier-only 基线臂）——同二进制 matched 对照，喂 S0/S1 rig。
- **model-invariant**：flat budget → 所有 bundle 的 pack 跨模型相同（本就如此）。**接缝**：`build_packs` 吃 `budget` 参数 → S2 per-model 各调一次 + `Vec<Vec<String>>` per-model 扇出；`Cell` struct + `run_code_fanout` 改签名 **缓到 S2**（§6 #2）。

## 2. pack 组装与预算

- **预算 = 整 pack 的 flat 常量**，取代 dossier `DOSSIER_MAX_BYTES=28KB` 自帽（一个预算统管 diff+0 壳+1 壳）。常量初值 S0 校准前 **provisional**（见 §5 / plan）。
- **diff 留 `{{BUNDLE_DIFFS}}` 原位** = 审什么的 mandate，**永远全量、绝不被预算砍**；pack 块顶 `{{DOSSIER}}` 槽、**保 `<dossier>` 标签**（复用 `render_dossier_section` 的 D8 惰性保真散文 + scope-guard + `</dossier>` 中和；加宽 prose 与 0 壳段）。
- **填充序（近→远填满预算）**：`剩余 = budget − diff字节`，依次填：
  1. **0 壳 · 改动符号完整体**（§3）
  2. **1 壳 · 引用点**（§3）
  3. **省略清单**（§4，溢出坐标）
- 退化与冗余：diff 超预算 → 正文段空、全进省略清单；0 壳与 diff hunk 重叠 = D12 **接受打包冗余不去重**。

## 3. radius 内容（0 壳 / 1 壳）

- **0 壳（新增）= enclosing def 完整体**：每改动 hunk 用 `innermost_enclosing_def` 定 def 起行 `i` + `block_end(lines,i)` 定块尾，抠**工作树** `[i..=block_end]` 全文；同 def 多 hunk 去重成一体。新增一个「按行范围抠文本」小函数；**无 per-body 帽**（预算 + 省略清单 + `block_end` 4000 行护栏兜底）。攻 tool_use 头号来源=模型想看改动符号完整函数体。
- **1 壳（复用）= 引用点**：`reference_points`（`\b名\b` 词边界全仓 grep = §5 regex-A 同语言引用）+ 自建渲染器；**per-name 帽 10 保留**（相关性/多样性帽，非预算帽）；**28KB 预算自帽退役、上移 `build_pack`**。攻 tool_use 次来源=模型想看 caller。
- **pack 无密度门（有意撤，2026-07-07 终审补账 I-2）**：dossier 的 `DOSSIER_MIN_REFS=5` 密度门是 M14 为「纯引用点卷宗」做的成本优化（稀疏 caller 不值得注入）；pack 的 payload 是 **0 壳 body、与 caller 密度无关**（body 直打「模型补读完整函数」的 tool_use），故 body 不受 ref 密度 gate → **pack 只要有改动符号即注入**。基线臂 `build_dossier`（`REDTEAM_PACK=0`）**保留**密度门 = 当前 dossier 行为，A/B 对照的 OFF 臂即现状。注：纯新增符号（零 caller）下 0 壳 body ≈ diff（D12 接受冗余）——「body==diff 时跳 0 壳」是精度 TODO（T32）。

## 4. 省略清单 + read_files

- **格式**：pack 尾段 `## 已省略（可 read_files 定点读）`；段首一行用法提示 `read_files({paths:[{path,start,end}]})`；一项一行 `path:start-end — <什么>`（如 `src/foo.rs:120-180 — fn bar 体`）。给**范围**直喂 read_files 入参；单引用点无范围则 `path:line`。
- **两来源**（均 §4「被省 radius 的 file:line」）：① 预算砍掉的 0/1 壳项；② per-name 帽溢出（第 11+ 引用点，升级现 `+N more` 计数为坐标）。
- **发出者**：`build_pack`（掌预算记账）。**有界**：项数封顶（复用 ~200 常量）+ 末尾 `… +N more omitted` 计数兜底——防大 fan-in 撑爆、守「无声截断=谎报覆盖」红线。
- **read_files**：现工具全齐（`{path,start,end}` 行段 / 批量 / 总预算截断 / 路径防逃逸），**零改**；坐标格式即入参形状。S1 **不加全局 prompt 指令**（内联提示够；实测模型无视再加）。
- 梯度：**近半径全函数（pack）→ 坐标（省略清单）→ read_files 定点读**。

## 5. 退出门与测量（#7）

- **S1 退出门**：残余 tool_use **跨模型 robust 下降**（% 由 S0 校准、非拍硬数）**且** agreement **不向假共识收窄**（D25 独立性门，与 tool_use **同为门**）。
- **测量 rig 归 S0、S1 复用**：S0 建 rig（基准 6f95c25、**串行 / 匹配 cell / 逐模型 / N=3**，聚 per-cell tool_use 和 + agreement 直方图 + robust 中心趋势）；S1 复用同 rig 量 post-S1、对 S0 基线做差。
  - **tool_use**：仪器 384fda6（`ToolUse` per cell，已穿 `CellRun`）→ rig 聚合。**关键区分**：读**省略项** = 设计好的逃生口、**非** KPI 失败；读**本该 packed 却没给的** = 打包失败。门看后者。
  - **agreement**：从 consolidate 后 finding 的 `models` 计数派生（一 merged finding 的 distinct 模型数 = agreement）→ rig 直方图化。
- **硬依赖**：**S1 验收门需 S0 基线在位**。S1 代码可先建/先合（旗标后），但「S1 达标」判定等 S0。
- **D25 解**（每模型扰动 / 来源标注 / 部分 radius）**开放**，仅门跳才触发设计。

## 6. 完整实现 vs S1 最小实现 · 差异账

> 复盘锚点：结论按最小路径取，但**每处缓的差异在此留痕**（缓去 TODO/Stage 见 §7）。MVP 记账纪律。

| # | 决策 | 完整实现（本可做） | S1 实取（最小） | 缓去 | 为何现在不做 |
|---|---|---|---|---|---|
| 1 | 预算形态 | per-model 真预算（`probe` `context×0.5` + models.dev 策展表 + 真 tokenizer 校准 D7）→ pack 随模型 context 分叉 | 一个 **flat 常量**吃所有模型，cells model-invariant | **S2**（per-model 真预算）+ **T27**（策展表/tokenizer 精度） | flat→单变量→S0 A/B 干净；精度是 S2 挣饭钱的活 |
| 2 | 融合规划器 | `(changes,models,budgets,索引)→cells` 全 per-model 规划器 + `Cell` struct + 执行层收 cells + 候补退役（D20）+ 索引入参 | **无 `Cell` struct**：`build_packs→Vec<String>` 复用现有平行数组接缝，`build_pack(bundle,root,budget)` model-invariant、仍扇多模型，**候补保留** | **S2**（`Cell` struct + per-model 规划器 + `run_code_fanout` 改签名 + D20 候补退役 + SCIP 索引入参） | 现 `Vec<String>`+`{{DOSSIER}}` 槽已是 cell 机制；Cell/改签名=纯 churn 无 S1 收益，S2-ready 不减 |
| 3 | pack 序列化 | 独立 `<pack>`/`<context>` 语义包裹 + 全新 prose，diff+body 物理一体 | diff 留 `{{BUNDLE_DIFFS}}`、pack 顶 `{{DOSSIER}}` 槽、**保 `<dossier>` 标签** | **T28**（实测触发，可能永不需要） | 改名=churn 无行为收益、标签对模型稳定 |
| 6 | 粗 radius 内容 | §4 全粒度——**小文件给全文**（imports/type/module const）、大文件才回落 enclosing；可能 per-body 帽 | 0 壳恒 enclosing def 体、1 壳复用引用点、**无 per-body 帽** | **T29**（小文件全文优化） | enclosing def 体已是 tool_use 头号杠杆核心，小文件全文是增量精度 |
| 4 | 省略清单 | 全量坐标无 cap + 按壳层/符号分组 + 富标注「为何值得读」 | 两来源坐标化、**封顶 ~200 + 计数兜底**、扁平列表简短标签 | **T30**（hiapi 规模 cap 频触则升分组/提 cap） | cap=防膨胀红线；S1 基准（自仓）fan-in 温和、罕触 |
| 5 | read_files 接线 | 全局 prompt 指令段专教「见省略清单必 read_files」+ 可能自动预取 | **只内联提示**（省略段首一行）+ 复用工具描述、无全局指令 | **T31**（实测模型无视内联提示则加） | 无证据会无视，先测后加 |

> **偏离登记（2026-07-07 整分支终审 I-2，非缓项而是行为决策）**：pack **有意撤除** dossier 的 `DOSSIER_MIN_REFS=5` 密度门（详见 §3）——body 是 payload、与 caller 密度无关，故不受 ref 密度 gate；默认 pack 臂只要有改动符号即注入，基线臂 `build_dossier` 保留密度门=当前行为。副作用：纯新增零 caller 符号下 0 壳 body≈diff（D12 接受冗余）→「body==diff 时跳 0 壳」列 **T32**（精度）。A/B 归因提示：pack-vs-baseline delta 混入「加 body」+「撤密度门」两变量，S0/S1 读数时知悉。

- 上位架构：`2026-07-06-redteam-context-management-design.md`；决策日志：`2026-07-06-redteam-context-management-decisions.md`。
- roadmap：**M15-S1**（`../roadmap/followup.md`）。
- 派生 TODO（§6 缓项指针）：**T27**（#1 预算精度）/ **T28**（#3 pack 包裹重塑）/ **T29**（#6 小文件全文）/ **T30**（#4 省略 cap 分组）/ **T31**（#5 全局指令）；#2 全 per-model 规划器 + 候补退役 → **S2 Stage**（Stage 追，无 TODO）。
- 前置：**S0 基线**（未跑，S1 验收硬依赖）；ToolUse 仪器 384fda6 已合 dev（本设计局部先声，[[redteam-latency-rounds-thread]]）。
- 记账纪律：[[mvp-vs-minimal-delta-ledger]]（§6 差异账即首次落地）。
