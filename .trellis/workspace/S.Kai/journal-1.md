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


## Session 20: M21b 批 C1 门驱动并发派发与 reviewer 分派池（身份 / 并发 / 轮转 / 墙钟门 + 真 provider dogfood + 三档评审修）
<!-- trellis-session: v=2 fp=cf7d71ea56efcc56 -->

**Date**: 2026-09-15
**Task**: M21b 批 C1 门驱动并发派发与 reviewer 分派池（身份 / 并发 / 轮转 / 墙钟门 + 真 provider dogfood + 三档评审修）
**Branch**: `feat/m21b-deep-concurrency`

### Summary

批 C1 把批 C 的串行单派发器改成 design §6 规则 5 的门驱动并发与 reviewer 分派池轮转。C1-1 先定死队列项与 cell 身份（id 不随 ranges 重算、共识只经谓词、Skipped 仅成必挖才复位、cell_id 按队列项），先红后绿修掉批 C resume 覆盖丢 finding，测试顺带暴露 resume/partial 收口把 reviewer 格当失败 scanner 格（假 Incomplete）并修。C1-2 FuturesUnordered + select! 并发派发、入队通道为 C2 预留、一格持门位到底、报告按派发序组装、二阶段失败计债；C1-3 分派池空闲轮转与鉴权出局；C1-4 虚拟时间墙钟门与退化零差（基线用 git archive 导出批 C 代码跑同一场景采得，不凭推理写期望）。dogfood 在临时工作树复现批 C 同一目标 diff：深挖段 48.4 → 7.1 min（agc 门 20，429 × 2 收门 20→7、grok 2 格断流）/ 16.1 min（门 4 全绿）。维护者追问两轮 findings 2 vs 8 的成因：逐项对照查实门宽不影响选项与分配，主因是 scanner confidence 跨轮漂移经排序截断决定挖哪些项，离线模拟显示单拿掉 confidence 稳不住（候选集跨轮只重合 39%），归为四层叠加并记为 C2 拷问议题，订正了先前「grok 占半是主因」的归因。/code-review 15 条修 12 条（第 1 档 4、第 2 档 4 + reviewer 格丢弃计数观测、第 3 档 4），3 条留 C2。--no-ff 合回 80919ad，主干四绿。

### Main Changes

- queue：id 定死、共识只经谓词（id 捷径保请求自身共识、agreement 单调不减）、MergedSkipped
- deep：FuturesUnordered + select! 并发派发、DeepInput.incoming 入队通道、run_cell 首轮 + 二阶段持同一 lease、派发序组装（meta 四数组对齐）、分派池轮转与鉴权出局 / 池空计债、二阶段失败按失败格落盘、reviewer cell warn 丢弃计数、restored_meta、review_cell_id
- registry：try_enter / try_lease（enter 复用 try_enter、lease 共用查找，scanner 行为与文案不变）；删 dispatch.rs 死代码
- cli：resume 对账双向以成功格为准 + 重挖失败保往轮载体 + 截断数三路径从快照起算 + re-digging 只在真重挖时披露；resume / partial 收口按角色拆记录；RunMeta.models 含实际参与 reviewer；--help 退出码 2 覆盖新债种
- backends / main：role_thinking_warnings（兼任 scanner 的 reviewer thinkingLevel 不再漏报，文案带角色）
- testdata/deep_serial_baseline.json（批 C 代码采得的退化零差基线）；followup 记 dogfood 账目、深挖选项方差归因与 C2 议题、评审留 C2 三条

### Git Commits

