# S2-2b code 文件包流水线 + positioning Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 S2-2a 的工具循环引擎接成 code 评审的默认路径——按文件包并发评审、确定性 quote→file:line 锚定、编造引用即丢弃——取代 Phase-1 的「单发 lens×model + 甩文件清单」。

**Architecture:** code 模式新增独立流水线 `run_code_review`：确定性文件筛选 → 轻量分包（目录邻近 + 同名词干）→ `bundle × model` 扇出（每包一次工具循环，所有 lens 降级为注入 rule）→ positioning（quote 确定性定位到行，找不到即丢弃）→ 复用 dedup/clamp/verdict/output。doc 模式维持 Phase-1 的 lens×model 单发路径不变（留到 S4）。引擎层（`ToolExecutor`/`run_tool_loop`/`backends`）冻结，只消费不改。

**Tech Stack:** Rust 2021；复用 `ToolLoopProvider`/`backend_from_config`/`FakeChatBackend`（S2-2a）；`tokio::sync::Semaphore`（新增 `sync` feature）做 `max_concurrency` 限流；`futures::future::join_all`；positioning 用 `std::fs` 受 root 约束读取（不 shell、不解析语言）。

## Global Constraints

逐条来自设计 §19 / 路线图 / S2-2a 收尾约定，**每个 task 的要求隐含包含本节**：

- **rustls-only，零 OpenSSL**：不得引入会传递拉入 `openssl-sys`/`native-tls` 的依赖；完成后 `cargo tree -i openssl-sys` 必须为空。本阶段只新增 `tokio` 的 `sync` feature（纯 Rust）。
- **引擎冻结**：不得修改 `src/tools/mod.rs`、`src/provider/tool_loop.rs`、`src/provider/backends.rs` 的核心逻辑（只 import/调用）。
- **doc 路径不回归**：Phase-1 的 doc e2e（`tests/review_e2e.rs::doc_review_end_to_end_rejects`）与全部既有测试必须保持绿。doc 维持 lens×model 单发，本阶段不碰 doc 行为。
- **positioning 在 dedup 之前**：准行号反哺 line-overlap 去重（§19.6 prose）。
- **severity 双保险（§7.4）不变**：bundle prompt 指示 compliance rule 封顶 medium + `clamp_compliance` 仍按 finding 的 lens tag 钳制。模型给每条 finding 打 `lens`（rule id）。
- **root 约束**：positioning 读取受 root 限制——拒绝绝对路径与 `..`；读不到/二进制/越权 → 丢弃该 finding（与「定位失败丢弃」一致）。
- **不 shell 搜索/读取**：positioning 用 `std::fs`。git diff 收集沿用 evidence 既有 `Command::new("git")` 模式（仅 code 证据收集，非引擎搜索）。
- **字符串英文，注释中文**：所有 prompt 文本、用户可见错误、模板用英文；代码注释保持中文。
- **分包启发式 = 轻量**：目录邻近 + 同名词干（stem）；**不做** import 关系分析（延后）。
- **定位失败 = 丢弃**：quote 经空白归一化 + 子串匹配仍找不到 → 丢弃（确定性幻觉过滤）。
- **复用而非重写**：`enrich`、`dedup`、`clamp_compliance`、`compute_verdict`、`Stats`、`output::render`、`select`、`output_language`、`call_with_retry`、`parse_lens_output`、`ToolLoopProvider`、`backend_from_config`、`FakeChatBackend` 全部复用。
- **TDD + 频繁提交**：每 task 红→绿→提交。测试 hermetic（无网络；单测不依赖真实 git——`FileChange` 直接注入；e2e 用 tempdir + `FakeChatBackend` + 注入 changes）。
- **退出码约定不变**：0=approve / 10=reject / 2=对照集不完整(failed_models 非空) 或错误。

### 携带项（S2-2a carry-forward，本阶段顺带或显式延后）
- `max_concurrency` 信号量：**本阶段 Task 7 实现**（路线图 §4 映射：分包扇出引入真并发处限流）。
- search per-file cap 微优化 / `Searcher` 每文件重建 / dup 常量抽 helper / `parse` 缺 id/name 默认空串：**非阻断，继续延后**（引擎冻结，不在本阶段动）。

---

## File Structure

| 文件 | 责任 | 动作 |
|---|---|---|
| `src/domain/finding.rs` | `RawFinding`/`Finding` 加 `quote`（+ `RawFinding.lens` tag） | 改 |
| `src/evidence/git.rs` | `FileChange` 类型 + `changed_files()` 逐文件 diff | 改 |
| `src/bundle/mod.rs` | 确定性文件筛选 `select_files` + 轻量分包 `group` | 新建 |
| `src/positioning/mod.rs` | `quote → file:line` 确定性锚定，失败丢弃 | 新建 |
| `src/prompt/mod.rs` | `build_bundle_prompt`（rule 注入 + bundle diff） | 改 |
| `src/prompt/templates/adversarial-code-bundle.md` | bundle 对抗模板（rules + tools + quote 契约） | 新建 |
| `src/orchestrate/mod.rs` | `enrich` 带 quote/lens；`run_code_fanout`（bundle×model + 信号量） | 改 |
| `src/cli.rs` | `run_code_review`（code 专属流水线） | 改 |
| `src/main.rs` | 按 mode 选 provider：code→`ToolLoopProvider`，doc→`DirectProvider` | 改 |
| `src/lib.rs` | `pub mod bundle; pub mod positioning;` | 改 |
| `Cargo.toml` | `tokio` 加 `sync` feature | 改 |
| `tests/review_e2e.rs` | code bundle 工具循环 e2e（锚定 + 丢编造） | 改 |

---

## Task 1: schema —— finding 加 `quote` + `lens` tag

**Files:**
- Modify: `src/domain/finding.rs`（`RawFinding`、`Finding`）
- Modify: `src/orchestrate/mod.rs:31-54`（`enrich`）
- Modify（测试 literal 补字段）: `src/provider/fake.rs`、`src/orchestrate/mod.rs` tests、`src/domain/report.rs` tests、`src/consolidate/dedup.rs` tests、`src/consolidate/correlate.rs` tests、`src/verdict.rs` tests、`src/output/mod.rs` tests、`tests/review_e2e.rs`

**Interfaces:**
- Produces:
  - `RawFinding` 新增 `#[serde(default)] pub quote: String` 与 `#[serde(default)] pub lens: Option<String>`
  - `Finding` 新增 `pub quote: String`（序列化）
  - `enrich(raw: &RawFinding, fallback_lens: &str, model: &str) -> Finding`：`lenses = vec![raw.lens.as_deref().unwrap_or(fallback_lens).to_string()]`，`quote = raw.quote.clone()`
- Consumes: 无（基础类型）

> 说明：`quote` 用 `#[serde(default)]` 让缺字段也能解析（positioning 会丢弃空 quote），prompt 文本把 quote 标为必填来约束模型——我们的契约走 prompt 文本，不靠 JsonSchema 强约束。`lens` 让模型给每条 finding 打 rule id，`enrich` 据此保住 §7.4 钳制；doc 路径模型不打 tag → `None` → 用 `fallback_lens`（即 doc lens id），行为不变。

- [ ] **Step 1: 写失败测试（finding.rs）**

在 `src/domain/finding.rs` 的 `mod tests` 末尾追加：

