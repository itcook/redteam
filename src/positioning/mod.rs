use crate::domain::{Finding, ScanOutput};
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
    let truncated = data.len() > POS_READ_MAX_BYTES;
    let slice = if truncated { &data[..POS_READ_MAX_BYTES] } else { &data[..] };
    match std::str::from_utf8(slice) {
        Ok(s) => Some(s.to_string()),
        // 截断切在多字节字符中间（错误只在末尾且确为截断）→ 退到最后一个完整字符；正文里的非法 UTF-8 仍视为二进制（评审 #22）
        Err(e) if truncated && e.error_len().is_none() => std::str::from_utf8(&slice[..e.valid_up_to()]).ok().map(str::to_string),
        Err(_) => None,
    }
}

/// 把 quote 在给定内容中确定性定位到行跨度；空 quote 或未命中返回 None。
fn locate_quote(content: &str, quote: &str) -> Option<(u32, u32)> {
    let file_lines: Vec<String> = content.lines().map(normalize_ws).collect();
    let raw_quote: Vec<String> = quote.lines().map(normalize_ws).collect();
    let first_idx = raw_quote.iter().position(|l| !l.is_empty())?;
    let last_idx = raw_quote.iter().rposition(|l| !l.is_empty())?;
    let first = &raw_quote[first_idx];
    // 先精确整行匹配（避免短/公共子串误锚到更早行），无则退子串包含（保召回）。
    let hit = match file_lines
        .iter()
        .position(|fl| fl == first)
        .or_else(|| file_lines.iter().position(|fl| fl.contains(first.as_str())))
    {
        Some(h) => h,
        // 软换行退路：markdown 散文 ~80 列断行，模型引用常把断行接成一行，
        // 首行永远配不上任何单个文件行 → 按词序列匹配（换行视同空格），
        // 不放宽内容只放宽断行。M11 dogfood 实锤：真 finding 曾被此处误杀。
        None => return locate_words(content, quote),
    };
    let ls = (hit as u32) + 1;
    let span = (last_idx - first_idx + 1) as u32;
    let le = (ls + span - 1).min(file_lines.len() as u32).max(ls);
    Some((ls, le))
}

/// 软换行退路：词级连续序列匹配（把换行视同空格）。词序列必须完全连续相等——
/// 内容不放宽，只放宽断行；折叠后仍不存在的引用照样 None（幻觉过滤不放水）。
fn locate_words(content: &str, quote: &str) -> Option<(u32, u32)> {
    let qwords: Vec<&str> = quote.split_whitespace().collect();
    if qwords.is_empty() {
        return None;
    }
    let mut cwords: Vec<(&str, u32)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        for w in line.split_whitespace() {
            cwords.push((w, (i as u32) + 1));
        }
    }
    if cwords.len() < qwords.len() {
        return None;
    }
    for start in 0..=(cwords.len() - qwords.len()) {
        if cwords[start..start + qwords.len()].iter().map(|(w, _)| *w).eq(qwords.iter().copied()) {
            return Some((cwords[start].1, cwords[start + qwords.len() - 1].1));
        }
    }
    None
}

/// M13 carry 专用定位（R4）：与 locate_quote 同匹配优先级（精确整行 > 子串 > 软换行词序），
/// 差异仅在前两层出现**多命中**时选最接近 old_line 者——carry 语义下首匹配会把 finding
/// 错锚到同文件另一处无辜同文（幽灵 finding 阻 approve）。单一命中与 locate_quote 同结果。
fn locate_quote_near(content: &str, quote: &str, old_line: u32) -> Option<(u32, u32)> {
    let file_lines: Vec<String> = content.lines().map(normalize_ws).collect();
    let raw_quote: Vec<String> = quote.lines().map(normalize_ws).collect();
    let first_idx = raw_quote.iter().position(|l| !l.is_empty())?;
    let last_idx = raw_quote.iter().rposition(|l| !l.is_empty())?;
    let first = &raw_quote[first_idx];
    let exact: Vec<usize> = file_lines.iter().enumerate()
        .filter(|(_, fl)| *fl == first).map(|(i, _)| i).collect();
    let hits: Vec<usize> = if exact.is_empty() {
        file_lines.iter().enumerate()
            .filter(|(_, fl)| fl.contains(first.as_str())).map(|(i, _)| i).collect()
    } else {
        exact
    };
    let hit = match hits.as_slice() {
        [] => return locate_words(content, quote),
        hs => *hs.iter().min_by_key(|&&i| (i as i64 + 1 - i64::from(old_line)).abs())?,
    };
    let ls = (hit as u32) + 1;
    let span = (last_idx - first_idx + 1) as u32;
    let le = (ls + span - 1).min(file_lines.len() as u32).max(ls);
    Some((ls, le))
}

