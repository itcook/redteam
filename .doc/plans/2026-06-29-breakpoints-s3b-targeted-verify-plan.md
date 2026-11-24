# S3-b targeted-verify Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 给 code 评审加三层 targeted-verify 尾段——对孤证（`agreement==1 ∧ severity≥block_threshold`）做定向 refute，被证伪的 finding 标记 `verify=refuted` 并降 confidence 下 floor（不翻 verdict、报告仍可见），守 verdict 完整性。

**Architecture:** verify 用 **review-as-refute** 复用现有 `ReviewProvider`/registry/`ToolLoopProvider`（零引擎改动）：让一个 skeptic model（优先选 registry 里 ≠ 原 finding 的 model，交叉验证）对 claimed finding 做对抗复审，返回的「反驳点 findings」**空=upheld / 非空=refuted**。pipeline 插入 `anchor → dedup → clamp → verify → verdict`。`Finding` 加 `Option<Verify>` 字段；refuted → `confidence=0.0`。顺带抽 `exit_code` helper 消除退出码重复（S3-a opus 终审建议）。correlate 仍是桩（继续推后）。

**Tech Stack:** Rust 2021, tokio(sync), async-trait, futures::join_all, serde/schemars。verify 复用 `ReviewProvider::run`（LensCall.findings 空/非空编码 upheld/refuted）。FakeProvider 脚本化 refute，全程无真实 key。

## Global Constraints

每个 task 的要求都隐含包含本节，逐条 verbatim：

- **rustls-only，零 OpenSSL**：不引入任何新依赖；`cargo tree -i openssl-sys` 必须空。
- **引擎层冻结**：禁止修改 `src/tools/`、`src/provider/tool_loop.rs`、`src/provider/backends.rs`。verify 复用 `ReviewProvider` trait（registry 路由到 `ToolLoopProvider`），不碰引擎。
- **doc 路径无回归**：`run_review`（doc）**不加 verify**（doc verify 留 S4）；它只随 `exit_code` helper 抽取做纯重构。doc 评审仍 lens×model 单发。
- **pipeline 顺序**：`run_code_review` 为 `select_files → group → run_code_fanout → anchor → dedup → clamp → verify → compute_verdict`。verify 在 dedup 之后（agreement 已算）、verdict 之前。
- **§7.4 severity 双保险**：不改 `clamp_compliance`、不改 `enrich` 的 lens 映射。verify 在 clamp 之后运行。
- **verify 机制（review-as-refute）**：复用 `ReviewProvider::run(LensRequest{model, prompt})`；refute 返回的 `LensCall.findings` **空=upheld、非空=refuted**；refute 调用失败 → **upheld**（保守：不冤枉、保留 finding 的 blocking 力）。
- **交叉 model**：refute 优先选 `models` 里第一个不在 `finding.models` 的 model；没有（单 model / 全部命中）则退化用 `models[0]`（自审）。
- **证伪处理**：refuted → 设 `verify=Some(Refuted)` 且 `confidence = 0.0`（降到默认 `conf_floor=0.5` 之下，`compute_verdict` 的 `confidence >= conf_floor` 过滤自然使其不 blocking）。`compute_verdict` 与 `verdict.rs` **不改**。
- **触发范围**：只 refute 孤证 blocking（`agreement == 1 && severity >= block_threshold`）。`agreement >= 2`（层①，多模型已互证）与 `severity < block_threshold` 一律 skip（`verify` 留 `None`）。
- **correlate 仍是桩**：本计划不碰 `src/consolidate/correlate.rs`，code 路径不调 correlate。
- **英文字符串 / 中文注释**：面向用户/模型的字符串英文；代码注释简体中文。
- **退出码 0/10/2 不变**：抽 `exit_code(verdict, failed_models_nonempty) -> i32`，语义 reject→10 / approve+failed→2 / clean approve→0。
- **测试基线**：起步 `cargo test` = 139 passed + 1 ignored。每个 task 结束须全绿 + `cargo clippy --all-targets -- -D warnings` 干净 + `cargo tree -i openssl-sys` 空。

## File Structure

