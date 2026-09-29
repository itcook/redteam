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
    // 排序三档（2026-09-29 本仓自评审探针实测）：
    // ① **被点名的类型**的方法最优先——`ignore::WalkBuilder::new(…)` 要的是 `WalkBuilder::new`，
    //    而 `ignore` 里一堆 `new`：按路径序只会给到 `Gitignore::new` / `IgnoreBuilder::new`，
    //    owner 虽有披露，「给对的那一条」却落空（A-2 的立项承诺）；
    // ② 顶层自由函数（同名时它才是 `krate::name` 指的那个，如 `serde_json::from_str`）；
    // ③ 其余 impl 方法。档内按路径 / 行号保持确定性。
    let rank = |h: &ContractHit, top: bool| match &h.owner {
        Some(ty) if names.contains(ty) => 0u8,
        _ if top => 1,
        _ => 2,
    };
    all.sort_by(|(at, a), (bt, b)| (rank(a, *at), &a.rel, a.line).cmp(&(rank(b, *bt), &b.rel, b.line)));
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
    render(crate_dir, hit, version, None)
}

/// 带函数体的契约块（A-4：`needs{kind:api}` 命中时才给——design §9.4 C「reviewer cell 带契约 + 函数体」）。
///
/// 体超过 `max_body_lines` 行 → 退回签名 + doc 并注明省略了多少行：模型要的是「这个 callee 到底怎么处理
/// 边界」，一屏看得完才有用；几百行的体只会把区域 pack 挤掉，那正是 A-3 立的规矩。
pub fn render_contract_with_body(crate_dir: &Path, hit: &ContractHit, version: &str, max_body_lines: usize) -> Option<String> {
    render(crate_dir, hit, version, Some(max_body_lines))
}

/// `body = None` → 签名 + doc；`Some(max)` → 再带函数体（超 max 行退回签名并注明）。
fn render(crate_dir: &Path, hit: &ContractHit, version: &str, body: Option<usize>) -> Option<String> {
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
    let sig = super::sig_end(&lines, &d);
    let (end, note) = match body {
        None => (sig, String::new()),
        Some(max) => {
            let b = crate::diffsym::block_end(&lines, l);
            if b > sig && b - l < max {
                (b, String::new())
            } else if b > sig {
                (sig, format!("…（函数体 {} 行，超过 {max} 行上限，未注入）\n", b - sig))
            } else {
                (sig, String::new()) // 单行体 / 定位不到块尾：签名里已经包含
            }
        }
    };
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
    let label = if body.is_some() && note.is_empty() && end > sig { "三方契约 + 函数体" } else { "三方契约" };
    Some(format!(
        "{krate} {version} — {rel}:{}-{}（{label}：{qualified}）\n{}\n{note}\n",
        start + 1,
        end + 1,
        lines[start..=end].join("\n"),
        krate = hit.krate,
        rel = hit.rel,
    ))
}

/// 从一批 Rust 源里抽「**要查哪个 crate 的哪些名字**」（A-3）。
///
/// 两个来源，并集：
/// - `use serde_json::Value;` / `use serde::{Serialize, Deserialize};` → 引入的末端名（多是类型，`pub fn` 搜不到，
///   但成本为零、且 `use foo::bar_fn` 这种直接引入函数的写法很常见）；
/// - `serde_json::from_str(…)` 这种**限定调用** → `(crate, name)`，这是主要来源：路径调用不写 `use`，
///   只看 `use` 会漏掉绝大多数真正被调的三方函数。
///
/// 不用「文件里所有标识符」去搜：`new` / `get` / `len` 这类名字在任何 crate 里都命中一片，
/// 注入的噪音会把仓内上下文挤掉，而 L4-B 的价值恰恰在于**给对的那一条**。
///
/// `known` = `Cargo.lock` 里的包名（归一后）。2018 版**不写 `use` 也能按路径调**
/// （`serde_json::to_string(&x)`），所以限定调用不能只认 `use` 引进来的 crate；但也不能谁都认——
/// 拿 `known` 兜底：能按路径调的必然是依赖，必然在 lock 里。`use` 那条路不过 `known`，
/// 因为「显式 use 了却定位不到」本身就是值得披露的降级原因。
pub fn contract_targets(
    sources: &[(&str, &str)],
    local_mods: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (path, content) in sources {
        if !path.ends_with(".rs") {
            continue;
        }
        merge_targets(&mut out, contract_targets_at(content, &[], local_mods, known));
    }
    out
}

