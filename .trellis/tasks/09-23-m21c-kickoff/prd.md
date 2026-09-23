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

## 待裁（开工前）

1. **切名（项 8）放最前还是最后？** 放最后 = roadmap 原序，稳；放最前 = 插件与 ship 立刻用上新引擎，
   日常 dogfood 产生的真实数据比继续打 PR181 这个固定靶子有价值（且 dev 二进制仍在、可随时对照）。
   代价：切名后若引擎回归，日常评审直接受影响。
2. **aacr smoke（项 7）是否仍是切名硬门？** roadmap 写「Line-Recall ≥ dev、Noise ≤ dev 1.5×，否则不切名」。
   但 aacr-bench 在外部仓、D7 B4 已暂退役、重启点待定；而今天已有 PR181 四轮 + oracle 口径可作对照。
   选项：① 维持硬门；② 降为可观测、切名门改用 PR181 口径；③ 推迟到 M21e 的多样性收口一并做。
3. **M21d 是否与 M21c 并行开？** 尸检指向 L4-B 三方契约注入才是覆盖的药；roadmap 本就写「M21d 与 c 并行」。
   并行代价 = 两条分支同时动 prompt / pack 面，冲突风险。
4. **库切换程序**（项 8 内）：`proj.db` 与 `proj-next.db` 的合并 / 改名 / 旧库只读复用，要在批 D 前定。
5. **判例前移的验收口径**（项 5）：roadmap 写「refute 调用数较 M21b 下降可观测」。M21b 三轮的复核触发是 24 / 22 / 24、
   驳回 0 / 2 / 1 —— 基数已经很小，「下降可观测」需要重新定义（按每轮触发数？按同一 PR 跨轮重复报同一 finding 的比例？）。

## 本任务的产出

1. 上面五条裁定 → 写进 decisions（D19）。
2. `.next/plans/2026-09-23-redteam-m21c-ledger-convergence-flip-plan.md`（锁定）：按裁定的批次展开到 Task 粒度。
3. roadmap M21c 条目补链 plan；本任务归档后逐批开 Trellis 子任务。
