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

use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub struct Bundle {
    pub files: Vec<FileChange>,
}

/// 单个文件包的文件数上限（超出按排序切块）。
pub const BUNDLE_MAX_FILES: usize = 8;

fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// 词干：文件名去掉首个 '.' 之后部分（widget.test.ts 与 widget.ts 同词干 "widget"）。
fn stem(path: &str) -> &str {
    let f = filename(path);
    match f.find('.') {
        Some(i) => &f[..i],
        None => f,
    }
}

fn uf_find(parent: &mut [usize], x: usize) -> usize {
    let mut root = x;
    while parent[root] != root {
        root = parent[root];
    }
    // 路径压缩
    let mut cur = x;
    while parent[cur] != cur {
        let next = parent[cur];
        parent[cur] = root;
        cur = next;
    }
    root
}

/// 轻量分包：同父目录或同词干并到一包；超 cap 按排序切块。确定性（先排序）。
pub fn group(mut changes: Vec<FileChange>) -> Vec<Bundle> {
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let n = changes.len();
    if n == 0 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..n).collect();
    #[allow(clippy::needless_range_loop)]
    for i in 0..n {
        for j in (i + 1)..n {
            let same_dir = parent_dir(&changes[i].path) == parent_dir(&changes[j].path);
            let same_stem = stem(&changes[i].path) == stem(&changes[j].path);
            if same_dir || same_stem {
                let ri = uf_find(&mut parent, i);
                let rj = uf_find(&mut parent, j);
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    // 按代表元聚组（成员索引升序，因已排序）
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = uf_find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    let mut bundles = Vec::new();
    for (_root, idxs) in groups {
        for chunk in idxs.chunks(BUNDLE_MAX_FILES) {
            let files = chunk.iter().map(|&k| changes[k].clone()).collect();
            bundles.push(Bundle { files });
        }
    }
    bundles
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

    #[test]
    fn group_unions_same_directory() {
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("src/auth/token.rs", "+b")]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].files.len(), 2);
    }

    #[test]
    fn group_unions_same_stem_across_dirs() {
        // widget.tsx 与 widget.css 同词干 → 同包
        let out = group(vec![fc("ui/widget.tsx", "+a"), fc("styles/widget.css", "+b")]);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn group_separates_unrelated() {
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("docs/readme.md", "+b")]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn group_splits_oversized_at_cap() {
        // 同目录 10 文件 → 切成 8 + 2
        let changes: Vec<FileChange> = (0..10)
            .map(|i| fc(&format!("src/m/f{i:02}.rs"), "+x"))
            .collect();
        let out = group(changes);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|b| b.files.len() <= BUNDLE_MAX_FILES));
        assert_eq!(out[0].files.len() + out[1].files.len(), 10);
    }
}
