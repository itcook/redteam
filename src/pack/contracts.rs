//! L4-B 三方契约定位链（M21d 批 A，design §9.4 B）：`use` → crate → `Cargo.lock` 精确版本 → registry 源目录。
//!
//! 为什么要精确版本：模型对三方 API 的知识停在它的训练时点，而本仓锁的可能是另一个大版本
//! ——「按记忆写对、按锁定版本是错的」这类误用，只有把**这一份**源码的签名摆到它面前才抓得到。
//!
//! 本模块只做**定位**（纯函数、离线可测）；抽契约与注入在 A-2 / A-3。
//! 非 Rust 生态的定位链明确不做（roadmap「不做」节，挂 M20 backlog）——所以 Go / TS 仓在本里程碑享受不到它。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 语言自带、不在 registry 里的根 crate。
const BUILTIN_CRATES: &[&str] = &["std", "core", "alloc", "proc_macro", "test"];

/// 从 Rust 源里抽**外部 crate 名**（`use` 语句的首段）。
///
/// 排除四类：`crate::` / `super::` / `self::` 这些仓内相对路径、语言自带 crate、
/// 调用方给出的仓内模块名（`local_mods`，2018 版里 `use foo::bar` 的 `foo` 可能是本 crate 的模块）、
/// 以及形如 `use x as y` 里被重命名出来的名字（只取首段，重命名不影响定位）。
///
/// 多行 `use a::{\n  b,\n  c,\n};` 只需首段，所以不必累积续行——续行不以 `use ` 开头，天然被跳过。
pub fn external_crates(content: &str, local_mods: &BTreeSet<String>) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in content.lines() {
        let t = line.trim();
        // 可见性前缀：`pub use` / `pub(crate) use` 也算
        let t = t.strip_prefix("pub").map(str::trim_start).unwrap_or(t);
        let t = if t.starts_with('(') { t.split_once(')').map(|(_, r)| r.trim_start()).unwrap_or(t) } else { t };
        let Some(rest) = t.strip_prefix("use ") else { continue };
        let head = rest
            .trim_start()
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .find(|s| !s.is_empty())
            .unwrap_or("");
        if head.is_empty()
            || matches!(head, "crate" | "super" | "self")
            || BUILTIN_CRATES.contains(&head)
            || local_mods.contains(head)
        {
            continue;
        }
        out.insert(head.to_string());
    }
    out
}

/// `Cargo.lock` → {包名: 版本}。只认 `[[package]]` 段里的 `name` / `version`（lock v3/v4 同形）。
///
/// 键用**包名原样**（`grep-regex`）；`use` 里写的是 lib 名（`grep_regex`），匹配时走 [`normalize_crate`]。
pub fn locked_versions(lock_text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let (mut name, mut in_pkg) = (None::<String>, false);
    let val = |line: &str| line.split_once('=').map(|(_, v)| v.trim().trim_matches('"').to_string());
    for line in lock_text.lines() {
        let t = line.trim();
        if t == "[[package]]" {
            (name, in_pkg) = (None, true);
            continue;
        }
        if t.starts_with('[') {
            in_pkg = false;
            continue;
        }
        if !in_pkg {
            continue;
        }
        if t.starts_with("name") {
            name = val(t);
        } else if t.starts_with("version") {
            if let (Some(n), Some(v)) = (name.take(), val(t)) {
                out.insert(n, v);
            }
        }
    }
    out
}

/// crate 名归一：`-` 与 `_` 等价（包名 `grep-regex` ↔ lib 名 `grep_regex`），大小写不敏感。
pub fn normalize_crate(s: &str) -> String {
    s.replace('-', "_").to_ascii_lowercase()
}

/// registry 源根：`$CARGO_HOME/registry/src`，`CARGO_HOME` 缺省 `~/.cargo`。**只解析不校验存在**。
pub fn registry_root() -> Option<PathBuf> {
    let home = match std::env::var_os("CARGO_HOME") {
        Some(h) => PathBuf::from(h),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".cargo"),
    };
    Some(home.join("registry").join("src"))
}

