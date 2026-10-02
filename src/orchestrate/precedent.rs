//! 判例前移（M21c 批 B-2，design §4 / §6 规则 2〔D13〕）：把「这条疑点人已经判过是误报」的判断
//! 提到**烧 token 之前**——入队侧，而不是 consolidate 层。
//!
//! 为什么要守卫：无守卫的判例沿用有一条漏报路径——**fix 引入的新 bug 被旧判例盖住**（design §4）。
//! 所以命中判例不等于豁免，还要过两道：
//! 1. **锚定可重定位**：判例的 quote 还能在当前文件里找到。找不到 = 那段代码已经不是人当初看的样子。
//! 2. **区域自判例以来无 delta**：判例 quote 的当前行范围与本轮 diff 的 hunk **不相交**。
//!
//! 第 2 道的口径〔本批自定〕：判例卡只有 `round`（字符串）与天粒度 `updated_at`，推不出 commit，
//! 按日期跑 `git log` 既不准也贵；而本轮 diff 就是已知的改动集，直接拿 hunk 相交即可。
//! 粗一点的做法「判例 file ∈ 变更文件集」行不通——scanner 只扫变更文件，那样**永远不豁免**、功能失效。

use crate::db::DismissCard;
use crate::domain::Suspicion;

/// 判例命中的处置。
#[derive(Clone, Debug, PartialEq)]
pub enum Precedent {
    /// 守卫全过 → 沿用结案：不入队、不烧 token。
    Exempt { card_id: String },
    /// 命中了但守卫不过 → 照常入队，带一句笔记给 reviewer。
    Note { card_id: String, note: String },
}

/// 机械匹配阈值。**比 refute 侧严**：那边命中只是「送进复核终审」，召回优先、阈值宽；
/// 这边命中会**直接豁免不入队**，没有下游终审，宽了就是漏报。
const QUOTE_OVERLAP_MIN: f32 = 0.5;

/// 一条疑点撞上哪张判例卡（取第一张命中的，卡序由调用方保证确定）。
///
/// 判据只用 **file 相同 ∧ quote 重叠**〔本批自定〕：`consolidate::verify` 那个 finding 版用
/// `title_jaccard`，而 `Suspicion` 没有 title——`hint` 是 <50 token 的一句话提示，与 finding title
/// 不同质，拿它算 jaccard 会制造虚假相关。两个版本因此不共用实现，不是重复。
pub fn matches<'a>(s: &Suspicion, cards: &'a [DismissCard]) -> Option<&'a DismissCard> {
    // 不另判「卡的 quote 是否为空」：`quote_overlap` 对空侧恒返回 0.0（`min == 0` 早退），
    // 空 quote 的卡永远达不到阈值。多加一个分支只是多一处没人盯得住的代码（变异检查实测它恒绿）。
    cards.iter().find(|c| {
        s.file == c.file && crate::consolidate::dedup::quote_overlap(&s.quote, &c.quote) >= QUOTE_OVERLAP_MIN
    })
}

/// 命中判例后过两道守卫，给出处置。`hunks` = 该文件本轮 diff 的新文件侧行范围。
pub fn judge(
    s: &Suspicion,
    cards: &[DismissCard],
    hunks: &[(usize, usize)],
    root: &std::path::Path,
) -> Option<Precedent> {
    let c = matches(s, cards)?;
    let id = c.id.clone();
    // 守卫 1：判例的 quote 还锚得上吗（用判例的 quote 而不是疑点的——它是人裁定时的那个锚）
    let Some((ls, le)) = crate::positioning::anchor_near(root, &c.file, &c.quote, s.line_start) else {
        return Some(Precedent::Note {
            card_id: id.clone(),
            note: format!("疑似判例 {id} 的变体，但该判例的引文已锚不上当前代码——按新问题重挖"),
        });
    };
    // 守卫 2：那片区域这一轮被改过吗
    if hunks.iter().any(|&(a, b)| ls as usize <= b && a <= le as usize) {
        return Some(Precedent::Note {
            card_id: id.clone(),
            note: format!(
                "疑似判例 {id} 的变体，但 {}:{ls}-{le} 这一轮被改过——旧结论可能已不适用，重挖",
                c.file
            ),
        });
    }
    Some(Precedent::Exempt { card_id: id })
}

#[cfg(test)]
mod tests {
    use super::*;

