# 无工具默认化 批0+批1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> 状态: 锁定 · design=`.doc/design/2026-07-13-redteam-no-tools-default-design.md`（v1）· decisions E8 · 批 2/3 等批 1 验收数据后另拆

**Goal:** 把 redteam review 的默认形态翻转为「变更集全景 pack + 无工具评审」（`tools=two-stage` 默认），并交付 T43 refute parse 韧性与批 1 验收所需的两枚实验仪器。

**Architecture:** 三条线：①T43——refute parse 失败时前缀嗅探兜底（重试层已由 tool_loop REPAIR 内建）；②默认翻转——`REDTEAM_NO_TOOLS`/`REDTEAM_PACK_SCOPE` 两个 env 仪器收编为 config `[review] tools` 三值档位 + changeset scope 无条件默认（兄弟传真 diff），配 per-model 预算函数护住 token 硬墙；③验收仪器——单格全审臂 + 纯 0 壳减法臂（dev 态 env 门）。

**Tech Stack:** Rust 单 crate（clap derive CLI / serde+toml config / tokio async）。

## Global Constraints

- 代码注释简体中文、模型面散文（system/prompt 模板）英文——与现仓一致。
- 每 task 完成时：`cargo test` 全绿（现基线 371 lib 测试）；`cargo clippy --all-targets 2>&1 | grep -c "warning:"` ≤ **8**（现基线，不得新增）；task 涉及 commands/*.md 时跑 `make verify`（本 plan 各 task 不涉及，可跳）。
- 每 task 一个 commit，message 末尾 trailer：`Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`。**永不 push**（用户 defer）。
- dev 态 env 仪器（Task 6）注释必须含「随 M12 拆」字样。
- 常数值抄 design §2.2 逐字：`PACK_BUDGET_BYTES=256_000`、`OUTPUT_RESERVE_TOKENS=16_000`、`BYTES_PER_TOKEN=3`、`SHELL_RESERVE_BYTES=24_000`、未知模型 context window 缺省 `128_000`。
- config `[review] tools` 值域逐字：`"off" | "two-stage" | "on"`，默认 `two-stage`。

---

### Task 1: T43——refute parse 失败前缀嗅探

**Files:**
- Modify: `src/consolidate/verify.rs`（`refute_one` 的 `Err(e)` 分支，约 :99-110；新增 `sniff_refute_verdict`）
- Test: 同文件 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `crate::provider::ProviderError`（`Parse(String)` variant，见 `src/provider/mod.rs:33`）；provider 的 Parse 错误 message 含原始输出前 120 字符（`diagnostic_parse_error`，`src/provider/tool_loop.rs:173` 格式 `"final JSON parse failed (finish_reason=…): <原文>"`)。
- Produces: `fn sniff_refute_verdict(parse_msg: &str) -> Option<VerifyVerdict>`（private）。

背景（写进代码注释的知识）：重试层已存在——tool_loop 对 parse 失败自动发 REPAIR_PROMPT 重试一轮；走到 `Err(Parse)` 说明 repair 也失败。本 task 只补嗅探层与兜底注记。

- [ ] **Step 1: 写失败测试（嗅探纯函数 + 端到端 Refuted 生效）**

在 `verify.rs` tests mod 末尾追加（`VerifyVerdict` 若未 derive `PartialEq` 则用 `matches!` 改写断言）：

```rust
#[test]
fn sniff_refute_verdict_prefix_semantics_only() {
    // T43 层①：markdown 包装的 verdict 前缀可救回
    let m = "structured-output parse failed: final JSON parse failed (finish_reason=stop): **REFUTED: The claim is false because…";
    assert_eq!(sniff_refute_verdict(m), Some(VerifyVerdict::Refuted));
    let u = "structured-output parse failed: final JSON parse failed (finish_reason=stop): Claim upheld: the guard is missing";
    assert_eq!(sniff_refute_verdict(u), Some(VerifyVerdict::Upheld));
    // 截断 JSON 头 → 嗅不出，交兜底
    let j = r#"structured-output parse failed: final JSON parse failed (finish_reason=stop): {"findings": ["#;
    assert_eq!(sniff_refute_verdict(j), None);
    // 只嗅头部前缀：正文提及 REFUTED 不得误判（"would be REFUTED if…"）
    let mid = "structured-output parse failed: final JSON parse failed (finish_reason=stop): The claim would be REFUTED if callers pass empty vec";
    assert_eq!(sniff_refute_verdict(mid), None);
}

/// 恒返 Parse 错误的 provider——模拟 repair 后仍 parse 失败、但原文头部带 verdict 前缀。
struct ParseErrProvider(String);
#[async_trait::async_trait]
impl crate::provider::ReviewProvider for ParseErrProvider {
    async fn run(&self, _req: crate::provider::LensRequest)
        -> Result<crate::provider::LensCall, crate::provider::ProviderError> {
        Err(crate::provider::ProviderError::Parse(self.0.clone()))
    }
}

#[tokio::test]
async fn refute_parse_failure_sniffs_refuted_prefix() {
    // T43 实证场景（粒度 A/B 两跑）：refute 意图 REFUTED 但 JSON 包装失败 → 现状按原 conf 存活
    // → verdict 假 reject。嗅探后应判 Refuted、confidence 归零，并留 warning 可观测。
    let provider = ParseErrProvider(
        "structured-output parse failed: final JSON parse failed (finish_reason=stop): **REFUTED: trailing text".into());
    let f = finding("missing auth", Severity::High, 1, "alpha");
    let (out, w, _u) = verify_findings(vec![f], &provider, &["alpha".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[]).await;
    assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted, "嗅探救回 REFUTED 语义");
    assert_eq!(out[0].confidence, 0.0, "refuted 照常降 conf 下 floor");
    assert_eq!(w.len(), 1, "嗅探路径仍必须产出 warning（可观测）");
    assert!(w[0].contains("sniffed"), "warning 注明嗅探来源: {}", w[0]);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib sniff_refute -- --nocapture && cargo test --lib refute_parse_failure`
Expected: FAIL —— `sniff_refute_verdict` not found。

- [ ] **Step 3: 实现嗅探函数 + Err 分支接线**

在 `source_snippet` 之后加：

```rust
/// T43 层①：refute structured-output parse 失败（repair 重试后仍失败）的前缀嗅探。
/// provider 的 Parse 错误携带原始输出前 120 字符（diagnostic_parse_error）——refute 契约的
/// verdict 前缀（REFUTED/UPHELD）在头部，JSON 包装失败通常不吞语义。只嗅 strip 包装后的
/// 头部前缀，不做全文词搜——refute 散文里 "would be REFUTED if" 会误判。嗅不出 → None，
/// 交层③兜底（保守 Upheld 保 blocking 力：假 approve 吞真问题是 r1 20260703 老事故方向）。
fn sniff_refute_verdict(parse_msg: &str) -> Option<VerifyVerdict> {
    // diagnostic 格式 "…(finish_reason=…): <原文>"——固定前缀先于原文出现，取第一个 "): " 之后。
    let raw = &parse_msg[parse_msg.find("): ")? + 3..];
    let stripped = raw.trim_start_matches(['*', '#', '`', '"', '\'', ' ', '\n']);
    let lower: String = stripped.chars().take(24).collect::<String>().to_lowercase();
    if lower.starts_with("refuted") {
        Some(VerifyVerdict::Refuted)
    } else if lower.starts_with("upheld") || lower.starts_with("claim upheld") {
        Some(VerifyVerdict::Upheld)
    } else {
        None
    }
}
```

`refute_one` 的 `Err(e)` 分支改为：

```rust
        // 保守：refute 调用失败不冤枉 finding（保留其 blocking 力），但失败必须进 warnings。
        // T43 层①：Parse 失败先嗅原文头部 verdict 前缀（层② repair 重试已由 tool_loop 内建）。
        Err(e) => {
            let sniffed = match &e {
                crate::provider::ProviderError::Parse(msg) => sniff_refute_verdict(msg),
                _ => None,
            };
            match sniffed {
                Some(verdict) => {
                    let warn = format!("refute [{}] model {model}: parse failed, verdict sniffed from raw prefix: {e}", finding.id);
                    (Verify { verdict, model, note: "sniffed from unparsed refute output".into() }, Some(warn), (0, 0, 0))
                }
                None => {
                    let warn = format!("refute [{}] model {model}: {e}", finding.id);
                    (Verify { verdict: VerifyVerdict::Upheld, model, note: "refute unavailable".into() }, Some(warn), (0, 0, 0))
                }
            }
        }
```

注意：外层 `verify_findings` 对 `Refuted` 已有「confidence=0.0 降下 floor」处理——嗅探产出走同一路径，勿另写。

- [ ] **Step 4: 跑测试确认通过 + 全量回归**

Run: `cargo test --lib`
Expected: 全绿（371+2）。既有 `refute_failure_is_conservatively_upheld` 必须仍绿（非 Parse 错误路径不变）。

- [ ] **Step 5: Commit**

```bash
git add src/consolidate/verify.rs
git commit -m "fix(refute): T43 层①——parse 失败前缀嗅探救回 verdict 语义

粒度 A/B 两跑实证：refute 意图 REFUTED 但 JSON 包装失败 → 按原 conf 存活
→ verdict 假 reject（outcome-determining）。层② repair 重试 tool_loop 已内建；
本层对 Parse 错误携带的原文头部（120 字符）strip 包装后嗅 verdict 前缀，
只嗅头部防散文误判；嗅不出维持保守 Upheld（宁假 reject，E8 裁决 G）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: tool_loop 工具开关参数化（拆 REDTEAM_NO_TOOLS env）

**Files:**
- Modify: `src/provider/tool_loop.rs`（`run_tool_loop` 签名约 :58-73、`ToolLoopProvider` 约 :200-245、`NO_TOOLS_SYSTEM` 注释约 :190、tests 全部 `run_tool_loop` 调用点）

**Interfaces:**
- Produces: `run_tool_loop(…, retry_budget: u32, tools_enabled: bool)`（尾加第 8 参）；`ToolLoopProvider::new(backend, root, tools_enabled: bool)`（第 3 参）。Task 3 的 registry 构造依赖 `new` 的新签名。

- [ ] **Step 1: 写失败测试**

把现有测试 `no_tools_env_swaps_system_and_still_parses` 改名重写为参数版（删 env 操作）：

```rust
#[tokio::test]
async fn tools_disabled_swaps_system_and_still_parses() {
    // tools=off/two-stage 第一趟：空工具集 + 上下文自足 system，单轮出 JSON 照常解析。
    let backend = ScriptedBackend::new(vec![text_turn(r#"{"findings": []}"#)]);
    let ex = ToolExecutor::new(std::env::temp_dir());
    let call = run_tool_loop(&backend, &ex, "sys", "review", 8, std::time::Duration::from_secs(60), 0, false)
        .await
        .unwrap();
    assert_eq!(call.iterations, 1);
    let sent = backend.sent_systems();
    assert!(sent[0].contains("No tools are available"), "无工具 system 生效: {}", sent[0]);
}
```

（`ScriptedBackend`/`text_turn`/`sent_systems` 以旧测试 `no_tools_env_swaps_system_and_still_parses` 现用的 mock 名与构造为准——同文件已存在，实现者照旧测试的写法迁移，不新造 mock。若旧测试无 system 断言助手，保留其原断言方式。）

- [ ] **Step 2: 跑测试确认编译失败**

Run: `cargo test --lib tools_disabled`
Expected: FAIL —— `run_tool_loop` 参数个数不符。

- [ ] **Step 3: 实现参数化**

`run_tool_loop` 签名尾加 `tools_enabled: bool`，函数体开头替换 env 读：

```rust
    // 工具档位（design 2026-07-13 §2.4）：off/two-stage 第一趟 = 空工具集 + 上下文自足
    // system（F1：上下文与工具是替代品，都给=最贵；F4：给了工具必被用）。backend 对空
    // tools 已有干净处理（不发 tools 键）。on = 对话式工具（诊断/对照档）。
    let tools = if tools_enabled { tool_specs() } else { Vec::new() };
    let system = if tools_enabled { system } else { NO_TOOLS_SYSTEM };
```

删除原 `let no_tools = std::env::var_os("REDTEAM_NO_TOOLS")…` 三行及其「实验仪器/随 M12 拆」注释块。

`NO_TOOLS_SYSTEM` 的 doc 注释改为转正措辞：

```rust
/// 上下文自足评审 system（tools=off / two-stage 第一趟）：明示无工具、只凭已给上下文裁决，
/// 证据不在场则降 confidence 或弃 claim（S2-b'' 转正，2026-07-13 design §2.1）。
```

`ToolLoopProvider` 加字段 `tools_enabled: bool`；`new` 加第 3 参并赋值；`run()` 里 `run_tool_loop(…, self.retry_budget, self.tools_enabled)`。

其余 6 个测试调用点尾加 `, true`（保持旧行为）。`main.rs:366` 的 `ToolLoopProvider::new(backend, cwd.to_path_buf())` 暂加 `, true`（Task 3 接真值——本 task 行为零变化）。

- [ ] **Step 4: 跑全量测试**

Run: `cargo test --lib && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
Expected: 全绿；clippy ≤8。

- [ ] **Step 5: Commit**

```bash
git add src/provider/tool_loop.rs src/main.rs
git commit -m "refactor(provider): 工具开关参数化——REDTEAM_NO_TOOLS env 仪器收编

run_tool_loop/ToolLoopProvider 加 tools_enabled 参数，NO_TOOLS_SYSTEM 从
实验壳转正为上下文自足 system。本 commit 行为零变化（调用点全传 true），
默认翻转在 config 管道接线后生效（E8 裁决 A）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: config `[review] tools` 三值档位 + `--tools` flag + registry 接线

**Files:**
- Modify: `src/config/mod.rs`（`ReviewConfig` 约 :56-85）
- Modify: `src/main.rs`（Review 子命令 args 约 :60-88、ReviewArgs 组装约 :392/440、registry 构造 :366 所在函数）
- Modify: `src/cli.rs`（`ReviewArgs` struct :16-42）
- Test: `src/config/mod.rs` tests + `src/cli.rs` 或 `src/config` 内 `effective_tools` 单测

**Interfaces:**
- Consumes: Task 2 的 `ToolLoopProvider::new(backend, root, tools_enabled)`。
- Produces: `config::ToolsMode`（`Off | TwoStage | On`，serde kebab-case）；`ReviewConfig.tools: ToolsMode`（默认 `TwoStage`）；`ReviewArgs.tools: Option<String>`；`config::ToolsMode::from_flag(s: &str) -> Option<ToolsMode>`。

- [ ] **Step 1: 写失败测试（config 解析 + 默认值 + flag 覆盖）**

`src/config/mod.rs` tests：

```rust
#[test]
fn tools_mode_parses_and_defaults_to_two_stage() {
    let c = Config::from_layers(&[r#"[review]
tools = "off""#]).unwrap();
    assert_eq!(c.review.tools, ToolsMode::Off);
    let d = Config::from_layers(&[""]).unwrap();
    assert_eq!(d.review.tools, ToolsMode::TwoStage, "默认 two-stage（E8 裁决 A）");
    let on = Config::from_layers(&[r#"[review]
tools = "on""#]).unwrap();
    assert_eq!(on.review.tools, ToolsMode::On);
}

#[test]
fn tools_mode_from_flag_covers_three_values() {
    assert_eq!(ToolsMode::from_flag("off"), Some(ToolsMode::Off));
    assert_eq!(ToolsMode::from_flag("two-stage"), Some(ToolsMode::TwoStage));
    assert_eq!(ToolsMode::from_flag("on"), Some(ToolsMode::On));
    assert_eq!(ToolsMode::from_flag("bogus"), None);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib tools_mode`
Expected: FAIL —— `ToolsMode` not found。

- [ ] **Step 3: 实现 config 侧**

`src/config/mod.rs`：

```rust
/// 评审工具档位（design 2026-07-13 §2.4，E8 裁决 A）：off=纯无工具；two-stage=第一趟
/// 无工具（needs 二阶段批 2 补全，当前行为=off 等价）；on=对话式工具（诊断/对照档）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolsMode { Off, TwoStage, On }

impl ToolsMode {
    /// CLI flag 字符串 → 档位（clap value_parser 已限值域，此处仍返回 Option 防漂移）。
    pub fn from_flag(s: &str) -> Option<ToolsMode> {
        match s {
            "off" => Some(ToolsMode::Off),
            "two-stage" => Some(ToolsMode::TwoStage),
            "on" => Some(ToolsMode::On),
            _ => None,
        }
    }
}
fn default_tools() -> ToolsMode { ToolsMode::TwoStage }
```

`ReviewConfig` 加字段（并同步 `impl Default for ReviewConfig`）：

```rust
    /// 工具档位：off | two-stage（默认）| on。
    #[serde(default = "default_tools")]
    pub tools: ToolsMode,
```

- [ ] **Step 4: 实现 CLI/main 侧**

`src/cli.rs` `ReviewArgs` 加（`serde(default)` 保旧 args.json 兼容——resume/背景 job 重建不炸）：

```rust
    /// `--tools <off|two-stage|on>`：覆盖 config 的工具档位。
    #[serde(default)]
    pub tools: Option<String>,
```

`src/main.rs` Review 子命令加 clap arg（放 `conf_floor` 之后）：

```rust
        /// 工具档位：off（纯无工具）| two-stage（默认：无工具评审，证据不足二阶段补查）
        /// | on（对话式工具，诊断/对照档）。缺省读 config [review].tools。
        #[arg(long, value_parser = ["off", "two-stage", "on"])]
        tools: Option<String>,
```

两处 ReviewArgs 组装点（:392 与 :440 附近）补 `tools` 字段透传；其余 ReviewArgs 字面量构造点（如 :1049 测试）补 `tools: None`。

registry 构造函数（:366 所在 fn）加参 `tools_enabled: bool`，`ToolLoopProvider::new(backend, cwd.to_path_buf(), tools_enabled)`。调用处解析生效值：

```rust
    let tools_mode = args.tools.as_deref()
        .and_then(crate::config::ToolsMode::from_flag)
        .unwrap_or(cfg.review.tools);
    let tools_enabled = matches!(tools_mode, crate::config::ToolsMode::On);
```

**resume 路径核查（必须做）**：`run_resume` 从 `read_args_raw` 重建 ReviewArgs 后同样走 registry 构造——确认该路径拿到重建后的 `args.tools` 再算 `tools_enabled`；若 resume 路径 registry 在 args 重建**之前**构造，把解析挪到构造点之后或传闭包。旧 job 的 args.json 无 tools 字段 → `None` → 落 config 默认，行为可接受。

- [ ] **Step 5: 跑全量测试 + 冒烟 --help**

Run: `cargo test --lib && cargo run --quiet -- review --help | grep -A2 -- "--tools"`
Expected: 全绿；help 出现三值说明。

- [ ] **Step 6: Commit**

```bash
git add src/config/mod.rs src/cli.rs src/main.rs
git commit -m "feat(config): [review] tools 三值档位 + --tools flag——无工具默认翻转生效

默认 two-stage（E8 裁决 A：未发布零外部用户、dogfood 即验证、on 档一键回退
风险有界）。批 1 内 two-stage 行为=纯无工具单趟，needs 二阶段批 2 补全。
args.json serde(default) 保旧 job resume 兼容。--help 同步（裸终端唯一在场文档）。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: changeset scope 默认化 + 兄弟真 diff + 降级空产出入 omit + 散文改写

**Files:**
- Modify: `src/cli.rs`（`build_packs` 内 changeset 分支 :375-390 与其测试）
- Modify: `src/pack/mod.rs`（`body_section_impl` 回落分支；两处省略清单标题 :164/:285 与测试 :504）
- Modify: `src/prompt/mod.rs`（`render_dossier_section` 信任声明 :272-277 与测试 :603-608）

**Interfaces:**
- Consumes: 无（Task 2/3 不影响本层）。
- Produces: `build_packs` 默认即变更集全景（无 env 门）；兄弟 `FileChange` 带真 diff。

- [ ] **Step 1: 写/改失败测试**

`src/cli.rs`：把 `build_packs_changeset_scope_augments_zero_shell_with_sibling_files` 改为默认行为测试——删 `ENV_LOCK`/env set，改名 `build_packs_default_gives_changeset_panorama_with_real_sibling_diff`，断言追加：兄弟条目携带其原 bundle 的 diff（pack 文本包含兄弟文件的改动 def 或全文块，且**同 path 不重复**）。同文件其它 build_packs 测试若因兄弟全景新增内容而红，按新默认更新断言（自家优先次序不变）。

`src/pack/mod.rs` tests 追加：

```rust
#[test]
fn body_section_degrade_empty_output_goes_to_omit() {
    // E8 裁决 D：全文超预算且 def 锚不到（如 hunk 全在 mod tests，T42 场景）→ 禁真空，
    // 必须 omit 可观测，不得静默消失。
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).unwrap();
    // 大文件（超 500B 预算）+ diff 行落在无 def 可锚处（文件头注释区）
    let filler = format!("// header\n{}", "// filler line\n".repeat(200));
    std::fs::write(root.join("src/a.rs"), &filler).unwrap();
    let b = Bundle { files: vec![FileChange {
        path: "src/a.rs".into(),
        diff: "@@ -1,0 +2,1 @@\n+// filler line\n".into(),
    }] };
    let mut omit = Vec::new();
    let s = body_section_impl(&b, root, 500, &mut omit, false);
    assert!(s.is_empty() || !s.contains("src/a.rs"), "无块可入");
    assert!(omit.iter().any(|o| o.contains("src/a.rs")), "降级空产出必入 omit: {omit:?}");
}
```

`src/prompt/mod.rs`：测试 :603-608 的四条断言改为（保留 `</dossier>` 断言）：

```rust
        assert!(s.contains("verbatim snapshots of the current working tree"), "0 壳快照声明在场");
        assert!(s.contains("lower your confidence or state the gap"), "证据不在场的降 conf 指令在场");
        assert!(!s.contains("read_files"), "无工具默认下散文不得指挥不存在的工具");
        assert!(!s.contains("tool calls"), "无工具默认下散文不得提及 tool calls");
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib build_packs_default_gives && cargo test --lib body_section_degrade && cargo test --lib prompt`
Expected: 三处 FAIL。

- [ ] **Step 3: 实现**

`src/cli.rs` :375-390 替换（删 env 读；`augmented` 中间变量随重写消失）：

```rust
    // 变更集全景 0 壳（S2-b'' 转正，design 2026-07-13 §2.1）：每格 = 自家文件（置顶）+
    // 兄弟改动文件。兄弟带真 diff——空 diff 会让 def 降级不可达且静默消失（E8 裁决 D）。
    let entries: Vec<String> = bundles
        .iter()
        .map(|b| {
            let mut files = b.files.clone();
            for ob in bundles {
                for fc in &ob.files {
                    if !files.iter().any(|e| e.path == fc.path) {
                        files.push(fc.clone());
                    }
                }
            }
            let pb = crate::bundle::Bundle { files };
            let pb = &pb;
            // …（以下渲染逻辑照旧：REDTEAM_PACK 引擎分派 build_pack/build_pack_scip 不动）
```

`src/pack/mod.rs` `body_section_impl` 全文超预算回落分支（`let defs = …` 起的段落）加贡献跟踪：

```rust
        let defs = compile_defs(lang);
        let lines: Vec<&str> = content.lines().collect();
        let mut contributed = false;
        for (start, end) in changed_def_ranges(&fc.diff, &lines, &defs) {
            // …（循环体照旧；两个入块/入 omit 分支各加 contributed = true）
        }
        // E8 裁决 D：降级空产出禁真空——def 锚不到时必须 omit 可观测（T42 场景）。
        if !symbol_mode && !contributed {
            omit.push(format!("{}:1-{} — 全文超预算且 def 锚定失败（降级空产出）", fc.path, n_lines));
        }
```

（注：入 omit 的 `omit.push(…改动符号体)` 分支也算 contributed——「有 omit 记录」即非静默。）

两处省略清单标题（:164 build_pack 与 :285 build_pack_scip 内）：

```rust
        let mut sec = String::from("## 已省略（预算裁剪；证据不足时可在补查趟点名）\n");
```

（删除第二行 `read_files({paths:…})` 提示行；:504 等相关测试断言同步为新标题。）

`src/prompt/mod.rs` :274 信任声明整句替换为：

```rust
         The sections below are context for this change, assembled mechanically (heuristic facts, not judgments): the full source of each changed file (0-shell; files too large for the budget fall back to changed-symbol bodies), where the changed symbols are used elsewhere in the repo — its blast radius — found by word-boundary search (1-shell), and an omission list of file:line coordinates that overflowed the budget. Judge whether THIS change breaks these sites; do not audit them for pre-existing defects this change does not cause. The 0-shell sources are verbatim snapshots of the current working tree — treat them as the ground truth for those files. If a claim hinges on content that is NOT included here (see the omission list), lower your confidence or state the gap explicitly instead of guessing; your duty to verify against the provided evidence is undiminished.\n\n\
```

- [ ] **Step 4: 跑全量测试**

Run: `cargo test --lib && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
Expected: 全绿；clippy ≤8。

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/pack/mod.rs src/prompt/mod.rs
git commit -m "feat(pack): 变更集全景 0 壳转正默认 + 兄弟真 diff + 禁真空 omit + 散文无工具化

REDTEAM_PACK_SCOPE env 仪器收编（S2-b'' 转正）；兄弟带真 diff 修 def 降级
不可达与静默消失（E8 裁决 D）；降级空产出必入 omit（T42 场景可观测）；
省略清单/信任声明散文改写——无工具默认下不得指挥不存在的工具。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: per-model 预算函数

**Files:**
- Modify: `src/config/mod.rs`（`ProviderConfig` 加可选字段）
- Modify: `src/cli.rs`（常数区 :326 附近加函数；`build_packs` 签名加 `budget` 参；两个调用点 :566/:693 与全部测试调用点）

**Interfaces:**
- Consumes: `Config.providers: BTreeMap<String, ProviderConfig>`。
- Produces: `pub(crate) fn pack_budget_bytes(cfg: &Config, model_names: &[String]) -> usize`；`build_packs(bundles, root, budget: usize)`。

- [ ] **Step 1: 写失败测试**

`src/cli.rs` tests：

```rust
#[test]
fn pack_budget_respects_model_token_wall() {
    // design §2.2 预算函数：min(256K, (min_ctx − 16K 输出预留)×3 − 24K 壳预留)。
    let cfg = crate::config::Config::from_layers(&[r#"
[providers.Big]
kind = "openai"
model = "big-ctx"
context_window_tokens = 1000000
[providers.Small]
kind = "openai"
model = "small-ctx"
context_window_tokens = 40000
"#]).unwrap();
    // 未声明窗口的模型 → 保守缺省 128K → (128K−16K)*3−24K=312K → 字节帽 256K 生效
    assert_eq!(pack_budget_bytes(&cfg, &["Unknown".into()]), 256_000);
    // 大窗模型同样被字节帽封顶
    assert_eq!(pack_budget_bytes(&cfg, &["Big".into()]), 256_000);
    // 小窗模型：(40_000−16_000)*3−24_000 = 48_000
    assert_eq!(pack_budget_bytes(&cfg, &["Small".into()]), 48_000);
    // 多模型取最小窗（一份 pack 全模型安全）
    assert_eq!(pack_budget_bytes(&cfg, &["Big".into(), "Small".into()]), 48_000);
}
```

（`ProviderConfig.kind` 的合法值以 `ProviderKind` 反序列化为准——实现者查 `config/mod.rs` 现有测试用例照抄一个合法 kind。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib pack_budget_respects`
Expected: FAIL。

- [ ] **Step 3: 实现**

`src/config/mod.rs` `ProviderConfig` 加：

```rust
    /// 模型 context window（token）。缺省按保守 128K 估——预算函数用它护 token 硬墙
    /// （design 2026-07-13 §2.2）；确知更大窗的模型在此声明以解锁完整字节帽。
    #[serde(default)]
    pub context_window_tokens: Option<u32>,
```

`src/cli.rs` :326 附近：

```rust
const PACK_BUDGET_BYTES: usize = 256_000;
/// 预算函数三常数（design 2026-07-13 §2.2 逐字）：输出预留（推理型模型 findings JSON +
/// reasoning）、字节/token 保守换算（代码英文 ~3.5-4B/token，往小估防超模型墙）、prompt
/// 壳预留（rules/指令/信任声明，pack 预算外但占窗）。
const OUTPUT_RESERVE_TOKENS: u32 = 16_000;
const BYTES_PER_TOKEN: usize = 3;
const SHELL_RESERVE_BYTES: usize = 24_000;

/// per-model pack 预算：min(字节帽, (启用模型最小窗 − 输出预留)×3 − 壳预留)。
/// 双层墙——字节帽是策略、token limit 是硬墙；多模型取最小窗=一份 pack 全模型安全
/// （常数查表，不复活 S2-c per-model 规划器）。未声明窗口 → 保守 128K。
pub(crate) fn pack_budget_bytes(cfg: &crate::config::Config, model_names: &[String]) -> usize {
    let min_ctx = model_names
        .iter()
        .map(|m| cfg.providers.get(m).and_then(|p| p.context_window_tokens).unwrap_or(128_000))
        .min()
        .unwrap_or(128_000);
    let by_window = (min_ctx.saturating_sub(OUTPUT_RESERVE_TOKENS) as usize)
        .saturating_mul(BYTES_PER_TOKEN)
        .saturating_sub(SHELL_RESERVE_BYTES);
    PACK_BUDGET_BYTES.min(by_window)
}
```

`build_packs` 签名加 `budget: usize`，函数体内两处 `PACK_BUDGET_BYTES` 改用 `budget`。两个真实调用点（:566/:693，其作用域内 `cfg` 与 `model_names` 均已在手）改：

```rust
    let (dossiers, dossier_meta) = build_packs(&bundles, root, pack_budget_bytes(&cfg, &model_names));
```

（若该作用域模型名变量名不同——如 `ctx.model_names`——以现名为准。）全部测试调用点加 `, 1_000_000`（预算不设限保持旧断言）或按测试语义给小值。

- [ ] **Step 4: 跑全量测试**

Run: `cargo test --lib && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
Expected: 全绿；clippy ≤8。

- [ ] **Step 5: Commit**

```bash
git add src/config/mod.rs src/cli.rs
git commit -m "feat(pack): per-model 预算函数——token 硬墙护栏（design §2.2）

min(256K 字节帽, (min_ctx−16K 输出预留)×3B/token−24K 壳预留)；多模型取
最小窗一份 pack 全模型安全；context_window_tokens config 可声明、缺省
保守 128K（现 128K 档模型下为 no-op 保险，非行为变化）。常数查表，
不复活 S2-c 规划器。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: 批 1 验收仪器——单格全审臂 + 纯 0 壳减法臂

**Files:**
- Modify: `src/cli.rs`（:536 `let bundles = group(…)` 之后；`merge_bundles` 新函数 + 测试）
- Modify: `src/pack/mod.rs`（`build_pack` 与 `build_pack_scip` 的 refs/callee 段 gate）

**Interfaces:**
- Produces: `fn merge_bundles(bundles: Vec<Bundle>) -> Vec<Bundle>`（纯函数，可测）；env 门 `REDTEAM_SINGLE_CELL`（非空生效）、`REDTEAM_PACK_SHELLS=0`。

- [ ] **Step 1: 写失败测试（merge 纯函数）**

`src/cli.rs` tests：

```rust
#[test]
fn merge_bundles_flattens_all_files_into_one() {
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    let fc = |p: &str| FileChange { path: p.into(), diff: format!("@@ -1,0 +1,1 @@\n+x // {p}\n") };
    let bs = vec![
        Bundle { files: vec![fc("src/a.rs")] },
        Bundle { files: vec![fc("src/b.rs"), fc("src/c.rs")] },
    ];
    let merged = merge_bundles(bs);
    assert_eq!(merged.len(), 1, "单格全审：全部并成一个 bundle");
    assert_eq!(merged[0].files.len(), 3);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib merge_bundles`
Expected: FAIL。

- [ ] **Step 3: 实现**

`src/cli.rs`（`build_packs` 附近）：

```rust
/// 验收仪器（E8 裁决 B，随 M12 拆）：REDTEAM_SINGLE_CELL 非空 → 全变更集并成单 bundle
/// （单格全审 A/B 臂——分格在全景 pack 范式下=N 倍重复喂全景，质量对照据此判批 4 收缩）。
fn merge_bundles(bundles: Vec<crate::bundle::Bundle>) -> Vec<crate::bundle::Bundle> {
    vec![crate::bundle::Bundle { files: bundles.into_iter().flat_map(|b| b.files).collect() }]
}
```

:536 `let bundles = group(select_files(changes), Some(root));` 之后插：

```rust
    // 验收仪器（随 M12 拆）：单格全审臂。增量轮（run_incremental）不接——验收只走全量。
    let bundles = if std::env::var_os("REDTEAM_SINGLE_CELL").is_some() {
        merge_bundles(bundles)
    } else {
        bundles
    };
```

`src/pack/mod.rs`：`build_pack` 与 `build_pack_scip` 内、构建 refs（及 scip 的 callee）之前各加：

```rust
    // 验收仪器（E8 裁决 E 第一层，随 M12 拆）：REDTEAM_PACK_SHELLS=0 → 纯 0 壳减法臂。
    // S2-b'' 的「质量未降」属于「全景+1壳+callee」整包——1 壳/callee 的边际价值由此臂判决。
    let zero_shell_only = std::env::var("REDTEAM_PACK_SHELLS").ok().as_deref() == Some("0");
```

并将 refs/callee 段的构建与 push 用 `if !zero_shell_only { … }` 包住（omit 清单照常渲染；`build_pack_scip` 的 `fallback_count` eprintln 一并纳入 gate）。此仪器不写 env 单测（并行测试 env 竞态；验收时以 manifest_json 直查 pack 形状为其验证——S 臂先例）。

- [ ] **Step 4: 跑全量测试**

Run: `cargo test --lib && cargo clippy --all-targets 2>&1 | grep -c "warning:"`
Expected: 全绿；clippy ≤8。

- [ ] **Step 5: Commit**

```bash
git add src/cli.rs src/pack/mod.rs
git commit -m "feat(experiment): 批 1 验收仪器——REDTEAM_SINGLE_CELL 单格全审臂 + REDTEAM_PACK_SHELLS=0 纯 0 壳臂

E8 裁决 B/E：单格臂判 fan-out 收缩（批 4）、纯 0 壳臂判 SCIP/1 壳生死
（第一层减法实验，指标含 needs 触发率——批 2 后补测）。dev 态 env 门，
随 M12 拆。

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

## 批 1 验收（controller 手册——非 SDD task，维护者/主会话执行）

语料：worktree@`6f95c25`（`git worktree add --detach <scratchpad>/s0-wt-6f95c25 6f95c25` + `.redteam/config.toml` 写 `[review]\nmax_concurrency = 1`），`--base 6f95c25~1 --models DeepSeek --no-dismissed --background`，binary=`make build` 后的 `target/aarch64-apple-darwin/release/redteam`。

| 臂 | env/flag | 判什么 |
|----|----------|--------|
| 主臂（新默认） | 无（two-stage 默认生效） | 对照 S2-b'' B 臂旧数据：质量协议（核心 bug 面/quote 逐字/语义存活）+ Σinput/轮次 |
| 单格臂 | `REDTEAM_SINGLE_CELL=1` | E8-B：质量守住 → 批 4 收缩 fan-out；预期 input ~−80% |
| 纯 0 壳臂 | `REDTEAM_PACK_SHELLS=0` | E8-E 第一层：与主臂无差 → SCIP/1 壳安乐死 |
| 异语料 smoke | 任选中型 Rust 开源仓一个真 PR，主臂配置 | 不炸、pack 形状正确、verdict 可读即过（smoke 门） |
| doc smoke（终审 #1 提前） | 本仓任一 design 文档，`review --mode doc` 新默认 | 无工具 doc 评审不炸、findings 可读即过；降级窗口验收（引用文件无法查证=已知） |

每臂 1 跑（DeepSeek 便宜；主臂若与旧 B 臂数据冲突再加跑）。跑前先 sqlite 直查 `jobs.manifest_json` 验 pack 形状（S 臂先例）。结果落 `.doc/research/`（新 dated research 或追加 07-09 文档）。**T43 生效核查**：验收跑的 warnings 里若再现 parse 失败，确认嗅探 warning 出现且不再假存活。

## Self-Review 记录

- **Spec 覆盖**：design §2.1（changeset 默认/无工具默认/散文修/doc 同批——doc 走同一 provider 与 tools_enabled，Task 2/3 自然覆盖，无独立 task）→ Task 2/3/4；§2.2 预算函数 → Task 5（梯子 L1-L3 排序=批 3 不在本 plan）；§2.3 二阶段=批 2 不在本 plan；§2.4 → Task 3；批 0=Task 1；验收三臂 → Task 6+验收手册。E8 裁决 A/B/C(部分)/D(真 diff+omit)/E(仪器)/G/H 已落；C 的 needs 本体与 F 升档=批 2。
- **占位扫描**：mock 名（ScriptedBackend 等）与 ProviderKind 合法值两处标注「以现文件为准」——非逻辑占位，是既有代码适配点，已给兜底指引。
- **类型一致性**：`ToolsMode`（Task 3 定义）→ Task 3 内消费；`tools_enabled: bool` Task 2 produce → Task 3 consume；`pack_budget_bytes`/`build_packs(budget)` Task 5 内自洽；`merge_bundles` Task 6 内自洽。`body_section_impl(…, symbol_mode)` 沿用现有签名（粒度 A/B 仪器引入）。
