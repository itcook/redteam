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
    let full = root.join(p);
    // 符号链接逃逸防护（镜像 ToolExecutor::do_read_file）：canonicalize 后必须仍在 root 真实路径内。
    let real = std::fs::canonicalize(&full).ok()?;
    let canon_root = std::fs::canonicalize(root).ok()?;
    if !real.starts_with(&canon_root) {
        return None;
    }
    let data = std::fs::read(&real).ok()?;
    let slice = if data.len() > POS_READ_MAX_BYTES { &data[..POS_READ_MAX_BYTES] } else { &data[..] };
    String::from_utf8(slice.to_vec()).ok()
}

/// 把 quote 在给定内容中确定性定位到行跨度；空 quote 或未命中返回 None。
fn locate_quote(content: &str, quote: &str) -> Option<(u32, u32)> {
    let file_lines: Vec<String> = content.lines().map(normalize_ws).collect();
    let raw_quote: Vec<String> = quote.lines().map(normalize_ws).collect();
    let first_idx = raw_quote.iter().position(|l| !l.is_empty())?;
    let last_idx = raw_quote.iter().rposition(|l| !l.is_empty())?;
    let first = &raw_quote[first_idx];
    // 先精确整行匹配（避免短/公共子串误锚到更早行），无则退子串包含（保召回）。
    let hit = file_lines
        .iter()
        .position(|fl| fl == first)
        .or_else(|| file_lines.iter().position(|fl| fl.contains(first.as_str())))?;
    let ls = (hit as u32) + 1;
    let span = (last_idx - first_idx + 1) as u32;
    let le = (ls + span - 1).min(file_lines.len() as u32).max(ls);
    Some((ls, le))
}

/// 把 quote 确定性定位到真实行；定位失败即丢弃（确定性幻觉过滤，code 线每条 finding 须指向真实代码）。
pub fn anchor(findings: Vec<Finding>, root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    for mut f in findings {
        let content = match safe_read(root, &f.file) {
            Some(c) => c,
            None => continue,
        };
        match locate_quote(&content, &f.quote) {
            Some((ls, le)) => {
                f.line_start = ls;
                f.line_end = le;
                out.push(f);
            }
            None => continue,
        }
    }
    out
}

/// doc 版锚定（空引用=遗漏声明 carve-out）：
///
/// - quote 为空/纯空白 → 遗漏类声明：原样保留（无引用可锚），透传到三层 verify 判真伪。
/// - quote 非空且在对应受审文档中命中 → 锚定到行。
/// - quote 非空但文档中找不到（编造）或 finding.file 不匹配任何受审文档 → 丢弃。
///
/// docs：受审文档的 (path, content) 对（来自 evidence::doc::collect_doc_files）。
/// 仅按已收集内容做内存内匹配，绝不按 model 控制的任意路径读盘。
pub fn anchor_doc(findings: Vec<Finding>, docs: &[(String, String)]) -> Vec<Finding> {
    let mut out = Vec::new();
    for mut f in findings {
        if f.quote.split_whitespace().next().is_none() {
            // 遗漏类：原样保留。
            out.push(f);
            continue;
        }
        // 引用类：按 finding.file 找对应受审文档内容（精确或后缀匹配）。
        let content = docs.iter().find_map(|(path, content)| {
            if path == &f.file || path.ends_with(&format!("/{}", f.file)) {
                Some(content.as_str())
            } else {
                None
            }
        });
        match content.and_then(|c| locate_quote(c, &f.quote)) {
            Some((ls, le)) => {
                f.line_start = ls;
                f.line_end = le;
                out.push(f);
            }
            None => continue, // 编造引用 / 未知文件 → 丢弃
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
            agreement: 1, hard_constraint: false, cluster_id: None, quote: quote.into(), verify: None,
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

    #[cfg(unix)]
    #[test]
    fn drops_in_root_symlink_escape() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "TOP SECRET").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), root.path().join("link.txt")).unwrap();
        // quote 即便匹配 root 外文件内容，也必须因越权被丢弃
        let out = anchor(vec![finding("link.txt", "TOP SECRET")], root.path());
        assert!(out.is_empty(), "symlink escape must drop the finding");
    }

    #[test]
    fn anchors_to_exact_line_not_earlier_substring() {
        let dir = tempfile::tempdir().unwrap();
        // 第 1 行含 "log.info" 作子串；第 3 行恰好是 "log.info"
        write(dir.path(), "a.rs", "log.info(x).bar();\nmid\nlog.info\n");
        let out = anchor(vec![finding("a.rs", "log.info")], dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 3); // 精确整行优先 → 第 3 行，而非第 1 行的子串
    }

    #[test]
    fn anchor_doc_anchors_contradiction_quote() {
        let docs = vec![("spec.md".to_string(), "line one\nthe wrong claim\nline three".to_string())];
        let out = anchor_doc(vec![finding("spec.md", "the wrong claim")], &docs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].line_start, 2);
    }

    #[test]
    fn anchor_doc_keeps_omission_empty_quote() {
        let docs = vec![("spec.md".to_string(), "whatever".to_string())];
        let out = anchor_doc(vec![finding("spec.md", "")], &docs);
        assert_eq!(out.len(), 1, "omission finding (empty quote) must be kept, not dropped");
    }

    #[test]
    fn anchor_doc_drops_fabricated_quote() {
        let docs = vec![("spec.md".to_string(), "real content only".to_string())];
        let out = anchor_doc(vec![finding("spec.md", "this text is not in the doc")], &docs);
        assert!(out.is_empty(), "fabricated quote must be dropped");
    }

    #[test]
    fn anchor_doc_drops_quote_finding_for_unknown_file() {
        let docs = vec![("spec.md".to_string(), "content".to_string())];
        let out = anchor_doc(vec![finding("other.md", "content")], &docs);
        assert!(out.is_empty(), "quote finding whose file matches no target is dropped");
    }

    #[test]
    fn anchor_doc_does_not_over_match_bare_suffix() {
        // docs path = "myspec.md", finding.file = "spec.md"
        // 纯字节后缀 "myspec.md".ends_with("spec.md") = true（过匹配）
        // 修复后：须要求前缀为 '/'，即 "myspec.md".ends_with("/spec.md") = false → 丢弃。
        let docs = vec![("myspec.md".to_string(), "some real content here".to_string())];
        let out = anchor_doc(vec![finding("spec.md", "some real content here")], &docs);
        assert!(out.is_empty(), "bare suffix over-match must be eliminated after separator-aware fix");
    }
}
