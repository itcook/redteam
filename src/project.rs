//! 项目级 `.redteam/` 目录：骨架初始化与 --dismissed 输入解析（spec §1.2/§1.4）。
use std::collections::HashSet;
use std::path::Path;

pub const PROJECT_DIR: &str = ".redteam";

const GITIGNORE_BODY: &str = "*\n";
const CONFIG_TEMPLATE: &str = "\
# redteam 项目级配置（优先级高于用户级 ~/.config/redteam/config.toml）。
# 密钥走环境变量或用户级 secrets.toml——绝不写入本文件（本目录在仓库领地内）。
#
# provider（通道层：1 provider : N models）
# [providers.my-gateway]
# kind = \"openai\"            # openai | anthropic | google(占位)
# base_url = \"https://…\"
# api_key_env = \"MY_KEY_ENV\"  # model 可覆盖
#
# model（模型层：--models 引用的名字空间）
# [models.GLM]
# provider = \"my-gateway\"    # 引用 [providers.X]（必填）
# id = \"glm-5.3\"             # 请求体里的 model id
# kind = \"anthropic\"         # 可选：覆盖 provider.kind
# context_window = 1000000     # pack 预算护墙（缺省 128K 保守）
# output_limit = 32768         # max_tokens（> REDTEAM_MAX_TOKENS env > 默认）
# reasoning = true             # 思考型（thinking 计入 output 额度）
# thinking_level = \"medium\"  # off|low|medium|high
# per_turn_timeout_secs = 900  # 单轮超时（> env > 150s；速度是模型属性）
# priority = 0                 # 候补链排序
#
# ship 循环的测试命令（可选；不设则按 Cargo.toml/package.json/Makefile 推断）：
# test_command = \"cargo test\"
";

/// 幂等创建 `.redteam/` 骨架，返回本次新建的条目；已存在的一律不动。
/// dismissed/ 目录已随台账机器化退役（design 2026-07-03：台账/评审史统一 proj.db）。
pub fn init_project(root: &Path) -> anyhow::Result<Vec<String>> {
    let dir = root.join(PROJECT_DIR);
    let mut created = Vec::new();
    let rounds = dir.join("rounds");
    if !rounds.is_dir() {
        std::fs::create_dir_all(&rounds)?;
        created.push(format!("{PROJECT_DIR}/rounds/"));
    }
    for (name, body) in [(".gitignore", GITIGNORE_BODY), ("config.toml", CONFIG_TEMPLATE)] {
        let p = dir.join(name);
        if !p.exists() {
            std::fs::write(&p, body)?;
            created.push(format!("{PROJECT_DIR}/{name}"));
        }
    }
    let db_path = dir.join(crate::db::DB_FILE);
    if !db_path.exists() {
        crate::db::ProjDb::open_at(&db_path)?;
        created.push(format!("{PROJECT_DIR}/{}", crate::db::DB_FILE));
    }
    Ok(created)
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
        assert_eq!(created.len(), 4, "rounds/ .gitignore config.toml proj.db");
        assert!(tmp.path().join(".redteam/rounds").is_dir());
        assert!(tmp.path().join(".redteam/proj.db").is_file(), "init 建库");
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
    fn parse_dismissed_csv_valid_and_invalid() {
        let s = parse_dismissed("d2b371819297f98a, 20ba9b3025a8b421").unwrap();
        assert_eq!(s.len(), 2);
        assert!(parse_dismissed("not-an-id").is_err());
        assert!(parse_dismissed("").is_err());
        assert!(parse_dismissed("D2B371819297F98A").is_err(), "大写不合法（id 恒小写）");
    }

}
