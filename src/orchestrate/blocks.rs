//! 稳定块（M21b 批 C3，design C3 §2〔D16〕）：`(文件, 行) → 结构块`。
//!
//! 深挖项的身份锚在**代码结构**而非报告行——同一份代码跨轮成同一块，而疑点的行区间每轮都在动
//! （C1 实测：按报告行 min/max 合并把跨轮重合度从 25% 压到 13–21%）。三条退路：
//! 代码走 enclosing def（与 H0 成区共用 `risk::regions` 的零件，口径不分叉）、markdown 走标题节、其余退固定行带。

use crate::hash::fnv1a64_hex;
use crate::risk::regions::{decl_block_end, enclosing_decl};

/// 退路块半径（与 `risk::regions::NO_DEF_PAD` 同值）：无 def / 无标题时 ±PAD 行。
const PAD: u32 = 10;
/// 块区间的行数上限（评审 #12）：标题节与 def 都可能很长——一份只有一个 `# 标题` 的 CHANGELOG
/// 会让整份文件成为一项，`region::build` 无条件渲染切片、把 64KB 证据预算顶穿且不进省略账。
/// 只截**区间**不改**键**：身份仍是标题行 / 声明行，跨轮稳定性不受影响。
const MAX_BLOCK_LINES: u32 = 200;
/// 退路块按固定行带量化（带宽 = 2·PAD + 1）：键只认带号、不认报告行——
/// 若键跟着行走，同一段里差一行的两条疑点就各成一项，跨轮必然对不上（本批要治的正是这个）。
const BAND: u32 = PAD * 2 + 1;

/// 一个稳定块：1-based 闭区间 + 身份键。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Block {
    pub start: u32,
    pub end: u32,
    /// 身份键：`def:<声明行哈希>` / `md:<标题行哈希>` / `pad:<带号>`。
    /// 空 = 未算出（旧快照 / 拿不到文件内容）——调用方据此退回 C1/C2 的 quote 谓词。
    pub key: String,
}

impl Block {
    /// 未算出的块（旧快照 / 内容不可读）。
    pub fn is_empty(&self) -> bool {
        self.key.is_empty()
    }

    /// 键是否来自**结构**（def / 标题节）。行带退路不是结构身份：同一段落差一行就可能跨带，
    /// 调用方据此在行带上仍走 quote 谓词，避免把 C2 认作同一条的疑点拆成两项（评审 #7）。
    pub fn is_structural(&self) -> bool {
        self.key.starts_with("def:") || self.key.starts_with("md:")
    }
}

/// 文件内容 + 行号 → 稳定块。`line` 为 1-based；越界行按最后一行算。
/// 每次调用都会重编该语言的 def 正则（~1.6 ms）——逐条到达都算块的调用方走 [`block_of_with`] 传缓存。
pub fn block_of(file: &str, content: &str, line: u32) -> Block {
    block_of_with(file, content, line, None)
}

