# M16 前清理批 Implementation Plan（doc 移除 + SCIP 拆除 + 仪器拆除/简化）

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps use checkbox 跟踪。

> 状态: 锁定 · 决策=E10（doc 移除）+ E8-E（SCIP 安乐死）+ E11 执行次序 · base=dev（批 4 合并后 8dcaf6f）· 用户定序 2026-07-14：本批=次序①②，安全组=次序③（另 plan）

**Goal:** 删除 E10/E8-E 判决的死代码（doc 模式、SCIP 引擎），拆除/简化测量目的已达成的 dev 仪器——为 M16 安全组与发布腾出干净地基。

**Architecture:** 四段净删除，无新机制：①doc 模式整线删（build_doc_prompt/D-lens/run_doc_fanout/doc 模板/mode 推断 doc 支/`--mode doc`）；②SCIP 引擎删（src/scip/、build_pack_scip/scip_refs/callee_section、PackEngine 枚举塌成单 regex 路径）；③Rust env 仪器：整拆测量已完成的 env 门、保留生产观测与生产旋钮；④M12 dogfood 自反馈仪器整拆（纯 prompt 层：feedback.md + ship.md 三处 + codex 镜像）。

**Tech Stack:** Rust 单 crate；无新依赖；纯删除。

## Global Constraints