/// 同上，但**只看文件里的这几段区间**（1-based 含端点；空 = 整文件）——A-4 的 `needs{kind:api}` 就落在
/// 某个调用点上，只该问那一处的 callee，而不是把整文件的三方调用都拖进深挖 cell。
///
/// `use` 语句永远在文件头、不在区间内，所以 crate 集仍从**全文**算；区间只用来筛「问哪些名字」。
pub fn contract_targets_at(
    content: &str,
    ranges: &[(u32, u32)],
    local_mods: &BTreeSet<String>,
    known: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    // `krate::fn(` —— 直接的自由函数调用
    let qualified = regex::Regex::new(r"\b([a-z][a-z0-9_]*)::([a-z_][a-z0-9_]*)\s*[(<]").expect("static regex");
    // `krate::Type::method(` —— Rust 里更常见的形态（`regex::Regex::new`、`serde_json::Value::from`）。
    // 取**方法名**去搜（类型名搜不到 `pub fn`）；搜出来的同名方法可能属别的类型，但块里会披露 owner
    // （`RegexSet::new` vs `Regex::new` 一眼可辨），这比完全不给强。
    let assoc = regex::Regex::new(r"\b([a-z][a-z0-9_]*)::([A-Z][A-Za-z0-9_]*)::([a-z_][a-z0-9_]*)\s*[(<]").expect("static regex");
    // `use crate::pack::region::{self as region, …}` 的 `region`、`mod region;` 之后的 `pub use region::X`
    // 都是**本仓符号**，不是三方 crate——`local_mods`（`src/` 顶层）照不到嵌套模块与别名，得从本文件的
    // use / mod 语句认（2026-09-29 自评审实测：省略清单里刷出 region / incremental / spec / tail 四条假 miss）。
    let locals: BTreeSet<String> = local_mods.union(&local_mod_decls(content)).cloned().collect();
    let externals = external_crates(content, &locals);
    let lines: Vec<&str> = content.lines().collect();
    let region: String = if ranges.is_empty() {
        content.to_string()
    } else {
        let mut v: Vec<&str> = Vec::new();
        for &(a, b) in ranges {
            let (lo, hi) = ((a.max(1) - 1) as usize, (b.max(1) - 1) as usize);
            v.extend(lines.iter().take(hi + 1).skip(lo).copied());
        }
        v.join("\n")
    };
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // ① use 引入的末端名——限定了区间时只收**区间里真的出现过**的那些
    for (head, tail) in use_tails(content) {
        if !externals.contains(&head) || (!ranges.is_empty() && !word_appears(&region, &tail)) {
            continue;
        }
        // `use ignore::WalkBuilder;` + `WalkBuilder::new(…)` 是 Rust 的主流写法，而类型名搜不到 `pub fn`：
        // 只收末端名等于什么都没问到（2026-09-29 探针实测）。引进来的若是类型，就去区域里找它的关联调用，
        // 把**方法名**也记上——与 `krate::Type::method(` 同款，owner 排序据此认得出是被点名的那个。
        if tail.starts_with(char::is_uppercase) {
            if let Ok(re) = regex::Regex::new(&format!(r"\b{}::([a-z_][a-z0-9_]*)\s*[(<]", regex::escape(&tail))) {
                for c in re.captures_iter(&region) {
                    out.entry(head.clone()).or_default().insert(c[1].to_string());
                }
            }
        }
        out.entry(head).or_default().insert(tail);
    }
    let callable = |k: &str| {
        !locals.contains(k)
            && !matches!(k, "std" | "core" | "alloc" | "crate" | "super" | "self")
            && (externals.contains(k) || known.contains(&normalize_crate(k)))
    };
    // ② 限定调用 `krate::name(` / `krate::name::<T>(`
    for c in qualified.captures_iter(&region) {
        let (k, n) = (&c[1], &c[2]);
        if callable(k) {
            out.entry(k.to_string()).or_default().insert(n.to_string());
        }
    }
    // ③ 限定关联调用 `krate::Type::method(`：方法名 + **类型名**都记。
    // 类型名自己搜不到 `pub fn`（零副作用），但它让 `find_contracts` 认得出「这个 owner 是被点名的」，
    // 从而把 `WalkBuilder::new` 排在同 crate 其它 `new` 前面（2026-09-29 本仓自评审探针实测）。
    for c in assoc.captures_iter(&region) {
        let (k, ty, n) = (&c[1], &c[2], &c[3]);
        if callable(k) {
            let e = out.entry(k.to_string()).or_default();
            e.insert(n.to_string());
            e.insert(ty.to_string());
        }
    }
    out
}

