# executor config v2（破坏性重排）设计

> 状态：**已批准 · T-B 实施中**（2026-11-25 协调者裁决 8 条；设计为准） · 基线 `a494e39`
> 任务：`.trellis/tasks/11-25-config-v2/`（PRD / 样例 / schema 草案 / hibot 交付清单）
> 范围：`src/config/mod.rs` 的**全部现存配置键**（`Config` / `ProviderConfig` / `ModelConfig` /
> `ReviewConfig` / `ScannersCfg` / `ReviewersCfg` / `RefutersCfg` / `GroupingCfg` / `PackCfg` /
> `MaxCells` / `Role` / `ProviderKind`），以及 `dist/config.schema.json`、`capabilities/redteam.json`、
> `model setup` 写盘、`config check` 诊断码、hibot 渲染器契约。
> 不在范围：Phase 4 verify 实现、Phase 5/7、`~/hibot` 任何改动、`dev` 分支。
>
> 证据等级：`[核实]` 已读源码 · `[文档]` 任务书/契约文字 · `[推定]` 推断 · `[待裁定]` 进 §9

---

## 0. 一页结论

1. **v2 = 两层**：`providers`（通道 + 模型固有属性）· `settings`（怎么用：语言、角色池、管线旋钮）。
   角色不再挂在模型下；模型清单从「按 id 做键的对象」改为**带 `id` 的数组**。
2. **全键覆盖**：v1 的 3 个顶层键、`providers` 5 个子键、`ModelConfig` 9 个子键、`review` 9 个子键
   （含 3 个角色节与 `grouping`/`pack` 的 9 个孙键）逐一在 §2 有落点，**无静默丢弃**。
3. **硬切**：不提供别名、不做自动迁移。v1 形状在 `config check`/`Config::load` 报 `legacy_shape`，
   每条都带「旧键 → 新键」提示。**schema 产物**升 `urn:redteam:config/2` + `schemaVersion: 2`；
   **配置文件本身不新增 `schemaVersion` 字段**（§3.9；Q3 已裁定不加）。
4. **`settings.*.models[]` = 轮转（已锁定）**。落地要钉死三件事：轮转粒度、游标生命周期、重试归属
   （§3.4）。历史行为：scanner 是「每条目 × 每组 × samples」的扇出；改轮转后，`samples = 1` 时同一组
   只由一个家族扫描，跨家族 `agreement` 会下降——这是**迁移影响**，不推翻轮转决定，补偿手段见 Q1。
5. 角色旋钮沿用协调者给的口径：`settings.review.{scanners,reviewers,refuters}.<knob>`（前缀搬运），
   角色池单独放 `settings.review.roles.<role>.models[]`。

---

## 1. v2 全量形状

严格 JSON、camelCase、无注释；未知键 job 模式 = error（本地 warning）。

```jsonc
{
  "settings": {
    "language": "en_US",                       // v1 顶层 language

    "review": {
      "blockThreshold": "medium",
      "confFloor": 0.5,
      "maxConcurrency": 10,                    // v1 review.maxConcurrency（全 kind 的 provider 闸缺省）
      "tools": "two-stage",

      "scanners":  { "samples": 1 },           // v1 review.scanners
      "reviewers": { "maxCells": "auto", "reenqueue": false },
      "refuters":  { "enabled": false },

      "grouping": { "minFiles": 4, "bundleLineThreshold": 200, "maxFilesPerGroup": 10, "semantic": false },
      "pack": { "body": "slice", "anchorWindow": 30, "findingWindow": 30, "findingWindowEnforce": false, "sliceMinBytes": 8192 },

      "roles": {
        "scanner":  { "models": [ {"provider": "deepseek", "id": "deepseek-chat", "reasoningEffort": "medium"} ] },
        "reviewer": { "models": [ {"provider": "glm", "id": "glm-5", "reasoningEffort": "high"} ] },
        "refuter":  { "models": [ {"provider": "deepseek", "id": "deepseek-chat", "reasoningEffort": "medium"} ] }
      }
    },

    "triage": { "models": [ {"provider": "deepseek", "id": "deepseek-chat", "reasoningEffort": "high"} ] },
    "verify": { "models": [ {"provider": "deepseek", "id": "deepseek-chat", "reasoningEffort": "high"} ] }
  },

  "providers": {
    "glm": {
      "api": "openai-completions",             // v1 kind；三值
      "baseUrl": "https://open.bigmodel.cn/api/paas/v4",   // 必填
      "apiKeyEnv": "HIBOT_LLM_GLM_KEY",
      "maxConcurrency": 10,
      "models": [
        {
          "id": "glm-5",
          "displayName": "GLM-5",
          "apiKeyEnv": "HIBOT_LLM_GLM_MODEL_KEY",   // 可选，模型级覆盖 provider
          "family": "glm",
          "contextWindow": 200000,
          "maxOutputTokens": 32768,
          "perTurnTimeoutSecs": 600,
          "reasoning": true,
          "thinkingLevelMap": { "minimal": null, "low": "low", "medium": "medium", "high": "high", "xhigh": "max", "max": "max", "ultra": "max" }
        }
      ]
    }
  }
}
```

