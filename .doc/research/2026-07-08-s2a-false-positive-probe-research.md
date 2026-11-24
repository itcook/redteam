# S2-a 静态假阳率探针（regex 引用 vs SCIP references）

> 状态: 已交付 · 一句话: redteam 仓 regex `\b名\b` 引用假阳率**双峰极化**——57% 符号 0 假阳、10% 符号 >50%，价值集中在「与 stdlib/域名词同名」的符号 + cap 10 交互。**定案=做廉价过滤 B（剥注释/字符串，−37% 假阳、零成本零风险，已实现）**；S2-a 独立 SCIP 里程碑撤，语义精度并入 S2-b。

喂 M15-S2-a 决策的零 LLM 成本实验（2026-07-08）。判据：假阳率显著→SCIP 去噪价值坐实；≈0→精度切法止损。**结果=既非显著亦非≈0，而是极化**，反过来质疑「精度切法单独作第一刀」的 ROI（见 §4）。

## 1. 方法

- **substrate**：`rust-analyzer scip .`（热 4.6s / 冷 12s E1）产 `index.scip`（3.4M / 37482 occurrence，Definition 5200 / Reference 32282）。
- **SCIP 解析**：`scip` crate v0.9.0 一次性 dump 程序（`scip_probe`，scratchpad）→ TSV `path\tline0\troles\tsymbol`。**Task 0 顺验✅**：scip crate 编译仅拉 10 crate（protobuf 3.7.2 自带预生成绑定）、**零 protoc**、10.7s build。生产实现可用。
- **regex 引用**：一次过扫 `git ls-files '*.rs'`（43 文件，path 与 SCIP relative_path 完全对齐）、逐行 tokenize 桶化 = 等价对每名 `\b名\b`（含注释/字符串命中，即 S1 `reference_points` 口径）。
- **假阳定义**：`fp = regex_refs \ SCIP_refs`（净引用，两侧剔 def 行）。改动符号→SCIP symbol 用**名字匹配取并集**（同名多 def 的 ref 全算非假阳）= **保守低估假阳、量出下界**。
- **切片**：按 SCIP descriptor 分 `fn/type`（改动符号真实构成）vs `term/other`（字段/常量/通用短名污染）。

## 2. 结果

| 切片 | 符号数 | 加权假阳率 | 中位 | 均值 | >50% | ==0 |
|---|---|---|---|---|---|---|
| 全部 def | 442 | 42.9% | 0% | 18% | 13% | 57% |
| **★fn/type**（改动符号构成） | 349 | **38.9%** | **0%** | 15% | 10% | **57%** |
| term/other（字段/常量/短名） | 93 | 52.2% | 0% | 25% | 24% | 56% |

- **regex 漏召回 miss=2887**（≈9% of SCIP ref）：path 全覆盖、非 bug → **宏/derive 生成引用**（SCIP 抓到、regex 看不进宏展开）= SCIP 的召回优势；且令上表假阳**偏保守**（低估）。
- **fn/type 假阳 top**（SCIP 去噪价值最大）：`f`(83%)/`json`(99%)/`args`(98%)/`cfg`(94%)/`db`(76%)/`collect`(99%)/`finding`(78%)/`report`(90%)/`write`(89%)/`bundle`(75%)/`raw`(85%)/`call`(72%)——两类：① **与 stdlib/常见 method 同名**（collect/write/json/call/f，regex 分不清本仓 fn 与 `.collect()`）；② **redteam 域名词作字段/变量**（finding/report/bundle/provider/model/db/verdict）。
- **6f95c25 真实改动符号**：`build_pack` 0% / `reference_points` 0% / `classify_refute` 17% / `run_job` 50% —— 印证双峰：独特名 0、稍通用名 17-50%。

## 3. 解读

1. **双峰极化**：57% 改动符号（独特函数名）假阳 0，SCIP **零增益**；10% 假阳 >50%（撞 stdlib/域名词同名），SCIP **大幅去噪**。加权 38.9% 被少数高假阳大众名拉高（top 12 贡献 ~1550/2224 假阳）。
2. **cap 10 是关键放大器**：高假阳符号（regex 100-250 命中）在 S1 `per-name cap 10` 下，注入的 10 个引用点**大概率全是 stdlib/变量假阳**；SCIP 给真引用（scip=2-42），cap 10 后是真 caller。→ **精度切法的真杠杆 = 1 壳 cap 10 名额纯度，不是省 tool_use**（与北极星正交，印证 Q5 指标错配判断）。
3. **域名词高频命中**：finding/report/bundle/provider/model/verdict/db 假阳 75-99%——而这些**正是 redteam 评审最常改的核心域符号**。→ 对真实变更集，改动符号撞高假阳名的概率**不低**（偏向「有价值」）。
4. **但 ROI 不均匀**：57% 符号零增益，为 10% 符号的 cap 10 纯度上全套 SCIP（5s 索引 + scip 依赖 + 双查询表），单独作里程碑 ROI 存疑。

## 4. 定案 = 廉价过滤 B（已实现）

探针在写 plan 前就动摇了 **Q1「精度切法单独作第一刀」**的 ROI 前提（57% 符号 SCIP 零增益）。定案 **B**：不上 SCIP，给 S1 regex 1 壳加**注释/字符串剥离**；S2-a 独立 SCIP 里程碑撤，剩余 63% 假阳（method call / 属性宏 / 局部同名）随 **S2-b 契约分类**一次装 SCIP 白捡。

**B 收益验证**（各档过滤对 fn/type 符号）：

| 档位 | 假阳率 | vs raw 假阳降幅 | 安全性 |
|---|---|---|---|
| raw（S1 现状） | 39% | — | — |
| **B: 剥注释/字符串** | **29%** | **−37%** | ✅ 纯句法零误伤 |
| B+: 再剥 `.method` | 26% | −46% | ❌ 误伤本仓 `self.方法()` |
| SCIP（理想上界） | 0% | −100% | 需索引 |

**认领一个纠偏**：B 砍的**不是** stdlib method 黑名单（`collect`/`write`/`.json()` 是真 method-call 语法，剥注释/字符串砍不动，要 B+ 剥 `.method` 才行、而那误伤本仓真方法，不做）。B 真砍的是**注释/字符串里的同名**——因 redteam 是评审工具、代码注释/prompt 字符串满是域名词，这块意外大（`finding` 117→24、`bundle` 90→22、`db` 161→73、`json` 202→128）。

**实现**（已交付、352 测试绿 / clippy 净新 0 / make verify 绿）：`src/dossier/mod.rs::strip_rust_noncode`（Rust 词法状态机：`//` 行 + `/* */` 嵌套块 + `"…"`/`b"…"`/raw `r#"…"#`，内容替空格保行号）；`reference_points` 命中行剥离后无词边界匹配 → 剔（仅 `.rs`）。顺带删死代码 `is_doc`/`is_comment_line`/`[doc]`（B 用「剔除」取代旧「标注」，注释命中不再作引用点）。多语言词法 → **T38**；定位 = **T37**（regex 地板持续优化）首个兑现。

## 5. 产物

- `scip_probe/`（scratchpad）：scip crate dump 程序（Task 0 验证载体）。
- `probe.py`（scratchpad）：假阳率探针。
- `occ.tsv`（scratchpad）：37482 occurrence dump。
- 关联：设计 `../design/2026-07-08-redteam-context-management-s2a-design.md`；决策 E4。
