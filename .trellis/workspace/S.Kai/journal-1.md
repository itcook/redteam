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


## Session 5: M21a 批 C：引用图分组 + 跨组索引 + --pack-only 实施、code-review 十修、并入批 A 基线并 --no-ff 合回
<!-- trellis-session: v=2 fp=b0d9310c006976fb -->

**Date**: 2026-09-08
**Task**: M21a 批 C：引用图分组 + 跨组索引 + --pack-only 实施、code-review 十修、并入批 A 基线并 --no-ff 合回
**Branch**: `feat/m21a-grouping`

### Summary

批 C 五个 task 全部落地：import_edges 三语言（tsconfig/go.mod/crate 根）、引用图分组（强弱边/弱缝先切/宁降不拆/孤立装箱/双阈值门）、跨组索引进 {{OTHER_CHANGED_FILES}}（降质+留指针+[delta]）、--pack-only 预算仪表；code-review high 十条 findings 修复并补测试；merge 已合入批 A 的 feat/m21-review-engine（6 处冲突，预算改读 active_roles）；用户裁决跳过 ship 评审，直接 --no-ff 合回 feat/m21-review-engine（7b3559b）。延后项记 followup：增量轮两份清单合并（D-3）、GroupingStats→RunMeta（F-5）、[review.grouping] 接线。
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
| `0bc01b8` | feat(bundle): import 边补 TS 裸模块 / Rust use crate / Go 包路径（followup 实现债）——import_edges 替代 extract_imports：ImportResolver 惰性缓存 tsconfig（baseUrl/paths/extends 链、JSONC 容错）/ go.mod module 映射 / crate 根（workspace 子 crate 各自为根，尾段 item 回退），目标 ∈ 改动集才成边、越出 root 即丢；Python 相对沿用 |
| `ca74eb5` | feat(bundle): 引用图分组——强弱边连通分量、弱缝先切、宁降不拆、孤立 bin-packing、ocr 双阈值门（design §3.2〔D3〕）——Bundle 带 edges/hub_order/over_budget，Grouping + GroupingStats/GroupingOpts(4/200/10)；引用边按改动 def 名词边界计次（ref_worthy 移植 risk_replay）；落单文件回池装箱；pack 按 hub_order 填（枢纽保全文）；merge_bundles 与 examples/dump_pack.rs 退场（followup 实现债），run_code_review / 增量路径分组下移到预算之后 |
| `bf161a5` | feat(prompt): 正反向跨组索引进 {{OTHER_CHANGED_FILES}} 槽 + [delta] 标注 + omit 留指针（design §10.6〔D6〕）——bundle::cross_index / render_cross_index：每文件一行 path (gN +a/-d) → [imports] ← [imported-by] [delta]，与本组有边者优先，双帽内先降质为 see group N 再尾注带组指针；调用方按组预渲染 others 与 dossiers 同形传入 fan-out / 二阶段补查；增量轮喂全分支非 delta 文件为只带路径的条目；other_files_manifest 退场；旧模板第 80 行改组感知措辞（K10） |
| `a88c025` | feat(cli): --pack-only 预算仪表——只装包、按 cell 报字节与 ÷3.5 估算，不调 provider（D7 B7）——run_pack_only：证据 → 门/分组 → 装包 → 同源 build_bundle_prompt 落盘 .redteam/packs/<job>/cell-<n>.md + summary.json，stdout 固定文本行（cell 字节/估算/pack/diff/over_budget + grouping 统计 + packs 目录）；与 --background/--resume/--incremental-from 互斥；gen_job_id 外移到 cli；examples/dump_pack.rs 的 eval 引用改历史注记 |
| `146bddb` | docs(followup): 批 C 延后项——增量轮两份清单合并（D-3）/ --pack-only 预算改角色条目（A 合回后）/ GroupingStats→RunMeta（F-5）/ [review.grouping] 接线；extract_imports 与 dump_pack 两条实现债标已偿 |
| `a3d2908` | fix(ship): 批 C code-review 十修——TS 点号基名精确匹配再剥扩展名（user.service 不误连 user.ts）；Rust 根文件回退只在父模块即 crate 根且 ≤1 段（super::* 不再粘到 lib.rs）；pub 前缀只在语句首行且需空白/括号（pubsub 续行不被剥）；tsconfig 数组 extends 按属性合并；Go 单行 import 块不进块态、块内只认 [别名] "spec" 整行；count_plus_minus 补 ---sep 正文行测试；引用边改单次合并 regex 扫描 O(n×size)；二阶段增强包沿用 hub_order；plan_cells/budget_and_spec 抽共用，--pack-only 预算改用 resolve_models 同一模型集、label/GITIGNORE_BODY 复用；过期注释更新 |
| `00081cc` | merge: feat/m21-review-engine（含批 A 配置节 be87c44 与 D12 文档）并入 feat/m21a-grouping——解 6 处冲突：project.rs 取批 A 版并保留 GITIGNORE_BODY pub(crate)；ReviewArgs 同时带 test_command / pack_only；预算改由 budget_and_spec(&RoleSets) 读 cfg.active_roles()（全量 / 增量 / --pack-only 同源，K15 已偿）；dump_pack.rs 保持删除；followup 双方条目合并 |

### Status

[OK] **Completed**


## Session 6: M21a 批 A 重做：config.json + provider→models→roles（D12），身份串 provider/id，评审修复，合入批 B/C
<!-- trellis-session: v=2 fp=0d53da7a64ff9d83 -->

**Date**: 2026-09-08
**Task**: M21a 批 A 重做：config.json + provider→models→roles（D12），身份串 provider/id，评审修复，合入批 B/C
**Branch**: `feat/m21a-config-json`

### Summary

批 A 按 D12 重做为严格 JSON（providers.<name>.models.<id>.roles.<role>，camelCase，无注释；toml 退出）；setup 走 SetupPatch 三层显式合并 + 写前校验 + 原子写；HOME 缺失报错、超时优先级、contextWindow 下限、零值校验、shape_guard（snake_case/旧键报错、未知键 warning）。用户裁定身份串固定 provider/id，name 改 displayName 只作显示。code-review 子 agent 报告 1 必修 + 6 建议 + 6 nit 全部并入（键序记 followup）。本机 TOML 已迁 JSON，model list 五条 ✓。feat/m21-review-engine（批 B/C）已预合并进本分支（三处机械冲突），并接上批 C followup 的 review.grouping→GroupingOpts；合并树 493+16+7+14 测试绿。待办：主工作树 --no-ff 合回。

### Git Commits