/// 在 registry 源根下找 `<crate>-<version>` 的解包目录。
///
/// registry 下有一层索引目录（`index.crates.io-<hash>` 等，可能多个：crates.io 与镜像并存），
/// 逐个找、取第一个命中；**目录名按归一后的 crate 名比对**，因为解包目录用的是包名（带 `-`）。
pub fn crate_src_dir(registry_src: &Path, crate_name: &str, version: &str) -> Option<PathBuf> {
    let want = normalize_crate(crate_name);
    let indexes = std::fs::read_dir(registry_src).ok()?;
    for idx in indexes.flatten() {
        let Ok(entries) = std::fs::read_dir(idx.path()) else { continue };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // 目录名 = `<包名>-<版本>`，而包名自身可含 `-`（`grep-regex`）→ 按**最后一个** `-` 拆，
            // 只对包名那半边做归一（整体归一会把版本号里的分隔符也吃掉）。
            let Some((pkg, ver)) = name.rsplit_once('-') else { continue };
            if ver == version && normalize_crate(pkg) == want {
                return Some(e.path());
            }
        }
    }
    None
}

/// 定位失败的原因——**要能进 RunMeta 披露**（roadmap 风险节：registry 在 CI / 他人机器缺失时降级不阻塞）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocateMiss {
    /// `Cargo.lock` 里没有这个包（`use` 的是 dev-dependency？工作区成员？名字拼错？）
    NotLocked(String),
    /// 锁到了版本，但 registry 里没有解包目录（没 `cargo fetch` 过 / 换了机器 / 用的是 git 或 path 依赖）
    NotVendored { krate: String, version: String },
}

/// 一批 crate 名 → 各自的源目录；定位不到的进 `misses`（调用方据此降级并披露）。
pub fn locate_all(
    registry_src: &Path,
    locked: &BTreeMap<String, String>,
    crates: &BTreeSet<String>,
) -> (BTreeMap<String, PathBuf>, Vec<LocateMiss>) {
    // lock 的键是包名，`use` 给的是 lib 名 → 先建一张归一化索引
    let by_norm: BTreeMap<String, (&String, &String)> =
        locked.iter().map(|(n, v)| (normalize_crate(n), (n, v))).collect();
    let (mut found, mut misses) = (BTreeMap::new(), Vec::new());
    for c in crates {
        let Some((pkg, ver)) = by_norm.get(&normalize_crate(c)).copied() else {
            misses.push(LocateMiss::NotLocked(c.clone()));
            continue;
        };
        match crate_src_dir(registry_src, pkg, ver) {
            Some(dir) => {
                found.insert(c.clone(), dir);
            }
            None => misses.push(LocateMiss::NotVendored { krate: pkg.clone(), version: ver.clone() }),
        }
    }
    (found, misses)
}

/// 一条三方契约的位置（A-2）。渲染成文本块的活交给 [`render_contract`]。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractHit {
    /// 归属 crate（`use` 里写的 lib 名）。
    pub krate: String,
    /// crate 源目录内的相对路径（进 prompt 的坐标要带 crate 标签，不能只给相对路径——同名文件太多）。
    pub rel: String,
    /// 1-based 行号。
    pub line: usize,
    pub name: String,
    /// 所属 `impl` 的类型名；`None` = 自由函数。
    ///
    /// 为什么必须区分：`serde_json::from_str`（自由函数，解析成 T）与 `Deserializer::from_str`
    /// （impl 方法，造一个 deserializer）同名不同物。把后者当契约摆给模型，**比不给更糟**——
    /// 它会照着错的签名推理。自由函数优先，impl 方法带类型名披露。
    pub owner: Option<String>,
}

/// crate 源里最多走的文件数（bound 大 crate：`syn` / `aws-sdk-*` 这类源码上千文件）。
const CRATE_WALK_MAX_FILES: usize = 400;
/// 单文件字节上限（与仓内文件外片段同口径）。
const CRATE_FILE_MAX_BYTES: u64 = 512 * 1024;
/// 每个名字最多取几处定义（同名函数散在多个模块时只取前几处，路径序）。
const HITS_PER_NAME: usize = 2;

