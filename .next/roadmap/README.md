# M21 评审引擎重构 · Roadmap

> 状态: v4（2026-09-09 并发形态 grill〔D13〕：design v10，M21a ✅；v3 2026-09-07 P0 出口〔D11〕；v2 2026-09-05 grill〔D7〕；
> 2026-09-15 M21b 批 C2 拷问〔D15〕design 升 v11，M21b plan 增批 C3）· 分支 `feat/m21-review-engine`
> · 工作树 `~/hobby/.worktrees/redteam/m21-review-engine`
> 权威框架: `../design/2026-09-04-redteam-review-engine-architecture-design.md`（v11，锁定）
> 证据 / 决策: `../research/2026-09-04-redteam-review-engine-architecture-research.md`（§14 aacr 事实）·
> `../decisions/2026-09-04-redteam-review-engine-architecture-decisions.md`（D1-D15）· P0 明细 `../private/research/2026-09-07-redteam-p0-validation-gate-research.md`
> 2026-09-07 起本目录连同上述文档自主 checkout `.doc/` 迁入分支随行区 `.next/`（`../README.md`）；含公司仓细节者在 `../private/`（gitignored）。
> 本目录独立于主 checkout `.doc/roadmap/`，只记本次重构；零散 polish 仍走 `.doc/TODO/`。

## 一句话

**scanner 疑点扫描（分组并行）+ 确定性风险路由 + 深挖优先级队列（reviewer 定向消费，预算随风险分）+ 疑点台账跨轮激活 +
跨家族并行 + verifier 流式复核。** 把「快 vs 好」挪到「钱 vs 质量」轴；慢模型的广度成本是钱与有界墙钟，不是线性时间。

词汇：角色 = scanner / reviewer / verifier（配置 = 模型下 `roles.scanner / reviewer / verifier`，`~/.redteam/config.json`〔D12〕）；阶段 = 快扫 / 深挖 / 复核。

## 铁律（每个里程碑都受约束）

1. **实证驱动**：机制上线必带观测仪表；A/B → 判决 → 转正 → 拆仪器（E7-E11 传统，SCIP 建了又拆是前车之鉴）。
2. **确定性优先**：排序 / 预算 / 分派 / 合并 / 截断在编排器；LLM 只做声明（疑点 / needs / finding / 判决）（C5，F5）。
3. **前缀稳定**：任何模板或段序改动过 cache 命中率回归（C3）；角色分化段只在尾部。
4. **新编排 pass 三标配**：注入互斥、证据段预算帽、读守卫（needs 二阶段三次事后补的教训）。
5. **契约对齐**：输出契约与 parse 字段级对齐；截断 / parse 兜底 per-backend 单点。
6. **翻转有对照**：分支硬切、无 `engine` 旗；对照基线 = `dev` 分支二进制并排跑同一 PR；切名前分支产物名 `redteam-next`，
   插件与 ship 循环继续用 dev 的 `redteam`，M21c 切名（D10）。
7. **无候补**：cell 重试耗尽即失败，进 coverage `failed` 分级；不自动换模型重跑（D7 A3）。
8. **提示词按角色分离**：共享头 + scanner / reviewer / verifier 角色尾，输出契约与 parse 字段级对齐；改模板必跑该角色的离线
   回归集，模板哈希入 RunMeta（D9）。

## 阶段与里程碑

| 编号 | 名称 | 一句话 | 依赖 | 状态 | 详见 |
|---|---|---|---|---|---|
| **P0** | 验证门 | E1 PR181 回放定风险规则与预算形状；E3 疑点契约召回；grill → design 锁定、M21 登记 | — | ✅ | `P0-validation-gate.md` |
| **M21a** | 疑点扫描地基 | 配置节（用户级唯一 `~/.redteam/`、角色列表、provider 在途上限）+ 引用图分组 + 正反向跨组索引 + **切片装包（scanner 默认 slice）** + 疑点契约 + 提示词共享头 / scanner 尾 + `--pack-only`；proj.db v4 一次重置 + cache 断点；出口 slice vs full A/B；分支硬切删除旧编排，`redteam-next` 仅 scanner 段 | P0 | ✅（2026-09-09 出口验收交付；两门槛改口径见 M21a 状态行） | `M21a-scan-foundation.md` |
| **M21b** | 风险路由与深挖队列 | 风险评分器 + 优先级队列（分派池、无候补）+ reviewer cell + 预算 / 必挖集 + verifier 流式段；finding 只由深挖产出 | M21a | ⏳ | `M21b-risk-deep-queue.md` |
| **M21c** | 台账·收敛·切名 | 疑点台账 + delta 激活 + 收敛三维 + coverage 分级 + 判例前移等小项；**切名前 aacr smoke**；`redteam-next` → `redteam` + dogfood | M21b | ⏳ | `M21c-ledger-convergence-flip.md` |
| **M21d** | 上下文深化 | L4-B 三方契约 + 仓库简介前缀 + anchor_window + 切片调优（二跳 / `slice_min_bytes`，按回退信号） | M21b（与 c 并行） | ⏳ | `M21d-context-depth.md` |
| **M21e** | 多样性与收官 | 第二家族 scanner + **aacr gate 三方对照（legacy / v2 / ocr）** + 校准收口 + 仪器拆除 + `--help`/文档 | M21c, M21d | ⏳ | `M21e-diversity-closeout.md` |

