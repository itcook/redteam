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
| `GroupingStats.cross_edges` / `weak_cut` 口径（A/B/C 合并核验 2026-09-08） | `cross_edges` 计全部边种（同目录分量一切即被 SameDir 边灌满），`weak_cut` 计超限分量内全部弱边（含留在同一强子分量内、实际未切的）；design §3.2 步 6 的 R8 触发度量是「跨组 **import** 边数」 | ✅ 已偿（2026-09-10，Trellis `09-10-grouping-stats-kinds`）：`cross_edges` 分种 `cross_import` / `cross_reference` / `cross_same_dir`、`weak_cut` 删除（真切口径 = cross_reference + cross_same_dir 无独立信息量）；「强边不跨组」不变量哨兵；PR181 基线 cross_import=0 / cross_reference=2 / cross_same_dir=6（旧 cross_edges=8、weak_cut=55 虚高）；同日评审修：三键改 `Option<usize>`（旧报告未采集 = None，不从总数猜分种） |
| 分种键 JSON 形状迁移兼容边界（2026-09-10 评审 Q3/Q4） | 分种三键 M21b 内两迁：7893439 引入（usize）、同日评审修改 `Option<usize>`；跨版本 resume 仅保证「新二进制读旧 JSON」（旧→新：缺键 default 归 None ✓）；更早二进制读新 JSON 不在契约内（7893439 前的必填 weak_cut 报缺键、7893439 的 usize 读不了 null，`cli/mod.rs:938` `.ok()` 静默吞后 resume 丢 grouping/head_sha 无 warning）；revert 只恢复代码不恢复已写报告 | M21b 未发布仅 dogfood、单二进制同源——不修；若 dogfood 需跨版本 resume，删旧作业重跑即可 |
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
| 分组 ≤ 5 未达（PR181 7 组） | 7 组中 4 组是 ≤ 3 文件的小强分量（同目录文档对、schema + 其测试对）；`bundle::group` 只对孤立**单文件**按目录装箱，多文件小分量不并箱（design「孤立文件 bin-packing」原文如此）→ 组数随小分量数线性涨，每组一次 scanner 调用 | ✅ 已偿（2026-09-10，Trellis `09-10-small-component-pooling`，用户裁选项 1〔D14〕）：小分量（≤3 文件且 est ≤ 预算/4，budget=None 不判）作原子单元与孤立文件同池装箱；`GroupingStats.pooled_small_components`（serde default）观测；PR181 回放 7→4 组、cross_import=0 保持、四键与 over_budget 不变，数据 `.next/private/m21b/pr181-packonly-pooling.txt` |
| 墙钟 ≤ 3 min 未达 | 分解（三跑仿真与实测逐跑吻合）：总墙钟 = 排队等待 + 最长生成格；排队来自 `review.scanners.workers`（缺省 4）< 组数 7 且按索引序派发（最大组排第六）；生成速度各格一致 ≈ 110 tok/s，最长格输出 20-38K token。✅ 已改（2026-09-09）：扇出按组 diff 字节降序派发（结果仍按 plan 序）；A/B：现状 311 / 402 / 457 s → 大格先发 + workers 4：283 / 247 / 234 s → + workers 8：189 / 202 / 251 s（无排队，= 最长生成格）。provider `maxConcurrency` 不是约束门，用户配置要同时提 `review.scanners.workers` | 用户裁（2026-09-09）：workers / 并发开大后可接受，门槛不再按 3 min 硬卡；剩余手段只有继续拆大格（组预算 / 超预算组再切）→ M21b 前与「小分量并箱」一起议分组策略；✅ 出口修补批（2026-09-09）：cell 数 > workers 时 run.warnings 排队告警；`CellRecord.queue_wait_ms` / `CellSummary.queue_wait_ms`（含在 duration_ms 内）；0 疑点且输出 < 2,000 token 的格 cell warn；`review --help` 补并发说明。两道门的分工已裁〔D13〕：删角色 `workers`、唯一闸 = provider 门，见下行 |
| A-1 def 口径统一 → PR181 分组变密（2026-09-10） | `changed_def_names` 改吃 `all_defs`/`changed_defs` 后：groups 4→3、edges 111→152、pooled_small 4→1、大格 11+12 文件 → 单 **28 文件** over_budget 格（est≈328K / pack≈206K / ladder=15）。归因：测试 def 大量入引用边名集（测试文件→被测函数成真实边） | 已记录（预期内，口径统一为本批显式偿还）；大格拆解与上行「拆大格」合并观测（切分键按每格 diff 字节），M21b 出口 / aacr-bench 重启时定 |
| 「拆大格」的杠杆不是字节预算 | 每 cell pack 预算 = min(768,000 B, (最小 scanner 窗 − 16K)×3 − 24K)；DeepSeek 1M 窗下即 768 KB ≈ 219K token。PR181 两个 over_budget 组（11 / 12 文件）都是撞 `maxFilesPerGroup`=10 的文件数门，估算字节 351K / 142K、落盘 pack 265K / 149K 只占预算 34% / 19%。墙钟驱动量是**每格 diff 字节**（输出 token ∝ diff：g6 49K diff → 22-38K 输出，g4 35K → 20-26K，g3 23K → 13K），与 pack 字节无关 | 用户裁（2026-09-09）：保持 followup——样本太少，aacr-bench 重启、跑更多测试集后再看要不要拆；届时拆分键按每格 diff 字节、先切引用计数最低的强边、切开边进交叉索引 |
| `scanners.workers` 形态（用户 2026-09-09 提） | 现为用户级配置键 `review.scanners.workers`（缺省 4，design §12.3 列为角色并行池）。用户意见：① 自适应规则要看 provider `maxConcurrency`、同 provider 下是否还有 reviewer / verifier 及其在跑情况，需一轮 grill；② 不设用户级配置项——workers 与评审内容相关（改动大小决定并行数），应由每次命令参数约束，与 provider 级 `maxConcurrency` 分层 | ✅ 已裁（2026-09-09 grill〔D13〕，design v10 §12.3）：删三个角色 `workers` 键、不设 `--workers`；唯一闸 = provider 在途门（单把 FIFO、scanner 先于 H0 的入队序、不预留）；`review.maxConcurrency` 缺省 8 → 10 + 429 排队重试（门收敛到网关真实上限、持名额等下一完成、独立计数）+ `maxConcurrency > 128` warning；队列入口五态并入 + 跨轮沿用结案带守卫；scan 项不等同组兄弟 cell，记派发时 / 落地时 agreement 供 M21e 定 hold。代码分两步：M21a 收口批只动 scanner 路径（删键、删角色信号量、按 provider 计排队告警、`InflightGate` 429 排队重试、help、测试）；队列侧五态与 agreement 字段随 M21b。✅ M21a 收口批已落（Trellis 任务 `09-09-d13-provider-gate`：641b4d3 删键 / 门唯一闸 / 告警按 provider；f7bae23 门自管 + Notify、429 排队重试、`RunMeta.provider_rate_limit`；4aabec3 评审修 11 条——准入 + 退避不齐发、只成功算完成、529、RAII、attempts / queue_wait_ms 口径；lib 538 / e2e 17 / clippy 7）；真跑核验记录只在 `.next/private/d13/`；评审延后项见下表 |
| scanner 首发解析失败原因不可观测 | dogfood 提交 3411a0e 的 slice 大格三跑首发全失败走 REPAIR（输入翻倍），直发同 prompt 三次首发皆合法 JSON（产品解析器 OK）→ 产品路径与直发的差异未定位 | ✅ 已加观测（2026-09-09）：`ScanCall.repair_note`（finish / 文本长度 / 文本头）进 cell warn；加观测后复跑一次首发即成功（未复现，此前三跑与另一队列并跑），下次出现由 cell warn 定位 |
| 网关 anthropic 流 usage 形态 | agc 网关 `message_start` 报 input 0、真值只在 `message_delta`（直发探测证实）→ GLM 格 input / cache 恒 0，`cache_hit_by_cell` 不含 GLM | ✅ 已修（2026-09-09）：`parse_anthropic_stream` 对 message_delta 的 input / cache 分列「在场即覆盖」+ 单测；双 scanner 复跑核 |
| 换思考档失去 DeepSeek 磁盘缓存前缀 | medium 与 high 落盘 prompt 逐字节相同，但 input token 差 13、cached 0.6% → DeepSeek 按 `reasoning_effort` 注入内部前缀 | 只记：生产档位固定无影响；A/B 或调档实验按新前缀计首跑成本 |
| `noise` 口径 | scan_score 噪音比 = 非 oracle 条 ÷ 总条（0.84），条数 52 vs E3 10.7 → 噪音比随条数涨，是上界；两臂各自独有的稳定项说明非纯噪音 | M21b 深挖 / verifier 落地后用 refute 率替代；R11 agreement 口径已在双 scanner 数据里可读（19 条） |

