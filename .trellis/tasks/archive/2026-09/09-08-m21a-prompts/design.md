# design · M21a 批 D 契约与提示词

> 需求见 `prd.md`；架构真相源 design v9 §4（契约）、§10.3（段序）、§10.7（共享头 + 角色尾）、决策 D6 / D9 / D10 / D11。

## 1. parse（`src/provider/parse.rs`）
- 抽公共前处理 `extract_json_object(raw) -> Option<&str>`（`</think>` 切除 → 围栏剥离 → 直接解析 / 配平扫描）；`parse_lens_output` 改为调用它
  （行为不变，单测既有）。
- `parse_scan_output(raw) -> Result<ScanOutput, ProviderError>`：先 `serde_json::from_str::<serde_json::Value>`，要求顶层对象含 `suspicions` 数组
  （缺 → `Parse("missing suspicions array")`）；逐条 `from_value::<RawSuspicion>`（宽松：字段 `Option`）→ 规整为 `Suspicion`：
  - `file`：`Path` 无 `..` / 非绝对（否则丢）；`line_start = max(1, ..)`；`line_end = max(line_start, line_end)`；
  - `hint`：`chars().take(300)`；`category`：`"L0".."L5"` 否则 `None`；`confidence`：`clamp(0,1)`，非数 → 0.5；`quote`：trim 为空 → 丢。
  - `needs`：`truncate(5)`；`kind` 缺省 `Evidence`（serde default 已有）；`scanned_areas`：`ranges` 过滤 `^\d+-\d+$` 且 a ≤ b。
  丢弃项计数入 `ParseStats { dropped_no_quote, dropped_bad_path }`——本批只在返回值里带（`parse_scan_output_with_stats`），供 F 写 warn。
- `REPAIR_PROMPT_SCAN`：与 `REPAIR_PROMPT` 同句式，「complete suspicions JSON object … shorten each hint」。

## 2. anchor（`src/positioning/mod.rs`）
- `pub struct AnchoredScan { pub output: ScanOutput, pub dropped: u32 }`；`pub fn anchor_suspicions(root, ScanOutput) -> AnchoredScan`：
  每条 `safe_read` + `locate_quote` → 改写行号；失败丢弃 + 计数。`needs` / `scanned_areas` 原样透传。

## 3. 模板（`src/prompt/templates/`）
- `shared-head.md`（system，字节稳定）：段序 `<role>` → `<rules>` → `<operating_stance>` → `<evidence>` → `<ci_covered>` → `<untrusted_input>` →
  `<bypass_class>` → `<output_language>` → `<repo_brief>`（`{{REPO_BRIEF}}`）→ `<spec_docs>`（`{{SPEC_DOCS}}`）。只含 `{{OUTPUT_LANGUAGE}}` /
  `{{REPO_BRIEF}}` / `{{SPEC_DOCS}}` 三个槽（后两者跨 cell 稳定，进 cache 区）。
- `scanner.md`（角色尾）：`<finding_bar>`（E3 B）→ `<precedents>`（`{{PRECEDENTS}}`）→ `<needs_policy>` → `<output_contract>`（字段与 `Suspicion` /
  `NeedEntry` / `ScannedArea` serde 名逐一对应；含「quote 不在文件即丢弃」「missing suspicions = invalid」）。
- 参考块与 diff 块不是模板文件，由 `build_scan_prompt` 用固定英文骨架渲染（`<reference>` 内 `<cross_index>` / `<user_focus>`；`<diff>` 内
  Target / Files / hunks，保留旧 prompt 的「勿据索引断言缺失」提示）。证据块 = `render_dossier_section(dossier)` 原样（`<evidence>` 包裹）。

## 4. `build_scan_prompt`（`src/prompt/mod.rs`）
```rust
pub struct PromptBlock { pub text: String, pub cache_boundary: bool }
pub struct ScanPrompt { pub system: String, pub blocks: Vec<PromptBlock> }   // blocks = [reference, evidence, diff, tail]
pub struct ScanPromptInput<'a> { target_label, bundle: &'a Bundle, user_focus, cross_index, precedents, dossier, spec_docs, repo_brief }
pub fn build_scan_prompt(i: &ScanPromptInput) -> ScanPrompt
impl ScanPrompt { pub fn flatten(&self) -> String /* blocks 按序 "\n\n" 拼接，不含 system */ }
pub struct TemplateHashes { pub shared_head: String, pub scanner_tail: String, pub legacy_bundle: String }
pub fn template_hashes() -> TemplateHashes   // fnv1a64_hex 于模板原文（不含槽值）
```
- 注入互斥：`cross_index` / `precedents` / `repo_brief` / `spec_docs` / `dossier` / diff / 路径各只进一个槽；全部 `neutralize_structure`；
  scan 路径用独立的 `SCAN_STRUCTURAL_TAGS` = 旧 6 标签 + `reference / cross_index / user_focus / evidence / diff / scanner / precedents / repo_brief`
  （只加包裹注入槽的段；`untrusted_input / finding_bar / needs_policy` 不包裹任何槽，不进表）；旧 `DOSSIER_STRUCTURAL_TAGS` 不动，旧 prompt 逐字节不变。
- 空渲染纪律：空 `cross_index` → `(none — this group covers the entire changeset)`；空 `precedents` / `repo_brief` / `spec_docs` → `(none)`；
  空 `dossier` → 证据块只含骨架句（不产空标签对以外的内容）。

## 5. provider（`src/provider/{tool_loop,backends}.rs`）
- `ChatMessage::UserBlocks(Vec<PromptBlock>)`（`PromptBlock` 定义在 prompt 模块，provider 引用）。
- anthropic：`system` 渲染为 `[{type:text, text, cache_control}]`（有 system 时）；`UserBlocks` → content 数组，`cache_boundary` 块带
  `cache_control`；末块自动断点规则：仅当整条 messages 里没有任何显式断点时才加（保持旧路径行为）。
- openai：`UserBlocks` → 单条 `content` = `flatten()`。
- `single_shot` / `tool_loop` 的 `match` 补臂（透传）。本批不改 `run_single_shot` 签名（F 接 `ScanPrompt`）。

## 6. 哈希入记录（`src/orchestrate/mod.rs`）
`to_record` 填 `template_hash = Some(template_hashes().legacy_bundle)`；`OnceLock` 缓存哈希避免每 cell 重算。

## 7. 评分器（`eval/scan_score.py`）
- 输入自动识别：`{"cells":[{"output":{"role":"scanner","suspicions":[...]}}]}`（redteam-next 结果）/ `{"runs":[...]}` 或 `{"items":[...]}`（E3 run）。
- `anchor_quote(repo, head, path, quote)` / `overlap` 从 `e3_contract_ab.py` 抽为共享模块 `eval/_oracle.py`（e3 脚本改 import，行为不变）。
- 输出 JSON + 一行摘要：`items / hit / union_hit / noise / unanchored`；`--baseline` 时追加 `Δhit / Δnoise`。`selftest`：临时 git 仓 + 合成 oracle。

## 8. 回滚
新增文件 + 加法改动；旧 prompt / parse 路径逐字节不变（快照测试守）。不合回即回滚。