/// 在 crate 源里找这些名字的 `pub fn` 定义。
///
/// 只走 `src/` 下的 `.rs`（`tests/` / `examples/` / `benches/` 不是公开契约）；按路径序走，
/// 命中数按 [`HITS_PER_NAME`] 与 `cap` 双重封顶——契约注入不得挤掉仓内上下文（预算由调用方给）。
///
/// 认这些形态：`pub fn` / `pub async fn` / `pub const fn` / `pub unsafe fn` / `pub extern "C" fn`
/// / `pub(crate) fn` 之类的受限可见性也认（它对本仓不可见，但同名判断用不着那么精确，宁可多给一点上下文）。
pub fn find_contracts(crate_dir: &Path, krate: &str, names: &BTreeSet<String>, cap: usize) -> Vec<ContractHit> {
    if names.is_empty() || cap == 0 {
        return Vec::new();
    }
    let alt = names.iter().map(|n| regex::escape(n)).collect::<Vec<_>>().join("|");
    let Ok(re) = regex::Regex::new(&format!(
        r#"^\s*pub(?:\s*\([^)]*\))?\s+(?:(?:async|const|unsafe|extern\s+"[^"]*")\s+)*fn\s+({alt})\b"#
    )) else {
        return Vec::new();
    };
    let src = crate_dir.join("src");
    let mut files: Vec<PathBuf> = Vec::new();
    collect_rs(&src, &mut files);
    files.sort();
    let mut all: Vec<(bool, ContractHit)> = Vec::new();
    for f in files.iter().take(CRATE_WALK_MAX_FILES) {
        if std::fs::metadata(f).map(|m| m.len() > CRATE_FILE_MAX_BYTES).unwrap_or(true) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        let rel = f.strip_prefix(crate_dir).unwrap_or(f).to_string_lossy().into_owned();
        // `impl` 栈：行级花括号计数的启发式（与 `bundle::resolve_rust_file` 的内联 mod 同一手法）——
        // 字符串 / 注释里的括号会失准，最坏退化成「算不出 owner」，不影响正确性只影响排序。
        let (mut impls, mut depth) = (Vec::<(String, usize)>::new(), 0usize);
        let (mut prev_opens, mut prev_closes) = (0usize, 0usize);
        for (i, line) in text.lines().enumerate() {
            depth = (depth + prev_opens).saturating_sub(prev_closes);
            while impls.last().is_some_and(|(_, d)| depth <= *d) {
                impls.pop();
            }
            let t = line.trim();
            (prev_opens, prev_closes) = (t.matches('{').count(), t.matches('}').count());
            if let Some(ty) = impl_type_of(t) {
                impls.push((ty, depth));
            }
            let Some(c) = re.captures(line) else { continue };
            // 顶层判据以**缩进**为准、花括号栈只用来取类型名（实测修正）：serde_json 的 `de.rs` 有 2700 行，
            // 字符串 / 宏 / 注释里的括号让深度算错，真正的自由函数 `from_str`（L2709，缩进 0）被误判成
            // impl 内，于是排在 `Deserializer::from_str`（L96）后面——**给模型错的那个同名函数**。
            // Rust 惯例里顶层 item 无缩进，这条判据稳得多。
            let top_level = line.len() == line.trim_start().len();
            all.push((
                top_level,
                ContractHit {
                    krate: krate.to_string(),
                    rel: rel.clone(),
                    line: i + 1,
                    name: c[1].to_string(),
                    owner: if top_level { None } else { impls.last().map(|(ty, _)| ty.clone()) },
                },
            ));
        }
    }
    // 顶层自由函数优先（同名时它才是 `krate::name` 指的那个），其余按路径 / 行号保持确定性
    all.sort_by(|(at, a), (bt, b)| (!at, &a.rel, a.line).cmp(&(!bt, &b.rel, b.line)));
    let all: Vec<ContractHit> = all.into_iter().map(|(_, h)| h).collect();
    let mut per_name = BTreeMap::<String, usize>::new();
    let mut out = Vec::new();
    for h in all {
        let n = per_name.entry(h.name.clone()).or_default();
        if *n >= HITS_PER_NAME {
            continue;
        }
        *n += 1;
        out.push(h);
        if out.len() >= cap {
            break;
        }
    }
    out
}

