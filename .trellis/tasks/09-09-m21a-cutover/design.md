# design · M21a 批 F 硬切与并行

> 需求与 K 编号见 `prd.md`；架构真相源 design v8 §2（编排形状）/ §4（scanner cell 契约）/ §5（多 scanner 并集）/ §8.4（coverage 分级）/ §10.3（段序与 cache 断点）/ §12.3（路由与限流）；决策 D7 A3 / A5、D10。

## 1. provider 层（K2 / K3 / K4 / K5）

### 1.1 `src/provider/scan_shot.rs`（新）
```rust
pub struct ScanCall {
    pub output: ScanOutput, pub stats: ScanParseStats,
    pub input_tokens: u64, pub output_tokens: u64, pub cached_input_tokens: u64,
    pub iterations: u32, pub finish_reason: Option<String>, pub reasoning_tokens: Option<u64>,
}
pub async fn run_scan_shot(backend: &dyn ChatBackend, prompt: &ScanPrompt, per_turn: Duration, retry_budget: u32) -> Result<ScanCall, ProviderError>
```
`messages = [System(prompt.system), UserBlocks(prompt.blocks.clone())]` → `timed_turn(backend, &messages, &[], per_turn, retry_budget)`（首发走 `turn`，铁律同旧单发）→ `parse_scan_output_with_stats(&turn.text)` 成功 `iterations = 1`；失败 → `messages.push(Assistant{text}); messages.push(User(REPAIR_PROMPT_SCAN))` → `timed_turn_final` 失败 `attach_usage` 首发 usage → 再解析 `iterations = 2`，仍失败 `diagnostic_parse_error(&repair)`。usage 三项跨轮累加；`reasoning_tokens` 取各轮 `Some` 之和（全 None → None）。

### 1.2 `src/provider/registry.rs`
```rust
pub struct ProviderRegistry {
    providers: BTreeMap<String, Box<dyn ReviewProvider>>,     // 旧表（M21b reviewer / verifier）
    scan: BTreeMap<String, ScanEntry>,                        // route_key → 条目
    gates: BTreeMap<String, Arc<InflightGate>>,               // provider → 在途门
    warnings: Vec<String>,                                    // 建 registry 时的告警（thinking 无映射等）
}
struct ScanEntry { backend: Box<dyn ChatBackend>, per_turn: Duration, retry_budget: u32, provider: String }
pub struct InflightGate { sem: Semaphore, current: AtomicU32, peak: AtomicU32 }
impl ProviderRegistry {
    pub fn insert_scan(&mut self, route_key: &str, provider: &str, cap: u32, backend: Box<dyn ChatBackend>, per_turn: Duration, retry_budget: u32);
    pub async fn scan(&self, route_key: &str, prompt: &ScanPrompt) -> Result<ScanCall, ProviderError>;   // 未注册 → Unavailable("scanner not in registry: …")
    pub fn inflight_peaks(&self) -> BTreeMap<String, u32>;
    pub fn push_warning(&mut self, s: String); pub fn warnings(&self) -> &[String];
}
```
`scan()`：`gate.sem.acquire()` → `current += 1`、`peak = max(peak, current)` → `run_scan_shot` → `current -= 1`（Drop 守卫，panic 安全）。同 provider 多条目共享同一把门（`insert_scan` 按 provider 名复用；cap 以首次为准）。

### 1.3 `src/provider/backends.rs`
- `pub(crate) fn thinking_params(kind: ProviderKind, family: &str, level: ThinkingLevel, max_tokens: u64) -> Option<Vec<(&'static str, Value)>>`：

| kind | family | Off | Low | Medium | High |
|---|---|---|---|---|---|
| anthropic | claude | 不发（Some(vec![])） | `thinking{enabled, budget_tokens: 2048}` | 8192 | 16384 |
| openai | deepseek | `thinking{disabled}` | `thinking{enabled}` + `reasoning_effort: low` | … `high` | … `max` |
| openai | glm | `thinking{enabled}` + `reasoning_effort: low`（5.3 不可关） | 同 Off | `high` | `max` |
| openai | kimi / moonshot | `thinking{disabled}` | `thinking{enabled}` | 同 | 同 |
| openai | openai / gpt / o | 不发 | `reasoning_effort: low` | `medium` | `high` |
| openai | gemini | 不发 | `reasoning_effort: low` | `medium` | `high` |
| openai | qwen | `enable_thinking: false` | `enable_thinking: true` | 同 | 同 |
| 其余 (kind, family) | — | `None`（无映射） | `None` | `None` | `None` |

  anthropic budget 夹到 `min(budget, max_tokens − 1024)`，若 < 1024 → `None`（无法满足 API 约束，按无映射处理）。`Some(vec![])` = 该档就是「不发」；`None` = 无映射 → 调用方 warning。
