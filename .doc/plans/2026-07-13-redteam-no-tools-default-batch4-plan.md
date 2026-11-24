# 无工具默认化 批4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> 状态: 锁定 · design=`.doc/design/2026-07-13-redteam-no-tools-default-design.md` §5 裁决 B/I · decisions **E9**（fan-out 收缩判决+dep_rank/clamp 删除裁定，2026-07-13）· base=dev merge 87e281c

**Goal:** 单格全审转正为默认（fan-out 收缩，E8-B/E9）+ lens 去实体化为 prompt 模板固有文本（E8-I：代码零结构）。

**Architecture:** 两段：①收缩——`merge_bundles` 从验收仪器转正为默认路径（全量+增量），`REDTEAM_SINGLE_CELL` env 门拆除，panorama 兄弟注入循环与 `dep_rank` 随单格范式失去参照系而删除（L4 拆缝立项时从 git 复活）；②去实体化——`{{RULES}}` 动态渲染烘干为 code/doc 模板固有攻击面文本，`src/lens/` 模块、rules 参数链、`--lenses` flag、config `LensSets`、state/报告 lenses 字段、`RawFinding.lens`/`Finding.lenses`、`clamp_compliance` 机械钳制全部退场；注意力引导由 `--focus` 独家承担。

**Tech Stack:** Rust 单 crate；无新依赖。

## Global Constraints

- 代码注释简体中文、模板散文英文——与现仓一致。
- 每 task：`cargo test --lib` 全绿（现基线 401）+ `cargo test --test review_e2e`（7）；`cargo clippy --all-targets 2>&1 | grep -c "warning:"` ≤ **8**；commit 带 trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`；**永不 push**。
- **删除性重构纪律**：每项删除后 `grep` 全仓确认零残留引用（函数名/字段名/env 名/flag 名）；删除的测试如果钉的是被删行为则随删，钉的是存留行为则改写保留。
- `REDTEAM_PACK_BODY=symbol`（粒度仪器）与 `REDTEAM_PACK_SHELLS`（E8-E 仪器，拆除归 M12/M16 不在本批）行为不动。
- lens 固有文本逐字用本 plan Task 2 给出的块（= 现 `rules_block(all_for(mode))` 渲染结果，行为字节级最小变化）。

---

### Task 1: fan-out 收缩——单格全审转正默认

**Files:**
- Modify: `src/cli.rs`（全量路径 `let bundles = group(...)` + SINGLE_CELL gate 约 :560-566；增量路径 `let bundles = group(...)` 约 :659；`merge_bundles` 注释；panorama 兄弟循环 build_packs 内约 :400-430；`second_pass_prompt` 兄弟遍历段约 :578-590；`dep_rank` + 相关测试）
- Test: 同文件 tests

**Interfaces:**
- Consumes: 现 `merge_bundles(Vec<Bundle>) -> Vec<Bundle>`（批 1 验收仪器，转正）。
- Produces: 全量与增量路径的 bundles 恒为单 bundle；`build_packs`/`second_pass_prompt` 签名不变（slice 通用），内部兄弟逻辑删除。

- [ ] **Step 1: 写/改失败测试**

```rust
#[test]
fn bundles_collapse_to_single_cell_by_default() {
    // E9：单格全审默认——group 产多 bundle 后恒并单格（无 env 门）
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let fc = |p: &str| FileChange { path: p.into(), diff: format!("@@ -1,0 +1,1 @@\n+x // {p}\n") };
    let bs = vec![
        Bundle { files: vec![fc("src/a.rs")] },
        Bundle { files: vec![fc("src/b.rs")] },
    ];
    let merged = merge_bundles(bs);
    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].files.len(), 2);
}
```

既有测试联动（按新语义改写，不是删断言）：
- `merge_bundles_flattens_all_files_into_one`：保留（语义未变，注释去「验收仪器/随 M12 拆」措辞改「E9 默认路径」）。
- `build_packs_default_gives_changeset_panorama_with_real_sibling_diff`：单格后「兄弟注入」不复存在——改写为单 bundle 版本：同一 bundle 含小文件+超预算大文件，断言小文件全文在场 + 大文件走 def 体回落（真 diff 锚仍必须工作——这正是原测试守的 E8-D 回归，锚从「兄弟」换成「同格文件」）。
- `build_packs_panorama_siblings_ordered_by_dep_rank`：随 dep_rank 删除（钉的是被删行为）。
- `dep_rank` 单元测试：随删。

- [ ] **Step 2: 跑确认失败/编译错** → `cargo test --lib bundles_collapse`。

- [ ] **Step 3: 实现**

`src/cli.rs` 全量路径（现 :560-566 的 gate 块）替换为：

```rust
    // 单格全审默认（E9，2026-07-13）：全变更集并成一个 bundle——分格在全景 pack 范式下
    // =N 倍重复喂全景（两跑实证 input −77%、质量未见下降）。bundle::group 保留（收集与
    // 未来 L4 拆缝用）；超预算由降级梯子（骨架/hunk 窗/省略清单）承接，不拆格。
    let bundles = merge_bundles(group(select_files(changes), Some(root)));