**身份串不变**：`provider/id`（报告、台账、route_key 全部沿用）`[核实 src/config/mod.rs:638]`。
**默认值全部沿用 v1**（阈值 0.5 / medium、并发 10、samples 1、maxCells auto、pack/grouping 各项、
contextWindow 128K 且下限 32K、maxOutputTokens 32768、perTurn 公式）——只换路径，不改数值。

---

## 2. v1 → v2 逐键映射表（覆盖每一个现存键）

依据：`src/config/mod.rs:921-935` 的 `ROOT` / `PROVIDER` / `MODEL` / `ROLE_ATTRS` / `REVIEW` /
`REVIEW_SECTIONS`，以及 `Config` / `ReviewConfig` 结构体字段。

### 2.1 顶层

| v1 | v2 | 处置 |
|---|---|---|
| `language` | `settings.language` | 移动；值 `zh_CN` / `en_US` 不变；缺省语义不变（本地 locale / job `en_US`） |
| `providers` | `providers` | 保留（形状改造见 2.2） |
| `review` | `settings.review` | 整节前缀搬运（角色清单另落 `roles`） |
| — | `settings.triage.models[]` / `settings.verify.models[]` | 新增（v1 无对应键） |

### 2.2 `providers.<p>`

| v1 | v2 | 处置 |
|---|---|---|
| `kind` | `api` | 改名 + 值收缩（映射见 2.6） |
| `baseUrl` | `baseUrl` | **变必填**（Q5：`model setup` 推断规则） |
| `apiKeyEnv` | `apiKeyEnv` | 不变 |
| `maxConcurrency` | `maxConcurrency` | 不变（缺省继承 `settings.review.maxConcurrency`） |
| `models`（对象，键 = id） | `models`（数组，元素带 `id`） | 形状变更；数组顺序 = `model list` 展示序 |

### 2.3 `providers.<p>.models.<id>` → `providers.<p>.models[]`

| v1 | v2 | 处置 |
|---|---|---|
| `displayName` | `displayName` | 不变 |
| `kind` | — | **删除**；迁移方法见 §2.6（不是简单删键） |
| `apiKeyEnv` | `apiKeyEnv` | 保留（模型级 key env，解析链不变） |
| `family` | `family` | 保留（agreement 归族 / reasoning 参数形状 / 同族告警都靠它） |
| `contextWindow` | `contextWindow` | 不变（缺省 128000，下限 32000） |
| `maxOutputTokens` | `maxOutputTokens` | 不变（env `REDTEAM_MAX_TOKENS` 仍为次选） |
| `perTurnTimeoutSecs` | `perTurnTimeoutSecs` | 不变（env `REDTEAM_PER_TURN_SECS` 仍为次选） |
| `reasoning` | `reasoning` | 保留（当前仅解析、无消费点；见 §3.5） |
| `roles.<role>.reasoningEffort` | `settings.review.roles.<role>.models[].reasoningEffort`（`verifier` → `settings.verify.models[]`） | 移动；条目携带 `{provider,id,reasoningEffort}` |
| `roles`（整键，含 `workers` 已删） | — | 删除；未被任何池引用的模型 → WARN `model_unused` |

### 2.4 `review.*`（v1）

| v1 | v2 | 处置 |
|---|---|---|
| `blockThreshold` | `settings.review.blockThreshold` | 不变（`low\|medium\|high\|critical`） |
| `confFloor` | `settings.review.confFloor` | 不变（0.0–1.0） |
| `maxConcurrency` | `settings.review.maxConcurrency` | 键名不变；语义本就是全 kind 的 provider 闸缺省（triage/verify 共用同一张门表）`[核实 src/cli/mod.rs:1080,1116]` |
| `tools` | `settings.review.tools` | 不变（`off\|two-stage\|on`） |
| `scanners.samples` | `settings.review.scanners.samples` | 前缀搬运 |
| `reviewers.maxCells` | `settings.review.reviewers.maxCells` | 前缀搬运（`"auto"` 或 ≥1 整数） |
| `reviewers.reenqueue` | `settings.review.reviewers.reenqueue` | 前缀搬运；**当前未接线**（仅解析）`[核实]`，保留不扩语义 |
| `refuters.enabled` | `settings.review.refuters.enabled` | 前缀搬运；缺省 **false**（批 D-4 离线基线：误驳率超门槛） |
| `verifiers` | — | 0.8 已更名 `refuters`；v2 继续报错，并给「→ `settings.review.refuters.enabled`」双重提示 |
| `grouping.minFiles` | `settings.review.grouping.minFiles` | 不变 |
| `grouping.bundleLineThreshold` | `settings.review.grouping.bundleLineThreshold` | 不变 |
| `grouping.maxFilesPerGroup` | `settings.review.grouping.maxFilesPerGroup` | 不变 |
| `grouping.semantic` | `settings.review.grouping.semantic` | 不变 |
| `pack.body` | `settings.review.pack.body` | 不变（`full\|slice`） |
| `pack.anchorWindow` | `settings.review.pack.anchorWindow` | 不变（≥1） |
| `pack.findingWindow` | `settings.review.pack.findingWindow` | 不变（0 = 关观测） |
| `pack.findingWindowEnforce` | `settings.review.pack.findingWindowEnforce` | 不变 |
| `pack.sliceMinBytes` | `settings.review.pack.sliceMinBytes` | 不变 |
| `risk`（已删） | — | 继续 warning「D11：无模式旋钮」 |
| `*.workers`（已删） | — | 继续 warning「D13：并发只看 provider 闸」 |

