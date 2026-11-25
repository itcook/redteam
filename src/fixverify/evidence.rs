//! 逐条核验的证据装配（4.2 / 4.3）：**只喂这一条 finding 相关的材料**，不喂整 PR。
//!
//! 三块证据，全部逐字取自 fix diff 或 post-fix 检出：
//! - `hunks`：该文件在 fix diff 里的段（改名时按新路径重定位，删除时保留被删行）；
//! - `related`：跨文件修复时，**其它文件里携带同一段代码的 hunks**（封顶 2 个文件）；
//! - `snapshot`：post-fix 检出中 finding 位置 ±[`SNAPSHOT_RADIUS`] 行的原文。
//!
//! quote 的逐字校验池 = 上述三块**模型实际看到的文本**（中和之后，见 `fixverify::prompt`）。

use crate::config::OutputLang;
use crate::fixverify::input::{locate, FileFix, FixChange, InputFinding};
use std::path::Path;

/// 现场快照半径（行）：与 refute 同量级，够看住构造与调用点，又不把整文件塞进 prompt。
pub const SNAPSHOT_RADIUS: u32 = 60;
/// 单块证据的字节帽（超帽按字符边界截断并显式注记，不静默丢）。
const HUNKS_BYTE_CAP: usize = 16_000;
const SNAPSHOT_BYTE_CAP: usize = 24_000;
const RELATED_BYTE_CAP: usize = 8_000;
const RELATED_FILES_MAX: usize = 2;
/// 跨文件线索的最小长度：太短的 quote 行（`}` / `return`）会在无关文件里到处命中。
const CLUE_MIN_CHARS: usize = 12;
/// B-lite：其它文件 hunk 引用「本文件被改符号名」时的快照半径（行）。
const BLAST_RADIUS: u32 = 40;
/// B-lite 最多看几个其它文件 / 每文件多少字节 / 每 hunk 列几条符号引用。
const BLAST_FILES_MAX: usize = 2;
const BLAST_BYTE_CAP: usize = 8_000;
const BLAST_REFS_PER_HUNK: usize = 3;

/// 一条 finding 的全部证据。
#[derive(Clone, Debug)]
pub struct Evidence {
    /// 是否定位到了这一条（false ⇒ 该条 `inconclusive`）。
    pub located: bool,
    pub path: String,
    pub kind: FixChange,
    pub renamed_from: Option<String>,
    pub hunks: String,
    pub related: String,
    /// B-lite：其它文件 hunk 里引用「本文件被改符号名」的段（符号名 + 引用处 file:line + ±N 快照）。
    pub blast: String,
    pub snapshot: String,
    /// 无法定位的原因（人读，随 `language`）。
    pub note: Option<String>,
    /// 证据降级原因（机器可读，进 `extensions.redteam.verify.warnings`）；不猜。
    pub warnings: Vec<String>,
}

fn zh(lang: &OutputLang) -> bool {
    lang.value.starts_with("zh")
}

fn msg(lang: &OutputLang, zh_text: &str, en_text: &str) -> String {
    if zh(lang) {
        zh_text.to_string()
    } else {
        en_text.to_string()
    }
}

