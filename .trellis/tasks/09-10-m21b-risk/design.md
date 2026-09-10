# M21b 批A · 风险评分器 技术设计

输入：prd.md + `.next/plans/2026-09-10-redteam-m21b-risk-deep-queue-plan.md` §批 A + design v10 §3.3/§6 + P0 §15.4（`../private/research/2026-09-07-redteam-p0-validation-gate-research.md`）。

## 1. 架构与边界

```
src/risk/                     ← 新模块（批 A 全部新增逻辑的家）
  mod.rs        route(changes, root, ledger, git) -> RiskRoute
                H0 门槛 / R 分 / tier / budget_cells
  regions.rs    regions(changes, files) -> Vec<Region>   成区（主区/涟漪）
  signals.rs    path/struct/extapi/hist 四类信号 + 乘子 + 常量权重表
```

依赖方向（单向，无环）：

- `risk` 吃：`bundle` 解析产物（changes/files/diff）、`diffsym` def 块与配平、`db.rs`（台账 dismissed/upheld）、git（`git log` hist、`git grep -w` fan-in）、`pack` 文件外片段机器（fan-in 词边界搜索，复用 M21a 切片批 E-3）。
- `risk` 被：`cli::run_code_review` 阶段 0（plan_cells 前）调用；`redteam-next risk` 只读子命令调用。
- `risk` 不吃：provider / orchestrate / prompt —— 零 LLM，裸终端可回归。

边界（不碰）：`src/pack/region.rs`（批 B）、队列与派发（批 C）、verifier（批 D）。与批 B 无文件交集（A 只在 `src/cli/mod.rs` 接线段）。

## 2. 数据流与契约

```
diff (git diff <base>) 
  → bundle 解析（既有） 
  → risk::regions        hunk → enclosing def 块（diffsym）；无 def → hunk ±10 行 padded
      Region { file, ranges, kind: Primary|Ripple, signals: Vec<SignalKind> }
  → risk::signals        四类信号计分 × 乘子 → 每区 score
  → risk::route          score ≥ θ=1.5 且 ≥2 类 → H0；R = Σ_{H0} score / (1 + ln(1+代码行))
      RiskRoute { hotspots: Vec<Hotspot>, r: f32, tier: Tier, budget_cells_hint }
  → RunMeta.risk: RiskMeta { hotspots: u32, r, tier, budget_cells }（serde default，旧报告兼容）
  → --pack-only 一行打印 / redteam-next risk --base <b> JSON（供 risk_replay compare 吃）
```

- **常量即 spec**：权重/乘子/θ=1.5/切点 0.5/3.5/预算 4/12/32 帽 48 全部 `const`，一处权重表 doc 注释引 P0 §15.4（`2026-09-07-redteam-p0-validation-gate-research.md` §15.4 v4 定案）。零配置旋钮：无 `[review.risk]`、无 `--risk`〔D11-3〕。
- **RiskRoute 形状可扩展**（M21c hist 信号激活、M21d extapi 与契约联动校准）：`SignalKind` 枚举 + `Hotspot { score, signals[] }` 显式携带信号明细而非只给总分，下游无需重算即可按信号种分流。
- **hist 数据源**：`--dismissed <db>` 显式指定；缺省读 `proj-next.db` 自身（M21a 分库裁定）。PR181 对拍时传 dev 旧 `proj.db` 复现 P0 标定条件。时间守卫：台账记录必须严格早于作业时刻。
- **def 口径统一（A-1 实现债，本批偿还）**：`bundle::changed_def_names`（hunk 首行锚定 + `block_end`、含测试 def）与 `pack::outside_names`（全改动行 + `value_end` + 容器/测试排除 + 重锚定）两套合并为一套：分组侧改吃 `diffsym` 的 `all_defs` / `changed_defs` 原始产物，重跑分组回归。

## 3. 关键规则（照抄 P0 §15.4，不发明）

| 类 | 信号 | 权重 |
|---|---|---|
| path | 敏感路径 glob（auth·secur·crypt·payment·billing·config·setting·secret·token·permission·migrat·callback·webhook） | 1.0 |
| path | 新增行关键词类（deser/exec_sql/condwrite/timeout/concurrency/destructive/auth/numeric_bounds/error_paths/resource_lifecycle）；涟漪区看区体 | 每类 0.5，封顶 1.5 |
| struct | 签名行被改 1.0｜def 删除 1.0｜新 def 0.5｜公开面拓宽 0.5｜fan-in ≥5（`git grep -w` 跨仓、排本文件与测试、名须像符号）1.0｜区间改动行 ≥ 全 diff Q3 0.5｜新文件 0.5｜涟漪 1.0 | 见左 |
| extapi | 有 path 信号的区里外部包符号调用 ≥3（Go import / TS import / Rust use，排本模块与标准库） | 1.0 |
| hist | base 祖先 12 个月 fix·bug·hotfix·revert·regress ≥3 → 0.5、≥6 → 1.0；台账 upheld 1.0 / dismissed 0.5 | 见左 |
| 乘子 | 声明区 ×0.5；测试路径或 `#[cfg(test)]` 之后 ×0.3；文档/schema ×0.2 | — |

