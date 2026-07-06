use grep_regex::RegexMatcher;
use grep_searcher::sinks::UTF8;
use grep_searcher::SearcherBuilder;
use ignore::WalkBuilder;
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};

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
            name: "read_files",
            description: "Read MULTIPLE files at once under the review root, each returned with line numbers. \
                          Prefer this over repeated read_file calls: batch every file you anticipate needing \
                          into a SINGLE call to save round-trips. Total output is bounded — an oversized batch \
                          is truncated with a note; fetch the remainder or precise line ranges with read_file.",
            parameters: json!({
                "type": "object",
                "properties": {
                    "paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Paths relative to the review root."
                    }
                },
                "required": ["paths"]
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
    /// 符号链接解析后的 root 真实路径，用于 canonical 逃逸检查。
    canonical_root: PathBuf,
}

const READ_MAX_BYTES: usize = 64 * 1024;
/// read_files 批量总预算（≈2× 单文件帽）：超即严格截断到 char 边界 + 注记，模型改用 read_file 取余。
/// 硬界护 context——批量读的总输出有上限，绝不比逐个 read_file 更撑 context（可调，待 A/B 校准）。
const READ_FILES_MAX_BYTES: usize = 128 * 1024;
const LIST_MAX_ENTRIES: usize = 500;
const SEARCH_MAX_MATCHES: usize = 100;