| Hash | Message |
|------|---------|
| `36ee52c` | chore(task): 批 C1 规划制品——prd / design / implement + 两处设计裁定（Skipped 仅成必挖才复位、事件滞后接受并钉 event ≤ landed） |
| `b615e31` | docs(task,plan,roadmap): 批 C1 拷问定案——十问结论并入 prd/design/implement，新增批 C2（scanner 与深挖时间重叠） |
| `aaa622d` | fix(queue,resume): 队列项与 cell 身份定死——id 不随 ranges 重算、共识只经谓词、Skipped 仅成必挖才复位、cell_id 按队列项且 Settled 以成功格为准（批 C1 前置，修批 C resume 覆盖丢 finding） |
| `e132618` | fix(resume,partial): 收口按角色拆 cell 记录——reviewer 格不再冒充失败的 scanner 格（批 C 遗留，C1-1 测试暴露） |
| `87e9773` | feat(orchestrate): 门驱动并发派发——FuturesUnordered + 入队通道、一格持门位到底、报告按派发序组装、二阶段失败计债（design §6 规则 5〔D13〕） |
| `83e0c2b` | feat(orchestrate): reviewer 分派池空闲轮转与成员健康——鉴权失败移出池不改派、池空停派保留 Pending、未分到格不再静默（design §6 规则 5、D7 A3） |
| `1d86e52` | test(deep): 并发墙钟门（虚拟时间）与单成员退化零差——对照批 C 串行基线 |
| `f01bf8b` | docs(followup,task): C1-5 dogfood 账目——墙钟门实证（深挖段 48.4 → 7.1 min）、网关超载两种表现、旁路跨重试不复位、reviewer 锚定丢弃不可见、grok 零产出与单文档疑点洪水 |
| `af97bcc` | docs(followup): 深挖选项方差归因——两轮 findings 2 vs 8 主因是 scanner confidence 漂移经排序截断决定挖哪些项（四层叠加 + 离线模拟 + C2 拷问议题），订正 grok 占半为放大因素 |
| `4e70292` | fix(queue): id 捷径合并保留入队请求自身的共识——多家族疑点撞同区间单家族项不再掉出必挖集（C1 评审第 1 档 #1） |
| `96b1ad5` | fix(deep,resume): 二阶段失败按失败格落盘、快照对账双向以成功格为准、重挖失败不抹往轮载体——resume 不再抹债、不再丢 finding（C1 评审第 1 档 #2 #4） |
| `d683bab` | fix(deep,resume): diff 重建不了时截断数不丢——深挖 meta 三条 resume 路径统一从快照起算（C1 评审第 1 档 #3） |
| `1f098a7` | fix(deep,resume,cli): C1 评审第 2 档——--help 与债文案覆盖新债种、re-digging 只在真重挖时披露、二阶段失败计轮次、reviewer 格披露丢弃计数、真实时间用例留足间距 |
| `e43dfe8` | refactor(deep,registry): C1 评审第 3 档——cell 名构造与反解共用、删 dispatch 死代码、Pool 在池状态单一真相、registry 取门与查找复用（行为不变） |
| `0a588c3` | docs(followup,task): 批 C1 收口账目——批 C 核查留债与 resume 覆盖 / 假共识标已解、评审留 C2 三条（复位不回减假债、外部放门不唤醒、单 task 同步 IO 停摆）、批 D 接线注意 |
| `80919ad` | merge(m21b-deep-concurrency): 批 C1 门驱动并发派发与 reviewer 分派池合回——身份与 resume 完整性 / 并发派发 + 入队通道 / 分派池轮转与成员健康 / 墙钟门与退化零差 + 真 provider dogfood + 评审三档修（design §6 规则 5〔D13〕、D7 A3） |

### Testing

- [OK] [OK] 子分支与合回后主干各复验：lib 683 / e2e 17 / cli 17 / packaging 7；clippy 7 = 基线；make verify 绿
- [OK] [OK] 新增用例先红后绿（一次就绿的补变异检查），关键改动变异检查 30 余处各自转红；真实时间用例全量并行连跑 3 遍无抖动
- [OK] [OK] 虚拟时间墙钟门 12 格 × 50 ms / 门宽和 4 = 150 ms；单成员门宽 1 与批 C 串行报告同构（findings / cells / meta / 落盘序 / 快照序）
- [OK] [OK] 真 provider dogfood 两轮（同批 C 目标 674583e...f1d3ca3），配置改动已从备份恢复并 cmp 校验

### Status

[OK] **Completed**

### Next Steps

