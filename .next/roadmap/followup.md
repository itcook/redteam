# M21 · followup（残留归属 / 实现债 / 跨里程碑观测面）

> 只记 M21 内跨里程碑事项；零散 polish 走 `.doc/TODO/`；M21 外的 backlog 走 `.doc/roadmap/followup.md` M20 段。
> v2（2026-09-05，D7 后）：R1 撤销、R16-R20 新增、实现债更新。

## design §17 残留 → 归属

| R | 未决 | 归属 | 裁决方式 |
|---|---|---|---|
| R2 | reviewer cell 保留 `needs` 二阶段 | M21b | 触发率实测 |
| R3 | 分组阈值本仓校准 | M21a 起步 / M21e 收口 | ocr 默认 4 / 200 / 10 → dogfood |
| R4 | 疑点 schema 细节 | 部分已裁〔D11〕（category L0-L5、quote 逐字 + 丢弃）；hint 归一化 / 合并 id → M21c | 台账数据 |
| R5 | 风险规则初版 + 预算函数形状 | 已裁〔D11〕→ M21b 实现（规则 v4、涟漪二梯队、两类信号、常量化、三档 4 / 12 / 32） | `eval/risk_replay.py` 回放为回归门 |
| R6 | `--risk all` 与现状等价性 | 销案〔D11〕：无 `--risk`；对照 = dev 二进制并排跑 PR181 | — |
| R7 | slice 回退信号阈值（`needs` 触发率、oracle 命中差）与 `slice_min_bytes` | M21a A/B 起步 / M21d 调优 | PR181 A/B + dogfood |
| R8 | LLM 语义分组何时值得开 | M21e | 跨组 import 边数阈值 |
| R9 | 角色 `workers` 与 provider `max_concurrency` 默认值 | M21b 起步 / M21e 收口 | 网关限流实测 |
| R10 | L4-B 非 Rust 生态定位链 | M21d 后置 → M20 backlog | 按宿主语料 |
| R11 | 必挖集阈值 `c_hi` | 已裁〔D11〕：置信不做门；必挖 = 主区 ∪ agreement ≥ 2 + 疑点保底格 → M21b | — |
| R12 | 台账激活上限 N 与停车通道形态 | M21c 起步 / M21e 收口 | 镜像 ship AskUser |
| R13 | （已消：流式持久化无旋钮，D10） | — | — |
| R14 | 实现债（下表） | 已裁〔D11〕路线：role + 输出枚举、台账表 + 队列快照表 → M21a / M21b | 先机械拆零语义再接线 |
| R15 | findings-recall 度量 | P0 小 oracle（PR181 三方并集 + proj.db upheld）/ aacr 重启后接管 | — |
| R16 | provider RPM 令牌桶 | M20 | 网关按 RPM 计费时再上 |
| R17 | google 原生后端 | M20 provider 拓展 | 第二家族需要 Gemini 原生协议时提前 |
| R18 | diffsym 补语言（Python 等） | M20 / 语料需要时 | aacr 子集扩语言前置 |
| R19 | 交互式配置（verifiers 选项 Auto / New Model / Same As Scanner / Same As Reviewer；JSON 无注释、`serde_json` 原地改写即可，无需保注释库） | M20 | — |
| R20 | aacr `gate` 规模与 `--pack-only` spike | M21e 前 | 40 个候选 PR `--preview` + `--pack-only` 算真实输入分布 |
| R21 | 非结构性缓存优化（provider 专属 context caching、跨 run 预热、DeepSeek 磁盘缓存调参） | M20 | 结构性部分（段序、共享头、三段界断点）已在 M21a；看命中率数据再定 |

（R1「第二家族默认开/关」撤销：列表即活跃集，多模型即多条目，无开关。）

## 实现债（rpiv 实现级发现 + D7 连带，plan 阶段直接消费）

