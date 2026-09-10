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
| R9 | provider `maxConcurrency` 缺省值（角色 `workers` 已删〔D13〕） | 缺省 10 起步（M21a 收口批）/ M21e 收口 | 429 排队重试的收敛门宽 + 网关限流实测 |
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
| `GroupingStats.cross_edges` / `weak_cut` 口径（A/B/C 合并核验 2026-09-08） | `cross_edges` 计全部边种（同目录分量一切即被 SameDir 边灌满），`weak_cut` 计超限分量内全部弱边（含留在同一强子分量内、实际未切的）；design §3.2 步 6 的 R8 触发度量是「跨组 **import** 边数」 | ✅ 已偿（2026-09-10，Trellis `09-10-grouping-stats-kinds`）：`cross_edges` 分种 `cross_import` / `cross_reference` / `cross_same_dir`（serde default）、`weak_cut` 删除（真切口径 = cross_reference + cross_same_dir 无独立信息量）；「强边不跨组」为不变量哨兵测试；PR181 基线 cross_import=0 / cross_reference=2 / cross_same_dir=6（旧 cross_edges=8、weak_cut=55 虚高） |
| Rust 仓 `lib.rs` / `main.rs` 超级枢纽 | `mod x;` 边是强边：改 `lib.rs` 一行 `pub mod x;` 即把所有改动顶层模块粘成不可切的强分量 → 单 cell `over_budget`；设计如此（import 强），对 PR181 规模（出口「≤ 5 组」）风险实在 | M21b 前用 `.next/private` 数据回放 `--pack-only` 看组数；超标则议 `mod x;` 边降为弱边或根文件特判 |
| C 精度缺口：TS 副作用导入 `import './x'`（无 `from`）不解析；弱边只认同父目录，design §3.2 写「同目录 / 同顶层模块」 | 索引与分组少边，不产错边 | M21e 清扫时按 dogfood 数据决定是否回补 |
| `--pack-only` 的 `est_tokens` 只计 user prompt，不含 system 段 | roadmap 出口「与真实 usage 差 < 20%」对账时会系统性偏低 | ✅ 批 F-4 K15（2026-09-09）：`bytes` = 共享头 system + 四块 user，与真实 scanner 请求同一 `build_scan_prompt`；`cell-n.md` 落盘同形 |
| D-1 接缝：`ScanOutput.suspicions` 无 `serde(default)`（`src/domain/suspicion.rs:42`） | 模型输出缺该键即解析失败 | ✅ D-1 定论（2026-09-08）：缺键 = 违约 → `Parse` 错走 `REPAIR_PROMPT_SCAN`（契约段明写「A response without the "suspicions" key is invalid」），domain 不改 |
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
| `legacy_opts_from_roles` 适配器（cli）+ `PlannedModel.priority` 恒 0 | 旧 fan-out 靠它跑到批 F | ✅ 批 F-4（2026-09-09）已删，连同 `run_code_fanout` / 候补链 / `TailCtx` / 旧模板 |
| `redteam-next` 独立库文件 `.redteam/proj-next.db`（批 B 评审裁定 2026-09-08：与 dev `proj.db` 分库，避免 v4 `cells.role NOT NULL` 让 dev checkpoint 写失败、旧库被 v4 守卫拒开） | `db::DB_FILE = "proj-next.db"`；`--help` / `load_ledger` / `init` 文案随之；dev 的 63 张判例卡对 next 不可见（M21a-b 不产 finding，无需） | M21c 切名时定切换程序：导出旧 `proj.db` 固定集 → 删 → `proj-next.db` 改名回 `proj.db`（或 `DB_FILE` 定回），`--help` 文案回改；`--dismissed` 指向 v0 `proj.db` 的只读复用届时一并议 |
| `SCHEMA_VERSION = "4"` 先于形状变化翻号 | 批 B 报告 JSON 与 v3 逐字节相同（五个 v4 字段全 default 不序列化），版本号已是 4（plan B-4 明文） | F-5 落 coverage / cache / 模板哈希值时不再翻号；若届时需区分「v4 未填」与「v4 无 coverage」再议 `4.1` |
| 台账固定集导出在仓内 python 脚本（`eval/export_ledger.py`） | 拒开文案指向它，插件分发不含 `eval/`；评审建议内置 `redteam ledger export`（CLAUDE.md 内置命令优先） | 用户 2026-09-08 裁定搁置（M21a-b 未发布、暂无导出需求）；发布前若保留拒开路径再评估内置 |
| 测试助手 `finding(..)` 三份（`consolidate/verify.rs` / `prompt/mod.rs` / `domain/suspicion.rs`）、`NeedEntry` 字面量十余处 | M21b 给 `Finding` / `NeedEntry` 加字段时要逐处补 | 抽 `#[cfg(test)] pub(crate) mod fixtures` 于 `domain/finding.rs`；`NeedEntry::evidence(..)` 构造器——M21b 加字段时顺手做 |
| 固定集 `ledger-upheld.jsonl` 3 行指向 gitignored `.doc/` 设计稿、数十行指向已改名历史路径 | quote 在现树锚不到；行已带 `head_sha`（部分作业无 stamp 为空） | D-5 / M21b 回归评分器按 `head_sha` 树锚定，空 `head_sha` 行按 `p0_oracle.py` 的 created_at 推断 |

