# redteam 上下文管理 · S2-a「SCIP 引用精度」设计（M15-S2-a）

> 状态: 作废(→S2-b + 廉价过滤 B) · 探针（`../research/2026-07-08-s2a-false-positive-probe-research.md`）否掉「精度切法单独作里程碑」：57% 改动符号 SCIP 零增益、真杠杆窄（cap 10 名额纯度，与 tool_use 正交）。**廉价过滤 B**（剥注释/字符串、−37% 假阳、零成本）已交付替代其廉价部分（承 T37）；语义引用精度并入 **S2-b**（跟契约分类一次装 SCIP 白捡）。本文 grill 决策栈（Q1-Q5）+ E4 作**过程存档**仍有效——记录探针如何在写 plan 前推翻 Q1。
> 上位架构（权威框架）= `2026-07-06-redteam-context-management-design.md`（§1/§2/§5/§6）；本文只解 S2-a-MVP 的**中层落地**，不重述架构。
> 决策/权衡/被否 = 上位决策日志 `../decisions/2026-07-06-redteam-context-management-decisions.md`（D1-D25 + E1-E4）+ 本文 **§5 决策摘要 / §6 差异账**。
> 里程碑: roadmap **M15-S2-a**（`../roadmap/followup.md`）。前身 = S1「regex 地板 pack」（`2026-07-07-redteam-context-management-s1-design.md`）。派生 plan: 待 writing-plans 产出。
> 拆分来由: 原笼统 S2 拆成 **S2-a**（本文·SCIP 引用精度）/ **S2-b**（双树 + 契约三桶分类）/ **S2-c**（per-model 真预算 + Cell struct + 规划器分叉 + 候补退役）。

## 0. 范围与北极星

- S2-a = blast-radius 引擎的**第一刀语义化**：把 S1 regex-pack 的 **1 壳引用点**（`\b名\b` 全仓 grep）换成**工作树 SCIP references**（同一符号，非同名 token）。窄路径 = **单语言 Rust + flat 预算 + 一基准变更集（6f95c25）**，承 S1。
- **切法 = 精度切法**〔Q1〕：只精化引用**精度**（剔 regex 同名假阳），**不**碰契约分类/剪枝/方向性/per-model 预算/Cell struct（全留 S2-b/c）。
- 北极星承架构 = 残余 tool_use→0；但 S2-a 的价值**不在** tool_use（精度切法去假阳、非补召回，与 tool_use 正交），**主指标改为静态假阳率**（§4）。
- **MVP 边界**：缓广度/双树/契约分类（见 §6 差异账），**不缓正确性门**（独立性 D25 / 无声截断红线 / 对抗内核）。
- **substrate 已实测**（E1/E4）：`rust-analyzer scip` 内建零侧载，冷全量 12s / 热 ~5s / 增量亚秒、15985 符号、def+ref+test-ref occurrence 齐全。

## 1. 数据流与模块（接缝）

现状（S1）：`build_packs(bundles, root, budget)` 内每 bundle 调 `pack::build_pack` → 0 壳（`innermost_enclosing_def`+`block_end` 抠工作树文本）+ 1 壳（`dossier::reference_points` = `\b名\b` grep 产 `Vec<SymRefs>`）+ 省略清单；`render_entries` 渲染 1 壳。

S2-a 目标（最省落法：**只换 1 壳的来源函数**）：

- 新模块 `src/scip/mod.rs`：跑 `rust-analyzer scip` → 解析索引（`scip` crate，protoc 依赖待验 §7）→ 建两张查询表：`(path,line)→Definition symbol`、`symbol→[Reference occurrence]`。**输出类型 = 同构 `Vec<SymRefs>`**（复用现有 `render_entries`，`build_pack` 下游一行不改）。
- **接缝 = 1 壳产出函数二选一**：`scip_reference_points(syms, root, index) -> Vec<SymRefs>`（SCIP 路径）替换 `dossier::reference_points`（regex 路径）在 `build_pack` 里的那一处调用；**符号级**在两者间选（§3）。
- **0 壳、diff、预算、省略清单、per-name cap 10、read_files 全不动**〔直接定〕。「精度」专指跨文件引用精度，0 壳抠本文件改动函数体 regex 已准、与之正交。
- **开关枚举化**〔直接定〕：S1 的 `REDTEAM_PACK` 布尔升为**单开关选引擎** —— `dossier`（M14 基线）/ `regex-pack`（S1）/ `scip-pack`（S2-a）三者**互斥**，消灭 `PACK=0 且 SCIP=1` 非法组合（让非法态不可表达）。诊断臂 `scip-strict` 见 §3。
- **model-invariant 不变**：flat budget 承 S1，SCIP pack 跨模型相同；per-model 分叉留 S2-c。

