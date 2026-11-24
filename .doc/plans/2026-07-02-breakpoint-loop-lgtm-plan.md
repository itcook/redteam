# M7 · Loop SKILL（lgtm）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付 brk 循环原语（R8.1 quote 锚定 id、R8.2 --dismissed、穷尽性指令、.breakpoint/ 骨架 + brk init）与 capstone `/breakpoint:lgtm` 循环对抗 SKILL。

**Architecture:** 批次 1（Task 1-4，Rust）改造 id 组成、新增 project 模块与 CLI 面、在共用尾段 `assemble_report` 单点接线 dismissed 标记；批次 2（Task 5-6，零 Rust）以 commands/lgtm.md 承载循环协议并纳入双宿主生成与 packaging 防漂移。Spec：`.doc/design/2026-07-01-breakpoint-loop-lgtm-design.md`。

**Tech Stack:** Rust 2021（clap derive/serde/anyhow/tempfile），Claude plugin commands + gen-codex-skills。

## Global Constraints

- 分支：`feat/loop-lgtm`，自 `dev` 切出；commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`。
- PONYTAIL full：最短正确 diff；不新增依赖（tempfile 已在 dev-deps）；rustls-only 不变。
- 引擎冻结区（`src/tools/`、`provider/tool_loop.rs`、`provider/backends.rs`）**本计划不触碰**。`backends.rs:130` 的 envelope 误分类是独立 TODO（`.doc/TODO.md`），勿并入。
- id 组成（spec §1.1 逐字）：`fnv1a64_hex([file, normalize_quote(quote), normalize_title(title)])`；quote 归一 = **去除全部空白、保留大小写**；title 归一 = **lowercase + 空白折叠单空格**；quote 空退化 `fnv1a64_hex([file, line_start, line_end, normalize_title(title)])`。`hash.rs` 的 FNV golden 测试**不动**。
- `--dismissed <csv|dir>`：目录模式取文件名 `^[0-9a-f]{16}\.md$`，零内容解析；csv 逐 token 校验同格式，非法 → 报错（进程 exit 2）。dismissed **不喂模型 prompt**。
- 契约：`previously_dismissed: bool`（serde default + skip-if-false）；`run.dismissed_reappeared: Vec<String>`（skip-if-empty，排序去重）；verdict 豁免与 `verify==Refuted` 同处；`schema_version` "1"→"2"。
- `.breakpoint/`：`config.toml`（注释模板）、`.gitignore`（内容仅 `*`）、`dismissed/`、`rounds/`；**不设项目级 secrets**；已存在文件一律不覆盖；项目配置查找硬切 `.breakpoint/config.toml`（旧 `.breakpoint.toml` 不再读，检测到则 stderr 提示）。
- lgtm 命令：frontmatter 含 `description` + `disable-model-invocation: true`；body 含 `${CLAUDE_PLUGIN_ROOT}/bin/brk ` 与 `$ARGUMENTS`（packaging 断言要求）。

---

### Task 1: R8.1 finding id 换代（quote 锚定、行无关）

**Files:**
- Modify: `src/orchestrate/mod.rs`（`enrich`，现 id 组装在 46-51 行）
- Test: 同文件 `#[cfg(test)] mod tests`

**Interfaces:**
- Consumes: `crate::hash::fnv1a64_hex`（不变）。
- Produces: `enrich(&RawFinding, &str, &str) -> Finding` 签名不变，仅 id 语义换代；新增私有 `normalize_quote(&str) -> String`、`normalize_title(&str) -> String`。Task 3 的本证测试依赖新 id 语义。

- [ ] **Step 1: 建分支**

```bash
git checkout dev && git checkout -b feat/loop-lgtm
```

- [ ] **Step 2: 写失败测试**（追加到 `src/orchestrate/mod.rs` 的 tests 模块；若该文件尚无 tests 模块则新建）

