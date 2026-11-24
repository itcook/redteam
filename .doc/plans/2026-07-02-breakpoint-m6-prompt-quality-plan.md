# M6 batch 1 · 提示词 & lens 质量 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans（内联执行，grilling 定案）. Steps use checkbox (`- [ ]`) syntax.

**Goal:** 消除 doc review 的 context 膨胀（渐进式 targeted 检索取代 M11 的"先全读"指令）、给 code review 加 bypass_class 类级指令、清掉 Phase-1 单 lens fan-out 死代码。

**Architecture:** 纯 prompt/清理批次。Task 1 删死码（run_fanout/build_prompt/单 lens 模板，生产零调用），Task 2 改两个生产模板 + 字符串断言 + 两组 dogfood。Spec：`.doc/design/2026-07-02-breakpoint-m6-prompt-quality-design.md`。

**Tech Stack:** prompt 模板（include_str!）、Rust 断言测试、brk live dogfood。

## Global Constraints

- 分支 `feat/m6-prompt-quality` 自 dev；trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`。
- lens 保持 `src/lens/mod.rs` static const，**不动**（决议：不 toml/不配置化）。
- 渐进检索是**层 1+2**（prompt/工具描述，非冻结区）；层 3 累积硬预算（tool_loop/tools 冻结区）**不在本计划**。
- bypass_class **只注入 `adversarial-code-bundle.md`**；渐进检索改 `adversarial-doc-bundle.md`；均生产模板。
- 语义以 spec §1/§2 基线为准（措辞可 dogfood 迭代，核实义务不减、normalizer 修复必须、承接穷尽性不重述）。
- dogfood 前必 `make build`（模板 include_str! 编译期内嵌）。

---

### Task 1: 清 Phase-1 单 lens fan-out 死代码（纯删，零行为变更）

**Files:**
- Modify: `src/orchestrate/mod.rs`（删 `run_fanout` 135-294 + 4 测试 + 悬空注释 + `build_prompt` import/调用）
- Modify: `src/prompt/mod.rs`（删 `build_prompt` + `CODE_TPL`/`DOC_TPL` + 其测试 + 改 exhaustiveness 断言）
- Delete: `src/prompt/templates/adversarial-code.md`、`src/prompt/templates/adversarial-doc.md`

**Interfaces:** 无对外接口变更（删的全是生产零调用项）。

- [ ] **Step 1: 建分支 + 前置守卫 grep**

```bash
git checkout dev && git checkout -b feat/m6-prompt-quality
grep -rn 'run_fanout\|build_prompt\|CODE_TPL\|DOC_TPL' src/cli.rs src/main.rs
```

Expected: 无输出（确认 cli/main 生产零引用）。若有输出 → 停，报告（前提被推翻）。

- [ ] **Step 2: 删 orchestrate 死码**
  - 删 `run_fanout` 函数（`src/orchestrate/mod.rs` 行 135 `pub async fn run_fanout` 至其闭合 `}` 即 294）。
  - 删依赖它的 4 个测试函数（整函数删）：`default_path_falls_back_to_next_priority`（~675）、
    `explicit_models_record_failure_no_fallback`（~693）、`default_path_full_chain_failure_no_failed_models`
    （~928）、`fallback_cell_records_hit_model_even_with_zero_findings`（~986）。
  - `use` 行（5）：`use crate::prompt::{build_bundle_prompt, build_doc_prompt, build_prompt};` 删去 `, build_prompt`。
  - `top_priority_model`（~296）：`grep -n 'top_priority_model' src/orchestrate/mod.rs`，若仅 run_fanout 用则一并删；若活函数也用则保留。
  - 悬空注释：`grep -n '见 run_fanout' src/orchestrate/mod.rs` 命中的活函数注释（run_code_fanout/run_doc_fanout 内，约 365/395/454/535/564），把"（见 run_fanout 注释）/（理由同 run_fanout…）"改成就地简述或删该引用短语——不留指向已删函数的悬空引用。

- [ ] **Step 3: 删 prompt 死码**
  - `src/prompt/mod.rs`：删 `CODE_TPL`（5）、`DOC_TPL`（6）两 const；删 `build_prompt` 函数（40 起至闭合）；
    删其测试（含 `build_prompt(` 的 `#[test]`：约 159/165 所在函数、`build_prompt_contains_language_directive_and_no_residual_placeholders` 202 整函数）。
  - **保留** `REFUTE_DOC_TPL`（9）与 refute 逻辑（`Mode::Doc => REFUTE_DOC_TPL` 那处是 build_refute_prompt，活代码，勿删）。
  - exhaustiveness 断言：`all_adversarial_templates_carry_exhaustiveness_directive` 数组删去
    `include_str!("templates/adversarial-doc.md")` 与 `include_str!("templates/adversarial-code.md")` 两行，
    只留 `adversarial-code-bundle.md`、`adversarial-doc-bundle.md`。

- [ ] **Step 4: 删死模板文件**

```bash
git rm src/prompt/templates/adversarial-code.md src/prompt/templates/adversarial-doc.md
```

- [ ] **Step 5: 编译驱动清残留 + 全量回归**

Run: `cargo build 2>&1 | tail -5`（跟编译错误/未用告警清干净）
Run: `cargo clippy --lib 2>&1 | grep -cE 'warning|error'` → Expected: `0`
Run: `cargo test 2>&1 | grep 'test result' | awk '{s+=$4} END{print s}'` → Expected: 全绿（数 = 268 减去删掉的死码测试数）
Run: `grep -rn 'run_fanout\|build_prompt\|CODE_TPL\|DOC_TPL' src` → Expected: 无输出

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "chore(prompt): remove dead Phase-1 single-lens fan-out (run_fanout/build_prompt/single-lens templates)

Generation-2 production uses run_code_fanout (bundle) / run_doc_fanout (single
cell, all lenses); the per-lens fan-out path had zero production callers, only
tests. Removes run_fanout, build_prompt, CODE_TPL/DOC_TPL, adversarial-{code,doc}.md
and their tests; exhaustiveness assertion narrowed to the two production templates.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: doc 渐进检索 + code bypass_class（prompt，TDD 断言先行 + dogfood）

**Files:**
- Modify: `src/prompt/templates/adversarial-doc-bundle.md`（替换 first-step 段 + temporal_stance ACTIVE duties 对齐 targeted）
- Modify: `src/prompt/templates/adversarial-refute-doc.md`（tools 段补渐进一句，可选低风险）
- Modify: `src/prompt/templates/adversarial-code-bundle.md`（`<finding_bar>` 后注入 `<bypass_class>`）
- Test: `src/prompt/mod.rs`（字符串断言）

**Interfaces:** 无代码接口；模板新增 `Retrieve progressively`（doc-bundle）与 `<bypass_class>`（code-bundle）。

- [ ] **Step 1: 写失败断言测试**（`src/prompt/mod.rs` tests，紧邻 exhaustiveness 断言）

```rust
    #[test]
    fn doc_review_uses_progressive_retrieval_not_preload() {
        let t = include_str!("templates/adversarial-doc-bundle.md");
        assert!(t.contains("Retrieve progressively"), "doc-bundle 缺渐进检索指令");
        assert!(!t.contains("read_file every code/spec file"), "膨胀元凶 first-step 全读指令未替换");
    }

    #[test]
    fn code_review_carries_bypass_class() {
        assert!(
            include_str!("templates/adversarial-code-bundle.md").contains("<bypass_class>"),
            "code-bundle 缺 bypass_class"
        );
    }
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --lib prompt::tests::doc_review_uses_progressive_retrieval_not_preload prompt::tests::code_review_carries_bypass_class`
Expected: FAIL（断言不成立）

- [ ] **Step 3: doc-bundle 渐进替换**——`adversarial-doc-bundle.md` 的 `<tools>` 里，**删除** M11 那段
  "Mandatory first step — BEFORE writing any finding: read_file every code/spec file… Only then write your findings."，替换为：

```
Retrieve progressively, not up front. Do NOT pre-load every referenced file. Read the
document first; then, while writing each finding that needs code evidence, retrieve only
what THAT finding requires: search_repo to locate the symbol, then read_file with a line
range to read just that span — not whole files. Stop retrieving once you can judge the
claim. Retrieval is minimized, but verification is NOT: you must still check every claim
of existing implementation and every stated dependency — precisely, not exhaustively.
```

  `<temporal_stance>` 末尾 "These last three are ACTIVE duties… retrieve the referenced code (read_file /
  search_repo)…" 中的 retrieve 明确为 targeted：把 "retrieve the referenced code (read_file / search_repo)"
  改为 "retrieve just the referenced span (search_repo to locate, then read_file with a line range)"。

