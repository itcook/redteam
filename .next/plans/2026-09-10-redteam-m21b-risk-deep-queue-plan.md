# M21b 风险路由与深挖队列 · 实施计划（四批 + 出口）

> 状态: 锁定（2026-09-10 派生自 design v10〔D13〕；同日修订：删 aacr 接线批——D7 B4 暂退役、随 M21c smoke 重启点一并做；切点与回放口径对齐 D11）· 上位 `../roadmap/M21b-risk-deep-queue.md`（范围与验收）· design
> `../design/2026-09-04-redteam-review-engine-architecture-design.md` §2 / §3.3 / §6 / §7 / §8.1 / §10.6-10.7 / §12.3 / §13 ·
> 决策 D1 勘误, D2, D6, D7, D10, D11, D13 · P0 规则定案 `../private/research/2026-09-07-redteam-p0-validation-gate-research.md` §15.4-15.7
> **执行形态**：每批一个 Trellis 任务（`m21b-risk` / `m21b-review-primitives` / `m21b-deep` / `m21b-verifier`，**仅需 prd，design / implement 开工时 grill 或脑暴产出**）+
> 一个子分支 `feat/m21b-<批>`（自 `feat/m21-review-engine` 切出，独立工作树）；批内小步提交；批收口用 dev `redteam` 跑 ship 评审，
> 修完 `--no-ff` 合回 `feat/m21-review-engine`。出口验收在本工作树单 session。

**Goal:** `redteam-next` 端到端产 finding：风险规则热区 H0 与 scanner 疑点进同一条优先级队列，reviewer 分派池按 provider 门空位
消费（预算随风险分 R、必挖集不截），finding 落地即流式持久化并由 verifier 流式复核；barrier 后 reduce 产报告。插件与 ship 循环
仍用 dev `redteam`（切名在 M21c）。

## M21a 已就位零件（本计划的地基，不重做）

- **InflightGate 全套**（D13 收口批 4aabec3）：429 排队重试 + 准入、`registry.lease()` / `ScanLease`、`gate_stats()` /
  `inflight_peaks()` / `rate_limit_meta()` / `queue_warnings()`；**本计划不再建任何角色级池 / 信号量**。
- `MaxCells`（Auto|Fixed）与 `ReviewersCfg { max_cells, reenqueue }`、`VerifiersCfg { enabled }` 已在 config（M21a A-2 建形）。
- `CellOutput::{Scan, Review, Verify}` 枚举、`CellRecord.role / template_hash / attempts / queue_wait_ms`、`queue_items` /
  `suspicions` 表壳（只建不写）、`Phase::{Collecting, Fanout(旧), Scan, Finalizing, Refute(旧)}`。
- `merge_suspicions`（quote 去空白重叠锚合并、agreement = 家族数）在 `src/consolidate/scan.rs`。
- 切片装包全套（核 / 一跳 / 文件地图 / 折叠 / 兜底梯子）与文件外片段（caller ±N / callee 签名）在 `src/pack/mod.rs`；跨组索引
  `{{OTHER_CHANGED_FILES}}` 槽在 `src/prompt/mod.rs`。
- `shared-head.md` + `scanner.md` 模板、`build_scan_prompt`、模板哈希、anthropic 三段界 cache 断点。
- `eval/`：`risk_replay.py`（P0 E1 Python 参考实现，v4 规则）、`scan_score.py`、`export_ledger.py`、fixtures
  `ledger-{dismissed,upheld,resolved}.jsonl`（63 / 48 / n）。
- 保留件：`src/cli/second_pass.rs`（三标配机器 + reviewer needs 二阶段机器）、`src/prompt/templates/adversarial-refute.md`（verifier 底稿）、
  `src/consolidate/verify.rs`（barrier 型 refute，含 ±60 快照 / 骨架中档 / 读帽全套，改造非重写）。

## 开工前置清单（M21b 前，followup 已记；批 A 开工前完成，产出进 followup / 决策记）

- **分组策略议定**：✅ 已偿（2026-09-10，Trellis `09-10-small-component-pooling`，用户裁选项 1〔D14〕）：小分量（≤3 文件且 est ≤ 预算/4，budget=None 不判）作原子单元与孤立文件同池装箱；PR181 7→4 组、cross_import=0 保持。拆大格键按每格 **diff 字节**（非 pack 字节）仍保持 followup 观测（用户裁：样本太少）。
- **分组预算门（估算字节）与装包（切片字节）口径对齐**：PR181 `--pack-only` 回放（数据 `.next/private/m21b/pr181-packonly-pooling.txt`）：估算 bytes vs pack_bytes 系统性偏高 1.2–4.8x（保守拆）；两 over_budget 组仍撞 `maxFilesPerGroup`=10 而非字节帽——正式结论随 followup 观测，不改门。
- **`GroupingStats` 分种计数**：✅ 已偿（2026-09-10，Trellis `09-10-grouping-stats-kinds`）：`cross_import` / `cross_reference` /
  `cross_same_dir` 三键（serde default）、`weak_cut` 删除（真切口径无独立信息量）；「强边不跨组」不变量哨兵测试；PR181 基线
  cross_import=0（复验强边不跨组不变量——恒为零，语义分组增量不在可观测范围，R8 判决另需跨组索引数据）/ cross_reference=2 / cross_same_dir=6、旧 weak_cut=55 虚高
  （真切口径 8）；数据 `.next/private/m21b/pr181-packonly-kinds.txt`。
