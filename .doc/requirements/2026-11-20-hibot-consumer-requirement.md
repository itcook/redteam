# redteam 作为 hibot audit 执行单元的需求

> 状态：WIP · 面向 `hibot`（自托管评审控制面）的 v1 接入需求。
> 契约权威定义在 hibot 仓库 `~/hibot/.docs/audit/contract.md`（`hibot.audit.executor/1`）；
> 本文是**从 redteam 视角**摘出的"要做什么、做到什么算合格"。
> 生成日 2026-11-20，已按 hibot 文档重组后的架构重写（v2）。

## 0. 背景与本文边界

hibot 的 audit 域对一次 PR 做三件事：**分诊 → 代码评审 → 复验**。v1 **只有一个执行单元：redteam**，
三种 kind 全部由它承担（hibot D55）。Pi 若要参与必须包适配器，v1 不做。

因此 redteam 在 v1 里不是"可选的插件"，而是**准入门槛**。当前能力对照：

| kind | 现状 | 结论 |
|---|---|---|
| `review` | ✅ 已有（`redteam review --base … --format json`） | 需要**加 `--job-dir` 模式** |
| `triage` | ❌ 没有 | **新增能力**，v1 阻塞项 |
| `verify` | ⚠️ 只有单条离线复核子命令（读一条 finding JSON） | **扩成批量**，v1 阻塞项 |
| 运行态心跳 | ❌ 没有 | 可选但推荐；没有它就不具备 LLM 并发刹车 |
| 配置投递 | 只有用户级 `~/.redteam/config.json` | 实现 job 模式后建议 `job_native`；否则用 `isolated_home` |

本文只定义**接口与能力**，不规定内部实现（prompt、模型、扫描策略仍是 redteam 自己的事）。

三条不可协商的前提：

1. **不产生结论**：只交 findings 与完整性自报；approve / request changes 由 hibot 的确定性谓词算。
2. **不持有 GitHub 凭据、不访问 GitHub**：所有 PR 信息由 hibot 以文件形式放进 job 目录。
3. **报告优先于退出码**：hibot 只认 `report.json` 里的 `status`；退出码是历史包袱，可保留但不参与判定。

## 1. 形态与调用

### 1.1 必须是 CLI

**不接受** SDK / 库 / 常驻服务 / 平台 API。runner 只需要 `fork + exec + 等退出`。

### 1.2 调用形式

```
redteam <triage|review|verify> --job-dir <dir> [--config <path>]
```

| 项 | 约定 |
|---|---|
| 第一个位置参数 | `<kind>`，与既有业务命令同级——**不新增 `job` 一级子命令** |
| `--job-dir <dir>` | **出现即表示外派模式**；不提供环境变量回退 |
| `--config <path>` | 可选；**它的值恒等于 `config/executor.json`**（见 §6.1） |
| 外派模式下 | **拒绝一切业务 flag**（`--base` / `--format` / `--scope` / `--focus` …），非零退出 |
| `<kind>` | 必须与 `job.json` 的 `kind` 一致，不一致视为调用错误 |
| 工作目录 | 不保证；不得依赖 cwd |
| stdin | 关闭 |
| stdout / stderr | 自由输出，仅供调试；**不得作为报告通道** |
| 报告 | 必须写 `<job-dir>/out/report.json` |

**为什么拒绝业务 flag**：它们与 manifest、`config/executor.json` 构成第二套真相。
要改行为，改配置或改 manifest，不要改命令行。

**为什么没有 `--pr`**：job 目录路径已含 `repos/<owner>/<repo>/pulls/<n>/`，manifest 里也有 `pr.number`。
日志里需要 PR 号时从 manifest 读——这样"手工重跑"和"runner 调用"的日志是同一种形状。

**`--config` 不是"第二个配置来源"**：它指向的就是 `config/executor.json`，同一份文件、同一个 hash。
它存在只是因为*很多工具只认固定的配置路径*，需要一个显式入口告诉它"配置在这里"。
用它来传"另一份配置"是**违约**。

## 2. 目录与输入

### 2.1 物理位置

```
~/hibot/repos/<owner>/<repo>/
├── mirror.git                      # bare mirror（repo 级）
└── pulls/<n>/
    ├── repo/                       # PR 级共享检出（跨阶段复用，只读）
    └── <job_id>/                   # ← --job-dir 指向这里
        ├── job.json
        ├── config/executor.json
        ├── input/*                  # 位置不固定，以 manifest 为准
        └── out/
            ├── report.json          # 终态报告（必需）
            └── state.json           # 运行态心跳（可选，见 §3.6）
```