| Hash | Message |
|------|---------|
| `9b817da` | feat(config): 批 A 重做——config.json + provider→models→roles（D12），setup 三层显式合并 + 写前校验，HOME 缺失报错，超时优先级，contextWindow 下限；toml 退出 |
| `e64276e` | fix(config): 身份串固定 provider/id（name→displayName）+ 重做评审修复（env 提示必修、写前 flag 校验、零值校验、嵌套键守卫、kind/apiKeyEnv 落模型层、原子写、max_tokens 同链） |
| `1f2f89c` | merge: feat/m21-review-engine（批 B schema v4 + 批 C 分组与索引）into feat/m21a-config-json——解三处冲突（ReviewArgs.pack_only 字段 / CellRecord::ok 的 CellOutput::review 包装 vs JSON 夹具 route_key p/<id> / Init 文案 proj-next.db + config.json）；合并树 492+16+7+14 测试绿 |
| `1208166` | feat(cli): review.grouping 配置接到 GroupingOpts（批 C followup「后合者一行」）——plan_cells 三入口同源，缺省 4/200/10 不变；followup 行销案 |
| `0ef7798` | chore(eval): 导出旧 proj.db 的 dismissed / upheld 固定集（verifier 离线回归集数据，D10） |
| `08484bd` | feat(domain): 疑点 schema（suspicion / category / scanned_areas / needs.kind）+ 按角色分叉的 CellOutput 枚举（D11 R14） |
| `7c5e253` | feat(state): CellRecord 带 role/output 枚举/template_hash；proj.db schema v4 一次重置（cells.role + queue_items + suspicions 表壳） |
| `0ec7912` | feat(report): RunMeta 加 coverage 五集合 / 模板哈希 / cache 命中 / 在途峰值 / refute 率字段；report schema_version 4 |
| `03b5c6c` | fix(schema): 评审修复——守卫五改 + role 唯一真相源 + Role/family 复用批 A + 独立库文件 proj-next.db |
| `3ac2332` | chore(eval): 固定集加 resolved / in_dismissed_ledger / head_sha，路径 URI 转义；oracle 兼容 v4 记录；followup 登记五条债 |
| `2c4bb76` | chore(task): m21a-schema 规划与实施制品（prd / design / implement，含 code-review 审核修复记录） |

### Status

[OK] **Completed**


## Session 8: M21a 批 A 重做收尾：评审第二批修复 + 隐私清理 + followup（主分支直接落地，无任务）
<!-- trellis-session: v=2 fp=004b61a73f46bb1f -->

**Date**: 2026-09-08
**Task**: M21a 批 A 重做收尾：评审第二批修复 + 隐私清理 + followup（主分支直接落地，无任务）
**Branch**: `feat/m21-review-engine`

### Summary

批 A 重做合回后，评审子 agent 第二轮报告直接在 feat/m21-review-engine 修：HTTP 整请求超时随模型 per_turn 自动抬（慢模型不再被 300s 先切断）、REDTEAM_PER_TURN_SECS 解析统一、confFloor / 空 provider 名 / 含 '/' / 空 model id 校验、形状错误带字段路径、旧 secrets.toml 提示、相对 REDTEAM_HOME 拒绝、model check 无 scanner 退 1、review / worker 路径配置错误硬失败、package.json 空 test 脚本不算、悬空测试引用修正。隐私：已跟踪文件清除公司仓路径 / 分支 / SHA / 名称（P0 归档 prd+implement、plan 出口验收、524 spike、crosslang-scan 默认路径、三处注释）。followup：评审未核实四项、perTurnTimeoutSecs 退役议题（用户提，按分组/切片后 dogfood 轮时数据裁决）。真机：用户改配置后 model list 四条 ✓（qwen 走 provider 级 apiKey 第 4 级）。

### Git Commits

| Hash | Message |
|------|---------|
| `14b7289` | fix(config): 重做评审第二批——HTTP 整请求超时罩住单轮（M2）、confFloor/空名/含 '/' 校验、形状错误带字段路径、旧 secrets.toml 提示、相对 REDTEAM_HOME 拒绝、model check 无 scanner 退 1、评审路径配置错误硬失败 |
| `a867504` | chore(privacy): 已跟踪文件清除公司仓路径 / 分支 / SHA / 名称引用（P0 task 归档 prd+implement、plan 出口验收、524 spike、crosslang-scan 默认路径、三处「承 hiapi F2」注释）——远端为公开仓，公司细节只记 .next/private/ |
| `3896bfb` | docs(followup): 记批 A 重做评审子 agent 未核实项（e3_contract_ab anchored 恒真式、risk_replay parse_diff 列首 --/++、probe kind 取首模型、check_all 串行）——M21b 用 risk_replay 作回归门前先核实 |
| `da137fa` | docs(followup): perTurnTimeoutSecs 退役议题——分组/切片落地后按 dogfood 轮时数据决定，M21e 拆除时旧键走 warning |

### Status

[OK] **Completed**


## Session 9: A/B/C 合并态三路核验 + 修补批（db 守卫 / 伪引用边 / super 假边 / 索引 e2e / 零值与空串 key / pack-only 不读 secrets）
<!-- trellis-session: v=2 fp=a10136a2f649883d -->

**Date**: 2026-09-08
**Task**: A/B/C 合并态三路核验 + 修补批（db 守卫 / 伪引用边 / super 假边 / 索引 e2e / 零值与空串 key / pack-only 不读 secrets）
**Branch**: `feat/m21-review-engine`

### Summary

三个只读子 agent 对照 M21a 计划核验合并后的批 A/B/C：全部交付；发现 B 库守卫在 user_version==4 时放行陌生库、C 同名 changed def 互成伪强边、顶层模块内联 mod 的 super 连到 lib.rs、跨组索引接线零集成测试，以及 A 零值校验 / 空串 env key / --pack-only 读 secrets 等边角。开 Trellis 任务 09-08-m21a-abc-fixup 一次修掉（8 项，5 个新单测 + 2 个新 e2e），其余九条接缝（cross_edges 口径、lib.rs 超级枢纽回放、CellOutput tag 口径、ScanOutput.suspicions 无 default、over_budget 未消费、TS 副作用导入、est_tokens 不含 system、setup 预检文案、并存期 secrets 提示）记 followup 交给 D/E/F。三道门：cargo test 499+16+7+16 绿、clippy 3 条既有、make verify 绿。结论：可以开 D / E。

### Git Commits

| Hash | Message |
|------|---------|
| `820bc46` | fix(m21a): db 陌生库守卫补 v4 分支 + 只读快路径 WAL 降级；同名 def 伪引用边；顶层模块内联 mod 的 super 假边 |
| `093b454` | fix(m21a): 多组跨组索引 e2e；pack/grouping 零值与空串 env key 校验；--pack-only 不读 secrets |
| `18d9b7c` | docs(followup): A/B/C 合并核验接缝九条（cross_edges/weak_cut 口径、lib.rs 超级枢纽回放、TS 副作用导入与同顶层模块弱边、est_tokens 不含 system、ScanOutput.suspicions 无 default、CellOutput tag 口径、over_budget 未消费、setup 预检文案、并存期 secrets 提示） |

### Status

[OK] **Completed**