- **速率型 429 棘轮与放弃判据**（D13 收口批评审延后项，用户裁）：看 PR181 `provider_rate_limit` 数据后定 Retry-After 速率信号
  不收门 / 最小回升 / 等待总时长 ≤ `perTurnTimeoutSecs`。

## 批间依赖与波次

| 波 | 批（子分支） | 前提 | 碰撞点 |
|---|---|---|---|
| 1 | **A 风险评分器**（`feat/m21b-risk`）· **B reviewer 静态零件**（`feat/m21b-review-primitives`） | 无，两批并行 | A 与 B 无文件交集（A: `src/risk/`+`src/cli/mod.rs` 接线段；B: `src/pack/`+`src/prompt/`+`src/provider/parse.rs`+`tool_loop.rs`） |
| 2 | **C 深挖队列与 reviewer 运行时**（`feat/m21b-deep`） | A、B 合回 | 独占 `src/orchestrate/`、`src/cli/mod.rs` 主流程、`src/state/` |
| 3 | **D verifier 流式段**（`feat/m21b-verifier`） | C 合回（finding 事件流） | 独占 `src/consolidate/verify.rs`、verifier.md；与 C 约定 `FindingEvent` 接口（C-3 定义） |
| 4 | 出口验收（本工作树，单 session） | A-D 全合回 | — |

（aacr 接线不在本里程碑：D7 B4 暂退役、重启节点待定，`reviewers/redteam.py` 随 M21c smoke 重启点一并做〔M21c 交付 7〕。）

```
git worktree add ~/hobby/.worktrees/redteam/m21b-risk             -b feat/m21b-risk             feat/m21-review-engine
git worktree add ~/hobby/.worktrees/redteam/m21b-review-primitives -b feat/m21b-review-primitives feat/m21-review-engine
```
- `.next/private/`（PR181 原始数据、oracle）gitignored，新工作树没有：批 A 的 A-5 回放对照从本工作树跑（或拷 `.next/private`）；其余批不需要。
- 每个子分支工作树自己 `cargo build`；`make build` 在出口验收前不跑（不覆盖插件 dev `redteam`）。

## Global Constraints（全批隐含）

- 每 task：`cargo test` 全绿 + `cargo clippy --all-targets` 零新增 + `make verify` 绿 + commit（简体中文 `type(scope): 摘要——细节`，
  绝不 push）；改动只在 task 列出的文件与其测试；批内发现的新问题记 `.next/roadmap/followup.md`，不顺手修（Critical 除外）。
- 铁律映射：新编排 pass 三标配（注入互斥 · 证据段预算帽 · 读守卫，B-5）；输出契约与 parse 字段级对齐（B-3 / D-3）；模板改动必跑对应
  离线回归集（B: PR181 scan_score 不退化；D: verifier 二分类，D10/D9）；机制上线必带观测仪表（C-6）。
- 无候补：cell 重试耗尽即失败进 coverage `failed`；reviewer cell 失败 / 预算截断 → 带债 approve（台账化披露，M21c 前先进 RunMeta）。
- 无角色级 `workers` / 无 `--workers`（D13）：唯一闸 = provider 在途门；派发由门空位驱动。
- dev 二进制与插件 `bin/redteam` 全程不动；`redteam-next` 不切名。
- 不做：台账与增量轮激活（M21c）· 三方契约注入（M21d，reviewer pack 留空槽）· 第二家族 scanner（M21e）· 判例变体触发（D-2 预留
  触发枚举槽，判例数据 M21c 才有）· 候补 / failover。
- 公司仓内容不进跟踪文件：测试 fixture 只用合成样本或本仓数据；PR181 跑分只落 `.next/private/`。

---

## 批 A · 风险评分器（`feat/m21b-risk`，Trellis `m21b-risk`）

规则即 spec：P0 §15.4 v4 定案（权重 / 乘子 / 成区 / θ=1.5）**常量化进代码**，逐条照抄不发明；`risk_replay.py` 的 Python 实现是
对照真值。零 LLM、零新配置旋钮（无 `[review.risk]`、无 `--risk`，D11）。

