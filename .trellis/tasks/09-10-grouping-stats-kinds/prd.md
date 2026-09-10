# GroupingStats 分种计数——cross_edges 按边种分列、weak_cut 口径修正

## Goal

修正 `GroupingStats` 两个观测口径缺陷（followup 实现债：M21a 批 A/B/C 合并核验发现、批 F 延后项 + 实现债两行登记，M21b
前置清单 #3），让 R8（LLM 语义分组何时值得开，M21e 判）的触发度量「跨组 import 边数」可读、切缝数不失真。**只改观测，
不动分组逻辑**；在 `feat/m21-review-engine` 本工作树直接实现（用户 2026-09-10 指定，不进 M21b 子批）。

## Background（已确认事实，代码证据）

- 边三种（`src/bundle/mod.rs:44-47` `EdgeKind`）：`Import`（强，有向）、`Reference`（强，有向）、`SameDir`（弱缝，无向存一次）；
  `is_strong()`（`:65`）= 非 SameDir。
- **缺陷 1**：`cross_edges`（`bundle/mod.rs:1371-1375`）= 端点异组的边数，混计三种边。SameDir 在同目录文件间两两存在，
  同目录文件被分到不同组即灌满计数；design §3.2 给 R8 定的触发度量是「跨组 **import** 边数」。
- **缺陷 2**：`weak_cut`（`bundle/mod.rs:1330`）= 超限分量内全部非强边数，在强边子分量切分**之前**计数——含切分后两端
  仍在同组的边，虚高（followup 定案：「只计真切的边」）。
- **恒等揭示（实现分析，2026-09-10 勘误：Reference 带次数）**：`EdgeKind::Reference { count }`，count ≥ 2 才强、
  = 1 为弱（`bundle/mod.rs:44-48` K3）→ 弱边 = SameDir + count=1 Reference。且强边（Import / 强 Reference）在现算法下
  端点永不异组（强连通必同强子分量；落单单文件强子分量与其他文件无强边）→ 跨组边全部是弱边。
  修正口径的 weak_cut（端点异组弱边）= `cross_reference + cross_same_dir`，独立信息量为零。
- 序列化：`GroupingStats` 无 `rename_all`，JSON 键 = Rust 字段名（snake_case），随 `RunMeta.grouping`（`report.rs:266`）进
  `run.grouping.*`，也随 `--pack-only` 的 `PackOnlyReport.grouping` 进 summary.json；派生 `Deserialize`（resume 回读原报告）。
- 消费面：`render_pack_only`（`cli/mod.rs:678-681`）打印 `grouping gate=… edges= weak_cut= cross_edges= …` 一行；测试
  字面量断言 4 处（`bundle/mod.rs:1675`、`cli/mod.rs:1870`、`report.rs:344`）+ 构造 3 处（`bundle/mod.rs:1177` empty_stats、
  `:1302` 单包、`:1379` 主流程）；eval 脚本零消费；无外部脚本。
- 单包直达路径（门未过）stats 全零，不受影响。

## Requirements

1. **字段形状（用户 2026-09-10 批：打平三标量 + 保留总数）**：`GroupingStats` 顶层加 `cross_import` / `cross_reference` /
   `cross_same_dir: usize`，均 `#[serde(default)]`；`cross_edges`（总数）保留。恒等式 `cross_edges == cross_import +
   cross_reference + cross_same_dir` 作测试断言。
2. **weak_cut 处理（Key Decision，用户已批；理由勘误见上）**：修正口径后 weak_cut = `cross_reference +
   cross_same_dir`（现算法强边不跨组），独立信息量为零 → **删除该字段**，JSON 不再有此键（旧 JSON 反序列化忽略未知键，
   resume 兼容；报告 v4 未发布仅 dogfood）。「弱边跨组数」由 cross_reference + cross_same_dir 求和可得；未来前置清单 #1
   引入切强边后，cross_import / 强引用跨组自然有值，届时再按需细分。`--pack-only` 打印行与全部测试字面量同步。
3. 计数实现：`cross_*` 三分种在现 `cross_edges` 计算处（`group_of` 建好后）按 `EdgeKind` 分列；删除 `:1312-1330` 处
   切分前计数。
4. `--pack-only` 打印行：`edges= weak_cut= cross_edges=` → `edges= cross_edges= cross_import= cross_reference= cross_same_dir=`。
5. 测试：①import / reference / samedir 跨组分种正确（同目录对被拆 → 只进 `cross_same_dir`；count=1 引用被拆 →
   `cross_reference`）；②恒等式断言 `cross_edges == cross_import + cross_reference + cross_same_dir`；③**不变量断言**：
   强边不跨组（cross_import == 0 ∧ 跨组 Reference 全为 count=1——算法回归哨兵，前置清单 #1 改分组时此断言会红并迫使
   显式确认）；④旧格式 JSON（无新字段、含旧 `weak_cut` 键）round-trip 成功。
6. 收尾：M21b plan「开工前置清单」#3 划掉；followup 实现债行（`cross_edges` / `weak_cut` 口径）与批 F 延后项行标 ✅。

## Acceptance Criteria

- [x] `cargo test` 全绿（579 passed）、`cargo clippy --all-targets` 零新增（存量 3 处在 diffsym/output/main，非本次文件）、`make verify` 绿
- [x] 分组结果零变化：现有分组测试的分组期望值全部不动，仅 stats 断言更新（PR181 对比 groups=7 edges=111 isolated=3
      over_budget=2 与 M21a 出口完全一致）
- [x] `run.grouping` JSON：新三键在场、`weak_cut` 键消失、`cross_edges` 总数在；旧格式 JSON 反序列化成功（单测锚定）
- [x] `--pack-only` 输出行含分种字段
- [x] PR181 `--pack-only`（`.next/private/m21b/pr181-packonly-kinds.txt`）：`cross_import=0 cross_reference=2
      cross_same_dir=6`（旧 cross_edges=8 → 75% 是同目录拆的、25% 弱引用；旧 weak_cut=55 为虚高，真切口径 8）；
      **cross_import=0 复验「强边不跨组」不变量（该值对任意输入恒为零，非 PR181 特有）；语义分组增量（无 import 边的语义关联）不在此度量可观测范围，M21e 判 R8 仍需跨组索引与 dogfood 数据**（2026-09-10 评审 Q5 措辞修正：原文「R8 直接证据」系过度解读）
- [x] followup 两行 + plan 前置清单 #3 收尾更新

## Out of Scope

- 分组算法 / 切分策略（前置清单 #1 分组策略议定另行）
- `edges`（全图总边数）分种（R8 只需 cross import）
- 分组预算门与装包口径对齐的结论（前置清单 #2；本任务 PR181 跑数只供数不做结论）
- `GroupingOpts` / eval 脚本 / M21b 各批任务范围

## Technical Notes

- 改动文件：`src/bundle/mod.rs`（结构 + 计数 + 测试）、`src/cli/mod.rs`（打印行 + 测试字面量）、
  `src/domain/report.rs`（测试字面量）；无其他文件。
- 兼容：新字段 `serde(default)`；删 `weak_cut` 对旧 JSON 无碍（`GroupingStats` 无 `deny_unknown_fields`）。
- 回滚：单 commit，revert 即恢复。
