# M15-S2-b'（三错配修正·第一批）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 pack 的块边界与模型消费单位对齐——0 壳升级为「改动文件全文」（预算内、超回落 def 体）+ refute pass 注入 finding 现场源码段 + 预算帽 64K→256K。

**Architecture:** 两个独立小件：① `src/pack/mod.rs::body_section` 取块从 enclosing-def 换整文件（regex/scip 两引擎共用此函数、同吃收益），预算帽随之 256K；② refute 链穿 root（`cli → verify_findings → refute_one`），`build_refute_prompt` 加 `source_context` 参数 + 两模板加 `{{SOURCE_CONTEXT}}` 段。零新依赖、零协议变更。

**Tech Stack:** Rust 2021 · 纯 std（文件读/字符串）· 复用现有 `render` 模板机 + `lang_for`/`compile_defs`/`changed_def_ranges` 回落路径。

## Global Constraints

- 全程简体中文：代码注释、commit message；模板内英文段沿用模板既有语言。
- **权威 spec** = `.doc/design/2026-07-09-redteam-context-management-s2b-prime-design.md`。只做①②③；类型定义块注入、变更集全景**不做**（ledger 缓）。
- **两引擎同吃**：`body_section` 是 `build_pack` 与 `build_pack_scip` 共用件，改它即两臂生效；不单独改任一引擎。
- **refute 协议不变**：只改 prompt 构造与模板，`classify_refute` 解析/REFUTED 前缀契约零触碰。
- 回落语义：整文件超预算 → 支持语言回落现状 def 体路径；非支持语言 → omit 坐标。**不砍语义只降粒度**。
- CI 门：`cargo test -p redteam` 全绿 + `cargo clippy --all-targets` 净新增警告 0 + `make verify` 绿。
- 每 Task 独立 commit，message 简体中文，结尾 `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`。**不 push**。

---

### Task 1: 0 壳整文件 + 预算帽 256K

**Files:**
- Modify: `src/pack/mod.rs`（`body_section` 重写取块逻辑；tests 更新）
- Modify: `src/cli.rs`（`PACK_BUDGET_BYTES` 64_000→256_000 + 注释；既有测试断言更新）

**Interfaces:**
- Consumes: 现有 `lang_for`/`compile_defs`/`changed_def_ranges`（回落路径原样复用）。
- Produces: `body_section` 签名不变（`(bundle, root, budget, omit) -> String`）；段标题 `## 改动源文（0 壳）`；每文件块头 `path:1-N（全文，N 行）` 或回落时 `path:start-end（def 体，全文超预算回落）`。**行为变更**：非支持语言文件（.md 等）现在也给全文（原为跳过）。

- [ ] **Step 1: 写失败测试**（`src/pack/mod.rs` tests；同时按下方清单改既有断言）

```rust
#[test]
fn body_section_gives_full_file_within_budget() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/a.rs"),
        "use std::fmt;\n\nfn helper() {}\n\nfn caller() {\n    helper();\n    let z = 2;\n}\n").unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "src/a.rs".into(),
        diff: "@@ -6,0 +6,1 @@\n+    let z = 2;\n".into(),
    }] };
    let mut omit = Vec::new();
    let s = body_section(&b, root, 1_000_000, &mut omit);
    assert!(s.contains("## 改动源文（0 壳）"), "新段标题: {s}");
    assert!(s.contains("src/a.rs:1-8（全文，8 行）"), "全文块头带行数: {s}");
    // 粒度对齐（E7）：全文包含改动 def 之外的内容——imports 与相邻函数
    assert!(s.contains("use std::fmt;") && s.contains("fn helper() {}"), "含 imports+相邻函数");
    assert!(omit.is_empty(), "预算充足无省略");
}

#[test]
fn body_section_falls_back_to_def_body_when_file_exceeds_budget() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // 大文件：filler 函数撑体积 + 一个小的改动函数
    let filler = format!("fn filler() {{\n{}}}\n", "    let x = 0;\n".repeat(200));
    std::fs::write(root.join("src/a.rs"),
        format!("{filler}\nfn caller() {{\n    work();\n}}\n")).unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "src/a.rs".into(),
        diff: "@@ -204,0 +204,1 @@\n+    work();\n".into(),
    }] };
    // 预算 500B：全文（~3.4KB）放不下 → 回落 caller def 体（~30B）放得下
    let mut omit = Vec::new();
    let s = body_section(&b, root, 500, &mut omit);
    assert!(s.contains("def 体，全文超预算回落"), "回落标注: {s}");
    assert!(s.contains("fn caller() {") && s.contains("work();"), "回落仍给改动 def 体");
    assert!(!s.contains("fn filler"), "回落不含无关大函数");
}

#[test]
fn body_section_gives_full_file_for_unsupported_lang() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("notes.md"), "# 标题\n\n改动的文档内容。\n").unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "notes.md".into(), diff: "@@ -3,0 +3,1 @@\n+改动的文档内容。\n".into(),
    }] };
    let mut omit = Vec::new();
    let s = body_section(&b, root, 1_000_000, &mut omit);
    assert!(s.contains("改动的文档内容。"), "非支持语言也给全文（E7：templates 格整读实证）: {s}");
    // 超预算时非支持语言无 def 可回落 → omit 坐标
    let mut omit2 = Vec::new();
    let s2 = body_section(&b, root, 10, &mut omit2);
    assert!(s2.is_empty() && omit2.len() == 1 && omit2[0].contains("notes.md"), "无回落进 omit: {omit2:?}");
}
```

