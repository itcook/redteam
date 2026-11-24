# S3-a 多模型 fan-out 基座 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 code 评审真正多模型对抗——`--models a,b` / `--models-all` / 非空 `default_models` 触发 bundle×model 全复制（replicate），跨模型相同 finding 在 dedup 汇成 `agreement`，并用确定性 import-graph 增强分包。

**Architecture:** 新增通用 `ProviderRegistry`（按 model 名路由到子 provider 实例，trait 边界不动）；`OrchestrateOpts.explicit` 改名 `replicate` 并在 `run_code_fanout` 实现「每 (bundle, model) 独立格子全跑」；统一 `resolve_models` helper 把 model 来源收敛为「显式列表→replicate / 空→候补」；`bundle::group` 增加相对 import 提取与 `GENERIC_STEMS` 排除；删除 `check_multi_model` 拒绝。doc 路径经同一 registry 白捡多模型，行为除字段改名外不变（真改留 S4）。

**Tech Stack:** Rust 2021, tokio(sync/rt-multi-thread), async-trait, futures::join_all, serde/serde_json, schemars, clap。import 提取手写逐行扫描（零新依赖）。FakeProvider 驱动全部测试，无真实 key。

## Global Constraints

每个 task 的要求都隐含包含本节，逐条 verbatim：

- **rustls-only，零 OpenSSL**：不引入任何新依赖；`cargo tree -i openssl-sys` 必须报「not found / 空」。import 提取用手写 str 扫描，不加 `regex` crate。
- **引擎层冻结**：禁止修改 `src/tools/`、`src/provider/tool_loop.rs`、`src/provider/backends.rs`（S2-2a 引擎）。`ProviderRegistry` 是 `src/provider/` 下的新文件，不触碰引擎。
- **doc 路径无回归**：`run_review` / `run_fanout` / `run_one_lens` 除「`explicit`→`replicate` 字段改名」和「改用 `resolve_models`」外行为不变；doc 评审仍是 `lens×model` 经 `DirectProvider`，章节锚定/doc 工具留 S4。
- **positioning 在 dedup 之前**：`run_code_review` 的流水线顺序（select_files → group → fan-out → anchor → dedup → clamp → verdict）不变。
- **§7.4 severity 双保险**：不改 `clamp_compliance`、不改 `enrich` 的 `raw.lens → lenses` 映射；L5 规范遵循 finding 仍封顶 medium。
- **root 限制**：`bundle` 读取文件做 import 提取时只 `root.join(改动文件相对路径)`（git 给出的仓内路径，非模型可控），读失败即跳过该文件；不读 root 外文件。`positioning::safe_read` 不动。
- **英文字符串 / 中文注释**：所有面向用户/模型的字符串用英文；代码注释用简体中文。
- **确定性分包，不上 LLM**：import-graph 是纯函数 + 文件读，无任何模型调用。
- **replicate 语义**：显式列表（`--models` / `--models-all` / 非空 `default_models` 任一）→ replicate=true（每 bundle×model 全跑，任一格失败记 `failed_models`）；三者全空 → 所有 provider 按 priority 候补、首个成功即止、replicate=false。`--models` 与 `--models-all` 互斥，`--models-all` 优先。
- **退出码 0/10/2 不变**：reject→10；approve 且 `failed_models` 非空→2；干净 approve→0。
- **测试基线**：起步 `cargo test` = 123 passed（120 lib + 3 e2e），1 ignored。每个 task 结束须全绿 + `cargo clippy --all-targets -- -D warnings` 干净 + `cargo tree -i openssl-sys` 空。

## File Structure

| 文件 | 责任 | 本计划动作 |
|---|---|---|
| `src/provider/registry.rs` | 按 model 名路由到子 provider 的注册表，自身 impl `ReviewProvider` | **新建**（Task 1） |
| `src/provider/mod.rs` | provider 子模块声明 | 加 `pub mod registry;`（Task 1） |
| `src/bundle/mod.rs` | 确定性筛选 + 分包；新增相对 import 提取 + 通用词干排除 | **改**（Task 2） |
| `src/orchestrate/mod.rs` | fan-out；`explicit`→`replicate` 改名；`run_code_fanout` 全复制分支 | **改**（Task 3） |
| `src/cli.rs` | `resolve_models` helper；`run_review`/`run_code_review` 接入；删 `check_multi_model`；`ReviewArgs` 加 `models_all` | **改**（Task 4） |
| `src/main.rs` | `--models-all` flag；`ProviderRegistry` 多实例构造；删 `check_multi_model` 调用 | **改**（Task 5） |
| `tests/review_e2e.rs` | 多模型 replicate 端到端 | 加 2 测试（Task 5） |

**任务依赖**：T1（registry，独立）→ T2（bundle，独立）→ T3（orchestrate 改名 + replicate）→ T4（cli 依赖 T3 的 `OrchestrateOpts.replicate`）→ T5（main+e2e 依赖 T1+T3+T4）。T2 与其余正交。

---

### Task 1: ProviderRegistry（按 model 名路由）

**Files:**
- Create: `src/provider/registry.rs`
- Modify: `src/provider/mod.rs:1-4`（加模块声明）
- Test: `src/provider/registry.rs`（同文件 `#[cfg(test)]`）

**Interfaces:**
- Consumes: `crate::provider::{LensCall, LensRequest, ProviderError, ReviewProvider}`（`provider/mod.rs:9-26`）；`ReviewProvider::run(&self, req: LensRequest) -> Result<LensCall, ProviderError>`；`FakeProvider`（`provider/fake.rs`，按 `req.model` 路由 `scripted` map）。
- Produces: `pub struct ProviderRegistry`；`ProviderRegistry::new() -> Self`；`ProviderRegistry::insert(&mut self, name: impl Into<String>, provider: Box<dyn ReviewProvider>)`；`ProviderRegistry::len(&self) -> usize`；`ProviderRegistry::is_empty(&self) -> bool`；`impl ReviewProvider for ProviderRegistry`（按 `req.model` 取子 provider，未知名 → `ProviderError::Unavailable`）。Task 5 用它在 main 装多实例。

