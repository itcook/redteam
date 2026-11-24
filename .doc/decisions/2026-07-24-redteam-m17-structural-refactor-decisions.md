# M17 结构重构组 · 决策日志

> 架构级单份决策日志，分节记（A/B/C/D 四批各一节，随各批 grill 追加）。
> design 引本文件；本文件只记 WHY（权衡/被否选项/推理），定论见对应 design。

## A · 尾段统一（2026-07-24 grill 七裁决，用户全数拍板）

**背景**：走读四大结构问题之一——全量走 `assemble_report`（TailCtx 本为统一尾段而抽），
增量在 `run_incremental_review` 手抄同套「refute 折账/Usage.refute/mark_dismissed/
compute_verdict/报告组装」，正确性链两份且已分叉。grill 前探明的事实底座：两条尾段的
真实差异只有 5 处（dispute 检测 / merge_carry / Stats.new / RunMeta 五字段 / spec_docs=""），
注释里的「refute(new-only)」不是分叉——`refute_targets` 按 `origin != Carried` 过滤，
共享函数已承载。

### A-Q1 泛化形态 → 数据扩展（CarryCtx）

- **裁决**：TailCtx 加 `carry: Option<CarryCtx>` 字段，assemble_report 内部在两个固定
  插入点判 Some；不做钩子/闭包，不做「两入口共享小步骤」。
- **被否 B（钩子/闭包，roadmap 原文措辞）**：调用方仅 3 个、插入点仅 2 个且位置固定，
  控制流抽象是 YAGNI；钩子签名会被 D4 修复反向绑架（合并逻辑要访问 agreement 语义，
  闭包边界切不干净）。
- **被否 C（各自串接私有小函数）**：治标——两份串接顺序仍会分叉，正是今天的病。
- **附带收益**：数据驱动可直接构造 CarryCtx 单测尾段，不用捏闭包。

### A-Q2 D4 修法 → is_same_bug 相似回落，决议 5 同构语义

- **裁决 2a**：merge_carry 先走现有精确 id 匹配，落空的 new 走相似回落；判据完全复用
  dedup 三条件谓词（同 file + 行重叠 + (title_jaccard ≥ TITLE_SIM ∨ quote_overlap ≥
  QUOTE_SIM)），抽共享函数 `is_same_bug`，不另立第二套阈值（第二套阈值=第二个漂移源；
  dedup 判据已被 dogfood 校准）。相似回落仅吸收进 origin=Carried 项（new-vs-new 已由
  上游 dedup 处理）。
- **裁决 2b**：相似命中语义与决议 5 精确 id 路径完全同构——保 carried 全体
  （severity/confidence/body/rec 不动）、models 并集、**agreement 不变**、new 副本丢弃。
- **被否（相似命中走 dedup 的 merge_into，severity/confidence 取 max）**：会造成
  「quote 漂移的 bug 比 quote 稳定的 bug 待遇更好」的怪相；决议 5 哲学=跨轮重看非独立
  证据，精确/相似命中无语义差别，只是 id 稳定性工程缺陷的补丁。
- **连带（自动成立）**：被吸收 new 不进 refute_targets（carried 豁免，与今日精确 id
  行为一致）；dispute 检测在 merge 前按精确 id 跑，不受影响。

### A-Q3 T44 增量 --spec 透传 → 纯机械，无新设计

- **裁决**：与 run_code_review 逐字同手法（render_spec_section_capped + 预算扣减），
  三处透传：fan-out、二阶段 mk、refute（统一后即 TailCtx.spec_docs 填真值）。spec 每轮
  现读工作树（不做跨轮快照——spec 演进本该被新轮看到）。
- **明确不做**：inc_section（manifest/claimed 卡）的预算入账与注入中和=D2/M18 的账。

### A-Q4 cli.rs 拆模块 → 五模块；job 状态机原地；dossier→diffsym 挂本批

- **裁决**：cli.rs → cli/{mod,tail,spec,second_pass,incremental}.rs（边界见 design 表）。
- **4a job 状态机不挪**：并发组刚定型、封盘/收尸语义敏感，「少动」最保险解释是位置也
  不动；上帝文件病灶靠抽四个内聚域已解决大半；真要挪归 M17-c（该批本就在此语义区活动）。
- **4b dossier 改名挂本批**，独立机械 commit；新名 `diffsym`（现存 diff/符号解析核心被
  pack/verify 活跃复用，`symbols` 太泛）。
