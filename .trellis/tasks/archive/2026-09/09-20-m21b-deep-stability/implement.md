# 批 C3 实施清单

> 顺序 C3-1 → C3-4。每 task 一 commit，简体中文 `type(scope): 摘要——细节`，绝不 push。
> 工作树 `~/hobby/.worktrees/redteam/m21b-deep-stability`，分支 `feat/m21b-deep-stability`（自 `feat/m21-review-engine` 切）。
> **C3-1 必须先落**：稳定块与结构化合并是 C3-2 排序键、C3-3 needs 入队的共同地基；零件先用单测钉住语义再接线。
> 定案见 design §11（`/grilling` Q1–Q12）与 decisions D16；候选集源头手段、diff 外 needs 装包不在本批。

## Checklist

### C3-1 稳定块与结构化合并（两轴共识）
- [x] `risk/regions.rs`：`enclosing_decl` / `decl_block_end` 提 `pub(crate)`，**逻辑一行不动**；`eval/risk_replay.py` 对拍先跑一次存基线读数
- [x] `orchestrate/blocks.rs`（新）：`block_of(file, content, line) -> Block { start, end, key }`——代码走 enclosing def、markdown 走标题节（同级或更高级标题切）、
      其余退 hunk ±10 行；`key` 取 decl / 标题行去空白哈希（位移不改身份）
- [x] `queue.rs`：`QueueItem` 增 `block`（serde default 空 → 旧快照退现行为）、`risk_band`、`peak_agreement`；`same_item(Scan|Needs)` 改
      「同文件 ∧ 同 `block.key`」；**取消 id 捷径**；并入后增量算项内聚簇（`same_suspicion` 并查集 → `peak_agreement`、`region_families` 写回 `agreement`）
- [x] `queue.rs`：必挖判定 `must_dig` 改 `kind == Primary ∨ peak_agreement ≥ 2`；Skipped 复位条件随之（旧快照无 `peak_agreement` → 退回 `agreement`）
- [x] `deep.rs`：入队路径（通道到达 / `enqueue_scan_cells` / H0）算块并填 `block`、`risk_band`（`route.zones` 取重叠且分数最高的区；无区 0）
- [x] 测试：块身份稳定（def 前插行后 `key` 不变）；文档标题节切分；无 def / 无标题退路；同 def 两条不相似疑点 → 一项且 `region_families = 2` / `peak_agreement = 1` → 非必挖；
      同一条疑点跨家族 → `peak_agreement = 2` → 必挖不截；旧快照（无新字段）行为不变；C2 的到达键幂等与 `members` 累积用例全绿
- [x] 变异检查：`same_item` 退回 quote 谓词 / id 捷径复活 / `peak` 用区域级 / 块 key 用行号 / 复位条件用 `agreement`——各自转红
- [x] `eval/risk_replay.py` 对拍：H0 成区逐字不变
- [x] Commit: `feat(queue,orchestrate): 稳定块与结构化合并——项身份锚到 enclosing def / 文档标题节，共识拆必挖轴与排序轴（design C3 §2 / §3〔D16〕）`

### C3-2 分层排序键与文件多样性
- [x] `queue.rs`：`cmp_priority` 改分层——`first_slot` → `kind` → `peak_agreement` → `risk_band` → 烟量 → 代码优先 → `confidence` → `id`；
      删除乘积式 `priority_score`（类别权重维持 1.0 不启用，注释写明理由）
- [x] `queue.rs`：`first_slot` = 该文件尚无已派发项 ∧ 本项是该文件 Pending 中的最高者；`pending_order` / `drain_among` / `pop_dispatch_where` 共用同一序
- [x] `pack/region.rs`：`render_notes` 去掉 `(confidence x.xx)`
- [x] `testdata/deep_serial_baseline.json` 重采（笔记字节与模板哈希变），commit 说明原因；其余差异逐条解释
- [x] 测试：分层键逐维单变量对；`first_slot` 派发序 A₁ → B₁ → A₂；保底随派发失效；drain 与派发同序；笔记渲染无 `confidence`；退化零差与重采基线同构
- [x] 变异检查：去掉 `first_slot` 层 / `risk_band` 不参与 / 烟量不参与 / confidence 提到首位 / drain 与派发用不同序——各自转红
- [x] Commit: `feat(queue,pack): 分层排序键接风险分、每文件保底一格、笔记去 confidence——选项不再由自评置信主导（design C3 §4 / §6〔D16〕）`

### C3-3 scanner needs 入队与观测
- [x] `orchestrate/mod.rs`：格落盘、疑点发完后，对 `kind = evidence` 且 `path ∈ 变更集` 的 needs 逐条发 `EnqueueReq`（`origin = Needs`、空 notes、
      `ranges` 由 `lines` 解析、到达键 `needs:{path}:{lines}:{指纹}`）；变更集外只计数
- [x] `deep.rs` / `report.rs`：`DeepMeta` 增 `suspicions_arrived` / `items_from_suspicions` / `files_candidate` / `files_selected` / `max_cells_per_file`
      （serde default、为零不序列化）；`enqueue_reasons` 增 `needs` / `needs_out_of_diff`