### 2.2 manifest 的 `inputs` 是唯一权威

```json
"inputs": {
  "diff_patch":   { "path": "input/diff.patch", "sha256": "…" },
  "merge_base":   { "path": "input/merge-base.sha", "sha256": "…" },
  "prior_report": { "path": "input/prior-report.json", "sha256": "…", "schema": "hibot.audit.report.triage/1" }
}
```

- **不要硬编码 `input/xxx`**：路径可能随 hibot 版本变化。
- **校验 `sha256`**：不匹配就是 hibot 的 bug 或传输损坏，报 `error`，不要"差不多就用"。
- `schema` 字段说明该文件属于哪个阶段——例如 review 只接受 `report.triage/1` 的 prior_report。

### 2.3 各 kind 的必需输入

| key | triage | review | verify | 含义 |
|---|---|---|---|---|
| `pr_context` | **必需** | 可选 | 可选 | PR 标题/描述/评论/linked issues/CI 状态/变更文件 |
| `diff_patch` | **必需** | **必需** | **必需** | unified diff；verify 时就是 fix diff |
| `merge_base` | **必需** | **必需** | **必需** | diff 基准 sha |
| `prior_report` | — | **必需**（triage 报告） | 可选（review 报告） | 上一阶段完整报告 |
| `dismissed` | — | **必需** | — | 驳回台账 CSV |
| `findings` | — | — | **必需** | 待逐条核验的 findings |

必需输入缺失 ⇒ `status=error` + reason，**不得**静默降级成空结果。

## 3. 输出契约

### 3.1 信封

```json
{
  "schema": "hibot.audit.report.<kind>/1",
  "job_id": "…",
  "kind": "triage|review|verify",
  "status": "ok|partial|error",
  "status_reason": null,
  "input": { "head_sha": "…", "base_sha": "…" },
  "executor": { "name": "redteam", "version": "0.1.0" },
  "findings": [],
  "usage": { "input_tokens": 0, "output_tokens": 0, "cached_input_tokens": 0, "wall_clock_ms": 0 },
  "extensions": { "redteam": { } }
}
```

**现存 `ReviewReport` 的映射**（薄适配，不必改内部类型）：

| `ReviewReport` | 标准报告 |
|---|---|
| `findings[]` | 直接映射，补 `fingerprint` |
| `verdict` | **丢弃**（hibot 重算） |
| `stats` / `run.usage` / `run.duration_ms` | `usage`（**不要 cost**，见下） |
| `scan` / `clusters` / `cells` / 其它 | `extensions.redteam` |
| `run.job_id` | `job_id` |
| — | `input.head_sha` / `base_sha`：**必须补**（现在只有 `job_id`，无法确定审的是哪两个提交） |

### 3.2 `status`

| status | 含义 | hibot 行为 |
|---|---|---|
| `ok` | 该 job 范围内完成 | 用 findings 算 verdict |
| `partial` | 有产出但覆盖不完整（缺包、角色失败、区域未扫） | **不得 approve**，记 incomplete；`status_reason` 必填 |
| `error` | 未产出可用结果 | 记 error，按策略重试 |

**禁止**用空的 `findings` 冒充 `ok`。`status=ok` 但 findings 里有阻塞项**不是** error——那是有效结论。

### 3.3 Finding

```json
{
  "fingerprint": "sha256:<64 hex>",
  "severity": "low|medium|high|critical",
  "confidence": 0.0,
  "file": "…", "line_start": 1, "line_end": 1,
  "quote": "逐字原文",
  "title": "…", "body": "…", "recommendation": "…",
  "refuted": false,
  "source": { "executor": "redteam", "model": "…", "role": "reviewer" }
}
```

**指纹算法（必须逐字实现，hibot 会复算）**：

```
fingerprint = "sha256:" + sha256( file + "\0" + normalize(quote) + "\0" + normalize(title) ).hex
normalize(s) = 逐行去首尾空白 → 丢弃空行 → 用单个空格连接
```