### 2.5 新增键

| v2 | 语义 | 缺省 |
|---|---|---|
| `providers.<p>.models[].thinkingLevelMap` | 我方档位 → provider 实际值（§3.5） | 缺省 → 回退 family 表 |
| `providers.<p>.apiKey` / `providers.<p>.models[].apiKey` | **本地内联**密钥（Q2）；job 模式出现即 `secret_field` | 无 |
| `settings.review.roles.{scanner,reviewer,refuter}.models[]` | 角色池（§3.4） | 空池 = 角色缺席 |
| `settings.triage.models[]` | triage 模型池 | 空 → `missing_role:triage` |
| `settings.verify.models[]` | fix 验证模型池（**不再回退 reviewer**） | 空 → `missing_role:verifier` |

### 2.6 `kind` → `api`：值映射 + 模型级 `kind` 的迁移方法

**值映射**（v2 只接受右列三个字面值，**不接受任何别名**）：

| v1 `kind`（含别名） | v2 `api` | 依据 |
|---|---|---|
| `openai` / `gpt` | `openai-completions` | 与 `openai-compatible` 在 backend 分派、默认 base、认证头上完全相同 `[核实 src/provider/backends.rs:926, src/model/mod.rs:12-27]` |
| `openai-compatible` / `openai_compatible` / `compatible` | `openai-completions` | 同上（合流是行为等价的） |
| `anthropic` / `claude` | `anthropic-messages` | `[核实 src/provider/backends.rs:916]` |
| `google` / `gemini` | —（拒绝） | 现行为即 `Unavailable("google not yet supported")` `[核实 src/provider/backends.rs:936]`；v2 在配置期就拒绝 |
| — | `openai-responses` | 新预留值；**不实现**：`config check` 报 `api_not_implemented` ERROR（已锁定） |

> 别名迁移提示必须按别名逐个给出（`gpt` / `openai_compatible` / `compatible` / `claude` / `gemini`），
> 否则用户看到「unknown variant」不知道对应哪个 v2 值。

**删除模型级 `kind` 的迁移方法**（不能只写「删除」）：

1. 对每个旧模型算有效 API：`effective = model.kind.or(provider.kind)`（v1 的真实语义
   `[核实 src/config/mod.rs:741]`）。
2. 同一 provider 下所有模型 `effective` 相同 → 把该值写到 `providers.<p>.api`，模型层 `kind` 直接删。
3. 同一 provider 下存在**不同** `effective` → 必须在 v2 **拆成多个 provider**（每个 API 一个），
   原 provider 名可留给多数派或主 API；另一组用新名（如 `<p>-anthropic`）。
   同时：
   - 角色池条目 `{provider,id}` 的 `provider` 改成新名；
   - `provider/id` 身份串随之变化（台账/报告/`secrets.json` 的 provider 键都要跟）；
   - `baseUrl` / `maxConcurrency` / `apiKeyEnv` 需要各自复制或调整；
   - 迁移提示必须显式说明这四点，不能静默拆。
4. v1 别名（`gpt` 等）在 v2 解析期仍**不接受**，只用于迁移提示。

### 2.7 不变的部分

- **凭据链（Q2 已裁定：本地允许内联 `apiKey`）**，按此顺序取第一个非空：
  1. `providers.<p>.models[].apiKey`（本地内联）
  2. `providers.<p>.models[].apiKeyEnv`(env)
  3. `secrets.providers[p].models[id]`
  4. `providers[p].apiKey`（本地内联）
  5. `providers[p].apiKeyEnv`(env)
  6. `secrets.providers[p].apiKey`
  `secrets.json` 形状不变；job 模式 secrets 为空，且 `apiKey` 键一律 `secret_field`。
- **job 配置**：任何路径出现 `apiKey`/`token` 等仍判 `secret_field`（递归检查，沿用
  `[核实 src/config/mod.rs:1080-1102]`）；诊断只输出 pointer/码，不输出密钥值。
