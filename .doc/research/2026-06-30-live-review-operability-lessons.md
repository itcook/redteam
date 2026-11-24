# 一线运维教训：一次 19 轮真实 codex 对抗评审 → breakpoints 可靠性补充建议

> 状态：研究/建议（对 `2026-06-25-breakpoints-adversarial-review-design.md` 的运维侧补充）
> 日期：2026-06-30
> 数据来源：在另一仓（hiapi/new-api，Go）用 `until-codex-approve` + codex 对一个真实 PR（generic async task 失败可观测性）做的 **19 轮**对抗评审全过程（rounds 1–19，其中 8–19 为作者亲历逐轮操盘）。
> 关联：[`codex-adversarial-review-harness.md`](./codex-adversarial-review-harness.md)（机制逆向，「重建前」）；本文是「重建中实跑 codex 暴露的运维问题」，正交于前者。

---

## 0. 一句话结论

设计文档（含 Phase 2 §19）在**架构与精度**上已经很到位——多模型 + targeted-verify + 文件包 + positioning，这几条恰好被本次 19 轮亲历**反向印证**为对的方向。但设计与现有实现的**最大缺口在「可靠性/可运维性」**：现有 `brk` 复刻了 codex 让我最痛的那一类故障（**进程永久卡死 / 崩溃即失忆 / 中途零持久化**），而这一类正是「实跑很多轮」才会暴露、设计阶段最容易低估的。本文给出按优先级排序的补丁建议，**P0 三条**（HTTP 超时 + job wall-clock deadline + 心跳/last_active）直接决定工具能不能稳定无人值守地跑循环。

---

## 1. 为什么这个视角补设计的盲区

设计与计划回答的是「**应该长什么样**」；本文回答的是「**实跑很多轮会怎样坏**」。后者只有真的连跑十几轮、亲手处理每一次卡死/重连/误报才拿得到。本次 19 轮里：

- **codex 单轮评审耗时 11–34 分钟**，不可预测；
- **约一半轮次的「首发」在 turn 收尾处永久卡死**（log 不再增长、job 永远停在 running），必须人工 `cancel` + 重启；
- **多轮遇到 codex「Reconnecting… 1/5…4/5 → Turn interrupted → failed」**（服务端/网络抖动），整轮作废重来；
- 我被迫**用「轮询 job 状态 + 监测 log 文件 mtime（冻结 >5min 即判定卡死）」这套土办法**去区分「在跑」和「卡死」——因为 codex 不给可靠的活性信号。

这套土办法本不该由调用方发明。下面每条建议都标注：**现象+本 session 证据 / 设计与实现现状 / 建议**，并落到设计 § 与 `src/` 文件。

---

## 2. 一线痛点总览

| # | 痛点 | 本 session 频次/证据 | 现有 brk 是否已防 | 建议 |
|---|---|---|---|---|
| P-1 | **进程永久卡死**（turn 收尾冻结，job 永停 running） | 约 6–7 次首发卡死需 cancel+重启（r9/r15/r16/r17/r19…） | ❌ 三风险叠加，完全未防 | A（R1.1–1.3）**P0** |
| P-2 | **崩溃/卡死即失忆**，已产出的 findings 全丢 | codex interim 已现 findings/假设（如「6 层编码」），收尾卡死后全废、20–34min 重来 | ❌ 零中途持久化；崩溃后 prune 直接删文件 | B（R2.1–2.2）**P0/P1** |
| P-3 | **错误不分类 → 重试失灵 + 重连风暴** | 「Reconnecting…4/5 → failed」整轮作废 ×多次 | ⚠️ 重试存在但粒度太粗、不分类、不读 Retry-After | C（R4.1–4.3）**P1** |
| P-4 | **无可靠终态/进度信号**，status 形同虚设 | `--wait` 超时(124)但 job 仍 detached 跑；单 job 与 --all 的 JSON 结构不一致；result 未终态时为空 | ❌ NDJSON 进度是死代码，status 永远 0/0 | D（R3.1–3.3）**P1** |
| P-5 | **单模型→误报假设无法证伪** | codex interim 抛「6 层编码绕过」假设，我跑 payload **实证为误报** | ✅ 多模型 + targeted-verify 已设计/实现 | E（R5）**已对，强化 R6** |
| P-6 | **无执行验证→模型「推断」运行时行为** | codex 自述「只读沙箱阻止跑测试」→ NUL/PG-JSONB finding 是「我推断」 | ❌ 只读 3 工具，无 run/exec | E（R6.1）**P2，高杠杆有争议** |
| P-7 | **收敛慢/打地鼠**（多轮同根因变体） | r12–r18 共 6 轮几乎全是 signed-URL/编码变体；我把修法从「枚举」改「归一化」才收口 | ⚠️ 部分靠 loop SKILL；prompt 可强化 | E（R7.1–7.2）**P2** |
| P-8 | **跨轮记忆/驳回手工回喂** | 每轮手工把 dismissed（裸 reload、6 层编码）写进 `--focus` 防重提 | ⚠️ 有稳定 `id`，但基于行号、跨 fixup 易碎 | F（R8.1–8.2）**P1** |
| P-9 | **上下文失控引发卡死** | 早期允许 codegraph + 宽读 → 卡死；被迫禁 codegraph、点名文件、"立即输出 JSON" | ✅ 文件包 + 读截断已是正解 | E（R9）**设计已对，背书** |
| P-10 | **结果解析脆弱** | codex 结果嵌 `storedJob.result.result`，字段 `body` 非 `description` | ✅ 扁平 versioned schema 已设计好 | D（R10.1）**已对** |

