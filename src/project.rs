//! 项目级 `.redteam/` 目录：骨架初始化与 --dismissed 输入解析（spec §1.2/§1.4）。
use std::collections::HashSet;
use std::path::Path;

pub const PROJECT_DIR: &str = ".redteam";

const GITIGNORE_BODY: &str = "*\n";
const CONFIG_TEMPLATE: &str = "\
# redteam 项目级配置（优先级高于用户级 ~/.config/redteam/config.toml）。
# 密钥走环境变量或用户级 secrets.toml——绝不写入本文件（本目录在仓库领地内）。
#
# [providers.MyProvider]
# kind = \"openai\"          # openai | anthropic | openai-compatible
# base_url = \"https://…\"
# model = \"…\"
# api_key_env = \"MY_KEY_ENV\"
# priority = 0
#
# ship 循环的测试命令（可选；不设则按 Cargo.toml/package.json/Makefile 推断）：
# test_command = \"cargo test\"
";

/// 幂等创建 `.redteam/` 骨架，返回本次新建的条目；已存在的一律不动。
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
    Ok(created)
}

pub fn is_hex16(s: &str) -> bool {
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
            "--dismissed: '{tok}' is neither an existing directory nor a 16-hex finding id \
             (for the ledger dir, run 'redteam init' to create .redteam/dismissed/)"
        );
        out.insert(tok.to_string());
    }
    anyhow::ensure!(!out.is_empty(), "--dismissed: empty value");
    Ok(out)
}

/// 内置命令优先（2026-07-02）：`--dismissed <dir>` 时载入台账内容，供 prompt 注入
/// 「勿再报」段。零格式解析：消费者是评审模型，原样注入即可（截断防爆，按 char 截）。
/// csv 形态无内容可读 → 返回空（仅豁免）。
/// 返回 (notes, warnings)：读失败不能静默——豁免（parse_dismissed）仍在，但「勿再报」注入
/// 丢失会让模型重报浪费轮次。警告走结构化通道进 run.warnings（--background 下 worker 的
/// stderr 被丢弃，eprintln 不可达，r5 dogfood aa74cc1b），同时保留 eprintln 供前台直调即时可见。
pub fn load_dismissed_notes(value: &str) -> (String, Vec<String>) {
    const PER_FILE_CHARS: usize = 600;
    const TOTAL_CHARS: usize = 4000;
    let mut warnings = Vec::new();
    let p = Path::new(value);
    if !p.is_dir() {
        return (String::new(), warnings);
    }
    let mut names: Vec<String> = match std::fs::read_dir(p) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.strip_suffix(".md").is_some_and(is_hex16))
            .collect(),
        Err(e) => {
            let w = format!("--dismissed ledger dir unreadable ({e}); do-not-re-report notes NOT injected");
            eprintln!("redteam: {w}");
            warnings.push(w);
            return (String::new(), warnings);
        }
    };
    names.sort(); // read_dir 无序 → 排序保 prompt 确定性
    let mut out = String::new();
    for name in names {
        let content = match std::fs::read_to_string(p.join(&name)) {
            Ok(c) => c,
            Err(e) => {
                let w = format!("ledger entry {name} unreadable ({e}); skipped from notes");
                eprintln!("redteam: {w}");
                warnings.push(w);
                continue;
            }
        };
        let snippet: String = content.trim().chars().take(PER_FILE_CHARS).collect();
        out.push_str("- ");
        out.push_str(&snippet);
        out.push('\n');
        if out.chars().count() >= TOTAL_CHARS {
            out.push_str("…(more ledger entries truncated)\n");
            break;
        }
    }
    (out, warnings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_creates_skeleton_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let created = init_project(tmp.path()).unwrap();
        assert_eq!(created.len(), 4, "dismissed/ rounds/ .gitignore config.toml");
        assert!(tmp.path().join(".redteam/dismissed").is_dir());
        assert!(tmp.path().join(".redteam/rounds").is_dir());
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

    #[test]
    fn load_dismissed_notes_dir_injects_content_sorted() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("bbbbbbbbbbbbbbbb.md"), "标题B\n驳回理由：证据已豁免").unwrap();
        std::fs::write(tmp.path().join("aaaaaaaaaaaaaaaa.md"), "标题A\n驳回理由：误报").unwrap();
        std::fs::write(tmp.path().join("README.md"), "非台账不注入").unwrap();
        let (notes, _w) = load_dismissed_notes(tmp.path().to_str().unwrap());
        assert!(notes.contains("标题A") && notes.contains("误报"), "{notes}");
        assert!(notes.contains("标题B") && notes.contains("证据已豁免"), "{notes}");
        assert!(!notes.contains("非台账"), "{notes}");
        // 排序确定性：A 在 B 前
        assert!(notes.find("标题A").unwrap() < notes.find("标题B").unwrap());
    }

    #[test]
    fn load_dismissed_notes_csv_or_missing_is_empty() {
        assert_eq!(load_dismissed_notes("d2b371819297f98a,20ba9b3025a8b421").0, "");
        assert_eq!(load_dismissed_notes("/no/such/dir").0, "");
    }

    #[test]
    fn load_dismissed_notes_truncates_long_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let long = "长".repeat(5000);
        std::fs::write(tmp.path().join("cccccccccccccccc.md"), &long).unwrap();
        let (notes, _w) = load_dismissed_notes(tmp.path().to_str().unwrap());
        assert!(notes.chars().count() < 700, "per-file cap: {}", notes.chars().count());
    }
}