| 债 | 现状 | 偿还 |
|---|---|---|
| ThinkingLevel schema 零消费 | `src/config/mod.rs` 先行入 schema、无消费点 | M21a 角色条目 `thinking_level` 首个消费者 |
| `extract_imports` 只认相对 import | TS 裸模块 / Rust `use crate` / Go 包路径不解析 | ✅ M21a 批 C C-1（2026-09-08）：`bundle::import_edges` + `ImportResolver`（tsconfig baseUrl/paths/extends、go.mod module、crate 根） |
| `CellRecord.findings: Vec<Finding>` 是 proj.db 契约 | 疑点放不进 | M21a schema v4 一次重置（cell 带角色、suspicions、queue_snapshot、coverage、runs meta），旧库先导出 dismissed / upheld 固定集（D10） |
| resume = manifest 差集 + 存档 prompt 重放 | 不理解阶段转移与队列 | M21b 队列快照 checkpoint |
| per-fanout 单一模型计划 | `OrchestrateOpts` 单模型 + replicate | M21a 角色化（cell 类型 → 角色列表 → route_key） |
| `priority` 候补链 / `resolve_models` 四分支 / `--models` `--models-all` | 全链复跑与候补语义 | M21a 退场（D7 A3 / A9） |
| 项目层配置与剥离清单 `from_layers_tagged` | 威胁模型 B 的剥离逻辑 | M21a 整段删除（D7 A8） |
| `XDG_CONFIG_HOME` 测试串行化 hack | `dirs_config()` 路径 | M21a 换 `REDTEAM_HOME` |
| `model setup` / `write_secret` 写旧形状 | `[provider."model"] api_key` | 已偿（批 A 重做 9b817da〔D12〕）：`secrets.json` `providers.X.{apiKey, models.<id>}`；`model setup` 走 `SetupPatch` 三层显式合并 + 写前校验 |
| `config.json` 未知字段 | `shape_guard` 按已知层级预扫：snake_case / 旧键名（`name`）/ 非法角色 → 错误；其余未知键 → warning（评审 #4）；不开 `deny_unknown_fields`（保向前兼容） | 新增配置字段时同步 `shape_guard` 已知键表（单测兜底）；`serde_json` 键序为字典序（`preserve_order` 未开，手写顺序落盘后丢失，纯观感） |
| 批 A 首版 TOML → JSON 迁移脚本 | 只在维护者本机跑过一次（`/tmp/rt-migrate-config-json.py`，2026-09-08），不入库 | 无外部用户持有首版 TOML（分支未发布）；`legacy_config_paths_present` 对 `$REDTEAM_HOME/config.toml` 只提示不读即够，不做内置迁移 |
| `perTurnTimeoutSecs` 是否退役（用户 2026-09-08 提） | 模型层显式单轮超时；批 C 分组 + 批 D 切片落地后单 cell 输入有界，慢模型「结构性需要 1100s+/轮」的前提（2026-09-03 PR181 Kimi 实证）预期消失，缺省链 `REDTEAM_PER_TURN_SECS > maxOutputTokens÷20+60` 加上 HTTP 超时自动罩住（14b7289）应已够用 | M21a 出口 / M21b dogfood 记录每 cell 实际轮时与超时次数；若无模型再需要显式值，M21e 从 schema、`--per-turn-timeout-secs`、`shape_guard` 已知键表一并拆除（旧配置含该键走 warning 不报错） |
| `GroupingStats.cross_edges` / `weak_cut` 口径（A/B/C 合并核验 2026-09-08） | `cross_edges` 计全部边种（同目录分量一切即被 SameDir 边灌满），`weak_cut` 计超限分量内全部弱边（含留在同一强子分量内、实际未切的）；design §3.2 步 6 的 R8 触发度量是「跨组 **import** 边数」 | F-5 接 RunMeta 前分种计数（import / reference / samedir），`weak_cut` 只计真切的边 |
| Rust 仓 `lib.rs` / `main.rs` 超级枢纽 | `mod x;` 边是强边：改 `lib.rs` 一行 `pub mod x;` 即把所有改动顶层模块粘成不可切的强分量 → 单 cell `over_budget`；设计如此（import 强），对 PR181 规模（出口「≤ 5 组」）风险实在 | M21b 前用 `.next/private` 数据回放 `--pack-only` 看组数；超标则议 `mod x;` 边降为弱边或根文件特判 |
| C 精度缺口：TS 副作用导入 `import './x'`（无 `from`）不解析；弱边只认同父目录，design §3.2 写「同目录 / 同顶层模块」 | 索引与分组少边，不产错边 | M21e 清扫时按 dogfood 数据决定是否回补 |
| `--pack-only` 的 `est_tokens` 只计 user prompt，不含 system 段 | roadmap 出口「与真实 usage 差 < 20%」对账时会系统性偏低 | 出口验收对账时加回 system 段常量（或 F-5 直接记 usage） |
| D-1 接缝：`ScanOutput.suspicions` 无 `serde(default)`（`src/domain/suspicion.rs:42`） | 模型输出缺该键即解析失败 | D-1 parse 层决定：容忍缺键 / 走 REPAIR，不在 domain 改 |
| D / F 接缝：`CellOutput` tag 值 = `scanner` / `reviewer` / `verifier`（批 B 裁定 3：tag == `Role` 串 == `cells.role` 列），不是计划 B-2 文本的 `scan` / `review` / `verify` | 契约文档与 prompt 输出协议须按前者写 | D-2 写 `<output_contract>` 时对齐；`missing_role_tag_is_error_not_default` 可加 `"role":"scan"` 报错断言 |
| E-2 接缝：`Bundle.over_budget` 到 pack 层只是可见、未被消费；二阶段 `second_pass_prompt` 用 `Bundle::from_files` 重建 → `edges` / `over_budget` 清零、`hub_order` 只靠插入序 | 梯子（切片 / 全文 → def 体 → 骨架 → hunk 窗 → omit）纯按预算触发 | ✅ 批 E K9 定论（2026-09-08）：pack 不消费 `over_budget`——slice 已是缩形、标记无法再降，消费它会污染 full 对照臂；标记留 `--pack-only` 仪表。切片不依赖 `edges`；二阶段恒 `full`（K2） |
| `model setup` 写前预检对「原文件 JSON 合法但校验失败」的文案 | `load_with_messages` 失败 → `existing = default` → 之后报「合并后配置无效」，实际是原文件已无效；不丢数据（拒写），仅误导 | M21e 清扫：预检失败直接报原文件错误 |
| 并存期 secrets 提示 | dev 二进制的 `~/.config/redteam/secrets.toml` 长期存在，`secrets.json` 未创建前每次调用都打「密钥已改读」notice；只用 env 存 key 的用户永远看到 | M21c 切名再议（如只在 `model list / check` 打） |
| 批 A 重做评审子 agent 提出、未核实项（2026-09-08） | `eval/e3_contract_ab.py:315` `anchored` 疑似恒真式；`eval/risk_replay.py` parse_diff 疑似把列首 `--` / `++` 内容行当文件头；`provider_probe_kind` 对混协议网关取字典序首个模型的 kind（只影响 `model list --remote`）；`check_all` 串行探测、每次 `resolve_key` 重读 secrets（性能观感） | M21b 起用 `risk_replay.py` 作回归门前先核实前两条；其余 M21e 清扫 |
| FINALIZE / REPAIR 三层兜底 | 为一次性大 JSON 服务 | reviewer cell 输出有界后 M21e 评估简化；`finish_reason` 降为观测信号 |
| `consolidate/verify.rs` 后置 barrier 形态 | refute_targets 需全集 | M21b 改为流消费者，agreement 从队列项取 |
| `eval/harness.py` 只测 dossier 召回 | findings 层无 oracle | aacr 重启后由 fork 评估器接管；本仓不再自建 |
| `adversarial-code-bundle.md` / `adversarial-refute.md` 单模板 | 角色协议段在头部、三立场混一 | M21a 拆 `shared-head.md` + `scanner.md`；M21b 加 `reviewer.md` / `verifier.md`（D9） |
| M6-b 提示词 harness（M20 backlog） | 无角色级回归集 | 三角色离线回归集随 M21a / M21b 落地（verifier 用 proj.db dismissed vs upheld，零新数据）；M20 只剩通用 harness |
| `examples/dump_pack.rs`（P0 E3 dev-time 组包仪器） | 用 lib pub API 照抄三处 `pub(crate)` 辅助与 `NO_TOOLS_SYSTEM` 落盘单发 prompt；随 `cargo test` 编译 | ✅ M21a 批 C C-2 已删（2026-09-08，`Bundle` 形状变更后亦无法编译）；产品等价物 `redteam review --pack-only`（C-4）；`eval/risk_replay.py` 保留为 M21b 风险规则的离线回归器 |
| `redteam test-command` 子命令（M21a 批 A K1，过渡形态） | 项目层 `test_command` 随项目 config 退场后，推断链进二进制（Cargo / package.json / Makefile / go.mod）；`review --test-command` 记 RunMeta | ship 协议进二进制（M21c 切名后 ship.md 第 2 步改调本子命令；协议整体内置时评估把它拆卸 / 并入）——用户 2026-09-08 要求记下拆卸 |
| ship.md 第 2 步「测试命令探测」仍在 prompt 侧（且第 28 行仍引用已退场的项目层 `.redteam/config.toml` `test_command`）；第 13 行仍透传 `--models`（code-review #12） | 插件在 M21a-b 跑 dev 二进制，无 `test-command`；`--models` 已被 clap 拒收 | M21c 切名后改为调用 `${CLAUDE_PLUGIN_ROOT}/bin/redteam test-command`（退出码 2 → AskUser）并删 `--models` 透传，删 prompt 侧启发式与旧路径引用 |
| `--resume` 老作业按当前 `[[scanners]]` 重建 registry 与 RunMeta.models | 批 A 删 `--models` 后 args.json 里的模型集不再是权威源（批 A ship 评审 8fc07250 亦指出） | M21b 队列快照落地后 resume 读快照里的角色条目；此前只在 dogfood 观察，不修 |
| `legacy_opts_from_roles` 适配器（cli）+ `PlannedModel.priority` 恒 0 | 旧 fan-out 靠它跑到批 F | 批 F-4 硬切时删除 |
| `redteam-next` 独立库文件 `.redteam/proj-next.db`（批 B 评审裁定 2026-09-08：与 dev `proj.db` 分库，避免 v4 `cells.role NOT NULL` 让 dev checkpoint 写失败、旧库被 v4 守卫拒开） | `db::DB_FILE = "proj-next.db"`；`--help` / `load_ledger` / `init` 文案随之；dev 的 63 张判例卡对 next 不可见（M21a-b 不产 finding，无需） | M21c 切名时定切换程序：导出旧 `proj.db` 固定集 → 删 → `proj-next.db` 改名回 `proj.db`（或 `DB_FILE` 定回），`--help` 文案回改；`--dismissed` 指向 v0 `proj.db` 的只读复用届时一并议 |
| `SCHEMA_VERSION = "4"` 先于形状变化翻号 | 批 B 报告 JSON 与 v3 逐字节相同（五个 v4 字段全 default 不序列化），版本号已是 4（plan B-4 明文） | F-5 落 coverage / cache / 模板哈希值时不再翻号；若届时需区分「v4 未填」与「v4 无 coverage」再议 `4.1` |
| 台账固定集导出在仓内 python 脚本（`eval/export_ledger.py`） | 拒开文案指向它，插件分发不含 `eval/`；评审建议内置 `redteam ledger export`（CLAUDE.md 内置命令优先） | 用户 2026-09-08 裁定搁置（M21a-b 未发布、暂无导出需求）；发布前若保留拒开路径再评估内置 |
| 测试助手 `finding(..)` 三份（`consolidate/verify.rs` / `prompt/mod.rs` / `domain/suspicion.rs`）、`NeedEntry` 字面量十余处 | M21b 给 `Finding` / `NeedEntry` 加字段时要逐处补 | 抽 `#[cfg(test)] pub(crate) mod fixtures` 于 `domain/finding.rs`；`NeedEntry::evidence(..)` 构造器——M21b 加字段时顺手做 |
| 固定集 `ledger-upheld.jsonl` 3 行指向 gitignored `.doc/` 设计稿、数十行指向已改名历史路径 | quote 在现树锚不到；行已带 `head_sha`（部分作业无 stamp 为空） | D-5 / M21b 回归评分器按 `head_sha` 树锚定，空 `head_sha` 行按 `p0_oracle.py` 的 created_at 推断 |

