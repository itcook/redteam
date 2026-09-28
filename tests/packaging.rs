use std::path::Path;
use std::process::Command;

fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

fn json(p: &str) -> serde_json::Value {
    serde_json::from_str(&read(p)).unwrap_or_else(|e| panic!("parse {p}: {e}"))
}

fn frontmatter(body: &str, key: &str) -> Option<String> {
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
    assert_eq!(m["name"], "redteam", "marketplace name");
    assert_eq!(m["owner"]["name"], "S.Kai", "owner.name");
    let plugins = m["plugins"].as_array().expect("plugins array");
    let entry = plugins.iter().find(|p| p["name"] == "redteam").expect("redteam entry");
    assert_eq!(entry["source"], "./plugins/redteam", "relative source");
    assert!(entry.get("version").is_none(), "version must live in plugin.json only");

    // plugin.json
    let p = json("plugins/redteam/.claude-plugin/plugin.json");
    assert_eq!(p["name"], "redteam", "plugin name → namespace");
    assert_eq!(p["version"], "1.0.0", "plugin version is authoritative");
    assert_eq!(p["author"]["email"], "laiwei@me.com");

    // .codex-plugin/plugin.json
    let c = json("plugins/redteam/.codex-plugin/plugin.json");
    assert_eq!(c["name"], "redteam");
    assert_eq!(c["skills"], "codex-skills", "codex skills dir → no Claude collision");
}

#[test]
fn commands_frontmatter_and_invocation_policy() {
    for name in ["review", "status", "result", "ship"] {
        let body = read(&format!("plugins/redteam/commands/{name}.md"));
        assert!(frontmatter(&body, "description").is_some(), "{name}: description required");
        assert!(
            body.contains("${CLAUDE_PLUGIN_ROOT}/bin/redteam "),
            "{name}: must invoke the bin/redteam launcher via plugin root"
        );
        assert!(
            !body.contains("redteam-darwin-arm64"),
            "{name}: must not reference the old flat binary name"
        );
        // 切名后（M21c 批 0）壳层一律指向 launcher `redteam`；这条断言留作防回归——
        // 防止有人把 M21 分支期的临时名 `redteam-next` 写回 commands。
        assert!(!body.contains("redteam-next"), "{name}: commands must point at the `redteam` launcher, never the retired branch name");
        assert!(body.contains("$ARGUMENTS"), "{name}: must forward $ARGUMENTS");
    }
    // review/ship 计费重（ship 是烧钱长循环，同 review 政策）→ 禁模型自动调用
    for name in ["review", "ship"] {
        let body = read(&format!("plugins/redteam/commands/{name}.md"));
        assert_eq!(frontmatter(&body, "disable-model-invocation").as_deref(), Some("true"),
            "{name} must disable model invocation");
    }
    // status/result 轻读 → 允许模型触发（不设该字段）
    for name in ["status", "result"] {
        let body = read(&format!("plugins/redteam/commands/{name}.md"));
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

    let committed = Path::new("plugins/redteam/codex-skills");
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
    for name in ["review", "status", "result", "ship"] {
        let s = read(&format!("plugins/redteam/codex-skills/{name}/SKILL.md"));
        assert!(s.contains(&format!("name: {name}")), "{name}: skill must declare name");
        assert!(!s.contains("CLAUDE_PLUGIN_ROOT"),
            "{name}: CLAUDE_PLUGIN_ROOT must be rewritten to CODEX_PLUGIN_ROOT");
        assert!(s.contains("${CODEX_PLUGIN_ROOT}/bin/redteam "),
            "{name}: codex skill must invoke the bin/redteam launcher");
        assert!(!s.contains("redteam-darwin-arm64"),
            "{name}: must not reference the old flat binary name");
        assert!(!s.contains("redteam-next"), "{name}: codex mirror must not reference the retired branch name");
    }
}

#[test]
fn binary_layout_and_launcher() {
    // launcher（稳定入口）存在且可执行
    let launcher = Path::new("plugins/redteam/bin/redteam");
    let lmeta = std::fs::metadata(launcher)
        .unwrap_or_else(|e| panic!("launcher bin/redteam missing: {e}"));
    // shipped darwin/aarch64 真二进制存在且非空
    let real = Path::new("plugins/redteam/bin/darwin/aarch64/redteam");
    let rmeta = std::fs::metadata(real)
        .unwrap_or_else(|e| panic!("darwin/aarch64 binary missing (run `make build`): {e}"));
    assert!(rmeta.len() > 0, "binary is empty");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(lmeta.permissions().mode() & 0o111 != 0, "launcher not executable");
        assert!(rmeta.permissions().mode() & 0o111 != 0, "binary not executable");
    }
    // 旧扁平二进制已删
    assert!(!Path::new("plugins/redteam/bin/redteam-darwin-arm64").exists(),
        "old flat binary must be removed");
    // launcher 内容：探测 os/arch + 127 报错
    let src = read("plugins/redteam/bin/redteam");
    assert!(src.contains("uname -s") && src.contains("uname -m"), "launcher must probe os/arch");
    assert!(src.contains("exit 127"), "launcher must exit 127 on resolution failure");
}

// launcher 命中：真机透传到真二进制。仅 darwin-arm64 本机有意义，跨平台跳过。
#[test]
#[cfg_attr(not(all(target_os = "macos", target_arch = "aarch64")), ignore)]
fn launcher_resolves_on_this_platform() {
    let ok = Command::new("plugins/redteam/bin/redteam")
        .arg("--help").output().expect("spawn launcher");
    assert!(ok.status.success(), "launcher --help should succeed on this platform");
}

// launcher 缺失分支：拷贝 launcher 到临时目录（无平台子目录）→ 127 + 报错。
// 不动 shipped 文件，避免与其它并行测试竞态；任意平台都能跑（sh 可用即可）。
#[test]
fn launcher_errors_when_binary_missing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let launcher = tmp.path().join("redteam");
    std::fs::copy("plugins/redteam/bin/redteam", &launcher).expect("copy launcher");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // 临时目录下没有 <os>/<arch>/redteam → launcher 命中缺失分支
    let miss = Command::new(&launcher).arg("review").output().expect("spawn launcher");
    assert_eq!(miss.status.code(), Some(127), "missing binary → exit 127");
    assert!(String::from_utf8_lossy(&miss.stderr).contains("no binary for"),
        "missing binary → clear error: {}", String::from_utf8_lossy(&miss.stderr));
}
