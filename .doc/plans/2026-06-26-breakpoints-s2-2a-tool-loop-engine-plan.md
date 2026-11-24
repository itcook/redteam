# S2-2a 工具循环引擎 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 breakpoints 增加一个带工具（read_file / search_repo / list_files）的多轮 LLM 评审引擎，模型能自取代码上下文后再产出 findings，零外部命令依赖、可完全离网单测。

**Architecture:** 三层新结构，沿用现有 `ReviewProvider` 边界。`src/tools/` 是纯文件系统的 `ToolExecutor`（链入 ripgrep 内核 crate `ignore`/`grep-regex`/`grep-searcher`，做 gitignore 感知遍历与搜索）。`src/provider/tool_loop.rs` 定义 provider 无关的 `ChatBackend` 抽象 + `run_tool_loop` 多轮驱动 + 测试用 `FakeChatBackend` + 把二者组合成 drop-in `ReviewProvider` 的 `ToolLoopProvider`。`src/provider/backends.rs` 是 Anthropic / OpenAI 两个协议适配（纯 body 构造 + 响应解析可 hermetic 测，HTTP 走 `#[ignore]` smoke）。既有单发 `DirectProvider` 原样保留作弱模型兜底。

**Tech Stack:** Rust 2021、tokio、async-trait、reqwest 0.13（rustls-only）、serde_json；新增 `ignore`、`grep-regex`、`grep-searcher`；dev 用 `tempfile`。

## Global Constraints

- **rustls-only，绝不引入 OpenSSL**：每次加依赖后 `cargo tree -i openssl-sys` 必须为空。
- **搜索/遍历零外部命令**：不得 shell 调 `rg`/`git grep`/`grep`/`find`；一律用进程内 `ignore`/`grep-*` crate。
- **ToolExecutor 限定在 root 内**：拒绝绝对路径与含 `..` 的路径（防越权读取宿主任意文件）。
- **代码字符串字面量用英语**；注释可中文（沿用本仓既有约定，见 `src/provider/direct.rs`）。
- **复用既有 finding 形状**：findings 一律经 `crate::provider::direct::parse_lens_output` 解析为 `LensOutput`/`RawFinding`，不另定义评审输出 schema。
- **既有 `DirectProvider` 单发路径原样保留**，本阶段不改它、不改 `orchestrate`、不改 `main.rs`。
- **测试 hermetic**：CI 不打网络；真实网络调用一律 `#[ignore]` smoke，靠 env 提供 key。
- **每个 Task 结束**：`cargo test` 全绿 + `cargo clippy --all-targets -- -D warnings` 干净。
- 当前分支：`feat/core-engine`（S2-2a 直接续在其上；若改用独立分支由执行者按 finishing-a-development-branch 收尾）。

---

## File Structure

| 文件 | 职责 |
|---|---|
| `Cargo.toml`（改） | 新增 `ignore`/`grep-regex`/`grep-searcher` 依赖 |
| `src/lib.rs`（改） | 增 `pub mod tools;` |
| `src/tools/mod.rs`（新） | `ToolSpec`/`ToolCall`/`ToolResult`/`tool_specs()` + `ToolExecutor`（read_file/search_repo/list_files） |
| `src/provider/mod.rs`（改） | 增 `pub mod tool_loop;` 与 `pub mod backends;` |
| `src/provider/tool_loop.rs`（新） | `ChatMessage`/`AssistantTurn`/`ChatBackend` + `run_tool_loop` + `FakeChatBackend` + `ToolLoopProvider` |
| `src/provider/backends.rs`（新） | `AnthropicBackend`/`OpenAiBackend`（纯 body+parse 函数 + HTTP `ChatBackend` 实现）+ `backend_from_config` |

依赖方向：`tools`（叶子，无 provider 依赖）← `tool_loop`（用 tools + 复用 direct::parse_lens_output）← `backends`（实现 tool_loop::ChatBackend）。

---

### Task 1: tools 模块骨架 + 依赖接入

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/tools/mod.rs`

**Interfaces:**
- Consumes: 无（叶子模块）。
- Produces:
  - `pub struct ToolSpec { pub name: &'static str, pub description: &'static str, pub parameters: serde_json::Value }`
  - `pub fn tool_specs() -> Vec<ToolSpec>`（read_file / search_repo / list_files 三项，`parameters` 为 JSON Schema 对象）
  - `pub struct ToolCall { pub id: String, pub name: String, pub args: serde_json::Value }`（`Clone+Debug+PartialEq`）
  - `pub struct ToolResult { pub id: String, pub content: String }`（`Clone+Debug+PartialEq`）
  - `pub struct ToolExecutor { root: std::path::PathBuf }` + `pub fn new(root: impl Into<PathBuf>) -> Self`

- [ ] **Step 1: 加依赖**

修改 `Cargo.toml`，在 `[dependencies]` 末尾（`reqwest` 行后）追加：

```toml
# ripgrep 内核 crate：gitignore 感知遍历(ignore) + 正则搜索(grep-*)；纯 Rust，无 OpenSSL。
ignore = "0.4"
grep-regex = "0.1"
grep-searcher = "0.1"
```

- [ ] **Step 2: 验证依赖未引入 OpenSSL**

Run: `cargo tree -i openssl-sys`
Expected: 报 `error: no crates ... openssl-sys`（即依赖图中不存在），或空。若出现 openssl-sys，停止并上报。

- [ ] **Step 3: 注册模块**

修改 `src/lib.rs`，在 `pub mod prompt;` 之前按字母序加一行：

```rust
pub mod tools;
```

- [ ] **Step 4: 写失败测试**

创建 `src/tools/mod.rs`，先只放测试（其余 Step 5 补）：

```rust
use serde_json::Value;
use std::path::PathBuf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_specs_lists_three_named_tools() {
        let specs = tool_specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["read_file", "search_repo", "list_files"]);
        // 每个 spec 的 parameters 是对象、含 "type":"object"
        for s in &specs {
            assert_eq!(s.parameters["type"], "object", "spec {} bad schema", s.name);
        }
    }

    #[test]
    fn read_file_spec_requires_path() {
        let specs = tool_specs();
        let rf = specs.iter().find(|s| s.name == "read_file").unwrap();
        assert_eq!(rf.parameters["required"][0], "path");
    }

    #[test]
    fn executor_stores_root() {
        let ex = ToolExecutor::new("/tmp/x");
        assert_eq!(ex.root, PathBuf::from("/tmp/x"));
    }
}
```

- [ ] **Step 5: 实现**

在 `src/tools/mod.rs` 顶部（测试之前）加：

```rust
use serde_json::{json, Value};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    pub id: String,
    pub content: String,
}

/// 暴露给模型的工具声明（name + 描述 + JSON Schema 参数）。
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
}

