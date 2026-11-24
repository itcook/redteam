# codex 对抗评审 harness 机制拆解（重建前的现状分析）

> 研究目的：在重建一套**更强壮、模型无关（可配置换用其他 LLM）、更高效**的对抗评审工具前，
> 先吃透现有 `until-codex-approve` + `codex:adversarial-review` 这套机制的运作逻辑、耦合点与瓶颈。
> 本文是逆向分析快照，作为新 session 的起点上下文。日期：2026-06-24。

## 重建动机（要解决的两个痛点）

1. **强依赖 codex**：无法脱离 codex CLI + GPT LLM 环境；OpenAI 服务不稳定时整套不可用。
2. **耗时长**：一轮评审 >10 分钟很常见。

目标：保留它有效的部分（对抗 prompt + 结构化收敛 + 核实循环），把"怎么调一个模型"抽象成可替换层，并改成更高效的并行形态。

---

## 0. 关键认知：这是「两层」，不是一层

日常用的 `until-codex-approve` 与它底层调的 `codex:adversarial-review` 职责完全不同。**耦合 codex 的只有内层；外层早已是模型无关的纯 Claude 编排。**

```
┌─ 外层：until-codex-approve（SKILL.md，纯 Claude 编排，无 codex 依赖）
│   职责：场景判定 → 算 base → 跑一轮 → 核实 findings(驳回/修复) → fixup 提交 → 重审
│   退出条件：最近一轮 verdict === "approve"
│   "对抗"含义①：跨轮博弈 —— Claude 驳回误报、修属实项，codex 重审
│
└─ 内层：codex:adversarial-review（codex 插件，单轮只读评审，强耦合 GPT/codex CLI）
    职责：收 git diff → 填对抗 prompt 模板 → 调 codex app-server → 拿结构化 {verdict,findings}
    "对抗"含义②：单轮内的对抗立场（prompt 让模型"以挑刺为目标"）
```

外层 = `~/.claude/skills/until-codex-approve/SKILL.md`（154 行），它**直接 `node codex-companion.mjs`**，
绕开 `/codex:adversarial-review` slash 命令（后者 `disable-model-invocation: true`，模型无法触发）。

文件位置：
- 外层 skill：`~/.claude/skills/until-codex-approve/SKILL.md`
- 内层插件：`~/.claude/plugins/marketplaces/openai-codex/plugins/codex/`
  （另有版本化缓存副本 `~/.claude/plugins/cache/openai-codex/codex/<version>/`，外层用 `sort -V | tail -1` 动态解析，禁止硬编码版本号）

---

## 1. 外层循环（until-codex-approve）逻辑

退出条件**唯一**：最近一次 codex 评审 `verdict === "approve"`。codex 只负责评审；**核实与修复由 Claude 完成**。

- **步骤 0 前置检查**：Node 就绪、动态解析 `codex-companion.mjs` 路径、codex CLI 就绪（`codex app-server --help`）。缺一即停、清晰报错。
- **步骤 1 场景判定**：
  - 场景 A 文档/规格：`--scope working-tree`，前台 `--wait`，循环里改文档。
  - 场景 B 实施代码：`--base <base> --scope branch`，按体量自动选前/后台，循环里改代码 + fixup 提交。
  - 场景 B 的 base 计算：团队流程"先文档后代码"，要**跳过开头那串纯文档提交**，只审实现代码（用 `DOC_RE` 正则逐提交前移 base）；开审前必须打印被排除/被评审的提交并请用户确认。
  - 收集 `SPEC_DOCS`：被排除的文档提交涉及的文档路径，喂进 `$FOCUS` 让 codex 据此核对实现是否偏离设计意图。
- **步骤 2 单轮评审**：`node "$SCRIPT" adversarial-review --wait --json [--base .. --scope branch] "$FOCUS"`。
  - `$FOCUS` 每轮必带三段：**穷尽性指令**（"尽可能全面找全，宁细勿漏"）+ **规格文档位置声明**（代码场景）+ **累计驳回清单**。