- **环境变量**：`REDTEAM_HOME` / `REDTEAM_MAX_TOKENS` / `REDTEAM_PER_TURN_SECS` 全部不变。
- **`secrets.json` 形状**：`providers.<p>.apiKey` + `providers.<p>.models.<id>` 不变（模型 id 仍是字符串键，
  不随 models 数组化而变）。
- **报告/台账身份**：`provider/id`、`route_key`、`model` 字段全部不变（拆 provider 的场景除外，见 §2.6）。

---

## 3. 语义说明

### 3.1 `provider.api`（三值）

- `openai-completions`：OpenAI Chat Completions 形状（现 `Openai` + `OpenaiCompatible` 合流）。
- `anthropic-messages`：Anthropic Messages 形状（现 `Anthropic`）。
- `openai-responses`：**仅预留**。`config check` 报 `api_not_implemented` ERROR；`Config::validate`
  同样拒绝（本地也不允许带着未实现 api 启动）。
- `google` 删除：json 值不在枚举内 → `legacy_shape` 专属文案「google 已移除；api 三值 …；hibot 不渲染
  google 模型」。
- 内部类型建议改名 `ProviderKind` → `ProviderApi`（变体 `OpenaiCompletions` / `AnthropicMessages` /
  `OpenaiResponses`），避免与 hibot Registry 的 `kind` 词表混淆。

### 3.2 `baseUrl` 必填

- 每个 provider 必须显式给 `baseUrl`；缺失 = ERROR `invalid_value`。
- 运行时不再按 api 猜默认 base（`effective_base` 的兜底链废止），但 `model setup` 可为**新建 provider**
  写默认值（Q5）。
- 理由：三个 api 里只有两个有「官方 base」可言，兼容网关才是常态；隐式默认会让「配错供应商」在运行期
  才暴露。

### 3.3 `models` 数组

- 元素必须有 `id`（字符串、非空）；同一 provider 内重复 `id` = ERROR `duplicate_model`。
- 数组顺序 = `model list` 展示序；不参与选模语义。
- 模型不得声明 `kind`/`api`；provider 是通道唯一真相。

### 3.4 `settings.*.models[]` 的角色池语义（轮转，已锁定）

条目：`{provider, id, reasoningEffort?}`；顺序 = 轮转顺序；长度 1 合法（hibot 只渲染 1）。

**轮转定义（需在 T-B 钉死，不能含糊）**：

1. **粒度**：
   - reviewer：每个待深挖格；
   - refuter：每条进入复核的 finding；
   - verify：每条待验证 finding（Phase 4）；
   - triage：**按轴**（security → drift → intent 依次取池）；I0/claims 固定用池首条；
   - scanner：按稳定的 `(group, sample)` 次序取池（同一 group 的第 k 个 sample 用
     `index = (group × samples + (k−1)) % len`；不依赖并发完成顺序）。
2. **游标生命周期**：进程内、每个池一份；作业开始归零（可复现）。
3. **重试归属**：同一格的**重试必须沿用该格已分配的模型**（不重新取模），否则重试会换家族、
   破坏 agreement 与审计一致性。
4. **忙门处理**：这是**「轮转 + 跳过忙门」**，不是严格阻塞轮转——reviewer 池从游标起找第一个能拿到
   provider 门位的成员；选中但忙 → 跳到下一位（现状 `Pool::next_lease`
   `[核实 src/orchestrate/deep.rs:878-895]`，**不能**说它与严格轮转等价）。T-B 需在文档/测试里
   把这个语义写明。
5. **不做失败回退**：模型调用失败按现有单格失败语义处理，不自动换池内下一个。

**迁移影响（scanner）**：v1 是扇出（`组 × 条目 × samples`，每个条目都扫每组）
`[核实 src/orchestrate/mod.rs:246-252]`；`agreement` = 同一疑点簇的去重家族数
`[核实 src/consolidate/scan.rs:143,156]`，复核门槛与「共识跳过」都读它
`[核实 src/consolidate/refute.rs:231]`。改轮转后，`samples = 1` 时同一组只由一个家族扫描，
agreement 结构性下降。**这是迁移影响，不推翻轮转决定**；补偿手段见 Q1（例如文档建议多家族场景把
`samples` 抬到 ≥ 池大小，或 hibot 渲染时把 `samples` 与池大小联动）。

### 3.5 `reasoningEffort` + `thinkingLevelMap` + `family`

- 每个角色条目可带 `reasoningEffort`（7 档：`minimal|low|medium|high|xhigh|max|ultra`），同一模型可在
  scanner 位 `low`、reviewer 位 `high`。
- **无 `thinkingLevelMap`**：完全沿用今天的 family 表（`(api, family, id)` → 参数形状；值原样透传）
  `[核实 src/provider/backends.rs:967-1017]`，本地配置零行为变化。
