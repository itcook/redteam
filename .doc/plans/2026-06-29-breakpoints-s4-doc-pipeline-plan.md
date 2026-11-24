# breakpoints S4 — doc 对抗评审流水线 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development（推荐）or superpowers:executing-plans 逐 task 执行。Steps 用 checkbox（`- [ ]`）跟踪。

**Goal:** 把 doc 评审线从 Phase-1 的单发 lens×model（DirectProvider，无 anchor/verify）拉到与 code 同等的「工具循环 + anchor + 三层 targeted-verify」，并正确处理 doc 特有的遗漏类 finding。

**Architecture:** doc 复用冻结的 `ToolLoopProvider`（引擎层不动），全 D1–D5 rule 注入一个 prompt、每 model 一次工具循环（`run_doc_fanout`）；`anchor_doc` 把「空引用」当遗漏声明保留、透传到 verify，「编造引用」丢弃；`dedup_doc` 退化到非行号相似度；verify 用 doc 版 refute 模板。顺带落地 carry-to-S4 ①（`compute_verdict` 显式排除 refuted）。

**Tech Stack:** Rust 2021、tokio、async-trait、serde、futures::join_all、tokio::sync::Semaphore。**不新增依赖。**

## Global Constraints

- **引擎层冻结**：`src/tools/`、`src/provider/tool_loop.rs`、`src/provider/backends.rs` 不得修改。doc 必须**原样复用** `ToolLoopProvider`（generic system prompt 不动），所有 doc 框架塞进 user prompt。
- **TLS 必须 rustls**：不得引入 OpenSSL（`cargo tree -i openssl-sys` 须为空）。本计划不碰依赖。
- **ToolExecutor / positioning 必须 root 受限**：`anchor_doc` **绝不**按 model 控制的任意路径读盘——只对 evidence 已收集的受审文档 `(path, content)` 做内存内匹配。
- **输出语言**：finding 文案默认走 `output_language()`（locale → 中文等）；prompt 模板保留 `{{OUTPUT_LANGUAGE}}` 指令。
- **零新 schema**：遗漏类用「空 quote」编码，不新增 Finding 字段。
- **`.doc/`、`.superpowers/` 为 gitignored scratch**，不进版本库。
- 测试替身 `FakeProvider` 是 **prompt-blind**（按 model 名返回脚本，无视 prompt 内容）——凡 e2e 涉及「refute 走不同模板」的断言，不能依赖 prompt 区分，只能依赖 model 路由与 finding 结构。

## 文件结构（创建/修改）

- 修改 `src/verdict.rs`：`compute_verdict` 排除 `verify==Refuted`（Task 1）。
- 修改 `src/consolidate/verify.rs`：注释更新 + `verify_findings`/`refute_one` 加 `Mode` 参数（Task 1 注释、Task 7 签名）。
- 修改 `src/evidence/doc.rs`：新增 `collect_doc_files`，`collect_doc` 复用之（Task 2）。
- 创建 `src/prompt/templates/adversarial-doc-bundle.md`（Task 3）。
- 创建 `src/prompt/templates/adversarial-refute-doc.md`（Task 7）。
- 修改 `src/prompt/mod.rs`：抽 `rules_block` helper、新增 `build_doc_prompt`、`build_refute_prompt` 加 `Mode`（Task 3、Task 7）。
- 修改 `src/orchestrate/mod.rs`：新增 `run_one_doc` + `run_doc_fanout`（Task 4）。
- 修改 `src/positioning/mod.rs`：抽 `locate_quote` helper、新增 `anchor_doc`（Task 5）。
- 修改 `src/consolidate/dedup.rs` + `src/consolidate/mod.rs`：新增 `dedup_doc` + re-export（Task 6）。
- 修改 `src/cli.rs`：`run_review` 改接 doc 新流水线（Task 8）。
- 修改 `src/main.rs`：doc 分支由 `DirectProvider` 换 `ToolLoopProvider`（Task 8）。
- 修改 `tests/review_e2e.rs`：doc 端到端（Task 8）。

---

### Task 1: `compute_verdict` 显式排除 refuted（carry-to-S4 ①）

**Files:**
- Modify: `src/verdict.rs:21-34`（`compute_verdict`）
- Modify: `src/consolidate/verify.rs:67-73`（注释更新，不改逻辑）
- Test: `src/verdict.rs`（内联 `#[cfg(test)]`）

**Interfaces:**
- Consumes: `Finding.verify: Option<Verify>`、`VerifyVerdict::Refuted`（已存在，S3-b）。
- Produces: `compute_verdict` 语义——被证伪的 finding 永不 blocking，不依赖 confidence 数值。

- [ ] **Step 1: 写失败测试**（追加到 `src/verdict.rs` 的 `mod tests`）

```rust
    #[test]
    fn refuted_finding_never_blocks_even_at_zero_floor() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut hi = f(Severity::High, 0.0, "L1", false);
        hi.verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "beta".into(), note: String::new() });
        // conf_floor=0 退化配置：修复前 0.0>=0.0 会 blocking；修复后 refuted 被排除 → Approve。
        assert_eq!(compute_verdict(&[hi], Severity::Medium, 0.0), Verdict::Approve);
    }

    #[test]
    fn upheld_finding_still_blocks() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut hi = f(Severity::High, 0.9, "L1", false);
        hi.verify = Some(Verify { verdict: VerifyVerdict::Upheld, model: "beta".into(), note: String::new() });
        assert_eq!(compute_verdict(&[hi], Severity::Medium, 0.5), Verdict::Reject);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib verdict::tests::refuted_finding_never_blocks_even_at_zero_floor`
Expected: FAIL（当前 refuted High@conf0 在 floor0 下仍 blocking → Reject）

- [ ] **Step 3: 改 `compute_verdict`**（`src/verdict.rs`）

第 1 行 import 加 `VerifyVerdict`：
```rust
use crate::domain::{Finding, Severity, Verdict, VerifyVerdict};
```
替换 `compute_verdict` 的 `blocking` 计算：
```rust
pub fn compute_verdict(
    findings: &[Finding],
    block_threshold: Severity,
    conf_floor: f32,
) -> Verdict {
    let blocking = findings.iter().any(|f| {
        f.severity >= block_threshold
            && f.confidence >= conf_floor
            // 实质修复（carry-to-S4 ①）：被证伪的 finding 永不 blocking，不靠 confidence 数值。
            && f.verify.as_ref().map_or(true, |v| v.verdict != VerifyVerdict::Refuted)
    });
    if blocking { Verdict::Reject } else { Verdict::Approve }
}
```

- [ ] **Step 4: 更新 verify.rs 注释**（`src/consolidate/verify.rs:67-72`）

