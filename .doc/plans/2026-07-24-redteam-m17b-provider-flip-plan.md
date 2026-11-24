# M17-b provider 翻转 · 实施计划

> 状态: 已交付(2026-07-27 合 dev 16f118b) · 对应 design `../design/2026-07-24-redteam-m17b-provider-flip-design.md`（八裁决闭集）；决策 `../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §B；里程碑=roadmap M17-b。
> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development，逐 task 派发。

**Goal:** 默认路径=显式 single_shot+repair；tool_loop 收缩 tools=on 诊断档；direct.rs 整删；D1 双侧修；T34/derive_kind 正名；probe 认证修。

**Architecture:** 先搬活物（Task 1 parse.rs——后续全部依赖它），再立新构（Task 2 single_shot），再收缩接线（Task 3），再修 bug（Task 4 D1 / Task 5 T34+P2），live 冒烟收口（Task 6）。**铁律：Task 1-3 零行为变化**（等价性锚测试证明），Task 4-5 仅修 bug。

**分支:** `feat/m17b-provider-flip`（从 dev 切出；纯本地，永不 push）。

## Global Constraints（每 task 隐含）

- 契约冻结（design §6）：LensCall 结构、iterations 1/2 口径、ReviewProvider trait、registry 行为、report schema、三档请求体与轮次行为全不动。
- 每 task 收尾：`cargo test` 全绿 + `cargo clippy --all-targets` 零新增告警 + commit。
- 注释/commit 简体中文。只 commit 绝不 push。
- 范围闭集：design §8 排除项一行不碰。
- 密钥纪律：任何时候不读不写 `~/.config/redteam/secrets.toml`；live 冒烟由控制器（主会话）执行，不在 subagent 内做。

---

### Task 1: parse.rs 承接活物，direct.rs 整删

**Files:** Create `src/provider/parse.rs`；Delete `src/provider/direct.rs`；Modify `src/provider/mod.rs`（mod 声明）、`src/provider/tool_loop.rs`（import 改道 + REPAIR_PROMPT/diagnostic_parse_error 迁出）

**Steps:**
- [ ] 1.1 `grep -rn "direct::" src/ tests/` 列引用面（已知：tool_loop 的 parse_lens_output import）。
- [ ] 1.2 创建 parse.rs：迁入 `parse_lens_output`+`LensOutput`（direct.rs:39 起）及 direct.rs 中**全部 parser 相关测试**（DeepSeek 截断/围栏剥离等 dogfood 沉淀一条不丢，逐个核对测试名清单入报告）；迁入 tool_loop 的 `REPAIR_PROMPT`+`diagnostic_parse_error`（pub(crate) 化，tool_loop 改 import）。
- [ ] 1.3 删 direct.rs 整文件（DirectProvider/build_anthropic_body/build_openai_body/extract_*/SYSTEM/三常量——先确认全部无 direct.rs 外引用）；mod.rs `pub mod direct`→`pub mod parse`；registry.rs doc 注释里 DirectProvider 提及句删除；mod.rs `anthropic_messages_url` 注释「backends 与 direct 共用」措辞改（引用对象消失）。
- [ ] 1.4 全绿 + clippy + commit：`refactor(provider): 活解析器迁 parse.rs，DirectProvider 死码整删（B-Q3）`

### Task 2: single_shot.rs + timed_turn 共享

**Files:** Create `src/provider/single_shot.rs`；Modify `src/provider/mod.rs`（timed_turn + mod 声明）、`src/provider/tool_loop.rs`（NO_TOOLS_SYSTEM 迁出后 import；三处 timeout+retry 手写块改用 timed_turn）

**Interfaces:**
- `pub(crate) async fn timed_turn(...)` 落 mod.rs：把 tool_loop 现有 `retry_classified(budget, || timeout(per_turn, backend.turn(...)))` 模式抽公共 helper（turn 与 turn_final 两形态，可用闭包参数或两个 helper，以调用侧最简为准）。
- `run_single_shot(backend, user_prompt, per_turn, retry_budget) -> Result<LensCall, ProviderError>`：system 恒 NO_TOOLS_SYSTEM（迁入本文件）；首发 `turn(&messages, &[])`（**保持现状语义，不用 turn_final**）→ parse 成功 iterations=1 / 失败 push REPAIR_PROMPT + `turn_final` → 成功 iterations=2 / 再败 `diagnostic_parse_error`。usage 跨两轮累加、cached 同；tool_use 恒 `ToolUse::default()`；needs 透传。
- `SingleShotProvider { backend: Box<dyn ChatBackend> }` impl ReviewProvider（不持 root/executor）。

**Steps:**
- [ ] 2.1 TDD：先写 single_shot 单测（FakeChatBackend 驱动，design §7-1 六场景：成功=1/repair=2 且 REPAIR_PROMPT 入 messages 且走 turn_final/repair 再败 diagnostic 含 finish_reason/retry 边界/needs 透传/tool_use 恒零）→ FAIL → 实现 → 绿。
- [ ] 2.2 **等价性锚迁移**：找出 tool_loop 测试群中所有 tools_enabled=false 驱动的测试（grep `false` 于 tool_loop.rs tests 及相关），**迁移为 run_single_shot 测试**（断言原样保留）；迁移清单入报告。tools=on 测试原样不动。本步 run_tool_loop 签名尚未改（Task 3），迁移的测试直接调 run_single_shot。
- [ ] 2.3 全绿 + clippy + commit：`feat(provider): single_shot+repair 显式实现（B-Q1/Q2，等价性锚迁移自 tool_loop 无工具测试群）`

### Task 3: tool_loop 收缩 + main.rs 装配翻转

**Files:** Modify `src/provider/tool_loop.rs`（删 tools_enabled 参与旗标字段）、`src/main.rs`（装配 match）

**Steps:**
- [ ] 3.1 `run_tool_loop` 删第 8 参 `tools_enabled`（恒 `tool_specs()`；`#[allow(too_many_arguments)]` 注释顺更新）；NO_TOOLS_SYSTEM 引用移除（已迁 single_shot）；自收敛早退/FINALIZE/repair 全机器**原样保留**。`ToolLoopProvider` 删 `tools_enabled` 字段与 new 参。
- [ ] 3.2 main.rs 装配（现 :353-359 附近）：`match tools_mode { On => ToolLoopProvider::new(backend, cwd), _ => SingleShotProvider::new(backend) }`（每 model 各自装配，registry insert 不变）。`tools_enabled` 局部变量消失。
- [ ] 3.3 全绿（含 e2e——fake provider 路径不受影响）+ clippy + commit：`refactor(provider): tool_loop 收缩 tools=on 诊断档，默认档装配翻转 single_shot（B-Q2/装配）`