- [ ] **Step 4: refute-doc 渐进补一句（可选）**——`adversarial-refute-doc.md` 的 `<tools>` 段末尾追加：
  `Read only the span needed to judge this one claim — a line range, not whole files.`

- [ ] **Step 5: code-bundle 注入 bypass_class**——`adversarial-code-bundle.md` 的 `</finding_bar>` 之后插入：

```
<bypass_class>
The exhaustiveness rule above requires reporting variants you observe; for input-handling
bypass findings (scheme/host/path allow- or deny-lists, encoding or sanitization, parsers,
validators), go further:
- Characterize the CLASS, not one instance: name the input-space dimension being bypassed
  (case, encoding depth, alternate scheme, protocol-relative, Unicode normalization, path
  traversal…) rather than the single input the code happens to miss.
- In the same finding, enumerate the strongest representative variants (e.g. mixed case,
  double/nested encoding, data:/javascript:/file: schemes, //host, ../ traversal) and note
  "and equivalents" — do not stop at the one case present in the diff.
- The recommended fix MUST be a normalizer / single canonicalization point, not another
  allow/deny-list entry. State that per-case patches leave the class open and are an
  incomplete fix.
</bypass_class>
```

- [ ] **Step 6: 断言过 + 全量回归**

Run: `cargo test` → Expected: 全绿（新 2 断言过）