| 文件 | 责任 | 本计划动作 |
|---|---|---|
| `src/domain/finding.rs` | domain 类型 | 加 `Verify`/`VerifyVerdict` + `Finding.verify` 字段（Task 1） |
| `src/orchestrate/mod.rs` | enrich + fan-out | `enrich` 初始 `verify: None`（Task 1） |
| `src/consolidate/verify.rs` | 三层 verify + refute | **新建**（Task 2） |
| `src/consolidate/mod.rs` | consolidate 子模块 | 加 `pub mod verify; pub use verify::verify_findings;`（Task 2） |
| `src/prompt/mod.rs` + `templates/adversarial-refute.md` | refute prompt | `build_refute_prompt` + 新模板（Task 2） |
| `src/cli.rs` | 流水线编排 | `exit_code` helper + `run_code_review` 插入 verify + `run_review` 用 helper（Task 3） |
| `src/output/mod.rs` | 渲染 | `render_md` finding 行加 verify 标记（Task 4） |
| `tests/review_e2e.rs` | e2e | refute 不翻 verdict（Task 4） |

**任务依赖**：T1（domain 字段）→ T2（verify 模块，用 `Verify` 类型）→ T3（cli 接 `verify_findings` + exit_code）→ T4（output + e2e）。

---

### Task 1: domain `Finding.verify` 字段 + `Verify` 类型

**Files:**
- Modify: `src/domain/finding.rs`（加 `Verify`/`VerifyVerdict`，`Finding` 加 `verify` 字段）
- Modify: `src/orchestrate/mod.rs`（`enrich` 加 `verify: None`）
- Modify: 所有 `Finding {}` 构造点加 `verify: None`（全是 test helper）：`src/verdict.rs:41`、`src/consolidate/correlate.rs:46`、`src/consolidate/dedup.rs:54`、`src/positioning/mod.rs:75`、`src/domain/finding.rs:111`、`src/domain/report.rs:68`、`src/output/mod.rs:54`
- Test: `src/domain/finding.rs`

**Interfaces:**
- Produces: `pub enum VerifyVerdict { Upheld, Refuted }` (Serialize, lowercase, PartialEq, Clone, Debug)；`pub struct Verify { pub verdict: VerifyVerdict, pub model: String, pub note: String }` (Serialize, Clone, Debug)；`Finding.verify: Option<Verify>`（`#[serde(skip_serializing_if = "Option::is_none")]`，放在 `cluster_id` 之后）。Task 2 的 `verify_findings` sets it; Task 4 output reads it.

- [ ] **Step 1: 写失败测试**

在 `src/domain/finding.rs` 的 `#[cfg(test)] mod tests` 追加：

```rust
    #[test]
    fn finding_serializes_verify_when_present_and_skips_when_none() {
        let mut f = Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec![], models: vec![], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "snippet".into(), verify: None,
        };
        // None → 不出现
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert!(v.get("verify").is_none(), "verify=None must be skipped");
        // Some(Refuted) → 出现，verdict 小写
        f.verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "gpt".into(), note: "guarded above".into() });
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["verify"]["verdict"], "refuted");
        assert_eq!(v["verify"]["model"], "gpt");
        assert_eq!(v["verify"]["note"], "guarded above");
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p breakpoints finding_serializes_verify`
Expected: 编译失败（`Verify`/`VerifyVerdict` 不存在、`Finding` 无 `verify` 字段）。

- [ ] **Step 3: 写实现**

在 `src/domain/finding.rs`：`Cluster` 定义之前加类型：

```rust
/// targeted-verify（S3-b）的判定：upheld=守住反驳（finding 成立）、refuted=被证伪。
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VerifyVerdict { Upheld, Refuted }

/// 一条 finding 的 verify 结果（refute pass 产出）。
#[derive(Clone, Debug, Serialize)]
pub struct Verify {
    pub verdict: VerifyVerdict,
    /// 执行 refute 的 model 名（交叉验证用哪个）。
    pub model: String,
    /// refute 理由（refuted 时为反驳要点；upheld 时可空）。
    #[serde(skip_serializing_if = "String::is_empty")]
    pub note: String,
}
```

`Finding` 结构在 `cluster_id` 字段之后加：