- `quote` 必须是检出中的**逐字文本**；hibot 用 `line_start..line_end` 复核。
- `line_*` 相对**head 检出**，不是 diff hunk 行号。
- `refuted: true` 保留在数组里供审计，但不参与判定。

### 3.4 usage：只统计 token，不统计货币

`input_tokens` / `output_tokens` / `cached_input_tokens` / `wall_clock_ms`。
**`input_tokens` 含 `cached_input_tokens`**（cached 是子集，不是额外开销）。**不要 cost/金额字段**——
货币取决于会变的定价表，hibot 不消费它。

### 3.5 扩展字段

`scan` / `clusters` / `cells` / 模型明细等**一律放进 `extensions.redteam`**，不要平铺到顶层。
hibot 不解释它们，只透传与存档。

### 3.6 运行态心跳 `out/state.json`（可选，但强烈建议）

redteam 是**一次性进程**，外部无法查询它的内存状态。要让 runner 知道"现在 LLM 有多忙"，
唯一的办法是**自己周期落盘**。hibot 不要求 `redteam status` 命令——那不是契约，而是你方便时可以加的人读工具。

**执行过程中每 ≤5s 原子覆写一次**（写 `state.json.tmp` 再 rename）：

```json
{
  "schema": "hibot.audit.state/1",
  "job_id": "…",
  "phase": "scanning",
  "at": "2026-11-21T10:00:00Z",
  "llm": {
    "in_flight": 14, "max_concurrency": 30, "requests_total": 42, "retrying": 0,
    "providers": [
      { "name": "deepseek", "in_flight": 4, "max_concurrency": 20 },
      { "name": "glm", "in_flight": 10, "max_concurrency": 10, "rate_limited": 0 }
    ]
  },
  "progress": { "unit": "cells", "done": 7, "total": 21 }
}
```

| 字段 | 要点 |
|---|---|
| `in_flight` | 该 job 全部在途请求数 |
| `max_concurrency` | 本 job 从 `config/executor.json` 读到的并发上限（用于自检） |
| `retrying` / `rate_limited` | 排查 429 时非常有用，建议填 |
| `phase` / `progress` | 自由文本；用于评论与排障，不参与判定 |
| `at` | RFC3339；**超过 30s 未更新 = 过期**，hibot 视为"未知"而不是 0 |
| **`providers[]`** | **按 provider 分项**。跨机路由要靠它判断"谁饱和"——只给总数时，hibot 不知道该绕开哪一个 |

**为什么值得实现（两条）**：

1. **刹车**：`maxConcurrency` 是 per-job 的，同一台机器跑 2 个 job 就是 2 倍并发，直接撞 429。
   有了 `in_flight`，runner 才能在饱和前刹住接纳，而不是等 429 打回来再退避。
2. **路由**：`providers[]` 让 hibot 知道"deepseek 还有余量、glm 满了"，
   从而给下一个 job 渲染成可用的那个 provider。只报总数无法做这个判断。

**不实现也不阻塞接入**：hibot 会退回"只看 slots、只用配置里的首选模型"，只是失去上面两件事。

**实现提示**：`providers[].name` 用 `config/executor.json` 里的 provider key（不是 apiType）。
一个 job 通常只会用到 1–2 个 provider，列表一般很短。

## 4. 三种 kind 的能力要求

### 4.1 `triage`（新增，v1 阻塞项）

目的：判断这个 PR **值不值得审、该怎么审**。它是评审的前半段，不逐行裁定。参考 skill:  ~/.pi/agent/skills/pr-triage/SKILL.md，除功能目标外，还需要参考 `review` 执行模式进行并发优化(fan-out + 并发执行)

`summary` 是固定键，**计数只算一次直接给数**，hibot 不重算：

```json
"summary": {
  "security_tier": 0,
  "blockers_count": 0,
  "structural_count": 0,
  "local_count": 0,
  "intent_undelivered": 0,
  "scope_creep_count": 0,
  "risk": "low|medium|high",
  "convention_drift": "none|local|structural",
  "fit": "in-scope|possibly-redundant|needs-scope-decision",
  "routing": { "focus": [], "specs": [], "wall_clock_s": 3600 }
}
```

计数口径（来自 hibot 的 pr-triage 实践，可直接沿用）：