## D13 收口批评审延后项（2026-09-09 code-review 三批 16 条中的 5 条设计层争议；记录只含本仓细节）

| 项 | 评审意见 | 归属 |
|---|---|---|
| 速率型 429 棘轮 | 收门公式对每次 RateLimited 都执行 `cap ← min(cap−1, others)` 且本轮不回升；RPM / TPM 型 429（网关按速率而非并发限流，本机 deepseek / qwen 即此类）在一个窗口内连撞 N 次门宽降 N，剩余格串行、warnings 还劝「set maxConcurrency ≤ 1」。候选：① Retry-After 在场视为速率信号不收门只等；② 最小回升——连续「门宽」次成功完成后 cap + 1（≤ 配置值）；③ 只在 `others < cap − 1` 时收 | **2026-09-10 用户裁（批 C 开工前）**：沿用 D13 现口径（只降不升）开批 C；①②③ 不现裁。依据：D13 三跑与 PR181 均未产 `provider_rate_limit`（无 429 样本），无数据拍板等于猜。触发再裁：真撞到带 Retry-After 的速率型 429，用 `hits` vs `effective_cap` 对照窗口是否被拧到 1。C-3 Fake 429 注入测现语义 |
| 放弃判据按次数 | `max(4, 门宽 × 2)` 次 × 500 ms 指数 ≈ 7.5 s 后放弃；Retry-After 被 30 s 硬夹（60 s 窗口醒来必再撞）。候选：等待总时长 ≤ 该条目 `per_turn`（已有旋钮）、Retry-After 在预算内不夹，删 RL_RETRY_FLOOR / 30 s 常量 | **同上裁**：沿用次数上限 + 30 s 夹；与棘轮同一触发再裁（真 RA 窗口 vs 放弃墙钟） |
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

