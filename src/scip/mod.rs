//! SCIP 语义索引（M15-S2-b）：跑 rust-analyzer scip → 解析成内存查询表 → 即弃。
//! 行号全 0-based，与 dossier::enclosing_def_start 对齐。
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;
use protobuf::Message;
use scip::types::Index;

/// SCIP symbol_roles 位掩码：Definition=1（其余 Import=2/WriteAccess=4/ReadAccess=8…本 MVP 只分 def/非 def）。
const ROLE_DEFINITION: i32 = 1;

/// 解析后的 SCIP 索引 + 查询表。构造一次、只读查询。
pub struct ScipIndex {
    def_at: HashMap<(String, usize), String>,          // (path, def_line0) -> symbol（位置对连接键）
    refs_of: HashMap<String, Vec<(String, usize)>>,    // symbol -> 引用点(path,line0)，去 Definition、已排序
    by_file: HashMap<String, Vec<(usize, String, bool)>>, // path -> [(line0, symbol, is_def)]，供 callee 区间查
    sig_of: HashMap<String, String>,                   // symbol -> signature_documentation.text
    local_defs: HashSet<String>,                        // 有 Definition occurrence 的 symbol 集合（本仓内 = local，判 [dep] 用，跑仓无关）
}

impl ScipIndex {
    pub fn from_index(idx: Index) -> Self {
        let mut def_at = HashMap::new();
        let mut refs_of: HashMap<String, Vec<(String, usize)>> = HashMap::new();
        let mut by_file: HashMap<String, Vec<(usize, String, bool)>> = HashMap::new();
        let mut sig_of = HashMap::new();
        let mut local_defs = HashSet::new();
        for doc in &idx.documents {
            let path = doc.relative_path.clone();
            for si in &doc.symbols {
                if let Some(d) = si.signature_documentation.as_ref() {
                    if !d.text.is_empty() {
                        sig_of.insert(si.symbol.clone(), d.text.clone());
                    }
                }
            }
            for occ in &doc.occurrences {
                let line0 = match occ.range.first() {
                    Some(&l) if l >= 0 => l as usize,
                    _ => continue,
                };
                let is_def = occ.symbol_roles & ROLE_DEFINITION != 0;
                by_file.entry(path.clone()).or_default().push((line0, occ.symbol.clone(), is_def));
                if is_def {
                    def_at.insert((path.clone(), line0), occ.symbol.clone());
                    local_defs.insert(occ.symbol.clone());
                } else {
                    refs_of.entry(occ.symbol.clone()).or_default().push((path.clone(), line0));
                }
            }
        }
        for v in refs_of.values_mut() {
            v.sort();
            v.dedup();
        }
        Self { def_at, refs_of, by_file, sig_of, local_defs }
    }

    /// 位置对：某文件某 0-based 行的 Definition symbol（改动符号 ↔ SCIP 连接键）。
    pub fn symbol_at(&self, path: &str, def_line0: usize) -> Option<&str> {
        self.def_at.get(&(path.to_string(), def_line0)).map(String::as_str)
    }

    /// caller 侧：symbol 的引用点（去 Definition），已排序去重。查无 → 空。
    pub fn callers_of(&self, symbol: &str) -> Vec<(String, usize)> {
        self.refs_of.get(symbol).cloned().unwrap_or_default()
    }

    /// callee 侧：def 区间[start0,end0]内引用到的 distinct symbol（去自身 own、去 Definition），已排序。
    pub fn callees_in(&self, path: &str, start0: usize, end0: usize, own: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        if let Some(occs) = self.by_file.get(path) {
            for (line0, sym, is_def) in occs {
                if *is_def || *line0 < start0 || *line0 > end0 || sym == own {
                    continue;
                }
                if seen.insert(sym.clone()) {
                    out.push(sym.clone());
                }
            }
        }
        out.sort();
        out
    }

    pub fn signature_of(&self, symbol: &str) -> Option<&str> {
        self.sig_of.get(symbol).map(String::as_str)
    }

    /// symbol 是否本仓内定义（有 Definition occurrence）。工作区索引里外部依赖 symbol 只以 Reference
    /// 出现、从无 Definition——故此判据与跑仓的 crate 名无关，repo-agnostic。
    pub fn is_local(&self, symbol: &str) -> bool {
        self.local_defs.contains(symbol)
    }