```rust
fn raw(file: &str, ls: u32, le: u32, title: &str, quote: &str) -> RawFinding {
    RawFinding {
        severity: crate::domain::Severity::High, title: title.into(), body: "b".into(),
        file: file.into(), line_start: ls, line_end: le, confidence: 0.9,
        recommendation: "r".into(), hard_constraint: false, quote: quote.into(), lens: None,
    }
}

#[test]
fn id_ignores_line_shift_when_quote_present() {
    let a = enrich(&raw("a.rs", 10, 12, "SQL injection", "let q = fmt!(sql);"), "L1", "m");
    let b = enrich(&raw("a.rs", 99, 101, "SQL injection", "let q = fmt!(sql);"), "L1", "m");
    assert_eq!(a.id, b.id, "行号位移不得改变 id");
}

#[test]
fn id_ignores_whitespace_reformat() {
    let a = enrich(&raw("a.rs", 1, 2, "SQL injection", "let q =\n    fmt!( sql );"), "L1", "m");
    let b = enrich(&raw("a.rs", 1, 2, "SQL injection", "letq=fmt!(sql);"), "L1", "m");
    assert_eq!(a.id, b.id, "quote 去空白后等价 → 同 id");
}

#[test]
fn id_title_normalization_case_and_ws() {
    let a = enrich(&raw("a.rs", 1, 1, "SQL  Injection", "x"), "L1", "m");
    let b = enrich(&raw("a.rs", 1, 1, "sql injection", "x"), "L1", "m");
    assert_eq!(a.id, b.id);
}

#[test]
fn id_composition_pinned() {
    // 组成 golden：钉死组成与归一（FNV 算法本身由 hash.rs golden 钉死）。
    let f = enrich(&raw("src/x.rs", 5, 7, "Weak  Hash", "md5( pw )"), "L1", "m");
    assert_eq!(f.id, crate::hash::fnv1a64_hex(&["src/x.rs", "md5(pw)", "weak hash"]));
}

#[test]
fn id_falls_back_to_lines_without_quote() {
    let a = enrich(&raw("a.rs", 1, 2, "t", ""), "L1", "m");
    let b = enrich(&raw("a.rs", 3, 4, "t", ""), "L1", "m");
    assert_ne!(a.id, b.id, "无 quote 退化回行号路径");
    assert_eq!(a.id, crate::hash::fnv1a64_hex(&["a.rs", "1", "2", "t"]));
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p breakpoint orchestrate::tests -- --nocapture`
Expected: FAIL（`id_ignores_line_shift_when_quote_present` 等断言不等/不匹配）

- [ ] **Step 4: 最小实现**（`src/orchestrate/mod.rs`，替换 enrich 内 id 组装，新增两个私有函数）

```rust
/// R8.1：quote 去除全部空白（保大小写）——token 序列即身份，免疫 reformat。
fn normalize_quote(q: &str) -> String {
    q.chars().filter(|c| !c.is_whitespace()).collect()
}

/// R8.1：title lowercase + 空白折叠单空格。
fn normalize_title(t: &str) -> String {
    t.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}
```

enrich 内：

```rust
    let nq = normalize_quote(&raw.quote);
    let nt = normalize_title(&raw.title);
    // R8.1：quote 锚定、行无关；quote 缺失退化行号路径（不保证跨 fixup 稳定）。
    let id = if nq.is_empty() {
        fnv1a64_hex(&[&raw.file, &raw.line_start.to_string(), &raw.line_end.to_string(), &nt])
    } else {
        fnv1a64_hex(&[&raw.file, &nq, &nt])
    };
```

- [ ] **Step 5: 跑测试确认通过 + 全量回归**

Run: `cargo test`
Expected: 全绿。若既有测试硬编码了旧 id 值（grep 确认），按新语义更新其期望值。

- [ ] **Step 6: Commit**