## Session 11: M21a 批 E：切片装包（feat/m21a-slice）——E-1 一跳 / E-2 切片配方 / E-3 文件外片段 / E-4 接线与观测 + code-review 十修
<!-- trellis-session: v=2 fp=2f671af057c86a97 -->

**Date**: 2026-09-08
**Task**: M21a 批 E：切片装包（feat/m21a-slice）——E-1 一跳 / E-2 切片配方 / E-3 文件外片段 / E-4 接线与观测 + code-review 十修
**Branch**: `feat/m21a-slice`

### Summary

Trellis 09-08-m21a-slice：diffsym 文件级 def / 全改动行锚定 / 一跳（ref_worthy 迁入）；pack 改动中心切片（核 / 一跳 / 文件头 + 全部签名 / 折叠标记带行号与 def 名 / 禁真空 hunk 窗 / 小文件全文逐字节等同 full / 梯子接管）与文件外片段（调用点 ±30、签名 + doc、四类跳过、段帽 = 组预算 / 8、每名 K=3）；plan_cells 读 review.pack、--pack-only 报 PackStats；e2e 覆盖两档。code-review high 十条全修（hunk 重锚定、值 def 括号配平、名字优先级与测试排除、sink 有效命中、import 行非调用点、非支持语言头限顶、多行 import 组、二阶段 second_pass 档、凭据判据拆分、cap 早退与 omit 收敛）。本仓两档对比：slice 比 full 省 28% pack 字节 / 20% token。三道门：lib 524 / e2e 17 绿、clippy 7 = 基线、make verify 绿。待用户 --no-ff 合回 feat/m21-review-engine；followup 记切片措辞交 D-2、--pack-body 旗待裁、常数归 R7、N 组 N 次走仓、extra_paths 泄漏 PLAUSIBLE、over_budget 组切片仍大。
## Session 10: M21a 批 D 契约与提示词：疑点 parse + shared-head/scanner 模板 + 四块段序 + 三段界 cache + 评分器 + 评审修复
<!-- trellis-session: v=2 fp=f3e5b208a1f8f412 -->

**Date**: 2026-09-08
**Task**: M21a 批 D 契约与提示词：疑点 parse + shared-head/scanner 模板 + 四块段序 + 三段界 cache + 评分器 + 评审修复
**Branch**: `feat/m21a-prompts`

### Summary

批 D 五步交付：parse_scan_output 字段级规整（缺 suspicions 即违约走 REPAIR_PROMPT_SCAN）+ anchor_suspicions（quote 不在文件即丢、就近命中、不可读单列计数）；shared-head.md（字节稳定 system：规则 / 立场 / 证据政策 / CI 跳过 / 免疫段 / brief / spec 槽）+ scanner.md（E3 B 臂疑似即列 + 判例槽 + needs 措辞 + 与 serde 字段逐一对应的契约）；build_scan_prompt 四块（参考 → 证据 → diff → 尾）+ cache_boundary + 独立 SCAN 中和表（旧 prompt 逐字节不变）；ChatMessage::UserBlocks 三段界 cache_control（末块规则守上限 4）+ template_hashes 入 CellRecord；eval/scan_score.py（E3 口径，本机复算 B 臂基线 union 9 一致）。code-review 子 agent 1 必修 + 11 建议 + 10 nit 并入 ced42e7；两项记 followup（结果 JSON 无 scanner 输出面 → F；增量轮框架语无槽 → M21c）。三道门 509+16+7+16 绿。待办：主工作树 --no-ff 合回；批 E 另开 session。

### Git Commits

| Hash | Message |
|------|---------|
| `ef8773e` | feat(diffsym): 文件内一跳（改动 def 的同文件 callee / caller）——DefRange / all_defs / changed_defs 全改动行锚定，ref_worthy 迁入 |
| `9ff03c3` | feat(pack): 改动中心切片——核/一跳/文件地图/折叠/禁真空兜底，slice_min_bytes 小文件全文（design §10.2〔D8〕） |
| `c09636e` | feat(pack): 文件外片段——caller 调用点 ±N 与 callee 签名，受预算帽（design §10.2 文件外层） |
| `d374040` | feat(cli): [review.pack] body 接线——slice 默认、full 旋钮、--pack-only 报装包形态（PackStats） |
| `4512734` | docs(followup): 批 E 延后项——切片措辞交 D-2 / --pack-body 旗待裁 / 常数归 R7 / 文件外读守卫口径；关闭「over_budget 未消费」（K9 定论） |
| `e492334` | fix(pack): 评审修复（code-review high 十条）——hunk 重锚定、值 def 括号配平、调用点名字优先级与测试排除、sink 只计有效命中、import 行非调用点、非支持语言头限顶、多行 import 组、二阶段带文件外片段、凭据判据拆分、cap 早退与 omit 收敛 |
| `e9693e8` | docs(followup): 批 E 评审帽外项——N 组 N 次走仓 / 截断非确定、增量 extra_paths 泄漏（PLAUSIBLE）、over_budget 组切片仍大（预算门与装包口径待对齐） |
| `e827f15` | chore(task): m21a-slice 规划与实施制品（prd / design / implement / jsonl，含 code-review 审核修复记录与两档对比数字） |

### Status

[OK] **Completed**


## Session 12: M21a 批 E 第二轮复审：code-review high 九修一不改（feat/m21a-slice）
<!-- trellis-session: v=2 fp=dd82b7d1b225f1fb -->

**Date**: 2026-09-09
**Task**: M21a 批 E 第二轮复审：code-review high 九修一不改（feat/m21a-slice）
**Branch**: `feat/m21a-slice`

### Summary

用户要求再跑一轮 code-review 复审（只传 effort + SendMessage 钉范围，评审员开头即确认范围）。12 条送验证 → 前十条九条成立一次修掉：二阶段沿用用户 review.pack（full 对照臂不再被切成 slice）、FileCtx 一次读取 / 编译 / 重锚定供 0 壳梯子与文件外搜索共用、评分式重锚定（低信息行不定位、容忍个别行被改）、export def 不被 import 短路 + body_names 剔除关键字、$ 标识符手工边界（非消耗式）、签名名小写优先、item 级 #[cfg(test)] 区间、字符串感知括号计数、增强包文件不二次注入；不改：非支持语言 hunk 窗 ±30（plan 原文，prd K10/K16 已明示）。帽外记 followup：分组边与文件外键的「改动 def 名」两套口径。三道门：lib 534 / e2e 17 绿、clippy 7 = 基线、make verify 绿。待用户 --no-ff 合回 feat/m21-review-engine。

### Git Commits

