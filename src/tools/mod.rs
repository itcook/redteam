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

const READ_MAX_BYTES: usize = 64 * 1024;
const LIST_MAX_ENTRIES: usize = 500;

impl ToolExecutor {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ToolExecutor { root: root.into() }
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
            "list_files" => self.do_list_files(&call.args),
            // search_repo 在 Task 4 接入
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
    fn unknown_tool_is_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let ex = ToolExecutor::new(dir.path());
        let r = ex.execute(&call("frobnicate", json!({})));
        assert!(r.content.contains("ERROR: unknown tool"), "got: {}", r.content);
    }

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
}
