# 无工具默认化 批2+批3 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> 状态: 锁定 · design=`.doc/design/2026-07-13-redteam-no-tools-default-design.md` §2.2/§2.3（E8 裁决 C/F/H）· 批 0/1 已合 dev（merge 4f52ac9）

**Goal:** 交付 needs 置换式二阶段补查（批 2）与依赖距离降级梯子+骨架档（批 3），补齐无工具默认形态的证据供给闭环。

**Architecture:** 批 2 三条线：①needs 契约（`LensOutput.needs` 结构化声明，cap 5）②二阶段编排在 orchestrate cell 粒度（`run_code_fanout`/`run_doc_fanout` 接第二趟 prompt 构造器闭包，第一趟 needs 非空→重跑→**整体替换**）③cli 侧构造器（needs 文件置顶置换 pack + 草稿「确认或修订」注入 + ToolExecutor 安全不变量复用）+ refute 证据升档（全文→快照兜底）+ needs 触发率观测。批 3 两条线：①骨架档插入 body_section 回落链（全文→def 体→骨架→hunk±N 兜底）②兄弟按依赖距离排序注入——body_section 顺序填充预算的现有机制自动实现「最远先降」。

**Tech Stack:** Rust 单 crate；无新依赖。

## Global Constraints

- 代码注释简体中文、模型面散文英文——与现仓一致。
- 每 task：`cargo test --lib` 全绿（现基线 381）+ `cargo test --test review_e2e`（7）；`cargo clippy --all-targets 2>&1 | grep -c "warning:"` ≤ **8**；commit 带 trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`；**永不 push**。
- needs 契约逐字（design §2.3 / E8-C）：`needs: [{path, lines?, reason}]`、**cap 5** 超出截断+warning、触发条件=非空即重跑、**恒 ≤2 趟/格**、第二趟**整体替换**第一趟（草稿以「确认或修订」语义随包）、路径安全**复用 ToolExecutor 既有不变量**（safe_join+canonicalize+读帽），不发明第二套。
- 二阶段失败（第二趟 warn）→ 保留第一趟结果 + warning 上浮（保守不丢产出）。
- refute 升档（E8-F）：锚点文件全文（64KB 帽内）→ ±60 快照兜底=现状不倒退；骨架档批 3 T6 后补插。
- 梯子（E8-D/design §2.2）：回落链=全文→改动 def 体→**骨架**→（非支持语言/def 锚不到）**hunk±N 兜底**——禁真空升级为「必有内容」；兄弟注入顺序=依赖距离（直接引用 rank 0 先入）。
- `REDTEAM_PACK_BODY=symbol` 仪器路径（symbol_mode）行为保持 S2-b 原样不动。

---

### Task 1: needs 契约——LensOutput/LensCall 结构 + 模板 output_contract

**Files:**
- Modify: `src/domain/finding.rs`（`LensOutput` 旁加 `NeedEntry`；`LensOutput` 加字段）
- Modify: `src/provider/mod.rs:16`（`LensCall` 加 `needs`）
- Modify: `src/provider/tool_loop.rs`（`Ok(LensCall {...})` 构造处两处塞 `needs: out.needs`——主路径与 repair 路径共用同一构造点则一处）
- Modify: `src/prompt/templates/adversarial-code-bundle.md:50-58`（output_contract）与 `adversarial-doc-bundle.md` 的 output_contract 段
- Test: `src/provider/direct.rs` tests（parse 层）

**Interfaces:**
- Produces: `pub struct NeedEntry { pub path: String, pub lines: Option<String>, pub reason: String }`（Clone/Debug/Deserialize/JsonSchema，`lines` 格式 `"start-end"` 字符串）；`LensOutput.needs: Vec<NeedEntry>`（serde default）；`LensCall.needs: Vec<NeedEntry>`。

- [ ] **Step 1: 写失败测试（parse 层识别 needs、缺省为空）**

`src/provider/direct.rs` tests 追加：

```rust
#[test]
fn parse_lens_output_reads_optional_needs() {
    let with = r#"{"findings": [], "needs": [{"path": "src/state/store.rs", "lines": "40-90", "reason": "verify write ordering"}]}"#;
    let o = parse_lens_output(with).unwrap();
    assert_eq!(o.needs.len(), 1);
    assert_eq!(o.needs[0].path, "src/state/store.rs");
    assert_eq!(o.needs[0].lines.as_deref(), Some("40-90"));
    // 旧输出无 needs 字段 → 空 vec（向后兼容）
    let without = r#"{"findings": []}"#;
    assert!(parse_lens_output(without).unwrap().needs.is_empty());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib parse_lens_output_reads`
Expected: FAIL —— no field `needs`。

- [ ] **Step 3: 实现结构**

`src/domain/finding.rs`（`LensOutput` 定义旁）：

```rust
/// 二阶段补查的证据声明（design 2026-07-13 §2.3，E8 裁决 C）：模型声明「判这个 claim
/// 还缺哪个文件」，非对话式工具调用。lines 可选 "start-end"（超大文件只载区间±上下文）。
#[derive(Clone, Debug, Deserialize, JsonSchema)]
pub struct NeedEntry {
    pub path: String,
    #[serde(default)]
    pub lines: Option<String>,
    pub reason: String,
}
```

`LensOutput` 加：

```rust
    /// 证据缺口声明（可选；非空触发二阶段补查趟）。
    #[serde(default)]
    pub needs: Vec<NeedEntry>,
```

`src/provider/mod.rs:16` `LensCall` 加 `pub needs: Vec<crate::domain::NeedEntry>`（并确认 `crate::domain` re-export——`NeedEntry` 加进 domain 模块的 pub use；以 `RawFinding` 的导出路径为准照抄）。`src/provider/tool_loop.rs` 全部 `LensCall {` 构造点塞 `needs: out.needs.clone()`（`out` 为 parse 产物；测试 mock 构造点塞 `needs: vec![]`）。其他 `LensCall {` 字面量（orchestrate/verify tests 等）grep 补 `needs: vec![]`。

- [ ] **Step 4: 模板 output_contract 更新**

`adversarial-code-bundle.md` output_contract 段（:50-58）在 findings 说明后追加（保持既有语气）：

```
Optionally include a top-level "needs" array (max 5 entries) when a claim's verdict
hinges on evidence NOT present in the provided context: each entry is
{"path": "<repo-relative file>", "lines": "<start-end, optional>", "reason": "<what you must verify>"}.
Declare needs ONLY for genuinely decisive evidence — the review will be re-run once with
those files added. Do NOT declare needs for files already shown in full above.
```

`adversarial-doc-bundle.md` 的 output_contract 段加同段文字。

- [ ] **Step 5: 全量测试 + commit**

Run: `cargo test --lib && cargo test --test review_e2e && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
Expected: 全绿 + ≤8。

```bash
git add src/domain/finding.rs src/provider/mod.rs src/provider/tool_loop.rs src/prompt/templates/adversarial-code-bundle.md src/prompt/templates/adversarial-doc-bundle.md src/provider/direct.rs
git commit -m "feat(needs): 二阶段证据声明契约——LensOutput.needs + 模板 output_contract

design 2026-07-13 §2.3（E8 裁决 C）：needs=[{path,lines?,reason}] 结构化声明
替代对话式工具调用；serde default 向后兼容、cap 与触发编排随后 task。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: needs 流出管道 + cell 级二阶段编排（orchestrate）

**Files:**
- Modify: `src/orchestrate/mod.rs`（`CellRun` 加 needs；`run_one_bundle` 透传；`run_code_fanout`/`run_doc_fanout` 加二阶段参数与执行；cap 5 截断+warning）
- Test: 同文件 tests

**Interfaces:**
- Consumes: `LensCall.needs`（Task 1）。
- Produces: `pub type SecondPrompt<'a> = Option<&'a (dyn Fn(usize, &[crate::domain::NeedEntry], &[crate::domain::Finding]) -> Option<String> + Sync)>;`——`run_code_fanout(..., second_prompt: SecondPrompt)` 尾参；`run_doc_fanout(..., second_prompt: SecondPrompt)`（doc 恒 idx=0）。闭包返回 None=本格不补跑。`RawResults` 加 `pub needs_stats: (u32, u32)`（触发格数, needs 总条数）。

行为规格：
- cell future 内：第一趟 `run_one_bundle` 完成且 `warn.is_none()` 且 needs 非空 → cap 5（超出 truncate + `RawResults.warnings` 追加 `"cell X: N needs declared, capped at 5"`）→ 调闭包构造第二趟 prompt → Some 则重跑一次。
- 第二趟成功（warn 为 None）→ **替换** findings/iterations 取两趟和/tokens 累加/duration 累加/attempts 取 max，`needs` 置空；第二趟失败 → 保留第一趟全部产出 + warning `"cell X second pass failed: ..."`。恒 ≤2 趟。
- CellSink `on_cell` 只在最终结果（替换后）调用一次（同 cell 覆盖语义天然兼容 job store）。
- `needs_stats` 统计第一趟声明（触发前）；模板未列 needs 的旧模型输出 → 空 → 零成本路径与现状 byte-identical。

- [ ] **Step 1: 写失败测试**

`src/orchestrate/mod.rs` tests（沿用现有 FakeProvider/doc_input 模式；provider mock 需支持「同 model 两次调用返回不同 script」——若现 FakeProvider 不支持序列，加一个 `SeqProvider(Mutex<Vec<LensCall>>)` 弹出式 mock）：

```rust
#[tokio::test]
async fn code_fanout_second_pass_replaces_first_on_needs() {
    // 第一趟：1 finding + 1 need；第二趟：2 findings 无 needs → 最终 2 findings（替换非合并）
    let first = lens_call_with(vec![rawf("draft finding")], vec![need("src/x.rs", "verify")]);
    let second = lens_call_with(vec![rawf("confirmed A"), rawf("confirmed B")], vec![]);
    let provider = SeqProvider::new(vec![first, second]);
    let mk = |_idx: usize, needs: &[NeedEntry], drafts: &[Finding]| {
        assert_eq!(needs.len(), 1);
        assert_eq!(drafts.len(), 1, "草稿传入构造器");
        Some("second pass prompt".to_string())
    };
    let out = run_code_fanout(&provider, "t", &bundles_one(), &rules(), "", "", &dossiers_one(), &opts_single(), None, Some(&mk)).await;
    assert_eq!(out.findings.len(), 2, "第二趟整体替换第一趟");
    assert_eq!(out.needs_stats, (1, 1));
}

#[tokio::test]
async fn code_fanout_second_pass_failure_keeps_first() {
    let first = lens_call_with(vec![rawf("draft finding")], vec![need("src/x.rs", "verify")]);
    let provider = SeqProvider::new(vec![first]); // 第二趟无 script → Err
    let mk = |_i: usize, _n: &[NeedEntry], _d: &[Finding]| Some("p2".to_string());
    let out = run_code_fanout(&provider, "t", &bundles_one(), &rules(), "", "", &dossiers_one(), &opts_single(), None, Some(&mk)).await;
    assert_eq!(out.findings.len(), 1, "第二趟失败保留第一趟");
    assert!(out.warnings.iter().any(|w| w.contains("second pass failed")));
}

#[tokio::test]
async fn code_fanout_needs_capped_at_five() {
    let six: Vec<NeedEntry> = (0..6).map(|i| need(&format!("src/f{i}.rs"), "r")).collect();
    let first = lens_call_with(vec![], six);
    let second = lens_call_with(vec![], vec![]);
    let provider = SeqProvider::new(vec![first, second]);
    let mk = |_i: usize, n: &[NeedEntry], _d: &[Finding]| { assert_eq!(n.len(), 5, "cap 5"); Some("p2".into()) };
    let out = run_code_fanout(&provider, "t", &bundles_one(), &rules(), "", "", &dossiers_one(), &opts_single(), None, Some(&mk)).await;
    assert!(out.warnings.iter().any(|w| w.contains("capped at 5")));
}
```

（`lens_call_with`/`need`/`bundles_one`/`dossiers_one`/`opts_single`/`SeqProvider` 为测试助手——`rawf`/`rules` 等同文件已有的照用，缺的按现有 mock 风格补齐；`run_code_fanout` 既有测试调用点全部尾加 `, None` 表示无二阶段。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib code_fanout_second_pass`
Expected: FAIL —— 参数不符。

- [ ] **Step 3: 实现**

`CellRun` 加 `needs: Vec<crate::domain::NeedEntry>`；`run_one_bundle` Ok 分支塞 `needs: call.needs.clone()`、Err 分支 `vec![]`。cell future 核心（replicate 与候补两分支共用助手，避免逻辑重复）：

```rust
/// 二阶段补查（design §2.3）：needs 非空 → cap 5 → 构造器给第二趟 prompt → 重跑一次。
/// 第二趟成功=整体替换（tokens/duration 累加、iterations 取和、attempts 取 max）；
/// 失败=保留第一趟 + warning。恒 ≤2 趟/格。
async fn maybe_second_pass(
    provider: &dyn ReviewProvider, label: &str, opts: &OrchestrateOpts,
    idx: usize, mut run: CellRun, mk: SecondPrompt<'_>,
    warnings: &mut Vec<String>, stats: &mut (u32, u32),
) -> CellRun {
    let Some(mk) = mk else { return run };
    if run.warn.is_some() || run.needs.is_empty() { return run; }
    stats.0 += 1;
    stats.1 += run.needs.len() as u32;
    if run.needs.len() > 5 {
        warnings.push(format!("cell bundle[{label}]: {} needs declared, capped at 5", run.needs.len()));
        run.needs.truncate(5);
    }
    let Some(p2) = mk(idx, &run.needs, &run.findings) else { return run };
    let run2 = run_one_bundle(provider, label, &p2, opts).await;
    match run2.warn {
        None => CellRun {
            model: run2.model,
            findings: run2.findings, // 整体替换
            tokens: (run.tokens.0 + run2.tokens.0, run.tokens.1 + run2.tokens.1, run.tokens.2 + run2.tokens.2),
            duration_ms: run.duration_ms + run2.duration_ms,
            attempts: run.attempts.max(run2.attempts),
            iterations: run.iterations + run2.iterations,
            tool_use: run.tool_use, needs: vec![], warn: None,
        },
        Some(w) => {
            warnings.push(format!("cell bundle[{label}] second pass failed: {w}"));
            run.needs.clear();
            run
        }
    }
}
```

并发注意：cell future 里无法直接 &mut 共享 warnings/stats——把 warning/stats 作为 future 返回值的一部分带出（`(model_hint, run, Vec<String>, (u32,u32))`），join 后在汇总循环累加（照 usage 累加的现有模式）。`maybe_second_pass` 相应改为返回 `(CellRun, Vec<String>, (u32, u32))` 纯值形态。`on_cell` 在二阶段完成后才调（移到 maybe_second_pass 之后）。`RawResults` 加 `needs_stats: (u32, u32)`（Default 0,0）。`run_doc_fanout` 同法加尾参（doc 单 cell，idx 恒 0；replicate 分支同样处理）。既有调用点（cli.rs 两处 code、一处 doc；测试多处）尾加 `None`——cli 真值 Task 3 接。

- [ ] **Step 4: 全量测试 + commit**

Run: `cargo test --lib && cargo test --test review_e2e && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
Expected: 全绿 + ≤8（`maybe_second_pass` 参数多可能撞 too_many_arguments——优先重组为小 struct 或纯值返回签名，实在不行按 run_tool_loop 先例 `#[allow]` 并注明）。

```bash
git add src/orchestrate/mod.rs
git commit -m "feat(orchestrate): cell 级二阶段补查编排——needs 触发置换式重跑

第一趟 needs 非空（cap 5 超出截断+warning）→ 闭包构造第二趟 prompt →
重跑一次整体替换（tokens/duration 累加、iterations 和、恒 ≤2 趟）；
第二趟失败保留第一趟+warning（保守不丢产出）。needs_stats 入 RawResults
（E8 裁决 E：SCIP 判决树的 needs 触发率指标）。doc fanout 同机制（E8-H）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: cli 二阶段 prompt 构造器 + needs 触发率入 result.json

**Files:**
- Modify: `src/cli.rs`（构造器 `second_pass_prompt`；`run_code_review`/增量轮/doc 路径接线；run 段 JSON 加 needs 统计）
- Test: 同文件 tests

**Interfaces:**
- Consumes: Task 2 的 `SecondPrompt` 签名；`crate::tools::ToolExecutor`（safe_join/读帽——若 `read_one` 非 pub(crate) 则提升可见性或提炼 `pub(crate) fn read_capped(&self, rel:&str, lines: Option<(u32,u32)>) -> Option<String>`）；`crate::prompt::build_bundle_prompt`（现签名 7 参照抄）；`pack_budget_bytes`。
- Produces: `fn second_pass_prompt(bundles: &[Bundle], root: &Path, budget: usize, target_label: &str, rules: &[&Lens], focus: &str, idx: usize, needs: &[NeedEntry], drafts: &[Finding]) -> Option<String>`。

行为规格（design §2.3 置换而非追加）：
1. needs 文件读取走 `ToolExecutor::new(root)` 的安全路径（绝对路径/`..`/symlink 逃逸→该条 skip + 不 panic）；带 `lines: Some("a-b")` 解析成功 → 只取 `[a−30, b+30]` 行窗；读帽沿用 executor 既有帽。
2. 第二趟 pack = needs 文件段（置顶，头 `path:起-止（补查证据）`或`path:1-N（补查证据，全文）`）+ 原 0 壳按预算装（先自家后兄弟，装不下的直接省略——置换语义：needs 挤掉最远兄弟）+ 省略清单。实现走最小路径：直接构造 `augmented bundle`——把 needs 文件以 `FileChange{path, diff:String::new()}` **前插**到 pb.files（自家文件之前的更前），复用 `build_pack`/`build_pack_scip` 现有顺序填充（先进先占预算）；needs 有 lines 时读区间写入 tempfile 不可行——改为直接把区间内容作为独立段拼在 dossier 前部（`## 补查证据\n{path}:{a}-{b}\n{content}`），全文 needs 走前插 bundle 路径。
3. 草稿注入：prompt 尾部（output_contract 前）追加段：

```
<first_pass_draft>
Your first pass produced these draft findings. CONFIRM or REVISE each using the
newly added evidence — drop a draft ONLY if the evidence refutes it, and do not
re-declare needs.
{每条: - [severity] title @ file:line_start（quote 头 80 字符）}
</first_pass_draft>
```

4. 触发率观测：`run_code_fanout` 返回的 `needs_stats` 写进 result JSON `run` 段（组装点在 cli.rs run/usage 附近，字段名 `"needs": {"triggered_cells": N, "declared": M}`，两值恒写、零也写——观测面）。

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn second_pass_prompt_injects_needs_evidence_and_drafts() {
    // fixture：两 bundle 各一文件 + 一个变更集外文件 evidence.rs
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"), "fn own() { let z = 1; }\n").unwrap();
    std::fs::write(root.join("src/b.rs"), "fn sib() {}\n").unwrap();
    std::fs::write(root.join("src/evidence.rs"), "pub const LIMIT: usize = 7;\n").unwrap();
    let bundles = vec![
        Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+ let z = 1;\n".into() }] },
        Bundle { files: vec![FileChange { path: "src/b.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn sib() {}\n".into() }] },
    ];
    let needs = vec![NeedEntry { path: "src/evidence.rs".into(), lines: None, reason: "verify LIMIT".into() }];
    let drafts = vec![]; // 空草稿也合法
    let rules = vec![crate::lens::by_id("L1").unwrap()];
    let p = second_pass_prompt(&bundles, root, 1_000_000, "t", &rules, "", 0, &needs, &drafts).unwrap();
    assert!(p.contains("pub const LIMIT"), "needs 文件全文进包: {p}");
    assert!(p.contains("first_pass_draft"), "草稿段在场");
    assert!(p.contains("CONFIRM or REVISE"), "确认或修订语义");
    // 路径逃逸：绝对路径 needs 被 skip、不 panic、其余照常
    let evil = vec![NeedEntry { path: "/etc/passwd".into(), lines: None, reason: "x".into() }];
    let p2 = second_pass_prompt(&bundles, root, 1_000_000, "t", &rules, "", 0, &evil, &drafts);
    if let Some(p2) = p2 { assert!(!p2.contains("root:")); }
}
```

（若 `lens::by_id` 不存在按现仓 lens 获取方式改写——批 4 lens 去实体化前它仍在。）

- [ ] **Step 2: 跑确认失败** → `cargo test --lib second_pass_prompt_injects` FAIL。

- [ ] **Step 3: 实现构造器 + 接线**

按行为规格实现 `second_pass_prompt`；`run_code_review`（全量+增量两处）与 doc 路径构造闭包接进 fanout 尾参：

```rust
    let mk = |idx: usize, needs: &[crate::domain::NeedEntry], drafts: &[crate::domain::Finding]| {
        second_pass_prompt(&bundles, root, pack_budget_bytes(&cfg, &model_names), &target_label, &rules, &focus, idx, needs, drafts)
    };
    let mut raw = run_code_fanout(provider, &target_label, &bundles, &rules, &focus, "", &dossiers, &oopts, sink, Some(&mk)).await;
```

（借用检查冲突时闭包内克隆所需值。doc 路径构造器简化版：文档全文已在 prompt——第二趟 = 原 doc prompt + `## 补查证据` 段 + 草稿段。）run 段 JSON 组装处写入 `needs` 统计对象。

- [ ] **Step 4: 全量测试 + commit**

```bash
git add src/cli.rs
git commit -m "feat(cli): 二阶段 prompt 构造器——needs 置顶置换 + 草稿确认或修订 + 触发率观测

置换语义（design §2.3）：needs 文件前插 0 壳（顺序填充先进先占、最远兄弟
自动让位）、lines 区间走独立补查证据段；路径安全复用 ToolExecutor 不变量
（逃逸 skip 不 panic）；needs 触发率入 result.json run.needs（E8-E 指标）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: refute 证据升档——锚点文件全文（64KB 帽）→ 快照兜底

**Files:**
- Modify: `src/consolidate/verify.rs`（`source_snippet` 改造；常数区）
- Test: 同文件 tests

**Interfaces:**
- Consumes/Produces: `source_snippet(root, &Finding) -> String` 签名不变（内部升档）。

行为规格（E8-F）：新常数 `SNIPPET_FULL_FILE_CAP: usize = 64_000`。文件成功读入（512K 读帽内）且总字节 ≤64_000 → 返回**整文件**，头 `{path}:1-{N}（锚点文件全文）`；超 64KB → 现状 ±60 行快照路径（头/24KB 帽/占位逻辑全部不变——兜底不倒退）。

- [ ] **Step 1: 失败测试**

```rust
#[test]
fn source_snippet_upgrades_to_full_file_within_cap() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("small.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
    let f = finding_at("small.rs", 2, 2); // 助手：构造 file/line_start/line_end 的 Finding，照现有测试造 Finding 的方式写
    let s = source_snippet(tmp.path(), &f);
    assert!(s.contains("（锚点文件全文）"), "64KB 内升档全文: {s}");
    assert!(s.contains("fn a()") && s.contains("fn c()"), "含锚点行窗之外的内容");
}

#[test]
fn source_snippet_falls_back_to_window_over_cap() {
    let tmp = tempfile::tempdir().unwrap();
    let big = "x".repeat(70_000) + "\nfn target() {}\n";
    std::fs::write(tmp.path().join("big.rs"), &big).unwrap();
    let f = finding_at("big.rs", 2, 2);
    let s = source_snippet(tmp.path(), &f);
    assert!(s.contains("（现场快照）"), "超帽回落 ±60 快照: {s}");
}
```

- [ ] **Step 2: 跑确认失败** → FAIL。
- [ ] **Step 3: 实现**（读入逻辑复用现有 File::open+take；在 hit_cap 判定后、行窗计算前插全文分支）。既有 snippet 测试若 fixture 小于 64KB 会走新全文路径——按语义更新断言（内容超集，多数断言应自然通过；钉「现场快照」头的测试改用大 fixture）。
- [ ] **Step 4: 全量测试 + commit**

```bash
git add src/consolidate/verify.rs
git commit -m "feat(refute): 证据升档——锚点文件全文（64KB 帽）→ ±60 快照兜底

E8 裁决 F：refute 是无工具世界唯一质量闸门、finding 数少成本可控；
64KB 内给整文件、超帽回落现状快照（行为不倒退）。骨架档批 3 T6 后补插。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5（批 3）: 骨架档——skeleton 函数 + body_section 回落链插档 + hunk±N 兜底

**Files:**
- Modify: `src/pack/mod.rs`（`skeleton_of` 新函数；`body_section_impl` 回落链）
- Test: 同文件 tests

**Interfaces:**
- Consumes: `crate::dossier::{compile_defs, changed_def_ranges, lang_for}`（现有）。
- Produces: `fn skeleton_of(content: &str, diff: &str, defs: &DefSet) -> String`（`DefSet` 为 `compile_defs` 返回类型的现名——以代码为准）；body_section_impl 回落链变为：全文 → 改动 def 体 → **骨架** → omit；非支持语言：全文 → **hunk±N（±20 行）兜底** → omit（禁真空升级）。

行为规格：
- 骨架 = 逐行扫描匹配 def 正则的行（签名行）全收 + `changed_def_ranges` 命中的 def 输出全文 + 其余行折叠为 `…（N 行折叠）` 标记；块头 `{path}:1-{N}（骨架：签名+改动 def）`。
- 插档条件：全文超预算、且改动 def 体（现路径）也超预算或产出为空（`contributed == false`）→ 试骨架（预算内）→ 仍超 → omit（现状消息）。
- 非支持语言超预算：原直接 omit → 先试 hunk±20 行（从 diff 的 hunk 头 `@@ -a,b +c,d @@` 解析新文件行号 c，取 `[c−20, c+d+20]`）→ 装不下才 omit。
- `symbol_mode`（REDTEAM_PACK_BODY=symbol 仪器）路径不变。

- [ ] **Step 1: 失败测试**

```rust
#[test]
fn body_section_falls_to_skeleton_when_def_body_too_big() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // 改动 def 本身巨大（def 体回落也超预算）+ 另有若干小 fn 签名
    let big_body = "    let x = 0;\n".repeat(300);
    std::fs::write(root.join("src/a.rs"),
        format!("fn tiny_one() {{}}\nfn changed_big() {{\n{big_body}}}\nfn tiny_two() {{}}\n")).unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "src/a.rs".into(),
        diff: "@@ -3,0 +3,1 @@\n+    let x = 0;\n".into(),
    }] };
    let mut omit = Vec::new();
    // 预算 800B：全文（~4.5KB）超、def 体（~4.2KB）超 → 骨架（3 签名行 + 折叠标记）应放得下
    let s = body_section_impl(&b, root, 800, &mut omit, false);
    assert!(s.contains("（骨架：签名+改动 def）") || s.contains("骨架"), "骨架档启用: {s}");
    assert!(s.contains("fn tiny_one()") && s.contains("fn tiny_two()"), "签名行全收");
    assert!(s.contains("折叠"), "折叠标记在场");
}

#[test]
fn body_section_unsupported_lang_falls_to_hunk_window() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    std::fs::write(root.join("notes.md"), &body).unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "notes.md".into(),
        diff: "@@ -100,0 +100,1 @@\n+line 100\n".into(),
    }] };
    let mut omit = Vec::new();
    let s = body_section_impl(&b, root, 900, &mut omit, false);
    assert!(s.contains("line 100") && s.contains("line 85"), "hunk±20 窗在场: {s}");
    assert!(!s.contains("line 10\n"), "窗外内容不在");
}
```

- [ ] **Step 2: 跑确认失败** → FAIL。
- [ ] **Step 3: 实现**（骨架逐行 def 正则匹配——`compile_defs` 产物怎么按行测试匹配，照 `changed_def_ranges`/`symbols_in_file` 内部对 def 正则的用法抄；折叠计数按连续非签名非改动区间聚合）。既有测试 `body_section_gives_full_file_for_unsupported_lang` 的超预算分支断言（omit）会被 hunk 窗改变——更新为新语义。
- [ ] **Step 4: 全量测试 + commit**

```bash
git add src/pack/mod.rs
git commit -m "feat(pack): 骨架档+hunk±N 兜底——回落链全文→def 体→骨架→窗（禁真空升级）

design §2.2 文件内梯子（E8 裁决 D）：骨架=签名地图+改动 def 全文+折叠标记
（符号 pack 对单文件的应用，粒度 A/B 有实测失效模式）；非支持语言超预算
从直接 omit 升级为 hunk±20 行窗。symbol_mode 仪器路径不动。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6（批 3）: 依赖距离排序 + refute 骨架档补插

**Files:**
- Modify: `src/cli.rs`（全景注入循环加兄弟排序）
- Modify: `src/consolidate/verify.rs`（source_snippet 中档插骨架）
- Test: 各自文件 tests

**Interfaces:**
- Consumes: Task 5 的 `skeleton_of`。
- Produces: `fn dep_rank(own: &[FileChange], sib_path: &str, root: &Path) -> u8`（0=own 文件源码词边界引用 sib 的模块名/文件 stem；1=否）。

行为规格：
- 全景循环：兄弟收集后 `sort_by_key(|fc| (dep_rank(&b.files, &fc.path, root), fc.path.clone()))` 再 append——body_section 顺序填充使 rank 1（无引用关系）的兄弟在预算紧张时最先落回落链/omit（=「最远先降」，爆炸半径残余岗位①）。自家文件恒在最前不参与排序。
- `dep_rank` MVP：sib 文件 stem（`b.rs`→`b`，`mod.rs`→取父目录名）；own 每文件 `read_to_string` 后 `\b{stem}\b` regex 词边界搜（复用 dossier 的 regex 构造惯例；读失败按 1 处理）。两级够用——多级图/传递闭包 YAGNI，痛点再升。
- refute 升档链补中档：全文（≤64KB）→ **骨架**（`lang_for` 支持语言且骨架 ≤24KB 时）→ ±60 快照。

- [ ] **Step 1: 失败测试**

```rust
#[test]
fn panorama_siblings_sorted_by_dep_rank() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // own 引用 zeta（词边界），不引用 alpha —— 尽管 alpha 字典序在前，zeta 应排前
    std::fs::write(root.join("src/own.rs"), "use crate::zeta::thing;\nfn f() { zeta::go(); }\n").unwrap();
    std::fs::write(root.join("src/alpha.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(root.join("src/zeta.rs"), "pub fn go() {}\n").unwrap();
    let own = vec![FileChange { path: "src/own.rs".into(), diff: "@@ -1,0 +1,1 @@\n+x\n".into() }];
    assert_eq!(dep_rank(&own, "src/zeta.rs", root), 0, "被 own 引用 → rank 0");
    assert_eq!(dep_rank(&own, "src/alpha.rs", root), 1, "未被引用 → rank 1");
}
```

（集成层排序断言：在现有 panorama 测试上加第三 bundle、断言 pack 中 zeta 段先于 alpha 段出现——`s.find(zeta块头) < s.find(alpha块头)`。）refute 骨架档测试：40KB 超 64KB？——用 >64KB 的 Rust 文件断言输出含「骨架」头而非快照头。

- [ ] **Step 2: 跑确认失败** → FAIL。
- [ ] **Step 3: 实现 + 全量测试**
- [ ] **Step 4: Commit**

```bash
git add src/cli.rs src/consolidate/verify.rs
git commit -m "feat(pack): 兄弟依赖距离排序 + refute 骨架中档

爆炸半径残余岗位①（E8）：直接引用的兄弟先入包、无关兄弟预算紧张时最先
降级（顺序填充天然实现最远先降）；MVP 两级 rank（词边界 stem 搜），多级
图 YAGNI。refute 升档链补中档：全文→骨架→快照。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## 批 2/3 验收（controller 手册——非 SDD task）

1. **needs 端到端冒烟（本仓语料）**：worktree@6f95c25 主臂重跑一次，观察 `run.needs` 统计落盘（触发率预期≈0=B 臂旧证据）；若零触发，构造一个「变更集引用外部常量」的小 fixture 仓手动诱发 needs 验证二阶段真跑通（manifest 双 prompt/替换语义/成本 ≤2 趟）。
2. **E8-E 终判数据**：主臂 vs 纯 0 壳臂（REDTEAM_PACK_SHELLS=0）各一跑对比 `run.needs` 触发率——1 壳砍掉后 needs 是否上升（预防针假说）。不升 → SCIP 线安乐死进入执行（拆除归 M12/M16 清单）。
3. **梯子路径实测**：ripgrep 语料（或更大变更集）一跑，确认骨架/hunk 窗/排序在真实超预算场景出现且 omit 可观测。
4. doc 二阶段冒烟：design 文档 doc 评审一跑，needs 声明与补查趟行为可读。

## Self-Review 记录

- **Spec 覆盖**：§2.3 needs 契约/cap/替换/安全/触发率 → T1-T3；§2.1 refute 升档（E8-F）→ T4+T6；§2.2 文件内梯子骨架+hunk 窗 → T5；依赖距离排序（岗位①）→ T6；E8-H doc 同机制 → T2/T3 内嵌；L4 拆缝=设计已后置不在本 plan；needs 触发率=E8-E 指标 → T3+验收手册。
- **占位扫描**：测试助手（SeqProvider/lens_call_with/finding_at 等）标注按现有 mock 风格补齐——适配点非逻辑占位；`DefSet` 类型名以 compile_defs 实际返回为准（同类）。
- **类型一致性**：`NeedEntry`（T1）→ `LensCall.needs`（T1）→ `CellRun.needs`/`SecondPrompt`（T2）→ `second_pass_prompt`（T3 签名与 T2 闭包参数序一致：idx, needs, drafts）；`skeleton_of`（T5）→ T6 refute 复用；`dep_rank`（T6 自洽）。