**既有测试断言同步清单**（行为变更的涟漪，逐条改）：
- `pack/mod.rs::body_section_renders_full_enclosing_body` → 语义翻转：整文件后 `fn helper` **在**输出里。改断言为全文含 helper+caller，测试名改 `body_section_renders_full_file`。
- `pack/mod.rs::body_section_budget_cut_body_goes_to_omit` → 预算 5：全文与回落都放不下 → omit；断言文案跟随新 omit 措辞（含「全文超预算」或坐标即可，实跑后按实际文案钉）。
- `pack/mod.rs::build_pack_end_to_end_has_bodies_refs_and_diff_reserved` + `build_pack_tiny_budget_spills_to_omission_list_with_coords` → 标题断言 `## 改动符号完整体（0 壳）` → `## 改动源文（0 壳）`。
- `pack/mod.rs::build_pack_empty_for_unsupported_and_no_symbols` → **语义翻转**：非支持语言（app.py）现在给全文 → pack 非空。改为断言含 `def f(): pass` 全文、测试名改 `build_pack_full_file_for_unsupported_lang`。
- `cli.rs::build_packs_default_has_bodies_off_arm_dossier_only_null_arm_empty` → `改动符号完整体` 断言 → `改动源文`。
- scip 臂测试若含 0 壳标题断言同步（grep `改动符号完整体` 全仓清零）。

- [ ] **Step 2: 跑确认失败**

Run: `cargo test -p redteam body_section 2>&1 | tail -8`
Expected: 3 个新测试 FAIL（旧逻辑无全文/无新标题）。

- [ ] **Step 3: 实现**

`src/pack/mod.rs` `body_section` 重写为：
```rust
/// 0 壳段：改动文件全文（粒度对齐模型文件级消费，E7 残余读解剖）；单文件全文超剩余预算
/// → 回落现状 def 体（支持语言）或 omit 坐标（非支持语言）。按 budget 字节封顶。
pub(crate) fn body_section(bundle: &Bundle, root: &Path, budget: usize, omit: &mut Vec<String>) -> String {
    let mut out = String::new();
    for fc in &bundle.files {
        // 工作树直读（bundle 路径来自 git diff、非模型可控，沿用 bundle::group 安全不变量）
        let content = std::fs::read_to_string(root.join(&fc.path)).unwrap_or_default();
        if content.is_empty() {
            continue;
        }
        let n_lines = content.lines().count();
        let block = format!("{}:1-{}（全文，{} 行）\n{}\n\n", fc.path, n_lines, n_lines, content);
        if out.len() + block.len() <= budget.saturating_sub(out.len()).saturating_add(out.len()) && out.len() + block.len() <= budget {
            out.push_str(&block);
            continue;
        }
        // 全文超预算：支持语言回落 def 体（不砍语义只降粒度）；非支持语言无 def 可回落 → omit
        let lang = match lang_for(&fc.path) {
            Some(l) => l,
            None => {
                omit.push(format!("{}:1-{} — 全文超预算（非支持语言无 def 回落）", fc.path, n_lines));
                continue;
            }
        };
        let defs = compile_defs(lang);
        let lines: Vec<&str> = content.lines().collect();
        for (start, end) in changed_def_ranges(&fc.diff, &lines, &defs) {
            let end = end.min(lines.len().saturating_sub(1));
            let body = lines.get(start..=end).unwrap_or(&[]).join("\n");
            if body.is_empty() {
                continue;
            }
            let block = format!("{}:{}-{}（def 体，全文超预算回落）\n{}\n\n", fc.path, start + 1, end + 1, body);
            if out.len() + block.len() > budget {
                omit.push(format!("{}:{}-{} — 改动符号体", fc.path, start + 1, end + 1));
            } else {
                out.push_str(&block);
            }
        }
    }
    if out.is_empty() {
        return String::new();
    }
    format!("## 改动源文（0 壳）\n{out}")
}
```
（注：上面预算判断保留一处笨写以示意；实现时就写 `if out.len() + block.len() <= budget`。）