## M21a 批 C 延后项（2026-09-08，Trellis `09-08-m21a-grouping`，prd K9 / K15 / R-C5.1）

| 项 | 现状 | 归属 |
|---|---|---|
| 增量轮两份清单 | 旧 prompt：`{{OTHER_CHANGED_FILES}}` 跨组索引已含全分支非 delta 文件 + `[delta]`，`<incremental_round>` 仍渲染 `two_layer_manifest` 双层清单（同信息两处，≤ 8K 字符）。新 `build_scan_prompt`（批 D-3，2026-09-08）只保住 `[delta]`（跨组索引槽），增量轮框架语（delta base 一句 + 上轮 claimed/resolved 顺带验证清单）**无槽** | 旧 prompt 与 `two_layer_manifest`、E11-Q5 红测群随 F-4 整体删；增量轮框架语 + 顺带验证清单在 M21c 台账回流时给 scan prompt 加槽（参考块内，需中和） |
| `review --format json` 无 scanner 输出面 | `run.cells` 是 CellSummary（无 output）；`eval/scan_score.py` 的 `cells[].output.suspicions` 输入形态今天没有产品面产出 | ✅ 批 F-3 / F-4（2026-09-09）：报告顶层 `cells: Vec<CellRecord>`（含 `output`，tag `role`），前台 / job / resume 三路都填；评分器零改 |
| `--pack-only` 预算的模型窗来源 | ✅ 合入批 A 时已改读 `cfg.active_roles()`（与真实评审同一 `budget_and_spec`） | — |
| `GroupingStats` 只在 `--pack-only` 可见 | stdout + `summary.json`；真实评审的 RunMeta 未接线（避免撞批 B 的 `report.rs`） | ✅ 批 F-5（2026-09-09）`RunMeta.grouping`；分种计数（:49）仍待 |
| `review.grouping` 未接配置 | ✅ 批 A 重做合入时接上（2026-09-08）：`cli::grouping_opts(cfg.review.grouping, budget)`，`plan_cells` 三个入口同源；`semantic`（R8）仍无消费点 | — |

## M21a 批 E 延后项（2026-09-08，Trellis `09-08-m21a-slice`，prd R-E4.6）

