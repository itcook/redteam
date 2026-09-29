//! 项目级 `.redteam/` 目录：骨架初始化与 --dismissed 输入解析（spec §1.2/§1.4）。
use std::collections::HashSet;
use std::path::Path;

pub const PROJECT_DIR: &str = ".redteam";

/// `.redteam/` 默认整目录忽略（里面是状态：proj.db、packs/、rounds/），
/// **但 `brief.md` 例外**——它是内容不是状态（design §10.5「内容非配置」），
/// 要随仓分发：团队与 CI 都得读得到，否则写了 brief 也只有本机那一份（M21d 批 B-1 实测发现）。
pub(crate) const GITIGNORE_BODY: &str = "*\n!.gitignore\n!brief.md\n";

/// 老项目的 `.gitignore` 升级（原封不动的旧默认 `*` → 放行 brief.md）。
///
/// **不能只挂在 `redteam init` 上**：存量用户早就 init 过了，不会专门再跑一次，于是
/// `.redteam/brief.md` 照旧被吞掉——他们本机 brief 生效、团队与 CI 读不到，正是这条修复要治的病
/// （2026-09-29 `/code-review` 指出）。`review` / `--pack-only` 写 `.redteam/` 时一并调用。
/// 用户改过的一律不碰（只认原封不动的旧默认这一种形态）。
pub fn upgrade_legacy_gitignore(root: &Path) -> bool {
    let gi = root.join(PROJECT_DIR).join(".gitignore");
    if std::fs::read_to_string(&gi).map(|c| c.trim() == "*").unwrap_or(false) {
        return std::fs::write(&gi, GITIGNORE_BODY).is_ok();
    }
    false
}

/// 幂等创建 `.redteam/` 骨架（`.gitignore` / `rounds/` / `db::DB_FILE`），返回本次新建的条目；已存在的一律不动。
/// 项目层 `config.toml` 已退场（M21a 批 A，D7 A8：配置用户级唯一，项目目录只放状态与内容）；
/// dismissed/ 目录已随台账机器化退役（design 2026-07-03：台账/评审史统一 proj.db）。
pub fn init_project(root: &Path) -> anyhow::Result<Vec<String>> {
    let dir = root.join(PROJECT_DIR);
    let mut created = Vec::new();
    let rounds = dir.join("rounds");
    if !rounds.is_dir() {
        std::fs::create_dir_all(&rounds)?;
        created.push(format!("{PROJECT_DIR}/rounds/"));
    }
    let gi = dir.join(".gitignore");
    if !gi.exists() {
        std::fs::write(&gi, GITIGNORE_BODY)?;
        created.push(format!("{PROJECT_DIR}/.gitignore"));
    } else if upgrade_legacy_gitignore(root) {
        created.push(format!("{PROJECT_DIR}/.gitignore（升级：brief.md 不再被忽略）"));
    }
    let db_path = dir.join(crate::db::DB_FILE);
    if !db_path.exists() {
        crate::db::ProjDb::open_at(&db_path)?;
        created.push(format!("{PROJECT_DIR}/{}", crate::db::DB_FILE));
    }
    Ok(created)
}

/// ship 循环的测试命令推断（M21a 批 A K1；项目层 `test_command` 随项目 config 退场）：
/// Cargo.toml → `cargo test`；package.json 有 `scripts.test` → `npm test`；Makefile 有 `test:` 目标 →
/// `make test`；go.mod → `go test ./...`；都没有 → None（`redteam test-command` 退出码 2）。
/// `--test-command` / `--override` 覆盖本推断。
pub fn infer_test_command(root: &Path) -> Option<String> {
    if root.join("Cargo.toml").is_file() {
        return Some("cargo test".into());
    }
    if let Ok(pkg) = std::fs::read_to_string(root.join("package.json")) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&pkg) {
            // npm init 的占位脚本 `echo "Error: no test specified" && exit 1` 不算有测试（#11）。
            if let Some(t) = v.get("scripts").and_then(|s| s.get("test")).and_then(|t| t.as_str()) {
                if !t.trim().is_empty() && !t.contains("no test specified") {
                    return Some("npm test".into());
                }
            }
        }
    }
    if let Ok(mk) = std::fs::read_to_string(root.join("Makefile")) {
        if mk.lines().any(is_make_test_target) {
            return Some("make test".into());
        }
    }
    if root.join("go.mod").is_file() {
        return Some("go test ./...".into());
    }
    None
}

/// Makefile 行是否为 `test` 目标行：`test:` / `test :` / `test: deps` / `test::`，
/// 而非 `test:=` / `test ::=` 变量赋值，也不是 `tests:` / `test-unit:` 等别的目标（#11）。
fn is_make_test_target(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("test") else { return false };
    let rest = rest.trim_start_matches([' ', '\t']);
    let Some(after) = rest.strip_prefix(':') else { return false };
    !after.starts_with('=') && !after.starts_with(":=")
}

