# implement · M21a 批 F

> 顺序执行，每 task 一 commit（简体中文 `type(scope): 摘要——细节`），绝不 push；三道门每 task：`cargo test`、`cargo clippy --all-targets`（≤ 基线 7 行）、`make verify`。决策编号见 `prd.md` K1-K15，定论见 `design.md`。

## 0. 预飞
- [x] `git status --short` 空（除本任务目录）、`git branch --show-current` = `feat/m21a-cutover`、`git merge-base HEAD feat/m21-review-engine` = fff7405。
- [x] 基线：lib 552 / 16 / 7 / e2e 17；clippy 7 行；`make verify` 绿。
- [x] Commit：`chore(task): m21a-cutover 规划制品（prd / design / implement / jsonl）`。

## 1. F-1 改名（`Cargo.toml` / `Makefile` / `.gitignore` / `tests/{packaging,review_e2e}.rs` / `src/main.rs` / `CLAUDE.md`）
- [x] `[[bin]] name = "redteam-next"`；六处 `CARGO_BIN_EXE_redteam-next`；Makefile `BIN` / `cp` / 删 `chmod` / `build-dev-baseline` 注释；`.gitignore` 规则；packaging 两处「不含 `redteam-next`」断言；clap `name` / `about` / `after_help`；CLAUDE.md 一句。
- [x] 门 + `make build`（产出 `redteam-next`、`git status` 干净、dev `redteam` sha 不变）。
- [x] Commit：`chore(build): 分支产物改名 redteam-next，make build 不覆盖插件 dev redteam（D10）`

## 2. F-2 scanner 通路（`src/provider/{scan_shot,registry,backends,tool_loop,mod}.rs` / `src/orchestrate/mod.rs` / `src/state/mod.rs` / `src/main.rs`）
- [x] `AssistantTurn.reasoning_tokens` + 16 处构造点 + openai 解析两处。
- [x] `thinking_params` 映射表 + 两 body 构造器 `thinking` 参数 + `with_thinking` + `backend_from_config` 注入 + `thinking_warning`；测试：每 (kind, family) 一断言、`None` 逐字节不变、anthropic budget 夹紧。
- [x] `scan_shot.rs`：`ScanCall` / `run_scan_shot`；测试：首发不用 `turn_final`、parse 失败走 `REPAIR_PROMPT_SCAN` 且 usage 累加、二次失败 `diagnostic_parse_error`。
- [x] registry：scan 表 / `InflightGate` / `insert_scan` / `scan` / `inflight_peaks` / warnings；测试：未注册 Unavailable、同 provider 两条目共享门、峰值计数。
- [x] `CellInput.scan: Option<ScanPrompt>`（`PromptBlock` / `ScanPrompt` 派生 serde）；`CellSink::on_roles`。
- [x] `run_scan_fanout` + `ScanEntrySpec` / `ScanFanoutInput` / `ScanCellResult` / `ScanResults` / `scan_template_hash`；测试：两条目 × 两组 = 四 cell（cell_id / route_key / manifest 含 scan）、workers 1 / 帽 8 → 峰值 1、帽 2 / workers 4 → 峰值 ≤ 2、失败 cell 入 `failed` + burned usage、warn 拼接（dropped / 零推理）、`on_cell` 逐格回调、needs 计数。
- [x] `build_registry` 改 `insert_scan`（旧 `insert` 路径与 `legacy_opts_from_roles` 校验暂留到 F-4）。
- [x] Commit：`feat(orchestrate): scanner 扇出——组 × 条目 × samples，route_key 按角色条目，thinking_level 首个消费者，provider 在途锁（design §5 / §12.3）`

## 3. F-3 并集与报告（`src/consolidate/{scan,mod,dedup}.rs` / `src/domain/report.rs` / `src/cli/tail.rs` / `src/output/mod.rs` / `src/bundle/mod.rs` / `src/pack/mod.rs`）
- [x] `merge_suspicions` + `MergedSuspicion`；`dedup::{QUOTE_SIM, LINE_GAP}` 与 `orchestrate::normalize_quote` 升 `pub(crate)`。
- [x] `Verdict::ScanOnly`、`ScanReport` / `ScanGroup` / `ScanStats`、`ReviewReport.{scan, cells}`、`RunMeta.grouping`、`CellSummary` 三字段、`GroupingStats` / `Gate` / `PackStats` 补 `Deserialize`；`exit_code` 臂；md 渲染。
- [x] 测试：两家族 agreement 2 / 同家族两 samples 1 / 跨组不合并 / 半重叠合并 / 排序确定；`exit_code(ScanOnly)`；md 快照；`ReviewReport` 序列化含 `cells[].output.role == "scanner"`。
- [x] Commit：`feat(consolidate): 疑点并集与 agreement（家族数）+ scan-only 报告（design §5〔D7〕）`