/// 三个评审工具的固定声明集。各 adapter 自行把 parameters 映射到本家协议字段。
pub fn tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "read_file",
            description: "Read a file under the review root, returned with line numbers. \
                          Optionally restrict to an inclusive 1-based line range.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the review root." },
                    "start": { "type": "integer", "description": "First line (1-based, inclusive)." },
                    "end": { "type": "integer", "description": "Last line (1-based, inclusive)." }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "search_repo",
            description: "Search file contents under the review root with a regular expression \
                          (gitignore-aware). Returns matching lines as path:line:text.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Rust-regex pattern." },
                    "path": { "type": "string", "description": "Optional subpath to scope the search." }
                },
                "required": ["pattern"]
            }),
        },
        ToolSpec {
            name: "list_files",
            description: "List files under the review root (gitignore-aware).",
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Optional subpath to list." }
                },
                "required": []
            }),
        },
    ]
}

/// 把工具调用落到真实文件系统上的执行器；所有访问被限制在 `root` 之内。
pub struct ToolExecutor {
    pub(crate) root: PathBuf,
}

impl ToolExecutor {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ToolExecutor { root: root.into() }
    }
}
```

并把测试里多余的 `use` 清掉（Step 4 顶部那两行 `use` 删除，改由实现块统一引入；测试 `use super::*;` 已覆盖）。

- [ ] **Step 6: 跑测试**

Run: `cargo test --lib tools::`
Expected: 3 个测试 PASS。

- [ ] **Step 7: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add Cargo.toml Cargo.lock src/lib.rs src/tools/mod.rs
git commit -m "feat(tools): scaffold tool specs + ToolExecutor; add ignore/grep deps"
```

---

### Task 2: ToolExecutor.read_file + execute 派发 + 越权防护

**Files:**
- Modify: `src/tools/mod.rs`

**Interfaces:**
- Consumes: Task 1 的 `ToolCall`/`ToolResult`/`ToolExecutor`。
- Produces: `pub fn execute(&self, call: &ToolCall) -> ToolResult`（本任务实现 read_file 分支 + 未知工具兜底；search/list 在 Task 3/4 补）。execute **永不 panic、永不返回 Err**——工具错误以 `ToolResult.content` 里的 `ERROR: ...` 文本回灌给模型。

- [ ] **Step 1: 写失败测试**

在 `src/tools/mod.rs` 的 `mod tests` 中追加：

```rust
    use std::io::Write;

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall { id: "t1".into(), name: name.into(), args }
    }

    fn write_file(dir: &std::path::Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn read_file_returns_numbered_lines() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.rs", "fn main() {}\nlet x = 1;\n");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("read_file", json!({ "path": "a.rs" })));
        assert!(r.content.contains("1\tfn main() {}"), "got: {}", r.content);
        assert!(r.content.contains("2\tlet x = 1;"), "got: {}", r.content);
        assert_eq!(r.id, "t1");
    }

    #[test]
    fn read_file_honors_line_range() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.rs", "l1\nl2\nl3\nl4\n");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("read_file", json!({ "path": "a.rs", "start": 2, "end": 3 })));
        assert!(r.content.contains("2\tl2"));
        assert!(r.content.contains("3\tl3"));
        assert!(!r.content.contains("l1"));
        assert!(!r.content.contains("l4"));
    }

    #[test]
    fn read_file_missing_is_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("read_file", json!({ "path": "nope.rs" })));
        assert!(r.content.starts_with("ERROR:"), "got: {}", r.content);
    }

    #[test]
    fn read_file_rejects_path_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("read_file", json!({ "path": "../secret" })));
        assert!(r.content.contains("ERROR: path escapes root"), "got: {}", r.content);
        let r2 = ex.execute(&call("read_file", json!({ "path": "/etc/hosts" })));
        assert!(r2.content.contains("ERROR: path escapes root"), "got: {}", r2.content);
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("frobnicate", json!({})));
        assert!(r.content.contains("ERROR: unknown tool"), "got: {}", r.content);
    }
```

- [ ] **Step 2: 跑测试看失败**

Run: `cargo test --lib tools::`
Expected: FAIL（`execute` 未定义）。

- [ ] **Step 3: 实现**

在 `src/tools/mod.rs` 的 `impl ToolExecutor` 块里（`new` 之后）加常量与方法。先在文件顶部 `use` 区补 `use std::path::{Component, Path};`：

```rust
const READ_MAX_BYTES: usize = 64 * 1024;

impl ToolExecutor {
    /// 把模型给的相对路径安全地拼到 root 下；拒绝绝对路径与 `..`。
    fn safe_join(&self, rel: &str) -> Option<PathBuf> {
        let p = Path::new(rel);
        if p.is_absolute() {
            return None;
        }
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return None;
        }
        Some(self.root.join(p))
    }

    pub fn execute(&self, call: &ToolCall) -> ToolResult {
        let content = match call.name.as_str() {
            "read_file" => self.do_read_file(&call.args),
            // search_repo / list_files 在 Task 3/4 接入
            other => format!("ERROR: unknown tool: {other}"),
        };
        ToolResult { id: call.id.clone(), content }
    }

    fn do_read_file(&self, args: &Value) -> String {
        let path = match args.get("path").and_then(Value::as_str) {
            Some(p) => p,
            None => return "ERROR: read_file requires a string 'path'".into(),
        };
        let full = match self.safe_join(path) {
            Some(p) => p,
            None => return format!("ERROR: path escapes root: {path}"),
        };
        let text = match std::fs::read_to_string(&full) {
            Ok(t) => t,
            Err(e) => return format!("ERROR: cannot read {path}: {e}"),
        };
        let start = args.get("start").and_then(Value::as_u64);
        let end = args.get("end").and_then(Value::as_u64);
        let mut out = String::new();
        let mut bytes = 0usize;
        for (i, line) in text.lines().enumerate() {
            let lnum = (i as u64) + 1;
            if let Some(s) = start {
                if lnum < s {
                    continue;
                }
            }
            if let Some(e) = end {
                if lnum > e {
                    break;
                }
            }
            let row = format!("{lnum:>6}\t{line}\n");
            bytes += row.len();
            if bytes > READ_MAX_BYTES {
                out.push_str("... [truncated]\n");
                break;
            }
            out.push_str(&row);
        }
        if out.is_empty() {
            out.push_str("(no lines in range)\n");
        }
        out
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib tools::`
Expected: 全部 PASS（含 Task 1 的 3 个）。

- [ ] **Step 5: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add src/tools/mod.rs
git commit -m "feat(tools): read_file with line numbers, range, traversal guard; execute dispatch"
```

---

### Task 3: ToolExecutor.list_files（gitignore 感知遍历）

**Files:**
- Modify: `src/tools/mod.rs`

**Interfaces:**
- Consumes: Task 2 的 `execute` 派发、`safe_join`。
- Produces: list_files 分支（接到 execute）。

- [ ] **Step 1: 写失败测试**

在 `mod tests` 追加：

```rust
    #[test]
    fn list_files_lists_tracked_excludes_gitignored() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/a.rs", "x");
        write_file(dir.path(), "src/b.rs", "y");
        write_file(dir.path(), "target/junk.o", "z");
        write_file(dir.path(), ".gitignore", "target/\n");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("list_files", json!({})));
        assert!(r.content.contains("src/a.rs"), "got: {}", r.content);
        assert!(r.content.contains("src/b.rs"), "got: {}", r.content);
        assert!(!r.content.contains("junk.o"), "gitignored file leaked: {}", r.content);
    }

    #[test]
    fn list_files_scopes_to_subpath() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/a.rs", "x");
        write_file(dir.path(), "docs/readme.md", "y");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("list_files", json!({ "path": "docs" })));
        assert!(r.content.contains("docs/readme.md"), "got: {}", r.content);
        assert!(!r.content.contains("src/a.rs"), "got: {}", r.content);
    }