/// carry 重锚定入口：把 quote 定位到当前树（root 约束读取与 anchor() 同防线）。
/// None = 定位失败（调用方收进 carried_dropped 审计，绝不静默）。
pub fn anchor_near(root: &Path, file: &str, quote: &str, old_line: u32) -> Option<(u32, u32)> {
    let content = safe_read(root, file)?;
    locate_quote_near(&content, quote, old_line)
}

/// scanner 疑点锚定结果（批 D-1）：`dropped` = 被丢总数（quote 不在文件 + 文件不可读），`dropped_unreadable` 单列文件不可读
/// （不存在 / 越界 / 二进制），F 写 cell warn 时分开说，不把「读不到」误诊成「quote 幻觉」（评审 #10）。
#[derive(Clone, Debug, PartialEq)]
pub struct AnchoredScan {
    pub output: ScanOutput,
    pub dropped: u32,
    pub dropped_unreadable: u32,
}

/// 疑点 quote 逐字硬约束（design §4〔D11〕：不在文件即丢弃）：定位成功改写 `line_start/line_end`，失败丢弃并计数；
/// `needs` / `scanned_areas` 原样透传。与 `anchor` 同一 `safe_read` 防线；定位用 `locate_quote_near`（同匹配优先级，多命中时取最接近
/// 模型自报行号者——scanner 的 quote 是短指针，`x.unwrap();` 类极易在同文件复现，首现会把疑点锚错地方，评审 #11）。
pub fn anchor_suspicions(root: &Path, mut out: ScanOutput) -> AnchoredScan {
    let mut kept = Vec::with_capacity(out.suspicions.len());
    let (mut dropped, mut dropped_unreadable) = (0u32, 0u32);
    for mut s in out.suspicions.drain(..) {
        match safe_read(root, &s.file) {
            None => {
                dropped += 1;
                dropped_unreadable += 1;
            }
            Some(content) => match locate_quote_near(&content, &s.quote, s.line_start) {
                Some((ls, le)) => {
                    s.line_start = ls;
                    s.line_end = le;
                    kept.push(s);
                }
                None => dropped += 1,
            },
        }
    }
    out.suspicions = kept;
    AnchoredScan { output: out, dropped, dropped_unreadable }
}

/// 把 quote 确定性定位到真实行；定位失败即丢弃（确定性幻觉过滤，code 线每条 finding 须指向真实代码）。
pub fn anchor(findings: Vec<Finding>, root: &Path) -> Vec<Finding> {
    anchor_review_findings(findings, root).0
}

