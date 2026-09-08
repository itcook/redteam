//! 项目级 `.redteam/` 目录：骨架初始化与 --dismissed 输入解析（spec §1.2/§1.4）。
use std::collections::HashSet;
use std::path::Path;

pub const PROJECT_DIR: &str = ".redteam";

pub(crate) const GITIGNORE_BODY: &str = "*\n";

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
            if v.get("scripts").and_then(|s| s.get("test")).and_then(|t| t.as_str()).is_some() {
                return Some("npm test".into());
            }
        }
    }
    if let Ok(mk) = std::fs::read_to_string(root.join("Makefile")) {
        if mk.lines().any(|l| l.starts_with("test:")) {
            return Some("make test".into());
        }
    }
    if root.join("go.mod").is_file() {
        return Some("go test ./...".into());
    }
    None
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
        assert_eq!(std::fs::read_to_string(tmp.path().join(".redteam/.gitignore")).unwrap(), "*\n");
        // 二跑：全部已存在 → 零创建
        assert!(init_project(tmp.path()).unwrap().is_empty());
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
    fn parse_dismissed_csv_valid_and_invalid() {
        let s = parse_dismissed("d2b371819297f98a, 20ba9b3025a8b421").unwrap();
        assert_eq!(s.len(), 2);
        assert!(parse_dismissed("not-an-id").is_err());
        assert!(parse_dismissed("").is_err());
        assert!(parse_dismissed("D2B371819297F98A").is_err(), "大写不合法（id 恒小写）");
    }

}