| Hash | Message |
|------|---------|
| `687335d` | fix(pack): 复审修复（第二轮 code-review high 九条 + 帽外项）——二阶段沿用用户 review.pack、FileCtx 一次解析 / 一次重锚定共用、评分式重锚定、export def 不被短路、$ 标识符边界、签名名小写优先、item 级 cfg(test) 区间、字符串感知括号计数、增强包文件不二次注入 |
| `f84b5d7` | feat(parse): 疑点契约 parse 字段级对齐 + quote 不在文件即丢弃 + needs 帽（design §4〔D11〕） |
| `e360cdf` | feat(prompt): 模板拆分 shared-head.md + scanner.md，build_scan_prompt 四块段序 + 注入互斥与中和（design §10.3 / §10.7〔D6 D9〕） |
| `86521b2` | feat(provider): anthropic kind 三段界 cache_control + 模板哈希入 CellRecord（D10） |
| `cfae1ec` | chore(eval): scan_score.py——scanner 疑点对 oracle 的离线回归评分（模板改动门，design §10.7） |
| `ced42e7` | fix(prompt): 批 D 评审修复——user_focus 框架语、scan/legacy 中和表分离、骨架入哈希、行号 clamp、路径与 ranges 规范化、锚定分计数与就近命中、分块会话尾断点、评分器口径与边角 |

### Status

[OK] **Completed**


## Session 13: M21a 批 D / E 合并后核验与修补（feat/m21-review-engine）
<!-- trellis-session: v=2 fp=1b4cf2fccab4e14d -->

**Date**: 2026-09-09
**Task**: M21a 批 D / E 合并后核验与修补（feat/m21-review-engine）
**Branch**: `feat/m21-review-engine`

### Summary

用户要求 D / E 合入后再核验一遍。两个只读审计子 agent 分别对照 prd / design / plan 审批 D（parse / anchor / 模板 / 段序 / 三段界 / 哈希 / 评分器）与批 E（diffsym / pack / 文件外 / 接线），加本仓 --pack-only 冒烟与临时干净工作树受控对照。D 自身范围零必修；E 一条必修（文件外调用点 regex 交替最左优先，短名是长名前缀时长名调用点漏检，临时仓复现）。合并后接缝两处：证据段前言仍描述「0 壳全文 + 1 壳」（批 E 交 D-2，但 D-2 早已合入，悬空）、build_scan_prompt 对已包装 dossier / 带壳 spec 会双壳。全部一次修掉并补测试；followup 关闭两项、新增合并后核验延后表。

### Main Changes

- scan 路径独立卷宗前言 SCAN_DOSSIER_PREAMBLE（描述切片块头 / 折叠标记 / 梯子标注 / 文件外调用点与签名块 / 省略清单，入 scanner 模板哈希）；scanner.md needs 措辞覆盖折叠体 / 省略片段；旧 render_dossier_section 前言抽常量、输出逐字节不变（golden 哈希 525ddd85186e20e9）
- build_scan_prompt 剥掉 CellPlan.dossiers 外壳 + 旧前言、cli::spec 带壳段（unwrap_shell），裸值与带壳值逐字节相同；scan 中和表移出 bundle / incremental_round
- anthropic 分块显式断点运行时封顶（每会话至多 3 显式 + system，多余静默降级，断点被吞掉的末条分块仍拿尾断点）
- pack：文件外调用点 regex 交替按长度降序；名字截断提示只在真正走仓时写；段帽 = 组预算 / 8（design 口径，原 remaining / 8）；-U0 式尾部删除点并入 hunk 跨度；diffsym is_comment_line 不把 *g = f() 解引用行当注释
- parse：行号数字串 / 浮点也认；needs.kind 大小写宽容

### Git Commits

| Hash | Message |
|------|---------|
| `8622dbc` | fix(prompt,provider,pack,parse): 批 D / E 合并后核验修补——scan 卷宗前言改描述切片形态（切片块头 / 折叠标记 / 梯子标注 / 文件外调用点与签名块 / 省略清单，入 scanner 模板哈希）+ scanner 尾 needs 措辞覆盖折叠体与省略片段；build_scan_prompt 剥掉已包装的 CellPlan.dossiers 与 cli::spec 带壳段（不双壳、真闭合不被中和）；anthropic 显式断点运行时封顶（每会话至多 3 显式 + system，多余静默降级）；旧 build_bundle_prompt 字节级 golden 哈希；文件外调用点 regex 交替按长度降序（短名是长名前缀时长名调用点漏检）、名字截断提示只在真正走仓时写、段帽以组预算为基（design 口径）、-U0 式尾部删除点并入 hunk 跨度；`*g = f()` 解引用行不当注释；parse 行号数字串 / 浮点与 needs.kind 大小写宽容；scan 中和表移出 scan 路径不存在的 bundle / incremental |
| `2170de4` | docs(followup): 批 D / E 合并后核验——关闭「切片层次的提示词措辞」与「ScanOutput.suspicions 缺键」两项；新增合并后核验延后表（多行 quote 只核首行 / 中和 vs quote 锚定 / F 接线备忘（scanner 通路、template_hash 合成、--pack-only 口径）/ dropped_malformed 混计 / _oracle 报错形态 / Full 档脏树与 dev 差异 / FileCtx 二次解析 / bracket_delta_code 块注释 / anchor_offset PLAUSIBLE / 批 E 制品测试名 / 骨架级梯子无测试） |

### Testing

- [OK] cargo test：lib 552（+8 新测试）/ 16 / 7 / e2e 17 绿；clippy 7 = 基线；make verify 绿
- [OK] 受控对照（临时干净工作树、同 diff 4163c46..HEAD）：切片 / 折叠数字不变，outside_sites 4→4、outside_sigs 12→14；--pack-only 对 dev / 1b50db6 / 4163c46 三个基线均跑通

### Status

[OK] **Completed**

### Next Steps

- 批 F 硬切与并行（feat/m21a-cutover）；F 接线备忘见 followup「合并后核验」表：新开 scanner 通路（System(head) + UserBlocks → parse_scan_output_with_stats → REPAIR_PROMPT_SCAN → anchor_suspicions）、CellRecord.template_hash 合成、--pack-only 口径切到 scan prompt
- 延后项：多行 quote 只核首行（M21c 合并逻辑时定）、中和 vs quote 锚定（F 出口看 dropped 计数）、FileCtx 二次解析 / bracket_delta_code 块注释 / anchor_offset PLAUSIBLE（dogfood 观测）


## Session 14: M21a 批 F 硬切与并行：F-1…F-6 实施 + dogfood + code-review 十修（feat/m21a-cutover）
<!-- trellis-session: v=2 fp=abba01f26731ef4e -->

**Date**: 2026-09-09
**Task**: M21a 批 F 硬切与并行：F-1…F-6 实施 + dogfood + code-review 十修（feat/m21a-cutover）
**Branch**: `feat/m21a-cutover`

### Summary

