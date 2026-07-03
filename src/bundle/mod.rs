use std::collections::BTreeMap;
use std::path::Path;

use crate::evidence::git::FileChange;

/// 噪声文件名（锁文件等，评审无价值）。
const NOISE_NAMES: &[&str] = &[
    "Cargo.lock", "package-lock.json", "yarn.lock", "pnpm-lock.yaml",
    "poetry.lock", "Gemfile.lock", "composer.lock",
];

/// 通用入口/聚合文件词干：同词干不代表逻辑相关，不参与同词干 union（只靠同目录 + import）。
const GENERIC_STEMS: &[&str] = &["mod", "lib", "main", "index", "__init__", "init"];

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

#[derive(Clone, Debug)]
pub struct Bundle {
    pub files: Vec<FileChange>,
}

/// 清单条目上限——超出截断为「… and N more files」（与 dismissed notes 同思路的体积防护）。
const MANIFEST_MAX_ENTRIES: usize = 200;
/// 清单总字符预算——条目数之外的直接 token 防护：每 bundle prompt 注入一份清单，
/// 大变更集下 O(bundles×manifest) 叠加，字符帽对症（dogfood 20260703 r1 6db068d2）。
const MANIFEST_MAX_CHARS: usize = 4000;

/// 全变更集清单（dogfood 20260702 #2）：`exclude` 号 bundle 之外所有变更文件的
/// `path (+adds/-dels)`。注入单包评审 prompt，让模型知晓视野外还有哪些文件在变——
/// 消「无证据实现/无 version」式跨 bundle 误报。空串 = 该包已覆盖全变更集。
pub fn other_files_manifest(bundles: &[Bundle], exclude: usize) -> String {
    let mut entries: Vec<String> = Vec::new();
    for (i, b) in bundles.iter().enumerate() {
        if i == exclude {
            continue;
        }
        for fc in &b.files {
            let (mut adds, mut dels) = (0usize, 0usize);
            for l in fc.diff.lines() {
                if l.starts_with("+++") || l.starts_with("---") {
                    continue;
                }
                if l.starts_with('+') {
                    adds += 1;
                } else if l.starts_with('-') {
                    dels += 1;
                }
            }
            entries.push(format!("{} (+{adds}/-{dels})", fc.path));
        }
    }
    // 双帽：条目数 + 总字符预算（先到者截断），尾注被截条数。
    let total = entries.len();
    let mut kept = 0usize;
    let mut chars = 0usize;
    for e in &entries {
        if kept >= MANIFEST_MAX_ENTRIES || chars + e.len() + 2 > MANIFEST_MAX_CHARS {
            break;
        }
        chars += e.len() + 2; // ", " 分隔
        kept += 1;
    }
    entries.truncate(kept);
    let mut s = entries.join(", ");
    let extra = total - kept;
    if extra > 0 {
        s.push_str(&format!(", … and {extra} more files"));
    }
    s
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
/// dotfile（首字符即 '.'）整名作词干，避免空词干把所有 dotfile 误并一包。
fn stem(path: &str) -> &str {
    let f = filename(path);
    match f.find('.') {
        Some(0) => f,
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

/// union-by-min-index：指向较小代表元，使 BTreeMap 按「组内最早文件」排序，bundle 顺序稳定直觉。
fn union(parent: &mut [usize], i: usize, j: usize) {
    let ri = uf_find(parent, i);
    let rj = uf_find(parent, j);
    if ri != rj {
        parent[ri.max(rj)] = ri.min(rj);
    }
}

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// 去掉 basename 最后一个扩展名；dotfile（basename 首字符即 '.'）整名保留。
fn strip_ext(p: &str) -> String {
    let slash = p.rfind('/');
    let dot = p.rfind('.');
    match (slash, dot) {
        (Some(s), Some(d)) if d > s + 1 => p[..d].to_string(), // basename 内、非首字符
        (None, Some(d)) if d > 0 => p[..d].to_string(),
        _ => p.to_string(),
    }
}

/// 改动集文件 → 模块键（去最后扩展名）。
fn norm_file(path: &str) -> String {
    strip_ext(path)
}

/// 把 base_dir 下的相对引用 rel 规范化为仓内键（处理 ./ 与 ../、去最后扩展名）。
fn norm_rel(base_dir: &str, rel: &str) -> String {
    let mut stack: Vec<&str> = if base_dir.is_empty() {
        Vec::new()
    } else {
        base_dir.split('/').collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    strip_ext(&stack.join("/"))
}

/// 提取一行里 needle 之后首个引号包裹、以 '.' 开头的相对路径（裸模块返回 None）。
fn quoted_rel_after(line: &str, needle: &str) -> Option<String> {
    let start = line.find(needle)? + needle.len();
    let rest = &line[start..];
    let qpos = rest.find(['\'', '"'])?;
    let qchar = rest.as_bytes()[qpos] as char;
    let after = &rest[qpos + 1..];
    let end = after.find(qchar)?;
    let inner = &after[..end];
    if inner.starts_with('.') {
        Some(inner.to_string())
    } else {
        None
    }
}

/// Python 相对 import token（前导点 + 点分模块）→ 相对路径片段；纯 "." 当前包返回 None。
fn py_rel(token: &str) -> Option<String> {
    let dots = token.chars().take_while(|&c| c == '.').count();
    if dots == 0 {
        return None;
    }
    let modpart = &token[dots..];
    let ups = dots - 1; // 1 点=当前目录，每多一点上溯一级
    if modpart.is_empty() && ups == 0 {
        return None; // 纯 "from . import x"，无具体文件
    }
    let mut rel = String::new();
    for _ in 0..ups {
        rel.push_str("../");
    }
    rel.push_str(&modpart.replace('.', "/"));
    Some(rel)
}

/// 从文件内容提取「指向同仓其它文件的相对引用键」（已就当前文件目录规范化、无扩展名）。
/// 仅解析相对引用：TS/JS `from './x'`/`require('./x')`、Python `from .x import`、Rust `mod x;`。
/// 裸模块/绝对引用（`import react`、`use crate::`、Go `import "pkg"`）不解析——要么外部依赖、
/// 要么需项目根映射，超出确定性 MVP（退化到目录+词干，工具循环兜底）。
pub fn extract_imports(path: &str, content: &str) -> Vec<String> {
    let ext = path.rsplit('.').next().unwrap_or("");
    let base = dir_of(path);
    let mut out = Vec::new();
    match ext {
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            for line in content.lines() {
                if let Some(rel) = quoted_rel_after(line, "from ") {
                    out.push(norm_rel(base, &rel));
                } else if let Some(rel) = quoted_rel_after(line, "require(") {
                    out.push(norm_rel(base, &rel));
                }
            }
        }
        "py" => {
            for line in content.lines() {
                let t = line.trim_start();
                if let Some(rest) = t.strip_prefix("from ") {
                    let token = rest.split_whitespace().next().unwrap_or("");
                    if token.starts_with('.') {
                        if let Some(rel) = py_rel(token) {
                            out.push(norm_rel(base, &rel));
                        }
                    }
                }
            }
        }
        "rs" => {
            for line in content.lines() {
                let t = line.trim();
                let t = t.strip_prefix("pub ").unwrap_or(t);
                if let Some(rest) = t.strip_prefix("mod ") {
                    if let Some(name) = rest.strip_suffix(';') {
                        let name = name.trim();
                        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                            let key = if base.is_empty() {
                                name.to_string()
                            } else {
                                format!("{base}/{name}")
                            };
                            out.push(key);
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// 轻量分包（确定性）：同父目录、或同词干（排除通用入口名）、或相对 import 互引 → 并到一包；
/// 超 cap 按排序切块。`root=Some` 时读各改动文件全文做 import union；`None` 退化到目录+词干。
pub fn group(mut changes: Vec<FileChange>, root: Option<&Path>) -> Vec<Bundle> {
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
            let si = stem(&changes[i].path);
            let same_stem = si == stem(&changes[j].path) && !GENERIC_STEMS.contains(&si);
            if same_dir || same_stem {
                union(&mut parent, i, j);
            }
        }
    }
    // 相对 import union（仅当给定 root）：A 引用了同在改动集里的 B → 并包。
    if let Some(root) = root {
        let key_to_idx: BTreeMap<String, usize> =
            (0..n).map(|i| (norm_file(&changes[i].path), i)).collect();
        #[allow(clippy::needless_range_loop)]
        for i in 0..n {
            // 安全不变量：changes[i].path 来自 git diff（仓内相对、git 跟踪、非模型可控），
            // 故此处 root.join 直读无需 positioning::safe_read 的 canonical-root/软链防逃逸；
            // 读失败即 continue 降级。若未来 caller 改喂不可信来源（spec/LLM 提议清单），须补该防护。
            let full = root.join(&changes[i].path);
            let content = match std::fs::read_to_string(&full) {
                Ok(c) => c,
                Err(_) => continue, // 读失败/二进制 → 跳过该文件 import（退化）
            };
            for key in extract_imports(&changes[i].path, &content) {
                if let Some(&j) = key_to_idx.get(&key) {
                    if i != j {
                        union(&mut parent, i, j);
                    }
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
    for (_root_idx, idxs) in groups {
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
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("src/auth/token.rs", "+b")], None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].files.len(), 2);
    }

    #[test]
    fn group_unions_same_stem_across_dirs() {
        // widget.tsx 与 widget.css 同词干 → 同包
        let out = group(vec![fc("ui/widget.tsx", "+a"), fc("styles/widget.css", "+b")], None);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn group_separates_unrelated() {
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("docs/readme.md", "+b")], None);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn group_splits_oversized_at_cap() {
        // 同目录 10 文件 → 切成 8 + 2
        let changes: Vec<FileChange> = (0..10)
            .map(|i| fc(&format!("src/m/f{i:02}.rs"), "+x"))
            .collect();
        let out = group(changes, None);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|b| b.files.len() <= BUNDLE_MAX_FILES));
        assert_eq!(out[0].files.len() + out[1].files.len(), 10);
        assert_eq!(out[0].files.len(), 8);
        assert_eq!(out[1].files.len(), 2);
        assert!(out[0].files[0].path.ends_with("f00.rs"));
        assert!(out[1].files[0].path.ends_with("f08.rs"));
    }

    #[test]
    fn group_does_not_union_distinct_dotfiles() {
        // 修复前：两个 dotfile 空词干 → 被错误并一包；修复后各自成包
        let out = group(vec![fc("a/.gitignore", "+x"), fc("b/.eslintrc", "+y")], None);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn extract_imports_ts_relative() {
        let src = "import {a} from '../util/b';\nconst c = require('./helper');\nimport x from 'react';\n";
        let mut got = extract_imports("src/feat/x.ts", src);
        got.sort();
        // '../util/b' 相对 src/feat：.. 上溯一级到 src，加 util/b → src/util/b
        // './helper' → src/feat/helper；'react' 裸模块忽略
        assert_eq!(got, vec!["src/feat/helper".to_string(), "src/util/b".to_string()]);
    }

    #[test]
    fn extract_imports_python_relative() {
        let src = "from .models import User\nfrom ..db.repo import Repo\nimport os\n";
        let mut got = extract_imports("pkg/api/view.py", src);
        got.sort();
        // '.models' → pkg/api/models；'..db.repo' 上溯一级 → pkg/db/repo；'os' 忽略
        assert_eq!(got, vec!["pkg/api/models".to_string(), "pkg/db/repo".to_string()]);
    }

    #[test]
    fn extract_imports_rust_mod() {
        let src = "pub mod token;\nmod helper;\nuse crate::other::thing;\n";
        let mut got = extract_imports("src/auth/mod.rs", src);
        got.sort();
        // mod 声明 → 同目录子模块；`use crate::` 不解析（MVP 范围外）
        assert_eq!(got, vec!["src/auth/helper".to_string(), "src/auth/token".to_string()]);
    }

    #[test]
    fn group_unions_by_relative_import_across_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src/feat")).unwrap();
        std::fs::create_dir_all(dir.path().join("util")).unwrap();
        std::fs::write(dir.path().join("src/feat/x.ts"), "import {b} from '../../util/b';\n").unwrap();
        std::fs::write(dir.path().join("util/b.ts"), "export const b = 1;\n").unwrap();
        // 不同目录、不同词干，仅靠 import 关联
        let changes = vec![fc("src/feat/x.ts", "+import"), fc("util/b.ts", "+export")];
        let out = group(changes, Some(dir.path()));
        assert_eq!(out.len(), 1, "relative import should union across dirs");
        assert_eq!(out[0].files.len(), 2);
    }

    #[test]
    fn group_generic_stems_do_not_over_union() {
        // 两个不同目录的 mod.rs：词干都是 "mod"（通用），不应仅因同词干并包
        let out = group(vec![fc("a/mod.rs", "+x"), fc("b/mod.rs", "+y")], None);
        assert_eq!(out.len(), 2, "generic stem 'mod' must not union unrelated files");
    }

    #[test]
    fn group_none_root_falls_back_to_dir_and_stem() {
        // root=None：无 import 提取，退化到目录+词干（同目录并包）
        let out = group(vec![fc("src/auth/login.rs", "+a"), fc("src/auth/token.rs", "+b")], None);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn other_files_manifest_lists_counts_and_excludes_self() {
        let bundles = vec![
            Bundle { files: vec![fc("a.rs", "+one\n+two\n-gone")] },
            Bundle { files: vec![fc("b.rs", "--- a/b.rs\n+++ b/b.rs\n+x")] },
        ];
        let m = other_files_manifest(&bundles, 0);
        assert_eq!(m, "b.rs (+1/-0)", "排除自身；+++/--- 头不计增删");
        let m1 = other_files_manifest(&bundles, 1);
        assert_eq!(m1, "a.rs (+2/-1)");
    }

    #[test]
    fn other_files_manifest_single_bundle_is_empty() {
        let bundles = vec![Bundle { files: vec![fc("a.rs", "+x")] }];
        assert!(other_files_manifest(&bundles, 0).is_empty(), "单包即全变更集 → 空");
    }

    #[test]
    fn other_files_manifest_truncates_over_cap() {
        let files: Vec<_> = (0..MANIFEST_MAX_ENTRIES + 5).map(|i| fc(&format!("f{i}.rs"), "+x")).collect();
        let bundles = vec![Bundle { files: vec![fc("self.rs", "+s")] }, Bundle { files }];
        let m = other_files_manifest(&bundles, 0);
        assert!(m.contains("more files"), "超上限截断并注记条数: {m}");
        assert!(m.len() <= MANIFEST_MAX_CHARS + 40, "字符预算生效（截断注記在预算外富余内）: len={}", m.len());
    }

    #[test]
    fn other_files_manifest_char_budget_truncates_long_paths() {
        // 50 个超长路径（每条 ~120 字符）→ 字符帽先于条目帽触发
        let files: Vec<_> = (0..50)
            .map(|i| fc(&format!("very/deeply/nested/module/path/segment/that/keeps/going/and/going/file_number_{i:04}_with_long_name.rs"), "+x"))
            .collect();
        let bundles = vec![Bundle { files: vec![fc("self.rs", "+s")] }, Bundle { files }];
        let m = other_files_manifest(&bundles, 0);
        assert!(m.len() <= MANIFEST_MAX_CHARS + 40, "字符预算截断: len={}", m.len());
        assert!(m.contains("more files"), "被截条数有注记: {m}");
    }
}
