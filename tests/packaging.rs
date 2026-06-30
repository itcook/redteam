fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

fn json(p: &str) -> serde_json::Value {
    serde_json::from_str(&read(p)).unwrap_or_else(|e| panic!("parse {p}: {e}"))
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
