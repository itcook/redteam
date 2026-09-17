//! 疑点并集与 agreement（M21a 批 F-3，design §5〔D7〕；prd K8）：同组内跨 cell 以 quote 为锚合并，`agreement` = 列出该疑点的
//! **家族数**（同家族多 samples 计一次；旧 `dedup::merge_into` 按模型数计，不复用）；hint 不归一（M21c R4）、全部保留；跨组不合并。
//! 谓词复用 `dedup` 的 `quote_overlap` / `QUOTE_SIM` / `LINE_GAP`（单一相似度口径）+ `orchestrate::normalize_quote`（去空白相等）。
//! 合并是「同一疑点」关系的**传递闭包**（并查集）——结果与 cell 输入顺序无关（前台扇出按 plan 序、resume / partial 按 db 序喂入，
//! 两条路径产出相同；评审修 4）。

use crate::consolidate::dedup::{quote_overlap, LINE_GAP, QUOTE_SIM};
use crate::domain::{MergedSuspicion, Suspicion};
use crate::orchestrate::{normalize_quote, ScanCellResult};

/// 去空白后短于此值的 quote（`}` / `);` 一类琐碎行）不走 `quote_overlap` 行重叠路径（min-set 公式对单行 quote 恒 1.0 → ±3 行内的
/// 不同问题会被粘成一条），只认去空白相等（评审帽外项）。
pub(crate) const MIN_QUOTE_CHARS_FOR_OVERLAP: usize = 12;

fn lines_overlap(a: &Suspicion, b: &Suspicion) -> bool {
    a.file == b.file
        && a.line_start <= b.line_end.saturating_add(LINE_GAP)
        && b.line_start <= a.line_end.saturating_add(LINE_GAP)
}

/// 「同一疑点」谓词：同文件 ∧ 行重叠（±LINE_GAP）∧（去空白 quote 相等 ∨（双方 quote 都不琐碎 ∧ quote 行重叠 ≥ QUOTE_SIM））。
pub(crate) fn same_suspicion(a: &Suspicion, b: &Suspicion) -> bool {
    if !lines_overlap(a, b) {
        return false;
    }
    let (na, nb) = (normalize_quote(&a.quote), normalize_quote(&b.quote));
    if na == nb {
        return true;
    }
    na.chars().count() >= MIN_QUOTE_CHARS_FOR_OVERLAP
        && nb.chars().count() >= MIN_QUOTE_CHARS_FOR_OVERLAP
        && quote_overlap(&a.quote, &b.quote) >= QUOTE_SIM
}

fn find(parent: &mut [usize], i: usize) -> usize {
    let mut r = i;
    while parent[r] != r {
        r = parent[r];
    }
    let mut c = i;
    while parent[c] != r {
        let next = parent[c];
        parent[c] = r;
        c = next;
    }
    r
}

/// 疑点到达键：`{scan cell_id}#{i}`，i = 疑点在该格锚定后 `suspicions` 里的下标（落盘输出同序，resume 可复算）。
/// 深挖入队（`EnqueueReq.arrival`）与收场拆簇对账（[`closure_members`]）共用这一口径（C2〔D15〕）。
pub fn arrival_key(cell_id: &str, i: usize) -> String {
    format!("{cell_id}#{i}")
}

