# redteam Roadmap · 已交付里程碑

总览与状态见 `summary.md`；开放项见 `followup.md`。以下按里程碑存档，明细指向对应 design/plan。

---

## M1 · 核心引擎 ✅
Phase 1（单发基线）→ Phase 2（design §19 权威架构）：tool-loop agent 执行核、code 文件包管线、
doc lens 管线、positioning + targeted-verify 尾段、多模型 fan-out、dedup/correlate、verdict、
输出 schema（含稳定 `id`）。Plan：`../plans/2026-06-26-breakpoints-phase2-roadmap.md`（S2→S3→S4）及子计划。

## M2 · 运维面 ✅
S5：文件式 job store + 状态机 + detached worker + `review/status/result/cancel/worker`；
`model setup/list/check`（TOML 分层配置 + 可用性预检）。Plans：`…s5a-jobs-plan.md`、`…s5b-model-commands-plan.md`。

## M3 · 可靠性 & 可运维核心 ✅（merge e53eaba，25 commit，245 tests 绿）
从 19 轮真实 codex 对抗评审蒸馏（`../research/2026-06-30-live-review-operability-lessons.md`），
`feat/reliability-core` 各批经 opus 终审 + live dogfood，2026-07-01 --no-ff 合 dev。
- **P0 内核**：HTTP/per-turn 超时、job deadline+部分报告、心跳 last_active、manifest+partial checkpoint、
  prune 转 Failed 不删、`--resume` 差集重放、进度接线。Design/Plan：`…reliability-core-{design,plan}.md`。
- **C 弹性**（e963d71..80e7d2d）：错误分类+共享 classifier、turn 级重试（删 whole-loop）、优雅降级。
- **D 契约**（7d47b58..1cef23b）：status→JSON 数组、`workspace` 字段 + `--workspaces` 跨查、phase 三态、
  `deadline_at` 一致、删 `--wait`。

## M4 · 打包分发 ✅ 多平台就绪（merge d9e96e7，246 tests + verify + dogfood）
S6 one-repo-marketplace + `commands/`→`codex-skills/` 生成；2026-07-01「多平台就绪」：`bin/<os>/<arch>/redteam`
+ `bin/redteam` launcher（uname→exec）、瘦身 10M→4.1M（opt-z+strip+lto+panic=abort）。
Design/Plan：`../design/2026-07-01-breakpoint-packaging-multiplatform-*`；S6 Plan：`../plans/2026-06-30-breakpoint-s6-packaging-plan.md`。
- **剩余活口子**：真正产出 linux/win 二进制（cross/CI 交叉构建）——仅填 darwin/aarch64，结构已就绪；软前置 M10。

## M5 · 交互命令优化 + 产品更名 redteam ✅（40a2367 + de2442a，2026-07-02）
**重大决策：breakpoint → redteam**（用户定，理由=完美符合「独立·模型无关·对抗」理念、简短、解开调试器隐喻命名死结）。
- **批1 全域更名**（feat!）：二进制 `brk`→`redteam`、循环技能 `lgtm`→`ship`（`/redteam:ship`，`fix(ship):` 前缀）、
  crate/lib/`plugins/redteam/`/`.redteam/`/`REDTEAM_*`/`~/.config/redteam/`——全层一次改净零垫层；本机数据已迁。
- **批2 `status --format table`**（de2442a）：opt-in 人读视图，默认 JSON 数组机读契约不动（ship 依赖）。
- **同日追加**（0dd225e+d92b0e7）：**产品原则「内置命令优先/裸终端可用」入 CLAUDE.md**；CLI help 全补齐 +
  `--version`；`--dismissed` 下沉双效（台账自动注入 prompt「勿再报」段，ship.md 删宿主编排、--focus 回归纯语义）。

## M7 · Loop SKILL 就绪 + 循环对抗层 ✅（merge c268b1f，2026-07-02，subagent-driven + 终审 Ready）
让 redteam 被外层循环 SKILL 无人值守驱动，并建那个 SKILL（design §17）。
- **redteam 原语**：R8.1 quote 锚定**行无关 id**（`hash(file+归一 quote+归一 title)`，跨 fixup/位移稳定）、
  R8.2 `--dismissed`（标记+verdict 豁免+`dismissed_reappeared`）+ `.redteam/` 项目目录 + `init` + schema_version 2、
  穷尽性指令入 4 对抗模板、`/redteam:ship` 循环命令（台账+刹车+双宿主）。