跨里程碑遗留、§17 残留归属、实现债 → `followup.md`。

## 依赖图

```
P0 ─▶ M21a ─▶ M21b ─┬─▶ M21c ─┐
                    └─▶ M21d ─┴─▶ M21e ─▶ 并入 M19 发布线
```

M21c 与 M21d 可并行（d 只依赖 reviewer cell 存在，不依赖台账）。

## 门禁（每个里程碑出口共用）

- `make verify` 绿，测试零回归；对照基线 = `dev` 二进制（不随分支演进，PR181 基线数字在 M21a 归档）。
- PR181 回放：墙钟与 findings 不劣于上一里程碑（数字见各里程碑验收）。
- cache 命中率不低于 dev 基线（dogfood L1 50% / Kimi 99%）；按 cell 记入 RunMeta，anthropic kind 三段界断点生效（M21a 起）。
- verifier refute 率入 RunMeta（M21b 起），供 `[review.verifiers] enabled` 的数据判决。
- 新旋钮同步进 `--help`（裸终端唯一在场文档，与代码同权）。

## 测试主场与基准（D7 B4）

- **主场**：PR181（连续性对照点：历史表已有 GLM 19 min / DeepSeek 5.3 min / ocr 三跑；网关真实度）+ 本仓 dogfood
  （增量轮、台账激活、收敛、ship 内循环）。
- **aacr-bench 暂退役**，fork `~/codes/aacr-bench`（`itcook/aacr-bench`）承载数据、评估器、`reviewers/redteam.py`、子集 JSONL；
  本仓只留 `eval/README` 与 `AACR_BENCH_DIR`。**重启点**：M21c 翻转前 `smoke`（5 正，legacy / v2 各 N=1，mock judge 取
  Line-P/R）；M21e 收官 `gate`（规模按 `--pack-only` spike 定，legacy / v2 / ocr，GLM judge，`--eval-rounds 3`，负样本 N=1）。
- 估算按 pack 字节 ÷ 3.5，不信网关 usage。

## 与既有账本的关系

- `.doc/roadmap/summary.md` 的 **M19 发布 1.0 后置于 M21e**：重构改 CLI 面（`--models` 退场、`--test-command`
  新增、配置搬到 `~/.redteam/`），`--help` 与 README 须在 M21e 定稿后再发。
- M20 backlog：R6 执行判（design §9.4 D 层）、google 后端（R17）、RPM 令牌桶（R16）、交互式配置（R19）不在本 roadmap。
- design 锁定后按规范派生 `../plans/2026-*-redteam-m21{a..e}-*-plan.md`（`.next/plans/`），本目录各里程碑文件是其范围与验收的上位定义。
  已派生：M21a `../plans/2026-09-07-redteam-m21a-scan-foundation-plan.md`（2026-09-08，六批 A-F + 出口，波次：A/B/C 并行 → D/E → F → 出口）；
  M21b `../plans/2026-09-10-redteam-m21b-risk-deep-queue-plan.md`（2026-09-10 派生，现为七批 + 出口，波次：A 风险评分器 / B reviewer 零件并行 →
  C 深挖队列与 reviewer 运行时 → C1 并发派发与分派池 → C2 scanner 与深挖时间重叠 → C3 深挖选项稳定性 → D verifier 流式段 → 出口；
  Trellis 任务 `m21b-{risk,review-primitives,deep,deep-concurrency,deep-streaming,deep-stability,verifier}`；原 aacr 接线批 2026-09-10 删、随 M21c smoke 做）。

## 状态图例

✅ 已交付 · 🟢 机制完成待验收 · 🔨 进行中 · ⏳ 未启 · 🧊 backlog · 🚫 作废