- `blockers_count = structural_count + intent_undelivered`（`local` 漂移不算阻塞）
- `intent_undelivered`：作者**声明过但 diff 未兑现**的项；CI 状态、既有失败是观测，不算
- `scope_creep_count`：diff 里有、但无任何声明对应的实质改动
- `security_tier`：2 = 存在 supply-chain / CI 投毒 / 代码执行类问题且 `confidence ≥ 8`；1 = 有安全问题但不至于阻塞；0 = 干净

**`routing` 是给代码评审用的**：`focus` 指定镜头（如 `billing` / `concurrency` / `retry-budget`），
`specs` 指定要遵循的规范文档，`wall_clock_s` 建议预算。**没有 routing 时，代码评审没有输入约束。**

### 4.2 `review`（已有，需适配）

- 现有 `review` 的能力保留；新增的是 **`--job-dir` 模式**：从 manifest 读 base/head/dismissed/prior_report。
- **必须消费 `prior_report`（triage 报告）**：
  - 用 `routing.focus` 收敛扫描范围；
  - 用 `summary` 决定风险档与镜头；
  - 把 triage 的 `blockers` 视为已确认（不需要重新发现）。
- `--base` 用 `merge_base` 的值（或直接用 `diff_patch`）。
- **`verify` 阶段要能吃 review 报告**（见 4.3）。

### 4.3 `verify`（单条 → 批量，v1 阻塞项）

目的：**验证修复**，不是重新评审。

- 输入：`findings.json`（hibot 从 D1 导出的阻塞项）+ `diff_patch`（= fix diff）
- 输出：`verifications[]`，**逐条覆盖** `findings.json` 的每个指纹，不得漏条：

```json
{ "finding_fingerprint": "sha256:…", "status": "resolved|unresolved|regressed", "evidence": "…", "quote": "…" }
```

- `regressed`：修复本身引入了新问题，必须给证据。
- **不得把整个 PR 再评一遍**——那等于多跑一轮 review，钱和口径都乱。

## 5. 失败与取消

| 场景 | 必须做 |
|---|---|
| 正常结束 | 写 `report.json`，`status=ok` |
| 覆盖不完整 | `status=partial` + reason（点名缺了什么） |
| 跑不起来 | 写 `status=error` + reason |
| 收到 `SIGTERM` | **先落盘 partial 报告再退出**（60s 内） |
| 被 `SIGKILL` | 无法要求；hibot 按"无报告 = error"处理 |
| 心跳停更 | 不影响正确性；hibot 视为"未知"，接纳退回纯 slots |

**不得自行设定硬超时**：预算由 hibot 按 diff 规模给（20/40/60 min）。接近预算时应收敛并落盘，
而不是继续跑然后被杀。

## 6. 无状态与配置

| 要求 | 说明 |
|---|---|
| 不跨 job 依赖状态 | `proj.db` 可写但 job 结束即弃 |
| 增量只靠 `--base <上次已审 sha>` | 不靠内部作业历史（hibot 明确不使用 `--incremental-from`） |
| 驳回记忆只靠 `--dismissed <csv>` | CSV 列固定 `fingerprint,actor,reason,created_at` |
| 配置只从 `config/executor.json`（或 `--config` 指向的同一文件）读 | **不要回退到用户级 `~/.redteam/config.json`** |
| 凭据只从 `apiKeyEnv` 指向的**环境变量**读 | 不得要求内联密钥 |
| 不写 `paths.repo` | 它是跨阶段共享的检出，污染会影响后续阶段 |

### 6.1 配置怎么送到 redteam 手里（重要）

redteam 现在只有**用户级**配置（早期的项目级支持已废弃），这带来一个具体问题：
hibot 渲染的配置放在 job 目录里，redteam 怎么找到它？

hibot 的契约允许三种投递方式，由 redteam 在能力声明里选：

| `config_delivery` | 谁做什么 | redteam 侧要做的 |
|---|---|---|
| `job_native` | redteam 自己读 `<job-dir>/config/executor.json` | **首选**：实现 job 模式时顺手读它 |
| `flag` | hibot 传 `--config <path>` | 接受 `--config` 并**只读它** |
| `isolated_home` | hibot 把文件放到 `$REDTEAM_HOME/config.json` | 现在的行为，零改动 |

**v1 现状**：redteam 还没实现 job 模式 ⇒ hibot 用 `isolated_home`：
给一个**全新的空 `REDTEAM_HOME`**，把渲染产物写成 `$REDTEAM_HOME/config.json`。

`isolated_home` 下的三条硬要求：