## M21b 批 B 接口接缝（2026-09-10，Trellis `09-10-m21b-review-primitives`，批 C 接线直接消费）

- **批 C 零件清单**（本批产出、签名即契约）：`pack::region::{QueueItemSpec（Origin 三值 + H0 无笔记构造校验）、RegionPack{evidence, cross_index, notes, stats}、build(item, bundle, root, idx, budget, opts, needs_evidence) -> Result<RegionPack, String>}`（互斥/读守卫在 build 内构造期执行，Err/退化不 panic）；`prompt::{ReviewPromptInput、build_review_prompt、review_template_hash}`；`provider::parse::{parse_review_output(_with_stats)(raw, model)、ReviewParseStats、REPAIR_PROMPT_REVIEW}`；`positioning::anchor_review_findings(findings, root) -> (kept, dropped, dropped_unreadable)`（`ReviewParseStats.dropped_quote` 由它回填）；`provider::stream_json::IncrementalFindings{feed/take/discarded/done}`。
- **`ChatBackend::on_text_delta` take 语义**（非 guard——MutexGuard 跨 await 非 Send）：backend 内部槽 take/put-back，FINALIZE/REPAIR 后续轮继续喂；共享 registry 实例不挂回调（缺省 None 逐字节不变），批 C 的 run_review_shot 须 **per-cell 构造 backend 实例**（`backend_from_config().with_text_delta(feed 闭包)`）再跑，reqwest Client 复用不贵。
- **`read_sse_with_delta` 与 `resp.text()` 等价性**：bytes_stream 聚合 + `from_utf8_lossy`；有效 UTF-8（SSE 恒是）下逐字节相同；无回调时 extract 短路（读取路径只剩字节聚合）。Cargo.toml 因此 +reqwest `stream` feature。
- **agreement / models 真值**：`parse_review_output_with_stats` 经 `orchestrate::enrich` 填 agreement=1、models=[model]——批 C 按队列记录改写落地时刻 agreement（D13）。
- **区域 pack 的 spec 段裁定**：spec 走共享头槽（与 scanner 共享头逐字节相同的验收迫使），不进 RegionPack——plan B-1 文本的「+ spec 段」按此解读，避免双份 token。
- **批 C 接线必须写死**（评审修后仍属调用方契约）：`parse_review_output_with_stats` 后调 `anchor_review_findings`，把 `(dropped, dropped_unreadable)` 回填 `ReviewParseStats.dropped_quote`——plain `parse_review_output` 丢计数；`IncrementalFindings::finalize()` 在旁路消费结束 / cell 死亡处调用（截断余下 discard 计数）。
- **双 Origin 同名**：`pack::region::Origin`（rule/scan/needs）与 `domain::finding::Origin`（carried/new）serde 都是小写词表——`QueueItem` 导入时改名或互指 doc，勿混队列来源与 finding 出身。
- **组 pack `prepare_files` symlink 存量暴露**：本批只给 region 装配路径加 canonicalize+读帽；组 pack 路径来自 git diff（非模型可控），M21e 或安全债另记。


## M21b 批 C 核查留债（2026-09-12，Trellis `09-10-m21b-deep`，修 4 条后剩余）

> ✅ **前两条已解（批 C1 C1-1 `aaa622d`，2026-09-14 合回）**：id 在 `new` 时定死、合并不重算；Skipped 仅成必挖才复位（`MergedSkipped`）。
> **已定案（2026-09-13 C1 拷问）**：前两条的处置已钉进 C1 任务 design——id 定死、Skipped 仅成必挖才复位（`MergedSkipped`）。
> **已有主（2026-09-12）**：下列前两条（队列项身份、Skipped 复位）由**批 C1 Task C1-1** 承接——
> 它们正是「当前不可触发、并发一开就真」的那类，必须在并发派发落地前解掉。第三条（`ItemKind::Needs`
> 无生产路径）仍是纯记账，等 M21c 增量轮决定留删。

