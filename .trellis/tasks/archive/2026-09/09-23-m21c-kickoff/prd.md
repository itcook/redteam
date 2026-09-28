# M21c 开工：定案与批次划分

> 上位：roadmap `.next/roadmap/M21c-ledger-convergence-flip.md`（九项 + 验收 + 风险）· design `.next/design/2026-09-04-…-design.md` §4 / §8 / §9 · 决策 D6、D10、D13、**D18**
> 前提：M21b ✅（2026-09-23 出口验收〔D18〕机械面全过 + 覆盖面记账），集成分支 `feat/m21-review-engine` HEAD `65e4b55`
> 数据纪律：PR181 读数与明细只落 `.next/private/`，公开侧只留汇总数字。

## 从 M21b 带进来的新事实（影响 M21c 的范围与期望值）

1. **覆盖缺口不在扫描召回、不在预算，在深挖的「转化」**（2026-09-23 尸检，四轮报告 + oracle 22 条锚点逐条比对）：
   漏掉的主题里，T6 / T7 / T8 / T12 的区域**已经在队列里、格也跑了**，模型在同一段代码上写出了别的缺陷；
   T11 是唯一的结构性缺口（区域从未入队）；T10 是量尺问题（同机制、锚点偏 10–15 行，严格重叠不认，已修）。
2. **⚠️ 需要修正的期望**：开工前我（Claude）曾说「判例前移是覆盖缺口的药」——**复核后不成立**。
   判例前移（交付项 5）治的是**收敛与噪音**（不再重报已 dismissed、跨轮沿用结案），它不教模型发现新的缺陷类型。
   对着尸检看，更可能治覆盖的是 **M21d 的三方契约注入**——T7（`mapSafeErrorCode` 与 `mapErrorToCode` 分叉）
   本质就是两个函数的契约分叉，正是 L4-B 要注入的东西。M21c 的目标应回到「收敛 + 可日常用」，不背覆盖指标。
3. **`maxCells` 回校已结案**〔D18-3〕：维持 `auto`(high 32)，抬到 64 不换覆盖（墙钟 +53%、token +66%、主题 6→5）。
   → 交付项 1「预算外疑点落账」的价值不在「补回被截的覆盖」，而在**跨轮延迟处理**与噪音控制，口径要按此写。
4. **配置已定稿**（永久基线 sha256 `76a5af70…`）：双 reviewer（glm + deepseek）、deepseek scanner medium / reviewer high、
   qwen scanner low、agc 门宽 6。墙钟 13–16 min（scan 段 7.5 min 占一半，瓶颈是慢家族的**恒定输出**，与组大小无关）。
5. **followup 里属于 M21c 的债**（已记，实施时顺带）：5xx 拿不到 `Retry-After`（需改 `ProviderError` 形状）；
   `cfg!(test)` 缩退避只覆盖 lib 单测（换 `tokio::time::pause()`）；resume 报告复用格不入深挖计数、`close_ms` 恒空；
   verifier 无修复轮无原文落盘；`thinkingLevel 不一致` warning 对 reviewer 是错的；verifier 轮转仍会挑中同模型；
   `REDTEAM_DUMP_DIR` 这个 dev 仪器发布前拆除。

## 批次划分（草案，待裁）

| 批 | 内容（roadmap 项） | 依赖 | 备注 |
|---|---|---|---|
| A | 台账表 + 预算外疑点落账（1）、coverage 五集合与角色分级（4） | — | 都在 db / output / orchestrate，形态独立 |
| B | 判例前移进快扫 prompt + 跨轮沿用结案守卫（5）、小项打包（6） | A | 收敛主线 |
| C | 增量轮激活（2）、收敛三维 + 活锁守卫（3） | A、B | ship 协议消费新字段 |
| D | 切名 + 库切换 + 插件 / ship 指向（8）、dogfood 内循环（9） | A–C | 出口 |
| — | aacr smoke（7） | 外部仓 | 见待裁 2 |

## 裁定（2026-09-23，写进 decisions D19）

1. **切名提到最前，作为批 0**——引擎已过机械面验收、dev 二进制保留、回退只需改插件 bin 指向；日常 dogfood 的数据比继续打 PR181 有价值（其 oracle 有 9 条取自 dev 自己的产出）。
2. **aacr smoke 降为可观测、移交 M21e**；切名门改用仓内口径：PR181 一轮机械面复现 + 全量测试绿 + `make verify` 绿。不拿一个没在跑的外部基准卡住切名。
3. **不开双分支并行**；批序改为 批 0 切名 → 批 A 台账落账（与 M21d 的 L4-B 并行）→ 批 B 判例前移（等台账有素材）→ 批 C 收敛三维 → 批 D dogfood。两条理由：判例前移要有素材；覆盖缺口的药是 L4-B 而非判例前移。
4. **库切换**：导出旧 `proj.db` 固定集 → 归档只读 `legacy-proj.db` → `proj-next.db` 改名 `proj.db`；不做自动 schema 迁移；`--dismissed` 指旧库作一次性复用。
5. **判例前移验收改口径**：主判据 = 两条构造用例；观测量 = 免入队 / 免重挖项数 + 跨轮重复报同一 finding 的比例。撤销「refute 调用数下降」（基数 24/22/24、驳回 0–2，落在噪声里）。

## 本任务的产出（已完成）

1. ✅ 五条裁定 → decisions **D19**（`b4ad358`）。
2. ✅ `.next/plans/2026-09-23-redteam-m21c-ledger-convergence-flip-plan.md`（锁定，批 0 + 四批到 Task 粒度）。
3. ✅ roadmap M21c 补链 plan、改出口与两处验收口径；M21e 接手 aacr smoke（`342dfb8`）。
4. 下一步：开批 0 的 Trellis 任务 `m21c-flip`（库切换 → 切名 → ship 协议 → 切名门 → 收口）。
