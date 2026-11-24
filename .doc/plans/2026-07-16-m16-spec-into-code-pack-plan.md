# --spec 接入 code pack Implementation Plan（M16 feature，优先于安全组）

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development. Steps 用 checkbox 跟踪。

> 状态: WIP · 待用户确认「设计决策」段后执行 · 缘起=清理批合入后发现 --spec 在 code 模式始终 inert（research 2026-07-09 §中大型验收 归因更正 2026-07-16）· 用户 2026-07-16 定：不删 --spec、接进 code pack，优先于安全组

**Goal:** 让 `redteam review --spec <设计/规格文档>` 在 code 模式真正生效——把 spec 文档内容作为**有界参照段**注入评审上下文，供模型按 `[L0] Spec Consistency`（模板早有此规则）检查「实现是否偏离设计意图」。评审对象仍是**代码**、spec 仅作**参照**——有界、与无工具范式兼容，区别于 E10 砍掉的「文档作为评审对象需无界代码地基」。

**Architecture:** 补齐一个早已存在的空槽。code bundle 模板已有 `[L0] Spec Consistency: whether the implementation drifts from the design intent of the provided spec docs`，但「provided spec docs」从未被填（`args.spec` 在 code 路径未消费——`{{SPEC_DOCS}}` 旧只在已删的 doc 模板）。本 feature：①读 `args.spec` 文档（有界）→ 造 `<spec_docs>` 参照段；②穿进第一趟（`build_bundle_prompt`+模板新增 `{{SPEC_DOCS}}` 槽）、第二趟（`second_pass_prompt`）、refute（verify.rs，承 hiapi F2「对照设计驳回误报」的真实价值）；③预算：spec 有独立帽、且从 pack 预算扣减（总上下文守界）；④`--help`/plugin 壳文案兑现承诺。

**Tech Stack:** Rust 单 crate；无新依赖；纯新增（feature）。

---

## 设计决策（★待用户确认后执行）

- **D1 注入形态**：独立 `<spec_docs>` prompt 段（紧邻 `<evidence>`），非塞进 pack/dossier。理由：spec 是**变更集级参照**非 per-file 代码；模板 `<evidence>`/L0 已引用「provided spec docs」，给它一个专属块最自然。
- **D2 预算**：`SPEC_BUDGET_BYTES = 64_000` 帽（超则截断+标记）；**且**把实际用掉的 spec 字节**从 pack 预算扣减**（`build_packs` 的 budget -= spec_used），总上下文守界、由降级梯子吸收。理由：与 own_diff_bytes 扣减同纪律，防 spec+pack 叠加撑爆窗口。
- **D3 refute 是否带 spec**：**带**。hiapi 真实价值（F2：gpt-5.5 结合设计 D8 驳回「像 bug 实为设计选择」的误报）正是 refute 拿到设计才有的。不带则丢掉这个已实证的最亮点。（scope：refute prompt 加一个可选 spec 参照段，与主评审同源。）
- **D4 路径信任模型 + 敏感文件守卫（用户 2026-07-16 补）**：`--spec` 路径是**用户 CLI 提供=可信**（运行者自身），**路径遍历**上允许任意用户可达路径（含 out-of-root，如 `../design.md`）——**区别于** needs 的模型提供=不可信（安全组 ④ 用 in-root 守卫）。用**直接有界读**（std::fs + size 帽），不套 ToolExecutor 的 in-root safe_join。
  **但注入内容会送到外部模型 provider**，故加**基本敏感文件守卫**：拒绝注入密钥/凭据类文件——denylist 匹配（扩展名 `.env`/`.pem`/`.key`/`.p12`/`.pfx`/`.pkcs12`/`.crt`/`.cer`、basename `id_rsa`/`id_ed25519`/`id_dsa`/`.netrc`/`.htpasswd`、以及名含 `secret`/`credential`/`password`/`token` 且非 `.md`/`.txt`/`.rst`/`.adoc` 文档类的）——命中 → **skip 该文件 + stderr 一行警告**（不硬中止评审，其余 spec 照常）；全被 skip → 等同未给 spec（已警告）。
  **信任面澄清（对照安全组 ④，一并入 decisions E12）**：`--spec`=**路径遍历信用户、内容敏感度不信任**（user-arg 也要防 secret exfil 给外部模型）；needs=路径与内容**皆不信任**（model-arg，in-root + git-tracked 守卫）。两条信任线不同源、不同守法。