- **`merge_into` 改写 `it.id` × 在途项**（`orchestrate/queue.rs`）：合并会 union ranges 后重算 id，而 InFlight 项已按
  旧 id 克隆派发 → 派发期并入会让 `mark_settled(&item.id)` 找不到项、卡在 InFlight，barrier 永不满足。**当前不可触发**
  （单派发器把全部入队放在消费循环前，在途期间队列不变），`live_agreement` 已按查不到退回派发值兜住观测面。
  批 C1 的并发派发一开即触发（多格同时在途时入队），Task C1-1 先把队列项身份与 spec 解耦（id 固定，ranges 可变）；批 D 的复核回灌同理。
- **`merge_into` 命中 Skipped 项返回 `MergedPending` 但不改回 Pending**：并入后 agreement 升到 ≥2（进必挖集）也不会被
  重挖。同样因 drain 在消费循环前、其后无入队而不可触发；解法与上条同源。
- **`ItemKind::Needs` / `QueueOrigin::Needs` 无生产路径**：needs 改走 cell 内二阶段补查轮（design §11 R2），
  队列 Needs 层与 `DeepMeta.enqueue_reasons["needs"]` 恒空。prd 的四层优先级实际只有三层活——保留枚举作 M21c
  增量轮的入队口，或届时删。

## Anthropic thinking 两处真债（2026-09-12 查证官方文档立项；批 C 守卫已按此更正）

批 C 曾加过一版「tools 非空即丢弃 Anthropic 思考参数」的守卫，前提错误已删（官方文档：
「Thinking works alongside tool use」；两条约束是 tool_choice 强制工具、thinking block 原样回传，
都与「有没有 tools」无关，而我们的 body 既不设 tool_choice 也不设 temperature/top_p）。
真正会 400 的是下面两条。**注意这条路径是活的、不是备用**（2026-09-12 dogfood 实证）：`kind` 可在
**模型级**覆盖 provider 级——实测 provider `agc` 声明 openai，其下 `grok-4.6` / `glm-5.3` 各自声明
anthropic，reviewer 全程走 anthropic body。此前「三 provider 全 openai kind，此 body 走不到」的判断
只看了 provider 级，已在代码注释与本条一并订正。因此下面第一条是**当下风险**而非将来时：

- **`type:"enabled"` 在 Claude 4.7+ 上直接 400**（文档点名 Opus 4.7/4.8、Opus 5、Sonnet 5、
  Fable 5/5.1、Mythos 5/5.1；4.6 为 deprecated 但仍成功）。而 `thinking_params`
  （`backends.rs`，anthropic+claude 分支）恒生成 `{"type":"enabled","budget_tokens":N}`
  ——**单发无工具路径同样中招**，与 tools 无关。出路 = 按 id 判版本后改发
  `thinking:{type:"adaptive"}` + `output_config:{effort:…}`（M21c 随 effort 档位一起迁）；
  过渡期至少给一条 warning，别让用户以为思考档生效了。
  **当下暴露面**：现配置的两个 anthropic-kind 模型 family 是 grok / glm，不在 thinking 映射表里
  （`thinking_params` 的 anthropic 分支只认 `claude`）→ 实际不发 thinking 参数、暂不会 400，
  RunMeta 里能看到「无参数映射，未发送」warning。但只要有人把 claude 模型配成 reviewer/verifier
  （family=claude 有映射），4.7+ 就会立刻 400——这是**现在就能踩到**的坑，不是配直连才有。
- **thinking block 不回传**：`ChatMessage::Assistant{text, tool_calls}` 结构里没有 thinking 字段，
  SSE 的 `thinking_delta` 直接丢弃，多轮 tool loop 的 assistant 消息因此不带 thinking 块。
  文档口径：turn 内回传是 **Required**；好消息是缺失通常「degrade gracefully」（静默禁用思考，
  不报错），**被改动过**的块才 400。要真正吃到 interleaved thinking 就得让 ChatMessage 带上
  thinking 块并原样回传（动数据结构 + SSE 解析，M21c）。

## Finding 的两根 agreement 轴（2026-09-12 拆分，批 D 接线必读）

design §7 写「落地的 finding 继承记录当时的 agreement〔D13〕」、§8.1 的跳过规则又说「疑点
agreement ≥ 2 ∧ reviewer 确认 → 免复核」——这个 `agreement` 与 `dedup::merge_into` 里既有的
`agreement = models.len()`（认同该 finding 的模型数，`consolidate::verify` 孤证复核的判据）
撞名。批 C 照 design 字面实现，把队列值盖进 `Finding.agreement`，两种语义挤在一个字段。

**已拆**（`Finding` 新增字段，serde default + 0 时不序列化，旧报告可读）：

