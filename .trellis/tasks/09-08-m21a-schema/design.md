# design · M21a 批 B schema v4

> 范围与验收见 `prd.md`；上位 design v8 §4（疑点契约）/ §8.3（疑点台账）/ §8.4（coverage 五集合）/ §15（落地序 1）· D10-2 · D11-7。

## 1. 边界

| 层 | 本批改动 | 不动 |
|---|---|---|
| domain（`src/domain/`） | 新 `suspicion.rs`（Suspicion / Category / ScannedArea / ScanOutput / Role / CellOutput / VerifyEntry）；`finding.rs` 的 `NeedEntry` 加 `kind` + `Serialize`；`report.rs` 加 RunMeta 五字段 + `SCHEMA_VERSION` | `Finding` / `RawFinding` / `LensOutput` / `Verify` / `Cluster` |
| state（`src/state/mod.rs`） | `CellRecord` 形状 v4；`JobState.roles`；`append_partial` 传 role 写列 | `JobStore` 写三形态、守卫 CAS、manifest / result API |
| db（`src/db.rs`） | `open_at` 版本守卫 + `user_version = 4` + `cells.role` 列 + 两张新表；`cell_put(job, cell, role, json)` | dismissed / resolved 四表与其 API |
| 消费点 | 机械适配到 `output`（见 §4） | 编排语义、旧 fan-out 路径（F-4 删） |
| eval | `export_ledger.py` + `fixtures/ledger-*.jsonl` | 其余脚本 |