- [ ] **Step 7: Commit（模板 + 测试）**

```bash
git add src/prompt/templates/adversarial-doc-bundle.md src/prompt/templates/adversarial-refute-doc.md src/prompt/templates/adversarial-code-bundle.md src/prompt/mod.rs
git commit -m "feat(prompt): progressive retrieval for doc review + bypass_class for code review

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

- [ ] **Step 8: 重编**

Run: `make build 2>&1 | tail -1`

- [ ] **Step 9: dogfood 靶 1 — 渐进检索 A/B（量化）**

```bash
export BRK_STATE_DIR=<scratchpad>/m6-doc-state
plugins/breakpoint/bin/brk review --mode doc .doc/design/2026-07-01-breakpoint-loop-lgtm-design.md --models Kimi --background
# 轮询 status 至 done；取 result 读 run.usage.input_tokens 与 findings
```

判据：`input_tokens` 从 M11 实测 ~785K **显著下降**（观察参考 <200K，非硬阈值）；**且**核实类真 drift（针对
as-built 代码的 finding，如 spec 陈旧行号 / lgtm.md 状态行格式不一致）**仍在**。token 大降 + 核实不丢 = 生效。

- [ ] **Step 10: dogfood 靶 2 — bypass_class 升级 + 不误伤（code 模式，需真 git 改动）**

在 scratchpad git 仓造两个改动：靶 A = URL scheme 校验用 case-sensitive 前缀 denylist（只挡 `javascript:` 小写）；
靶 B = 正常输入处理、无绕过漏洞。分别 `brk review --mode code`（对改动 bundle）：
- 靶 A 判据：finding 升级为**类级**（"scheme 校验前缀匹配、整类可绕"而非"缺挡一个 case"）+ body 枚举多个
  代表变体 + recommendation 指向 normalizer；对照 dev 二进制同靶大概率只报单 case（A/B）。
- 靶 B 判据：`<bypass_class>` 不硬套变体清单误报。

- [ ] **Step 11: 迭代与收尾**——逐靶记录 PASS/FAIL；措辞需迭代则改模板 → `cargo test` → `make build` → 重跑
  失败靶（每轮一个变量，同 M11 手法）。全过后二进制入 commit：

```bash
git add plugins/breakpoint/bin/darwin/aarch64/brk
git commit -m "build(m6): rebuild binary with progressive-retrieval + bypass_class templates

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

清理 scratchpad 工作区与 state。

---

## Self-Review 记录

- **Spec 覆盖**：§1 渐进检索→T2 S3/S4；§2 bypass_class→T2 S5；§3 死码→T1；§4 lens 常量化=不动代码（无任务，决议已锁）；§5 执行两批次→T1/T2；验收 §1/§2→T2 S9/S10。无缺口。
- **占位符**：`<scratchpad>` 为会话 scratchpad 路径指代（执行时代入）；余无 TBD。
- **一致性**：断言关键词 "Retrieve progressively" / `<bypass_class>` 在测试与模板文本间一致；死码删除清单行号与前置 grep 结果对齐；REFUTE_DOC_TPL 明确保留（防误删活代码）。
- 风险点（执行留意）：T1 悬空注释（"见 run_fanout"）与 top_priority_model 死活需 grep 逐一处理，编译不报、靠人清；T2 doc-bundle 的 first-step 段删除要精确匹配整段，勿误伤 temporal_stance。