- **dogfood 实证**（self-hosting 7 轮，session 20260702-200624）：17 引擎级修复、26 驳回、勿再报注入活体有效
  （reappeared 全程零）。**协议边界**：活跃分支每轮修复自身成下轮评审对象 → approve 结构性不可达 → 按停滞守卫收官。
- **id 稳定边界**（非 bug）：依赖模型逐字引用独特 quote + 稳定 title；doc 模式抽象概括类措辞漂移会致 id 变——
  这正是协议保留「刹车链复裁 + 人确认」而非纯 id 匹配的根据。
- Design/Plan：`../design/2026-07-01-breakpoint-loop-lgtm-design.md`、`../plans/2026-07-02-…-plan.md`。

## M11 · doc 评审守卫化 ✅（merge 711edcc，2026-07-02，三靶 dogfood 全 PASS）
doc = 纯「文档交付守卫」（能否安全作为实施输入），**不是**设计/脑暴技能（出界）。双模板方向性指令
（`<temporal_stance>` 四分类 + tools 段程序性首步核实 + refute Temporal check）；顺手根治 `locate_quote` 软换行
误杀真 finding（词级序列退路）。回归 A/B：M7 spec 的 3 条方向性误报消失、正确方向真 drift 捕获。
Design：`../design/2026-07-02-breakpoint-doc-guard-design.md`。

## M12 · dogfood 自反馈 ✅（fef92fd，2026-07-02 立项即交付）⚠️ **发布前移除**
ship 收工派 subagent 捕获 **redteam 工具自身**缺陷/改进 + **确定性量化记分卡**，汇总到跨 repo 中枢
`${REDTEAM_FEEDBACK_DIR:-~/.redteam/feedback}`。**纯 prompt 实现、`src/` 零改动**（jq 配方保确定性）。
- 交付：`plugins/redteam/feedback.md` + ship.md 两段（标「本地调试·发布前移除」）+ codex 镜像。
- 记分卡效度边界：地面真值=宿主裁决（方向性可用、绝对值不可信）、只测 precision 不测 recall。
- **定位=开发期临时仪器，发布前整体拆除**（拆除步见 `../TODO`）。Design：`../design/2026-07-02-breakpoint-dogfood-self-feedback-design.md`。

## M13 · 成本重构·支柱 1 轮次增量协议 ✅ 机制交付（merge fc1e583，22 commits，312 tests+verify 绿；二进制 fdf5fe6）
r1 全量、r2..rn 只审 fix-delta，重锚定携带 + `resolve` 销案 + resolution_disputed 假修复回路；尾流守卫退役
（增量下误触发）+ 销案争议刹车；`review --incremental-from <job-id>`，v1 仅 code 模式。**顺带解决自指尾流/
approve 结构性不可达**（全量重审语义消解）。
- **背景**：dogfood 实证单 ship 会话（5 轮×3 模型）输入 17.1M/输出 0.6M（28.5:1），钱在重发上下文非模型思考。
  调研 `../research/2026-07-03-token-cost-tree-sitter-research.md` + 五支柱重设计。
- **实施**（2026-07-04，subagent-driven 13 Task，d579948..781e29c）：每 Task 双评审 + fable 全分支终审 Ready；
  评审净增四加固：resolve 空 reason 沿用、缺 --base 拒跑（消 panic）、**脏工作树拒跑**（防蒸发携带项）、
  git_at 关 quotePath（非 ASCII）。Design（grilling Q1-Q8 + R1-R6）：`../design/2026-07-03-redteam-incremental-rounds-design.md`；
  评审：`../reviews/2026-07-03-incremental-rounds-eng-review.md`；Plan：`../plans/2026-07-03-redteam-incremental-rounds-plan.md`。