---

## 3. 痛点 → 建议（分主题）

### A. 永久卡死与 worker 收敛 —— 最高优先（P0）

**现象+证据**：codex 最折磨人的不是「跑得慢」，而是**卡死后伪装成「在跑」**。job 状态停在 `running`，但 log 文件 mtime 不再前进；`--wait` 这边超时退出（exit 124），detached 的 job 却还挂着。我只能靠**轮询 + 监测 log mtime（连续 ~5min 不动判定 HUNG）→ cancel → 重启**。19 轮里这套救援跑了 6–7 次。

**现状（Explore 实证）**：`brk` **原样复刻了这个最坏故障，且三处风险叠加**：
1. `src/provider/{backends,direct}.rs` 全用 `reqwest::Client::new()`，**无 `.timeout()`/`.connect_timeout()`**，无整体 deadline —— 单个挂死连接**永久**阻塞该 turn；
2. fan-out 是 `futures::future::join_all`（`orchestrate/mod.rs`）—— 一个 cell 永久阻塞 → 整个 gather 永久阻塞 → worker 永停 `Running`；
3. `JobState` 无心跳/`last_active`，`prune`（`state/store.rs`）只回收 **pid 已死**的作业 —— **「进程活着但挂死」的 worker 永远不会被回收**。
   工具循环虽有 `DEFAULT_MAX_ITERS=12` + 强制收尾轮（`tool_loop.rs`），但那只防**轮数**不防**时间**：无 per-turn 超时时，单轮就能永久挂起，轮次上限形同虚设。

**建议**：

- **R1.1（P0）每请求 HTTP 超时 + 每 turn deadline**。`reqwest::ClientBuilder` 设 `connect_timeout`（如 10s）+ `timeout`（如 120s/turn），并在每次 HTTP 调用外再包一层 `tokio::time::timeout`（整体 turn deadline）。**这是单点最高价值的修复**——没有它，重试、max-iters、watchdog 全是空中楼阁，因为一个 wedged 连接能让一切阻塞到天荒地老。codex 的「卡死」本质就是缺这一条。
- **R1.2（P0）job 级 wall-clock deadline + watchdog**。给每个 job 配一个总墙钟上限（依本次实测 11–34min，默认建议 30–45min，可配），到点 worker **自我中止**并写终态（`timed_out`/`failed`）**外加已 checkpoint 的部分结果**（见 B）。再加一个基于心跳的 wedge 检测：心跳停滞超阈值即判 worker 假死。
- **R1.3（P0）心跳 / `last_active` 时间戳**。`JobState` 增 `last_active`，每完成一个 cell/turn 就原子刷新。这正是我手工用 log mtime 重建的信号——`brk` 应原生暴露，让 `status` 与任何上层 supervisor 能**一眼区分「慢」与「死」**，不必再发明 mtime 探测术。

