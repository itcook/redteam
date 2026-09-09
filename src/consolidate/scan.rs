//! 疑点并集与 agreement（M21a 批 F-3，design §5〔D7〕；prd K8）：同组内跨 cell 以 quote 为锚合并，`agreement` = 列出该疑点的
//! **家族数**（同家族多 samples 计一次；旧 `dedup::merge_into` 按模型数计，不复用）；hint 不归一（M21c R4）、全部保留；跨组不合并。
//! 谓词复用 `dedup` 的 `quote_overlap` / `QUOTE_SIM` / `LINE_GAP`（单一相似度口径）+ `orchestrate::normalize_quote`（去空白相等）。

use crate::consolidate::dedup::{quote_overlap, LINE_GAP, QUOTE_SIM};
use crate::domain::{MergedSuspicion, Suspicion};
use crate::orchestrate::{normalize_quote, ScanCellResult};

fn lines_overlap(a: &Suspicion, b: &Suspicion) -> bool {
    a.file == b.file
        && a.line_start <= b.line_end.saturating_add(LINE_GAP)
        && b.line_start <= a.line_end.saturating_add(LINE_GAP)
}

/// 「同一疑点」谓词：同文件 ∧ 行重叠（±LINE_GAP）∧（去空白 quote 相等 ∨ quote 行重叠 ≥ QUOTE_SIM）。
pub(crate) fn same_suspicion(a: &Suspicion, b: &Suspicion) -> bool {
    lines_overlap(a, b)
        && (normalize_quote(&a.quote) == normalize_quote(&b.quote) || quote_overlap(&a.quote, &b.quote) >= QUOTE_SIM)
}