1. **不得读取 `~/.redteam/` 的任何东西**——hibot 不会去清理它，但读了就破坏可复现性；
2. **不得写回用户目录**（除 `$REDTEAM_HOME` 内的路径）；
3. `REDTEAM_HOME` 是**每个 job 一个、全新空目录**，job 结束即删——不要试图跨 job 复用 `proj.db`。

**建议的演进**：实现 job 模式时直接读 `config/executor.json`，把 `config_delivery` 标成 `job_native`，
这样 hibot 侧不需要再做 HOME 摆布，也少一个"文件名约定"要维护。

### 6.2 配置内容由 hibot 渲染，redteam 不要补默认值

hibot 会把 Registry（provider / 模型 / 凭据名）+ Profile（角色→模型 / 并发 / 工具）渲染成完整配置，
**并且允许 Profile 透过 `passthrough` 透传 redteam 特有的字段**（如 `review.findingWindow`、`tools`）。

对 redteam 的含义：

- 配置里**没有**的字段，用 redteam 自己的内置默认值——但**默认值要稳定**，
  否则"同一个 job 目录重跑两次结果不同"；
- 需要新配置项时，**不要**让 hibot 改代码：请 hibot 运维在 Profile 的 `passthrough` 里加一行；
- **不要**在 job 模式下偷偷读用户级配置来"补全"缺失字段（这会毁掉可复现性）。

## 7. 资源与并发

- 遵守配置里的 `maxConcurrency`，不自行放大。**这里的并发是"整个 job 的总量"，不是"每个 provider 各一份"**——
  若两者语义不同，请在 `state.json` 的 `providers[].max_concurrency` 里如实反映实际值。
- 可被 `SIGTERM` 在 60s 内中断，不留下后台子进程。
- 文档化单 job 的峰值内存/磁盘参考值（hibot 要做 slot 规划）。
- 建议在 `state.json` 里如实区分 `retrying`：**等待重试的不算在途**，
  否则 hibot 会把退避中的 job 误判为打满并发，从而过早刹住接纳。

## 8. 验收（conformance）

hibot 提供三个夹具，**三种 kind 都要通过**：

| 夹具 | 期望 |
|---|---|
| `clean` | `ok`，findings 为空 |
| `known-vuln` | `ok`，至少一条 finding 的 `file:line` 命中注入点，`quote` 原文可查 |
| `partial` | `partial` + reason，**不得** `ok` |

自动校验项：

1. 报告通过 JSON Schema；
2. `status` 与 `status_reason` 一致；
3. 每条 finding 的 `quote` 能在检出里逐字命中；
4. `fingerprint` 能按 §3.3 复算一致；
5. `verify` 的 `verifications` 覆盖 `findings.json` 全部指纹；
6. 同一夹具连跑两次，findings 指纹集合一致（措辞可不同）。

## 9. 待确认（给 hibot 侧）

1. `pr-context.json` 的 `comments[]` 是否包含 bot 评论（含 hibot 自己的 sticky）——triage 判意图时可能需要排除。
2. `recovery` 类语义：review 报告里的 `unresolved[]` 目前是可选字段，redteam 是否要产出、产出什么。
3. 单 job 峰值资源的参考口径（内存 / 磁盘 / 时长分位数）。
4. **配置里 `providers` 的 key 用哪个**：现已确认用 `config/executor.json` 里的 provider key
   （不是 apiType，也不是模型 id）——但若 hibot 渲染时改了 key 的命名规则，`state.json.providers[].name`
   会对不上。需要 hibot 保证 key 稳定。
5. **`passthrough` 的边界**：hibot 承诺 `passthrough` 不得覆盖 `providers` / `models` / `roles` / `limits`。
   若 redteam 希望某些字段由自己完全掌控（例如 `tools`），需要显式告知 hibot 侧把它排除在渲染之外。

## 10. 非目标

- 不做 GitHub API、不发 PR 评论、不管 runner 调度、不做多仓编排。
- 不生成 PR 评论 Markdown——hibot 自己渲染（报告是真相源，评论是投影）。
- 不实现 `fix`（写代码需要写权限，属 hibot 自管 skill）。
- **不要求提供 `status` 子命令**：心跳写文件即可；命令若加，也只是人读便利，不参与契约。
- 不要求理解 hibot 的 D1 schema 或状态机。