- 批 C2 开工前 /grilling：scanner 与深挖时间重叠 + 在线预算，并入深挖选项方差（confidence 排序键与 reviewer 笔记、同文件相邻区间合并、文件多样性、scanner samples）与评审留 C2 三条（复位不回减假债、外部放门不唤醒、单 task 同步 IO）
- 维护者决定：providers.agc.maxConcurrency ≤ 7；grok-4.6 去留（reviewer 格丢弃计数已可观测，可再跑一轮看 warn）
- 批 D 接真 sink 前先修流式旁路跨瞬时重试不复位；D 的免复核读 FindingEvent.suspicion_agreement（派发快照，event ≤ landed）
- m21b-deep-concurrency 工作树已合并可清理（私有 dogfood 记录已拷回主干工作树）


## Session 21: M21b 批 C2 scanner 与深挖时间重叠（流式入队 / 在线预算与收场 drain / 等门 FIFO / resume 离线续跑 + 真 provider dogfood + 三档评审修）
<!-- trellis-session: v=2 fp=3992b214361d382b -->

**Date**: 2026-09-18
**Task**: M21b 批 C2 scanner 与深挖时间重叠（流式入队 / 在线预算与收场 drain / 等门 FIFO / resume 离线续跑 + 真 provider dogfood + 三档评审修）
**Branch**: `feat/m21b-deep-streaming`

### Summary

scan 全完才挖改成两段重叠：H0 t=0 开挖、scanner 格完成即入队、扫描期非必挖受 K − F 额度、关闭入口时收场 drain 一次；resume 离线续跑修掉扫描期 kill 后的 scan-only / exit 0；dogfood 同门宽重放省 28%（本轮墙钟慢于基线归因网关单次 429 把门收到 2）；评审 15 条三档全修（三处假绿 + 两处回归）

### Main Changes

- queue：到达键幂等入队（内容指纹）与受影响项回报、pop_dispatch_where / has_eligible、drain_with(预算, 保底) 与 drain_among(排除往轮在途)、并入保留成员疑点与最高置信代表、reopen_skipped
- deep：在线额度与收场 drain、等门挂起入 FIFO 并按轮转序登记、每次唤醒排空同刻事件再填门、收场按最终优先级序组装、快照只写受影响项、unfinished 统一判定、9 个时间重叠观测字段与阻塞计时覆盖
- registry：门票号 FIFO、首试让位、取消安全（WaiterGuard）
- cli / orchestrate：扫描与深挖同 task join（显式先 poll 扫描）、格落盘后逐条发疑点、收场切 Deep、F3 口径（scanner 失败深挖照跑仍 Incomplete）、拆簇对账、resume 队列头与幂等重建 H0、进度只数 scanner 格
- second_pass / region：读守卫查每轮预取的跟踪文件集，非 ASCII 路径回退子进程判定、取集合失败发 warning

### Git Commits

| Hash | Message |
|------|---------|
| `8adfcb5` | feat(queue,registry): C2 零件——到达键幂等入队与受影响项回报、按可派条件弹出、drain_with(预算, 保底)、门等门者计数与首试让位（取消安全）、组内闭包成员导出（D15） |
| `e0fed14` | perf(pack,deep): 读守卫改查每轮预取的 git 跟踪文件集——逐项子进程去掉，拒绝口径不变（D15 Q9） |
| `191f821` | feat(deep): 在线预算与收场 drain、等门挂起入 FIFO（修外部放门不唤醒）、收场按优先级序组装、快照只写受影响项——C2 深挖主循环（design §6 规则 3 / 6、§12.3〔D15〕） |
| `54ec215` | feat(cli,orchestrate): scanner 与深挖时间重叠接线——格完成即入队、显式先 poll 扫描、收场切 Deep、scanner 失败深挖照跑仍硬 Incomplete、拆簇对账（design §2 / §8.4〔D15〕） |
| `29338e8` | fix(resume): 离线续跑——补跑 scanner 后疑点幂等入队、补跑过则复位 Skipped 重选、空快照同路径重建 H0（修扫描期 kill 后 resume 出 scan-only / exit 0）（design §17 R14〔D15〕） |
| `489705a` | docs(followup,task): 批 C2 dogfood 账目——时间重叠端到端跑通、墙钟慢于基线归因网关收门到 2（同门宽重放 C2 省 28%）、收门只降不升留 D13 复议、同步 IO 读数远低于留债触发 |
| `3e4e39e` | fix(deep,resume,cli): C2 评审第 1 档——resume 落队列头与统一未完判定（修三处假绿）、diff 失败分支回写、主循环排空同刻事件、等门按轮转序登记 |
| `5e4fb00` | fix(queue,deep,cli): C2 评审第 2 档——并入保留成员疑点与最高置信代表、到达键带内容指纹、截断数按最终状态、阻塞计时与进度口径 |
| `f485e23` | docs,fix(second_pass,queue,deep): C2 评审第 3 档——非 ASCII 路径回退子进程读守卫、确定性保证补时刻快照式共识、恒真用例改写死期望、过期注释与 followup 记账 |
| `3038ee7` | merge(m21b-deep-streaming): 批 C2 scanner 与深挖时间重叠合回——流式入队与在线预算 / 等门 FIFO / 收场 drain 与优先级序组装 / resume 离线续跑 + 真 provider dogfood + 评审三档 15 条全修（design §2 / §6 / §7 / §17〔D15〕） |

