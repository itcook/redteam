---
description: Loop adversarial review until verdict=approve — adjudicate, fix, dismiss, re-review; with dismissed ledger and escalation brakes
disable-model-invocation: true
---

# ship — 循环对抗评审直到 approve

驱动「评审 → 裁决 → 修复/驳回 → 再评审」循环直到 `verdict = approve`。评审器：
`${CLAUDE_PLUGIN_ROOT}/bin/redteam`。

用户参数：$ARGUMENTS
- `--max-rounds <N>`（本命令专属，可选）：预算护栏；未给则**无轮数上限**——目标是 approve。
- 其余参数**原样透传**给 `redteam review`（--base/--scope/--spec/--focus 等；模型集由 `~/.redteam/config.json` 的角色定，没有 `--models` 这类旗）。
  透传前先摘除上述专属参数。redteam 开启 `args_override_self`（同名 flag 重复后者胜），
  ship 管理的 `--background` 追加在用户参数之后即权威生效；唯一需连值摘除的是
  `--resume <值>`（它切同步补跑、打印全报告而非 job id，破坏循环）。凡摘除不得静默。
- **台账 zero-config**：驳回判例台账在 `.redteam/proj.db`，`redteam review` 自动加载
  （豁免 + refute 判例裁决），无需传任何 flag；报告 `run.ledger` 披露加载数。
  用户显式传 `--dismissed`/`--no-dismissed` 时原样透传（那是合法的覆盖语义）。
  **split-brain 警示**：显式 `--dismissed <文件>` 下评审豁免走该文件，而 `dismiss add`
  写的是 proj.db——本会话新建卡对后续轮**不生效**。此场景误报不建卡，记简报交用户自决。

## 0. 起手
1. 运行 `${CLAUDE_PLUGIN_ROOT}/bin/redteam init`（幂等）；创建本会话目录
   `.redteam/rounds/<YYYYMMDD>/<HHmmss>/`（两层：日期目录下再分秒级会话目录，本地时间）。
   后文 `rounds/<session>/` 均指代此两层路径，不逐处展开。
2. 先跑项目测试命令，红树即停（先修好基线，不拿坏基线喂评审）。
   **测试命令**（本会话缓存结论）：调 `${CLAUDE_PLUGIN_ROOT}/bin/redteam test-command` 取——
   推断链在二进制里（`--override` > `Cargo.toml`→`cargo test` > `package.json` 有 `scripts.test`→`npm test` >
   `Makefile` 有 test 目标→`make test` > `go.mod`→`go test ./...`），与 `review --test-command` 同源。
   退出码 2 = 推不出 → 先看项目 CLAUDE.md/记忆约定，仍无则 AskUserQuestion 一次并把答案记入 session.md；
   明确无测试的项目跳过测试步。

## 0.1 跨轮状态重建（铁律：不信会话记忆）
跨轮判定一律**从盘上重建**，不依赖本次对话上下文里记住的轮次/id 状态——长会话
compaction 后模型记忆不可靠，唯一可信源是磁盘文件与 db 台账。三条重建口：
- **轮次链/轮数**：`ls rounds/<session>/` 数 `rN.result.json` 序列即为已完成轮数；
  每个 job id 读该文件内 `run.job_id`（下一轮 `--incremental-from` 的值即从此文件重建，
  不必记在上下文里）。
- **复裁资格**（每 id 至多一次升级复裁）：`${CLAUDE_PLUGIN_ROOT}/bin/redteam dismiss show <id>`
  的 status + history——已复裁过 vs 首次再现，查这里定，不靠记忆比对。
- **修复历史/销案争议资格**：`${CLAUDE_PLUGIN_ROOT}/bin/redteam resolve show <id>` 的
  history 条数——判断该 id 是否已被销案 ≥2 次（见下文销案争议查表口径）。

