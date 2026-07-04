---
name: ship
description: Loop adversarial review until verdict=approve — adjudicate, fix, dismiss, re-review; with dismissed ledger and escalation brakes
---

# ship — 循环对抗评审直到 approve

驱动「评审 → 裁决 → 修复/驳回 → 再评审」循环直到 `verdict = approve`。评审器：
`${CODEX_PLUGIN_ROOT}/bin/redteam`。

用户参数：$ARGUMENTS
- `--max-rounds <N>`（本命令专属，可选）：预算护栏；未给则**无轮数上限**——目标是 approve。
- 其余参数**原样透传**给 `redteam review`（--mode/--base/--scope/--spec/--lenses/--models 等）。
  透传前先摘除上述专属参数。redteam 开启 `args_override_self`（同名 flag 重复后者胜），
  ship 管理的 `--background` 追加在用户参数之后即权威生效；唯一需连值摘除的是
  `--resume <值>`（它切同步补跑、打印全报告而非 job id，破坏循环）。凡摘除不得静默。
- **台账 zero-config**：驳回判例台账在 `.redteam/proj.db`，`redteam review` 自动加载
  （豁免 + refute 判例裁决），无需传任何 flag；报告 `run.ledger` 披露加载数。
  用户显式传 `--dismissed`/`--no-dismissed` 时原样透传（那是合法的覆盖语义）。
  **split-brain 警示**：显式 `--dismissed <文件>` 下评审豁免走该文件，而 `dismiss add`
  写的是 proj.db——本会话新建卡对后续轮**不生效**。此场景误报不建卡，记简报交用户自决。

## 0. 起手
1. 运行 `${CODEX_PLUGIN_ROOT}/bin/redteam init`（幂等）；创建本会话目录
   `.redteam/rounds/<YYYYMMDD-HHmmss>/`（本地时间，秒级）。
2. code 模式：先跑项目测试命令，红树即停（先修好基线，不拿坏基线喂评审）；doc 模式跳过。
   **测试命令探测**（本会话缓存结论）：`.redteam/config.toml` 的 `test_command` >
   `Cargo.toml`→`cargo test` > `package.json` 有 `scripts.test`→`npm test` >
   `Makefile` 有 test 目标→`make test` > 项目 CLAUDE.md/记忆约定 > 都推断不出 →
   AskUserQuestion 一次并把答案记入 session.md；明确无测试的项目跳过测试步。

## 每轮 rN
1. **起评审**：`${CODEX_PLUGIN_ROOT}/bin/redteam review <透传参数> --background`
   （`--background` 放在用户透传参数之后——`args_override_self` 语义下后者胜。）
   - 台账自动生效（proj.db）：豁免既往驳回 + 疑似判例变体强制进 refute 判例裁决——
     不要自己读台账拼 --focus 摘要；用户 $ARGUMENTS 里的 `--focus` 原样透传即可。
   → 记下 job id。轮询 `${CODEX_PLUGIN_ROOT}/bin/redteam status <job>`（间隔约 20-30s），只等**终态**
     （done / failed / timed_out / cancelled）。redteam 后台 job 有 40 分钟 wall-clock deadline watchdog（到点自我
     终止并出部分报告），**永不永久卡死**——因此**不要自己判卡死去 cancel**。
     `cancelled`（外部 `redteam cancel` 中止）→ 视为用户主动中止，停轮、报告用户，不算 approve。
   - `last_active`/`phase` 仅作进度信息（活着/慢/尾段），**不作 cancel 依据**：推理模型的超长单轮
     turn 与 `finalizing` 尾段的 `last_active` 设计上静止，按「心跳停滞 N 分钟」判死会误杀正常慢 job。
   - **轮询期间不改动工作树**（working-tree scope 下评审正在读活文件）。
2. **取结果**：`${CODEX_PLUGIN_ROOT}/bin/redteam result <job>` → 存档到
   `rounds/<session>/rN.result.json`（结果已持久于 proj.db 可随时重取；此文件存档
   仅供收工反馈的 jq 配方读取——**发布前随 feedback 仪器一并拆除本步**）。
   - job `failed/timed_out` 但有部分报告（`verdict: incomplete`）→ 用部分 findings
     照常裁决，简报注记 partial，**不算 approve**。
   - job 硬失败无产出（全 provider 挂等）→ 重试一次，再失败 → 停，报告用户。
     基础设施故障不消耗轮次、不触发停滞守卫。

## 内循环（逐条裁决）
1. **分拣**：`verify == refuted` 一般跳过（redteam 已豁免，简报记一笔）。**例外需目检**：
   refute note 是判例引用、或 finding body 明确反驳某条驳回理由的 → 读一眼 note 与 body：
   反驳被正面回应 → 跳过；note 答非所问（打稻草人/复述原理由）→ 该条**照常裁决**，一句话
   记简报——机器证伪不当剥夺强异议的复裁通道。`previously_dismissed`
   按台账状态分流（`${CODEX_PLUGIN_ROOT}/bin/redteam dismiss show <id>` 读 status）：
   `human-confirmed` 或已 `re-upheld` 的静默跳过；仅 `dismissed` 状态的**首次再现**
   进入刹车链（每 id 至多一次）。