> 这三条合起来正面消解 Explore 总结的「三个相互叠加的永久 Running 风险」。**若只做一件事，做 R1.1。**

### B. 中途持久化与崩溃可恢复（P0/P1）

**现象+证据**：codex 的 interim「Assistant message captured」清楚显示它**中途已经有了 findings / 假设**（如那条「6 层编码」假设、以及多条已成形 finding），但**收尾一卡死，这 20–34 分钟的产出全部丢弃**，只能从头重跑。多模型并行时这种浪费会被放大 N 倍。

**现状（Explore 实证）**：`run_job`（`cli.rs`）只在 `run_review_dispatch` 返回**完整** `Ok(report)` 后才写 `result.json`；`join_all` 是 all-or-nothing；**进程死亡 = 全部 findings 丢失**。更糟：worker 被杀后 `prune` 直接 `remove_job_files` **删掉整套文件**（而非转 `Failed`），且只在别的命令偶发触发时懒惰执行——**崩溃证据直接消失**。

**建议**：

- **R2.1（P0）增量 finding checkpoint**。把 `join_all` 改为「**完成即落盘**」：每个 (bundle×model) / (lens×model) cell 一产出 raw findings 就立刻 append 到 job 目录（一个 results NDJSON）。因为 Phase 2 的 cell 本就是隔离上下文，天然适合 streaming 持久化。这把「丢全部」降为「最多丢正在飞的那个 cell」。
- **R2.2（P1）终态失败也要出「部分报告」**。job 进入 `failed`/`timed_out`/`cancelled` 时，仍写 `result.json`，`verdict: incomplete` + 已 checkpoint 的 findings + `run.warnings: ["partial: N/M cells done"]`，**绝不静默删除**（改掉现在 prune 删文件的行为：崩溃作业应转 `failed` 并保留原因，而非蒸发）。上层循环 SKILL 据此决定「用部分结果」还是「只补跑缺失 cell」——后者是巨大的效率红利：cell 9/10 卡死不该让 cell 1–8 陪葬。

### C. 错误分类与重试粒度（P1）

**现象+证据**：codex 多轮死于「Reconnecting… 1/5 … 4/5 → Turn interrupted → failed」——服务端/网络抖动，**整轮评审作废重来**。一次瞬时抖动不该让 30 分钟的工作清零。

**现状（Explore 实证）**：所有 HTTP 错误经 `error_for_status()` 后塌缩成单一 `ProviderError::Unavailable(String)`（`backends.rs`/`direct.rs`），429/5xx/4xx/超时/DNS 不分。`call_with_retry`（`orchestrate/mod.rs`）退避 `200*2^attempt`、默认 2 次，但：① 不读 `Retry-After`；② 401/400 等不可重试错误也照退避；③ **重试粒度是整个 LensCall**——第 7 轮 HTTP 失败会**从头重跑整个工具循环**（重新 read/search 全部上下文），昂贵且仍可能再撞抖动。

**建议**：

- **R4.1（P1）provider 错误分类**。`ProviderError` 拆出 `RateLimited{retry_after}` / `ServerError(5xx)` / `Timeout` / `Network`（可重试）vs `Auth(401)` / `BadRequest(400)` / `ContextTooLong`（**不可重试，立即 fail**）。只重试可重试类、尊重 `Retry-After`。别在 400 上烧 3 次退避。
- **R4.2（P1）在「turn」而非「整个循环」粒度重试**。带着已累积的 message 历史重试单次 HTTP turn，让一次瞬时抖动不丢整个循环的已读上下文。codex 的「reconnect 风暴」杀掉整轮，正是因为没有 turn 级续跑；有了它本可扛过去。
- **R4.3（P1）整模型失联 → 优雅降级而非整 run 失败**。多模型设计已有 `failed_models`（§8.1）；确保它对**循环中途**的模型死亡也鲁棒（不只调用入口），让一个模型崩溃降级为「其余模型的部分结果」，run 继续出。

### D. 终态信号 / 进度 / status 契约（P1）

**现象+证据**：我无法信任 codex 的 status——`--wait` 超时但 job 仍 detached 跑；单 job 查询与 `--all` 的 JSON 结构不同；job 未终态时 `result` 返回空。只能靠轮询 + log mtime 自己拼。

