# hibot 交付清单 · redteam executor config v2

> 状态：**正式交付（T-B 完成，2026-11-25）** · 基线 `a494e39`
> 设计正文：`.doc/design/2026-11-25-redteam-config-v2-design.md`
> 冻结依据：`redteam config schema` 生成物 + `capabilities/redteam.json` 的 `schemaVersion`（两者测试钉死相等）。

## 1. 交付物

| # | 交付物 | 位置 | sha256 |
|---|---|---|---|
| 1 | v2 schema（正式生成物，draft-07） | `dist/config.schema.json`（副本：`.trellis/tasks/11-25-config-v2/config-v2.schema.json`） | `9884be3d8805fc58ec8712ac9bb85c8fc2829b259dd4e940829815bec2e2150b` |
| 2 | v2 样例（`models[]` 长度 1） | `.trellis/tasks/11-25-config-v2/config-v2.sample.json` | `159a0f19b465dd4934f8ac86eb50c8c676b3159026ca56b2e3ef046d595172b2` |
| 3 | v1 基线（hibot `audit-pr-468` 副本，只读源） | `.trellis/tasks/11-25-config-v2/config-v1.before.json` | `6147620f423d15e729358970c9db0ec55ca9539d7715ed76d92bd8120f82ffd2` |
| 4 | v1→v2 全键映射表 | 设计文档 §2 / §5 | — |
| 5 | Registry/Profile → v2 路径对照表 | 本文 §3 | — |
| 6 | `config check` 诊断码表 | 设计文档 §4 / 本文 §5 | — |

其它事实：

- `dist/config.schema.json` 的 `$id = urn:redteam:config/2`、`schemaVersion = 2`、`additionalProperties: false`、无 `apiKey` 属性。
- `capabilities/redteam.json` 的 `extensions.config_schema.schemaVersion = 2`；
  `tests/capabilities.rs` 断言它与 schema 产物相等且等于 2。
- **配置文件本身不含 `schemaVersion` 字段**（schema 元数据 ≠ 实例字段；设计 §3.9 / Q3）。
- 样例已通过 `config check --kind review|triage|verify`（0 error）。
- schema 收紧为 `additionalProperties: false`（与 job 模式 unknown_key = ERROR 一致），并**隐藏 `apiKey` 槽**（executor 契约不含明文密钥；本地反序列化仍支持，但 job/`config check` 出现即 `secret_field`）。

## 2. 给 hibot 的破坏性变更摘要

1. 顶层 `language` → `settings.language`（同值）。
2. `providers.<p>.kind` → `providers.<p>.api`，三值：`openai-completions` / `anthropic-messages` /
   `openai-responses`（预留，`config check` 报 `api_not_implemented` ERROR）；`google` 移除（hibot 拒绝渲染）。
3. `providers.<p>.baseUrl` **必填**。
4. `providers.<p>.models` 从「按 id 做键的对象」→「带 `id` 的数组」。
5. 模型级 `kind` 删除；模型级 `apiKeyEnv`、`family`、`contextWindow`、`maxOutputTokens`、
   `perTurnTimeoutSecs`、`reasoning`、`thinkingLevelMap?` 保留。
6. 角色从模型下移到 `settings`：`settings.review.roles.{scanner,reviewer,refuter}.models[]`、
   `settings.triage.models[]`、`settings.verify.models[]`；条目 `{provider,id,reasoningEffort?}`。
   同一模型可在多个池里出现。**池 = 轮转顺序**（长度 1 合法）。
7. `review.*` 整节移入 `settings.review`（`scanners` / `reviewers` / `refuters` 三个旋钮节的名字不变）；
   `review.refuters.enabled` → `settings.review.refuters.enabled`（默认仍 **false**）。
8. `verify` **不再回退 reviewer**：`settings.verify.models` 为空 → `missing_role:verifier`。
9. `triage` 不再复用 reviewer：`settings.triage.models` 必填（空 → `missing_role:triage`）。
10. 渲染产物不新增版本字段；版本识别走 capabilities 的 `schemaVersion` 与 schema `$id`。

## 3. Registry/Profile → v2 路径对照表（替换 hibot `.docs/runtime/config.md` §2.1）

| hibot 输入 | v2 产物路径 |
|---|---|
| Registry provider key | `providers.<key>` |
| Registry provider kind/apiType | `providers.<key>.api`：`openai-compatible`/`openai` → `openai-completions`；`anthropic`/`claude` → `anthropic-messages`；`google` → **拒绝渲染** |
| Registry baseUrl | `providers.<key>.baseUrl`（必填；Registry 缺 baseUrl 时不得兜底猜测） |
| Registry 凭据名 | `providers.<key>.apiKeyEnv`（只写 env 名，不落盘） |
| Registry model id | `providers.<key>.models[].id` |
| Registry model 属性 | `contextWindow` / `maxOutputTokens` / `reasoning` / `family?` 同名字段 |
| Profile role→model + effort | `settings.review.roles.<scanner|reviewer|refuter>.models[0] = {provider,id,reasoningEffort}`（**长度 1**） |
| Profile triage 角色 | `settings.triage.models[0] = {provider,id,reasoningEffort}`（新增，必渲染） |
| Profile verify 角色 | `settings.verify.models[0] = {provider,id,reasoningEffort}`（新增；无回退） |
| Profile limits.maxConcurrency | `settings.review.maxConcurrency` + `providers.<key>.maxConcurrency` |
| Profile tools | `settings.review.tools`（**注意**：hibot 现文档 §3 的 passthrough 示例写成顶层 `tools`，v1/v2 实际都在 review 节） |
| Profile passthrough | 深合并进 `settings.*` / `providers.*`；冲突拒绝规则不变 |
| 仓策略 language | `settings.language`（`zh_CN` / `en_US`，缺省 `en_US`） |

