# 批 C3 技术设计：深挖选项稳定性

> 输入：prd + plan 批 C3 节 + design v11 §6 规则 1 / 2 / 3、§7〔D2, D7, D11, D13, D15〕+ C1 / C2 dogfood 的方差归因与改前基线对。
> 定案：2026-09-20 `/grilling` 十二问（Q1–Q12），结论见 §11；权衡与被否见 decisions **D16**（本批新增节）。
> **不在本批**：scanner 源头手段产品化（samples / temperature）、变更集外 needs 的装包、类别权重启用、C2 评审帽外项（快照批量事务 / Semaphore / 并查集）、verifier。

## 1. 边界与文件

| 文件 | 动作 | 内容 |
|---|---|---|
| `src/orchestrate/queue.rs` | 修改 | `same_item` 改结构化（同文件 ∧ 同块）；取消 id 捷径；项内聚簇算 `peak_agreement` / `region_families`；分层排序键 `cmp_priority`；每文件保底一格；`QueueItem` 增 `block`、`risk_band`、`peak_agreement` |
| `src/orchestrate/blocks.rs`（新） | 新增 | 「文件 + 行 → 稳定块」：代码走 enclosing def、markdown 走标题节、其余退 hunk ±10 行；块 = `(start, end, key)` |
| `src/risk/regions.rs` | 修改 | `enclosing_decl` / `decl_block_end` 提 `pub(crate)`，**逻辑不动**（H0 成区行为逐字不变，`eval/risk_replay.py` 对拍） |
| `src/orchestrate/deep.rs` | 修改 | 入队时算块与风险档（`route.zones`）；五个观测标量；needs 入队计数 |
| `src/orchestrate/mod.rs` | 修改 | `run_scan_fanout` 在发疑点之后发**变更集内**的 needs（到达键 `needs:{path}:{lines}`） |
| `src/pack/region.rs` | 修改 | `render_notes` 去 confidence 数字；needs 项（无笔记）的证据段口径 |
| `src/domain/report.rs` | 修改 | `DeepMeta` 增五个标量（serde default） |
| `src/orchestrate/testdata/deep_serial_baseline.json` | 重采 | 笔记字节变 → 基线与模板哈希变，commit 写明原因 |

不碰：入口五态与到达键幂等（C2）、收场 drain 与在线额度、门与等门 FIFO、resume 队列头与未完判定、`run_deep` 的 `sink` 类型、H0 成区行为。

## 2. 稳定块（Q3）

**动机**：C1 实测按「报告行 min/max」合并把跨轮重合度从 25% 压到 13–21%——跨度本身每轮都不一样，身份就不稳。
def 边界来自代码结构，同一份代码跨轮恒定。

```text
block_of(file, line) -> Block { start, end, key }
  1. 代码（diffsym::lang_for(file) = Some）：
       enclosing_decl(lines, line-1, defs) → Some(di) → (di+1, decl_block_end(lines, di, defs)+1, key = "def:<decl 行去空白哈希>")
       None（不在任何 def 内，如 use 段 / 常量区）→ 退 3
  2. markdown（.md）：最近的 `^#{1,6} ` 标题行 → 该行起、到下一个**同级或更高级**标题前一行止；key = "md:<标题行去空白哈希>"
       文件开头无标题 → 退 3
  3. 其余（json/yaml/txt/无解析）：hunk 式退路 (line-10, line+10)，key = "pad:<起行>"
```

- `key` 而非行号做身份：同一 def 的起止行跨轮可能因上下文微移，`key` 取 decl / 标题行的文本哈希，位移不影响身份；`id` 仍按 `file:scan:start-end` 生成（形状不变、可读），
  但**合并判定用 `key`**（见 §3）。
- 块计算一次：入队时算好存进 `QueueItem.block`（serde default 空 = 旧快照，退回现有行为）。
- 代价（已接受）：单项变大 → reviewer 拿到整个 def 而非三行。pack 侧本就按区域切片（§10.2），证据质量由 `region::build` 的既有预算控制；护栏盯 findings 数。

## 3. 合并谓词与两轴共识（Q4 / Q10）

```text
same_item(a: &QueueItem, b: &EnqueueReq)（Scan | Needs）
  = a.spec.file == b.spec.file ∧ a.block.key == block_of(b).key      // 结构定项
（Rule 分支不变：文件 + 区间相交）
id 捷径取消：块 key 已经覆盖「同一区间」的情形，不再需要「区间逐字相同」的特例。

项内聚簇（每次并入后增量算，members 已存全量疑点）：
  clusters = 并查集(members, same_suspicion)            // quote 定共识，口径与 merge_suspicions 一致
  peak_agreement = max(簇内不同家族数)                   // 必挖轴
  region_families = |全项家族并集|                       // 排序轴
