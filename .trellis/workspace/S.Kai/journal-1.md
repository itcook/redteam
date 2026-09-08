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


## Session 3: P0 验证门收尾：E1 风险规则回放 + E3 疑点契约 A/B，.next/ 分支随行文档区迁移
<!-- trellis-session: v=2 fp=8b884f84834698b9 -->

**Date**: 2026-09-07
**Task**: P0 验证门收尾：E1 风险规则回放 + E3 疑点契约 A/B，.next/ 分支随行文档区迁移
**Branch**: `feat/m21-review-engine`

### Summary

E1：eval/risk_replay.py 零 LLM 回放 PR181，规则 v4（θ=1.5，H0 需 ≥2 类信号）主裁决集区间级 4/4、并集空热区 0.47；held-out 本仓 oracle B 区间级 20/34、文件级 29/34；预算三档 |H0| 中位 0/5/23。E3：DeepSeek v4 flash 同 bundle 同 focus 各 3 跑，疑点契约人工主题命中三跑并集 5:1 → design §4 成立；附带 c_hi 不成立、需钉推理档、quote 守约弱。迁移：.roadmap 与主 checkout .doc 的 M21 design/decisions/research 入 .next/（入库），PR181 细节与原始数据入 .next/private/（gitignored，远端公开仓）。待办：grill 必答项（涟漪去留、两类信号规则、θ 常量化、三档起步值、R11 改 agreement 口径）→ design 锁定 → 派生 M21a-e plan。

### Git Commits

| Hash | Message |
|------|---------|
| `44d7f75` | docs(next): 分支随行文档区 .next/——.roadmap 迁为 .next/roadmap，主 checkout .doc 的 M21 设计/决策/研究迁入 |
| `1a44197` | feat(eval): P0 验证门 E1/E3——零 LLM 风险规则回放 + 疑点契约 A/B + 组包仪器，结果摘要入路线图 |

### Status

[OK] **Completed**


## Session 4: grill D11 → design v8 锁定；M21a plan 派生（六批 + 出口）
<!-- trellis-session: v=2 fp=bc42fdeaaf037c56 -->

**Date**: 2026-09-08
**Task**: grill D11 → design v8 锁定；M21a plan 派生（六批 + 出口）
**Branch**: `feat/m21-review-engine`

### Summary

grill 七项必答（涟漪二梯队 / 两类信号进 §3.3 / 无 [review.risk] 与 --risk、θ 权重常量化 / 预算 R 三档 4-12-32 帽 48 / 必挖 = 主区 ∪ agreement≥2 + 疑点保底 ⌈max_cells/4⌉ / R6 销案 / R14 role + 输出枚举 + 台账表 + 队列快照表）记入 decisions D11，design 锁定 v8，roadmap P0 ✅、followup 归位、主账本登记 M21。派生 M21a plan：.next/plans/2026-09-07-redteam-m21a-scan-foundation-plan.md，六批 A 配置节 / B schema v4 / C 分组索引 + --pack-only / D 契约提示词 / E 切片 / F 硬切并行 + 出口十条命令化验收；波次 A/B/C 并行（各自工作树与子分支）→ D/E → F → 出口。下一步：开三个 session 起 m21a-config / m21a-schema / m21a-grouping。

### Git Commits

| Hash | Message |
|------|---------|
| `47fc9d8` | docs(design): D11 P0 出口裁决——design 锁定 v8；roadmap P0 ✅、R5/R6/R11/R14 归位、--risk 提法清除 |
| `dc2c008` | docs(plan): M21a 疑点扫描地基 plan 派生——六批 A-F + 出口验收，波次与子分支/工作树布局 |

### Status

[OK] **Completed**


## Session 7: M21a 批 B schema v4：疑点 schema + 角色分叉 CellOutput + proj-next.db v4 + RunMeta 观测字段 + 固定集导出；code-review 审核修复并合回
<!-- trellis-session: v=2 fp=d0fc14d8945963aa -->

**Date**: 2026-09-08
**Task**: M21a 批 B schema v4：疑点 schema + 角色分叉 CellOutput + proj-next.db v4 + RunMeta 观测字段 + 固定集导出；code-review 审核修复并合回
**Branch**: `feat/m21a-schema`

### Summary

批 B 四 task 落地（export_ledger 固定集 63/3/48、domain/suspicion.rs、CellRecord v4 + db 守卫、RunMeta 五字段 + SCHEMA_VERSION 4）；code-review 技能撞网关，主 agent 手工审读 + 五路视角出 20 余条，用户三裁定（proj-next.db 分库 / 导出内置搁置 / role 只由 output tag 派生）后修复两笔；两次合入 engine（含批 A、批 C）并 --no-ff 合回 b7af7d3；批 A JSON 重做后核验 config::Role / EffectiveModel.family / resume 回查三链完好。定：核心能力稳定前不跑 ship 评审。

### Git Commits

| Hash | Message |
|------|---------|
| `0ef7798` | chore(eval): 导出旧 proj.db 的 dismissed / upheld 固定集（verifier 离线回归集数据，D10） |
| `08484bd` | feat(domain): 疑点 schema（suspicion / category / scanned_areas / needs.kind）+ 按角色分叉的 CellOutput 枚举（D11 R14） |
| `7c5e253` | feat(state): CellRecord 带 role/output 枚举/template_hash；proj.db schema v4 一次重置（cells.role + queue_items + suspicions 表壳） |
| `0ec7912` | feat(report): RunMeta 加 coverage 五集合 / 模板哈希 / cache 命中 / 在途峰值 / refute 率字段；report schema_version 4 |
| `03b5c6c` | fix(schema): 评审修复——守卫五改 + role 唯一真相源 + Role/family 复用批 A + 独立库文件 proj-next.db |
| `3ac2332` | chore(eval): 固定集加 resolved / in_dismissed_ledger / head_sha，路径 URI 转义；oracle 兼容 v4 记录；followup 登记五条债 |
| `2c4bb76` | chore(task): m21a-schema 规划与实施制品（prd / design / implement，含 code-review 审核修复记录） |

### Status

[OK] **Completed**