把原注释替换为：
```rust
        if v.verdict == VerifyVerdict::Refuted {
            // 证伪 → 降 confidence 到 0（作为信号，md 渲染/排序可用）。
            // verdict 的实质排除已落在 compute_verdict（显式 verify==Refuted 不 blocking），
            // 不再依赖该 confidence 数值——故退化配置 conf_floor=0 下亦安全。
            findings[i].confidence = 0.0;
        }
```

- [ ] **Step 5: 跑全部 verdict + verify 测试**

Run: `cargo test --lib verdict:: && cargo test --lib consolidate::verify`
Expected: PASS（新 2 测试绿；既有 verify 测试 `lone_blocking_refuted_drops_confidence_and_marks` 仍绿——confidence=0 保留）

- [ ] **Step 6: Commit**

```bash
git add src/verdict.rs src/consolidate/verify.rs
git commit -m "fix(s4): compute_verdict excludes refuted findings (carry-to-s4 #1)"
```

---

### Task 2: `collect_doc_files` 结构化收集受审文档

**Files:**
- Modify: `src/evidence/doc.rs`
- Test: `src/evidence/doc.rs`（内联）

**Interfaces:**
- Produces: `pub fn collect_doc_files(targets: &[String]) -> anyhow::Result<Vec<(String, String)>>`，返回 `(path, content)` 对，供 `anchor_doc`（Task 5）按文件锚定。`collect_doc` 改为复用之（行为不变）。

- [ ] **Step 1: 写失败测试**（追加到 `src/evidence/doc.rs` 的 `mod tests`）

```rust
    #[test]
    fn collect_doc_files_returns_path_content_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("spec.md");
        std::fs::write(&p, "# Design\nhello").unwrap();
        let ps = p.to_string_lossy().to_string();
        let files = collect_doc_files(&[ps.clone()]).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].0, ps);
        assert!(files[0].1.contains("hello"));
    }

    #[test]
    fn collect_doc_files_empty_errors() {
        assert!(collect_doc_files(&[]).is_err());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib evidence::doc::tests::collect_doc_files_returns_path_content_pairs`
Expected: FAIL（`collect_doc_files` 未定义）

- [ ] **Step 3: 实现**（替换 `src/evidence/doc.rs` 顶部的 `collect_doc`，新增 `collect_doc_files`）

```rust
/// 读取每个目标文档，返回 (path, content) 对——供 anchor_doc 按文件锚定引文。
pub fn collect_doc_files(targets: &[String]) -> anyhow::Result<Vec<(String, String)>> {
    if targets.is_empty() {
        anyhow::bail!("doc mode requires at least one target document");
    }
    let mut out = Vec::new();
    for t in targets {
        let text = std::fs::read_to_string(t)
            .map_err(|e| anyhow::anyhow!("failed to read doc {t}: {e}"))?;
        out.push((t.clone(), text));
    }
    Ok(out)
}

/// 读取 doc 模式目标文档，拼成 {{REVIEW_INPUT}} 主体。
pub fn collect_doc(targets: &[String]) -> anyhow::Result<String> {
    let files = collect_doc_files(targets)?;
    let mut out = String::new();
    for (t, text) in &files {
        out.push_str(&format!("\n===== FILE: {t} =====\n{text}\n"));
    }
    Ok(out)
}
```

- [ ] **Step 4: 跑全部 evidence::doc 测试**

Run: `cargo test --lib evidence::doc`
Expected: PASS（新 2 测试 + 既有 `doc_mode_inlines_file_contents`/`empty_targets_errors` 全绿）

- [ ] **Step 5: Commit**

```bash
git add src/evidence/doc.rs
git commit -m "feat(s4): collect_doc_files returns per-target (path,content) pairs"
```

---

### Task 3: `build_doc_prompt` + doc-bundle 模板（合并一包 ×model）

**Files:**
- Create: `src/prompt/templates/adversarial-doc-bundle.md`
- Modify: `src/prompt/mod.rs`（抽 `rules_block` helper、新增 `build_doc_prompt`）
- Test: `src/prompt/mod.rs`（内联）

**Interfaces:**
- Consumes: `ReviewInput`（`target_label`/`body`/`spec_docs`）、`Lens`（`id`/`title`/`focus`/`max_severity`）。
- Produces: `pub fn build_doc_prompt(input: &ReviewInput, rules: &[&Lens], user_focus: &str) -> String`，供 `run_doc_fanout`（Task 4）使用。教模型「遗漏类留空 quote、引用类给逐字 quote」。

- [ ] **Step 1: 创建模板** `src/prompt/templates/adversarial-doc-bundle.md`

```
<role>
You are performing an ADVERSARIAL review of a spec/design document — prove it is NOT ready to implement.
Break confidence in the document; do not validate it. Give no credit for good intent or partial coverage. Gaps are gaps.
</role>

<rules>
Attack the document against ALL of these rules. For each, surface every material issue you find:
{{RULES}}
</rules>

<tools>
You have read_file / search_repo / list_files under the review root. When the document references a spec, a config, or source files, READ them and check whether the document's claims actually hold. Do not invent content you have not read.
</tools>

<review_method>
Actively disprove readiness: find contradictions, undefined behaviors, missing branches, unstated assumptions, and unmeasurable success criteria.
User focus (weight heavily, but still report other material issues): {{USER_FOCUS}}
Referenced spec/code docs (read them via tools to verify claims): {{SPEC_DOCS}}
</review_method>

<finding_bar>
Report only material findings. No style/naming/low-value cleanup, no evidence-free speculation.
Each finding answers: what is unclear / wrong / missing — why it blocks implementation — impact — concrete fix.
</finding_bar>

<output_language>
Write every finding's title, body, and recommendation in the language the document explicitly requires if it states one; otherwise default to {{OUTPUT_LANGUAGE}}.
</output_language>

<output_contract>
Return ONLY JSON matching the provided schema: an object with a "findings" array.
Each finding: severity (low|medium|high|critical), title, body, file (the document path the finding is about), line_start>=1, line_end>=1, confidence 0..1, recommendation, hard_constraint (boolean, optional), quote.
QUOTE RULES — this determines how the finding is anchored:
- For a CONTRADICTION / INCONSISTENCY / wrong-statement finding: set "quote" to the EXACT verbatim text from the document that is wrong or self-conflicting. A fabricated quote (text not present in the document) causes the finding to be DROPPED.
- For an OMISSION finding (something that SHOULD be specified but is entirely ABSENT from the document): leave "quote" as an empty string "". An empty quote signals an omission claim and is NOT dropped — it is routed to independent verification.
If nothing material, return {"findings": []}. Prefer one strong finding over several weak.
</output_contract>

<document_context>
Target: {{TARGET_LABEL}}
{{REVIEW_INPUT}}
</document_context>
</output>
```

- [ ] **Step 2: 写失败测试**（追加到 `src/prompt/mod.rs` 的 `mod tests`）

