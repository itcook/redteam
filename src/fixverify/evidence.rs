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
    pub snapshot: String,
    /// 无法定位的原因（人读，随 `language`）。
    pub note: Option<String>,
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
    // 只留该文件的 hunk 块（`@@` 起）：`diff --git` / `index` / `---` / `+++` 是 patch 级头，
    // 本文件路径与变更形态已经由 `<change>` 告知；删掉它们也顺带保证「不喂整 PR」可被断言。
    let hunks = match hit {
        Some(f) => {
            let blocks = hunk_blocks(&f.diff);
            if blocks.is_empty() {
                "(no content hunks in the fix diff: this file was renamed/deleted verbatim)".to_string()
            } else {
                take_bytes(&blocks.join("\n"), HUNKS_BYTE_CAP)
            }
        }
        None => "(this file has no hunks in the fix diff)".to_string(),
    };
    let clue = clue_of(&finding.quote);
    let related = related_hunks(fixes, &path, clue);

    // post-fix 检出：删除式修复没有文件可读，但 diff 的 `-` 行仍是证据。
    let snapshot = match crate::positioning::safe_read(root, &path) {
        Some(content) => {
            let (s, e) = finding.span();
            snapshot_window(&content, &path, s, e)
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
                snapshot: "(unreadable)".to_string(),
                note: Some(note),
            };
        }
        None => "(unreadable in the post-fix checkout — it may be binary or oversized)".to_string(),
    };

    Evidence { located: true, path, kind, renamed_from, hunks, related, snapshot, note: None }
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
        let hi = ((end as usize).saturating_add(SNAPSHOT_RADIUS as usize)).min(n);
        ((start.saturating_sub(SNAPSHOT_RADIUS).max(1)) as usize, hi, String::new())
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