必挖 = kind == Primary ∨ peak_agreement ≥ 2              // 取代原 agreement ≥ 2
```

- **为什么必挖不用区域级**：实测若按区域并家族，必挖项从每轮 4 条涨到 7–8 个区域（PR181 18/23），必挖不受预算截断 → `maxCells` 失效。
- `QueueItem.agreement` 保留为区域级（= `region_families`，落盘与 finding 的 `suspicion_agreement` 继续用它，语义改为「这一块被几个家族碰过」，
  在 design §6 与 decisions 注明改写）；`peak_agreement` 新增字段（serde default 0 → 旧快照退回 `agreement` 判定）。
- **C1 决策改写**：「共识只经谓词、id 捷径不计共识」→「**结构定项、谓词定共识**」。C2 的到达键幂等、`members` 累积、往轮在途占位均不变。
- 与入口五态的交互：New / MergedPending / MergedInFlight / MergedSettled / MergedSkipped / Duplicate 六种结局不变；只有「哪些请求落进同一项」变了。
  Skipped 复位条件随必挖轴改为 `peak_agreement ≥ 2`。

## 4. 分层排序键（Q5）与文件多样性（Q6）

```text
cmp_priority(a, b):
  1. first_slot(a) vs first_slot(b)                     // 每文件保底一格；false(0) 在前
  2. kind                                                // Primary < Ripple < Suspicion < Needs（不变）
  3. peak_agreement                                      // 降序
  4. risk_band                                           // 降序：3 = score ≥ THETA(1.5) · 2 = ≥ THETA/2 · 1 = > 0 · 0 = 无区
  5. 项内疑点条数 |members|                               // 降序（烟量）
  6. is_code                                             // 代码在前（!(.next/ 前缀 ∨ .md 后缀)）
  7. max(confidence)                                     // 降序，末位 tie-break（跨轮不可比，只做同分裂解）
  8. id                                                  // 全序兜底
```

- `first_slot(it)` = 「本项是该文件的第一格」：`已派发同文件项数（InFlight | Settled）== 0 ∧ 本项是该文件 Pending 中按 2–8 键最高者`。
  随派发推进自然失效（该文件有格之后其余项回到常规序），drain 与 `pop_dispatch_where` 共用同一序。
- `risk_band` 入队时算：把块映射到 `RiskRoute.zones` 中**重叠且分数最高**的区；无重叠 → 0。H0 项直接用自身区的分。
  `route` 已传入 `run_deep`（C2-5 起 resume 也总是重算），不新增数据通路。
- **类别权重维持 1.0**（不启用）：两轮 L0 占比 59% vs 21%，标注自身在漂。
- **confidence 不再进乘积**：现行 `conf × agreement × 类别权重` 因权重恒 1、agreement 仅 1/2，实际退化为 confidence 排序，而它跨轮整体平移（中位 0.35 vs 0.45）。

## 5. needs 入队（Q8）

- `run_scan_fanout` 在格落盘、疑点发完之后，对 `kind = evidence` 且 **`path ∈ 变更集**（`bundles` 里出现过的文件）的 needs 逐条发 `EnqueueReq`：
  `origin = Needs`、`notes = []`、`ranges = parse_need_lines(lines)`（缺失 → 整文件退块）、到达键 `needs:{path}:{lines}:{指纹}`。
- 变更集外的**不发**，只在 `enqueue_reasons.needs_out_of_diff` 计数（两轮实测约 6–8 条/轮，路径 Jaccard 67%——信号不差，但 `pick_bundle` 会静默退回 `bundles[0]` 造成
  「文件名 A、证据 B」的错配包，装包归属另开批）。
- 排序：`peak_agreement = 1`、`region_families = 1`、`risk_band` 照算、烟量 = 同块 needs 条数；`kind = Needs` 天然排在疑点之后 → 预算有余才吃格。
- pack：needs 项无笔记（`QueueItemSpec::new` 已允许 `Origin::Needs` + 空 notes），走与 H0 同款的「区域独立审」，证据 = 块切片。

## 6. 笔记去 confidence（Q7）

`render_notes` 输出改为 `- {file}:{a}-{b}: {hint} — quote: {quote}`（去掉 `(confidence x.xx)`）。
依据 design §7：疑点以「一位评审者的笔记」注入、指令是「确认、驳回或补充」而非「验证以下」，括注理由即防锚定；E3 实证 flash 级自评置信与命中无关。
连带：reviewer prompt 字节变 → `template_hash` 变、退化零差基线重采（C3-2 一并做，commit 说明原因）。

## 7. 观测（Q11）

`run.deep` 增五个标量（serde default、为零不序列化）：

| 字段 | 定义 |
|---|---|
| `suspicions_arrived` | 经通道 / 回放到达并被接受的疑点数（`Duplicate` 不计） |
| `items_from_suspicions` | 收场时 `kind == Suspicion` 的项数（÷ 上一行 = 合并压缩比） |
| `files_candidate` | 队列项覆盖的文件数（全部项，不分状态） |
| `files_selected` | 派发过的项覆盖的文件数（InFlight ∪ Settled） |
| `max_cells_per_file` | 同一文件派发格数的最大值（**辅助门槛直接读它**） |

