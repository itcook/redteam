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
    }
}