- **D5 `--mode` 无需动作**：清理批 Task 1 已删 `--mode`（doc 删后 code 是唯一模式）。本 feature 不涉。

---

## Global Constraints

- 注释简体中文；`cargo test --lib` 全绿 + `cargo test --test review_e2e`（现 4）；`cargo clippy --all-targets 2>&1 | grep -c "warning:"` ≤ **8**；commit trailer `Co-Authored-By: Claude Sonnet 5 <noreply@anthropic.com>`（实施者档）；**永不 push**。
- 涉 `plugins/redteam/commands/` → `make gen` → `make verify` 0 FAIL。
- 空 `--spec`（默认）行为**完全不变**（`render_spec_section` 返空段、pack 预算不扣、模板槽渲染 `(none)` 或空）——本 feature 对不带 --spec 的既有评审零影响，回归测试钉住。

---

### Task 1: spec 参照段构造器（有界读 + 预算）

**Files:** `src/cli.rs`（新增 `render_spec_section` + `SPEC_BUDGET_BYTES`）；测试同文件 `#[cfg(test)]`。

**Interfaces:**
- Produces: `pub(crate) fn render_spec_section(spec_paths: &[String]) -> (String, usize)` —— 返回 (`<spec_docs>` 段字符串, 实际用掉字节数)；空/全不可读 → `(String::new(), 0)`。

- [ ] Step 1: 写失败测试 `spec_section_empty_when_no_paths`：`render_spec_section(&[])` == `(String::new(), 0)`。
- [ ] Step 2: 写失败测试 `spec_section_reads_and_wraps`：临时文件写 `"DESIGN D8: fail-open"`，`render_spec_section(&[path])` 返回段含 `<spec_docs>`、含文件路径、含 `DESIGN D8`、含 L0 参照框架语（如「检查实现是否偏离设计意图」），bytes_used>0。
- [ ] Step 3: 写失败测试 `spec_section_caps_and_marks_truncation`：写一个 > `SPEC_BUDGET_BYTES` 的文件，返回段字节 ≤ 帽+段壳、含截断标记；CJK 边界用 `floor_char_boundary` 不 panic。
- [ ] Step 3b: 写失败测试 `spec_section_rejects_sensitive_files`（D4 守卫）：`render_spec_section(&["x.env".into(), "id_rsa".into(), "design.md".into()])`（三文件都建、都有内容）→ 返回段**只含 design.md**、不含 x.env/id_rsa 的内容；`spec_section_all_sensitive_returns_empty`：只给 `secrets.pem` → `(String::new(), 0)`。
- [ ] Step 4: 实现（含 D4 敏感文件守卫）：
```rust
const SPEC_BUDGET_BYTES: usize = 64_000; // --spec 参照文档总帽——超则截断+标记（D2）

/// D4 守卫：拒绝把密钥/凭据类文件注入送外部模型。命中=skip+warn。
/// 路径遍历信用户（user-arg），但内容敏感度不信任（防 secret exfil）。
fn is_sensitive_spec_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    const DENY_EXT: &[&str] = &[".env",".pem",".key",".p12",".pfx",".pkcs12",".crt",".cer"];
    const DENY_BASE: &[&str] = &["id_rsa","id_ed25519","id_dsa",".netrc",".htpasswd"];
    const DOC_EXT: &[&str] = &[".md",".txt",".rst",".adoc"];
    if DENY_EXT.iter().any(|e| base.ends_with(e)) { return true; }
    if DENY_BASE.iter().any(|b| base == *b || base.starts_with(&format!("{b}."))) { return true; }
    // 名含敏感词且非文档类 → 拒（design.md 这类文档放行）
    let doc = DOC_EXT.iter().any(|e| base.ends_with(e));
    if !doc && ["secret","credential","password","token"].iter().any(|w| base.contains(w)) {
        return true;
    }
    false
}

/// 读 --spec 文档 → 有界参照段（供 [L0] 实现-对照-设计）。返回 (段, 用掉字节)。
/// 路径=用户 CLI 提供=可信路径遍历（D4）→ 直接有界读，允许 out-of-root；但敏感文件 denylist。
pub(crate) fn render_spec_section(spec_paths: &[String]) -> (String, usize) {
    if spec_paths.is_empty() { return (String::new(), 0); }
    let mut body = String::new();
    let mut used = 0usize;
    for p in spec_paths {
        if is_sensitive_spec_path(p) {
            eprintln!("redteam: 拒绝把疑似密钥/凭据文件注入评审上下文（防泄漏给外部模型）：{p}");
            continue;
        }
        if used >= SPEC_BUDGET_BYTES { break; }
        let Ok(mut content) = std::fs::read_to_string(p) else { continue };
        let remaining = SPEC_BUDGET_BYTES - used;
        let truncated = content.len() > remaining;
        if truncated {
            let cut = floor_char_boundary(&content, remaining);
            content.truncate(cut);
        }
        used += content.len();
        body.push_str(&format!(
            "### {p}\n```\n{content}\n```{}\n\n",
            if truncated { "\n（…spec 截断：超参照预算）" } else { "" }
        ));
    }
    if body.is_empty() { return (String::new(), 0); }
    let section = format!(
        "<spec_docs>\n关联设计/规格文档（供 [L0] 检查实现是否偏离设计意图；评审对象仍是代码、spec 仅作参照）：\n\n{body}</spec_docs>\n"
    );
    (section, used)
}
```
（`floor_char_boundary`：若 crate 已有则复用；无则 `let mut c=remaining; while !content.is_char_boundary(c) { c-=1 } c`。）
- [ ] Step 5: 跑测试全绿；clippy ≤8。
- [ ] Step 6: Commit `feat(spec): --spec 参照段构造器——有界读+预算帽+敏感文件守卫（承 E10；D4 防 secret exfil）`（trailer）。