```rust
    #[test]
    fn doc_prompt_injects_all_rules_doc_body_and_omission_contract() {
        use crate::domain::{Mode, ReviewInput};
        use crate::lens::by_id;
        let input = ReviewInput {
            mode: Mode::Doc, target_label: "spec.md".into(),
            collection_guidance: String::new(), body: "DOCBODY".into(),
            spec_docs: vec!["design.md".into()],
        };
        let rules = vec![by_id("D1").unwrap(), by_id("D2").unwrap(), by_id("D5").unwrap()];
        let p = build_doc_prompt(&input, &rules, "auth");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("[D1]") && p.contains("[D2]") && p.contains("[D5]"), "all rules injected");
        assert!(p.contains("DOCBODY"), "doc body inlined");
        assert!(p.contains("design.md"), "spec docs injected");
        assert!(p.contains("auth"), "user focus injected");
        assert!(p.to_lowercase().contains("omission"), "omission/empty-quote contract present");
        assert!(p.to_lowercase().contains("compliance rule"), "D5 compliance cap note present");
    }
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test --lib prompt::tests::doc_prompt_injects_all_rules_doc_body_and_omission_contract`
Expected: FAIL（`build_doc_prompt` 未定义）

- [ ] **Step 4: 抽 `rules_block` helper + 新增 `build_doc_prompt`**（`src/prompt/mod.rs`）

在 `const REFUTE_TPL` 之后加：
```rust
const DOC_BUNDLE_TPL: &str = include_str!("templates/adversarial-doc-bundle.md");
```

在 `build_bundle_prompt` 之前新增共享 helper：
```rust
/// 把 rule 集格式化为关注清单（code-bundle 与 doc-bundle 共用）。
fn rules_block(rules: &[&Lens]) -> String {
    rules
        .iter()
        .map(|l| {
            let cap = if l.max_severity.is_some() {
                " (compliance rule: severity MUST be low or medium unless an explicit hard project constraint, then set hard_constraint=true)"
            } else {
                ""
            };
            format!("- [{}] {}: {}{}", l.id, l.title, l.focus, cap)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
```

把 `build_bundle_prompt` 里内联的 `rules_block` 计算替换为调用 helper：
```rust
    let rules_block = rules_block(rules);
```
（即删掉原 `let rules_block = rules.iter().map(...).join("\n");` 整段，替换成上面一行。其余 `diffs_block`/`file_list`/`.replace(...)` 不动。）

新增 `build_doc_prompt`：
```rust
/// 构造 doc 对抗 prompt：全 D 系 rule 注入为一份关注清单、整篇文档内联、教模型遗漏类留空 quote。
pub fn build_doc_prompt(input: &ReviewInput, rules: &[&Lens], user_focus: &str) -> String {
    let spec_docs = if input.spec_docs.is_empty() {
        "(none)".to_string()
    } else {
        input.spec_docs.join(", ")
    };
    DOC_BUNDLE_TPL
        .replace("{{RULES}}", &rules_block(rules))
        .replace("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus })
        .replace("{{SPEC_DOCS}}", &spec_docs)
        .replace("{{TARGET_LABEL}}", &input.target_label)
        .replace("{{REVIEW_INPUT}}", &input.body)
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}
```

注：`build_bundle_prompt` 里局部变量名 `rules_block` 与函数名 `rules_block` 同名会遮蔽——把局部变量改名为 `rules_str`（`let rules_str = rules_block(rules);` 并把后续 `.replace("{{RULES}}", &rules_block)` 改 `&rules_str`），避免遮蔽歧义。

- [ ] **Step 5: 跑全部 prompt 测试**

Run: `cargo test --lib prompt`
Expected: PASS（新测试 + 既有 `bundle_prompt_interpolates_rules_files_and_contract` 等全绿）

- [ ] **Step 6: Commit**

```bash
git add src/prompt/mod.rs src/prompt/templates/adversarial-doc-bundle.md
git commit -m "feat(s4): build_doc_prompt (all D-rules in one prompt) + shared rules_block helper"
```

---

### Task 4: `run_doc_fanout`（整篇文档为单一单元 × model）

**Files:**
- Modify: `src/orchestrate/mod.rs`（import 加 `build_doc_prompt`；新增 `run_one_doc` + `run_doc_fanout`）
- Test: `src/orchestrate/mod.rs`（内联 `mod tests`）

**Interfaces:**
- Consumes: `build_doc_prompt`（Task 3）、`ReviewInput`、`OrchestrateOpts`、`call_with_retry`、`enrich`、`RawResults`、`PlannedModel`（均已存在本模块）。
- Produces: `pub async fn run_doc_fanout(provider, input, rules, focus, opts) -> RawResults`，供 `run_review`（Task 8）使用。replicate=每 model 一遍、失败记 `failed_models`；非 replicate=单条候补链。enrich fallback lens=`"doc"`。

- [ ] **Step 1: 写失败测试**（追加到 `src/orchestrate/mod.rs` 第二个 `mod tests` 里、`code_fanout_*` 测试附近）

```rust
    fn doc_input() -> ReviewInput {
        ReviewInput {
            mode: Mode::Doc, target_label: "spec.md".into(),
            collection_guidance: String::new(), body: "DOCBODY".into(), spec_docs: vec![],
        }
    }

    #[tokio::test]
    async fn doc_fanout_replicate_runs_every_model() {
        use std::collections::BTreeMap;
        let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
        scripted.insert("alpha".into(), vec![raw_at("spec.md", "alpha-issue", Severity::High)]);
        scripted.insert("beta".into(), vec![raw_at("spec.md", "beta-issue", Severity::High)]);
        let provider = crate::provider::fake::FakeProvider { scripted };
        let rules = vec![by_id("D1").unwrap(), by_id("D2").unwrap()];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true, no_fallback: false, max_concurrency: 8, retries: 0,
        };
        let out = run_doc_fanout(&provider, &doc_input(), &rules, "", &opts).await;
        assert_eq!(out.findings.len(), 2, "one finding per model over the single doc");
        let models: std::collections::BTreeSet<&str> =
            out.findings.iter().flat_map(|f| f.models.iter().map(|s| s.as_str())).collect();
        assert!(models.contains("alpha") && models.contains("beta"));
        assert!(out.failed_models.is_empty());
    }

    #[tokio::test]
    async fn doc_fanout_replicate_records_failed_model() {
        let provider = FakeProvider::with("alpha", vec![raw_at("spec.md", "alpha-issue", Severity::High)]);
        let rules = vec![by_id("D1").unwrap()];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true, no_fallback: false, max_concurrency: 8, retries: 0,
        };
        let out = run_doc_fanout(&provider, &doc_input(), &rules, "", &opts).await;
        assert_eq!(out.findings.len(), 1);
        assert!(out.failed_models.contains(&"beta".to_string()));
    }

    #[tokio::test]
    async fn doc_fanout_fallback_chain_first_success_wins() {
        // 非 replicate 候补：claude 无脚本、gpt 有 → 候补命中 gpt，不记 failed_models。
        let provider = FakeProvider::with("gpt", vec![raw_at("spec.md", "found-by-gpt", Severity::High)]);
        let rules = vec![by_id("D1").unwrap()];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "claude".into(), priority: 30 },
                PlannedModel { name: "gpt".into(), priority: 20 },
            ],
            replicate: false, no_fallback: false, max_concurrency: 4, retries: 0,
        };
        let out = run_doc_fanout(&provider, &doc_input(), &rules, "", &opts).await;
        assert_eq!(out.findings.len(), 1);
        assert_eq!(out.findings[0].models, vec!["gpt".to_string()]);
        assert!(out.failed_models.is_empty());
    }
```