## M21a 批 C 延后项（2026-09-08，Trellis `09-08-m21a-grouping`，prd K9 / K15 / R-C5.1）

| 项 | 现状 | 归属 |
|---|---|---|
| 增量轮两份清单 | `{{OTHER_CHANGED_FILES}}` 跨组索引已含全分支非 delta 文件（只带路径）+ `[delta]`，而 `<incremental_round>` 仍渲染 `two_layer_manifest` 双层清单（同信息两处，≤ 8K 字符） | D-3 段序翻转时合并：索引为唯一来源，`two_layer_manifest` 与其 E11-Q5 红测测试群随段重写迁移 |
| `--pack-only` 预算的模型窗来源 | ✅ 合入批 A 时已改读 `cfg.active_roles()`（与真实评审同一 `budget_and_spec`） | — |
| `GroupingStats` 只在 `--pack-only` 可见 | stdout + `summary.json`；真实评审的 RunMeta 未接线（避免撞批 B 的 `report.rs`） | F-5 观测接线 |
| `review.grouping` 未接配置 | ✅ 批 A 重做合入时接上（2026-09-08）：`cli::grouping_opts(cfg.review.grouping, budget)`，`plan_cells` 三个入口同源；`semantic`（R8）仍无消费点 | — |

## M21a 批 E 延后项（2026-09-08，Trellis `09-08-m21a-slice`，prd R-E4.6）