`enqueue_reasons` 增 `needs`、`needs_out_of_diff` 两个 key。逐格数组（`peak_agreement[]` / `region_families[]`）**不加**——可从 `DeepRun.items` + scan 报告离线复算。

## 8. 测试形状

| 用例 | 构造 | 断言 |
|---|---|---|
| 块身份稳定 | 同一 def 内两条不同 quote 的疑点，行号相差 20 | 同一项；def 起止行改变（前面插入若干行）后 `key` 不变 |
| 文档标题节 | markdown 一节内 5 条疑点 + 下一节 1 条 | 合成 2 项，节边界按同级标题切 |
| 无 def / 无标题退路 | `.json` 与文件开头 | 退 ±10 行块，不 panic |
| 结构定项 · 谓词定共识 | 同 def 内两条不相似疑点分属 alpha / beta | 一项；`region_families = 2`、`peak_agreement = 1` → **非必挖** |
| 必挖不膨胀 | 同 def 内同一条疑点被 alpha / beta 提 | `peak_agreement = 2` → 必挖、不受 drain 截断 |
| 分层排序 | 构造 peak / risk_band / 烟量 / 代码-文档 各维度单变量对 | 逐层单调；confidence 只在全等时裂解 |
| 每文件保底一格 | 文件 A 有 5 个高分项、文件 B 1 个低分项、K = 3 | 派发序 A₁ → B₁ → A₂ |
| 保底随派发失效 | A 已有 1 格在途 | A₂ 回到常规序，不再抢 B 的保底位 |
| needs 入队 | 变更集内 / 外各一条 | 内的入队且排在疑点后；外的只计数 `needs_out_of_diff` |
| 笔记无置信 | 任一疑点项 | 渲染串不含 `confidence`；模板哈希变更被基线捕获 |
| 观测标量 | 一轮多文件多疑点 | 五个标量与手算一致 |
| H0 回归哨 | `eval/risk_replay.py` 对拍 | H0 成区逐字不变 |
| 退化零差 | 单成员门宽 1 | 与重采后的基线同构 |

## 9. 兼容与回滚

- schema v4 不变；`QueueItem` 增 `block` / `risk_band` / `peak_agreement`（serde default），旧快照缺字段 → 退回现有行为（块按区间、band 0、必挖用 `agreement`）。
- 行为变化（有意）：同 def 的疑点合成一项（项数下降、单项变大）；必挖集按 `peak_agreement`（比现状更严）；排序不再由 confidence 主导；每文件保底一格；
  reviewer 笔记少一个数字（prompt 字节变）。
- 回滚 = 不合回；合回后 revert 该 merge。

## 10. 风险

1. **H0 被连带改坏**：只提 `enclosing_decl` / `decl_block_end` 的可见性，逻辑不动；`eval/risk_replay.py` 对拍 + H0 相关用例是哨。
2. **单项变大伤证据质量**：护栏（findings ≥ 11、代码格占比 ≥ 68%）盯着；若破，优先缩块（def → def 内 hunk 邻域）而不是回退身份。
3. **块解析覆盖不足**：`diffsym` 只覆盖部分语言，退路是 ±10 行——与现状等价，不会更差。
4. **保底一格摊薄热点文件**：热点文件的第 2、3 项仍按价值排在其他文件的第 2 项之前，只让出第一轮；`max_cells_per_file` 可观测。

## 11. 定案记录（2026-09-20，`/grilling` 十二问）

| # | 问题 | 决定 |
|---|---|---|
| Q1 | 目标函数 | A：主目标稳定性，产出设护栏（findings ≥ 11、代码格占比 ≥ 68%） |
| Q2 | 候选集是否进范围 | C：不进；另跑一次性 samples 2 测量轮，数据进 followup |
| Q3 | 项身份 | B：enclosing def，文档退标题节 |
| Q4 | 合并后共识 | C：两轴——`peak_agreement` 定必挖、区域级进排序 |
| Q5 | 排序键 | 分层键（peak → risk_band → 烟量 → 代码优先 → confidence 末位 → id）；类别权重不启用 |
| Q6 | 文件多样性 | D：每文件保底一格 + `max_cells_per_file` 可观测 |
| Q7 | 笔记 confidence | A：去掉数字，基线重采 |
| Q8 | scanner needs | B：只接变更集内；diff 外装包归属记账 C4 / M21d |
| Q9 | 验收门槛 | B：逐项 ≥ 40% + 单文件 ≤ 1/3 + 护栏；30–50% 边界带补第三轮 |
| Q10 | 合并谓词 | B：结构定项、谓词定共识；取消 id 捷径 |
| Q11 | 观测 | B：五个标量 + 两个 `enqueue_reasons` key；逐格数组不加 |
| Q12 | 拆分与边界 | C3-1…C3-4；六条明确不做 |
