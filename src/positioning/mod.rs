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
/// 首匹配定位（软换行 quote 锚定的基础件；生产锚定走 `locate_quote_near`——首匹配会把短 quote 锚到
/// 同文件另一处无辜同文，见 `anchor_suspicions` 教训。保留供 near 实现复用与软换行测试锁。）
#[allow(dead_code)]
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

/// hibot 2.5：**逐字**重锚。返回 `(line_start, line_end, quote)`：
/// - quote 是检出原文的**字节切片**（首 token 到末 token），不是模型报的文本；
/// - 行号是该切片在检出中的真实行范围。
///
/// 定位器两级：先找**逐字子串**（模型的 quote 常常是行内片段，如 `danger()` 落在
/// `return danger();` 里），找不到再退回「空白无关的 token 序列匹配」（缩进/多空格/换行差异）。
/// 两条路都要求**整段 quote 的内容**匹配：首行真实、后续行虚构时 token 序列对不上 → `None`
/// （不得把幻觉换成别处的真代码）。多命中取最接近模型自报行号的那处。
///
/// 命中后 quote = 检出原文的**字节切片**，行号 = 该切片的真实行范围；最后再断言 slice 确实落在
/// 报告的行范围内（与 hibot 的 `line_start..line_end` 复核同口径）。
///
/// 与 `anchor_review_findings` 的差别：后者只改行号、保留模型 quote；这里连 quote 一起换成检出原文。
pub fn verbatim_span(root: &Path, file: &str, quote: &str, hint_line: u32) -> Option<(u32, u32, String)> {
    let content = safe_read(root, file)?;
    let trimmed = quote.trim();
    if trimmed.is_empty() {
        return None;
    }
    let hit = literal_span(&content, trimmed, hint_line).or_else(|| token_span(&content, trimmed, hint_line))?;
    let (start, end) = hit;
    if start >= end || end > content.len() {
        return None;
    }
    let (ls, le) = (line_at(&content, start), line_at(&content, end - 1));
    let quote = content[start..end].to_string();
    // 独立断言：报告的行范围内必须逐字包含这个 quote（不只依赖构造过程）。
    let lines: Vec<&str> = content.lines().collect();
    let block = lines[(ls - 1) as usize..(le as usize).min(lines.len())].join("\n");
    if !block.contains(&quote) {
        return None;
    }
    Some((ls, le, quote))
}

/// 1-based 行号（`byte` 必须是字符边界）。
fn line_at(content: &str, byte: usize) -> u32 {
    1 + content[..byte].bytes().filter(|b| *b == b'\n').count() as u32
}

/// 逐字子串：命中多处时取起点行最接近 `hint_line` 的。
fn literal_span(content: &str, quote: &str, hint_line: u32) -> Option<(usize, usize)> {
    let mut best: Option<(usize, u32)> = None;
    let mut from = 0usize;
    while let Some(pos) = content[from..].find(quote) {
        let start = from + pos;
        let distance = (i64::from(line_at(content, start)) - i64::from(hint_line)).unsigned_abs() as u32;
        match best {
            Some((_, d)) if d <= distance => {}
            _ => best = Some((start, distance)),
        }
        from = start + quote.len();
    }
    best.map(|(start, _)| (start, start + quote.len()))
}

/// 空白无关的 token 序列匹配：返回文件里那段连续 token 的字节区间。
fn token_span(content: &str, quote: &str, hint_line: u32) -> Option<(usize, usize)> {
    let want: Vec<&str> = quote.split_whitespace().collect();
    if want.is_empty() {
        return None;
    }
    // token 流：(起始字节, 结束字节, 1-based 行号)
    let mut toks: Vec<(usize, usize, u32)> = Vec::new();
    let mut line = 1u32;
    let mut start: Option<usize> = None;
    for (i, ch) in content.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                toks.push((s, i, line));
            }
            if ch == '\n' {
                line += 1;
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        toks.push((s, content.len(), line));
    }
    if want.len() > toks.len() {
        return None;
    }
    let mut best: Option<(usize, u32)> = None;
    for i in 0..=(toks.len() - want.len()) {
        let hit = want.iter().enumerate().all(|(k, w)| {
            let (s, e, _) = toks[i + k];
            &content[s..e] == *w
        });
        if !hit {
            continue;
        }
        let distance = (i64::from(toks[i].2) - i64::from(hint_line)).unsigned_abs() as u32;
        match best {
            Some((_, d)) if d <= distance => {}
            _ => best = Some((i, distance)),
        }
    }
    let (i, _) = best?;
    Some((toks[i].0, toks[i + want.len() - 1].1))
}