/// 合并：按组分桶、桶内顺序扫描并入首个匹配簇；代表疑点 = confidence 最高者（并列取先到）；输出按
/// (group, file, line_start, line_end, hint) 排序（确定性）。失败格（`output == None`）不参与。
pub fn merge_suspicions(cells: &[ScanCellResult]) -> Vec<MergedSuspicion> {
    let mut clusters: Vec<MergedSuspicion> = Vec::new();
    for c in cells {
        let Some(out) = &c.output else { continue };
        for s in &out.suspicions {
            match clusters.iter_mut().find(|m| m.group == c.group && same_suspicion(&m.suspicion, s)) {
                Some(m) => {
                    if s.confidence > m.suspicion.confidence {
                        m.suspicion = s.clone();
                    }
                    if !m.families.contains(&c.family) {
                        m.families.push(c.family.clone());
                    }
                    if !m.cell_ids.contains(&c.cell_id) {
                        m.cell_ids.push(c.cell_id.clone());
                    }
                    if !m.hints.contains(&s.hint) {
                        m.hints.push(s.hint.clone());
                    }
                }
                None => clusters.push(MergedSuspicion {
                    group: c.group,
                    suspicion: s.clone(),
                    agreement: 0,
                    families: vec![c.family.clone()],
                    cell_ids: vec![c.cell_id.clone()],
                    hints: vec![s.hint.clone()],
                }),
            }
        }
    }
    for m in &mut clusters {
        m.families.sort();
        m.families.dedup();
        m.agreement = m.families.len() as u32;
    }
    clusters.sort_by(|a, b| {
        (a.group, &a.suspicion.file, a.suspicion.line_start, a.suspicion.line_end, &a.suspicion.hint)
            .cmp(&(b.group, &b.suspicion.file, b.suspicion.line_start, b.suspicion.line_end, &b.suspicion.hint))
    });
    clusters
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CellOutput, Category, ScanOutput};
    use crate::state::CellRecord;

    fn susp(file: &str, ls: u32, le: u32, hint: &str, conf: f32, quote: &str) -> Suspicion {
        Suspicion { file: file.into(), line_start: ls, line_end: le, hint: hint.into(), category: Some(Category::L1), confidence: conf, quote: quote.into() }
    }
    fn cell(id: &str, group: usize, family: &str, susps: Vec<Suspicion>) -> ScanCellResult {
        let out = ScanOutput { suspicions: susps, ..Default::default() };
        ScanCellResult {
            cell_id: id.into(), group, route_key: format!("p/{family}"), family: family.into(), sample: 1,
            record: CellRecord::ok(id, format!("p/{family}"), CellOutput::Scan(out.clone())).with_family(family),
            output: Some(out),
        }
    }

    /// AC3：两家族同 quote → agreement 2、hints 双收、代表取高 confidence；同家族两 samples → 1；跨组不合并。
    #[test]
    fn two_families_same_quote_agree_two_same_family_samples_one_cross_group_never() {
        let q = "let t = secret();";
        let cells = vec![
            cell("scan[g1]@p/alpha#1", 0, "alpha", vec![susp("a.rs", 3, 3, "leak", 0.6, q)]),
            cell("scan[g1]@p/beta#1", 0, "beta", vec![susp("a.rs", 4, 4, "secret in log", 0.9, "let t = secret();  ")]),
            cell("scan[g1]@p/alpha#2", 0, "alpha", vec![susp("a.rs", 3, 3, "leak", 0.6, q)]),
            cell("scan[g2]@p/alpha#1", 1, "alpha", vec![susp("a.rs", 3, 3, "leak", 0.6, q)]),
        ];
        let m = merge_suspicions(&cells);
        assert_eq!(m.len(), 2, "{m:?}");
        assert_eq!((m[0].group, m[0].agreement), (0, 2));
        assert_eq!(m[0].families, vec!["alpha", "beta"]);
        assert_eq!(m[0].cell_ids, vec!["scan[g1]@p/alpha#1", "scan[g1]@p/beta#1", "scan[g1]@p/alpha#2"]);
        assert_eq!(m[0].hints, vec!["leak", "secret in log"]);
        assert_eq!((m[0].suspicion.confidence, m[0].suspicion.line_start), (0.9, 4), "代表 = 高置信者");
        assert_eq!((m[1].group, m[1].agreement, m[1].cell_ids.len()), (1, 1, 1), "跨组不合并");
    }

    /// AC3：quote 半重叠（一方引整块、一方引一行）→ 合并；同文件不同处 / 不同文件 → 不合并；失败格不参与。
    #[test]
    fn partial_quote_overlap_merges_and_distinct_places_stay_apart() {
        let block = "let a = 1;\nlet t = secret();\nlog(t);";
        let cells = vec![
            cell("c1", 0, "alpha", vec![susp("a.rs", 2, 4, "block", 0.5, block), susp("a.rs", 40, 40, "far", 0.5, "let t = secret();")]),
            cell("c2", 0, "beta", vec![susp("a.rs", 3, 3, "line", 0.7, "let t = secret();"), susp("b.rs", 3, 3, "other file", 0.7, "let t = secret();")]),
            ScanCellResult { output: None, ..cell("c3", 0, "gamma", vec![]) },
        ];
        let m = merge_suspicions(&cells);
        let keys: Vec<(String, u32, u32)> = m.iter().map(|x| (x.suspicion.file.clone(), x.agreement, x.suspicion.line_start)).collect();
        assert_eq!(keys, vec![("a.rs".into(), 2, 3), ("a.rs".into(), 1, 40), ("b.rs".into(), 1, 3)], "{m:?}");
        assert_eq!(m[0].hints, vec!["block", "line"]);
    }

    /// 输出排序确定：与输入 cell 顺序无关（除代表并列取先到）。
    #[test]
    fn output_sorted_by_group_file_line() {
        let a = cell("c1", 1, "alpha", vec![susp("z.rs", 9, 9, "z", 0.5, "zz"), susp("a.rs", 1, 1, "a", 0.5, "aa")]);
        let b = cell("c2", 0, "alpha", vec![susp("m.rs", 5, 5, "m", 0.5, "mm")]);
        let m1 = merge_suspicions(&[a.clone(), b.clone()]);
        let m2 = merge_suspicions(&[b, a]);
        let k = |v: &Vec<MergedSuspicion>| v.iter().map(|x| (x.group, x.suspicion.file.clone())).collect::<Vec<_>>();
        assert_eq!(k(&m1), k(&m2));
        assert_eq!(k(&m1), vec![(0, "m.rs".into()), (1, "a.rs".into()), (1, "z.rs".into())]);
    }
}