- 注释简体中文；`cargo test --lib` 全绿 + `cargo test --test review_e2e`（7）；`cargo clippy --all-targets 2>&1 | grep -c "warning:"` ≤ **8**；commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`；**永不 push**。
- **删除纪律**：每项删除后 grep 全仓确认零残留（函数名/字段/env 名/`--mode`/`--lenses` 类）；钉「被删行为」的测试随删，钉「存留行为」的改写保留。
- 保留不动：`REDTEAM_HTTP_TIMEOUT_SECS`（生产旋钮）、`REDTEAM_SMOKE_ANTHROPIC_KEY`（smoke 靶）、生产观测 `Usage`/`needs_stats`/`iterations`、`REDTEAM_PACK_BUDGET`(若有)/预算函数。
- 涉 `plugins/redteam/commands/` 改动 → `make gen` 重生成 codex-skills → `make verify` 0 FAIL。

---

### Task 1: doc 模式整线删除（E10）

**Files:** `src/prompt/mod.rs`(build_doc_prompt+DOC 模板常量)、`src/prompt/templates/adversarial-doc-bundle.md`+`adversarial-refute-doc.md`(删文件)、`src/orchestrate/mod.rs`(run_doc_fanout)、`src/lens/`已删无关、`src/consolidate/verify.rs`(refute doc 分支)、`src/cli.rs`(mode 推断 doc 支/run_review doc 路径)、`src/main.rs`(`--mode` clap arg 的 doc 值/mode 参数)、`src/domain`(Mode enum 若有 Doc 变体)、`src/evidence/mod.rs`(doc targets 收集)

**Interfaces:** Produces: 全仓无 Mode::Doc、无 `--mode doc`；review 恒 code 模式。

- [ ] Step 1: 查 `Mode` enum 定义与 Doc 变体全部消费点：`grep -rn "Mode::Doc\|mode.*doc\|--mode\|run_doc_fanout\|build_doc_prompt" src/`，列全删除面。
- [ ] Step 2: 删 `run_doc_fanout`+其测试、`build_doc_prompt`+其测试、两个 doc 模板文件、verify 的 doc 分支、evidence 的 doc targets 收集。
- [ ] Step 3: `Mode` enum——若仅 Code/Doc 二值则整个 Mode 抽象可塌（review 恒 code）；若 Mode 还承载别的语义则只删 Doc 变体。以代码为准，倾向塌掉 Mode（YAGNI）。
- [ ] Step 4: main.rs 删 `--mode` clap arg；mode 推断逻辑删（不再有 doc/code 歧义）。`--spec` **保留**（code 模式对照 spec 文档，E10 收窄后的唯一 doc-ish 输入）。
- [ ] Step 5: **base/scope 转必选**——`--base` 缺省行为核查：现「无 base→working-tree」保留 or 转必选？裁定=**保留 working-tree 默认**（评当前未提交改动是合法常用场景），但 mode 恒 code（歧义源是 doc-target-vs-base，doc 删后消失）。删 mode 推断即可，base/scope 语义不动。（注：E10 提「转必选」是 doc 删的可选连带；实评估后 working-tree 默认有用、保留，只删 mode 歧义。）
- [ ] Step 6: doc lens（D1-D5）——lens 已 E8-I 去实体化为模板固有文本；doc 模板删后 D-lens 文本随之消失，无独立代码。确认 `src/lens` 已空（批 4 删）、无残留 D 引用。
- [ ] Step 7: `grep -rn "Mode::Doc\|--mode\|doc_fanout\|doc_prompt\|adversarial-doc" src/ = 0`；`cargo test --lib && cargo test --test review_e2e`；clippy ≤8；涉 commands 则 make gen+verify。
- [ ] Step 8: Commit `feat(cli): 移除文档评审模式（E10）——范式冲突，交互式 grill 工具承接`（trailer）。

---

### Task 2: SCIP 引擎拆除（E8-E 安乐死）

**Files:** `src/scip/mod.rs`(删文件)、`src/lib.rs`(`mod scip` 删)、`src/pack/mod.rs`(build_pack_scip/scip_refs/callee_section/1壳 refs_section 若随 E8-E 一并判死)、`src/cli.rs`(PackEngine 枚举/pack_engine()/REDTEAM_PACK)、Cargo.toml(scip crate 依赖若仅此用)

**Interfaces:** Produces: 唯一 pack 路径=`build_pack`（regex 0 壳全文+梯子+省略）；无 PackEngine 分派。

> **命名陷阱（"dossier" 四重语义，删错伤核心）**：
> - **删**：`PackEngine::Dossier` 臂（`REDTEAM_PACK=0`，cli 老 A/B 基线）→ 随之 `dossier::build_dossier`（仅此臂用）+ `dossier::reference_points`（仅 build_dossier / 本 task 删的 1壳 refs_section / scip_refs 兜底用，删后全死）+ `pack::scip_refs`/`callee_section`/`build_pack_scip` 及其测试。
> - **保留（勿动）**：①`prompt::render_dossier_section`——这是**输出 XML 包装 `<dossier>…</dossier>`**，所有 pack 交付与主二阶段路径（cli:570）都用它，与 Dossier 引擎臂无关，且是 ②安全向量中和点；②`src/dossier/mod.rs` 的 diff/符号解析核心（`hunk_changed_lines`/`hunk_anchors`/`enclosing_def_start`/`compile_defs`/`lang_for`/`symbols_in_file`）——梯子骨架档与 verify 依赖，保留。
> - **不碰**：`REDTEAM_NO_DOSSIER`/`suppressed`/`DossierMeta.suppressed` 归 Task 3（本 task 只把 build_packs 的 `match (&engine, &scip_index)` 塌成单 `build_pack` 调用，`suppressed` 分支原样留给 Task 3）。

- [ ] Step 1: 删 `src/scip/mod.rs` + `mod scip`；删 `build_pack_scip`/`scip_refs`/`callee_section` + 其测试。
- [ ] Step 2: **PackEngine 枚举塌缩**——现 Regex/Dossier/Scip{strict} 四臂。Scip 删、`Dossier`(REDTEAM_PACK=0 老基线臂)是死仪器一并删 → 只剩 Regex → 枚举与 `pack_engine()`/`REDTEAM_PACK` env 整个删,`build_packs` 直调 `build_pack`。
- [ ] Step 3: **1 壳 refs_section 去留裁定**——E8-E 判 1 壳/SCIP 无边际价值。refs_section（regex 引用点，`REDTEAM_PACK_SHELLS=0` 曾用于减法臂）随 SCIP 判决一并删：`build_pack` 的 1 壳段删、all_syms/reference_points 调用删。0 壳全文+梯子+省略=唯一内容。**这也消掉一处语言依赖**（compile_defs 在 1 壳的用途）。保留 compile_defs 供梯子 def 体/骨架（超预算降级仍需）。
- [ ] Step 4: Cargo.toml——`scip`/protobuf 类依赖若仅 scip 模块用则删,`cargo build` 确认。
- [ ] Step 5: `grep -rn "scip\|Scip\|SCIP\|PackEngine\|REDTEAM_PACK\b\|build_pack_scip\|refs_section\|callee_section" src/ = 0`(注释/历史文档级残留除外)；测试全绿；clippy ≤8。
- [ ] Step 6: Commit `feat(pack): 拆除 SCIP 引擎+1壳引用点（E8-E 安乐死）——唯一路径 0壳全文+梯子`（trailer）。

---

### Task 3: dev 仪器拆除/简化（E11 次序②）

**Files:** `src/provider/tool_loop.rs`(REDTEAM_TOOL_LOG marker)、`src/tools/mod.rs`(tool_log fn)、`src/pack/mod.rs`(REDTEAM_PACK_SHELLS/PACK_BODY,body_section symbol_mode)、`src/dossier/mod.rs`(REDTEAM_NO_DOSSIER)、`src/cli.rs`(相关门)

**规则（用户 2026-07-14）**：整拆测量目的已达成/不再可测的；无工具后仍需/可测的测量仪器做简化适配（保留仪器、移除其内不再需要的功能/指标）。

- [ ] Step 1: **REDTEAM_TOOL_LOG 整拆**（残余读解剖仪器,目的=驱动无工具重设计,已达成；无工具默认下无 tool 调用可记,仅 tools=on 罕见诊断档有）：删 tool_loop marker 埋点 + `src/tools/mod.rs` `tool_log` fn。
- [ ] Step 2: **REDTEAM_PACK_SHELLS 整拆**（E8-E 减法臂,判决已落,且 1壳已随 Task 2 删）。
- [ ] Step 3: **REDTEAM_PACK_BODY=symbol 整拆**（粒度 A/B S 臂,判决已落=全文默认/符号非降级档；`body_section_impl` 的 `symbol_mode` 参数与分支整删,body_section 恒全文路径）。
- [ ] Step 4: **REDTEAM_NO_DOSSIER 整拆**（S1/M14-D9 老 null 臂 A/B toggle,死仪器）。**级联（非单纯 env 删）**：`build_packs` 的 `let suppressed = …NO_DOSSIER…` + `if suppressed { String::new() }` 分支删（塌成恒 `render_dossier_section(build_pack(...))`）；`DossierMeta.suppressed` 字段删（`src/domain/report.rs`）；`DossierMeta { bundles, injected, suppressed }` 构造去 suppressed；cli.rs:2103-2147 及 report.rs:224 等测试同步。**保留 `DossierMeta.bundles`/`injected`**（生产观测，cli 注释「唯一可观测入口，供归因/诊断」）——只删 suppressed。（注：`render_dossier_section` 是输出包装，勿与本 env 混淆，不动。）
- [ ] Step 5: **生产观测保留不动**：Usage/needs_stats/iterations——这些非 dev 仪器,是发布后可观测,留。**needs_stats 简化核查**：E8-E needs 触发率指标其宿主(1壳预防针假说)已死,但 needs 触发率本身仍是二阶段生产观测(显示补查是否发生)→保留、不简化。
- [ ] Step 6: **M12 dogfood 自反馈 → 独立 Task 4**（用户 2026-07-15 定=整拆）。本 task 只管 Rust env 仪器；M12 纯 prompt 层，见 Task 4。
- [ ] Step 7: `grep -rn "REDTEAM_TOOL_LOG\|REDTEAM_PACK_SHELLS\|REDTEAM_PACK_BODY\|REDTEAM_NO_DOSSIER\|symbol_mode\|tool_log" src/ = 0`；`grep REDTEAM_HTTP_TIMEOUT_SECS\|REDTEAM_SMOKE` 仍在(保留)；测试全绿；clippy ≤8。
- [ ] Step 8: Commit `chore: 拆除测量已达成的 dev 仪器（TOOL_LOG/PACK_SHELLS/PACK_BODY/NO_DOSSIER）`（trailer）。

---

### Task 4: M12 dogfood 自反馈仪器整拆（用户 2026-07-15 定=整拆）

**Files:** `plugins/redteam/feedback.md`(删文件)、`plugins/redteam/commands/ship.md`(三处删)、`plugins/redteam/codex-skills/ship/SKILL.md`(make gen 重生成)

**Interfaces:** Produces: ship 循环无 feedback subagent 派发、无 observations 约定、无 rN.result.json 反馈存档；ship 核心循环（轮询/裁决/停机/md 报告）不动。纯 prompt 层，src 零消费（`grep REDTEAM_FEEDBACK src/ = 0` 已验）。

**性质：编辑判断活**——ship.md 里 feedback 仪器与核心循环交织，需甄别删净而不伤核心。dev-time 仪器（M12 记分卡本标「发布前移除」），无工具已真实战场验证、dogfood-as-practice 由交互式重跑承接，整拆。

- [ ] Step 1: 删 `plugins/redteam/feedback.md`（记分卡 prompt 全文，6.4KB）。
- [ ] Step 2: `ship.md` §收工反馈整节删——`## 收工反馈（本地调试阶段 · 发布前移除）` 标题起至该节末（feedback subagent 派发全段，约 :139 起至文件尾/下一节前）。
- [ ] Step 3: `ship.md` **工具观察** bullet 删——`- **工具观察（本地调试阶段 · 发布前移除）**：…observations.md 追加一行…无异常不写。`（约 :108-110 整条）。
- [ ] Step 4: `ship.md` 外循环「取结果」步——**⚠️ 更正（controller 2026-07-15 复核）：rN.result.json 存档实为双用途，NOT feedback 专属，不整删**。dismiss（:82 `--from rounds/<session>/rN.result.json`）与 resolve（:97 同）都读它；文档里「此文件存档仅供收工反馈的 jq 配方读取——发布前随 feedback 仪器一并拆除本步」（:55-56）是**自相矛盾的错误归因**，按字面整删存档会打断 dismiss/resolve。→ **只删/改写这句误导括注**：去掉「仅供收工反馈的 jq 配方读取——发布前随 feedback 仪器一并拆除本步」，保留「存档到 `rN.result.json`（结果已持久于 proj.db 可随时重取）」的存档步本身，并可补注真实消费者（dismiss/resolve 的 `--from`）。**保留** failed/timed_out 子 bullet（:57-60）与 md 报告步（:102）。
- [ ] Step 5: `make gen` 重生成 `codex-skills/ship/SKILL.md` 镜像 → `make verify` 0 FAIL。
- [ ] Step 6: `grep -rn "feedback\|observations.md\|记分卡\|REDTEAM_FEEDBACK" plugins/redteam/commands/ plugins/redteam/codex-skills/ = 0`（git 历史级除外）；`ls plugins/redteam/feedback.md` 不存在；ship 核心循环节（内循环/停机/刹车链/md 报告）目检未伤。
- [ ] Step 7: Commit `chore: 移除 M12 dogfood 自反馈仪器（feedback.md + ship 记分卡段）——无工具已实战验证，dogfood 由交互重跑承接`（trailer）。

---

## 验收（controller）
- 清理批合前:`redteam review --base dev`(本仓或 hiapi worktree)单跑一发确认正常评审未回归(单格/梯子/needs 都在、verdict 可读)。纯删除批风险低,一发 smoke 够。
- doc 删后 `redteam review --help` 无 `--mode`;SCIP 删后 `REDTEAM_PACK=scip` 无效(env 已删)。

## Self-Review
- E10 doc 移除 → Task 1;E8-E SCIP → Task 2(含 1壳/refs);Rust env 仪器 → Task 3;M12 dogfood 自反馈 → Task 4(用户 2026-07-15 定=整拆,纯 prompt 层拆为独立 task,reviewer 可独立否决)。base/scope「转必选」实评估后改为「保留 working-tree 默认、只删 mode 歧义」(Step 1-5 注)。语言依赖:Task 2 删 1壳消一处,残留只剩梯子 def 体/骨架(超预算触发,compile_defs 保留)。