/// 组内「同一疑点」簇（[`merge_suspicions`] 与 [`closure_members`] 共用同一并查集）：按 cell id 排序后逐格展开疑点，
/// 按组分桶，桶内两两按 `same_suspicion` 连通；返回 (组, 簇成员 = (格, 该格内疑点下标))，簇按组、根序，成员按展开序。
/// 失败格（`output == None`）不参与。
fn suspicion_clusters(cells: &[ScanCellResult]) -> Vec<(usize, Vec<(&ScanCellResult, usize)>)> {
    // 输入按 cell id 排序：两条喂入路径（plan 序 / db 序）同一序；并查集本身与序无关，排序只稳住 hints / cell_ids 的排列。
    let mut ordered: Vec<&ScanCellResult> = cells.iter().filter(|c| c.output.is_some()).collect();
    ordered.sort_by(|a, b| a.cell_id.cmp(&b.cell_id));
    let mut groups: std::collections::BTreeMap<usize, Vec<(&ScanCellResult, usize)>> = Default::default();
    for c in ordered {
        if let Some(out) = &c.output {
            for i in 0..out.suspicions.len() {
                groups.entry(c.group).or_default().push((c, i));
            }
        }
    }
    let mut out = Vec::new();
    for (group, items) in groups {
        let n = items.len();
        let mut parent: Vec<usize> = (0..n).collect();
        for i in 0..n {
            for j in (i + 1)..n {
                if same_suspicion(suspicion_at(items[i]), suspicion_at(items[j])) {
                    let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                    if ri != rj {
                        parent[rj.max(ri)] = rj.min(ri);
                    }
                }
            }
        }
        let mut clusters: std::collections::BTreeMap<usize, Vec<usize>> = Default::default();
        for i in 0..n {
            let r = find(&mut parent, i);
            clusters.entry(r).or_default().push(i);
        }
        for members in clusters.into_values() {
            out.push((group, members.into_iter().map(|i| items[i]).collect()));
        }
    }
    out
}

/// 簇成员 → 疑点（簇只含成功格，`output` 恒在）。
fn suspicion_at((c, i): (&ScanCellResult, usize)) -> &Suspicion {
    &c.output.as_ref().expect("suspicion_clusters skips failed cells").suspicions[i]
}

/// 组内闭包簇的成员到达键（C2 收场拆簇对账〔D15〕）：与 [`merge_suspicions`] 同簇；队列侧逐条合并若把一簇拆进多项，
/// 调用方据此计数（`DeepMeta.queue_split_clusters`）。
pub fn closure_members(cells: &[ScanCellResult]) -> Vec<Vec<String>> {
    suspicion_clusters(cells)
        .into_iter()
        .map(|(_, members)| members.into_iter().map(|(c, i)| arrival_key(&c.cell_id, i)).collect())
        .collect()
}

/// 队列拆簇数（C2 收场对账〔D15〕）：一个组内闭包簇的成员（到达键）落进 ≥ 2 个队列项即计 1——逐格到达、队列逐条合并的非传递残差
/// （A、C 先到互不匹配，B 后到只并入先命中的一项）。成员没进任何项（入队请求构造失败 / 深挖侧早退）不计。
pub fn split_clusters(cells: &[ScanCellResult], items: &[crate::orchestrate::queue::QueueItem]) -> u32 {
    let owner: std::collections::HashMap<&str, &str> =
        items.iter().flat_map(|it| it.arrivals.iter().map(move |k| (k.as_str(), it.id.as_str()))).collect();
    closure_members(cells)
        .iter()
        .filter(|members| {
            let homes: std::collections::BTreeSet<&str> = members.iter().filter_map(|k| owner.get(k.as_str()).copied()).collect();
            homes.len() >= 2
        })
        .count() as u32
}