```rust
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verify: Option<Verify>,
```

`src/orchestrate/mod.rs` 的 `enrich`（返回 `Finding {...}`）在 `cluster_id: None,` 之后加 `verify: None,`。

把上述 7 个 test-helper 的 `Finding {}` 构造点各加 `verify: None,`（与 `cluster_id` 相邻即可）：`verdict.rs:41`、`consolidate/correlate.rs:46`、`consolidate/dedup.rs:54`、`positioning/mod.rs:75`、`domain/finding.rs:111`（既有 `finding_serializes_quote` 测试的 helper）、`domain/report.rs:68`、`output/mod.rs:54`。用 `grep -rn "cluster_id: None" src/` / `grep -rn "cluster_id:None" src/` 定位每一处。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p breakpoints`
Expected: 全绿（新增 1 + 既有全过，所有构造点已补 `verify: None`）。`cargo clippy --all-targets -- -D warnings` 干净。

- [ ] **Step 5: 提交**

```bash
git add src/domain/finding.rs src/orchestrate/mod.rs src/verdict.rs src/consolidate/correlate.rs src/consolidate/dedup.rs src/positioning/mod.rs src/domain/report.rs src/output/mod.rs
git commit -m "feat(s3b): add Finding.verify field + Verify/VerifyVerdict types"
```

---

### Task 2: verify 模块（三层 + 交叉 refute + prompt 模板）

**Files:**
- Create: `src/consolidate/verify.rs`
- Modify: `src/consolidate/mod.rs`（加 `pub mod verify;` + re-export）
- Create: `src/prompt/templates/adversarial-refute.md`
- Modify: `src/prompt/mod.rs`（`build_refute_prompt` + include_str）
- Test: `src/consolidate/verify.rs`、`src/prompt/mod.rs`

**Interfaces:**
- Consumes: `Finding`、`Verify`、`VerifyVerdict`、`Severity`（domain）；`ReviewProvider`、`LensRequest`（provider）；`Semaphore`、`futures::future::join_all`、`Arc`。
- Produces: `pub async fn verify_findings(findings: Vec<Finding>, provider: &dyn ReviewProvider, models: &[String], block_threshold: Severity, max_concurrency: usize) -> Vec<Finding>`；`pub fn build_refute_prompt(finding: &Finding) -> String`（in `prompt/mod.rs`）. Task 3 calls `verify_findings` in `run_code_review`.

- [ ] **Step 1: 写失败测试（prompt + verify）**

在 `src/prompt/mod.rs` 的 `#[cfg(test)] mod tests` 追加：

```rust
    #[test]
    fn refute_prompt_carries_finding_and_upheld_empty_contract() {
        use crate::domain::{Finding, Severity};
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "missing auth".into(),
            body: "no check before db write".into(), file: "src/a.rs".into(),
            line_start: 12, line_end: 14, confidence: 0.9, recommendation: "add check".into(),
            lenses: vec!["L1".into()], models: vec!["claude".into()], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "db.write(token)".into(), verify: None,
        };
        let p = build_refute_prompt(&f);
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("src/a.rs") && p.contains("12"), "file:line injected");
        assert!(p.contains("missing auth") && p.contains("db.write(token)"), "title+quote injected");
        // upheld=空 findings 的契约必须在 prompt 中
        assert!(p.to_lowercase().contains("findings"), "output contract present");
    }
```

