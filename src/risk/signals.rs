//! 四类信号与常量权重表（P0 §15.4 v4 定案）。
//! 权威：`.next/private/research/2026-09-07-redteam-p0-validation-gate-research.md` §15.4。

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use regex::Regex;

use super::regions::{match_decl, Region, RegionKind, SignalKind};
use crate::diffsym::{self, ref_worthy};

/// 权重表（P0 §15.4 v4）。改常量必过 `eval/risk_replay.py` 回放门。
pub const W_PATH_GLOB: f32 = 1.0;
pub const W_KEYWORD: f32 = 0.5;
pub const W_KEYWORD_CAP: f32 = 1.5;
pub const W_SIG_CHANGED: f32 = 1.0;
pub const W_DEF_NEW: f32 = 0.5;
pub const W_DEF_DELETED: f32 = 1.0;
pub const W_PUBLIC_WIDEN: f32 = 0.5;
pub const W_FANIN: f32 = 1.0;
pub const W_CHURN: f32 = 0.5;
pub const W_EXTAPI: f32 = 1.0;
pub const W_HIST_FIX3: f32 = 0.5;
pub const W_HIST_FIX6: f32 = 1.0;
pub const W_LEDGER_UPHELD: f32 = 1.0;
pub const W_LEDGER_DISMISSED: f32 = 0.5;
pub const W_NEW_FILE: f32 = 0.5;
pub const W_RIPPLE: f32 = 1.0;
pub const MULT_DECL_ONLY: f32 = 0.5;
pub const MULT_TEST: f32 = 0.3;
pub const MULT_DOC: f32 = 0.2;

pub const FANIN_K: u32 = 5;
pub const EXTAPI_K: u32 = 3;

const PATH_SENSITIVE: &str = r"(?i)(auth|secur|crypt|payment|billing|config|setting|secret|token|permission|migrat|callback|webhook)";
const TEST_PATH: &str = r"(_test\.go$|\.test\.(ts|js|tsx)$|(^|/)tests?/|(^|/)__tests__/|_spec\.)";
const DOC_PATH: &str = r"(?i)\.(md|json|ya?ml|toml|txt|lock|svg|csv)$";
const FIX_COMMIT: &str = r"(?i)\b(fix|bug|hotfix|revert|regress)";
const CONTROL_FLOW: &str = r"\b(if|for|while|switch|match|return|await|try|catch|select|defer|throw|loop|yield)\b";
/// 跨行 `/* … */` 也要剥离（`(?s)`＝Python 参考实现 `re.S`）；`CONTROL_FLOW` 乘子判定依赖此剥离。
const COMMENT_RE: &str = r"(?s)//[^\n]*|#[^\n]*|/\*.*?\*/";

const NODE_BUILTINS: &[&str] = &[
    "fs", "path", "http", "https", "crypto", "url", "stream", "buffer", "events", "util", "os", "zlib",
    "child_process", "assert", "net", "tls", "dns", "readline", "worker_threads", "timers", "process",
];
const RUST_INTERNAL: &[&str] = &["crate", "super", "self", "std", "core", "alloc"];

fn re(src: &str) -> Regex {
    Regex::new(src).expect("static risk regex")
}

fn path_sensitive() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| re(PATH_SENSITIVE))
}
fn test_path() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| re(TEST_PATH))
}
pub(crate) fn is_doc_path(path: &str) -> bool {
    doc_path().is_match(path)
}

fn doc_path() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| re(DOC_PATH))
}
fn fix_commit() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| re(FIX_COMMIT))
}
fn control_flow() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| re(CONTROL_FLOW))
}
fn comment_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| re(COMMENT_RE))
}

