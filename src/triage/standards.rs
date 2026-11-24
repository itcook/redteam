//! 3.2 `StandardsMap`：逐模块 `doc → linter → peer`（grill 定案 2026-11-24）。
//!
//! 标准 = **repo 实际携带的东西**（人工基线 `pr-triage` Step 2）：显式架构/约定文档 >
//! 机器可执行的 linter 配置 > 同目录兄弟文件。`no-standard` 只在三者皆无时出现。
//!
//! 本模块只负责**发现与读取**；「漂移是否结构性」由漂移轴的模型判定。
//! `routing.specs` 只取 doc 来源（`prior.rs` 只能注入 repo 内可读文本），linter/peer 只进披露。

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::{Component, Path};

/// 显式文档候选（逐级向上查找，最近者优先）。
const DOC_NAMES: &[&str] = &[
    "ARCHITECTURE.md",
    "CONTRIBUTING.md",
    "AGENTS.md",
    "CLAUDE.md",
    "CONVENTIONS.md",
];

/// 机器可执行规则候选（不把 `Cargo.toml` / `package.json` 当 linter——那是清单不是规则）。
const LINTER_NAMES: &[&str] = &[
    ".editorconfig",
    ".eslintrc",
    ".eslintrc.js",
    ".eslintrc.cjs",
    ".eslintrc.json",
    ".eslintrc.yml",
    "eslint.config.js",
    "eslint.config.mjs",
    "eslint.config.cjs",
    "biome.json",
    ".prettierrc",
    ".prettierrc.json",
    ".prettierrc.js",
    "ruff.toml",
    ".ruff.toml",
    "pyproject.toml",
    ".flake8",
    "setup.cfg",
    "tox.ini",
    ".golangci.yml",
    ".golangci.yaml",
    "rustfmt.toml",
    ".rustfmt.toml",
    "clippy.toml",
    "checkstyle.xml",
    "spotless.xml",
];

/// 单个块的读取上限（字节）。prompt 预算由调用方再按总分片裁剪。
const DOC_READ_MAX: usize = 8 * 1024;
const LINTER_READ_MAX: usize = 1536;
const PEER_READ_MAX: usize = 2560;
const PACK_MAX: usize = 60 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StandardSource {
    Doc,
    Linter,
    Peer,
}

