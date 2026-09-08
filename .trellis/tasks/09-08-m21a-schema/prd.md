# M21a 批 B · schema v4（Trellis `m21a-schema` · 子分支 `feat/m21a-schema`）

> 上位：`.next/plans/2026-09-07-redteam-m21a-scan-foundation-plan.md` 批 B（Task B-1 ~ B-5）· design v8 §4 / §8.3-8.4 / §15 ·
> 决策 D10-2（proj.db v4 一次重置、先导出）· D11-7（R14：cell 带 `role` + 角色分叉输出枚举；单独表只有疑点台账与队列快照；
> coverage 进作业元数据）。波次 1，与批 A（`feat/m21a-config`）、批 C（`feat/m21a-grouping`）并行；批 D 依赖本批（疑点类型在 domain）。

## Goal

把数据契约换到 M21 形状：疑点 schema 与按角色分叉的 cell 输出枚举进 domain；`CellRecord` / `proj.db` 一次重置到 v4
（cell 带 role、疑点台账与队列快照表壳）；`RunMeta` 预留 coverage 五集合与观测字段；重置前把旧库 dismissed / upheld findings
导出为入库固定集（verifier 离线回归集数据，D10）。本批**只换形状不接线**：不产疑点、不写台账、不填 coverage（F-5 / M21b-c）。

## 背景与已确认事实（仓内证据）

- **旧库**：`~/hobby/redteam/.redteam/proj.db`（主 checkout，本工作树无 `.redteam/`）。`PRAGMA user_version = 0`；表
  dismissed / dismissed_history / resolved / resolved_history / jobs / cells / results（+ sqlite_sequence）；
  dismissed 63 行（status 全为 `dismissed`）；results 26 行（schema_version "2"×2、"3"×24）；其中 `verify.verdict = upheld`
  的 finding **48 行**（按 job×finding 计；去重 id 为 42），refuted 65；cells 131 行；resolved 3 行。
  jobs.workspace 只有 `/Users/weilai/hobby/{redteam,breakpoints}`，upheld/dismissed 的 file 顶层目录全在本仓——**无公司仓内容**，固定集可入库。
- **建表**只在 `src/db.rs:60-119` `open_at`（`CREATE TABLE IF NOT EXISTS` ×7，无 `user_version`）；`open`（`db.rs:53`）与
  `project.rs:53`（`init`）、`main.rs:1421+`（`--dismissed <path>`）都经 `open_at`。
- **CellRecord**（`src/state/mod.rs:149-190`）：`findings: Vec<Finding>` 平铺，无 role；消费点：`src/orchestrate/mod.rs:133`
  `to_record`、`src/cli/mod.rs:445` `attach_cells`、`:464-471` `partial_report`、`:542-604` `run_resume`（两处字面量构造）、
  `src/cli/incremental.rs:212`、`src/main.rs:1207`（`status --cells` 直接 dump JSON）、测试 `src/cli/mod.rs:1045/1091/1201/1525/1580/2109/2138`、
  `src/orchestrate/mod.rs:677-683`、`src/cli/incremental.rs:327`、`tests/review_e2e.rs:294`。
- **NeedEntry**（`src/domain/finding.rs:48-54`）只 `Deserialize + JsonSchema`，无 `kind`；字面量构造点：`src/orchestrate/mod.rs:444`、
  `src/cli/second_pass.rs:239/255/277-278/287/311`。`LensOutput`（`finding.rs:57-63`）由 `src/provider/parse.rs:10` 消费，本批不动。
- **schema_version** 字面量 `"3"`：`src/cli/mod.rs:473,2155`、`src/cli/tail.rs:167`、`src/cli/incremental.rs:315,495`、
  `tests/review_e2e.rs:612`；`src/output/mod.rs:194` 测试用 `"2"`。运行时无人比对该值（`--incremental-from` 预检不查）。
- **RunMeta**（`src/domain/report.rs:88-137`）已有 `cells: Vec<CellSummary>` / `needs` / `dossier` 等 serde-default 字段范式；
  `render_md`（`src/output/mod.rs:47`）只读 usage / ledger / cells / warnings。`JobState`（`src/state/mod.rs:60-93`）有 `models: Vec<String>`。
