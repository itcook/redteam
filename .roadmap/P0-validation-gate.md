# P0 · 验证门（先于任何实现）

> 状态: 🔨 · 依赖 — · design §16 E1 / E3 · 出口 = design v4 → 锁定 + M21 登记
> E2（lost-in-the-middle × 切片）需要 slice 实现才能 A/B，移至 M21d 作其验收门。

## 目标

用零 LLM 或快模型级成本，在动代码前回答三个决定架构成立与否的问题：风险规则能否命中慢模型独有的 findings、
疑点契约是否真的比 finding 契约召回高、预算函数该长什么样。

## E1 · PR181 回放（零 LLM）

- **产物**：`eval/risk_replay.py`（复用 `eval/harness.py` 的 materialize + 分桶框架，不引第三方依赖）。
- **输入**：PR181 diff（40 文件 +1750/−69，base 见 `.doc/reviews/2026-09-04-pr181-three-way-redteam-ocr-rpiv.md`）；
  三方 findings：GLM 6 条、DeepSeek 3 条、rpiv 8 条；另取本仓 proj.db 历史 dogfood 中 upheld 的 findings 作第二个小 oracle
  （D7 B7），看规则对它们所在文件 / 区域的命中率。
- **基准说明**：aacr-bench 暂退役（D7 B4），P0 不用它；重启点在 M21c 翻转前 `smoke`、M21e 收官 `gate`（见 README）。
- **步骤**：
  1. 实现 design §3.3 四类信号初版（路径/类别、结构、外部 API、历史），输出 H0 与 PR 级风险分 R。
  2. 先做 GLM ∖ DeepSeek 差集（慢模型独有 findings）。
  3. 计算差集落在 H0 热区内的比例；同时列出 H0 中无任何 finding 的「空热区」数（假阳面）。
- **裁决**：落中 ≥ 2/3 → 规则初版成立，进 M21b；否则修规则再回放，最多三轮。空热区比例过高（> 1/2）→ 收紧阈值。
- **顺带产出**：预算函数 `f(R, 规模)` 的形状（R5）：从 H0 大小与 R 的分布反推低 / 中 / 高三档 `max_cells` 起步值。
- **写回**：research 新节「E1 回放数据」。

## E3 · 疑点契约 vs finding 契约召回（快模型，低成本）

- **前提**：疑点模式 prompt 模板草稿（scratch，不入产品）；oracle = GLM 6 ∪ rpiv 8 ∪ Kimi 3 ∪ ocr 3 去重（约 10-14 条）。
- **方法**：DeepSeek v4 flash，PR181 同 bundle 同 focus，两种契约各 N=3。
- **指标**：疑点索引命中 oracle 数 vs finding 契约命中数；疑点总数与噪音比；output token；墙钟。
- **裁决**：疑点模式命中 ≥ finding 模式 → design §4 契约成立；反之回到 D6 重议采纳项 1。
- **顺带产出**：`c_hi`（R11）= 命中 oracle 的疑点置信分布下沿；疑点 schema 字段集（R4）：hint 归一化是否必要、
  category 枚举取哪几类。
- **写回**：research 新节「E3 契约对照」。

## grill 与锁定

- **必答项**（锁定前）：R5 预算函数形状、R11 `c_hi`、R6 `--risk all` 等价性的精确定义、R14 实现债路线
  （CellRecord 联合体 vs 疑点新表）。
- **可后置项**：R1 / R3 / R7 / R8 / R9 / R10 / R12 / R13 / R15（各归属见 `followup.md`）。
- **锁定动作**：design 状态头改「锁定」；派生 M21a-e plan 到 `.doc/plans/`；`.doc/roadmap/summary.md` 登记 M21
  并链回本目录；本文件状态 ✅。

## 出口条件

E1 与 E3 数据入 research → grill 必答项裁决入 decisions（D7+）→ design 锁定 → M21 登记。