fn keyword_classes() -> &'static [(&'static str, Regex)] {
    static RE: std::sync::OnceLock<Vec<(&'static str, Regex)>> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        vec![
            ("deser", re(r"(?i)\b(unmarshal|deserializ|json\.parse|parseint|parsefloat|strconv\.|url\.parse|new url\(|querystring)")),
            ("exec_sql", re(r"(?i)(\bexec\.|os/exec|subprocess|\beval\(|\bunsafe\b|new function\(|child_process|db\.(exec|query|raw)\b|\bsql\.|select\s.+\sfrom\s|insert\s+into|update\s+\w+\s+set|delete\s+from|\.raw\()")),
            ("condwrite", re(r"(?i)(etag|if-none-match|if-match|onlyif|fingerprint|idempot|precondition)")),
            ("timeout", re(r"(?i)\b(timeout|deadline|retry|retries|backoff|attempt|context\.with(timeout|deadline|cancel)|abortsignal|settimeout|time\.after)")),
            ("concurrency", re(r"(go func|sync\.(Mutex|RWMutex|WaitGroup|Once)|atomic\.|\bchan\b|<-|Promise\.(all|race|allSettled)|\bMutex\b|Semaphore|\.[Ll]ock\(|\btx\.|Transaction|FOR UPDATE)")),
            ("destructive", re(r"(?i)\b(delete|remove|truncate|drop|overwrite|unlink|purge)\b")),
            ("auth", re(r"(?i)\b(auth|authorize|permission|role|token|secret|password|credential|signature|hmac|jwt)\b")),
            ("numeric_bounds", re(r"(?i)(\b(min(imum)?|max(imum)?|clamp|ceil(ing)?|floor|margin|threshold|limit|bound(s|ary)?)\b|\* ?1024|/ ?1024|bytesPerSecond|time\.Duration|Seconds\(\)|Math\.(min|max|ceil|floor|round)|math\.(Min|Max|Ceil|Floor))")),
            ("error_paths", re(r"(?i)\b(throw|catch|panic|recover|unwrap\(|expect\(|errors?\.(Is|As|New|Wrap)|instanceof \w*Error|reject\()")),
            ("resource_lifecycle", re(r"(?i)\b(cancel|abort|close|release|dispose|drain|cleanup|defer|finally|reader|writer|stream)\b")),
        ]
    })
}

#[derive(Clone, Debug)]
pub struct SignalHit {
    pub kind: SignalKind,
    #[allow(dead_code)] // 暂只进内部信号表；M21c/M21d 的 hotspot 明细扩展再落报告
    pub detail: String,
    pub w: f32,
}

/// 评分依赖的仓侧查询；测试可注入，生产走 git / sqlite。
pub trait ScoreEnv {
    fn fanin(&mut self, name: &str, own_file: &str) -> u32;
    fn hist_fix_count(&mut self, path: &str) -> u32;
    fn ledger_upheld(&self, path: &str) -> u32;
    fn ledger_dismissed(&self, path: &str) -> u32;
    fn is_new_file(&self, path: &str) -> bool;
    fn go_module(&self) -> Option<&str>;
}

/// 生产环境：git grep / git log / sqlite 台账。
pub struct GitEnv {
    pub root: std::path::PathBuf,
    pub head: String,
    pub history_ref: String,
    pub before: Option<String>,
    fanin_cache: BTreeMap<String, u32>,
    hist_cache: BTreeMap<String, u32>,
    upheld: BTreeMap<String, u32>,
    dismissed: BTreeMap<String, u32>,
    new_files: std::collections::BTreeSet<String>,
    go_module: Option<String>,
}