用户「开批 F 硬切与并行」。建子分支 feat/m21a-cutover 与工作树、Trellis 任务 09-09-m21a-cutover；三个只读研究子 agent 出代码地图，写 prd（K1-K15）/ design / implement。F-1 产物改名 redteam-next（make build 不覆盖 dev、gitignore、packaging 断言）；F-2 scanner 通路（scan_shot 两轮单发、registry scan 表 + provider 在途门、thinking 映射表按 (kind, family, id)、reasoning_tokens、run_scan_fanout 组 × 条目 × samples、CellInput.scan）；F-3 疑点并集 agreement = 家族数 + ScanOnly 报告 + md；F-4 硬切：cli 四条路径收 ProviderRegistry、finish_scan_report / scanned_coverage、resume scan 重放、增量 bail、pack-only 切 scan prompt、删旧扇出 / 旧模板 / 旧尾段 / 二阶段构造器，e2e 与 cli 单测改 scan；F-5 观测单测；F-6 dogfood（3 组 × DeepSeek / Qwen，6 cell scan-only 366 s，57 疑点，覆盖 0.87，零告警）+ 评分器读取验证 + code-review high 十条全修（resume 旧作业覆盖、partial 丢失败格、gpt-4 误发 reasoning_effort、合并顺序依赖改并查集、resume 贫化收口共用 scan_cell_record、manifest 模板哈希、lease 先门后 worker、glm-4 Off、pinned 看映射、覆盖率分母）+ 帽外五项；followup / roadmap 打勾。三道门：lib 531 / 16 / 7 / e2e 17 绿、clippy 三条既有、make verify 绿。未 push；待用户 --no-ff 合回 feat/m21-review-engine。

### Main Changes

- F-1：[[bin]] redteam-next、CARGO_BIN_EXE_redteam-next、Makefile BIN/cp、.gitignore、packaging 断言、clap name/about
- F-2：provider/scan_shot.rs、registry scan 表 + InflightGate + lease、backends thinking_params(kind, family, id, level) + *_ext body + with_thinking + reasoning_tokens、orchestrate run_scan_fanout / scan_cell_record / ScanEntrySpec、CellInput.scan + template_hash、FakeChatBackend repeating/with_delay
- F-3：consolidate/scan.rs merge_suspicions（并查集）、Verdict::ScanOnly、ScanReport / MergedSuspicion、ReviewReport.{scan, cells}、RunMeta.grouping、md Scan 段
- F-4：cli scan 路径（run_code_review / finish_scan_report / scanned_coverage / run_resume / partial_report / run_pack_only）、增量 bail、删旧扇出与模板、--help 文案、main build_registry 只装 scan 条目、Phase::Scan、Arc<T>: ChatBackend
- F-5 / F-6：观测单测、dogfood、code-review 十修 + 五帽外、followup / roadmap 更新

### Git Commits

| Hash | Message |
|------|---------|
| `495c80e` | chore(task): m21a-cutover 规划制品（prd / design / implement / jsonl）——批 F 硬切与并行：K1-K15 定案（route_key = provider/id、registry scan 表 + 在途门、thinking 映射表、CellInput.scan、agreement = 家族数、ScanOnly 退出码 0、报告顶层 cells、模板哈希 head+tail、增量 bail、gitignore redteam-next、--pack-only 切 scan prompt） |
| `4335a23` | chore(build): 分支产物改名 redteam-next，make build 不覆盖插件 dev redteam（D10）——[[bin]] name / CARGO_BIN_EXE_redteam-next 六处 / Makefile BIN 与 cp 改名、删 chmod、加 build-dev-baseline 说明 / .gitignore 忽略 plugins/redteam/bin/**/redteam-next（防 5 MB 分支二进制误入公开仓）/ packaging 断言 commands 与 codex 镜像不出现 redteam-next / clap name·about·after_help 标「M21 重构分支产物」/ CLAUDE.md 常用节一句 |
| `eb0b3ad` | feat(orchestrate): scanner 扇出——组 × 条目 × samples，route_key 按角色条目，thinking_level 首个消费者，provider 在途锁（design §5 / §12.3）——provider/scan_shot.rs 两轮单发（System(共享头) + UserBlocks → parse_scan_output_with_stats → REPAIR_PROMPT_SCAN + turn_final → diagnostic_parse_error，usage / reasoning 跨轮累加）；registry 加 scan 表 + per-provider InflightGate（Semaphore + 在途峰值）+ warnings，同 provider 多条目共享一把门；backends thinking_params 映射表按 (kind, family, level)（deepseek / glm / kimi / gpt·o / gemini / qwen / claude，无映射 → 不发 + thin |
| `1cf6003` | feat(consolidate): 疑点并集与 agreement（家族数）+ scan-only 报告（design §5〔D7〕）——consolidate/scan.rs merge_suspicions：同组 ∧ 同文件 ∧ 行重叠 ±LINE_GAP ∧（去空白 quote 相等 ∨ quote_overlap ≥ QUOTE_SIM）→ 合并，agreement = 家族数（同家族多 samples 计一次，不复用 dedup::merge_into 的模型数口径），代表 = 高置信者、hint 全留不归一、cell_ids 追溯、跨组不合并、输出按 (group, file, line, hint) 排序；domain：Verdict::ScanOnly（"scan-only"）、MergedSuspicion、ScanGroup / ScanStats / ScanReport、ReviewReport.{scan, cells: Vec<CellRecord>}（评分器读顶层 cells[].output）、RunMeta.grouping、CellSummary. |
| `acbc161` | refactor(orchestrate): 硬切旧 fan-out / 候补链 / 单发 finding 路径——scanner 疑点路径唯一（铁律 6〔D10〕）——cli：run_code_review / run_review_dispatch / run_job / run_resume 收 ProviderRegistry，scan 路径 = plan_cells → run_scan_fanout → finish_scan_report（merge_suspicions → ScanReport / RunMeta coverage 五集合 · template_hashes · cache_hit_by_cell · provider_inflight_peak · grouping · cells 摘要与顶层 cells → 任一 cell 失败 Incomplete/3 否则 ScanOnly/0）；scanned_coverage（改动行 ∩ scanned_areas）；resume 用 manifest.scan 重放（旧格式作业记失败）；partial_r |
| `3af7048` | feat(state): scanner cell 落盘 + RunMeta 观测（分组 / 疑点数 / 覆盖率 / cache / 墙钟 / 在途峰值 / coverage 五集合）——观测填值随 F-4 的 finish_scan_report 落地（coverage selected / completed / reused / failed、template_hashes 两键、cache_hit_by_cell = cached ÷ input、provider_inflight_peak、grouping、每 cell 摘要含 cell / role / cached、needs triggered / declared、scanned_coverage、Phase::Scan、JobState.roles）；本 task 补定向单测 finish_scan_report_fills_observability_and_flips_incomplete_on_failed_cell（组骨架补齐、类别分布、agreement ≥ 2、usage 从记录重算、失败格 usage |
| `3411a0e` | fix(orchestrate,cli,provider): 评审修复（code-review high 十条 + 帽外五项）——resume 拒旧格式作业不覆盖原报告、旧非 scanner 记录翻失败格；partial 报告收全部记录（失败格进 coverage.failed / usage）；thinking_params 加 id：gpt-4 系无映射 + warning、GLM-5.x 不可关 / 4.x Off 真关；merge_suspicions 并查集传递闭包 + 输入按 cell id 序（六排列同结果）+ 琐碎 quote 只认相等；orchestrate::scan_cell_record 扇出 / resume 共用，resume 沿用原报告 groups / grouping / head_sha / dossier / scanned_coverage 并并入 registry warnings；CellInput.template_hash 随 manifest 落盘、重放优先取之（不一致 warning）；registry lease 先过 provid |
| `5e4856f` | docs(task,followup,roadmap): 批 F 收口——implement 收口记录（dogfood 6 cell scan-only 366 s / 57 疑点 / agreement≥2 六条 / 覆盖 0.87 / thinking 映射零告警）与评审修记录；followup 关闭 `--pack-only` system 段 / legacy 适配器 / GroupingStats 接线 / 结果 JSON scanner 输出面四项，新增批 F 延后表（resume 生产验证 / GLM anthropic 映射 / 增量与台账 M21c / 无消费者旗 / finding 形态字段恒空 / attempts 口径 / 保留旧件 / 大 diff 组体量 / 分种计数 / thinking+tools 守卫 / resolve 文案 / exit_code 第二参 / ScanResults 字段 / anchor 重读）；roadmap M21a 交付项 1-9 打勾、状态 🟢 机制完成待验收、README 同步；出口验收 1-10 另起 session |