- **有 `thinkingLevelMap`**（模型级，键 ∈ 7 档，值 = 字符串或 `null`）：
  1. `map[level]` 为字符串，且 family 形状**有强度槽**（deepseek/glm/openai/gemini/claude）→
     用它替换强度槽的值；结构参数（`thinking:{type:…}` / `enable_thinking`）仍由 family 表决定。
  2. `map[level]` 为字符串，但 family 形状**没有强度槽**（kimi / qwen 只有开关）→
     按 family 形状**原样发送**（只有开关），并给 warning `reasoning_effort_unmapped`；
     **不发明** `reasoning_effort`（Kimi 同发会 400）。
  3. `map[level]` 为 `null` → **整组推理参数都不发**（含结构开关）+ warning `reasoning_effort_suppressed`
     ——这是给「该档位下显式关推理」的语义，不只是关强度。
  4. map 存在但缺该档键 → 等同 `null`（map 存在即权威，避免「半张表」隐式回落）。
  5. family 表对该 `(api,family,id)` 完全无形状 → 仍不发 + warning（map 只能翻译值，不能创造形状）。
- `reasoning: bool` 保留（语义：思考 token 计入输出额度）。`[核实]` 当前**只有解析、无消费点**
  （全仓 grep 仅结构体与夹具），v2 不扩义、不删键；它不参与参数形状选择。
- `family` 缺省仍取 id 首段小写；它同时服务 agreement 归族与「refuter 同族」告警，**不可删**。

### 3.6 角色开关与「不做 fallback」

- `refuter`：`settings.review.refuters.enabled` 仍是显式开关（缺省 **false**）。池空 + `enabled=true` 时分叉：
  - **本地**：保留今天的自动异族回退（从 reviewer → scanner 池挑家族不同的模型；无外族才同族 + warning）。
  - **job**：**跳过复核**，报告 `extensions.redteam.refute_skipped = "no_refuter"`（现有行为，不许回退）。
  - 池非空：一律按池轮转（§3.4），不再走自动回退。
- `verify`：`settings.verify.models` 为空 → `missing_role:verifier`；**不再回退 reviewer**
  （删除 `RoleSets::fix_verifiers` 的回退分支 `[核实 src/config/mod.rs:621-625]`）。
- `triage`：`settings.triage.models` 为空 → `missing_role:triage`（Q7 命名）。

### 3.7 review 管线旋钮

`blockThreshold` / `confFloor` / `tools` / `scanners.samples` / `reviewers.*` / `refuters.enabled` /
`grouping.*` / `pack.*` 语义与默认值全部不变，只换路径。
`settings.review.maxConcurrency` 仍是 **provider 在途闸的缺省**（唯一闸，无角色级 workers）；
`providers.<p>.maxConcurrency` 覆盖它。triage/verify 共用同一张门表
`[核实 src/cli/mod.rs:1080,1116]`。

### 3.8 `config check --kind` 角色最小集（v2）

| kind | 要求 | 错误码 |
|---|---|---|
| `review` | scanner 池非空 + reviewer 池非空 | `missing_role:scanner` / `missing_role:reviewer` |
| `triage` | `settings.triage.models` 非空 | `missing_role:triage`（Q7） |
| `verify` | `settings.verify.models` 非空 | `missing_role:verifier` |
| 任意 | 池内 `provider`/`id` 必须存在 | `unknown_model` |
| 任意 | 同池重复 `(provider,id)` | `duplicate_model` |
| 任意 | provider 声明了未被任何池引用的模型 | WARN `model_unused`（替代 v1 `no_roles`） |

**边界**：通用 `Config::validate` **不得**要求三个池全部非空（那会让本地一份只跑 review 的配置
无法加载）；最小集只在 `config check --kind` 按 kind 检查。

### 3.9 schema 身份（产物级，不含实例字段）

- schema 产物：`$id: "urn:redteam:config/2"`、`schemaVersion: 2`、`title: "redteam executor config"`；
  `dist/config.schema.json` 与 `capabilities/redteam.json` 的
  `extensions.config_schema.schemaVersion` **必须同时为 2**，加测试断言两者相等（再加一条断言等于常量 2）。
- **配置文件本身不新增 `schemaVersion` 字段**：本轮锁定的是 schema 文件与 capabilities 的版本同步，
  没有要求 executor JSON 自描述版本。若确有需要（便于 `rendered_sha256` 归因），单列建议 Q3，
  不能当成已确定契约。v1 配置文件不含该键，v2 也不含。
- `scripts/release.sh:163` 的 release notes 文案同步改为 `/2`、`schemaVersion: 2`。
- 版本硬切靠形状判定（`providers.*.models` 是对象、顶层 `review`/`language`、模型 `roles`/`kind`），
  不靠实例字段。

---

## 4. 诊断码（`config check` / job 错误）