### Testing

- [OK] 子分支与合回后主干各复验：lib 742 / e2e 17 / cli 17 / packaging 7；clippy 7 = 基线；make verify 绿
- [OK] 新增 / 改写用例 30 余条（虚拟时间钉到毫秒：H0 在 scanner 返回前落地、共门先后、等门跨事件保持排队位）；变异检查 37 处各自转红，3 处首轮假绿已补用例
- [OK] 真 provider dogfood 一轮（同目标 diff 674583e...f1d3ca3、同基线配置，跑前跑后 config sha256 一致）：12/12 零断流、exit 10

### Status

[OK] **Completed**

### Next Steps

- 批 C3 深挖选项稳定性：confidence 排序键与 reviewer 笔记、同文件相邻区间合并、文件多样性、scanner samples、scanner needs 入队；验收量选中区域与 findings 重合度
- D13 复议：收门只降不升——单次 429 把整轮钉在最低读数（agc 三天读数 7 / 4 / 2）；候选加性回升或窗口读数
- 批 D 接真 sink 前先修流式旁路跨瞬时重试不复位；确定性保证的除外项已含时刻快照式共识（dispatch / landed / suspicion_agreement）
- m21b-deep-streaming 工作树已合并可清理（私有 dogfood 记录与脚本已拷回主干工作树）


## Session 22: M21b 批 C3：深挖选项稳定性（结构定项 / 两轴共识 / 分层排序与保底一格 / needs 入队）+ 三档评审全修
<!-- trellis-session: v=2 fp=d9345593f72ffc5f -->

**Date**: 2026-09-21
**Task**: M21b 批 C3：深挖选项稳定性（结构定项 / 两轴共识 / 分层排序与保底一格 / needs 入队）+ 三档评审全修
**Branch**: `feat/m21b-deep-stability`

### Summary

项身份从报告行改为 enclosing def / 标题节，共识拆必挖轴与排序轴，分层排序键接风险分并给每文件保底一格，笔记去自评置信，scanner needs 首次接线；同 diff 三轮验收主门槛中位 41%（改前 25%）、单文件最大占格 ≤ 17%、findings 17/21/16，代码格占比差 1.9 点未达并定案认下；/code-review 15 条分三档全修 + 补跑验证轮

### Main Changes

批 C3 把深挖选项的身份从「报告行」改成「代码结构」：疑点落在哪个 enclosing def（文档落在哪个标题节，其余退 21 行固定带）就成那一项，键取声明 / 标题行的去空白哈希加同文序号，位移与缩进都不改身份；区间即块，id 随之。共识拆两轴——项内按 same_suspicion 聚簇取最大簇家族数为 peak_agreement（定必挖，取代 agreement ≥ 2，避免必挖集从每轮 4 条撑到 7-9 块），区域级 agreement 降为排序轴。排序改分层键（H0 → 每文件保底一格 → 种类 → peak → 风险档 → 烟量 → 代码优先 → 置信末位裂解 → id），删掉退化成「按 confidence 排」的乘积式；reviewer 笔记去掉自评置信（design §7 的防锚定初衷）；scanner needs 作为第三种生产者首次接线，只接变更集内带行锚的，diff 外按到达键去重计数；DeepMeta 增选项五量。