- [ ] **Step 1: 写失败测试**

在 `src/provider/registry.rs` 写整个文件骨架（仅测试 + 空类型让它编译失败到断言）。先写测试：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{RawFinding, Severity};
    use crate::provider::fake::FakeProvider;

    fn rf(title: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false, quote: "q".into(), lens: None,
        }
    }

    #[tokio::test]
    async fn routes_by_model_name() {
        let mut reg = ProviderRegistry::new();
        reg.insert("alpha", Box::new(FakeProvider::with("alpha", vec![rf("from-alpha")])));
        reg.insert("beta", Box::new(FakeProvider::with("beta", vec![rf("from-beta")])));
        assert_eq!(reg.len(), 2);
        let a = reg.run(LensRequest { model: "alpha".into(), prompt: "p".into() }).await.unwrap();
        assert_eq!(a.findings[0].title, "from-alpha");
        let b = reg.run(LensRequest { model: "beta".into(), prompt: "p".into() }).await.unwrap();
        assert_eq!(b.findings[0].title, "from-beta");
    }

    #[tokio::test]
    async fn unknown_model_is_unavailable() {
        let reg = ProviderRegistry::new();
        let e = reg.run(LensRequest { model: "ghost".into(), prompt: "p".into() }).await;
        assert!(matches!(e, Err(ProviderError::Unavailable(_))));
    }
}
```

并在 `src/provider/mod.rs` 第 1-4 行的 `pub mod` 区块按字母序加入（在 `pub mod fake;` 之前）：

```rust
pub mod backends;
pub mod direct;
pub mod fake;
pub mod registry;
pub mod tool_loop;
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p breakpoints provider::registry`
Expected: 编译失败（`cannot find type ProviderRegistry`）。

- [ ] **Step 3: 写最小实现**

在 `src/provider/registry.rs` 顶部（测试 mod 之前）加实现：

```rust
use crate::provider::{LensCall, LensRequest, ProviderError, ReviewProvider};
use async_trait::async_trait;
use std::collections::BTreeMap;

/// 按 model 名路由到子 provider 的注册表（通用基建）。
/// `LensRequest.model` 即 provider 名（= 注册键，也就是 orchestrate 里 `PlannedModel.name`）；
/// 命中则转调对应子 provider，未知名返回 `Unavailable`（让该格记入 failed_models）。
/// code 路径填 `ToolLoopProvider` 实例、doc 路径填 `DirectProvider` 实例，二者皆 `ReviewProvider`。
#[derive(Default)]
pub struct ProviderRegistry {
    providers: BTreeMap<String, Box<dyn ReviewProvider>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self { providers: BTreeMap::new() }
    }
    pub fn insert(&mut self, name: impl Into<String>, provider: Box<dyn ReviewProvider>) {
        self.providers.insert(name.into(), provider);
    }
    pub fn len(&self) -> usize {
        self.providers.len()
    }
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[async_trait]
impl ReviewProvider for ProviderRegistry {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        match self.providers.get(&req.model) {
            Some(p) => p.run(req).await,
            None => Err(ProviderError::Unavailable(format!(
                "model not in registry: {}",
                req.model
            ))),
        }
    }
}
```

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p breakpoints provider::registry`
Expected: PASS（2 测试）。再跑 `cargo clippy --all-targets -- -D warnings`，干净。

- [ ] **Step 5: 提交**

```bash
git add src/provider/registry.rs src/provider/mod.rs
git commit -m "feat(s3a): ProviderRegistry routes LensRequest by model name"
```

---

### Task 2: bundle 分包增强（import-graph + 修同词干 over-grouping）

**Files:**
- Modify: `src/bundle/mod.rs`（`group` 签名加 `root`、新增 `extract_imports`/`GENERIC_STEMS`、`same_stem` 排除通用词干、更新现有测试调用）
- Test: `src/bundle/mod.rs`（同文件 `#[cfg(test)]`）

**Interfaces:**
- Consumes: `crate::evidence::git::FileChange { path: String, diff: String }`；`std::path::Path`。
- Produces: `pub fn group(changes: Vec<FileChange>, root: Option<&Path>) -> Vec<Bundle>`（**签名变更**：加 `root`；`None` 退化到目录+词干，`Some` 额外做 import union）；`pub fn extract_imports(path: &str, content: &str) -> Vec<String>`（返回已规范化的相对引用键，无扩展名）。Task 4 的 `run_code_review` 改调 `group(select_files(changes), Some(root))`。

- [ ] **Step 1: 写失败测试**

在 `src/bundle/mod.rs` 的 `#[cfg(test)] mod tests` 内追加（保留现有测试，仅给现有 `group(...)` 调用补 `, None` 见 Step 3）：