**现状（Explore 实证）**：`state/store.rs::append_log`（NDJSON 进度）**是死代码**（仅自身单测调用）；`progress.done/total` 生产路径从不更新，`brk status` 永远显示 `0/0`。设计 §6/§458 要求「每完成一个 lens×model 写一条 NDJSON」——**未实现**。

**建议**：

- **R3.1（P1）把 NDJSON 进度真正接线**。每 cell 完成 append `{ts, event, done, total}` 并原子更新 `progress` 与 `last_active`（与 R1.3 同源）。设计已规约，只差实现——这让 `brk status` 从摆设变成上层循环可依赖的活性信号。
- **R3.2（P1）统一、稳定的 status 契约**。`status <id> --json` 与 `status --all --json` **共用同一 job schema**（我被 codex 的双 schema 坑过），含 `last_active`/`progress`/`phase`/`elapsed_ms`；文档化「result 仅终态可取」规则。
- **R3.3（P1）`--wait` 不许「说谎」**。若 `--wait` 命中客户端超时，**要么完全拥有该 job（超时即 kill）、要么返回 job-id 让调用方接管轮询**——绝不留一个调用方看不见的 detached job（codex 的 exit-124-but-job-continues 是我踩过的雷）。

### E. 收敛效率：交叉验证 + 执行验证 + 类级 prompt

#### R5（已对）多模型 + targeted-verify 被本 session 实证为对的方向

codex interim 抛出过一条「6 层百分号编码可绕过 5 层解码」的**高危假设**——它**没法自己证伪**（只读沙箱、纯推断），presented as concern。我跑了 6 层 + 混合编码 payload，**实证其为误报**（解码后 url.Parse 取不到 host → 整段 redact）。这正是设计 §19.6 **第 3 层定向 refute** 要守的场景：`agreement==1` 的孤证 + 会触发 reject。**设计对了**。多模型 replicate + agreement 也被 r12–r18 的「单模型串行找变体」反证为有价值（第二个模型可能一次surface多条或当场驳回误报）。

#### R6（P2，高杠杆但有争议）给 targeted-verify 加一个「可选、强沙箱、执行级」核实层

**这是我最想强调的一条**：本 session 里**最有决定性的核实，全部来自「跑代码」而非「再问一个 LLM」**——
- 6 层编码假设：我**跑 payload** 证伪；
- NUL/PG-JSONB finding：codex 自述「只读沙箱阻止跑测试」，只能「**我推断**」；我**跑测试**证实并修复；
- 协议相对 signed URL、编码 scheme：都是**我写 payload 跑 SanitizeUpstreamSnippet** 才确认真伪。

现有 brk 只读（`tools/mod.rs` 仅 `read_file`/`search_repo`/`list_files`），所谓 verify 是另一个 LLM 的 refute——**它一样会推断**。建议：在 §19.6 的尾段 verify **第 3 层之上**，加一个**可选、默认关、重度沙箱化**的执行核实：仅对「会触发 reject 的孤证」、且仅当项目显式提供 `verify_cmd` 时，允许在隔离沙箱里跑模型给出的最小复现 payload / 或跑项目自带测试，把「模型猜」升级为「执行判」。

- **务必正视取舍**：跑模型产出的代码很危险（需强沙箱：网络隔离、只读 FS+临时可写、资源/时间上限），与设计「纯静态二进制」气质相悖。故定位为**opt-in 的独立 tier**，**不**进默认评审 fan-out（保持「默认静态评审」纯净），只在 verdict-blocking 孤证的尾段、用户授权时启用。
- **更轻的替代**：`run_existing_tests`（只跑项目**自己**的测试套件，不执行模型新写代码）安全得多，能验「该 finding 的断言在现有测试下成立吗」；但证伪「绕过类」往往需要**新 payload**，故两者都列。

#### R7（P2）类级发现 + 属性/fuzz 验证 —— 终结「打地鼠」

