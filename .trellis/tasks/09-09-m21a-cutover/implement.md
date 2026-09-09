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
- [ ] `merge_suspicions` + `MergedSuspicion`；`dedup::{QUOTE_SIM, LINE_GAP}` 与 `orchestrate::normalize_quote` 升 `pub(crate)`。
- [ ] `Verdict::ScanOnly`、`ScanReport` / `ScanGroup` / `ScanStats`、`ReviewReport.{scan, cells}`、`RunMeta.grouping`、`CellSummary` 三字段、`GroupingStats` / `Gate` / `PackStats` 补 `Deserialize`；`exit_code` 臂；md 渲染。
- [ ] 测试：两家族 agreement 2 / 同家族两 samples 1 / 跨组不合并 / 半重叠合并 / 排序确定；`exit_code(ScanOnly)`；md 快照；`ReviewReport` 序列化含 `cells[].output.role == "scanner"`。
- [ ] Commit：`feat(consolidate): 疑点并集与 agreement（家族数）+ scan-only 报告（design §5〔D7〕）`

## 4. F-4 硬切（`src/cli/{mod,tail,incremental,second_pass}.rs` / `src/orchestrate/mod.rs` / `src/prompt/{mod.rs,templates/}` / `src/bundle/mod.rs` / `src/main.rs` / `tests/review_e2e.rs` / `src/provider/registry.rs`）
- [ ] `run_code_review` 切 scan 路径（`finish_scan_report` 共用）；`scanned_coverage`；`run_resume` scan 重放；`partial_report`；`run_incremental_review` bail；`run_pack_only` 改 `build_scan_prompt`（K15）；`build_packs` 产裸 pack 文本。
- [ ] 删：orchestrate 旧扇出与 13 测试；`legacy_opts_from_roles` 与两测；`TailCtx` / `assemble_report` / `mark_dismissed` / `CarryCtx` 与测试；`second_pass_prompt` 与其 prompt 文本测试；`build_bundle_prompt` / `CODE_BUNDLE_TPL` / 模板文件 / `legacy_bundle` / `render_dossier_section` + 旧前言 / `DOSSIER_STRUCTURAL_TAGS`（若无他用）/ `render_incremental_section` 与测试群；`two_layer_manifest` 与测试；`consolidate` 私有项 `#[allow(dead_code)]`。
- [ ] `--help` 文案（K13）；`build_registry` 去 legacy 校验；`JobCellSink` `Phase::Scan` + `on_roles`。
- [ ] e2e：四例改 scan、两例 `#[ignore]`、一例删、`result_md_three_states` 补字段、`pack_only_*` 回归；cli 单测（`code_review_tests` / `spec_tests` / `job_tests` / `resume_tests`）改 scan fake。
- [ ] 门：全量绿、clippy ≤ 基线、`--help` 无 `--models`、`adversarial-code-bundle.md` 不存在。
- [ ] Commit：`refactor(orchestrate): 硬切旧 fan-out / 候补链 / 单发 finding 路径——scanner 疑点路径唯一（铁律 6〔D10〕）`

## 5. F-5 观测落盘（`src/cli/mod.rs` / `src/state/mod.rs` / `src/domain/report.rs`）
- [ ] `Phase::Scan`；RunMeta 全表填值（design §5）；`ReviewReport.cells` 前台 / job / resume 三路都填；`attach_cells` 扩字段。
- [ ] 测试：RunMeta 快照（coverage / template_hashes / cache_hit_by_cell / provider_inflight_peak / grouping / phase_history）；一 cell 失败 → Incomplete / 3；覆盖率三例。
- [ ] Commit：`feat(state): scanner cell 落盘 + RunMeta 观测（分组 / 疑点数 / 覆盖率 / cache / 墙钟 / 在途峰值 / coverage 五集合）`

## 6. F-6 收口
- [ ] dogfood：`cargo run -q -- review --base feat/m21-review-engine --format json > /tmp/rt-scan.json`（真实 scanner，K14）；`python3 eval/scan_score.py selftest` + 对 dogfood 输出 `score` 读取（合成 oracle）；记录 cells / 疑点数 / 覆盖率 / 墙钟 / thinking warning 到 §收口记录。
- [ ] code-review 子 agent（范围 `feat/m21-review-engine...HEAD`）→ 修 → §评审修。
- [ ] followup 登记（resume 生产验证 / GLM anthropic thinking 映射 / `result.md` 文案 / 增量与 dismissed e2e ignore / `Stats` 恒零）；roadmap `M21a-scan-foundation.md` 交付项 6 / 7 / 8 / 9 打 ✅、验收段注明出口另起。
- [ ] journal；（用户）`--no-ff` 合回。

## 收口记录（F-6 填）

## 评审修（F-6 填）