/// `impl Foo`/`impl<'a> Foo<'a>`/`impl Trait for Foo` → 被实现的类型名（后者取 `for` 之后的）。
fn impl_type_of(t: &str) -> Option<String> {
    let rest = t.strip_prefix("impl")?;
    if !rest.starts_with([' ', '<']) {
        return None; // `implementation_detail()` 之类
    }
    let rest = rest.rsplit(" for ").next().unwrap_or(rest);
    rest.trim_start_matches(|c: char| c == '<' || c == '\'' || c.is_alphanumeric() || c == '_' || c == ',' || c == ' ')
        .strip_prefix('>')
        .map(str::trim_start)
        .unwrap_or(rest.trim_start())
        .split(|c: char| !(c.is_alphanumeric() || c == '_'))
        .find(|s| !s.is_empty())
        .map(str::to_string)
}

/// 递归收 `.rs`（不跟符号链接；目录序由调用方排序保证确定性）。
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => collect_rs(&p, out),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "rs") => out.push(p),
            _ => {}
        }
    }
}

/// 把一条命中渲染成契约块：**签名 + 紧邻上方的 doc**，函数体不带（design §9.4 B「签名 + doc，函数体按需」）。
///
/// 签名尾与 doc 的口径**复用仓内文件外片段的那一套**（`sig_end` + `is_comment_line`），
/// 避免同一件事在两处各有一套行为。坐标带 crate 标签与**锁定版本**——模型据此判断「这份签名属于哪个版本」，
/// 而版本正是 L4-B 存在的理由。
pub fn render_contract(crate_dir: &Path, hit: &ContractHit, version: &str) -> Option<String> {
    let text = std::fs::read_to_string(crate_dir.join(&hit.rel)).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let l = hit.line.checked_sub(1)?;
    if l >= lines.len() {
        return None;
    }
    let d = crate::diffsym::DefRange {
        start: l,
        end: (l + super::SIG_MAX_LINES - 1).min(lines.len() - 1),
        name: hit.name.clone(),
        kind: "",
    };
    let end = super::sig_end(&lines, &d);
    let mut start = l;
    while start > 0 && l - start < super::OUTSIDE_DOC_MAX_LINES && crate::diffsym::is_comment_line(lines[start - 1].trim_start()) {
        start -= 1;
    }
    // 同名歧义要在坐标里说清是谁的（design §9.4 B「同名歧义带 crate 标签全给」）：
    // `serde_json::from_str` 与 `Deserializer::from_str` 必须一眼可辨。
    let qualified = match &hit.owner {
        Some(ty) => format!("{ty}::{}", hit.name),
        None => format!("{}::{}", hit.krate, hit.name),
    };
    Some(format!(
        "{krate} {version} — {rel}:{}-{}（三方契约：{qualified}）\n{}\n\n",
        start + 1,
        end + 1,
        lines[start..=end].join("\n"),
        krate = hit.krate,
        rel = hit.rel,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn external_crates_skips_local_builtin_and_relative() {
        let src = r#"
use std::collections::BTreeMap;
use core::fmt;
use crate::domain::Finding;
use super::helper;
use self::inner::X;
use serde_json::Value;
pub use anyhow::Result;
pub(crate) use tokio::sync::Mutex;
use grep_regex::RegexMatcher;
use mymod::thing;
use serde::{
    Serialize,
    Deserialize,
};
use reqwest as http;
"#;
        let got = external_crates(src, &set(&["mymod"]));
        assert_eq!(got, set(&["serde_json", "anyhow", "tokio", "grep_regex", "serde", "reqwest"]));
        assert!(!got.contains("std") && !got.contains("crate") && !got.contains("mymod"), "{got:?}");
        // 多行 use 的续行不该被当成新的 use（`Serialize,` 不以 use 开头，天然跳过）
        assert!(!got.contains("Serialize"), "{got:?}");
    }

    #[test]
    fn locked_versions_reads_package_sections() {
        let lock = r#"
version = 4

[[package]]
name = "anyhow"
version = "1.0.99"

[[package]]
name = "grep-regex"
version = "0.1.13"
dependencies = [
 "regex",
]

[metadata]
name = "not-a-package"
version = "9.9.9"
"#;
        let m = locked_versions(lock);
        assert_eq!(m.get("anyhow").map(String::as_str), Some("1.0.99"));
        assert_eq!(m.get("grep-regex").map(String::as_str), Some("0.1.13"));
        assert!(!m.contains_key("not-a-package"), "`[metadata]` / `[patch]` 这些段不是包，段头一出现就得停收: {m:?}");
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn normalize_bridges_package_and_lib_names() {
        assert_eq!(normalize_crate("grep-regex"), normalize_crate("grep_regex"));
        assert_eq!(normalize_crate("Serde_JSON"), "serde_json");
    }

    #[test]
    fn locate_all_reports_why_it_missed() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("registry/src");
        let idx = reg.join("index.crates.io-1949cf8c6b5b557f");
        std::fs::create_dir_all(idx.join("grep-regex-0.1.13")).unwrap();
        // 同一 crate 的**另一个版本**也在（真实 registry 里多版本共存是常态）——必须挑锁定的那个，
        // 否则会把另一个大版本的签名摆给模型，正是 L4-B 要防的「按记忆写对、按锁定版本是错的」的反面
        std::fs::create_dir_all(idx.join("grep-regex-0.9.0")).unwrap();
        let locked: BTreeMap<String, String> = [("grep-regex", "0.1.13"), ("anyhow", "1.0.99")]
            .into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();

        let (found, misses) = locate_all(&reg, &locked, &set(&["grep_regex", "anyhow", "nosuch"]));

        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found["grep_regex"].ends_with("grep-regex-0.1.13"),
                "要挑 Cargo.lock 锁定的那个版本、且 lib 名与包名之间要能桥接: {found:?}");
        assert!(misses.contains(&LocateMiss::NotLocked("nosuch".into())), "{misses:?}");
        assert!(
            misses.contains(&LocateMiss::NotVendored { krate: "anyhow".into(), version: "1.0.99".into() }),
            "锁到了但没解包 → 要能说清是哪个版本没 fetch: {misses:?}"
        );
    }

    /// registry 里**只有别的版本**时必须判未解包（确定性构造：上一条用例放两个版本，
    /// 命中哪个取决于 `read_dir` 顺序，钉不住）。把另一个大版本的签名摆给模型，
    /// 正是 L4-B 要防的「按记忆写对、按锁定版本是错的」的反面。
    #[test]
    fn a_different_version_in_registry_is_not_a_hit() {
        let dir = tempfile::tempdir().unwrap();
        let reg = dir.path().join("registry/src");
        std::fs::create_dir_all(reg.join("index.crates.io-x").join("serde-9.9.9")).unwrap();
        let locked: BTreeMap<String, String> = [("serde".to_string(), "1.0.0".to_string())].into();

        let (found, misses) = locate_all(&reg, &locked, &set(&["serde"]));

        assert!(found.is_empty(), "锁 1.0.0 却命中了 9.9.9: {found:?}");
        assert_eq!(misses, vec![LocateMiss::NotVendored { krate: "serde".into(), version: "1.0.0".into() }]);
    }

    fn fake_crate(dir: &Path, files: &[(&str, &str)]) {
        for (rel, body) in files {
            let p = dir.join("src").join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        }
    }

    #[test]
    fn find_contracts_matches_public_fn_forms_only() {
        let d = tempfile::tempdir().unwrap();
        fake_crate(d.path(), &[
            ("lib.rs", "/// 取一条。\n/// 第二行 doc。\npub fn get(k: &str) -> Option<String> { None }\n\
                        fn private_get(k: &str) {}\n\
                        pub async fn fetch(u: &str) -> Result<(), ()> { Ok(()) }\n\
                        pub const fn size() -> usize { 4 }\n\
                        pub unsafe fn raw() {}\n\
                        pub extern \"C\" fn ffi() {}\n\
                        // 注释里的 pub fn get_commented() 不算\n"),
            ("nested/mod.rs", "pub(crate) fn get2(v: u8) {}\n"),
        ]);
        // tests/ 不是公开契约面，不该被走
        std::fs::create_dir_all(d.path().join("tests")).unwrap();
        std::fs::write(d.path().join("tests/it.rs"), "pub fn get() {}\n").unwrap();

        let names = set(&["get", "get2", "fetch", "size", "raw", "ffi", "private_get", "nosuch"]);
        let hits = find_contracts(d.path(), "k", &names, 50);
        let got: BTreeSet<String> = hits.iter().map(|h| h.name.clone()).collect();
        assert_eq!(got, set(&["get", "get2", "fetch", "size", "raw", "ffi"]), "{hits:?}");
        assert!(!hits.iter().any(|h| h.rel.contains("tests")), "tests/ 不是公开契约面: {hits:?}");
        // lib.rs 里只有一处 `get`；若搜索根错成整个 crate 目录，tests/it.rs 的那处会挤进来
        assert_eq!(hits.iter().filter(|h| h.name == "get").count(), 1, "{hits:?}");

        // 渲染：签名 + 上方 doc，不带函数体；坐标带 crate 与版本
        let g = hits.iter().find(|h| h.name == "get" && h.rel.ends_with("lib.rs")).unwrap();
        let block = render_contract(d.path(), g, "1.2.3").unwrap();
        assert!(block.starts_with("k 1.2.3 — src/lib.rs:1-3（三方契约：k::get）"), "{block}");
        assert!(block.contains("/// 取一条。") && block.contains("/// 第二行 doc。"), "doc 要带上: {block}");
        assert!(block.contains("pub fn get(k: &str) -> Option<String>"), "{block}");
        assert!(!block.contains("None }\n\nfn private_get"), "不该把后续代码也带出来: {block}");
    }

    /// 自由函数优先、impl 方法带类型名（真实场景：`serde_json::from_str` vs `Deserializer::from_str`
    /// 同名不同物——把后者当契约摆给模型，它会照着错的签名推理，**比不给更糟**）。
    #[test]
    fn free_functions_outrank_impl_methods_and_owners_are_disclosed() {
        let d = tempfile::tempdir().unwrap();
        fake_crate(d.path(), &[
            // 真实形态（serde_json de.rs 的缩影）：impl 方法在前、自由函数在 2600 行之后，
            // 中间塞一段会把花括号计数带偏的内容 —— 顶层判据必须用**缩进**而不是括号深度。
            ("de.rs", "pub struct Deserializer;\n\
                       impl<'a> Deserializer<'a> {\n\
                       \x20   /// 造一个 deserializer。\n\
                       \x20   pub fn from_str(s: &'a str) -> Self { Deserializer }\n\
                       \x20   // 块**内**的字符串字面量把深度抬虚（serde_json de.rs 的真实形态）：\n\
                       \x20   // 闭合之后栈里仍留着这个 impl，`impls.is_empty()` 从此永远为假\n\
                       \x20   const BRACE: &str = \"{\";\n\
                       }\n\
                       /// 解析成 T。\n\
                       pub fn from_str<T>(s: &str) -> Result<T, ()> { Err(()) }\n"),
        ]);

        let hits = find_contracts(d.path(), "sj", &set(&["from_str"]), 50);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].owner, None, "自由函数必须排第一: {hits:?}");
        assert_eq!(hits[1].owner.as_deref(), Some("Deserializer"), "impl 方法要认出归属: {hits:?}");

        let free = render_contract(d.path(), &hits[0], "1.0").unwrap();
        assert!(free.contains("（三方契约：sj::from_str）") && free.contains("解析成 T"), "{free}");
        let meth = render_contract(d.path(), &hits[1], "1.0").unwrap();
        assert!(meth.contains("（三方契约：Deserializer::from_str）"), "同名歧义要一眼可辨: {meth}");
    }

    #[test]
    fn find_contracts_respects_caps_and_empty_inputs() {
        let d = tempfile::tempdir().unwrap();
        fake_crate(d.path(), &[
            ("lib.rs", "pub fn a() {}\npub fn b() {}\npub fn c() {}\n"),
            ("m1.rs", "pub fn a() {}\n"),
            ("m2.rs", "pub fn a() {}\n"),
        ]);
        assert_eq!(find_contracts(d.path(), "k", &set(&["a", "b", "c"]), 2).len(), 2, "cap 封顶");
        // 同名三处、cap 充足 → 每名仍只取 HITS_PER_NAME 处（否则一个大 crate 里的同名函数能刷满整段预算）
        let many = find_contracts(d.path(), "k", &set(&["a"]), 50);
        assert_eq!(many.len(), HITS_PER_NAME, "每名封顶: {many:?}");
        assert!(find_contracts(d.path(), "k", &BTreeSet::new(), 50).is_empty(), "没有引用名 → 不走仓");
        assert!(find_contracts(d.path(), "k", &set(&["a"]), 0).is_empty(), "cap 0 → 不走仓");
        // 源目录不存在（git / path 依赖被误当 registry 包）→ 空，不 panic
        assert!(find_contracts(Path::new("/nonexistent"), "k", &set(&["a"]), 50).is_empty());
    }

    /// registry 根不存在（CI / 他人机器）→ 不 panic、不阻塞，全部计入 misses。
    #[test]
    fn missing_registry_degrades_instead_of_failing() {
        let locked: BTreeMap<String, String> = [("anyhow".to_string(), "1.0.99".to_string())].into();
        let (found, misses) = locate_all(Path::new("/nonexistent/registry/src"), &locked, &set(&["anyhow"]));
        assert!(found.is_empty());
        assert_eq!(misses.len(), 1);
    }
}