```bash
git add src/orchestrate/mod.rs
git commit -m "feat(id): R8.1 quote-anchored line-free finding id

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: `.breakpoint/` 项目目录 —— project 模块 + `brk init` + config 硬切

**Files:**
- Create: `src/project.rs`
- Modify: `src/lib.rs`（加 `pub mod project;`，按现有 mod 列表字母序插入）
- Modify: `src/config/mod.rs:145`（config_paths 项目层路径）与 `load()`（约 :99，加旧文件提示）
- Modify: `src/main.rs`（`Cmd` 枚举加 `Init` + handler）
- Test: `src/project.rs` 内 tests 模块（tempfile）

**Interfaces:**
- Produces: `project::PROJECT_DIR: &str = ".breakpoint"`；`project::init_project(root: &Path) -> anyhow::Result<Vec<String>>`（返回本次创建的条目，幂等）；`project::parse_dismissed(value: &str) -> anyhow::Result<HashSet<String>>`。Task 3 消费 `parse_dismissed`。

- [ ] **Step 1: 写失败测试**（`src/project.rs` 底部）

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_skeleton_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let created = init_project(tmp.path()).unwrap();
        assert_eq!(created.len(), 4, "dismissed/ rounds/ .gitignore config.toml");
        assert!(tmp.path().join(".breakpoint/dismissed").is_dir());
        assert!(tmp.path().join(".breakpoint/rounds").is_dir());
        assert_eq!(std::fs::read_to_string(tmp.path().join(".breakpoint/.gitignore")).unwrap(), "*\n");
        // 二跑：全部已存在 → 零创建
        assert!(init_project(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn init_never_overwrites_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".breakpoint")).unwrap();
        std::fs::write(tmp.path().join(".breakpoint/.gitignore"), "custom\n").unwrap();
        init_project(tmp.path()).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join(".breakpoint/.gitignore")).unwrap(), "custom\n");
    }

    #[test]
    fn parse_dismissed_csv_valid_and_invalid() {
        let s = parse_dismissed("d2b371819297f98a, 20ba9b3025a8b421").unwrap();
        assert_eq!(s.len(), 2);
        assert!(parse_dismissed("not-an-id").is_err());
        assert!(parse_dismissed("").is_err());
        assert!(parse_dismissed("D2B371819297F98A").is_err(), "大写不合法（id 恒小写）");
    }

    #[test]
    fn parse_dismissed_dir_takes_filenames_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("d2b371819297f98a.md"), "任意内容 不解析").unwrap();
        std::fs::write(tmp.path().join("README.md"), "x").unwrap();
        std::fs::write(tmp.path().join("20ba9b3025a8b421.txt"), "x").unwrap();
        let s = parse_dismissed(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(s, std::collections::HashSet::from(["d2b371819297f98a".to_string()]));
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test project` → Expected: 编译失败（模块不存在）

- [ ] **Step 3: 实现 `src/project.rs`**

```rust
//! 项目级 `.breakpoint/` 目录：骨架初始化与 --dismissed 输入解析（spec §1.2/§1.4）。
use std::collections::HashSet;
use std::path::Path;

pub const PROJECT_DIR: &str = ".breakpoint";

const GITIGNORE_BODY: &str = "*\n";
const CONFIG_TEMPLATE: &str = "\
# breakpoint 项目级配置（优先级高于用户级 ~/.config/breakpoint/config.toml）。
# 密钥走环境变量或用户级 secrets.toml——绝不写入本文件（本目录在仓库领地内）。
#
# [providers.MyProvider]
# kind = \"openai\"          # openai | anthropic | openai-compatible
# base_url = \"https://…\"
# model = \"…\"
# api_key_env = \"MY_KEY_ENV\"
# priority = 0
#
# lgtm 循环的测试命令（可选；不设则按 Cargo.toml/package.json/Makefile 推断）：
# test_command = \"cargo test\"
";

/// 幂等创建 `.breakpoint/` 骨架，返回本次新建的条目；已存在的一律不动。
pub fn init_project(root: &Path) -> anyhow::Result<Vec<String>> {
    let dir = root.join(PROJECT_DIR);
    let mut created = Vec::new();
    for sub in ["dismissed", "rounds"] {
        let p = dir.join(sub);
        if !p.is_dir() {
            std::fs::create_dir_all(&p)?;
            created.push(format!("{PROJECT_DIR}/{sub}/"));
        }
    }
    for (name, body) in [(".gitignore", GITIGNORE_BODY), ("config.toml", CONFIG_TEMPLATE)] {
        let p = dir.join(name);
        if !p.exists() {
            std::fs::write(&p, body)?;
            created.push(format!("{PROJECT_DIR}/{name}"));
        }
    }
    if root.join(".breakpoint.toml").exists() {
        eprintln!("breakpoint: legacy .breakpoint.toml is no longer read; move it into .breakpoint/config.toml");
    }
    Ok(created)
}

fn is_hex16(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// R8.2 `--dismissed <csv|dir>`：目录 → 文件名即 id（`<16hex>.md`，零内容解析）；
/// 否则 csv，逐 token 校验 16 位小写十六进制。
pub fn parse_dismissed(value: &str) -> anyhow::Result<HashSet<String>> {
    let p = Path::new(value);
    if p.is_dir() {
        let mut out = HashSet::new();
        for entry in std::fs::read_dir(p)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".md") {
                if is_hex16(stem) {
                    out.insert(stem.to_string());
                }
            }
        }
        return Ok(out); // 空目录合法：无驳回
    }
    let mut out = HashSet::new();
    for tok in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        anyhow::ensure!(
            is_hex16(tok),
            "--dismissed: '{tok}' is neither an existing directory nor a 16-hex finding id"
        );
        out.insert(tok.to_string());
    }
    anyhow::ensure!(!out.is_empty(), "--dismissed: empty value");
    Ok(out)
}
```