/// scanner 疑点锚定结果（批 D-1）：`dropped` = 被丢总数（quote 不在文件 + 文件不可读），`dropped_unreadable` 单列文件不可读
/// （不存在 / 越界 / 二进制），F 写 cell warn 时分开说，不把「读不到」误诊成「quote 幻觉」（评审 #10）。
#[derive(Clone, Debug, PartialEq)]
pub struct AnchoredScan {
    pub output: ScanOutput,
    pub dropped: u32,
    pub dropped_unreadable: u32,
    /// 跨文件重定位救回的条数（M21c 批 B-3）：quote 是真的、file 写错了。
    /// 与 `dropped` 分开记——「救回来几条」和「丢了几条」是两件事，合并就看不出这一层有没有在起作用。
    pub relocated: u32,
}

/// 疑点 quote 逐字硬约束（design §4〔D11〕：不在文件即丢弃）：定位成功改写 `line_start/line_end`，失败丢弃并计数；
/// `needs` / `scanned_areas` 原样透传。与 `anchor` 同一 `safe_read` 防线；定位用 `locate_quote_near`（同匹配优先级，多命中时取最接近
/// 模型自报行号者——scanner 的 quote 是短指针，`x.unwrap();` 类极易在同文件复现，首现会把疑点锚错地方，评审 #11）。
pub fn anchor_suspicions(root: &Path, out: ScanOutput) -> AnchoredScan {
    anchor_suspicions_within(root, out, &[])
}

/// 同上，外加**跨文件重定位**（M21c 批 B-3，design §9 L2）：quote 在它自称的文件里找不到时，
/// 到 `changed` 里再找一遍——**恰好一个文件命中才接受**（改写 file 并计数），零或多个一律拒绝。
///
/// 救的是「quote 是真的、file 写错了」这一类：scanner 一次看一组多个文件，串文件名比 reviewer 常见得多。
/// 现在这种疑点会被当幻觉整条丢掉，而它指向的问题是实的。
///
/// 「唯一命中才接受」是硬条件：两个文件都含同一段代码时无从判断是哪一处，猜一个等于把定位
/// 变回概率性的——而这一层存在的全部理由就是确定性过滤。`changed` 为空 = 不做重定位，
/// 行为与今日逐字节一致。
pub fn anchor_suspicions_within(root: &Path, mut out: ScanOutput, changed: &[String]) -> AnchoredScan {
    let mut kept = Vec::with_capacity(out.suspicions.len());
    let (mut dropped, mut dropped_unreadable) = (0u32, 0u32);
    let mut relocated = 0u32;
    for mut s in out.suspicions.drain(..) {
        let hit = safe_read(root, &s.file).and_then(|c| locate_quote_near(&c, &s.quote, s.line_start));
        if let Some((ls, le)) = hit {
            (s.line_start, s.line_end) = (ls, le);
            kept.push(s);
            continue;
        }
        // 自称的文件里没找到：可能是文件读不了，也可能是 quote 对不上——两种都试一次跨文件重定位
        match relocate_across(root, &s.quote, changed) {
            Some((file, ls, le)) => {
                s.file = file;
                (s.line_start, s.line_end) = (ls, le);
                relocated += 1;
                kept.push(s);
            }
            None => {
                dropped += 1;
                if safe_read(root, &s.file).is_none() {
                    dropped_unreadable += 1;
                }
            }
        }
    }
    out.suspicions = kept;
    AnchoredScan { output: out, dropped, dropped_unreadable, relocated }
}