### Testing

- [OK] cargo test lib 531 / 16 / 7 / e2e 17；clippy 三条既有；make verify 绿；make build 产 redteam-next 不覆盖 dev；dogfood scan-only 退出 0；scan_score.py selftest + 读 dogfood 报告

### Status

[OK] **Completed**

### Next Steps

- 用户 --no-ff 合回 feat/m21-review-engine；出口验收 1-10 另起 session（需 .next/private：PR181 单 / 双 scanner、E2 slice vs full A/B、cache 命中、--pack-only 误差、模板回归、dev 基线归档、schema v4 计数）
- M21b 前：GLM anthropic 形态 thinking 映射、resume 生产验证、GroupingStats 分种计数（followup 批 F 延后表）


## Session 15: D13 并发形态 grill 落文档 + Trellis 任务 d13-provider-gate 交付（provider 门唯一闸、429 排队重试、评审修 11 条、真跑核验）
<!-- trellis-session: v=2 fp=ebe634fe9e51eef7 -->

**Date**: 2026-09-10
**Task**: D13 并发形态 grill 落文档 + Trellis 任务 d13-provider-gate 交付（provider 门唯一闸、429 排队重试、评审修 11 条、真跑核验）
**Branch**: `feat/m21-review-engine`

### Summary

grill 定 D13（删角色 workers / 无 --workers、provider 门单把 FIFO 不预留、队列入口五态并入 + 跨轮沿用结案带守卫、scan 项不等兄弟 cell、maxConcurrency 缺省 10 + 429 排队重试）→ design v10 / D13 / roadmap 落文档；Trellis 任务：config 删三键 + 缺省 10 + >128 warning、扇出删角色信号量 + 排队告警按 provider、InflightGate 自管 + Notify + 429 排队重试 + RunMeta.provider_rate_limit；code-review 三批 16 条修 11（准入 + 退避不齐发、只成功算完成、429 按 HTTP 调用计、RAII、529、attempts / queue_wait_ms 口径、removed 键表、u32 饱和）延后 5 进 followup；真跑：缺省门宽零排队、门宽 1 排队告警 + queue_wait 173.5 s；门 lib 538 / e2e 17 / clippy 7 / verify 绿

### Git Commits

| Hash | Message |
|------|---------|
| `84dee09` | docs(design,decisions,roadmap): D13 并发形态 grill 落文档——design v10：删角色 workers / 无 --workers，唯一闸 = provider 在途门（单把 FIFO + scanner 先于 H0 入队序 + 429 排队重试，review.maxConcurrency 缺省 10、> 128 warning）、队列入口五态并入 + 跨轮沿用结案带守卫（锚定可重定位 ∧ 区域无 delta）、agreement 活值、scanner 逐 cell 交付 / reviewer 逐 finding 定性为持久化旁路、scan 项不等同组兄弟 cell（记派发时 / 落地时 agreement）；decisions 新增 D13（Q1-Q5 / Q-A / Q-B / Q4 候选与被否）+ D2 勘误；roadmap followup 关闭「scanners.workers 形态」行、R9 收窄，M21a 状态句、M21b 目标 / 交付 2-3-10 / 验收、M21c 判例前移改沿用结案带守卫、M21e 校准项、READM |
| `f5cb451` | chore(task): d13-provider-gate 规划制品（prd / design / implement / jsonl） |
| `641b4d3` | refactor(config,orchestrate): 删角色 workers 三键与扇出信号量——唯一闸 = provider 门，排队告警按 provider 计，review.maxConcurrency 缺省 10 + >128 warning（design v10 §12.3〔D13〕） |
| `f7bae23` | feat(provider): InflightGate 自管计数 + Notify，429 排队重试——门宽收敛到网关真实上限、持名额等本 provider 下一完成或 Retry-After、独立计数；RunMeta.provider_rate_limit（design v10 §12.3〔D13〕） |
| `3fe0f94` | docs(roadmap,task): D13 收口批落地记录——followup「scanners.workers 形态」行归位（641b4d3 / f7bae23）、implement 步 0-3 打勾；真跑核验（缺省门宽零排队；门宽 1 时排队告警 + g1 queue_wait 173.5 s + 峰值 1）记录只在 .next/private/d13/ |
| `4aabec3` | fix(provider,config,cli): D13 收口批评审修——429 等待者先退避再准入（active < 有效门宽才预占，门满回去等下一完成，不齐发）；只有成功完成才唤醒等待者（Timeout / 5xx 不算）；429 计数按每次 HTTP 调用独立；尝试括号 RAII（取消不泄漏在途）；Notify enable 先注册再检条件、seq0 与 others 同临界区；HTTP 529 归 RateLimited；attempts = 轮次 + 429 重试、queue_wait_ms = 门等待 + 429 排队；排队告警扇出 / resume 共用 registry::queue_warnings 且按有效门宽（已收窄不劝调大）；resume 记 queue_wait_ms；config 已删键统一 removed 表（risk / workers）；review.maxConcurrency usize→u32 饱和；删 run_scan_shot 壳、shrink_to 与收门共用 |
| `2c32703` | docs(design,roadmap,task): D13 收口批评审收口——design §12.3 评审修补记（准入 / 只成功算完成 / 529 / attempts 与 queue_wait_ms 口径）、followup 新表「D13 收口批评审延后项」（速率型 429 棘轮 / 时间预算放弃 / 策略非 Option / 503 / cap.max(1)）+ 行归位 4aabec3、prd K13-K20 与 AC 打勾、task design §5.1、implement §3-4 打勾 |

