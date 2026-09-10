# M21b 批A · 风险评分器 执行计划

前置（本脑暴已收口）：分组策略已裁选项 1（小分量同池装箱）——独立前置任务落地合回后，本任务 `git merge` 最新 `feat/m21-review-engine` 再开工；429 棘轮移批 C 前；预算门口径分析随前置任务回放。

## 顺序清单

### Task A-1 成区：主区与涟漪区  ✅（56f0397）
- [x] Add `src/risk/regions.rs`：`regions(changes, files) -> Vec<Region>`（hunk → enclosing def 块，复用 `diffsym` def 块与配平；局部 const 不算 def；无 def → hunk ±10 行 padded 只与 padded 相邻合并；相邻函数各自成区；`kind: Primary | Ripple`，涟漪 = 改动/新增/删除声明名在本文件 ≤4 处引用点的 enclosing def）
- [x] **def 口径统一**：`bundle::changed_def_names` 改吃 `all_defs`/`changed_defs`（`pack::outside_names` 本即同源）
- [x] 重跑 PR181 分组回归（基线 = 并池后 `pr181-packonly-pooling.txt`）：**有实测变化，已记录在案**（groups 4→3、edges 111→152、pooled 4→1、大格 11+12 → 28 文件；归因 = 全改动行/测试 def 入引用边名集；见 research §6）
- [x] 测试：三语言合成文件（def 块 / 无 def padded / 涟漪 ≤4 / 相邻不并 / 局部 const 不成区，7 测）
- [x] Commit `feat(risk): 成区——hunk→def 块、padded 兜底、主区/涟漪二梯队（design §3.3，P0 v4）`

### Task A-2 四类信号与评分  ✅（9fc0bbc）
- [x] Add `src/risk/signals.rs`：权重表 const + doc 注释引 P0 §15.4；path glob、关键词类（每类 0.5 封顶 1.5，涟漪看区体）、结构（签名/删除/新 def/公开面/fan-in ≥5/Q3/新文件/涟漪）、extapi（≥3 外部符号调用，import/use 解析排本模块与 std）、hist（git log 12 个月 + 台账，时间守卫）、乘子（0.5/0.3/0.2）
- [x] fan-in 词边界搜索 + `ref_worthy` 假名过滤（GENERIC_NAMES）
- [x] hist 台账读 `dismissed` 表 + `results` 内 upheld，`--dismissed` 指定、时间守卫（严格早于 before）
- [x] 测试：每类信号合成用例（8 测）+ 可注入 `ScoreEnv` 桩
- [x] Commit `feat(risk): 四类信号（路径/结构/外部API/历史）+ 乘子 + 常量权重表（P0 v4 定案）`

### Task A-3 H0 门槛、R 分与预算三档  ✅（c7b1b42）
- [x] Add `src/risk/mod.rs`：`route(...) -> RiskRoute { hotspots, r: f32, tier, budget_cells_hint }`；θ=1.5 且 ≥2 类；R 公式；切点 0.5/3.5；`budget_cells(tier, cfg.max_cells)`：Fixed 直用、Auto 查表 min(48)
- [x] 测试：θ 边界；单类不出 H0；R 公式；三档切点；Fixed 覆盖 Auto；帽 48（5 测）
- [x] Commit `feat(risk): H0 门槛（θ=1.5 且 ≥2 类）+ R 风险分 + 预算三档 4/12/32 帽 48（design §3.3/§6）`

### Task A-4 接线：阶段 0 调用 + 观测  ✅（b5d1ee1）
- [x] Modify `src/cli/mod.rs`：`run_code_review` 在 `plan_cells` 前调 `risk::route`；`--pack-only` 打印 H0 区数/R/档位一行；`run_resume` 沿用原报告值
- [x] Modify `src/domain/report.rs`：`RiskMeta`（serde default）
- [x] `review --help` 无新旗（零旋钮）
- [x] 测试：RunMeta 快照含 risk 段；`--pack-only` risk 行；空 diff → `hotspots=0 r=0 tier=low`
- [x] Commit `feat(cli): 阶段 0 风险路由接线——H0/R/tier/预算入 RunMeta 与 --pack-only（design §2）`