impl ToolExecutor {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let canonical_root = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        ToolExecutor { root, canonical_root }
    }

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
            "read_files" => self.do_read_files(&call.args),
            "list_files" => self.do_list_files(&call.args),
            "search_repo" => self.do_search_repo(&call.args),
            other => format!("ERROR: unknown tool: {other}"),
        };
        ToolResult { id: call.id.clone(), content }
    }

    fn do_read_file(&self, args: &Value) -> String {
        let path = match args.get("path").and_then(Value::as_str) {
            Some(p) => p,
            None => return "ERROR: read_file requires a string 'path'".into(),
        };
        let start = args.get("start").and_then(Value::as_u64);
        let end = args.get("end").and_then(Value::as_u64);
        self.read_one(path, start, end)
    }

    /// 批量读多个整文件（延迟主线：一轮拿全所需文件，砍 tool-loop 轮数——单数 read_file 的
    /// affordance 把模型锁进「一轮一读」串行到撞满 max_iters）。总输出受 READ_FILES_MAX_BYTES
    /// 严格约束（截到 char 边界 + 注记），超额让模型改用 read_file 取余，故不比逐个读更撑 context。
    fn do_read_files(&self, args: &Value) -> String {
        let paths = match args.get("paths").and_then(Value::as_array) {
            Some(a) => a,
            None => return "ERROR: read_files requires an array 'paths'".into(),
        };
        if paths.is_empty() {
            return "ERROR: read_files requires a non-empty 'paths' array".into();
        }
        let mut out = String::new();
        for pv in paths {
            let rel = match pv.as_str() {
                Some(s) => s,
                None => continue, // 忽略非字符串项
            };
            out.push_str(&format!("===== {rel} =====\n"));
            out.push_str(&self.read_one(rel, None, None));
            out.push('\n');
            if out.len() > READ_FILES_MAX_BYTES {
                let mut cut = READ_FILES_MAX_BYTES;
                while cut > 0 && !out.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.truncate(cut);
                out.push_str("\n... [read_files: total budget reached; request remaining files or line ranges with read_file]\n");
                break;
            }
        }
        out
    }

    /// 读单文件为带行号字符串（词法 safe_join + canonicalize 符号链接逃逸防护 + READ_MAX_BYTES
    /// 帽 + 可选行段）。返回值可能是 "ERROR: …" 文案（工具结果即字符串，错误内联给模型）。
    /// read_file / read_files 共用此实现。
    fn read_one(&self, path: &str, start: Option<u64>, end: Option<u64>) -> String {
        let full = match self.safe_join(path) {
            Some(p) => p,
            None => return format!("ERROR: path escapes root: {path}"),
        };
        // 先词法 safe_join，再 canonicalize 做符号链接逃逸防护：真实路径必须仍在 root 内。
        let real = match std::fs::canonicalize(&full) {
            Ok(r) => r,
            Err(e) => return format!("ERROR: cannot read {path}: {e}"),
        };
        if !real.starts_with(&self.canonical_root) {
            return format!("ERROR: path escapes root: {path}");
        }
        // 有界读取：最多 READ_MAX_BYTES+1 字节进内存，避免超大/二进制文件全量读入 (I2/T2)。
        use std::io::Read;
        let file = match std::fs::File::open(&real) {
            Ok(f) => f,
            Err(e) => return format!("ERROR: cannot read {path}: {e}"),
        };
        let mut text = String::new();
        if let Err(e) = file.take((READ_MAX_BYTES as u64) + 1).read_to_string(&mut text) {
            return format!("ERROR: cannot read {path}: {e}");
        }
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

    /// 计算遍历起点：给了 path 则用 safe_join 后的子路径，否则 root。
    /// canonicalize 成功且不在 canonical_root 内时拒绝（符号链接逃逸防护）。
    fn scope_dir(&self, args: &Value) -> Result<PathBuf, String> {
        match args.get("path").and_then(Value::as_str) {
            Some(p) => {
                let scope = self.safe_join(p).ok_or_else(|| format!("ERROR: path escapes root: {p}"))?;
                // 若符号链接解析成功，检查真实路径是否在 canonical_root 内。
                if let Ok(real) = std::fs::canonicalize(&scope) {
                    if !real.starts_with(&self.canonical_root) {
                        return Err(format!("ERROR: path escapes root: {p}"));
                    }
                }
                // canonicalize 失败（路径不存在）→ 正常返回，walk 会遍历空集。
                Ok(scope)
            }
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
            let rel = match dent.path().strip_prefix(&self.root) {
                Ok(r) => r.to_string_lossy().into_owned(),
                Err(_) => continue, // scope⊆root 保证必成功；防御性跳过，绝不暴露绝对路径
            };
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

#[cfg(test)]
mod tests {
    use super::*;
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
    fn read_files_batches_multiple_with_headers() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.rs", "fn a() {}\n");
        write_file(dir.path(), "b.rs", "fn b() {}\n");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("read_files", json!({ "paths": ["a.rs", "b.rs"] })));
        assert!(r.content.contains("===== a.rs ====="), "含 a 头: {}", r.content);
        assert!(r.content.contains("===== b.rs ====="), "含 b 头");
        assert!(r.content.contains("fn a() {}") && r.content.contains("fn b() {}"), "两文件正文都在");
    }

    #[test]
    fn read_files_total_budget_truncates_with_note() {
        let dir = tempfile::tempdir().unwrap();
        // 三个 ~50KB 单长行文件 → 合计 ~150KB > READ_FILES_MAX_BYTES(128KB) → 严格截断带注记
        for f in ["a.rs", "b.rs", "c.rs"] {
            write_file(dir.path(), f, &format!("{}\n", "x".repeat(50_000)));
        }
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("read_files", json!({ "paths": ["a.rs", "b.rs", "c.rs"] })));
        assert!(r.content.len() <= READ_FILES_MAX_BYTES + 200, "总输出受硬界约束，实得 {}", r.content.len());
        assert!(r.content.contains("total budget reached"), "带预算截断注记");
    }

    #[test]
    fn read_files_bad_path_is_inline_error_not_whole_failure() {
        let dir = tempfile::tempdir().unwrap();
        write_file(dir.path(), "a.rs", "good\n");
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("read_files", json!({ "paths": ["a.rs", "../secret"] })));
        assert!(r.content.contains("good"), "好文件仍读到");
        assert!(r.content.contains("ERROR: path escapes root"), "坏路径内联报错不整体失败: {}", r.content);
    }

    #[test]
    fn read_files_empty_or_missing_paths_is_error() {
        let dir = tempfile::tempdir().unwrap();
        let ex = ToolExecutor::new(dir.path());
        assert!(ex.execute(&call("read_files", json!({ "paths": [] }))).content.starts_with("ERROR:"), "空 paths 报错");
        assert!(ex.execute(&call("read_files", json!({}))).content.starts_with("ERROR:"), "缺 paths 报错");
    }

    #[test]
    fn unknown_tool_is_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("frobnicate", json!({})));
        assert!(r.content.contains("ERROR: unknown tool"), "got: {}", r.content);
    }

    #[test]
    fn tool_specs_lists_named_tools() {
        let specs = tool_specs();
        let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["read_file", "read_files", "search_repo", "list_files"]);
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

    // C1 安全回归测试：root 内的符号链接指向 root 外时，read_file 必须拒绝。
    #[cfg(unix)]
    #[test]
    fn read_file_rejects_in_root_symlink_escape() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "TOP SECRET").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.path().join("escape")).unwrap();
        let ex = ToolExecutor::new(root.path());
        let r = ex.execute(&call("read_file", json!({ "path": "escape" })));
        assert!(r.content.contains("ERROR: path escapes root"), "symlink escape not blocked: {}", r.content);
        assert!(!r.content.contains("TOP SECRET"), "leaked host file: {}", r.content);
    }

    // M5 search_repo 命中数超过 100 条时应截断并输出 `... [truncated]`。
    #[test]
    fn search_repo_caps_at_100_matches_with_truncated() {
        let dir = tempfile::tempdir().unwrap();
        // 写入 200 行匹配的文件，确保超过 SEARCH_MAX_MATCHES=100。
        let body: String = (1..=200).map(|i| format!("needle_{i}\n")).collect();
        write_file(dir.path(), "big.rs", &body);
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("search_repo", json!({ "pattern": "needle" })));
        assert!(r.content.contains("... [truncated]"), "expected truncated marker, got: {}", r.content);
        // 计算 path:line: 格式行数，不超过 100。
        let result_lines = r.content.lines().filter(|l| l.contains("big.rs:")).count();
        assert!(result_lines <= 100, "expected ≤100 result lines, got {result_lines}");
    }
}