- **纪律**：迁移零语义 diff（签名/逻辑/pub 面不变），测试随主体走。

### A-Q5 契约冻结面 → schema v3 不 bump、exit_code 不动

- **裁决**：schema v3 字段集/exit_code 映射/resume 路径/全量路径全部零变化承诺。
  预期行为变化仅两处且都是修 bug：D4（跨轮同 bug 双条→单条，stats 随之变）、
  T44（增量 --spec 从静默忽略变生效——仅影响本就传 --spec 的增量用户，无 --spec 运行
  零 token 轮廓变化，不触发独立 A/B 纪律）。
- **被否（bump schema / 加披露字段）**：v3 语义未变；被吸收 new 的 models 并进
  carried，现有 models 列表已可见。

### A-Q6 测试策略 → 四层网 + 冒烟增量 e2e 进本批

- **裁决**：①tail 单测（CarryCtx Some/None 直测插入点）②D4 回归（quote 漂移场景 +
  is_same_bug 边界，守 under-merge）③T44 断言 spec 段在场 ④迁移零语义门（393 lib +
  4 e2e + clippy 零新告警）。**冒烟级增量 e2e 1 例进本批**（现状增量主流程 e2e 为零，
  而本批重构的恰是它；完整矩阵仍留 M18）。

### A-Q7 范围纪律 → 七裁决闭集，一行不多

- **点名排除**：D2（M18 安全批）/ D3（M18）/ exit code Incomplete（M18，Q5 已冻结）/
  refute conf_floor 门（M18 P2）/ 检查点 `let _=` 吞错（M17-c 主题）/ T48（M18）/
  T49 缓存前缀（后置 M17-b，2026-07-23 已裁）。

**定论落点**：`../design/2026-07-24-redteam-m17a-tail-unification-design.md`（锁定）；
实施计划 `../plans/2026-07-24-redteam-m17a-tail-unification-plan.md`。

## B · provider 翻转（2026-07-24 grill 八裁决，用户全数拍板）

**背景**：走读结构问题之二——整层围绕 12 轮 tool_loop 搭建，M15 无工具默认后该路径在
loop 机器里空转（tools_enabled=false → 空 tools + 首轮直出，max_iters/FINALIZE/executor
全闲置）；DirectProvider 死码埋活解析器。grill 前探明的关键事实：①无工具路径**功能上已是
single_shot+repair**，翻转=把隐式路径提成显式结构，可做到零行为变化；②Off/TwoStage 在
provider 层完全等价（二阶段语义在 cli 层）；③D1 是 OpenAI/Anthropic **双侧**同款洞（走读
只点了 OpenAI 侧）。

### B-Q1 Provider 形态 → 双类型分派；首发保持 turn(空 tools) 语义

- **裁决**：SingleShotProvider（默认档 Off/TwoStage，不持 root/executor）+
  ToolLoopProvider（tools=on 诊断档，删 tools_enabled 旗标）；main.rs 装配按档位 match。
  ChatBackend trait 与 registry 不动。
- **修正 roadmap 原文陷阱**：followup 曾写「复用 turn_final 单轮」——但现状无工具首发走
  `turn(空 tools)`（OpenAI 侧不带 json_object），turn_final 才带。首发改 turn_final =
  给已验证路径加 json mode 强制（行为/轮廓变化 + 部分兼容端点怪癖）。
- **被否（首发用 turn_final/json_object）**：记 followup A/B 项，不混本批——本批铁律=
  纯结构重构零行为变化。

### B-Q2 tool_loop 收缩 → 四点

- 裁决：①run_tool_loop 删 tools_enabled 参（恒 true），NO_TOOLS_SYSTEM 迁出；②新
  single_shot.rs：run_single_shot = timed+retry 的 turn(空 tools) → parse / REPAIR+
  turn_final / diagnostic_parse_error，与现状 tools_enabled=false 路径逐字节同语义
  （iterations 1/2 口径沿用）；③共享面防双份复辟：REPAIR_PROMPT/diagnostic_parse_error/
  timed_turn 三者共享，FINALIZE_PROMPT/DEFAULT_SYSTEM 留 tool_loop 私有；④双 Provider
  结构对应收缩。
- **被否（tools=on 档顺手瘦身/重设计）**：诊断档保持现状语义，只删旗标——范围纪律。