2. **排序**：severity 降序为主序，同级按 file 分组。
3. **逐条**（读 quote + 文件上下文 + body 后裁决），每条属实项标注**来源：原始/衍生**——
   衍生 = quote 锚定的代码由本会话既往 `fix(ship): rN` commit 引入。**衍生标注必须附
   引入 commit 的短 sha**（`git blame` 取证，不凭记忆），写进简报——标注是停机「尾流
   守卫」的判据输入，必须留下可审计线索，防自证式标签空转：
   - **属实 → 修**：root-cause 修法——共享函数一处守卫优于逐调用点补丁。
   - **误报 → 建判例卡**：`${CODEX_PLUGIN_ROOT}/bin/redteam dismiss add <id>
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
`fix(ship): rN <一句话摘要>` → 生成 `rounds/<session>/rN.md` 简报：
1. `${CODEX_PLUGIN_ROOT}/bin/redteam result <job> --format md > rounds/<session>/rN.md`
   （产品化模板：Summary/severity 表/Info 表/Warnings/Finding Details 全由二进制渲染，勿手拼）；
2. 把首行标题补上轮次后缀：`# <repo> Redteam Review #<job>` → `…#<job>-round <N>`；
3. 文末**追加裁决段**（协议层内容，宿主写）：`## Adjudication` ——修复/驳回/存疑清单，
   属实项带**原始/衍生**标（衍生附引入 commit 短 sha）、本轮 fix commit sha。
→ 下一轮。
- **工具观察（本地调试阶段 · 发布前移除）**：若本轮察觉 redteam/ship **工具级**异常（耗时反常、
  `finalizing` 卡顿、某 finding 的 quote 在文件里配不上、status/协议摩擦），往
  `rounds/<session>/observations.md` 追加**一行**——供收工反馈合成;无异常不写。

## 停机（每轮内循环完成后判定；即使 verdict=approve，待刹车 id 先走完刹车链)
1. **verdict = approve** → 收工：写 `rounds/<session>/session.md` 总结（轮数、修复/
   驳回统计、遗留观察），向用户报告可 ship。
2. **尾流守卫**：活跃分支上每轮修复自身会成为下轮评审对象，approve 可能结构性不可达
   （自指尾流）。**连续 2 轮属实项全为衍生、且属实数不增**（本轮 ≤ 上轮）→ 判定
   **原始问题面已清空**，收工（协议级视同达成目标）：session.md 如实注明「按尾流守卫
   收工；最后一轮衍生修复未经再评审——有限停机的固有代价」。衍生数反弹（修复引入了
   新真问题群）→ 守卫不触发，循环继续。**仅属实项 ≥1 的轮计入连续判定**——零属实轮
   不是「全衍生」（空集真空真值不作数），走 approve/停滞守卫路径。**与 approve 同约束：
   `dismissed_reappeared` 中待复裁的 id 必须先走完 §3 升级刹车链，方可按本条收工**——
   尾流出口不得吞掉待复裁信号。
3. **升级刹车链**（每 id 至多一次）：`run.dismissed_reappeared` 中处于 `dismissed` 状态
   的 id → 对照卡上驳回理由（`dismiss show <id>`），带新 finding body **复裁一次**：
   改判属实 → 修 + `dismiss set-status <id> withdrawn --round rM`；仍判误报 →
   `dismiss set-status <id> re-upheld --round rM`，**停，AskUserQuestion 交人裁决**。
   人裁误报 → `set-status <id> human-confirmed --round rK`，此后该 id 永久静默；
   人裁属实 → 修 + `set-status <id> withdrawn --round rK`，继续。
4. **修复受阻**：某 finding 属实但你修不了（需产品/架构决策、改动越权）→ 停，交用户。
5. **停滞守卫**：本轮仍有未豁免 findings 但零代码改动（未修也未新驳）→ 空转，停，交用户。
6. `--max-rounds` 达限（仅用户显式给出时）→ 停，输出未收敛摘要。

## 收工反馈（本地调试阶段 · 发布前移除）
> 开发期临时仪器,发布前连同 `${CODEX_PLUGIN_ROOT}/feedback.md`、本节、observations 一行约定一并删除。
> 反馈对象是 **redteam/ship 工具自身**（好不好用、准不准），非被审代码。

会话终结时（§停机 任一路径：approve 收工 / 停交用户），**写完 `session.md` 之后**派一个反馈 subagent
（**对被审工作树只读，仅写 home 中枢**）：
令其读 `${CODEX_PLUGIN_ROOT}/feedback.md` 并按其执行——产出工具自身的**量化记分卡** + **缺陷/改进项**，
写入 home 中枢 `${REDTEAM_FEEDBACK_DIR:-~/.redteam/feedback}`。派发时把会话目录 `rounds/<session>/` 与
dismissed 台账目录路径交给它。**本步不阻塞收工判定**：失败仅告警，不改变 approve/停机结论、不消耗轮次、
不碰被审工作树。
