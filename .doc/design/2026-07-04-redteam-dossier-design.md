# redteam 卷宗（dossier，支柱 2）设计——决策态

2026-07-04。分支 `feat/dossier`（基 dev @ d4475db，含 M13 全量）。状态：grilling 定案，待拆 plan。

上游：
- five-pillars 支柱 2：`.doc/research/2026-07-03-cost-redesign-five-pillars.md` §3.2 / §6。
- 查全率评测（穷人版 vs rust-analyzer LSIF）：`.doc/research/2026-07-04-dossier-recall-eval.md`。
- 前置里程碑 M13 增量协议（本设计吃它的 delta / 抄它的穿线）：
  `.doc/design/2026-07-03-redteam-incremental-rounds-design.md`。
- grilling 记录：本文 §决策即 grilling Q1-Q9 的固化。

---

## 目标（一句话）

fan-out 前，二进制为每 bundle 产一份**纯事实卷宗**（改动符号的仓内引用点），注入该 bundle
的所有 cell，把「每个 cell 自己 grep callers/callees」这一步一次性做掉、三模型共享——
cell 工具轮数 12 → 3-6，与 M13 轮次增量正交相乘（~2-2.5×），对抗判断语义分毫不动。

## 核心原则（承 five-pillars）

**事实与判断分离。** 事实（谁调谁、改动符号被用在哪）确定性、共享无害、只获取一次；判断
（这段代码是否有问题）才是对抗价值、值得花 N 个模型。三个法官看**同一份事实**卷宗，对抗
独立性不受损——只要卷宗装事实不装观点。

---

## 决策（grilling Q1-Q9 固化）

### D1 · 分段读不并入（Q1）
支柱 3 的「强制分段读」与卷宗**零代码共享**（分段读在 `src/tools/mod.rs::do_read_file` 的返回值，
卷宗在 bundle→prompt 注入），只是同一关注点非同一段代码。踢出本设计树，降级为独立 mini-patch
（TODO 已挂）：`do_read_file` 已支持 `start`/`end` 行段，只需补「无 range 且文件超阈值 → 返回
大纲 + 请指定行段」。无缓存对冲，独立可发。属 token 轮廓类变更，M13 live 冻结窗口禁动。

### D2 · v1 内容 = 仅引用点（Q2）
five-pillars 列的三类事实，v1 **只装 ②引用点**（changed symbol 的仓内引用点：位置 + 头行/签名 + ±3 行）。
- **砍 ① hunk 完整块**：`{{BUNDLE_DIFFS}}` 已带改动代码 + ±3 行 context，git hunk header
  （`@@ … fn foo`）白送「改的是哪个函数」；要看整函数未改部分是 `read_file` 一次调用的事，
  预注入每个 hunk 完整块会顶爆预算（评测 §4.4）。
- **砍 ③ 文件大纲**：与分段读（D1）天然重复，且大半是与改动无关的符号。
- **留 ② 的唯一理由**：里程碑目标「轮数 12→3-6」那 12 轮正是烧在自己 grep callers/callees；
  引用点替 cell 做掉这步即达标，①③不贡献该目标（YAGNI）。

### D3 · 每轮重建，吃 M13 delta；引用点是 blast-radius 证据非新靶（Q3）
- **每 (轮, bundle) 重建一份**卷宗（grep、零 LLM、确定性，重建极廉价）。「只建一次」= 一次
  fan-out 内 3 模型共享，非跨轮共享。
- **符号输入 = M13 已算好的 delta**（`changed_files_since` 产物）；r1 时 delta = 全变更集，
  r2..rn = fix-delta。卷宗引擎**不自己算 delta**。
- **邻域 = 内容增强，永不改评审范围**：引用点（callers）是本轮改动的 blast radius，作**事实**
  摆出；能提的 finding 仍受 M13 管辖——必须是 **delta 引起的**（「本 fix 改了 X 契约、打断 caller Y」），
  引用点是证据、成因是 delta。**划线靠 prompt 守**（见 D8）：判「delta 有没有打断这点」（在范围），
  不是「审这点有没有旧 bug」（出范围）。

### D4 · v1 语言 = Rust + Go + TS/JS；非支持语言无损降级（Q4）
- 引擎两半：**改动符号提取**（语言相关，per-language def 正则 + 括号取 enclosing scope）+
  **引用点搜索**（语言无关，`rg -wF` 词边界）。
- v1 带三语言提取器。bundle 里非支持语言的文件 → 不产卷宗条目（prompt 层空渲染，退回今天现状，
  零回退）= five-pillars 的「无 grammar 语言 grep 兜底、行为无损降级」。
- Dogfood 近期扩到 Go/TS 仓（hiapi/backend：Go 206K + web JS/TS 115K），故 v1 即带、不等。

