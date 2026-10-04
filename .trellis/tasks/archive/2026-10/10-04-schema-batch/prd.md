# B 类 schema 改动包

## Goal

四条挂着「攒够一批 schema 改动一起做」的债合批。攒够了（到期看板标的 ⚠️）。

## 开工时的分野（与账上假设不同）

账上把四条都当「schema 改动」，核下来**只有 B-1 真动 sqlite schema**：

| | 实际动的 | 要不要迁移 |
|---|---|---|
| B-1 `first_job`/`last_job` 改名 | `suspicions` 表的列名 | **要**（`CREATE TABLE IF NOT EXISTS` 不改已存在表的列名） |
| B-2 `last_verdict()` tie-breaker | 只改一行 SQL | 不要——`results` 不是 WITHOUT ROWID 表，隐式 `rowid` 就是插入序 |
| B-3 `Finding` 带队列项 id | 报告 JSON 的 serde 字段 | 不要（`#[serde(default)]` 向后兼容） |
| B-4 `CellRecord.attempts` 拆分 | 报告 JSON 的 serde 字段 | 不要（同上） |

所以不翻 `USER_VERSION`（除非 B-1 裁成要改名）。`SCHEMA_VERSION` 按既有先例不翻
（批 B 的 v4 字段全 default 不序列化时就定过这个口径）。

## B-2 已落地

`last_verdict()` 原本 `ORDER BY created_at DESC LIMIT 1`，而 `created_at` 是
`strftime('%Y-%m-%dT%H:%M:%SZ')`——**只到秒**。同秒落盘的两轮在排序上并列，谁被选成「上一轮」
由 sqlite 的扫描顺序决定，而激活闸门正是按这条的 verdict 开关的。
改为 `ORDER BY created_at DESC, rowid DESC`，不动 schema。

## B-4 已落地（价值最高的一条）

账上说 `attempts` 混三件事，核下来是**五件**：探索轮 / FINALIZE 收尾轮 / 修复轮 /
**二阶段补查的全部轮次** / 429 重试。第四件是 `deep.rs:1208` 的 `c.iterations += c2.iterations`
——二阶段被累加进首轮的 `iterations`，再进 `attempts`。

落地形态：

- `iterations` 语义**收窄**：探索轮 + 收尾轮 + 修复轮，**不再含二阶段**
- 新增 `repair_rounds` / `two_stage_rounds` / `rl_retries` 三个分量（`CellRecord` 与 `CellSummary` 各一份）
- `attempts` 保留为**总数** = `iterations + two_stage_rounds + rl_retries`，doc 订正
  （原 doc 写「候补链实际尝试的 model 数」——那是 M21a 退场的候补链留下的，实现早就在写轮次和）
- md 的 cell 行把非 0 分量标出来：`(3 att: 1 repair, 1 2nd-stage)`，不再只给一个总数

`repair_rounds` 的取值在 scanner / reviewer 两侧都是 `iterations - 1`，**不是估算**：
两条路径的 `iterations` 都只有 1（首轮即成）或 2（修复后成）两个构造点。

切名门「修复轮为 0」从此可自动化——此前 PR181 那轮 24 格 `attempts > 1` 被误判成修复轮，
实际全是二阶段补查，靠 `needs_two_stage_triggered` 恰好等于 24 才辨清。

## B-1 已裁并落地：`first_seen_sha` / `last_seen_sha`（2026-10-04 用户裁）

债写的改名方向是错的

账上写「改名为 `first_round` / `last_round` 与对外一致」，理由是「对外经
`DormantLead.first_round` 转成 round，那个名字反而准」。**这个理由不成立**：

- `cli/mod.rs:564` 是 `first_round: r.first_job`——**直接赋值，没有任何转换**
- 存的是 HEAD sha（`output/mod.rs:145` 取前 9 字符当 short sha 显示；测试夹具是 `"sha0"`）
- `DormantLead.first_round` 的 doc 自己写着「第一次见到它的那轮 **head_sha**」——**名字和注释打架**

所以照债改 = 把一个不准的名字换成另一个不准的名字，还要付 DB 迁移的代价。
ship / commands 不读 `first_round`（grep 无命中），改对外字段的风险低。

**裁定：一次改到准**。DB 列 + Rust 字段 + 对外 JSON `DormantLead.first_round`
全改成 `first_seen_sha` / `last_seen_sha`（62 行）。迁移用
`ALTER TABLE suspicions RENAME COLUMN`，幂等（先查列在不在），挂 `open_at`——
与三态遗留行同理：这类库已是 v4 且表齐全，`setup_schema` 根本不跑。
**次序约束**：改名必须早于任何按新列名发的 SQL（包括 `normalize_legacy_activated`）——
用例里 `suspicion_upsert` 用的就是新列名，它能跑通就是这条次序的哨兵。

## B-3 已落地（改了账上的实现方式）

账上写「让 finding 带上产出它的队列项 id」。改成**记在项上**：`QueueItem.produced: Vec<String>`。
理由：给 `Finding` 加字段要改 43 处构造点（vs 项侧 12 处），还把归因细节泄进**对外报告 JSON**；
而 `promote_dug_suspicions` 的入参本来就是 `items`。信息量一样。

回填点在 `land`——那是唯一同时拿得到「项」与「它自己的输出」的地方。
新判据 = `it.produced.contains(&f.id)` ∧ 该 finding 未被 verifier 驳回；
`produced` 为空（resume 旧快照）→ 回落旧的区间判据，不因为换口径而集体漏晋升。

### 原计划

`Finding` 带产出它的队列项 id，收紧 `promote_dug_suspicions` 的归因——现在判据是
「finding 落在该项区间内」，而同一区间里的 finding 可能来自兄弟疑点、往轮携带、
或已被 verifier 驳回的那条。后果是**提前**把一条还没真挖出结果的台账行请出激活池
（偏向少挖，方向安全，但语义是错的）。

## Acceptance Criteria

- [x] B-2：`last_verdict()` 加 `rowid` 兜底，不动 schema
- [x] B-4：三分量落地 + `attempts` 订正为总数 + md 标出非 0 分量
- [x] B-1：`first_seen_sha`/`last_seen_sha` + 幂等列改名迁移（含次序哨兵）
- [x] B-3：`QueueItem.produced` + `promote` 收紧（兄弟疑点 / 已驳回 / 旧快照回落 三条用例）
- [x] 五条各有用例（四条债 + md 渲染）；**五个变异全部精确命中**对应用例，无交叉无遗漏
- [x] 变异点逐个 `grep -c` 确认唯一（上一批的教训），每次替换后打印计数校验
- [x] **备份名 `basename` 冲突当场捕获**：`src/cli/mod.rs` 与 `src/output/mod.rs` 都被
      `basename` 成 `mod.rs.bak`、后者覆盖前者，还原时会把 output 全文写进 cli。
      在执行前停住，手动精确还原残留变异；重跑时改用路径压平做备份名。已写进状态机指南
- [ ] 全量测试绿、`make verify` 绿、clippy 本批零新增

## Notes

- 现存库只有本仓与 PR181 两个，工具未发布——B-1 若要迁移，成本主要在代码面不在数据面。