- **合并门两件手动活全过**：Task 14 单跳回放（job 005f71636991，disputed 活体触发 9bb14796；手术=副本库整行
  INSERT 回填 r1 result，主库零触碰）+ 争议刹车靶三轮演练（干净 sonnet 探针仅读 ship.md 即推出停机交人+拒 r4，
  证协议文本自足）。回放派生修复 `select_files` 滤二进制 diff（7e2a127，实证 36% input 烧在 build 产物 bundle）。
- **live 降幅**：效果验收延后并入 M14，经 M14 卷宗 A/B 量得（修改型变更集 input −38~50%）。
- **支柱 5 + L1 缓存 ✅**（d579948）：cached_input_tokens 观测全链路 + input_tokens 归一 + Anthropic cache_control
  滚动断点。与本里程碑正交（L1 只降账单不减重发量）。
- **Task 14 保全率口径注**：r2 属实 5 id 仅 5361b545 真在 delta 域，其余 4 条 hunk 级出域（历史靠全量冗余撞上）——
  「r2 属实 100% 捕获」线的隐含前提与数据不符，属**验收线口径缺陷非机制缺陷**（design §0 已知缺口的实测形态）。

## M15 · 上下文管理架构（重设计 → 无工具评审默认化）✅（2026-07-14 收尾归档）

**北极星兑现**（延迟>效率>质量>成本，2026-07-06 重定）：从 M14 时代（arm A：input 2997K / 39 轮 / ~24min）
到新默认（**~75K / 恒 1-2 轮 / ~1.5min**）= input −97.5%、挂钟 −94%；质量由 refute 闸门（证据升档后
判得更准）+ 每批验收多轮对照守住。dev 合并链：b6aabed → 16323ed → 5fd6c9f → 4f52ac9 → 87e281c → 8dcaf6f。

**弧线**（各转折的数据与推理见 research `2026-07-09-s2b-static-validation-research.md` 全弧 + decisions E4-E9）：
架构 grill（blast-radius+双树 SCIP+per-model 规划器框架）→ S1 pack 机制（弱通过：tool_use −8~10%）
→ S2-a 探针否决（假阳双峰，撤；改交付 strip_rust_noncode −37% 假阳）→ S2-b SCIP 单树 MVP + 静态验收
抓真 bug（local 符号未过滤 dfc8ade）→ **残余读解剖定案设计层三错配**（E7：粒度[符号 vs 文件级消费]/
范围[per-bundle vs 变更集]/覆盖[refute 零 pack]；S2-c 死刑）→ S2-b' 0 壳整文件（tool_use −34%=pack 线
最大单笔）→ **S2-b'' 无工具 A/B（质量不降 + 轮次 39→5 + input −91%；「上下文与工具是替代品」）**
→ 粒度 A/B（符号 pack 质量未守住 → 转正超预算降级档）→ 无工具默认化 grill 九裁决（E8，design
`2026-07-13-redteam-no-tools-default-design.md`）→ **四批 SDD 交付**：
- **批 0/1**（merge 4f52ac9）：T43 refute parse 三层韧性（嗅探/REPAIR/保守兜底）、`[review] tools` 三值
  默认 **two-stage**、changeset 全景 0 壳默认+兄弟真 diff+禁真空、per-model 预算函数（token 硬墙）。
- **批 2/3**（merge 87e281c）：needs 置换式二阶段（cap 5/整体替换/草稿确认或修订；doc 臂端到端首证）、
  refute 证据升档（锚点全文 64K→骨架→快照）、骨架档（签名+hunk 全改动行+折叠；档位互斥）+hunk±20 兜底、
  dep_rank 兄弟排序、二阶段证据段预算纪律（终审抓）。
- **批 4**（merge 8dcaf6f）：**单格全审转正默认**（E9：−77%~−86% 两语料）、**lens 去实体化**（E8-I 代码
  零结构：模板固有文本+全字段/flag/模块退场，净 −293 行）。

**判决与安乐死**：E8-E **SCIP/1 壳安乐死**（纯 0 壳臂质量无差 + needs 预防针假说证伪；引擎/仪器拆除归
M12/M16）；E9 fan-out 收缩 + dep_rank/clamp_compliance 删除（复活点=git 656ed23/fee48e8/0354160^）；
爆炸半径：粒度主张证伪、选择原则残余仅剩 L4 弱耦合缝拆分（痛点驱动后置）。