注：`raw_at`、`FakeProvider`、`by_id`、`PlannedModel`、`Severity`、`RawFinding`、`ReviewInput`、`Mode` 在该测试模块均已 use（见现有 `code_fanout_*` 测试）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib orchestrate::tests::doc_fanout_replicate_runs_every_model`
Expected: FAIL（`run_doc_fanout` 未定义）

- [ ] **Step 3: import 加 `build_doc_prompt`**（`src/orchestrate/mod.rs:5`）

```rust
use crate::prompt::{build_bundle_prompt, build_doc_prompt, build_prompt};
```

- [ ] **Step 4: 实现 `run_one_doc` + `run_doc_fanout`**（加在 `run_code_fanout` 之后、`#[cfg(test)]` 之前）

```rust
/// 单篇文档评审（整篇文档为单一单元、无 bundle；候补链语义同 run_one_bundle）。
async fn run_one_doc(
    provider: &dyn ReviewProvider,
    input: &ReviewInput,
    rules: &[&Lens],
    focus: &str,
    opts: &OrchestrateOpts,
) -> Result<(Vec<Finding>, (u64, u64)), String> {
    let prompt = build_doc_prompt(input, rules, focus);

    let mut chain: Vec<&PlannedModel> = opts.models.iter().collect();
    if !opts.replicate {
        chain.sort_by_key(|m| Reverse(m.priority));
    }
    let chain: Vec<&PlannedModel> = if opts.replicate || opts.no_fallback {
        chain.into_iter().take(1).collect()
    } else {
        chain
    };

    let mut errs: Vec<String> = Vec::new();
    for pm in &chain {
        match call_with_retry(provider, &pm.name, &prompt, opts.retries).await {
            Ok(call) => {
                let fs = call.findings.iter().map(|r| enrich(r, "doc", &pm.name)).collect();
                return Ok((fs, (call.input_tokens, call.output_tokens)));
            }
            Err(e) => errs.push(format!("doc model {}: {e}", pm.name)),
        }
    }
    Err(errs.join("; "))
}

/// doc 线扇出（整篇文档为单一单元）：
/// - replicate=true：每 model 各跑一遍（对抗复制），失败记 failed_models（守 exit-2）；Semaphore 限并发。
/// - replicate=false：单条候补链（按 priority 降序、首个成功即止），整链失败只记 warning。
pub async fn run_doc_fanout(
    provider: &dyn ReviewProvider,
    input: &ReviewInput,
    rules: &[&Lens],
    focus: &str,
    opts: &OrchestrateOpts,
) -> RawResults {
    if opts.replicate {
        let sem = Arc::new(Semaphore::new(opts.max_concurrency.max(1)));
        let futures = opts.models.iter().map(|pm| {
            let sem = sem.clone();
            let model_name = pm.name.clone();
            let single = OrchestrateOpts {
                models: vec![pm.clone()],
                replicate: true,
                no_fallback: opts.no_fallback,
                max_concurrency: opts.max_concurrency,
                retries: opts.retries,
            };
            async move {
                let _permit = sem.acquire().await.expect("semaphore not closed");
                let res = run_one_doc(provider, input, rules, focus, &single).await;
                (model_name, res)
            }
        });
        let results = futures::future::join_all(futures).await;
        let mut out = RawResults::default();
        for (model_hint, res) in results {
            match res {
                Ok((fs, (it, ot))) => {
                    out.findings.extend(fs);
                    out.usage.0 += it;
                    out.usage.1 += ot;
                }
                Err(warn) => {
                    out.warnings.push(warn);
                    if !out.failed_models.contains(&model_hint) {
                        out.failed_models.push(model_hint);
                    }
                }
            }
        }
        out
    } else {
        // 单一单元候补链：仅一个任务，无需信号量。
        let mut out = RawResults::default();
        match run_one_doc(provider, input, rules, focus, opts).await {
            Ok((fs, (it, ot))) => {
                out.findings.extend(fs);
                out.usage.0 += it;
                out.usage.1 += ot;
            }
            Err(warn) => out.warnings.push(warn),
        }
        out
    }
}
```

- [ ] **Step 5: 跑全部 orchestrate 测试**

Run: `cargo test --lib orchestrate`
Expected: PASS（3 新 doc 测试 + 既有 fanout 测试全绿）

- [ ] **Step 6: Commit**

```bash
git add src/orchestrate/mod.rs
git commit -m "feat(s4): run_doc_fanout (whole-doc unit x model, doc enrich lens)"
```

---

### Task 5: `anchor_doc`（空引用=遗漏保留、编造丢弃）

**Files:**
- Modify: `src/positioning/mod.rs`（抽 `locate_quote` helper；`anchor` 复用之；新增 `anchor_doc`）
- Test: `src/positioning/mod.rs`（内联）

**Interfaces:**
- Consumes: `Finding`（`file`/`quote`/`line_start`/`line_end`）、`normalize_ws`（已存在）。
- Produces: `pub fn anchor_doc(findings: Vec<Finding>, docs: &[(String, String)]) -> Vec<Finding>`。空 quote→保留（透传 verify）；非空且命中→锚行；非空未命中 / file 不匹配任何受审文档→丢弃。**只对 `docs` 内存内匹配，绝不读 model 控制路径。**

- [ ] **Step 1: 写失败测试**（追加到 `src/positioning/mod.rs` 的 `mod tests`）

```rust
    #[test]
    fn anchor_doc_anchors_contradiction_quote() {
        let docs = vec![("spec.md".to_string(), "line one\nthe wrong claim\nline three".to_string())];
        let out = anchor_doc(vec![finding("spec.md", "the wrong claim")], &docs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
    }

    #[test]
    fn anchor_doc_keeps_omission_empty_quote() {
        let docs = vec![("spec.md".to_string(), "whatever".to_string())];
        let out = anchor_doc(vec![finding("spec.md", "")], &docs);
        assert_eq!(out.len(), 1, "omission finding (empty quote) must be kept, not dropped");
    }

    #[test]
    fn anchor_doc_drops_fabricated_quote() {
        let docs = vec![("spec.md".to_string(), "real content only".to_string())];
        let out = anchor_doc(vec![finding("spec.md", "this text is not in the doc")], &docs);
        assert!(out.is_empty(), "fabricated quote must be dropped");
    }

    #[test]
    fn anchor_doc_drops_quote_finding_for_unknown_file() {
        let docs = vec![("spec.md".to_string(), "content".to_string())];
        let out = anchor_doc(vec![finding("other.md", "content")], &docs);
        assert!(out.is_empty(), "quote finding whose file matches no target is dropped");
    }
```

