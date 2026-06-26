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
    // root 在 Task 2-4 的工具执行方法中使用；此处仅骨架。
    #[allow(dead_code)]
    pub(crate) root: PathBuf,
}

impl ToolExecutor {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        ToolExecutor { root: root.into() }
    }
}

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