- [ ] **Step 4: 接线**：`src/lib.rs` 加 `pub mod project;`；`src/config/mod.rs`：

```rust
// config_paths()（:143-147）项目层硬切：
    v.push(PathBuf::from(".breakpoint").join("config.toml"));
// load()（:99）体内起始处加：
        if std::path::Path::new(".breakpoint.toml").exists() {
            eprintln!("breakpoint: legacy .breakpoint.toml is no longer read; move it to .breakpoint/config.toml");
        }
```

`src/main.rs` `Cmd` 枚举加变体 + match 臂：

```rust
    /// 幂等创建项目级 .breakpoint/ 骨架（config.toml 模板、.gitignore、dismissed/、rounds/）。
    Init,
```

```rust
        Cmd::Init => match breakpoint::project::init_project(std::path::Path::new(".")) {
            Ok(c) if c.is_empty() => println!(".breakpoint/ already complete"),
            Ok(c) => c.iter().for_each(|p| println!("created {p}")),
            Err(e) => { eprintln!("error: {e}"); std::process::exit(2); }
        },
```

- [ ] **Step 5: 跑测试**：`cargo test project` → PASS；`cargo test` 全绿（若有测试依赖旧 `.breakpoint.toml` 路径，改为在 tempdir 写 `.breakpoint/config.toml`）。手动冒烟：`cargo run -- init` 两次，二次输出 `already complete`。

- [ ] **Step 6: Commit**

```bash
git add src/project.rs src/lib.rs src/config/mod.rs src/main.rs
git commit -m "feat(project): .breakpoint/ home — brk init skeleton, --dismissed parsing, config hard-cut

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: R8.2 契约接线 —— 字段、verdict 豁免、尾段标记、CLI flag、schema bump

**Files:**
- Modify: `src/domain/finding.rs`（Finding 加字段）、`src/domain/report.rs`（RunMeta 加字段）
- Modify: `src/verdict.rs`（compute_verdict 豁免）
- Modify: `src/cli.rs`（ReviewArgs、TailCtx、assemble_report、`mark_dismissed`、schema_version :154/:437）
- Modify: `src/main.rs`（Cmd::Review 加 `--dismissed` 并透传两处 ReviewArgs 构造）
- Test: `src/verdict.rs`、`src/cli.rs` 各自 tests 模块

**Interfaces:**
- Consumes: Task 2 的 `project::parse_dismissed`；Task 1 的新 id 语义（本证测试）。
- Produces: `Finding.previously_dismissed: bool`；`RunMeta.dismissed_reappeared: Vec<String>`；`cli::mark_dismissed(&mut [Finding], &HashSet<String>) -> Vec<String>`；`ReviewArgs.dismissed: Option<String>`；schema_version = "2"。

- [ ] **Step 1: 写失败测试**

`src/verdict.rs` tests（fixture `f(...)` 需同步加 `previously_dismissed: false`）：

```rust
    #[test]
    fn dismissed_blocking_finding_is_exempt() {
        let mut hi = f(Severity::High, 0.9, "L1", false);
        hi.previously_dismissed = true;
        assert_eq!(compute_verdict(&[hi], Severity::Medium, 0.5), Verdict::Approve);
    }
```

`src/cli.rs` tests（本证的单元形态，跨 Task 1+3）：

```rust
    #[test]
    fn dismissed_survives_line_shift_and_is_marked_exempt() {
        use crate::orchestrate::enrich;
        use std::collections::HashSet;
        let raw = |ls: u32, le: u32| crate::domain::RawFinding {
            severity: Severity::High, title: "Weak hash".into(), body: "b".into(),
            file: "src/x.rs".into(), line_start: ls, line_end: le, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false,
            quote: "md5(pw)".into(), lens: None,
        };
        let before = enrich(&raw(5, 7), "L1", "m");          // 第一轮：驳回它
        let dismissed: HashSet<String> = [before.id.clone()].into();
        let mut after = vec![enrich(&raw(50, 52), "L1", "m")]; // 修复位移后再现
        let reappeared = mark_dismissed(&mut after, &dismissed);
        assert_eq!(reappeared, vec![before.id]);
        assert!(after[0].previously_dismissed);
        assert_eq!(compute_verdict(&after, Severity::Medium, 0.5), Verdict::Approve);
    }
