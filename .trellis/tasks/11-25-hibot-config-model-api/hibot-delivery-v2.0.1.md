# hibot 增量：config v2.0.1（`schemaVersion` 仍为 2）

> 2026-11-25 · redteam `models[]` 条目可覆盖 `api` / `apiKey`。**不破坏旧配置**，只需刷新冻结 sha。

## 一句话语义

`providers.<p>.models[]` 的每个条目现在可以写 `"api": "openai-completions" | "anthropic-messages"`
（+ 预留 `openai-responses`，配置期仍拒绝）来覆盖 provider 的 `api`；**有效 api = `models[].api ?? provider.api`**。
`baseUrl` 仍只在 provider 级——同一 provider 下确实需要不同端点时才拆 provider。`apiKey` / `apiKeyEnv`
的解析链不变（条目级本来就在最高优先），job 模式条目级 `apiKey` 仍判 `secret_field`。

## 对 hibot 的两点影响

1. **schema 产物换了哈希**（形状只增不改，`$id` 与 `schemaVersion` 不变）：
   - `$id: urn:redteam:config/2`、`schemaVersion: 2`、`title: redteam executor config`（未变）
   - `dist/config.schema.json` 新 sha256：`246d2c97b1099090f30cfcb048e70daaef5c87673bc23c9ae50c435c5091514a`
   - 变更点：`definitions.ModelConfig.properties.api`（`anyOf[ProviderApi, null]`，可选）；`apiKey` 仍**不在** schema。
2. **迁移提示口径修正**（影响 hibot 转述给作者的文案）：v1 模型级 `kind` 不再要求「拆 provider」，
   提示改为 `model.kind → model.api = "<api>"`；同一 provider 可混多种 api。

## 渲染建议

```json
"models": [
  { "id": "pro",  "contextWindow": 1000000 },
  { "id": "opus", "api": "anthropic-messages", "contextWindow": 200000 }
]
```

省略 `api` 的条目行为与今天**逐字节一致**（序列化也不写该键）。

## 校验

- `redteam config check config/executor.redteam.json --kind review|triage|verify` 均 `RESULT ok`（样例见任务目录）。
- v1 形状配置仍逐字段报迁移提示（含新的 `model.api` 指向）。