### Task A-1 成区：主区与涟漪区
- Add: `src/risk/regions.rs`（`regions(changes, files) -> Vec<Region>`：hunk → enclosing def 块〔复用 `diffsym` 的 def 块与配平；
  局部 const 不算 def；无 def → hunk ± 10 行 padded，只与 padded 相邻合并；相邻函数各自成区〕；`kind: Primary | Ripple`：
  涟漪 = 改动 / 新增 / 删除的声明名与 hunk 外围符号在本文件引用点（≤ 4 处，不跨文件）的 enclosing def；区间带 `signals: Vec<SignalKind>`）。
  **def 口径统一**（followup 实现债）：分组侧 `bundle::changed_def_names`（hunk 首行锚定 + `block_end`、含测试 def）与文件外搜索键
  `pack::outside_names`（全改动行 + `value_end` + 容器 / 测试排除 + 重锚定）两套——风险规则复用时一并统一（分组侧改吃
  `all_defs` / `changed_defs`），重跑分组回归。
- 测试：三语言合成文件（def 块 / 无 def padded / 涟漪 ≤4 处 / 相邻不并 / 局部 const 不成区）；分组回归重跑零变化（或预期内变化记录在案）。
- Commit: `feat(risk): 成区——hunk→def 块、padded 兜底、主区/涟漪二梯队（design §3.3，P0 v4）`

### Task A-2 四类信号与评分
- Add: `src/risk/signals.rs`（路径 glob〔auth·secur·crypt·payment·billing·config·setting·secret·token·permission·migrat·callback·webhook〕
  1.0；新增行关键词类〔deser / exec_sql / condwrite / timeout / concurrency / destructive / auth / numeric_bounds / error_paths /
  resource_lifecycle〕每类 0.5 封顶 1.5，涟漪区看区体；结构〔签名行被改 1.0 | def 删除 1.0 | 新 def 0.5 | 公开面拓宽 0.5 | fan-in ≥ 5
  〔`git grep -w` 跨仓、排本文件与测试、名须像符号，复用 M21a 切片批 Task E-3〔文件外片段〕词边界搜索〕1.0 | 区间改动行 ≥ 全 diff Q3 0.5 | 新文件 0.5 | 涟漪 1.0〕；
  extapi〔有 path 信号的区里外部包符号调用 ≥ 3：Go import / TS import / Rust use 解析排本模块与标准库，1.0〕；历史〔base 祖先
  12 个月 fix·bug·hotfix·revert·regress 提交 ≥ 3 → 0.5、≥ 6 → 1.0；台账 upheld 1.0 / dismissed 0.5，严格早于作业时刻——读
  proj.db `dismissed` 表与 `results` 内 upheld verdicts，带时间守卫；**数据源经 `--dismissed` 指定**，缺省读 `proj-next.db` 自身
  （M21a 分库裁定：dev 旧库判例卡对 next 不可见）；PR181 出口对拍传 dev 旧 `proj.db` 复现 P0 标定条件〕；乘子〔声明区 ×0.5；测试路径或 `#[cfg(test)]` 之后 ×0.3；
  文档 / schema ×0.2〕。全部权重 = `const`，一处一张权重表 doc 注释引 P0 §15.4）。
- 测试：每类信号合成用例各一；乘器叠加顺序；fan-in 假名过滤（`if` / `err` 类不算）；台账时间守卫（作业后记录不计分）。
- Commit: `feat(risk): 四类信号（路径/结构/外部API/历史）+ 乘子 + 常量权重表（P0 v4 定案）`

### Task A-3 H0 门槛、R 分与预算三档
- Add: `src/risk/mod.rs`（`route(changes, root, ledger, git) -> RiskRoute { hotspots: Vec<Hotspot>, r: f32, tier }`：score ≥ θ=1.5
  **且**命中 ≥ 2 类信号才入 H0（关键词类单独不成区）；R = Σ_{H0} score ÷ (1 + ln(1 + 代码改动行))；切点 **0.5 / 3.5**〔D11-4 锁定常量；
  P0 §15.7 实测三分位 0.47 / 3.64 作回校参考，M21b 出口按截断数 / 台账落入数回校〕分 low / mid /
  high → `max_cells_hint = 4 / 12 / 32`（硬帽 48）；`budget_cells(tier, cfg.max_cells) -> u32`：`Fixed(n)` 直用、`Auto` 查表并
  `min(48)`）。
- 测试：θ 边界；单类信号不出 H0；R 公式对拍 `risk_replay.py`（同 diff 同 R，容差 1e-6）；三档切点；Fixed 覆盖 Auto；帽 48。
- Commit: `feat(risk): H0 门槛（θ=1.5 且 ≥2 类）+ R 风险分 + 预算三档 4/12/32 帽 48（design §3.3/§6）`

### Task A-4 接线：阶段 0 调用 + 观测
- Modify: `src/cli/mod.rs`（`run_code_review` 在 `plan_cells` 前调 `risk::route`：H0 / R / tier 进 `RunMeta`〔新字段
  `risk: Option<RiskMeta { hotspots: u32, r, tier, budget_cells }>`，serde default〕+ `--pack-only` 打印 H0 区数 / R / 档位一行；
  `run_resume` 沿用原报告值）、`src/domain/report.rs`（RiskMeta 字段）、`src/main.rs`（`--help` 无新旗——零旋钮）。