```

- [ ] **Step 2: 跑测试确认失败**：`cargo test` → 编译错误（字段/函数不存在）。

- [ ] **Step 3: 实现**

`src/domain/finding.rs` Finding 加（quote 字段之后）：

```rust
    /// R8.2：命中 --dismissed 清单——verdict 豁免、保留证据（spec §1.2 三件套）。
    #[serde(default, skip_serializing_if = "is_false")]
    pub previously_dismissed: bool,
```

同文件加 `fn is_false(b: &bool) -> bool { !*b }`。

`src/domain/report.rs` RunMeta 加（failed_models 之后）：

```rust
    /// R8.2 派生索引：本次命中 dismissed 的 finding id（权威源是 findings.previously_dismissed）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dismissed_reappeared: Vec<String>,
```

`src/verdict.rs` compute_verdict 的 blocking 条件加一行（Refuted 豁免同处）：

```rust
            && !f.previously_dismissed // R8.2：人已驳回 → 不 blocking（证据保留）
```

`src/cli.rs`：

```rust
/// R8.2：命中标记 + 派生索引（排序去重）。单点应用于共用尾段，三路径（code/doc/resume）同享。
pub(crate) fn mark_dismissed(
    findings: &mut [Finding],
    dismissed: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut reappeared = Vec::new();
    if dismissed.is_empty() {
        return reappeared;
    }
    for f in findings.iter_mut() {
        if dismissed.contains(&f.id) {
            f.previously_dismissed = true;
            reappeared.push(f.id.clone());
        }
    }
    reappeared.sort();
    reappeared.dedup();
    reappeared
}
```

- `ReviewArgs` 加 `#[serde(default)] pub dismissed: Option<String>,`（serde default 保证旧 job 的 args.json 可反序列化，resume 兼容）。
- `TailCtx` 加 `dismissed: &'a std::collections::HashSet<String>,`；`assemble_report` 在 `verify_findings` 之后、`compute_verdict` 之前：

```rust
    let mut findings = findings;
    let dismissed_reappeared = mark_dismissed(&mut findings, ctx.dismissed);
```

RunMeta 构造加 `dismissed_reappeared,`。
- 三个 TailCtx 构造方（`run_code_review`/`run_review`/`run_resume`）函数体起始统一解析（resume 因重读 args.json 自然继承）：

```rust
    let dismissed = match args.dismissed.as_deref() {
        Some(v) => crate::project::parse_dismissed(v)?,
        None => Default::default(),
    };
```

并传 `dismissed: &dismissed`。
- `src/main.rs` `Cmd::Review` 加 `#[arg(long)] dismissed: Option<String>,`，两处 ReviewArgs 字面量构造（前台 + background job args）补 `dismissed`。
- schema_version：`cli.rs:154`、`cli.rs:437` `"1"→"2"`；`grep -rn '"schema_version"\|schema_version' src tests` 同步所有断言/fixture（含 `output/mod.rs:59`）。
- `cargo build` 后按编译器指引给所有 Finding/RunMeta 字面量补 `previously_dismissed: false,` / `dismissed_reappeared: vec![],`（纯机械，测试 fixture 居多）。

- [ ] **Step 4: 跑测试**：`cargo test` → 全绿。手动冒烟：

```bash
cargo run -- review --dismissed not-an-id 2>&1; echo "exit=$?"   # 期望：报错 + exit=2
```

- [ ] **Step 5: Commit**