### Task A-5 回放回归门  ✅
- [x] **先修 Python 侧两 bug**（均坐实）：parse_diff 状态机守卫；`e3_contract_ab.py` anchored 取 `anchor_quote` 真值
- [x] 修后重跑基线：PR181 `R=4.336/H0=17`、oracle B `20/34 (0.59) / 29/34 (0.85)`——与 P0 记录逐位一致
- [x] `compare <replay.json> <rust-out.json>`（区级 Jaccard + R 差 + 未配对清单，`--tol` 超差退出 1）
- [x] `redteam-next risk --base <b> [--head <h>] [--dismissed <db>] [--before <ts>]`：读 `merge-base(base,head)...head` 与 head commit 内容（不依赖工作树），零 provider 输出 JSON
- [x] oracle B 导出 → `eval/fixtures/oracle-hobby.json`（38 条 / 7 diff sets）
- [x] 对拍（`.next/private/m21b/risk/`，门记录 `.next/research/2026-09-10-redteam-m21b-risk-replay-gate.md`）：PR181 R Δ=3.4e-08 / H0 17/17 Jaccard 1.000；oracle B 七 diff 全部 R Δ ≤ 4.6e-07 / Jaccard 1.000；oracle 命中 主裁决 4/4、并集 14/22（≥ P0 基线）
- [x] 台账口径勘误（P0 §15.1）：PR181 回放**不带台账**；dev 旧 `proj.db` + `--before` 守卫是 **oracle B** 侧口径
- [x] Commit `feat(eval): risk compare 子命令 + redteam risk 只读子命令——Rust 实现对拍 Python v4 回归门（D11）`

### Task A-6 收口
- [x] `--help` risk 子命令说明；权重表注释引 P0 §15.4 复核
- [x] 收口评审（core-stable 期不跑 dev `redteam` ship）：code-review 制品 `.rpiv/artifacts/reviews/2026-09-10_13-46-54_range-0f434df-4156796.md`——两镜片（subagent lane 故障 → `bg_delegate` inspect-only），🔴0 / 🟡8 全修（含 3 条安全项：git argv `--end-of-options`、读守卫、台账只读打开）/ 🔵4 / 💭5；修后全量复验（四绿 + PR181/oracle B 门 8 例全绿 + 分组零回归）
- [x] `--no-ff` 合回 `feat/m21-review-engine`（eaf8bb5；合回后全量四绿，清理批 B 遗留 unused import）

## 验证命令

```bash
cargo test && cargo clippy --all-targets && make verify
# 分组回归（A-1 后）：
#   PR181 --pack-only 对比 m21b/pr181-packonly-kinds.txt 基线（分种三键 + 组数 + cross_edges）
# R 对拍（A-3/A-5）：
#   redteam-next risk --base <b> > rust-out.json && python3 eval/risk_replay.py compare <replay.json> rust-out.json
```

## 风险文件与回滚点

- `src/bundle/mod.rs` / `src/pack/mod.rs`（A-1 def 口径统一）——动分组侧行为，靠 PR181 分组回归把关；异常即 revert A-1 commit。
- `eval/risk_replay.py`（A-5 双重身份：bug 修正 + compare 新增）——修 bug 单独 commit 或至少分块，对拍数字以修后基线为准。
- RunMeta 新字段 serde default，旧报告兼容无迁移。
- 回滚单元 = `feat/m21b-risk` 整分支，不合回不污染主线。

## task.py start 前检查

- [ ] 前置任务（小分量并池）已合回 `feat/m21-review-engine`，本分支已 merge 拉齐
- [ ] `.next/private/p0-raw/` 可达（从 `m21-review-engine` 树拷或直接引用绝对路径）
- [ ] implement.jsonl / check.jsonl 各含至少一条真实条目（子 agent 派发模式下）