fn merge_targets(into: &mut BTreeMap<String, BTreeSet<String>>, more: BTreeMap<String, BTreeSet<String>>) {
    for (k, names) in more {
        into.entry(k).or_default().extend(names);
    }
}

/// `use a::b::{c, d as e};` → `[(a, b), (a, c), (a, d), (a, e)]`（头 + 各末端 token，别名也收）。
fn use_tails(content: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in content.lines() {
        let t = line.trim();
        let t = t.strip_prefix("pub").map(str::trim_start).unwrap_or(t);
        let t = if t.starts_with('(') { t.split_once(')').map(|(_, r)| r.trim_start()).unwrap_or(t) } else { t };
        let Some(rest) = t.strip_prefix("use ") else { continue };
        let head = rest.trim_start().split(|c: char| !(c.is_alphanumeric() || c == '_')).find(|s| !s.is_empty()).unwrap_or("");
        if head.is_empty() {
            continue;
        }
        for tok in rest.split(|c: char| !(c.is_alphanumeric() || c == '_')).filter(|s| !s.is_empty()).skip(1) {
            if tok != "as" && tok != head {
                out.push((head.to_string(), tok.to_string()));
            }
        }
    }
    out
}

/// 本文件声明的子模块（`mod region;`）——同文件里的 `pub use region::X` / `region::build(` 都是本仓符号。
/// `local_mods`（`src/` 顶层目录）照不到它们：自评审实测省略清单因此刷出 region / incremental / spec / tail
/// 四条假 miss。`use crate::…` 那条路不必管：它的头就是 `crate`，`external_crates` 本来就跳过。
fn local_mod_decls(content: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in content.lines() {
        let t = line.trim();
        let t = t.strip_prefix("pub").map(str::trim_start).unwrap_or(t);
        let t = if t.starts_with('(') { t.split_once(')').map(|(_, r)| r.trim_start()).unwrap_or(t) } else { t };
        let Some(rest) = t.strip_prefix("mod ") else { continue };
        let name = rest.trim_end_matches([';', ' ', '{']).trim();
        if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            out.insert(name.to_string());
        }
    }
    out
}

/// `name` 是否以**完整标识符**出现在文本里（`from_str` 不该被 `parse_from_string` 命中）。
fn word_appears(text: &str, name: &str) -> bool {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == name)
}

/// 本 crate 的顶层模块名（`src/` 下的目录与 `.rs` 文件名）。
///
/// 2018 版里 `use foo::bar` 的 `foo` 既可能是三方 crate，也可能是本 crate 的顶层模块
/// （`mod foo;` 之后可以直接 `use foo::…`）。把它们排除掉，否则会拿着本仓的模块名去 registry 里空转。
pub fn local_module_names(repo_root: &Path) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(repo_root.join("src")) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        let name = match e.file_type() {
            Ok(t) if t.is_dir() => p.file_name().and_then(|n| n.to_str()).map(str::to_string),
            Ok(t) if t.is_file() && p.extension().is_some_and(|x| x == "rs") => {
                p.file_stem().and_then(|n| n.to_str()).map(str::to_string)
            }
            _ => None,
        };
        match name.as_deref() {
            None | Some("main") | Some("lib") | Some("mod") => {}
            Some(n) => {
                out.insert(n.to_string());
            }
        }
    }
    out
}

