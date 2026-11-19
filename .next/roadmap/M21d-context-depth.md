# M21d · 上下文深化

> 状态: ✅ 已交付（批 A / B / C0 / C 全部收口，2026-09-30）· 依赖 M21b（与 M21c 并行）· design §9.4 / §10.2 / §10.4 / §10.5 · 决策 D4, D5, D8
> 出口：三方契约注入产量可观测；brief 前缀 cache 不降；切片回退信号（R7）若触发则二跳 / `slice_min_bytes` 调优完成；
> 交付项 7 已结案为**不做**（2026-11-18 D22：2026-09-23 墙钟前提被推翻 + 质量/成本理由），只保留 `diff_bytes` 观测
> · plan `../plans/2026-09-29-redteam-m21d-context-depth-plan.md`（三批，2026-09-29 派生）· 决策 **D20**
> **本里程碑不背 PR181 那批覆盖缺口的指标**〔D20-2〕：逐条核对后，那五个漏检主题没有一条是三方库 callee 契约误用
> ——它们是「模型看着正确的代码写出别的缺陷」，属注意力 / 取样问题，已单独立项〔D20-3〕。

## 目标

补齐评审上下文的两个盲区：三方 callee 的契约，以及大文件内的注意力稀释；并把仓库简介放进稳定前缀。

## 交付项

| # | 项 | design | 落点 |
|---|---|---|---|
| 1 | L4-B 三方契约注入（Rust 先行）：`use` → crate → Cargo.lock 精确版本 → registry 路径 → `pub fn` + 上方 `///`；同名歧义带 crate 标签全给、cap；默认全注入（签名 + doc，函数体按需）；registry 缺失 → 降级「无契约」并披露 | §9.4 B | `src/pack`（callee 注入扩根路径）, `src/diffsym` |
| 2 | `needs{kind: api}` 联动：深挖 cell 带对应契约 + 函数体 | §9.4 C | `src/orchestrate/queue.rs`, `src/pack` |
| 3 | 风险信号「关键模块 + 外部 API 密集」校准：extapi 信号 v4 已随 M21b 入 `src/risk`，本项只做与三方契约注入联动的「关键模块」判定与 k 阈值校准（D5） | §3.3, §9.4 | `src/risk` |
| 4 | 仓库简介入稳定前缀：`.redteam/brief.md`（约 2K token）或复用 `--spec`；段位在系统协议之后、spec 之前；cache 回归 | §10.5 | `src/prompt/mod.rs` |
| 5 | 切片调优（slice 已在 M21a 默认落地）：按 §10.2 回退信号加二跳选项、校准 `slice_min_bytes`；可选合成 util 文件 LITM 实验（原 E2 形态，挂 `.doc/TODO` 既有条目） | §10.2 | `src/pack/mod.rs` |
| 6 | `anchor_window = ±N` 确定性后滤：落在改动行 ±N 外的疑点 / finding 降级或丢 | §10.4 | `src/positioning` |
| 7 | ❌ **拆大组 / 大格 —— 2026-11-18 否决（D22）**（2026-09-22 用户裁上调，原挂「aacr-bench 重启后再看」）：切分键 = 每格 diff 字节，先切引用计数最低的强边、切开的边进跨组索引；验收按「最慢 scanner 格墙钟」与 `close_ms / 总墙钟`，不按组数。依据：批 D dogfood 里 scanner 段占墙钟 55%、`close_ms` ≡ 最慢那一格、而 provider 门在途峰值只有 2（门宽 16 / 20）——拆格是把串行生成换成并行，在门这边零代价。明细与反事实表见 followup 同名条目。**2026-09-23 实测推翻墙钟前提**：同配置三轮里 qwen 每格输出恒在 11–23k token、与组大小无关（2 文件的小组也写 11–16k），拆组只会让它对每个子组各写一遍——最慢格不降、token 翻倍；deepseek 的输出才随规模走（拆后 155s → 约 45s），但它不在关键路径上。故交付项 7 **不挂墙钟验收**，只按上下文深度（每格 diff 字节、切片回退）与预算分配判；scan 墙钟另走「换慢家族 / 封输出上限」，见 followup | §3.2, §10.2 | `src/bundle`（`group` 切分）, `src/pack` |

## 不做

非 Rust 生态定位链（R10，进 M20 backlog）· D 层测试执行（M20 R6）· tree-sitter（不立项）。

## 验收

- **批 A 实测（2026-09-29）**：本仓批 A 范围注入 21 块 / 1 miss；`CARGO_HOME=/nonexistent` 对照 0 注入且不阻塞；
  PR181 全量回归通过（verdict 一致、findings 在带内、失败格 0；三组注入 0 / miss 0）。「版本相关 API 误用」一条**改判**〔D21〕——反事实对照显示名气大的 API 上模型先验已够，
  L4-B 的正面证明改挂「先验薄靶子」followup；签名级漂移归编译门。
- 三方契约〔D20-1 改验收对象〕：**本仓自评审**每 PR callee 注入数与 input 增量可观测；构造一例版本相关 API 误用
  （Cargo.lock 版本与模型参数知识不一致）被深挖抓到。~~PR181~~ 换掉的理由：PR181 实测 15 `.ts` + 10 `.go`、
  **零 Rust**，而交付项 1 是 Rust 先行、非 Rust 定位链在「不做」里 → 原验收在它上面永远产不出注入。
  PR181 继续作 Go / TS 侧回归靶子（verdict / findings 不劣化），但不背 L4-B 指标。
- **批 B 实测（2026-09-29）**：brief 缺失时 prompt 逐字节一致（e2e 按字节比对钉住）；
  稳定前缀的中和改为**与角色无关**（三表并集），brief 与 spec 两条路都过；
  窗口后滤先只观测——本仓一轮窗外 2 / 41（4.9%）。
- ~~brief 前缀：cache 命中率不降~~ → **cache 对照未单独跑**：批 B 把「两侧前缀逐字节相同」做成了
  可断言的用例（比命中率更直接地钉住前提），命中率本身随日常 dogfood 观察。
- 若 M21a A/B 或 dogfood 触发回退信号：二跳 / `slice_min_bytes` 调优后复跑 PR181 A/B，slice 命中回到 ≥ full。
- `anchor_window` 后滤：PR181 改动行外 finding 数可观测，不误伤 carried 项。

## 风险

- registry 路径在 CI / 他人机器缺失：必须降级不阻塞，且披露进 RunMeta。
- slice 切错 → 缺依赖类漏：三层兜底与小文件全文已随 M21a 落地；本里程碑只在回退信号触发时介入。
