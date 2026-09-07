# M21d · 上下文深化

> 状态: ⏳ · 依赖 M21b（与 M21c 并行）· design §9.4 / §10.2 / §10.4 / §10.5 · 决策 D4, D5, D8
> 出口：三方契约注入产量可观测；brief 前缀 cache 不降；切片回退信号（R7）若触发则二跳 / `slice_min_bytes` 调优完成

## 目标

补齐评审上下文的两个盲区：三方 callee 的契约，以及大文件内的注意力稀释；并把仓库简介放进稳定前缀。

## 交付项

| # | 项 | design | 落点 |
|---|---|---|---|
| 1 | L4-B 三方契约注入（Rust 先行）：`use` → crate → Cargo.lock 精确版本 → registry 路径 → `pub fn` + 上方 `///`；同名歧义带 crate 标签全给、cap；默认全注入（签名 + doc，函数体按需）；registry 缺失 → 降级「无契约」并披露 | §9.4 B | `src/pack`（callee 注入扩根路径）, `src/diffsym` |
| 2 | `needs{kind: api}` 联动：深挖 cell 带对应契约 + 函数体 | §9.4 C | `src/orchestrate/queue.rs`, `src/pack` |
| 3 | 风险信号「关键模块 + 外部 API 密集」接入评分器 | §3.3 | `src/risk` |
| 4 | 仓库简介入稳定前缀：`.redteam/brief.md`（约 2K token）或复用 `--spec`；段位在系统协议之后、spec 之前；cache 回归 | §10.5 | `src/prompt/mod.rs` |
| 5 | 切片调优（slice 已在 M21a 默认落地）：按 §10.2 回退信号加二跳选项、校准 `slice_min_bytes`；可选合成 util 文件 LITM 实验（原 E2 形态，挂 `.doc/TODO` 既有条目） | §10.2 | `src/pack/mod.rs` |
| 6 | `anchor_window = ±N` 确定性后滤：落在改动行 ±N 外的疑点 / finding 降级或丢 | §10.4 | `src/positioning` |

## 不做

非 Rust 生态定位链（R10，进 M20 backlog）· D 层测试执行（M20 R6）· tree-sitter（不立项）。

## 验收

- 三方契约：PR181 每 PR callee 注入数与 input 增量可观测；构造一例版本相关 API 误用（Cargo.lock 版本与模型参数知识
  不一致）被深挖抓到。
- brief 前缀：cache 命中率不降；brief 缺失时行为与今日一致。
- 若 M21a A/B 或 dogfood 触发回退信号：二跳 / `slice_min_bytes` 调优后复跑 PR181 A/B，slice 命中回到 ≥ full。
- `anchor_window` 后滤：PR181 改动行外 finding 数可观测，不误伤 carried 项。

## 风险

- registry 路径在 CI / 他人机器缺失：必须降级不阻塞，且披露进 RunMeta。
- slice 切错 → 缺依赖类漏：三层兜底与小文件全文已随 M21a 落地；本里程碑只在回退信号触发时介入。