- 测试：RunMeta 快照含 risk 段；`--pack-only` 输出行；空 diff → r=0 low 档。
- Commit: `feat(cli): 阶段 0 风险路由接线——H0/R/tier/预算入 RunMeta 与 --pack-only（design §2）`

### Task A-5 回放回归门
- **前置核实**（followup「批 A 重做评审子 agent 提出、未核实项」）：`risk_replay.py` `parse_diff` 疑似把列首 `--` / `++`
  内容行当文件头、`e3_contract_ab.py:315` `anchored` 疑似恒真式——两条先核实修正，再作回归门。
- Modify: `eval/risk_replay.py`（加 `compare <replay.json> <rust-out.json>` 子命令：Rust `risk` 输出 vs Python v4 区级对拍，
  报区级 Jaccard 与 R 差；`--help` 用法段）、`src/cli/mod.rs` 或 `src/main.rs`（`redteam-next risk --base <b>` 只读子命令：
  跑阶段 0 风险路由打印 JSON〔供 compare 吃〕；不调 provider——裸终端可回归）。
- 回放口径〔D11-3〕：**PR181（私有 fixture，`.next/private/`）+ 本仓 oracle B（入库 fixture；若尚无，本 task 把 P0 §15.5 oracle B
  的本仓子集导出入 `eval/fixtures/`）**，数字写进 research；`θ 与权重改动必过此门`（在本工作树跑，数据落 `.next/private/m21b/risk/`）：
  Rust vs Python 区级一致率、oracle 命中不低于 P0 基线（PR181 主裁决 4/4、并集 14/22）。
- Commit: `feat(eval): risk compare 子命令 + redteam risk 只读子命令——Rust 实现对拍 Python v4 回归门（D11）`

### Task A-6 收口
- `--help`：`risk` 子命令说明；权重表常量注释引 P0 §15.4。
- ship 评审（dev `redteam`）→ 修 → `--no-ff` 合回。

---

## 批 B · reviewer 静态零件（`feat/m21b-review-primitives`，Trellis `m21b-review-primitives`）

reviewer cell 的全部静态零件：区域 pack、模板、parse、SSE 流式旁路、三标配。全部可用合成输入 + `FakeChatBackend` 测试，不依赖队列。

### Task B-1 区域 pack 构造
- Add: `src/pack/region.rs`（`RegionPack::build(item: &QueueItemSpec, bundle, cross_index, spec, root, budget) -> RegionPack`：
  队列项 `{file, ranges, origin, notes}` → 区域切片〔复用切片配方：核 = ranges 内 def 全文 + diff；一跳；文件地图；折叠；兜底梯子——
  由构造即切片，**不受 `[review.pack] body` 控制**〕+ 爆炸半径邻居〔改动 def 的 caller body / callee 签名，复用文件外片段机器，受
  预算帽〕+ 跨组索引行〔§10.6 已有，只取本文件相关〕+ spec 段 + **三方契约空槽**〔`NeedKind::Api` 预留渲染位，M21d 填〕+
  笔记段〔origin = scan | needs 时渲染疑点为「一位评审者的笔记」；H0（origin = rule）无笔记段〕；所有注入段过 `neutralize_structure`）。
- 测试：禁真空（ranges 的 diff 行全在 pack 内）；笔记段中和（`</notes>` 类标记）；预算超限进 omit 清单；H0 无笔记段；契约槽空渲染不占字节。
- Commit: `feat(pack): reviewer 区域 pack——构造即切片+半径+跨组索引+spec+笔记，契约槽留空（design §7/§10.2）`

### Task B-2 `reviewer.md` 角色尾 + `build_review_prompt`
- Add: `src/prompt/templates/reviewer.md`（角色尾：`<finding_bar>` 完整 finding 版〔危害链 + 修复建议〕；措辞「确认、驳回或补充」、
  笔记非事实〔防锚定〕、H0「独立评审此区域」；`<output_contract>` = findings JSON，字段与 B-3 parse 对齐）。
- Modify: `src/prompt/mod.rs`（`ReviewPromptInput` + `build_review_prompt`：共享头 → 参考材料〔spec / 跨组索引 / 笔记〕→ 证据〔区域
  pack〕→ diff 次尾 → reviewer 角色尾；段序与 scanner 同构〔前缀稳定，铁律 3〕；`review_template_hash()`）。
- 测试：共享头字节稳定（与 scanner prompt 头逐字节相同）；角色尾含契约关键字段；模板文件字段名与 parse 一一对应（字段扫描）；模板哈希入册。
- Commit: `feat(prompt): reviewer.md 角色尾 + build_review_prompt——共享头不变、段序同构、模板哈希（design §10.7〔D9〕）`