| 轴 | 字段 | 谁写 | M21b 里谁读 |
|---|---|---|---|
| finding 可信度 | `agreement` | `enrich` 起手 1；`dedup::merge_into` = `models.len()` | **深挖路径上无人读**（见下「轴已失效」）；carry(`carried.agreement = 2`) 与旧报告仍用 |
| 区域可疑度 | `suspicion_agreement` | 深挖 `EmitCtx`（队列项**落地时刻**值，后涨不回溯） | 批 D 跳过规则 `≥ 2 ∧ reviewer 确认`（design §8.1） |

**`agreement` 轴在 M21b 已无产生机制**（2026-09-12 与维护者确认）：design §6 规则 5 的分派池是
「每项只由一个模型消费…**不双审**」，多 reviewer 条目是不同 worker 审不同疑点，不是多模型审同一点。
于是单 reviewer 配置下 `agreement` 恒 1，多 reviewer 也只剩「同一 bug 被分到两格、各自发现、且恰好
不同模型」这种偶然。对照旧架构（M20 多模型 fan-out 并行审同一份代码）那里 `agreement ≥ 2` 才是强信号。

因此：
- `consolidate/verify.rs` 现存的 `f.agreement == 1 ∧ severity ≥ block ∧ confidence ≥ floor` 是
  **M20 遗留规则**，在 M21b 下 `agreement == 1` 恒真、条件形同虚设。design §8.1 的新触发集是
  `severity ≥ block ∧ confidence ≥ floor`（**不带 agreement**）+ 判例变体命中——plan D-2 本就要重写这段。
- 批 D 决策点①：`agreement` 保留还是标 legacy。倾向保留不动（schema 稳定，carry / 旧报告仍读）。
- 批 D 决策点②：§8.1「疑点 agreement ≥ 2 **∧ reviewer 确认**」里的「reviewer 确认」指什么？
  任何进入复核的 finding 本就是 reviewer 报出来的——若「确认」即指此，跳过条件退化为
  `suspicion_agreement ≥ 2`；若另有含义（如 confidence 门），design 未写，需批 D 定。

- dedup 合并：`agreement` 按 models 重算，`suspicion_agreement` 取 max（区域属性，非模型属性）。
- `FindingEvent.suspicion_agreement` 同名同义；scanner / 非深挖路径留 0。
- **批 D 写跳过规则时读 `suspicion_agreement`，不要读 `agreement`**——后者在单 reviewer 配置下
  恒为 1，用它判跳过等于永不跳过。
- design §7/§8.1 的行文仍是旧词，读到 `agreement` 需按上表对照（文档不改，避免与历史记录打架）。

## 批 C dogfood 自评审未修项（2026-09-12，`redteam-next review --base 674583e`，reviewer = glm-5.3，13 条）

真 provider 跑自己的变更集，reviewer 审的正是本批「两轴拆分」。已修 3 条（[1] 旁路发射跳过 quote 锚定、
[2] 疑点共识取值时点与文档不符、[4] 两轴测试断言为空——删掉 `max` 行仍绿），下列留账：

- **[3] dedup 合并谓词与队列合并谓词不同源**（medium/0.55）：队列按「同文件 ∧ 行重叠 ∧ quote 主锚」合并，
  `is_same_bug` 按「同文件 ∧ 行重叠 ∧（标题 Jaccard ≥ 0.5 OR quote 重叠）」。于是两个在队列里**没合并**的项
  （quote 不同、标题相近）产出的 finding 可能在 dedup 被合并，`suspicion_agreement` 取 max 时就用了另一项的
  疑点共识——批 D 免复核信号跨项串值。批 D 定跳过规则时决定：dedup 合并 finding 时疑点共识取 max / 取 min /
  拆开不合并。
- **疑点共识双轴的表述张力**（[8][9][11][12]）：[11] 戳得准——`suspicion_agreement` 的文档说「三个 scanner 盯过
  不代表 reviewer 报的这条更可信」，而 design §8.1 恰恰把免复核建在它上面。这是 design 的内在张力，不是注释写错：
  §8.1 的「∧ reviewer 确认」是把区域可疑度转成 finding 可信度的那一步，批 D 需把「确认」落成可计算条件
  （见本文件「两根 agreement 轴」决策点②）。
- **[10] md 报告不展示 `suspicion_agreement`**（low）：JSON 有、Markdown 无。批 D 跳过规则落地后再决定展示形态
  （纯数字没有解释力，得和「是否免复核」一起出）。
- ✅ **[13] 事件载荷缺不变量断言**（low；C1-2 通道并入分叉用例已钉）：`FindingEvent.suspicion_agreement == ev.finding.suspicion_agreement`
  无测试钉住。批 C1 处理事件滞后时一并加。
- 误报：[5]（称 fixtures 模块为本批新建——实为既有）；[6]（称 plan 不在变更集——实在，疑 pack 截断）。
- **`ReviewCall.finish_reason` 不进 `CellRecord`**：算出来了却没落盘，报告看不出哪格被截断（观测缺失，功能正常）。

