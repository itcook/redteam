# breakpoint dogfood 自反馈 — 设计（纯 prompt 编排）

> 定位:lgtm 循环在评审主线**之外**捕获 **breakpoint 工具自身**(brk + lgtm)的缺陷/改进建议 +
> 一份**确定性量化记分卡**,按 run 汇总到**跨 repo 中枢**。把本项目反复出现的"dogfood 破案"
> 从轶事变成**可追踪信号**。

**日期**:2026-07-02 · **状态**:待评审 · **范围**:纯 prompt(feedback.md + lgtm.md 一段),**brk 内部零改动**

> ⚠️ **开发期临时仪器,发布前整体移除**。本功能是开发阶段打磨 brk 的脚手架,**不是产品特性**。
> 纯 prompt 编排 → 移除面只有:`plugins/breakpoint/feedback.md`、`lgtm.md` 的 feedback 段、codex 镜像。
> **不碰任何 Rust**。移除已挂 `.doc/TODO.md`。故一切取舍偏向"零侵入、一刀可删":独立 feedback.md、
> 独立 scratch、独立中枢目录,不与生产评审路径/二进制耦合。lgtm.md 里该段显式标「本地调试阶段(发布前移除)」。

---

## 1. 目标与非目标

**目标**
- 每次 lgtm run 结束,subagent 产出:①**量化记分卡**(brk 这一 run 表现的可追踪指标);②**定性缺陷/改进项**。
- 记录**集中汇总**到 home 中枢,**永不落进被审 repo**,跨 repo 可比。
- 主线负担 ≈ 零;**brk 二进制零改动**。

**非目标(明确出界)**
- ❌ **不总结被审对象的缺陷**——对象是**工具**,不是被审代码。故 M9「运动员做裁判」顾虑**不适用**。
- ❌ **不用 hook**;**不改 brk**(不加 `feedback record` 子命令——纯 prompt 由 subagent 完成)。
- ❌ **不测漏报(recall)**、**不做加权合成魔数分**(见 §4 效度警告)。
- ❌ **v1 不覆盖裸 `brk review`**(无 lgtm 裁决 → 无 real/false 信号);仅 lgtm 路径。
- ❌ **不自动毕业**进 roadmap/TODO;原始遥测常驻中枢,值得动手的由人**手工毕业**。

---

## 2. 架构:纯 prompt,subagent 执行

三根支柱:
1. **对象 = 工具自身**。反馈讲 brk/lgtm 好不好用、准不准,不是被审代码有没有 bug。
2. **subagent 全干,brk 不动**。lgtm 收工时派**只读+可写中枢**的 subagent,令其读 `feedback.md`(格式/步骤规范)
   执行。确定性靠 **jq 配方**(不是 Rust,也不是模型脑内数);中枢写盘靠 subagent **继承父会话 bypass**(§5)。
3. **记分卡分两层**:Tier-1 机械项(jq 精确算)、Tier-2 判定项(从简报数)。见 §4。

**为什么 subagent 而非宿主自写**:长 lgtm 会话的宿主上下文保持干净;合成任务有界、适合委派;
**不阻塞收工判定**(反馈失败不影响 approve/停机)。

---

## 3. 组件

### 3.1 `plugins/breakpoint/feedback.md`(新,dev-time,subagent 的执行规范)
内容 = 给 subagent 的**指令 + 格式契约**:
- **输入**:`<session>` 目录(`rN.result.json`、`rN.md` 简报、dismissed 台账、`observations.md`)。
- **Tier-1 机械指标(用 jq 精确算,附确定 jq 配方)**:`raised`(跨轮去重 id)、`refuted`(`verify=refuted`)、
  `agreement_ge2`、`anchor_fail`(`line_start==0`)、`dismissed_reappeared`、`provider`(`failed_models`/`partial`)、`rounds`。
  <!-- 确切 jq 配方在实现期对齐真实 result JSON schema;设计只锁"经 jq 精确算,不脑内数"。 -->
- **Tier-2 判定指标(从 `rN.md` 简报的修复/驳回/存疑清单数)**:`{real, false, uncertain}` 三整数。
- **stamping(经 Bash)**:`ts`(`date -u`)、`repo`(git remote 或 workspace basename)、`brk_version`(`brk --version`)。
- **定性 digest**:合成 `observations.md` 活体一行 + 产物异常(warnings/failed_models/anchor_fail)→ 缺陷/改进项。
- **输出(subagent 写,§5 权限)**:向 `${BRK_FEEDBACK_DIR:-$HOME/.breakpoint/feedback}/scorecard.jsonl`
  **追加一行**(§4 schema,Bash 追加);写 `.../<session>.md` = 记分卡渲染 + 定性 digest。
- **必带 §4 三条效度警告**(防误用)。

### 3.2 中枢目录 `~/.breakpoint/feedback/`
- `scorecard.jsonl` — 一行一 run,**跨所有 repo** 汇总,看趋势(`jq`;人类友好渲染归 M5 `--format table` 那批)。
- `<session>.md` — 该 run 的记分卡 + 定性反馈,人读。
- **永不落被审 repo**;`$BRK_FEEDBACK_DIR` 覆盖(如指向 breakpoints 开发 repo 的 `.doc/dogfood/`)。
<!-- ponytail: scorecard.jsonl 单行 Bash 追加假设 lgtm run 非并发(单用户顺序);真并发再上锁,YAGNI。 -->

