# M21b 批A：风险评分器——成区/四类信号/H0+R/预算三档 + risk_replay 回归门

## Goal

`src/risk/` 新模块把 P0 E1 v4 定案规则移植为 Rust（成区主区/涟漪二梯队、四类信号常量权重、H0 门槛 θ=1.5 且 ≥2 类、R 风险分与预算三档 4/12/32 帽 48），接进 review 阶段 0 与 RunMeta，`redteam-next risk` 只读子命令 + `eval/risk_replay.py compare` 作回归门。零 LLM、零配置旋钮（无 `[review.risk]` / `--risk`，D11-3）。

## Requirements

- 成区：hunk → enclosing def 块（无 def → hunk ± 10 行 padded）；主区（含改动行）与涟漪区（改动/新增/删除声明名在本文件 ≤ 4 处引用点的 enclosing def，不跨文件）〔D11-1〕
- 四类信号与权重 = P0 §15.4 定案常量（path / 结构 / extapi / hist，含乘子），一处权重表注释引 P0；hist 读 proj.db dismissed/upheld 带时间守卫
- H0：score ≥ θ=1.5 且 ≥ 2 类信号，关键词类单独不成区；R = Σ_{H0} score ÷ (1 + ln(1+代码行))；切点 **0.5 / 3.5**（D11-4 锁定；P0 实测三分位 0.47/3.64 作回校参考）；`budget_cells`：Auto → 4/12/32 min 48、Fixed 直用
- 接线：`run_code_review` 阶段 0 调用，H0/R/tier/budget 入 RunMeta；`--pack-only` 打印一行；resume 沿用原报告值
- 回归门：`risk_replay.py compare`（Rust vs Python v4 区级对拍）+ `redteam-next risk` 只读子命令；回放口径 = PR181（私有 fixture）+ 本仓 oracle B（入库 fixture，缺则本批导出），数字进 research
- 前置核实（followup「批 A 重做评审子 agent 提出、未核实项」）：`risk_replay.py` parse_diff 列首 `--`/`++` 疑似缺陷、`e3_contract_ab.py` `anchored` 恒真式——先核再作门
- 任务级明细（文件落点/测试/commit 点）：`.next/plans/2026-09-10-redteam-m21b-risk-deep-queue-plan.md` §批 A（Task A-1…A-6）

## Acceptance Criteria

- [ ] `cargo test` / `clippy` / `make verify` 全绿；改动只在批 A 列出的文件与测试
- [ ] 权重/乘子/θ/切点全部为代码常量（无配置面），doc 注释引 P0 §15.4
- [ ] R 公式对拍 `risk_replay.py` 同 diff 同 R（容差 1e-6）；PR181 回放区级一致率与 oracle 命中不低于 P0 基线（主裁决 4/4、并集 14/22），数字落 `.next/private/m21b/risk/` 并写 research
- [ ] RunMeta 含 risk 段（hotspots/r/tier/budget_cells）；`redteam-next risk --base <b>` 裸终端可跑、不调 provider
- [ ] 收口：dev `redteam` ship 评审通过 → `--no-ff` 合回 `feat/m21-review-engine`

## Notes

- design.md / implement.md 不预写：开工时 grill 或脑暴产出（以本 prd + plan 批 A 节 + design §3.3 为输入）。
- 决策依据：D11（R5-a/b/c/d、R14）、D7 B7；design v10 §3.3 / §6 规则 3。
- M21c/M21d 接口：`RiskRoute` 输出形状需可扩展（M21c 台账 hist 信号激活、M21d extapi 与契约联动校准）。