/// 装配一条 finding 的证据。`root` 是 post-fix 检出根。
pub fn build(root: &Path, lang: &OutputLang, finding: &InputFinding, fixes: &[FileFix]) -> Evidence {
    let hit = locate(fixes, &finding.file);
    let path = hit.map(|f| f.path.clone()).unwrap_or_else(|| finding.file.clone());
    let kind = hit.map(|f| f.kind).unwrap_or(FixChange::Modified);
    let renamed_from = hit.and_then(|f| f.old_path.clone());
    // 只读一次：快照、符号提取、重锚都用这份原文（prepare 阶段同步做，async 阶段不再碰 fs）。
    let content = crate::positioning::safe_read(root, &path);

    // 只留该文件的 hunk 块（`@@` 起）：`diff --git` / `index` / `---` / `+++` 是 patch 级头，
    // 本文件路径与变更形态已经由 `<change>` 告知；删掉它们也顺带保证「不喂整 PR」可被断言。
    let blocks = hit.map(|f| hunk_blocks(&f.diff)).unwrap_or_default();
    let hunks = match hit {
        Some(_) if blocks.is_empty() => {
            "(no content hunks in the fix diff: this file was renamed/deleted verbatim)".to_string()
        }
        Some(_) => take_bytes(&blocks.join("\n"), HUNKS_BYTE_CAP),
        None => "(this file has no hunks in the fix diff)".to_string(),
    };

    // B-lite：本文件被改的符号名 → 其它文件 hunk 里引用它们的段。
    // 取不到符号（语言不支持 / 改动不在 def 内 / 全被 ref_worthy 滤掉）就**不发该段**并留 warning，
    // 绝不靠猜补位——否则「没证据」会被读成「没问题」。
    let mut warnings = Vec::new();
    let symbols = match (hit, &content) {
        (Some(f), Some(c)) if !blocks.is_empty() => {
            let syms = changed_symbols(&f.path, c, &f.diff);
            if crate::diffsym::lang_for(&f.path).is_none() {
                warnings.push(format!("symbol_blast_radius_skipped:unsupported_language:{}", f.path));
            } else if syms.is_empty() {
                warnings.push(format!("symbol_blast_radius_skipped:no_symbols:{}", f.path));
            }
            syms
        }
        _ => Vec::new(),
    };
    let blast = blast_radius(root, fixes, &path, &symbols);

    let clue = clue_of(&finding.quote);
    let related = related_hunks(fixes, &path, clue);

    // post-fix 检出：删除式修复没有文件可读，但 diff 的 `-` 行仍是证据。
    let snapshot = match &content {
        Some(c) => {
            let (s, e) = finding.span();
            snapshot_window(c, &path, s, e)
        }
        None if kind == FixChange::Deleted => {
            "(absent from the post-fix checkout: the fix deleted this file; the removed lines are in <fix_diff>)".to_string()
        }
        None if hit.is_none() => {
            let note = msg(
                lang,
                &format!("无法定位：`{}` 既不在修复 diff 中，也无法在检出里读取", finding.file),
                &format!("not located: `{}` is neither part of the fix diff nor readable in the checkout", finding.file),
            );
            return Evidence {
                located: false,
                path,
                kind,
                renamed_from,
                hunks,
                related,
                blast,
                snapshot: "(unreadable)".to_string(),
                note: Some(note),
                warnings,
            };
        }
        None => "(unreadable in the post-fix checkout — it may be binary or oversized)".to_string(),
    };

    Evidence { located: true, path, kind, renamed_from, hunks, related, blast, snapshot, note: None, warnings }
}