```

- [ ] **Step 2: 跑测试看失败**

Run: `cargo test --lib tools::list_files`
Expected: FAIL（list_files 仍走 unknown-tool 兜底）。

- [ ] **Step 3: 实现**

在文件顶部 `use` 区加 `use ignore::WalkBuilder;`。在 `execute` 的 match 里把 `// search_repo / list_files ...` 注释那行替换为：

```rust
            "list_files" => self.do_list_files(&call.args),
```

在 `impl ToolExecutor` 里加常量与方法：

```rust
const LIST_MAX_ENTRIES: usize = 500;

impl ToolExecutor {
    /// 计算遍历起点：给了 path 则用 safe_join 后的子路径，否则 root。
    fn scope_dir(&self, args: &Value) -> Result<PathBuf, String> {
        match args.get("path").and_then(Value::as_str) {
            Some(p) => self.safe_join(p).ok_or_else(|| format!("ERROR: path escapes root: {p}")),
            None => Ok(self.root.clone()),
        }
    }

    fn do_list_files(&self, args: &Value) -> String {
        let scope = match self.scope_dir(args) {
            Ok(p) => p,
            Err(e) => return e,
        };
        // require_git(false)：即便目录非 git 仓也尊重 .gitignore（评审目标未必是 git 仓）。
        let walk = WalkBuilder::new(&scope).require_git(false).build();
        let mut out = String::new();
        let mut n = 0usize;
        for dent in walk {
            let dent = match dent {
                Ok(d) => d,
                Err(_) => continue,
            };
            if !dent.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }
            let rel = dent
                .path()
                .strip_prefix(&self.root)
                .unwrap_or(dent.path())
                .to_string_lossy();
            out.push_str(&rel);
            out.push('\n');
            n += 1;
            if n >= LIST_MAX_ENTRIES {
                out.push_str("... [truncated]\n");
                break;
            }
        }
        if out.is_empty() {
            out.push_str("(no files)\n");
        }
        out
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib tools::`
Expected: 全部 PASS。

- [ ] **Step 5: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add src/tools/mod.rs
git commit -m "feat(tools): list_files via ignore walk (gitignore-aware, scoped, capped)"
```

---

### Task 4: ToolExecutor.search_repo（grep 正则搜索）

**Files:**
- Modify: `src/tools/mod.rs`

**Interfaces:**
- Consumes: Task 2/3 的 `execute`、`scope_dir`、`safe_join`。
- Produces: search_repo 分支（接到 execute）。执行器自此功能完整。

- [ ] **Step 1: 写失败测试**

在 `mod tests` 追加：

```rust
    #[test]
    fn search_repo_finds_matches_with_locations() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "src/a.rs", "let token = secret();\nok\n");
        write_file(dir.path(), "src/b.rs", "no match here\n");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("search_repo", json!({ "pattern": "secret" })));
        assert!(r.content.contains("src/a.rs:1:"), "got: {}", r.content);
        assert!(r.content.contains("token = secret"), "got: {}", r.content);
        assert!(!r.content.contains("b.rs"), "got: {}", r.content);
    }

    #[test]
    fn search_repo_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "keep.rs", "needle\n");
        write_file(dir.path(), "build/gen.rs", "needle\n");
        write_file(dir.path(), ".gitignore", "build/\n");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("search_repo", json!({ "pattern": "needle" })));
        assert!(r.content.contains("keep.rs:1:"), "got: {}", r.content);
        assert!(!r.content.contains("gen.rs"), "gitignored hit leaked: {}", r.content);
    }

    #[test]
    fn search_repo_no_match_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.rs", "nothing\n");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("search_repo", json!({ "pattern": "zzz" })));
        assert!(r.content.contains("(no matches)"), "got: {}", r.content);
    }

    #[test]
    fn search_repo_bad_regex_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("search_repo", json!({ "pattern": "(" })));
        assert!(r.content.starts_with("ERROR:"), "got: {}", r.content);
    }
```

- [ ] **Step 2: 跑测试看失败**

Run: `cargo test --lib tools::search_repo`
Expected: FAIL（search_repo 仍走 unknown-tool 兜底）。

- [ ] **Step 3: 实现**

在文件顶部 `use` 区加：

```rust
use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
```

在 `execute` 的 match 里加分支（放在 list_files 那行旁）：

```rust
            "search_repo" => self.do_search_repo(&call.args),
```

在 `impl ToolExecutor` 里加常量与方法：

```rust
const SEARCH_MAX_MATCHES: usize = 100;

impl ToolExecutor {
    fn do_search_repo(&self, args: &Value) -> String {
        let pattern = match args.get("pattern").and_then(Value::as_str) {
            Some(p) => p,
            None => return "ERROR: search_repo requires a string 'pattern'".into(),
        };
        let scope = match self.scope_dir(args) {
            Ok(p) => p,
            Err(e) => return e,
        };
        let matcher = match RegexMatcher::new(pattern) {
            Ok(m) => m,
            Err(e) => return format!("ERROR: invalid regex: {e}"),
        };

        let mut out = String::new();
        let mut hits = 0usize;
        let walk = WalkBuilder::new(&scope).require_git(false).build();
        'outer: for dent in walk {
            let dent = match dent {
                Ok(d) => d,
                Err(_) => continue,
            };
            if !dent.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                continue;
            }
            let path = dent.path().to_path_buf();
            let rel = match path.strip_prefix(&self.root) {
                Ok(r) => r.to_string_lossy().to_string(),
                Err(_) => continue, // scope⊆root 保证必成功；防御性跳过，绝不暴露绝对路径
            };
            let mut searcher = SearcherBuilder::new().line_number(true).build();
            let mut local: Vec<(u64, String)> = Vec::new();
            let _ = searcher.search_path(
                &matcher,
                &path,
                UTF8(|lnum, line| {
                    local.push((lnum, line.trim_end().to_string()));
                    Ok(local.len() < SEARCH_MAX_MATCHES)
                }),
            );
            for (lnum, line) in local {
                out.push_str(&format!("{rel}:{lnum}:{line}\n"));
                hits += 1;
                if hits >= SEARCH_MAX_MATCHES {
                    out.push_str("... [truncated]\n");
                    break 'outer;
                }
            }
        }
        if out.is_empty() {
            out.push_str("(no matches)\n");
        }
        out
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib tools::`
Expected: 全部 PASS（read_file/list_files/search_repo + Task1 specs 共约 13 个）。

- [ ] **Step 5: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add src/tools/mod.rs
git commit -m "feat(tools): search_repo via grep-regex/grep-searcher (gitignore-aware, capped)"
```

---

### Task 5: ChatBackend 抽象 + run_tool_loop 驱动 + FakeChatBackend

