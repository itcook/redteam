//! 成区：hunk → enclosing def 块（无 def → hunk ±10 行 padded）；主区 / 涟漪二梯队。
//! 对照真值：`eval/risk_replay.py` `zones_for_file`（P0 §15.4 / design §3.3）。

use std::collections::{BTreeMap, BTreeSet};

use crate::diffsym::{self, ref_worthy};
use crate::evidence::git::FileChange;

/// 无 def 行的 padded 半径（行，1-based 区间两端各扩）。
const NO_DEF_PAD: usize = 10;
/// 仅 padded 区间相邻合并的间隙（行）。
const ADJ_GAP: usize = 5;
/// 涟漪引用点上限：超过视为弥散，不成卫星区（交 fan-in）。
const RIPPLE_MAX_SITES: usize = 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionKind {
    Primary,
    Ripple,
}

/// 四类信号占位（A-2 填充分数）；成区阶段只标 kind 槽。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SignalKind {
    Path,
    Struct,
    ExtApi,
    Hist,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub file: String,
    /// 1-based inclusive，与 `risk_replay.py` 区间口径一致。
    pub start: usize,
    pub end: usize,
    pub kind: RegionKind,
    pub symbols: Vec<String>,
    pub padded: bool,
    pub signals: Vec<SignalKind>,
    /// 归属本区的新增行正文（无 `+` 前缀）；涟漪区为空。
    pub added: Vec<String>,
    /// 归属本区的删除行正文（无 `-` 前缀）；涟漪区为空。
    pub removed: Vec<String>,
    pub score: f32,
    pub mult: f32,
}

struct RawZone {
    start: usize,
    end: usize,
    symbols: BTreeSet<String>,
    padded: bool,
    added: Vec<String>,
    removed: Vec<String>,
}

/// `changes` 与 `contents` 按 path 对齐：缺全文的文件不成区（head 树无此文件 / 读失败）。
pub fn regions(changes: &[FileChange], contents: &BTreeMap<String, String>) -> Vec<Region> {
    let mut out = Vec::new();
    for c in changes {
        let Some(text) = contents.get(&c.path) else { continue };
        out.extend(zones_for_file(&c.path, &c.diff, text));
    }
    out
}