```rust
    #[test]
    fn extract_imports_ts_relative() {
        let src = "import {a} from '../util/b';\nconst c = require('./helper');\nimport x from 'react';\n";
        let mut got = extract_imports("src/feat/x.ts", src);
        got.sort();
        // '../util/b' 相对 src/feat → util/b；'./helper' → src/feat/helper；'react' 裸模块忽略
        assert_eq!(got, vec!["src/feat/helper".to_string(), "util/b".to_string()]);
    }

    #[test]
    fn extract_imports_python_relative() {
        let src = "from .models import User\nfrom ..db.repo import Repo\nimport os\n";
        let mut got = extract_imports("pkg/api/view.py", src);
        got.sort();
        // '.models' → pkg/api/models；'..db.repo' 上溯一级 → pkg/db/repo；'os' 忽略
        assert_eq!(got, vec!["pkg/api/models".to_string(), "pkg/db/repo".to_string()]);
    }

    #[test]
    fn extract_imports_rust_mod() {
        let src = "pub mod token;\nmod helper;\nuse crate::other::thing;\n";
        let mut got = extract_imports("src/auth/mod.rs", src);
        got.sort();
        // mod 声明 → 同目录子模块；`use crate::` 不解析（MVP 范围外）
        assert_eq!(got, vec!["src/auth/helper".to_string(), "src/auth/token".to_string()]);
    }

    #[test]
    fn group_unions_by_relative_import_across_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/feat")).unwrap();
        std::fs::create_dir_all(dir.path().join("util")).unwrap();
        std::fs::write(dir.path().join("src/feat/x.ts"), "import {b} from '../../util/b';\n").unwrap();
        std::fs::write(dir.path().join("util/b.ts"), "export const b = 1;\n").unwrap();
        // 不同目录、不同词干，仅靠 import 关联
        let changes = vec![fc("src/feat/x.ts", "+import"), fc("util/b.ts", "+export")];
        let out = group(changes, Some(dir.path()));
        assert_eq!(out.len(), 1, "relative import should union across dirs");
        assert_eq!(out[0].files.len(), 2);
    }

    #[test]
    fn group_generic_stems_do_not_over_union() {
        // 两个不同目录的 mod.rs：词干都是 "mod"（通用），不应仅因同词干并包
        let out = group(vec![fc("a/mod.rs", "+x"), fc("b/mod.rs", "+y")], None);
        assert_eq!(out.len(), 2, "generic stem 'mod' must not union unrelated files");
    }

    #[test]
    fn group_none_root_falls_back_to_dir_and_stem() {
        // root=None：无 import 提取，退化到目录+词干（同目录并包）
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("src/auth/token.rs", "+b")], None);
        assert_eq!(out.len(), 1);
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p breakpoints bundle`
Expected: 编译失败（`extract_imports` 不存在；`group` 参数个数不符）。

- [ ] **Step 3: 写实现**

在 `src/bundle/mod.rs` 顶部 import 区加 `use std::path::Path;`。在 `BUNDLE_MAX_FILES` 常量附近加通用词干常量，并新增提取相关私有函数 + 公开 `extract_imports`：

```rust
/// 通用入口/聚合文件词干：同词干不代表逻辑相关，不参与同词干 union（只靠同目录 + import）。
const GENERIC_STEMS: &[&str] = &["mod", "lib", "main", "index", "__init__", "init"];

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// 去掉 basename 最后一个扩展名；dotfile（basename 首字符即 '.'）整名保留。
fn strip_ext(p: &str) -> String {
    let slash = p.rfind('/');
    let dot = p.rfind('.');
    match (slash, dot) {
        (Some(s), Some(d)) if d > s + 1 => p[..d].to_string(), // basename 内、非首字符
        (None, Some(d)) if d > 0 => p[..d].to_string(),
        _ => p.to_string(),
    }
}

/// 改动集文件 → 模块键（去最后扩展名）。
fn norm_file(path: &str) -> String {
    strip_ext(path)
}

/// 把 base_dir 下的相对引用 rel 规范化为仓内键（处理 ./ 与 ../、去最后扩展名）。
fn norm_rel(base_dir: &str, rel: &str) -> String {
    let mut stack: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    strip_ext(&stack.join("/"))
}

/// 提取一行里 needle 之后首个引号包裹、以 '.' 开头的相对路径（裸模块返回 None）。
fn quoted_rel_after(line: &str, needle: &str) -> Option<String> {
    let start = line.find(needle)? + needle.len();
    let rest = &line[start..];
    let qpos = rest.find(['\'', '"'])?;
    let qchar = rest.as_bytes()[qpos] as char;
    let after = &rest[qpos + 1..];
    let end = after.find(qchar)?;
    let inner = &after[..end];
    if inner.starts_with('.') {
        Some(inner.to_string())
    } else {
        None
    }
}

/// Python 相对 import token（前导点 + 点分模块）→ 相对路径片段；纯 "." 当前包返回 None。
fn py_rel(token: &str) -> Option<String> {
    let dots = token.chars().take_while(|&c| c == '.').count();
    if dots == 0 {
        return None;
    }
    let modpart = &token[dots..];
    let ups = dots - 1; // 1 点=当前目录，每多一点上溯一级
    if modpart.is_empty() && ups == 0 {
        return None; // 纯 "from . import x"，无具体文件
    }
    let mut rel = String::new();
    for _ in 0..ups {
        rel.push_str("../");
    }
    rel.push_str(&modpart.replace('.', "/"));
    Some(rel)
}

/// 从文件内容提取「指向同仓其它文件的相对引用键」（已就当前文件目录规范化、无扩展名）。
/// 仅解析相对引用：TS/JS `from './x'`/`require('./x')`、Python `from .x import`、Rust `mod x;`。
/// 裸模块/绝对引用（`import react`、`use crate::`、Go `import "pkg"`）不解析——要么外部依赖、
/// 要么需项目根映射，超出确定性 MVP（退化到目录+词干，工具循环兜底）。
pub fn extract_imports(path: &str, content: &str) -> Vec<String> {
    let ext = path.rsplit('.').next().unwrap_or("");
    let base = dir_of(path);
    let mut out = Vec::new();
    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            for line in content.lines() {
                if let Some(rel) = quoted_rel_after(line, "from ") {
                    out.push(norm_rel(base, &rel));
                } else if let Some(rel) = quoted_rel_after(line, "require(") {
                    out.push(norm_rel(base, &rel));
                }
            }
        }
        "py" => {
            for line in content.lines() {
                let t = line.trim_start();
                if let Some(rest) = t.strip_prefix("from ") {
                    let token = rest.split_whitespace().next().unwrap_or("");
                    if token.starts_with('.') {
                        if let Some(rel) = py_rel(token) {
                            out.push(norm_rel(base, &rel));
                        }
                    }
                }
            }
        }
        "rs" => {
            for line in content.lines() {
                let t = line.trim();
                let t = t.strip_prefix("pub ").unwrap_or(t);
                if let Some(rest) = t.strip_prefix("mod ") {
                    if let Some(name) = rest.strip_suffix(';') {
                        let name = name.trim();
                        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                            let key = if base.is_empty() {
                                name.to_string()
                            } else {
                                format!("{base}/{name}")
                            };
                            out.push(key);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}
```