注：`finding(file, quote)` helper 已存在（`src/positioning/mod.rs:74`）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib positioning::tests::anchor_doc_keeps_omission_empty_quote`
Expected: FAIL（`anchor_doc` 未定义）

- [ ] **Step 3: 抽 `locate_quote` helper + `anchor` 复用之**（`src/positioning/mod.rs`）

在 `anchor` 之前新增：
```rust
/// 把 quote 在给定内容中确定性定位到行跨度；空 quote 或未命中返回 None。
fn locate_quote(content: &str, quote: &str) -> Option<(u32, u32)> {
    let file_lines: Vec<String> = content.lines().map(normalize_ws).collect();
    let raw_quote: Vec<String> = quote.lines().map(normalize_ws).collect();
    let first_idx = raw_quote.iter().position(|l| !l.is_empty())?;
    let last_idx = raw_quote.iter().rposition(|l| !l.is_empty())?;
    let first = &raw_quote[first_idx];
    // 先精确整行匹配（避免短/公共子串误锚到更早行），无则退子串包含（保召回）。
    let hit = file_lines
        .iter()
        .position(|fl| fl == first)
        .or_else(|| file_lines.iter().position(|fl| fl.contains(first.as_str())))?;
    let ls = (hit as u32) + 1;
    let span = (last_idx - first_idx + 1) as u32;
    let le = (ls + span - 1).min(file_lines.len() as u32).max(ls);
    Some((ls, le))
}
```

把 `anchor` 函数体替换为复用 helper（行为不变：空 quote / 未命中 → 丢弃）：
```rust
/// 把 quote 确定性定位到真实行；定位失败即丢弃（确定性幻觉过滤，code 线每条 finding 须指向真实代码）。
pub fn anchor(findings: Vec<Finding>, root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    for mut f in findings {
        let content = match safe_read(root, &f.file) {
            Some(c) => c,
            None => continue,
        };
        match locate_quote(&content, &f.quote) {
            Some((ls, le)) => {
                f.line_start = ls;
                f.line_end = le;
                out.push(f);
            }
            None => continue,
        }
    }
    out
}
```

- [ ] **Step 4: 新增 `anchor_doc`**（加在 `anchor` 之后）

```rust
/// doc 版锚定（空引用=遗漏声明 carve-out）：
/// - quote 为空/纯空白 → 遗漏类声明：原样保留（无引用可锚），透传到三层 verify 判真伪。
/// - quote 非空且在对应受审文档中命中 → 锚定到行。
/// - quote 非空但文档中找不到（编造）或 finding.file 不匹配任何受审文档 → 丢弃。
/// docs：受审文档的 (path, content) 对（来自 evidence::doc::collect_doc_files）。
/// 仅按已收集内容做内存内匹配，绝不按 model 控制的任意路径读盘。
pub fn anchor_doc(findings: Vec<Finding>, docs: &[(String, String)]) -> Vec<Finding> {
    let mut out = Vec::new();
    for mut f in findings {
        if f.quote.split_whitespace().next().is_none() {
            // 遗漏类：原样保留。
            out.push(f);
            continue;
        }
        // 引用类：按 finding.file 找对应受审文档内容（精确或后缀匹配）。
        let content = docs.iter().find_map(|(path, content)| {
            if path == &f.file || path.ends_with(&f.file) {
                Some(content.as_str())
            } else {
                None
            }
        });
        match content.and_then(|c| locate_quote(c, &f.quote)) {
            Some((ls, le)) => {
                f.line_start = ls;
                f.line_end = le;
                out.push(f);
            }
            None => continue, // 编造引用 / 未知文件 → 丢弃
        }
    }
    out
}
```

- [ ] **Step 5: 跑全部 positioning 测试**

Run: `cargo test --lib positioning`
Expected: PASS（4 新 anchor_doc 测试 + 既有 anchor 测试全绿——`anchor` 行为不变）

- [ ] **Step 6: Commit**

```bash
git add src/positioning/mod.rs
git commit -m "feat(s4): anchor_doc keeps omission (empty quote), drops fabricated; shared locate_quote"
```

---

### Task 6: `dedup_doc`（退化到非行号相似度）

**Files:**
- Modify: `src/consolidate/dedup.rs`（新增 `dedup_doc`）
- Modify: `src/consolidate/mod.rs`（re-export `dedup_doc`）
- Test: `src/consolidate/dedup.rs`（内联）

**Interfaces:**
- Consumes: `title_jaccard`、`TITLE_SIM`、`merge_into`（均同模块私有，已存在）。
- Produces: `pub fn dedup_doc(findings: Vec<Finding>) -> Vec<Finding>`，同 file + 标题相似即并（**不要求行重叠**），合并 bump agreement / union models。`pub use dedup::dedup_doc;`。

- [ ] **Step 1: 写失败测试**（追加到 `src/consolidate/dedup.rs` 的 `mod tests`）

```rust
    #[test]
    fn dedup_doc_merges_same_title_cross_model_ignoring_lines() {
        let v = vec![
            f("1","spec.md",0,0,"missing error branch","claude",Severity::High),
            f("2","spec.md",99,99,"Missing error branch","gpt",Severity::High),
        ];
        let out = dedup_doc(v);
        assert_eq!(out.len(), 1, "same title merges regardless of line distance");
        assert_eq!(out[0].agreement, 2);
    }

    #[test]
    fn dedup_doc_keeps_distinct_titles() {
        let v = vec![
            f("1","spec.md",0,0,"missing error branch","claude",Severity::High),
            f("2","spec.md",0,0,"ambiguous retry policy","claude",Severity::High),
        ];
        assert_eq!(dedup_doc(v).len(), 2);
    }

    #[test]
    fn dedup_doc_different_files_never_merge() {
        let v = vec![
            f("1","a.md",0,0,"same title","claude",Severity::High),
            f("2","b.md",0,0,"same title","claude",Severity::High),
        ];
        assert_eq!(dedup_doc(v).len(), 2);
    }
```

注：`f(id,file,ls,le,title,model,sev)` helper 已存在（`src/consolidate/dedup.rs:53`）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib consolidate::dedup::tests::dedup_doc_merges_same_title_cross_model_ignoring_lines`
Expected: FAIL（`dedup_doc` 未定义）