/// 在 `changed` 里找 quote 的唯一归属（排除它自称的那个文件——那个已经试过了）。
/// 多命中 / 零命中 → `None`。
fn relocate_across(root: &Path, quote: &str, changed: &[String]) -> Option<(String, u32, u32)> {
    if quote.trim().is_empty() {
        return None;
    }
    // 不另判「跳过自称的那个文件」「变更集是否为空」：前者在上一步 `locate_quote_near` 已经试过且失败，
    // 再命中一次也只是得到同一个答案；后者空集的 for 循环本就不执行。两个分支都没有可观测效果，
    // 变异检查实测恒绿——加一个没人盯得住的分支不如不加。
    let mut found: Option<(String, u32, u32)> = None;
    for f in changed {
        let Some(c) = safe_read(root, f) else { continue };
        let Some((ls, le)) = locate_quote(&c, quote) else { continue };
        if found.is_some() {
            return None; // 多命中 → 无从判断是哪一处，拒绝
        }
        found = Some((f.clone(), ls, le));
    }
    found
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
        // near 版（质量评审 🟡3）：finding 自带自报行号 + 短 quote，首匹配会把幽灵 finding 锚到同文件
        // 另一处无辜同文（scanner 侧 anchor_suspicions 的既有教训）——取最接近自报行号者。
        match locate_quote_near(&content, &f.quote, f.line_start) {
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
            agreement: 1, suspicion_agreement: 0, hard_constraint: false, cluster_id: None, quote: quote.into(), refute: None,
            previously_dismissed: false, origin: Origin::New, outside_window: false,
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

    /// 跨文件重定位（M21c 批 B-3，design §9 L2）：quote 是真的、file 写错了 → 在变更集里唯一命中就救回来。
    ///
    /// 「唯一命中才接受」是硬条件：两个文件都含同一段代码时无从判断是哪一处，猜一个等于把这一层
    /// 从确定性过滤变回概率性的。
    #[test]
    fn relocates_a_misattributed_quote_only_when_exactly_one_file_matches() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/real.rs", "fn real() {\n    let token = secret();\n}\n");
        write(dir.path(), "src/claimed.rs", "fn other() {\n    nothing();\n}\n");
        let changed = vec!["src/real.rs".to_string(), "src/claimed.rs".to_string()];
        let one = |file: &str, quote: &str| ScanOutput {
            suspicions: vec![crate::domain::Suspicion {
                file: file.into(), line_start: 99, line_end: 99, hint: "h".into(),
                category: None, confidence: 0.5, quote: quote.into(),
            }],
            ..Default::default()
        };

        // ① 自称 claimed.rs，quote 实际只在 real.rs → 改写 file、计入 relocated
        let a = anchor_suspicions_within(dir.path(), one("src/claimed.rs", "let token = secret();"), &changed);
        assert_eq!((a.relocated, a.dropped), (1, 0), "{a:?}");
        assert_eq!(a.output.suspicions[0].file, "src/real.rs", "file 要被改写成真正的归属");
        assert_eq!(a.output.suspicions[0].line_start, 2);

        // ② 两个文件都含同一段 → 多命中，拒绝（照旧丢弃）
        write(dir.path(), "src/twin.rs", "fn twin() {\n    let token = secret();\n}\n");
        let twins = vec!["src/real.rs".to_string(), "src/twin.rs".to_string(), "src/claimed.rs".to_string()];
        let b = anchor_suspicions_within(dir.path(), one("src/claimed.rs", "let token = secret();"), &twins);
        assert_eq!((b.relocated, b.dropped), (0, 1), "多命中必须拒绝: {b:?}");
        assert!(b.output.suspicions.is_empty());

        // ③ 变更集里谁都没有 → 照旧丢弃（真幻觉）
        let c = anchor_suspicions_within(dir.path(), one("src/claimed.rs", "这段代码根本不存在();"), &changed);
        assert_eq!((c.relocated, c.dropped), (0, 1), "{c:?}");

        // ④ 自称的文件里本来就找得到 → 不走重定位（优先信模型自报的归属）
        let d = anchor_suspicions_within(dir.path(), one("src/real.rs", "let token = secret();"), &changed);
        assert_eq!((d.relocated, d.dropped), (0, 0));
        assert_eq!(d.output.suspicions[0].file, "src/real.rs");

        // ⑤ 变更集为空 = 不做重定位，行为与今日逐字节一致
        let e = anchor_suspicions_within(dir.path(), one("src/claimed.rs", "let token = secret();"), &[]);
        assert_eq!((e.relocated, e.dropped), (0, 1), "空变更集不该救回任何东西: {e:?}");
        assert_eq!(anchor_suspicions(dir.path(), one("src/claimed.rs", "let token = secret();")).dropped, 1,
                   "老入口的行为不变");
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

    /// 2.5：quote 由检出原文纠正——缩进 / 多空格 / 差一行都命中，返回的 quote 是文件字节切片。
    #[test]
    fn verbatim_span_corrects_whitespace_and_line_numbers_from_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let body = "fn main() {\n    let token  =   secret();\n    ok();\n}\n";
        write(dir.path(), "src/a.rs", body);
        // 模型报的 quote 行内空格被折叠、还差一行；输出必须是检出原文（双空格保留）。
        let (ls, le, q) = verbatim_span(dir.path(), "src/a.rs", "let token = secret();", 1).unwrap();
        assert_eq!((ls, le), (2, 2));
        assert_eq!(q, "let token  =   secret();", "quote 必须换成检出原文，而不是只改行号");
    }

    /// 多行：后续行缩进同样以检出为准（只改行号的实现会漏掉这一层）。
    #[test]
    fn verbatim_span_keeps_inner_indentation_of_later_lines() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/b.rs", "fn f() {\n    if (x) {\n        y();\n    }\n}\n");
        let (ls, le, q) = verbatim_span(dir.path(), "src/b.rs", "if (x) {\n  y();\n}", 2).unwrap();
        assert_eq!((ls, le), (2, 4));
        assert_eq!(q, "if (x) {\n        y();\n    }");
    }

    /// 反例：首行真实、后续行虚构——整段 token 序列对不上就是幻觉，必须失败（不得换成别处真代码）。
    #[test]
    fn verbatim_span_rejects_partially_fabricated_quotes() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/c.rs", "if (x) {\n    y();\n}\n");
        assert!(verbatim_span(dir.path(), "src/c.rs", "if (x) {\n    fabricated();\n}", 1).is_none());
    }

    /// 合法子串：模型的 quote 常常只是行内片段（`danger()` 落在 `return danger();` 里）。
    #[test]
    fn verbatim_span_accepts_inline_substrings() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/e.rs", "fn f() {\n    return danger();\n}\n");
        let (ls, le, q) = verbatim_span(dir.path(), "src/e.rs", "danger()", 1).unwrap();
        assert_eq!((ls, le, q.as_str()), (2, 2, "danger()"));
        assert!(verbatim_span(dir.path(), "src/e.rs", "danger(1)", 1).is_none(), "内容不符仍要拒绝");
    }

    /// 多命中取最接近自报行号者（短 quote 在同文件复现时不能锚到首现）。
    #[test]
    fn verbatim_span_picks_the_hit_nearest_the_hint() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "src/d.rs", "x.unwrap();\na();\nx.unwrap();\n");
        assert_eq!(verbatim_span(dir.path(), "src/d.rs", "x.unwrap();", 1).unwrap().0, 1);
        assert_eq!(verbatim_span(dir.path(), "src/d.rs", "x.unwrap();", 3).unwrap().0, 3);
    }
}