### B-Q3 parse_lens_output 落点 → 新 parse.rs；direct.rs 整删

- 裁决：parse.rs 承接 parse_lens_output + LensOutput + direct.rs 全部 parser 测试
  （DeepSeek 截断/围栏剥离等 dogfood 沉淀一条不丢）+ Q2c 共享修复面（REPAIR_PROMPT/
  diagnostic_parse_error——同属「输出解析与修复」域）；timed_turn 落 mod.rs（与
  retry_classified 同居传输层）。direct.rs 456 行整删（死码：DirectProvider/双份 body
  构造/extract_*/三常量）；anthropic_messages_url 注释措辞顺改。

### B-Q4 D1 修法 → 公共 error 体探测，双侧接线

- 裁决：error_body_to_provider_error(v)->Option<ProviderError> 探 OpenAI
  `{"error":{...}}` 与 Anthropic `{"type":"error",...}` 两形状；映射沿用现有档：
  rate_limit/overloaded→RateLimited、timeout→Timeout、server/api_error→Server、
  auth/permission→Auth(不可重试)、invalid_request/context_length→BadRequest(不可重试)、
  **未知→Unavailable(保守可重试，与 envelope 层哲学一致)**。接线三处：Anthropic turn
  （decode_json 后）、OpenAI 非流式回落分支、OpenAI 整体纯 JSON error 体（SSE 帧内 error
  不强求——网关报错通常不走 SSE）。
- **修后义务**：T35（Kimi 尾部超时，疑同源）复测挂验收。

### B-Q5 T34 正名 → LensRequest.model→route_key；derive_kind 返 ProviderKind

- 裁决：纯 rename 零行为；registry doc 散文随之减半。**不动** PlannedModel.name/config
  键侧（config 契约面，动了影响用户 TOML 映射，收益低）。derive_kind String→ProviderKind
  消 parse 往返。

### B-Q6 P2 三件 → 注释对齐/probe 修/常量自动归一

- 6a 退避注释不符：**改注释**对齐实现（实测 attempt=0 → 400ms，按实测写）——改实现是为
  对齐散文付行为变化，被否。
- 6b probe 一律 bearer_auth：**修**——按 kind 分派头（Anthropic 要 x-api-key+
  anthropic-version）；这是用户可见假失败的真 bug 非 polish。
- 6c 常量双份：direct.rs 删除自动归一，零额外动作；MAX_TOKENS 调值不在本批（token 轮廓）。

### B-Q7 测试/验收 → 四层网 + live 冒烟

- 裁决：①single_shot 单测（FakeChatBackend 全分支）；②**等价性锚**：现状
  tools_enabled=false 测试群迁移为 single_shot 测试而非删除（断言原样=零行为直接证据），
  tools=on 测试原样保留；③D1 四锚；④probe 按 kind 选头抽纯函数可测。验收=全量 test+
  clippy+make verify+**live 冒烟一发**（纯结构重构也要一次真枪）+T35 复测（Kimi 配置在
  则跑，不在记 followup 不阻塞）。契约冻结：LensCall/iterations 口径/trait/registry/
  report schema 全不动。

### B-Q8 范围闭集

- 点名排除：T49 缓存前缀（本批交付后解锁，仍独立批）/首发 json_object A/B（记 followup）/
  MAX_TOKENS 调值/Google 适配(M20)/tools=on 档重设计/SSE parser 重构/model 层其余 P2(M18)。

**定论落点**：`../design/2026-07-24-redteam-m17b-provider-flip-design.md`（锁定）；
实施计划 `../plans/2026-07-24-redteam-m17b-provider-flip-plan.md`。

## C · state 夹层合并（2026-07-24 grill 六裁决，用户全数拍板）

**背景**：走读结构问题之三——state/{job,store}.rs 是 SQLite 迁移后未清的旧门面：13 个
自由函数每调全量重开连接（create_dir_all+open+WAL pragma+8 表 DDL，热路径 on_cell 每
cell ≥2 开）；错误三层压扁（anyhow→io::Error::other→18 处 `let _=` 吞）=R2.1 流式检查点
卖点静默失效风险；result json/md 双事务写窗口可互丢。**grill 探底新抓 D5**（走读+M16 终审
双漏）：main.rs:545-551 Cmd::Worker 起手 deadline_at 写是裸 RMW（read→改→write_state
整状态回写），cancel 落在读写间被覆回非终态=首个终态封盘第三例绕过——证明散弹修洞不收敛，
必须类型收口。生产裸 write_state 仅存 2 处（:449 首建合法 + :549 即 D5）。
事实约束：rusqlite Connection 是 Send 不 Sync → 持久持有需 Mutex；detached worker 的
stderr 是 Stdio::null()（eprintln 进黑洞）。