fn zones_for_file(path: &str, diff: &str, content: &str) -> Vec<Region> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let lang = diffsym::lang_for(path);
    let defs = lang.map(diffsym::compile_defs);
    let hunks = diffsym::parse_hunks(diff);
    let mut raw: Vec<RawZone> = Vec::new();
    for h in &hunks {
        let s = h.start + 1; // 1-based
        let e = s + h.count.max(1) - 1;
        let mut groups: BTreeMap<usize, (usize, usize, String)> = BTreeMap::new();
        let mut nodef: Vec<usize> = Vec::new();
        for ln in s..=e {
            let i0 = ln.saturating_sub(1);
            let d = defs.as_deref().and_then(|d| enclosing_decl(&lines, i0, d));
            match d {
                None => nodef.push(ln),
                Some(di) => {
                    groups.entry(di).or_insert_with(|| {
                        let end0 = decl_block_end(&lines, di, defs.as_deref());
                        let name = defs
                            .as_deref()
                            .and_then(|d| diffsym::match_def(lines[di], d))
                            .map(|(_, n)| n)
                            .unwrap_or_default();
                        (di + 1, end0 + 1, name)
                    });
                }
            }
        }
        let mut hz: Vec<RawZone> = Vec::new();
        for (_, (a, b, name)) in groups {
            let mut symbols = BTreeSet::new();
            if !name.is_empty() {
                symbols.insert(name);
            }
            hz.push(RawZone { start: a, end: b, symbols, padded: false, added: Vec::new(), removed: Vec::new() });
        }
        let nodef: Vec<usize> = nodef
            .into_iter()
            .filter(|&ln| {
                lines.get(ln.saturating_sub(1)).is_some_and(|l| substantive(l))
            })
            .collect();
        if !nodef.is_empty() {
            let lo = nodef.iter().copied().min().unwrap().saturating_sub(NO_DEF_PAD).max(1);
            let hi = (nodef.iter().copied().max().unwrap() + NO_DEF_PAD).min(lines.len());
            hz.push(RawZone {
                start: lo,
                end: hi.max(lo),
                symbols: BTreeSet::new(),
                padded: true,
                added: Vec::new(),
                removed: Vec::new(),
            });
        }
        for (ln0, t) in &h.plus {
            let ln = ln0 + 1;
            let idx = hz.iter().position(|z| z.start <= ln && ln <= z.end).or_else(|| hz.first().map(|_| 0));
            if let Some(i) = idx {
                hz[i].added.push(t.clone());
            }
        }
        if let Some(first) = hz.first_mut() {
            first.removed.extend(h.minus.iter().cloned());
        }
        raw.extend(hz);
    }
    raw.sort_by_key(|z| (z.start, z.end));
    let merged = merge_zones(raw);
    let mut primary: Vec<Region> = merged
        .into_iter()
        .map(|z| Region {
            file: path.to_string(),
            start: z.start,
            end: z.end,
            kind: RegionKind::Primary,
            symbols: z.symbols.into_iter().collect(),
            padded: z.padded,
            signals: Vec::new(),
            added: z.added,
            removed: z.removed,
            score: 0.0,
            mult: 1.0,
        })
        .collect();

    let mut changed_names: BTreeSet<String> = BTreeSet::new();
    for r in &primary {
        for n in &r.symbols {
            if ref_worthy(n) {
                changed_names.insert(n.clone());
            }
        }
    }
    if let Some(d) = defs.as_deref() {
        for h in &hunks {
            // 新增行与删除行的声明名同入 changed_names（对照 Python `adefs`/`rdefs`）：
            // 删除的声明名（如 PR181 `-const sideTransferErrPrefix = …`）仍会从引用点产生涟漪区。
            for t in h.plus.iter().map(|(_, t)| t).chain(h.minus.iter()) {
                if let Some((_, n)) = match_decl(t, d) {
                    if ref_worthy(&n) {
                        changed_names.insert(n);
                    }
                }
            }
        }
    }
    if let (Some(d), true) = (defs.as_deref(), !changed_names.is_empty()) {
        let taken: Vec<(usize, usize)> = primary.iter().map(|r| (r.start, r.end)).collect();
        let mut sat: BTreeSet<(usize, usize)> = BTreeSet::new();
        for name in &changed_names {
            let sites: Vec<usize> = lines
                .iter()
                .enumerate()
                .filter(|(i, l)| word_hit(l, name) && !taken.iter().any(|&(a, b)| a <= i + 1 && *i < b))
                .map(|(i, _)| i)
                .collect();
            if sites.is_empty() || sites.len() > RIPPLE_MAX_SITES {
                continue;
            }
            for i in sites {
                let Some(di) = enclosing_decl(&lines, i, d) else { continue };
                let rng = (di + 1, decl_block_end(&lines, di, Some(d)) + 1);
                if taken.iter().any(|&t| overlap(rng, t)) {
                    continue;
                }
                sat.insert(rng);
            }
        }
        for (start, end) in sat {
            let sym = enclosing_decl(&lines, start.saturating_sub(1), d)
                .and_then(|di| diffsym::match_def(lines[di], d).map(|(_, n)| n))
                .into_iter()
                .collect();
            primary.push(Region {
                file: path.to_string(),
                start,
                end,
                kind: RegionKind::Ripple,
                symbols: sym,
                padded: false,
                signals: Vec::new(),
                added: Vec::new(),
                removed: Vec::new(),
                score: 0.0,
                mult: 1.0,
            });
        }
    }
    primary
}

fn merge_zones(raw: Vec<RawZone>) -> Vec<RawZone> {
    let mut merged: Vec<RawZone> = Vec::new();
    for z in raw {
        let gap = if merged.last().is_some_and(|m| m.padded && z.padded) { ADJ_GAP } else { 0 };
        let same_kind = merged.last().is_some_and(|m| m.padded == z.padded);
        if same_kind && merged.last().is_some_and(|m| z.start <= m.end + gap) {
            let m = merged.last_mut().unwrap();
            m.end = m.end.max(z.end);
            m.symbols.extend(z.symbols);
            m.padded = m.padded && z.padded;
            m.added.extend(z.added);
            m.removed.extend(z.removed);
        } else {
            merged.push(z);
        }
    }
    merged
}