**现象+证据**：r12–r18 共 6 轮几乎全是同一根因（signed-URL/百分号编码脱敏）的**狭窄变体**：r12 URL 编码已知凭证 → r13 JSON-escaped `\/` → r14 整段百分号编码 → r15 双重/混合大小写 → r16 编码 scheme → r18 协议相对。单模型每轮只找「下一个变体」，我每轮修一个。**真正收口是 r15 我把修法从「枚举变体」改成「归一化（迭代解码 + 逐三元组大小写无关）」**——一个泛化的 normalizer 顶掉了后续一堆变体。一个会「找类不找例」或「对脱敏器跑 fuzz」的评审，本可 2 轮收敛而非 7 轮。

**建议**：

- **R7.1（P2）prompt 要类级**。攻击面模板里，对「输入处理绕过」类 finding，要求模型**刻画通用类 + 最强变体（最深编码 / 混合大小写 / 替代 scheme / 协议相对）**，而非只报一个实例，并把「修复应是 normalizer，不是逐 case」写进 recommendation。一条「这是一类，修法必须归一化」的 finding 能折叠掉 N 轮变体。（§16 prompt 复用面。）
- **R7.2（P2）对「绕过类」finding 上属性/fuzz 验证**（配 R6 执行层 / 或 loop SKILL）。脱敏/解析/校验绕过，最高信号的核实是**在输入空间上 fuzz**，而非单 payload：既确认类、又一次性扫出**所有**变体，从根上终结打地鼠。这条与 R6 互补——我的 7 轮，正是在「改成归一化」那刻坍缩的；一个 fuzzer 会逼着第 1 轮就归一化。

#### R9（设计已对，背书）文件包 + 读截断正是「防卡死」的正解

早期我允许 codex 用 codegraph + 宽读时频繁卡死，被迫**禁 codegraph、点名文件、要求"立即输出 JSON"、禁读大 model/CAS 文件**。设计 §19.1「扇出轴应是代码本身、**缩小每个 subagent 上下文**」+ §19.5 文件包 + `tools/mod.rs` 的读截断（64KB/500 行/100 命中），正是这套手工约束的工程化。**强力背书**。补一句 **R9.2**：每个 cell 应有硬 token+turn+**time** 预算，命中即强制收尾该 cell（出已有 findings）而非放飞——time 预算并入 R1。

### F. 跨轮记忆：quote 锚定的 id + 结构化驳回（P1）

**现象+证据**：我每一轮都**手工**维护「驳回清单」并塞回 `--focus`（"以下已核实为误报，除非有新证据勿再提：①裸 task_id reload owner …②6 层编码…"）。不回喂，codex 就重提已驳回项。19 轮重复了 19 次这套体力活。

**现状（设计/Explore）**：设计正确地把循环留给外层 SKILL（§17），并用稳定 `id` 支撑驳回清单。但 **`id = hash(file+line+title)`（`hash.rs`/`finding.rs`）基于行号**——跨 fixup 提交行号一漂移，同一逻辑 finding 就换了 `id`，**驳回清单失效、误报重新冒头**。这正是我被迫手工回喂的根因。

**建议**：

- **R8.1（P1）`id` 改为 quote 锚定、行无关**。Phase 2 已有 `quote` + positioning，把稳定 id 算成 `hash(file + normalized_quote + normalized_title)`（引用锚定、与行号解耦），让一条被驳回的 finding **在周边代码位移后依然保持同一 id**、依然被驳回。这是一条**具体、低成本、由我亲历直接催生**的 schema 修正。
- **R8.2（P1）驳回结构化输入 + 升级刹车信号**。给 brk 一个 `--dismissed <ids|file>`（而非只塞 prose `--focus`），命中 dismissed id 的 finding 由 brk **预过滤或标 `previously_dismissed: true`**，循环 SKILL 不必每轮重新 litigate；dismissed id 再现即触发设计 §17 的升级刹车（交用户裁决）。把我手工干了 19 次的事自动化。

---

## 4. 设计已经对的地方（本 session 实证背书）

不全是批评——这次亲历**反向验证**了设计的几个关键判断：

