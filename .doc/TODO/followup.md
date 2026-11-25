# redteam TODO · 开放项明细

编号对照 `summary.md`。

> **⚠️ 2026-07-22 重编**:summary.md 已按橡皮鸭走读重规划全面重编(吸收/作废/保留三分),**处置状态以
> summary.md 为准**;本文件旧明细段保留作背景参考,未逐段刷新。走读全清单=
> `../reviews/2026-07-22-rubber-duck-walkthrough-review.md`。

---

## T01 · M14 卷宗 follow-ups ⏳（终审 fable 提 + dogfood 补充；D9 验收前后处理，非合并门）

> **③ 排序 ✅（444c0e3）· ⑥ 降级可观测 ✅（9e86ffc）· ② 单行截断 ✅（720f149）· ④ `\`标记跳过卫生 ✅（720f149）
> 已交付**——见 done.md。以下剩 **①⑤ + ④余项**（大 hunk 跨两函数查全洞 / 脏树行号错位，降级无害）。

① **`reference_points` 性能**（Important #2）：现 per-symbol×per-bundle 全仓 walk + 跨 bundle 不去重
   + hits 在 cap 前无界收集——hiapi 规模（Go 206K/TS 115K）r1 几十符号=几十次全仓 walk，秒~十秒级串行。
   修：每 bundle 一次 alternation 正则 `\b(?:s1|s2|…)\b` 单遍 walk 按命中词归桶 + per-name 收集上限（如 200）。
   **hiapi 规模 dogfood 前必做**；本仓（12K）可忽略。
② **单行超长 context 退化** ✅（720f149）：`truncate_line` 把单行内容截 ≤512B（UTF-8 char 边界回退）+ 固定
   截断注记，`reference_points` 建 ±3 context 时逐行过它——一个 minified 引用点不再撑爆 28KB 预算。见 done.md。
③ **hits 确定性排序**（#5）：per-name cap 现留 walk 序（OS 目录序未排序）→ 跨机不确定；排序 by (path,line) 利 A/B 复现。
④ **锚定 off-by-one 查全洞**（#6，降级无害）：`\ No newline at end of file` 多加一 ✅（720f149 加
   `starts_with('\\')` 卫生守卫——当前架构下 drift 被 hunk_anchor 首行锚定 + 每 @@ 重置掩盖，非活 bug，
   防未来 per-行锚定时暴露）。**余**：大 hunk 跨两函数只锚首个（真查全洞，修需 per-改动行解析、撞 ① 性能
   顾虑故后置）；脏树时行号（HEAD）与内容（工作树）错位——均按 note 保持已记录。
⑤ **`REDTEAM_NO_DOSSIER` 生命周期**：dev-time A/B 仪器，D9 验收后按仪器纪律拆除或升格进 `--help`
   （当前仅存在于代码注释，裸终端不可见）。门阈值 `DOSSIER_MIN_REFS=5` provisional，随验收校准。
⑥ **降级可观测性**（2026-07-05 dogfood 反复点名）：卷宗降级全静默——index 失配 `.get(i).unwrap_or("")`、
   IO 读错 `unwrap_or_default`、开关抑制、删除/非支持文件盲区，均无信号。宜在 `run.warnings` 或 cell 记录
   暴露「本次卷宗为空/降级/被抑制」（也利于诊断「卷宗内容稀疏 vs 模型忽略」）。

## T25-余 · 自收敛路径无修复轮 🧊（T24 残留半件；tokens 半件已随 M18 批2 交付归 done）

T24 的截断修复轮只加在 FINALIZE 失败点；探索轮里模型提前吐最终文本（无 tool_calls）时若同样被截断，
`tool_loop.rs` 自收敛点仍直接报废整格。dogfood 观察到的失败都在 FINALIZE 路径，故 T24 刻意范围外——
数据门控：观察到自收敛路径也掉格再把同款一次修复轮对称加上去。

## T02 · 分段读 mini-patch ⏳（**支柱 3 半件**，2026-07-04 grilling Q1 从 M14 设计树剥离）

`src/tools/mod.rs::do_read_file` 已支持 `start`/`end` 行段；补一条「无 range 且文件超阈值 → 返回大纲 +
请指定行段」即砍掉大文件整读的膨胀单元。与卷宗**零代码共享**（卷宗走 bundle→prompt 注入，此件走 tool
executor 返回值）；无缓存对冲（five-pillars「分段读无此冲突可先行」），独立可发。只需定两参数=文件大小
阈值 + 大纲返回格式，一个 mini-task。注意：属 token 轮廓类变更，live 收编/A-B 窗口期间禁动。
（支柱 3 另一半「陈旧结果驱逐」硬门控于 L1 缓存观测数据——见 `../roadmap/followup.md` M14 尾注。）

## T03 · 支柱 5 欠账：refute 分账 + 每轮成本估算 ⏳（2026-07-06 支柱对账固化）

Task 14 回放实证：两代 run.usage 均只记 fan-out，**refute（skeptic 复核）那趟 token 未入账**——
`verify.rs` 零 usage 记录。five-pillars 支柱 5 的完整定义=per-cell 轮数（✅ iterations）/ explore·refute
分账（❌）/ cached（✅）/ 每轮成本估算进 result.json 与 status（❌）。补齐后 A/B 与 ship 简报的成本
口径才完整。落点：refute 路径 LensCall usage 聚合进 run.usage（或独立 refute_usage 字段，机读契约加法）。

## T04 · git() 全量路径 quotePath 缺口 ⏳（2026-07-04 M13 终审顺带发现，先于该分支的既有问题）

`evidence/git.rs` 顶部老 `git()`（collect_code/changed_files 用）未关 core.quotePath——非 ASCII 文件名
在全量评审里会被逐文件 pathspec 配不上而静默滤掉。修法：同 git_at 一行 `-c core.quotePath=false`。
顺手项：给 `incremental_preflight` 的 ensure 文案加「被 ship.md 守卫分发子串匹配引用，改动须同步」注释
（防漂移；漂移后果=误归降级全量档，方向安全只多烧成本）。

## T05 · ship.md 停机条件感知半失败 ⏳（2026-07-03 记，M13 评审 codex #6 拆出的既有问题）

`failed_models` 非空时 verdict 仍可 approve 但 exit=2（cli.rs `exit_code`），而 ship.md 停机只看
`verdict=approve` → 半失败轮（部分模型挂、对抗火力不足）会被当合法 approve 收工。修法候选：停机判定改
「verdict=approve **且** failed_models 空」，或简报强制注记半失败。M13 健康基线守卫（R2）已堵「半失败作
carry 基线」的一半，这条管停机本身。独立批次（一批不混两个协议变更）。
见 `../reviews/2026-07-03-incremental-rounds-eng-review.md` §3。

## T06 · 发布前移除 dogfood 自反馈仪器 ⏳（2026-07-02 记；M12 定位=开发期临时仪器）

`plugins/redteam/feedback.md` + ship.md 收工派 subagent 段 + observations scratch + `~/.redteam/feedback/`
中枢（design：`../design/2026-07-02-breakpoint-dogfood-self-feedback-design.md`，文内旧名按 CLAUDE.md 对照读）
是打磨 redteam 用的**非产品特性**。发布前整体拆除：删 feedback.md、ship.md 的 feedback 段、codex 镜像对应物
→ `make gen`/`make verify` 复绿。无 Rust/packaging 回滚（引擎零改动）。

## T07 · ship rounds 目录层级两层化 ⏳（2026-07-03 用户提出；简报格式已交付 be6167f，目录层级漏改）

第三轮 dogfood 起手实证仍建单层 `rounds/20260703-064707/`。改 `.redteam/rounds/<date>/<round_start_time>/`
两层；rN.result.json 存档步随 feedback 拆除（T06）一并处理（result 已持久入库随取随有）。纯 ship.md 协议层改动+gen。

## T08 · 停机汇总 session.md 产品/模板化 ⏳（2026-07-03 用户提出）

session.md 也是给人看的，现状宿主自由发挥——需像 rN.md 一样定模板（轮数/修复/驳回统计、守卫注记、遗留
观察骨架）。落点待定：纯 ship.md 模板段（宿主拼）vs 二进制聚合渲染（`redteam session summary` 读 proj.db
多 job 聚合；内置命令优先倾向后者，但轮次↔job 映射目前只在宿主手里——先从 ship.md 模板段起步最薄）。

## T09 · `watch <job>` 阻塞等终态子命令 ⏳（2026-07-02 r6 dogfood e967def 派生）

把 ship 轮询状态机（间隔/终态判断）下沉进二进制，宿主一次阻塞调用替代 prompt 层轮询循环。内置命令优先
原则的下一个毕业候选；等 ship 实测中轮询确成痛点再做。

## T10 · adversarial prompt 整体重排/调优 🧊（2026-07-04 grilling Q8 尾定延后）

`adversarial-code-bundle.md` 等模板累积多段（role/rules/stance/tools/finding_bar/bypass/output/
{{INCREMENTAL_ROUND}}/{{DOSSIER}}），段序与措辞到「害理解/模型误配权重」时值得整体重排。重排=token 轮廓类
变更，须独立 A/B（与卷宗等同批会毁单变量归因）；与 ship.md 协议类 TODO 同桶，live 达标解冻后统一开一批。
等真触发（dogfood 显示段序害权重）再动。

## T11 · 二进制分发改走 GitHub Release，bin 退出 git 🧊（2026-07-04 用户定性：bin 入仓属决策失误）

bin 入仓是 marketplace 分发权宜。等 M10/OpenCode 或发布里程碑一并重排分发面（CI 交叉构建出 Release 资产，
marketplace 壳层引 Release URL 或安装脚本）。近端已用 select_files 滤 Binary diff（7e2a127）兜住评审噪声。

## T12 · 其他子命令行为层核查 🧊（2026-07-02 记，用户点名但另立）

status/result/model/init 的行为与参数语义深层过一遍（非 help 文案层，那已在 M5 批A 覆盖）。
等具体痛点或下一轮 dogfood 驱动。

## T13 · result --format md 骨架 i18n 🧊（2026-07-03 用户点出，定 A 暂缓）

内置渲染器模板骨架（Summary/Severity/Field/Value/Recommendation…）硬编码英文——内容层已随
OUTPUT_LANGUAGE 免费本地化，骨架英文=devtool 惯例（cargo/git 同例）。若出现非英文骨架真需求：
render_md 单点加 locale 表头字典（~30 行，output_language() 已在），zh/en 起步；代价=每语言译名
永续维护。等真用户需求再上。

## T49 · 二阶段/增量轮 prompt 前缀稳定性 × provider 缓存 ⏳（2026-07-23 上下文管理答疑派生；后置 M17-b）

现状：二阶段补查为**置换式**——needs 文件前插 augmented bundle（cli.rs `second_pass_prompt`），
第一/二趟 prompt 前缀完全不同，provider 端 prompt cache 必 miss，256K 级 pack 整份 input 付两遍；
增量轮轮间同理。方案：pack 段字节级稳定、needs 证据改在其后**追加**——第二趟命中缓存前缀，大 pack
时省约一半 input 费（增量轮吃轮次链缓存同理）。须先重开一次小设计裁决（置换 vs 追加：置换当初为
预算纯度，但 needs 触发时预算通常有富余）。属 token 轮廓类变更，独立成批 A/B；**后置于 M17-b**
（provider 翻转后 single_shot 路径稳定再动，现在动是在旧骨架上打补丁）。

## T50 · needs 观测驱动主动注入 🧊（2026-07-23 派生；数据门控）

`needs_stats`（触发格数/声明条数）已入 RunMeta。启动条件=**积累若干轮（建议 5+）dogfood 的 needs
数据**后统计模型反复点名的文件类别（类型定义/trait 接口/配置等），预算有余时把高频类别主动进
pack——用数据回答「变更集外什么值得给」，不回已安乐死的 SCIP 路线。现在没数据，任何设计都是猜，
勿提前动。

## T53 · 窗口预算最大化 + LITM 验证门 ⏳（2026-07-28 768K 帽批派生；观察触发）

**现状**：帽 256K→768K 已交付（merge 0ae7a8d）。768K ≈ 1M 窗的 ~22%（真实 3.5B/tok 折算 ≈219K tok），
上限锚 gpt-5.6 272K 计费悬崖。用户裁决：贵不如效率重要、悬崖可特化处理——剩余窗口值得吃，但后置。

**最大化路径**（两步，机制零新增）：
1. **悬崖入配置**：VibeCodingFast 用 `--context-tokens 256K` 重声明（272K 是它的经济窗口、1M 只是技术窗口）
   → 窗口墙替它算出 ~696K，全局常数不再承载计费语义。
2. **帽退役为纯注意力护栏**：绝对帽提到 1.5-2M——带 gpt 的编队被其窗口墙自动压线下；
   纯国产队（DeepSeek/Kimi/MiniMax 全 1M 无悬崖）真正用上大窗。
   注意两步必须同做：只做 1 不做 2 会把全队预算压到 696K（比现状还小 9%）。

**LITM 验证门**（提帽前置门；朴素 A/B 不可行——两臂 pack 内容不同，稀释与缺证据混淆）：
- **方案 3·观察性去混淆（常态，零成本）**：大盘子评审跑两档回放时，recall 比较条件化在
  「finding 锚定证据在两臂 pack 均在场」子集上——精确省略清单逐坐标可机械判定 quote 落没落在
  被裁区间。证据双在场只有大臂漏 → 稀释实锤；小臂漏且证据在其 omit 清单 → 缺证据（预算收益实锤）。
- **方案 1·位置扫描（触发式，单预算档即可）**：固定 768K 满载，轮换已知缺陷文件在 pack 位置
  （头/中/尾，填充序近→远确定性可控），看 recall 随位置曲线。中段掉 = LITM 实锤。
  触发条件：观察到 findings 向 pack 头部聚集（proj.db cell 明细现在就能查）。
- **方案 2·植入缺陷+良性填充（要造靶，备用）**：已知缺陷文件两档都装下，L 臂追加无害文件撑满；
  指标只看植入缺陷 recall（M13 刹车靶/M14 2-pre 同款手艺）。
- 缓解因素（不立专项的理由）：768K 仅巨型变更集 bind（日常盘子两档 pack 逐字节同）；
  2026 代 1M 窗模型 LITM 弱于 2024 代；pack 结构化（坐标头+近→远排序）非均质长文。

**关联**：T49 缓存前缀稳定性（帽越大 cache-miss 代价越大，gpt-5.6 cache write 1.25x 官方条款）；
T27-余 真 tokenizer 校准（3B/tok 记账保守度影响悬崖余量计算）。

## T54 · hibot audit exec 遗留：`partial_fixture` 间歇失败 ⏳（2026-11-25 交付期记录；根因未定位）

- 现象：`tests/job_conformance.rs::partial_fixture_uses_missing_provider_key_and_reports_partial`
  在全量并行 `cargo test` 中偶发失败；单跑该测试、单跑该 test binary、以及重跑全量均通过。
- 已排除：v2/Phase 4/Phase 6 代码变更（多轮对照无规律）；`cargo test --no-fail-fast` 在 main CI 全绿。
- 待查：该夹具用本地端口构造 401，并行时端口/临时目录竞争、`seal_job_env` 进程级环境变量、`partial` 计数时序；
  可改成不依赖本地端口的确定性失败方式。
- 影响：CI 误报；hibot 联调时可能误判为执行单元回归。
- 证据：各阶段 `evidence/cargo-test.log`（`.trellis/tasks/archive/2026-11/`）。

## T55 · 诊断 pointer 口径（`config check` / WARN） ⏳（2026-11-25）

- `missing_role:<role>` 的 pointer 落在 `/providers`；对 `settings.verify.models` / `settings.triage.models` /
  `settings.review.roles.<role>.models` 这类池缺失应指向对应池路径。
- `providers.<p>.models[{id}] 未被任何角色池引用` 在 v2 数组结构下写法不对：应为 `models[<index>]` 或 `models["id"]`。
- 影响：hibot 渲染/运维按 pointer 定位时被误导；仅诊断质量，不影响判定。
- 落点：`src/config/mod.rs`（check/guard 的 pointer 拼接）、`src/cli/config_cmd.rs`；补断言。

## T56 · verify 增强的两条 followup ⏳（2026-11-25 U7 增强期记录）

- **文档类修复**：`<fix_scope>` 只给文件名；当实质修复落在 `.md`/文档时，模型只读代码文件就判 `resolved`
  （#400 b/Q2）。候选：doc-only 场景给受限摘录（仍不进 quote 校验池），或在 prompt 明确“文档类修复需看文件内容”。
- **非确定性交互**：模型对“同一请求内回退/重选”不敏感（#400 a/affinity：`:158` 拒绝 → `:174` 回退）。
  B-lite 已把引用行与快照放进 prompt，但模型仍判 resolved。候选：blast 段显式提示“回看同一请求内的回退路径”。
- 约束：不改人工基线（`expected` 不随跑出结果调整）；真实模型稳定性只观测、不作门禁。
- 证据：`.trellis/tasks/archive/2026-11/11-25-hibot-verify-enhance/`、`scripts/verify-pr400.sh`。

## T57 · config 本地子命令优化 & 交互式配置 ⏳（2026-11-25 用户提出）

- 现状：`config schema|check` 面向 executor 契约；本地配置仍靠手写 `~/.redteam/config.json`，
  `model setup` 只覆盖部分字段，v2 的 `settings.*`/角色池/`models[].api` 没有友好的编辑入口。
- 目标：本地模式（非 job）提供 `config init` / `config edit`（编辑器）/ `config validate`，
  以及交互式向导：provider(`api`/`baseUrl`/凭据 env) → `models[]`（含 `api`/`thinkingLevelMap`）
  → `settings.{triage,review.roles,verify}.models` → `language`。
- 约束：不改变 job/executor 契约；密钥只写 `secrets.json` 或环境变量（`apiKey` 仅本地可选，job 仍 `secret_field`）；
  若新增本地专用键需单独评审（可能牵动 schemaVersion 语义）。
- 关联：v2.0.1 已支持 `models[].api/apiKey` 覆盖；向导需覆盖这两项与 v1→v2 迁移提示。

## T58 · triage / verify 独立运行适配与改造 ⏳（2026-11-25 用户提出）

- 现状：`triage` / `verify` 只有 `--job-dir` 外派形态，本地无法直接对当前分支/PR 跑；
  本地 review 则支持 `--base/--scope/--spec` 等。
- 目标：提供本地入口（如 `triage --base main`、`verify --findings <file> --base <sha>`），
  复用同一管线与报告层，明确与本地 review 的配置、台账（dismissed/resolve）与增量轮边界。
- 约束：不破坏 job 模式与契约；不引入第二套判定/解析；verify 仍需 anchored findings、指纹覆盖不变量；
  本地台账默认关闭（与 job 一致），除非显式打开。
- 关联：T57 的本地配置入口、`settings.verify.models`。

## T59 · hibot runner 端到端（外部依赖） ⏳（2026-11-25 Phase 6 记录）

- 现状：hibot `apps/` 为空、M0/M1 未开始；已完成的是**手工 job 目录**联调（三 kind、schema/指纹/quote/覆盖/心跳）。
- 触发：hibot M0/M1 落地后，跑 runner → job 目录 → 执行单元 → report 接纳/上传全链路。
- 本仓不需改契约；若联调暴露问题按新阶段处理。
- 证据：`.trellis/tasks/archive/2026-11/11-25-hibot-phase6-acceptance/delivery/hibot-handoff.md`。