| 项 | 现状 | 归属 |
|---|---|---|
| 切片层次的提示词措辞：dossier 前言（`render_dossier_section`）仍描述「0 壳全文 + 1 壳引用点」；scanner 尾需说明「折叠体 `…（a-b 行折叠：name）` 只是签名可见、缺证据走 `needs{kind: evidence}`」与「文件外片段是调用点 ±N / 签名 + doc，非全文」 | ✅ 2026-09-09 合并后核验修补：scan 路径独立前言 `SCAN_DOSSIER_PREAMBLE`（描述全文 / 切片块头、折叠标记、梯子标注、`## 文件外片段` 两类块、省略清单与行号 / quote / needs 用法；入 scanner 模板哈希）+ scanner.md `<needs_policy>`「折叠体 / 省略片段 / 包外文件未展示 → 对该路径与行段声明 need」；旧路径 `render_dossier_section` 前言抽常量、输出逐字节不变（golden 测试） | — |
| `--pack-body <full\|slice>` CLI 覆盖旗（镜像 `--tools`） | 批 E 不加（plan 范围）；A/B 走 `REDTEAM_HOME=<临时目录>` + `config.json` `{"review":{"pack":{"body":"full"}}}`，出口 3 双 scanner 配置本就要临时 config | 用户裁 / M21e |
| 切片常数：一跳 K=3 调用点 / 名、`anchorWindow` 30、文件外段帽 = 组预算 / 8、名字上限 40、走仓上限 20000 文件、文件头 ≤ 80 行、多行签名 ≤ 6、胶水 ≤ 3 | 常量在 `src/pack/mod.rs`；`anchorWindow` / `sliceMinBytes` 已是配置键 | R7（出口 3 A/B 回退信号后调优 / M21d） |
| 文件外搜索的读守卫口径 = `search_repo` 工具（ignore 走仓 + denylist，不逐文件 `git ls-files`）；路径非模型可控 | 未跟踪但未被 gitignore 的支持语言文件会被搜到调用点 | 威胁模型 B 若要求「只读 git-tracked」，一次 `git ls-files -z` 取集合再过滤（F-2 scanner 路径评估） |
| 文件外走仓每组一次（N 组 = N 次串行全仓扫描，每文件新建 Searcher）；`> 20000` 文件截断处非确定（walker 序） | 本仓 2 组 2.0 s；PR181 规模 ≤ 5 组可接受 | 大仓变慢再做「一次走仓、按组分发」（build_packs 层合并名集）；截断时改按路径排序后截 |
| 评审 PLAUSIBLE 未核实：增量路径 `extra_paths`（全分支非 delta 文件）是否会经文件外片段泄漏进 pack | 文件外搜索只排除 `changeset`（当轮 bundles 的文件），增量轮的非 delta 分支文件不在其中——会被当普通仓文件搜调用点（内容非密，但与跨组索引「只带路径」的口径不一致） | F-2 / 增量轮清单合并（D-3）时定口径：`changeset ∪ extra_paths` 或维持 |
| 切片默认后 `Bundle.over_budget` 组的 pack 仍可能很大（本仓 cell-2：6 文件切片 + 1 梯子 = 220 KB） | 切片只降无关体，改动密集文件（本批 pack/mod.rs 核 57）切片 ≈ 全文 | R7 / lib.rs 超级枢纽议题一并看：分组预算门用估算字节、装包用切片字节，两者口径待对齐（M21b 前用 PR181 回放） |
| 「改动 def 名」两套口径：分组引用边 `bundle::changed_def_names`（hunk 首行锚定 + `block_end`、含测试 def）vs 文件外搜索键 `pack::outside_names`（全改动行 + `value_end` + 容器 / 测试排除 + 重锚定坐标） | 边与搜索键落在不同符号集上；分组侧改口径会扰动 C 的确定性测试与 stats | M21b 风险规则复用 `changed_defs` 时一并统一（分组侧改为吃 `all_defs` / `changed_defs`），届时重跑分组回归 |

## M21a 批 D / E 合并后核验（2026-09-09，两个只读审计子 agent + 冒烟）延后项