**遗留**（未随 M15 关账）：doc 模板 tools 散文（批 0-3 债）→ M16 前散文 pass；SCIP 引擎 + 实验仪器
（REDTEAM_TOOL_LOG/PACK_SHELLS/PACK_BODY）拆除 → M12/M16 清单；classify_refute 加固面（验收常客）→ TODO。

## M16 · 发布基线（P0 必清，门控）✅（2026-07-21 三批全清合 dev）

威胁模型 B（本地审不可信仓/PR）下的发布门控三批，执行次序（用户 2026-07-14 定，E11 尾）①→②→③全落 dev：

**①清理批**（plan `../plans/2026-07-14-m16-pre-cleanup-plan.md`，decisions E10/E8-E）：doc 模式移除（`build_doc_prompt`/D1-D5 lens/`run_doc_fanout`/doc bundle+refute 模板/mode doc 分支全删；`--base/--scope` 转必选）+ SCIP 引擎拆除（E8-E 安乐死，纯 0 壳臂质量无差）+ 实验仪器拆除（REDTEAM_TOOL_LOG/PACK_SHELLS/PACK_BODY）。验证=src/ grep `scip`/`REDTEAM_TOOL_LOG`/`PACK_SHELLS`/`PACK_BODY` 全空。

**②安全组**（merge 774f425 + e25c26d + 73f6a30，plan `../plans/2026-07-14-m16-security-group-plan.md`，decisions E11 grill 七裁决，内部修序 ①config→④needs→③单行化→②轻加固）：威胁模型 B 四守卫——①恶意仓 `.redteam/config.toml` 偷 key（项目层白名单只留 [review] 无害键、拒 providers.*）②被审内容 prompt injection（结构中和纵深，无工具默认已缩可利用面）③render_precedents 二阶注入（单行化+行首 `- [` 转义）④needs 变更集外任意读（git-tracked 守卫 + 读披露 + 敏感 denylist 兜底，E11-Q4 dogfood 抓、跨模型存活 high conf）。dogfood 复审再抓两真洞（git 参数注入 + 文件路径未中和）已修（73f6a30）。opus 终审 READY TO MERGE、四守卫全闭合，余项记债 T46。

**③并发组**（merge 290236e，7 commits=6 SDD 任务 + 1 终审 fix；plan `../plans/2026-07-21-m16-concurrency-group-plan.md`，decisions E12）：redteam 无 daemon 裸终端下 worker 与 status/list/cancel 跨进程并发操作同一 job 状态的**三向死结 + panic 兜底**——①终态覆写（worker 迟到写覆盖已 cancel 的 Cancelled）②猝死永久 Running（worker 崩溃无人收尸）③cancel 盲杀（无条件 SIGTERM 复用 pid）。**四件套内核**：守卫式原子读改写（`job_update_guarded` 在 SQLite BEGIN IMMEDIATE 事务内 CAS + `update_state_if_active` 承载**首个终态封盘**：任一终态一旦落定永不覆写）+ pid 身份锚（`procinfo::start_time` macOS 读 proc_bsdinfo 启动时刻防 pid 复用、非 macOS 降级纯存活）+ 惰性收尸（`reap_if_dead`：status/list 读态时对 Running 但 worker 已死的 job 就地守卫落 Failed）+ 进程级 panic hook（Cmd::Worker `set_hook` 兜底 `mark_failed_panic` 守卫落 Failed，payload 只取 message 无 backtrace/location=B 报告干净）。SDD 全程每任务 implementer + 隔离 reviewer；整支 **opus 终审**抓 1 Important（run_job Pending→Running 裸 write_state 绕过守卫的封盘窄洞，cancel 抢窗可覆回 Running）已修（改走守卫 + `is_terminal()` 早退 + 守卫拒写时重读真终态返回其 exit_code）并 opus 隔离复核 APPROVED。**390 lib + 4 e2e 全绿、clippy 零新增、make verify 无镜像漂移；正常路径字节级不变、首个终态封盘经终审确认无绕过。全程纯本地未 push。**

