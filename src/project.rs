//! 项目级 `.breakpoint/` 目录：骨架初始化与 --dismissed 输入解析（spec §1.2/§1.4）。
use std::collections::HashSet;
use std::path::Path;

pub const PROJECT_DIR: &str = ".breakpoint";

const GITIGNORE_BODY: &str = "*\n";
const CONFIG_TEMPLATE: &str = "\
# breakpoint 项目级配置（优先级高于用户级 ~/.config/breakpoint/config.toml）。
# 密钥走环境变量或用户级 secrets.toml——绝不写入本文件（本目录在仓库领地内）。
#
# [providers.MyProvider]
# kind = \"openai\"          # openai | anthropic | openai-compatible
# base_url = \"https://…\"
# model = \"…\"
# api_key_env = \"MY_KEY_ENV\"
# priority = 0
#
# lgtm 循环的测试命令（可选；不设则按 Cargo.toml/package.json/Makefile 推断）：
# test_command = \"cargo test\"
";

/// 幂等创建 `.breakpoint/` 骨架，返回本次新建的条目；已存在的一律不动。
pub fn init_project(root: &Path) -> anyhow::Result<Vec<String>> {
    let dir = root.join(PROJECT_DIR);
    let mut created = Vec::new();
    for sub in ["dismissed", "rounds"] {
        let p = dir.join(sub);
        if !p.is_dir() {
            std::fs::create_dir_all(&p)?;
            created.push(format!("{PROJECT_DIR}/{sub}/"));
        }
    }
    for (name, body) in [(".gitignore", GITIGNORE_BODY), ("config.toml", CONFIG_TEMPLATE)] {
        let p = dir.join(name);
        if !p.exists() {
            std::fs::write(&p, body)?;
            created.push(format!("{PROJECT_DIR}/{name}"));
        }
    }
    if root.join(".breakpoint.toml").exists() {
        eprintln!("breakpoint: legacy .breakpoint.toml is no longer read; move it into .breakpoint/config.toml");
    }
    Ok(created)
}

fn is_hex16(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// R8.2 `--dismissed <csv|dir>`：目录 → 文件名即 id（`<16hex>.md`，零内容解析）；
/// 否则 csv，逐 token 校验 16 位小写十六进制。
pub fn parse_dismissed(value: &str) -> anyhow::Result<HashSet<String>> {
    let p = Path::new(value);
    if p.is_dir() {
        let mut out = HashSet::new();
        for entry in std::fs::read_dir(p)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(".md") {
                if is_hex16(stem) {
                    out.insert(stem.to_string());
                }
            }
        }
        return Ok(out); // 空目录合法：无驳回
    }
    let mut out = HashSet::new();
    for tok in value.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        anyhow::ensure!(
            is_hex16(tok),
            "--dismissed: '{tok}' is neither an existing directory nor a 16-hex finding id"
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
        assert_eq!(created.len(), 4, "dismissed/ rounds/ .gitignore config.toml");
        assert!(tmp.path().join(".breakpoint/dismissed").is_dir());
        assert!(tmp.path().join(".breakpoint/rounds").is_dir());
        assert_eq!(std::fs::read_to_string(tmp.path().join(".breakpoint/.gitignore")).unwrap(), "*\n");
        // 二跑：全部已存在 → 零创建
        assert!(init_project(tmp.path()).unwrap().is_empty());
    }

    #[test]
    fn init_never_overwrites_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".breakpoint")).unwrap();
        std::fs::write(tmp.path().join(".breakpoint/.gitignore"), "custom\n").unwrap();
        init_project(tmp.path()).unwrap();
        assert_eq!(std::fs::read_to_string(tmp.path().join(".breakpoint/.gitignore")).unwrap(), "custom\n");
    }

    #[test]
    fn parse_dismissed_csv_valid_and_invalid() {
        let s = parse_dismissed("d2b371819297f98a, 20ba9b3025a8b421").unwrap();
        assert_eq!(s.len(), 2);
        assert!(parse_dismissed("not-an-id").is_err());
        assert!(parse_dismissed("").is_err());
        assert!(parse_dismissed("D2B371819297F98A").is_err(), "大写不合法（id 恒小写）");
    }

    #[test]
    fn parse_dismissed_dir_takes_filenames_only() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("d2b371819297f98a.md"), "任意内容 不解析").unwrap();
        std::fs::write(tmp.path().join("README.md"), "x").unwrap();
        std::fs::write(tmp.path().join("20ba9b3025a8b421.txt"), "x").unwrap();
        let s = parse_dismissed(tmp.path().to_str().unwrap()).unwrap();
        assert_eq!(s, std::collections::HashSet::from(["d2b371819297f98a".to_string()]));
    }
}
