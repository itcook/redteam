# M21d 批 B：稳定前缀与确定性后滤

> plan `.next/plans/2026-09-29-redteam-m21d-context-depth-plan.md` 批 B · design §10.5 / §10.4 · 交付项 4 与 6

## 开工调研改了两处范围理解

1. **B-1 比 plan 预想的小**：`src/prompt/templates/shared-head.md` 里 `<repo_brief>{{REPO_BRIEF}}</repo_brief>`
   **槽位已存在**，段位也正好在 `output_language` 之后、`spec_docs` 之前（design §10.3 要求的「系统协议之后、
   spec 之前」）。所有调用点现在传 `""`。剩下的只是：读 `.redteam/brief.md` → 帽 → 中和 → 填参数。
2. **B-2 的键被占了**：`review.pack.anchorWindow`（默认 30）今天是 `src/pack/region.rs` 的**禁真空窗半径**
   （队列项区间未被 def 覆盖时 ±N 并入切片），不是 design §10.4 说的后滤判定半径。

## 开工裁定（2026-09-29，用户裁）

| # | 裁定 | 被否 | 理由 |
|---|---|---|---|
| 1 | 后滤半径**新开一个键** `review.pack.findingWindow` | 复用 `anchorWindow` | 两个半径调参方向相反：装包半径大一点更安全（多给上下文），判定半径大一点更宽松（少过滤）。共用一个键，调任一方都要同时跑两套回归；现有默认 30 也是按装包调的 |
| 2 | **先只观测，降级默认关** | 降级默认开 / 直接丢弃 | 与 plan 的验收「PR181 改动行外 finding 数可观测」对齐：先拿真实分布再定阈值。直接丢 = 静默少报（D18 反对的那一类）——reviewer 可能说对了问题、只是 quote 锚到邻近旧代码 |
| 3 | 后滤**只挂 finding 侧**，疑点侧只计数不处置 | 两侧都做 | finding 是最终产出、直接进 verdict，位置噪音在这里才有害；疑点本就是「指针」，落在改动行外的疑点深挖时会看上下文，过滤它反而伤召回 |

## AC

**B-1**
- `.redteam/brief.md` 存在 → 进 `repo_brief` 槽，帽 8KB（约 2K token），超帽截断并披露 warning。
- **按不可信证据注入**：过与 notes / needs 证据同款的结构中和（防 XML 标签逃逸）。
- 缺失 / 不可读 → 与今日**逐字节一致**（空槽）。
- 每轮只读一次盘，不逐 cell 读。
- 离线基线（`verify_once`）不注入（既有注释已声明）。

**B-2**
- 新配置 `review.pack.findingWindow`（默认 30，0 = 关）。
- 每条 finding 算「到最近改动行的距离」，落在窗外的**计数并披露**；`carried` 项不计入（跨轮沿用的锚点本就会漂）。
- 降级行为（不计入 verdict）由开关控制，**默认关**；开了才改变产出。
- RunMeta 可观测：窗外 finding 数、窗口值。

## 验收
- B-1：本仓与 PR181 各一轮，cache 命中率不降；brief 缺失时 prompt 逐字节一致（用例钉住）。
- B-2：PR181 一轮，改动行外 finding 数可观测；carried 项不被误计。
- 每处行为改动配会转红的用例 + 变异检查；跨层接线配集成级用例（**首跑与 resume 两条形态各一条**）。