### Status

[OK] **Completed**


## Session 16: M21b 批 B reviewer 静态零件
<!-- trellis-session: v=2 fp=26cd72759aaf3e5b -->

**Date**: 2026-09-10
**Task**: M21b 批 B reviewer 静态零件
**Branch**: `feat/m21b-review-primitives`

### Summary

落地 RegionPack（构造即切片+半径+笔记）、reviewer.md+build_review_prompt、finding parse、IncrementalFindings SSE 旁路、三标配。三路 code-review 16 条必修已修（symlink 逃逸、REPAIR 转义、finalize、Drop guard、near 锚定）；dev redteam ship 因 config schema 错位未跑。--no-ff 合入 feat/m21-review-engine（27bf391）。

### Git Commits

| Hash | Message |
|------|---------|
| `081ab70` | feat(pack): reviewer 区域 pack——构造即切片+半径+跨组索引+spec+笔记，契约槽留空（design §7/§10.2） |
| `6f235b3` | feat(prompt): reviewer.md 角色尾 + build_review_prompt——共享头不变、段序同构、模板哈希（design §10.7〔D9〕） |
| `039a299` | feat(parse): reviewer finding 契约 parse——字段级对齐 + REPAIR 一轮（铁律 5） |
| `ce4ced4` | feat(provider): SSE 流式旁路——增量 JSON 解析、finding 对象闭合即落盘回调、未闭合丢弃计数（design §7〔D10〕） |
| `f515ed4` | feat(pack): reviewer 三标配——注入互斥+证据预算帽+读守卫（铁律 4，second_pass 机器复用） |
| `7b11f16` | docs(trellis): 批 B 工件勾选——B-1..B-5 完成态与验收记录 |
| `d1c2b61` | docs(followup): M21b 批 B 接口接缝——批 C 接线零件清单与 on_text_delta take 语义 |
| `4afa9a5` | fix(review): 批 B 评审修——REPAIR 句转义、截断 finalize、symlink 逃逸、Drop guard、near 锚定、索引双帽 |
| `fb45684` | chore(review): 批 B code-review 制品（4afa9a5）——0 阻塞 / 16 条合回前必修已修；followup 记批 C 接线与双 Origin |

### Status

[OK] **Completed**



## Session 18: M21b 批 A 风险评分器（+ 前置 #1 小分量并池）
<!-- trellis-session: v=2 fp=3d5c7c1794fda9ca -->

**Date**: 2026-09-10
**Task**: M21b 批 A 风险评分器（+ 前置 #1 小分量并池）
**Branch**: `feat/m21b-risk`

### Summary

前置 #1（用户裁选项 1）小分量并池：≤3 文件 ∧ est ≤ 预算/4 作原子单元与孤立文件同池（GroupingStats.pooled_small_components）；PR181 7→4 组、cross_import=0 保持。批 A：src/risk/ 移植 P0 v4（成区主/涟漪、四类信号常量权重、H0 θ=1.5 ∧ ≥2 类、R、预算三档 4/12/32 帽 48，零旋钮）；阶段 0 接线 RunMeta.risk + --pack-only 一行 + resume 沿用；redteam-next risk 只读回放子命令（读历史 commit 不依赖工作树）+ risk_replay.py compare（θ/R/Jaccard 三判定）；先修 Python 两处已核实缺陷（parse_diff 状态机守卫、e3 anchored 恒真式）。回放门：PR181 R Δ=3.4e-08 / H0 17/17 Jaccard 1.000；oracle B 七 diff 7/7 全绿（R Δ≤4.6e-07）；oracle 命中主裁决 4/4、并集 14/22 ≥ P0 基线；oracle B 导出 eval/fixtures/oracle-hobby.json。def 口径统一（bundle 改吃 all_defs/changed_defs）→ 分组实测变化记录在案（research §6 + followup）。收口 code-review（subagent lane 宿主故障 → bg_delegate 双镜片）：0 阻塞 / 8 重要全修（含 git argv --end-of-options、读守卫、台账只读）/ 4 建议 / 5 讨论；修后门 8 例复跑全绿。--no-ff 合回（eaf8bb5）。

### Git Commits

| Hash | Message |
|------|---------|
| `21de550` | feat(bundle): 小分量与孤立文件同池装箱——压组数（M21b 前置 #1，D14） |
| `69ff71d` | chore(task): m21b-risk 规划制品——prd 收敛 / design / implement / jsonl（批 A 脑暴收口） |
| `56f0397` | feat(risk): 成区——hunk→def 块、padded 兜底、主区/涟漪二梯队（design §3.3，P0 v4） |
| `9fc0bbc` | feat(risk): 四类信号（路径/结构/外部API/历史）+ 乘子 + 常量权重表（P0 v4 定案） |
| `c7b1b42` | feat(risk): H0 门槛（θ=1.5 且 ≥2 类）+ R 风险分 + 预算三档 4/12/32 帽 48（design §3.3/§6） |
| `b5d1ee1` | feat(cli): 阶段 0 风险路由接线——H0/R/tier/预算入 RunMeta 与 --pack-only（design §2） |
| `1f72af5` | feat(eval): risk compare 子命令 + redteam risk 只读子命令——Rust 实现对拍 Python v4 回归门（D11） |
| `4156796` | docs(risk): def 口径统一分组回归记录——PR181 groups 4→3/edges 111→152/28 文件大格（预期内变化，research §6） |
| `03f056c` | fix(risk,eval): 批 A 评审修 8 条——关键词注释跨行剥离（(?s)）、parse_hunks 状态机守卫、compare θ/Jaccard 判定、git argv --end-of-options×3、工作树读守卫、台账只读打开、标定注释勘误、risk 调用次序与打印精度 |
| `3d4cb01` | chore(review): 批 A 收口 code-review 制品（0f434df..4156796）——0 阻塞 / 8 重要（全修）/ 4 建议 / 5 讨论 |
| `840d8ac` | fix(test): 清理批 B 合并遗留的 2 处 unused import（Origin）——恢复 clippy 存量口径 |
| `be6b3b1` | docs(roadmap,plan): 批 A 台账口径勘误——PR181 不带台账，dev 旧 proj.db + --before 为 oracle B 侧口径（P0 §15.1） |

### Status

[OK] **Completed**

### Next Steps

- 批 C（m21b-deep）前提 A+B 已合回，可开工；m21b-risk 工作树/分支已合并可清理


## Session 19: M21b 批 C 深挖队列与 reviewer 运行时（核查 + 三路评审 + 真 provider dogfood 收口）
<!-- trellis-session: v=2 fp=0f53710994402f6c -->

