use std::path::Path;
use std::process::Command;

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

fn list_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap()
            .map(|e| e.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() { walk(&p, out); } else { out.push(p); }
        }
    }
    walk(root, &mut out);
    out
}

#[test]
fn codex_skills_in_sync_with_commands() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let status = Command::new("bash")
        .arg("scripts/gen-codex-skills")
        .arg(tmp.path())
        .status()
        .expect("run gen-codex-skills");
    assert!(status.success(), "gen-codex-skills exit non-zero");

    let committed = Path::new("plugins/breakpoint/codex-skills");
    let gen_files = list_files(tmp.path());
    let com_files = list_files(committed);

    // 文件集合（相对路径）一致
    let rel = |base: &Path, fs: &[std::path::PathBuf]| -> Vec<String> {
        fs.iter().map(|p| p.strip_prefix(base).unwrap().to_string_lossy().into_owned()).collect()
    };
    assert_eq!(rel(committed, &com_files), rel(tmp.path(), &gen_files),
        "codex-skills 文件集合漂移：commands/ 改了未重跑 gen-codex-skills？");

    // 内容逐字节一致
    for r in rel(committed, &com_files) {
        let a = std::fs::read(committed.join(&r)).unwrap();
        let b = std::fs::read(tmp.path().join(&r)).unwrap();
        assert_eq!(a, b, "codex-skills/{r} 内容漂移：commands/ 改了未重跑 gen-codex-skills");
    }
}

#[test]
fn codex_skills_have_name_and_no_claude_var() {
    for name in ["review", "status", "result"] {
        let s = read(&format!("plugins/breakpoint/codex-skills/{name}/SKILL.md"));
        assert!(s.contains(&format!("name: {name}")), "{name}: skill must declare name");
        assert!(!s.contains("CLAUDE_PLUGIN_ROOT"),
            "{name}: CLAUDE_PLUGIN_ROOT must be rewritten to CODEX_PLUGIN_ROOT");
        assert!(s.contains("${CODEX_PLUGIN_ROOT}/bin/brk-darwin-arm64"),
            "{name}: codex skill must invoke bundled binary");
    }
}