在新文件 `src/consolidate/verify.rs` 写测试 mod（实现在 Step 3）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Severity, VerifyVerdict};
    use crate::provider::fake::FakeProvider;
    use crate::domain::RawFinding;
    use std::collections::BTreeMap;

    fn finding(title: &str, sev: Severity, agreement: u32, model: &str) -> Finding {
        Finding {
            id: title.into(), severity: sev, title: title.into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec!["L1".into()], models: vec![model.into()],
            agreement, hard_constraint: false, cluster_id: None, quote: "q".into(), verify: None,
        }
    }
    fn rawf(title: &str) -> RawFinding {
        RawFinding { severity: Severity::High, title: title.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(), lens: None }
    }

    #[tokio::test]
    async fn lone_blocking_refuted_drops_confidence_and_marks() {
        // skeptic 返回非空（找到反驳）→ refuted；confidence 降 0
        let provider = FakeProvider::with("beta", vec![rawf("actually guarded above")]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(out[0].verify.as_ref().unwrap().model, "beta", "交叉选了非 alpha 的 model");
        assert_eq!(out[0].confidence, 0.0, "refuted 降 confidence 下 floor");
    }

    #[tokio::test]
    async fn lone_blocking_upheld_keeps_confidence() {
        // skeptic 返回空（守不住反驳）→ upheld；confidence 不动
        let provider = FakeProvider::with("beta", vec![]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld);
        assert_eq!(out[0].confidence, 0.9);
    }

    #[tokio::test]
    async fn consensus_finding_is_skipped() {
        // agreement>=2（层①）→ 不 refute，verify 留 None
        let provider = FakeProvider::with("beta", vec![rawf("x")]);
        let f = finding("agreed bug", Severity::Critical, 2, "alpha");
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4).await;
        assert!(out[0].verify.is_none());
        assert_eq!(out[0].confidence, 0.9);
    }

    #[tokio::test]
    async fn below_block_threshold_is_skipped() {
        // severity < block_threshold → 不 refute
        let provider = FakeProvider::with("beta", vec![rawf("x")]);
        let f = finding("minor", Severity::Low, 1, "alpha");
        let out = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4).await;
        assert!(out[0].verify.is_none());
    }

    #[tokio::test]
    async fn refute_failure_is_conservatively_upheld() {
        // provider 对所选 model 无 script → 调用失败 → 保守 upheld（保留 finding 的 blocking 力）
        let provider = FakeProvider::with("alpha", vec![]); // 只有 alpha，单 model
        let f = finding("missing auth", Severity::High, 1, "alpha");
        // models 只有 alpha → 退化自审 alpha；但 FakeProvider 有 alpha script(空) → upheld
        let out = verify_findings(vec![f], &provider, &["alpha".into()], Severity::Medium, 4).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld);

        // 真失败：所选 model 无 script
        let provider2 = FakeProvider { scripted: BTreeMap::new() };
        let f2 = finding("missing auth", Severity::High, 1, "ghost");
        let out2 = verify_findings(vec![f2], &provider2, &["ghost".into()], Severity::Medium, 4).await;
        assert_eq!(out2[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld, "调用失败保守 upheld");
        assert_eq!(out2[0].confidence, 0.9, "upheld 不降 confidence");
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p breakpoints verify ; cargo test -p breakpoints refute_prompt`
Expected: 编译失败（`verify_findings`/`build_refute_prompt` 不存在）。

- [ ] **Step 3: 写实现**

新建 `src/prompt/templates/adversarial-refute.md`：

```
<role>
You are a skeptical verifier. A prior adversarial review CLAIMED the problem below. Your job is to REFUTE the claim — find concrete evidence in the code that it is a false positive, does not actually hold, is already guarded, or is overstated.
</role>

<claimed_finding>
File: {{FILE}}:{{LINE_START}}
Title: {{TITLE}}
Severity: {{SEVERITY}}
Body: {{BODY}}
Quoted code: {{QUOTE}}
</claimed_finding>

<tools>
You have read_file / search_repo / list_files under the review root. Read the actual code around the claim — the surrounding function, callers, guards — before judging. Do not invent code you have not read.
</tools>

<method>
Attack the claim: Is the quoted code real and in the context described? Does the alleged failure actually occur, or is it prevented by a guard, an invariant, or a caller? Is the severity justified by real impact? Only a concrete, code-grounded refutation counts.
</method>

<output_contract>
Return ONLY JSON: an object with a "findings" array.
- If you find concrete, code-grounded reasons the claim is WRONG / a false positive / already handled / overstated: return EXACTLY ONE finding whose title is the refutation reason and whose body cites the evidence. A non-empty array marks the claim REFUTED.
- If, after reading the code, the claim HOLDS and you cannot refute it: return {"findings": []}. An empty array marks the claim UPHELD.
When in doubt, do NOT fabricate a refutation — return an empty array.
Write every title and body in {{OUTPUT_LANGUAGE}}.
</output_contract>
```

在 `src/prompt/mod.rs`：顶部加 `const REFUTE_TPL: &str = include_str!("templates/adversarial-refute.md");`，并加函数：

```rust
/// 构造 refute prompt：给定 claimed finding，让 skeptic 对抗复审。
/// 复用 review-as-refute——返回的 findings 空=upheld、非空=refuted（见模板 output_contract）。
pub fn build_refute_prompt(finding: &crate::domain::Finding) -> String {
    REFUTE_TPL
        .replace("{{FILE}}", &finding.file)
        .replace("{{LINE_START}}", &finding.line_start.to_string())
        .replace("{{TITLE}}", &finding.title)
        .replace("{{SEVERITY}}", finding.severity.as_str())
        .replace("{{BODY}}", &finding.body)
        .replace("{{QUOTE}}", &finding.quote)
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}
```

新建 `src/consolidate/verify.rs`（测试 mod 在 Step 1 已写，放其上方）：

```rust
use crate::domain::{Finding, Severity, Verify, VerifyVerdict};
use crate::prompt::build_refute_prompt;
use crate::provider::{LensRequest, ReviewProvider};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// 选交叉 model：第一个不在 finding.models 里的；没有则退化用 models[0]（自审）。
fn pick_cross_model<'a>(models: &'a [String], finding_models: &[String]) -> Option<&'a str> {
    models
        .iter()
        .find(|m| !finding_models.contains(m))
        .or_else(|| models.first())
        .map(|s| s.as_str())
}

