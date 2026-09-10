# M21c · 台账·收敛·默认翻转

> 状态: ⏳ · 依赖 M21b · design §8 / §4 判例前移 / §9 L2 重定位 · 决策 D6
> 出口：二进制切名 `redteam-next` → `redteam`，插件与 ship 指向新二进制；dev 二进制保留为对照基线；ship 内循环 dogfood 通过

## 目标

让预算截断从「漏」变「延迟到下一轮」，让收敛与 coverage 在新形态下仍有确定性语义，然后翻转默认。

## 交付项

| # | 项 | design | 落点 |
|---|---|---|---|
| 1 | 疑点台账表（proj.db，镜像 DismissCard / ResolveCard）：预算外疑点落账（quote 锚 + hint + confidence + origin + 落入原因）；报告披露为低置信线索；**不进 verdict** | §8.2 | `src/db.rs`, `src/output` |
| 2 | 增量轮激活：`delta ∩ 锚定文件 ≠ ∅` → 重入队优先级 +1 档；carry 机器（anchor_near / rename 迁移）复用；注入措辞照抄 claimed-resolved 卡；preflight 守卫扩展（上轮 Incomplete → 台账疑点不可信） | §8.2 | `src/cli/incremental.rs`, `src/consolidate/carry.rs` |
| 3 | 收敛三维（无 blocking ∧ 台账无 delta 邻接未处理项 ∧ 无激活超限）+ 活锁守卫（激活 > N → 强制入必挖集或停车交人）；ship 协议消费新 RunMeta 字段 | §8.3 | `src/verdict.rs` 不动；RunMeta；`plugins/redteam/commands/ship.md` + `make gen` |
| 4 | coverage 五集合（selected / completed / reused / failed / waived，per cell 类型）+ 角色分级：快扫失败 → 硬 Incomplete；深挖截断/失败 → 带债 approve | §8.4 | `src/orchestrate` CellSink.manifest, `src/cli/incremental.rs` preflight |
| 5 | 判例前移进快扫 prompt：同文件活跃判例摘要；机械匹配命中 → **跨轮沿用结案带守卫**（锚定可重定位 ∧ 区域自判例以来无 delta → dismissed 豁免不入队 / upheld carry 不重挖；守卫不过 → 入队带判例笔记）〔D13，替代「降级候选」〕 | §4, §6 规则 2 | `src/prompt/mod.rs` render_precedents 复用；`src/consolidate/verify.rs` matches_precedent 前移调用点 |
| 6 | 小项打包：refute prompt `analysis` 先于 verdict；positioning 跨文件重定位（changed files 内唯一命中，多/零命中拒绝）；`scanned_areas` 消费进 coverage | §8.1, §9 L2 | `src/prompt/templates/adversarial-refute.md`, `src/positioning` |
| 7 | **切名前 aacr `smoke`**（D7 B4 重启点）：fork 上 5 个 rs / go / ts 正样本，legacy 与 v2 各 N=1，`JUDGE_USE_MOCK=true` 取 Line-P / Line-R；结果写回 research；`reviewers/redteam.py` 适配器与 pipeline 挂载随本项一并接线（本仓 `eval/README` + `AACR_BENCH_DIR`） | design §16 | `~/codes/aacr-bench`；本仓 `eval/` |
| 8 | **切名**（D10）：`redteam-next` → `redteam`，`make build` 放置新二进制，插件与 ship 指向它；dev 二进制归档为对照基线（R6 销案〔D11〕：对照 = 并排跑 PR181 比 oracle 命中）；`--help` / README 初稿按新 CLI 面 | 铁律 6 | Makefile, `plugins/redteam/`, `src/cli` |
| 9 | dogfood 内循环：ship r1-rN 至 approve；发现修复回流；台账激活至少观测到一例 | — | — |

## 验收

- 三轮增量 dogfood：≥ 1 例休眠疑点被 delta 激活并晋升为 finding；收敛判定与 exit code 五档契约一致。
- coverage 五集合不相交并 = 全集；人工 kill 一个快扫 cell → verdict Incomplete；kill 一个 deep cell → 带债 approve + warning。
- 判例前移后：已 dismissed 变体在快扫层不产疑点，或产而在队列入口被守卫豁免；构造「判例区域有 delta」用例 → 必须重挖；refute 调用数较 M21b 下降可观测。
- aacr `smoke`：`redteam-next` 的 Line-Recall ≥ dev，Noise 不高于 dev 1.5×；否则不切名，先回 M21b 调预算 / 规则。
- 切名后全量测试绿；dev 二进制 PR181 基线数字已归档；`make verify` 绿（commands 改动随 gen）。

## 风险

- 台账激活的「+1 档」若把大量低价值疑点顶到必挖集边缘 → 以激活计数与 N 上限（R12）封顶。
- 跨轮沿用结案若守卫过松会盖住 fix 引入的新 bug：守卫要求区域无 delta，有 delta 一律重挖；refute 仍是终审。