- **多模型 + agreement + targeted-verify（§8/§19.6）**：单模型会抛无法自证的高危假设（6 层编码），孤证定向 refute 正是解药。✅
- **文件包缩小上下文（§19.1/§19.5）**：宽读是卡死的诱因，bounded per-cell context 是正解。✅
- **positioning/quote 锚定（§19.6）+ 幻觉过滤**：codex 偶有「引用对不上行号」的漂移，确定性 quote→行锚定能免费过滤。✅
- **确定性 verdict（§10）**：稳定的二值退出信号是外层循环能自动化的前提；本 session 的整个 until-codex-approve 循环就靠它。✅
- **扁平 versioned 输出 schema（§11）**：codex 的 `storedJob.result.result` 嵌套 + `body`/`description` 不一致让我解析得很狼狈；brk 的扁平 `schema_version` 是对的。✅（仅补 R10.1：明确 result 仅终态可取 + `incomplete` verdict 表达部分结果。）

---

## 5. 优先级建议速查

| 优先 | 建议 | 落点（§ / src） | 一句话 |
|---|---|---|---|
| **P0** | R1.1 每请求 HTTP 超时 + per-turn deadline | `provider/backends.rs`,`direct.rs` | 单点最高价值；无它一切防卡死皆空 |
| **P0** | R1.2 job wall-clock deadline + watchdog | `state/`,`cli.rs::run_job` | 到点自我中止 + 出部分结果 |
| **P0** | R1.3 心跳 `last_active` | `state/job.rs` | 原生区分「慢 vs 死」，免发明 mtime 探测 |
| **P0** | R2.1 增量 finding checkpoint | `orchestrate/mod.rs`,`cli.rs` | 完成即落盘，崩溃只丢 in-flight cell |
| **P1** | R2.2 终态失败出部分报告（不删文件） | `state/store.rs::prune`,`cli.rs` | 崩溃转 failed + 保留原因/部分 findings |
| **P1** | R4.1/4.2 错误分类 + turn 级重试 | `provider/mod.rs`,`orchestrate` | 抖动可续跑；不在 400 上烧退避 |
| **P1** | R3.1–3.3 接线 NDJSON 进度 + 统一 status + `--wait` 不说谎 | `state/store.rs`,`main.rs` | 让 status 真正可用 |
| **P1** | R8.1 `id` 改 quote 锚定（行无关） | `hash.rs`,`finding.rs`,`positioning/` | 驳回清单跨 fixup 不失效 |
| **P1** | R8.2 `--dismissed` 结构化 + 升级刹车 | CLI / loop SKILL（§17） | 自动化我手工干 19 次的回喂 |
| **P2** | R6 可选强沙箱执行级 verify | `tools/`,`consolidate/verify.rs` | 「模型猜」→「执行判」（高杠杆、有争议） |
| **P2** | R7.1/7.2 类级 prompt + fuzz 验证 | `prompt/`,verify/loop | 终结打地鼠（我 7 轮坍缩成 1 的教训） |

---

## 附：本 session 19 轮事实速查

- **轮次/findings 轨迹**（fixed/dismissed）：r8:1 · r9:5 · r10:1 · r11:1 · r12:2 · r13:2 · r14:1 · r15:2 · r16:2 · r17:4 · r18:1 · r19:进行中。累计约 22 条，~21 修复 + 1 驳回（裸 reload owner）+ 1 实证证伪假设（6 层编码）。
- **耗时**：单轮完成 11–34 分钟；多轮首发卡死/重连失败需 cancel+重启（r9/r15/r16×2/r17×2/r19…）。
- **打地鼠区间**：r12–r18 共 6 轮为 signed-URL/编码脱敏的狭窄变体；r15「改枚举为归一化」后才实质收口。
- **救援土办法**（本不该由调用方发明，对应 R1.3/R3）：轮询 `status` + `stat -f %m` 监测 log mtime，连续 ~5min 冻结判 HUNG → cancel → 极简 FOCUS 重启（禁 codegraph、点名文件、禁读大文件、"立即输出 JSON"）。
- **codex 不稳定证据**：turn 收尾卡死（log 冻结、job 永停 running）、`Reconnecting…N/5 → Turn interrupted → failed`、`--wait` exit 124 但 job 仍 detached、单 job 与 --all 的 status JSON 结构不一致、result 未终态为空。

> 这些「证据」逐条对应一条 brk 应当**原生**提供能力的缺口——本文的全部建议，本质是「把我手工补 codex 短板的那些土办法，沉淀进 breakpoints 的工程」。