**记债**（非 blocker，威胁模型 B 内）：T47（并发组测试覆盖缺口——反序列化 eprintln 无测试 + reap 三缺口 + worker 真 panic 经 hook 全链路 e2e；已核实 orchestrate 全程 join_all/无 tokio::spawn，hook 只在真致命 panic 触发不误伤优雅降级，缺的仅回归覆盖）、T46（安全组终审遗留：安全 warning 进 run.warnings/ledger + needs 拒绝披露 DRY + render_incremental_section 第五注入面 + needs denylist 补 .netrc/.p12 等）。

**dogfood 轮（2026-07-21，合并后，单模型 gpt-5.6-sol 审 f028f92..dev，4.7min/113.6K input）**：4 findings，人裁=修三处+两卡入台账（hotfix 814e010 + build e8dc24f）——**F2 真洞（opus 终审漏判）**：spawn 后父进程 pre-spawn Pending 快照裸 write_state 补写 pid，子进程抢先推进 Running/终态时被覆回=绕过封盘（终审曾判「创建期无终态可覆，良性」，漏了 spawn 在两次写之间）→ 改走守卫只补 pid/pid_start；**F4 真缺口**：worker 写 Running 前早夭（args 读失败 exit(2) 不落终态）→ Pending+死 pid 永久悬置而 reap 只认 Running → reap 扩 Pending（创建窗口由无 pid 放过挡住）；**F1 测试子项真**：procinfo 两锚依赖测试非 macOS 必挂 → #[cfg] 门控（设计降级部分=E12 已接受 → dismiss 卡 6d29b2f3）；**F3 现仓不可达**：panic hook 误伤论——grep 证实零 catch_unwind/tokio::spawn，与 opus 终审专项核实一致 → dismiss 卡 7a6f0a14（若未来引入 catch_unwind/spawn 需重审）。新增 3 测试（Pending 收尸/创建窗口不误收/pid 登记保 status+拒终态），393 lib+4 e2e 全绿。**教训：dogfood 在整支 opus 终审之后仍抓到 1 真洞+1 真缺口——外部模型视角对「良性」判定的复核价值实证。**

