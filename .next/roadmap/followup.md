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
