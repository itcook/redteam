# 小分量与孤立文件同池装箱（M21b 前置，分组策略选项 1）

## Goal

`bundle::group` 装箱扩展：多文件小强分量不再各自成组，改为**原子单元**与孤立文件同池贪心装箱，压组数（PR181 7 组 → 预期 4-5 组，达标 ≤5）。2026-09-10 脑暴用户裁定（followup:148 选项 1）。

## Background

- PR181 出 7 组：3 大强分量 + 4 个小强分量（files=2-3、est 21-29K vs budget≈144K）各自成组 + 3 孤立装一池；组数随小分量数线性涨，每组一次 scanner 调用。
- 用户裁定选项 1（vs 选项 2「组数门槛 ≤ 强分量数+1」）：单一装箱规则、语义直白；代价「不相关文件同格」小分量场景可接受。
- 硬约束：小分量作原子单元进池（分量内文件不拆散）——「强边不跨组」哨兵不变量保持（09-10-grouping-stats-kinds 已立测试）。

## Requirements

- 小分量判定：多文件分量（含弱缝切出的子分量）`len ≤ 3` 且 `est ≤ budget_bytes/4` → 进池；`budget_bytes = None` 时保守不判（无预算信息，保持原行为）
- 池为单元级装箱：单元 = 孤立单文件 或 小分量整体；按单元首文件（目录，路径）排序；est 与 max_files_per_group 双帽贪心
- 观测：`GroupingStats` 加 `pooled_small_components: Option<usize>`（serde default），PR181 回放可查
- 不加用户配置旋钮（常量判定，plan Global Constraints）

## Acceptance Criteria

- [x] 合成测试：小分量+孤立同池、原子性（强边不跨组）、大分量不受影响、budget=None 照旧、池格双帽切分
- [x] PR181 `--pack-only` 回放：组数 7→4 ≤5、`cross_import=0` 保持、`pooled_small_components=4`；数字落 `.next/private/m21b/pr181-packonly-pooling.txt`
- [x] `cargo test` / `clippy` / `make verify` 全绿（clippy 明细 3 条存量、零新增）
- [x] commit 直提 `feat/m21-review-engine`（无子分支，与 grouping-stats-kinds 先例同）；followup:148 划掉、决策记 D14

## Out of Scope

- 预算门口径结论（随本任务回放数据一并观察，正式结论记 followup）
- 429 棘轮、批 A risk 模块本体