/// 对单条孤证做 refute：选交叉 model → build_refute_prompt → provider.run。
/// 返回 Verify：空 findings=Upheld、非空=Refuted；调用失败=保守 Upheld。
async fn refute_one(provider: &dyn ReviewProvider, finding: &Finding, models: &[String]) -> Verify {
    let model = match pick_cross_model(models, &finding.models) {
        Some(m) => m.to_string(),
        None => return Verify { verdict: VerifyVerdict::Upheld, model: String::new(), note: "no model".into() },
    };
    let prompt = build_refute_prompt(finding);
    match provider.run(LensRequest { model: model.clone(), prompt }).await {
        Ok(call) if !call.findings.is_empty() => {
            let note = call.findings.first().map(|f| f.title.clone()).unwrap_or_default();
            Verify { verdict: VerifyVerdict::Refuted, model, note }
        }
        Ok(_) => Verify { verdict: VerifyVerdict::Upheld, model, note: String::new() },
        // 保守：refute 调用失败不冤枉 finding（保留其 blocking 力）。
        Err(_) => Verify { verdict: VerifyVerdict::Upheld, model, note: "refute unavailable".into() },
    }
}

/// 三层 targeted-verify（S3-b）。
/// 层①：agreement>=2 跳过（多模型已互证）。层②：引用存在性由 positioning 已砍编造。
/// 层③：仅对孤证 blocking（agreement==1 && severity>=block_threshold）做定向 refute。
/// refuted → verify=Refuted 且 confidence=0.0（降下 floor，不翻 verdict）；upheld → verify=Upheld。
/// Semaphore 限 max_concurrency。
pub async fn verify_findings(
    mut findings: Vec<Finding>,
    provider: &dyn ReviewProvider,
    models: &[String],
    block_threshold: Severity,
    max_concurrency: usize,
) -> Vec<Finding> {
    let targets: Vec<usize> = findings
        .iter()
        .enumerate()
        .filter(|(_, f)| f.agreement == 1 && f.severity >= block_threshold)
        .map(|(i, _)| i)
        .collect();
    if targets.is_empty() {
        return findings;
    }
    let sem = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let verdicts = futures::future::join_all(targets.iter().map(|&i| {
        let sem = sem.clone();
        let f = &findings[i];
        async move {
            let _permit = sem.acquire().await.expect("semaphore not closed");
            refute_one(provider, f, models).await
        }
    }))
    .await;
    for (&i, v) in targets.iter().zip(verdicts) {
        if v.verdict == VerifyVerdict::Refuted {
            findings[i].confidence = 0.0;
        }
        findings[i].verify = Some(v);
    }
    findings
}
```

在 `src/consolidate/mod.rs` 加（按字母序）：

```rust
pub mod verify;
pub use verify::verify_findings;
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p breakpoints verify ; cargo test -p breakpoints refute_prompt`
Expected: PASS（5 verify + 1 prompt）。`cargo clippy --all-targets -- -D warnings` 干净。

- [ ] **Step 5: 提交**

```bash
git add src/consolidate/verify.rs src/consolidate/mod.rs src/prompt/mod.rs src/prompt/templates/adversarial-refute.md
git commit -m "feat(s3b): three-layer verify with cross-model review-as-refute"
```

---

### Task 3: cli `exit_code` helper + `run_code_review` 插入 verify

**Files:**
- Modify: `src/cli.rs`（加 `exit_code` helper；`run_code_review` 插入 `verify_findings`；`run_review`/`run_code_review` 退出码改用 helper）
- Test: `src/cli.rs`

**Interfaces:**
- Consumes: `verify_findings`（Task 2）；`Verdict`、`Severity`（domain）。
- Produces: `pub fn exit_code(verdict: Verdict, failed_models_nonempty: bool) -> i32`. `run_code_review` 流水线插入 verify。

- [ ] **Step 1: 写失败测试**

在 `src/cli.rs` 的 `#[cfg(test)] mod code_review_tests`（或新增 mod）追加：