- H0：score ≥ θ=1.5 **且** ≥ 2 类信号；关键词类单独不成区。
- R = Σ_{H0} score ÷ (1 + ln(1 + 代码改动行))；切点 0.5 / 3.5〔D11-4 锁定；P0 §15.7 实测三分位 0.47/3.64 作回校参考，M21b 出口按截断数/台账落入数回校〕。
- `budget_cells(tier, cfg.max_cells)`：Auto → low/mid/high = 4/12/32，`min(48)` 硬帽；Fixed(n) 直用。

## 4. 回放回归门（A-5）

- **前置 bug 修正（脑暴已核实坐实，2026-09-10）**：
  1. `eval/risk_replay.py:148` `parse_diff`：removed 行内容以 `-- ` 开头被当 `--- ` 文件头吞掉（dels 少计）、added 行内容以 `++ ` 开头误当 `+++ ` 新文件头（污染 files）。修法：文件头识别仅在 hunk 状态机预期位置接受（`+++ ` 后必须紧跟 `@@` 或行首上下文），或等价的状态机守卫。
  2. `eval/e3_contract_ab.py:278` `"anchored": rng is not None and unanch == unanch` 恒真（`rng` 已被回退兜底重赋值）。修法：在 `anchor_quote` 失败分支记 `was_anchored` 布尔，表格列取真值。
- `risk_replay.py compare <replay.json> <rust-out.json>`：Rust `risk` 输出 vs Python v4 区级对拍，报区级 Jaccard 与 R 差。
- `redteam-next risk --base <b>`：只读子命令跑阶段 0 风险路由打印 JSON，不调 provider。
- 回放口径〔D11-3〕：PR181（私有 fixture，`.next/private/p0-raw/`，位于 `m21-review-engine` 工作树——本树跑时拷 `.next/private/` 或从该树取输入，数字落本树 `.next/private/m21b/risk/`）+ 本仓 oracle B（`eval/fixtures/` 入库；P0 §15.5 的 7 diff 子集本批导出）。
- 验收线：Rust vs Python 区级一致率、oracle 命中不低于 P0 基线（PR181 主裁决 4/4、并集 14/22）。R 容差 1e-6。

## 5. 兼容与迁移

- `RunMeta.risk` 为 `Option<RiskMeta>` + serde default：旧报告 resume 不破，`run_resume` 沿用原报告值不重算。
- `--help` 无新旗（除 `risk` 子命令自身说明）；`redteam-next` 不切名。
- 分组侧 def 口径统一是行为变更点：重跑 PR181 分组回归，零变化或预期内变化记录在案（基线 = 前置任务「小分量并池」合回后的 `feat/m21-review-engine`）。

## 6. 取舍记录

- **零配置旋钮**（D11-3）：θ/权重改动唯一通道 = 改常量 + 过回放门，杜绝调参漂移。
- **不跨文件涟漪**（P0 §15.6）：fan-in 已计入结构信号，跨文件涟漪按引用数爆炸；oracle B 漏项主因「改动函数之外的同文件交互点」也非跨文件可解。
- **Python v4 为对照真值而非 Rust 的 spec 来源**：spec 是 P0 §15.4 表格；对拍差异先疑 Rust 实现，但 Python 侧已核实的 parse_diff bug 说明真值自身也需修正后再作门。
- **e3_contract_ab.py 修复不重跑 E3 实验**：anchored 列只影响人工确认表标注，E3 结论（arm 对比）不依赖该列。

## 7. 运维/回滚

- 全部新增代码在 `src/risk/` + 两处小接线（cli / report 字段）；回滚 = revert 单分支 `feat/m21b-risk`，不影响 `feat/m21-review-engine` 其他能力。
- `risk` 子命令只读（不写 db、不调 provider），误跑无害。
- eval 侧修正（parse_diff / anchored）改变回放数字口径：修后先重跑一遍 Python 基线记录新数字（PR181 H0 区数可能因 dels 计数变化而变），再作 Rust 对拍基准——旧 P0 数字仅作参考线。