## 批 C1 拷问定案带出的账（2026-09-13，`/grilling` Q1–Q10，定案全文在 Trellis 任务 `09-13-m21b-deep-concurrency/design.md` §9）

- ✅ **批 C resume 覆盖 cell 丢 finding（C1-1 `aaa622d` 已修；评审修 `96b1ad5` 补对账双向以成功格为准 + 重挖失败保载体）**：`run_deep` 的 `review[{n}]` 每次调用从 1 起、`cell_put` 按 cell 名
  upsert → resume 的 `review[1]@m` 覆盖首跑同名格（不同队列项）→ 二次 resume 永久丢 finding。批 C C-5 清单的「Settled 以 cells 为准」
  也未实现。`redteam-next` 在 M21c 切名前不进插件与 ship 循环，故不热修。C1 定案：cell_id = `review[{item.id}]@{model}` + 补齐该校验。
- ✅ **队列 id 捷径造假共识（C1-1 `aaa622d` 已修；评审修 `4e70292`：捷径合并不并家族但保请求自身共识，agreement 单调不减）**：上游 `merge_suspicions` 已按 `same_suspicion` 组内传递闭包，进队列的同文件
  scan 项互不满足谓词，生产上 scan 项在队列里的合并**全部**走 `find_merge_idx` 的 id 捷径（行区间逐字相同）并抬 agreement。
  C1 定案：捷径合并只并笔记与区间，**只有谓词合并才计共识**。→ 批 C 合回后的 `suspicion_agreement` 可能偏高，C1 合回后收紧；
  若数据显示 quote 噪声（如尾部分号）导致真共识漏判，修 `same_suspicion` 的 quote 归一化，不靠捷径。
- ✅ **二阶段补查四处静默（C1-2 已修）**：重新 lease 失败吞掉、第二轮用共享 backend、policy 重试 / 等待未回读、第二轮失败吞掉且 token 不入账；
  `needs_two_stage_triggered` 虚报。C1 定案：一格持门位到底；第二轮失败 → 保留首轮 finding + warning + 计债；全被读守卫拒 → skipped 不计债。
- ✅ **兼任角色（code-review 批 C 第 12 条）已消解（C1-3 补发 reviewer 档位 warning）**：`RoleAttrs` 只有 `thinking_level`，深挖 per-cell backend 按 reviewer 自己的
  `EffectiveModel` 构造。残余仅兼任时 reviewer 的 thinking 无映射 warning 被 `continue` 跳过（C1-3 补发）。verifier 接线（批 D）时同样检查。
- **scanner 与深挖时间重叠 → 批 C2**（plan 已立节）：流式入队、在线预算与保底预留、scanner 先入门的门让位、reviewer 已开挖时的
  scanner 失败口径。C1 留 `DeepInput.incoming` 通道与「通道关闭」barrier。
- ✅ **报告确定性（C1-2 已修）**：dedup 取输入序首个为 base（标题 / 正文 / 建议保留 base），并发完成序会让报告文本逐次漂移；
  meta 四数组 push 时点不一，批 C 串行下只要有失败格就已错位。C1 定案：报告按派发序组装、meta 按派发序对齐。
- **reviewer 模型去留的数据口径**：批 C dogfood grok-4.6 0 条 vs glm-5.3 13 条（同变更集同 12 格）。C1 定案不引入权重（design「无 priority」），
  C1-5 dogfood 混合池统计每模型每格产出，作为配置去留依据。数据已出，见下节「批 C1 dogfood 带出的账」。
- ✅ **resume / partial 报告把 reviewer 格当失败的 scanner 格（批 C 遗留，C1-1 测试暴露，C1-1 后单独修）**：`run_resume` 收口与
  `partial_report` 把 cells 表全量喂 scan-only 收口，`scan_cells_from_records` 把非 scanner 记录判失败 → 假 `scan incomplete` 警告、
  `coverage.failed[Scanner]` 列 reviewer 格；深挖跑完、零 finding、收尾被 kill 的作业 resume 后误判 **Incomplete / exit 3**（应 Approve / 0）。
  修法：两处按角色拆记录，scanner 格走 scan-only 收口，reviewer 格按自身角色进 coverage——resume 往轮成功且本轮未重挖的记
  `reused[Reviewer]`（`DeepRun.reused_cells`），partial 记 completed / failed；partial 的「partial: done/total」只数 scanner 格。

## 批 C1 dogfood 带出的账（2026-09-14，C1-5；同批 C 目标 diff `674583e...f1d3ca3`，明细在私有记录）

墙钟门实证：同 diff、同配置，深挖段 48.4 min（批 C glm 串行）→ **7.1 min**（混合池、agc 门 20，总墙钟 11.2 min）/ 16.1 min（门 4）；
分派池两成员严格交替领项，门收窄与快慢混杂下无死锁。