pub fn is_hex16(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `--dismissed` csv 形态解析：逐 token 校验 16 位小写十六进制（仅豁免，无判例内容）。
/// dir 形态已退役（design 2026-07-03：台账机器化进 .redteam/proj.db，见 cli::load_ledger）。
pub fn parse_dismissed(value: &str) -> anyhow::Result<HashSet<String>> {
    let mut out = HashSet::new();
    for tok in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        anyhow::ensure!(
            is_hex16(tok),
            "--dismissed: '{tok}' is neither an existing ledger db file nor a 16-hex finding id"
        );
        out.insert(tok.to_string());
    }
    anyhow::ensure!(!out.is_empty(), "--dismissed: empty value");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_skeleton_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let created = init_project(tmp.path()).unwrap();
        assert_eq!(created.len(), 3, "rounds/ .gitignore db");
        assert!(!tmp.path().join(".redteam/config.toml").exists(), "项目层 config 退场（D7 A8）");
        assert!(tmp.path().join(".redteam/rounds").is_dir());
        assert!(tmp.path().join(".redteam").join(crate::db::DB_FILE).is_file(), "init 建库（M21a-b 并行期文件名见 db::DB_FILE）");
        assert!(!tmp.path().join(".redteam/dismissed").exists(), "dismissed/ 目录已退役");
        // `brief.md` 要随仓分发（design §10.5「内容非配置」）——整目录忽略会让写了 brief 的人
        // 只在本机有那一份，团队与 CI 都读不到（M21d 批 B-1 实测发现）。
        let gi = std::fs::read_to_string(tmp.path().join(".redteam/.gitignore")).unwrap();
        assert_eq!(gi, "*\n!.gitignore\n!brief.md\n");
        // 二跑：全部已存在 → 零创建
        assert!(init_project(tmp.path()).unwrap().is_empty());
    }

    /// 老项目（批 B-1 之前建的）`.gitignore` 是光秃秃一个 `*`，brief.md 会被吞掉 → 升级它；
    /// **用户改过的一律不碰**（只认原封不动的旧默认这一种形态）。
    #[test]
    fn init_upgrades_only_the_untouched_legacy_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".redteam")).unwrap();
        std::fs::write(tmp.path().join(".redteam/.gitignore"), "*\n").unwrap();
        let created = init_project(tmp.path()).unwrap();
        assert!(created.iter().any(|c| c.contains("brief.md 不再被忽略")), "{created:?}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(".redteam/.gitignore")).unwrap(),
            "*\n!.gitignore\n!brief.md\n"
        );

        // 用户自定义的不动
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp2.path().join(".redteam")).unwrap();
        std::fs::write(tmp2.path().join(".redteam/.gitignore"), "*\n!mine.txt\n").unwrap();
        init_project(tmp2.path()).unwrap();
        assert_eq!(std::fs::read_to_string(tmp2.path().join(".redteam/.gitignore")).unwrap(), "*\n!mine.txt\n");
    }

    #[test]
    fn init_never_overwrites_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".redteam")).unwrap();
        std::fs::write(tmp.path().join(".redteam/.gitignore"), "custom\n").unwrap();
        init_project(tmp.path()).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join(".redteam/.gitignore")).unwrap(), "custom\n");
    }

    #[test]
    fn infer_test_command_four_ecosystems_and_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(infer_test_command(tmp.path()), None);
        std::fs::write(tmp.path().join("go.mod"), "module x\n").unwrap();
        assert_eq!(infer_test_command(tmp.path()).as_deref(), Some("go test ./..."));
        std::fs::write(tmp.path().join("Makefile"), "build:\n\techo\ntest: build\n\tcargo test\n").unwrap();
        assert_eq!(infer_test_command(tmp.path()).as_deref(), Some("make test"), "Makefile test 目标优先于 go.mod");
        std::fs::write(tmp.path().join("package.json"), r#"{"scripts":{"test":"vitest"}}"#).unwrap();
        assert_eq!(infer_test_command(tmp.path()).as_deref(), Some("npm test"));
        std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        assert_eq!(infer_test_command(tmp.path()).as_deref(), Some("cargo test"));
        // package.json 无 scripts.test 不算
        let t2 = tempfile::tempdir().unwrap();
        std::fs::write(t2.path().join("package.json"), r#"{"name":"x"}"#).unwrap();
        assert_eq!(infer_test_command(t2.path()), None);
    }

    #[test]
    fn infer_test_command_edge_cases() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("Makefile"), "test:=1\nbuild:\n\techo\n").unwrap();
        assert_eq!(infer_test_command(t.path()), None, "test:= 是变量赋值不是目标");
        std::fs::write(t.path().join("Makefile"), "test :\n\tcargo test\n").unwrap();
        assert_eq!(infer_test_command(t.path()).as_deref(), Some("make test"), "test : 允许空格");
        let t2 = tempfile::tempdir().unwrap();
        std::fs::write(t2.path().join("package.json"), r#"{"scripts":{"test":"echo \"Error: no test specified\" && exit 1"}}"#).unwrap();
        assert_eq!(infer_test_command(t2.path()), None, "npm init 占位脚本不算");
        std::fs::write(t2.path().join("package.json"), r#"{"scripts":{"test":""}}"#).unwrap();
        assert_eq!(infer_test_command(t2.path()), None, "空 test 脚本不算（评审 N7）");
        assert!(is_make_test_target("test: build") && is_make_test_target("test::") && is_make_test_target("test\t:"));
        assert!(!is_make_test_target("test ::= x") && !is_make_test_target("tests:") && !is_make_test_target("test-unit:"));
    }

    #[test]
    fn parse_dismissed_csv_valid_and_invalid() {
        let s = parse_dismissed("d2b371819297f98a, 20ba9b3025a8b421").unwrap();
        assert_eq!(s.len(), 2);
        assert!(parse_dismissed("not-an-id").is_err());
        assert!(parse_dismissed("").is_err());
        assert!(parse_dismissed("D2B371819297F98A").is_err(), "大写不合法（id 恒小写）");
    }

}