#[cfg(test)]
mod live_probe {
    //! 对**本机真实环境**的探针（不是断言本机状态）：本仓的 Cargo.lock + 本机 registry 能定位到几个 crate。
    //! 环境缺失（CI / 没 fetch 过）时整条链应当安静降级——所以这条用例只在能定位到时才断言细节。
    use super::*;

    #[test]
    fn locates_real_crates_of_this_repo_when_registry_is_present() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let Ok(lock) = std::fs::read_to_string(root.join("Cargo.lock")) else { return };
        let locked = locked_versions(&lock);
        assert!(locked.len() > 50, "本仓 Cargo.lock 应解析出上百个包，实际 {}", locked.len());
        assert!(locked.contains_key("grep-regex"), "包名带连字符的那个要在: {:?}", locked.keys().take(5).collect::<Vec<_>>());

        let Some(reg) = registry_root() else { return };
        if !reg.exists() {
            return; // 没 registry 的机器：降级路径由上面的单测覆盖
        }
        let want: BTreeSet<String> =
            ["serde_json", "anyhow", "tokio", "reqwest", "grep_regex"].iter().map(|s| s.to_string()).collect();
        let (found, misses) = locate_all(&reg, &locked, &want);
        assert!(!found.is_empty(), "本机有 registry 却一个都没定位到: {misses:?}");
        for (k, dir) in &found {
            assert!(dir.join("src").exists() || dir.join("Cargo.toml").exists(), "{k} 指向的不像 crate 源: {dir:?}");
        }
        println!("定位到 {}/{}: {:?}", found.len(), want.len(), found.keys().collect::<Vec<_>>());

        // 抽一条真实契约看看形状（本仓确实调了 serde_json::from_str / to_string）
        if let Some(dir) = found.get("serde_json") {
            let names: BTreeSet<String> = ["from_str", "to_string", "from_value"].iter().map(|s| s.to_string()).collect();
            let hits = find_contracts(dir, "serde_json", &names, 20);
            println!("serde_json 命中 {} 条: {:?}", hits.len(),
                     hits.iter().map(|h| format!("{}:{}", h.rel, h.name)).collect::<Vec<_>>());
            if let Some(h) = hits.first() {
                let ver = locked.get("serde_json").cloned().unwrap_or_default();
                let block = render_contract(dir, h, &ver).expect("渲染");
                println!("--- 契约块示例 ---\n{block}");
                assert!(block.contains(&h.name), "块里要有函数名");
                assert!(block.lines().count() <= super::super::SIG_MAX_LINES + super::super::OUTSIDE_DOC_MAX_LINES + 3,
                        "块不该无界：{} 行", block.lines().count());
            }
        }
    }
}
