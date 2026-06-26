use crate::evidence::git::FileChange;

/// 噪声文件名（锁文件等，评审无价值）。
const NOISE_NAMES: &[&str] = &[
    "Cargo.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
    "poetry.lock", "Gemfile.lock", "composer.lock",
];

fn filename(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[i + 1..],
        None => path,
    }
}

fn is_noise(path: &str) -> bool {
    let f = filename(path);
    NOISE_NAMES.contains(&f) || f.ends_with(".lock")
}

/// 确定性文件筛选：丢掉空 diff 与噪声（锁文件）。代码决定，不交模型。
pub fn select_files(changes: Vec<FileChange>) -> Vec<FileChange> {
    changes
        .into_iter()
        .filter(|c| !c.diff.trim().is_empty())
        .filter(|c| !is_noise(&c.path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fc(path: &str, diff: &str) -> FileChange {
        FileChange { path: path.into(), diff: diff.into() }
    }

    #[test]
    fn select_keeps_normal_code_files() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("src/b.rs", "+y")]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn select_drops_lockfiles() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("Cargo.lock", "+dep"), fc("ui/yarn.lock", "+pkg")]);
        let paths: Vec<&str> = out.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["src/a.rs"]);
    }

    #[test]
    fn select_drops_empty_diff() {
        let out = select_files(vec![fc("src/a.rs", "+x"), fc("src/blank.rs", "   ")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "src/a.rs");
    }
}