/// 本文件在 fix diff 里**被改的符号名**（diffsym 的 def 解析；Go/Rust/TS 支持）。
fn changed_symbols(path: &str, content: &str, diff: &str) -> Vec<String> {
    let Some(lang) = crate::diffsym::lang_for(path) else { return Vec::new() };
    let defs = crate::diffsym::compile_defs(lang);
    let lines: Vec<&str> = content.lines().collect();
    let all = crate::diffsym::all_defs(&lines, &defs);
    let mut out: Vec<String> = crate::diffsym::changed_defs(diff, &all)
        .into_iter()
        .map(|d| d.name)
        .filter(|n| crate::diffsym::ref_worthy(n))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// B-lite：其它文件 hunk 里引用了 `symbols` 的段（每条：符号名 + 引用处 `path:line` + 该 hunk 的 ±N 快照）。
///
/// 只看 **fix diff 里被改的其它文件**（hibot 的 fix job 只改 findings 命中文件；未进 diff 的调用点由
/// CI 兜底编译错误）。排序（path、hunk 顺序、符号名）固定 → 同输入同输出。
fn blast_radius(root: &Path, fixes: &[FileFix], own_path: &str, symbols: &[String]) -> String {
    if symbols.is_empty() {
        return String::new();
    }
    let mut ordered: Vec<&FileFix> = fixes
        .iter()
        .filter(|f| f.path != own_path && f.kind != FixChange::Deleted)
        .collect();
    ordered.sort_by(|a, b| a.path.cmp(&b.path));
    let mut out = String::new();
    let mut files = 0usize;
    for fix in ordered {
        if files >= BLAST_FILES_MAX {
            break;
        }
        let Some(content) = crate::positioning::safe_read(root, &fix.path) else { continue };
        let lines: Vec<&str> = content.lines().collect();
        let blocks = hunk_blocks(&fix.diff);
        let hunks = crate::diffsym::parse_hunks(&fix.diff);
        if blocks.len() != hunks.len() {
            continue;
        }
        let mut sections: Vec<String> = Vec::new();
        for (block, h) in blocks.iter().zip(hunks.iter()) {
            let hits: Vec<&String> = symbols.iter().filter(|sym| mentions(block, sym)).collect();
            if hits.is_empty() {
                continue;
            }
            let hs = (h.start + 1).max(1) as u32;
            let he = (h.start + h.count.max(1)) as u32;
            let mut refs = String::new();
            for sym in hits.iter().take(BLAST_REFS_PER_HUNK) {
                match first_line_with(&lines, sym, hs, he) {
                    Some(line) => refs.push_str(&format!("- {sym} referenced at {}:{line}\n", fix.path)),
                    None => refs.push_str(&format!("- {sym} referenced in this hunk ({})\n", fix.path)),
                }
            }
            sections.push(format!("{refs}{}", snapshot_window_with(&content, &fix.path, hs, he, BLAST_RADIUS)));
        }
        if sections.is_empty() {
            continue;
        }
        files += 1;
        out.push_str(&format!("### {} (changed by the fix)\n", fix.path));
        out.push_str(&take_bytes(&sections.join("\n"), BLAST_BYTE_CAP));
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// 引用处行号：先在 hunk 的新文件范围内找，找不到再全文找（1-based）。
fn first_line_with(lines: &[&str], sym: &str, hs: u32, he: u32) -> Option<u32> {
    let lo = (hs as usize).saturating_sub(1);
    let hi = (he as usize).min(lines.len());
    if let Some(i) = lines.get(lo..hi).and_then(|w| w.iter().position(|l| mentions(l, sym))) {
        return Some((lo + i + 1) as u32);
    }
    lines.iter().position(|l| mentions(l, sym)).map(|i| (i + 1) as u32)
}

/// 标识符边界匹配（避免 `Pin` 命中 `Pinned`）。
fn mentions(text: &str, name: &str) -> bool {
    let mut from = 0usize;
    while let Some(i) = text[from..].find(name) {
        let start = from + i;
        let end = start + name.len();
        let before_ok = text[..start].chars().next_back().is_none_or(|c| !is_ident(c));
        let after_ok = text[end..].chars().next().is_none_or(|c| !is_ident(c));
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// quote 的逐字校验：**内部逐字**，只忽略整体首尾空白（换行搬运差异）。
pub fn quote_is_verbatim(quote: &str, material: &str) -> bool {
    let q = quote.trim();
    !q.is_empty() && material.contains(q)
}

/// 跨文件线索：finding 原 quote 里第一行足够长的代码。
fn clue_of(quote: &str) -> Option<&str> {
    quote
        .lines()
        .map(str::trim)
        .find(|l| l.chars().count() >= CLUE_MIN_CHARS)
}

fn take_bytes(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    let cut = s.char_indices().take_while(|(i, _)| *i < cap).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0);
    format!("{}\n…(truncated at {cap} bytes)", &s[..cut])
}

/// ±[`SNAPSHOT_RADIUS`] 行原文（无行号前缀，保证多行 quote 可逐字命中）。
fn snapshot_window(content: &str, path: &str, start: u32, end: u32) -> String {
    snapshot_window_with(content, path, start, end, SNAPSHOT_RADIUS)
}

/// 同上，半径可指定（B-lite 用更小的 ±[`BLAST_RADIUS`]）。
fn snapshot_window_with(content: &str, path: &str, start: u32, end: u32, radius: u32) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let n = lines.len();
    if n == 0 {
        return format!("{path}: (the file is empty in the post-fix checkout)");
    }
    // 声称行号越界（修复让文件变短 / finding 行号陈旧）→ 给文件尾部，并显式注记。
    let (lo, hi, note) = if start as usize > n {
        let lo = n.saturating_sub(120).max(1);
        (lo, n, format!(" — claimed lines {start}-{end} are beyond the file (now {n} lines)"))
    } else {
        let hi = ((end as usize).saturating_add(radius as usize)).min(n);
        ((start.saturating_sub(radius).max(1)) as usize, hi, String::new())
    };
    let body = lines[lo - 1..hi].join("\n");
    let body = take_bytes(&body, SNAPSHOT_BYTE_CAP);
    format!("{path}:{lo}-{hi} (post-fix snapshot){note}\n{body}")
}

/// 其它文件里携带同一段代码的 hunk 块（跨文件修复：只在相关 hunks 内取证）。
fn related_hunks(fixes: &[FileFix], exclude: &str, clue: Option<&str>) -> String {
    let Some(clue) = clue else { return String::new() };
    let mut out = String::new();
    let mut files = 0usize;
    for fix in fixes.iter().filter(|f| f.path != exclude) {
        if files >= RELATED_FILES_MAX {
            break;
        }
        let blocks: Vec<&str> = hunk_blocks(&fix.diff).into_iter().filter(|b| b.contains(clue)).collect();
        if blocks.is_empty() {
            continue;
        }
        let head = format!("### {} ({})\n", fix.path, fix.kind.as_str());
        let body = take_bytes(&blocks.join("\n"), RELATED_BYTE_CAP);
        if out.chars().count() + head.chars().count() + body.chars().count() > RELATED_BYTE_CAP * RELATED_FILES_MAX {
            break;
        }
        files += 1;
        out.push_str(&head);
        out.push_str(&body);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// 把一个文件的 diff 按 `@@` 头切成 hunk 块（段头/`---`/`+++` 行不进块）。
fn hunk_blocks(diff: &str) -> Vec<&str> {
    let mut blocks: Vec<&str> = Vec::new();
    let mut start: Option<usize> = None;
    let mut offset = 0usize;
    for line in diff.split_inclusive('\n') {
        if line.starts_with("@@ ") {
            if let Some(s) = start {
                blocks.push(&diff[s..offset]);
            }
            start = Some(offset);
        }
        offset += line.len();
    }
    if let Some(s) = start {
        blocks.push(&diff[s..]);
    }
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(file: &str, quote: &str, line: u32) -> InputFinding {
        InputFinding {
            fingerprint: format!("sha256:{}", "a".repeat(64)),
            severity: "high".into(),
            confidence: 0.9,
            file: file.into(),
            line_start: line,
            line_end: line,
            quote: quote.into(),
            title: "t".into(),
            body: "b".into(),
            recommendation: "r".into(),
        }
    }

    fn fixes(pairs: &[(&str, &str)]) -> Vec<FileFix> {
        pairs
            .iter()
            .map(|(p, d)| FileFix {
                path: (*p).to_string(),
                old_path: None,
                kind: FixChange::Modified,
                diff: (*d).to_string(),
            })
            .collect()
    }

    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        for (p, body) in files {
            let path = d.path().join(p);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        d
    }

    #[test]
    fn snapshot_is_a_char_verbatim_window_without_line_prefixes() {
        let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        let dir = repo(&[("a.rs", &body)]);
        let ev = build(dir.path(), &OutputLang::default(), &finding("a.rs", "line 100", 100), &[]);
        assert!(ev.snapshot.starts_with("a.rs:40-160 (post-fix snapshot)\n"));
        assert!(ev.snapshot.contains("\nline 100\n"), "正文必须逐字、无行号前缀");
        assert!(ev.located);
    }

    #[test]
    fn beyond_eof_falls_back_to_tail_with_a_note() {
        let dir = repo(&[("a.rs", "one\ntwo\nthree\n")]);
        let ev = build(dir.path(), &OutputLang::default(), &finding("a.rs", "three", 99), &[]);
        assert!(ev.snapshot.contains("beyond the file"), "{}", ev.snapshot);
        assert!(ev.snapshot.contains("three"));
    }

    #[test]
    fn moved_construct_is_found_in_another_files_hunks_only() {
        let clue = "db.query('SELECT * FROM users WHERE id=' + id)";
        let dir = repo(&[("b.rs", "fn other() {}\n")]);
        let fixes = fixes(&[
            ("a.rs", "@@ -1,2 +1,1 @@\n-old\n-db.query('SELECT * FROM users WHERE id=' + id)\n+patched\n"),
            ("c.rs", "@@ -1 +1,1 @@\n+db.query('SELECT * FROM users WHERE id=' + id)\n"),
            ("b.rs", "@@ -1 +1 @@\n-fn other() {}\n+fn other2() {}\n"),
        ]);
        let ev = build(dir.path(), &OutputLang::default(), &finding("a.rs", clue, 2), &fixes);
        assert!(ev.related.contains("### c.rs (modified)"), "{}", ev.related);
        assert!(!ev.related.contains("b.rs"), "无关文件不得进证据: {}", ev.related);
        assert!(ev.related.contains(clue));
    }

    #[test]
    fn deleted_file_needs_no_snapshot_but_stays_located() {
        let dir = repo(&[("keep.rs", "x\n")]);
        let mut f = fixes(&[("gone.rs", "@@ -1 +0,0 @@\n-return db.query(id);\n")]);
        f[0].kind = FixChange::Deleted;
        let ev = build(dir.path(), &OutputLang::default(), &finding("gone.rs", "return db.query(id);", 1), &f);
        assert!(ev.located, "删除式修复仍可判定（构造消失）");
        assert!(ev.snapshot.contains("deleted this file"));
        assert!(ev.hunks.contains("-return db.query(id);"));
    }

    #[test]
    fn absent_and_untouched_file_is_not_located() {
        let dir = repo(&[("keep.rs", "x\n")]);
        let zh = build(dir.path(), &crate::config::Language::ZhCn.output(), &finding("nope.rs", "q", 1), &[]);
        assert!(!zh.located);
        assert!(zh.note.as_deref().unwrap().contains("无法定位"));
        let en = build(dir.path(), &crate::config::Language::EnUs.output(), &finding("nope.rs", "q", 1), &[]);
        assert!(!en.located);
        assert!(en.note.as_deref().unwrap().contains("not located"), "{:?}", en.note);
    }

    #[test]
    fn quote_must_match_verbatim_including_interior_whitespace() {
        let material = "fn f() {\n    let x = 1;\n}";
        assert!(quote_is_verbatim("let x = 1;", material));
        assert!(quote_is_verbatim("    let x = 1;", material));
        assert!(quote_is_verbatim("  let x = 1;  ", material), "只忽略整体首尾空白");
        assert!(!quote_is_verbatim("let x =  1;", material), "内部空白改动必须失败");
        assert!(!quote_is_verbatim("let x = 1;\n  }", material), "改空白/缩进必须失败");
        assert!(!quote_is_verbatim("   ", material));
    }

    fn fix(path: &str, diff: &str) -> FileFix {
        FileFix { path: path.into(), old_path: None, kind: FixChange::Modified, diff: diff.into() }
    }

    /// B-lite 的复现格式：符号名 + 引用处 file:line + 该 hunk 的 ±N 快照。
    #[test]
    fn blast_radius_names_the_symbol_its_reference_and_a_snapshot() {
        let dir = repo(&[
            (
                "service/execution.go",
                "package service\n\nfunc PinnedChannelAllowed(c *gin.Context) bool {\n\tfresh, err := load(c)\n\treturn strict(fresh, err)\n}\n",
            ),
            (
                "middleware/distributor.go",
                "package middleware\n\nfunc Distribute() {\n\tallowed, e := llmconfig.PinnedChannelAllowed(c, snap, channel)\n\tif e != nil || !allowed {\n\t\tabort()\n\t}\n\tfallback, _ := pick()\n\t_ = fallback\n}\n",
            ),
            ("service/other.go", "package service\n\nfunc unrelated() {}\n"),
        ]);
        let fixes = vec![
            fix(
                "service/execution.go",
                "@@ -3,3 +3,4 @@\n func PinnedChannelAllowed(c *gin.Context) bool {\n-\treturn old(c)\n+\tfresh, err := load(c)\n+\treturn strict(fresh, err)\n }\n",
            ),
            fix(
                "middleware/distributor.go",
                "@@ -3,2 +3,3 @@\n func Distribute() {\n-\tallowed, e := llmconfig.ChannelAllowed(c, snap, channel)\n+\tallowed, e := llmconfig.PinnedChannelAllowed(c, snap, channel)\n \tif e != nil || !allowed {\n",
            ),
            fix("service/other.go", "@@ -3,1 +3,2 @@\n func unrelated() {}\n+// touched\n"),
        ];
        let ev = build(dir.path(), &OutputLang::default(), &finding("service/execution.go", "return old(c)", 4), &fixes);
        assert!(ev.located);
        assert!(ev.blast.contains("### middleware/distributor.go (changed by the fix)"), "{}", ev.blast);
        assert!(
            ev.blast.contains("- PinnedChannelAllowed referenced at middleware/distributor.go:4"),
            "引用处必须带 file:line: {}",
            ev.blast
        );
        assert!(ev.blast.contains("(post-fix snapshot)"), "必须附该 hunk 的快照: {}", ev.blast);
        assert!(!ev.blast.contains("service/other.go"), "无关文件不得入选: {}", ev.blast);
        assert!(ev.warnings.is_empty(), "{:?}", ev.warnings);
    }

    #[test]
    fn blast_radius_degrades_with_a_warning_instead_of_guessing() {
        // ① 不支持的语言：不发该段，留 warning。
        let dir = repo(&[("docs/plan.md", "# plan\n\nold line\n")]);
        let fixes = vec![fix("docs/plan.md", "@@ -3,1 +3,1 @@\n-old line\n+new line\n")];
        let ev = build(dir.path(), &OutputLang::default(), &finding("docs/plan.md", "old line", 3), &fixes);
        assert!(ev.blast.is_empty(), "{}", ev.blast);
        assert_eq!(ev.warnings, vec!["symbol_blast_radius_skipped:unsupported_language:docs/plan.md"]);

        // ② 支持的语言但改在 def 外（注释行）→ 同样不发段 + warning。
        let dir = repo(&[("a.go", "package a\n\n// note\nfunc f() {}\n")]);
        let fixes = vec![fix("a.go", "@@ -3,1 +3,2 @@\n // note\n+// more\n")];
        let ev = build(dir.path(), &OutputLang::default(), &finding("a.go", "// note", 3), &fixes);
        assert!(ev.blast.is_empty(), "{}", ev.blast);
        assert_eq!(ev.warnings, vec!["symbol_blast_radius_skipped:no_symbols:a.go"]);
    }

    #[test]
    fn hunk_blocks_split_on_at_headers() {
        let diff = "diff --git a/x b/x\n@@ -1 +1 @@\n-a\n+b\n@@ -9,2 +9,2 @@\n-c\n+d\n";
        let blocks = hunk_blocks(diff);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].starts_with("@@ -1 +1 @@") && !blocks[0].contains("diff --git"));
        assert!(blocks[1].contains("+d"));
    }

    #[test]
    fn rename_relocates_to_the_new_path() {
        let dir = repo(&[("src/new.rs", "fn fixed() {}\n")]);
        let mut f = fixes(&[("src/new.rs", "@@ -1 +1 @@\n-fn vuln() {}\n+fn fixed() {}\n")]);
        f[0].kind = FixChange::Renamed;
        f[0].old_path = Some("src/old.rs".into());
        let ev = build(dir.path(), &OutputLang::default(), &finding("src/old.rs", "fn vuln() {}", 1), &f);
        assert!(ev.located);
        assert_eq!(ev.path, "src/new.rs");
        assert_eq!(ev.renamed_from.as_deref(), Some("src/old.rs"));
        assert!(ev.snapshot.contains("fn fixed()"));
    }

    #[test]
    fn unreadable_path_does_not_escape_the_checkout() {
        let dir = repo(&[("keep.rs", "x\n")]);
        let ev = build(dir.path(), &OutputLang::default(), &finding("../etc/passwd", "root", 1), &[]);
        assert!(!ev.located, "越界路径必须定位失败，而不是读到仓外文件");
    }

}