- [ ] **Step 3: 实现 `dedup_doc`**（加在 `dedup` 之后，`src/consolidate/dedup.rs`）

```rust
/// doc 版去重（退化到非行号相似度）：同 file + 标题相似即并，不要求行重叠。
/// 遗漏类 finding 无有意义行号，故 doc 线按主题（标题）聚类。
pub fn dedup_doc(findings: Vec<Finding>) -> Vec<Finding> {
    let mut clusters: Vec<Finding> = Vec::new();
    for f in findings {
        let mut merged = false;
        for c in clusters.iter_mut() {
            if c.file == f.file && title_jaccard(&c.title, &f.title) >= TITLE_SIM {
                merge_into(c, &f);
                merged = true;
                break;
            }
        }
        if !merged {
            clusters.push(f);
        }
    }
    clusters
}
```

- [ ] **Step 4: re-export**（`src/consolidate/mod.rs:5`）

```rust
pub use dedup::{dedup, dedup_doc};
```

- [ ] **Step 5: 跑全部 consolidate 测试**

Run: `cargo test --lib consolidate`
Expected: PASS（3 新 dedup_doc 测试 + 既有全绿）

- [ ] **Step 6: Commit**

```bash
git add src/consolidate/dedup.rs src/consolidate/mod.rs
git commit -m "feat(s4): dedup_doc merges by file+title (non-line similarity)"
```

---

### Task 7: doc refute 模板 + `Mode` 穿线进 verify

**Files:**
- Create: `src/prompt/templates/adversarial-refute-doc.md`
- Modify: `src/prompt/mod.rs`（`build_refute_prompt` 加 `Mode`、空 quote 友好渲染）
- Modify: `src/consolidate/verify.rs`（`verify_findings`/`refute_one` 加 `Mode` 参数）
- Modify: `src/cli.rs:260-262`（`run_code_review` 调 `verify_findings` 传 `Mode::Code`）
- Test: `src/prompt/mod.rs` + `src/consolidate/verify.rs`（内联）

**Interfaces:**
- Produces: `build_refute_prompt(finding: &Finding, mode: Mode) -> String`（Code→code 模板、Doc→doc 模板；空 quote 替换为占位说明）；`verify_findings(..., mode: Mode)`、`refute_one(..., mode: Mode)`。
- Consumes: 由 `run_code_review`（Mode::Code）与 `run_review`（Mode::Doc，Task 8）调用。

- [ ] **Step 1: 创建 doc refute 模板** `src/prompt/templates/adversarial-refute-doc.md`

```
<role>
You are a skeptical verifier. A prior adversarial review of a DESIGN/SPEC DOCUMENT CLAIMED the problem below — a contradiction, an ambiguity, or an OMISSION (something the document allegedly fails to specify). Your job is to REFUTE the claim: show, with concrete evidence from the document or the specs/code it references, that the claim is a false positive — the document actually addresses it, the alleged contradiction is not real, or the allegedly-missing content is in fact present.
</role>

<claimed_finding>
Document: {{FILE}}
Title: {{TITLE}}
Severity: {{SEVERITY}}
Body: {{BODY}}
Claimed evidence (empty means the claim is that this is OMITTED entirely from the document): {{QUOTE}}
</claimed_finding>

<tools>
You have read_file / search_repo / list_files under the review root. Read the document and any spec/config/source it references before judging. For an omission claim, search the document and referenced specs for the content alleged to be missing. Do not invent content you have not read.
</tools>

<method>
Attack the claim: For a contradiction/ambiguity, is the quoted text real and actually conflicting in context? For an omission, is the content genuinely absent, or is it specified elsewhere in the document or a referenced spec? Only a concrete, grounded refutation counts.
</method>

<output_contract>
Return ONLY JSON: an object with a "findings" array.
- If you find concrete, grounded reasons the claim is WRONG / a false positive / actually addressed: return EXACTLY ONE finding whose title is the refutation reason and whose body cites the evidence. A non-empty array marks the claim REFUTED.
- If, after reading, the claim HOLDS and you cannot refute it: return {"findings": []}. An empty array marks the claim UPHELD.
When in doubt, do NOT fabricate a refutation — return an empty array.
Write every title and body in {{OUTPUT_LANGUAGE}}.
</output_contract>
</output>
```

- [ ] **Step 2: 写失败测试**

prompt（追加到 `src/prompt/mod.rs` 的 `mod tests`）：
```rust
    #[test]
    fn refute_prompt_doc_omission_frames_empty_quote() {
        use crate::domain::{Finding, Mode, Severity};
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "missing rollback branch".into(),
            body: "no rollback path on failure".into(), file: "spec.md".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "specify rollback".into(),
            lenses: vec!["D2".into()], models: vec!["alpha".into()], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: String::new(), verify: None,
        };
        let p = build_refute_prompt(&f, Mode::Doc);
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.to_lowercase().contains("omit"), "doc refute frames omission");
        assert!(!p.contains("Quoted code"), "doc template (not code template)");
    }
```

verify（追加到 `src/consolidate/verify.rs` 的 `mod tests`）：
```rust
    #[tokio::test]
    async fn doc_omission_lone_blocking_refuted() {
        use crate::domain::Mode;
        // doc 模式：空引用遗漏类孤证 High → refute（doc 模板路径）。skeptic 返回非空 → refuted。
        let provider = FakeProvider::with("beta", vec![rawf("actually specified in section 3")]);
        let mut f = finding("missing rollback branch", Severity::High, 1, "alpha");
        f.quote = String::new();
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Doc).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(out[0].confidence, 0.0);
    }
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test --lib prompt::tests::refute_prompt_doc_omission_frames_empty_quote`
Expected: FAIL（`build_refute_prompt` 仍是单参；`Mode` 未穿线）

- [ ] **Step 4: 改 `build_refute_prompt`**（`src/prompt/mod.rs`）

`const REFUTE_TPL` 之后加：
```rust
const REFUTE_DOC_TPL: &str = include_str!("templates/adversarial-refute-doc.md");
```

替换 `build_refute_prompt`：
```rust
/// 构造 refute prompt：给定 claimed finding，让 skeptic 对抗复审。
/// 复用 review-as-refute——返回的 findings 空=upheld、非空=refuted（见模板 output_contract）。
/// mode 选模板：Code→code 模板、Doc→doc 模板（空 quote 渲染为遗漏占位说明）。
pub fn build_refute_prompt(finding: &crate::domain::Finding, mode: Mode) -> String {
    let tpl = match mode {
        Mode::Code => REFUTE_TPL,
        Mode::Doc => REFUTE_DOC_TPL,
    };
    let quote = if finding.quote.split_whitespace().next().is_none() {
        "(empty — the finding claims this is omitted from the document)".to_string()
    } else {
        finding.quote.clone()
    };
    tpl.replace("{{FILE}}", &finding.file)
        .replace("{{LINE_START}}", &finding.line_start.to_string())
        .replace("{{LINE_END}}", &finding.line_end.to_string())
        .replace("{{TITLE}}", &finding.title)
        .replace("{{SEVERITY}}", finding.severity.as_str())
        .replace("{{BODY}}", &finding.body)
        .replace("{{QUOTE}}", &quote)
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}
```

