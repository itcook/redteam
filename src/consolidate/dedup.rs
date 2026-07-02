use crate::domain::Finding;
use std::collections::BTreeSet;

pub const LINE_GAP: u32 = 3;
pub const TITLE_SIM: f32 = 0.5;
/// quote 行重叠系数阈值：两条 finding 引用的代码行重叠 ≥ 此比例 → 视为同一处问题。
pub const QUOTE_SIM: f32 = 0.5;

pub fn title_jaccard(a: &str, b: &str) -> f32 {
    let ta: BTreeSet<String> = a.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
    let tb: BTreeSet<String> = b.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
    if ta.is_empty() && tb.is_empty() { return 1.0; }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    if union == 0.0 { 0.0 } else { inter / union }
}

/// quote 行重叠系数：两条 quote 归一化成非空行集合后，交集 / **较小**集合（重叠系数，非
/// Jaccard）——对「一方引 1 行、一方引整块」的不等长引用鲁棒。两个模型对同一 bug 即便标题
/// 措辞不同，引用的代码行仍高度重叠，作为 title_jaccard 之外的合并补救信号。
/// 任一 quote 为空（如 doc 遗漏类）→ 返回 0，不作 quote 信号。
fn quote_overlap(a: &str, b: &str) -> f32 {
    let norm = |q: &str| -> BTreeSet<String> {
        q.lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|l| !l.is_empty())
            .collect()
    };
    let (sa, sb) = (norm(a), norm(b));
    let min = sa.len().min(sb.len());
    if min == 0 { return 0.0; }
    sa.intersection(&sb).count() as f32 / min as f32
}

fn overlaps(a: &Finding, b: &Finding) -> bool {
    a.file == b.file
        && a.line_start <= b.line_end.saturating_add(LINE_GAP)
        && b.line_start <= a.line_end.saturating_add(LINE_GAP)
}

fn merge_into(base: &mut Finding, other: &Finding) {
    if other.severity > base.severity { base.severity = other.severity; }
    if other.confidence > base.confidence { base.confidence = other.confidence; }
    for l in &other.lenses { if !base.lenses.contains(l) { base.lenses.push(l.clone()); } }
    for m in &other.models { if !base.models.contains(m) { base.models.push(m.clone()); } }
    base.agreement = base.models.len() as u32;
    if other.recommendation.len() > base.recommendation.len() { base.recommendation = other.recommendation.clone(); }
    base.hard_constraint = base.hard_constraint || other.hard_constraint;
}

/// under-merge：仅当 file 同 + 行重叠 + 标题相似 三者皆成立才合并。
pub fn dedup(findings: Vec<Finding>) -> Vec<Finding> {
    let mut clusters: Vec<Finding> = Vec::new();
    for f in findings {
        let mut merged = false;
        for c in clusters.iter_mut() {
            // 同 file + 行重叠（positioning 锚定的真实行号）+（标题相似 OR quote 行重叠）。
            // quote 信号补救 title_jaccard 的同义表述盲点：不同模型对同一 bug 措辞不同（标题
            // Jaccard 低）但引用同段代码 → 仍合并 + agreement 加成（dogfood#4）。
            if overlaps(c, &f)
                && (title_jaccard(&c.title, &f.title) >= TITLE_SIM
                    || quote_overlap(&c.quote, &f.quote) >= QUOTE_SIM)
            {
                merge_into(c, &f);
                merged = true;
                break;
            }
        }
        if !merged { clusters.push(f); }
    }
    clusters
}

