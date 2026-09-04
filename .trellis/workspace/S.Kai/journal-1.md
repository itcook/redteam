# Journal - S.Kai (Part 1)

> AI development session journal
> Started: 2026-09-03

---



## Session 1: AnthropicBackend 流式化：修大 bundle 长思考 524 + PR181 评审交付
<!-- trellis-session: v=2 fp=525f7403e616910c -->

**Date**: 2026-09-04
**Task**: AnthropicBackend 流式化：修大 bundle 长思考 524 + PR181 评审交付
**Branch**: `dev`

### Summary

根因：2d9945d 只修了 OpenAI 侧流式，AnthropicBackend 仍整读 JSON，大 bundle 长思考连接静默被 Cloudflare 524。修复链四层洋葱：流式 SSE 聚合、三层分流（整体 JSON 优先/行首 data: 锚定/envelope 兜底，dogfood F1）、截断完整性门（F2）、per-provider 超时旋钮（Kimi 30tok/s×32K 需 1200s）。顺产：resume join_all 并行+报告回写 proj.db、RunMeta.cells + Cells 表可观测、spec/backend/error-handling.md 填充。PR181 dogfood 双模型完整 reject 8 findings；与 rpiv A/B 结论=互补层（redteam 独有 F3 配置回归洞+Kimi 独有 callback 队列退化洞）。T54✅ T55🧊余 T56（GLM usage 低报 20 倍）待查。

### Git Commits

| Hash | Message |
|------|---------|
| `2e797dc` | feat(provider): AnthropicBackend 流式化——治大 bundle 长思考 524（对齐 2d9945d OpenAI 侧） |

### Status

[OK] **Completed**


## Session 2: 模型配置重构：provider/model 概念分离 + ocr 三方对照实验
<!-- trellis-session: v=2 fp=f011bdd6a2d09720 -->

**Date**: 2026-09-04
**Task**: 模型配置重构：provider/model 概念分离 + ocr 三方对照实验
**Branch**: `dev`

### Summary

config 硬切新形状：[providers.X]通道层(kind/base_url/api_key_env,1:N)+[models.X]模型层(provider引用/id/kind覆盖/api_key_env覆盖/context_window/output_limit/reasoning/thinking_level/per_turn/priority)，secrets嵌套[provider."model_id"]，覆盖链 model>provider；旧形状报指向性迁移错误。狗粮首评即审出8个真缺陷(F1 secrets静默毁key/F2 威胁模型B的models节后门等)全修。附带：ocr(open-code-review)同PR同网关四跑对照——非流式client在AGC/Cloudflare下524全灭(跨anthropic/responses双协议证实)，流式清零524但多轮循环招429限流，redteam单发流式唯一零失败路线；报告.doc/reviews/。M19方向(ocr语义分组+code_comment增量输出借鉴)入账。

### Git Commits

| Hash | Message |
|------|---------|
| `1ae876b` | feat(config)!: provider/model 概念分离——[providers.X] 通道层 + [models.X] 模型层 |
| `84e0136` | fix(config): 狗粮 ccbedddcdd01 8 findings 全修——secrets 防毁/威胁模型 B 补 models 剥离/setup 适配收尾 |

### Status

[OK] **Completed**