- `build_anthropic_tools_body(.., thinking: Option<&[(&str, Value)]>)` / `build_openai_tools_body(.., thinking: Option<&[(&str, Value)]>)`：在既有 `max_tokens` 插入之后逐对 `body.insert`；`None` / 空 → 逐字节不变（既有 body 测试全部传 `None`）。
- `AnthropicBackend` / `OpenAiBackend` 加 `thinking: Option<Vec<(&'static str, Value)>>` + `with_thinking(v)`；`backend_from_config(em, key) -> Result<Box<dyn ChatBackend>>` 签名不变，内部 `em.thinking_level.map(|l| thinking_params(em.kind, &em.family, l, em.max_tokens_effective()))`；新 `pub(crate) fn thinking_warning(em) -> Option<String>`（配置了档位但 `thinking_params == None` → 「thinkingLevel <l> 对 <provider/id>（kind, family）无参数映射，未发送」），`build_registry` 收进 `registry.push_warning`。
- `AssistantTurn.reasoning_tokens: Option<u64>`：openai 非流式 `usage.completion_tokens_details.reasoning_tokens`、流式最后 usage 块同路径；anthropic `None`。`FakeChatBackend` 与全部 16 处字面量补 `reasoning_tokens: None`。

## 2. 编排（K1 / K5 / K6 / K7）：`src/orchestrate/mod.rs`
```rust
pub struct ScanEntrySpec { pub route_key: String, pub family: String, pub thinking_pinned: bool }   // 由 cli 从 EffectiveModel 转出
pub struct ScanFanoutInput<'a> {
    pub target_label: &'a str, pub bundles: &'a [Bundle], pub dossiers: &'a [String], pub others: &'a [String],
    pub user_focus: &'a str, pub spec_docs: &'a str, pub root: &'a Path,
    pub entries: &'a [ScanEntrySpec], pub workers: usize, pub samples: u32,
}
pub struct ScanCellResult { pub cell_id: String, pub group: usize /*0-based 索引*/, pub route_key: String, pub family: String, pub sample: u32, pub record: CellRecord, pub output: Option<ScanOutput> /*锚定后；失败 None*/ }
pub struct ScanResults { pub cells: Vec<ScanCellResult>, pub warnings: Vec<String>, pub usage: (u64,u64,u64), pub failed: Vec<String>, pub needs_declared: u32 }
pub async fn run_scan_fanout(registry: &ProviderRegistry, input: &ScanFanoutInput<'_>, sink: Option<&dyn CellSink>) -> ScanResults
```
- prompts：每组一次 `build_scan_prompt(&ScanPromptInput { target_label, bundle, user_focus, cross_index: others[g], precedents: "", dossier: dossiers[g], spec_docs, repo_brief: "" })`。
- cells：`for g in bundles / for e in entries / for k in 1..=samples` → `cell_id = format!("scan[g{}]@{}#{}", g+1, e.route_key, k)`；manifest = `CellInput { cell, model: route_key, prompt: prompts[g].flatten(), scan: Some(prompts[g].clone()) }`；`sink.on_roles(&[Role::Scanner])`（`CellSink` 新默认方法）→ `sink.manifest(&inputs)`。
- 执行：`Arc<Semaphore>(workers)`；`join_all`；每 cell：permit → `t0` → `registry.scan(route_key, &prompts[g])`：
  - Ok(call)：`anchor_suspicions(root, call.output)` → `warn` 片段：`call.stats` 各非零计数（`dropped_no_quote / dropped_bad_path / dropped_malformed / needs_dropped / needs_truncated / ranges_dropped`）、`anchored.dropped`（quote not found）、`dropped_unreadable`、零推理（`thinking_pinned && call.reasoning_tokens == Some(0) && call.output_tokens < 200`）；`CellRecord::ok(cell_id, route_key, CellOutput::Scan(anchored.output)).with_family(family)`，填 `duration_ms / input / output / cached / iterations / attempts = 1 / template_hash = scan_template_hash()`，`warn = Some(joined)`（`ok` 保持 true）。`needs_declared += output.needs.len()`。
  - Err(e)：`CellRecord::failed(cell_id, route_key, Role::Scanner, e.to_string())` + `e.burned_usage()` 入 tokens、`duration_ms`；`failed.push(cell_id)`；`warnings.push("cell <id> failed: <e>")`。
  - `sink.on_cell(record.clone())`。
