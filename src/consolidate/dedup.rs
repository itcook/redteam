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
/// pub：判例机械预筛（verify::matches_precedent）复用同一相似度口径。
pub fn quote_overlap(a: &str, b: &str) -> f32 {
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

/// dedup 与 merge_carry 共用的「同一 bug」谓词（D4：单一真相源，不另立第二套阈值）。
pub(crate) fn is_same_bug(a: &Finding, b: &Finding) -> bool {
    overlaps(a, b)
        && (title_jaccard(&a.title, &b.title) >= TITLE_SIM
            || quote_overlap(&a.quote, &b.quote) >= QUOTE_SIM)
}

fn merge_into(base: &mut Finding, other: &Finding) {
    if other.severity > base.severity { base.severity = other.severity; }
    if other.confidence > base.confidence { base.confidence = other.confidence; }
    for m in &other.models { if !base.models.contains(m) { base.models.push(m.clone()); } }
    base.agreement = base.models.len() as u32;
    // 疑点共识是**区域**属性，两条 finding 合并即同一区域：取较大的那份（落地时刻值，
    // 后涨不回溯〔D13〕；批 D 跳过复核读它）。与上面按模型数算的 agreement 是两个轴。
    base.suspicion_agreement = base.suspicion_agreement.max(other.suspicion_agreement);
    // rec 跟随 body 同源（body 恒保留 base 的）——「更长 rec 胜」会让合并后 body 讲 A 问题、
    // rec 讲 B 方案的串台（dogfood 20260702 r1 3cf04bba）。唯一例外：base rec 为空时补位
    // （空 rec 比借来的 rec 更糟；dogfood 20260703 r1 c3fe15d9），只填真空、绝不覆盖。
    if base.recommendation.trim().is_empty() && !other.recommendation.trim().is_empty() {
        base.recommendation = other.recommendation.clone();
    }
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
            if is_same_bug(c, &f) {
                merge_into(c, &f);
                merged = true;
                break;
            }
        }
        if !merged { clusters.push(f); }
    }
    clusters
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Origin, Severity};
    fn f(id:&str, file:&str, ls:u32, le:u32, title:&str, model:&str, sev:Severity) -> Finding {
        Finding { id:id.into(), severity:sev, title:title.into(), body:"b".into(), file:file.into(),
            line_start:ls, line_end:le, confidence:0.8, recommendation:"r".into(),
            models:vec![model.into()], agreement:1, suspicion_agreement: 0,
            hard_constraint:false, quote:String::new(), cluster_id:None, verify:None,
            previously_dismissed: false, origin: Origin::New }
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
    fn merged_recommendation_stays_with_base_body() {
        // 合并保留 base 的 body，rec 必须同源——即便 other 的 rec 更长（防串台回归）
        let mut a = f("1","a.rs",10,12,"missing auth check","claude",Severity::High);
        a.recommendation = "add guard".into();
        let mut b = f("2","a.rs",11,13,"Missing auth check","gpt",Severity::High);
        b.recommendation = "a much longer but unrelated recommendation about symlinks".into();
        let out = dedup(vec![a, b]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].recommendation, "add guard", "rec 与保留的 body 同源，不取更长者");
    }

    #[test]
    fn merged_recommendation_vacuum_fill_when_base_empty() {
        // base rec 为空 → 用 other 的补位（防静默丢失），但只填真空
        let mut a = f("1","a.rs",10,12,"missing auth check","claude",Severity::High);
        a.recommendation = "  ".into();
        let mut b = f("2","a.rs",11,13,"Missing auth check","gpt",Severity::High);
        b.recommendation = "add guard at entry".into();
        let out = dedup(vec![a, b]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].recommendation, "add guard at entry", "空 rec 由合并方补位");
    }

    /// 两个 agreement 是两根轴，合并时各走各的算法：
    /// `agreement` = 认同这条 finding 的模型数（models.len()）；
    /// `suspicion_agreement` = 区域被几个 scanner 家族标记（区域属性，合并取 max）。
    /// 批 D 的跳过规则（design §8.1，疑点 ≥ 2）与孤证复核（verify.rs，模型 == 1）因此可以同时成立：
    /// 一条单模型报出、但落在双家族疑点区的 finding——正是「三方一致 → 免复核」要认的那种。
    #[test]
    fn two_agreement_axes_merge_independently() {
        let mut a = f("1", "a.rs", 10, 12, "missing auth check", "claude", Severity::High);
        a.suspicion_agreement = 3;
        let mut b = f("2", "a.rs", 11, 13, "Missing auth check", "gpt", Severity::High);
        b.suspicion_agreement = 1;
        let out = dedup(vec![a, b]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].agreement, 2, "两个模型认同 → 模型轴 2");
        assert_eq!(out[0].suspicion_agreement, 3, "区域轴取 max，不随模型数变");

        // 单模型 finding 落在双家族疑点区：模型轴 1（孤证）∧ 区域轴 2（跳过判据成立）。
        let mut lone = f("3", "b.rs", 5, 5, "unchecked write", "claude", Severity::High);
        lone.suspicion_agreement = 2;
        let out = dedup(vec![lone]);
        assert_eq!((out[0].agreement, out[0].suspicion_agreement), (1, 2));
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
    fn is_same_bug_false_when_lines_dont_overlap() {
        // 行不重叠 → 即便标题完全相同也不是同一 bug（D4：merge_carry 复用同一谓词）
        let a = f("1","a.rs",10,12,"missing auth check","claude",Severity::High);
        let b = f("2","a.rs",100,102,"missing auth check","gpt",Severity::High);
        assert!(!is_same_bug(&a, &b));
    }

    #[test]
    fn is_same_bug_false_when_title_and_quote_both_low_similarity() {
        // under-merge 守卫：行重叠但 title Jaccard 与 quote overlap 双低 → false
        let mut a = f("1","a.rs",10,12,"missing auth check","claude",Severity::High);
        a.quote = "entries.iter().take(entries.len() - keep)".into();
        let mut b = f("2","a.rs",10,12,"unvalidated write to db","gpt",Severity::High);
        b.quote = "let _ = std::fs::remove_file(path);".into();
        assert!(!is_same_bug(&a, &b));
    }

    #[test]
    fn is_same_bug_true_for_synonym_titles_same_quote() {
        // 标题措辞不同但语义相同（title Jaccard 低于阈值），quote 引同一行 → is_same_bug 应为 true
        let mut a = f("1","a.rs",10,12,"unwrap() may panic","claude",Severity::High);
        a.quote = "let val = opt.unwrap();".into();
        let mut b = f("2","a.rs",10,12,"potential panic from unwrap","gpt",Severity::High);
        b.quote = "let val = opt.unwrap();".into();
        assert!(is_same_bug(&a, &b), "同义不全等标题 + 同 quote → is_same_bug 应为 true");
    }
}