| 码 | 级别 | 触发 | 备注 |
|---|---|---|---|
| `legacy_shape` | ERROR | v1 形状/旧键/旧别名 | 复用现有码；message 逐字段给迁移提示（§5）。**对外唯一口径**，不另设 `legacy_schema_version` |
| `unknown_key` | ERROR（job）/ WARN（本地） | 未知键 | 沿用 |
| `secret_field` | ERROR | 任何 `apiKey`/`token`/… | 沿用；不输出密钥值 |
| `invalid_value` | ERROR | 类型/枚举/范围 | 沿用 |
| `api_not_implemented` | ERROR | `api: "openai-responses"` | 新增 |
| `unknown_model` | ERROR | 池条目引用不存在的 provider/model | 新增（pointer = 池条目） |
| `duplicate_model` | ERROR | 同池重复；同 provider 重复 model id | 新增 |
| `missing_role:<role>` | ERROR | 见 §3.8 | 沿用 |
| `model_unused` | WARN | provider 下模型未被任何池引用 | 替代 `no_roles` |
| `refuter_no_models` | WARN | `enabled=true` 且池空 | 新增（Q6） |
| `reasoning_effort_unmapped` | WARN | 选了档但形状无强度槽 / 形状未知 | 沿用现有 warning 通道 |
| `reasoning_effort_suppressed` | WARN | `thinkingLevelMap[level] = null` 或缺档键 | 新增 |

迁移期兼容：`legacy_shape` 的 message 保持「一行一条」，保证 hibot 的 `ERROR|WARN <pointer> <code>: <message>`
单行契约不被破坏。

---

## 5. v1→v2 迁移提示（逐字段文案）

`shape_guard` 在解析前按 `Value` 走一遍，以下命中都给**专属错误**（不是 warning），文案统一格式
`<json pointer>: <为什么> → <新写法>`：

| 旧键/形状 | 提示（要点） |
|---|---|
| 顶层 `language` | `language` 已移入 `settings.language`（hibot 契约同步） |
| 顶层 `review` | `review` 已整节移入 `settings.review`（角色池在 `settings.review.roles.*.models[]`） |
| `review.verifiers` | 0.8 已更名 `refuters`；v2 为 `settings.review.refuters.enabled` |
| `review.scanners.samples` | → `settings.review.scanners.samples` |
| `review.reviewers.maxCells` / `.reenqueue` | → `settings.review.reviewers.*` |
| `review.refuters.enabled` | → `settings.review.refuters.enabled` |
| `providers.<p>.kind` | → `providers.<p>.api`；值映射表（含别名 `gpt`/`openai_compatible`/`compatible`/`claude`/`gemini`） |
| `providers.<p>.models` 是对象 | → 数组，元素增加 `"id": "<旧键>"` |
| 模型级 `kind` | 已删除：按 `model.kind.or(provider.kind)` 算有效 API；同一 provider 混多种 API → 必须拆 provider（§2.6） |
| 模型 `roles` | → `settings.review.roles.<role>.models[]` 条目 `{provider,id,reasoningEffort}`；`verifier` → `settings.verify.models[]` |
| 模型 `name` | 已改名 `displayName`（沿用现有提示） |
| snake_case 任意键 | camelCase（现有提示） |

**不做**自动迁移命令（Q8）：迁移是一次性动作，且 hibot 会整份重渲染；本地用户按提示手改。

---

## 6. 内部改动面（T-B 预览，仅供评估工作量）

- `config`：新增 `Settings`/`RolePool`/`RoleModelRef`；`ProviderApi`；`ModelConfig` 去 `kind`/`roles`、
  加 `thinkingLevelMap`；`ProviderConfig.models: Vec<ModelConfig>`（`id` 入结构）；`RoleSets` 由 settings 池
  构建（不再遍历模型 roles）。
- **`Role::Triager` 不预设**：triage 池确实需要独立的 backend role_key（避免与 reviewer 同键覆盖），
  但是否扩公共 `Role` 枚举，T-B 先列影响面再定：`Role::ALL`、`REVIEW_PIPELINE`、`role_key`、
  报告 `models`/`roles` 字段、状态/心跳的角色名消费点。可选方案：内部专用 key 字符串，
  不改公共枚举。
- `config::validate`：未知模型、重复、`api_not_implemented`、`baseUrl` 必填；**不校验三池齐全**。
- `shape_guard` / `strict_issues` / `locate_shape_error`：已知键表 + 迁移文案 + 路径。
- `provider::backends`：`ProviderApi` 分派；`reasoning_effort_params` 增「map 翻译强度槽」与
  「null 抑制整组」两个入口。
- `cli::build_registry_*`：按 settings 池注册（scanner/reviewer/refuter/triage/verifier 各自 role_key）。
- `consolidate::refute::pick_refuter`：入参改「refuter 池」（或保留 RoleSets，但池来源变更）；
  若采纳 Q6 默认则删自动异族回退分支。