### D4a · Go/TS 完成度与验收门（Q4a）
- **Go**：机制近 Rust 最安全，def 正则 3-5 条，陷阱面比 Rust 还小；枢纽名（`init`/`TableName`）
  归歧义标注旋钮。基本照抄评测原型。
- **TS 提取洞现在补全**：评测 §6 明指「类方法 / 对象字面量方法未覆盖」——web 主干构造，
  v1 补齐正则（非语义级洞，纯工程活）。default export 改名（1.0% 发生率）归模型精读义务，不补机制。
- **oracle 复跑（scip-go/scip-typescript，半天量级）= 诊断非门**：查全率漏检不是正确性事故
  （降级到「自己 grep」= 今天现状，不丢 finding），真兜底是四判据 + live A/B（D9）。oracle 留作
  live 失望时定位工具。

### D5 · 架构 = LangSpec 数据表 + 一个通用提取器（Q5）
评测毙了 scout 与 tree-sitter，grep 是唯一后端——「可插拔槽位」后端维塌成单点。三语言提取
**算法完全相同**（def 正则 + 括号配平取 scope），只有正则集不同，是**数据非行为**。

```
struct LangSpec { def_patterns: &'static [&'static str] /* 正则 */ }
// v1 三语言 scope 全花括号系，无需 scope_style 字段；Python 来时才补 Indent（YAGNI）
const RUST: LangSpec = ...; const GO: LangSpec = ...; const TS: LangSpec = ...; // ts/tsx/js/jsx 共用

fn lang_for(path: &str) -> Option<&'static LangSpec>          // 按扩展名派发，None = 降级
fn extract_symbols(diff: &str, spec: &LangSpec) -> Vec<Symbol> // 一个算法
fn reference_points(symbols: &[Symbol]) -> Vec<Entry>          // 语言无关，rg -wF
```
- **不建 trait**（无动态分发需求，全编译期）；**不建 scout/tree-sitter 后端槽位**（永远填不满）。
- 加语言 = 加一个 LangSpec 条目（无损降级、纯增量的干净兑现）。将来若真上 tree-sitter，
  是提取层重写（§5：语言数 >3-4 的可维护性论据，非查全论据），属未来。

### D6 · 注入（2-B）：抄 M13 `{{INCREMENTAL_ROUND}}` 穿线（Q6）
- **新增 `<dossier>` 兄弟块**，置 `<bundle>` 之后（阅读流：先 diffs 后 blast radius），
  `{{DOSSIER}}` 占位符。
- **空渲染纪律（M13 同款）**：非支持语言 / bundle 无改动符号 / 卷宗关闭 → 渲染空串，零行为变化。
  整块（wrapper + 散文 + 条目）由 `render_dossier_section` 渲染，空时整块消失。
- **per-bundle 一份，3 cell 同注入**（同 `{{BUNDLE_DIFFS}}`）；与 diff **构造上去重**
  （卷宗 = 引用点 callers，diff = 改动 hunk 本身，无重叠）。
- **静态注入**（初始 prompt 就带，非 tool 拉取）——省掉 12 轮 grep 的往返，是本支柱的命根。

### D7 · 预算 + 标注（Q7）
沿用 `src/bundle/mod.rs::cap_entries` 已确立的双帽 idiom（条目数 + 字符预算，先到者截断，尾注被截数）。
- **两级帽**：per-name **cap 10**（>10 引用点留前 10 by (file,line)；截掉的恰是 `new`/`run` 类
  高频低信名，选哪 10 不敏感）+ per-bundle **≤8K 全局兜底**（cap 不咬时的独立防线，超预算按符号
  整条丢、尾注被丢数）。
- **字节估算，零 tokenizer**（同现状 `e.len()`）：卷宗 model-agnostic（一份注给 3 个不同 tokenizer），
  预算是软护栏。字符常量按 ~8K tokens 保守标定（bytes/3.5，多为 ASCII + 少量 CJK 注释），
  **文档记 bytes/token 假设**，正式实现校常量一次（兑现评测 §5「换真 tokenizer 校帽值」，校常量不引库）。
- **四条标注串**（英文，遵 devtool 骨架惯例）：

  | 触发 | 标注串 |
  |---|---|
  | per-name cap 咬（>10） | `+N more, K defs same name, heuristic` |
  | 歧义（defs_same_name>1） | `K defs share this name (incl. trait dispatch), heuristic` |
  | doc 注释命中 | 条目前缀 `[doc]`（52% 良性上下文，标形态保留） |
  | 全局预算丢弃 | `… and N reference points omitted for budget` |

- **不预抽泛型**：卷宗预算与 `cap_entries` 是同 idiom 不同格式（多行、换行拼接、标注不同），
  复制 ~15 行 idiom 给卷宗自己的格式，出现第三个真调用者再抽（ponytail）。