| 项 | 现状 | 归属 |
|---|---|---|
| 切片层次的提示词措辞：dossier 前言（`render_dossier_section`）仍描述「0 壳全文 + 1 壳引用点」；scanner 尾需说明「折叠体 `…（a-b 行折叠：name）` 只是签名可见、缺证据走 `needs{kind: evidence}`」与「文件外片段是调用点 ±N / 签名 + doc，非全文」 | pack 段序：`## 改动源文（0 壳）` → `## 文件外片段` → `## 已省略（预算裁剪）`；块头 `path:1-N（切片：核 c · 一跳 h · 折叠 f 段）` / `path:a-b（调用点：name）` / `path:a-b（签名：name）` | D-2 共享头 / scanner 尾（`src/prompt/*` 归批 D，E 不碰） |
| `--pack-body <full\|slice>` CLI 覆盖旗（镜像 `--tools`） | 批 E 不加（plan 范围）；A/B 走 `REDTEAM_HOME=<临时目录>` + `config.json` `{"review":{"pack":{"body":"full"}}}`，出口 3 双 scanner 配置本就要临时 config | 用户裁 / M21e |
| 切片常数：一跳 K=3 调用点 / 名、`anchorWindow` 30、文件外段帽 = 组预算 / 8、名字上限 40、走仓上限 20000 文件、文件头 ≤ 80 行、多行签名 ≤ 6、胶水 ≤ 3 | 常量在 `src/pack/mod.rs`；`anchorWindow` / `sliceMinBytes` 已是配置键 | R7（出口 3 A/B 回退信号后调优 / M21d） |
| 文件外搜索的读守卫口径 = `search_repo` 工具（ignore 走仓 + denylist，不逐文件 `git ls-files`）；路径非模型可控 | 未跟踪但未被 gitignore 的支持语言文件会被搜到调用点 | 威胁模型 B 若要求「只读 git-tracked」，一次 `git ls-files -z` 取集合再过滤（F-2 scanner 路径评估） |
| 文件外走仓每组一次（N 组 = N 次串行全仓扫描，每文件新建 Searcher）；`> 20000` 文件截断处非确定（walker 序） | 本仓 2 组 2.0 s；PR181 规模 ≤ 5 组可接受 | 大仓变慢再做「一次走仓、按组分发」（build_packs 层合并名集）；截断时改按路径排序后截 |
| 评审 PLAUSIBLE 未核实：增量路径 `extra_paths`（全分支非 delta 文件）是否会经文件外片段泄漏进 pack | 文件外搜索只排除 `changeset`（当轮 bundles 的文件），增量轮的非 delta 分支文件不在其中——会被当普通仓文件搜调用点（内容非密，但与跨组索引「只带路径」的口径不一致） | F-2 / 增量轮清单合并（D-3）时定口径：`changeset ∪ extra_paths` 或维持 |
| 切片默认后 `Bundle.over_budget` 组的 pack 仍可能很大（本仓 cell-2：6 文件切片 + 1 梯子 = 220 KB） | 切片只降无关体，改动密集文件（本批 pack/mod.rs 核 57）切片 ≈ 全文 | R7 / lib.rs 超级枢纽议题一并看：分组预算门用估算字节、装包用切片字节，两者口径待对齐（M21b 前用 PR181 回放） |