- `scan_template_hash()`：`format!("{}+{}", h.shared_head, h.scanner_tail)`（K10）。
- 保留：`CellSink`（加 `fn on_roles(&self, _: &[Role]) {}`）、`finding_id` / `enrich` / `normalize_quote`（升 `pub(crate)`，merge 复用）。

## 3. 合并与报告（K8 / K9）

### 3.1 `src/consolidate/scan.rs`（新，`consolidate/mod.rs` 注册）
```rust
pub struct MergedSuspicion { pub group: usize, pub suspicion: Suspicion, pub agreement: u32, pub families: Vec<String> /*排序去重*/, pub cell_ids: Vec<String>, pub hints: Vec<String> }
pub fn merge_suspicions(cells: &[ScanCellResult]) -> Vec<MergedSuspicion>
```
按组分桶；桶内顺序扫描：与已有簇 `same_suspicion(a, b)` = `a.file == b.file && (normalize_quote(a.quote) == normalize_quote(b.quote) || quote_overlap(&a.quote, &b.quote) >= QUOTE_SIM) && lines_overlap(a, b, LINE_GAP)` → 并入（families 加、cell_ids 加、hints 加、代表取 confidence 更高者）；否则新簇。`agreement = families.len()`。输出排序：(group, file, line_start, line_end, hint)。`QUOTE_SIM` / `LINE_GAP` 从 `dedup` 升 `pub(crate)`。

### 3.2 `src/domain/report.rs`
- `Verdict::ScanOnly`（`as_str = "scan-only"`）；`exit_code`（`cli/tail.rs`）加 `(ScanOnly, _) => 0`；`output::render_md` verdict 臂「🔎 scan-only」。
- `ScanGroup { label, files: Vec<String>, cells: Vec<String>, suspicions: u32, pack: PackStats }`、`ScanStats { total, cells_ok, cells_failed, agreement_ge2, by_category: BTreeMap<String,u32>, needs_declared }`、`ScanReport { groups, suspicions: Vec<MergedSuspicion>, scanned_coverage: Option<f32>, stats }`；`ReviewReport { …, #[serde(default, skip_serializing_if = Option::is_none)] scan: Option<ScanReport>, #[serde(default, skip_serializing_if = Vec::is_empty)] cells: Vec<CellRecord> }`。`RunMeta.grouping: Option<GroupingStats>`（`GroupingStats` / `Gate` / `PackStats` 补 `Deserialize`；`MergedSuspicion` 派生 serde）。
- `CellSummary` 加 `cell: String`、`role: String`、`cached_input_tokens: u64`（default，空 / 0 不序列化不必要——直接序列化）。

### 3.3 md 渲染（`src/output/mod.rs`）
scan-only 报告：标题 verdict → 「Groups」表（label / files / cells / suspicions / pack body・sliced・folded_lines）→ 「Suspicions」按组列 `- file:a-b [Lx] conf 0.xx · agreement n (families) — hint` + 缩进一行 `quote` 首行（≤ 120 字符）→ 「Coverage」`scanned_coverage` 百分比、needs declared、failed cells 列表 → 既有 Run 段。