/// 合并：按组分桶；桶内所有 (cell, 疑点) 两两按 `same_suspicion` 并查集连通（[`suspicion_clusters`]）；每簇代表 = confidence 最高者（并列取
/// (file, line_start, line_end, hint) 最小者）；`families` 排序去重、`cell_ids` / `hints` 按 cell id 序去重；输出按
/// (group, file, line_start, line_end, hint) 排序。失败格（`output == None`）不参与。
pub fn merge_suspicions(cells: &[ScanCellResult]) -> Vec<MergedSuspicion> {
    let mut out: Vec<MergedSuspicion> = Vec::new();
    for (group, members) in suspicion_clusters(cells) {
        let items: Vec<(&ScanCellResult, &Suspicion)> = members.iter().map(|&m| (m.0, suspicion_at(m))).collect();
        let (_, best) = items
            .iter()
            .copied()
            .min_by(|&(_, sa), &(_, sb)| {
                sb.confidence
                    .partial_cmp(&sa.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| (&sa.file, sa.line_start, sa.line_end, &sa.hint).cmp(&(&sb.file, sb.line_start, sb.line_end, &sb.hint)))
            })
            .expect("non-empty cluster");
        let mut families: Vec<String> = items.iter().map(|(c, _)| c.family.clone()).collect();
        families.sort();
        families.dedup();
        let mut cell_ids: Vec<String> = Vec::new();
        let mut hints: Vec<String> = Vec::new();
        for (c, s) in &items {
            if !cell_ids.contains(&c.cell_id) {
                cell_ids.push(c.cell_id.clone());
            }
            if !hints.contains(&s.hint) {
                hints.push(s.hint.clone());
            }
        }
        out.push(MergedSuspicion { group, suspicion: best.clone(), agreement: families.len() as u32, families, cell_ids, hints });
    }
    out.sort_by(|a, b| {
        (a.group, &a.suspicion.file, a.suspicion.line_start, a.suspicion.line_end, &a.suspicion.hint)
            .cmp(&(b.group, &b.suspicion.file, b.suspicion.line_start, b.suspicion.line_end, &b.suspicion.hint))
    });
    out
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
        assert_eq!(m[0].cell_ids, vec!["scan[g1]@p/alpha#1", "scan[g1]@p/alpha#2", "scan[g1]@p/beta#1"], "cell id 序");
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

    /// 评审修 4：合并结果与输入顺序无关——链式相似（S1~S2、S2~S3、S1≁S3）任意排列都得同一簇；代表固定为高置信者。
    #[test]
    fn merge_is_order_independent_transitive_closure() {
        let q3 = "let t = secret();\nlog(token_value);\nreturn t;";
        let s1 = susp("a.rs", 10, 12, "s1", 0.6, q3);
        let s2 = susp("a.rs", 14, 14, "s2", 0.9, "log(token_value);  ");
        let s3 = susp("a.rs", 10, 10, "s3", 0.5, "let t = secret();");
        assert!(same_suspicion(&s1, &s2) && same_suspicion(&s1, &s3) && !same_suspicion(&s2, &s3));
        let mk = |order: [&Suspicion; 3]| vec![
            cell("scan[g1]@p/alpha#1", 0, "alpha", vec![order[0].clone()]),
            cell("scan[g1]@p/beta#1", 0, "beta", vec![order[1].clone()]),
            cell("scan[g1]@p/gamma#1", 0, "gamma", vec![order[2].clone()]),
        ];
        for order in [[&s1, &s2, &s3], [&s1, &s3, &s2], [&s2, &s1, &s3], [&s2, &s3, &s1], [&s3, &s1, &s2], [&s3, &s2, &s1]] {
            let m = merge_suspicions(&mk(order));
            assert_eq!(m.len(), 1, "{order:?} → {m:?}");
            assert_eq!((m[0].agreement, m[0].suspicion.hint.as_str()), (3, "s2"), "{m:?}");
            assert_eq!(m[0].families, vec!["alpha", "beta", "gamma"]);
        }
    }

    /// 评审帽外项：琐碎 quote（`}`）只认去空白相等，不走 quote_overlap（min-set 对单行恒 1.0）；相等仍合并。
    #[test]
    fn trivial_quotes_only_merge_on_exact_match() {
        let a = susp("a.rs", 5, 5, "brace", 0.5, "}");
        let b = susp("a.rs", 7, 7, "other brace", 0.5, "  }  ");
        let c = susp("a.rs", 6, 6, "close paren", 0.5, ");");
        assert!(same_suspicion(&a, &b), "去空白相等仍合并");
        assert!(!same_suspicion(&a, &c), "琐碎且不等 → 不合并");
        let long_a = susp("a.rs", 5, 5, "x", 0.5, "db.write(token); // unsafe");
        let long_b = susp("a.rs", 6, 6, "y", 0.5, "db.write(token); // unsafe\nlog(token);");
        assert!(same_suspicion(&long_a, &long_b), "非琐碎走行重叠");
    }

    /// 输出排序确定：与输入 cell 顺序无关。
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

    /// C2 拆簇对账〔D15〕：`closure_members` 与 `merge_suspicions` 同簇——链式相似 S1~S2、S1~S3、S2≁S3 同一簇三个到达键，
    /// 另一处独立疑点单独成簇；失败格不参与；到达键 = `{cell_id}#{该格内下标}`。
    #[test]
    fn closure_members_match_merge_suspicions_clusters() {
        let q3 = "let t = secret();\nlog(token_value);\nreturn t;";
        let s1 = susp("a.rs", 10, 12, "s1", 0.6, q3);
        let s2 = susp("a.rs", 14, 14, "s2", 0.9, "log(token_value);  ");
        let s3 = susp("a.rs", 10, 10, "s3", 0.5, "let t = secret();");
        let far = susp("a.rs", 90, 90, "far", 0.5, "unrelated_call(x);");
        let cells = vec![
            cell("scan[g1]@p/beta#1", 0, "beta", vec![far.clone(), s2]),
            cell("scan[g1]@p/alpha#1", 0, "alpha", vec![s1]),
            cell("scan[g1]@p/gamma#1", 0, "gamma", vec![s3]),
            ScanCellResult { output: None, ..cell("scan[g1]@p/delta#1", 0, "delta", vec![far]) },
        ];
        let merged = merge_suspicions(&cells);
        let mut members = closure_members(&cells);
        for m in &mut members {
            m.sort();
        }
        members.sort();
        assert_eq!(
            members,
            vec![
                vec!["scan[g1]@p/alpha#1#0".to_string(), "scan[g1]@p/beta#1#1".to_string(), "scan[g1]@p/gamma#1#0".to_string()],
                vec!["scan[g1]@p/beta#1#0".to_string()],
            ]
        );
        assert_eq!(merged.len(), members.len(), "簇数一致");
        let sizes: std::collections::BTreeSet<usize> = merged.iter().map(|m| m.cell_ids.len()).collect();
        assert_eq!(sizes, [1, 3].into_iter().collect(), "{merged:?}");
    }

    /// 拆簇计数（C2 收场对账〔D15〕）：链 S2~S1~S3（S2≁S3）逐格到达队列——S2、S3 先到各成一项、S1 后到只并入先命中的 S2 项 → 拆 1 簇；
    /// S1 先到则后两条都并入它 → 0。独立疑点永不计；没进队列的成员不计。
    #[test]
    fn split_clusters_counts_clusters_spread_over_queue_items() {
        use crate::orchestrate::queue::{DeepQueue, EnqueueReq};
        let q3 = "let t = secret();\nlog(token_value);\nreturn t;";
        let (s1, s2, s3) = (
            susp("a.rs", 10, 12, "s1", 0.6, q3),
            susp("a.rs", 14, 14, "s2", 0.9, "log(token_value);  "),
            susp("a.rs", 10, 10, "s3", 0.5, "let t = secret();"),
        );
        let far = susp("a.rs", 90, 90, "far", 0.5, "unrelated_call(x);");
        let cells = vec![
            cell("scan[g1]@p/alpha#1", 0, "alpha", vec![s1]),
            cell("scan[g1]@p/beta#1", 0, "beta", vec![far, s2]),
            cell("scan[g1]@p/gamma#1", 0, "gamma", vec![s3]),
        ];
        let feed = |order: &[(usize, usize)]| {
            let mut q = DeepQueue::new();
            for &(c, i) in order {
                let cell = &cells[c];
                let s = &cell.output.as_ref().unwrap().suspicions[i];
                q.enqueue(EnqueueReq::scan(s, &cell.family, arrival_key(&cell.cell_id, i)).unwrap());
            }
            q.items().to_vec()
        };
        let late_bridge = feed(&[(1, 0), (1, 1), (2, 0), (0, 0)]);
        assert_eq!(late_bridge.len(), 3, "far + S2 项 + S3 项");
        assert_eq!(split_clusters(&cells, &late_bridge), 1);
        let early_bridge = feed(&[(0, 0), (1, 1), (2, 0), (1, 0)]);
        assert_eq!(split_clusters(&cells, &early_bridge), 0);
        assert_eq!(split_clusters(&cells, &late_bridge[..1]), 0, "成员没进任何项不计");
    }
}