- **真网关超载有两种表现，D13 只认其一**：12 路并发时 agc 429 × 2，棘轮把门 20 收到 7（两格排队 64 / 78 s 后重试成功，记账正确）；
  同时 grok-4.6 两格 `truncated anthropic stream: no finish_reason`，`DEFAULT_RETRY_BUDGET` 三次尝试全断 → 失败计债。门 4 与批 C 串行零出现 → 负载所致。
  D13 只以 429 收门，断流不收门。候选：同 provider 短窗内断流连发视同过载信号参与收门。配置侧 `providers.agc.maxConcurrency ≤ 7`（run.warnings 已提示，用户决定）。
  ✅ **配置已定（2026-09-15）：agc `maxConcurrency` 20 → 6**。7 不是测出的硬上限——收门公式 `min(门宽 − 1, 我方其余在途)` 的单次读数
  （某次 429 时我方另有 7 个在途），而同轮开头 12 路同发只拒了 2 个；限流更像按速率 / token 或与其他调用方共用额度。
  断流从哪一档起出现未知且无自动恢复，取 6 留一格余量；去掉 grok 后 12 格全是 glm 长流，下一轮 dogfood 看 429 / 断流再调。
  **门 6 实测（2026-09-15，C2 开工前补跑 C1 基线，同目标 diff）**：深挖 t=0 六格同发仍撞 429 × 2 → 收到 4（run.warnings 建议 ≤ 4），
  三格排队重试后成功，12/12 成功、**零断流**；总墙钟 20.6 min（scanner 段 6.2 / 深挖段 14.3）。门 6 保持（自动收门兜底、首波排队）还是改 4，待用户定。
- **流式旁路跨瞬时重试不复位（批 D 接真 sink 前须处理）**：`timed_turn_with` 内部重试复用同一 backend，delta 回调持续喂同一个 `IncrementalFindings`——
  断流那次的半截 JSON 与重试输出拼进同一解析器。sink 为 no-op 时无害；接真 sink 后可能发出重试前的草稿对象。修法二选一：每次尝试复位解析器，或旁路只开在首个尝试。
- ✅ **reviewer 格 quote 锚定丢弃数不可见（C1 评审第 2 档已补：reviewer cell warn 与 scanner 同口径披露丢弃计数）**：`ReviewParseStats.dropped_quote` 算了却不进 `CellRecord`（scanner 格有「N suspicions dropped」warn，reviewer 没有）。
  直接后果：模型去留数据分不清「零发现」与「quote 全锚不上被静默丢」。小修：`land` 里与 `stream discarded` 并列写进 cell warn。
- **reviewer 模型去留数据**：grok-4.6 三轮 22 个成功格 **0 条** finding（C1 两轮 needs 也为 0、输出 1–6K token）；glm-5.3 24 格 26 条（cell 内）。
  混合池无权重，零产出成员直接占掉一半预算格——这是两轮 findings 偏少、波动大的**放大因素之一，不是主因**（主因见下条「深挖选项方差」）。先补上条观测再定配置去留。
  ✅ **决定（2026-09-15）：grok-4.6 暂不作 reviewer**——配置删去 reviewer 角色、保留模型条目（加载时提示「未声明任何 roles」），
  后续再做针对性测试与修复。重测时看 reviewer cell warn 的丢弃计数（C1 评审第 2 档已补），区分「零发现」与「quote 锚不上被丢」。