```bash
git add src/domain/finding.rs src/domain/report.rs src/verdict.rs src/cli.rs src/main.rs src/output/mod.rs
git commit -m "feat(dismissed): R8.2 --dismissed mark+exempt+aggregate; schema_version 2

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 4: 穷尽性指令内建 + 陈旧 bail 注释修正

**Files:**
- Modify: `src/prompt/templates/adversarial-{doc,code,doc-bundle,code-bundle}.md`（`<finding_bar>` 节内）
- Modify: `src/cli.rs:179` 与 `:276` 附近（doc comment + bail 文案）
- Test: `src/prompt/mod.rs` tests 模块

**Interfaces:** 无对外接口变更；模板文本通过 include_str! 编译期内嵌。

- [ ] **Step 1: 写失败测试**（`src/prompt/mod.rs` tests）

```rust
    #[test]
    fn all_adversarial_templates_carry_exhaustiveness_directive() {
        for t in [
            include_str!("templates/adversarial-doc.md"),
            include_str!("templates/adversarial-code.md"),
            include_str!("templates/adversarial-doc-bundle.md"),
            include_str!("templates/adversarial-code-bundle.md"),
        ] {
            assert!(t.contains("Exhaustiveness:"), "穷尽性指令缺失（spec §1.3 分界原则）");
        }
    }
```

- [ ] **Step 2: 确认失败**：`cargo test prompt` → FAIL。

- [ ] **Step 3: 实现**：四个模板的 `<finding_bar>` 节末尾（`</finding_bar>` 前）各加同一行：

```
Exhaustiveness: report ALL material findings you can identify in THIS pass — never hold findings back for a later round. For a recurring pattern, either list every instance or report the class once and enumerate all its variants.
```

`src/cli.rs` 两处（:179 注释 + :187 bail、:276 同型）统一改为：

```rust
/// `--background` 由 main.rs 在 dispatch 之前分叉（spawn worker）；到达这里即路由 bug。
```

```rust
        anyhow::bail!("--background forks in main.rs before dispatch; reaching here is a routing bug");
```

`grep -rn "not yet implemented" src tests` 确认无残留引用。

- [ ] **Step 4: 跑测试**：`cargo test` → 全绿。

- [ ] **Step 5: Commit**

```bash
git add src/prompt/templates src/prompt/mod.rs src/cli.rs
git commit -m "feat(prompt): built-in exhaustiveness directive; fix stale --background bail text

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 5: `/breakpoint:lgtm` 命令（批次 2 · skill 创作）+ 双宿主生成 + packaging 防漂移

**Files:**
- Create: `plugins/breakpoint/commands/lgtm.md`
- Modify: `tests/packaging.rs`（三处 name 清单 + invocation 策略）
- Generate: `plugins/breakpoint/codex-skills/lgtm/SKILL.md`（`make gen` 产出，提交）

**Interfaces:**
- Consumes: Task 1-4 交付的 `brk init`/`--dismissed`/新 id/穷尽性模板。
- Produces: `/breakpoint:lgtm` 命令；packaging 断言集扩至 4 命令。

> 本任务按 spec §2.0 属 **skill 创作**：执行者以 `superpowers:writing-skills` 的质量标准打磨措辞与结构，但**协议语义以下文基线为准**（来源 spec §2.1-2.4，不得增删停机条件/裁决规则）。

- [ ] **Step 1: 先更新 packaging 测试（失败先行）**——`tests/packaging.rs`：
  - `commands_frontmatter_and_invocation_policy`：首行循环 `["review", "status", "result"]` → `["review", "status", "result", "lgtm"]`；「禁模型自动调用」断言从单独 review 改为循环 `["review", "lgtm"]`（lgtm 是烧钱长循环，同 review 政策）；「允许模型触发」循环维持 `["status", "result"]`。
  - `codex_skills_have_name_and_no_claude_var`：循环 `["review", "status", "result"]` → `["review", "status", "result", "lgtm"]`。

Run: `cargo test --test packaging` → Expected: FAIL（lgtm.md 不存在）

- [ ] **Step 2: 写 `plugins/breakpoint/commands/lgtm.md`**（基线全文；打磨措辞可以，语义不可动）

````markdown
---
description: Loop adversarial review until verdict=approve — adjudicate, fix, dismiss, re-review; with dismissed ledger and escalation brakes
disable-model-invocation: true
---

# lgtm — 循环对抗评审直到 approve

驱动「评审 → 裁决 → 修复/驳回 → 再评审」循环直到 `verdict = approve`。评审器：
`${CLAUDE_PLUGIN_ROOT}/bin/brk`。

用户参数：$ARGUMENTS
- `--max-rounds <N>`（本命令专属，可选）：预算护栏；未给则**无轮数上限**——目标是 approve。
- `--dismissed-dir <path>`（本命令专属，可选）：驳回台账目录，默认 `.breakpoint/dismissed/`。
- 其余参数**原样透传**给 `brk review`（--mode/--base/--scope/--spec/--lenses/--models 等）。
  透传前先从参数串中摘除上述两个专属参数。

