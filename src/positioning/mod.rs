use crate::domain::Finding;
use std::path::{Component, Path};

const POS_READ_MAX_BYTES: usize = 512 * 1024;

/// 空白归一化：所有空白折叠为单空格并去首尾。
fn normalize_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 受 root 约束读取：拒绝绝对路径与 `..`；二进制/超大/不存在 → None。
fn safe_read(root: &Path, rel: &str) -> Option<String> {
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir)) {
        return None;
    }
    let data = std::fs::read(root.join(p)).ok()?;
    let slice = if data.len() > POS_READ_MAX_BYTES { &data[..POS_READ_MAX_BYTES] } else { &data[..] };
    String::from_utf8(slice.to_vec()).ok()
}

/// 把 quote 确定性定位到真实行；定位失败即丢弃（确定性幻觉过滤）。
pub fn anchor(findings: Vec<Finding>, root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    for mut f in findings {
        let content = match safe_read(root, &f.file) {
            Some(c) => c,
            None => continue,
        };
        let file_lines: Vec<String> = content.lines().map(normalize_ws).collect();
        // 保留空行的原始 quote（各行归一化）：跨度按"首个→最后一个非空行"算，含中间空行、去首尾空行
        let raw_quote: Vec<String> = f.quote.lines().map(normalize_ws).collect();
        let first_idx = raw_quote.iter().position(|l| !l.is_empty());
        let last_idx = raw_quote.iter().rposition(|l| !l.is_empty());
        let (first_idx, last_idx) = match (first_idx, last_idx) {
            (Some(a), Some(b)) => (a, b),
            _ => continue, // 空 quote → 丢弃
        };
        let first = &raw_quote[first_idx];
        match file_lines.iter().position(|fl| fl.contains(first.as_str())) {
            Some(i) => {
                let ls = (i as u32) + 1;
                // 真实跨度（含中间空行）：首→尾非空行的距离 + 1
                let span = (last_idx - first_idx + 1) as u32;
                let le = (ls + span - 1).min(file_lines.len() as u32).max(ls);
                f.line_start = ls;
                f.line_end = le;
                out.push(f);
            }
            None => continue, // 编造 → 丢弃
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    use std::io::Write;

    fn finding(file: &str, quote: &str) -> Finding {
        Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: file.into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec!["L1".into()], models: vec!["m".into()],
            agreement: 1, hard_constraint: false, cluster_id: None, quote: quote.into(),
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn anchors_exact_quote_to_correct_line() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "fn main() {}\nlet token = secret();\nok();\n");
        let out = anchor(vec![finding("a.rs", "let token = secret();")], dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
        assert_eq!(out[0].line_end, 2);
    }

    #[test]
    fn anchors_despite_whitespace_differences() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "x\n    db.write(token);\n");
        // quote 多了内部空格
        let out = anchor(vec![finding("a.rs", "db.write(token);")], dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
    }

    #[test]
    fn drops_fabricated_quote() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "real line\n");
        let out = anchor(vec![finding("a.rs", "this text is not in the file")], dir.path());
        assert!(out.is_empty());
    }

    #[test]
    fn drops_on_path_traversal_or_missing() {
        let dir = tempfile::tempdir().unwrap();
        let out = anchor(
            vec![finding("../escape", "x"), finding("/etc/hosts", "x"), finding("nope.rs", "x")],
            dir.path(),
        );
        assert!(out.is_empty());
    }

    #[test]
    fn multiline_quote_sets_range() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "l1\nif x {\n    bad();\n}\n");
        let out = anchor(vec![finding("a.rs", "if x {\n    bad();\n}")], dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
        assert_eq!(out[0].line_end, 4);
    }

    #[test]
    fn multiline_quote_with_interior_blank_spans_correctly() {
        let dir = tempfile::tempdir().unwrap();
        // if 块含中间空行 → 占文件第 2..5 行
        write(dir.path(), "a.rs", "x\nif y {\n\n    bad();\n}\n");
        let out = anchor(vec![finding("a.rs", "if y {\n\n    bad();\n}")], dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
        assert_eq!(out[0].line_end, 5);
    }
}