把 `stem` 旁的 `same_stem` 判定改为排除通用词干，并把 `group` 签名加 `root`、在双循环后追加 import union。完整替换现有 `group`：

```rust
/// 轻量分包（确定性）：同父目录、或同词干（排除通用入口名）、或相对 import 互引 → 并到一包；
/// 超 cap 按排序切块。`root=Some` 时读各改动文件全文做 import union；`None` 退化到目录+词干。
pub fn group(mut changes: Vec<FileChange>, root: Option<&Path>) -> Vec<Bundle> {
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let n = changes.len();
    if n == 0 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..n).collect();
    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        for j in (i + 1)..n {
            let same_dir = parent_dir(&changes[i].path) == parent_dir(&changes[j].path);
            let si = stem(&changes[i].path);
            let same_stem = si == stem(&changes[j].path) && !GENERIC_STEMS.contains(&si);
            if same_dir || same_stem {
                union(&mut parent, i, j);
            }
        }
    }
    // 相对 import union（仅当给定 root）：A 引用了同在改动集里的 B → 并包。
    if let Some(root) = root {
        let key_to_idx: std::collections::BTreeMap<String, usize> =
            (0..n).map(|i| (norm_file(&changes[i].path), i)).collect();
        for i in 0..n {
            let full = root.join(&changes[i].path);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue, // 读失败/二进制 → 跳过该文件 import（退化）
            };
            for key in extract_imports(&changes[i].path, &content) {
                if let Some(&j) = key_to_idx.get(&key) {
                    if i != j {
                        union(&mut parent, i, j);
                    }
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
    for (_root_idx, idxs) in groups {
        for chunk in idxs.chunks(BUNDLE_MAX_FILES) {
            let files = chunk.iter().map(|&k| changes[k].clone()).collect();
            bundles.push(Bundle { files });
        }
    }
    bundles
}

/// union-by-min-index：指向较小代表元，使 BTreeMap 按「组内最早文件」排序，bundle 顺序稳定直觉。
fn union(parent: &mut [usize], i: usize, j: usize) {
    let ri = uf_find(parent, i);
    let rj = uf_find(parent, j);
    if ri != rj {
        parent[ri.max(rj)] = ri.min(rj);
    }
}
```

**注意**：原 `group` 内联的 union 逻辑（`parent[ri.max(rj)] = ri.min(rj)`）抽成 `union` helper 复用；`uf_find` 不变。把现有 `#[cfg(test)]` 里所有 `group(vec![...])` 调用改成 `group(vec![...], None)`（共 5 处：`group_unions_same_directory`、`group_unions_same_stem_across_dirs`、`group_separates_unrelated`、`group_splits_oversized_at_cap`、`group_does_not_union_distinct_dotfiles`）。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p breakpoints bundle`
Expected: PASS（现有 + 6 新增）。`group_unions_same_stem_across_dirs` 用 `widget` 词干（非通用）仍合并。再 `cargo clippy --all-targets -- -D warnings` 干净。

- [ ] **Step 5: 提交**

```bash
git add src/bundle/mod.rs
git commit -m "feat(s3a): import-graph union in bundling; exclude generic stems"
```

---

### Task 3: orchestrate replicate 分支（`explicit`→`replicate` 改名 + 全复制）

**Files:**
- Modify: `src/orchestrate/mod.rs`（`OrchestrateOpts.explicit` 改名 `replicate`，更新 `run_one_lens`/`run_fanout`/`run_one_bundle` 内部引用与测试；`run_code_fanout` 加 replicate 全复制分支）
- Modify: `src/cli.rs`（仅把两处 `OrchestrateOpts { ... explicit: ... }` 字段名改为 `replicate:`，值暂保持 `args.models.is_some()`；完整 model 选取重构在 Task 4）
- Test: `src/orchestrate/mod.rs`（同文件 `#[cfg(test)]`）

**Interfaces:**
- Consumes: `Bundle`、`Lens`、`ReviewProvider`、`RawResults`、`run_one_bundle`、`Semaphore`、`futures::future::join_all`（均已在文件内）。
- Produces: `OrchestrateOpts { models, replicate, no_fallback, max_concurrency, retries }`（字段 `explicit` → `replicate`）；`run_code_fanout` 行为：`replicate=true` 时每 (bundle, model) 独立格子全跑、失败记 `failed_models`；`replicate=false` 时每 bundle 一条候补链（现状）。Task 4 的 `resolve_models` 产出的 `replicate: bool` 赋给该字段。

- [ ] **Step 1: 写失败测试**

在 `src/orchestrate/mod.rs` 的 `#[cfg(test)] mod tests` 内追加。先看现有测试如何构造 `OrchestrateOpts` 与 `FakeProvider`，沿用同风格；这里用 `pub scripted` 手动塞两个 model：

