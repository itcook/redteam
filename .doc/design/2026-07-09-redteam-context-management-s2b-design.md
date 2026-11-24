# redteam 上下文管理·S2-b 设计（双树+契约三桶 → 单树 MVP）

> 状态: 已交付→安乐死（E8-E，2026-07-13）· 机制曾合 dev（16323ed）；验收+needs 指标判 SCIP/1 壳无边际价值，引擎拆除归 M16（复活点=git 16323ed..5731ae0）。保留为考古。
> 决策过程/权衡/被否/substrate 实测 → `../decisions/2026-07-06-redteam-context-management-decisions.md` §E6。
> 承架构框架 `2026-07-06-…-design.md` §1-6（D4/D5/D6/D9/D13/D14）；差异账 → `../roadmap/mvp-delta-ledger.md`（S2-b 段）。
> 前身 S2-a（SCIP 引用精度）探针证伪作废；S2-b 是**第一个真把 SCIP 接进二进制**的里程碑。

## 0. 范围（MVP 边界）

第一个接 SCIP 的档，全套 D4/D5/D6/D9 过大 → 窄路径核心先。核心闭环 =
**分类器判 A/B/C → radius 深度分档 → SCIP references 填**，单语言 Rust。

| 件 | MVP | 缓去 |
|---|---|---|
| SCIP substrate 接线（跑 indexer + `scip` crate 解析 + symbol→references 索引） | ✅ 入 | — |
| 1 壳引用 regex → SCIP references（查无退 regex-A 符号级兜底） | ✅ 入 | — |
| 契约分类器 A/B/C 驱动方向性 radius | ✅ 入（窄） | — |
| 单树（工作树）+ 旧签名廉价取 | ✅ 入 | 双树→S2-c |
| callee 签名注入（1 跳不递归） | ✅ 入 | on/off→A-B |
| 双树 HEAD 全索引 + 删除符号旧 callers / 缓存基建 / caller 剪枝 / 传递深度+多样性排序 / 穷尽性 flag / build-gate / per-model 分叉 | 🧊 出 | S2-c·S4·分支E（见 ledger） |

## 1. Substrate（SCIP 接线）

- **引擎**：`rust-analyzer scip`（内建子命令、零侧载；热 4.6s / 冷 12s / 增量亚秒，E1/E4）。`scip` crate v0.9（免 protoc，Task 0 验，E5）解析。
- **何时何地**：review 前置，对**工作树**跑一次；输出 **tempfile**，解析完即删——**零仓库足迹**，不碰 `.redteam/`。
- **无缓存**：每 review 重跑全量（review 分钟级、4.6s 可忽略）。缓存基建（`.redteam/index`+`cache` LRU、双树 staleness D13/D14）缓 S2-c/S4。
- **索引单元 = 仓**：本仓单 cargo crate、一次调用覆盖全 crate；D21 per-project scoping 是 S3 的活。
- **降级**：探测 `rust-analyzer` 在不在（借，D16）。不在 → 回退 S1 regex pack 原样 + 一行提示（D15）。真触发点 = RA 没装（E4 已证 RA 容错、索引报错罕见）。
- **toggle**（`REDTEAM_PACK` 枚举化，现为 bool `0`=dossier 基线）：
  - 默认 = 当前 regex-pack **不变**（验收过前不翻默认，照 S1「同二进制 on/off」纪律）。
  - `scip` = 新引擎（SCIP references + 分类器 + callee 注入，带符号级兜底）。
  - `scip-strict` = 诊断臂，**关兜底**（纯 SCIP），供静态验收量纯 SCIP recall。

## 2. 符号解析（改动符号 ↔ SCIP symbol 连接键）

- **按位置对（唯一解）**：改动 def 的 def-start 行（`enclosing_def_start`，0-based）→ 该行的 SCIP **Definition** occurrence（role bit=Definition）→ 精确 symbol 串。Rust `fn`/name 同行，与 SCIP name-occurrence 行对齐、无偏移。
- **杀 `defs_same_name` 启发**：SCIP symbol 串位置唯一 → 同名不同符号天然分开；scip 路径下 `SymRefs` ambiguity 恒 1（regex 的 `[N defs share this name]` 标注消失）。
- **符号级 regex 兜底**：某改动符号在该位置**查无 Definition occurrence**（宏生成/derive/多行签名边角/SCIP 盲区）→ 该符号单独退回 regex-A `reference_points`，宁多勿漏；打一条**无声降级计数**（可观测，防 SCIP 覆盖率暗降）。
- **callee 侧符号**：改动 def 范围内的 Reference occurrence → target symbol；可指向外部 dep/stdlib（契约在索引里，E3）。