### D8 · 惰性保真措辞（Q8）——prompt-only，A/B 兜底
无机制能强制「模型真核实了」；惰性若发生靠 D9 的 A/B 事后抓（finding 坍缩/agreement 收窄）。
- **`<dossier>` 块散文**（`render_dossier_section` 内，三句压四层：is-what+heuristic / blast-radius 非 audit / 读穿 / 核实义务不减）：
  > The entries below are where this change's changed symbols are used elsewhere in the repo —
  > its blast radius — found by word-boundary search (heuristic facts, not judgments; renamed or
  > dynamic uses may be missing). Judge whether **this change** breaks these sites; do not audit
  > them for pre-existing defects this change does not cause. The ±3-line excerpts are a starting
  > point — read the full file at any site you must judge, and search yourself for uses you suspect
  > are missing; your duty to verify is undiminished.
- **`<tools>` 第 17 行和解（Option B，加一条件从句，空卷宗时 vacuously 成立）**：
  > …read the surrounding files, callers, and callees yourself before judging. **Where a dossier of
  > reference points is provided below, start from it and search only beyond what it lists.** Do not
  > invent code you have not read.
  （硬留「自己 grep callers/callees」会与卷宗打架——模型照做去 grep、卷宗白建。）
- **整体 prompt 重排延后**（TODO）：重排 = token 轮廓类变更，与卷宗同批毁 A/B 单变量归因；
  属 M13 live 冻结窗口排除项，解冻后与 ship.md 协议类 TODO 同批各自 A/B。因卷宗而生的编辑
  （`<tools>` 和解、`{{DOSSIER}}`）在 M14 内，段序重排出 M14。

### D9 · A/B 监控 + 验收门（Q9）
- **「监控」= 2-C 一次性 A/B 实验，非常驻子系统**：`Finding.agreement:u32` + `models` 已落
  result.json（已确认），A/B = 卷宗 on vs off 两份 result 对 diff，**零新仪器**。
- **红线**：agreement 分布**不显著收窄**（不向共识锐化——喂同一份卷宗致 3 模型趋同 = 独立性塌缩）。
- **双重身份搭 M13 live（一石二鸟）**：2-C 的 ship 会话，**off 臂 = M13 live ≥50% input 降验收**
  （M13 效果验收本就延后并入本批），**on−off delta = M14 验收**。一次 live 落两个里程碑。
- **三道门**：
  - 成本（input_tokens/耗时进一步降，M14 ~2-2.5×）：不达标 → **调参**（cap/内容/措辞），不 revert。
  - 四判据（真 finding 不丢 / 误报不增 / id 稳定 / reappeared 零）：回退 = **硬门**，卷宗在 steer 判断，合 dev 前必改。
  - 独立性（agreement 分布不收窄）：**硬门**。
- **闭 five-pillars §6 老问题「agreement 分母变化对 refute 触发的影响」**：M13 已把 refute 改成
  只打 new，卷宗不改 agreement 计算；其对 refute 的影响是间接的（仅当 shift 了 agreement 分布），
  而那 shift 正是本 A/B 要抓的——不需单独机制，A/B 一网兜全。

---

## 实施批次（承 roadmap 主线）

- **2-A 卷宗引擎**（纯新模块，hermetic，零耦合）：LangSpec 表 + extract_symbols + reference_points
  + 两级帽/四标注（D2/D4/D4a/D5/D7）。可脱离 LLM 单测（喂 diff 出卷宗字符串，断言查全/预算/标注）。
- **2-B 注入接线**（prompt/orchestrate/bundle，走 M13 Task 10 `{{INCREMENTAL_ROUND}}` 同款穿线；
  需 M13 已合 dev——已满足）：`render_dossier_section` + 模板 `{{DOSSIER}}` + `<tools>` 和解 +
  吃 M13 delta 做符号输入（D3/D6/D8）。
- **2-C 自带 A/B 的 live**（D9）：以 M13 live 后成本结构为新基线；ship 会话双重身份。

## 非目标 / 不做清单（承 five-pillars §5 + 评测）
- 不上 scout / tree-sitter（评测对 Rust 无对价，两盲区同盲）；不追精确调用图（卷宗=定位提示，判断归模型）。
- 不上 embeddings/RAG；不做宿主侧编排（全落二进制）。
- v1 不覆盖语义级盲区（同名歧义靠 cap+标注治理不消除；宏生成标识符/dunder 隐式调用/计算名/
  运算符分发 = 模型精读义务，tree-sitter 亦不解）。

## 泳道纪律
- 2-A 收尾 + 2-B 收尾各跑 `/redteam:ship --base dev` 增量循环——实施批次本身即 M13 live 收编素材。
- 收编 session 期 dev **冻结 token 轮廓类变更**（观测类补丁安全；卷宗注入/分段读启用不行）。