```rust
    #[test]
    fn raw_finding_deserializes_quote_and_lens() {
        let j = r#"{"severity":"high","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":2,"confidence":0.9,"recommendation":"r","quote":"let x = secret();","lens":"L1"}"#;
        let r: RawFinding = serde_json::from_str(j).unwrap();
        assert_eq!(r.quote, "let x = secret();");
        assert_eq!(r.lens.as_deref(), Some("L1"));
    }

    #[test]
    fn raw_finding_quote_defaults_empty_when_absent() {
        let j = r#"{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r"}"#;
        let r: RawFinding = serde_json::from_str(j).unwrap();
        assert_eq!(r.quote, "");
        assert_eq!(r.lens, None);
    }

    #[test]
    fn finding_serializes_quote() {
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec![], models: vec![], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "snippet".into(),
        };
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["quote"], "snippet");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoints --lib domain::finding 2>&1 | tail -20`
Expected: 编译失败（`Finding`/`RawFinding` 无 `quote`/`lens` 字段）

- [ ] **Step 3: 加字段**

`src/domain/finding.rs`：`RawFinding` 在 `hard_constraint` 之后加：

```rust
    #[serde(default)]
    pub hard_constraint: bool,
    /// 逐字引用出问题的代码原文（positioning 用；prompt 契约要求必填）。
    #[serde(default)]
    pub quote: String,
    /// 模型给本条 finding 打的 rule/lens id（保住 §7.4 severity 钳制）。
    #[serde(default)]
    pub lens: Option<String>,
```

`Finding` 在 `cluster_id` 之前加：

```rust
    /// 逐字引用（positioning 锚定依据，序列化进输出）。
    pub quote: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cluster_id: Option<String>,
```

- [ ] **Step 4: 改 `enrich` 带 quote/lens**

`src/orchestrate/mod.rs` 的 `enrich`：签名第二参改名 `fallback_lens`，函数体：

```rust
pub fn enrich(raw: &RawFinding, fallback_lens: &str, model: &str) -> Finding {
    let id = fnv1a64_hex(&[
        &raw.file,
        &raw.line_start.to_string(),
        &raw.line_end.to_string(),
        &raw.title.to_lowercase(),
    ]);
    let lens = raw.lens.as_deref().unwrap_or(fallback_lens).to_string();
    Finding {
        id,
        severity: raw.severity,
        title: raw.title.clone(),
        body: raw.body.clone(),
        file: raw.file.clone(),
        line_start: raw.line_start,
        line_end: raw.line_end,
        confidence: raw.confidence,
        recommendation: raw.recommendation.clone(),
        lenses: vec![lens],
        models: vec![model.to_string()],
        agreement: 1,
        hard_constraint: raw.hard_constraint,
        cluster_id: None,
        quote: raw.quote.clone(),
    }
}
```

> `run_one_lens` 现有调用 `enrich(r, lens.id, &pm.name)` 不用改——`lens.id` 现作 `fallback_lens`，doc/旧路径 `raw.lens=None` → 用 `lens.id`，行为等价。

- [ ] **Step 5: 补全所有 literal**

每处 `RawFinding { ... }` literal 补 `quote: "...".into(), lens: None,`（测试里给非空 quote 便于后续复用；空亦可）：
- `src/provider/fake.rs:30` 的 `raw()` helper
- `src/orchestrate/mod.rs` tests 的 `raw()` helper
- `tests/review_e2e.rs:6` 的 `raw()` helper

每处 `Finding { ... }` literal 补 `quote: "snippet".into(),`（放在 `cluster_id` 之前）：
- `src/domain/report.rs` tests 的 `f()`
- `src/consolidate/dedup.rs` tests 的 `f()`
- `src/consolidate/correlate.rs` tests 的 `f()`
- `src/verdict.rs` tests 的 `f()`
- `src/output/mod.rs` tests 的 `report()` 里的 `Finding`

> 提示：`rg -n "hard_constraint: false" src tests` 与 `rg -n "RawFinding \{" src tests` 找全所有 literal。逐个补到编译通过。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p breakpoints --lib 2>&1 | tail -20`
Expected: 全绿（含新 3 测试）

- [ ] **Step 7: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(domain): add quote + lens tag to findings for positioning"
```

---

## Task 2: evidence —— `FileChange` 逐文件 diff 收集

**Files:**
- Modify: `src/evidence/git.rs`
- Test: `src/evidence/git.rs`（pure helper 单测；git-shelling 路径不单测，与 Phase-1 `collect_code` 一致）

**Interfaces:**
- Produces:
  - `pub struct FileChange { pub path: String, pub diff: String }`（`#[derive(Clone, Debug, PartialEq)]`）
  - `pub fn changed_files(base: Option<&str>, branch_scope: bool) -> anyhow::Result<Vec<FileChange>>`
- Consumes: 既有 `git()` 私有 helper、`dedup_files()`

- [ ] **Step 1: 写失败测试（pure helper）**

`changed_files` 内部把「文件列表 + 逐文件 diff」组装成 `Vec<FileChange>`。抽一个纯函数 `assemble_changes(files: &[String], per_file: impl Fn(&str) -> String) -> Vec<FileChange>` 便于 hermetic 测。在 `src/evidence/git.rs` 的 `mod tests` 追加：

```rust
    #[test]
    fn assemble_changes_pairs_files_with_diffs() {
        let files = vec!["a.rs".to_string(), "b.rs".to_string()];
        let out = assemble_changes(&files, |p| format!("diff for {p}"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], FileChange { path: "a.rs".into(), diff: "diff for a.rs".into() });
        assert_eq!(out[1].path, "b.rs");
    }

    #[test]
    fn assemble_changes_skips_empty_diffs() {
        let files = vec!["a.rs".to_string(), "empty.rs".to_string()];
        let out = assemble_changes(&files, |p| if p == "empty.rs" { String::new() } else { "x".into() });
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "a.rs");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoints --lib evidence::git 2>&1 | tail -20`
Expected: 编译失败（`FileChange`/`assemble_changes` 未定义）

- [ ] **Step 3: 实现**

`src/evidence/git.rs` 顶部（`use` 之后）加类型与纯函数，并加 `changed_files`：

```rust
#[derive(Clone, Debug, PartialEq)]
pub struct FileChange {
    pub path: String,
    pub diff: String,
}

/// 把文件列表与「逐文件取 diff」的闭包组装成 FileChange；跳过空 diff（纯模式变更等）。
fn assemble_changes(files: &[String], per_file: impl Fn(&str) -> String) -> Vec<FileChange> {
    files
        .iter()
        .filter_map(|p| {
            let diff = per_file(p);
            if diff.trim().is_empty() {
                None
            } else {
                Some(FileChange { path: p.clone(), diff })
            }
        })
        .collect()
}

/// 收集逐文件 diff：branch scope = base...HEAD；working-tree = cached + worktree。
/// 仅 main 调用（shell git）；单测注入 FileChange，不走此函数。
pub fn changed_files(base: Option<&str>, branch_scope: bool) -> anyhow::Result<Vec<FileChange>> {
    if branch_scope {
        let base = base.ok_or_else(|| anyhow::anyhow!("branch scope requires --base"))?;
        let merge_base = git(&["merge-base", base, "HEAD"])?.trim().to_string();
        let range = format!("{merge_base}...HEAD");
        let files = dedup_files(&git(&["diff", "--name-only", &range])?);
        Ok(assemble_changes(&files, |p| {
            git(&["diff", &range, "--", p]).unwrap_or_default()
        }))
    } else {
        let mut raw = git(&["diff", "--cached", "--name-only"])?;
        raw.push_str(&git(&["diff", "--name-only"])?);
        let files = dedup_files(&raw);
        Ok(assemble_changes(&files, |p| {
            let mut d = git(&["diff", "--cached", "--", p]).unwrap_or_default();
            d.push_str(&git(&["diff", "--", p]).unwrap_or_default());
            d
        }))
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p breakpoints --lib evidence::git 2>&1 | tail -20`
Expected: 全绿