- **步骤 3 判定与循环**：
  - `approve` → 写记录、退出。
  - `needs-attention` → **逐条对抗性核实**（先验证再实施，结合 `confidence`）：属实→修；误报→记入 `dismissed`、不改。
  - 场景 B 有修复 → 追加 fixup 提交（`git add -A -- ':!**/reviews/'` 排除评审记录），让 `base...HEAD` 反映修复。
  - 本轮全驳回 → 把 dismissed 拼成 focus 回喂下一轮让 codex 重新考虑。
  - **升级刹车**：同一已驳回 finding 后续轮再现且仍判误报 → 停止循环，交用户裁决（防死循环）。
  - 不设硬轮次上限，循环至 approve。
- **每轮收尾**：派后台 subagent（haiku）把本轮记录写到 `DOC_DIR/reviews/{doc|code}/round-NN.md`，主循环不等待。

**重建启示**：这一整层的"核实-驳回-修复-重审-升级刹车"逻辑**已经模型无关**，是纯编排，可整套保留/复用。

---

## 2. 内层执行链（codex 耦合全在这）

`node codex-companion.mjs adversarial-review [--base <ref>] [--scope branch] "<focus>"`：

```
parseCommandInput()                    解析 --base/--scope/--model/--wait/--background/--json
  ↓
resolveReviewTarget(cwd,{base,scope})  → {mode:'working-tree'|'branch', label, baseRef}
  ↓
collectReviewContext(cwd, target)      跑 git，产出喂模型的"证据包"
  ↓                                    ├ working-tree: git diff --cached + git diff + untracked
  │                                    └ branch: merge-base + git diff <base>...HEAD
  │                                    阈值：fileCount≤2 且 diff≤256KB 才内联完整 diff，
  │                                    否则只给"文件清单 + 让模型自己去看"的轻量指引
  ↓
buildAdversarialReviewPrompt()         读 prompts/adversarial-review.md，{{占位符}}插值
  ↓
runAppServerTurn(turn/start)           ← 真正调 codex（见 §3）
  ↓
parseStructuredOutput(finalMessage)    → {verdict, findings[], summary, next_steps}
```

关键文件/函数（行号为逆向时估值，可能随版本漂移）：
- `scripts/codex-companion.mjs`：CLI 入口、`executeReviewRun()`、`buildAdversarialReviewPrompt()`
- `scripts/lib/codex.mjs`：`runAppServerTurn()`、`captureTurn()`、`parseStructuredOutput()`、stderr 清理、model alias
- `scripts/lib/app-server.mjs`：JSON-RPC 客户端、`withAppServer()` 重试降级
- `scripts/lib/git.mjs`：`resolveReviewTarget()`、`collectReviewContext()`
- `scripts/lib/state.mjs` / `job-control.mjs` / `tracked-jobs.mjs`：job 生命周期与存储
- `scripts/lib/prompts.mjs`：`loadPromptTemplate()` + `interpolateTemplate()`（`{{KEY}}` 正则替换）

---

## 3. 它到底怎么调 codex（最重的耦合）

**不是 `codex exec`，而是 `codex app-server` 的 JSON-RPC（JSONL over stdio）。** 这是慢和脆的根源。

- 调用：`client.request("turn/start", { threadId, input:[{type:"text",text:prompt}], model, effort, outputSchema })`
  - `ephemeral: true`（thread 不持久化）；sandbox `read-only`（评审不改文件）。
- **app-server broker**（`app-server-broker.mjs`）：长驻代理进程，经 unix socket **复用一个 app-server 连接 + 鉴权**，给多个 `codex-companion` 调用路由请求（`turn/start`/`review/start` 互斥排队）。broker 忙（RPC code `-32001`）时降级直连。
- **完成检测**：通知流状态机 `turn/started → item/completed → turn/completed`，外加 finalAnswer + 无 pending subagent 才算完，250ms 宽限兜底。
- **两条评审路径分叉**：
  - `/codex:review`（native）→ `review/start`，用 codex 内置评审引擎。
  - `/codex:adversarial-review` → `turn/start` + 自定义 prompt + 自定义 schema。**「对抗」区别于「普通 review」的全部技术实现就在这条分叉。**