fn overlap(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 <= b.1 && b.0 <= a.1
}

fn substantive(line: &str) -> bool {
    let t = line.trim_start();
    !t.is_empty() && !t.starts_with('/') && !t.starts_with('#') && !t.starts_with('*')
}

fn word_hit(line: &str, name: &str) -> bool {
    line.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '$')
        .any(|tok| tok == name)
}

/// 结构信号与 enclosing 只认真正声明：const/var 只在顶层（无缩进）计入（对照 `is_decl`）。
pub(crate) fn match_decl(line: &str, defs: &[(&'static str, regex::Regex)]) -> Option<(&'static str, String)> {
    let (kind, name) = diffsym::match_def(line, defs)?;
    if kind == "const" && line.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    Some((kind, name))
}

fn enclosing_decl(lines: &[&str], line0: usize, defs: &[(&'static str, regex::Regex)]) -> Option<usize> {
    if lines.is_empty() {
        return None;
    }
    let line0 = line0.min(lines.len().saturating_sub(1));
    (0..=line0).rev().find(|&i| {
        match_decl(lines[i], defs).is_some() && (i == line0 || diffsym::block_end(lines, i) >= line0)
    })
}

fn decl_block_end(lines: &[&str], i: usize, defs: Option<&[(&'static str, regex::Regex)]>) -> usize {
    if let Some(d) = defs {
        if let Some(("const", _)) = match_decl(lines[i], d) {
            if !lines[i].contains('{') && !lines[i].trim_end().ends_with('(') {
                return i;
            }
        }
    }
    diffsym::block_end(lines, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fc(path: &str, diff: &str) -> FileChange {
        FileChange { path: path.into(), diff: diff.into() }
    }

    fn contents(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(p, c)| ((*p).to_string(), (*c).to_string())).collect()
    }

    #[test]
    fn rust_hunk_becomes_enclosing_fn() {
        let src = "fn alpha() {\n    let x = 1;\n}\nfn beta() {\n    let y = 2;\n}\n";
        let diff = "@@ -2,1 +2,1 @@\n-    let x = 1;\n+    let x = 2;\n";
        let rs = regions(&[fc("src/a.rs", diff)], &contents(&[("src/a.rs", src)]));
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].kind, RegionKind::Primary);
        assert_eq!((rs[0].start, rs[0].end), (1, 3));
        assert_eq!(rs[0].symbols, vec!["alpha"]);
        assert!(!rs[0].padded);
    }

    #[test]
    fn adjacent_fns_stay_separate() {
        let src = "fn alpha() {\n    1\n}\nfn beta() {\n    2\n}\n";
        let diff = "@@ -2,1 +2,1 @@\n-    1\n+    10\n@@ -5,1 +5,1 @@\n-    2\n+    20\n";
        let rs = regions(&[fc("src/a.rs", diff)], &contents(&[("src/a.rs", src)]));
        assert_eq!(rs.len(), 2, "{rs:?}");
        assert_eq!(rs[0].symbols, vec!["alpha"]);
        assert_eq!(rs[1].symbols, vec!["beta"]);
    }

    #[test]
    fn no_def_pads_ten_lines() {
        let src: String = (1..=30).map(|i| format!("x{i} = {i}\n")).collect();
        let diff = "@@ -15,1 +15,1 @@\n-x15 = 15\n+x15 = 150\n";
        let rs = regions(&[fc("notes.txt", diff)], &contents(&[("notes.txt", &src)]));
        assert_eq!(rs.len(), 1);
        assert!(rs[0].padded);
        assert_eq!(rs[0].kind, RegionKind::Primary);
        assert_eq!((rs[0].start, rs[0].end), (5, 25));
    }

    #[test]
    fn local_const_is_not_a_region() {
        let src = "fn outer() {\n    const LIMIT: u8 = 1;\n    let x = LIMIT;\n}\n";
        let diff = "@@ -3,1 +3,1 @@\n-    let x = LIMIT;\n+    let x = LIMIT + 1;\n";
        let rs = regions(&[fc("src/a.rs", diff)], &contents(&[("src/a.rs", src)]));
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].symbols, vec!["outer"]);
        assert_eq!((rs[0].start, rs[0].end), (1, 4));
    }

    #[test]
    fn ripple_enclosing_def_within_four_sites() {
        let src = "pub fn compute_total() -> u8 { 1 }\nfn helper() {\n    let t = compute_total();\n}\nfn other() { 0 }\n";
        let diff = "@@ -1,1 +1,1 @@\n-pub fn compute_total() -> u8 { 1 }\n+pub fn compute_total() -> u8 { 2 }\n";
        let rs = regions(&[fc("src/a.rs", diff)], &contents(&[("src/a.rs", src)]));
        assert!(rs.iter().any(|r| r.kind == RegionKind::Primary && r.symbols == ["compute_total"]));
        let rip: Vec<_> = rs.iter().filter(|r| r.kind == RegionKind::Ripple).collect();
        assert_eq!(rip.len(), 1, "{rs:?}");
        assert_eq!(rip[0].symbols, vec!["helper"]);
    }

    #[test]
    fn go_and_ts_enclosing_defs() {
        let go = "func Alpha() int {\n\treturn 1\n}\nfunc Beta() int {\n\treturn 2\n}\n";
        let go_diff = "@@ -2,1 +2,1 @@\n-\treturn 1\n+\treturn 10\n";
        let rs = regions(&[fc("pkg/a.go", go_diff)], &contents(&[("pkg/a.go", go)]));
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].symbols, vec!["Alpha"]);

        let ts = "export function wrapBody(): number {\n  return 1;\n}\nexport function other(): number {\n  return 2;\n}\n";
        let ts_diff = "@@ -2,1 +2,1 @@\n-  return 1;\n+  return 10;\n";
        let rs = regions(&[fc("src/a.ts", ts_diff)], &contents(&[("src/a.ts", ts)]));
        assert_eq!(rs.len(), 1);
        assert_eq!(rs[0].symbols, vec!["wrapBody"]);
    }

    #[test]
    fn ripple_from_deleted_decl_name_only() {
        // A-5 修的行为（原无断言）：删除行声明名只经 `changed_names`（minus 扫描）进入涟漪计算——
        // 删除点所在区（process）不含该名，引用点（caller）应成涟漪区。对照 Python `rdefs`（PR181 真实形态）。
        let src = "fn keep() { 1 }\nconst OLD_PREFIX: &str = \"x\";\nfn process() { 1 }\nfn caller() { helper(OLD_PREFIX); helper(OLD_PREFIX); }\n";
        let diff = "@@ -2,1 +2,0 @@\n-const OLD_PREFIX: &str = \"x\";\n";
        let rs = regions(&[fc("src/a.rs", diff)], &contents(&[("src/a.rs", src)]));
        let rip: Vec<_> = rs.iter().filter(|r| r.kind == RegionKind::Ripple).collect();
        assert_eq!(rip.len(), 1, "{rs:?}");
        assert_eq!(rip[0].symbols, vec!["caller"]);
        assert_eq!((rip[0].start, rip[0].end), (4, 4));
    }

    #[test]
    fn ripple_skips_when_sites_exceed_four() {
        let mut src = String::from("pub fn compute_total() -> u8 { 1 }\n");
        for i in 0..6 {
            src.push_str(&format!("fn site_{i}() {{ compute_total(); }}\n"));
        }
        let diff = "@@ -1,1 +1,1 @@\n-pub fn compute_total() -> u8 { 1 }\n+pub fn compute_total() -> u8 { 2 }\n";
        let rs = regions(&[fc("src/a.rs", diff)], &contents(&[("src/a.rs", &src)]));
        assert!(rs.iter().all(|r| r.kind != RegionKind::Ripple), "{rs:?}");
    }
}
