# followup 对账 + A 类小修

## Goal

两件事：**让账本可信**，再做掉已确认改动量很小的四条。

起因是上一个单元（增量轮重建）顺手核实四条旧债时，发现两条已解、一条已过期。扩大抽查到 9 条——
**7 条早已解决、无人回标**。脚本数出 226 条「未解」，真实数字远低于此。这不是记账洁癖问题：
账本不可信，每次「挑活」都要把同样的条目重新调研一遍。

## 根因：条件触发式的债没有触发机制

一批债挂的触发条件是「**M21c 切名后**做」，而切名 2026-09-28 就完成了——它们集体到期却无人认领，
因为**没有任何机制在切名那天提醒**。同形的还有「攒够一批 schema 改动一起做」（已攒够 4 条）、
「发布前」、「下一轮 PR181 dogfood」。

处置：followup 顶部加「▲ 到期看板」，把触发条件、是否已满足、挂着的债三列并排。写条件触发式的债时
登进该表；条件满足当天回来扫一遍。

## 第 0 步：对账（12 条回标）

| 债 | 账上写的 | 核实 |
|---|---|---|
| `DB_FILE` 切回 `proj.db` | M21c 切名时做 | ✅ `db.rs:13` 已切，`proj-next.db` 降为历史常量 |
| ship.md 调内置 `test-command` | M21c 切名后改 | ✅ `ship.md:28` 已改，`--models` 透传已删 |
| `ThinkingLevel` schema 零消费 | M21a 首个消费者 | ✅ `main.rs:529/555` |
| 测试助手 `finding()` 三份 | M21b 顺手抽 | ✅ `finding.rs:268 mod fixtures` |
| `verify.rs` 后置 barrier | M21b 改流消费者 | ✅ `refute_targets` 零引用 |
| `per-fanout` 单模型计划 | M21a 角色化 | ✅ 深挖走 `input.reviewers: &[EffectiveModel]` |
| `priority` 候补链 / `resolve_models` / `--models` | M21a 退场 | ✅ 三者全零引用，`--models` 已被 clap 拒收 |
| `from_layers_tagged` | M21a 整段删 | ✅ 零引用 |
| `XDG_CONFIG_HOME` 串行化 hack | M21a 换 `REDTEAM_HOME` | ✅ 已换，XDG 只剩旧路径探测 |
| 三模板单文件 | M21a/b 拆四份 | ✅ shared-head / scanner / reviewer / verifier 都在 |
| 状态机盘点「第 2 步」 | 写进 `.trellis/spec/` | ✅ 已写，本轮又补了第四种病 |
| 收敛三维不可证伪 | 批 D 跑小 diff 验 | ✅ 已观测到 `converged: true` |

## A 类四条

**A1 secrets 并存提示收窄**（切名后到期的那条）。两条旧路径迁移 notice 原本挂在
`Config::load_with_messages` 上——配置每条命令都加载，而 dev 二进制的旧 TOML 在并存期长期存在，
于是只用 env 存 key 的用户每次调用都看见同样两行。摘成 `legacy_migration_notices()`，
只由 `model list` / `model check` 调。

**A2 md 收敛渲染的一致性兜底**。标题信任 `converged`、理由来自三个分量，两者一致是**调用方**
的保证，渲染侧没有。真不一致时原来输出「Not converged — ；建议再跑一轮」，一个理由都列不出，
而 ship 循环读的正是这一行。

**A3 `enqueue_activated` 的「静默丢弃」——核下来不是静默问题，是第四种病。**
`from_ledger` 里 `line_start.max(1)` + `line_end.max(start)` 已保证区间合法，而
`QueueItemSpec::new` 只拒 `a == 0 || b < a`——**这个分支不可达**。
不删的理由是后果重：台账回写（`activations` +1）在 CLI 层**早于**入队发生，真丢掉会让疑点
假性逼近活锁门（N=3 必挖 / N=5 停车），最后报告对人说「机制挖不动它」，语义正好反了。
所以把不变量钉在代码里：debug 下 `debug_assert!(false)`，release 下披露而非静默；
真正守住它的是新加的不变量用例，不是那段代码。

**A4 三态遗留行规范化**（发布门）。批 C 两态化时没规范化已有数据，旧二进制写的
`status='activated'` 行在新语义下既不被激活也不被披露，**永久卡住**。
关键：这类库**已经是 v4 且表齐全**，`needs_setup` 为假、`setup_schema` 整条迁移路径不跑——
所以规范化挂在 open 上，幂等。**只复位 `activated`，不碰 `promoted`**（那会让已晋升的被反复重挖）。

## Acceptance Criteria

- [x] 12 条回标完成；「▲ 到期看板」落地，含已满足/未到两类触发条件
- [x] A1：`legacy_migration_notices()` 在 `model list` / `model check` 打；加载路径不再带它
- [x] A2：`converged` 与三分量矛盾时，收敛行说出矛盾而不是留空理由
- [x] A3：`debug_assert!` + 不变量用例 `from_ledger_never_fails_on_any_ledger_row`（边界值逐个压）
- [x] A4：`activated` → `dormant`，`promoted` 与 `dormant` 不动
- [x] 四条各有用例；变异检查四条各自转红对应用例：
      M1 A2 失效、M2 A4 不规范化、M3 A4 过度（连 `promoted` 一起复位）、M4 去掉 `line_end.max(start)`
- [x] **变异检查本身的假绿已抑制**：M4 首跑显示 `ok`——查下去是
      `perl -0777 -pe`（无 `/g`）只改了**第一处**，而同一行文本在 `queue.rs` 出现两次
      （306 / 339），被测的 `from_ledger` 在 339。按行号重做后正确转红。
      已写进状态机指南的检查项：用文本替换做变异时先确认替换点唯一且正确，否则
      会把「用例没红」读成「代码是对的」
- [ ] 全量测试绿、`make verify` 绿、clippy 本批零新增

## Notes

- A1 摘掉那两条 notice 时全量测试**照绿**——原行为无人盯，属本会话反复出现的假绿形态。
- A1 的用例一度红在真机环境：`~/.config/redteam/` 确实存在（dev 二进制的旧配置），
  探测到 4 条而非 2 条。断言已收紧到用例自己造的那两个文件，不数总数。
- 未做：B 类 schema 包（4 条，另起一批，动 schema 要集中）；C 类依赖真实运行，与 ship 同场。