```rust
    #[tokio::test]
    async fn code_fanout_replicate_runs_every_model_per_bundle() {
        use crate::evidence::git::FileChange;
        use std::collections::BTreeMap;
        // 两个 model 各产 1 条 finding（不同 title），手动多 model script
        let mut scripted: BTreeMap<String, Vec<RawFinding>> = BTreeMap::new();
        scripted.insert("alpha".into(), vec![raw_at("src/a.rs", "alpha-issue", Severity::High)]);
        scripted.insert("beta".into(), vec![raw_at("src/a.rs", "beta-issue", Severity::High)]);
        let provider = crate::provider::fake::FakeProvider { scripted };
        let bundles = vec![
            Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "+x".into() }] },
            Bundle { files: vec![FileChange { path: "src/b.rs".into(), diff: "+y".into() }] },
        ];
        let l1 = by_id("L1").unwrap();
        let rules = vec![l1];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true,
            no_fallback: false,
            max_concurrency: 8,
            retries: 0,
        };
        let out = run_code_fanout(&provider, "working tree", &bundles, &rules, "", &opts).await;
        // 2 bundle × 2 model = 4 格，各 1 finding → 4 条
        assert_eq!(out.findings.len(), 4);
        let models: std::collections::BTreeSet<&str> =
            out.findings.iter().flat_map(|f| f.models.iter().map(|s| s.as_str())).collect();
        assert!(models.contains("alpha") && models.contains("beta"));
        assert!(out.failed_models.is_empty());
    }

    #[tokio::test]
    async fn code_fanout_replicate_records_failed_model() {
        use crate::evidence::git::FileChange;
        // 只有 alpha 有 script；beta 不可达 → 记 failed_models
        let provider = crate::provider::fake::FakeProvider::with(
            "alpha",
            vec![raw_at("src/a.rs", "alpha-issue", Severity::High)],
        );
        let bundles = vec![Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "+x".into() }] }];
        let l1 = by_id("L1").unwrap();
        let rules = vec![l1];
        let opts = OrchestrateOpts {
            models: vec![
                PlannedModel { name: "alpha".into(), priority: 10 },
                PlannedModel { name: "beta".into(), priority: 5 },
            ],
            replicate: true,
            no_fallback: false,
            max_concurrency: 8,
            retries: 0,
        };
        let out = run_code_fanout(&provider, "working tree", &bundles, &rules, "", &opts).await;
        assert_eq!(out.findings.len(), 1); // alpha 成功
        assert!(out.failed_models.contains(&"beta".to_string()));
    }
```

如果文件内还没有 `raw_at` 测试 helper，在测试 mod 顶部加（与现有 `raw`/`f` helper 同风格）：

```rust
    fn raw_at(file: &str, title: &str, sev: Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(), lens: Some("L1".into()),
        }
    }
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p breakpoints orchestrate`
Expected: 编译失败（`OrchestrateOpts` 无 `replicate` 字段；现有测试用 `explicit`）。

- [ ] **Step 3: 写实现**

1）`OrchestrateOpts` 定义（`orchestrate/mod.rs:17-24`）把 `pub explicit: bool` 改为 `pub replicate: bool`。

2）全文件把 `opts.explicit` 改为 `opts.replicate`（出现在 `run_one_lens`、`run_fanout`、`run_one_bundle`）；把构造 `OrchestrateOpts { ... explicit: ... }` 处（`run_fanout` 显式分支内的 `single_opts`）字段名改 `replicate:`。语义等价（旧 `explicit` 即「全复制对抗模式」），doc 路径 `run_fanout` 行为不变。

3）现有 `#[cfg(test)] mod tests` 内所有 `explicit:` 字段改 `replicate:`。

4）重写 `run_code_fanout`（`orchestrate/mod.rs:265-306`）为分流实现：

```rust
/// code 线扇出，用 Semaphore 限 max_concurrency：
/// - replicate=true：每 (bundle, model) 独立格子全跑（对抗复制），失败记 failed_models（守 exit-2）。
/// - replicate=false：每 bundle 一条候补链（容错，首个成功即止），整链失败只记 warning。
pub async fn run_code_fanout(
    provider: &dyn ReviewProvider,
    target_label: &str,
    bundles: &[Bundle],
    rules: &[&Lens],
    focus: &str,
    opts: &OrchestrateOpts,
) -> RawResults {
    let sem = Arc::new(Semaphore::new(opts.max_concurrency.max(1)));
    if opts.replicate {
        // 全复制：bundle × model 笛卡尔积，每格只试该 model（不候补）
        let futures = bundles.iter().flat_map(|bundle| {
            opts.models.iter().map(move |pm| {
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
                    let res = run_one_bundle(provider, target_label, bundle, rules, focus, &single).await;
                    (model_name, res)
                }
            })
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
        // 候补：每 bundle 一条候补链（按 priority 降序、首个成功即止）
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
}
```

**注意**：`run_one_bundle` 内部已按 `opts.replicate`（原 `explicit`）决定候补链 take(1)；replicate 单格 `single.models=[pm]` 时只试该 model、失败不候补，正确。

5）`src/cli.rs` 两处 `OrchestrateOpts { ... explicit: ... }`（`run_review` 约 137-143、`run_code_review` 约 243-249）字段名改 `replicate:`，值暂保持原表达式（`run_review` 是 `explicit`/`run_code_review` 是 `explicit`，即 `args.models.is_some()`）——保持编译与现状行为，Task 4 再换成 `resolve_models` 的 `replicate`。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p breakpoints`
Expected: 全绿（新增 2 测试 + 既有全过，含 cli/e2e，因字段已同步改名）。`cargo clippy --all-targets -- -D warnings` 干净。

- [ ] **Step 5: 提交**

```bash
git add src/orchestrate/mod.rs src/cli.rs
git commit -m "feat(s3a): rename explicit->replicate; run_code_fanout full-replicate branch"
```

---

### Task 4: cli `resolve_models` 统一选取 + 删 `check_multi_model`

**Files:**
- Modify: `src/cli.rs`（新增 `resolve_models`；`run_review`/`run_code_review` 改用；删 `check_multi_model` 函数 + 两处调用 + 其测试；`ReviewArgs` 加 `models_all`）
- Test: `src/cli.rs`（同文件 `#[cfg(test)]`）

**Interfaces:**
- Consumes: `Config`（`cfg.providers: BTreeMap<String, ProviderConfig>`、`cfg.review.default_models: Vec<String>`）；`PlannedModel { name, priority }`、`OrchestrateOpts { ..., replicate }`（Task 3）。
- Produces: `pub fn resolve_models(models_flag: Option<&str>, models_all: bool, cfg: &Config) -> (Vec<PlannedModel>, bool)`（返回 `(planned, replicate)`）；`ReviewArgs` 新增 `pub models_all: bool`。`check_multi_model` 被**删除**。Task 5 的 main 用 `resolve_models` 决定构造哪些 provider 实例。

- [ ] **Step 1: 写失败测试**

在 `src/cli.rs` 的某个 `#[cfg(test)]` mod（如 `code_review_tests` 同级或新增 `mod resolve_tests`）追加：