/// reviewer finding 的 quote 逐字硬约束（M21b 批 B，design §7〔D11 同款〕）：定位成功改写行号，失败丢弃并计数
/// （旧 `anchor` 的计数版——丢弃不静默，计数回填 `ReviewParseStats.dropped_quote`）；文件不可读单列。
/// 返回 (kept, dropped_total, dropped_unreadable)。
pub fn anchor_review_findings(findings: Vec<Finding>, root: &Path) -> (Vec<Finding>, u32, u32) {
    let mut out = Vec::new();
    let (mut dropped, mut dropped_unreadable) = (0u32, 0u32);
    for mut f in findings {
        let content = match safe_read(root, &f.file) {
            Some(c) => c,
            None => {
                dropped += 1;
                dropped_unreadable += 1;
                continue;
            }
        };
        match locate_quote(&content, &f.quote) {
            Some((ls, le)) => {
                f.line_start = ls;
                f.line_end = le;
                out.push(f);
            }
            None => dropped += 1,
        }
    }
    (out, dropped, dropped_unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Origin, Severity};
    use std::io::Write;

    fn finding(file: &str, quote: &str) -> Finding {
        Finding {
            id: "x".into(), severity: Severity::High, title: "t".into(), body: "b".into(),
            file: file.into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), models: vec!["m".into()],
            agreement: 1, hard_constraint: false, cluster_id: None, quote: quote.into(), verify: None,
            previously_dismissed: false, origin: Origin::New,
        }
    }

    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    /// 批 D-1 AC2：疑点 quote 在文件 → 改写行号；不在文件 / `..` 路径 → 丢弃并计数；needs / scanned_areas 透传。
    #[test]
    fn anchor_suspicions_rewrites_lines_and_counts_dropped() {
        use crate::domain::{NeedEntry, NeedKind, ScanOutput, ScannedArea, Suspicion};
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/a.rs", "fn main() {}\n    let token = secret();\nok();\n");
        let sus = |file: &str, quote: &str| Suspicion {
            file: file.into(), line_start: 99, line_end: 99, hint: "h".into(), category: None, confidence: 0.5, quote: quote.into(),
        };
        let out = ScanOutput {
            suspicions: vec![sus("src/a.rs", "let token = secret();"), sus("src/a.rs", "fabricated()"), sus("../a.rs", "ok();")],
            needs: vec![NeedEntry { path: "src/b.rs".into(), lines: None, reason: "r".into(), kind: NeedKind::Api }],
            scanned_areas: vec![ScannedArea { file: "src/a.rs".into(), ranges: vec!["1-3".into()] }],
        };
        let a = anchor_suspicions(dir.path(), out);
        assert_eq!((a.dropped, a.dropped_unreadable), (2, 1), "quote 幻觉 1 + 路径越界 1（评审 #10 分开计）");
        assert_eq!(a.output.suspicions.len(), 1);
        assert_eq!((a.output.suspicions[0].line_start, a.output.suspicions[0].line_end), (2, 2), "空白差异仍定位");
        assert_eq!(a.output.needs.len(), 1);
        assert_eq!(a.output.scanned_areas.len(), 1);
        // 短 quote 多命中：取最接近模型自报行号的那处，而非首现（评审 #11）
        write(dir.path(), "src/b.rs", "a();\nx.unwrap();\nb();\nc();\nd();\nx.unwrap();\n");
        let near = anchor_suspicions(dir.path(), ScanOutput { suspicions: vec![sus("src/b.rs", "x.unwrap();")].into_iter().map(|mut s| { s.line_start = 6; s.line_end = 6; s }).collect(), ..Default::default() });
        assert_eq!(near.output.suspicions[0].line_start, 6);
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
    fn locate_survives_soft_wrap_joined_quote() {
        // M11 dogfood 实锤：markdown 散文 ~80 列软换行，模型引用时把断行接成一行
        // （"…function in\nsrc/store.py…" → "…function in src/store.py…"）。
        // 首行匹配失败必须走全文折叠退路，否则真 finding 被幻觉过滤器误杀。
        let content = "# Spec\n\nThis design RELIES on the existing `validate_record` function in\nsrc/store.py to reject malformed records before import.\n\ntail line\n";
        let joined = "This design RELIES on the existing `validate_record` function in src/store.py to reject malformed records before import.";
        let (ls, le) = locate_quote(content, joined).expect("soft-wrap joined quote must anchor");
        assert_eq!((ls, le), (3, 4), "应锚回原两行跨度");
    }

    #[test]
    fn locate_soft_wrap_fallback_rejects_fabrication() {
        // 退路不得放水：折叠后仍不存在的引用照样丢弃。
        let content = "alpha beta\ngamma delta\n";
        assert!(locate_quote(content, "alpha delta epsilon").is_none());
    }

    #[test]
    fn anchor_near_picks_hit_closest_to_old_line() {
        let dir = tempfile::tempdir().unwrap();
        // 同 quote 出现在第 2 行与第 20 行
        let mut body = String::from("head\nx.unwrap();\n");
        for _ in 0..17 { body.push_str("filler\n"); }
        body.push_str("x.unwrap();\ntail\n");
        write(dir.path(), "a.rs", &body);
        // 上轮锚在 18 行附近 → 就近选 20 行，而非首个命中的 2 行（R4 幽灵 finding 防护）
        assert_eq!(anchor_near(dir.path(), "a.rs", "x.unwrap();", 18), Some((20, 20)));
        // 上轮锚在 3 行附近 → 选 2 行
        assert_eq!(anchor_near(dir.path(), "a.rs", "x.unwrap();", 3), Some((2, 2)));
    }

    #[test]
    fn anchor_near_single_hit_ignores_old_line_and_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "one\nlet t = secret();\nthree\n");
        // 单一命中：old_line 再离谱也锚到唯一处（零影响承诺）
        assert_eq!(anchor_near(dir.path(), "a.rs", "let t = secret();", 999), Some((2, 2)));
        // quote 消失 → None（调用方收进 carried_dropped）
        assert_eq!(anchor_near(dir.path(), "a.rs", "vanished();", 2), None);
        // 文件消失 → None
        assert_eq!(anchor_near(dir.path(), "gone.rs", "let t = secret();", 2), None);
    }

    #[test]
    fn anchor_near_multiline_quote_spans_from_nearest_first_line() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "if x {\n    bad();\n}\nmid\nif x {\n    bad();\n}\n");
        // 两处同型块（首行 1 与 5），old_line=6 → 就近选 5，跨度 3 行
        assert_eq!(anchor_near(dir.path(), "a.rs", "if x {\n    bad();\n}", 6), Some((5, 7)));
    }
}