## 4. cli（K6 / K7 / K11 / K13 / K15）
- `run_code_review`：`roles.scanners` 空 → Err（沿用文案）；`entries = scanners.map(ScanEntrySpec { route_key: em.name, family: em.family, thinking_pinned: em.thinking_level.is_some_and(|l| l != Off) })`；`plan_cells` 全字段接住（`stats` / `pack_stats`）；`run_scan_fanout` → `merge_suspicions` → `scanned_coverage(bundles, cells)` → `ScanReport` → `RunMeta`（§5）→ `verdict = if failed.is_empty() { ScanOnly } else { Incomplete }` → `ReviewReport { verdict, stats: Stats::default_zero, findings: [], clusters: [], run, scan: Some, cells: records }` → `exit_code`。`sink.on_phase(Finalizing)` 保留。
- `scanned_coverage`：`changed: BTreeSet<(file, line)>` 来自每 bundle 文件 `diffsym::parse_hunks(&fc.diff)` 的 `changed`（升 `pub`）；`scanned` = 各 cell `scanned_areas` 展开 `a-b` 区间（1-based → 0-based）∩ changed；`Some(scanned.len() / changed.len())`，changed 空 → None。
- `run_resume`：manifest 差集 cell 用 `input.scan` 重放 `registry.scan`（无 `scan` 字段 = 旧作业 → 该 cell 记 failed「manifest 无 scan prompt（旧格式作业）」）；ok 的 cell 记录进 `reused`；尾段 = 同 `run_code_review` 的 merge / report（抽 `finish_scan_report(..)` 共用）。
- `partial_report`：`CellOutput::Scan` 记录 → `merge_suspicions` → `Incomplete` 报告（scan 段有）。
- `run_incremental_review` → `anyhow::bail!("增量轮（--incremental-from）在 M21c 疑点台账回流后恢复；本版本请跑全量")`，`incremental_preflight` 与其测试保留；`render_incremental_section` / `two_layer_manifest` 删。
- `run_pack_only`：`build_scan_prompt` → `bytes = system.len() + 2 + flatten.len()`，`cell-n.md` = `system + "\n\n" + flatten`；跨组索引槽格式不变（e2e 断言沿用）。
- `build_registry`（main.rs）：scanners → `registry.insert_scan(em.name, em.provider, em.max_concurrency.unwrap_or(cfg.review.max_concurrency), backend, per_turn, DEFAULT_RETRY_BUDGET)` + `thinking_warning`；`legacy_opts_from_roles` 删、空 scanner 检查内联。
- `JobCellSink`：`manifest` 打 `Phase::Scan`；`on_roles` 写 `JobState.roles`。
- `--help`：见 prd K13；Review doc「scanner 扇出（组 × 条目 × samples）→ 疑点索引 + 覆盖自报，不产 finding（M21b 深挖后才有 finding）；退出码：0 = scan 完成、1 = 命令失败、3 = 任一 scanner cell 失败（incomplete）；2 / 10 在 M21b 前不可达」。

## 5. 观测（K10）：RunMeta 填法
| 字段 | 值 |
|---|---|
| `models` / `model_ids` | scanners 的 `name` / `id_of` |
| `usage` | fanout 三元组；`refute` 0 |
| `warnings` | registry warnings + spec 拒读 + fanout warnings + 「no reviewable evidence」 |
| `failed_models` | 空（scan 路径失败进 coverage） |
| `dossier` / `needs` | `dossier_meta`；`NeedsMeta { declared, second_pass: 0 }` |
| `cells` | 每 cell `CellSummary { cell, model, role, ok, iterations, attempts, duration_ms, input, output, cached }` |
| `coverage` | `selected[scanner]` = 全部 cell_id；`completed` = ok；`failed` = 失败；`reused` = resume 沿用；`waived` 空 |
| `template_hashes` | `{ "shared_head": .., "scanner_tail": .. }` |
| `cache_hit_by_cell` | 每 cell `cached ÷ input`（input 0 → 跳过） |
| `provider_inflight_peak` | `registry.inflight_peaks()` |
| `grouping` | `CellPlan.stats` |
| `head_sha` / `test_command` / `ledger` / `job_id` / `duration_ms` | 同旧 |
`phase_history`：Collecting → Scan → Finalizing（`Refute` 不再出现）。

## 6. 删除面（K11）与测试改造
见 prd R-F4；e2e 改造：`code_bundle_tool_loop_anchors_and_drops_fabricated` → `scan_fake_backend_anchors_and_drops_fabricated`（`FakeChatBackend` 回 `{"suspicions":[真, 编造]}`，registry `insert_scan`，断言真疑点行号被 quote 改写、编造被丢、`warn` 含 dropped 计数、退出 0）；`code_replicate_two_models_yield_agreement_two` → 两家族 fake 同 quote → `scan.suspicions[0].agreement == 2`；`code_replicate_unreachable_model_exits_two` → 未注册条目 → `Incomplete` / 3、`coverage.failed` 含该 cell；`background_job_smoke_*` → phases 含 `scan` 无 `refute`、`template_hash == scan_template_hash()`、`verdict == scan-only`；`result_md_three_states` 补字段；`pack_only_*` 按 K15 回归。cli / orchestrate / registry / single_shot 单测按 F-4 清单删改。

## 7. 回滚
每 task 一 commit、可编译可测；F-4 之前新旧并存（scan 路径只在测试里跑）；合回后回滚 = revert `--no-ff` merge。运行期无逃生口（硬切，D10）——对照靠 dev 二进制。