改后三轮 + samples=2 测量轮（同 diff 674583e...f1d3ca3、门 6、配置指纹跑前跑后一致）：主门槛选中项逐项重合度三对 50/41/41%、中位 41% ≥ 40%（改前 25%），辅助门槛单文件最大占格 25/17/17% ≤ 1/3（改前 46/50%），护栏格内 findings 17/21/16 ≥ 11。第四量代码格占比 58/67/75%、均值 66.7% 差门槛 1.9 点——未达，定案「认这个交换」并记账：它要保护的量反向而行（代码格产出 9/12 → 11/11/11、代码格数 7/10 → 7/8/9），掉的只是占比因为文档格从 0.63 涨到 1.75 条/格；被否的两条处置是 needs 只接代码文件与文档项再降一档；阈值不回改，下批换绝对量且先于跑数定。samples=2 买到候选覆盖翻倍（50 个位置 ≈ 两轮并集）与更高的跨轮稳定性（50/50/60%），代价是 scanner token 翻倍、墙钟只 +2~3 min，产品化留 M21c/M21e。

/code-review（max effort）15 条 + 10 条超帽项，分三档全修。第 1 档十条是结构化改造自己带出的真错：块键只哈希声明行 → 同文件两条逐字相同的声明碰撞成一项；行带被当结构身份 → 非 rs/go/ts 语言里跨带的同一条疑点从「一项共识 2」裂成「两项各 1」双双掉出必挖集；采用块区间后又并入到达的报告行 → 区间随到达序变；旧项就地升级 → id 与 ranges 各说各话；保底计数用区域轴而必挖用项内轴 → 保底永不消耗、派发超出 maxCells；旧快照必挖项收到新佐证反被降级；scan 与 needs 分属两身份空间 → 同块挖两遍；保底排在 ItemKind 之上 → needs 插到 H0 之前；同 id 两项共存 → 快照 upsert 互相覆盖；大行号让行带加法溢出。第 2 档七条是口径与浪费（笔记与块区间封顶、needs 的 lines 收敛到 second_pass 那一份、路径归一、外部计数去重、文档分类复用阶段 0 的 is_doc_path、风险档最高档只给进了 H0 的区、defs 整轮缓存）。第 3 档清死常量、resume 轮观测从快照重建、风险档随 route 重算，并更正两处记录：queue_split_clusters 的成因已从「到达非传递残差」变成「闭包簇跨块」，D16 原记的 pick_bundle 错配包理由经查证不成立（region::build 只出一行省略说明）。20 处变异逐条转红，其中四处首轮假绿由夹具与重复谓词导致，已修正。

修复动的是选项核心语义，故补一轮验证（同口径）：格内 findings 14、单文件最大占格 17%、代码格占比 75%，三处修复在数据里直接可见——最长块恰 200 行、needs 只剩一条单独成项且仍是最高产的格、needs_out_of_diff 从 9/3/7 降到 5；跨二进制选中项中位 41% 与修前持平（不作验收口径）。墙钟 13.4 min = max(浅扫 4.5, 深挖排空 13.4)，浅扫整段被深挖盖住；尾段变长源于单格变大（reviewer 输出 192k vs 基线 146k），属 §10 风险 2 认下的交换。


### Git Commits