已修（同日提交）：scan 卷宗前言与 needs 措辞（上表 ✅）· `build_scan_prompt` 接受已包装的 `CellPlan.dossiers` 与 `cli::spec` 带壳段（剥壳，
不双壳）· anthropic 显式断点运行时封顶（每会话至多 3 显式 + system，多余静默降级）· 旧 `build_bundle_prompt` 字节级 golden 哈希测试 ·
文件外调用点 regex 交替按长度降序（短名是长名前缀时长名调用点漏检）· 名字截断提示只在真正走仓时写 · 段帽以组预算为基（design 口径）·
`-U0` 式尾部删除点并入 hunk 跨度 · `*g = f()` 解引用行不当注释 · parse 行号数字串 / 浮点与 `needs.kind` 大小写宽容 · scan 中和表移出
`bundle` / `incremental_round`。

| 项 | 现状 | 归属 |
|---|---|---|
| 多行 quote 只核首行 | `positioning::locate_quote_near` 只对 quote 首个非空行做精确 / 子串匹配，后续行按行数外推（与 `locate_quote` 同口径，prd R2 如此）；design §4 写「quote 逐字硬约束」，scanner 的 quote 是跨模型合并与判例预筛的锚——真实首行 + 编造后续行能过「不在文件即丢」门 | M21c 疑点合并（`quote_overlap`）落地时定是否对 scanner 走全行匹配（空白归一） |
| 中和 vs quote 锚定 | scan 中和表含 `diff` / `evidence` / `reference` 等常见词：被审源码里字面 `</diff>` 等被改写成 `<\/diff>`（且小写化），模型照抄的 quote 锚不回文件被丢。已移出 scan 路径不存在的 `bundle` / `incremental_round`；其余标签是真包裹段，不能删 | F 出口 dogfood 看 `dropped` 计数；若可归因到此，改中和形态为保大小写 + 在前言告知模型「quote 取原文」 |
| F 接线备忘（批 D 审计 I 项） | `run_single_shot(backend, user_prompt: &str, ..)` 硬编 `NO_TOOLS_SYSTEM` + `User(String)` + `parse_lens_output`；`LensRequest { prompt: String }` / `LensCall { findings }` 承载不了 `ScanPrompt` / `ScanOutput` | F-2 新开 scanner 通路：`[System(head), UserBlocks(blocks)]` → `parse_scan_output_with_stats` → `REPAIR_PROMPT_SCAN` → `anchor_suspicions(root, ..)`；`CellRecord.template_hash` 单值，F 定 head + tail 合成（如 `fnv1a64_hex(&[head, tail])`）；`--pack-only` 的 `bytes` / `est_tokens` 仍按旧 `build_bundle_prompt` 计，F 切到 scan prompt 后同步换（含 system 段） |
| `ScanParseStats.dropped_malformed` 混计 | 非对象疑点与无 `file` 的 `scanned_area` 同一计数 | F 写 warn 文案若需分开再拆字段 |
| `eval/_oracle.load_oracle` 缺 `--set` 直接 `ValueError` 回溯 | 非友好提示 | M21e 清扫 |
| Full 档脏树场景与 dev 基线的差异 | `synth_diff` 一旦任一 hunk 重锚定偏移，梯子 `changed_def_ranges` 锚点从「每 hunk 首行」变「全部改动行」（每行一个单行 hunk）→ Full 档 def 体级产出比 dev 多；K10「Full = 现状」应加此例外（复审修 2 有记录、措辞未更新） | 出口 3 A/B 用干净树即无此差异；prd K10 措辞补例外 |
| `FileCtx`「一次解析」未尽 | `all_defs` 对同一文件在 `slice_file` 与 `outside_names` 各算一次，`content.lines().collect()` 三次（切片 / 梯子 / 文件外） | 大仓变慢再缓存进 `FileCtx`（与「N 组 N 次走仓」同批） |
| `bracket_delta_code` 只认行内 `//` | 块注释 / 多行字符串里的括号仍计深度（多为 `value_end` 400 行护栏回落单行 = 少吞，或 import 组保留提前截止） | 语料出现再修 |
| `anchor_offset` 锚错位置（PLAUSIBLE，未构造实证） | hunk 只有一个有信息量但内容常见的 `+` 行（如 `Ok(())`）且坐标陈旧时取最近同文本行为偏移；禁真空性质仍成立、语义定位可能错 | dogfood 观测（脏树评审场景） |
| 批 E 归档制品 `implement.md` §2 / §3 自报测试名与实际不符 | 测试按场景合并重命名（覆盖在，见审计映射） | 归档制品不改；此处记一笔 |
| slice 档骨架级梯子无测试 | `slice_ladder_takes_over_when_over_budget` 只到 def 体级；骨架级代码与 Full 共用 | M21e 清扫时补 |