## 2. 核心机制

- **改动符号 → SCIP symbol = 按位置对**〔机制定，唯一可靠通路〕：`ChangedSym` 已带行号 → 在**工作树**索引里找覆盖该行、`role=Definition` 的 occurrence → 取其 symbol string → 查全仓 `role=Reference` occurrence = 精确 1 壳引用。**按名字对 = 退化回 regex 同名毛病，禁用。**
- **references 过滤**：排除 Definition occurrence 自身与符号 body 内自引用（非 caller）；`local ` 前缀符号（函数内局部）不作 def 锚点。
- **单树（工作树）**〔Q3〕：只精化**存活**改动符号；**删除符号**（工作树位置对不上）走 §3 符号级 regex 兜底——删除符号的旧 caller = 悬空引用 = 同名 grep 天然准。HEAD 树 / 双树 diff 对齐留 S2-b（跟契约分类一起建一起验，那时才有真消费者读旧签名/旧调用图）。

## 3. 降级 + 诊断臂

- **符号级兜底**〔Q2〕：SCIP 整体产出成功后，仍**逐个改动符号**判——SCIP 查得到 → 用 SCIP references；SCIP 查无（宏展开 / derive / cfg 分支 / unresolved，真实 Rust 仓常态）→ **该符号**退回 regex-A `\b名\b` grep。守两条线：① 对抗「宁多展不漏缝」（SCIP 盲区多是宏/生成代码 = bug 高发区）；② 无声截断红线（退化对模型可见，1 壳照常有内容）。
- **整仓降级**：`rust-analyzer` 不在 PATH / scip 崩 / 超时 → 整仓退 regex-A（= S1 地板原样）+ 一行提示（承 D15/D16）。
- **不实现 D8 build-gate**〔直接定〕：E4 实测 RA 容错（duplicate-symbol warning 仍 exit 0 出全索引，编译错不停产）→ D8「工作树编译不过⟹索引失败」premise 对 rust-analyzer **不成立**。S2-a「索引失败」纯触发上述整仓 regex 降级，**不产**「diff 不 build」finding。build-gate 语义留分支 E 专判（大概率得 `cargo check` 另做、非 RA scip）。
- **`scip-strict` 诊断臂**〔Q4〕：第四个开关值——SCIP 查无就**空 + 标注**、**不**退 regex。**只在校准/验收跑一两格**，量出「改动符号 SCIP 命中率 vs 退 regex 率」= SCIP 真实覆盖率。生产默认 `scip-pack` 带兜底，覆盖率只作解释主对照 delta 的探针（§4）。

## 4. 验收门与测量

- **主指标 = 静态假阳率（零 LLM 成本）**〔Q5〕：纯静态比 `regex_refs \ SCIP_refs` = regex 多抓的同名假阳集，量假阳率。**判据方向**：假阳率显著 → SCIP 去噪价值坐实、精度切法成立；假阳率≈0 → regex 本就准、**精度切法对本仓无价值，及早止损、不进端到端**。「显著」阈值 = 先跑一次静态量出 redteam 仓真实分布再定（承 S0「先证尺子」纪律、不拍硬数）。
- **端到端确认（小 N，条件触发）**：静态假阳率显著**才**跑小 N 端到端，看 tool_use / agreement 是否受益；主对照 = **`scip-pack` vs S1 `regex-pack`** 单变量（1 壳来源变、其余全同），对 research **附四**（S1 A/B）做差；S0 **附三** OFF baseline **不重跑**、复用。
- **独立性门贯穿（D25，锁）**：端到端若跑，agreement 分布须「不向假共识收窄」，与 tool_use 同为门。
- **诊断臂读数**：`scip-strict` 覆盖率解释主对照——覆盖率高但 delta 小 = SCIP 精度对下游无帮助；覆盖率低 = 本仓 SCIP 吃不到（宏多）、样本不对（非 SCIP 无效），换仓再评。
- **测量 rig 复用 S0**（串行 / 匹配 cell / 逐模型 / N=3）。

## 5. 决策摘要（grill 定论 · 一句 decision / 一句被否）