## 每轮 rN
1. **起评审**：
   - **r1（或无健康增量基线时）**：`${CLAUDE_PLUGIN_ROOT}/bin/redteam review <透传参数> --background`（全量）。
   - **r2 起（用户参数带 --base，即 branch scope）**：追加 `--incremental-from <上一轮 job id>`
     ——只审上轮 stamped HEAD 以来已提交的 fix delta，未动代码的 findings 由 redteam 重锚定携带；
     verdict 仍是合并集口径（携带 ∪ 新发），**不要**自己把旧 findings 复述进 --focus。
     **前提：本轮修复已全部 commit（工作树净）**——delta 只含已提交改动。
     working-tree scope：不加该 flag，照旧全量循环。
   - **守卫报错处置**（redteam 显式拒跑，不静默降级）：谱系（rebase/reset）/同族/健康基线/无 stamp/
     无持久化 result 报错 → 本轮**去掉 `--incremental-from` 改跑全量**，简报注记原因与守卫名；
     **空 delta 或 delta 全被噪声过滤报错**（"empty delta" / "all noise-filtered"）→ 按 §停机
     停滞守卫处理（空转，停，交用户）；**脏工作树报错**（"clean working tree"）→ 停，先处置未
     提交改动（评审前的修复必须已 commit；改全量并不消除问题）。
   （`--background` 放在全部参数之后——`args_override_self` 语义下后者胜。）
   → 记下 job id（**增量链条的下一环**：本轮 job id 就是下轮 `--incremental-from` 的值）。
   轮询 `${CLAUDE_PLUGIN_ROOT}/bin/redteam status <job>`（间隔约 20-30s），只等**终态**
   （done / failed / timed_out / cancelled）。redteam 后台 job 有 40 分钟 wall-clock deadline watchdog（到点自我
   终止并出部分报告），**永不永久卡死**——因此**不要自己判卡死去 cancel**。
   `cancelled`（外部 `redteam cancel` 中止）→ 视为用户主动中止，停轮、报告用户，不算 approve。
   - `last_active`/`phase` 仅作进度信息（活着/慢/尾段），**不作 cancel 依据**：推理模型的超长单轮
     turn 与 `finalizing` 尾段的 `last_active` 设计上静止，按「心跳停滞 N 分钟」判死会误杀正常慢 job。
   - **轮询期间不改动工作树**（评审与携带重锚定正在读活文件）。
2. **取结果**：`${CLAUDE_PLUGIN_ROOT}/bin/redteam result <job>` → 存档到
   `rounds/<session>/rN.result.json`——**链持久化协议义务，非取用便利**：跨轮状态
   重建（§0.1）依赖此文件序列存在且可 `ls` 枚举，此步不可省略；同时供内循环
   `dismiss add --from` 与收轮 `resolve add --from` 读取快照四件套。
   - job `failed/timed_out` 但有部分报告（`verdict: incomplete`）→ 用部分 findings
     照常裁决，简报注记 partial，**不算 approve**。
   - job 硬失败无产出（全 provider 挂等）→ 重试一次，再失败 → 停，报告用户。
     基础设施故障不消耗轮次、不触发停滞守卫。

## 内循环（逐条裁决）
1. **分拣**：`verify == refuted` 一般跳过（redteam 已豁免，简报记一笔）。**例外需目检**：
   refute note 是判例引用、或 finding body 明确反驳某条驳回理由的 → 读一眼 note 与 body：
   反驳被正面回应 → 跳过；note 答非所问（打稻草人/复述原理由）→ 该条**照常裁决**，一句话
   记简报——机器证伪不当剥夺强异议的复裁通道。`run.resolution_disputed` 中的 id 不适用
   refuted 跳过——即使其 verify=refuted，也按 §3 争议通道处理（销案争议优先于机器证伪）。
   `previously_dismissed` 按台账状态分流（`${CLAUDE_PLUGIN_ROOT}/bin/redteam dismiss show
   <id>` 读 status）：`human-confirmed` 或已 `re-upheld` 的静默跳过；仅 `dismissed` 状态的**首次再现**
   进入刹车链（每 id 至多一次）。
   增量轮报告的 `run.resolution_disputed` 非空 → 这些 id 是「已销案却再现」的假修复信号，
   在 §3 按属实项优先处理。
2. **排序**：severity 降序为主序，同级按 file 分组。增量轮先看 origin（rN.md 的 Origin 行 /
   JSON 的 `.origin`）：`carried` = 上轮遗留（多为存疑未决——已裁决过的要么已 resolve 要么已
   dismiss），不重复全量裁决，只推进其未决状态；`new` = 本轮 delta 新发，照常逐条裁决。