## M17-a · 尾段统一 ✅（merge 8b9eef2，7 commits，419 tests 绿，2026-07-24）
正确性链收敛单实现：assemble_report 泛化 CarryCtx 数据扩展（A-Q1 裁决 A），增量手抄尾段
（~80 行）删除；D4 跨轮 quote 漂移去重（is_same_bug 单一真相源 + merge_carry 相似回落，
决议 5 同构、agreement 不加成）；T44 增量 --spec 三处透传生效；cli.rs（3672 行）拆五模块
（tail/spec/second_pass/incremental+mod 留守，job 状态机原地，multiset 验证零语义）；
dossier→diffsym 改名（对外 schema 名冻结）。冒烟增量 e2e 首条落地。opus 终审 READY
（零 Critical/Important；M-1 duration_ms 口径收敛已补记 design §7）。
Design：`../design/2026-07-24-redteam-m17a-tail-unification-design.md`；
Plan：`../plans/2026-07-24-redteam-m17a-tail-unification-plan.md`；
决策：`../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §A。

## M17-b · provider 翻转 ✅（merge 16f118b，10 commits，427 tests 绿，2026-07-27）
默认路径显式化：SingleShotProvider（single_shot+repair，首发 turn(空 tools) 语义、iterations 1/2
口径、首发禁 turn_final 铁律锚进测试）；tool_loop 收缩 tools=on 诊断档（8→7 参，12 轮机器逐字
保留）；direct.rs 456 行死码整删（活解析器迁 parse.rs，7 条 dogfood parser 测试零丢失）；D1
双侧修（error_body_to_provider_error 映射表+两口接线；评审/终审两轮抓 error:null 与非对象占位
误伤，谓词收敛 is_object）；T34 route_key 正名+derive_kind 强类型+probe 按 kind 认证修+退避
注释对齐。live 冒烟✅（31s 端到端抓真缺陷）；T35 复测记 followup（Kimi unreachable）。
opus 终审：三档等价性逐字节确认。
Design：`../design/2026-07-24-redteam-m17b-provider-flip-design.md`；
Plan：`../plans/2026-07-24-redteam-m17b-provider-flip-plan.md`；决策 §B。

## M17-c · state 夹层合并 ✅（merge 06d589e，10 commits，440 tests 绿，2026-07-27）
JobStore{Mutex<ProjDb>} 单实现：13 自由函数收敛方法（写收口三形态、anyhow 直通）、旧门面
job.rs/store.rs 整删（19 测断言零改迁移）；D5 堵洞（worker deadline 裸 RMW 覆终态——grill 新抓、
走读+M16 终审双漏，stamp_deadline 三测锚）；错误披露四级（检查点失败 AtomicU32→run.warnings、
终态写三分支必响、panic hook open 失败披露）；连接三生命周期复用（worker Arc 贯穿）；result
单列 upsert 消双事务窗口；背景 job 冒烟 e2e 首条（run_job 进程内全链路）。
中途评审抓 1 Critical（panic hook Arc 共享 Mutex 同线程重入自死锁——hook 路径退回现开连接修复）。
opus 终审：12 写点清单式排查零绕过、锁纪律全过、READY 零新 findings。
Design：`../design/2026-07-24-redteam-m17c-state-store-design.md`；
Plan：`../plans/2026-07-24-redteam-m17c-state-store-plan.md`；决策 §C。

## M17-d · ship 协议硬化 ✅（merge 175af20，4 commits，445 tests 绿，2026-07-27）
**grill 中途问题重定义**（原「ship-step 下沉」作废——走读结构问题四系原则演绎非实证，用户裁决
A+B，推理留痕 decisions §D）。交付：T05 半失败 approve 交人出口（AskUser 二选一）；「跨轮状态
重建」协议节（铁律=从盘重建不信记忆，三读口：rounds 文件链/dismiss show/resolve show）；销案
争议判定查表化（resolve history≥2，评审独立验证与原记忆比对严格等价）；max-rounds 目录计数；
rN.result.json 义务化；T07 两层目录；T08 session.md 固定模板（md 薄版）；唯一 Rust 改动=
resolve add|show 子命令空间化（对称 dismiss）。live ship 循环验收挂账 M19 发布前 dogfood。
Design：`../design/2026-07-27-redteam-m17d-ship-protocol-hardening-design.md`；
Plan：`../plans/2026-07-27-redteam-m17d-ship-protocol-hardening-plan.md`；决策 §D。

## M17 · 结构重构组 ✅ 全组收官（2026-07-24 立项 ~ 2026-07-27 交付）
a 尾段统一 / b provider 翻转 / c state 夹层合并 / d ship 协议硬化（重定义）。四批合计
~34 commits，420 lib+25 集成 e2e 全绿。走读四大结构问题闭合：正确性链单实现、默认路径显式化、
state 单店化、ship 协议硬化。过程中额外抓获并修复：D5（封盘第三例绕过，双终审漏）、panic hook
Mutex 重入自死锁（Critical）、D1 双侧+奇形占位、首发铁律测试锚。决策日志=
`../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md`（A-D 四节）。

## M18 · 发布硬化 ✅（四批 SDD + 收官 dogfood，2026-07-27 ~ 2026-07-28）
grill 八裁决锁定（2026-07-27）。纪律=零优化（token 轮廓类全逐 M20），每项修 bug/补披露/锚测试。
- **批1 安全组**（合 dev eb67922）：D2 增量注入面中和（manifest+claimed 卡 neutralize+oneline，
  两向量红测法）；denylist 单源合一（+.netrc/.p12，并集收紧）；T46①warning 入账+②DRY；git `--` 分隔。
- **批2 契约组**（含批4 polish 搭车）：exit code 五档定稿（0 干净/1 命令错/2 降级/3 incomplete/10
  reject，唯一契约变化）；D3 result --format md 三态（缺 md 现算渲染）；refute conf_floor 门；
  T48 source_snippet 行窗+越界 clamp；--conf-floor 值域；T25 整链失败 tokens 入账；T26 setup 自动
  probe；resume 披露清零。
- **批3 e2e**（合 dev 00e23fc，4 commits）：**T47③ 真 panic 全链路破局**（REDTEAM_TEST_PANIC env
  钩子 fan-out 前触发，真 spawn 二进制断言 Failed+封盘不变式——M16 以来最后一块端到端盲区）；
  退出码 1/3 链 e2e（exit 3 构造=空 manifest 走 partial_report）；md 三态 e2e；台账注入链路 e2e
  （dismiss→previously_dismissed 锚+判例精确命中）；is_same_bug 同义非全等补例。
- **收官 dogfood**（DeepSeek+gpt-5.6-sol 双模型审 e8dc24f..dev 累计 diff，三轮 live 内循环
  reject→reject→**approve** 收敛）：r1 抓 6（3H/3M）→人裁修 F3（守卫写 Err 折叠假成功→三态显式
  分流）/F5（resolve+dismiss show 统一纯 JSON，history 入数组字段）/F6（spawn 失败落 Failed 不留
  Pending 僵尸），dismiss F1（D4 有意权衡）/F4（五档有意变更）入台账，F2 被模型自 refute（conf
  0.0）；r2 增量复评认可 F3 掉出携带集+追打 1 条有效新 finding（detached worker 守卫写失败仅
  return 不落终态）→补 best-effort Failed 终态写；r3 approve，低危声明残余 dismiss 入台账。
  **台账链路 live 验证**：previously_dismissed 豁免/carry drop/增量重锚定全程如设计工作。
  修复合 dev（merge 含 397a73b/e83635e），475 tests 绿 + clippy 零新增 + verify 过。
Design：`../design/2026-07-27-redteam-m18-release-hardening-design.md`；
Plan：`../plans/2026-07-27-redteam-m18-release-hardening-plan.md`；决策同日 decisions。

## M21a · 疑点扫描地基 ✅（分支 feat/m21-review-engine，六批 A-F 子分支 --no-ff 合回 + 出口验收，2026-09-08 ~ 2026-09-09）
- 交付：用户级唯一 JSON 配置〔D12〕+ secrets 守卫；proj-next.db schema v4 一次重置（旧库拒开、固定集导出 63 / 48）；引用图分组（import 强 / 引用计次 / 同目录弱，
  弱缝先切、宁降不拆、孤立装箱）+ 正反向跨组索引；疑点契约（quote 必填、逐条校验、锚定丢弃）；提示词拆共享头 + scanner 尾、段序翻转、模板哈希、anthropic 三段
  cache 断点；切片装包缺省（核 / 一跳 / 文件地图 / 折叠 / 文件外片段 / 小文件全文）；多 scanner 并行 + 疑点并集（agreement = 家族数）；分支硬切旧编排，产物
  `redteam-next`；`--pack-only` 预算仪表；RunMeta 观测（分组 / 覆盖 / cache / 在途峰值 / 每格摘要）。
- 出口验收（PR181 + dogfood，数据只在分支 `.next/private/m21a/`）：oracle 并集 16 / 22（E3 基线 9）、人工主题 6 / 12；双 scanner agreement = 2 共 19 条；
  DeepSeek 磁盘缓存二跑起 ≥ 95%；`--pack-only` 误差 0.9-14.5%；E2 slice ≥ full 维持默认；两门槛改口径——「分组 ≤ 5」撤为可观测项（拆大格待 aacr-bench），
  「墙钟 ≤ 3 min」改按「排队 + 最长格」可读（大格先发 + workers 8：189 / 202 / 251 s；现状配置 311 / 402 / 457 s）。
- 出口顺手修：anthropic message_delta usage 兜底（网关 GLM 格 usage 恒 0）；scanner 首发解析失败诊断进 cell warn；扇出大格先发；排队告警 + `queue_wait_ms`；
  零疑点短输出 warn；help 并发说明。门：lib 533 / e2e 17 / clippy 3 / make verify 绿。
- 延后：见分支 `.next/roadmap/followup.md`「出口验收延后项」（小分量并箱 / 拆大格 / workers 形态 grill / GLM anthropic 思考映射 / noise 口径）。