## M21a 批 F 延后项（2026-09-09，Trellis `09-09-m21a-cutover`）

| 项 | 现状 | 归属 |
|---|---|---|
| resume scan 重放无生产验证 | `run_resume` 用 manifest `scan` 请求重放差集 cell（旧格式作业记失败）；只有单测 / e2e 空 manifest 锚，无真 provider dogfood；角色集按当前配置重建 | M21b 队列快照 checkpoint 落地后重做（resume 读快照里的角色条目） |
| thinking 映射未在真网关实测的家族 | 映射表按官方文档（2026-09-09）：GLM anthropic 形态文档无字段 → 无映射 + warning；Qwen `enable_thinking` / Gemini `reasoning_effort` 未实测；本仓 dogfood 只覆盖 DeepSeek（`thinking` + `reasoning_effort`）与 Qwen（openai 形态） | 出口 2 已跑（2026-09-09）：GLM 5.3 anthropic 形态无映射 → registry warning 1、未发思考参数，网关默认仍有推理输出，并集 / agreement 正常；映射待 Z.ai anthropic 文档给字段；其余按模型接入时实测 |
| 增量轮显式报错 | `run_incremental_review` bail（M21c 恢复）；e2e `incremental_from_bails_until_m21c` 锚住报错；原 carry 端到端锚（携带 / dispute / re-raise）与 `render_incremental_section` / `two_layer_manifest` 随旧路径删 | M21c 疑点台账 + delta 激活时按 scan prompt 参考块加槽重建（批 C 延后项已记） |
| dismissed 台账只披露不消费 | `load_ledger` 仍加载并进 `run.ledger`；`Ledger.ids / cards` 无消费者（`mark_dismissed` / 判例注入随旧尾段删）；e2e 改为披露断言 | M21c 判例前移进 scanner prompt（`<precedents>` 槽已在 D-2）+ 豁免裁决 |
| 无消费者的旗 | `--block-threshold` / `--conf-floor` / `--tools` / `--dismissed` / `--no-dismissed` 保留、`--help` 注明生效里程碑（prd K13） | M21b（前三）/ M21c（后二）接消费者；届时改回文案 |
| 报告里的 finding 形态字段恒空 | `stats` 全零、`findings` / `clusters` 空数组仍序列化（schema 不翻号，followup :70）；插件 `result.md` / `review.md` 文案「呈现 findings」 | M21b 深挖产 finding 后自然填；插件文案 M21c 切名时改 |
| `CellSummary.attempts` 口径 | 字段注释仍写「候补链尝试的 model 数」，scan 路径恒 1（无候补，D7 A3） | M21e 清扫改注释或删字段 |
| 保留未用的旧件 | `provider/{single_shot,tool_loop,fake}.rs` 的 `ReviewProvider` 表 / `run_single_shot` / `NO_TOOLS_SYSTEM`、`consolidate/{dedup,verify,carry}`、`cli/second_pass` 零件（`allow(dead_code)`）、`orchestrate::enrich / finding_id`、`adversarial-refute.md` | M21b reviewer / verifier 复用；未复用的 M21e 删 |
| 大 diff 组的单 cell 体量 | 本分支 dogfood diff：cell-3 26 文件 / 466 KB diff / 782 KB prompt（≈ 224K token，16 文件走梯子）——分组预算门按估算字节、装包按切片字节（批 E 延后项已记） | R7 / lib.rs 超级枢纽议题一并看；M21b 前用 PR181 回放 |
| `GroupingStats` 分种计数 | `RunMeta.grouping` 已接（F-5），`cross_edges` / `weak_cut` 仍按全部边种计（:49） | ✅ 2026-09-10 已偿（`09-10-grouping-stats-kinds`，见上表同行） |
| Anthropic thinking 注入未按 tools 守卫 | `thinking{enabled}` 与工具往返同发时 Anthropic 要求 `tool_choice` / 交替约束，M21b reviewer 工具循环走 anthropic kind 会 400（评审帽外项，当前 scanner 无工具不可达） | M21b reviewer 接线时在 `build_anthropic_tools_body_ext` 按 tools 非空守卫 |
| `resolve --help` 文案仍承诺增量轮消费 | `resolve` 子命令 doc「增量轮据此不再携带该 finding」——增量轮 M21c 恢复前无消费者 | M21c 恢复增量轮时一并核文案 |
| `exit_code(verdict, failed_models_nonempty)` 第二参在 scan 路径恒 false | 五档契约保留 2 档给 M21b（reviewer 失败带债 approve） | M21b 定 failed_models 语义后再定是否收敛签名 |
| `ScanResults.usage / failed / needs_declared` 生产无消费者 | `finish_scan_report` 从记录重算；三字段只有测试读 | M21e 清扫时删或改成 `#[cfg(test)]` 访问器 |
| `anchor_suspicions` 每疑点重读同文件 | 一 cell 几十条疑点 × `safe_read` | 语料大时按文件缓存一次读（M21c 跨文件重定位落地时顺手） |