**Files:**
- Modify: `src/provider/mod.rs`
- Create: `src/provider/tool_loop.rs`

**Interfaces:**
- Consumes: `crate::tools::{ToolCall, ToolResult, ToolSpec, tool_specs, ToolExecutor}`、`crate::provider::direct::parse_lens_output`、`crate::provider::{LensCall, ProviderError}`。
- Produces:
  - `pub enum ChatMessage { System(String), User(String), Assistant { text: String, tool_calls: Vec<ToolCall> }, ToolResults(Vec<ToolResult>) }`（`Clone+Debug+PartialEq`）
  - `pub struct AssistantTurn { pub text: String, pub tool_calls: Vec<ToolCall>, pub input_tokens: u64, pub output_tokens: u64 }`（`Clone+Debug`）
  - `#[async_trait] pub trait ChatBackend: Send + Sync { async fn turn(&self, messages: &[ChatMessage], tools: &[ToolSpec]) -> Result<AssistantTurn, ProviderError>; }`
  - `pub async fn run_tool_loop(backend: &dyn ChatBackend, executor: &ToolExecutor, system: &str, user_prompt: &str, max_iters: u32) -> Result<LensCall, ProviderError>`
  - `pub struct FakeChatBackend`（测试用，脚本化 turns 并记录收到的 messages）

- [ ] **Step 1: 注册模块**

修改 `src/provider/mod.rs`，在 `pub mod fake;` 后加：

```rust
pub mod tool_loop;
```

- [ ] **Step 2: 写失败测试**

创建 `src/provider/tool_loop.rs`，先放 use + 测试（实现 Step 3 补）：

```rust
use crate::provider::direct::parse_lens_output;
use crate::provider::{LensCall, ProviderError};
use crate::tools::{tool_specs, ToolCall, ToolExecutor, ToolResult, ToolSpec};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::Mutex;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    const FINDINGS_JSON: &str = r#"{"findings":[{"severity":"high","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r"}]}"#;

    fn final_turn(text: &str) -> AssistantTurn {
        AssistantTurn { text: text.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1 }
    }

    fn toolcall_turn(name: &str, args: serde_json::Value) -> AssistantTurn {
        AssistantTurn {
            text: String::new(),
            tool_calls: vec![ToolCall { id: "c1".into(), name: name.into(), args }],
            input_tokens: 2,
            output_tokens: 3,
        }
    }

    fn tmp_executor_with(rel: &str, body: &str) -> (tempfile::TempDir, ToolExecutor) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(rel);
        std::fs::File::create(&p).unwrap().write_all(body.as_bytes()).unwrap();
        let ex = ToolExecutor::new(dir.path());
        (dir, ex)
    }

    #[tokio::test]
    async fn final_only_returns_parsed_findings() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        let backend = FakeChatBackend::new(vec![final_turn(FINDINGS_JSON)]);
        let call = run_tool_loop(&backend, &ex, "sys", "review", 8).await.unwrap();
        assert_eq!(call.findings.len(), 1);
        assert_eq!(call.findings[0].title, "t");
        // 单轮 usage
        assert_eq!((call.input_tokens, call.output_tokens), (1, 1));
        // 没触发工具执行
        assert_eq!(backend.turn_count(), 1);
    }

    #[tokio::test]
    async fn tool_call_then_final_feeds_results_back() {
        let (_d, ex) = tmp_executor_with("a.rs", "needle here");
        let backend = FakeChatBackend::new(vec![
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            final_turn(FINDINGS_JSON),
        ]);
        let call = run_tool_loop(&backend, &ex, "sys", "review", 8).await.unwrap();
        assert_eq!(call.findings.len(), 1);
        // 两轮 usage 累加：(2+1, 3+1)
        assert_eq!((call.input_tokens, call.output_tokens), (3, 4));
        // 第二轮收到的 messages 必须含 ToolResults，且内容是真读到的文件文本
        let second = backend.messages_at(1);
        let fed = second.iter().any(|m| matches!(
            m, ChatMessage::ToolResults(rs) if rs.iter().any(|r| r.content.contains("needle here"))
        ));
        assert!(fed, "tool result not fed back: {second:?}");
    }

    #[tokio::test]
    async fn exceeding_max_iters_errors() {
        let (_d, ex) = tmp_executor_with("a.rs", "x");
        // 永远只发 tool_call，从不收敛
        let backend = FakeChatBackend::new(vec![
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
        ]);
        let err = run_tool_loop(&backend, &ex, "sys", "review", 2).await.unwrap_err();
        assert!(matches!(err, ProviderError::Parse(_)), "got: {err:?}");
    }
}
```

- [ ] **Step 3: 跑测试看失败**

Run: `cargo test --lib provider::tool_loop`
Expected: FAIL（类型/函数未定义）。

- [ ] **Step 4: 实现**

在 `src/provider/tool_loop.rs` 的 use 之后、`#[cfg(test)]` 之前插入：

```rust
/// provider 无关的会话消息。各 backend 自行渲染到本家协议。
#[derive(Clone, Debug, PartialEq)]
pub enum ChatMessage {
    System(String),
    User(String),
    Assistant { text: String, tool_calls: Vec<ToolCall> },
    ToolResults(Vec<ToolResult>),
}

/// 一轮模型输出：要么带 tool_calls（需执行后续轮），要么是最终文本。
#[derive(Clone, Debug)]
pub struct AssistantTurn {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// 单轮往返抽象：给定历史 + 工具声明，返回模型这一轮的输出。
#[async_trait]
pub trait ChatBackend: Send + Sync {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError>;
}

/// 多轮工具循环：发→若 tool_calls 则执行回灌→直至最终文本→解析为 findings。
/// usage 跨轮累加；超过 max_iters 仍未收敛则 Parse 错误。
pub async fn run_tool_loop(
    backend: &dyn ChatBackend,
    executor: &ToolExecutor,
    system: &str,
    user_prompt: &str,
    max_iters: u32,
) -> Result<LensCall, ProviderError> {
    let tools = tool_specs();
    let mut messages = vec![
        ChatMessage::System(system.to_string()),
        ChatMessage::User(user_prompt.to_string()),
    ];
    let mut in_tok = 0u64;
    let mut out_tok = 0u64;

    for _ in 0..max_iters {
        let turn = backend.turn(&messages, &tools).await?;
        in_tok += turn.input_tokens;
        out_tok += turn.output_tokens;

        if turn.tool_calls.is_empty() {
            let out = parse_lens_output(&turn.text)?;
            return Ok(LensCall {
                findings: out.findings,
                input_tokens: in_tok,
                output_tokens: out_tok,
            });
        }

        let results: Vec<ToolResult> =
            turn.tool_calls.iter().map(|c| executor.execute(c)).collect();
        messages.push(ChatMessage::Assistant {
            text: turn.text,
            tool_calls: turn.tool_calls,
        });
        messages.push(ChatMessage::ToolResults(results));
    }

    Err(ProviderError::Parse(format!(
        "tool loop exceeded {max_iters} iterations without final output"
    )))
}

/// 测试替身：按序吐预置 turns，并记录每轮收到的 messages 快照。
pub struct FakeChatBackend {
    turns: Mutex<VecDeque<AssistantTurn>>,
    seen: Mutex<Vec<Vec<ChatMessage>>>,
}

impl FakeChatBackend {
    pub fn new(turns: Vec<AssistantTurn>) -> Self {
        FakeChatBackend {
            turns: Mutex::new(turns.into()),
            seen: Mutex::new(Vec::new()),
        }
    }
    pub fn turn_count(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
    pub fn messages_at(&self, idx: usize) -> Vec<ChatMessage> {
        self.seen.lock().unwrap()[idx].clone()
    }
}

#[async_trait]
impl ChatBackend for FakeChatBackend {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        _tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError> {
        self.seen.lock().unwrap().push(messages.to_vec());
        self.turns
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ProviderError::Unavailable("no scripted turn".into()))
    }
}
```