### Task B-3 finding 契约 parse
- Modify: `src/provider/parse.rs`（`parse_review_output_with_stats(raw) -> Result<(CellOutput::Review 载荷, ReviewParseStats)>`：
  复用 `</think>` 切除 / 围栏剥离 / 配平扫描 / REPAIR 一轮〔对齐 `parse_scan_output_with_stats` 形态〕；finding 字段校验对齐现有
  `domain::Finding`〔quote 定位失败丢弃 + 计数，复用 positioning〕；needs cap 5）、`REPAIR_PROMPT_REVIEW`）。
- 测试：围栏 / 前言 / 截断 / quote 不在文件 / needs 超帽；REPAIR 一轮成功路径。**顺手**（followup 实现债）：抽 `#[cfg(test)]
  pub(crate) mod fixtures` 于 `domain/finding.rs`（`finding(..)` 三份合一）+ `NeedEntry::evidence(..)` 构造器，本批起新增字段不再逐处补。
- Commit: `feat(parse): reviewer finding 契约 parse——字段级对齐 + REPAIR 一轮（铁律 5）`

### Task B-4 SSE 流式旁路：finding 对象闭合即回调
- Modify: `src/provider/tool_loop.rs`（`ChatBackend` 加 `fn on_text_delta(&self) -> Option<Box<dyn FnMut(&str) + Send>>` 缺省 None
  ——非破坏扩展）、`src/provider/backends.rs`（anthropic `content_block_delta.text_delta` 与 openai SSE `choices[].delta.content`
  两处喂回调；回调只读累积文本，不改变请求 / 聚合行为）、`src/provider/parse.rs` 或新 `src/provider/stream_json.rs`
  （`IncrementalFindings`：喂增量文本，`findings` 数组内对象逐个闭合即 `emit(Finding)`；非 findings 段 / 思考文本容忍；未闭合对象
  discard 并计数——后接 warning）、`src/provider/fake.rs`（fake 支持逐片吐 delta 供测试）。
- 测试：合成 SSE 流三 finding 分片到达 → 三次 emit + 最终聚合一致；JSON 中途截断 → 已闭合对象保留、余下 discard 计数；与
  `parse_review_output` 最终结果一致性（旁路不改契约）；无回调 backend 行为逐字节不变。
- Commit: `feat(provider): SSE 流式旁路——增量 JSON 解析、finding 对象闭合即落盘回调、未闭合丢弃计数（design §7〔D10〕）`

### Task B-5 三标配：注入互斥、证据段预算帽、读守卫
- Modify: `src/pack/region.rs`（笔记段与 needs 证据段互斥——同一 pack 至多一个注入段；区域 pack 总字节帽复用证据段预算口径）、
  `src/cli/second_pass.rs`（读守卫机器复用：区域 pack 装配路径上的文件读取全部走守卫帽——把 `second_pass.rs` 的读帽 / 越界检查
  抽成可复用函数供 region.rs 调用，second_pass 原调用点行为不变）。
- 测试：互斥断言（同 pack 笔记 + needs 证据并存 → 构造期报错）；预算帽；读守卫命中（大文件 / 越界路径）。
- Commit: `feat(pack): reviewer 三标配——注入互斥+证据预算帽+读守卫（铁律 4，second_pass 机器复用）`

### Task B-6 收口
- 模板回归：`eval/scan_score.py` 对 PR181 基线不退化（改的是新增模板，scanner 路径零 diff 即过）。
- ship 评审（dev `redteam`）→ 修 → `--no-ff` 合回。

---

## 批 C · 深挖队列与 reviewer 运行时（`feat/m21b-deep`，Trellis `m21b-deep`；前提 A、B 已合回）

核心编排批。reviewer cell 是 finding 唯一生产者；队列是编排器确定性代码（零 token）；派发由 provider 门空位驱动。

### Task C-1 队列项与入口五态
- Add: `src/orchestrate/queue.rs`（`QueueItem { file, ranges, score, origin: Rule | Scan | Needs, notes: Vec<Suspicion>, agreement,
  state: Pending | InFlight | Settled, ... }`；`enqueue(&mut self, item) -> EnqueueOutcome` 五态：无命中 → 新项 agreement=1；
  排队中 → 并入〔异族 agreement+1、notes 追加、区间取并、重算 score，≥2 标必挖〕；在途 → 只改记录不注入不重挖；本轮已结案 → 并入
  沿用结案；跨轮判例 → 守卫接口**空过**〔判例 M21c，本批直接按无命中入队〕；合并谓词复用 `merge_suspicions`：同文件 ∧ 行重叠 ∧
  quote 相等或不琐碎重叠〔hint 只辅助，R4〕；同家族重复只合并不加 agreement）。
- 测试：五态各一构造用例；agreement 活值重算优先级；同家族不加；跨文件不合并。
- Commit: `feat(orchestrate): 深挖队列项与入口五态——merge_suspicions 谓词复用、agreement 活值（design §6 规则 2〔D13〕）`