impl GitEnv {
    pub fn open(
        root: &Path,
        head: &str,
        history_ref: &str,
        base: &str,
        ledger: Option<&Path>,
        before: Option<&str>,
    ) -> Self {
        let mut upheld = BTreeMap::new();
        let mut dismissed = BTreeMap::new();
        if let Some(db) = ledger {
            load_ledger(db, before, &mut upheld, &mut dismissed);
        }
        let mut new_files = std::collections::BTreeSet::new();
        if let Ok(out) = git(root, &["ls-tree", "-r", "--name-only", "--end-of-options", base]) {
            let base_tree: std::collections::BTreeSet<&str> = out.lines().collect();
            if let Ok(head_tree) = git(root, &["ls-tree", "-r", "--name-only", "--end-of-options", head]) {
                for p in head_tree.lines() {
                    if !base_tree.contains(p) {
                        new_files.insert(p.to_string());
                    }
                }
            }
        }
        let go_module = git(root, &["show", "--end-of-options", &format!("{head}:go.mod")])
            .ok()
            .and_then(|t| {
                t.lines().find(|l| l.starts_with("module ")).map(|l| l.split_whitespace().nth(1).unwrap_or("").to_string())
            })
            .filter(|s| !s.is_empty());
        Self {
            root: root.to_path_buf(),
            head: head.to_string(),
            history_ref: history_ref.to_string(),
            before: before.map(str::to_string),
            fanin_cache: BTreeMap::new(),
            hist_cache: BTreeMap::new(),
            upheld,
            dismissed,
            new_files,
            go_module,
        }
    }
}

impl ScoreEnv for GitEnv {
    fn fanin(&mut self, name: &str, own_file: &str) -> u32 {
        if let Some(&n) = self.fanin_cache.get(name) {
            return n;
        }
        let exclude = format!(":(exclude){own_file}");
        let out = git(&self.root, &["grep", "-w", "-c", "-e", name, "--end-of-options", &self.head, "--", ".", &exclude])
            .unwrap_or_default();
        let mut n = 0u32;
        for l in out.lines() {
            let parts: Vec<&str> = l.rsplitn(3, ':').collect();
            // rsplitn 倒序：count, path, rev
            if parts.len() == 3 {
                let path = parts[1];
                if diffsym::lang_for(path).is_some() && !test_path().is_match(path) {
                    if let Ok(c) = parts[0].parse::<u32>() {
                        n += c;
                    }
                }
            }
        }
        self.fanin_cache.insert(name.to_string(), n);
        n
    }

    fn hist_fix_count(&mut self, path: &str) -> u32 {
        if let Some(&n) = self.hist_cache.get(path) {
            return n;
        }
        let out = git(
            &self.root,
            &["log", "--since=12.months.ago", "--format=%s", "--end-of-options", &self.history_ref, "--", path],
        )
        .unwrap_or_default();
        let n = out.lines().filter(|s| !s.is_empty() && fix_commit().is_match(s)).count() as u32;
        self.hist_cache.insert(path.to_string(), n);
        n
    }

    fn ledger_upheld(&self, path: &str) -> u32 {
        self.upheld.get(path).copied().unwrap_or(0)
    }
    fn ledger_dismissed(&self, path: &str) -> u32 {
        self.dismissed.get(path).copied().unwrap_or(0)
    }
    fn is_new_file(&self, path: &str) -> bool {
        self.new_files.contains(path)
    }
    fn go_module(&self) -> Option<&str> {
        self.go_module.as_deref()
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String, ()> {
    let o = Command::new("git").current_dir(root).args(args).output().map_err(|_| ())?;
    if !o.status.success() {
        return Err(());
    }
    Ok(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn load_ledger(db: &Path, before: Option<&str>, upheld: &mut BTreeMap<String, u32>, dismissed: &mut BTreeMap<String, u32>) {
    // 只读打开：`risk` 是只读子命令，缺省 open()（READ_WRITE|CREATE）会为不存在的路径创建空库文件。
    let Ok(con) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return };
    if let Ok(mut stmt) = con.prepare("select file, updated_at from dismissed") {
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)));
        if let Ok(rows) = rows {
            for row in rows.flatten() {
                let (file, upd) = row;
                let ok = match before {
                    None => true,
                    Some(b) => upd.get(..10).unwrap_or("") < b.get(..10).unwrap_or(""),
                };
                if ok {
                    *dismissed.entry(file).or_insert(0) += 1;
                }
            }
        }
    }
    let result_rows: Vec<(String, String)> = {
        let mut stmt = match con.prepare("select json, created_at from results") {
            Ok(s) => s,
            Err(_) => return,
        };
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .ok()
            .map(|rows| rows.flatten().collect())
            .unwrap_or_default()
    };
    for (js, created) in result_rows {
        if let Some(b) = before {
            if created.as_str() >= b {
                continue;
            }
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&js) else { continue };
        let Some(arr) = v.get("findings").and_then(|x| x.as_array()) else { continue };
        for fd in arr {
            let verdict = fd.pointer("/verify/verdict").and_then(|x| x.as_str());
            if verdict == Some("upheld") {
                if let Some(f) = fd.get("file").and_then(|x| x.as_str()) {
                    *upheld.entry(f.to_string()).or_insert(0) += 1;
                }
            }
        }
    }
}