### C-Q1 合并形态 → JobStore 持 Mutex<ProjDb>，自由函数全删

- 裁决：state/ 收敛单 mod.rs：类型区原样 + JobStore 类型（13 自由函数全变方法，连接
  开一次复用至 store 生命周期终）。**不留兼容壳**（壳=病灶本身）；db.rs ProjDb 不动
  （干净 SQL 层，病在 state 夹层）。
- 被否：薄壳保自由函数（治标，转发层还在）；store 并进 db.rs（混 SQL 与业务状态职责）。

### C-Q2 写收口三方法 + D5 收编

- 裁决：写路径全收口——①create_job（唯一整状态写，语义钉死 Pending 首建，:449 改用）；
  ②update_if_active（平移，语义不动）；③pub write_state 消灭（测试 fixture 走
  #[cfg(test)] put_state_for_test 或 db 层直写）。**D5 收编**：deadline RMW 改
  update_if_active 闭包内判 + 回归测试（预置 Cancelled→断言不被覆）；挂 D 编号进走读
  评审档。
- **不动**：reap_if_dead/run_job 的「读→判断→守卫写」模式——读写间隙由守卫兜底（写时
  重读重判），非洞；只有「读→改→裸整写」才是。

### C-Q3 错误披露 → 四级分级（按调用场景非按函数）

- 裁决：①JobStore 签名统一 anyhow::Result 直通（io::Error 包裹层删=旧文件层遗迹）；
  ②worker 热路径（on_cell/on_phase/on_meta/进度写）吞而有痕：eprintln + **JobCellSink
  AtomicU32 失败计数器 → run_job 收尾塞 report.run.warnings**（stderr 在 detached
  worker 下是黑洞，warnings 才是用户真能看见的通道）；③终态写必响（eprintln+前台传
  Result；panic hook 无传播通道则 eprintln 尽力）；④一次性命令直接 `?` 冒。
- 被否：全 Result 硬中断（检查点失败杀 15min 评审=为可观测性付业务代价，倒挂）。

### C-Q4 连接复用三生命周期 + result 单列 upsert

- 裁决：worker 进程 = Cmd::Worker 入口一个 Arc<JobStore> 穿 run_job+JobCellSink+收尾；
  一次性命令 = 入口构造一次；**台账消费面（load_ledger/resolve/preflight 直用 ProjDb）
  本批不动**。Mutex 锁粒度=方法级（方法全同步，不跨 await 持锁），与现状「各开连接+
  SQLite 写锁排队」语义等价。result 修法=db 层 result_put_json/result_put_md 单列
  `ON CONFLICT DO UPDATE SET 己列=excluded.己列`——读改写窗口整个消失，比合并事务更简。
- 被否：全局连接池/once_cell 单例（两进程三生命周期，显式传优于隐式全局；SQLite 不需要池）。

### C-Q5 测试 → 断言零改迁移 + 背景 job 冒烟 e2e 进本批

- 裁决：并发测试群（concurrent_progress/pre_cancel/pre_done/concurrent_completion/
  cancel 三态/reap 四测/panic 两测）**断言一条不改**迁到新 API（等价性锚手法）。新增：
  D5 回归 / 连接复用不变式（单连接全生命周期 + Arc<JobStore> 进程内并发 lost-update，
  与跨进程旧测并存）/ 披露分级测 / **背景 job 冒烟 e2e**（核实现状：e2e 全 background=
  false，真 worker 链路零覆盖=T45/T47③ 旧债，本批重构恰是 worker 共享连接，风险落点
  即此；真 spawn 版优先，fixture 贵则 run_job 进程内驱动兜底）。
- T47③ panic 注入 e2e：背景 fixture 顺利则可选加测，不顺利不阻塞（M18 仍挂账）。

### C-Q6 范围闭集

- 点名排除：exit Incomplete(M18)/台账层连接治理/db.rs DDL 拆分与 schema 迁移机制(M20)/
  JobState 字段清理（serde 兼容风险零收益）/Phase 增强/job 状态机函数搬家（M17-a 已裁
  原地，本批只改调用方式）。
