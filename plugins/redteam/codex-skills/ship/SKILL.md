---
name: ship
description: Loop adversarial review until verdict=approve — adjudicate, fix, dismiss, re-review; with dismissed ledger and escalation brakes
---

# ship — 循环对抗评审直到 approve

驱动「评审 → 裁决 → 修复/驳回 → 再评审」循环直到 `verdict = approve`。评审器：
`${CODEX_PLUGIN_ROOT}/bin/redteam`。

用户参数：$ARGUMENTS
- `--max-rounds <N>`（本命令专属，可选）：预算护栏；未给则**无轮数上限**——目标是 approve。
- `--dismissed-dir <path>`（本命令专属，可选）：驳回台账目录，默认 `.redteam/dismissed/`。
- 其余参数**原样透传**给 `redteam review`（--mode/--base/--scope/--spec/--lenses/--models 等）。
  透传前先从参数串中摘除上述两个专属参数；`--dismissed` 是 **ship 管理的 flag**（起评审时由
  ship 注入台账目录，clap 拒绝重复 flag）——若用户误传 `--dismissed`，一并摘除，以 `--dismissed-dir` 为准。

## 0. 起手
1. 运行 `${CODEX_PLUGIN_ROOT}/bin/redteam init`（幂等）；创建本会话目录
   `.redteam/rounds/<YYYYMMDD-HHmmss>/`（本地时间，秒级）。
2. code 模式：先跑项目测试命令，红树即停（先修好基线，不拿坏基线喂评审）；doc 模式跳过。
   **测试命令探测**（本会话缓存结论）：`.redteam/config.toml` 的 `test_command` >
   `Cargo.toml`→`cargo test` > `package.json` 有 `scripts.test`→`npm test` >
   `Makefile` 有 test 目标→`make test` > 项目 CLAUDE.md/记忆约定 > 都推断不出 →
   AskUserQuestion 一次并把答案记入 session.md；明确无测试的项目跳过测试步。

## 每轮 rN
1. **起评审**：`${CODEX_PLUGIN_ROOT}/bin/redteam review --background --dismissed <dismissed-dir> <透传参数>`
   - `--dismissed` 自带双效：豁免既往驳回 + **台账内容自动注入评审 prompt 的「勿再报」段**——
     不要自己读台账拼 --focus 摘要；用户 $ARGUMENTS 里的 `--focus` 原样透传即可。
   → 记下 job id。轮询 `${CODEX_PLUGIN_ROOT}/bin/redteam status <job>`（间隔约 20-30s），只等**终态**
     （done / failed / timed_out）。redteam 后台 job 有 40 分钟 wall-clock deadline watchdog（到点自我
     终止并出部分报告），**永不永久卡死**——因此**不要自己判卡死去 cancel**。
   - `last_active`/`phase` 仅作进度信息（活着/慢/尾段），**不作 cancel 依据**：推理模型的超长单轮
     turn 与 `finalizing` 尾段的 `last_active` 设计上静止，按「心跳停滞 N 分钟」判死会误杀正常慢 job。
   - **轮询期间不改动工作树**（working-tree scope 下评审正在读活文件）。
2. **取结果**：`${CODEX_PLUGIN_ROOT}/bin/redteam result <job>` → 存档到
   `rounds/<session>/rN.result.json`。
   - job `failed/timed_out` 但有部分报告（`verdict: incomplete`）→ 用部分 findings
     照常裁决，简报注记 partial，**不算 approve**。
   - job 硬失败无产出（全 provider 挂等）→ 重试一次，再失败 → 停，报告用户。
     基础设施故障不消耗轮次、不触发停滞守卫。

## 内循环（逐条裁决）
1. **分拣**：`verify == refuted` 跳过（redteam 已豁免，简报记一笔）。`previously_dismissed`
   按台账状态分流：`human-confirmed` 或已 `re-upheld` 的静默跳过；仅 `dismissed(rN)` 状态
   的**首次再现**进入刹车链（每 id 至多一次）。