/// 同 [`block_of`]，但由调用方提供已编译的 def 正则（`None` = 本函数自己编）。
/// 主循环的同步段上逐条到达都要算块，重编正则占了 `block_of` 98% 的时间（评审效率项）。
pub fn block_of_with(file: &str, content: &str, line: u32, defs: Option<&[(&'static str, regex::Regex)]>) -> Block {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return band_block(line.max(1), 0);
    }
    let len = lines.len() as u32;
    let ln = line.clamp(1, len);
    let line0 = (ln - 1) as usize;
    if let Some(lang) = crate::diffsym::lang_for(file) {
        let owned;
        let defs = match defs {
            Some(d) => d,
            None => {
                owned = crate::diffsym::compile_defs(lang);
                &owned
            }
        };
        if let Some(di) = enclosing_decl(&lines, line0, defs) {
            let end0 = decl_block_end(&lines, di, Some(defs)).max(di);
            return capped(di as u32 + 1, end0 as u32 + 1, key_of("def", &lines, di));
        }
    } else if is_markdown(file) {
        if let Some((hi, end0)) = heading_section(&lines, line0) {
            return capped(hi as u32 + 1, end0 as u32 + 1, key_of("md", &lines, hi));
        }
    }
    band_block(ln, len)
}

/// 区间封顶（键不变，见 [`MAX_BLOCK_LINES`]）。
fn capped(start: u32, end: u32, key: String) -> Block {
    Block { start, end: end.min(start.saturating_add(MAX_BLOCK_LINES - 1)), key }
}

/// 固定行带退路：带号 = ⌊(行 − 1) / BAND⌋，块 = 该带（末带按文件尾截断）。
/// 行号来自模型输出（needs 的 `lines` 不过锚定），近 `u32::MAX` 时加法会溢出——全程饱和运算（评审 #15）。
fn band_block(line: u32, len: u32) -> Block {
    let band = line.max(1).saturating_sub(1) / BAND;
    let start = band.saturating_mul(BAND).saturating_add(1);
    let mut end = start.saturating_add(BAND - 1);
    if len > 0 {
        end = end.min(len).max(start.min(len));
    }
    Block { start, end: end.max(start), key: format!("pad:{band}") }
}

/// 身份键 = 声明行 / 标题行的**去空白**哈希 + 同文同文本的第几处。
///
/// 哈希保证位移与缩进不改身份；序号补掉碰撞（评审 #1）：一个文件里可以有多条逐字相同的声明
/// （本仓 `src/cli/mod.rs:971` 与 `:3457` 都是 `fn on_cell(&self, rec: crate::state::CellRecord) {`，
/// `impl` 不是 def 模式，两处的 enclosing def 就是这一行）。只按哈希，两个毫不相干的区域会并成一项——
/// 正是本批要消灭的那种不稳定。序号只数「本行之前有几条同文本的行」，与行号无关，上游插删不改。
fn key_of(prefix: &str, lines: &[&str], idx: usize) -> String {
    let norm = compact(lines[idx]);
    let ord = lines[..idx].iter().filter(|l| compact(l) == norm).count();
    format!("{prefix}:{}#{ord}", fnv1a64_hex(&[&norm]))
}

fn compact(line: &str) -> String {
    line.chars().filter(|c| !c.is_whitespace()).collect()
}

fn is_markdown(file: &str) -> bool {
    let ext = file.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    ext == "md" || ext == "markdown"
}

/// markdown 标题节：最近的标题行起，到**下一个标题**前一行止（不分级别，文件被切成不重叠的分区）。
///
/// 实施细化（design C3 §2 写的是「同级或更高级标题切」）：按级别切会让父节包住子节——
/// `# 顶` 前言里的疑点拿到整份文档的区间，与 `## 甲` 里的疑点两项重叠、同一段挖两遍。
/// 分区语义各归各、身份同样稳定（键仍是标题行哈希），故取之。
/// 围栏（``` / ~~~）内的 `# ...` 不算标题——本仓文档里大量 shell 片段以 `#` 起头。
fn heading_section(lines: &[&str], line0: usize) -> Option<(usize, usize)> {
    let mut fence = false;
    let mut heads: Vec<usize> = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim_start();
        if t.starts_with("```") || t.starts_with("~~~") {
            fence = !fence;
            continue;
        }
        if !fence && is_heading(t) {
            heads.push(i);
        }
    }
    let pos = heads.iter().rposition(|&i| i <= line0)?;
    let hi = heads[pos];
    let end = heads.get(pos + 1).map_or(lines.len() - 1, |&i| i.saturating_sub(1));
    Some((hi, end.max(hi)))
}