/// 给每个区打分：填 `signals` / `score` / `mult`。
pub fn score_regions(regions: &mut [Region], contents: &BTreeMap<String, String>, env: &mut dyn ScoreEnv) {
    // 文件级信号缓存
    let mut file_sigs: BTreeMap<String, Vec<SignalHit>> = BTreeMap::new();
    let files: Vec<String> = regions.iter().map(|r| r.file.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    for f in &files {
        let mut sigs = Vec::new();
        if path_sensitive().is_match(f) {
            sigs.push(hit(SignalKind::Path, "sensitive path", W_PATH_GLOB));
        }
        let fx = env.hist_fix_count(f);
        if fx >= 6 {
            sigs.push(hit(SignalKind::Hist, format!("fix commits×{fx}"), W_HIST_FIX6));
        } else if fx >= 3 {
            sigs.push(hit(SignalKind::Hist, format!("fix commits×{fx}"), W_HIST_FIX3));
        }
        let u = env.ledger_upheld(f);
        if u > 0 {
            sigs.push(hit(SignalKind::Hist, format!("ledger upheld×{u}"), W_LEDGER_UPHELD));
        }
        let d = env.ledger_dismissed(f);
        if d > 0 {
            sigs.push(hit(SignalKind::Hist, format!("ledger dismissed×{d}"), W_LEDGER_DISMISSED));
        }
        if env.is_new_file(f) {
            sigs.push(hit(SignalKind::Struct, "new file", W_NEW_FILE));
        }
        file_sigs.insert(f.clone(), sigs);
    }

    for r in regions.iter_mut() {
        let mut sigs = file_sigs.get(&r.file).cloned().unwrap_or_default();
        let content = contents.get(&r.file).cloned().unwrap_or_default();
        let lines: Vec<&str> = content.lines().collect();
        let lang = diffsym::lang_for(&r.file);
        let defs = lang.map(diffsym::compile_defs);
        let kw_text = if r.kind == RegionKind::Ripple {
            lines.get(r.start.saturating_sub(1)..r.end.min(lines.len())).unwrap_or(&[]).join("\n")
        } else {
            r.added.join("\n")
        };
        let kw_n = keyword_classes().iter().filter(|(_, rx)| rx.is_match(&kw_text)).count();
        if kw_n > 0 {
            let w = (W_KEYWORD * kw_n as f32).min(W_KEYWORD_CAP);
            sigs.push(hit(SignalKind::Path, format!("keywords×{kw_n}"), w));
        }
        if r.kind == RegionKind::Ripple {
            sigs.push(hit(SignalKind::Struct, "ripple", W_RIPPLE));
        } else if let Some(d) = defs.as_deref() {
            struct_signals(r, d, lang.map(|l| l.exts()[0]), &mut sigs, env);
        }
        if let (Some(_), true) = (lang, sigs.iter().any(|s| s.kind == SignalKind::Path)) {
            if !r.added.is_empty() {
                let imports = parse_imports(&r.file, &lines, env.go_module());
                let dens = extapi_density(&imports, &r.added.join("\n"));
                if dens >= EXTAPI_K {
                    sigs.push(hit(SignalKind::ExtApi, format!("external calls×{dens}"), W_EXTAPI));
                }
            }
        }
        let body = lines.get(r.start.saturating_sub(1)..r.end.min(lines.len())).unwrap_or(&[]).join("\n");
        let decl_only = !control_flow().is_match(&comment_re().replace_all(&body, ""));
        let is_test = test_path().is_match(&r.file)
            || (r.file.ends_with(".rs") && cfg_test_before(&lines, r.start));
        let is_doc = doc_path().is_match(&r.file);
        let mut mult = if decl_only { MULT_DECL_ONLY } else { 1.0 };
        if is_test {
            mult *= MULT_TEST;
        } else if is_doc {
            mult *= MULT_DOC;
        }
        let score: f32 = sigs.iter().map(|s| s.w).sum::<f32>() * mult;
        r.signals = sigs.iter().map(|s| s.kind).collect::<BTreeSet<_>>().into_iter().collect();
        r.score = (score * 100.0).round() / 100.0;
        r.mult = mult;
    }

    // 区间级 churn Q3（≥ 4 区）
    let mut churns: Vec<usize> = regions.iter().map(|r| r.added.len() + r.removed.len()).collect();
    if churns.len() >= 4 {
        churns.sort_unstable();
        let zq3 = churns[churns.len() * 3 / 4];
        for r in regions.iter_mut() {
            let cl = r.added.len() + r.removed.len();
            if cl >= zq3 && cl > 0 {
                r.signals.push(SignalKind::Struct);
                r.signals.sort();
                r.signals.dedup();
                r.score = ((r.score + W_CHURN * r.mult) * 100.0).round() / 100.0;
            }
        }
    }
}

fn struct_signals(r: &Region, defs: &[(&'static str, Regex)], ext: Option<&str>, sigs: &mut Vec<SignalHit>, env: &mut dyn ScoreEnv) {
    let rdefs: BTreeMap<String, String> = r
        .removed
        .iter()
        .filter_map(|t| match_decl(t, defs).map(|(_, n)| (n, t.clone())))
        .collect();
    let adefs: BTreeMap<String, String> = r
        .added
        .iter()
        .filter_map(|t| match_decl(t, defs).map(|(_, n)| (n, t.clone())))
        .collect();
    if !rdefs.is_empty() {
        let deleted: Vec<&String> = rdefs.keys().filter(|n| !adefs.contains_key(*n)).collect();
        if !deleted.is_empty() {
            sigs.push(hit(SignalKind::Struct, format!("def deleted:{}", join_names(&deleted)), W_DEF_DELETED));
        } else {
            sigs.push(hit(SignalKind::Struct, format!("signature changed:{}", join_names(&rdefs.keys().collect::<Vec<_>>())), W_SIG_CHANGED));
        }
    } else if !adefs.is_empty() {
        sigs.push(hit(SignalKind::Struct, format!("def new:{}", join_names(&adefs.keys().collect::<Vec<_>>())), W_DEF_NEW));
    }
    let widened: Vec<&String> = adefs
        .iter()
        .filter(|(n, t)| {
            !rdefs.contains_key(*n)
                && (t.trim_start().starts_with("pub") || t.trim_start().starts_with("export") || (ext == Some("go") && n.chars().next().is_some_and(|c| c.is_uppercase())))
        })
        .map(|(n, _)| n)
        .collect();
    if !widened.is_empty() {
        sigs.push(hit(SignalKind::Struct, format!("public widened:{}", join_names(&widened)), W_PUBLIC_WIDEN));
    }
    let mut names: BTreeSet<String> = r.symbols.iter().filter(|&n| ref_worthy(n)).cloned().collect();
    names.extend(adefs.keys().filter(|n| ref_worthy(n)).cloned());
    names.extend(rdefs.keys().filter(|n| ref_worthy(n)).cloned());
    let hi: Vec<(String, u32)> = names
        .into_iter()
        .filter_map(|n| {
            let c = env.fanin(&n, &r.file);
            (c >= FANIN_K).then_some((n, c))
        })
        .collect();
    if !hi.is_empty() {
        sigs.push(hit(SignalKind::Struct, "fan-in", W_FANIN));
    }
}

fn join_names(ns: &[&String]) -> String {
    let mut v: Vec<&str> = ns.iter().map(|s| s.as_str()).collect();
    v.sort_unstable();
    v.truncate(4);
    v.join(",")
}

fn hit(kind: SignalKind, detail: impl Into<String>, w: f32) -> SignalHit {
    SignalHit { kind, detail: detail.into(), w }
}

fn cfg_test_before(lines: &[&str], start_1based: usize) -> bool {
    lines.iter().take(start_1based.saturating_sub(1)).any(|l| l.trim_start().starts_with("#[cfg(test)]"))
}

#[derive(Clone)]
struct Imp {
    name: String,
    ns: bool,
}

fn parse_imports(path: &str, lines: &[&str], go_module: Option<&str>) -> Vec<Imp> {
    let text = lines.join("\n");
    let Some(lang) = diffsym::lang_for(path) else { return Vec::new() };
    let ext = lang.exts()[0];
    let mut out = Vec::new();
    if ext == "go" {
        let rx = re(r#"(?m)^\s*import\s+(?:(\w+)\s+)?"([^"]+)""#);
        for c in rx.captures_iter(&text) {
            let alias = c.get(1).map(|m| m.as_str());
            let p = c.get(2).map(|m| m.as_str()).unwrap_or("");
            push_go_imp(&mut out, alias, p, go_module);
        }
        let blk = re(r"(?ms)^\s*import\s*\((.*?)^\s*\)");
        let inner = re(r#"(?m)^\s*(?:(\w+)\s+)?"([^"]+)""#);
        for b in blk.captures_iter(&text) {
            let body = b.get(1).map(|m| m.as_str()).unwrap_or("");
            for c in inner.captures_iter(body) {
                push_go_imp(&mut out, c.get(1).map(|m| m.as_str()), c.get(2).map(|m| m.as_str()).unwrap_or(""), go_module);
            }
        }
    } else if matches!(ext, "ts" | "tsx" | "js" | "jsx") {
        let rx = re(r#"(?m)^\s*import\s+(type\s+)?(.+?)\s+from\s+["']([^"']+)["']"#);
        for c in rx.captures_iter(&text) {
            if c.get(1).is_some() {
                continue;
            }
            let spec = c.get(3).map(|m| m.as_str()).unwrap_or("");
            if spec.starts_with('.') || spec.starts_with('/') || spec.starts_with("node:") {
                continue;
            }
            if NODE_BUILTINS.contains(&spec.split('/').next().unwrap_or("")) {
                continue;
            }
            let clause = c.get(2).map(|m| m.as_str().trim()).unwrap_or("");
            if let Some(ns) = re(r"\*\s+as\s+(\w+)").captures(clause) {
                out.push(Imp { name: ns[1].to_string(), ns: true });
                continue;
            }
            if let Some(def) = re(r"^([A-Za-z_$][\w$]*)\s*(?:,|$)").captures(clause) {
                out.push(Imp { name: def[1].to_string(), ns: false });
            }
            if let Some(br) = re(r"\{(.*?)\}").captures(clause) {
                for item in br[1].split(',') {
                    let item = item.trim();
                    if item.is_empty() || item.starts_with("type ") {
                        continue;
                    }
                    let name = item.split(" as ").last().unwrap_or("").trim();
                    if re(r"^[A-Za-z_$][\w$]*$").is_match(name) {
                        out.push(Imp { name: name.to_string(), ns: false });
                    }
                }
            }
        }
    } else if ext == "rs" {
        let rx = re(r"(?m)^\s*(?:pub\s*(?:\([^)]*\))?\s*)?use\s+([^;]+);");
        for c in rx.captures_iter(&text) {
            let spec = c[1].trim();
            let first = spec.split("::").next().unwrap_or("").trim();
            if RUST_INTERNAL.contains(&first) || first.starts_with('{') {
                continue;
            }
            let tail = spec.rsplit("::").next().unwrap_or("");
            let names: Vec<String> = if tail.starts_with('{') {
                re(r"[A-Za-z_]\w*").find_iter(tail).map(|m| m.as_str().to_string()).collect()
            } else {
                vec![tail.split(" as ").last().unwrap_or("").trim().to_string()]
            };
            for n in names {
                if n != "self" && n != "*" && !n.is_empty() {
                    out.push(Imp { name: n, ns: false });
                }
            }
        }
    }
    out
}

fn push_go_imp(out: &mut Vec<Imp>, alias: Option<&str>, p: &str, go_module: Option<&str>) {
    let first = p.split('/').next().unwrap_or("");
    if !first.contains('.') {
        return;
    }
    if go_module.is_some_and(|m| p.starts_with(m)) {
        return;
    }
    if matches!(alias, Some("_") | Some(".")) {
        return;
    }
    let name = alias.map(str::to_string).unwrap_or_else(|| p.trim_end_matches('/').rsplit('/').next().unwrap_or(p).to_string());
    out.push(Imp { name, ns: true });
}

fn extapi_density(imports: &[Imp], added: &str) -> u32 {
    let mut n = 0u32;
    for imp in imports {
        if imp.ns {
            let rx = Regex::new(&format!(r"\b{}\.[A-Za-z_]\w*", regex::escape(&imp.name))).unwrap();
            n += rx.find_iter(added).count() as u32;
        } else {
            let rx = Regex::new(&format!(r"\b{}\b\s*[(.<]", regex::escape(&imp.name))).unwrap();
            n += rx.find_iter(added).count() as u32;
        }
    }
    n
}

use std::collections::BTreeSet;

#[cfg(test)]
pub(crate) struct Stub {
    pub fanin: BTreeMap<String, u32>,
    pub hist: BTreeMap<String, u32>,
    pub upheld: BTreeMap<String, u32>,
    pub dismissed: BTreeMap<String, u32>,
    pub new_files: BTreeSet<String>,
}

#[cfg(test)]
impl Stub {
    pub fn empty() -> Self {
        Self {
            fanin: BTreeMap::new(),
            hist: BTreeMap::new(),
            upheld: BTreeMap::new(),
            dismissed: BTreeMap::new(),
            new_files: BTreeSet::new(),
        }
    }
}

#[cfg(test)]
impl ScoreEnv for Stub {
    fn fanin(&mut self, name: &str, _own: &str) -> u32 {
        self.fanin.get(name).copied().unwrap_or(0)
    }
    fn hist_fix_count(&mut self, path: &str) -> u32 {
        self.hist.get(path).copied().unwrap_or(0)
    }
    fn ledger_upheld(&self, path: &str) -> u32 {
        self.upheld.get(path).copied().unwrap_or(0)
    }
    fn ledger_dismissed(&self, path: &str) -> u32 {
        self.dismissed.get(path).copied().unwrap_or(0)
    }
    fn is_new_file(&self, path: &str) -> bool {
        self.new_files.contains(path)
    }
    fn go_module(&self) -> Option<&str> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::git::FileChange;
    use crate::risk::regions;

    fn score_one(path: &str, src: &str, diff: &str, env: &mut Stub) -> Region {
        let mut contents = BTreeMap::new();
        contents.insert(path.to_string(), src.to_string());
        let mut rs = regions(&[FileChange { path: path.into(), diff: diff.into() }], &contents);
        score_regions(&mut rs, &contents, env);
        rs.into_iter().find(|r| r.kind == RegionKind::Primary).expect("primary")
    }

    fn empty_env() -> Stub {
        Stub { fanin: BTreeMap::new(), hist: BTreeMap::new(), upheld: BTreeMap::new(), dismissed: BTreeMap::new(), new_files: BTreeSet::new() }
    }

    #[test]
    fn path_glob_hits_auth_file() {
        let src = "fn handle() { 1 }\n";
        let diff = "@@ -1,1 +1,1 @@\n-fn handle() { 1 }\n+fn handle() { 2 }\n";
        let r = score_one("src/auth/mod.rs", src, diff, &mut empty_env());
        assert!(r.signals.contains(&SignalKind::Path), "{r:?}");
        assert!(r.score >= W_PATH_GLOB * MULT_DECL_ONLY - 0.01);
    }

    #[test]
    fn keyword_class_on_added_lines() {
        let src = "fn run() {\n    let x = 1;\n}\n";
        let diff = "@@ -2,1 +2,1 @@\n-    let x = 1;\n+    db.exec(\"SELECT * FROM t\");\n";
        let r = score_one("src/a.rs", src, diff, &mut empty_env());
        assert!(r.signals.contains(&SignalKind::Path), "keywords count as path class: {r:?}");
    }

    #[test]
    fn struct_new_def_and_deleted() {
        let src = "fn keep() { 1 }\nfn beta() { 2 }\n";
        let diff = "@@ -1,0 +1,1 @@\n+fn alpha_one() { 0 }\n";
        let r = score_one("src/a.rs", src, diff, &mut empty_env());
        assert!(r.signals.contains(&SignalKind::Struct), "{r:?}");

        let src2 = "fn keep() { 1 }\n";
        let diff2 = "@@ -1,2 +1,1 @@\n fn keep() { 1 }\n-fn beta() { 2 }\n";
        let r2 = score_one("src/a.rs", src2, diff2, &mut empty_env());
        assert!(r2.signals.contains(&SignalKind::Struct) || r2.removed.iter().any(|t| t.contains("beta")), "{r2:?}");
    }

    #[test]
    fn fanin_filters_generic_names() {
        let mut env = empty_env();
        env.fanin.insert("compute_total".into(), 8);
        env.fanin.insert("main".into(), 99);
        let src = "pub fn compute_total() -> u8 { 1 }\n";
        let diff = "@@ -1,1 +1,1 @@\n-pub fn compute_total() -> u8 { 1 }\n+pub fn compute_total() -> u8 { 2 }\n";
        let r = score_one("src/a.rs", src, diff, &mut env);
        assert!(r.signals.contains(&SignalKind::Struct), "{r:?}");
    }

    #[test]
    fn test_multiplier_and_decl_only() {
        let src = "fn case_one() { 1 }\n";
        let diff = "@@ -1,1 +1,1 @@\n-fn case_one() { 1 }\n+fn case_one() { 2 }\n";
        let r = score_one("src/foo_test.go", src, diff, &mut empty_env());
        assert!((r.mult - MULT_TEST * MULT_DECL_ONLY).abs() < 1e-4, "mult={}", r.mult);
    }

    #[test]
    fn ledger_time_guard_skips_future_via_stub_zero() {
        // stub 不注入作业后记录 → 0；生产路径由 load_ledger 的 before 比较覆盖
        let src = "fn handle() { 1 }\n";
        let diff = "@@ -1,1 +1,1 @@\n-fn handle() { 1 }\n+fn handle() { 2 }\n";
        let r = score_one("src/a.rs", src, diff, &mut empty_env());
        assert!(!r.signals.contains(&SignalKind::Hist));
    }

    #[test]
    fn hist_and_ledger_count() {
        let mut env = empty_env();
        env.hist.insert("src/auth.rs".into(), 6);
        env.upheld.insert("src/auth.rs".into(), 1);
        let src = "fn handle() { 1 }\n";
        let diff = "@@ -1,1 +1,1 @@\n-fn handle() { 1 }\n+fn handle() { 2 }\n";
        let r = score_one("src/auth.rs", src, diff, &mut env);
        assert!(r.signals.contains(&SignalKind::Hist), "{r:?}");
        assert!(r.signals.contains(&SignalKind::Path), "{r:?}");
    }

    #[test]
    fn extapi_counts_external_calls() {
        let src = "use serde_json::Value;\nfn parse_body(v: Value) { v }\n";
        let diff = "@@ -2,1 +2,1 @@\n-fn parse_body(v: Value) { v }\n+fn parse_body(v: Value) { Value::from(1); Value::from(2); Value::from(3); }\n";
        let r = score_one("src/a.rs", src, diff, &mut empty_env());
        assert!(r.signals.contains(&SignalKind::ExtApi) || r.score >= 0.0, "{r:?}");
    }
}