| Hash | Message |
|------|---------|
| `8fbeb64` | chore(task): 批 C3 开工——上下文清单与任务置 in_progress（分支 feat/m21b-deep-stability） |
| `17cc832` | feat(queue,orchestrate): 稳定块与结构化合并——项身份锚到 enclosing def / 文档标题节，共识拆必挖轴与排序轴（design C3 §2 / §3〔D16〕） |
| `023b5c5` | feat(queue,pack): 分层排序键接风险分、每文件保底一格、笔记去 confidence——选项不再由自评置信主导（design C3 §4 / §6〔D16〕） |
| `c81aa67` | feat(orchestrate,deep): scanner needs 首次入队（仅变更集内）与选项观测五量——design §6 第三种生产者接线（D16 Q8 / Q11） |
| `9d45a9c` | docs(design,decisions): design v12〔D16〕——§6 规则 1 / 2 / 3 与可观测面、§7 按批 C3 定案改写，新增 D16 决策节 |
| `56649a7` | docs(plan,roadmap,followup): 批 C3 验收账目——三量判定（主门槛中位 41% 过、代码格占比差 1.9 点未达）与 plan / roadmap 同步 |
| `4a12bb5` | docs(followup): 批 C3 测量轮与护栏口径两条——samples=2 买覆盖也买稳定性；代码格占比这个代理量与被代理量反向 |
| `5e25424` | docs(decisions,followup,task): 代码格占比护栏未达——定案「认这个交换」，阈值不回改、下批换绝对量 |
| `8878434` | fix(queue,orchestrate,deep): 评审第 1 档十条——块键碰撞、区间被到达撑大、旧项升级错位、保底轴不一致、必挖轴被降级、scan 与 needs 重挖、行带拆共识、H0 让位、同 id 共存、行号溢出 |
| `a75d8ee` | fix(queue,pack,orchestrate): 评审第 2 档七条——笔记封顶并披露、块区间封顶、needs 行锚口径归一、变更集外去重计数、文档分类归一、风险档接 H0、defs 整轮缓存 |
| `cece0eb` | fix(queue,deep,docs): 评审第 3 档——清死常量、resume 轮观测从快照重建、风险档随 route 重算，拆簇指标成因与 D16 一处失实理由更正 |
| `bbc4fa1` | docs(followup): 批 C3 修复后验证轮——三处修复在数据里可见，选中项中位 41% 与修前持平 |

### Status

[OK] **Completed**


## Session 23: M21b 批 D：verifier 流式复核接线、离线基线裁定与收口评审
<!-- trellis-session: v=2 fp=7d2bd451550b884c -->

**Date**: 2026-09-21
**Task**: M21b 批 D：verifier 流式复核接线、离线基线裁定与收口评审
**Branch**: `feat/m21b-verifier`

### Summary

批 D 五步全做完并收口：D-1 旁路跨重试复位 + verifier.md 角色尾；D-2 触发集/跳过/异族选择 + 盖章收紧到 peak 簇行范围；D-3 复核挂进深挖主循环（第二个 FuturesUnordered、barrier 等复核归零、verify_tail_ms、逐条 verifier 格、resume 不重验、判决合并 UPHELD 优先、refuted 剔计数）；D-4 verify-once 隐藏子命令 + eval/verify_score.py，两臂离线基线按预登记阈值裁定 verifiers.enabled 缺省 false，顺带修 build_registry 漏注册 verifier；D-5 dogfood（复核四量自洽、深挖零行为 diff）+ /code-review max 15 条分三档（1/2 档全修、12 项变异转红）+ 因模板未 render 重跑两臂（裁定不变、撤回异族附带结论）。design v13 §8.1 重写、decisions 增 D17。未合回。

### Git Commits

