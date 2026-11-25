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

## 交付记录（2026-11-25）

| 提交 | 内容 |
|---|---|
| `826899d` | `feat(config)`：`models[]` 条目可覆盖 `api` + schema 产物 + README + 任务目录（样例 / hibot 增量说明） |
| `ab6b50d` | `docs(config)`：设计 §2.2/§2.3/§2.6/§3.1/§7 |

**验收**：`make verify` exit 0；`cargo test` exit 0（lib 1180 / verify_conformance 15 / verify_fixtures 4 / 其余全绿）；
`cargo build --all-targets` 0 warning。

**schema**：新 sha256 = `246d2c97b1099090f30cfcb048e70daaef5c87673bc23c9ae50c435c5091514a`；
`$id: urn:redteam:config/2`、`schemaVersion: 2`、`title` 未变；新增仅 `ModelConfig.properties.api`（`apiKey` 仍隐藏）。

**`config check`（冻结 v2 样例 + 条目级覆盖样例）**：review / triage / verify 三 kind 均
`RESULT ok 0 errors, 0 warnings`。附带条目级覆盖的样例（`config-v2.0.1.sample.json`，`family: glm` 匹配
anthropic reasoning 映射）；若 family 与协议形状不匹配会得到 `reasoning_effort_unmapped` WARN——这是
有效 api 确实进入 reasoning 解析的旁证。

**合并**：协调者独立复核（本地复算 schema sha 一致、`$id`/`schemaVersion` 未变、`EffectiveModel.api` 单点解析、
迁移提示已改、`make verify` 0 告警、全量 `cargo test` 0 failed）并合入 `hibot-executor = 98f5347`。
首轮 `partial_fixture` 失败是既有 flaky（单跑 25/25 通过），与本次无关。

**followup（协调者记，不返工）**

1. `providers.<p>.models[{id}] 未被任何角色池引用，不会参与任何作业` 这条 WARN（`src/config/mod.rs:840`）
   在数组结构下应写成索引或 `models["id"]`，现在把 id 塞进了 `[...]`。
2. 与 `missing_role` 的 pointer 落在 `/providers` 的问题一起，后续统一清理诊断 pointer 口径。