## 2. 类型（`src/domain/suspicion.rs`）

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]           // "L0".."L5"（design §4：category 取 L0-L5）
pub enum Category { L0, L1, L2, L3, L4, L5 }

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Suspicion {
    pub file: String, pub line_start: u32, pub line_end: u32,
    pub hint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub category: Option<Category>,
    pub confidence: f32,
    pub quote: String,                        // 必填；逐字硬约束在 D-1 parse 校验
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScannedArea { pub file: String, pub ranges: Vec<String> }   // "a-b" 形式，D-1 校验

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScanOutput {
    pub suspicions: Vec<Suspicion>,
    #[serde(default)] pub needs: Vec<NeedEntry>,
    #[serde(default)] pub scanned_areas: Vec<ScannedArea>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role { Scanner, Reviewer, Verifier }
impl Role { pub fn as_str(&self) -> &'static str }   // "scanner" | "reviewer" | "verifier"（写 cells.role 列）

/// verifier cell 的单条判决（design §8.1：REFUTED | UPHELD + 一句理由）。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerifyEntry { pub id: String, pub verdict: VerifyVerdict, #[serde(default)] pub note: String }

/// 一 cell 一条、按角色分叉（D11-7）：tag 缺失 → serde 报错，不默认。
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum CellOutput {
    Scan(ScanOutput),                                   // {"role":"scanner","suspicions":[..],"needs":[..],"scanned_areas":[..]}
    Review { findings: Vec<Finding>, #[serde(default)] needs: Vec<NeedEntry> },
    Verify { verdicts: Vec<VerifyEntry> },
}
impl CellOutput {
    pub fn role(&self) -> Role;                         // Scan→Scanner, Review→Reviewer, Verify→Verifier
    pub fn findings(&self) -> &[Finding];               // Review→切片；其余→&[]（消费点适配用）
    pub fn into_findings(self) -> Vec<Finding>;
    pub fn needs(&self) -> &[NeedEntry];
}
```

`Scan(ScanOutput)` 用 newtype 变体：serde 内部 tag 对 newtype 变体要求内容为结构体/map，`ScanOutput` 是结构体，满足；
其字段与 tag 平铺在同一对象。`Review` 用结构体变体，`Finding` 已 `Serialize + Deserialize`。

`NeedEntry`（留在 `finding.rs`）：

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NeedKind { #[default] Evidence, Api }
pub struct NeedEntry { pub path, pub lines: Option<String>, pub reason, #[serde(default)] pub kind: NeedKind }   // + Serialize
```
字面量构造点（`orchestrate/mod.rs:444`、`second_pass.rs` 六处）补 `kind: NeedKind::Evidence`（或 `..Default::default()` 需 `NeedEntry: Default`，
不加——`path`/`reason` 无合理缺省）。

## 3. CellRecord v4 与 proj.db v4

```rust
pub struct CellRecord {                // 评审后（2026-09-08）：不存 role 字段，`fn role(&self) -> Role { self.output.role() }`
    pub cell: String,
    pub model: String,
    #[serde(default)] pub family: String,      // 取批 A EffectiveModel.family（PlannedModel.family → CellRun.family → with_family）
    pub ok: bool,
    pub output: CellOutput,            // 失败格：ok=false + 该角色的空输出（Review{findings:[],needs:[]} 等）
    #[serde(skip_serializing_if = "Option::is_none")] pub warn: Option<String>,
    #[serde(default)] pub duration_ms / input_tokens / output_tokens / cached_input_tokens: u64,
    #[serde(default)] pub attempts / iterations: u32,
    #[serde(default)] pub tool_use: ToolUse,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub template_hash: Option<String>,   // D-4 填
}
impl CellRecord {
    pub fn ok(cell, model, output: CellOutput) -> Self;         // family 空，调用方 with_family 回填
    pub fn failed(cell, model, role: Role, warn: String) -> Self; // 空输出占位（run_resume 失败分支）
    pub fn with_family(self, family) -> Self;
    pub fn role(&self) -> Role;                                 // = output.role()，唯一真相源
}
// Role = pub use crate::config::Role（批 A）；family 不再从 model 串推算
```

DDL（`open_at`，在既有七表之后）：

```sql
ALTER 无；cells 表整体改为：
CREATE TABLE IF NOT EXISTS cells (job_id TEXT NOT NULL, cell TEXT NOT NULL, role TEXT NOT NULL, record_json TEXT NOT NULL, PRIMARY KEY (job_id, cell));
CREATE TABLE IF NOT EXISTS queue_items (job_id TEXT NOT NULL, item_id TEXT NOT NULL, state TEXT NOT NULL, json TEXT NOT NULL, PRIMARY KEY (job_id, item_id));
CREATE TABLE IF NOT EXISTS suspicions (id TEXT PRIMARY KEY, file TEXT NOT NULL, quote_norm TEXT NOT NULL, hint TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL, first_job TEXT NOT NULL DEFAULT '', last_job TEXT NOT NULL DEFAULT '', activations INTEGER NOT NULL DEFAULT 0, json TEXT NOT NULL);
PRAGMA user_version = 4;
```

`cell_put(job_id, cell, role, record_json)` 写 `role` 列；`cell_all` 不变（读 record_json）。

**库文件**：`DB_FILE = "proj-next.db"`（用户裁定 2026-09-08：与 dev 二进制的 `proj.db` 分库；M21c 切名时定切换程序）。

**版本守卫（`open_at`，评审后版本）**：

```
只读快路径：user_version == 4 且九表齐全 → 不写任何字节（只读文件可开）
否则 BEGIN IMMEDIATE：
  v > 4 → Err（不降级）
  v < 4 且有表：
    有陌生表（∉ 九表）→ Err（不是 redteam 库，不改写）
    cells 已有 role 列 → 视为 v4（.dump 恢复丢版本号），只补版本号
    否则 rows = Σ count(*) over 在场表：rows > 0 → Err（提示导出，不叫删库）；rows == 0 → DROP 全部
  CREATE IF NOT EXISTS 九表；v != 4 → PRAGMA user_version = 4
COMMIT → 之后才 journal_mode = WAL
```
守卫在 `open_at` 单点，`open` / `init` / `--dismissed <path>` 三入口全覆盖。

## 4. 消费点适配（旧 fan-out 路径 F-4 前继续工作）

| 位置 | 改法 |
|---|---|
| `orchestrate/mod.rs:133` `to_record` | `output: CellOutput::Review { findings: self.findings.clone(), needs: self.needs.clone() }`，`role: Role::Reviewer`（旧 finding 路径语义上是 reviewer 形态），`family: family_of(model)`，`template_hash: None` |
| `cli/mod.rs:445` `attach_cells` | 字段平移不变（不读 findings） |
| `cli/mod.rs:464-471` `partial_report` | `flat_map(|r| r.output.into_findings())` |
| `cli/mod.rs:573-595` `run_resume` 两处字面量 | 成功 → `CellRecord::ok(cell, model, CellOutput::Review{..})` 后回填运行学；失败 → `CellRecord::failed(..)` |
| `cli/mod.rs:604` 合并 findings | `into_findings()` |
| `cli/incremental.rs:212` | 只读 `ok`/`cell`，不变 |
| `main.rs:1207` `status --cells` | dump 不变，形状随之变（无插件消费者） |
| 测试字面量（cli/orchestrate/incremental/e2e 共十余处） | `CellRecord::ok(c, m, vec![...])` → `CellRecord::ok(c, m, CellOutput::Review { findings: vec![...], needs: vec![] })`；可加测试辅助 `CellOutput::review(findings)` |

## 5. RunMeta / JobState 字段（B-4）

```rust
pub const SCHEMA_VERSION: &str = "4";      // report.rs；七处字面量改引用
#[derive(Default)] pub struct Coverage {   // 每集合：role → cell 列表；计数 = len；不相交并 = 全集（F-5 维护）
    pub selected / completed / reused / failed / waived: BTreeMap<Role, Vec<String>>,
}
impl Coverage { pub fn is_empty(&self) -> bool }
pub struct CacheHit { pub cell: String, pub ratio: f32 }
RunMeta += {
    #[serde(default, skip_serializing_if = "Coverage::is_empty")] coverage: Coverage,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")] template_hashes: BTreeMap<String, String>,   // "shared_head"/"scanner" → fnv1a64 hex
    #[serde(default, skip_serializing_if = "Vec::is_empty")] cache_hit_by_cell: Vec<CacheHit>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")] provider_inflight_peak: BTreeMap<String, u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")] refute_rate: Option<f32>,
}
JobState += { #[serde(default, skip_serializing_if = "Vec::is_empty")] roles: Vec<String> }
```
`Role` 作 BTreeMap 键需 `Ord`；serde 对非字符串键的 map 会报错——`Role` 序列化为字符串（`rename_all = "lowercase"` 的单元变体），
serde_json 接受字符串键的枚举。

## 6. `eval/export_ledger.py`

```
用法：python3 eval/export_ledger.py [--db .redteam/proj.db] [--out-dir eval/fixtures]
dismissed：SELECT id,status,title,file,quote,severity,reason,round,updated_at FROM dismissed ORDER BY id
  → 行：{"id","status","title","file","quote","severity","reason","round","updated_at"}
upheld：  SELECT job_id, json FROM results ORDER BY job_id → 逐 finding 过滤 verify.verdict=="upheld"
  → 行：{"id","status":"upheld","title","file","quote","severity","round":"","job_id","line_start","line_end","confidence",
         "body","recommendation","verify_model","verify_note"}
写 <out-dir>/ledger-dismissed.jsonl / ledger-upheld.jsonl（ensure_ascii=False，一行一 JSON，末尾换行）；stdout：`dismissed N / upheld M`。
```
不去重、不改写内容；`round` 对 upheld 不可得（results 无轮次字段）留空串，`job_id` 补足溯源。

## 7. 兼容与回滚

- 旧 `cells` 行 / 旧 `CellRecord` JSON 不兼容（tag 必需）；由版本守卫保证运行时读不到。`JobState` / `RunMeta` / `ReviewReport`
  的旧 JSON 保持可读（新增字段全 default）。
- `schema_version` 升 4 后无运行时比对方；ship 壳层未读该字段。
- 回滚：子分支不合回即回滚；固定集与脚本独立于二进制。用户本机旧库仅被拒开，不被改写（守卫只读计数）。
- 碰撞：批 A-4 改 `src/cli/mod.rs:609` `resolve_models` 调用与本批 :573-604 邻近——后合者 rebase；本批不动 `src/prompt/`、`src/bundle/`。