```

增量路径 :659 同样包 `merge_bundles(...)`。删除 `REDTEAM_SINGLE_CELL` env 读及其注释（grep 全仓零残留）。

`build_packs` 内 panorama 兄弟注入循环（克隆 b.files + 追加 other bundles 文件的整段）删除——单 bundle 下无兄弟，直接对 `b` 构建：

```rust
    let entries: Vec<String> = bundles
        .iter()
        .map(|b| {
            let own_diff_bytes: usize = b.files.iter().map(|fc| fc.diff.len()).sum();
            // …（渲染逻辑照旧，pb 即 b）
```

`second_pass_prompt` 的兄弟遍历段（needs 前插后追加其他 bundle 文件的循环）删除——保留 needs 前插 + own 文件。`dep_rank` 函数删除。

- [ ] **Step 4: 全量测试 + 删除性 grep**

Run: `cargo test --lib && cargo test --test review_e2e && grep -rn "REDTEAM_SINGLE_CELL\|dep_rank" src/ tests/ | wc -l`
Expected: 全绿 + grep 计 0；clippy ≤8。

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs
git commit -m "feat(cli): 单格全审转正默认——fan-out 收缩（E9 判决落地）

单格臂两跑实证 input −77%/挂钟 1/3/质量未见系统性下降（E8-B→E9）；
分格在全景 pack 范式下=N 倍重复喂同一全景（大变更集 8 格=651K 实证）。
SINGLE_CELL 仪器转正拆门；panorama 兄弟循环与 dep_rank 随单格范式失去
参照系删除（L4 拆缝立项时从 656ed23/fee48e8 复活，E9 裁定不留死代码）；
超预算由降级梯子承接不拆格。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: lens 去实体化 A——模板固有文本 + rules 参数链/lens 模块/机械钳制退场

**Files:**
- Modify: `src/prompt/templates/adversarial-code-bundle.md`（`{{RULES}}` 占位行）与 `adversarial-doc-bundle.md`（同）
- Modify: `src/prompt/mod.rs`（`rules_block` 删；`build_bundle_prompt`/`build_doc_prompt` 的 `rules: &[&Lens]` 参数删；`{{RULES}}` 插值行删；相关测试）
- Modify: `src/orchestrate/mod.rs`（`run_code_fanout`/`run_doc_fanout` 的 `rules` 参数删；`use crate::lens::Lens` 删；`enrich(raw, fallback_lens, model)` 的 `fallback_lens` 参数删）
- Modify: `src/verdict.rs`（`clamp_compliance` 删 + 其测试）
- Modify: `src/cli.rs`（`clamp_compliance` 调用 :262 删；`select(...)` 两处调用与 `rules` 变量删——lens_ids 数据面留给 Task 3）
- Delete: `src/lens/mod.rs`（`src/lib.rs`/`main.rs` 的 `mod lens` 声明同步删）
- Test: 各文件 tests

**Interfaces:**
- Produces: `build_bundle_prompt(target_label, bundle, focus, others, incremental, dossier)`（6 参）；`build_doc_prompt(input, user_focus)`（2 参）；`run_code_fanout`/`run_doc_fanout` 少一个 rules 参数；`enrich(raw, model)`。Task 3 依赖这些新签名。

**固有攻击面文本（逐字，= 现 rules_block 渲染输出）**——`adversarial-code-bundle.md` 的 `{{RULES}}` 行替换为：

```
- [L1] Security & Boundaries: auth, authorization, tenant isolation, trust boundaries, input validation
- [L2] Data & State: data loss/corruption/duplication, irreversible state changes, rollback safety, idempotency
- [L3] Concurrency & Failure: race conditions, ordering assumptions, re-entrancy, retries, partial failure, timeouts, degraded dependencies, null/empty state
- [L4] Evolution & Observability: version skew, schema drift, migration hazards, compatibility regressions, observability gaps
- [L0] Spec Consistency: whether the implementation drifts from the design intent of the provided spec docs
- [L5] Standards Compliance: coding conventions, naming, structure, team standards; severity low/medium unless a hard project constraint (compliance rule: severity MUST be low or medium unless an explicit hard project constraint, then set hard_constraint=true)
```

`adversarial-doc-bundle.md` 的 `{{RULES}}` 行替换为：

```
- [D1] Internal Consistency: section contradictions, mutually exclusive requirements, self-conflicting constraints
- [D2] Completeness & Ambiguity: missing branches, undefined error handling, under-specification, ambiguity that will bite implementation
- [D3] Hidden Assumptions & Risk: unstated assumptions, unvalidated dependencies; whether the design addresses security/data/failure/migration
- [D4] Testability & Success Criteria: whether success criteria are measurable and acceptance conditions are defined
- [D5] Standards Compliance: whether the document follows team doc standards/templates; severity low/medium unless a hard project constraint (compliance rule: severity MUST be low or medium unless an explicit hard project constraint, then set hard_constraint=true)
```

（标号 L*/D* 保留为纯文本组织符——不再对应任何代码实体；模型输出的 `lens` 键在 Task 3 删字段后被 serde 静默忽略。）

- [ ] **Step 1: 写/改失败测试**

`src/prompt/mod.rs`：既有 `bundle_prompt_interpolates_rules_files_and_contract` 改写——不再构造 `rules` vec，断言固有文本在场：

```rust
#[test]
fn bundle_prompt_carries_builtin_attack_surface() {
    // E8-I 去实体化：攻击面清单是模板固有文本，非渲染产物
    let bundle = bundle_one();
    let p = build_bundle_prompt("working tree", &bundle, "auth", "", "", "");
    assert!(p.contains("[L1] Security & Boundaries"), "code 攻击面固有: {p}");
    assert!(p.contains("[L5] Standards Compliance") && p.contains("hard_constraint=true"), "compliance 散文注记在场");
    assert!(!p.contains("{{RULES}}"), "占位符已烘干");
}
```

（`bundle_one` 等 fixture 以现文件既有为准。）doc 同型测试。`clamp_compliance` 测试随删。

- [ ] **Step 2: 跑确认编译失败** → 参数不符。

- [ ] **Step 3: 实现**（按 Files 清单逐项删/改；`enrich` 内 `raw.lens.as_deref().unwrap_or(fallback_lens)` 一行在本 task 改为 `raw.lens.clone().unwrap_or_default()` 暂存——`RawFinding.lens` 字段本体 Task 3 删，届时该行随之消失；若更省事可在本 task 直接令 `lenses: vec![]` 并注明 Task 3 收尾）。

- [ ] **Step 4: 全量测试 + grep**

Run: `cargo test --lib && cargo test --test review_e2e && grep -rn "rules_block\|clamp_compliance\|crate::lens\|mod lens" src/ | wc -l`
Expected: 全绿 + 0；clippy ≤8。

- [ ] **Step 5: Commit**

```bash
git add -A src/
git commit -m "feat(prompt): lens 去实体化 A——攻击面清单烘干为模板固有文本

E8-I（用户定义：代码零结构）：{{RULES}} 动态渲染→模板固有文本（逐字
=原渲染输出，行为字节级最小变化）；src/lens 模块/rules 参数链/
clamp_compliance 机械钳制退场（E9：钳制唯一实职=L5/D5 cap Medium，
替代=模板 compliance 散文+refute 防线，standards 假 high 记验收观察）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: lens 去实体化 B——数据面/CLI 面/宿主壳清理

**Files:**
- Modify: `src/domain/finding.rs`（`RawFinding.lens` 字段删 + 测试改；`Finding.lenses` 字段删）
- Modify: `src/consolidate/dedup.rs`（:45 lenses merge 行删 + fixture 联动）、`src/consolidate/carry.rs`/`correlate.rs`（fixture 里 lenses/lens 字段联动删）
- Modify: `src/orchestrate/mod.rs`（`enrich` 里 lenses 构造残留清理）
- Modify: `src/config/mod.rs`（`LensSets`/`lenses` 字段/`default_code_lenses`/`default_doc_lenses` 删）
- Modify: `src/main.rs`（`--lenses` clap arg 删、ReviewArgs 组装联动）
- Modify: `src/cli.rs`（`ReviewArgs.lenses` 字段删【serde 旧 args.json 兼容：删字段后 serde 默认忽略未知键 ✓】；`lens_ids` resolve 段 :701-709/:799-804 删；`TailCtx.lens_ids` 与 run 段 `lenses` 输出 :223/:306 删；`on_meta` 调用点少一参）
- Modify: `src/orchestrate/mod.rs`（`CellSink::on_meta(&self, models)` 签名——lenses 参数删）
- Modify: `src/state/job.rs`（`lenses` 字段删 + :188/:208 pinned JSON 测试改）
- Modify: `src/domain/report.rs`（RunMeta 若含 lenses 字段则删——以代码为准）
- Modify: `plugins/redteam/commands/ship.md:13`（透传参数清单去 `--lenses`）→ 跑 `make gen` 重生成 codex-skills → `make verify` 必须 0 FAIL
- Test: 各文件 tests

**Interfaces:**
- Consumes: Task 2 的新签名。
- Produces: 报告/状态 JSON 不再含 lens 相关键；`redteam review --help` 无 `--lenses`。

- [ ] **Step 1: 写/改失败测试**

`src/state/job.rs` pinned JSON 测试（:188/:208）去 `"lenses":[]` 键；`src/domain/finding.rs` 的 `raw_finding_deserializes_quote_and_lens` 改写：

```rust
#[test]
fn raw_finding_ignores_legacy_lens_key() {
    // E8-I：模型仍可能输出 lens 键——serde 静默忽略，不炸不存
    let j = r#"{"severity":"high","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":2,"confidence":0.9,"recommendation":"r","quote":"q","lens":"L1"}"#;
    let r: RawFinding = serde_json::from_str(j).unwrap();
    assert_eq!(r.quote, "q");
}
```

（注意：serde 默认忽略未知键——若 RawFinding 带 `deny_unknown_fields` 则此测试会红，届时去掉该属性；以现代码为准。）

- [ ] **Step 2: 跑确认编译失败**。

- [ ] **Step 3: 实现**（按 Files 清单逐项；`--help` 冒烟确认 `--lenses` 消失）。

- [ ] **Step 4: 全量测试 + grep + make verify**

Run: `cargo test --lib && cargo test --test review_e2e && grep -rn "lenses\|\.lens\b" src/ plugins/redteam/commands/ | grep -v "//" | wc -l && make verify 2>&1 | grep -c FAIL`
Expected: 全绿；grep 仅剩注释/历史文档级残留（正文代码 0）；verify 0 FAIL；clippy ≤8。

- [ ] **Step 5: Commit**

```bash
git add -A src/ plugins/ codex-skills/ 2>/dev/null; git add -A
git commit -m "feat(cli): lens 去实体化 B——数据面/CLI/宿主壳清理

RawFinding.lens/Finding.lenses/LensSets/--lenses flag/state·run lenses
字段全退场（消费面查证：外部零读方，唯一足迹 ship.md 透传一词已删+
make gen 重生成）；旧 args.json/模型输出的 lens 键 serde 静默忽略向后
兼容；注意力引导由 --focus 独家承担（E8-I 代码零结构完成）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## 批 4 验收（controller 手册——非 SDD task）

| 项 | 跑法 | 判什么 |
|----|------|--------|
| 本仓语料单跑 | worktree@6f95c25，新默认（单格+无 lens 实体），DeepSeek | 对照单格臂跑 2（75K/2.5min）：input 同量级、verdict 可读、quote 逐字、**standards 假 high 观察**（clamp 删除后是否有 naming nit 报 high 且存活） |
| ripgrep 12-commit | 同批 2/3 ladder 语料 | 单格+大变更集：梯子承接（骨架/省略在场）、input 对照 651K 应大降（1 格 vs 8 格）、无撞墙（272KB 全景在 128K token 墙内=~90K token ✓ 单格 prompt 峰值与批 2/3 单格相同） |
| doc 冒烟 | design 文档一跑 | doc 单 cell 本就单格——lens 去实体化后 doc 评审照常、needs 机制不回归 |

## Self-Review 记录

- **Spec 覆盖**：E9 判决三项（单格默认/dep_rank 删/clamp 删）→ T1/T2；E8-I 代码零结构清单（参数/常量/enum/模块/flag/字段/报告）→ T2+T3；ship.md+make gen → T3；焦点 prompt 变体（杂交备选）=只有单格质量掉才启用，YAGNI 不建。
- **占位扫描**：fixture 名（bundle_one 等）与 report.rs RunMeta lenses 有无——标注以现代码为准（适配点）；无逻辑占位。
- **类型一致性**：T2 produce 的 6 参/2 参签名 → T3 consume；`enrich(raw, model)` T2 定 → T3 清残留；`merge_bundles` T1 自洽。
- **联动风险自查**：Task 1 改写 panorama 测试保 E8-D 真 diff 回归锚（换参照物不删保护）；Task 3 的 serde 兼容双向验证（旧 args.json 读入/模型 lens 键忽略）。