### Task C-2 优先级、预算与必挖集
- Modify: `src/orchestrate/queue.rs`（优先级键：主区 H0 最高 → 涟漪 → 疑点 `confidence × agreement × 类别权重` → needs 中档；
  **非 FIFO、不用置信阈值切**；预算 = A-3 `budget_cells`；必挖集 = 主区 H0 ∪ {agreement ≥ 2} **不截**〔confidence 只进优先级不做
  门〕；保底 = 疑点保留 ⌈maxCells/4⌉ 格，主区吃穿时帽 = 主区 + 保底并披露；graceful drain：到期后在途跑完、未开跑者标
  `skipped(budget)`；`maxCells = 0` 时 H0 仍被挖〔构造验收〕）。
- 测试：优先级排序快照；必挖不截（0 预算用例）；保底配额；drain 后 skipped 项清单。
- Commit: `feat(orchestrate): 优先级键+预算+必挖集不截+保底配额+graceful drain（design §6 规则 1/3/4〔D11〕）`

### Task C-3 门驱动派发器与 reviewer cell 运行时
- Add: `src/orchestrate/queue.rs` 派发器（tokio mpsc：scanner cell 为 sender、reviewer 为消费者；**单派发器**按 reviewer 条目所在
  provider 门空位弹出最高优先项——`registry.lease()` 天然等待门位，派发循环 = 「取空闲 reviewer 条目 × pop 最高项 × lease」；无
  worker 数〔D13〕；t=0 入队序 scanner 先于 H0〔§12.3 最长链优先〕；scan 来源项不等同组兄弟 cell〔先到先挖、后到并入记录〕）、
  `src/orchestrate/review_shot.rs`（`run_review_shot`：`build_review_prompt` + B-4 `on_text_delta` 挂 `IncrementalFindings` →
  finding 闭合即 `store.append_partial` 流式落盘 + 发射 `FindingEvent`〔`tokio::mpsc`，批 D 的接口：finding + 落地时刻 agreement +
  reviewer family〕→ `parse_review_output` 终判；usage / attempts / queue_wait_ms 对齐 `ScanCall` 口径）。
- Modify: `src/orchestrate/mod.rs`（`run_code_review` 接线：`run_scan_fanout` 每疑点 / needs 入队〔scan cell 完成回调里〕、
  A-4 的 H0 t=0 入队；reviewer cell 记录 `CellOutput::Review` 落 `cells` 表〔role=reviewer〕）。
- **顺手项**（followup 已归 M21b）：① Anthropic thinking×tools 守卫——`build_anthropic_tools_body_ext` 按 tools 非空守卫
  （thinking{enabled} 与工具往返同发会 400，reviewer 工具循环走 anthropic kind 时不可达）；② reviewer needs 二阶段（R2，design
  §11「needs 二阶段补查、two-stage 语义沿用」）——`--tools <off|two-stage|on>` 首个消费者，复用 `second_pass.rs` 机器，触发率入
  RunMeta 供 R2 判决；③ `AttemptPolicy` Option 穿三层重构（D13 收口批评审延后项）——旧退避提成 `BudgetBackoff` 策略，
  `retry_classified(budget, &dyn AttemptPolicy, op)` 单路径，reviewer / verifier 接入时一并改。
- 测试：FakeChatBackend 多 finding 流 → 全部落盘不丢；派发序 = 优先级 × 门空位；scanner 完成后疑点入队 → reviewer 消费；
  慢模型超时 → 已闭合 finding 保留、cell 记失败；needs 二阶段触发与触发率计数；anthropic body 带 thinking 时 tools 守卫断言。
- Commit: `feat(orchestrate): 门驱动派发器+reviewer cell 运行时——流式落盘+FindingEvent 发射（design §6 规则 5/§7）`

### Task C-4 barrier 与 reduce 接线
- Modify: `src/cli/mod.rs`（barrier：scanner 全完 ∧ 队列空 ∧ 在途 0 ∧ 复核在途 0 → reduce：`consolidate::dedup`〔已有〕→ 报告；
  verdict 从 findings 重建〔blocking 判定沿用 `verdict.rs`〕；报告 = findings + 疑点低置信披露〔`deep: skipped(budget)` 项不静默〕；
  scan-only 旧报告形态保留为「无 reviewer 条目时」的退化路径；**exit_code 第二参**：failed_models 语义在 reviewer 失败带债 approve
  下定夺后收敛签名或保持〔followup 行〕）、`src/domain/report.rs`（`DeepMeta { queue_depth_peak,
  enqueue_reasons: BTreeMap<origin, u32>, dedup_hits, budget_skipped, item_wait_ms: Vec<u64>, dispatch_agreement, landed_agreement }`）。
- 测试：Fake 全流程 e2e（2 组 × 2 scanner → 疑点入队 → 2 reviewer cell → findings → 报告）；无 reviewer 配置 → scan-only 退化；
  reviewer cell 失败 → 带债 approve + coverage failed。
- Commit: `feat(cli): barrier 语义+reduce 接线——findings 报告重建、预算截断披露不静默（design §2/§6 规则 6/9）`