## 跨里程碑观测面（每个里程碑出口必报，写进 RunMeta 或 research）

墙钟（PR181 低 / 高风险各一）· findings oracle 命中数 · cache 命中率 · 疑点数与噪音比 · 预算截断数 · 台账落入 / 激活数 ·
每 cell 输出 token 与 output_max_tokens 比 · 复核触发 / 跳过 / refute 率 · per-provider 在途峰值 · 429 / 503 计数 ·
coverage `failed` 数。

## 明确不在本 roadmap

- D 层测试执行（M20 R6 执行判）。
- google 原生后端（R17）、RPM 令牌桶（R16）、交互式配置（R19）、候补 / failover（D7 A3 否决）。
- tree-sitter：不立项；重启条件 = > 2 个非三语言主语言、或 omit / 骨架质量可归因到 def 错配、或跨组索引需要可靠
  reference 提取。
- MEMORY_COMPRESSION / MaxToolRequestTimes / PLAN_TASK 独立阶段：不引入（rpiv 决策表，理由见 research §13）。
- LLM 兜底 re-location：有条件项，随 M21c 跨文件重定位落地后按 anchor 失败率再议。
- aacr-bench 的 vendor / submodule：不做，fork 承载（D7 B1）。
- `engine = legacy | v2` 旗：不做，分支硬切、对照 = dev 二进制（D10）。
- 新开项目 / 新 repo：不做，理由见 decisions D10。