2. **排序**：severity 降序为主序，同级按 file 分组。
3. **逐条**（读 quote + 文件上下文 + body 后裁决）：
   - **属实 → 修**：root-cause 修法——共享函数一处守卫优于逐调用点补丁。
   - **误报 → 写 `<dismissed-dir>/<id>.md`**：内容 = finding 快照（title/file/quote/
     severity）+ 驳回理由 + 状态行 `dismissed(rN, <日期>)`。
   - **存疑 → 先验证再定，禁止直接猜**：能执行就执行（微测试/复现脚本）。
     **绕过类/输入处理类强制走 fuzz 配方**：① 从 finding 提取被绕过的校验点与输入空间
     维度（编码层数/大小写/分隔符/scheme…）；② 对维度组合写 property 测试（内联枚举
     生成即可，不引新依赖）；③ 跑测试枚举出全部失败变体，一次定性整类；④ 修复走类级
     （normalizer/单点归一），逐 case 补丁视为未完成；⑤ property 测试留在项目测试套件
     作回归资产。验证后仍不可判 → 攒到轮末批量 AskUserQuestion，不逐条打断。
   - 先验：`agreement ≥ 2` 加强属实先验；upheld 孤证正常裁决（verify 是意见不是判决）。

## 收轮
修复全落 → 跑测试命令至绿 → **单次 commit（仅修复，不含 .redteam/）**，消息
`fix(ship): rN <一句话摘要>` → 写 `rounds/<session>/rN.md` 简报（job id、verdict、
修复/驳回/存疑清单、commit sha）→ 下一轮。
- **工具观察（本地调试阶段 · 发布前移除）**：若本轮察觉 redteam/ship **工具级**异常（耗时反常、
  `finalizing` 卡顿、某 finding 的 quote 在文件里配不上、status/协议摩擦），往
  `rounds/<session>/observations.md` 追加**一行**——供收工反馈合成;无异常不写。

## 停机（每轮内循环完成后判定；即使 verdict=approve，待刹车 id 先走完刹车链)
1. **verdict = approve** → 收工：写 `rounds/<session>/session.md` 总结（轮数、修复/
   驳回统计、遗留观察），向用户报告可 ship。
2. **升级刹车链**（每 id 至多一次）：`run.dismissed_reappeared` 中处于 `dismissed` 状态
   的 id → 对照台账里当时的驳回理由，带新 finding body **复裁一次**：改判属实 → 修 +
   删台账文件；仍判误报 → 台账追加 `re-upheld(rM)`，**停，AskUserQuestion 交人裁决**。
   人裁误报 → 追加 `human-confirmed(rK)`，此后该 id 永久静默；人裁属实 → 修 + 删文件，继续。
3. **修复受阻**：某 finding 属实但你修不了（需产品/架构决策、改动越权）→ 停，交用户。
4. **停滞守卫**：本轮仍有未豁免 findings 但零代码改动（未修也未新驳）→ 空转，停，交用户。
5. `--max-rounds` 达限（仅用户显式给出时）→ 停，输出未收敛摘要。

## 收工反馈（本地调试阶段 · 发布前移除）
> 开发期临时仪器,发布前连同 `${CODEX_PLUGIN_ROOT}/feedback.md`、本节、observations 一行约定一并删除。
> 反馈对象是 **redteam/ship 工具自身**（好不好用、准不准），非被审代码。

会话终结时（§停机 任一路径：approve 收工 / 停交用户），**写完 `session.md` 之后**派一个反馈 subagent
（**对被审工作树只读，仅写 home 中枢**）：
令其读 `${CODEX_PLUGIN_ROOT}/feedback.md` 并按其执行——产出工具自身的**量化记分卡** + **缺陷/改进项**，
写入 home 中枢 `${REDTEAM_FEEDBACK_DIR:-~/.redteam/feedback}`。派发时把会话目录 `rounds/<session>/` 与
dismissed 台账目录路径交给它。**本步不阻塞收工判定**：失败仅告警，不改变 approve/停机结论、不消耗轮次、
不碰被审工作树。