/// doc 版去重（退化到非行号相似度）：同 file + 标题相似即并，不要求行重叠。
/// 遗漏类 finding 无有意义行号，故 doc 线按主题（标题）聚类。
pub fn dedup_doc(findings: Vec<Finding>) -> Vec<Finding> {
    let mut clusters: Vec<Finding> = Vec::new();
    for f in findings {
        let mut merged = false;
        for c in clusters.iter_mut() {
            if c.file == f.file && title_jaccard(&c.title, &f.title) >= TITLE_SIM {
                merge_into(c, &f);
                merged = true;
                break;
            }
        }
        if !merged {
            clusters.push(f);
        }
    }
    clusters
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    fn f(id:&str, file:&str, ls:u32, le:u32, title:&str, model:&str, sev:Severity) -> Finding {
        Finding { id:id.into(), severity:sev, title:title.into(), body:"b".into(), file:file.into(),
            line_start:ls, line_end:le, confidence:0.8, recommendation:"r".into(),
            lenses:vec!["L1".into()], models:vec![model.into()], agreement:1,
            hard_constraint:false, quote:String::new(), cluster_id:None, verify:None,
            previously_dismissed: false }
    }
    #[test]
    fn merges_overlapping_same_title_cross_model() {
        let v = vec![
            f("1","a.rs",10,12,"missing auth check","claude",Severity::High),
            f("2","a.rs",11,13,"Missing auth check","gpt",Severity::Critical),
        ];
        let out = dedup(v);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, Severity::Critical);     // 取最高
        assert_eq!(out[0].agreement, 2);                      // 跨模型一致
        assert!(out[0].models.contains(&"gpt".to_string()) && out[0].models.contains(&"claude".to_string()));
    }
    #[test]
    fn under_merge_keeps_distinct_when_titles_differ() {
        let v = vec![
            f("1","a.rs",10,12,"missing auth check","claude",Severity::High),
            f("2","a.rs",10,12,"unvalidated write to db","claude",Severity::High),
        ];
        assert_eq!(dedup(v).len(), 2); // 行重叠但标题不相似 → 不合并
    }
    #[test]
    fn different_files_never_merge() {
        let v = vec![
            f("1","a.rs",10,12,"same title","claude",Severity::High),
            f("2","b.rs",10,12,"same title","claude",Severity::High),
        ];
        assert_eq!(dedup(v).len(), 2);
    }

    #[test]
    fn merges_cross_model_via_quote_when_titles_differ() {
        // 同段代码、措辞不同的标题，但 quote 引同一行 → 靠 quote 信号合并、agreement=2（dogfood#4）
        let q = "for (mtime, path) in entries.iter().take(entries.len() - keep) {";
        let mut a = f("1","s.rs",60,62,"deletes ALL file types in jobs directory","deepseek",Severity::High);
        a.quote = q.into();
        let mut b = f("2","s.rs",61,61,"removes every file type from jobs_dir","kimi",Severity::Critical);
        b.quote = q.into();
        let out = dedup(vec![a, b]);
        assert_eq!(out.len(), 1, "quote 行重叠应合并即便标题不相似");
        assert_eq!(out[0].agreement, 2);
        assert_eq!(out[0].severity, Severity::Critical); // 取最高
    }

    #[test]
    fn quote_distinct_keeps_under_merge() {
        // 同 file 行重叠、标题不同、quote 也引不同代码行 → 仍不合并（不 over-merge 同行不同问题）
        let mut a = f("1","s.rs",60,62,"usize underflow panic","deepseek",Severity::Critical);
        a.quote = "entries.iter().take(entries.len() - keep)".into();
        let mut b = f("2","s.rs",60,62,"silent removal error swallowed","kimi",Severity::Low);
        b.quote = "let _ = std::fs::remove_file(path);".into();
        assert_eq!(dedup(vec![a, b]).len(), 2, "quote 引不同行 + 标题不同 → 不合并");
    }

    #[test]
    fn dedup_doc_merges_same_title_cross_model_ignoring_lines() {
        let v = vec![
            f("1","spec.md",0,0,"missing error branch","claude",Severity::High),
            f("2","spec.md",99,99,"Missing error branch","gpt",Severity::High),
        ];
        let out = dedup_doc(v);
        assert_eq!(out.len(), 1, "same title merges regardless of line distance");
        assert_eq!(out[0].agreement, 2);
    }

    #[test]
    fn dedup_doc_keeps_distinct_titles() {
        let v = vec![
            f("1","spec.md",0,0,"missing error branch","claude",Severity::High),
            f("2","spec.md",0,0,"ambiguous retry policy","claude",Severity::High),
        ];
        assert_eq!(dedup_doc(v).len(), 2);
    }

    #[test]
    fn dedup_doc_different_files_never_merge() {
        let v = vec![
            f("1","a.md",0,0,"same title","claude",Severity::High),
            f("2","b.md",0,0,"same title","claude",Severity::High),
        ];
        assert_eq!(dedup_doc(v).len(), 2);
    }
}
