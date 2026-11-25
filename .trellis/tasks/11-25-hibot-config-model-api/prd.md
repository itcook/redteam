# config v2.0.1：`models[]` 条目可覆盖 `api` / `apiKey`

> 基线 `77aee9b` · 2026-11-25 · 协调者 `01a20ba0`（用户裁决）

## 决策（已定，不再讨论）

1. **保持 `schemaVersion = 2`、`$id = urn:redteam:config/2`**：只加可选键，不破坏旧配置；重生成 schema 并换哈希。
2. 条目级**只允许覆盖 `api` 与 `apiKey`**；`baseUrl` 仍在 provider 级（需要不同端点 → 拆 provider）。

## 范围

| # | 内容 | 验收 |
|---|---|---|
| 1 | `ModelConfig` 增 `api: Option<ProviderApi>`（camelCase `api`） | serde/JsonSchema 自动；不写时不序列化 |
| 2 | **有效 API = `model.api.unwrap_or(provider.api)`**：backend 构造 / reasoning 参数 / registry 分派都走有效值；`provider.baseUrl` 仍是唯一端点 | 单测：同 provider 下 A 走 anthropic、B 走 openai；无覆盖时与今天一致 |
| 3 | 迁移口径修正：`model.kind` → **`model.api`**（不再要求「混协议拆 provider」）；§2.6 与 `config check` 提示同步 | 单测断言提示文案指向 `model.api` |
| 4 | schema：`ModelConfig.api` 三值枚举；`apiKey` 仍不进 schema；job 模式条目 `apiKey` 仍 `secret_field`；重生成 `dist/config.schema.json` | `make verify` 零 diff；给出新 sha256 |
| 5 | 文档：设计 §2.2/§2.3/§3.1 + README；hibot 增量说明（新 sha + 一句话语义） | 文档自洽 |
| 6 | 测试：覆盖 / 一致 / 迁移提示 / job 模式 secret | `cargo test` 全绿、告警 0 |

## 不做

- 不改 `schemaVersion` / `$id`；不加 `baseUrl` 覆盖；不碰 triage/verify/review 逻辑（只动 config→backend 的有效值解析）；
  不重建入库二进制；不改 `dev`。