## 4. 渲染器改造清单（hibot 侧）

- [ ] 模型清单：对象 → 数组，元素写 `id`。
- [ ] 角色：从「每个模型下的 `roles`」改为「`settings.*.models[]` 池」，每角色只渲染 1 条（长度 1 合法）。
- [ ] 新增 triage/verify 池渲染（不允许留空；`config check --kind` 会拒）。
- [ ] `kind` → `api` + 值映射；`google` 直接拒绝发 job。
- [ ] `baseUrl` 必填校验（Registry 数据先自检，缺失在渲染期就报错）。
- [ ] `language` 写 `settings.language`。
- [ ] `review.*` 前缀改 `settings.review.*`（`scanners`/`reviewers`/`refuters` 名字不变）。
- [ ] 渲染 CI：比对 `capabilities.extensions.config_schema.schemaVersion` 与 `config schema` 的
      `schemaVersion`（都必须是 2）；渲染后对产物跑 `redteam config check --kind <kind>`。
- [ ] 更新 `.docs/runtime/config.md` §2.1 表（v1 → v2）。
- [ ] 多模型池时：按设计 §3.4 的轮转语义理解；`samples=1` + 多家族场景注意跨家族 agreement 下降（Q1）。

## 5. `config check` 诊断码（v2）

| 码 | 级别 | 触发 |
|---|---|---|
| `legacy_shape` | ERROR | v1 形状/旧键/旧别名；message 逐字段给迁移提示（对外唯一口径） |
| `unknown_key` | ERROR(job) / WARN(local) | 未知键 |
| `secret_field` | ERROR | 任何 `apiKey` / `token` / …（job 只认 `apiKeyEnv`；不输出密钥值） |
| `invalid_value` | ERROR | 类型/枚举/范围 |
| `api_not_implemented` | ERROR | `api: "openai-responses"` |
| `unknown_model` | ERROR | 池条目引用不存在的 provider/model |
| `duplicate_model` | ERROR | 同池重复 / 同 provider 重复 `id` |
| `missing_role:<role>` | ERROR | `review` 缺 scanner/reviewer；`triage` 缺 triage；`verify` 缺 verifier |
| `model_unused` | WARN | provider 下的模型未被任何池引用 |
| `refuter_no_models` | WARN | `enabled=true` 且池空（job 将跳过复核） |
| `reasoning_effort_unmapped` | WARN | 选了档，但 family 形状无强度槽（kimi/qwen）或形状未知（**config check 会输出**） |
| `reasoning_effort_suppressed` | WARN | `thinkingLevelMap[level] = null` 或缺档键（省略参数，不保证上游关闭推理；**config check 会输出**） |

## 6. 裁决记录（2026-11-25）

| 问题 | 裁决 |
|---|---|
| Q1 scanner 轮转补偿 | `(group,sample)` 稳定轮转；文档写「多家族需 `samples ≥ 池大小`」；不加 `scanner.fanout` |
| Q2 本地内联 `apiKey` | **允许**（仅本地 config；job 出现即 `secret_field`）。渲染器**绝不写**该键 |
| Q3 实例级 `schemaVersion` | **不新增** |
| Q4 triage 轮转粒度 | 按轴；I0/claims 用池首条 |
| Q5 `model setup` baseUrl 推断 | 显式 > 已知供应商默认 > 其余报错 |
| Q6 refuter 空池 | 本地自动异族回退；job 跳过 + `refute_skipped="no_refuter"` |
| Q7 triage 缺角码名 | `missing_role:triage` |
| Q8 `config migrate` | 不提供 |

## 7. 样例不是无损迁移（给 hibot 的明确说明）

`config-v2.sample.json` 按任务书要求每个角色池只放 1 条。v1 样例的 scanner 池有 2 条
（deepseek + glm）、reviewer 池有 2 条（deepseek + glm），因此：

- scanner 只保留 deepseek，reviewer 只保留 glm（被移除的模型仍在 `providers` 里）；
- triage/verify 是 v2 新增的显式配置，样例取 v1 运行时行为（reviewer 首条 = deepseek，high 档）。

真实渲染时由 Profile/饱和度决定选哪一个，不应把本样例的选择当作固定映射。

## 8. 验收（T-B 出口）

- `config-v2.sample.json` 通过 `redteam config check --kind review|triage|verify`（0 error）。
- `dist/config.schema.json` 与 capabilities 的 `schemaVersion` 双 2，测试钉住。
- 设计 §2 的每个 v1 键在 `config check` 里都有专属迁移提示（逐键单测）。