---

### Task 2: 第一趟主评审接入（build_bundle_prompt + 模板槽 + fanout + run_code_review 读 args.spec）

**Files:** `src/prompt/mod.rs`（`build_bundle_prompt` 加 `spec_docs` 参 + `{{SPEC_DOCS}}` 渲染）、`src/prompt/templates/adversarial-code-bundle.md`（加 `{{SPEC_DOCS}}` 槽 + `<evidence>` 提及 spec）、`src/orchestrate/mod.rs`（`run_code_fanout` 透传 spec 段）、`src/cli.rs`（`run_code_review` 读 `args.spec`→`render_spec_section`→扣 pack 预算→穿进 fanout）。

**Interfaces:**
- Consumes: `render_spec_section`（Task 1）。
- Produces: `build_bundle_prompt(target_label, bundle, user_focus, other_files, incremental, dossier, spec_docs)`（7 参）；`run_code_fanout` 尾增 `spec_docs: &str`。

- [ ] Step 1: 模板 `adversarial-code-bundle.md`——`<evidence>` 块内 diffs/full-source/dossier 清单**补一项** spec docs；在 `<review_method>` 前**新增**：
```
<spec_docs>
{{SPEC_DOCS}}
</spec_docs>
```
（若 `{{SPEC_DOCS}}` 空则渲染为 `(none — no spec docs provided)`。）
- [ ] Step 2: 写失败测试 `bundle_prompt_injects_spec_docs`：`build_bundle_prompt(...,"<spec_docs>…DESIGN D8…</spec_docs>\n")` 输出含该段；`bundle_prompt_spec_none_renders_placeholder`：空 spec → 含 `(none` 占位、不含裸 `{{SPEC_DOCS}}`。
- [ ] Step 3: `build_bundle_prompt` 加末位参 `spec_docs: &str`，`render` 表加 `("{{SPEC_DOCS}}", if spec_docs.is_empty() { "(none — no spec docs provided)" } else { spec_docs })`。更新既有测试调用点（末位补 `""`）。
- [ ] Step 4: `run_code_fanout` 尾增 `spec_docs: &str`，透传给每个 `build_bundle_prompt`。更新调用点。
- [ ] Step 5: `run_code_review`：fan-out 前 `let (spec_section, spec_used) = render_spec_section(&args.spec);`，`let budget = pack_budget_bytes(cfg, &model_names).saturating_sub(spec_used);`（D2 扣减），把 `&spec_section` 穿进 `run_code_fanout`。
- [ ] Step 6: 写 e2e/集成测试 `code_review_with_spec_injects_reference`（FakeProvider 断言 prompt 含 spec 段）；`code_review_without_spec_unchanged`（空 --spec 回归钉：prompt 无 spec 段、budget 未扣）。
- [ ] Step 7: 全测试绿；clippy ≤8。
- [ ] Step 8: Commit `feat(spec): 第一趟主评审接入 --spec 参照段（模板 L0 槽落地）`（trailer）。

---

### Task 3: 第二趟补查接入（second_pass_prompt）