### Task C-5 Phase Deep 与 resume 队列快照
- Modify: `src/state/mod.rs`（`Phase::Deep` / `Phase::Verify`〔serde lowercase；旧 `Refute` 保留可读，新作业不再打〕；
  checkpoint：`queue_items` 表写队列快照〔item JSON + state〕，每项状态转换时更新）、`src/cli/mod.rs`（`run_resume` 扩展：
  读快照 → Settled 项沿用已落盘 finding〔不双计〕、Pending/InFlight 重跑〔`run_review_shot` 重入〕；scan 记录沿用现有差集重放；
  **registry / RunMeta.models 按快照角色条目重建**〔followup：批 A 删 `--models` 后 args.json 模型集不再是权威源〕）。
- 测试：kill 模拟（构造部分快照）→ resume 续跑 → finding 不丢不双计；未闭合对象丢弃计入 warning；旧格式作业（无快照）→ 按现有
  scan-only resume 语义。
- Commit: `feat(state): Phase Deep/Verify+队列快照 checkpoint+resume 读快照重跑（design §17 R14〔D11〕）`

### Task C-6 观测收口
- Modify: `src/domain/report.rs` / `src/cli/mod.rs`（RunMeta：队列深度峰值、入队原因分布〔rule/scan/needs〕、去重命中、预算截断数、
  项等待时长、每 reviewer cell 输出 token〔< `maxOutputTokens`/2 的有界性口径〕、复核触发 / 跳过数〔批 D 填 refute 率〕、
  per-provider 每角色在途峰值〔`gate_stats` 已有，按角色分列〕、门等待时长、429 收敛门宽〔已有〕、派发时 / 落地时 agreement）。
- 测试：RunMeta 快照含 deep 段全字段。
- Commit: `feat(report): 深挖观测收口——队列/预算/等待/agreement 全链入 RunMeta（design §6 观测面）`

### Task C-7 收口
- 本仓 dogfood：`redteam-next review --base dev`（或最近提交）端到端产 finding（真 provider，配置 reviewers 档）。
- ship 评审（dev `redteam`）→ 修 → `--no-ff` 合回。

---

## 批 D · verifier 流式段（`feat/m21b-verifier`，Trellis `m21b-verifier`；前提 C 已合回）

### Task D-1 `verifier.md` 拆分
- Add: `src/prompt/templates/verifier.md`（从 `adversarial-refute.md` 拆 verifier 角色尾：`analysis` 先于 `verdict` 且「不可撤回」；
  「找出这条 finding 不成立的证据」；输出契约 `REFUTED | UPHELD` + 理由 + analysis；共享头沿用）。
- Modify: `src/prompt/mod.rs`（`build_verify_prompt`：finding + ±60 快照〔`verify.rs` 的 `source_snippet` 机器〕+ 判例摘要槽〔M21c
  空〕+ spec；`verifier_template_hash()`）、`src/prompt/templates/adversarial-refute.md` 删除或降级注明（最后引用者消失）。
- 测试：共享头字节稳定；analysis 先于 verdict 的段序断言；模板哈希。
- Commit: `feat(prompt): verifier.md 角色尾拆分——analysis 先于 verdict、怀疑者立场（design §8.1/§10.7）`

### Task D-2 触发集、跳过与 verifier 选择
- Add: `src/consolidate/verify.rs` 扩展（触发：`severity ≥ block ∧ confidence ≥ floor` + 判例变体槽〔枚举预留，M21c 填〕；
  触发集消费 `--block-threshold` / `--conf-floor` 旗〔M21a 无消费者旗，`--help` 文案届時改回〕；跳过：两异族 scanner
  标记〔finding 区域 agreement ≥ 2，按**落地时刻**值，后涨不回溯〕∧ reviewer 确认；verifier 选择：
  verifiers 活跃集〔拥有 `verifier` 角色的模型，D12〕非空则从中选；缺省自动异族〔优先 reviewers 档 → scanners 档；`family` 缺省
  model_id 首段〕；无异族 → 同族 + warning「无异族 verifier」；`review.verifiers.enabled = false` → 全跳过〔记录 skip 理由〕）。
- 测试：触发 / 跳过判定表；异族选择序；全 deepseek 构造 → 同族 + warning；enabled=false。
- Commit: `feat(consolidate): verifier 触发集+跳过规则+异族自动选择（design §8.1〔D7〕）`

### Task D-3 流式消费与 refute 率
- Modify: `src/consolidate/verify.rs`（改 `FindingEvent` 流消费者：finding 落地即入复核流〔与深挖尾部重叠〕，消费循环经
  `registry.lease()` 走同一 provider 门；verdict parse 对齐契约；`REFUTED` → confidence 归零、永不 blocking；`UPHELD` 原样；
  调用失败 → 保守 upheld + warning；refute 率 = refuted ÷ 触发复核数 → RunMeta `refute_rate`；复核在途计数接入 C-4 barrier）、
  `src/cli/mod.rs`（`Phase::Verify` 打点；复核触发 / 跳过数入 RunMeta）。