- [x] 测试：变更集内 needs 入队且排在疑点之后（预算有余才吃格）；变更集外只计数不入队；needs 项 pack 走区域切片、无笔记；
      五个标量与手算一致；`DeepMeta` 旧 JSON 兼容
- [x] 变异检查：不过滤变更集 / needs 排到疑点之前 / `max_cells_per_file` 取错维度 / `files_selected` 统计全部项——各自转红
- [x] Commit: `feat(orchestrate,deep): scanner needs 首次入队（仅变更集内）与选项观测五量——design §6 第三种生产者接线（D16 Q8 / Q11）`

### C3-4 收口
- [x] `cargo test` / `clippy --all-targets` 零新增（基线 7）/ `make verify` / `eval/risk_replay.py` 对拍
- [x] `make build` → 同 diff **三跑**（1-2 对落在 30%–50% 边界带，按 Q9 加跑第三轮）：
  - 主门槛逐项重合度 50% / 41% / 41% → **中位 41% ≥ 40%**（改前 25%）✅
  - 辅助门槛 `max_cells_per_file` 25% / 17% / 17% ≤ 1/3（改前 46% / 50%）✅
  - 护栏 findings 17 / 21 / 16 ≥ 11 ✅；**代码格占比均值 66.7% < 68% ❌ → 2026-09-20 定案「认这个交换」**
    （代理量与被代理量反向：代码格产出 9 / 12 → 11 / 11 / 11；阈值不回改，下批换绝对量）
  - 三轮零 429、门全程 6；config sha256 `06986246…` 跑前跑后一致
- [x] samples = 2 一次性测量轮（跑完已还原、指纹校验）：候选位置 50 ≈ 两轮并集、token 翻倍、墙钟 +2～3 min；
  与三轮 samples 1 的选中项重合度 50 / 50 / 60% **高于** samples 1 彼此的 50 / 41 / 41% —— 数据进 followup
- [x] 记入本工作树 `.next/private/m21b/2026-09-20-batch-c3-dogfood.md`（改后三轮 + 测量轮 + 对照），账目进 followup；decisions 增 D16 节、design 升 v12（§6 规则 1 / 2 / 3 与可观测面、§7）
- [x] code-review 审核（max effort，15 条 + 10 条超帽项）→ 分三档全修：
  - 第 1 档 10 条真错（块键碰撞 / 行带拆共识 / 区间被到达撑大 / 旧项升级错位 / 保底轴不一致 / 必挖轴被降级 /
    scan 与 needs 重挖 / H0 让位 / 同 id 共存 / 行号溢出）：`8878434`
  - 第 2 档 7 条口径与浪费（笔记与块区间封顶 / needs 行锚口径归一 / 路径归一 / 外部计数去重 / 文档分类归一 /
    风险档接 H0 / defs 整轮缓存 + `--help` 同步）：`a75d8ee`
  - 第 3 档：死常量清理、resume 轮观测从快照重建、风险档随 route 重算、拆簇指标成因更正、D16 失实理由更正，其余记账进 followup
  - 变异检查 20 处逐条转红（脚本 `.next/private/m21b/raw/mutate_c3_tier1.py`）
- [ ] `--no-ff` 合回 `feat/m21-review-engine`；私有记录拷回集成工作树；归档任务 + journal

## 验证命令

```bash
cargo test && cargo clippy --all-targets   # 基线 7
make verify
python3 eval/risk_replay.py                # H0 成区对拍（参数见脚本头）
# dogfood：make build && <临时工作树> ./…/redteam-next review --base 674583e --format json
python3 .next/private/m21b/raw/selection_overlap.py <r1.json> <r2.json> [--by-file]
```

## 风险文件与回滚

- 高风险：`queue.rs`（合并谓词 + 必挖轴 + 排序键三处同时变——先红后绿逐条钉，变异检查覆盖每一层）；
  `risk/regions.rs`（只提可见性，H0 行为逐字不变，对拍是哨）；`pack/region.rs`（笔记字节 → 模板哈希 → 基线重采，别与真实漂移混淆）。
- 改前基线对（`.next/private/m21b/2026-09-20-batch-c3-baseline-pair.md`）与 C1 / C2 全部既有用例是第一道回归哨。
- 回滚 = 不合回；合回后 revert 该 merge。

## start 前核对

- [x] 批 C2 已合回（`3038ee7`）并归档（`fffd595`）；集成分支四绿、clippy 7、`make verify` 绿
- [x] 改前基线对已跑（2026-09-20 两轮，门 6 无 429）与度量脚本 `selection_overlap.py` 已入私有 `raw/`
- [x] `/grilling` Q1–Q12 定案已落 prd 与 design §11
- [x] `implement.jsonl` / `check.jsonl` 各 ≥ 1 条真条目（开工时补）
- [x] decisions D16 节与 design v11 §6 / §7 的改写（C3-4 收口时一并落）