- **后台 job 存储**：`$CLAUDE_PLUGIN_DATA/state/` 或 `/tmp/codex-companion/`，按 workspace 路径 sha256 分桶；`state.json` + `jobs/<id>.json` + `<id>.log`。`status`/`result` 子命令读文件，轮询 2s、默认等 4min。

---

## 4. 对抗 prompt + schema（最值钱的资产，模型无关，可直接搬）

### 4.1 对抗 prompt 模板 `prompts/adversarial-review.md`（84 行）

灵魂段落（全文逐 `<tag>` 摘要）：
- `<role>`：**"Your job is to break confidence in the change, not to validate it."**
- `<operating_stance>`：默认怀疑；happy-path 才跑通即算缺陷；不给"良好意图/部分修复/会跟进"任何加分。
- `<attack_surface>`（钦定优先攻击面）：鉴权/越权/租户隔离/信任边界；数据丢失/损坏/重复/不可逆；回滚/重试/部分失败/幂等；竞态/顺序假设/陈旧状态/重入；空态/null/超时/降级依赖；版本漂移/schema 漂移/迁移风险/兼容回归；可观测性盲区。
- `<review_method>`：主动证伪——找被破坏的不变量、缺失的守卫、未处理失败路径；追踪坏输入/重试/并发/半完成操作如何流经代码；用户给了 focus 则重点加权但仍报其他实质问题。
- `<finding_bar>`：只报实质问题，禁 style/命名/低价值清理/无证据猜测；每条须答"会出什么错 / 为何脆弱 / 影响 / 怎么改"。
- `<structured_output_contract>`：只返回符合 schema 的 JSON；有任何值得阻断的实质风险用 `needs-attention`；**只有无法给出任何实质对抗 finding 才 `approve`**；每条须含 file/line_start/line_end/confidence/recommendation；summary 写成 ship/no-ship 判断而非中性复述。
- `<grounding_rules>`：必须可由提供上下文佐证，**禁止脑补**文件/行/代码路径/攻击链/运行时行为；依赖推断要显式标注、confidence 据实。
- `<calibration_rules>`：宁要一条硬的，不要一堆灌水；看着安全就直说并返回 0 findings。
- `<final_check>`：交付前自检每条 finding 是否 adversarial（非 stylistic）、绑定具体代码位置、在真实失败场景下可信、对修复工程师可操作。
- 模板占位符：`{{TARGET_LABEL}}` `{{USER_FOCUS}}` `{{REVIEW_COLLECTION_GUIDANCE}}` `{{REVIEW_INPUT}}`。

### 4.2 输出 schema `schemas/review-output.schema.json`（双保险：prompt 要求 + RPC outputSchema 强制）

```jsonc
{
  "verdict": "approve" | "needs-attention",   // 二值收敛信号，外层循环靠它退出
  "summary": string,                           // ship/no-ship 判断
  "findings": [{
    "severity": "critical" | "high" | "medium" | "low",
    "title": string, "body": string,
    "file": string, "line_start": int>=1, "line_end": int>=1,
    "confidence": number 0~1,                   // 外层据此决定核实力度
    "recommendation": string
  }],
  "next_steps": [string]
}
```

---

## 5. 痛点根因定位

### 5.1 强依赖 codex —— 耦合点其实很集中，只有三处

| 耦合 | 位置 | 性质 |
|---|---|---|
| app-server JSON-RPC 协议 + broker | `lib/app-server.mjs`、`app-server-broker.mjs` | codex 私有协议，无法移植 |
| `turn/start` 的 `model`/`effort`/`outputSchema` 参数语义 | `lib/codex.mjs` | GPT 专有（structured output、reasoning effort 枚举） |
| reasoning summary 提取、model alias（spark→gpt-5.3-codex-spark）、effort 枚举 | `lib/codex.mjs`、`codex-companion.mjs` | GPT 专有 |

**反之，下列全是模型无关的通用骨架，可整段复用思路/代码**：
- 对抗 prompt 模板 + 输出 schema（§4，**最有价值，直接搬**）
- git 证据收集 + 256KB/2-file 内联阈值（`collectReviewContext`）
- CLI 参数 / job 生命周期 / 前后台 / status-result 轮询 / 工作区分桶存储
- 外层 `until-codex-approve` 的整个核实-驳回-修复-重审-升级刹车循环