| # | 决策 | 定论 | 被否 / 反方向 |
|---|---|---|---|
| Q1 | 第一刀切法 | 精度切法（SCIP 替换 regex 引用；契约分类等留 S2-b/c） | 能力切法（先做契约三桶）——更重、substrate 与语义能力同批、归因脏 |
| Q2 | 降级接缝 | 符号级兜底（SCIP 查无该符号退 regex-A） | 仓级降级（静默给空 caller，违无声截断红线）；折中盲区标注（多一层模型自读 tool_use） |
| Q3 | 树数 | 单树（工作树）；双树留 S2-b | 双树先探对齐雷——建了第一刀几乎用不上、归因混删除符号一路 |
| Q4 | 测量诊断 | 加 `scip-strict` 诊断臂量覆盖率 | 只看端到端——兜底稀释信号、归因黑洞（分不清 SCIP 无效 vs 没被吃到） |
| Q5 | 验收门 | 静态假阳率为主、显著才端到端 | 直接 tool_use 端到端——精度切法去假阳与 tool_use 正交、刻舟求剑 + 烧钱 |

## 6. 完整实现 vs S2-a 最小 · 差异账

> 复盘锚点：结论按最小路径取，每处缓的差异在此留痕（缓去 Stage/TODO 见 §7）。承 [[mvp-vs-minimal-delta-ledger]]。

| # | 决策 | 完整实现（本可做） | S2-a 实取（最小） | 缓去 | 为何现在不做 |
|---|---|---|---|---|---|
| 1 | 树数 | 双树（HEAD SHA 缓存 + 工作树现推 + 端点对齐 D5/D13） | **单树**：仅工作树、存活符号 | **S2-b** | 删除符号有 Q2 兜底、HEAD 树第一刀无独立收益、对齐机制等真消费者（契约分类）再验 |
| 2 | 契约分类 | 双树 diff → 三桶 A/B/C + 传递展 + 剪枝 + 方向性（D4/D6/D9） | 无——只替换引用来源、radius 深度/排序不变 | **S2-b** | 这是 S2 的净新能力、独立一刀；先证 substrate 通路 |
| 3 | 降级混源 | 纯 SCIP（无 regex），盲区走 regex-B 跨缝 | **符号级 regex-A 兜底**（混源） | S3（regex-B） | 宁多勿漏；混源污染归因由诊断臂 `scip-strict` 剥离度量 |
| 4 | 索引产物 | `.redteam/index/`+`cache/` SHA 归档 + 自 gitignore + LRU（D14） | **临时文件即弃、零仓库足迹** | **S2-b** | 工作树索引不跨 run 缓存（D13）、第一刀无 HEAD 缓存需求 |
| 5 | build-gate | 索引失败=「diff 不 build」确定性 finding（D8） | 不做——索引失败纯触发 regex 降级 | 分支 E（专判） | E4 实测 RA 容错、premise 不成立、需 cargo check 另做 |
| 6 | 穷尽性 flag | enum/trait AST 标注待裁信号（D7） | 不做——SCIP 不给全 AST | S2-later | 需 AST 源、缺口、非引用精度范畴 |
| 7 | 0 壳 | SCIP def 范围 / 小文件全文 | 不动，续用 regex enclosing def 抠体 | T29 | 与引用精度正交、regex 抠本文件已准 |

## 7. 开放/隐变量 + 关联

- **`scip` crate 免 protoc 待验**（实现 Task 0）：`scip` crate 是否自带预生成 protobuf 绑定、免构建期 protoc（本机 protoc 缺）。
- **SCIP 覆盖率隐变量**：redteam 仓宏/derive 多寡定命中率——E1 已见 15985 符号 + def/ref/test-ref 齐全（预期不低），诊断臂 §3 量实值。
- **「显著」阈值待校准**：先静态跑一次定分布，同 S0 尺子先行。
- **build-gate premise（分支 E）**：E4 反证 D8，留 S2-later 专判（cargo check 另做）。
- 上位架构：`2026-07-06-redteam-context-management-design.md`；决策日志：同名 `decisions/`（D1-D25 + E1-E4）。
- roadmap：**M15-S2-a**（`../roadmap/followup.md`）；前身 S1 设计 `2026-07-07-redteam-context-management-s1-design.md`。
- 承 T37（regex-A 地板持续优化）：静态假阳率恰是量「地板有多脏、SCIP 净化多少」的尺。