3. **逐条**（读 quote + 文件上下文 + body 后裁决）：
   - **属实 → 修**：root-cause 修法——共享函数一处守卫优于逐调用点补丁。
   - **resolution_disputed 项 → 按修复未生效处理**：重读代码确认——确实没修好 → 重修
     （修好后收轮时重新 resolve）；模型误判（其实已修好）→ 按误报走 dismiss 建卡。
     **查表判定（非记忆比对）：该 id ∈ 本轮 `run.resolution_disputed` 且
     `resolve show <id>` history 已有 ≥2 条 → 走 §停机 销案争议刹车**（语义等价于
     「修过两次仍 disputed」：查表口径与原「第二次出现」记忆比对判的是同一件事）。
   - **误报 → 建判例卡**：`${CLAUDE_PLUGIN_ROOT}/bin/redteam dismiss add <id>
     --reason "<驳回理由>" --from rounds/<session>/rN.result.json --round rN`
     （快照四件套由 --from 自动抄写，勿手拼；理由写实质论据——它是未来 refute
     判例裁决的依据文本）。
   - **存疑 → 先验证再定，禁止直接猜**：能执行就执行（微测试/复现脚本）。
     **绕过类/输入处理类强制走 fuzz 配方**：① 从 finding 提取被绕过的校验点与输入空间
     维度（编码层数/大小写/分隔符/scheme…）；② 对维度组合写 property 测试（内联枚举
     生成即可，不引新依赖）；③ 跑测试枚举出全部失败变体，一次定性整类；④ 修复走类级
     （normalizer/单点归一），逐 case 补丁视为未完成；⑤ property 测试留在项目测试套件
     作回归资产。验证后仍不可判 → 攒到轮末批量 AskUserQuestion，不逐条打断。
   - 先验：`agreement ≥ 2` 加强属实先验；upheld 孤证正常裁决（verify 是意见不是判决）。

## 收轮
修复全落 → 跑测试命令至绿 → **单次 commit（仅修复，不含 .redteam/）**，消息
`fix(ship): rN <一句话摘要>`
→ **销案**（增量链条的关键一环，紧跟 commit）：对本轮每个「属实→已修」的 finding id 逐个
  `${CLAUDE_PLUGIN_ROOT}/bin/redteam resolve add <id> --from rounds/<session>/rN.result.json --round rN --reason "<一句修法>"`
  （快照四件套由 --from 自动抄写，与 dismiss add 对称；已修复≠误报，落独立表）。
  下一轮增量据此**不再携带**该 finding；若其 quote 未随修复消失，会自动进下轮「顺带验证」
  ——假修复会以 `resolution_disputed` 回来。**存疑未决项不 resolve**（携带到下轮继续在场）。
→ 生成 `rounds/<session>/rN.md` 简报：
1. `${CLAUDE_PLUGIN_ROOT}/bin/redteam result <job> --format md > rounds/<session>/rN.md`
   （产品化模板：Summary/severity 表/Info 表/Warnings/Finding Details 全由二进制渲染，勿手拼）；
2. 把首行标题补上轮次后缀：`# <repo> Redteam Review #<job>` → `…#<job>-round <N>`；
3. 文末**追加裁决段**（协议层内容，宿主写）：`## Adjudication` ——修复/驳回/存疑清单，
   属实项带 origin 标（carried/new，报告已算好）、销案 id 清单、本轮 fix commit sha。
→ 下一轮。