```rust
#[cfg(test)]
mod resolve_tests {
    use super::*;

    fn cfg(providers: &str, defaults: &str) -> Config {
        let toml = format!("{providers}\n[review]\ndefault_models=[{defaults}]\n");
        Config::from_layers(&[&toml]).unwrap()
    }
    fn provs() -> &'static str {
        "[providers.alpha]\nkind=\"openai\"\nmodel=\"a\"\npriority=10\n\
         [providers.beta]\nkind=\"openai\"\nmodel=\"b\"\npriority=5\n"
    }

    #[test]
    fn explicit_models_flag_replicates() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(Some("alpha,beta"), false, &c);
        assert!(rep);
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn models_all_replicates_all_providers_by_priority() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(None, true, &c);
        assert!(rep);
        // 按 priority 降序：alpha(10) 在 beta(5) 前
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn nonempty_default_models_replicates() {
        let c = cfg(provs(), "\"beta\",\"alpha\"");
        let (m, rep) = resolve_models(None, false, &c);
        assert!(rep);
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["beta", "alpha"]);
    }

    #[test]
    fn empty_everything_falls_back_to_candidate_chain() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(None, false, &c);
        assert!(!rep, "no explicit list → candidate chain (fallback)");
        assert_eq!(m.iter().map(|p| p.name.clone()).collect::<Vec<_>>(), vec!["alpha", "beta"]);
    }

    #[test]
    fn models_all_overrides_models_flag() {
        let c = cfg(provs(), "");
        let (m, rep) = resolve_models(Some("alpha"), true, &c);
        assert!(rep);
        assert_eq!(m.len(), 2, "--models-all takes precedence over --models");
    }
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test -p breakpoints resolve_tests`
Expected: 编译失败（`resolve_models` 不存在）。

- [ ] **Step 3: 写实现**

1）在 `src/cli.rs` 删除 `check_multi_model` 函数（约 59-73）及其 doc 注释段里关于它的描述；若 `#[cfg(test)]` 内有 `check_multi_model` 相关测试，一并删除。

2）`ReviewArgs`（约 15-32）在 `pub models: Option<String>,` 之后加：

```rust
    pub models_all: bool,
```

3）加 `resolve_models`（放在 `parse_format` 之后、`run_review` 之前）：

```rust
/// 统一 model 选取（S3-a）。返回 (planned_models, replicate)。
/// 显式列表（`--models-all` / `--models` / 非空 `default_models` 任一）→ replicate=true。
/// 三者全空 → 所有已配置 provider 按 priority 降序候补，replicate=false。
/// `--models-all` 优先于 `--models`。
pub fn resolve_models(
    models_flag: Option<&str>,
    models_all: bool,
    cfg: &Config,
) -> (Vec<PlannedModel>, bool) {
    let all_by_priority = || -> Vec<String> {
        let mut v: Vec<_> = cfg.providers.iter().collect();
        v.sort_by_key(|p| std::cmp::Reverse(p.1.priority));
        v.into_iter().map(|(k, _)| k.clone()).collect()
    };
    let (names, replicate): (Vec<String>, bool) = if models_all {
        (all_by_priority(), true)
    } else if let Some(s) = models_flag {
        (
            s.split(',').map(|x| x.trim().to_string()).filter(|s| !s.is_empty()).collect(),
            true,
        )
    } else if !cfg.review.default_models.is_empty() {
        (cfg.review.default_models.clone(), true)
    } else {
        (all_by_priority(), false)
    };
    let models = names
        .iter()
        .map(|n| PlannedModel {
            name: n.clone(),
            priority: cfg.providers.get(n).map(|p| p.priority).unwrap_or(0),
        })
        .collect();
    (models, replicate)
}
```

4）`run_review`（doc）替换内联 model 选取（约 117-143 的 `explicit`/`model_names`/`models`/`oopts`）为：

```rust
    // 3. 模型集 + replicate 判定（统一 resolve_models）
    let (models, replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let oopts = OrchestrateOpts {
        models,
        replicate,
        no_fallback: false,
        max_concurrency: cfg.review.max_concurrency,
        retries: 2,
    };
```

删除 `run_review` 开头的 `check_multi_model(&args.models)?;` 调用。其余（lens、fan-out、consolidate）不变。

5）`run_code_review` 同样替换内联 model 选取（约 226-249）为同一段，并删除其 `check_multi_model(&args.models)?;` 调用。

6）`run_code_review`/`run_review` 内引用 `model_names` 的地方（report 组装、correlate 的 `lead_model`）保持不变（`model_names` 仍可用）。

7）`#[cfg(test)] mod code_review_tests` 的 `args()` helper 加 `models_all: false,` 字段（在 `models:` 之后）。

- [ ] **Step 4: 运行测试，确认通过**

Run: `cargo test -p breakpoints`
Expected: 全绿（resolve_tests 5 + 既有，check_multi_model 测试已删）。`cargo clippy --all-targets -- -D warnings` 干净。

- [ ] **Step 5: 提交**

```bash
git add src/cli.rs
git commit -m "feat(s3a): unified resolve_models (replicate semantics); drop check_multi_model"
```

---

### Task 5: main 多 provider 构造 + `--models-all` flag + replicate e2e

**Files:**
- Modify: `src/main.rs`（`Cmd::Review` 加 `--models-all`；删 `check_multi_model` 调用；用 `resolve_models` 构造 `ProviderRegistry` 多实例；接线 `run_code_review`/`run_review`）
- Modify: `tests/review_e2e.rs`（加 2 个 replicate 测试）
- Test: `tests/review_e2e.rs`

**Interfaces:**
- Consumes: `ProviderRegistry`（Task 1）；`resolve_models`（Task 4）；`ReviewArgs.models_all`（Task 4）；`backend_from_config(&ProviderConfig, String) -> Result<Box<dyn ChatBackend>, ProviderError>`、`ToolLoopProvider::new(Box<dyn ChatBackend>, PathBuf)`、`DirectProvider::from_config(&ProviderConfig, String)`（引擎层，只调用不改）；`run_code_review(&ReviewArgs, &dyn ReviewProvider, &Config, Vec<FileChange>, &Path)`。
- Produces: 端到端可跑的多模型 code 评审（`--models-all` / 多 `default_models` → registry 路由 + replicate → agreement 合并）。

