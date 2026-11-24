//! 2.5 quote 逐字校验：**重锚 → 用检出原文修正 quote 与行号 → 生成指纹 → 匹配 dismissed**。
//!
//! 顺序不能换：指纹的身份字段是 `file + normalize(quote) + normalize(title)`，先修正 quote 再算指纹，
//! 同一 head 的重跑才稳定（hibot 会复算）。`dismissed` 匹配发生在指纹之后——那是下一步（2.4）。
//!
//! 定位用 [`crate::positioning::verbatim_span`]：整段 quote 的 token 序列必须逐字命中；
//! 命中后 **quote 换成检出原文的字节切片**（缩进/多空格/换行差异都被纠正），行号取该切片的真实行范围。
//! 首行真实、后续行虚构 → token 序列对不上 → 丢弃。模糊定位只用于找候选，绝不把幻觉换成别处真代码。

use crate::domain::{CellOutput, Finding};
use crate::state::CellRecord;
use std::path::Path;

#[derive(Clone, Debug, Default)]
pub struct Verified {
    pub findings: Vec<Finding>,
    /// 本轮因 quote 无法逐字命中被丢弃的条数（含上游锚定阶段已丢弃的，见 [`upstream_dropped`]）。
    pub dropped: u32,
}

/// 逐条重锚 + 用检出原文修正 quote/行号；失败即丢弃（计数不静默）。
pub fn verify(findings: Vec<Finding>, root: &Path) -> Verified {
    let mut kept = Vec::with_capacity(findings.len());
    let mut dropped = 0u32;
    for mut f in findings {
        match crate::positioning::verbatim_span(root, &f.file, &f.quote, f.line_start) {
            Some((ls, le, quote)) => {
                f.line_start = ls;
                f.line_end = le;
                f.quote = quote;
                kept.push(f);
            }
            None => dropped += 1,
        }
    }
    Verified { findings: kept, dropped }
}

/// reviewer 格在**解析阶段**就丢掉的 finding 数（`anchor_review_findings` 的计数，落在 cell 记录上）。
///
/// 这些条目从来进不了 `findings[]`，只在报告出口统计本轮的丢弃会发现「空结果 + ok」——
/// 与「模型零发现」分不开。所以要一并计入 `dropped_unverified_quotes` 并触发 `partial`。
pub fn upstream_dropped(cells: &[CellRecord]) -> u32 {
    cells
        .iter()
        .filter(|c| matches!(c.output, CellOutput::Review { .. }))
        .map(|c| c.dropped_quote)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Origin, Severity};
    use std::io::Write;

    fn finding(file: &str, quote: &str, line: u32) -> Finding {
        Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: file.into(), line_start: line, line_end: line, confidence: 0.9,
            recommendation: "r".into(), models: vec!["m".into()],
            agreement: 1, suspicion_agreement: 0, hard_constraint: false, cluster_id: None,
            quote: quote.into(), refute: None, previously_dismissed: false,
            origin: Origin::New, outside_window: false,
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn rewrites_quote_and_lines_from_the_checkout() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "src/a.rs", "fn main() {\n    let token  =   secret();\n}\n");
        let v = verify(vec![finding("src/a.rs", "let token = secret();", 1)], d.path());
        assert_eq!(v.dropped, 0);
        let f = &v.findings[0];
        assert_eq!((f.line_start, f.line_end), (2, 2), "差一行要纠正");
        assert_eq!(f.quote, "let token  =   secret();", "quote 用检出原文（多空格保留）");
    }

    /// 2.5 出口验收的反例：**只改行号不改空白必须判失败**。
    /// 这里的判据是「报告的 quote 必须等于检出切片」；只改行号的实现会留下模型文本，于是 `assert_eq` 失败。
    #[test]
    fn line_number_only_fix_would_be_rejected() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "src/a.rs", "fn main() {\n    let token  =   secret();\n}\n");
        let model_quote = "let token = secret();";
        let v = verify(vec![finding("src/a.rs", model_quote, 2)], d.path());
        let f = &v.findings[0];
        assert_ne!(f.quote, model_quote, "只改行号会留下模型文本——本实现必须换掉 quote");
        assert_eq!(f.quote, "let token  =   secret();");
    }

    #[test]
    fn hallucinated_quote_is_dropped_and_counted() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "src/a.rs", "fn main() {}\nok();\n");
        let v = verify(vec![finding("src/a.rs", "fabricated()", 1), finding("src/a.rs", "ok();", 2)], d.path());
        assert_eq!(v.dropped, 1);
        assert_eq!(v.findings.len(), 1);
        assert_eq!(v.findings[0].quote, "ok();");
    }

    /// 首行真实、后续行虚构：整段 token 序列对不上 → 丢弃（不得只认首行就把别处的真代码塞回去）。
    #[test]
    fn partially_fabricated_quote_is_dropped() {
        let d = tempfile::tempdir().unwrap();
        write(d.path(), "src/a.rs", "if (x) {\n    y();\n}\n");
        let v = verify(vec![finding("src/a.rs", "if (x) {\n    fabricated();\n}", 1)], d.path());
        assert_eq!(v.dropped, 1);
        assert!(v.findings.is_empty());
    }

    #[test]
    fn upstream_review_cell_drops_are_counted_once() {
        let mut rec = CellRecord::ok("review[x]", "p/r", CellOutput::Review { findings: vec![], needs: vec![] });
        rec.dropped_quote = 3;
        let scan = CellRecord::ok("scan[x]", "p/s", CellOutput::Scan(Default::default()));
        assert_eq!(upstream_dropped(&[rec, scan]), 3, "只数 reviewer 格");
    }
}
