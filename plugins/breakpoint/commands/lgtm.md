---
description: Loop adversarial review until verdict=approve — adjudicate, fix, dismiss, re-review; with dismissed ledger and escalation brakes
disable-model-invocation: true
---

# lgtm — 循环对抗评审直到 approve

驱动「评审 → 裁决 → 修复/驳回 → 再评审」循环直到 `verdict = approve`。评审器：
`${CLAUDE_PLUGIN_ROOT}/bin/brk`。

用户参数：$ARGUMENTS
- `--max-rounds <N>`（本命令专属，可选）：预算护栏；未给则**无轮数上限**——目标是 approve。
- `--dismissed-dir <path>`（本命令专属，可选）：驳回台账目录，默认 `.breakpoint/dismissed/`。
- 其余参数**原样透传**给 `brk review`（--mode/--base/--scope/--spec/--lenses/--models 等）。
  透传前先从参数串中摘除上述两个专属参数。

## 0. 起手
1. 运行 `${CLAUDE_PLUGIN_ROOT}/bin/brk init`（幂等）；创建本会话目录
   `.breakpoint/rounds/<YYYYMMDD-HHmmss>/`（本地时间，秒级）。
2. code 模式：先跑项目测试命令，红树即停（先修好基线，不拿坏基线喂评审）；doc 模式跳过。
   **测试命令探测**（本会话缓存结论）：`.breakpoint/config.toml` 的 `test_command` >
   `Cargo.toml`→`cargo test` > `package.json` 有 `scripts.test`→`npm test` >
   `Makefile` 有 test 目标→`make test` > 项目 CLAUDE.md/记忆约定 > 都推断不出 →
   AskUserQuestion 一次并把答案记入 session.md；明确无测试的项目跳过测试步。

## 每轮 rN
1. **组装 --focus**：读 `<dismissed-dir>/*.md`，用各文件的 title + 驳回理由生成摘要：
   「以下问题已人工裁决为误报，勿再报：…」。台账为空则不加 --focus。
   若用户 $ARGUMENTS 自带 `--focus <内容>`，两者**合并为一条** `--focus` 值再透传——
   clap 不接受重复 flag：合并顺序为用户内容在前、驳回摘要附后（用换行或分号分隔均可）；
   透传前先从 $ARGUMENTS 摘除原始 `--focus` 及其值，避免重复传参。
2. **起评审**：`${CLAUDE_PLUGIN_ROOT}/bin/brk review --background --dismissed <dismissed-dir> [--focus <合并后内容>] <透传参数（已摘除 --focus）>`
   → 记下 job id。轮询 `${CLAUDE_PLUGIN_ROOT}/bin/brk status <job>`（间隔约 20-30s）：
   - `last_active` 持续前进 = 活着（慢 ≠ 死）；`phase` 走到 `finalizing` 属正常尾段。
   - 心跳停滞超过 ~5 分钟 → `${CLAUDE_PLUGIN_ROOT}/bin/brk cancel <job>` 后重起本轮。
   - **轮询期间不改动工作树**（working-tree scope 下评审正在读活文件）。
3. **取结果**：`${CLAUDE_PLUGIN_ROOT}/bin/brk result <job>` → 存档到
   `rounds/<session>/rN.result.json`。
   - job `failed/timed_out` 但有部分报告（`verdict: incomplete`）→ 用部分 findings
     照常裁决，简报注记 partial，**不算 approve**。
   - job 硬失败无产出（全 provider 挂等）→ 重试一次，再失败 → 停，报告用户。
     基础设施故障不消耗轮次、不触发停滞守卫。

## 内循环（逐条裁决）
1. **分拣**：`verify == refuted` 跳过（brk 已豁免，简报记一笔）。`previously_dismissed`
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
修复全落 → 跑测试命令至绿 → **单次 commit（仅修复，不含 .breakpoint/）**，消息
`fix(lgtm): rN <一句话摘要>` → 写 `rounds/<session>/rN.md` 简报（job id、verdict、
修复/驳回/存疑清单、commit sha）→ 下一轮。

## 停机（每轮内循环完成后判定；即使 verdict=approve，待刹车 id 先走完刹车链)
1. **verdict = approve** → 收工：写 `rounds/<session>/session.md` 总结（轮数、修复/
   驳回统计、遗留观察），向用户报告 LGTM。
2. **升级刹车链**（每 id 至多一次）：`run.dismissed_reappeared` 中处于 `dismissed` 状态
   的 id → 对照台账里当时的驳回理由，带新 finding body **复裁一次**：改判属实 → 修 +
   删台账文件；仍判误报 → 台账追加 `re-upheld(rM)`，**停，AskUserQuestion 交人裁决**。
   人裁误报 → 追加 `human-confirmed(rK)`，此后该 id 永久静默；人裁属实 → 修 + 删文件，继续。
3. **修复受阻**：某 finding 属实但你修不了（需产品/架构决策、改动越权）→ 停，交用户。
4. **停滞守卫**：本轮仍有未豁免 findings 但零代码改动（未修也未新驳）→ 空转，停，交用户。
5. `--max-rounds` 达限（仅用户显式给出时）→ 停，输出未收敛摘要。