## 停机（每轮内循环完成后判定；即使 verdict=approve，待刹车 id 先走完刹车链)
1. **verdict = approve**（合并集口径：携带 ∪ 本轮新发，redteam 已算好）：
   - **`run.failed_models` 为空**（对照集完整）→ 看 `run.convergence`（M21c 批 C；旧报告无此键）：
     - **`converged: true`**，或**该键缺失** → 收工。缺失 = 本轮算不出收敛（diff 重建不了），
       按旧行为收口——不因为少一个新字段改变既有判定。
     - **`converged: false` 且 `activation_overrun > 0`** → **停车交人**：有疑点被 delta 反复激活
       却始终挖不出结果，机制已给过自愈机会。`AskUserQuestion` 交用户裁决（报告的
       `Not converged` 行与 warning 里有具体是哪几条），按「停机（活锁）」记判定行，不算 approve。
     - **`converged: false` 且只是 `delta_adjacent > 0`** → **不收工**，再跑一轮：
       台账里还有疑点落在本轮改过的文件上没挖完（多半是上一轮被预算截断的），
       下一轮预算重置、delta 多半已缩小，有机会消化。`--max-rounds` 护栏照常生效。
       判定行仍按最终那一轮的结果记。
   - **`run.failed_models` 非空**（半失败：N 个模型未参战，对抗火力不全）→ **不收工**，
     `AskUserQuestion` 二选一交用户裁决：
     - 「接受降级 approve（N 模型未参战，对抗火力不全）」→ 用户接受后按 approve 收工，
       但下方 session.md 模板的判定行记 **「降级 approve（用户接受）」**（不得记「approve」）。
     - 「重跑本轮」→ 不收工，回到本轮起评审重跑（同一轮次号，不新开 rN）。
   - 收工时写 `rounds/<session>/session.md`，**固定模板**（宿主填值，禁自由发挥，
     不得增删字段/改措辞）：
     ```markdown
     # ship session <date>/<time>
     - 轮数：N（r1..rN，job id 清单）
     - 判定：approve | 降级 approve（用户接受）| 停机（<守卫名>）
     - 统计：修复 X · 驳回 Y（新judgment卡 Z）· 销案 W · 复裁 V
     - 半失败注记：<有则列轮次与 failed_models>
     - carried 遗留：<id+一句话 | 无>
     - 遗留观察：<非 finding 级备忘 | 无>
     ```
     向用户报告可 ship。**可选终扫**：用户需要发版级信心时，建议手动裸跑一次全量
     `redteam review`（不带 --incremental-from）——非默认步骤，成本一整轮，收益是对
     跨文件耦合的最后一次全量对抗。
2. **销案争议刹车**：**查表判定（非记忆比对）**：id ∈ 本轮 `run.resolution_disputed`
   且 `resolve show <id>` history 已有 ≥2 条 → 停，AskUserQuestion 交人裁决（修复者与
   评审者死锁不该机器无限循环；对称升级刹车链的「每 id 至多一次复裁」精神；语义等价于
   原「同一 id 第二次出现」记忆比对判据——修过两次仍 disputed = 死锁交人）。
3. **升级刹车链**（每 id 至多一次）：`run.dismissed_reappeared` 中处于 `dismissed` 状态
   的 id → 对照卡上驳回理由（`dismiss show <id>`），带新 finding body **复裁一次**：
   改判属实 → 修 + `dismiss set-status <id> withdrawn --round rM`；仍判误报 →
   `dismiss set-status <id> re-upheld --round rM`，**停，AskUserQuestion 交人裁决**。
   人裁误报 → `set-status <id> human-confirmed --round rK`，此后该 id 永久静默；
   人裁属实 → 修 + `set-status <id> withdrawn --round rK`，继续。
4. **修复受阻**：某 finding 属实但你修不了（需产品/架构决策、改动越权）→ 停，交用户。
   working-tree scope 的全量循环若陷自指尾流僵局（修复自身反复被重审出衍生问题、approve 不可达），
   也按本条停机交人。
5. **停滞守卫**：本轮仍有未豁免 findings 但零代码改动（未修也未新驳）→ 空转，停，交用户。
   增量轮的「empty delta」守卫报错同走本条（无新改动可审 = 空转）。
6. `--max-rounds` 达限（仅用户显式给出时）→ 停，输出未收敛摘要。轮数**按目录计数**：
   `ls rounds/<session>/` 中 `rN.result.json` 的数量即已完成轮数，不自数（§0.1 铁律）。

> 尾流守卫已随增量协议**退役**：增量轮的新 finding 定义上全是「衍生」（delta 就是上轮修复），
> 「连续全衍生」失去判别力；自指尾流问题由「未动代码携带判决、只审 delta」结构性消解，
> approve 经合并集天然可达。
> （此消解仅对增量轮成立；不携带的全量循环——working-tree scope——按上述 §4 出口交人。）