更新既有测试 `refute_prompt_carries_finding_and_upheld_empty_contract`：把 `build_refute_prompt(&f)` 改为 `build_refute_prompt(&f, crate::domain::Mode::Code)`。

- [ ] **Step 5: `Mode` 穿线进 verify**（`src/consolidate/verify.rs`）

第 1 行 import 加 `Mode`：
```rust
use crate::domain::{Finding, Mode, Severity, Verify, VerifyVerdict};
```

`refute_one` 加参数 + 传给 `build_refute_prompt`：
```rust
async fn refute_one(provider: &dyn ReviewProvider, finding: &Finding, models: &[String], mode: Mode) -> Verify {
    let model = match pick_cross_model(models, &finding.models) {
        Some(m) => m.to_string(),
        None => return Verify { verdict: VerifyVerdict::Upheld, model: String::new(), note: "no model".into() },
    };
    let prompt = build_refute_prompt(finding, mode);
    match provider.run(LensRequest { model: model.clone(), prompt }).await {
        Ok(call) if !call.findings.is_empty() => {
            let note = call.findings.first().map(|f| f.title.clone()).unwrap_or_default();
            Verify { verdict: VerifyVerdict::Refuted, model, note }
        }
        Ok(_) => Verify { verdict: VerifyVerdict::Upheld, model, note: String::new() },
        Err(_) => Verify { verdict: VerifyVerdict::Upheld, model, note: "refute unavailable".into() },
    }
}
```

`verify_findings` 加参数 + 传给 `refute_one`：
```rust
pub async fn verify_findings(
    mut findings: Vec<Finding>,
    provider: &dyn ReviewProvider,
    models: &[String],
    block_threshold: Severity,
    max_concurrency: usize,
    mode: Mode,
) -> Vec<Finding> {
```
函数体内 `refute_one(provider, f, models).await` → `refute_one(provider, f, models, mode).await`。

更新该文件既有 verify 测试的全部 `verify_findings(...)` 调用：在末尾加 `, Mode::Code`（这些测试均 code-flavored）。共 5 处（`lone_blocking_refuted_drops_confidence_and_marks`、`lone_blocking_upheld_keeps_confidence`、`consensus_finding_is_skipped`、`below_block_threshold_is_skipped`、`refute_failure_is_conservatively_upheld` 内 2 处调用）。在 `mod tests` 顶部 `use` 加 `use crate::domain::Mode;`。

- [ ] **Step 6: 改 `run_code_review` 调用点**（`src/cli.rs:260-262`）

```rust
    let findings = crate::consolidate::verify_findings(
        findings, provider, &model_names, block, cfg.review.max_concurrency, Mode::Code,
    ).await;
```

- [ ] **Step 7: 跑相关测试 + 全量编译**

Run: `cargo test --lib prompt && cargo test --lib consolidate::verify && cargo build`
Expected: PASS（新 doc refute 测试 + 既有全绿；cli.rs 编译通过）

- [ ] **Step 8: Commit**

```bash
git add src/prompt/mod.rs src/prompt/templates/adversarial-refute-doc.md src/consolidate/verify.rs src/cli.rs
git commit -m "feat(s4): doc refute template; thread Mode through verify_findings/build_refute_prompt"
```

---

### Task 8: 接线 `run_review` 新 doc 流水线 + main.rs ToolLoop + e2e

**Files:**
- Modify: `src/cli.rs`（imports；`run_review` body 第 4–7 步）
- Modify: `src/main.rs`（doc 分支由 DirectProvider 换 ToolLoopProvider；删 DirectProvider import）
- Test: `tests/review_e2e.rs`（新增 doc e2e）

**Interfaces:**
- Consumes: `run_doc_fanout`（Task 4）、`anchor_doc`（Task 5）、`dedup_doc`（Task 6）、`verify_findings(...Mode)`（Task 7）、`collect_doc_files`（Task 2）。
- Produces: `run_review` 走「fan-out → anchor_doc → dedup_doc → clamp → correlate(桩) → verify(Doc) → verdict」；main.rs 两 mode 统一 ToolLoopProvider。

- [ ] **Step 1: 写 e2e 失败测试**（追加到 `tests/review_e2e.rs`）

```rust
#[tokio::test]
async fn doc_review_keeps_omission_drops_fabricated_anchors_contradiction() {
    use breakpoints::cli::{run_review, ReviewArgs};
    use breakpoints::domain::{RawFinding, Severity, Verdict};
    use std::collections::BTreeMap;

    let dir = tempfile::tempdir().unwrap();
    let doc_path = dir.path().join("spec.md");
    std::fs::write(
        &doc_path,
        "intro line\nthe system MUST never retry\nlater the system retries 3 times\nconclusion\n",
    ).unwrap();
    let dp = doc_path.to_string_lossy().to_string();

    fn rf(file: &str, title: &str, quote: &str, sev: Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: quote.into(), lens: None,
        }
    }
    let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
    scripted.insert("alpha".into(), vec![
        rf(&dp, "self-contradiction on retry", "the system MUST never retry", Severity::High),
        rf(&dp, "missing rollback branch", "", Severity::High),                  // 遗漏：空引用
        rf(&dp, "fabricated claim", "this text is nowhere in the doc", Severity::High), // 编造
    ]);
    scripted.insert("beta".into(), vec![
        rf(&dp, "self-contradiction on retry", "the system MUST never retry", Severity::High),
    ]);
    let provider = breakpoints::provider::fake::FakeProvider { scripted };

    let cfg = breakpoints::config::Config::from_layers(&[
        "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
         [providers.beta]\nkind=\"openai\"\nmodel=\"b\"\npriority=5\n\
         [review]\ndefault_models=[\"alpha\",\"beta\"]\n",
    ]).unwrap();

    let args = ReviewArgs {
        mode: Some("doc".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: None, models: None, models_all: false,
        no_correlate: false, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![dp.clone()],
    };
    let (report, code) = run_review(args, &provider, &cfg).await.unwrap();

    // 矛盾：两 model 合并 agreement=2、锚到第 2 行。
    let contra = report.findings.iter()
        .find(|f| f.title.contains("contradiction")).expect("contradiction kept");
    assert_eq!(contra.agreement, 2);
    assert_eq!(contra.line_start, 2, "anchored to the contradicting line");
    // 遗漏：空引用，未被 anchor 丢弃（doc carve-out 的核心）。
    assert!(report.findings.iter().any(|f| f.title.contains("rollback")),
        "omission finding must survive anchor");
    // 编造引文：被丢弃。
    assert!(!report.findings.iter().any(|f| f.title.contains("fabricated")),
        "fabricated quote dropped");
    // verdict：矛盾 High agreement=2（跳过 verify）→ Reject、exit 10。
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
}
```