### 5.2 一轮 >10min —— 瓶颈非常明确

1. **GPT 推理本身占 80–95%**，随 effort 指数放大（low 2–5min / medium 5–15min / high 15–30min），大 diff 进一步拉长推理链。
2. app-server 首次启动 + 可能重鉴权：30–60s。
3. git diff 收集、completion 检测、JSON 解析：合计个位数秒，可忽略。

**核心结论：慢不在 harness，在"单进程、单模型、串行跑一个高 effort 的整体评审"这个形态。harness 本身很轻。**

---

## 6. 重建设计支点（边界已划清）

1. **要保留的不是 codex，是 prompt + schema + verdict 二值收敛 + 外层核实循环**。这四样全模型无关，且是工具真正有效的原因。
2. **唯一要换的是 §3 那层"怎么把 prompt 送进一个模型、怎么拿回符合 schema 的 JSON"**。抽象成 `ReviewProvider` 接口（输入 prompt+schema，输出结构化 findings），即可挂任意 LLM：
   - Claude：tool-use / 强制 JSON schema；
   - 任意模型：prompt 内嵌 schema + 解析 + 失败重试。
3. **效率上的更优形态**：codex 是"一个大模型串行高 effort 扫全量"。换成 Claude 原生编排可以：
   - **按攻击面维度并行 fan-out**（鉴权/数据/并发/边界各派一个 reviewer）；
   - **对每条 finding 并行对抗核实**（多个 skeptic 投票驳回误报）；
   - 早退 + 去重 + 多视角，墙钟从 10min 串行 → 分钟级并行，且天然多模型、更抗误报。
   - 对应 Claude Code 内建的 `Agent`/`Workflow` adversarial-verify 模式（find → dedup → 多 lens 核实 → loop-until-dry）。

**一句话**：想要的"更强壮、模型无关、更高效"，本质是把 codex 的单进程串行评审，**重写成一个 Claude 编排的并行对抗 workflow，复用它的 prompt 灵魂**。

---

## 7. 下一步候选（新 session 可挑一个起）

- **A. 出重建设计**：定义 `ReviewProvider` 抽象 + 并行 fan-out/对抗核实 workflow 形态 + 复用清单，落 design 文档（建议先 brainstorming 厘清需求与取舍）。
- **B. 可行性原型**：用 Claude `Workflow`/`Agent` 把"对抗 prompt + schema + verdict 循环"跑通最小版，对照 codex 实测质量与耗时差异。
- **C. 继续吃透现状**：native `review/start` vs 对抗 `turn/start` 的差异细节、broker 并发/鉴权处理、git 证据阈值逻辑。

---

## 附：关键源文件清单（重建时按需精读）

```
~/.claude/skills/until-codex-approve/SKILL.md                    外层编排循环（模型无关）
~/.claude/plugins/marketplaces/openai-codex/plugins/codex/
  prompts/adversarial-review.md                                  对抗 prompt 模板 ★直接搬
  schemas/review-output.schema.json                              输出 schema ★直接搬
  commands/adversarial-review.md                                 slash 命令（disable-model-invocation）
  commands/review.md / result.md                                 native review / 结果读取
  scripts/codex-companion.mjs                                    CLI 入口 + 执行编排
  scripts/lib/codex.mjs                                          turn/start 调用 + 输出解析（GPT 耦合）
  scripts/lib/app-server.mjs                                     JSON-RPC 客户端（codex 协议耦合）
  scripts/app-server-broker.mjs                                  长驻代理进程（鉴权/连接复用）
  scripts/lib/git.mjs                                            git 证据收集（模型无关）
  scripts/lib/{state,job-control,tracked-jobs,process,render}.mjs  job 生命周期/存储（模型无关）
  scripts/lib/prompts.mjs                                        模板插值（模型无关）
  skills/codex-cli-runtime/SKILL.md                              内部 helper 契约
  skills/codex-result-handling/SKILL.md                          结果呈现纪律（评审后禁自动改码）
```