/// finding 锚点到最近改动行的距离后滤（design §10.4，M21d 批 B-2）。
///
/// 只标记、不删——`review.pack.findingWindowEnforce` 才决定要不要降级〔批 B 开工裁定 2〕。
/// 返回标记条数。`window == 0` = 关（一条都不标）。
///
/// 三种不标的情形，各有理由：
/// - **carried**：跨轮沿用的锚点本就会漂，拿它判「离改动多远」没有意义（plan 明写不误伤）；
/// - **文件不在变更集**：needs 补查会把变更集外的文件带进评审，那里根本没有「改动行」可比；
/// - **该文件的 diff 解不出改动行**（新增文件的 `/dev/null` diff 等）：没有基准就不判。
pub fn mark_outside_window(findings: &mut [Finding], bundles: &[crate::bundle::Bundle], window: u32) -> u32 {
    if window == 0 {
        return 0;
    }
    let mut changed: std::collections::BTreeMap<&str, Vec<usize>> = std::collections::BTreeMap::new();
    for b in bundles {
        for fc in &b.files {
            let lines = crate::diffsym::hunk_changed_lines(&fc.diff);
            if !lines.is_empty() {
                changed.entry(fc.path.as_str()).or_default().extend(lines);
            }
        }
    }
    let mut marked = 0;
    for f in findings.iter_mut() {
        f.outside_window = false;
        if f.origin == crate::domain::Origin::Carried {
            continue;
        }
        let Some(lines) = changed.get(f.file.as_str()) else { continue };
        // `hunk_changed_lines` 给的是 0-based 行号，finding 是 1-based
        let (lo, hi) = (f.line_start.saturating_sub(1) as usize, f.line_end.saturating_sub(1) as usize);
        let near = lines.iter().any(|&l| {
            let d = if l < lo { lo - l } else { l.saturating_sub(hi) };
            d <= window as usize
        });
        if !near {
            f.outside_window = true;
            marked += 1;
        }
    }
    marked
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use crate::domain::{Origin, Severity};

    fn f(file: &str, line: u32, origin: Origin) -> Finding {
        Finding {
            id: format!("{file}:{line}"), severity: Severity::Medium, title: "t".into(), body: "b".into(),
            file: file.into(), line_start: line, line_end: line, confidence: 0.8, recommendation: "r".into(),
            models: vec![], agreement: 1, suspicion_agreement: 0, hard_constraint: false,
            quote: String::new(), cluster_id: None, refute: None, previously_dismissed: false,
            origin, outside_window: false,
        }
    }

    fn bundle(path: &str, diff: &str) -> crate::bundle::Bundle {
        crate::bundle::Bundle::from_files(vec![crate::evidence::git::FileChange { path: path.into(), diff: diff.into() }])
    }

    /// 窗口后滤只标记不删；三种「不判」的情形各有理由（carried / 变更集外 / 窗口关）。
    #[test]
    fn marks_only_what_it_can_judge() {
        // diff 改的是第 10 行（hunk 头 1-based，`hunk_changed_lines` 给 0-based）
        let bs = [bundle("a.rs", "@@ -10,1 +10,1 @@\n+changed\n")];
        let mut fs = vec![
            f("a.rs", 10, Origin::New),  // 正中改动行
            f("a.rs", 35, Origin::New),  // 距 25 行，窗内（±30）
            f("a.rs", 60, Origin::New),  // 距 50 行，窗外
            f("a.rs", 60, Origin::Carried), // 同样远，但跨轮沿用 → 锚点本就会漂，不判
            f("other.rs", 60, Origin::New), // 不在变更集 → 没有基准，不判
        ];

        let n = mark_outside_window(&mut fs, &bs, 30);

        assert_eq!(n, 1, "{:?}", fs.iter().map(|x| (x.id.clone(), x.outside_window)).collect::<Vec<_>>());
        assert_eq!(
            fs.iter().map(|x| x.outside_window).collect::<Vec<_>>(),
            [false, false, true, false, false]
        );

        // window = 0 → 整条后滤关掉
        let mut fs2 = vec![f("a.rs", 999, Origin::New)];
        assert_eq!(mark_outside_window(&mut fs2, &bs, 0), 0);
        assert!(!fs2[0].outside_window);
    }

    /// 重复调用要幂等（收口可能跑两次：首跑与 resume 走同一个收口函数）。
    #[test]
    fn marking_is_idempotent_and_resets() {
        let bs = [bundle("a.rs", "@@ -10,1 +10,1 @@\n+changed\n")];
        let mut fs = vec![f("a.rs", 60, Origin::New)];
        assert_eq!(mark_outside_window(&mut fs, &bs, 30), 1);
        assert_eq!(mark_outside_window(&mut fs, &bs, 30), 1, "重复标记不翻倍");
        // 窗口调宽 → 标记要被**清掉**，不能留着上一轮的结论
        assert_eq!(mark_outside_window(&mut fs, &bs, 100), 0);
        assert!(!fs[0].outside_window, "放宽后要复位");
    }

}