- **eval/**：零第三方依赖 python3、路径经参数传入（`eval/README.md`）；`eval/p0_oracle.py:178` 已有从 results JSON 取
  `verify.verdict == "upheld"` 的读法可照抄；`eval/out/*.json` 已入库，`eval/fixtures/` 不存在、`.gitignore` 不排除。
- **P0 §15.1 数字**（plan B-1 验收）：dismissed 63 / upheld 48——与上面直接查库结果一致。
- **碰撞面**：批 A-4 改 `src/cli/mod.rs` 的 `resolve_models`（约 :139-260、:609）；批 C-4 加 `--pack-only`。本批在 `src/cli/mod.rs`
  的改动限于 CellRecord 消费点（:445-471、:573-595）——与 A 的 :609 邻近，后合者 rebase 一次。

## Requirements

| ID | 需求（对应 plan Task） | 说明 |
|---|---|---|
| R1 | **B-1 旧库导出固定集** | 新增 `eval/export_ledger.py`（零依赖）：读 proj.db `dismissed` 全表 → `eval/fixtures/ledger-dismissed.jsonl`；读 `results.json` 中 `verify.verdict == "upheld"` 的 findings → `eval/fixtures/ledger-upheld.jsonl`。每行至少含 `id / file / quote / title / severity / status / round`；upheld 行额外带 `job_id`、`line_start / line_end / confidence / body / recommendation / verify.model / verify.note`；dismissed 行额外带 `reason / updated_at`。按 job×finding 计行（48），不按 id 去重。`--db` 缺省 `.redteam/proj.db`，`--out-dir` 缺省 `eval/fixtures`。stdout 打印两个计数。固定集入库。 |
| R2 | **B-2 domain 疑点类型与角色枚举** | 新增 `src/domain/suspicion.rs`：`Suspicion { file, line_start, line_end, hint, category: Option<Category>, confidence: f32, quote }`、`Category { L0..L5 }`（序列化 `"L0"`…）、`ScannedArea { file, ranges: Vec<String> }`、`ScanOutput { suspicions, needs, scanned_areas }`、`Role { Scanner, Reviewer, Verifier }`（小写）、`CellOutput`（`#[serde(tag = "role", rename_all = "lowercase")]`）= `Scan(ScanOutput)` \| `Review { findings: Vec<Finding>, needs }` \| `Verify { verdicts: Vec<VerifyEntry> }`；`NeedEntry` 加 `kind: NeedKind { Evidence, Api }`（serde default Evidence）并补 `Serialize`。`domain/mod.rs` 重导出。 |
| R3 | **B-3 CellRecord v4 + proj.db v4 一次重置** | `CellRecord { cell, role, model, family, ok, output: CellOutput, warn, duration_ms, input_tokens, output_tokens, cached_input_tokens, attempts, iterations, tool_use, template_hash: Option<String> }`，`CellRecord::ok(cell, model, output: CellOutput)`。`db.rs`：`PRAGMA user_version = 4`；`cells` 加 `role TEXT NOT NULL`（`cell_put` 从记录取 role 写列）；新表 `queue_items(job_id, item_id, state, json)` 与 `suspicions(id, file, quote_norm, hint, status, first_job, last_job, activations, json)`，本批只建不写；**旧库守卫**：`open_at` 遇 `user_version < 4` 且库非空 → 拒开，错误文案指向「先 `eval/export_ledger.py` 导出，再删除 `.redteam/proj.db` 重建」；不写迁移。全部 CellRecord 消费点机械适配（`CellOutput::findings()` 访问器），旧 fan-out 路径继续产 `Review` 变体直到 F-4。 |
| R4 | **B-4 作业元数据** | `RunMeta` 加 `coverage: Coverage { selected, completed, reused, failed, waived }`（每集合 = 按 role 分组的 cell 列表，计数即长度）、`template_hashes: BTreeMap<String, String>`、`cache_hit_by_cell: Vec<CacheHit { cell, ratio }>`、`provider_inflight_peak: BTreeMap<String, u32>`、`refute_rate: Option<f32>`——全部 serde default、空值不序列化，本批不填值；`ReviewReport.schema_version` 统一为常量 `SCHEMA_VERSION = "4"`（七处字面量收口）；`JobState` 加 `roles: Vec<String>`（default、空不序列化）。`render_md` 对空 coverage 零改动。 |
| R5 | **B-5 收口** | `cargo test` / `cargo clippy --all-targets` / `make verify` 绿；四个 task 各一 commit（简体中文 `type(scope)` 前缀，plan 给定文案）；收口审核 = code-review 子 agent（不跑 ship 评审：核心能力稳定前无需也不能跑，用户 2026-09-08 定）→ 修 → `--no-ff` 合回 `feat/m21-review-engine`（合回动作由用户触发）。 |

## Technical Notes（仓内证据裁定的实现约束）

- 「五表」（plan B-3 测试 / 出口 8）按「jobs / cells / results + queue_items / suspicions 五张运行时表」读；测试断言全部 9 张表存在
  （7 张既有 + 2 张新）与 `user_version = 4`，dismissed / resolved 四张判例表结构不变。
- 「库非空」= dismissed / resolved / jobs / cells / results 任一表有行。`user_version < 4` 且已有表但全空（例如 dev 二进制 `init` 出的空库）
  → 丢表重建为 v4（无数据、零风险）；有行 → 拒开。全新文件 → 直接建 v4。
- **role 唯一真相源 = `output` 的枚举 tag**（评审裁定 2026-09-08，偏离 plan 字段清单）：`CellRecord` 不存 `role` 字段，
  `CellRecord::role()` 与 `cells.role` 列都由 `output.role()` 派生；`Role` 复用批 A 的 `config::Role`。
- `family` 取批 A 角色条目 `EffectiveModel.family`（`PlannedModel.family` → `CellRun.family` → `with_family`；resume 按 manifest
  model 名回查），不再从 model 串推算。
- **独立库文件**（用户裁定 2026-09-08）：`db::DB_FILE = "proj-next.db"`，与 dev 二进制的 `proj.db` 分库；M21c 切名时定切换程序（followup）。
- 守卫（评审后）：版本高于 4 拒开不降级；含陌生表拒开不改写；`cells` 已有 `role` 列的 v0 库视为 v4 只补版本号；只读 v4 库不写任何字节可开；
  守卫 + 建表 + 版本号在一个 `BEGIN IMMEDIATE` 事务里；WAL 在守卫之后。
- `cache_hit_by_cell` 用具名结构而非元组（JSON 可读、jq 友好）——对 plan 文本的唯一形状偏离，F-5 填值前可改。
- 旧 `cells` JSON（`{"cell","model","ok","findings"}`）不再兼容：`tag` 缺失即解析错误，由 v4 重置守卫保证不会读到旧行。
- `status --cells` 输出形状随 CellRecord 变化（`findings` → `output`）；插件 commands 未消费该字段（`plugins/redteam/commands/*.md` 无引用）。

## Acceptance Criteria

- [x] AC1（R1）`python3 eval/export_ledger.py --db ~/hobby/redteam/.redteam/proj.db` 输出 `dismissed 63 / resolved 3 / upheld 48`；三份 jsonl 入库，
      每行含 `id / file / quote / title / severity / status / round`，upheld 行含 `job_id / head_sha / in_dismissed_ledger`（9 行与 dismissed 同 id）；
      脚本零第三方依赖、路径经 URI 转义、不读 `.next/private`。
- [x] AC2（R2）serde round-trip 三变体（Scan / Review / Verify）字节级等价；`{"cell":…,"output":{"suspicions":[]}}`（缺 `role` tag）解析失败而非默认；
      无 `kind` 的旧 `NeedEntry` JSON 仍可读且 `kind == Evidence`；`Category` 序列化为 `"L0"`…`"L5"`。
- [x] AC3（R3）空目录 `JobStore::open` → `.redteam/proj-next.db` 9 张表 + `user_version = 4`；`user_version 0` 且有数据的库 → Err，
      文案含 `export_ledger.py`、不叫删库、库一个字节不改；`user_version 0` 但全空 → 重建为 v4；版本 5 → 拒开不降级；陌生表 → 拒开不改写；
      `.dump` 恢复丢版本号的 v4 库 → 接受；只读 v4 库 → 可开；`CellRecord` v4 round-trip；`read_partial` 读回 `role()`；`cells.role` 列值与记录一致。
- [x] AC4（R4）`RunMeta::default()` 序列化不含 coverage / template_hashes / cache_hit_by_cell / provider_inflight_peak / refute_rate 键；
      赋值后 round-trip；旧 v2/v3 报告 JSON 反序列化不变；`ReviewReport` 序列化 `schema_version == "4"`；`render_md` 快照与改动前逐字节相同。
- [x] AC5（R5）`cargo test` 全绿、`cargo clippy --all-targets` 零新增、`make verify` 绿；`git log` 四笔 commit 文案与 plan 一致；未 push。
- [x] AC6（隐私）`git diff --stat` 新增文件只在 `eval/`、`src/domain/`、`src/state/`、`src/db.rs`、`src/domain/report.rs` 及其消费点；
      fixture 内 `file` 字段无公司仓路径。

## 实施记录（2026-09-08）

四笔 commit：0ef7798（B-1）/ 08484bd（B-2）/ 7c5e253（B-3）/ 0ec7912（B-4）；随后 09d116c 合入 `feat/m21-review-engine`（含批 A），
再按 code-review 审核（技能本体两次撞网关 429/524，结论来自主 agent 手工审读 + 五路视角）修复：守卫五改、Role / family 复用批 A、
role 字段删除改派生、独立库文件、导出脚本三修、oracle 兼容读、`JobState.roles` 枚举、帮助文本。旧库无需删除（分库）。
收口方式（用户 2026-09-08 定）：redteam 核心能力稳定前不跑 ship 评审，本批以 code-review 审核修复替代；直接 `--no-ff` 合回 `feat/m21-review-engine`。

## Out of Scope

- 疑点 parse（D-1）、scanner 扇出与 `CellOutput::Scan` 的生产写入（F-2）、coverage / cache / 模板哈希填值（D-4 / F-5）、
  台账写入与队列快照写入（M21b-c）、`family` 从配置条目取值（依赖 A-2，F-2 接）。
- 旧库迁移代码（D10：一次重置）；`dismissed` / `resolved` 表结构变更。
- `CellSummary` 加 role、`render_md` 呈现 coverage（F-5）。
- 合回 `feat/m21-review-engine` 的 merge 与 ship 评审循环的自动执行。