### Task 4: D1 双侧 200+error 体分类修复

**Files:** Modify `src/provider/parse.rs`（error_body_to_provider_error + 测试）、`src/provider/backends.rs`（三处接线）

**Steps:**
- [ ] 4.1 TDD：D1 四锚先行（design §7-3：200+OpenAI `{"error":{"type":"rate_limit_error"}}` → RateLimited 可重试 / 200+Anthropic `{"type":"error","error":{"type":"overloaded_error"}}` → 可重试 / 200+auth 类 → Auth 不可重试 / 正常 choices/content 体 → None 不干预）→ FAIL。
- [ ] 4.2 实现 `error_body_to_provider_error(v: &Value) -> Option<ProviderError>`（parse.rs；映射表见 design §4，未知 error 类型 → Unavailable 保守可重试）。
- [ ] 4.3 接线三处：①`AnthropicBackend::turn` decode_json 之后先探 error 再 parse_anthropic_turn；②`parse_openai_stream` 的非流式 JSON 回落分支（backends.rs:637 测试所在路径）前探；③OpenAI post 读得整体纯 JSON error 体（非 SSE）时探。每处：Some(e) → return Err(e)。SSE 帧内 error 不做。
- [ ] 4.4 全绿 + clippy + commit：`fix(provider): D1 双侧 200+error 体按语义分类（可重试类不再烧格；B-Q4，疑 T35 同源待复测）`

### Task 5: T34 route_key 正名 + derive_kind 返 ProviderKind + probe 认证修

**Files:** Modify `src/provider/mod.rs`（LensRequest 字段）、波及点（orchestrate/fake/registry/tool_loop/single_shot）、`src/model/mod.rs`（derive_kind 签名+调用点）、`src/model/probe.rs`（按 kind 分派头）

**Steps:**
- [ ] 5.1 `LensRequest.model` → `route_key`：`grep -rn "\.model" src/ | grep -i "req\|lens"` 找全波及点纯替换；registry doc 散文精简（路由键概念已由字段名自说明）。
- [ ] 5.2 `derive_kind` 返 `ProviderKind`（调用点同步；String parse 往返消除）。
- [ ] 5.3 probe：`list_models` 增 kind 感知——「按 kind 选头」抽纯函数（Anthropic → x-api-key + anthropic-version；其余 bearer）+ 单测；HttpProber 调用它。`model check`/`model setup` 调用链路核对传 kind。
- [ ] 5.4 P2-6a：mod.rs 退避注释按实测改（attempt=0 → `200<<1`=400ms，措辞「200·2^(n+1) ms，首试 400ms」）。
- [ ] 5.5 全绿 + clippy + commit：`refactor(provider): T34 route_key 正名 + derive_kind 强类型 + probe 按 kind 认证（B-Q5/Q6）`

### Task 6: 收口验证（控制器亲跑，不派 subagent）

- [ ] 6.1 `make build` + `make verify`。
- [ ] 6.2 **live 冒烟**：真实小变更集单模型 `redteam review`（默认 two-stage 档走 SingleShotProvider），确认端到端产出 verdict/report 正常、warnings 无异常。
- [ ] 6.3 T35 复测：secrets 中 Kimi 配置在 → 同一冒烟场景跑 Kimi 观察尾部超时是否消解；不在 → followup 记「T35 复测待 Kimi 配置」不阻塞。
- [ ] 6.4 commit（如有 build 产物）。

---

## 收尾

- 整分支 opus 终审（review-package MERGE_BASE=分支起点）→ 修 findings → 合 dev（--no-ff，纯本地）。
- roadmap M17-b 置 ✅、done.md 记账、本 plan 状态头改「已交付」、T34 标交付、T35 复测结果记账。
