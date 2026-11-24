# M17-b provider 翻转 · 设计

> 状态: 锁定 · 2026-07-24 grill 八裁决全数拍板；WHY 见 `../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §B；实施计划 `../plans/2026-07-24-redteam-m17b-provider-flip-plan.md`；里程碑=roadmap M17-b。

## 1. 目标

provider 层骨架翻到与无工具默认范式一致：默认路径=显式 `single_shot + repair` 薄实现，
tool_loop 收缩为 tools=on 诊断档独立装配；DirectProvider 死码删除（活解析器迁出）；
顺带修 D1（200+error 体误分类，双侧）、T34 正名、P2 三件。**铁律：纯结构重构零行为变化**
（现状 tools_enabled=false 路径功能上已是 single_shot+repair，翻转只是提成显式结构）。

## 2. Provider 形态

```
main.rs 装配（按 tools_mode match）：
  Off | TwoStage → SingleShotProvider { backend }            // 默认档；不持 root/executor
  On            → ToolLoopProvider   { backend, root }       // 诊断档；tools_enabled 字段删除
```

- `ChatBackend` trait、`ProviderRegistry`、`LensCall`（含 iterations 1/2 口径）全部不动。
- **首发语义**：single_shot 首发 = `turn(&messages, &[])`（保持现状：OpenAI 侧**不带**
  json_object）；repair 用 `turn_final`。首发改 json_object 强制是被否项（→followup A/B）。

## 3. 模块布局

| 文件 | 内容 |
|---|---|
| `provider/single_shot.rs`（新） | `NO_TOOLS_SYSTEM`（自 tool_loop 迁入）· `run_single_shot(backend, user_prompt, per_turn, retry_budget) -> Result<LensCall>`：timed+retry 的 `turn(空 tools)` → parse 成功 iterations=1 / 失败 REPAIR+`turn_final` → 成功 iterations=2 / 再败 `diagnostic_parse_error`；tool_use 恒零、needs 透传 · `SingleShotProvider` |
| `provider/parse.rs`（新） | `parse_lens_output` + `LensOutput`（自 direct.rs 迁入，测试群一条不丢）· `REPAIR_PROMPT` · `diagnostic_parse_error` · `error_body_to_provider_error`（D1，见 §4） |
| `provider/mod.rs` | `timed_turn` helper（timeout+retry_classified 包装，与传输层同居）· `pub mod direct`→`pub mod parse` · anthropic_messages_url 注释措辞更新 · 退避注释对齐实现（P2-6a：按实测 attempt=0→400ms 写） |
| `provider/tool_loop.rs` | `run_tool_loop` 删 `tools_enabled` 参（恒 `tool_specs()`）；`ToolLoopProvider` 删旗标字段；12 轮/FINALIZE/自收敛 repair（Fix 4 分支）原样保留（tools=on 下模型提前吐文本仍是真路径）；`FINALIZE_PROMPT`/`DEFAULT_SYSTEM` 留私有 |
| `provider/direct.rs` | **整文件删除**（DirectProvider/build_*_body/extract_*/SYSTEM/三常量全死码；常量双份随删自动归一=P2-6c 零动作） |
| `provider/backends.rs` | D1 接线（§4）；其余不动 |
| `model/mod.rs` + `model/probe.rs` | `derive_kind` 返 `ProviderKind`（消 String parse 往返）；probe 按 kind 分派认证头（P2-6b，§5） |

## 4. D1：200+error 体分类修复（双侧）

`error_body_to_provider_error(v: &Value) -> Option<ProviderError>`（parse.rs）：

- 形状：OpenAI `{"error":{type/code/message}}`；Anthropic `{"type":"error","error":{...}}`。
- 映射（沿用现有档，无新档）：`rate_limit`/`overloaded`→RateLimited ·
  `timeout`→Timeout · `server_error`/`api_error`→Server · `authentication`/`permission`
  →Auth（不可重试）· `invalid_request`/`context_length`→BadRequest（不可重试）·
  **未知类型→Unavailable（保守可重试）**。
- 接线三处：①Anthropic `turn`（decode_json 之后、parse_anthropic_turn 之前）；
  ②OpenAI 非流式 JSON 回落分支；③OpenAI 整体纯 JSON error 体。SSE 帧内 error 不强求。
- **修后义务**：T35 Kimi 尾部超时复测（挂验收；配置不在则记 followup 不阻塞）。

## 5. T34 正名 + P2

- `LensRequest.model` → `LensRequest.route_key`（纯 rename；波及 orchestrate 构造点/
  fake/registry/tool_loop/single_shot 透传；registry doc 散文减半）。**不动**
  PlannedModel.name/config 键侧（config 契约面）。
- `derive_kind(provider, explicit, base_url) -> ProviderKind`（调用点 probe/model setup 同步）。
- probe：`list_models` 按 kind 分派——Anthropic 用 `x-api-key` + `anthropic-version`，
  其余 bearer_auth；「按 kind 选头」抽纯函数可测。

## 6. 契约冻结面

- 零变化承诺：LensCall 结构、iterations 口径（1/2 语义）、ReviewProvider trait、
  registry 行为、report schema、全部三档（off/two-stage/on）的请求体与轮次行为。
- 唯一 pub 面变化：LensRequest 字段名（crate 内部类型，无外部消费者）。
- 预期行为变化仅 D1（200+error 体从烧格变可重试/正确分类）与 probe（Anthropic 假失败
  变真探测）——两者都是修 bug。

## 7. 测试网

1. **single_shot 单测**（FakeChatBackend）：成功=1 / repair 成功=2（REPAIR_PROMPT 入
   messages、走 turn_final）/ repair 再败=diagnostic Parse 含 finish_reason / retry 边界 /
   needs 透传 / tool_use 恒零。
2. **等价性锚（最关键）**：现状 tools_enabled=false 的 tool_loop 测试群**迁移为
   single_shot 测试**（断言原样保留=零行为直接证据）；tools=on 测试原样保留。
3. **D1 四锚**：200+OpenAI rate_limit→RateLimited 可重试 / 200+Anthropic overloaded→
   可重试 / 200+auth→不可重试快败 / 正常体不受影响。
4. **probe**：按 kind 选头纯函数测。

验收门：全量 `cargo test`（424+）+ clippy 零新增 + `make verify` + **live 冒烟一发**
（真实小变更集单模型 review 端到端）+ T35 复测。

## 8. 范围闭集

本 design §2-§7 即全部范围。点名排除（decisions §B-Q8）：T49（本批后解锁）/首发
json_object A/B（followup）/MAX_TOKENS 调值/Google(M20)/tools=on 档重设计/SSE parser
重构/model 层其余 P2(M18)。