## 0. 起手
1. 运行 `${CLAUDE_PLUGIN_ROOT}/bin/brk init`（幂等）；创建本会话目录
   `.breakpoint/rounds/<YYYYMMDD-HHmmss>/`（本地时间，秒级）。
2. code 模式：先跑项目测试命令，红树即停（先修好基线，不拿坏基线喂评审）；doc 模式跳过。
   **测试命令探测**（本会话缓存结论）：`.breakpoint/config.toml` 的 `test_command` >
   `Cargo.toml`→`cargo test` > `package.json` 有 `scripts.test`→`npm test` >
   `Makefile` 有 test 目标→`make test` > 项目 CLAUDE.md/记忆约定 > 都推断不出 →
   AskUserQuestion 一次并把答案记入 session.md；明确无测试的项目跳过测试步。

## 每轮 rN
1. **组装 --focus**：读 `<dismissed-dir>/*.md`，用各文件的 title + 驳回理由生成摘要：
   「以下问题已人工裁决为误报，勿再报：…」。台账为空则不加 --focus。
2. **起评审**：`${CLAUDE_PLUGIN_ROOT}/bin/brk review --background --dismissed <dismissed-dir> [--focus <摘要>] <透传参数>`
   → 记下 job id。轮询 `${CLAUDE_PLUGIN_ROOT}/bin/brk status <job>`（间隔约 20-30s）：
   - `last_active` 持续前进 = 活着（慢 ≠ 死）；`phase` 走到 `finalizing` 属正常尾段。
   - 心跳停滞超过 ~5 分钟 → `${CLAUDE_PLUGIN_ROOT}/bin/brk cancel <job>` 后重起本轮。
   - **轮询期间不改动工作树**（working-tree scope 下评审正在读活文件）。
3. **取结果**：`${CLAUDE_PLUGIN_ROOT}/bin/brk result <job>` → 存档到
   `rounds/<session>/rN.result.json`。
   - job `failed/timed_out` 但有部分报告（`verdict: incomplete`）→ 用部分 findings
     照常裁决，简报注记 partial，**不算 approve**。
   - job 硬失败无产出（全 provider 挂等）→ 重试一次，再失败 → 停，报告用户。
     基础设施故障不消耗轮次、不触发停滞守卫。

## 内循环（逐条裁决）
1. **分拣**：`verify == refuted` 跳过（brk 已豁免，简报记一笔）。`previously_dismissed`
   按台账状态分流：`human-confirmed` 或已 `re-upheld` 的静默跳过；仅 `dismissed(rN)` 状态
   的**首次再现**进入刹车链（每 id 至多一次）。
2. **排序**：severity 降序为主序，同级按 file 分组。
3. **逐条**（读 quote + 文件上下文 + body 后裁决）：
   - **属实 → 修**：root-cause 修法——共享函数一处守卫优于逐调用点补丁。
   - **误报 → 写 `<dismissed-dir>/<id>.md`**：内容 = finding 快照（title/file/quote/
     severity）+ 驳回理由 + 状态行 `dismissed(rN, <日期>)`。
   - **存疑 → 先验证再定，禁止直接猜**：能执行就执行（微测试/复现脚本）。
     **绕过类/输入处理类强制走 fuzz 配方**：① 从 finding 提取被绕过的校验点与输入空间
     维度（编码层数/大小写/分隔符/scheme…）；② 对维度组合写 property 测试（内联枚举
     生成即可，不引新依赖）；③ 跑测试枚举出全部失败变体，一次定性整类；④ 修复走类级
     （normalizer/单点归一），逐 case 补丁视为未完成；⑤ property 测试留在项目测试套件
     作回归资产。验证后仍不可判 → 攒到轮末批量 AskUserQuestion，不逐条打断。
   - 先验：`agreement ≥ 2` 加强属实先验；upheld 孤证正常裁决（verify 是意见不是判决）。

## 收轮
修复全落 → 跑测试命令至绿 → **单次 commit（仅修复，不含 .breakpoint/）**，消息
`fix(lgtm): rN <一句话摘要>` → 写 `rounds/<session>/rN.md` 简报（job id、verdict、
修复/驳回/存疑清单、commit sha）→ 下一轮。