```rust
    #[test]
    fn exit_code_maps_all_cases() {
        use crate::domain::Verdict;
        assert_eq!(exit_code(Verdict::Reject, false), 10);
        assert_eq!(exit_code(Verdict::Reject, true), 10);   // reject 优先
        assert_eq!(exit_code(Verdict::Approve, true), 2);   // approve 但对照集不完整
        assert_eq!(exit_code(Verdict::Approve, false), 0);  // 干净 approve
    }

    #[tokio::test]
    async fn lone_blocking_refuted_does_not_reject() {
        use crate::domain::{Severity, Verdict, VerifyVerdict};
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        // 单 model alpha：review 报一条 High 孤证(单 model → agreement=1)；refute 退化自审 alpha，
        // FakeProvider 同脚本对 refute 也返回非空 → refuted → confidence 0 → 不 reject。
        // 交叉 model 选择本身已在 verify.rs 单测验证；本 e2e 只验「证伪翻转 verdict」全链路。
        let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha"]
"#]).unwrap();
        let provider = crate::provider::fake::FakeProvider::with(
            "alpha", vec![raw("src/a.rs", "db.write(token);", Severity::High)],
        );
        let my_args = ReviewArgs {
            mode: Some("code".into()), base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, lenses: Some("L1".into()), models: None, models_all: false,
            no_correlate: true, block_threshold: None, conf_floor: None,
            background: false, format: "json".into(), targets: vec![],
        };
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
        let (report, code) = run_code_review(&my_args, &provider, &cfg, changes, dir.path()).await.unwrap();
        let f = &report.findings[0];
        assert_eq!(f.verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(f.confidence, 0.0);
        assert_eq!(report.verdict, Verdict::Approve, "孤证被自审 refute 证伪 → 不翻 verdict");
        assert!(code == 0 || code == 2, "approve → 0 或 2（不 10）");
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p breakpoints exit_code_maps ; cargo test -p breakpoints lone_blocking_refuted_does_not_reject`
Expected: 编译失败（`exit_code` 不存在）/ 断言失败（verify 未接入）。

- [ ] **Step 3: 写实现**

在 `src/cli.rs` 加 helper（放在 `parse_format` 附近）：

```rust
/// 退出码映射（统一 run_review/run_code_review）：reject→10、approve+对照集不完整→2、干净 approve→0。
pub fn exit_code(verdict: Verdict, failed_models_nonempty: bool) -> i32 {
    match (verdict, failed_models_nonempty) {
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2,
        (Verdict::Approve, false) => 0,
    }
}
```

`run_code_review`：在 `clamp_compliance(&mut findings);` 之后、`compute_verdict` 之前插入 verify（`provider`、`model_names`、`block`、`cfg.review.max_concurrency` 均在作用域内）：