- `triage::triage_route` / `TriageRequest`：改为「轴 → 池下标」映射（Q4）。
- `job::run` verify：**只读配置契约**，不实现 Phase 4；`capabilities.kinds` 不加 `verify`。
- `config_schema_json`：`/2` + `schemaVersion: 2`；`capabilities/redteam.json` 同步。
- `model setup`：写 `api` + models 数组 + 角色池条目；`--base-url` 推断（Q5）。
- 文档/生成物：`dist/config.schema.json`、`scripts/release.sh:163`、README 配置段、`.doc/plans/…` 里的
  v1 示例（T-B 一并改）。

---

## 7. hibot 交付清单

详见 `.trellis/tasks/11-25-config-v2/hibot-delivery.md`（正式交付：对照表 + v2 样例 + schema 正式产物 + sha256）。
`dist/config.schema.json` 由 `make schema` 生成，与 `capabilities` 的 `schemaVersion` 由测试钉死相等。
要点：

1. `dist/config.schema.json`（`$id: urn:redteam:config/2`、`schemaVersion: 2`）+ 其 sha256。
2. `capabilities/redteam.json` 的 `extensions.config_schema.schemaVersion: 2`（hibot 的渲染 CI 比对点）。
3. Registry/Profile → v2 路径对照表（替换 hibot `.docs/runtime/config.md` §2.1 的 v1 表）。
4. v2 样例（`config-v2.sample.json`，`models[]` 长度 1 的角色池）。
5. 迁移须知：模型对象→数组、角色→settings、`kind`→`api`（含别名与拆 provider 规则）、`baseUrl` 必填、
   `language`→`settings.language`、`review.refuters.enabled`→`settings.review.refuters.enabled`、
   新增 `settings.triage/verify.models`。
6. `config check` 新码表（§4）。

---

## 8. 兼容性与风险

| 风险 | 影响 | 处置 |
|---|---|---|
| 本地旧配置全面失效 | `Config::load` 直接报错，`review` 全部拒跑 | 文案逐字段给迁移提示；release notes 置顶；不静默兼容 |
| hibot 尚未改渲染器 | job 全部 `legacy_shape` | 交付 schema/样例后 hibot 先改；`capabilities` 的 `schemaVersion` 是拒发闸 |
| scanner 轮转削弱 agreement（samples=1） | 跨家族共识/复核门槛下降 | Q1：文档建议多家族场景抬高 samples 或渲染侧联动；**不改 agreement 阈值** |
| 模型级 `kind` 存在混用 provider | 简单删键会改变通道语义 | §2.6 迁移方法：算有效 API、拆 provider、改角色引用与身份串 |
| `baseUrl` 必填抬高首次配置成本 | `model setup` 新用户多一步 | Q5：已知供应商推断默认 base |
| `openai-responses` 预留值被误用 | 运行期才失败 | `config check` + `validate` 双处 ERROR |
| `models` 数组化破坏 `secrets.json` 的模型键 | 无（secrets 仍按 id 字符串） | 文档写明 |
| 生成的 schema 与手写草案漂移 | 评审依据失真 | 草案明确标注 draft；T-B `make schema` 后重发 sha256，hibot 以正式产物为准 |

---

## 9. 裁决结果（2026-11-25 协调者批准）

| # | 问题 | 裁决 |
|---|---|---|
| Q1 | scanner 轮转的 agreement 补偿 | 按 §3.4 的 `(group, sample)` 稳定轮转；文档写「多家族需 `samples ≥ 池大小`」；**不加** `scanner.fanout` |
| Q2 | 本地内联 `apiKey` | **允许**（解析链见 §2.7）；job 模式出现 `apiKey`（任何值，含空串）→ `secret_field` |
| Q3 | 实例级 `schemaVersion` | **不加**；只升 schema 产物 `$id`/`schemaVersion` 与 capabilities |
| Q4 | triage 轮转粒度 | 按轴（security → drift → intent）；I0/claims 用池首条 |
| Q5 | `model setup` 的 baseUrl 推断 | 显式 `--base-url` > 已知供应商默认 > 其余报错 |
| Q6 | refuter 空池 + `enabled=true` | **本地**保留自动异族回退；**job** 跳过并记 `refute_skipped="no_refuter"`（见 §3.6） |
| Q7 | triage 缺角码名 | **`missing_role:triage`**（不造 `triager`） |
| Q8 | `redteam config migrate` | 不提供；硬切 + 逐字段提示 |

> Q2 的本地内联 `apiKey` 是**新增契约**：它只在本机 `$REDTEAM_HOME/config.json` 生效；
> hibot 渲染的 job 配置**绝不写**该键，job 模式出现即 `secret_field`（含空串）。

---

## 10. T-B 实施分层（批准后执行）

1. **schema/常量层**：`ProviderApi`、`Settings`/`RolePool`/`RoleModelRef`、`thinkingLevelMap`；
   schema `$id`/`schemaVersion`；capabilities 同步 + 两处相等测试。