/// 一段可注入的三方契约文本 + 它的账（A-3）。
#[derive(Debug, Default)]
pub struct ContractSection {
    /// 渲染好的块（已按 `cap` 截断）。
    pub text: String,
    /// 实际注入的契约块数。
    pub blocks: usize,
    /// 定位不到的原因（进 omit / RunMeta 披露——registry 在 CI / 他人机器缺失时必须看得见是降级了）。
    pub misses: Vec<LocateMiss>,
    /// 因为超帽没能进去的块数。
    pub spilled: usize,
}

/// 组装三方契约段：定位 → 抽取 → 渲染，总字节受 `cap` 封顶。
///
/// `cap` 由调用方给**仓内上下文用剩的额度**——三方契约不得挤掉仓内的调用点与签名（它们更贴近本次改动）。
pub fn contracts_section(
    repo_root: &Path,
    sources: &[(&str, &str)],
    local_mods: &BTreeSet<String>,
    cap: usize,
) -> ContractSection {
    let Some(reg) = registry_root() else { return ContractSection::default() };
    contracts_section_in(repo_root, &reg, sources, local_mods, cap)
}

/// 同上，但 registry 根由调用方给——`CARGO_HOME` 是进程级全局，测试里改它会互相打架，
/// 所以把「从哪儿找」这件事留在边界上（[`contracts_section`] 是唯一读环境的那层）。
pub fn contracts_section_in(
    repo_root: &Path,
    registry_src: &Path,
    sources: &[(&str, &str)],
    local_mods: &BTreeSet<String>,
    cap: usize,
) -> ContractSection {
    let Some((locked, known)) = lock_of(repo_root) else { return ContractSection::default() };
    let mut targets = contract_targets(sources, local_mods, &known);
    drop_own_package(repo_root, &mut targets);
    assemble(registry_src, &locked, &targets, cap, None)
}

/// `needs{kind: api}` 的契约段（A-4，design §9.4 C）：只看**声明命中的那几段区间**，并带函数体。
///
/// 与 [`contracts_section`] 的区别只有这两条——定位链、预算帽、降级披露全走同一套。
pub fn api_contracts_section(
    repo_root: &Path,
    content: &str,
    ranges: &[(u32, u32)],
    local_mods: &BTreeSet<String>,
    cap: usize,
    max_body_lines: usize,
) -> ContractSection {
    let Some(reg) = registry_root() else { return ContractSection::default() };
    api_contracts_section_in(repo_root, &reg, content, ranges, local_mods, cap, max_body_lines)
}

/// 同上，registry 根由调用方给（测试用；`CARGO_HOME` 是进程级全局）。
#[allow(clippy::too_many_arguments)] // 仓根 / registry / 源文 / 区间 / 本地模块 / 帽 / 体上限各不同源
pub fn api_contracts_section_in(
    repo_root: &Path,
    registry_src: &Path,
    content: &str,
    ranges: &[(u32, u32)],
    local_mods: &BTreeSet<String>,
    cap: usize,
    max_body_lines: usize,
) -> ContractSection {
    let Some((locked, known)) = lock_of(repo_root) else { return ContractSection::default() };
    let mut targets = contract_targets_at(content, ranges, local_mods, &known);
    drop_own_package(repo_root, &mut targets);
    assemble(registry_src, &locked, &targets, cap, Some(max_body_lines))
}

/// `Cargo.lock` → (包名 → 版本, 归一包名集)。读不到 = 不是 Cargo 工作区，安静降级（不是错误）。
///
/// **本仓自己的包名要剔掉**：集成测试里的 `redteam::pack::…` 是本 crate，不是三方——留着它只会在
/// 省略清单里刷一条「registry 无解包目录」（2026-09-29 自评审实测）。
fn lock_of(repo_root: &Path) -> Option<(BTreeMap<String, String>, BTreeSet<String>)> {
    let lock = std::fs::read_to_string(repo_root.join("Cargo.lock")).ok()?;
    let locked = locked_versions(&lock);
    let known = locked.keys().map(|k| normalize_crate(k)).collect();
    Some((locked, known))
}