`src/cli.rs` 预算帽：
```rust
/// pack flat 预算（字节）——S2-b' 提到 256K：0 壳升级整文件后 cli.rs 级大文件（~90KB）
/// 需装入（E7）；256K ≈ 1M-context 模型的 ~6%、256K 模型的 ~25%，安全。per-model 真预算仍缓（T27）。
const PACK_BUDGET_BYTES: usize = 256_000;
```

- [ ] **Step 4: 跑确认通过 + 全仓涟漪清零**

Run: `cargo test -p redteam 2>&1 | tail -5 && grep -rn "改动符号完整体" src/ | wc -l`
Expected: 全绿；grep 计数 0（标题全量替换完成）。

- [ ] **Step 5: clippy + commit**

Run: `cargo clippy --all-targets 2>&1 | grep -c "warning:"`（无净新增）
```bash
git add src/pack/mod.rs src/cli.rs
git commit -m "feat(pack): 0 壳升级改动文件全文 + 预算帽 256K（S2-b' ①③，E7 粒度错配）

body_section 取块从 enclosing-def 换整文件（预算内；超回落 def 体、非支持
语言 omit）；非支持语言文件也给全文（templates 格整读实证）。两引擎共用
同吃。顺带吞 T42（锚定失败不再致空 pack）、超集承接 T29。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: refute pass 带现场源码段

**Files:**
- Modify: `src/consolidate/verify.rs`（新 `source_snippet` + `refute_one`/`verify_findings` 穿 root；tests 补参）
- Modify: `src/prompt/mod.rs`（`build_refute_prompt` 加 `source_context` 参数；tests 补参）
- Modify: `src/prompt/templates/adversarial-refute.md` + `adversarial-refute-doc.md`（加 `{{SOURCE_CONTEXT}}` 段）
- Modify: `src/cli.rs`（两处 `verify_findings(` 调用点传 root）

**Interfaces:**
- Consumes: `Finding { file, line_start, line_end, .. }`；cli 层已有 root。
- Produces:
  - `verify.rs`: `fn source_snippet(root: &Path, f: &Finding) -> String`——读 `root.join(&f.file)`，行窗 `[line_start-60, line_end+60]`（1-based 夹紧），输出带 `path:起-止（现场快照）` 头；超 24_000 字节截到 char 边界+注记；文件读不到/为空 → `"(source unavailable — read the file yourself)"`。
  - `verify_findings(..., root: &Path)`（参数追加在 `precedents` 前后皆可，取 `mode` 之后为宜）；`refute_one` 同。
  - `build_refute_prompt(finding, mode, dismissed_notes, source_context: &str)`——`source_context` 经 `render` 以 `{{SOURCE_CONTEXT}}` 注入（值内容不回扫，注入安全由 render 单 pass 保证）；空串渲染为 `(none)`。
  - 模板两份各加段（放 `{{QUOTE}}` 段之后、output contract 之前），英文沿模板既有语言：
    ```
    ## Source at the claimed location (snapshot from the working tree)

    {{SOURCE_CONTEXT}}

    Use it as your starting evidence; verify beyond it with tools if needed.
    ```

- [ ] **Step 1: 写失败测试**

`src/consolidate/verify.rs` tests：
```rust
#[test]
fn source_snippet_windows_and_clamps() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    let body: String = (1..=200).map(|i| format!("line{i}\n")).collect();
    std::fs::write(root.join("src/a.rs"), &body).unwrap();
    let f = mk_finding("t", "src/a.rs", 100, 102); // 复用/仿既有测试的 Finding 构造助手
    let s = source_snippet(root, &f);
    assert!(s.contains("line40") && s.contains("line162"), "±60 行窗: {s}");
    assert!(!s.contains("line39\n") && !s.contains("line163"), "窗外不含");
    assert!(s.contains("src/a.rs:40-162"), "快照头带范围");
    // 读不到 → 占位
    let f2 = mk_finding("t", "no/such.rs", 1, 1);
    assert_eq!(source_snippet(root, &f2), "(source unavailable — read the file yourself)");
}
```
`src/prompt/mod.rs` tests（既有 refute 测试补第 4 参 `""` 后追加）：
```rust
#[test]
fn refute_prompt_injects_source_context() {
    let f = mk_finding(); // 仿既有测试构造
    let p = build_refute_prompt(&f, crate::domain::Mode::Code, "", "src/a.rs:40-162（现场快照）\nfn x() {}");
    assert!(p.contains("Source at the claimed location"), "段标题在场");
    assert!(p.contains("fn x() {}"), "快照内容注入");
    let p2 = build_refute_prompt(&f, crate::domain::Mode::Code, "", "");
    assert!(p2.contains("(none)"), "空快照渲染占位");
}
```

- [ ] **Step 2: 跑确认失败**

Run: `cargo test -p redteam source_snippet refute_prompt_injects 2>&1 | tail -6`
Expected: 编译失败（`source_snippet` 未定义、`build_refute_prompt` 参数不符）。

- [ ] **Step 3: 实现**

按 Interfaces 实现四处；`refute_one` 内 `let ctx = source_snippet(root, finding);` → `build_refute_prompt(finding, mode, &notes, &ctx)`。`cli.rs` 两处 `verify_findings(` 调用点补传 root（调用点上下文里 root 已在作用域；若在共用尾段结构里则通过该结构穿一个 `root: PathBuf` 字段——implementer 按现场最小改动选择，报告里说明选了哪种）。verify.rs 既有 ~8 个测试调用点补 root 参（用各测试已有的 tempdir 或 `Path::new(".")`——refute 走 FakeProvider 不真读文件时传 `Path::new(".")` 即可，snippet 读不到渲染占位、断言不受影响）。

- [ ] **Step 4: 跑确认通过 + 契约同源测试自检**

Run: `cargo test -p redteam 2>&1 | tail -5`
Expected: 全绿——特别确认既有「refute 模板↔解析 schema 契约同源测试」（T16）仍绿（REFUTED 前缀/空数组契约未被模板新段破坏）。

- [ ] **Step 5: clippy + verify + commit**

Run: `cargo clippy --all-targets 2>&1 | grep -c "warning:" && make verify 2>&1 | tail -2`
```bash
git add src/consolidate/verify.rs src/prompt/mod.rs src/prompt/templates/adversarial-refute.md src/prompt/templates/adversarial-refute-doc.md src/cli.rs
git commit -m "feat(refute): 注入 finding 现场源码段（S2-b' ②，E7 覆盖错配）

refute prompt 附 finding.file ±60 行快照（24KB 帽、读不到渲染占位），
杀核实趟零 pack 裸奔重读。只改 prompt 构造与模板；REFUTED 解析契约零触碰。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## 验收（解剖法，controller 亲跑；design §3）

带 `REDTEAM_TOOL_LOG` + `max_concurrency=1` 重跑 6f95c25（DeepSeek、`--no-dismissed`）一次，比对本轮解剖基线（research 解剖节）：
- **门**：「整读本 bundle 改动文件」类调用消失/大减；refute pass（无 marker tool-loop）重读次数明显降（基线 14 调用/趟）。
- 记录：input 变化、「search struct」与「跨 bundle 读」残余占比 → 喂 ledger 缓项（类型定义块/变更集全景）决策。
- 不设 tool_use 总数 A/B 门。

## Self-Review

- Spec 覆盖：design §2 ①=Task 1（整文件+回落+非支持语言全文+吞 T42）、③=Task 1（256K）、②=Task 2（snippet+穿参+模板）；§3 验收=验收节。缓项（类型块/全景）无 Task（正确）。
- 占位符扫描：无 TBD；代码/命令/期望齐。
- 类型一致：`body_section` 签名不变；`source_snippet(root,&Finding)->String`、`build_refute_prompt(4 参)`、`verify_findings(+root)` 跨 Task 一致；`{{SOURCE_CONTEXT}}` 与 render prefix-free（现有键无 `{{SOURCE` 前缀）。