- [ ] **Step 5: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(evidence): per-file FileChange collection for code bundling"
```

---

## Task 3: bundle —— 确定性文件筛选 `select_files`

**Files:**
- Create: `src/bundle/mod.rs`
- Modify: `src/lib.rs`（加 `pub mod bundle;`）

**Interfaces:**
- Produces: `pub fn select_files(changes: Vec<FileChange>) -> Vec<FileChange>`
- Consumes: `crate::evidence::git::FileChange`

- [ ] **Step 1: lib.rs 注册模块**

`src/lib.rs` 按字母序加（在 `pub mod consolidate;` 之前的合适位置，紧跟既有 `pub mod` 列表的字母序）：

```rust
pub mod bundle;
```

- [ ] **Step 2: 写失败测试**

新建 `src/bundle/mod.rs`：

```rust
use crate::evidence::git::FileChange;

#[cfg(test)]
mod tests {
    use super::*;
    fn fc(path: &str, diff: &str) -> FileChange {
        FileChange { path: path.into(), diff: diff.into() }
    }

    #[test]
    fn select_keeps_normal_code_files() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("src/b.rs", "+y")]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn select_drops_lockfiles() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("Cargo.lock", "+dep"), fc("ui/yarn.lock", "+pkg")]);
        let paths: Vec<&str> = out.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs"]);
    }

    #[test]
    fn select_drops_empty_diff() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("src/blank.rs", "   ")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "src/a.rs");
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p breakpoints --lib bundle 2>&1 | tail -20`
Expected: 编译失败（`select_files` 未定义）

- [ ] **Step 4: 实现 `select_files`**

在 `src/bundle/mod.rs` 顶部（`use` 之后、`#[cfg(test)]` 之前）加：

```rust
/// 噪声文件名（锁文件等，评审无价值）。
const NOISE_NAMES: &[&str] = &[
    "Cargo.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
    "poetry.lock", "Gemfile.lock", "composer.lock",
];

fn filename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

fn is_noise(path: &str) -> bool {
    let f = filename(path);
    NOISE_NAMES.contains(&f) || f.ends_with(".lock")
}

/// 确定性文件筛选：丢掉空 diff 与噪声（锁文件）。代码决定，不交模型。
pub fn select_files(changes: Vec<FileChange>) -> Vec<FileChange> {
    changes
        .into_iter()
        .filter(|c| !c.diff.trim().is_empty())
        .filter(|c| !is_noise(&c.path))
        .collect()
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p breakpoints --lib bundle 2>&1 | tail -20`
Expected: 全绿

- [ ] **Step 6: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(bundle): deterministic file selection (drop noise + empty diffs)"
```

---

## Task 4: bundle —— 轻量分包 `group`

**Files:**
- Modify: `src/bundle/mod.rs`

**Interfaces:**
- Produces:
  - `pub struct Bundle { pub files: Vec<FileChange> }`（`#[derive(Clone, Debug)]`）
  - `pub const BUNDLE_MAX_FILES: usize = 8`
  - `pub fn group(changes: Vec<FileChange>) -> Vec<Bundle>`
- Consumes: `FileChange`

> 轻量启发式：先按 path 排序保证确定性；并查集 union 条件 = 同父目录 **或** 同词干(stem)；每组超过 `BUNDLE_MAX_FILES` 按排序顺序切块。**不做 import 分析**（延后）。

- [ ] **Step 1: 写失败测试**

`src/bundle/mod.rs` 的 `mod tests` 内追加：

```rust
    #[test]
    fn group_unions_same_directory() {
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("src/auth/token.rs", "+b")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].files.len(), 2);
    }

    #[test]
    fn group_unions_same_stem_across_dirs() {
        // widget.tsx 与 widget.css 同词干 → 同包
        let out = group(vec![fc("ui/widget.tsx", "+a"), fc("styles/widget.css", "+b")]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn group_separates_unrelated() {
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("docs/readme.md", "+b")]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn group_splits_oversized_at_cap() {
        // 同目录 10 文件 → 切成 8 + 2
        let changes: Vec<FileChange> = (0..10)
            .map(|i| fc(&format!("src/m/f{i:02}.rs"), "+x"))
            .collect();
        let out = group(changes);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|b| b.files.len() <= BUNDLE_MAX_FILES));
        assert_eq!(out[0].files.len() + out[1].files.len(), 10);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoints --lib bundle 2>&1 | tail -20`
Expected: 编译失败（`Bundle`/`group`/`BUNDLE_MAX_FILES` 未定义）

- [ ] **Step 3: 实现 `group`**

在 `src/bundle/mod.rs`（`select_files` 之后、`#[cfg(test)]` 之前）加：

```rust
use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Bundle {
    pub files: Vec<FileChange>,
}

/// 单个文件包的文件数上限（超出按排序切块）。
pub const BUNDLE_MAX_FILES: usize = 8;

fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// 词干：文件名去掉首个 '.' 之后部分（widget.test.ts 与 widget.ts 同词干 "widget"）。
fn stem(path: &str) -> &str {
    let f = filename(path);
    match f.find('.') {
        Some(i) => &f[..i],
        None => f,
    }
}

fn uf_find(parent: &mut [usize], x: usize) -> usize {
    let mut root = x;
    while parent[root] != root {
        root = parent[root];
    }
    // 路径压缩
    let mut cur = x;
    while parent[cur] != cur {
        let next = parent[cur];
        parent[cur] = root;
        cur = next;
    }
    root
}

/// 轻量分包：同父目录或同词干并到一包；超 cap 按排序切块。确定性（先排序）。
pub fn group(mut changes: Vec<FileChange>) -> Vec<Bundle> {
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let n = changes.len();
    if n == 0 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            let same_dir = parent_dir(&changes[i].path) == parent_dir(&changes[j].path);
            let same_stem = stem(&changes[i].path) == stem(&changes[j].path);
            if same_dir || same_stem {
                let ri = uf_find(&mut parent, i);
                let rj = uf_find(&mut parent, j);
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    // 按代表元聚组（成员索引升序，因已排序）
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = uf_find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut bundles = Vec::new();
    for (_root, idxs) in groups {
        for chunk in idxs.chunks(BUNDLE_MAX_FILES) {
            let files = chunk.iter().map(|&k| changes[k].clone()).collect();
            bundles.push(Bundle { files });
        }
    }
    bundles
}
```

> 把 `mod tests` 顶部已有的 `use super::*;` 保留即可（`Bundle`/`group`/`BUNDLE_MAX_FILES`/`fc` 都可见）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p breakpoints --lib bundle 2>&1 | tail -20`
Expected: 全绿

- [ ] **Step 5: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(bundle): lightweight file bundling (dir-proximity + same-stem, capped)"
```

---

## Task 5: positioning —— `quote → file:line` 确定性锚定

**Files:**
- Create: `src/positioning/mod.rs`
- Modify: `src/lib.rs`（加 `pub mod positioning;`）

**Interfaces:**
- Produces: `pub fn anchor(findings: Vec<Finding>, root: &std::path::Path) -> Vec<Finding>`
- Consumes: `crate::domain::Finding`