2. **config 解析/校验**：models 数组化、baseUrl 必填、未知模型/重复/未实现 api；
   `shape_guard`/`strict_issues`/`locate_shape_error` 全量迁移文案；validate 不查三池齐全。
3. **provider 层**：api 分派 + `thinkingLevelMap`（强度槽翻译 / null 抑制 / 无强度槽 warning）。
4. **角色/注册表**：RoleSets 从 settings 池构建；triage 池 role_key 方案（先列 `Role` 消费者影响）；
   reviewer/refuter/scanner 轮转粒度与重试归属；refute 的本地自动异族回退 / job 跳过分叉。
5. **kind 接线**：triage 按轴取池；verify 只读配置契约（不实现 Phase 4、不加 capabilities）；
   `config check --kind` 最小集。
6. **CLI/写盘**：`model setup` 写 v2（api/models 数组/池条目/baseUrl 推断）；`config schema` 产物。
7. **文档/生成物**：`dist/config.schema.json`、release notes、README、hibot 交付（正式 sha256）。
8. **测试**：v1 形状迁移文案逐键断言（含别名与拆 provider 场景）；schema/capabilities 版本一致；
   `thinkingLevelMap` 四态（有槽字符串 / 无槽字符串 / null / 缺键）；数组化与池解析；job 模式 strict；
   hibot 样例过 `config check`。

验收：`make verify` 绿；`cargo test` 全绿；`config check` 对 §2 每个 v1 键都给出专属提示；
hibot 的 v2 样例（`config-v2.sample.json`）`config check --kind review|triage|verify` 全绿。

---

## 附录 A：v1 样例 → v2 逐字段 diff（hibot `audit-pr-468`）

v1 源（只读）：`~/hibot/.docs/examples/audit-pr-468/config/executor.redteam.json`
副本：`.trellis/tasks/11-25-config-v2/config-v1.before.json`
v2：`.trellis/tasks/11-25-config-v2/config-v2.sample.json`

**v1 的角色事实**（按 provider 名 / model id 字典序）：

| 角色 | v1 池 |
|---|---|
| scanner | `deepseek/deepseek-chat`（medium）、`glm/glm-5`（medium） |
| reviewer | `deepseek/deepseek-chat`（high）、`glm/glm-5`（high） |
| refuter | `deepseek/deepseek-chat`（medium） |
| verifier | 无声明；`fix_verifiers()` 缺省回退 reviewer → 排序后首条 = `deepseek/deepseek-chat` |
| triage | v1 无配置；`triage_route()` 取 reviewer 排序后首条 = `deepseek/deepseek-chat` |

**v2 样例的选择**（任务书要求 `models[]` 长度 1，因此多模型池必须收敛为一条）：

| 池 | v2 样例 | 说明 |
|---|---|---|
| scanner | `deepseek/deepseek-chat` medium | 保留 v1 首条；**glm-5 从 scanner 池移除** |
| reviewer | `glm/glm-5` high | 在 v1 两条里选 glm（与 scanner 异族，便于复核独立性）；**deepseek 从 reviewer 池移除** |
| refuter | `deepseek/deepseek-chat` medium | 与 v1 一致（唯一一条） |
| triage | `deepseek/deepseek-chat` high | **显式新增**；取 v1 运行时行为（reviewer 首条），档位用 high（原 triage 决策：高推理档） |
| verify | `deepseek/deepseek-chat` high | **显式新增**；取 v1 回退结果（reviewer 首条），不再依赖回退 |

**这不是无损迁移**：两个多模型池（scanner/reviewer）按「长度 1」要求各收敛为一条，被移除的模型仍在
`providers` 里（hibot 可随时按 Profile/饱和度重新渲染）；本地多模型配置应把池写全（长度 > 1 合法）。

| v1 | v2 | 说明 |
|---|---|---|
| `language: "en_US"` | `settings.language: "en_US"` | 移动 |
| `review.blockThreshold/confFloor/maxConcurrency/tools` | `settings.review.*` | 前缀搬运（并发键名不变） |
| `review.refuters.enabled: true` | `settings.review.refuters.enabled: true` | 前缀搬运 |
| `providers.deepseek.kind: "openai-compatible"` | `providers.deepseek.api: "openai-completions"` | 值映射 |
| `providers.glm.kind: "openai-compatible"` | `providers.glm.api: "openai-completions"` | 值映射 |
| `providers.*.models: { "<id>": {...} }` | `models: [ { "id": "<id>", ... } ]` | 数组化；`family` 显式补上（缺省同值） |
| 模型 `roles.*` | `settings.review.roles.<role>.models[]` + `settings.triage/verify.models[]` | 角色上移（选择见上表） |
| — | （无 `schemaVersion`） | 配置文件不新增版本字段（§3.9） |
| 未写 `samples`/`maxCells`/`reenqueue` | 同样不写 | 走默认值，避免样例噪声 |