## 3. 契约分类器（A/B/C）

分类**纯新树 regex/diff，不碰 SCIP、不需旧树**（SCIP 只在 §4 radius 展开登场）。信号源 = `symbols_in_file` 的**捕获路径**：

- **A（签名面变）**：某 `+` 行本身匹配 def 正则（签名行进 diff）；**或** kind ∈ {struct/enum/trait/type/const/static/macro} 被触到 → **粗判 A**（字段/变体/方法增删都值传递、罕见+高价，不细抠）。
- **B（仅体变）**：符号只经**外围 def** 路径捕获（签名行没进 diff）。
- **C（装饰）**：hunk 剥注释/空白（复用 `strip_rust_noncode`）后无改动实体 → radius≈0。**MVP 缓精确 C-skip**（分桶只 {A,B}、注释改动归 B=1 跳 caller 安全过展；精确 skip 撞符号/hunk 粒度 → ledger #12）。

落点：`ChangedSym` 加 `bucket`（A/B）+ `def_line0`（0-based 新树 def 行=位置对连接键），`symbols_in_file` 两条捕获路径分别打标。
**已知天花板**：多行签名只改参数行（fn 名行未动）→ 误判 B → 缓 T40；D4 下 B 仍展 1 跳 caller=优雅降级非漏（ponytail 注释标明）。

## 4. Radius 模型（方向性，D9）

方向纯**结构不对称**、零标量权重。均 **1 跳**（传递缓 S2-c）。

| 侧 | 桶 | 展开 |
|---|---|---|
| **caller**（谁引用改动符号，SCIP references 替 regex grep） | A | 直接 caller **完整外围体**（预算封顶、近先填、溢出→省略坐标）——高危桶付 body 钱，杀「读 caller 查用法」 |
| caller | B | 直接 caller **片段**（当前 `RefHit` 表示，SCIP 来源） |
| caller | C | 不展 |
| **callee**（改动符号调了谁） | 全 | 改动 def 内 callee 引用 → **只注入签名**（1 跳不递归无体，D9「callee 要契约」）；外部 dep 打 `[dep]` 标、溢出先降级 |

A/B 的意义 = **caller 厚度**（非深度）：既让分类器有牙、honor「caller 要 body」、又躲闭包爆炸。
输出复用 S1 pack 三段（0 壳/1 壳/省略清单）；1 壳的 `SymRefs.hits` 由 SCIP references 填、A 桶 caller 提升为外围体。

## 5. 验收门

- **主 = 静态、零 LLM**（三件）：
  1. **recall delta** = `SCIP_refs \ regex_refs`（SCIP 抓到、regex 漏的 caller）——tool_use 杠杆代理（S2-a 假阳探针的**补集**；探针已见 miss=2887 宏/derive，量其中真 caller 占比）。
  2. **分类器分布**：真 diff 集跑 A/B/C，报桶分布 + 抽查（A 是否真对应签名变）。
  3. **callee 注入产量**：每 bundle 注入几条 callee 签名 + 抽查是否正是模型会 search 的。
- **端到端仅「静态显著才跑」**：`scip-pack` vs S1 `regex-pack`，对**附四**（S1 A/B 基线）做差，**附三 OFF 不重跑**（成本纪律）。先单模型 DeepSeek 探方向，只下门决策才 3 模型×N=3。
- **门判据（D25 锁，co-equal 双门）**：残余 tool_use **robust 下降** **且** agreement **不向假共识收窄**。
- **要盯点**：A-caller 给全体 + callee 注入都**加 input token** → input 可能升而 tool_use 降（正是「多给前置、省补读」的赌注）→ 端到端**同时**量 input 与 tool_use；D25 独立性门在此更吃紧（A-caller 全体灌更多共享上下文）。

## 6. 差异账

完整 vs MVP 逐项 → `../roadmap/mvp-delta-ledger.md` M15-S2-b 段（11 项，缓去 S2-c/S4/分支E/T40/A-B）。

## 7. 关联

- 架构框架 `2026-07-06-…-design.md`（§1 blast radius / §2 三桶 / §5 索引引擎）；决策 §E6。
- roadmap M15 staging；前身 S2-a 作废存档 `2026-07-08-…-s2a-design.md`；探针 `../research/2026-07-08-s2a-false-positive-probe-research.md`。
- 下一步：落 S2-b plan（writing-plans）。