## 4. F-4 硬切（`src/cli/{mod,tail,incremental,second_pass}.rs` / `src/orchestrate/mod.rs` / `src/prompt/{mod.rs,templates/}` / `src/bundle/mod.rs` / `src/main.rs` / `tests/review_e2e.rs` / `src/provider/registry.rs`）
- [x] `run_code_review` 切 scan 路径（`finish_scan_report` 共用）；`scanned_coverage`；`run_resume` scan 重放；`partial_report`；`run_incremental_review` bail；`run_pack_only` 改 `build_scan_prompt`（K15）；`build_packs` 产裸 pack 文本。
- [x] 删：orchestrate 旧扇出与 13 测试；`legacy_opts_from_roles` 与两测；`TailCtx` / `assemble_report` / `mark_dismissed` / `CarryCtx` 与测试；`second_pass_prompt` 与其 prompt 文本测试；`build_bundle_prompt` / `CODE_BUNDLE_TPL` / 模板文件 / `legacy_bundle` / `render_dossier_section` + 旧前言 / `DOSSIER_STRUCTURAL_TAGS`（若无他用）/ `render_incremental_section` 与测试群；`two_layer_manifest` 与测试；`consolidate` 私有项 `#[allow(dead_code)]`。
- [x] `--help` 文案（K13）；`build_registry` 去 legacy 校验；`JobCellSink` `Phase::Scan` + `on_roles`。
- [x] e2e：四例改 scan、两例 `#[ignore]`、一例删、`result_md_three_states` 补字段、`pack_only_*` 回归；cli 单测（`code_review_tests` / `spec_tests` / `job_tests` / `resume_tests`）改 scan fake。
- [x] 门：全量绿、clippy ≤ 基线、`--help` 无 `--models`、`adversarial-code-bundle.md` 不存在。
- [x] Commit：`refactor(orchestrate): 硬切旧 fan-out / 候补链 / 单发 finding 路径——scanner 疑点路径唯一（铁律 6〔D10〕）`

## 5. F-5 观测落盘（`src/cli/mod.rs` / `src/state/mod.rs` / `src/domain/report.rs`）
- [x] `Phase::Scan`；RunMeta 全表填值（design §5）；`ReviewReport.cells` 前台 / job / resume 三路都填；`attach_cells` 扩字段。
- [x] 测试：RunMeta 快照（coverage / template_hashes / cache_hit_by_cell / provider_inflight_peak / grouping / phase_history）；一 cell 失败 → Incomplete / 3；覆盖率三例。
- [x] Commit：`feat(state): scanner cell 落盘 + RunMeta 观测（分组 / 疑点数 / 覆盖率 / cache / 墙钟 / 在途峰值 / coverage 五集合）`

## 6. F-6 收口
- [x] dogfood：`cargo run -q -- review --base feat/m21-review-engine --format json > /tmp/rt-scan.json`（真实 scanner，K14）；`python3 eval/scan_score.py selftest` + 对 dogfood 输出 `score` 读取（合成 oracle）；记录 cells / 疑点数 / 覆盖率 / 墙钟 / thinking warning 到 §收口记录。
- [x] code-review 子 agent（范围 `feat/m21-review-engine...HEAD`）→ 修 → §评审修。
- [x] followup 登记（resume 生产验证 / GLM anthropic thinking 映射 / `result.md` 文案 / 增量与 dismissed e2e ignore / `Stats` 恒零）；roadmap `M21a-scan-foundation.md` 交付项 6 / 7 / 8 / 9 打 ✅、验收段注明出口另起。
- [x] journal（8591293）；（用户）`--no-ff` 合回 `feat/m21-review-engine`（2026-09-09 完成）；task archive 于 2026-09-10。

## 收口记录（F-6 填，2026-09-09）

