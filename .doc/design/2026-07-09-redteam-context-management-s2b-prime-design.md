# redteam 上下文管理·S2-b'（三错配修正·第一批）设计

> 状态: 已交付（2026-07-10 合 dev 5fd6c9f）· 0 壳整文件后被无工具默认化承接超越（changeset 全景→单格）。立项依据 = 残余读解剖
> （`../research/2026-07-09-s2b-static-validation-research.md` 解剖节，2026-07-09 定案）。
> WHY → `../decisions/2026-07-06-redteam-context-management-decisions.md` §E7。
> 差异账 → `../roadmap/mvp-delta-ledger.md`（S2-b' 段）。

## 0. 立项依据（一句话）

残余读解剖定案：模型不重读 pack 已给内容（实施层全排除），残余 tool_use 全落 pack 结构上
不给的四类——根因 = **设计层三错配**：**粒度**（符号级 pack vs 模型文件级消费）、**范围**
（per-bundle vs per-changeset）、**覆盖**（refute pass 零 pack）。模型缺**成块原文**非行级精度。

## 1. 范围（第一批 = 无设计张力的三件）

| # | 件 | 对准 | 状态 |
|---|---|---|---|
| ① | 0 壳升级「改动文件全文」 | 粒度错配（杀「整读改动文件」类残余读，每格第一动作） | ✅ 本批 |
| ② | refute pass 带上下文 | 覆盖错配（杀 refute 零 pack 裸奔，每 finding 乘数） | ✅ 本批 |
| ③ | 预算帽 64K→256K | ①的前提（cli.rs 单文件 ~90KB > 64K 帽） | ✅ 本批 |
| — | 类型完整定义块注入（对 struct/enum 给整块非签名行） | 「search struct」类残余 | 🧊 缓：①解剖后若该类仍在再做（ledger） |
| — | 变更集全景（cell 附其他 bundle diff 一览） | 范围错配 | 🧊 缓：与 D12 独立性张力、需 grill；①②解剖后若跨 bundle 读仍是大头再启（ledger） |

## 2. 机制

- **① 0 壳整文件**：`body_section` 取块从「改动 hunk 的 enclosing def 体」升级为「改动文件全文」
  （预算内）；单文件超剩余预算 → **回落现状 def 体**（不砍语义、只降粒度）；再不够 → omit 坐标（现状）。
  非支持语言文件（.md 等）同样给全文（模型对 templates 格的整读实证需求；纯文本无需 def 解析）。
  **顺带吞 T42**（test-mod hunk 锚定失败→空 pack：整文件后锚定与否不再决定 pack 有无）。
- **② refute 带上下文**：`build_refute_prompt` 注入 finding 所指文件的相关段——`finding.file` +
  `line_start..line_end` 扩窗（±60 行，预算封顶 ~24KB）；文件读不到则跳过（行为同现状）。
  只改 prompt 构造、不改 refute 协议/解析。
- **③ 预算帽**：`PACK_BUDGET_BYTES` 64_000 → 256_000（flat；1M-context 模型的 ~6%、256K 模型的 ~25%，
  安全）。= T27 的 MVP 半兑现（per-model 真预算仍缓 ledger）。

## 3. 验证 = 解剖法（非 A/B 计数）

带 `REDTEAM_TOOL_LOG`（仪器 4fe9b9b）+ `max_concurrency=1` 重跑同语料（6f95c25、DeepSeek、scip 或
regex 臂均可——①②③全与引擎正交）一次，判据：
- **门**：「整读本 bundle 改动文件」类调用**消失或大减**（现状每格第一动作必发）；refute pass 的
  重读次数明显降（现状 14 调用/趟）。
- 顺带记录：input 变化（预期升）、「search struct/跨 bundle 读」两类残余占比（喂③④缓项决策）。
- 不设 tool_use 总数 A/B 门（已证 N=1 噪声 ±40% 淹没计数信号；行为类别消失是强信号、一跑可见）。

## 4. 关联

- research 解剖节（立项依据）；decisions E7（WHY：S2-b' 立项 + S2-c 死刑 + S3/S4 重定位）。
- 吸收/兑现：T29（小文件全文→整文件超集）、T42（锚定 bug 被①吞）、T27（MVP 半兑现）。
- plan：`../plans/2026-07-09-redteam-context-management-s2b-prime-plan.md`。