**Date**: 2026-09-13
**Task**: M21b 批 C 深挖队列与 reviewer 运行时（核查 + 三路评审 + 真 provider dogfood 收口）
**Branch**: `feat/m21b-deep`

### Summary

批 C 实施 9 commit 后核查：C-1..C-6 落地但 C-7 未做、clippy +8、resume 空变更集 panic、错误路径漏 finalize（implement.md 曾把该测试项删掉后打勾）。修 4 条后跑三路 code-review 报 15 条：流式旁路生产从未接线（with_text_delta 只在测试里）、reviewer cell 不落盘（后台 attach_cells 丢格）、resume 丢 finding、空变更集+reviewer 假绿灯、观测快照定格在 scan 段——第 1/2 档修 11 条。第 3 档：thinking×tools 守卫按官方文档证伪撤销（真 400 源是 4.7+ 拒 type:enabled 与 thinking block 不回传，记 followup）；与维护者核对后拆 Finding 两根 agreement 轴（agreement=模型数，M21b 分派池不双审下基本恒 1；suspicion_agreement=区域疑点共识，批 D 免复核判据）。并发派发+多 reviewer 轮转另立批 C1。C-7 真 provider dogfood 两轮（base 674583e）：grok-4.6 exit 2/0 finding（needs 被注入互斥跳过）、glm-5.3 exit 10/13 finding（验收达成）；自评审修 3 条，其中两轴测试断言为空（删 max 行仍绿）是 code-review 漏掉的，已变异验证。一度误判 finish_reason 恒 None 意味流式空转——实为 CellRecord 无该字段，stream_discarded=2 反证旁路是活的，已纠正入记录。--no-ff 合回 eac0a60，主干四绿。

### Main Changes

- src/orchestrate/{queue,dispatch,review_shot,deep}.rs 新增：入口五态、优先级/预算/必挖/保底/drain、per-cell backend 流式旁路、旁路与终判同一锚定门、barrier
- cli：reduce 接线、带债 exit 2、scan-only 退化、resume 读快照（全 Settled 也收口）、观测在 overlay 刷新、--help 订正为深挖口径
- domain：Finding.suspicion_agreement 拆轴、CellOutput::findings_mut；state：Phase Deep/Verify + queue_items 快照
- provider：backend_from_config_with_delta；撤销 thinking×tools 守卫；订正 anthropic body「走不到」误判（模型级 kind 覆盖）
- docs：plan 立批 C1；followup 记批 C 核查留债/两根 agreement 轴/thinking 两处真债/dogfood 未修项

### Git Commits

| Hash | Message |
|------|---------|
| `c346617` | feat(orchestrate): 深挖队列项与入口五态——merge_suspicions 谓词复用、agreement 活值（design §6 规则 2〔D13〕） |
| `a2751ce` | feat(orchestrate): 优先级键+预算+必挖集不截+保底配额+graceful drain（design §6 规则 1/3/4〔D11〕） |
| `d79ff3c` | feat(orchestrate): reviewer cell 运行时——FindingSink no-op、旁路/终判按 id 去重、finalize（design §7） |
| `c08d229` | feat(orchestrate): 门驱动单派发——lease × pop 最高 Pending，空队列不占门（design §6 规则 5〔D13〕） |
| `5e06655` | feat(cli): stage0 保留 RiskRoute + DeepMeta 形状；dispatch 租约活过 cell（design §2/§9） |
| `89984ca` | feat(cli): barrier+reduce 接线——H0/疑点入队、门驱动深挖、findings 报告与带债 exit 2（design §2/§6） |
| `f1c355d` | feat(state): Phase Deep/Verify + queue_items 快照 API；resume 零行仍 scan-only（design §17 R14〔D11〕） |
| `0d74e2c` | feat(deep): Fake 429 压测+anthropic thinking×tools 守卫+item_wait_ms+快照接线与 resume 重跑（design §6/§7/§17） |
| `8b84c9c` | feat(deep): --tools 二阶段补查轮 + RunMeta.deep 观测收口 + 四绿（design §11 R2/§6 观测面） |
| `b93a7d9` | fix(deep,review_shot): 核查修 4 条——resume 空变更集早退、错误路径必调 finalize、resume 带回 spec 段、agreement 落地活值 |
| `8041bc7` | docs(followup,task): 批 C 核查账目——四绿口径勘误、留债三条、被降标的 finalize 测试项复位 |
| `936c209` | fix(deep,cli): 评审第 1 档——流式旁路生产接线、reviewer cell 落盘、resume 不丢 finding、假绿灯与观测快照 |
| `dff7f9f` | fix(deep,queue): 评审第 2 档——agreement 随请求进队列、等待时长派发时定格、attempts/usage 入账、守卫拒绝披露、on 档不弱于 two-stage |
| `674583e` | fix(provider): 撤销 thinking×tools 守卫——前提经查证不成立，真 400 源另有其二 |
| `db054bb` | refactor(domain): 拆开 Finding 的两根 agreement 轴——新增 suspicion_agreement，深挖不再代笔模型数 |
| `cdd6ea5` | docs(domain,followup): 订正 agreement 轴的口径——M21b 分派池下它已无产生机制 |
| `f1d3ca3` | docs(plan,roadmap): 新增 M21b 批 C1——门驱动并发派发与 reviewer 分派池轮转 |
| `86706e8` | fix(deep,review_shot): C-7 dogfood 自评审修 3 条——旁路发射过锚定门、疑点共识取值时点、两轴测试断言为空 |
| `cfcabf3` | docs(followup,plan): C-7 dogfood 账目——自评审未修项、C1-2 补事件侧滞后与分叉用例、thinking 债改当下时态 |
| `eac0a60` | merge(m21b-deep): 批 C 深挖队列与 reviewer 运行时合回——入口五态/优先级预算/barrier/Phase Deep+resume/观测 + 两轮评审修 + 真 provider dogfood（design §2/§6/§7/§8.4〔D11/D13〕） |

### Testing

- [OK] lib 651 / e2e 17 / packaging 7 / cli 17 全绿；clippy 7 = 基线；make verify 绿（子分支与合回后主干各复验一次）
- [OK] 变异验证 3 处：删 max 行 → 两轴测试红；去掉旁路锚定 → 报 [real,ghost]；旧 finalize 测试（探针实测 discarded==0）
- [OK] 真 provider dogfood 两轮，墙钟 18.3 / 50.7 min（批 C1 串行基线）

### Status

[OK] **Completed**

### Next Steps

- 开批 C1（feat/m21b-deep-concurrency）：C1-1 队列项身份与 spec 解耦 → C1-2 并发派发（先试 FuturesUnordered）→ C1-3 分派池轮转 → C1-4 墙钟门
- 批 D 可与 C1 并行；唯一碰撞点 run_deep 的 sink 接线；D 写跳过规则读 suspicion_agreement 不读 agreement
- m21b-deep 工作树已合并可清理（.next/private/m21b/ dogfood 记录需先拷回主干工作树）