- [ ] **Step 1: 写失败测试（e2e）**

在 `tests/review_e2e.rs` 末尾追加。沿用文件已有的 helper 风格（`write`、`raw`、`ReviewArgs` 构造）；若无则参照 `src/cli.rs` 的 `code_review_tests`：

```rust
#[tokio::test]
async fn code_replicate_two_models_yield_agreement_two() {
    use breakpoints::cli::{run_code_review, ReviewArgs};
    use breakpoints::config::Config;
    use breakpoints::domain::{Severity, Verdict};
    use breakpoints::evidence::git::FileChange;
    use breakpoints::provider::registry::ProviderRegistry;
    use breakpoints::provider::fake::FakeProvider;
    use breakpoints::domain::RawFinding;

    fn rf(file: &str, quote: &str, sev: Severity) -> RawFinding {
        RawFinding {
            severity: sev, title: "shared issue".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()),
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();

    // default_models=[alpha,beta] → replicate；两 model 对同位置同引用各报一条
    let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha","beta"]
"#]).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("alpha", Box::new(FakeProvider::with("alpha", vec![rf("src/a.rs", "db.write(token);", Severity::High)])));
    reg.insert("beta", Box::new(FakeProvider::with("beta", vec![rf("src/a.rs", "db.write(token);", Severity::Critical)])));

    let args = ReviewArgs {
        mode: Some("code".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: Some("L1".into()), models: None, models_all: false,
        no_correlate: true, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![],
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    db.write(token);".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path()).await.unwrap();

    // 两 model 同 file 同行同引用 → anchor 行2 → dedup 合并 → 1 条 agreement=2、取最高 severity
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].agreement, 2);
    assert_eq!(report.findings[0].line_start, 2);
    assert_eq!(report.findings[0].severity, Severity::Critical);
    assert_eq!(report.verdict, Verdict::Reject);
    assert_eq!(code, 10);
}

#[tokio::test]
async fn code_replicate_unreachable_model_exits_two() {
    use breakpoints::cli::{run_code_review, ReviewArgs};
    use breakpoints::config::Config;
    use breakpoints::domain::{Severity, Verdict};
    use breakpoints::evidence::git::FileChange;
    use breakpoints::provider::registry::ProviderRegistry;
    use breakpoints::provider::fake::FakeProvider;
    use breakpoints::domain::RawFinding;

    fn rf_low(file: &str, quote: &str) -> RawFinding {
        RawFinding {
            severity: Severity::Low, title: "minor".into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "fix".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()),
        }
    }

    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/a.rs"), "fn f() {\n    let x = 1;\n}\n").unwrap();

    // default=[alpha,ghost]：alpha 产 low（approve），ghost 不在 registry → failed_models → exit 2
    let cfg = Config::from_layers(&[r#"[review]
default_models=["alpha","ghost"]
"#]).unwrap();
    let mut reg = ProviderRegistry::new();
    reg.insert("alpha", Box::new(FakeProvider::with("alpha", vec![rf_low("src/a.rs", "let x = 1;")])));

    let args = ReviewArgs {
        mode: Some("code".into()), base: None, scope: "working-tree".into(),
        spec: vec![], focus: None, lenses: Some("L1".into()), models: None, models_all: false,
        no_correlate: true, block_threshold: None, conf_floor: None,
        background: false, format: "json".into(), targets: vec![],
    };
    let changes = vec![FileChange { path: "src/a.rs".into(), diff: "+    let x = 1;".into() }];
    let (report, code) = run_code_review(&args, &reg, &cfg, changes, dir.path()).await.unwrap();

    assert_eq!(report.verdict, Verdict::Approve);   // 仅 low → approve
    assert!(report.run.failed_models.contains(&"ghost".to_string()));
    assert_eq!(code, 2);                            // approve 但对照集不完整
}
```

- [ ] **Step 2: 运行测试，确认失败**

Run: `cargo test --test review_e2e code_replicate`
Expected: 编译/链接失败或断言失败（main 尚未接 registry，但 e2e 直接调 `run_code_review`，故应先因 `ReviewArgs` 缺 `models_all` 之外的原因——实际 Task 4 已加 `models_all`，故此处应直接 PASS 库层逻辑）。若 Task 4 已落地，本步骤预期 **PASS**；其失败信号来自 main 接线（Step 3 验证编译）。若 e2e 已绿，记录并继续 Step 3。

> 说明：e2e 走库函数 `run_code_review`，不经 main；它验证 registry+replicate 全链路。main 接线的正确性由 Step 4 的 `cargo build` + `--help` smoke 验证。

- [ ] **Step 3: 写实现（main 接线）**

在 `src/main.rs`：

1）`Cmd::Review` 变体在 `models: Option<String>,` 之后加：

```rust
        #[arg(long)]
        models_all: bool,
```

2）`main` 的解构（`Cmd::Review { ... }`）加入 `models_all,`。

3）删除 `check_multi_model` 调用块（约 73-76）及其 `use` 导入里的 `check_multi_model`（改为 `use breakpoints::cli::{parse_format, resolve_models, run_code_review, run_review, ReviewArgs};`）。

4）删除旧的单 lead provider 选取块（`lead` / `lead_pc`，约 96-116）。

5）`ReviewArgs` 构造（约 119-122）加 `models_all,` 字段。

6）把 `let result = match detected { ... }` 整块替换为「先用 `resolve_models` 定 model 集，再按 mode 构造 registry 多实例」：

