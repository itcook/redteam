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
