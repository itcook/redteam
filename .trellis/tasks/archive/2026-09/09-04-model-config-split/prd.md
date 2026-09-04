# 模型配置重构：provider/model 概念分离

## 背景

现有 `[providers.X]` 把 provider 与 model 一对一耦合（`model = "glm-5.3"` 单值字段），
导致：1 个 provider 多个 model 无法表达；模型属性（context window/output limit/思考档）
与通道属性（base_url/协议/key）混在一节；output_limit 等模型属性无配置位（F3 的
per-model max_tokens 权衡只能靠全局 env）。

## 目标形状

```toml
# ---- 通道层：provider（1 provider : N models）----
[providers.agc]
kind = "openai"                      # openai | anthropic | google（google 规划占位）
base_url = "https://api.aigocode.team/v1"
api_key_env = "AGC_TEAM_API_KEY"     # 该 provider 默认 env（model 可覆盖）

[providers.deepseek]
kind = "openai"
base_url = "https://api.deepseek.com"
api_key_env = "DEEPSEEK_API_KEY"

# ---- 模型层：model（--models 引用的名字空间）----
[models.GLM]
provider = "agc"                     # ← 必填引用（用户清单未列，分离后必需）
id = "glm-5.3"                       # 请求体里的 model id
kind = "anthropic"                   # 可选：覆盖 provider.kind
# api_key_env = "..."                # 可选：覆盖 provider.api_key_env
context_window = 1000000
output_limit = 32768                 # max_tokens 的 model 级来源（> env > 32768 默认）
reasoning = true
thinking_level = "medium"            # off|low|medium|high（schema 先行，backend 消费后续接）
# per_turn_timeout_secs = 900        # ← 从 provider 迁到 model（速度是模型属性，Kimi 1200 实证）
# priority = 0                       # 候补链排序（缺省 0；--models-all/default 全空时按此排序）
```

secrets.toml 同步嵌套化（key 粒度 = model 级，与 model.api_key_env 覆盖语义对齐）：

```toml
[agc.glm-5.3]
api_key = "sk-xxx"
[agc.kimi-k3]
api_key = "sk-yyy"
```

## 生效链（覆盖/解析优先级）

- **kind**：model.kind > provider.kind
- **api_key**：model.api_key_env(env) > provider.api_key_env(env) > secrets[provider.id].api_key
- **max_tokens**：model.output_limit > REDTEAM_MAX_TOKENS env > 32768
- **per_turn**：model.per_turn_timeout_secs > REDTEAM_PER_TURN_SECS env > 150s
- **context window（pack 预算）**：model.context_window > 128K 保守缺省
- **候补链**（--models/default 全空时）：全部 models 按 priority 降序
- **google kind**：enum 占位 + backend 构造返回明确 not-yet-supported 错误

## 决策点（本 PRD 补充，用户清单之外）

1. `provider` 引用字段为 model 必填（分离后的关联键）。
2. `per_turn_timeout_secs` 从 provider 迁到 model（速度是模型属性）。
3. `priority` 保留在 model 级（候补链语义不变，挂靠点随名字空间迁移）。
4. **不做旧形状兼容**（硬切）：providers 节出现 `model`/`context_window_tokens` 等旧字段时
   解析报错并提示「provider/model 已分离」；用户配置手动迁移。
5. `reasoning`/`thinking_level` 本任务只入 schema + 校验，backend 消费（reasoning_effort/
   thinking budget）留后续。

## 范围

- 改：config/mod.rs（双 struct + resolve_key/resolve_models/write_provider）、config/secrets.rs
  （嵌套）、cli/mod.rs + tail.rs（resolve_models/预算/model_ids_for）、main.rs（build_registry
  merged 构造 + model 子命令显示适配）、provider/backends.rs（backend_from_config 吃 merged）、
  init 模板、相关测试、用户级配置迁移。
- 不改：配置 CLI 交互（后续交互式化）、backend 内部收发逻辑。

## 验收

- 新形状配置解析生效：GLM/Kimi/DeepSeek 三模型（1 网关 provider 2 model + 直连 1）model
  list/check 全绿。
- --models GLM 走 agc provider anthropic kind（覆盖链生效）。
- output_limit/per_turn 的 model 级覆盖在请求体/超时上生效。
- cargo test 全量绿；旧形状配置报指向性错误。