```rust
            let fmt = parse_format(&format);
            let args = ReviewArgs {
                mode, base: base.clone(), scope, spec, focus, lenses, models, models_all,
                no_correlate, block_threshold, conf_floor, background, format, targets,
            };

            let (planned, _replicate) = resolve_models(args.models.as_deref(), args.models_all, &cfg);
            if planned.is_empty() {
                eprintln!("no provider available, please configure [providers.*] first");
                std::process::exit(2);
            }
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

            // 按 mode 构造通用 ProviderRegistry：code 填 ToolLoop 实例、doc 填 Direct 实例。
            let mut registry = breakpoints::provider::registry::ProviderRegistry::new();
            for pm in &planned {
                let pc = match cfg.providers.get(&pm.name) {
                    Some(p) => p.clone(),
                    None => {
                        eprintln!("error: model '{}' has no [providers.{}] config", pm.name, pm.name);
                        std::process::exit(2);
                    }
                };
                let key = cfg.resolve_key(&pm.name).unwrap_or_default();
                match detected {
                    Mode::Code => {
                        let backend = match backend_from_config(&pc, key) {
                            Ok(b) => b,
                            Err(e) => {
                                eprintln!("error: {e}");
                                std::process::exit(2);
                            }
                        };
                        registry.insert(pm.name.clone(), Box::new(ToolLoopProvider::new(backend, cwd.clone())));
                    }
                    Mode::Doc => {
                        registry.insert(pm.name.clone(), Box::new(DirectProvider::from_config(&pc, key)));
                    }
                }
            }

            let result = match detected {
                Mode::Code => {
                    let changes = match breakpoints::evidence::git::changed_files(
                        base.as_deref(),
                        scope_enum == Scope::Branch,
                    ) {
                        Ok(c) => c,
                        Err(e) => {
                            eprintln!("error: {e}");
                            std::process::exit(2);
                        }
                    };
                    run_code_review(&args, &registry, &cfg, changes, &cwd).await
                }
                Mode::Doc => run_review(args, &registry, &cfg).await,
            };
```

**注意**：`run_review` 取 `args` by value（保持现签名），故 code 臂用 `&args`、doc 臂用 `args`（move）——二者互斥 match 臂，编译通过。`base` 在 `args` 构造前已 `.clone()` 进 `args`，code 臂用 `base.as_deref()` 仍可（`base` 未被 move 进 args，args 持有的是 `base.clone()`）。确认 `ReviewArgs.base` 用的是 `base.clone()`。

- [ ] **Step 4: 验证编译 + e2e + smoke**

Run: `cargo build && cargo test && cargo clippy --all-targets -- -D warnings`
Expected: build 通过（main 接线类型正确）；全部测试绿（含 2 个新 e2e）；clippy 干净。

Run: `cargo run -q -- review --help | grep -- --models-all`
Expected: 输出含 `--models-all` flag。

Run: `cargo tree -i openssl-sys`
Expected: 报 not found（无 OpenSSL）。

- [ ] **Step 5: 提交**

```bash
git add src/main.rs tests/review_e2e.rs
git commit -m "feat(s3a): main builds ProviderRegistry; --models-all flag; replicate e2e"
```

---

## Self-Review

**1. Spec 覆盖**（对照路线图 §S3-a 决策）：
- ProviderRegistry（多 provider 实例化）→ Task 1 ✅
- replicate 全复制 → Task 3（`run_code_fanout` replicate 分支）✅
- cross-model agreement → 复用既有 `dedup::merge_into`（无需改），由 Task 5 e2e 验证 `agreement=2` ✅
- import-graph 分包增强 + 修 over-grouping → Task 2 ✅
- 删 check_multi_model → Task 4（函数+调用）+ Task 5（main 调用）✅
- model 选取语义（`--models`/`--models-all`/`default_models`→replicate；空→候补；`--models-all` 优先）→ Task 4 `resolve_models` ✅
- doc 白捡多模型（registry 通用）→ Task 5（doc 填 Direct 实例）✅
- `--models a,b` 跑通合并后单一 report → Task 5 e2e ✅
- failed_models → exit 2 → Task 3（replicate 记录）+ Task 5 e2e ✅
- 留给 S3-b（不在本计划）：三层 verify、`Finding.verify` 字段、real correlate ——本计划不触碰 `consolidate/correlate.rs`、不加 verify。✅ 边界清晰。

**2. Placeholder 扫描**：无 TBD/TODO；每个改码 step 都给完整代码；测试均为可运行断言。✅

**3. 类型一致性**：
- `OrchestrateOpts.replicate`：Task 3 定义，Task 4 `resolve_models` 产出并赋值，Task 5 间接经 cli 使用——名称一致。✅
- `resolve_models(Option<&str>, bool, &Config) -> (Vec<PlannedModel>, bool)`：Task 4 定义，Task 5 调用签名一致。✅
- `ProviderRegistry::new/insert/len/is_empty` + `impl ReviewProvider`：Task 1 定义，Task 5 用 `new`/`insert` + 作为 `&dyn ReviewProvider` 传入——一致。✅
- `group(Vec<FileChange>, Option<&Path>)`：Task 2 定义；`run_code_review` 调用点改 `Some(root)` 属 Task 4 范围——**补充**：Task 4 的 `run_code_review` 第 1 步 `group(select_files(changes))` 须改为 `group(select_files(changes), Some(root))`。在 Task 4 Step 3 追加该改动（见下）。
- `ReviewArgs.models_all: bool`：Task 4 加字段，Task 5 main 构造 + e2e 构造都含——一致。✅
- `extract_imports(&str, &str) -> Vec<String>`：Task 2 定义并自测，外部暂无消费者（group 内部用），无跨任务签名风险。✅

**修复（类型一致性 #3 发现的缺口）— 已重新归属到 Task 2**：`group` 改签名后唯一调用点 `run_code_review` 的 `group(select_files(changes))` 会编译断，故该一行改动（→ `group(select_files(changes), Some(root))`，`root: &Path` 已是入参）**必须随 Task 2 的签名改动一起做**，否则 Task 2 自己的 `cargo test` 不编译。Task 4 不再触碰该调用点。两个既有 `code_review_tests` 仍绿（tempdir root 下单文件改动无额外 import union，行为不变）。