> 算法：逐 finding 在 `root.join(file)` 读源（受 root 约束，拒绝绝对/`..`）。把源行与 quote 各行**空白归一化**；取 quote 首个非空行，找首个**包含**它的源行 → 命中行即 `line_start`，`line_end = line_start + quote_行数 - 1`（钳到文件长度）。读不到/越权/二进制/空 quote/找不到 → **丢弃**该 finding。

- [ ] **Step 1: lib.rs 注册模块**

`src/lib.rs` 按字母序加：

```rust
pub mod positioning;
```

- [ ] **Step 2: 写失败测试**

新建 `src/positioning/mod.rs`：

```rust
use crate::domain::Finding;
use std::path::{Component, Path};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    use std::io::Write;

    fn finding(file: &str, quote: &str) -> Finding {
        Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: file.into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec!["L1".into()], models: vec!["m".into()],
            agreement: 1, hard_constraint: false, cluster_id: None, quote: quote.into(),
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn anchors_exact_quote_to_correct_line() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "fn main() {}\nlet token = secret();\nok();\n");
        let out = anchor(vec![finding("a.rs", "let token = secret();")], dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
        assert_eq!(out[0].line_end, 2);
    }

    #[test]
    fn anchors_despite_whitespace_differences() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "x\n    db.write(token);\n");
        // quote 多了内部空格
        let out = anchor(vec![finding("a.rs", "db.write(token);")], dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
    }

    #[test]
    fn drops_fabricated_quote() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "real line\n");
        let out = anchor(vec![finding("a.rs", "this text is not in the file")], dir.path());
        assert!(out.is_empty());
    }

    #[test]
    fn drops_on_path_traversal_or_missing() {
        let dir = tempfile::tempdir().unwrap();
        let out = anchor(
            vec![finding("../escape", "x"), finding("/etc/hosts", "x"), finding("nope.rs", "x")],
            dir.path(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn multiline_quote_sets_range() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "l1\nif x {\n    bad();\n}\n");
        let out = anchor(vec![finding("a.rs", "if x {\n    bad();\n}")], dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
        assert_eq!(out[0].line_end, 4);
    }
}
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p breakpoints --lib positioning 2>&1 | tail -20`
Expected: 编译失败（`anchor` 未定义）

- [ ] **Step 4: 实现 `anchor`**

在 `src/positioning/mod.rs`（`use` 之后、`#[cfg(test)]` 之前）加：

```rust
const POS_READ_MAX_BYTES: usize = 512 * 1024;

/// 空白归一化：所有空白折叠为单空格并去首尾。
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 受 root 约束读取：拒绝绝对路径与 `..`；二进制/超大/不存在 → None。
fn safe_read(root: &Path, rel: &str) -> Option<String> {
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let data = std::fs::read(root.join(p)).ok()?;
    let slice = if data.len() > POS_READ_MAX_BYTES { &data[..POS_READ_MAX_BYTES] } else { &data[..] };
    String::from_utf8(slice.to_vec()).ok()
}

/// 把 quote 确定性定位到真实行；定位失败即丢弃（确定性幻觉过滤）。
pub fn anchor(findings: Vec<Finding>, root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    for mut f in findings {
        let content = match safe_read(root, &f.file) {
            Some(c) => c,
            None => continue,
        };
        let file_lines: Vec<String> = content.lines().map(normalize_ws).collect();
        let quote_lines: Vec<String> = f
            .quote
            .lines()
            .map(normalize_ws)
            .filter(|l| !l.is_empty())
            .collect();
        let first = match quote_lines.first() {
            Some(q) => q,
            None => continue, // 空 quote → 丢弃
        };
        match file_lines.iter().position(|fl| fl.contains(first.as_str())) {
            Some(i) => {
                let ls = (i as u32) + 1;
                let span = quote_lines.len() as u32; // ≥1（first 存在）
                let le = (ls + span - 1).min(file_lines.len() as u32).max(ls);
                f.line_start = ls;
                f.line_end = le;
                out.push(f);
            }
            None => continue, // 编造 → 丢弃
        }
    }
    out
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p breakpoints --lib positioning 2>&1 | tail -20`
Expected: 全绿（5 测试）

- [ ] **Step 6: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(positioning): deterministic quote->line anchoring, drop fabricated"
```

---

## Task 6: prompt —— `build_bundle_prompt` + bundle 模板

**Files:**
- Create: `src/prompt/templates/adversarial-code-bundle.md`
- Modify: `src/prompt/mod.rs`

**Interfaces:**
- Produces: `pub fn build_bundle_prompt(target_label: &str, bundle: &crate::bundle::Bundle, rules: &[&crate::lens::Lens], user_focus: &str) -> String`
- Consumes: `crate::bundle::Bundle`、`crate::lens::Lens`、既有 `output_language()`

- [ ] **Step 1: 建模板文件**

新建 `src/prompt/templates/adversarial-code-bundle.md`：

```
<role>
You are performing an ADVERSARIAL software review of a bundle of related changed files.
Your job is to break confidence in the change, not to validate it.
</role>

<rules>
Attack this change through every applicable rule below. Tag each finding with the id of the rule it belongs to.
{{RULES}}
</rules>

<operating_stance>
Default to skepticism. Assume subtle, high-cost, user-visible failure until evidence says otherwise.
Give no credit for good intent, partial fixes, or likely follow-up. Happy-path-only counts as a weakness.
</operating_stance>

<tools>
You have tools to inspect the repository: read_file (read any file under the review root, returned with line numbers), search_repo (regex search, gitignore-aware), and list_files. The diffs below show only what changed — read the surrounding files, callers, and callees yourself before judging. Do not invent code you have not read.
</tools>

<review_method>
Actively disprove the change: violated invariants, missing guards, unhandled failure paths, assumptions that break under stress.
User focus (weight heavily but still report other material issues): {{USER_FOCUS}}
</review_method>

<finding_bar>
Report only material findings. No style/naming/low-value cleanup, no evidence-free speculation.
Each finding answers: what breaks / why vulnerable / impact / concrete fix.
</finding_bar>

<output_language>
Write every finding's title, body, and recommendation in the language the reviewed material explicitly requires if it states one; otherwise default to {{OUTPUT_LANGUAGE}}.
</output_language>

<output_contract>
Return ONLY JSON matching the provided schema: an object with a "findings" array.
Each finding has: severity (low|medium|high|critical), title, body, file (path relative to the review root), line_start>=1, line_end>=1, confidence 0..1, recommendation, quote, lens, hard_constraint (boolean, optional).
- quote: the EXACT verbatim source text of the problem, copied character-for-character from the file. This is used to locate the finding; a quote that does not appear in the file will be DISCARDED.
- lens: the id of the rule from the rules list above that this finding belongs to (e.g. "L1").
- hard_constraint: set true when severity is critical or a compliance rule designates a hard block; omit or set false otherwise.
If nothing material, return {"findings": []}. Prefer one strong finding over several weak.
Every finding must be grounded in code you actually read — do not invent files, lines, or behavior.
</output_contract>

<bundle>
Target: {{TARGET_LABEL}}
Files in this bundle: {{FILE_LIST}}