- [ ] **Step 5: 跑测试**

Run: `cargo test --lib provider::tool_loop`
Expected: 3 个测试 PASS。

- [ ] **Step 6: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add src/provider/mod.rs src/provider/tool_loop.rs
git commit -m "feat(provider): ChatBackend abstraction + run_tool_loop driver + FakeChatBackend"
```

---

### Task 6: ToolLoopProvider（drop-in ReviewProvider）

**Files:**
- Modify: `src/provider/tool_loop.rs`

**Interfaces:**
- Consumes: Task 5 的 `ChatBackend`/`run_tool_loop`、`crate::tools::ToolExecutor`、`crate::provider::{ReviewProvider, LensRequest, LensCall, ProviderError}`。
- Produces:
  - `pub struct ToolLoopProvider`（持 `Box<dyn ChatBackend>` + `ToolExecutor` + system + max_iters）
  - `pub fn new(backend: Box<dyn ChatBackend>, root: std::path::PathBuf) -> ToolLoopProvider`（默认 system + `DEFAULT_MAX_ITERS`）
  - `impl ReviewProvider for ToolLoopProvider`：`run(LensRequest)` 用 `req.prompt` 作 user_prompt 跑 `run_tool_loop`。

- [ ] **Step 1: 写失败测试**

在 `src/provider/tool_loop.rs` 的 `mod tests` 追加：

```rust
    use crate::provider::{LensRequest, ReviewProvider};

    #[tokio::test]
    async fn tool_loop_provider_is_drop_in_review_provider() {
        let (_d, ex_root) = {
            let dir = tempfile::tempdir().unwrap();
            std::fs::File::create(dir.path().join("a.rs"))
                .unwrap()
                .write_all(b"needle here")
                .unwrap();
            let root = dir.path().to_path_buf();
            (dir, root)
        };
        let backend = FakeChatBackend::new(vec![
            toolcall_turn("read_file", json!({ "path": "a.rs" })),
            final_turn(FINDINGS_JSON),
        ]);
        let provider = ToolLoopProvider::new(Box::new(backend), ex_root);
        // 经 trait 对象调用，证明 drop-in
        let p: &dyn ReviewProvider = &provider;
        let call = p
            .run(LensRequest { model: "any".into(), prompt: "review".into() })
            .await
            .unwrap();
        assert_eq!(call.findings.len(), 1);
        assert_eq!((call.input_tokens, call.output_tokens), (3, 4));
    }
```

- [ ] **Step 2: 跑测试看失败**

Run: `cargo test --lib provider::tool_loop`
Expected: FAIL（`ToolLoopProvider` 未定义）。

- [ ] **Step 3: 实现**

在 `src/provider/tool_loop.rs` 顶部 use 区补：

```rust
use crate::provider::{LensRequest, ReviewProvider};
use std::path::PathBuf;
```

在非测试区（`FakeChatBackend` 之前或之后均可，放 `run_tool_loop` 之后）加：

```rust
const DEFAULT_MAX_ITERS: u32 = 12;
const DEFAULT_SYSTEM: &str =
    "You are an adversarial code reviewer. Use the provided tools to read and search the \
     codebase as needed, then return ONLY JSON matching the requested schema (an object with a \
     \"findings\" array).";

/// 把一个 ChatBackend 与一个 ToolExecutor 组合成可直接喂给编排层的 ReviewProvider。
pub struct ToolLoopProvider {
    backend: Box<dyn ChatBackend>,
    executor: ToolExecutor,
    system: String,
    max_iters: u32,
}

impl ToolLoopProvider {
    pub fn new(backend: Box<dyn ChatBackend>, root: PathBuf) -> Self {
        ToolLoopProvider {
            backend,
            executor: ToolExecutor::new(root),
            system: DEFAULT_SYSTEM.to_string(),
            max_iters: DEFAULT_MAX_ITERS,
        }
    }
}