    // `DismissCard` 是纯数据结构、没有不变式，字面量即可；
    // 指南里「夹具走真实写入口」针对的是**状态**（DB 行、队列项），不是这种。
    fn card(id: &str, file: &str, quote: &str) -> DismissCard {
        DismissCard {
            id: id.into(), status: "dismissed".into(), title: "t".into(), file: file.into(),
            quote: quote.into(), severity: "high".into(), reason: "人判过是误报".into(),
            round: "r1".into(), updated_at: String::new(),
        }
    }

    fn sus(file: &str, line: u32, quote: &str) -> Suspicion {
        Suspicion {
            file: file.into(), line_start: line, line_end: line, hint: "h".into(),
            category: None, confidence: 0.6, quote: quote.into(),
        }
    }

    fn repo(body: &str) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a.rs"), body).unwrap();
        t
    }

    /// 机械匹配：同文件 ∧ quote 重叠达阈值才算命中。
    #[test]
    fn matches_requires_same_file_and_quote_overlap() {
        let cards = vec![card("1111111111111111", "a.rs", "let x = take(v);")];
        assert!(matches(&sus("a.rs", 2, "let x = take(v);"), &cards).is_some(), "逐字同 → 命中");
        assert!(matches(&sus("b.rs", 2, "let x = take(v);"), &cards).is_none(), "异文件不命中");
        assert!(matches(&sus("a.rs", 2, "完全不同的一行代码();"), &cards).is_none(), "quote 不重叠不命中");
        // 空 quote 的卡命不中——由 `quote_overlap` 的 `min == 0 → 0.0` 兜住，不需要单独判
        let blank = vec![card("2222222222222222", "a.rs", "   ")];
        assert!(matches(&sus("a.rs", 2, "let x = take(v);"), &blank).is_none(), "空 quote 的卡不得命中");
    }

    /// 两道守卫全过 → 豁免。
    #[test]
    fn both_guards_pass_yields_exemption() {
        let t = repo("fn f() {\n    let x = take(v);\n}\n");
        let cards = vec![card("1111111111111111", "a.rs", "let x = take(v);")];
        // 本轮改的是别处（第 9 行），判例区域（第 2 行）没动
        let got = judge(&sus("a.rs", 2, "let x = take(v);"), &cards, &[(9, 9)], t.path());
        assert_eq!(got, Some(Precedent::Exempt { card_id: "1111111111111111".into() }));
    }

    /// 守卫 1 不过（判例引文锚不上）→ 入队带笔记，**不豁免**。
    #[test]
    fn unanchorable_precedent_does_not_exempt() {
        let t = repo("fn f() {\n    let y = other();\n}\n"); // 判例的 quote 已不在文件里
        let cards = vec![card("1111111111111111", "a.rs", "let x = take(v);")];
        let got = judge(&sus("a.rs", 2, "let x = take(v);"), &cards, &[(9, 9)], t.path());
        match got {
            Some(Precedent::Note { card_id, note }) => {
                assert_eq!(card_id, "1111111111111111");
                assert!(note.contains("锚不上"), "{note}");
            }
            other => panic!("该入队带笔记，得到 {other:?}"),
        }
    }

    /// 守卫 2 不过（判例区域本轮被改过）→ 必须重挖。这是 design §4 点名要防的漏报路径：
    /// fix 引入的新 bug 被旧判例盖住。
    #[test]
    fn delta_touching_the_precedent_region_forces_a_redig() {
        let t = repo("fn f() {\n    let x = take(v);\n}\n");
        let cards = vec![card("1111111111111111", "a.rs", "let x = take(v);")];
        // 本轮 hunk 覆盖第 2 行 —— 正是判例所指的那一行
        let got = judge(&sus("a.rs", 2, "let x = take(v);"), &cards, &[(1, 3)], t.path());
        match got {
            Some(Precedent::Note { note, .. }) => assert!(note.contains("这一轮被改过"), "{note}"),
            other => panic!("该重挖，得到 {other:?}"),
        }
        // 边界：hunk 紧贴但不相交 → 仍豁免（第 2 行 vs 改动在 3-4 行之后）
        let near = judge(&sus("a.rs", 2, "let x = take(v);"), &cards, &[(3, 4)], t.path());
        assert!(matches!(near, Some(Precedent::Exempt { .. })), "不相交就不该拦: {near:?}");
    }

    /// 没命中判例 → `None`（调用方按无判例正常入队）。
    #[test]
    fn no_match_is_none() {
        let t = repo("fn f() {\n    let x = take(v);\n}\n");
        assert!(judge(&sus("a.rs", 2, "let x = take(v);"), &[], &[], t.path()).is_none());
    }
}
