# M21c 台账·收敛·默认翻转 · 实施计划（批 0 + 四批）

> 状态: 锁定（2026-09-23 派生自 design §4 / §8 / §9 与裁决 **D19**）· 上位 `../roadmap/M21c-ledger-convergence-flip.md`（范围与验收）
> 批序按 D19-3 重排：**批 0 切名先行** → 批 A 台账落账（与 M21d 的 L4-B 并行）→ 批 B 判例前移（等台账有素材）→ 批 C 收敛三维 → 批 D dogfood 内循环。

## M21b 已就位零件（本计划的地基，不重做）

- 深挖队列 / 分派池 / 预算与必挖集 / verifier 流式复核（批 A–D + 出口验收〔D18〕）。
- `db.rs` v4 schema：`suspicions` 表**已建、未写**（design §8.3 / D11-7 明写「只建不写」）；`queue_items` 已在 resume 路径消费。
- `Coverage` 五集合（selected / completed / reused / failed / waived）**结构已在** `src/domain/report.rs:92`，分级语义写在 doc 注释里，
  填值只做了一部分（scanner + reviewer + verifier 的 selected / completed / reused）。
- `prompt::render_precedents`（`src/prompt/mod.rs:448`）与 `consolidate::verify::matches_precedent`（`src/consolidate/verify.rs:138`）已在**终判侧**工作，判例前移 = 把它们挪到快扫 / 入队侧。
- 配置定稿（基线 sha256 `76a5af70…`）：双 reviewer、deepseek scanner medium / reviewer high、qwen scanner low、agc 门宽 6。

## 全批隐含约束（Global Constraints）

1. **每处行为改动配会转红的用例**，合回前跑变异检查（改坏 → 至少一条用例红）；新增旋钮要有端到端接线的用例（默认值也要钉）。
2. **私有数据纪律**：PR181 / 公司仓的读数与明细只落 `.next/private/`，公开侧只留汇总数字。
3. `make verify` 绿（commands 改动随 `make gen` 重生成）；clippy 不增新警告。
4. **dev 二进制不动**：批 0 切名后它仍在原处，作为对照基线与回退路径。
5. 改代码走小分支 + `/code-review` 后合回（M21b 的教训：变异检查测的是用例够不够严，覆盖不到「输入形态没想到」的那类 bug）。

## 批 0 · 切名与库切换（`feat/m21c-flip`，Trellis `m21c-flip`）

> 目标：让插件与 ship 用上新引擎，日常评审开始产生真实数据。**这是后面所有批次的素材来源**（台账要有历史才有内容）。

### Task 0-1 库切换程序〔D19-4〕
- 导出旧 `.redteam/proj.db` 的固定集（dismissed / resolved 卡 + 其 history）→ 归档为只读 `legacy-proj.db`；
  `proj-next.db`（`db::DB_FILE`）改名 `proj.db`，`DB_FILE` 常量随之改；**不做自动 schema 迁移**。
- `--dismissed <legacy 路径>` 作一次性只读复用（现有旗已支持 db 文件路径，验证跨 schema 读取不炸）。
- 验收：构造「旧库 + 新库并存」用例——导出 → 归档 → 改名后，`--dismissed legacy-proj.db` 能读到卡且不写回；
  新库照常读写；旧库缺失时不报错（zero-config 语义不变）。

### Task 0-2 切名本体〔D10〕
- `redteam-next` → `redteam`：`Makefile`（`make build` 放置位置）、`plugins/redteam/bin/darwin/aarch64/`、
  `plugins/redteam/commands/*.md` 壳层里的二进制名、`--help` 与 README 初稿按新 CLI 面。
- dev 二进制归档为对照基线（另存 + README 写明回退路径：改插件 bin 指向即可切回）。
- 验收：`make build` 产物落在新名下、插件调用通；packaging 测试绿。

### Task 0-3 ship 协议对齐
- `plugins/redteam/commands/ship.md` 第 2 步改调 `redteam test-command`；删 `--models` 透传与旧路径引用；`make gen` 重生成 codex-skills 镜像。
- 验收：`make verify` 绿（gen 产物零 diff）。

### Task 0-4 切名门〔D19-2〕
- PR181 跑一轮，要求**机械面复现**：失败格 0、`max_tokens` 截断 0、解析失败 0；全量测试绿；`make verify` 绿。
- dev 二进制的 PR181 基线数字归档进 `.next/private/`（R6 销案的对照口径）。
- 不过则不切名，回头查因。

### Task 0-5 收口
- dev-time 仪器 `REDTEAM_DUMP_DIR` 定去留：拆除 / 升为正式开关（进 `--help`）二选一——切名后它已属用户面。
- followup 里属于 provider 层的两条债一并处理或明确推后：5xx 拿不到 `Retry-After`（需改 `ProviderError` 形状）、
  `cfg!(test)` 缩退避只覆盖 lib 单测（换 `#[tokio::test(start_paused = true)]`）。

## 批 A · 台账落账与 coverage 分级（`feat/m21c-ledger`，与 M21d L4-B 并行）