Changed hunks:
{{BUNDLE_DIFFS}}
</bundle>
```

- [ ] **Step 2: 写失败测试**

`src/prompt/mod.rs` 的 `mod tests` 追加（注意需要 `use crate::bundle::Bundle;` 与 `crate::evidence::git::FileChange;`）：

```rust
    #[test]
    fn bundle_prompt_interpolates_rules_files_and_contract() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        use crate::lens::by_id;
        let bundle = Bundle {
            files: vec![FileChange { path: "src/auth.rs".into(), diff: "+let t = secret();".into() }],
        };
        let rules = vec![by_id("L1").unwrap(), by_id("L5").unwrap()];
        let p = build_bundle_prompt("working tree", &bundle, &rules, "auth");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("[L1]") && p.contains("[L5]"), "rule ids injected");
        assert!(p.contains("src/auth.rs"), "file path injected");
        assert!(p.contains("let t = secret();"), "diff injected");
        assert!(p.contains("quote"), "quote contract present");
        assert!(p.contains("auth"), "user focus injected");
        // L5 是 compliance rule → 注入 severity 封顶提示
        assert!(p.to_lowercase().contains("compliance rule"), "compliance cap note present");
    }
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p breakpoints --lib prompt 2>&1 | tail -20`
Expected: 编译失败（`build_bundle_prompt` 未定义）

- [ ] **Step 4: 实现 `build_bundle_prompt`**

`src/prompt/mod.rs`：顶部 `const` 区加模板引入，并加 builder。在 `const DOC_TPL` 之后加：

```rust
const CODE_BUNDLE_TPL: &str = include_str!("templates/adversarial-code-bundle.md");
```

在 `build_prompt` 之后加：

```rust
use crate::bundle::Bundle;
use crate::lens::Lens;