- 贯穿铁律：并发四件套（守卫 CAS/封盘/身份锚/收尸/panic hook）语义原样。

**定论落点**：`../design/2026-07-24-redteam-m17c-state-store-design.md`（锁定）；
实施计划 `../plans/2026-07-24-redteam-m17c-state-store-plan.md`。

## D · ship 协议硬化（2026-07-27 grill；**中途用户推翻原立项，问题重定义**）

**重定义过程（本节最重要的记录）**：原立项「ship-step 下沉」=判定全面进二进制（ship
命名空间 start/triage/step/summary 四命令 + proj.db ship_rounds 表 + 决策 JSON 契约），
grill 已推进到 Q3 命令形态时**用户叫停**：不符预期，要求回到问题定义。复盘承认：走读把
「135 行散文状态机」列为结构问题之四是从「内置命令优先」原则**演绎**的，而 a/b/c 各有
实证缺陷，d 的实证底座只有 T05 一条 + 结构性风险——把演绎当实证直接跑方案是流程错误。
问题重摆（按实证强度）：①T05 半失败 approve 误收工（实证，M13 评审记录）；②跨轮记账靠
LLM 记忆，compaction 必漂（理论强实证弱）；③每轮背 134 行规则执行漂移（理论）；④T07/
T08/T09 polish。**用户裁决：A+B**（修实证缺陷 + 消记账脆弱性），全面下沉作废。

**关键技术发现（决定 B 厚度）**：逐项核查发现跨轮状态**数据已全部在盘上**——轮次链=
rounds/<session>/rN.result.json 序列（run.job_id 在内）；复裁资格=dismiss 台账 history；
修复历史=resolve_history（同 id ≥2 条=修过两次）；轮数=目录计数。真病灶收窄为：ship.md
**缺「从盘重建、不信记忆」的协议**，判定写成记忆比对口径。

### D-QA T05 修法 → 半失败 approve 走「交人」出口

- 裁决：不当合法 approve 收工——AskUserQuestion 二选一（接受降级 approve/重跑本轮）。
- 被否：注记后照常收工（T05 的病就是静默当合格）；判定收紧强制续跑（模型持续挂→无限
  循环，需配套计数器，复杂化）。

### D-QB 记账修法 → B-薄（纯协议修订 + 按需一个读口）

- 裁决：①ship.md 新增「跨轮状态重建」节（铁律=跨轮判定一律从盘重建 + 三条重建口）；
  ②判定改查表口径：销案争议刹车「第二次出现」（记忆比对）→「id∈本轮 disputed 且
  resolve history ≥2 条」（纯查表）；max-rounds→rounds 目录 rN 计数；③rN.result.json
  存档从便利升格为链持久化协议义务。
- 被否 B-厚（ship_rounds 表+查询命令）：数据已在盘上，加表=第二真相源漂移风险；命令空间
  扩张已被用户否；等发布后实证再升级。
- 前置核查：resolve 无读口（只有平铺写口）→ 补 `resolve show`。

### D-QC resolve 形态 + 顺带项

- C1：Resolve 升级子命令空间 `resolve add|show`（对称 dismiss；show 输出卡+history）。
  破坏面=裸形态 `resolve <id> --from…`，消费方仅 ship.md（本批同步改）+ 零外部用户=
  改契约最后窗口。被否：--show flag 切模式（丑）/顶层新命令（污染命令面）。
- C2：T07 两层目录**随行**（本批正在动 rounds 协议地位，一行改）；T08 **随行薄版**
  （ship.md 收工步固定模板骨架，纯 md；二进制聚合渲染版随下沉方案作废）；T09 **不随行**
  （维持等痛点）。

### D-QD 验收与闭集

- 验收：Rust 常规门 + md 隔离评审按九项裁决清单核对 + make gen/verify 零 diff；
  **live ship 循环验收并入 M19 发布前全仓 dogfood**（成本一整轮，届时全要素实测）。
- 闭集排除：ship 四命令与 ship_rounds 表（作废留痕，实证再启可复用本节推理）/T09/
  停机未涉条款措辞重写（T10 同理）/feedback 复活。

**定论落点**：`../design/2026-07-27-redteam-m17d-ship-protocol-hardening-design.md`（锁定）；
实施计划 `../plans/2026-07-27-redteam-m17d-ship-protocol-hardening-plan.md`。