**Files:** `src/cli.rs`（`second_pass_prompt` 透传 spec 段到其内部 `build_bundle_prompt` 调用；`run_code_review` 二阶段构造点传 `spec_section`）。

**Interfaces:** `second_pass_prompt(bundles, root, budget, target_label, focus, idx, needs, drafts, incremental, spec_docs)`（10 参）。

- [ ] Step 1: 查 `second_pass_prompt` 尾部如何组装 prompt（`build_bundle_prompt` 或 render）；spec 段与 needs evidence 并存时各自独立块。
- [ ] Step 2: 写失败测试 `second_pass_injects_spec`：带 spec 的二阶段 prompt 含 `<spec_docs>` 段。
- [ ] Step 3: `second_pass_prompt` 加末位参 `spec_docs: &str` 透传；`run_code_review` 二阶段闭包/构造点传入同一 `spec_section`（第一趟已算好，复用不重读）。更新调用点。
- [ ] Step 4: 全测试绿；clippy ≤8。
- [ ] Step 5: Commit `feat(spec): 第二趟补查接入 --spec 参照段`（trailer）。

---

### Task 4: refute 接入（承 hiapi F2「对照设计驳回误报」）

**Files:** `src/consolidate/verify.rs`（refute prompt 加可选 spec 参照段）、`src/cli.rs`（把 `spec_section` 传进 verify_findings 链）、可能 `src/prompt/templates/adversarial-refute-*.md`（refute 模板加 `{{SPEC_DOCS}}` 槽，若 refute 走模板）。

**Interfaces:** `verify_findings(..., spec_docs: &str)` 尾增参（沿现有签名末位加）。

- [ ] Step 1: 查 refute 当前 prompt 组装（verify.rs 的 refute pass——finding+evidence+precedents 如何拼）；确定 spec 段插入点（与 evidence 并列，框架语「参照设计判断是否为设计允许行为」）。
- [ ] Step 2: 写失败测试 `refute_prompt_carries_spec`：带 spec 的 refute prompt 含 spec 参照段。
- [ ] Step 3: 实现 refute prompt 加 spec 段；`verify_findings` 尾增 `spec_docs` 参，`run_code_review` 传 `&spec_section`。空 spec → refute 行为完全不变（回归钉）。更新调用点。
- [ ] Step 4: 全测试绿；clippy ≤8。
- [ ] Step 5: Commit `feat(spec): refute 接入 --spec 参照段（承 hiapi F2 对照设计驳回误报）`（trailer）。

---

### Task 5: --help 文案兑现 + plugin 壳同步（诚实在场文档）

**Files:** `src/main.rs`（`--spec` clap help 文案）、`plugins/redteam/commands/review.md`（`--spec` 示例/说明）、`plugins/redteam/codex-skills/review/SKILL.md`（make gen 同步）。

**Interfaces:** Produces: `--help` 与壳文案与实际行为一致（不再空承诺）。

- [ ] Step 1: `main.rs` `--spec` 的 clap doc 改为兑现语，如：「参照的设计/规格文档路径（可多次）——注入评审上下文，供 [L0] 检查实现是否偏离设计意图（评审对象仍是代码，spec 仅作参照；总注入 ≤64KB，超则截断）」。
- [ ] Step 2: `review.md` 若把 `--spec` 列作示例——保留并确认说明与实际一致（L0 实现对照设计、有界）。
- [ ] Step 3: `make gen` → `make verify` 0 FAIL。
- [ ] Step 4: `--help` 实跑核对文案；Commit `docs(spec): --help 与 plugin 壳兑现 --spec 语义（L0 实现对照设计）`（trailer）。

---

## 验收（controller）
- 单跑 `redteam review --base dev --spec <本仓某 design.md> --models <便宜模型>` 一发：确认 prompt 真含 `<spec_docs>` 段（可 REDTEAM 仪器或 FakeProvider dump 核）、评审正常、空 --spec 对照跑行为不变。
- 若有条件：小型真实「实现偏离设计」样例，看 L0 是否命中（承 hiapi F2 精神，但本仓样例即可）。

## Self-Review
- D1-D5 决策落各 task：D1/D2→Task1+2、D3→Task4、D4→Task1（直接读）、D5 无动作。空 --spec 零影响=贯穿回归钉（Task2 Step6 / Task4 Step3）。predecessor：research 归因更正已记（2026-07-16）；执行前需在 decisions 补记「E12：--spec 接 code pack + user-arg vs model-arg 双信任面（对照安全组 ④）」。