### 3.3 观察 scratch(lgtm 主线,仅异常时一行)
- `.breakpoint/rounds/<session>/observations.md`(**在**被审 workspace 内,与 `rN.md` 同级,gitignored 工作数据)。
- 主线**仅撞见 brk/lgtm 异常**才追加一行(耗时异常、finalizing 卡、quote 配不上文件、协议摩擦…)。
  无异常不写 → 主线负担≈零。feedback.md 令 subagent 把它折进定性 digest。

### 3.4 `lgtm.md` 改动(收轮追加一步)
- 每轮 rN:异常 → 追加 `observations.md` 一行。
- 收工(停机判定后,**不阻塞判定**)追加一段,**显式标「本地调试阶段(发布前移除)」**:
  派只读+可写中枢的 subagent,令其读 `${CLAUDE_PLUGIN_ROOT}/feedback.md` 并按其执行;失败仅告警,不影响收工。
- codex 镜像:`make gen` 同步 lgtm.md;feedback.md 亦随插件分发(`${CODEX_PLUGIN_ROOT}/feedback.md`)。

---

## 4. 记分卡:schema + 效度警告

`scorecard.jsonl` 单行:
```json
{
  "ts": "2026-07-02T14:30:00Z",
  "repo": "breakpoints",
  "session": "20260702-143000",
  "mode": "code",
  "models": ["DeepSeek", "Kimi"],
  "brk_version": "0.x.y",
  "rounds": 3,
  "raised": 14, "refuted": 4, "agreement_ge2": 6,
  "anchor_fail": 2, "dismissed_reappeared": 1,
  "provider": { "failed_models": [], "partial": false },
  "adjudication": { "real": 7, "false": 3, "uncertain": 0 },
  "headline_precision_proxy": 0.70
}
```
`headline_precision_proxy = real / (real + false)`。Tier-1 各计数经 jq 精确算;Tier-2 `adjudication` 从简报数。

**三条效度警告(必须随 headline 标注)**:
1. **地面真值是宿主裁决,非客观准确**。宿主是运动员(写/拥有被审代码),有动机把真缺陷当误报驳掉 →
   `false` 被污染。→ **方向性/相对**可用,**绝对值不可信**。
2. **只测 precision,测不了 recall**。单标量会藏起 precision/recall 张力:优化它 → brk 变胆小 → 漏报涨、分反更好看。
   **故不做合成分**,只留记分卡看趋势。
3. `uncertain` 归错或 escalation 悬而未决会轻微污染 `real`——接受(escalation 罕见)。

**价值定位**:不是给 brk 打分排名,是**趋势信号**——prompt/lens/引擎改动后,
`raised/refuted/anchor_fail/precision_proxy` 的走向告诉你"这次改动让 brk 更准还是更糙"。

---

## 5. 权限(已核查坐实,2026-07-02)

纯 prompt 方案下,写中枢的是 **subagent**(非 brk),故走 CC 权限——但已验证可行:
- **subagent 继承父会话权限模式**(frontmatter `permissionMode` 被忽略)。宿主评审跑 bypass → subagent 亦 bypass。
- **bypass 真跳过工作区外写**(例外仅显式 `ask` 规则 + `rm -rf /`~`~` 断路器)→ subagent 写 `~/.breakpoint/` 放行。
- **非-bypass 兜底**:顶级 `additionalDirectories: ["~/.breakpoint/"]`(settings)或 `--add-dir`(CLI)预授权。
- 前提是本工具本就用于 **bypass 下的开发期 dogfood**,让渡(不再走 brk-native 写)可接受。
- 文档:`permission-modes#bypasspermissions` / `sub-agents#configure-subagents` / `permissions#working-directories`。

---

## 6. 测试 / 验证

- **无 Rust 改动 → 无新单测**。
- feedback.md 的 **jq 配方**在实现期对齐真实 `rN.result.json` schema 后,用一个 fixture session 手验:jq 数出的
  Tier-1 计数与人工核对一致(确定性验证)。
- 端到端:发布前 self-hosting dogfood 走通一遍(lgtm 收工 → subagent 出记分卡 + digest 落中枢)。
- `make verify`:lgtm.md ↔ codex 镜像不漂移(含新增 feedback 段 + feedback.md 分发)。

---

## 7. 已砍 / 延后(YAGNI)

- **不加 `brk feedback record` 子命令**(pivot 2026-07-02):纯 prompt + jq,brk 零改动,移除面最小。
- **不新增 `verdicts.jsonl` 主线产物**:real/false 由 subagent 从既有简报数。
- **不做 `brk feedback show` 趋势渲染器**:`jq` 够用;人类友好视图与 M5 `--format table` 同批。
- **不做 template-hash 指纹**:`brk_version` 足够关联"换构建后分变了"。
- **per-run 一行**(非 per-round);**裸 `brk review` 路径不做**。

---

## 8. 交付物清单(纯 prompt)

- `plugins/breakpoint/feedback.md`(新):subagent 执行规范(Tier-1 jq 配方 + Tier-2 数法 + stamping + 输出 + 效度警告)。
- `plugins/breakpoint/commands/lgtm.md`:①每轮异常→observations 一行;②收工派 feedback subagent 一段(标「本地调试阶段·发布前移除」)。
- `codex-skills/lgtm/SKILL.md` + feedback.md 镜像:`make gen` 同步。
- roadmap 落一条(建议新里程碑「dogfood 自反馈」,拟排 M5 前;标注发布前移除)。
- **`src/` 零改动**。

---

## 9. 发布前移除清单(已挂 TODO)

删 `plugins/breakpoint/feedback.md` + lgtm.md 的 feedback 段 + codex 镜像对应物 → `make gen`/`make verify` 复绿。
无 Rust/packaging 回滚。恢复干净发布面。