## M21a 出口验收延后项（2026-09-09，另起 session；记录与原始数据只在 `.next/private/m21a/`，本表不含被评仓细节）

结果：验收 1-10 中 8 过；未过两项都在验收 1（PR181 v2 scan）：**分组 7 > 5**、**墙钟 311 / 403 / 458 s > 3 min**。oracle 并集 16 / 22（E3 基线 9）、
人工主题严格并集 6（≥ 5）、`--pack-only` 误差 0.9-14.5%、DeepSeek 磁盘缓存二跑起 ≥ 95%、E2 slice ≥ full 维持默认、双 scanner agreement 正确、schema v4 / 配置 / 门与文档全过。

| 项 | 现状 | 归属 |
|---|---|---|
| 分组 ≤ 5 未达（PR181 7 组） | 7 组中 4 组是 ≤ 3 文件的小强分量（同目录文档对、schema + 其测试对）；`bundle::group` 只对孤立**单文件**按目录装箱，多文件小分量不并箱（design「孤立文件 bin-packing」原文如此）→ 组数随小分量数线性涨，每组一次 scanner 调用 | 用户裁：小分量（文件数 ≤ 3 且估算字节远小于预算）与孤立文件同池装箱（代价：不相关文件同格）或把门槛改为「组数 ≤ 强分量数 + 1」；M21b 前定 |
| 墙钟 ≤ 3 min 未达 | 分解（三跑仿真与实测逐跑吻合）：总墙钟 = 排队等待 + 最长生成格；排队来自 `review.scanners.workers`（缺省 4）< 组数 7 且按索引序派发（最大组排第六）；生成速度各格一致 ≈ 110 tok/s，最长格输出 20-38K token。✅ 已改（2026-09-09）：扇出按组 diff 字节降序派发（结果仍按 plan 序）；A/B：现状 311 / 402 / 457 s → 大格先发 + workers 4：283 / 247 / 234 s → + workers 8：189 / 202 / 251 s（无排队，= 最长生成格）。provider `maxConcurrency` 不是约束门，用户配置要同时提 `review.scanners.workers` | 用户裁（2026-09-09）：workers / 并发开大后可接受，门槛不再按 3 min 硬卡；剩余手段只有继续拆大格（组预算 / 超预算组再切）→ M21b 前与「小分量并箱」一起议分组策略；✅ 出口修补批（2026-09-09）：cell 数 > workers 时 run.warnings 排队告警；`CellRecord.queue_wait_ms` / `CellSummary.queue_wait_ms`（含在 duration_ms 内）；0 疑点且输出 < 2,000 token 的格 cell warn；`review --help` 补并发说明。两道门的分工已裁〔D13〕：删角色 `workers`、唯一闸 = provider 门，见下行 |
| 「拆大格」的杠杆不是字节预算 | 每 cell pack 预算 = min(768,000 B, (最小 scanner 窗 − 16K)×3 − 24K)；DeepSeek 1M 窗下即 768 KB ≈ 219K token。PR181 两个 over_budget 组（11 / 12 文件）都是撞 `maxFilesPerGroup`=10 的文件数门，估算字节 351K / 142K、落盘 pack 265K / 149K 只占预算 34% / 19%。墙钟驱动量是**每格 diff 字节**（输出 token ∝ diff：g6 49K diff → 22-38K 输出，g4 35K → 20-26K，g3 23K → 13K），与 pack 字节无关 | 用户裁（2026-09-09）：保持 followup——样本太少，aacr-bench 重启、跑更多测试集后再看要不要拆；届时拆分键按每格 diff 字节、先切引用计数最低的强边、切开边进交叉索引 |
| `scanners.workers` 形态（用户 2026-09-09 提） | 现为用户级配置键 `review.scanners.workers`（缺省 4，design §12.3 列为角色并行池）。用户意见：① 自适应规则要看 provider `maxConcurrency`、同 provider 下是否还有 reviewer / verifier 及其在跑情况，需一轮 grill；② 不设用户级配置项——workers 与评审内容相关（改动大小决定并行数），应由每次命令参数约束，与 provider 级 `maxConcurrency` 分层 | ✅ 已裁（2026-09-09 grill〔D13〕，design v10 §12.3）：删三个角色 `workers` 键、不设 `--workers`；唯一闸 = provider 在途门（单把 FIFO、scanner 先于 H0 的入队序、不预留）；`review.maxConcurrency` 缺省 8 → 10 + 429 排队重试（门收敛到网关真实上限、持名额等下一完成、独立计数）+ `maxConcurrency > 128` warning；队列入口五态并入 + 跨轮沿用结案带守卫；scan 项不等同组兄弟 cell，记派发时 / 落地时 agreement 供 M21e 定 hold。代码分两步：M21a 收口批只动 scanner 路径（删键、删角色信号量、按 provider 计排队告警、`InflightGate` 429 排队重试、help、测试）；队列侧五态与 agreement 字段随 M21b。✅ M21a 收口批已落（Trellis 任务 `09-09-d13-provider-gate`：641b4d3 删键 / 门唯一闸 / 告警按 provider；f7bae23 门自管 + Notify、429 排队重试、`RunMeta.provider_rate_limit`；4aabec3 评审修 11 条——准入 + 退避不齐发、只成功算完成、529、RAII、attempts / queue_wait_ms 口径；lib 538 / e2e 17 / clippy 7）；真跑核验记录只在 `.next/private/d13/`；评审延后项见下表 |
| scanner 首发解析失败原因不可观测 | dogfood 提交 3411a0e 的 slice 大格三跑首发全失败走 REPAIR（输入翻倍），直发同 prompt 三次首发皆合法 JSON（产品解析器 OK）→ 产品路径与直发的差异未定位 | ✅ 已加观测（2026-09-09）：`ScanCall.repair_note`（finish / 文本长度 / 文本头）进 cell warn；加观测后复跑一次首发即成功（未复现，此前三跑与另一队列并跑），下次出现由 cell warn 定位 |
| 网关 anthropic 流 usage 形态 | agc 网关 `message_start` 报 input 0、真值只在 `message_delta`（直发探测证实）→ GLM 格 input / cache 恒 0，`cache_hit_by_cell` 不含 GLM | ✅ 已修（2026-09-09）：`parse_anthropic_stream` 对 message_delta 的 input / cache 分列「在场即覆盖」+ 单测；双 scanner 复跑核 |
| 换思考档失去 DeepSeek 磁盘缓存前缀 | medium 与 high 落盘 prompt 逐字节相同，但 input token 差 13、cached 0.6% → DeepSeek 按 `reasoning_effort` 注入内部前缀 | 只记：生产档位固定无影响；A/B 或调档实验按新前缀计首跑成本 |
| `noise` 口径 | scan_score 噪音比 = 非 oracle 条 ÷ 总条（0.84），条数 52 vs E3 10.7 → 噪音比随条数涨，是上界；两臂各自独有的稳定项说明非纯噪音 | M21b 深挖 / verifier 落地后用 refute 率替代；R11 agreement 口径已在双 scanner 数据里可读（19 条） |