- **深挖选项方差过大：scanner confidence 排序 + 预算截断（建议作批 C2 开工前拷问议题）**
  - **现象**：C1 两轮同 diff、同配置，报告 findings **2（格内 3，两格同一问题经 dedup 合并）vs 8**。门宽不影响选项与分配（C1 截断在派发前一次定死、无在途入队；
    两轮都严格按派发序交替分给 glm / grok）。两轮**共同挖到的 6 项产出完全一致**（`review_shot.rs` 规则项 glm 两轮各 1 条，其余 5 项两轮都 0）——
    差异全在各自独有的 6 项。
  - **直接原因 = scanner confidence 跨轮漂移**：同一 roadmap 文档的状态行 ① 打 0.63–0.74（高于代码疑点 0.40–0.45）、② 只打 0.50–0.60；预算只取 34 项里的 12 项、
    分数挤在 0.5–0.7 → ① 10 个疑点位里 8 个是文档行（glm 那一半 4 格文档行合计 1 条），② 降到 5 个，代码项与 plan 文档挤进来（`plan.md:261` 一格 4 条）。
    疑点总数几乎不变（42 / 41，该文档都是 27 条），变的只是打分。
  - **confidence 的两处用法都站不住**：
    ① **排序键**——design §6 规则 1「疑点项 = confidence × agreement × 类别权重」、规则 3「confidence 只进优先级不做门」；但预算截断下排序**就是门**。
    决策记录 R11 引 E3「flash 级自评置信与命中无关」（命中项 0.25–0.50、均值 0.36），据此删了置信门槛、却留了排序键。
    ② **喂 reviewer**——`region.rs` `render_notes` 把 `(confidence 0.74)` 写进评审者笔记：已知无信息量的数字当参考，有锚定风险。
  - **只移除 confidence 稳不住**（离线模拟，两轮报告数据，按「文件:行」算重合）：scanner 疑点位置本身跨轮只重合 **39%**；现行排序选中项重合 **25%**；
    不看 confidence 的结构化排序（同文件相邻区间合并 + 代码优先 + 按文件轮转）**13–21%**，未更稳——候选集不同，任何排序都选不出同一批。
    结构化改善的是**分配**：27 条状态行合并成 1 个区域，10 个疑点位里文档区域 ① 8 → 1、② 5 → 0，腾出的格给了代码区域。
  - **四层叠加与候选对策**：
    1. scanner 候选集跨轮波动（39%）→ scanner samples ≥ 2（跨样本合并抬 agreement，配置已支持，成本约翻倍）/ 降温
    2. confidence 排序 + 预算截断把噪声变成门 → 移出排序键与 reviewer 笔记，改用结构化信号（agreement、与 H0 热区 / 文件风险分的关系）
    3. 相邻疑点各占一格、无文件多样性（单文档 27 / 42 条疑点，独占 scanner 格）→ 同文件相邻区间合并成一项、按文件轮转或 per-file 帽
    4. 有效产出格太少放大方差（零产出成员占半、单格离群）→ 成员去留 + 锚定丢弃观测（见上两条）
  - **同分 tie-break 的系统偏向**：`cmp_priority` 同分按 id 字典序，`.next/…` 恒排在 `src/…` 前 → 同分时文档行固定赢代码行
    （② `review_shot.rs:218` 0.55 输给三个同分 roadmap 行）。design「顺序字典序无语义」实际上有偏向。
  - **归属与验收口径**：队列优先级 / 预算规则本身，C1 明确不碰；C2 本就要重做在线预算与疑点保底预留，同一张桌子 → 建议并入 C2 开工前拷问。
    改动的验收指标：同 diff 跑两次，量「选中区域重合度」与「findings 重合度」，不只看单轮 findings 数。
- **并发形态下 kill / resume 用例缺口**（dogfood ② glm 对 C1 旧版 plan 的意见之一，部分成立）：C1 resume 用例为单 InFlight；并发 kill 时多项同时 InFlight、
  部分格已落盘——restore 全部复位 Pending + 「Settled 以成功格为准」理论上覆盖，缺多 InFlight 的端到端用例。C1 评审时顺带补或留 C2。

## 批 C1 收口带出的账（2026-09-14，`/code-review` 15 条修 12 条后留 C2；合回 `feat/m21-review-engine`）

- **Skipped 复位后 `budget_skipped` 不回减 → 假债**（评审 #5，C2 议题）：经入队通道并入把 Skipped 项复位成 Pending 并挖掉后，
  `meta.budget_skipped` 仍按 drain 那一刻计 → `overlay_deep` 仍判债并披露「skipped(budget) N」。与 C1 design §2.3「不回减（drain 时刻披露口径）」
  的裁定冲突——披露口径与计债口径需拆开定。触发要经入队通道，生产 `incoming = None`，**C2 接上流式入队即真**。
- **外部持门者放门时主循环不被唤醒**（评审 #6，C2 议题）：主循环只在「在途格完成 / 通道到达」时重试 `try_lease`；门位被深挖以外的持有者释放
  （C2 起 scanner 与深挖同门）时派发要等下一个在途深挖格完成才补位。探针：门 2、外部先占 1、4 项 × 100 ms → 300 ms（及时补位约 210 ms）。
  C1 的兜底阻塞 lease 只管「在途为空」的情形。C2 做「等待者感知的门让位」时一并设计唤醒源。
- **单 task 并发内的同步 IO 让所有在途格停摆**（评审 #11，C2 议题）：`FuturesUnordered` 同 task 并发，而填门循环里 `region::build`
  每项同步起一次 `git ls-files`（读守卫）并读文件、二阶段 `need_section` 每条 need 同样——阻塞期间其余在途格的流读取与计时器一起停。
  门宽 20 一次填 20 项即 20 次同步子进程；C2 scanner 与深挖重叠后更频繁。候选：`spawn_blocking`，或按 bundle 预取一次 tracked 文件集。
- **与批 D 的接线现状**：C1 未改 `run_deep` 的 `sink` 参数类型（仍 `&dyn FindingSink`，生产传 `&()`）；D 接真 sink 时注意
  `FindingEvent.suspicion_agreement` 是派发快照（`event ≤ landed`，偏差只会多复核），以及旁路跨瞬时重试不复位（上节）须先修。