```rust
    // 6. consolidate：dedup → clamp → verify（三层 targeted-verify，孤证定向 refute）
    let mut findings = dedup(anchored);
    clamp_compliance(&mut findings);
    let findings = crate::consolidate::verify_findings(
        findings, provider, &model_names, block, cfg.review.max_concurrency,
    ).await;
```

注意：`block` 当前在 verdict 段才计算；把 `let block = ...` 上移到 verify 之前（verify 与 verdict 用同一 `block_threshold`）。`compute_verdict(&findings, block, floor)` 保持不变。

`run_code_review` 与 `run_review` 末尾的 `let code = match (verdict, !report.run.failed_models.is_empty()) {...}` 两处都替换为：

```rust
    let code = exit_code(verdict, !report.run.failed_models.is_empty());
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p breakpoints`
Expected: 全绿。`cargo clippy --all-targets -- -D warnings` 干净。

- [ ] **Step 5: 提交**

```bash
git add src/cli.rs
git commit -m "feat(s3b): wire verify into run_code_review; extract exit_code helper"
```

---

### Task 4: output verify 展示 + replicate refute e2e

**Files:**
- Modify: `src/output/mod.rs`（`render_md` finding 行加 verify 标记）
- Modify: `tests/review_e2e.rs`（refute 不翻 verdict e2e）
- Test: `src/output/mod.rs`、`tests/review_e2e.rs`

**Interfaces:**
- Consumes: `Finding.verify`、`VerifyVerdict`（Task 1）；`run_code_review`、`ProviderRegistry`、`FakeProvider`。

- [ ] **Step 1: 写失败测试**

`src/output/mod.rs` 的 `#[cfg(test)] mod tests` 追加：

```rust
    #[test]
    fn md_marks_refuted_verify() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut r = report();
        r.findings[0].verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "gpt".into(), note: "guarded".into() });
        let s = render(&r, Format::Md);
        assert!(s.to_lowercase().contains("refuted"), "md must mark refuted findings");
    }
```

`tests/review_e2e.rs` 末尾追加：

```rust
#[tokio::test]
async fn code_replicate_lone_blocking_refuted_flips_to_approve() {
    use breakpoints::cli::{run_code_review, ReviewArgs};
    use breakpoints::config::Config;
    use breakpoints::domain::{Severity, Verdict, VerifyVerdict, RawFinding};
    use breakpoints::evidence::git::FileChange;
    use breakpoints::provider::registry::ProviderRegistry;
    use breakpoints::provider::fake::FakeProvider;

    fn rf(file: &str, quote: &str, title: &str, sev: Severity) -> RawFinding {
        RawFinding { severity: sev, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()) }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();

    // 单 model alpha 自审：review 报一条 High 孤证(agreement=1)；refute 退化自审 alpha，
    // FakeProvider 同脚本对 refute 也返回非空 → refuted → confidence 0 → 不翻 verdict。
    let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha"]
"#]).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("alpha", Box::new(FakeProvider::with("alpha", vec![rf("src/a.rs", "db.write(token);", "missing auth", Severity::High)])));

    let args = ReviewArgs {
        mode: Some("code".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: Some("L1".into()), models: None, models_all: false,
        no_correlate: true, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![],
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path()).await.unwrap();

    let f = report.findings.iter().find(|f| f.file == "src/a.rs").expect("finding present");
    assert_eq!(f.verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted, "孤证被交叉 refute 证伪");
    assert_eq!(f.confidence, 0.0, "refuted 降 confidence 下 floor");
    assert_eq!(report.verdict, Verdict::Approve, "证伪后不翻 verdict");
    assert!(code == 0 || code == 2, "approve → 0/2，非 10");
}
```