#[async_trait]
impl ReviewProvider for ToolLoopProvider {
    async fn run(&self, req: LensRequest) -> Result<LensCall, ProviderError> {
        run_tool_loop(
            self.backend.as_ref(),
            &self.executor,
            &self.system,
            &req.prompt,
            self.max_iters,
        )
        .await
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib provider::tool_loop`
Expected: 4 个测试 PASS。

- [ ] **Step 5: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add src/provider/tool_loop.rs
git commit -m "feat(provider): ToolLoopProvider composes ChatBackend + ToolExecutor as ReviewProvider"
```

---

### Task 7: AnthropicBackend（tools 协议适配）

**Files:**
- Modify: `src/provider/mod.rs`
- Create: `src/provider/backends.rs`

**Interfaces:**
- Consumes: `crate::provider::tool_loop::{ChatBackend, ChatMessage, AssistantTurn}`、`crate::tools::{ToolCall, ToolSpec}`、`crate::provider::ProviderError`。
- Produces:
  - 纯函数 `pub(crate) fn build_anthropic_tools_body(model: &str, messages: &[ChatMessage], tools: &[ToolSpec], max_tokens: u64) -> serde_json::Value`
  - 纯函数 `pub(crate) fn parse_anthropic_turn(resp: &serde_json::Value) -> Option<AssistantTurn>`
  - `pub struct AnthropicBackend { model, base_url: Option<String>, api_key, client }` + `pub fn new(model, base_url, api_key)` + `impl ChatBackend`

- [ ] **Step 1: 注册模块**

修改 `src/provider/mod.rs`，在 `pub mod tool_loop;` 后加：

```rust
pub mod backends;
```

- [ ] **Step 2: 写失败测试**

创建 `src/provider/backends.rs`，先放 use + 测试：

```rust
use crate::provider::tool_loop::{AssistantTurn, ChatBackend, ChatMessage};
use crate::provider::ProviderError;
use crate::tools::{ToolCall, ToolSpec};
use async_trait::async_trait;
use serde_json::{json, Value};

const ANTHROPIC_DEFAULT_BASE: &str = "https://api.anthropic.com";
const MAX_TOKENS: u64 = 8192;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::tool_specs;

    fn specs() -> Vec<ToolSpec> {
        tool_specs()
    }

    #[test]
    fn anthropic_body_has_system_tools_and_messages() {
        let msgs = vec![
            ChatMessage::System("sys".into()),
            ChatMessage::User("review".into()),
        ];
        let b = build_anthropic_tools_body("claude-x", &msgs, &specs(), MAX_TOKENS);
        assert_eq!(b["model"], "claude-x");
        assert_eq!(b["system"], "sys");
        // tools 用 input_schema（Anthropic 协议字段名）
        assert_eq!(b["tools"][0]["name"], "read_file");
        assert!(b["tools"][0]["input_schema"].is_object());
        // system 不进 messages；user 进
        assert_eq!(b["messages"][0]["role"], "user");
        assert_eq!(b["messages"][0]["content"], "review");
    }

    #[test]
    fn anthropic_body_renders_assistant_toolcall_and_toolresult() {
        let msgs = vec![
            ChatMessage::User("review".into()),
            ChatMessage::Assistant {
                text: "looking".into(),
                tool_calls: vec![ToolCall {
                    id: "tu1".into(),
                    name: "read_file".into(),
                    args: json!({ "path": "a.rs" }),
                }],
            },
            ChatMessage::ToolResults(vec![crate::tools::ToolResult {
                id: "tu1".into(),
                content: "file body".into(),
            }]),
        ];
        let b = build_anthropic_tools_body("m", &msgs, &specs(), MAX_TOKENS);
        // assistant 消息含 tool_use 块
        let asst = &b["messages"][1];
        assert_eq!(asst["role"], "assistant");
        let tu = asst["content"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["type"] == "tool_use")
            .unwrap();
        assert_eq!(tu["id"], "tu1");
        assert_eq!(tu["name"], "read_file");
        assert_eq!(tu["input"]["path"], "a.rs");
        // tool_result 作为 user 消息块，tool_use_id 对应
        let tr = &b["messages"][2];
        assert_eq!(tr["role"], "user");
        assert_eq!(tr["content"][0]["type"], "tool_result");
        assert_eq!(tr["content"][0]["tool_use_id"], "tu1");
        assert_eq!(tr["content"][0]["content"], "file body");
    }

    #[test]
    fn parse_anthropic_tool_use_turn() {
        let resp = json!({
            "content": [
                { "type": "text", "text": "let me look" },
                { "type": "tool_use", "id": "tu9", "name": "read_file", "input": { "path": "a.rs" } }
            ],
            "usage": { "input_tokens": 10, "output_tokens": 20 }
        });
        let t = parse_anthropic_turn(&resp).unwrap();
        assert_eq!(t.text, "let me look");
        assert_eq!(t.tool_calls.len(), 1);
        assert_eq!(t.tool_calls[0].id, "tu9");
        assert_eq!(t.tool_calls[0].name, "read_file");
        assert_eq!(t.tool_calls[0].args["path"], "a.rs");
        assert_eq!((t.input_tokens, t.output_tokens), (10, 20));
    }

    #[test]
    fn parse_anthropic_final_text_turn() {
        let resp = json!({
            "content": [{ "type": "text", "text": "{\"findings\":[]}" }],
            "usage": { "input_tokens": 1, "output_tokens": 2 }
        });
        let t = parse_anthropic_turn(&resp).unwrap();
        assert!(t.tool_calls.is_empty());
        assert_eq!(t.text, "{\"findings\":[]}");
    }

    #[test]
    fn parse_anthropic_missing_content_is_none() {
        assert!(parse_anthropic_turn(&json!({ "id": "x" })).is_none());
    }
}
```

- [ ] **Step 3: 跑测试看失败**

Run: `cargo test --lib provider::backends`
Expected: FAIL（函数未定义）。

- [ ] **Step 4: 实现纯函数 + backend**

在 `src/provider/backends.rs` 的 use 之后、`#[cfg(test)]` 之前插入：

```rust
/// 把 provider 无关的 messages 渲染成 Anthropic Messages API 请求体。
/// system 提到顶层；tool_use / tool_result 用 content 块表达。
pub(crate) fn build_anthropic_tools_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
    max_tokens: u64,
) -> Value {
    let mut system = String::new();
    let mut msgs: Vec<Value> = Vec::new();
    for m in messages {
        match m {
            ChatMessage::System(s) => system = s.clone(),
            ChatMessage::User(u) => msgs.push(json!({ "role": "user", "content": u })),
            ChatMessage::Assistant { text, tool_calls } => {
                let mut blocks: Vec<Value> = Vec::new();
                if !text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": text }));
                }
                for c in tool_calls {
                    blocks.push(json!({
                        "type": "tool_use", "id": c.id, "name": c.name, "input": c.args
                    }));
                }
                msgs.push(json!({ "role": "assistant", "content": blocks }));
            }
            ChatMessage::ToolResults(results) => {
                let blocks: Vec<Value> = results
                    .iter()
                    .map(|r| json!({
                        "type": "tool_result", "tool_use_id": r.id, "content": r.content
                    }))
                    .collect();
                msgs.push(json!({ "role": "user", "content": blocks }));
            }
        }
    }
    let tool_decls: Vec<Value> = tools
        .iter()
        .map(|t| json!({
            "name": t.name, "description": t.description, "input_schema": t.parameters
        }))
        .collect();
    json!({
        "model": model,
        "max_tokens": max_tokens,
        "system": system,
        "tools": tool_decls,
        "messages": msgs,
    })
}

/// 从 Anthropic 响应抽取这一轮：text 块拼接 + tool_use 块 → ToolCall；usage 取不到则 0。
pub(crate) fn parse_anthropic_turn(resp: &Value) -> Option<AssistantTurn> {
    let blocks = resp.get("content")?.as_array()?;
    let mut text = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                tool_calls.push(ToolCall {
                    id: b.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                    name: b.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                    args: b.get("input").cloned().unwrap_or_else(|| json!({})),
                });
            }
            _ => {}
        }
    }
    let input_tokens = resp["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let output_tokens = resp["usage"]["output_tokens"].as_u64().unwrap_or(0);
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens })
}

/// 直连 Anthropic 的 ChatBackend（rustls-only，沿用 direct.rs 的 HTTP 形状）。
pub struct AnthropicBackend {
    model: String,
    base_url: Option<String>,
    api_key: String,
    client: reqwest::Client,
}

impl AnthropicBackend {
    pub fn new(model: String, base_url: Option<String>, api_key: String) -> Self {
        AnthropicBackend { model, base_url, api_key, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl ChatBackend for AnthropicBackend {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError> {
        let base = self.base_url.as_deref().unwrap_or(ANTHROPIC_DEFAULT_BASE);
        let url = format!("{}/v1/messages", base.trim_end_matches('/'));
        let body = build_anthropic_tools_body(&self.model, messages, tools, MAX_TOKENS);
        let resp: Value = self
            .client
            .post(&url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?
            .json()
            .await
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
        parse_anthropic_turn(&resp)
            .ok_or_else(|| ProviderError::Parse("anthropic response missing content".into()))
    }
}
```

- [ ] **Step 5: 跑测试**

Run: `cargo test --lib provider::backends`
Expected: 5 个测试 PASS。

- [ ] **Step 6: clippy + 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add src/provider/mod.rs src/provider/backends.rs
git commit -m "feat(provider): AnthropicBackend tool-use adapter (pure body+parse, HTTP)"
```

---

### Task 8: OpenAiBackend + backend_from_config 工厂

**Files:**
- Modify: `src/provider/backends.rs`

**Interfaces:**
- Consumes: Task 7 的同文件类型、`crate::config::{ProviderConfig, ProviderKind}`。
- Produces:
  - 纯函数 `pub(crate) fn build_openai_tools_body(model: &str, messages: &[ChatMessage], tools: &[ToolSpec]) -> serde_json::Value`
  - 纯函数 `pub(crate) fn parse_openai_turn(resp: &serde_json::Value) -> Option<AssistantTurn>`
  - `pub struct OpenAiBackend { model, base_url, api_key, client }` + `pub fn new(...)` + `impl ChatBackend`
  - `pub fn backend_from_config(pc: &ProviderConfig, api_key: String) -> Result<Box<dyn ChatBackend>, ProviderError>`（S2-2b 用它从配置造 backend；Google → Unavailable）

- [ ] **Step 1: 写失败测试**

在 `src/provider/backends.rs` 的 `mod tests` 追加：

```rust
    use crate::config::{ProviderConfig, ProviderKind};

    #[test]
    fn openai_body_has_messages_and_function_tools() {
        let msgs = vec![
            ChatMessage::System("sys".into()),
            ChatMessage::User("review".into()),
        ];
        let b = build_openai_tools_body("gpt-x", &msgs, &specs());
        assert_eq!(b["model"], "gpt-x");
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["role"], "user");
        // tools 用 {type:function, function:{name, parameters}}
        assert_eq!(b["tools"][0]["type"], "function");
        assert_eq!(b["tools"][0]["function"]["name"], "read_file");
        assert!(b["tools"][0]["function"]["parameters"].is_object());
    }