dogfood：`cargo run -q -- review --base feat/m21-review-engine --format json`（本分支 F-1…F-5 + 制品，diff 3 组：4 / 4 / 26 文件；`--pack-only` 预估 cell-3 782 KB ≈ 224K token、over_budget=1）；活跃 scanner = `deepseek/deepseek-v4-flash`（thinkingLevel high → `thinking enabled` + `reasoning_effort max`）+ `qwen/qwen3.8-flash`（medium → `enable_thinking true`）。

| 项 | 结果 |
|---|---|
| verdict / 退出码 | scan-only / 0；warnings 0（两家族 thinking 映射都命中，无「无映射」告警） |
| cells | 6 = 3 组 × 2 条目，全部 ok（coverage selected 6 / completed 6 / failed 0）；provider 在途峰值 deepseek 2 / qwen 3 |
| 墙钟 | 366.5 s（最慢 cell：qwen g3 288 s；deepseek g3 137 s）；每 cell 一轮（iterations 1，无 REPAIR） |
| usage | input 496,869 / output 77,092 / cached 2,048（qwen 两格各 1,024；deepseek 三格 0——`prompt_cache_hit_tokens` 未命中或该网关不回报） |
| 疑点 | 合并后 57（agreement ≥ 2：6）；by_category L0 5 / L1 7 / L2 9 / L3 8 / L4 20 / L5 8；needs 21（6 cell 全触发） |
| 覆盖自报 | scanned_coverage 0.868（改动行 ∩ 自报区间） |
| 锚定丢弃（cell warn） | deepseek g1 1 / g2 2+2（unreadable：已删除的 `adversarial-code-bundle.md` 一类）/ g3 4；qwen g2 1+1 |
| 评分器 | `eval/scan_score.py selftest` ok；对本报告 + 合成 oracle（3 条：Cargo.toml / Makefile 两条命中锚、1 条不存在文件）`score` 读取顶层 `cells[]` 形态成功（结果见 `/tmp/rt-score.json`，仅验读取，非 oracle 评分） |
| `.redteam/` | 前台评审不落盘（无 job）；`--pack-only` 落 `packs/<job>`（gitignored） |

观察（不改代码，记 followup / 出口）：① g1 / g2 的疑点几乎全落在 `.trellis` 制品与构建文件上（本分支 diff 含大量文档）——出口用 PR181 才有代码语料；② DeepSeek 三格 cached 0：三组 prompt 共享同一共享头（≥ 64 token），按其磁盘缓存应命中——出口 4 对照 dev 基线时看 `prompt_cache_hit_tokens` 字段是否被该网关回报；③ qwen 单格 288 s（输出 22K token，medium thinking）是墙钟大头，出口 1「≤ 3 min」用 DeepSeek 单 scanner 配置跑。

## 评审修（F-6 填，2026-09-09，`/code-review high`，范围 feat/m21-review-engine...HEAD，SendMessage 钉范围）