- [ ] **Step 2: 跑 e2e 确认失败**

Run: `cargo test --test review_e2e doc_review_keeps_omission_drops_fabricated_anchors_contradiction`
Expected: FAIL（旧 `run_review` 仍是单发 lens×model、无 anchor/verify）

- [ ] **Step 3: 改 cli.rs imports**（`src/cli.rs:3,8,10`）

```rust
use crate::consolidate::{apply_clusters, correlate, dedup, dedup_doc};
```
```rust
use crate::orchestrate::{run_code_fanout, run_doc_fanout, OrchestrateOpts, PlannedModel};
```
```rust
use crate::positioning::{anchor, anchor_doc};
```
（即：`consolidate` 加 `dedup_doc`；`orchestrate` 把 `run_fanout` 换成 `run_doc_fanout`——`run_fanout` 不再被 cli 使用，仍 pub+被 orchestrate 自身测试用，故只从 cli import 移除；`positioning` 加 `anchor_doc`。）

- [ ] **Step 4: 改 `run_review` body 第 4–7 步**（`src/cli.rs:154-173`）

把现有「4. fan-out / 5. consolidate / 6. verdict」三段（从 `let focus = ...` 到 `let verdict = compute_verdict(...)`）替换为：
```rust
    // 4. fan-out（doc：全 D rule 一包 × model 工具循环）
    let focus = args.focus.clone().unwrap_or_default();
    let raw = run_doc_fanout(provider, &input, &lenses, &focus, &oopts).await;

    // 5. positioning（doc anchor：空引用=遗漏保留、编造丢弃；按受审文档内容锚定）
    let docs = crate::evidence::doc::collect_doc_files(&args.targets)?;
    let anchored = anchor_doc(raw.findings, &docs);

    // 6. consolidate：dedup_doc（非行号相似度）→ clamp → correlate(桩) → verify(三层，doc refute)
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
    let mut findings = dedup_doc(anchored);
    clamp_compliance(&mut findings);
    let correlate_on = cfg.review.correlate && !args.no_correlate;
    let lead_model = model_names.first().cloned().unwrap_or_default();
    let clusters = correlate(provider, &lead_model, &findings, correlate_on).await;
    apply_clusters(&mut findings, &clusters);
    let findings = crate::consolidate::verify_findings(
        findings, provider, &model_names, block, cfg.review.max_concurrency, Mode::Doc,
    ).await;

    // 7. verdict
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let verdict = compute_verdict(&findings, block, floor);
```
（第 7 步「组装报告」`let stats = ...` 起、第 8 步 exit_code 不动。`clusters` 仍来自 correlate，report.clusters 不变。注意原 body 里 `let block` 在第 6 步顶部定义并被 verdict 用——上面已把 `block` 上移到第 6 步开头，verdict 复用同一个 `block`，删掉原第 6 步里重复的 `let block`。）

- [ ] **Step 5: 改 main.rs doc 分支**（`src/main.rs`）

删除第 7 行 `use breakpoints::provider::direct::DirectProvider;`。
把 registry 构造的 `match detected { Mode::Code => {...} Mode::Doc => {...} }`（第 118–132 行）整体替换为不分 mode 的统一构造：
```rust
                let backend = match backend_from_config(&pc, key) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(2);
                    }
                };
                registry.insert(pm.name.clone(), Box::new(ToolLoopProvider::new(backend, cwd.clone())));
```
（即 doc 也走 ToolLoopProvider——它自带的 read_file/search_repo 工具让模型核对文档对被引用 spec/代码的引述。下方 `match detected { Mode::Code => run_code_review(...), Mode::Doc => run_review(...) }` 不动。）

- [ ] **Step 6: 跑 e2e + 全量测试 + clippy**

Run: `cargo test && cargo clippy --all-targets -- -D warnings`
Expected: PASS（新 doc e2e 绿；全部既有测试绿；clippy 干净，无 `DirectProvider` 未用 import 警告）

- [ ] **Step 7: 确认 TLS 后端无回归**

Run: `cargo tree -i openssl-sys`
Expected: 空（无 openssl-sys）

- [ ] **Step 8: Commit**

```bash
git add src/cli.rs src/main.rs tests/review_e2e.rs
git commit -m "feat(s4): wire run_review to doc tool-loop pipeline (fanout/anchor_doc/dedup_doc/verify); main uses ToolLoop for doc"
```

---

## Self-Review（写完计划后的检查）

- **Spec 覆盖**（roadmap §S4 + 4 项决策）：doc 上工具循环=Task 8（main ToolLoop）+ Task 3（read 工具说明进 prompt）✓；合并一包 ×model=Task 3+4 ✓；空引用=遗漏=Task 5（anchor_doc）+ Task 3（prompt 教模型）✓；dedup 退化=Task 6 ✓；doc verify=Task 7（doc refute 模板）+ Task 8（verify Mode::Doc 接线）✓；conf_floor=0 实质修复=Task 1 ✓；correlate 沿用桩=Task 8（保留 correlate/apply_clusters 调用）✓。
- **类型一致性**：`build_doc_prompt(&ReviewInput,&[&Lens],&str)`、`run_doc_fanout(&dyn ReviewProvider,&ReviewInput,&[&Lens],&str,&OrchestrateOpts)`、`anchor_doc(Vec<Finding>,&[(String,String)])`、`dedup_doc(Vec<Finding>)`、`build_refute_prompt(&Finding,Mode)`、`verify_findings(...,Mode)` 在定义与调用处一致 ✓。`collect_doc_files -> Vec<(String,String)>` 与 anchor_doc 入参一致 ✓。
- **占位符扫描**：每 step 含完整代码/模板，无 TODO/TBD ✓。
- **跨 task 编译依赖**：Task 7 改 `verify_findings` 签名 → 同 task 内更新 `run_code_review` 调用点（Step 6）；Task 8 改 cli imports（移除 `run_fanout`、加 `run_doc_fanout`/`anchor_doc`/`dedup_doc`）；Task 6 须同步 `consolidate/mod.rs` re-export，否则 Task 8 import 失败 ✓。
- **FakeProvider prompt-blind**：e2e 的 Reject 来自 agreement=2 的矛盾（跳过 verify），不依赖 prompt 区分模板；遗漏 finding 仅断言「存活」不断言其 verify 结论（prompt-blind 下 beta refute 返回非空→refuted，但不影响 Reject）✓。
- **冻结约束**：无 task 触碰 `src/tools/`、`tool_loop.rs`、`backends.rs`；doc 复用 `ToolLoopProvider::new`（公有构造，非修改）✓。