    /// review 前置：跑 rust-analyzer scip 对工作树建索引 → 解析 → 删临时文件。
    /// RA 缺 / 失败 / 解析错 → None（调用方降级 regex-pack，绝不阻塞）。
    pub fn build(root: &Path) -> Option<ScipIndex> {
        // 借工具链：探 rust-analyzer 在不在（D16）；探针/子进程输出静默，勿脏用户终端。
        if !Command::new("rust-analyzer")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output().ok()?.status.success() {
            return None;
        }
        let tmp = tempfile::Builder::new().prefix("redteam-scip-").suffix(".scip").tempfile().ok()?;
        let out_path = tmp.path().to_path_buf();
        let status = Command::new("rust-analyzer")
            .arg("scip").arg(root)
            .arg("--output").arg(&out_path)
            .current_dir(root)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().ok()?;
        if !status.success() {
            return None;
        }
        let bytes = std::fs::read(&out_path).ok()?;
        // tmp（NamedTempFile）drop 时自动删；解析后即弃
        let idx = Index::parse_from_bytes(&bytes).ok()?;
        Some(ScipIndex::from_index(idx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ::scip::types::{Document, Index, Occurrence, Signature, SymbolInformation};

    // 构造一个 occurrence：range=[line0,0,0]、给定 symbol/roles
    fn occ(line0: i32, symbol: &str, roles: i32) -> Occurrence {
        Occurrence { range: vec![line0, 0, 0], symbol: symbol.into(), symbol_roles: roles, ..Default::default() }
    }
    // 构造带签名的 SymbolInformation
    // 注：signature_documentation 实际类型是 Signature（非 brief 原写的 Document）——
    // 已核对 scip 0.9.0 生成源码 src/generated/scip.rs：Signature 是为线格式兼容 Document 而设的独立
    // message，字段形状相同（含同名 text: String），本 MVP 只用得到 .text，无行为差异。
    fn sym_info(symbol: &str, sig: &str) -> SymbolInformation {
        let mut si = SymbolInformation { symbol: symbol.into(), ..Default::default() };
        si.signature_documentation = protobuf::MessageField::some(Signature { text: sig.into(), ..Default::default() });
        si
    }

    fn mini_index() -> Index {
        // a.rs: 第2行(0-based=1) def `foo()`；bar 在第0行调 foo（reference）
        // b.rs: 第5行调 foo（reference）
        let a = Document {
            relative_path: "src/a.rs".into(),
            occurrences: vec![
                occ(0, "S bar().", ROLE_DEFINITION),
                occ(0, "S foo().", 0),            // bar 里调 foo = 引用
                occ(1, "S foo().", ROLE_DEFINITION),
            ],
            symbols: vec![sym_info("S foo().", "pub fn foo(x: i32) -> i32"), sym_info("S bar().", "fn bar()")],
            ..Default::default()
        };
        let b = Document {
            relative_path: "src/b.rs".into(),
            occurrences: vec![occ(5, "S foo().", 0)],
            ..Default::default()
        };
        Index { documents: vec![a, b], ..Default::default() }
    }

    #[test]
    fn queries_join_def_refs_callees_and_signatures() {
        let idx = ScipIndex::from_index(mini_index());
        // 位置对：a.rs 第1行是 foo 的 def
        assert_eq!(idx.symbol_at("src/a.rs", 1), Some("S foo()."));
        assert_eq!(idx.symbol_at("src/a.rs", 99), None);
        // caller：foo 被 a.rs:0 与 b.rs:5 引用（去 Definition、排序）
        assert_eq!(idx.callers_of("S foo()."), vec![("src/a.rs".to_string(), 0), ("src/b.rs".to_string(), 5)]);
        // callee：bar 的 def 区间[0,0]内引用了 foo（去自身 bar、去 Definition）
        assert_eq!(idx.callees_in("src/a.rs", 0, 0, "S bar()."), vec!["S foo().".to_string()]);
        // 签名
        assert_eq!(idx.signature_of("S foo()."), Some("pub fn foo(x: i32) -> i32"));
    }

    #[test]
    #[ignore] // 需本机装 rust-analyzer；手动 `cargo test -p redteam scip_build -- --ignored`
    fn scip_build_indexes_this_repo() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let idx = ScipIndex::build(root).expect("本仓应能被 rust-analyzer 索引");
        // 本仓已知符号：dossier::reference_points 应有 def 位置对
        // （精确行号随源码漂移，故只断言签名表非空 + 某已知函数签名可查）
        assert!(
            idx.signature_of("rust-analyzer cargo redteam 0.1.0 dossier/reference_points().")
                .is_some(),
            "reference_points 签名应在索引里"
        );
    }
}