8 角度 39 候选 → 33 送验证 → 存活 29，按上限报 10 条（正确性优先）+ 帽外已验证项。十条全部核实成立，一次修掉（commit「fix(orchestrate,cli,provider): 评审修复…」）：
1. `run_resume` 对旧格式作业（partial 全是 reviewer 记录且 ok）会产出空 scan-only 覆盖原报告 → 任一非 scanner 记录即拒 resume（`partial_report` 带理由，不覆盖）；`scan_cells_from_records` 把 ok 的非 scanner 记录翻成失败格。
2. `partial_report` 只喂 ok 记录 → 失败格从 coverage.failed / cells_failed / usage 消失、与 `attach_cells` 回填自相矛盾 → 全部记录进收口。
3. `family_class` 把 gpt-4 系归推理型发 `reasoning_effort` → 400 且无 warning → `thinking_params` 加 id 参数：gpt-5 / o 系 / 显式 family=openai 才映射，其余 gpt → `gpt-legacy` 无映射 + warning。
4. `merge_suspicions` 就地换代表导致簇归属依赖输入顺序（plan 序 vs db 序两路不同）→ 并查集传递闭包 + 输入按 cell id 排序 + 代表末选（六种排列测试）。
5. `run_resume` 贫化收口（组骨架 / 分组 / head_sha / 覆盖自报 / dossier 全 None、丢 registry warnings、单格 warn 契约不同）→ 抽 `orchestrate::scan_cell_record` 两路共用；沿用作业原报告的 groups / grouping / head_sha / dossier / scanned_coverage；并入 `registry.warnings()`。
6. resume 重放冻结 prompt 却盖当前模板哈希 → `CellInput.template_hash` 随 manifest 落盘，重放优先取之，与当前不一致 warning。
7. workers 许可在 provider 门前被占（饱和 / 限流 provider 饿死其他 provider）→ registry `lease`（先门后 worker 许可）；在途峰值改在真正发请求时计（等 worker 的租约不计）。
8. glm Off 对 4.x 仍发 enabled+low → 按 id 区分：GLM-5.x 不可关保留迁移口径，其余 Off 真关（`thinking disabled`）。
9. `thinking_pinned` 不看映射 → 无映射家族误报零推理 → `ScanEntrySpec::from` 只在 `thinking_params` 有非空映射时为 true。
10. 覆盖率分母含删除点（纯删除恒 0%、混合低报）→ 只计 `+` 行；纯删除 → None。
帽外已验证并修：`--incremental-from --background` 先建 job 再在 worker 里 bail → main 层早拒（e2e 锚）；琐碎 quote（`}`）经 min-set 公式误合并 → 去空白 < 12 字符只认相等；hint 换行伪造 md bullet → 渲染单行化；`CellSummary` 两处手写映射 → `CellSummary::from_record`；`CellInput.prompt` 与 `scan` 双存（每 cell MB 级）→ scan cell 留空。
未改（记 followup）：Anthropic thinking + tools 互斥守卫（M21b 工具往返才可达）；`resolve --help` 仍提增量轮消费（M21c）；`exit_code` 第二参在 scan 路径恒 false；`ScanResults.usage / failed / needs_declared` 生产无消费者（测试用）；`lines_overlap` 与 `dedup::overlaps` 同形不同型；`anchor_suspicions` 每疑点重读同文件。
修后：lib 531 / 16 / 7 / e2e 17 绿；clippy 三条既有；make verify 绿。

## 7. 出口验收（2026-09-09，另起 session；plan 表 4；记录 `.next/private/m21a/2026-09-09-m21a-exit-acceptance-record.md`）

- [x] 1-10 全部跑完：8 过；验收 1 的分组（7 > 5）与墙钟（311 / 403 / 458 s > 3 min）未过——成因、数据与处置选项登记 followup「M21a 出口验收延后项」，roadmap M21a 状态 🟡 待裁。
- [x] 顺手修两处观测缺口（本 session 改码，lib 532 / e2e 17 / clippy 3 = 基线）：`parse_anthropic_stream` 对 `message_delta` 的 input / cache 分列在场即覆盖（agc 网关 GLM 格 usage 恒 0）+ 单测；`ScanCall.repair_note` 首发解析失败诊断进 cell warn（dogfood 一格三跑全 REPAIR、直发三次皆合法，待诊断定位）+ 断言。
- [x] research §17 dev 对照基线、§18 E2 slice vs full；E3 评分基线 → `score-slice.json`（改模板后 `--baseline` 用）。
- [x] 用户裁（2026-09-09）：墙钟按 workers / 并发开大后可接受、门槛改按「排队 + 最长格」可读；分组「≤ 5」撤、改可观测项，拆大格保持 followup 待 aacr-bench；workers 形态（自适应规则 / 改命令参数不设配置键）留 grill。
- [x] 出口修补批：排队告警（cell 数 > workers → run.warnings）、`CellRecord/CellSummary.queue_wait_ms`（含在 duration_ms 内）、零疑点短输出 cell warn（阈值 2,000 token）、`review --help` 并发说明；lib 533 / clippy 3 / e2e 17 / make verify 绿；roadmap M21a ✅、README 同步。
- [x] 墙钟 A/B（2026-09-09 09:46）：`run_scan_fanout` 大格先发（按组 diff 字节降序派发、plan 序回收）+ 单测；A 现状 311 / 402 / 457 → B1 workers 4：283 / 247 / 234 → B2 workers 8：189 / 202 / 251 s。scanner 并发的约束门是 `review.scanners.workers`（缺省 4），provider `maxConcurrency` 不是；lib 533 / e2e 17 / clippy 3。