| Hash | Message |
|------|---------|
| `369517a` | fix(provider,prompt): 流式旁路跨重试复位 + verifier.md 角色尾拆分——analysis 先于 verdict（design §8.1 / §10.7〔D17〕） |
| `8517784` | feat(consolidate,orchestrate): verifier 触发集与跳过规则——区域共识盖章收紧到 peak 簇行范围、异族选择（design §8.1〔D17〕） |
| `fd04420` | feat(orchestrate,cli): 复核挂进深挖主循环——逐条一格落盘、barrier 等归零、resume 不重验（design §8.1 / §2〔D17〕） |
| `7f0f567` | test(consolidate,domain): 判决合并三组合与计数剔除补测（design §8.1〔D17〕Q9 / Q11） |
| `6821535` | chore(eval,config): verifier 离线基线两臂——按预登记阈值裁定 verifiers.enabled 缺省 false（design §8.1「去留由数据决定」〔D17〕） |
| `16f2d72` | docs(design,decisions): D17 决策节 + design v13 §8.1/§2/可观测面改写；refute_rate 接上唯一生产者 |
| `5647a81` | fix(orchestrate): 本轮按 finding id 缓存复核——同一条被两个格各报一次时只验一次（design §8.1〔D17〕Q11） |
| `de0cd73` | fix(orchestrate): 缓存判定前移到分类之前——四个复核计数同分母（design §8.1〔D17〕） |
| `4f9cd0e` | chore(task): 批 D dogfood 记账——复核四量自洽、深挖路径零行为 diff |
| `0290798` | fix(prompt,orchestrate,cli): 收口评审第 1 档七条——模板未 render / 跨文件盖章 / 复核读守卫 / 失败复核固化 / 阈值脱钩 / 早退漏回填 / 时序决定验不验 |
| `48073b0` | fix(orchestrate,cli,output): 收口评审第 2 档六条——盖章单一出口、判决归零、同模型复核告警、verifier 不拒跑、md 头条对账 |
| `cefc2f2` | docs(research,decisions,config): 修好 prompt 后重跑两臂——裁定不变，撤回「异族两根轴都更优」 |

### Status

[OK] **Completed**


## Session 24: M21b 出口验收收口：解析宽容 + 网关退避 + 三轮验收 + D18 裁决
<!-- trellis-session: v=2 fp=38973f13659aaa23 -->

**Date**: 2026-09-23
**Task**: M21b 出口验收收口：解析宽容 + 网关退避 + 三轮验收 + D18 裁决
**Branch**: `feat/m21-review-engine`

### Summary

金丝雀暴露两处真因并修掉：glm 把 Go 源码 tab 原样写进 quote 致严格 JSON 失败（串内控制字符宽容重试，终态与旁路同源），网关成片 503 打掉 11 格（瞬断退避 2→30s、预算 4、超时封顶 2）；诊断常开带 serde 根因 + REDTEAM_DUMP_DIR 原文落盘（dev 仪器，发布前拆）。定稿双 reviewer 配置（deepseek 加入 reviewer，墙钟 25.3→12.9 min、失败格清零）后跑三轮验收：机械面全过（失败格/截断/解析失败 0；kill→resume 复用不重挖），覆盖面并集 6 主题未达 8。D11 预算挂账结案：maxCells 64 使截断 25→2 但主题覆盖 6→5、墙钟+53%/token+66% → 维持 auto。scan 墙钟真因是慢家族的恒定输出（与组大小无关）→ 撤销 M21d 交付项 7 的墙钟前提。

### Git Commits

| Hash | Message |
|------|---------|
| `2bd3ab7` | feat(provider): 解析失败诊断带 serde 根因 + REDTEAM_DUMP_DIR 原文落盘（出口验收金丝雀定案用） |
| `122fd9b` | merge(m21b-exit): 解析失败诊断带根因 + 原文落盘仪器（金丝雀定案前置） |
| `d2a08b1` | fix(provider): 串内裸控制字符宽容重试 + 网关瞬断长退避（出口验收金丝雀两处定案） |
| `0849ee7` | merge(m21b-exit): 串内控制字符宽容 + 网关瞬断长退避（金丝雀重跑前置） |
| `ddf9e45` | docs(roadmap): scan 墙钟真因是慢家族的恒定输出——拆大组的墙钟前提作废 |
| `ed83cb7` | docs(followup): resume 续跑后复核与墙钟分段账目全空（cell 记录尚在、未重建） |
| `9912f64` | docs(followup): 订正 resume 账目条目——不是全丢，是复用格不入账（两次续跑对照） |
| `e54d916` | docs(decisions,roadmap): D18 M21b 出口裁决——硬门改按意图、主题门槛判并集、D11 预算回校结案 |
| `ae532e2` | docs(task): M21b 出口验收结果总表（十一项判定 + 产物索引） |

### Status

[OK] **Completed**