/// 从目标里剔掉本仓自己的包（集成测试写 `redteam::pack::…`，那是本 crate 不是三方）。
fn drop_own_package(repo_root: &Path, targets: &mut BTreeMap<String, BTreeSet<String>>) {
    if let Some(own) = root_package_name(repo_root) {
        targets.retain(|k, _| normalize_crate(k) != normalize_crate(&own));
    }
}

/// `Cargo.toml` 的 `[package] name`（工作区根无 `[package]` 时 None）。
fn root_package_name(repo_root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(repo_root.join("Cargo.toml")).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_package = t == "[package]";
            continue;
        }
        if in_package {
            if let Some(v) = t.strip_prefix("name").map(str::trim_start).and_then(|r| r.strip_prefix('=')) {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// 定位 → 抽取 → 渲染，总字节受 `cap` 硬帽。`body = Some(n)` 时带函数体（A-4）。
fn assemble(
    registry_src: &Path,
    locked: &BTreeMap<String, String>,
    targets: &BTreeMap<String, BTreeSet<String>>,
    cap: usize,
    body: Option<usize>,
) -> ContractSection {
    let mut out = ContractSection::default();
    if cap == 0 || targets.is_empty() {
        return out;
    }
    let names: BTreeSet<String> = targets.keys().cloned().collect();
    let (found, misses) = locate_all(registry_src, locked, &names);
    out.misses = misses;
    for (krate, dir) in &found {
        let Some(want) = targets.get(krate) else { continue };
        let version = locked
            .iter()
            .find(|(k, _)| normalize_crate(k) == normalize_crate(krate))
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        for hit in find_contracts(dir, krate, want, HITS_PER_NAME * 4) {
            let block = match body {
                None => render_contract(dir, &hit, &version),
                Some(max) => render_contract_with_body(dir, &hit, &version, max),
            };
            let Some(block) = block else { continue };
            if out.text.len() + block.len() > cap {
                out.spilled += 1;
                continue;
            }
            out.text.push_str(&block);
            out.blocks += 1;
        }
    }
    out
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

    /// 被点名的类型的方法优先（2026-09-29 本仓自评审探针实测）：`krate::Type::method(` 里 `Type` 也进了
    /// 目标集，`find_contracts` 据此把 `Type::method` 排在同 crate 其它同名方法前面——否则按路径序
    /// 只会给到别的类型的 `new`，owner 虽有披露，「给对的那一条」却落空。
    #[test]
    fn the_named_owners_method_outranks_other_same_named_methods() {
        let d = tempfile::tempdir().unwrap();
        fake_crate(
            d.path(),
            &[
                // 路径序在前、且同名——没有 owner 优先就是它们占满 HITS_PER_NAME
                ("aaa.rs", "pub struct Other;\nimpl Other {\n    pub fn new(x: u8) -> Self { Other }\n}\n"),
                ("bbb.rs", "pub struct Another;\nimpl Another {\n    pub fn new(x: u8) -> Self { Another }\n}\n"),
                ("zzz.rs", "pub struct WalkBuilder;\nimpl WalkBuilder {\n    pub fn new(p: &str) -> Self { WalkBuilder }\n}\n"),
            ],
        );

        let hits = find_contracts(d.path(), "k", &set(&["new", "WalkBuilder"]), 50);

        assert_eq!(
            hits.first().map(|h| (h.owner.clone(), h.rel.clone())),
            Some((Some("WalkBuilder".into()), "src/zzz.rs".into())),
            "被点名的 owner 要排第一: {hits:?}"
        );
        // 没点名类型时退回原序（路径序），行为不变
        let plain = find_contracts(d.path(), "k", &set(&["new"]), 50);
        assert_eq!(plain.first().map(|h| h.rel.clone()), Some("src/aaa.rs".into()), "{plain:?}");
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

    #[test]
    fn contract_targets_takes_use_tails_and_qualified_calls() {
        let rs = r#"
use serde_json::Value;
use anyhow::{Context, bail};
use mymod::helper;
fn f(s: &str) -> Value {
    let v = serde_json::from_str(s).unwrap();
    let t = serde_json::to_string_pretty(&v).unwrap();
    let r = regex::Regex::new("x").unwrap();
    let n = tokio::spawn(async {});
    let g = v.get("k");              // 无 crate 限定 → 不该被当成目标
    mymod::local_fn(t, r, n, g);     // 本仓模块 → 不该进
    nosuchthing::do_it();            // 既没 use、也不在 lock 里 → 不该进（否则 misses 全是垃圾）
}
"#;
        let got = contract_targets(&[("src/a.rs", rs)], &set(&["mymod"]), &set(&["regex", "tokio", "mymod"]));

        assert_eq!(got["serde_json"], set(&["Value", "from_str", "to_string_pretty"]));
        assert_eq!(got["anyhow"], set(&["Context", "bail"]));
        assert_eq!(
            got["regex"], set(&["Regex", "new"]),
            "`regex::Regex::new` 没写 use，靠 Cargo.lock 认；方法名去搜、类型名留着给 owner 排序"
        );
        assert_eq!(got["tokio"], set(&["spawn"]));
        assert!(!got.contains_key("nosuchthing"), "不在 lock 里的限定路径不该当 crate: {:?}", got.keys().collect::<Vec<_>>());
        assert!(!got.contains_key("mymod"), "本仓模块不该去 registry 空转: {:?}", got.keys().collect::<Vec<_>>());
        // `v.get("k")` 是方法调用、不带 crate 限定——**不能**把它算进任何 crate：
        // `get` / `new` / `len` 这类名字在任何 crate 里都命中一片，注入的噪音会把仓内上下文挤掉
        assert!(got.values().all(|n| !n.contains("get")), "无限定的方法名混进来了: {got:?}");
    }

    /// `use krate::Type;` 之后按裸类型名调（Rust 主流写法）也要能抽出目标——只收末端名等于什么都没问到。
    #[test]
    fn use_of_a_type_picks_up_its_associated_calls() {
        let rs = "use ignore::WalkBuilder;\nuse serde_json::Value;\nfn f(p: &str) -> usize {\n    let v = Value::Null;\n    WalkBuilder::new(p).hidden(false).build().count() + v.is_null() as usize\n}\n";

        let got = contract_targets_at(rs, &[], &BTreeSet::new(), &set(&["ignore", "serde_json"]));

        assert_eq!(got["ignore"], set(&["WalkBuilder", "new"]), "类型名 + 它的**关联**调用都要进");
        assert_eq!(got["serde_json"], set(&["Value"]), "`Value::Null` 是常量不是调用，只留类型名: {got:?}");
        // 已知边界：`.hidden(false)` / `.build()` 是**实例方法**，没有类型推断就无从归属到某个 crate。
        // 硬凑（看见 `.hidden(` 就去 ignore 里搜）会把 `new` / `get` / `build` 这类名字在每个 crate 里
        // 都命中一片——正是本函数开头拒绝的那条路。builder 的默认值（`hidden` 默认开）因此进不了契约段。
        assert!(!got["ignore"].contains("hidden") && !got["ignore"].contains("build"), "{got:?}");
        // 区间之外的不收（与 ① 同款）：第 3-4 行没提 WalkBuilder
        assert_eq!(contract_targets_at(rs, &[(3, 4)], &BTreeSet::new(), &set(&["ignore"])).get("ignore"), None);
    }

    #[test]
    fn contract_targets_skips_non_rust_sources() {
        let ts = "import { x } from 'serde_json';\nserde_json::from_str(s);\n";
        assert!(
            contract_targets(&[("src/a.ts", ts)], &BTreeSet::new(), &set(&["serde_json"])).is_empty(),
            "非 .rs 不走 Rust 定位链（Go / TS 是已知缺口）"
        );
    }

    #[test]
    fn local_module_names_reads_src_layout() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src/pack")).unwrap();
        std::fs::write(d.path().join("src/lib.rs"), "").unwrap();
        std::fs::write(d.path().join("src/main.rs"), "").unwrap();
        std::fs::write(d.path().join("src/domain.rs"), "").unwrap();
        std::fs::write(d.path().join("src/notes.md"), "").unwrap();
        std::fs::write(d.path().join("src/pack/mod.rs"), "").unwrap();

        let got = local_module_names(d.path());

        assert_eq!(got, set(&["domain", "pack"]), "顶层模块 = 目录 + .rs 文件名，排除 lib/main/mod 与非 .rs");
        assert!(local_module_names(Path::new("/nonexistent")).is_empty(), "没有 src/ → 空，不 panic");
    }

    /// 组装段：定位 → 抽取 → 渲染，且 **cap 是硬帽**（超出的记 spilled，不进文本）。
    #[test]
    fn contracts_section_renders_within_cap_and_counts_spill() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.lock"), "[[package]]\nname = \"kit\"\nversion = \"2.1.0\"\n").unwrap();
        let reg = d.path().join("registry/src");
        let kit = reg.join("index-x/kit-2.1.0");
        fake_crate(&kit, &[(
            "lib.rs",
            "/// 造一个东西。\npub fn make(a: u8) -> u8 {\n    let secret = a + 1;\n    secret\n}\n\n/// 拆一个东西。\npub fn take(a: u8) -> u8 {\n    a\n}\n",
        )]);
        let src = "use kit::make;\nfn f() { kit::take(1); make(2); }\n";

        let big = contracts_section_in(&repo, &reg, &[("src/a.rs", src)], &BTreeSet::new(), 4096);
        assert_eq!((big.blocks, big.spilled), (2, 0), "两个名字都该抽到: {}", big.text);
        assert!(big.text.contains("kit 2.1.0"), "坐标要带锁定版本（L4-B 的理由就在版本上）: {}", big.text);
        assert!(big.text.contains("pub fn make") && big.text.contains("拆一个东西"), "签名 + 上方 doc: {}", big.text);
        assert!(!big.text.contains("let secret"), "函数体默认不带（design §9.4 B）: {}", big.text);
        assert!(big.misses.is_empty(), "{:?}", big.misses);

        // 帽只放得下一块 → 另一块计 spilled，且**文本不得超帽**
        let one = contracts_section_in(&repo, &reg, &[("src/a.rs", src)], &BTreeSet::new(), big.text.len() - 1);
        assert_eq!((one.blocks, one.spilled), (1, 1), "{}", one.text);
        assert!(one.text.len() < big.text.len(), "帽是硬的: {} 字节", one.text.len());
        assert_eq!(contracts_section_in(&repo, &reg, &[("src/a.rs", src)], &BTreeSet::new(), 0).blocks, 0);
    }

    /// 本仓自己的包不是三方：`redteam::pack::…`（集成测试的写法）不该进目标、更不该在省略清单里
    /// 刷一条「registry 无解包目录」。
    #[test]
    fn the_repos_own_package_is_not_a_third_party_target() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.toml"), "[package]\nname = \"redteam\"\nversion = \"0.1.0\"\n\n[dependencies]\nkit = \"2\"\n").unwrap();
        std::fs::write(repo.join("Cargo.lock"), "[[package]]\nname = \"redteam\"\nversion = \"0.1.0\"\n\n[[package]]\nname = \"kit\"\nversion = \"2.1.0\"\n").unwrap();
        let reg = d.path().join("registry/src");
        std::fs::create_dir_all(reg.join("index-x")).unwrap();
        // 两类本仓符号：包名 `redteam`（集成测试 `use redteam::…`）、同文件声明的子模块 `helper`
        let own = "use redteam::pack::Packed;\nfn t() { kit::make(1); }\n";
        let submod = "mod helper;\npub use helper::Thing;\nfn u() { helper::run(1); }\n";

        let sec = contracts_section_in(&repo, &reg, &[("tests/e2e.rs", own), ("src/lib.rs", submod)], &BTreeSet::new(), 4096);

        assert_eq!(sec.misses, vec![LocateMiss::NotVendored { krate: "kit".into(), version: "2.1.0".into() }],
                   "只该报三方那条——本仓包名与子模块都不是三方: {:?}", sec.misses);
        assert_eq!(root_package_name(&repo).as_deref(), Some("redteam"));
    }

    /// A-4：`needs{kind:api}` 段——**只问声明命中的区间**，并带函数体；体太长退回签名并注明。
    #[test]
    fn api_section_scopes_to_ranges_and_carries_the_body() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("Cargo.lock"), "[[package]]\nname = \"kit\"\nversion = \"2.1.0\"\n").unwrap();
        let reg = d.path().join("registry/src");
        let long_body = (0..80).map(|i| format!("    let x{i} = {i};\n")).collect::<String>();
        fake_crate(
            &reg.join("index-x/kit-2.1.0"),
            &[("lib.rs", &format!(
                "/// 造一个东西。\npub fn make(a: u8) -> u8 {{\n    let secret = a + 1;\n    secret\n}}\n\n/// 很长的那个。\npub fn huge(a: u8) -> u8 {{\n{long_body}    a\n}}\n"
            ))],
        );
        let src = "use kit::make;\nfn f() {\n    kit::make(1);\n}\nfn g() {\n    kit::huge(2);\n}\n";

        // 只问第 3 行那一处调用 → 只给 make，不给 g() 里的 huge
        let scoped = api_contracts_section_in(&repo, &reg, src, &[(3, 3)], &BTreeSet::new(), 8192, 60);
        assert_eq!(scoped.blocks, 1, "{}", scoped.text);
        assert!(scoped.text.contains("三方契约 + 函数体：kit::make"), "{}", scoped.text);
        assert!(scoped.text.contains("let secret = a + 1;"), "A-4 要的是连函数体: {}", scoped.text);
        assert!(!scoped.text.contains("huge"), "区间外的调用不该被拖进来: {}", scoped.text);

        // 体超上限 → 退回签名 + doc，并说清省了多少行（几百行的体只会把区域切片挤掉）
        let huge = api_contracts_section_in(&repo, &reg, src, &[(6, 6)], &BTreeSet::new(), 8192, 10);
        assert_eq!(huge.blocks, 1, "{}", huge.text);
        assert!(huge.text.contains("pub fn huge") && !huge.text.contains("let x40"), "{}", huge.text);
        assert!(huge.text.contains("超过 10 行上限，未注入"), "退回要说明白: {}", huge.text);

        // 帽同样是硬的
        assert_eq!(api_contracts_section_in(&repo, &reg, src, &[(3, 3)], &BTreeSet::new(), 0, 60).blocks, 0);
    }

    /// 三条降级路径：非 Cargo 工作区 / 包没锁 / 锁了没解包——一律空段 + 可披露原因，不 panic。
    #[test]
    fn contracts_section_degrades_without_lock_or_registry() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let reg = d.path().join("registry/src");
        std::fs::create_dir_all(reg.join("index-x")).unwrap();
        let src = "use kit::make;\nfn f() { kit::take(1); }\n";

        let no_lock = contracts_section_in(&repo, &reg, &[("src/a.rs", src)], &BTreeSet::new(), 4096);
        assert!(no_lock.text.is_empty() && no_lock.misses.is_empty(), "没 Cargo.lock 不是错误，安静降级");

        std::fs::write(repo.join("Cargo.lock"), "[[package]]\nname = \"other\"\nversion = \"1.0.0\"\n").unwrap();
        let not_locked = contracts_section_in(&repo, &reg, &[("src/a.rs", src)], &BTreeSet::new(), 4096);
        assert_eq!(not_locked.misses, vec![LocateMiss::NotLocked("kit".into())]);
        assert_eq!(not_locked.blocks, 0);

        std::fs::write(repo.join("Cargo.lock"), "[[package]]\nname = \"kit\"\nversion = \"2.1.0\"\n").unwrap();
        let not_vendored = contracts_section_in(&repo, &reg, &[("src/a.rs", src)], &BTreeSet::new(), 4096);
        assert_eq!(not_vendored.misses, vec![LocateMiss::NotVendored { krate: "kit".into(), version: "2.1.0".into() }]);
        assert!(not_vendored.text.is_empty());
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
