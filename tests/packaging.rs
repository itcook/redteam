fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

fn json(p: &str) -> serde_json::Value {
    serde_json::from_str(&read(p)).unwrap_or_else(|e| panic!("parse {p}: {e}"))
}

fn frontmatter<'a>(body: &'a str, key: &str) -> Option<String> {
    // 取前两个 "---" 之间、形如 "key: value" 的值
    let mut lines = body.lines();
    assert_eq!(lines.next(), Some("---"), "frontmatter must start with ---");
    for l in lines {
        if l == "---" { break; }
        if let Some(v) = l.strip_prefix(&format!("{key}:")) {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[test]
fn manifests_valid_and_named() {
    // marketplace.json
    let m = json(".claude-plugin/marketplace.json");
    assert_eq!(m["name"], "breakpoint", "marketplace name");
    assert_eq!(m["owner"]["name"], "S.Kai", "owner.name");
    let plugins = m["plugins"].as_array().expect("plugins array");
    let entry = plugins.iter().find(|p| p["name"] == "breakpoint").expect("breakpoint entry");
    assert_eq!(entry["source"], "./plugins/breakpoint", "relative source");
    assert!(entry.get("version").is_none(), "version must live in plugin.json only");

    // plugin.json
    let p = json("plugins/breakpoint/.claude-plugin/plugin.json");
    assert_eq!(p["name"], "breakpoint", "plugin name → namespace");
    assert_eq!(p["version"], "1.0.0", "plugin version is authoritative");
    assert_eq!(p["author"]["email"], "laiwei@me.com");

    // .codex-plugin/plugin.json
    let c = json("plugins/breakpoint/.codex-plugin/plugin.json");
    assert_eq!(c["name"], "breakpoint");
    assert_eq!(c["skills"], "codex-skills", "codex skills dir → no Claude collision");
}

#[test]
fn commands_frontmatter_and_invocation_policy() {
    for name in ["review", "status", "result"] {
        let body = read(&format!("plugins/breakpoint/commands/{name}.md"));
        assert!(frontmatter(&body, "description").is_some(), "{name}: description required");
        assert!(
            body.contains("${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64"),
            "{name}: must invoke bundled binary via plugin root"
        );
        assert!(body.contains("$ARGUMENTS"), "{name}: must forward $ARGUMENTS");
    }
    // review 计费重 → 禁模型自动调用
    let review = read("plugins/breakpoint/commands/review.md");
    assert_eq!(frontmatter(&review, "disable-model-invocation").as_deref(), Some("true"),
        "review must disable model invocation");
    // status/result 轻读 → 允许模型触发（不设该字段）
    for name in ["status", "result"] {
        let body = read(&format!("plugins/breakpoint/commands/{name}.md"));
        assert!(frontmatter(&body, "disable-model-invocation").is_none(),
            "{name} must allow model invocation");
    }
}