## 停机（每轮内循环完成后判定；即使 verdict=approve，待刹车 id 先走完刹车链)
1. **verdict = approve** → 收工：写 `rounds/<session>/session.md` 总结（轮数、修复/
   驳回统计、遗留观察），向用户报告 LGTM。
2. **升级刹车链**（每 id 至多一次）：`run.dismissed_reappeared` 中处于 `dismissed` 状态
   的 id → 对照台账里当时的驳回理由，带新 finding body **复裁一次**：改判属实 → 修 +
   删台账文件；仍判误报 → 台账追加 `re-upheld(rM)`，**停，AskUserQuestion 交人裁决**。
   人裁误报 → 追加 `human-confirmed(rK)`，此后该 id 永久静默；人裁属实 → 修 + 删文件，继续。
3. **修复受阻**：某 finding 属实但你修不了（需产品/架构决策、改动越权）→ 停，交用户。
4. **停滞守卫**：本轮仍有未豁免 findings 但零代码改动（未修也未新驳）→ 空转，停，交用户。
5. `--max-rounds` 达限（仅用户显式给出时）→ 停，输出未收敛摘要。
````

- [ ] **Step 3: 生成 + 验证**

```bash
make gen && cargo test --test packaging && make verify
```

Expected: 全绿；`plugins/breakpoint/codex-skills/lgtm/SKILL.md` 生成（`${CODEX_PLUGIN_ROOT}` 已替换）。

- [ ] **Step 4: Commit**

```bash
git add plugins/breakpoint/commands/lgtm.md plugins/breakpoint/codex-skills/lgtm tests/packaging.rs
git commit -m "feat(lgtm): /breakpoint:lgtm loop command — converge to approve with dismissed ledger and brakes

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 6: 验收（live dogfood，交互执行）

**Files:** 无代码改动；产出 `.breakpoint/rounds/<session>/` 工件与验收记录。

前置：Task 1-5 全绿；`make build` 重编二进制进插件。三条路径（spec §4）：

- [ ] **(a) 收敛路径**：在临时分支埋 ≥3 个已知缺陷（至少 1 个绕过类，如手写逐 case 的输入校验漏变体），跑 `/breakpoint:lgtm --base <分支基点>`，观察至 approve。检查：`rounds/<session>/` 含 rN.md + rN.result.json + session.md；commit 序列纯修复；预期 ≤3 轮（观察参考）。
- [ ] **(b) 刹车路径**：预埋一条 `dismissed(r1)` 状态的误报台账文件并使其再现（quote/title 不变），验证：复裁 → `re-upheld` → AskUser 刹车 → 人裁后正确续跑或收编。
- [ ] **(c) self-hosting 靶**：对 `feat/loop-lgtm` 分支自身跑 `/breakpoint:lgtm --base dev`，兼任批次 1 终审补充。
- [ ] **端到端本证（live 版，补单元本证的真模型链路）**：真实 review 产出一条 finding → 手建 `dismissed/<id>.md` → 在 quote 上方插空行制造位移 → 重跑 `brk review --dismissed .breakpoint/dismissed/` → 断言 result.json 中同 id 再现、`previously_dismissed: true`、verdict 不受其影响、`run.dismissed_reappeared` 含该 id。
- [ ] 收尾：更新 `.doc/roadmap.md`（M7 状态、R8.2 措辞修正）；`finishing-a-development-branch` 流程收分支。

---

## Self-Review 记录

- **Spec 覆盖**：§1.1→T1；§1.2→T2(解析)+T3(契约)；§1.3→T4；§1.4→T2；§2.x→T5；§4→各任务测试+T6。无缺口。`.breakpoint/` 的 secrets non-goal 无需任务（不做即达成）。
- **占位符扫描**：无 TBD/TODO；T5 基线全文内嵌；T6 属交互 dogfood 清单（非代码步）。
- **类型一致性**：`parse_dismissed -> anyhow::Result<HashSet<String>>`（T2 定义、T3 消费一致）；`mark_dismissed(&mut [Finding], &HashSet<String>) -> Vec<String>`（T3 内定义与测试一致）；`previously_dismissed`/`dismissed_reappeared` 命名各处一致；enrich 签名不变。
- 注意点（执行时留意）：Finding 字面量补字段是全仓机械改（编译器指引）；schema_version 断言更新需 grep 兜底。