> 说明：单 model alpha 自审让 `FakeProvider`（按 model 返回固定脚本）的同一应答同时充当 review（报孤证）与 refute（返回非空→refuted），无需区分 prompt。交叉-model 选择由 `verify.rs` 单测覆盖；此 e2e 聚焦「证伪→不翻 verdict」全链路。`report.findings[0]` 即该条（单 model 单文件单 finding）。

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p breakpoints md_marks_refuted ; cargo test --test review_e2e code_replicate_lone_blocking_refuted`
Expected: 失败（md 无 refuted 标记 / verify 行为未串到 e2e）。

- [ ] **Step 3: 写实现**

`src/output/mod.rs` 的 `render_md`，finding 行渲染处（`for f in group {...}`）在 provenance 后追加 verify 标记。把现有 `s.push_str(&format!("- **{}** ...))` 改为在其后补一行（当 `f.verify` 存在时）：

```rust
        for f in group {
            s.push_str(&format!("- **{}** `{}:{}` _(lens {}, models {}, agree {})_\n  {}\n  → {}\n",
                f.title, f.file, f.line_start,
                f.lenses.join(", "), f.models.join(", "), f.agreement,
                f.body, f.recommendation));
            if let Some(v) = &f.verify {
                let tag = match v.verdict {
                    crate::domain::VerifyVerdict::Refuted => "refuted",
                    crate::domain::VerifyVerdict::Upheld => "upheld",
                };
                s.push_str(&format!("  _[verify: {} by {}]_\n", tag, v.model));
            }
        }
```

e2e（Step 1）已是单 model 自审编排，无需额外脚本调整。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test`
Expected: 全绿（output + e2e）。`cargo clippy --all-targets -- -D warnings` 干净。`cargo tree -i openssl-sys` 空。

- [ ] **Step 5: 提交**

```bash
git add src/output/mod.rs tests/review_e2e.rs
git commit -m "feat(s3b): render verify verdict in md; refute-flips-verdict e2e"
```

---

## Self-Review

**1. Spec 覆盖**（对照路线图 §S3 + scoping 决策）：
- 三层 verify：层① agreement≥2 skip（T2 `consensus_finding_is_skipped`）✅；层② 引用存在性（positioning 既有，本计划不动）✅；层③ 孤证 refute（T2/T3/T4）✅
- `Finding.verify` 字段 → T1 ✅
- review-as-refute 复用 registry/ToolLoopProvider、零引擎改动 → T2（`ReviewProvider::run`，空/非空编码）✅
- 交叉 model（≠ finding.models，退化自审）→ T2 `pick_cross_model` + `lone_blocking_refuted...model=="beta"` ✅
- 证伪处理：verify=refuted + confidence=0.0 不翻 verdict → T2/T3/T4 ✅
- exit_code helper（opus 终审建议）→ T3 ✅
- correlate 仍桩、不纳入 → 本计划不碰 correlate.rs ✅
- doc 无回归：run_review 不加 verify，只用 exit_code helper → T3 ✅

**2. Placeholder 扫描**：无 TBD/TODO；每个改码 step 有完整代码；测试均可运行断言。T4 e2e 的脚本编排带「实现者按实跑、必要时调脚本并记录」的明确指引（非占位，是对 FakeProvider 同 model 同应答特性的真实适配）。✅

**3. 类型一致性**：
- `Verify { verdict: VerifyVerdict, model: String, note: String }` / `VerifyVerdict { Upheld, Refuted }`：T1 定义，T2/T3/T4 使用，字段名一致。✅
- `verify_findings(Vec<Finding>, &dyn ReviewProvider, &[String], Severity, usize) -> Vec<Finding>`：T2 定义，T3 调用签名一致（`provider, &model_names, block, cfg.review.max_concurrency`）。✅
- `build_refute_prompt(&Finding) -> String`：T2 定义并自测。✅
- `exit_code(Verdict, bool) -> i32`：T3 定义，run_review/run_code_review 两处用。✅
- `Finding.verify` 在所有构造点（T1 列的 7 test + enrich）补 `None`——T1 Step 3 用 grep 定位，避免遗漏导致后续 task 编译断。✅

**4. 跨 task 编译依赖**：T1 加 `Finding.verify` 字段会让所有 `Finding {}` 构造点编译断 → T1 自身全补 `verify: None`（含生产 `enrich`）。后续 task 不再有字段引发的断裂。T2/T3/T4 均为新增或局部修改，无跨文件签名断裂。