    #[test]
    fn openai_body_renders_assistant_toolcall_and_tool_message() {
        let msgs = vec![
            ChatMessage::Assistant {
                text: String::new(),
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    args: json!({ "path": "a.rs" }),
                }],
            },
            ChatMessage::ToolResults(vec![crate::tools::ToolResult {
                id: "call_1".into(),
                content: "file body".into(),
            }]),
        ];
        let b = build_openai_tools_body("m", &msgs, &specs());
        let asst = &b["messages"][0];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["tool_calls"][0]["id"], "call_1");
        assert_eq!(asst["tool_calls"][0]["type"], "function");
        assert_eq!(asst["tool_calls"][0]["function"]["name"], "read_file");
        // arguments 是 JSON 字符串
        assert_eq!(asst["tool_calls"][0]["function"]["arguments"], "{\"path\":\"a.rs\"}");
        // tool 结果消息
        let tr = &b["messages"][1];
        assert_eq!(tr["role"], "tool");
        assert_eq!(tr["tool_call_id"], "call_1");
        assert_eq!(tr["content"], "file body");
    }

    #[test]
    fn parse_openai_tool_call_turn() {
        let resp = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_7",
                        "type": "function",
                        "function": { "name": "search_repo", "arguments": "{\"pattern\":\"x\"}" }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 6 }
        });
        let t = parse_openai_turn(&resp).unwrap();
        assert_eq!(t.text, "");
        assert_eq!(t.tool_calls.len(), 1);
        assert_eq!(t.tool_calls[0].id, "call_7");
        assert_eq!(t.tool_calls[0].name, "search_repo");
        assert_eq!(t.tool_calls[0].args["pattern"], "x");
        assert_eq!((t.input_tokens, t.output_tokens), (5, 6));
    }

    #[test]
    fn parse_openai_final_text_turn() {
        let resp = json!({
            "choices": [{ "message": { "content": "{\"findings\":[]}" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let t = parse_openai_turn(&resp).unwrap();
        assert!(t.tool_calls.is_empty());
        assert_eq!(t.text, "{\"findings\":[]}");
    }

    #[test]
    fn parse_openai_missing_choices_is_none() {
        assert!(parse_openai_turn(&json!({ "id": "x" })).is_none());
    }

    #[test]
    fn from_config_maps_kind_to_backend() {
        let anth = ProviderConfig {
            kind: ProviderKind::Anthropic, model: "m".into(),
            base_url: None, api_key_env: None, priority: 0,
        };
        assert!(backend_from_config(&anth, "k".into()).is_ok());
        let goog = ProviderConfig {
            kind: ProviderKind::Google, model: "m".into(),
            base_url: None, api_key_env: None, priority: 0,
        };
        assert!(matches!(
            backend_from_config(&goog, "k".into()),
            Err(ProviderError::Unavailable(_))
        ));
    }
```

- [ ] **Step 2: 跑测试看失败**

Run: `cargo test --lib provider::backends`
Expected: FAIL（openai 相关 + 工厂未定义）。

- [ ] **Step 3: 实现**

在 `src/provider/backends.rs` use 区补：

```rust
use crate::config::{ProviderConfig, ProviderKind};
```

在文件顶部常量区补：

```rust
const OPENAI_DEFAULT_BASE: &str = "https://api.openai.com/v1";
```

在 `AnthropicBackend` 的 ChatBackend 实现之后插入：

```rust
/// 把 messages 渲染成 OpenAI 兼容 Chat Completions 请求体。
/// system/user 直接成消息；assistant tool_calls 用 function 形状；tool 结果用 role:"tool"。
pub(crate) fn build_openai_tools_body(
    model: &str,
    messages: &[ChatMessage],
    tools: &[ToolSpec],
) -> Value {
    let mut msgs: Vec<Value> = Vec::new();
    for m in messages {
        match m {
            ChatMessage::System(s) => msgs.push(json!({ "role": "system", "content": s })),
            ChatMessage::User(u) => msgs.push(json!({ "role": "user", "content": u })),
            ChatMessage::Assistant { text, tool_calls } => {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|c| json!({
                        "id": c.id,
                        "type": "function",
                        "function": { "name": c.name, "arguments": c.args.to_string() }
                    }))
                    .collect();
                msgs.push(json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { json!(text) },
                    "tool_calls": calls
                }));
            }
            ChatMessage::ToolResults(results) => {
                for r in results {
                    msgs.push(json!({
                        "role": "tool", "tool_call_id": r.id, "content": r.content
                    }));
                }
            }
        }
    }
    let tool_decls: Vec<Value> = tools
        .iter()
        .map(|t| json!({
            "type": "function",
            "function": {
                "name": t.name, "description": t.description, "parameters": t.parameters
            }
        }))
        .collect();
    json!({ "model": model, "messages": msgs, "tools": tool_decls })
}

/// 从 OpenAI 兼容响应抽取这一轮：message.content（可空）+ tool_calls（arguments 是 JSON 字符串）。
pub(crate) fn parse_openai_turn(resp: &Value) -> Option<AssistantTurn> {
    let msg = resp.get("choices")?.as_array()?.first()?.get("message")?;
    let text = msg.get("content").and_then(Value::as_str).unwrap_or("").to_string();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for c in calls {
            let f = match c.get("function") {
                Some(f) => f,
                None => continue,
            };
            let args_str = f.get("arguments").and_then(Value::as_str).unwrap_or("{}");
            let args = serde_json::from_str::<Value>(args_str).unwrap_or_else(|_| json!({}));
            tool_calls.push(ToolCall {
                id: c.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                name: f.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
                args,
            });
        }
    }
    let input_tokens = resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let output_tokens = resp["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    Some(AssistantTurn { text, tool_calls, input_tokens, output_tokens })
}

/// 直连 OpenAI 兼容端点的 ChatBackend。
pub struct OpenAiBackend {
    model: String,
    base_url: Option<String>,
    api_key: String,
    client: reqwest::Client,
}

impl OpenAiBackend {
    pub fn new(model: String, base_url: Option<String>, api_key: String) -> Self {
        OpenAiBackend { model, base_url, api_key, client: reqwest::Client::new() }
    }
}

#[async_trait]
impl ChatBackend for OpenAiBackend {
    async fn turn(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolSpec],
    ) -> Result<AssistantTurn, ProviderError> {
        let base = self.base_url.as_deref().unwrap_or(OPENAI_DEFAULT_BASE);
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let body = build_openai_tools_body(&self.model, messages, tools);
        let resp: Value = self
            .client
            .post(&url)
            .header("authorization", format!("Bearer {}", self.api_key))
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?
            .json()
            .await
            .map_err(|e| ProviderError::Unavailable(e.to_string()))?;
        parse_openai_turn(&resp)
            .ok_or_else(|| ProviderError::Parse("openai response missing choices".into()))
    }
}

/// 按 ProviderConfig.kind 造对应 backend（S2-2b 编排层入口）。Google 暂不支持。
pub fn backend_from_config(
    pc: &ProviderConfig,
    api_key: String,
) -> Result<Box<dyn ChatBackend>, ProviderError> {
    match pc.kind {
        ProviderKind::Anthropic => Ok(Box::new(AnthropicBackend::new(
            pc.model.clone(),
            pc.base_url.clone(),
            api_key,
        ))),
        ProviderKind::Openai | ProviderKind::OpenaiCompatible => Ok(Box::new(OpenAiBackend::new(
            pc.model.clone(),
            pc.base_url.clone(),
            api_key,
        ))),
        ProviderKind::Google => Err(ProviderError::Unavailable("google not yet supported".into())),
    }
}
```

- [ ] **Step 4: 跑测试**

Run: `cargo test --lib provider::backends`
Expected: 11 个测试 PASS。

- [ ] **Step 5: 全套 + clippy + 提交**

```bash
cargo test
cargo clippy --all-targets -- -D warnings
git add src/provider/backends.rs
git commit -m "feat(provider): OpenAiBackend tool adapter + backend_from_config factory"
```

Expected: 全库测试绿（Plan-1 既有 62 + 本阶段新增约 28，合计约 90，数字以实际为准）；clippy 干净。

---

### Task 9（可选 smoke）: 真实网络冒烟（Anthropic + OpenAI 工具循环）

> 仅在你手上有 key 且想端到端验真链路时做；CI 不跑。若略过，本阶段已可由 Fake 全量验证。

**Files:**
- Modify: `src/provider/backends.rs`

**Interfaces:** Consumes Task 6 `ToolLoopProvider`、Task 7/8 backends。

- [ ] **Step 1: 加 ignored smoke 测试**

在 `src/provider/backends.rs` 的 `mod tests` 追加：

```rust
    /// 真实网络冒烟：需 key + 网络，手动 `--ignored` 跑。
    /// `BRK_SMOKE_ANTHROPIC_KEY=<key> cargo test -- --ignored smoke_anthropic_tool_loop`
    #[tokio::test]
    #[ignore = "requires real API key + network"]
    async fn smoke_anthropic_tool_loop() {
        use crate::provider::tool_loop::ToolLoopProvider;
        use crate::provider::{LensRequest, ReviewProvider};
        let key = std::env::var("BRK_SMOKE_ANTHROPIC_KEY").expect("set BRK_SMOKE_ANTHROPIC_KEY");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn main(){ let p=\"secret\"; }").unwrap();
        let backend = AnthropicBackend::new("claude-3-5-haiku-latest".into(), None, key);
        let p = ToolLoopProvider::new(Box::new(backend), dir.path().to_path_buf());
        let call = p
            .run(LensRequest {
                model: "x".into(),
                prompt: "Read a.rs with the read_file tool, then return JSON {\"findings\":[]} if clean.".into(),
            })
            .await
            .expect("real anthropic tool loop");
        // 只断言链路通：拿到了可解析的 findings 数组（可能为空）
        let _ = call.findings.len();
    }
```

- [ ] **Step 2: 跑普通套件确认 ignored 不影响 CI**

Run: `cargo test --lib provider::backends`
Expected: smoke 显示为 ignored，其余 PASS。

- [ ] **Step 3: 提交**

```bash
cargo clippy --all-targets -- -D warnings
git add src/provider/backends.rs
git commit -m "test(provider): ignored network smoke for anthropic tool loop"
```

---

## 范围外（明确不在 S2-2a）

- 文件分包 / 确定性文件筛选 / lens→rule / `bundle×model` 编排 / positioning → **S2-2b**。
- `max_concurrency` 信号量限流 → **S2-2b**（真并发在分包扇出引入；本阶段引擎单条循环无并发需求）。路线文档映射相应更新。
- 把 `ToolLoopProvider` 接成 `main.rs` 默认评审路径 → **S2-2b**。
- 多模型 / targeted-verify → **S3**。

---

## Self-Review

**1. Spec coverage（对照路线 S2-2a 条目）**
- tool 执行器（read/search/list，ripgrep crate，零外部命令）→ Task 1–4 ✓
- 多轮 loop 驱动（发→执行→回灌→终止）→ Task 5 ✓
- Anthropic tools + OpenAI fn-calling 两 adapter → Task 7/8 ✓
- 单发 prompt-embed-schema 兜底保留 → 不动 `direct.rs`，`parse_lens_output` 复用 ✓（Global Constraints 固化）
- FakeProvider 脚本化 tool-call 隔离测试 → `FakeChatBackend`（Task 5/6）✓
- `max_concurrency` 信号量 → 经评估移 S2-2b，已在"范围外"标注并将更新路线映射 ✓（对路线的有意偏离，已显式记录）

**2. Placeholder scan**：无 TBD/TODO；每个 code step 含完整可编译代码与确切命令/预期。

**3. Type consistency**：`ToolCall{id,name,args}`/`ToolResult{id,content}` 全程一致；`ChatMessage` 四变体在 Task 5 定义、Task 7/8 渲染时字段名一致（`Assistant{text,tool_calls}`、`ToolResults(Vec<ToolResult>)`）；`AssistantTurn{text,tool_calls,input_tokens,output_tokens}` 在驱动与两 parser 一致；`build_anthropic_tools_body` 用 `input_schema`、`build_openai_tools_body` 用 `function.parameters`，与各自 parser 对称；`backend_from_config` 返回 `Box<dyn ChatBackend>` 与 `ToolLoopProvider::new` 入参吻合；复用既有 `LensCall`/`ProviderError`/`parse_lens_output`/`ProviderConfig`/`ProviderKind` 签名均按现状引用。