/// 构造一个文件包的对抗 prompt：所有适用 rule 注入为关注清单，bundle diff 内联。
pub fn build_bundle_prompt(
    target_label: &str,
    bundle: &Bundle,
    rules: &[&Lens],
    user_focus: &str,
) -> String {
    let rules_block = rules
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
        .join("\n");

    let diffs_block = bundle
        .files
        .iter()
        .map(|fc| format!("### {}\n```diff\n{}\n```", fc.path, fc.diff))
        .collect::<Vec<_>>()
        .join("\n\n");

    let file_list = bundle
        .files
        .iter()
        .map(|fc| fc.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    CODE_BUNDLE_TPL
        .replace("{{TARGET_LABEL}}", target_label)
        .replace("{{RULES}}", &rules_block)
        .replace("{{FILE_LIST}}", &file_list)
        .replace("{{BUNDLE_DIFFS}}", &diffs_block)
        .replace("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus })
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}
```

> `Lens` 已有 `max_severity: Option<Severity>` 字段（见 `src/lens/mod.rs`），`is_some()` 即 compliance rule。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p breakpoints --lib prompt 2>&1 | tail -20`
Expected: 全绿

- [ ] **Step 6: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(prompt): build_bundle_prompt with rule injection + bundle diffs"
```

---

## Task 7: orchestrate —— `run_code_fanout`（bundle×model + `max_concurrency` 信号量）

**Files:**
- Modify: `Cargo.toml`（`tokio` 加 `sync` feature）
- Modify: `src/orchestrate/mod.rs`

**Interfaces:**
- Produces: `pub async fn run_code_fanout(provider: &dyn ReviewProvider, target_label: &str, bundles: &[Bundle], rules: &[&Lens], focus: &str, opts: &OrchestrateOpts) -> RawResults`
- Consumes: `build_bundle_prompt`、`enrich`、`call_with_retry`、`Bundle`、`Lens`、`OrchestrateOpts`、`RawResults`

> 每个 bundle 一条候补链（复用 `run_one_lens` 的链语义），用 `tokio::sync::Semaphore` 限到 `max_concurrency` 并发。多模型显式矩阵不在此实现——`check_multi_model` 已挡（S3 才放开）。enrich 用 `fallback_lens="code"`；模型给的 `raw.lens` 优先。

- [ ] **Step 1: Cargo.toml 加 tokio sync feature**

`Cargo.toml` 的 tokio 行改为：

```toml
tokio = { version = "1", features = ["rt-multi-thread", "macros", "process", "fs", "time", "sync"] }
```

- [ ] **Step 2: 写失败测试**

`src/orchestrate/mod.rs` 的 `mod tests` 追加（含一个并发探针 provider，断言在飞请求数不超过 `max_concurrency`）：

```rust
    use crate::bundle::Bundle;
    use crate::evidence::git::FileChange;
    use crate::lens::by_id as lens_by_id;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn raw_q(t: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: t.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(), lens: Some("L1".into()),
        }
    }

    fn bundle(path: &str) -> Bundle {
        Bundle { files: vec![FileChange { path: path.into(), diff: "+x".into() }] }
    }

    #[tokio::test]
    async fn code_fanout_runs_every_bundle() {
        let p = FakeProvider::with("m", vec![raw_q("found")]);
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10 }],
            explicit: true, no_fallback: false, max_concurrency: 4, retries: 0,
        };
        let bundles = vec![bundle("src/a.rs"), bundle("src/b.rs"), bundle("src/c.rs")];
        let rules = vec![lens_by_id("L1").unwrap()];
        let r = run_code_fanout(&p, "wt", &bundles, &rules, "", &opts).await;
        assert_eq!(r.findings.len(), 3, "one finding per bundle");
        // raw.lens=Some("L1") → enrich 用 L1（非 fallback）
        assert!(r.findings.iter().all(|f| f.lenses == vec!["L1".to_string()]));
    }

    // 并发探针：断言同时在飞的 provider 调用数不超过 max_concurrency。
    struct ConcProbe {
        inflight: AtomicUsize,
        max_seen: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl ReviewProvider for ConcProbe {
        async fn run(&self, _req: LensRequest) -> Result<crate::provider::LensCall, ProviderError> {
            let cur = self.inflight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_seen.fetch_max(cur, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            Ok(crate::provider::LensCall { findings: vec![], input_tokens: 0, output_tokens: 0 })
        }
    }

    #[tokio::test]
    async fn code_fanout_respects_max_concurrency() {
        let probe = Arc::new(ConcProbe { inflight: AtomicUsize::new(0), max_seen: AtomicUsize::new(0) });
        let opts = OrchestrateOpts {
            models: vec![PlannedModel { name: "m".into(), priority: 10 }],
            explicit: true, no_fallback: false, max_concurrency: 2, retries: 0,
        };
        let bundles: Vec<Bundle> = (0..6).map(|i| bundle(&format!("src/f{i}.rs"))).collect();
        let rules = vec![lens_by_id("L1").unwrap()];
        let probe_ref: &ConcProbe = &probe;
        let _ = run_code_fanout(probe_ref, "wt", &bundles, &rules, "", &opts).await;
        assert!(probe.max_seen.load(Ordering::SeqCst) <= 2, "concurrency exceeded cap: {}", probe.max_seen.load(Ordering::SeqCst));
        assert!(probe.max_seen.load(Ordering::SeqCst) >= 1);
    }
```

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test -p breakpoints --lib orchestrate 2>&1 | tail -20`
Expected: 编译失败（`run_code_fanout` 未定义）

- [ ] **Step 4: 实现 `run_one_bundle` + `run_code_fanout`**

`src/orchestrate/mod.rs`：顶部 `use` 区加：

```rust
use crate::bundle::Bundle;
use crate::lens::Lens;
use crate::prompt::build_bundle_prompt;
use std::sync::Arc;
use tokio::sync::Semaphore;
```

在 `run_fanout` 之后加：

```rust
/// 单个文件包的评审（候补链复用 run_one_lens 的语义）。
async fn run_one_bundle(
    provider: &dyn ReviewProvider,
    target_label: &str,
    bundle: &Bundle,
    rules: &[&Lens],
    focus: &str,
    opts: &OrchestrateOpts,
) -> Result<(Vec<Finding>, (u64, u64)), String> {
    let prompt = build_bundle_prompt(target_label, bundle, rules, focus);

    let mut chain: Vec<&PlannedModel> = opts.models.iter().collect();
    if !opts.explicit {
        chain.sort_by_key(|m| Reverse(m.priority));
    }
    let chain: Vec<&PlannedModel> = if opts.explicit || opts.no_fallback {
        chain.into_iter().take(1).collect()
    } else {
        chain
    };

    let label = bundle.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>().join(",");
    let mut errs: Vec<String> = Vec::new();
    for pm in &chain {
        match call_with_retry(provider, &pm.name, &prompt, opts.retries).await {
            Ok(call) => {
                let fs = call
                    .findings
                    .iter()
                    .map(|r| enrich(r, "code", &pm.name))
                    .collect();
                return Ok((fs, (call.input_tokens, call.output_tokens)));
            }
            Err(e) => errs.push(format!("bundle [{label}] model {}: {e}", pm.name)),
        }
    }
    Err(errs.join("; "))
}

/// code 线扇出：bundle × model（每包一条候补链），用 Semaphore 限 max_concurrency。
pub async fn run_code_fanout(
    provider: &dyn ReviewProvider,
    target_label: &str,
    bundles: &[Bundle],
    rules: &[&Lens],
    focus: &str,
    opts: &OrchestrateOpts,
) -> RawResults {
    let sem = Arc::new(Semaphore::new(opts.max_concurrency.max(1)));
    let futures = bundles.iter().map(|bundle| {
        let sem = sem.clone();
        async move {
            let _permit = sem.acquire().await.expect("semaphore not closed");
            run_one_bundle(provider, target_label, bundle, rules, focus, opts).await
        }
    });
    let results = futures::future::join_all(futures).await;

    let mut out = RawResults::default();
    for res in results {
        match res {
            Ok((fs, (it, ot))) => {
                out.findings.extend(fs);
                out.usage.0 += it;
                out.usage.1 += ot;
            }
            Err(warn) => out.warnings.push(warn),
        }
    }
    out
}
```

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p breakpoints --lib orchestrate 2>&1 | tail -20`
Expected: 全绿（含并发探针）

- [ ] **Step 6: 供应链核验 + clippy + 提交**

```bash
cargo tree -i openssl-sys 2>&1 | head -3   # 必须为空/报 not found
cargo clippy --all-targets -- -D warnings
git add -A && git commit -m "feat(orchestrate): run_code_fanout (bundle x model) with max_concurrency semaphore"
```

---

## Task 8: cli + main —— `run_code_review` 接成 code 默认路径

**Files:**
- Modify: `src/cli.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Produces: `pub async fn run_code_review(args: &ReviewArgs, provider: &dyn ReviewProvider, cfg: &Config, changes: Vec<FileChange>, root: &std::path::Path) -> anyhow::Result<(ReviewReport, i32)>`
- Consumes: `bundle::{select_files, group}`、`select(Mode::Code,..)`、`run_code_fanout`、`positioning::anchor`、`dedup`、`clamp_compliance`、`compute_verdict`、`Stats`、`backend_from_config`、`ToolLoopProvider`、`evidence::git::changed_files`、`evidence::detect_mode`

> 流水线：select_files → group → rules(code lenses) → models → run_code_fanout → **positioning::anchor（dedup 前）** → dedup → clamp → verdict → report。doc 维持 `run_review` 旧路径；main 按 `detect_mode` 选 provider（code→ToolLoopProvider，doc→DirectProvider）。correlate 在 code 线本阶段不接（S3 与 verify 一起做实）。

- [ ] **Step 1: 写失败测试（cli）**

`src/cli.rs` 末尾加 `mod tests`（用 `FakeProvider`，不走工具循环——`FakeProvider` 忽略 prompt 直接吐 findings，positioning 仍对 root 真文件锚定）：

```rust
#[cfg(test)]
mod code_review_tests {
    use super::*;
    use crate::domain::{RawFinding, Verdict};
    use crate::evidence::git::FileChange;
    use crate::provider::fake::FakeProvider;
    use std::io::Write;

    fn write(dir: &std::path::Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    fn args() -> ReviewArgs {
        ReviewArgs {
            mode: Some("code".into()), base: None, scope: "working-tree".into(),
            spec: vec![], focus: None, lenses: Some("L1".into()), models: Some("m".into()),
            no_correlate: true, block_threshold: None, conf_floor: None,
            background: false, format: "json".into(), targets: vec![],
        }
    }

    fn raw(file: &str, quote: &str, sev: crate::domain::Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: "issue".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()),
        }
    }

    #[tokio::test]
    async fn code_review_anchors_real_drops_fabricated() {
        use crate::domain::Severity;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn f() {\n    db.write(token);\n}\n");
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        // 真 quote（锚定到第 2 行）+ 编造 quote（丢弃）
        let provider = FakeProvider::with("m", vec![
            raw("src/a.rs", "db.write(token);", Severity::High),
            raw("src/a.rs", "this is fabricated", Severity::Critical),
        ]);
        let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
        let (report, code) = run_code_review(&args(), &provider, &cfg, changes, dir.path()).await.unwrap();
        assert_eq!(report.findings.len(), 1, "fabricated dropped");
        assert_eq!(report.findings[0].file, "src/a.rs");
        assert_eq!(report.findings[0].line_start, 2);
        assert_eq!(report.verdict, Verdict::Reject); // High ≥ medium block
        assert_eq!(code, 10);
    }

    #[tokio::test]
    async fn code_review_empty_changes_approves() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        let provider = FakeProvider::with("m", vec![]);
        let (report, code) = run_code_review(&args(), &provider, &cfg, vec![], dir.path()).await.unwrap();
        assert!(report.findings.is_empty());
        assert_eq!(code, 0);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p breakpoints --lib code_review 2>&1 | tail -20`
Expected: 编译失败（`run_code_review` 未定义）

- [ ] **Step 3: 实现 `run_code_review`**

`src/cli.rs`：顶部 `use` 区补：

```rust
use crate::bundle::{group, select_files};
use crate::evidence::git::FileChange;
use crate::orchestrate::run_code_fanout;
use crate::positioning::anchor;
use std::path::Path;
```

在 `run_review` 之后加：

```rust
/// code 专属流水线（S2-2b）：bundle × model 工具循环 + positioning 尾段。
/// 退出码同 run_review：0=approve、10=reject、2=对照集不完整。
pub async fn run_code_review(
    args: &ReviewArgs,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    changes: Vec<FileChange>,
    root: &Path,
) -> anyhow::Result<(ReviewReport, i32)> {
    check_multi_model(&args.models)?;
    let t0 = std::time::Instant::now();

    // 1. 确定性筛选 + 分包
    let bundles = group(select_files(changes));

    // 2. rules = code lenses（lens → 注入 rule）
    let lens_ids: Vec<String> = args
        .lenses
        .as_deref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| cfg.review.lenses.code.clone());
    let rules = crate::lens::select(Mode::Code, &lens_ids)?;

    // 3. 模型集（与 run_review 同逻辑）
    let explicit = args.models.is_some();
    let model_names: Vec<String> = match &args.models {
        Some(s) => s.split(',').map(|x| x.trim().to_string()).collect(),
        None if !cfg.review.default_models.is_empty() => cfg.review.default_models.clone(),
        None => {
            let mut v: Vec<_> = cfg.providers.iter().collect();
            v.sort_by_key(|p| std::cmp::Reverse(p.1.priority));
            v.into_iter().map(|(k, _)| k.clone()).collect()
        }
    };
    let models: Vec<PlannedModel> = model_names
        .iter()
        .map(|n| PlannedModel {
            name: n.clone(),
            priority: cfg.providers.get(n).map(|p| p.priority).unwrap_or(0),
        })
        .collect();
    let oopts = OrchestrateOpts {
        models,
        explicit,
        no_fallback: false,
        max_concurrency: cfg.review.max_concurrency,
        retries: 2,
    };

    // 4. fan-out（bundle × model）
    let focus = args.focus.clone().unwrap_or_default();
    let target_label = if args.scope == "branch" {
        format!("branch {}...HEAD", args.base.as_deref().unwrap_or("?"))
    } else {
        "working tree".to_string()
    };
    let raw = run_code_fanout(provider, &target_label, &bundles, &rules, &focus, &oopts).await;

    // 5. positioning（dedup 之前：准行号反哺 line-overlap 去重）
    let anchored = anchor(raw.findings, root);

    // 6. consolidate：dedup → clamp（correlate 留到 S3）
    let mut findings = dedup(anchored);
    clamp_compliance(&mut findings);

    // 7. verdict
    let block = args
        .block_threshold
        .as_deref()
        .map(parse_sev)
        .unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    let verdict = compute_verdict(&findings, block, floor);

    // 8. 组装报告
    let stats = Stats::from_findings(&findings);
    let report = ReviewReport {
        schema_version: "1".into(),
        mode: Mode::Code,
        verdict,
        stats,
        findings,
        clusters: vec![],
        next_steps: vec![],
        run: RunMeta {
            job_id: String::new(),
            models: model_names,
            lenses: lens_ids,
            duration_ms: t0.elapsed().as_millis() as u64,
            usage: Usage {
                input_tokens: raw.usage.0,
                output_tokens: raw.usage.1,
            },
            warnings: raw.warnings,
            failed_models: raw.failed_models,
        },
    };

    let code = match (verdict, !report.run.failed_models.is_empty()) {
        (Verdict::Reject, _) => 10,
        (Verdict::Approve, true) => 2,
        (Verdict::Approve, false) => 0,
    };
    Ok((report, code))
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p breakpoints --lib code_review 2>&1 | tail -20`
Expected: 全绿

- [ ] **Step 5: main 按 mode 选 provider 并路由**

`src/main.rs`：`use` 区补：

```rust
use breakpoints::evidence::{detect_mode, EvidenceOpts, Scope};
use breakpoints::domain::Mode;
use breakpoints::provider::backends::backend_from_config;
use breakpoints::provider::tool_loop::ToolLoopProvider;
use breakpoints::cli::run_code_review;
```

把 `Cmd::Review` 分支里「构造 provider + 调 run_review」整段，改为先探 mode，再分流。替换从 `let cfg = Config::load()...` 到 `run_review(...)` 的逻辑为：

```rust
            let cfg = Config::load().unwrap_or_default();

            if let Err(e) = check_multi_model(&models) {
                eprintln!("error: {e}");
                std::process::exit(2);
            }

            // 探测 mode 以选 provider 形态。
            let scope_enum = if scope == "branch" { Scope::Branch } else { Scope::WorkingTree };
            let mode_opt = mode.as_deref().map(|m| if m == "code" { Mode::Code } else { Mode::Doc });
            let eopts = EvidenceOpts {
                mode: mode_opt,
                base: base.clone(),
                scope: scope_enum,
                spec: spec.clone(),
                targets: targets.clone(),
            };
            let detected = match detect_mode(&eopts) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("error: {e}");
                    std::process::exit(2);
                }
            };

            // lead provider 选取（与既有逻辑一致）。
            let lead = models
                .as_deref()
                .and_then(|s| s.split(',').next())
                .map(|s| s.trim().to_string())
                .or_else(|| cfg.review.default_models.first().cloned())
                .or_else(|| {
                    let mut v: Vec<_> = cfg.providers.iter().collect();
                    v.sort_by_key(|p| std::cmp::Reverse(p.1.priority));
                    v.first().map(|(k, _)| (*k).clone())
                });
            let lead_pc = match lead.as_deref().and_then(|n| cfg.providers.get(n).map(|pc| (n, pc))) {
                Some((n, pc)) => {
                    let key = cfg.resolve_key(n).unwrap_or_default();
                    (n.to_string(), pc.clone(), key)
                }
                None => {
                    eprintln!("no provider available, please configure [providers.*] first");
                    std::process::exit(2);
                }
            };

            let fmt = parse_format(&format);
            let args = ReviewArgs {
                mode, base: base.clone(), scope, spec, focus, lenses, models,
                no_correlate, block_threshold, conf_floor, background, format, targets,
            };

            let result = match detected {
                Mode::Code => {
                    // code：工具循环 + bundle 流水线
                    let backend = match backend_from_config(&lead_pc.1, lead_pc.2) {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("error: {e}");
                            std::process::exit(2);
                        }
                    };
                    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                    let provider = ToolLoopProvider::new(backend, cwd.clone());
                    let changes = match breakpoints::evidence::git::changed_files(base.as_deref(), scope_enum == Scope::Branch) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("error: {e}");
                            std::process::exit(2);
                        }
                    };
                    run_code_review(&args, &provider, &cfg, changes, &cwd).await
                }
                Mode::Doc => {
                    // doc：维持 Phase-1 单发 lens×model（留到 S4）
                    let provider = DirectProvider::from_config(&lead_pc.1, lead_pc.2);
                    run_review(args, &provider, &cfg).await
                }
            };

            match result {
                Ok((report, code)) => {
                    println!("{}", render(&report, fmt));
                    code
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    2
                }
            }
```

> 注意：`run_review` 仍取 `ReviewArgs`（owned），`run_code_review` 取 `&ReviewArgs`。上面 doc 分支把 `args` move 进 `run_review`；code 分支借用——因二者互斥（match 不同臂），借用检查通过。若编译器抱怨 move/borrow，将 `run_review(args, ...)` 改为在该臂内重建 args 或对 code 臂先 clone。`base`/`scope` 等在构造 `args` 前已 clone 出 `base.clone()`/`scope_enum`，构造 `args` 时把原值 move 进去即可。

- [ ] **Step 6: 编译 + 全套测试 + clippy**

Run:
```bash
cargo build 2>&1 | tail -20
cargo test 2>&1 | tail -15
cargo clippy --all-targets -- -D warnings
```
Expected: build 通过；全套绿（doc e2e 仍过）；clippy 干净

- [ ] **Step 7: 提交**

```bash
git add -A && git commit -m "feat(cli): run_code_review code pipeline; main routes code->tool-loop, doc->direct"
```

---

## Task 9: e2e —— code bundle 工具循环全链路

**Files:**
- Modify: `tests/review_e2e.rs`

**Interfaces:**
- Consumes: `breakpoints::cli::run_code_review`、`breakpoints::provider::tool_loop::{ToolLoopProvider, FakeChatBackend, AssistantTurn}`、`breakpoints::tools::ToolCall`、`breakpoints::evidence::git::FileChange`

> 与 Task 8 的 cli 单测不同：这里用真正的 `ToolLoopProvider + FakeChatBackend` 跑一遍工具循环（read_file → 最终 findings），证明引擎 → bundle 流水线 → positioning 端到端打通：编造引用被丢、真引用锚到正确行。

- [ ] **Step 1: 写 e2e 测试**

`tests/review_e2e.rs` 顶部 `use` 补：

```rust
use breakpoints::cli::run_code_review;
use breakpoints::evidence::git::FileChange;
use breakpoints::provider::tool_loop::{AssistantTurn, FakeChatBackend, ToolLoopProvider};
use breakpoints::tools::ToolCall;
use serde_json::json;
```

追加测试：

```rust
#[tokio::test]
async fn code_bundle_tool_loop_anchors_and_drops_fabricated() {
    let root = tempfile::tempdir().unwrap();
    // 真实文件：db.write(token); 在第 3 行
    std::fs::create_dir_all(root.path().join("src")).unwrap();
    std::fs::write(
        root.path().join("src/auth.rs"),
        "fn login() {\n    let token = user_input;\n    db.write(token);\n}\n",
    )
    .unwrap();

    // 模型脚本：先 read_file，再吐两条 finding（一真一编造）
    let findings_json = r#"{"findings":[
        {"severity":"high","title":"unvalidated write","body":"writes unvalidated input","file":"src/auth.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"validate","quote":"db.write(token);","lens":"L1"},
        {"severity":"critical","title":"phantom sqli","body":"fabricated","file":"src/auth.rs","line_start":1,"line_end":1,"confidence":0.95,"recommendation":"x","quote":"execute_raw_sql(evil)","lens":"L1"}
    ]}"#;
    let backend = FakeChatBackend::new(vec![
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![ToolCall { id: "c1".into(), name: "read_file".into(), args: json!({ "path": "src/auth.rs" }) }],
            input_tokens: 2,
            output_tokens: 2,
        },
        AssistantTurn { text: findings_json.into(), tool_calls: vec![], input_tokens: 3, output_tokens: 5 },
    ]);
    let provider = ToolLoopProvider::new(Box::new(backend), root.path().to_path_buf());

    let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
    let args = ReviewArgs {
        mode: Some("code".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: Some("L1".into()), models: Some("m".into()),
        no_correlate: true, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![],
    };
    let changes = vec![FileChange { path: "src/auth.rs".into(), diff: "+    db.write(token);".into() }];

    let (report, code) = run_code_review(&args, &provider, &cfg, changes, root.path()).await.unwrap();

    // 编造引用被 positioning 丢弃 → 只剩 1 条
    assert_eq!(report.findings.len(), 1, "fabricated finding must be dropped");
    let f = &report.findings[0];
    assert_eq!(f.file, "src/auth.rs");
    assert_eq!(f.line_start, 3, "db.write(token); is on line 3");
    assert_eq!(f.quote, "db.write(token);");
    assert_eq!(f.lenses, vec!["L1".to_string()]);
    // 编造的是 critical；被丢后只剩 high → 仍 ≥ medium block → reject、exit 10
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
    // usage 跨工具轮累加（2+3, 2+5）
    assert_eq!(report.run.usage.input_tokens, 5);
    assert_eq!(report.run.usage.output_tokens, 7);
}
```

- [ ] **Step 2: 跑 e2e 确认通过**

Run: `cargo test --test review_e2e 2>&1 | tail -20`
Expected: 全绿（含原 doc e2e + 新 code e2e）

- [ ] **Step 3: 全套 + clippy + 供应链 + 提交**

```bash
cargo test 2>&1 | tail -10
cargo clippy --all-targets -- -D warnings
cargo tree -i openssl-sys 2>&1 | head -3   # 仍为空
git add -A && git commit -m "test(e2e): code bundle tool-loop pipeline (anchor real, drop fabricated)"
```

---

## Self-Review

**1. Spec coverage**（路线图 S2-2b「关键构建」逐条）：
- 确定性文件筛选 → Task 3 ✓
- 文件分包启发式（目录邻近 / 同名多语言资源；import 延后） → Task 4 ✓
- lens→rule（按文件类型注入关注清单，含 L5 封顶 medium） → Task 6（rule 注入 + compliance cap 提示）+ Task 1（`lens` tag 保 clamp）✓
- `bundle×model` 取代 `lens×model`（code 线） → Task 7 + Task 8 ✓
- positioning 尾段（`quote` + `quote→file:line` 解析 + 缺失即丢弃） → Task 1（schema）+ Task 5（anchor）+ Task 8（接进流水线，dedup 前）✓
- CLI 接线 + e2e → Task 8 + Task 9 ✓
- `max_concurrency` 信号量（路线图 §4 并入 S2-2b） → Task 7 ✓
- 复用 evidence/consolidate/verdict/output → Task 2/8 ✓
- 超大 changeset 不再「甩清单」 → Task 4 分包 + 切块取代（main code 路径走 bundle，不再调旧 `decide_inline`）✓

**2. Placeholder scan**：每个 code step 含完整代码；无 TBD/TODO/"add error handling" 占位。✓

**3. Type consistency**：
- `FileChange{path,diff}`（Task 2）→ bundle（3/4）、cli（8）、e2e（9）一致引用。
- `Bundle{files}`（Task 4）→ prompt（6）、orchestrate（7）一致。
- `enrich(raw, fallback_lens, model)`（Task 1）→ orchestrate `run_one_bundle` 调 `enrich(r,"code",&pm.name)`（7）；`run_one_lens` 旧调 `enrich(r, lens.id, ...)` 仍合签名 ✓。
- `run_code_fanout(provider, target_label, bundles, rules, focus, opts)`（Task 7）→ cli `run_code_review` 调用签名一致（8）✓。
- `anchor(findings, root)`（Task 5）→ cli 调用一致（8）✓。
- `build_bundle_prompt(target_label, bundle, rules, user_focus)`（Task 6）→ orchestrate 调用一致（7）✓。
- `run_code_review(&args, provider, cfg, changes, root)`（Task 8）→ cli 单测（8）、e2e（9）、main（8）一致 ✓。

**4. 约束符合**：positioning 在 dedup 前（Task 8 step 3 顺序）✓；severity 双保险（Task 6 cap 提示 + Task 1 lens tag → 既有 `clamp_compliance` 不变）✓；doc 不回归（main 按 mode 分流，doc 仍 `run_review`+`DirectProvider`；Phase-1 doc e2e 不动）✓；无新 OpenSSL（只加 tokio `sync`，Task 7 step 6 / Task 9 step 3 验 `cargo tree -i openssl-sys` 空）✓；引擎冻结（只 import `ToolLoopProvider`/`backend_from_config`/`FakeChatBackend`，不改 tools/tool_loop/backends）✓。

**遗留/延后（记账本，最终评审 triage）**：
- code 线 `correlate` 未接（S3 与 verify 一起做实）——`run_code_review` 不调 correlate，clusters 恒空。
- `enrich` 的 `id` 在 positioning 前算（用模型给的行号）；positioning 改行后 id 不重算（id 仅 cluster 用，S3 revisit）。
- `run_review` 的 code 旧路径（单发 lens×model）保留为弱模型兜底，但 CLI 默认不再路由到它（main code→`run_code_review`）；§19.2 的「弱模型显式回落单发」开关留作后续。
- `changed_files` 的 git-shell 路径无单测（与 Phase-1 `collect_code` 一致；e2e 走注入 changes）。
