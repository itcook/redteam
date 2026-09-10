# M21b 批A：风险评分器——成区/四类信号/H0+R/预算三档 + risk_replay 回归门

## Goal

`src/risk/` 新模块把 P0 E1 v4 定案规则移植为 Rust（成区主区/涟漪二梯队、四类信号常量权重、H0 门槛 θ=1.5 且 ≥2 类、R 风险分与预算三档 4/12/32 帽 48），接进 review 阶段 0 与 RunMeta，`redteam-next risk` 只读子命令 + `eval/risk_replay.py compare` 作回归门。零 LLM、零配置旋钮（无 `[review.risk]` / `--risk`，D11-3）。

## Background（脑暴核实与裁定，2026-09-10）

- **前置疑点已核实坐实（followup「批 A 重做评审子 agent 提出、未核实项」），A-5 内先修再作门**：
  - `eval/risk_replay.py:148` `parse_diff`：removed 行内容以 `-- ` 开头（SQL/Haskell 注释等）被当文件头 `--- ` 吞掉（dels 少计、removed 丢行）；added 行内容以 `++ ` 开头误当 `+++ ` 新文件头（污染 files 字典）。
  - `eval/e3_contract_ab.py:278` `"anchored": rng is not None and unanch == unanch` 恒真，且 `rng` 在该行前已被回退兜底重赋值（永非 None）→ anchored 列恒 True（本意应记「quote 真锚定而非回退行号」）。修后不重跑 E3 实验（结论不依赖该列）。
- **开工前置裁定**：① 分组策略 **选项 1（用户裁）**——小分量（文件数 ≤ 3 且估算字节远小于预算）与孤立文件同池装箱（代价：不相关文件同格，可接受；强边不跨组不变量保持）；实现为批 A 开工前独立前置任务，合回 `feat/m21-review-engine` 后本分支 merge 拉齐，A-1 def 口径统一在并池后基线上跑分组回归。② 预算门口径对齐：`m21b/pr181-packonly-kinds.txt` 已有数据显示估算 bytes vs pack_bytes 系统性偏高 1.2–4.8x，结论随前置任务回放验证。③ 429 棘轮移批 C 开工前裁定（与批 A 零文件交集；PR181 无 provider_rate_limit 观测，数据源实为 `.next/private/d13/` dogfood）。④ GroupingStats 分种已偿（09-10-grouping-stats-kinds）。
- **fixture 现状**：oracle B 不存在（`eval/fixtures/` 仅 3 个台账 jsonl），A-5 按预设导出 P0 §15.5 本仓 7 diff 子出入库；PR181 私有数据在 `m21-review-engine` 工作树 `.next/private/p0-raw/`（curated/oracle/replay 齐备）。

## Requirements

- 成区：hunk → enclosing def 块（无 def → hunk ± 10 行 padded）；主区（含改动行）与涟漪区（改动/新增/删除声明名在本文件 ≤ 4 处引用点的 enclosing def，不跨文件）〔D11-1〕
- 四类信号与权重 = P0 §15.4 定案常量（path / 结构 / extapi / hist，含乘子），一处权重表注释引 P0；hist 台账经 `--dismissed` 指定（缺省 `proj-next.db` 自身，M21a 分库裁定）带时间守卫，PR181 对拍传 dev 旧 `proj.db` 复现 P0 标定条件；def 口径统一：`bundle::changed_def_names` vs `pack::outside_names` 两套，复用时一并统一 + 重跑分组回归（followup 实现债）
- H0：score ≥ θ=1.5 且 ≥ 2 类信号，关键词类单独不成区；R = Σ_{H0} score ÷ (1 + ln(1+代码行))；切点 **0.5 / 3.5**（D11-4 锁定；P0 实测三分位 0.47/3.64 作回校参考）；`budget_cells`：Auto → 4/12/32 min 48、Fixed 直用
- 接线：`run_code_review` 阶段 0 调用，H0/R/tier/budget 入 RunMeta；`--pack-only` 打印一行；resume 沿用原报告值
- 回归门：`risk_replay.py compare`（Rust vs Python v4 区级对拍）+ `redteam-next risk` 只读子命令；回放口径 = PR181（私有 fixture）+ 本仓 oracle B（入库 fixture，本批导出），数字进 research；Python 侧 bug 修后先重跑基线记录新数字再对拍
- 任务级明细（文件落点/测试/commit 点）：plan §批 A（Task A-1…A-6），执行清单见 `implement.md`，技术设计见 `design.md`

## Acceptance Criteria

- [ ] `cargo test` / `clippy` / `make verify` 全绿；改动只在批 A 列出的文件与测试
- [ ] 权重/乘子/θ/切点全部为代码常量（无配置面），doc 注释引 P0 §15.4
- [ ] R 公式对拍 `risk_replay.py` 同 diff 同 R（容差 1e-6）；PR181 回放区级一致率与 oracle 命中不低于 P0 基线（主裁决 4/4、并集 14/22；以 parse_diff 修后新基线复核），数字落 `.next/private/m21b/risk/` 并写 research
- [ ] RunMeta 含 risk 段（hotspots/r/tier/budget_cells）；`redteam-next risk --base <b>` 裸终端可跑、不调 provider
- [ ] def 口径统一后 PR181 分组回归：零变化或预期内变化记录在案（基线 = 小分量并池合回后）
- [ ] 收口：dev `redteam` ship 评审通过 → `--no-ff` 合回 `feat/m21-review-engine`

## Out of Scope

- M21b 前置任务本体（小分量并池实现、预算门口径结论、429 棘轮裁定）
- 台账与增量轮激活（M21c）、三方契约注入（M21d，reviewer pack 留空槽）、第二家族 scanner（M21e）、判例变体触发（D-2 预留枚举槽）
- 跨文件涟漪（P0 §15.6 已裁不做）；快扫档独立途径（将来）
- E3 实验重跑（anchored 列修复只影响人工确认表标注）

## Notes

- 决策依据：D11（R5-a/b/c/d、R14）、D7 B7；design v10 §3.3 / §6 规则 3；P0 §15.4/§15.5/§15.6/§15.7。
- M21c/M21d 接口：`RiskRoute` 输出形状需可扩展——`Hotspot` 显式携带 `signals[]` 明细（M21c 台账 hist 信号激活、M21d extapi 与契约联动校准），见 design.md §2。
- 制品：`design.md`（架构/契约/取舍/回滚）、`implement.md`（A-1…A-6 顺序清单/验证命令/风险点）已随本脑暴产出。