impl StandardSource {
    pub fn as_str(self) -> &'static str {
        match self {
            StandardSource::Doc => "doc",
            StandardSource::Linter => "linter",
            StandardSource::Peer => "peer",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ModuleStandard {
    /// repo 相对目录（根用 `"."`）。
    pub module: String,
    /// 该模块下被改动的文件（repo 相对路径）。
    pub files: Vec<String>,
    /// 最近命中的显式文档（repo 相对路径）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// 命中的 linter 配置（最近优先，≤2）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub linters: Vec<String>,
    /// peer 来源的兄弟文件（仅 doc/linter 都缺时填充，≤3）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub peers: Vec<String>,
    /// 解析出的最强来源；`None` = `no-standard`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<StandardSource>,
}

impl ModuleStandard {
    pub fn no_standard(&self) -> bool {
        self.source.is_none()
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct StandardsMap {
    pub modules: Vec<ModuleStandard>,
    /// `no-standard` 的模块（披露用）。
    pub no_standard: Vec<String>,
}

/// 逐模块解析；`changed` 是 repo 相对路径（来自 `diff_patch` 投影）。
pub fn build(repo: &Path, changed: &[String]) -> StandardsMap {
    let root = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let mut by_module: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for f in changed {
        let module = module_of(f);
        by_module.entry(module).or_default().push(f.clone());
    }
    let mut modules = Vec::new();
    for (module, mut files) in by_module {
        files.sort();
        files.dedup();
        let (doc, linters) = resolve_doc_and_linters(&root, &module);
        let peers = if doc.is_none() && linters.is_empty() {
            resolve_peers(&root, &module, &files)
        } else {
            Vec::new()
        };
        let source = if doc.is_some() {
            Some(StandardSource::Doc)
        } else if !linters.is_empty() {
            Some(StandardSource::Linter)
        } else if !peers.is_empty() {
            Some(StandardSource::Peer)
        } else {
            None
        };
        modules.push(ModuleStandard { module, files, doc, linters, peers, source });
    }
    let no_standard = modules.iter().filter(|m| m.no_standard()).map(|m| m.module.clone()).collect();
    StandardsMap { modules, no_standard }
}

/// 文件 → 模块目录（repo 相对，根为 `"."`）。
pub fn module_of(file: &str) -> String {
    Path::new(file)
        .parent()
        .map(|p| {
            let s = p.to_string_lossy().replace('\\', "/");
            if s.is_empty() { ".".to_string() } else { s }
        })
        .unwrap_or_else(|| ".".to_string())
}

fn resolve_doc_and_linters(root: &Path, module: &str) -> (Option<String>, Vec<String>) {
    // 最近祖先优先；模块目录 → 逐级到根。
    let mut ancestors: Vec<String> = Vec::new();
    let mut cur = if module == "." { String::new() } else { module.to_string() };
    loop {
        ancestors.push(cur.clone());
        if cur.is_empty() {
            break;
        }
        cur = Path::new(&cur)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
    }

    let mut linters: Vec<String> = Vec::new();
    let mut doc: Option<String> = None;
    for anc in &ancestors {
        let rel_dir = if anc.is_empty() { "" } else { anc.as_str() };
        // 文档：最近祖先的先命中即停。
        if doc.is_none() {
            doc = doc_in_dir(root, rel_dir);
        }
        // linter：最近祖先优先，最多 2 份。
        if linters.len() < 2 {
            for name in LINTER_NAMES {
                if linters.len() >= 2 {
                    break;
                }
                let rel = join_rel(rel_dir, name);
                if is_readable_file(root, &rel) && !linters.contains(&rel) {
                    linters.push(rel);
                }
            }
        }
        if doc.is_some() && linters.len() >= 2 {
            break;
        }
    }
    // ADR / guidance 只在根解析（它们不按目录放置）。
    if doc.is_none() {
        doc = adr_for(root, module).or_else(|| guidance_for(root, module));
    }
    (doc, linters)
}

fn doc_in_dir(root: &Path, rel_dir: &str) -> Option<String> {
    for name in DOC_NAMES {
        let rel = join_rel(rel_dir, name);
        if is_readable_file(root, &rel) {
            return Some(rel);
        }
    }
    // 约定文档的常见落点（只查本目录，不做全仓搜索）。
    for name in ["docs/CONVENTIONS.md", "docs/CONTRIBUTING.md", ".github/CONTRIBUTING.md"] {
        let rel = join_rel(rel_dir, name);
        if is_readable_file(root, &rel) {
            return Some(rel);
        }
    }
    None
}

/// 根级 `docs/adr/*.md`：优先文件名含模块名（大小写无关），否则取字典序前 2 中的第一份。
fn adr_for(root: &Path, module: &str) -> Option<String> {
    let rel_dir = "docs/adr";
    let dir = root.join(rel_dir);
    if !dir.is_dir() {
        return None;
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    let needle = module.rsplit('/').next().unwrap_or("").to_lowercase();
    if !needle.is_empty() {
        if let Some(hit) = names.iter().find(|n| n.to_lowercase().contains(&needle)) {
            return Some(format!("{rel_dir}/{hit}"));
        }
    }
    names.first().map(|n| format!("{rel_dir}/{n}"))
}

/// `.rpiv/guidance/<module>/architecture.md` → 逐级向上回落。
fn guidance_for(root: &Path, module: &str) -> Option<String> {
    let mut cur = if module == "." { String::new() } else { module.to_string() };
    loop {
        let rel = if cur.is_empty() {
            ".rpiv/guidance/architecture.md".to_string()
        } else {
            format!(".rpiv/guidance/{cur}/architecture.md")
        };
        if is_readable_file(root, &rel) {
            return Some(rel);
        }
        if cur.is_empty() {
            return None;
        }
        cur = Path::new(&cur)
            .parent()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
    }
}

/// 同目录、同扩展名的兄弟文件（最多 3 份）；排除被改动的文件本身。
fn resolve_peers(root: &Path, module: &str, changed: &[String]) -> Vec<String> {
    let dir_abs = if module == "." { root.to_path_buf() } else { root.join(module) };
    let exts: BTreeSet<String> = changed
        .iter()
        .filter_map(|f| Path::new(f).extension().map(|e| e.to_string_lossy().to_string()))
        .collect();
    let changed_set: BTreeSet<&str> = changed.iter().map(String::as_str).collect();
    let mut peers = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir_abs) else { return peers };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    for name in names {
        let rel = join_rel(if module == "." { "" } else { module }, &name);
        if changed_set.contains(rel.as_str()) {
            continue;
        }
        let Some(ext) = Path::new(&name).extension().map(|e| e.to_string_lossy().to_string()) else {
            continue;
        };
        if !exts.contains(&ext) {
            continue;
        }
        if is_readable_file(root, &rel) {
            peers.push(rel);
            if peers.len() >= 3 {
                break;
            }
        }
    }
    peers
}

/// 漂移轴 prompt 的 standards pack：每模块来源 + 文档/规则/兄弟文件的**内容摘录**。
/// 模型没有工具，规则必须随 prompt 到场。`max_bytes` 是本 pack 的硬上限。
pub fn render_pack(map: &StandardsMap, root: &Path, lang_zh: bool) -> String {
    if map.modules.is_empty() {
        return if lang_zh { "（无可解析模块）\n".into() } else { "(no resolved modules)\n".into() };
    }
    let mut out = String::new();
    for m in &map.modules {
        let header = match m.source {
            Some(s) => format!(
                "## module `{}` (source={})\nchanged: {}\n",
                m.module,
                s.as_str(),
                m.files.join(", ")
            ),
            None => format!(
                "## module `{}` (source=no-standard)\nchanged: {}\n{}\n",
                m.module,
                m.files.join(", "),
                if lang_zh {
                    "该模块没有文档、linter，也没有可读的兄弟文件——不要凭空编造标准；只在有明确证据时报告漂移。"
                } else {
                    "This module has no docs, linter, or readable siblings — do not invent a standard; report drift only with concrete evidence."
                }
            ),
        };
        out.push_str(&header);
        if let Some(doc) = &m.doc {
            if let Some(text) = read_text(root, doc, DOC_READ_MAX) {
                out.push_str(&format!("### doc `{doc}`\n{text}\n"));
            }
        }
        for l in &m.linters {
            if let Some(text) = read_text(root, l, LINTER_READ_MAX) {
                out.push_str(&format!("### linter `{l}`\n{text}\n"));
            }
        }
        for p in &m.peers {
            if let Some(text) = read_text(root, p, PEER_READ_MAX) {
                out.push_str(&format!("### peer `{p}`\n{text}\n"));
            }
        }
        out.push('\n');
        if out.len() >= PACK_MAX {
            out.push_str(if lang_zh { "（标准包已截断）\n" } else { "(standards pack truncated)\n" });
            break;
        }
    }
    out
}

/// `routing.specs`：只取 doc 来源；blocker 相关模块优先，去重，≤`cap`。
pub fn doc_specs(map: &StandardsMap, priority_files: &[String], cap: usize) -> Vec<String> {
    let priority_modules: BTreeSet<String> = priority_files.iter().map(|f| module_of(f)).collect();
    let mut ordered: Vec<&ModuleStandard> = map.modules.iter().collect();
    ordered.sort_by_key(|m| (!priority_modules.contains(&m.module), m.module.clone()));
    let mut out = Vec::new();
    for m in ordered {
        if out.len() >= cap {
            break;
        }
        if let Some(doc) = &m.doc {
            if !out.contains(doc) {
                out.push(doc.clone());
            }
        }
    }
    out
}

fn join_rel(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

fn is_readable_file(root: &Path, rel: &str) -> bool {
    read_text(root, rel, 1).is_some()
}

/// repo 内文本读取：拒绝绝对路径 / `..` / 越界/ 二进制；截断不破坏 UTF-8 边界。
fn read_text(root: &Path, rel: &str, max: usize) -> Option<String> {
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let real = std::fs::canonicalize(root.join(p)).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    if !real.starts_with(&root) || !real.is_file() {
        return None;
    }
    let bytes = std::fs::read(&real).ok()?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return None;
    }
    let truncated = bytes.len() > max;
    let slice = &bytes[..bytes.len().min(max)];
    let mut text = String::from_utf8_lossy(slice).to_string();
    if truncated {
        // 截断点可能落在多字节字符中间：lossy 已替换为 U+FFFD，标记一下即可。
        text.push_str("\n… (truncated)\n");
    }
    Some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn resolves_doc_first_then_linter_then_peer_and_no_standard() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        // mod-doc：模块目录里有 AGENTS.md（最近祖先命中）。
        write(root, "src/mod-doc/AGENTS.md", "## API rules\n- always validate\n");
        write(root, "src/mod-doc/a.rs", "fn a() {}\n");
        // mod-linter：只有 linter。
        write(root, "src/mod-linter/rustfmt.toml", "edition = \"2021\"\n");
        write(root, "src/mod-linter/b.rs", "fn b() {}\n");
        // mod-peer：只有兄弟文件。
        write(root, "src/mod-peer/c.rs", "fn c() {}\n");
        write(root, "src/mod-peer/d.rs", "fn d() {}\n");
        // mod-none：新目录 + 唯一改动文件（没有兄弟）。
        write(root, "src/mod-none/e.rs", "fn e() {}\n");

        let changed = vec![
            "src/mod-doc/a.rs".to_string(),
            "src/mod-linter/b.rs".to_string(),
            "src/mod-peer/c.rs".to_string(),
            "src/mod-none/e.rs".to_string(),
        ];
        let map = build(root, &changed);
        let get = |m: &str| map.modules.iter().find(|x| x.module == m).unwrap();
        assert_eq!(get("src/mod-doc").source, Some(StandardSource::Doc));
        assert_eq!(get("src/mod-doc").doc.as_deref(), Some("src/mod-doc/AGENTS.md"));
        assert_eq!(get("src/mod-linter").source, Some(StandardSource::Linter));
        assert_eq!(get("src/mod-peer").source, Some(StandardSource::Peer));
        assert!(get("src/mod-peer").peers.contains(&"src/mod-peer/d.rs".to_string()));
        assert_eq!(get("src/mod-none").source, None);
        assert_eq!(map.no_standard, vec!["src/mod-none".to_string()]);

        let zh = render_pack(&map, root, true);
        assert!(zh.contains("always validate"), "doc 内容必须进 pack");
        assert!(zh.contains("edition = \"2021\""), "linter 内容必须进 pack");
        assert!(zh.contains("fn d()"), "peer 内容必须进 pack");
    }

    #[test]
    fn root_docs_linters_and_adr_are_found_by_walking_up() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(root, "CONTRIBUTING.md", "conventions\n");
        write(root, ".editorconfig", "root = true\n");
        write(root, "src/deep/x.rs", "fn x() {}\n");
        let map = build(root, &["src/deep/x.rs".to_string()]);
        let m = &map.modules[0];
        assert_eq!(m.doc.as_deref(), Some("CONTRIBUTING.md"));
        assert!(m.linters.contains(&".editorconfig".to_string()));
        assert_eq!(m.source, Some(StandardSource::Doc), "doc 优先于 linter");

        // 无 doc 时用根级 ADR；文件名含模块名优先。
        let d2 = tempfile::tempdir().unwrap();
        let root2 = d2.path();
        write(root2, "docs/adr/0001-general.md", "general\n");
        write(root2, "docs/adr/0002-billing.md", "billing rules\n");
        write(root2, "src/billing/y.rs", "fn y() {}\n");
        let map2 = build(root2, &["src/billing/y.rs".to_string()]);
        assert_eq!(map2.modules[0].doc.as_deref(), Some("docs/adr/0002-billing.md"));
    }

    #[test]
    fn specs_are_doc_only_priority_first_and_capped() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(root, "src/a/AGENTS.md", "a\n");
        write(root, "src/b/AGENTS.md", "b\n");
        write(root, "src/a/x.rs", "x\n");
        write(root, "src/b/y.rs", "y\n");
        let map = build(root, &["src/a/x.rs".to_string(), "src/b/y.rs".to_string()]);
        let specs = doc_specs(&map, &["src/b/y.rs".to_string()], 8);
        assert_eq!(specs, vec!["src/b/AGENTS.md".to_string(), "src/a/AGENTS.md".to_string()]);
        assert_eq!(doc_specs(&map, &[], 1).len(), 1, "cap 生效");
    }

    #[test]
    fn paths_outside_the_repo_are_never_injected() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("repo");
        std::fs::create_dir_all(&root).unwrap();
        write(d.path(), "outside.md", "secret\n");
        write(&root, "src/a.rs", "fn a() {}\n");
        let map = build(&root, &["src/a.rs".to_string()]);
        assert!(map.modules[0].no_standard(), "越界文档不算标准");
        assert!(render_pack(&map, &root, false).contains("no-standard"));
    }
}