- 测试：Fake e2e——finding 落地 → verifier 消费 → refute 链路；失败保守 upheld；barrier 等复核在途归零。
- Commit: `feat(consolidate): verifier 流式消费——finding 落地即复核、失败保守 upheld、refute 率入 RunMeta（design §8.1）`

### Task D-4 离线回归集与基线
- Add: `eval/verify_score.py`（读 `eval/fixtures/ledger-{dismissed,upheld}.jsonl` → 逐条构造 finding + 快照 → 调
  `redteam-next verify`〔或 review 内嵌路径的只读出口〕→ 二分类准确率 / 混淆矩阵；`--baseline` 对比）。
- 手动：跑一次全量 fixtures 记基线数字（真 provider，落 `.next/private/m21b/verify/` + research 引用）；改 verifier.md 必跑。
- Commit: `chore(eval): verify_score.py——dismissed/upheld 二分类离线回归与基线（design §10.7〔D9〕）`

### Task D-5 收口
- 模板回归：verifier 基线不低于本批首跑；scanner 路径零 diff。
- ship 评审（dev `redteam`）→ 修 → `--no-ff` 合回。

---

## 出口验收（本工作树；`BK=<PR181 工作树绝对路径，只记 .next/private/>`，数据落 `.next/private/m21b/`）

| # | 验收 | 命令 / 数据 | 门槛 |
|---|---|---|---|
| 1 | PR181 端到端 | `redteam-next review --base a7c827b70 --format json` ×3（reviewers 档配 GLM 5.3 或实测最稳档） | findings oracle 命中 ≥ GLM 单发 6 条；低风险 PR 5-8 min、高风险 7-10 min（design §13） |
| 2 | dev 对照（R6 销案） | dev `redteam` 与 `redteam-next` 并排跑 PR181，比 P0 oracle 22 条命中 | `redteam-next` 命中 ≥ dev；截断数 / 台账落入数 / 保底格使用数入 RunMeta 供三档起步值回校 |
| 3 | 预算与必挖 | 构造 `maxCells = 0` 与小帽用例 | 截断项 100% 出现在 RunMeta；必挖集从不被截（0 时 H0 仍被挖） |
| 4 | verifier | PR181 全程 | blocking finding 全有复核记录或跳过理由；全 deepseek 构造 → 同族 + warning；refute 率可读；复核尾部 ≤ 1 min |
| 5 | provider 门 | `maxConcurrency = 1` 构造；同 provider reviewer+scanner 门宽 < cell 数构造；PR181 全程 | 同 provider 串行可观测；入队序 scanner 先可观测；无 429/503 或有 429 时门宽收敛、cell 零失败 |
| 6 | kill → resume | 后台 job 中途 kill | 从队列快照续跑；已落盘 finding 不丢不双计；未闭合对象丢弃计入 warning |
| 7 | 输出有界性 | PR181 RunMeta 每 reviewer cell 输出 token | 全部 < `maxOutputTokens`/2；FINALIZE/REPAIR 三层不触发 |
| 8 | 回归集 | `eval/risk_replay.py compare`；`eval/verify_score.py`；`eval/scan_score.py` | risk 对拍一致率与 oracle 命中不低于 P0 基线；verifier 二分类基线数字入 research；scanner 不退化 |
| 9 | 门与文档 | `make verify`；`redteam-next --help` | 绿；含 `risk` 子命令与深挖说明；无 `engine` / `--risk` / `--models` / `--workers` |

出口通过后：roadmap `M21b-risk-deep-queue.md` 交付项打勾、状态 ✅、README 表更新；三档预算与 refute 率数字写进
`../private/research/`（或新 research）；主 checkout `.doc/roadmap/done.md` 记 M21b。

## 风险与回滚

- **派发器是全新并发核心**：C-3 先用 Fake 多轮压测（乱序完成 / 慢 consumer / 429 注入）再接真 provider；死锁退路 = 派发循环超时
  报警 + graceful drain 兜底。
- 流式旁路（B-4）必须证明「无回调时逐字节不变」，否则 cache 与对拍基线全废——回归锚在 B-4 测试。
- 慢模型 per-turn 死亡只丢未闭合对象（流式兜底），无旋钮；死亡率高调 `perTurnTimeoutSecs`（followup 记录，不加新旋钮）。
- 预算三档是起步值（E1 §15.7），出口 2 的 RunMeta 数据回校；形状不依赖再调。
- 队列合并 hint 归一化过松会误合并：quote 主锚、hint 辅助（R4），M21c 台账数据再校。
- 无候补下 provider 抖动 = coverage `failed`：先调在途上限与退避，再看失败率（M21a D13 经验已内建）。
- 子分支回滚 = 不合回；合回后回滚 = revert 该 `--no-ff` merge；proj.db schema 不变（v4 表壳已建，只开始写）。