### Task A-1 预算外疑点落账（roadmap 项 1）
- `suspicions` 表写入：`quote_norm` 锚 + hint + confidence + origin（scan / rule / needs）+ **落入原因**（预算截断 / 规则豁免）+ 首末作业 + 激活次数。
- 写入点 = 深挖收场（`budget_skipped` 那批）与入队被豁免的项；幂等 upsert（同 quote 锚 + 同文件 → 更新 last_job 与计数）。
- 验收：构造用例——预算 0 时 H0 仍被挖（既有用例）且被截项 100% 落账；同一疑点跨两轮只有一行、`last_job` 更新。

### Task A-2 报告披露低置信线索（roadmap 项 1 后半）
- 报告新增段：台账里与本轮变更集**锚定相邻**的休眠疑点，标为低置信线索；**不进 verdict**、不计入 findings 数。
- 验收：构造用例——披露段存在且 verdict / exit code 不受其影响。

### Task A-3 coverage 五集合填值与角色分级（roadmap 项 4）
- `failed` / `waived` 补齐（现只填了前三集）：`waived` = 预算截断 + 规则豁免；`failed` = 重试耗尽。
- 分级落地：scanner 失败 → 硬 Incomplete（现已是）；深挖截断 / 失败 → 带债 approve（现已是，补 coverage 记录）；verifier 失败 → 保守 upheld（现已是）。
- 验收：五集合**不相交且并集 = 全集**的性质用例；kill 一个 scanner cell → Incomplete；kill 一个 deep cell → 带债 approve + warning。

### Task A-4 resume 账目重建（followup 债）
- 续跑收口时按 `cells` 里的记录重建复核四量与 `close_ms`（口径与正常路径同源），或显式标「续跑不可重建」而非静默 null。
- 验收：kill → resume 后报告里复核计数与 `close_ms` 非空且与 cell 记录自洽（M21b 实测：复用格不入账、极端情况全空）。

### Task A-5 回归门
- 跑 PR181 一轮，确认台账落账不改变 verdict 与 findings 集合（只增披露）；数字进私有记录。

## 批 B · 判例前移与小项（`feat/m21c-precedent`，前提：批 0 + 若干轮日常 dogfood 攒出素材）

### Task B-1 判例摘要进快扫 prompt（roadmap 项 5）
- 复用 `render_precedents`：同文件活跃判例摘要注入 scanner prompt；措辞照抄 claimed-resolved 卡。
- 验收：prompt 快照用例（模板哈希变更可见）；无判例时零注入、prompt 逐字节不变。

### Task B-2 跨轮沿用结案守卫〔D13〕
- `matches_precedent` 前移到**入队侧**：机械匹配命中 → 锚定可重定位 ∧ 区域自判例以来无 delta → dismissed 豁免不入队 / upheld carry 不重挖；守卫不过 → 入队并带判例笔记。
- 验收〔D19-5〕：**两条构造用例**——① 已 dismissed 变体在快扫层不产疑点，或产而在入队被豁免；② 判例区域有 delta → 必须重挖。
- 观测量（新 RunMeta 字段）：判例命中导致的免入队 / 免重挖项数。

### Task B-3 小项打包（roadmap 项 6）
- refute prompt 的 `analysis` 先于 `verdict`（字段序）；`positioning` 跨文件重定位（changed files 内唯一命中才接受，多 / 零命中拒绝）；`scanned_areas` 消费进 coverage。
- verifier 侧两条债：判决解析失败没有修复轮、也没有原文落盘（M21b 实测撞过一次，根因查不了）。

### Task B-4 验收与观测
- 同一 PR 连续两轮重复报同一 finding 的比例（人工判机制，按 `file:line` 对齐）——作观测量，不设门槛。

## 批 C · 增量激活与收敛三维（`feat/m21c-converge`）

### Task C-1 增量轮激活（roadmap 项 2）
- `delta ∩ 锚定文件 ≠ ∅` → 休眠疑点重入队、优先级 +1 档；carry 机器（anchor_near / rename 迁移）复用。
- preflight 守卫扩展：上轮 Incomplete → 台账疑点不可信，不激活。
- 验收：构造 delta 命中用例 → 休眠疑点被激活并进队列；上轮 Incomplete 时不激活。

### Task C-2 收敛三维 + 活锁守卫（roadmap 项 3）
- 收敛 = 无 blocking ∧ 台账无 delta 邻接未处理项 ∧ 无激活超限；激活 > N → 强制入必挖集或停车交人。
- 验收：构造活锁用例（同一疑点反复激活）→ N 次后停车并给出可读理由。

### Task C-3 ship 协议消费新字段
- `plugins/redteam/commands/ship.md` 读收敛三维的新 RunMeta 字段；`make gen` 同步。

## 批 D · dogfood 内循环与出口（roadmap 项 9）

- 三轮增量 dogfood：**≥ 1 例休眠疑点被 delta 激活并晋升为 finding**；收敛判定与 exit code 五档契约一致。
- 出口验收按 roadmap「验收」节逐条判；aacr smoke 已按 D19-2 移出本里程碑。
- 收口：roadmap 标 ✅、followup 结账、私有记录归档。

## 与 M21d 的分工（D19-3）

| 面 | 归属 | 说明 |
|---|---|---|
| prompt 模板 | **独占** | L4-B 三方契约注入在 M21d 分支；判例前移在 M21c 批 B。两者不同时动 `src/prompt/` |
| pack / 装包 | M21d | 切片调优、anchor_window、三方契约 |
| db / 台账 / coverage | M21c | 批 A |
| 队列入口与守卫 | M21c | 批 B / C |