## D13 收口批评审延后项（2026-09-09 code-review 三批 16 条中的 5 条设计层争议；记录只含本仓细节）

| 项 | 评审意见 | 归属 |
|---|---|---|
| 速率型 429 棘轮 | 收门公式对每次 RateLimited 都执行 `cap ← min(cap−1, others)` 且本轮不回升；RPM / TPM 型 429（网关按速率而非并发限流，本机 deepseek / qwen 即此类）在一个窗口内连撞 N 次门宽降 N，剩余格串行、warnings 还劝「set maxConcurrency ≤ 1」。候选：① Retry-After 在场视为速率信号不收门只等；② 最小回升——连续「门宽」次成功完成后 cap + 1（≤ 配置值）；③ 只在 `others < cap − 1` 时收 | 用户裁（K5「只降不升」是 D13 定案；A5 实测形态是并发密度，先看 PR181 `provider_rate_limit` 数据） |
| 放弃判据按次数 | `max(4, 门宽 × 2)` 次 × 500 ms 指数 ≈ 7.5 s 后放弃；Retry-After 被 30 s 硬夹（60 s 窗口醒来必再撞）。候选：等待总时长 ≤ 该条目 `per_turn`（已有旋钮）、Retry-After 在预算内不夹，删 RL_RETRY_FLOOR / 30 s 常量 | M21b 前定；与上一行同一批数据 |
| `AttemptPolicy` 以 `Option` 穿三层 | `timed_turn(_final)_with` / `run_scan_shot_with` 三对平行签名 + 循环体四处 `if let Some`；429 在有 / 无策略两条路径语义不同（7.5 s vs 1.2 s 放弃）。候选：旧退避提成 `BudgetBackoff` 策略，`retry_classified(budget, &dyn AttemptPolicy, op)` 单路径，M21b 6 个调用点各传一个 | M21b 接 reviewer / verifier 时一并改（K11 本批不动 single_shot / tool_loop） |
| HTTP 503 归类 | 本批 529 归 RateLimited 进排队重试；503 仍归 Server 走 2 次抖动预算。A5 列 503 为并发密度失败形态，但 503 也可能是维护 | 用户裁 |
| `InflightGate::new` 的 `cap.max(1)` | validate 已挡 0，这里是静默兜底；候选 NonZeroU32 或记 warning | 低优，M21e 清理 |

## 跨里程碑观测面（每个里程碑出口必报，写进 RunMeta 或 research）

墙钟（PR181 低 / 高风险各一）· findings oracle 命中数 · cache 命中率 · 疑点数与噪音比 · 预算截断数 · 台账落入 / 激活数 ·
每 cell 输出 token 与 output_max_tokens 比 · 复核触发 / 跳过 / refute 率 · per-provider 每角色在途峰值 · 门等待时长 · 429 / 503 计数与 429 收敛门宽 · 派发时 / 落地时 agreement〔D13〕·
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