fn is_heading(t: &str) -> bool {
    let n = t.chars().take_while(|&c| c == '#').count();
    (1..=6).contains(&n) && t.chars().nth(n) == Some(' ')
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_FNS: &str = "use std::fmt;\n\nfn alpha() {\n    let x = 1;\n    let y = 2;\n}\n\nfn beta() {\n    let z = 3;\n}\n";

    #[test]
    fn code_line_maps_to_enclosing_def() {
        let b = block_of("src/a.rs", TWO_FNS, 4);
        assert_eq!((b.start, b.end), (3, 6));
        let b2 = block_of("src/a.rs", TWO_FNS, 5);
        assert_eq!(b.key, b2.key, "同一 def 内的两行同块");
        let other = block_of("src/a.rs", TWO_FNS, 9);
        assert_ne!(b.key, other.key, "相邻 def 是两块");
        assert_eq!((other.start, other.end), (8, 10));
    }

    /// 身份键锚在声明行文本上：上游插行让块位移，`key` 必须不变——否则跨轮就是两项（本批的核心不变式）。
    #[test]
    fn def_key_survives_line_shift() {
        let shifted = format!("// 新增注释\n// 又一行\n{TWO_FNS}");
        let a = block_of("src/a.rs", TWO_FNS, 4);
        let b = block_of("src/a.rs", &shifted, 6);
        assert_eq!(a.key, b.key);
        assert_eq!((b.start, b.end), (5, 8), "区间随位移，键不随");
    }

    /// 缩进改变（如 fn 被包进 impl）不改身份。
    #[test]
    fn def_key_ignores_indentation() {
        let flat = "fn alpha() {\n    1\n}\n";
        let nested = "impl T {\n    fn alpha() {\n        1\n    }\n}\n";
        assert_eq!(block_of("src/a.rs", flat, 2).key, block_of("src/a.rs", nested, 3).key);
    }

    #[test]
    fn markdown_splits_into_heading_sections() {
        let doc = "# 顶\n\n正文 a\n\n## 甲\n\n正文 b\n正文 c\n\n## 乙\n\n正文 d\n";
        let a = block_of("docs/x.md", doc, 7);
        assert_eq!((a.start, a.end), (5, 9), "节止于下一个标题前一行");
        assert_eq!(block_of("docs/x.md", doc, 8).key, a.key, "同节两行同块");
        assert_ne!(block_of("docs/x.md", doc, 12).key, a.key, "相邻节是两块");
        let top = block_of("docs/x.md", doc, 3);
        assert_eq!((top.start, top.end), (1, 4), "父节前言不包住子节——区间不重叠");
        assert_eq!((block_of("docs/x.md", doc, 12).start, block_of("docs/x.md", doc, 12).end), (10, 12), "末节到文件尾");
    }

    /// 围栏内的 `# ...` 不是标题——否则一段 shell 示例就把文档切碎。
    #[test]
    fn fenced_hash_line_is_not_a_heading() {
        let doc = "## 甲\n\n```bash\n# 这是注释\nmake build\n```\n\n正文\n";
        let a = block_of("docs/x.md", doc, 4);
        let b = block_of("docs/x.md", doc, 8);
        assert_eq!(a.key, b.key);
        assert_eq!((a.start, a.end), (1, 8));
    }

    #[test]
    fn markdown_before_first_heading_falls_back_to_band() {
        let doc = "前言一行\n\n# 甲\n正文\n";
        assert!(block_of("docs/x.md", doc, 1).key.starts_with("pad:"));
        assert!(block_of("docs/x.md", doc, 4).key.starts_with("md:"));
    }

    /// 无 def 的代码行（use 段 / 常量区）退行带：同带同键，跨带不同键。
    #[test]
    fn no_def_falls_back_to_band() {
        let b = block_of("src/a.rs", TWO_FNS, 1);
        assert_eq!(b.key, "pad:0");
        assert_eq!((b.start, b.end), (1, 10), "末带按文件尾截断");
        let long = "x\n".repeat(60);
        assert_eq!(block_of("data/x.json", &long, 3).key, block_of("data/x.json", &long, 18).key, "同带同键");
        assert_ne!(block_of("data/x.json", &long, 3).key, block_of("data/x.json", &long, 22).key, "跨带不同键");
        assert_eq!((block_of("data/x.json", &long, 22).start, block_of("data/x.json", &long, 22).end), (22, 42));
    }

    /// 内容拿不到（读失败 / 空文件）仍给一个确定的块：按行带算，同带的两条疑点照样并一项。
    #[test]
    fn empty_and_out_of_range_do_not_panic() {
        let b = block_of("src/a.rs", "", 99);
        assert_eq!((b.start, b.end, b.key.as_str()), (85, 105, "pad:4"));
        assert_eq!(block_of("src/a.rs", "", 90).key, b.key);
        let c = block_of("src/a.rs", TWO_FNS, 999);
        assert_eq!((c.start, c.end), (8, 10), "越界行按最后一行算");
        assert!(!block_of("src/a.rs", TWO_FNS, 0).key.is_empty());
    }

    /// 同一文件里两条逐字相同的声明必须是两个块（评审 #1：本仓 cli/mod.rs 就有两处同签名的 `fn on_cell`）。
    #[test]
    fn identical_decl_lines_get_distinct_keys() {
        let src = "impl A {\n    fn run(&self) {\n        1\n    }\n}\nimpl B {\n    fn run(&self) {\n        2\n    }\n}\n";
        let first = block_of("src/a.rs", src, 3);
        let second = block_of("src/a.rs", src, 8);
        assert_ne!(first.key, second.key, "{first:?} vs {second:?}");
        assert_eq!((first.start, first.end), (2, 4));
        assert_eq!((second.start, second.end), (7, 9));
        // 序号只数同文本行，位移不改身份
        let shifted = format!("// 新行\n{src}");
        assert_eq!(block_of("src/a.rs", &shifted, 9).key, second.key);
    }

    /// 文档里重复的同名标题同理（`## 验收` 在一份文档里出现多次）。
    #[test]
    fn repeated_headings_get_distinct_keys() {
        let doc = "# 甲\n\n## 验收\n正文 1\n\n# 乙\n\n## 验收\n正文 2\n";
        assert_ne!(block_of("d.md", doc, 4).key, block_of("d.md", doc, 9).key);
    }

    /// 行号来自模型输出，近 u32::MAX 不得 panic / 回绕（评审 #15）。
    #[test]
    fn huge_line_numbers_saturate() {
        for line in [u32::MAX, u32::MAX - 2, u32::MAX / 2] {
            let b = block_of("src/a.rs", "", line);
            assert!(b.start <= b.end, "{b:?}");
            let c = block_of("data/x.json", "x\ny\n", line);
            assert!(c.start <= c.end, "{c:?}");
        }
    }

    /// 区间封顶但身份不变（评审 #12）：一份只有一个标题的长文档不得把整份文件变成一项的区间。
    #[test]
    fn long_sections_are_capped_but_keep_their_key() {
        let doc = format!("# 只有一个标题\n{}", "正文\n".repeat(500));
        let head = block_of("CHANGELOG.md", &doc, 2);
        let deep = block_of("CHANGELOG.md", &doc, 400);
        assert_eq!((head.start, head.end), (1, MAX_BLOCK_LINES), "区间封顶");
        assert_eq!(head.key, deep.key, "同一节仍是同一身份");
        let long_fn = format!("fn big() {{\n{}}}\n", "    step();\n".repeat(400));
        let b = block_of("src/a.rs", &long_fn, 100);
        assert_eq!((b.start, b.end), (1, MAX_BLOCK_LINES), "def 同样封顶");
    }

    /// 传入已编译的 defs 与自己编的结果逐字一致（效率项只动性能，不动语义）。
    #[test]
    fn precompiled_defs_match_self_compiled() {
        let lang = crate::diffsym::lang_for("src/a.rs").unwrap();
        let defs = crate::diffsym::compile_defs(lang);
        for line in [1, 4, 9] {
            assert_eq!(block_of_with("src/a.rs", TWO_FNS, line, Some(&defs)), block_of("src/a.rs", TWO_FNS, line));
        }
    }

    #[test]
    fn structural_keys_are_marked() {
        assert!(block_of("src/a.rs", TWO_FNS, 4).is_structural(), "def 是结构身份");
        assert!(block_of("d.md", "# 甲\n正文\n", 2).is_structural(), "标题节是结构身份");
        assert!(!block_of("data/x.json", "{\n}\n", 1).is_structural(), "行带不是");
        assert!(!Block::default().is_structural());
    }

    #[test]
    fn default_block_is_empty() {
        assert!(Block::default().is_empty());
        assert!(!block_of("src/a.rs", TWO_FNS, 4).is_empty());
    }
}
