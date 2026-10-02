//! pack 引擎（M15-S1 起；M21a 批 E 升为改动中心切片）：每 bundle 的预算治理上下文块——
//! 0 壳改动源文（`[review.pack] body = slice` 缺省〔D8〕：核 / 文件内一跳 / 文件地图 / 折叠 / 禁真空兜底；
//! `full` 旋钮 = 整文件）+ 文件外片段（slice 档）+ 回落梯子（def 体 → 骨架 → hunk 窗 → omit 坐标）+ 精确省略清单。
//! 唯一路径，无引擎分派。hermetic：喂 (bundle, root, budget, opts) 出字符串与形态统计，零 LLM。
//! 复用 src/diffsym 的 diff / 符号 / 一跳解析；每个改动文件只读一次、hunk 只重锚定一次（`FileCtx`）。

use std::collections::BTreeSet;
use std::path::Path;

use regex::Regex;

use crate::bundle::Bundle;
pub use crate::config::PackBody;
use crate::config::PackCfg;
use crate::diffsym::{
    all_defs, body_names, bracket_delta_code, changed_def_ranges, changed_defs, compile_defs, hunk_changed_lines,
    is_comment_line, lang_for, match_def, one_hop, parse_hunk_new_range, parse_hunks, ref_worthy, rust_test_ranges,
    DefRange, Hunk, OneHop,
};
use crate::evidence::git::FileChange;

pub mod contracts;
pub mod region;
pub use region::{Origin, QueueItemSpec, RegionPack, RegionStats};

/// 省略清单条目上限（防大 fan-in 撑爆；超出末尾计数兜底）。
pub(crate) const OMIT_MAX_ENTRIES: usize = 200;
/// 文件头（首个 def 之前的注释 / doc / import 行）最多保留行数，其余折叠。
const HEADER_MAX_LINES: usize = 80;
/// 多行签名最多向下延续行数（到首个 `{` 或 `;` 为止）。
const SIG_MAX_LINES: usize = 6;
/// 顶层胶水（不在任何 def 内的行：impl 头 / 闭合括号 / 空行 / 属性）连续 ≤ 此行数原样保留，否则折叠。
const GLUE_MAX_LINES: usize = 3;

/// 装包旋钮（`[review.pack]`，design §12.1）：`body` 形态、hunk 窗半径、小文件全文阈值。
/// 由 `PackCfg` 转出（缺省值唯一真相源在 config），pack 自身不读 `Config`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackOpts {
    pub body: PackBody,
    pub anchor_window: usize,
    pub slice_min_bytes: usize,
}

impl From<&PackCfg> for PackOpts {
    fn from(p: &PackCfg) -> Self {
        PackOpts { body: p.body, anchor_window: p.anchor_window as usize, slice_min_bytes: p.slice_min_bytes as usize }
    }
}

impl Default for PackOpts {
    /// = config 缺省（slice / 30 / 8192〔D8〕）。
    fn default() -> Self { PackOpts::from(&PackCfg::default()) }
}

impl PackOpts {
    /// `full` 档（其余取缺省）：Full 路径 / 对照臂。
    pub fn full() -> Self { PackOpts { body: PackBody::Full, ..PackOpts::default() } }

    /// 二阶段补查档（批 E K2，复审修 1 / 9）：在用户配置之上只改一件事——`slice_min_bytes = MAX`，每文件都走全文块
    /// （needs 是显式索要全文）；`body` / `anchor_window` 沿用用户值：slice 档第一趟 draft 引用的 `## 文件外片段`
    /// 第二趟同样在场，full 对照臂不被切成 slice。
    pub fn second_pass(base: &PackOpts) -> Self { PackOpts { slice_min_bytes: usize::MAX, ..base.clone() } }
}

/// 装包形态统计（批 E K13，铁律 1）：`--pack-only` stdout + summary.json 可读；RunMeta 接线在 F-5。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PackStats {
    /// `slice` / `full`（批 F-3 改 String：进报告 JSON 需 Deserialize）。
    pub body: String,
    /// 有内容的改动文件数（工作树缺失的不计）。
    pub files: usize,
    /// 切片块入正文的文件数。
    pub sliced: usize,
    /// `≤ slice_min_bytes` 走全文的文件数（slice 档）。
    pub small_full: usize,
    /// 全文块入正文的文件数（full 档，或 slice 档无可解析 hunk）。
    pub full: usize,
    /// 第一级装不下、进入回落梯子（def 体 / 骨架 / hunk 窗）的文件数。
    pub ladder: usize,
    /// 有条目进省略清单的文件数。
    pub omitted: usize,
    /// 切片展开的一跳 def 总数。
    pub one_hop_defs: usize,
    /// 切片折叠掉的行数总和。
    pub folded_lines: usize,
    /// 文件外片段：调用点块数 / 签名块数 / 段字节。
    pub outside_sites: usize,
    pub outside_sigs: usize,
    pub outside_bytes: usize,
    /// 本组自己的 diff 字节（M21d 批 C，交付项 7 的切分键）。
    ///
    /// 拆大组的验收**不挂墙钟**〔2026-09-23 实测推翻墙钟前提〕，改按「每格 diff 字节」判。
    /// 这个数此前只有 `--pack-only` 的仪表报得出，真实评审的报告里读不到——
    /// 于是「组是不是太大」永远只能靠跑一次仪表，日常 dogfood 攒不出分布。
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub diff_bytes: usize,
    /// L4-B 三方契约（M21d 批 A）：注入的契约块数。
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub contracts: usize,
    /// 定位不到而降级的 crate 数（registry 在 CI / 他人机器缺失时必须看得见是降级了，roadmap 风险节）。
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub contract_misses: usize,
    /// 超帽没进去的契约块数（仓内上下文优先，三方契约用剩下的额度）。
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub contract_spilled: usize,
    /// 帽被仓内上下文吃光、整段没注入。这件事只写进 pack 正文的省略清单就等于没披露——
    /// 报告与 md 读的是统计（dogfood 抓到：PR181 的大组正是这种，md 上一片空白）。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub contract_budget_skipped: bool,
}

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

/// `pack_bundle` 产物：正文 + 形态统计。
#[derive(Clone, Debug)]
pub struct Packed {
    pub text: String,
    pub stats: PackStats,
    /// 每个文件被折叠掉的行区间（1-based 含端点，M21d 批 C-1）。
    ///
    /// 只在运行时传递、**不进报告 JSON**（一组可能几百段，报告会被撑爆）。
    /// 用途：判 design §10.2 的回退信号——scanner 的 `needs{kind:evidence}` 是不是在
    /// **反复索要被折叠的函数体**。光看 needs 条数判不了这件事（三轮都稳定在三成）。
    pub folded: std::collections::BTreeMap<String, Vec<(u32, u32)>>,
}

/// 切片块的形态（块头与统计用）。
struct SliceOut {
    text: String,
    one_hop: usize,
    folded_lines: usize,
    /// 折叠掉的行区间（1-based 含端点）——与渲染出的 `…（a-b 行折叠…）` 同源，不另算一遍。
    folded_ranges: Vec<(u32, u32)>,
}

// ---------------------------------------------------------------------------------------------
// 改动文件上下文：一次读取 / 一次 def 编译 / 一次 hunk 重锚定，0 壳与文件外片段共用（复审修 2 + 效率项）
// ---------------------------------------------------------------------------------------------

struct FileCtx<'a> {
    fc: &'a FileChange,
    content: String,
    n_lines: usize,
    defs: Option<Vec<(&'static str, Regex)>>,
    /// 重锚定后的 hunk（坐标已按 + 行文本对齐到工作树内容）。
    hunks: Vec<Hunk>,
    /// 供旧口径 API（`changed_def_ranges` / `skeleton_of` / `hunk_window_block` / `changed_defs`）消费的 diff：
    /// 坐标未偏移时就是原 diff；偏移过则是按重锚定坐标合成的只含坐标的 diff。
    diff: String,
}

/// 按填充序（`hub_order`，枢纽先）准备每个改动文件；工作树缺失 / 空内容的跳过（沿用 bundle::group 安全不变量：
/// 路径来自 git diff、非模型可控）。
fn prepare_files<'a>(bundle: &'a Bundle, root: &Path) -> Vec<FileCtx<'a>> {
    let mut out = Vec::new();
    for fc in bundle.fill_order() {
        if let Some(ctx) = prepare_one(fc, root) {
            out.push(ctx);
        }
    }
    out
}

/// 单文件上下文：一次读取 / def 编译 / hunk 重锚定（组 pack 与 reviewer 区域 pack〔批 B〕共用）。
fn prepare_one<'a>(fc: &'a FileChange, root: &Path) -> Option<FileCtx<'a>> {
    let content = std::fs::read_to_string(root.join(&fc.path)).unwrap_or_default();
    if content.is_empty() {
        return None;
    }
    let lines: Vec<&str> = content.lines().collect();
    let n_lines = lines.len();
    let (hunks, shifted) = anchor_hunks(&lines, parse_hunks(&fc.diff));
    let diff = if shifted { synth_diff(&hunks) } else { fc.diff.clone() };
    let defs = lang_for(&fc.path).map(compile_defs);
    Some(FileCtx { fc, content, n_lines, defs, hunks, diff })
}

/// hunk 重锚定搜索半径（行）。
const ANCHOR_SEARCH_MAX: usize = 2000;

/// 有信息量的 + 行：去空白后 ≥ 4 字符且含字母数字——只含 `}` / `)` / 空行的 + 行在任何函数里都能对上，不参与定位。
fn informative(text: &str) -> bool {
    let t = text.trim();
    t.len() >= 4 && t.chars().any(|c| c.is_alphanumeric())
}

/// hunk 的整体行偏移（评审修 4，复审修 4）：只按有信息量的 + 行打分；声明坐标处全部吻合 → 0；否则按 ±1、±2 … 找首个
/// 全部吻合的偏移；都不全吻合时取得分最高、且 ≥ 一半 + 行并严格优于原位的偏移（容忍 hunk 内个别行被后续编辑改过）；
/// 无有信息量的 + 行（纯删除 / 只有括号行）→ 0（保留原坐标，禁真空窗兜底）。
fn anchor_offset(lines: &[&str], h: &Hunk) -> i64 {
    let plus: Vec<(usize, &str)> = h.plus.iter().filter(|(_, t)| informative(t)).map(|(l, t)| (*l, t.as_str())).collect();
    if plus.is_empty() {
        return 0;
    }
    let score = |off: i64| -> usize {
        plus.iter()
            .filter(|(l, text)| {
                let i = *l as i64 + off;
                i >= 0 && (i as usize) < lines.len() && lines[i as usize] == *text
            })
            .count()
    };
    let total = plus.len();
    let at_zero = score(0);
    if at_zero == total {
        return 0;
    }
    let (mut best_off, mut best) = (0i64, at_zero);
    for d in 1..=ANCHOR_SEARCH_MAX as i64 {
        for off in [d, -d] {
            let sc = score(off);
            if sc == total {
                return off;
            }
            if sc > best {
                best = sc;
                best_off = off;
            }
        }
    }
    if best > at_zero && best * 2 >= total { best_off } else { 0 }
}

/// 逐 hunk 重锚定；返回 (偏移后的 hunks, 是否有偏移)。
fn anchor_hunks(lines: &[&str], hunks: Vec<Hunk>) -> (Vec<Hunk>, bool) {
    let n = lines.len();
    let mut shifted = false;
    let out = hunks
        .into_iter()
        .map(|h| {
            let off = anchor_offset(lines, &h);
            if off == 0 || n == 0 {
                return h;
            }
            shifted = true;
            let sh = |l: usize| ((l as i64 + off).max(0) as usize).min(n - 1);
            Hunk {
                start: sh(h.start),
                count: h.count,
                changed: h.changed.iter().map(|&l| sh(l)).collect(),
                plus: h.plus.into_iter().map(|(l, t)| (sh(l), t)).collect(),
                minus: h.minus,
            }
        })
        .collect();
    (out, shifted)
}

/// 由重锚定后的 hunk 合成一份只含坐标的 diff（每个改动行一个单行 hunk），喂 `changed_def_ranges` / `skeleton_of` /
/// `hunk_window_block` / `changed_defs`——这些旧口径 API 只消费坐标。
fn synth_diff(hunks: &[Hunk]) -> String {
    let lines: BTreeSet<usize> = hunks.iter().flat_map(|h| h.changed.iter().copied()).collect();
    lines.iter().map(|l| format!("@@ -{0},1 +{0},1 @@\n+\n", l + 1)).collect()
}

/// diff 各 hunk 的新文件起止 `(c, d)`（`@@ -a,b +c,d @@`；d 缺省 1）。
/// M21c 批 B-2 起也给判例守卫用（判「判例区域自那以来有没有 delta」），故提为 crate 可见。
pub(crate) fn hunk_ranges(diff: &str) -> Vec<(usize, usize)> {
    diff.lines().filter(|l| l.starts_with("@@")).filter_map(parse_hunk_new_range).collect()
}

/// 文件头保留行：注释 / doc / import / package / mod 一类（首个 def 之前的地图）。
fn is_header_line(t: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "//", "/*", "*", "#", "--", "use ", "pub use ", "pub(crate) use ", "extern crate ", "import ", "export ", "from ",
        "package ", "mod ", "pub mod ", "pub(crate) mod ", "require(",
    ];
    PREFIXES.iter().any(|p| t.starts_with(p))
}

/// import 类行（多行 import 组的开头；调用点判定排除，评审修 6 / 8）。`export` 只算 re-export（`export {…} from` /
/// `export *`）——`export function foo(` / `export const x = f()` 是 def / 代码行，不能被短路掉（复审修 3）。
fn is_import_line(t: &str) -> bool {
    const P: &[&str] = &["use ", "pub use ", "pub(crate) use ", "extern crate ", "import ", "from ", "require(", "mod ", "pub mod ", "pub(crate) mod "];
    P.iter().any(|p| t.starts_with(p))
        || (t.starts_with("export ") && (t.starts_with("export {") || t.starts_with("export *") || t.contains(" from ")))
}

/// def 签名尾行：从 start 向下到首个含 `{` 或以 `;` 结尾的行（含），最多 `SIG_MAX_LINES` 行，不越 def 尾。
fn sig_end(lines: &[&str], d: &DefRange) -> usize {
    let hi = d.end.min(d.start + SIG_MAX_LINES - 1).min(lines.len().saturating_sub(1));
    (d.start..=hi).find(|&i| lines[i].contains('{') || lines[i].trim_end().ends_with(';')).unwrap_or(hi)
}

/// 区间排序 + 合并（重叠或相邻）。
fn merge_ranges(mut v: Vec<(usize, usize)>) -> Vec<(usize, usize)> {
    v.sort_unstable();
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 + 1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// 改动中心切片（design §10.2〔D8〕，批 E K6 / K7）：`≤ slice_min_bytes` 或无可解析改动行 → None（调用方给全文）。
/// 展开区间 = 核（`changed_defs`）∪ 一跳 ∪ 禁真空窗（任一改动行未被覆盖的 hunk 取 ±`anchor_window`）；
/// 保留行 = 文件头（≤ `HEADER_MAX_LINES`）+ 全部 def 签名；其余按外围 def 折叠成 `…（a-b 行折叠：name）`，
/// 顶层胶水 ≤ `GLUE_MAX_LINES` 原样、更长折叠成 `…（a-b 行折叠）`。非支持语言：无 def，展开只来自禁真空窗。
fn slice_file(ctx: &FileCtx<'_>, opts: &PackOpts) -> Option<SliceOut> {
    if ctx.content.len() <= opts.slice_min_bytes {
        return None;
    }
    let lines: Vec<&str> = ctx.content.lines().collect();
    let n = lines.len();
    if n == 0 {
        return None;
    }
    // 改动行与每 hunk 跨度（坐标已在 prepare_files 重锚定）
    let mut changed_lines: BTreeSet<usize> = BTreeSet::new();
    let mut spans: Vec<(usize, usize)> = Vec::new();
    for h in ctx.hunks.iter().filter(|h| !h.changed.is_empty()) {
        changed_lines.extend(h.changed.iter().map(|&l| l.min(n - 1)));
        // hunk 跨度；无后随上下文的尾部纯删除点（= start + count，`-U0` / 合成 diff）也并入，否则它不触发禁真空窗（合并后审计 nit 7）
        let hi = (h.start + h.count.max(1) - 1).max(h.changed.iter().copied().max().unwrap_or(0));
        spans.push((h.start.min(n - 1), hi.min(n - 1)));
    }
    if changed_lines.is_empty() {
        return None;
    }
    let (all, changed, hop) = match ctx.defs.as_deref() {
        Some(d) => {
            let all = all_defs(&lines, d);
            let changed = changed_defs(&ctx.diff, &all);
            let hop = one_hop(&lines, &all, &changed);
            (all, changed, hop)
        }
        None => (Vec::new(), Vec::new(), OneHop::default()),
    };
    // 展开区间：核 ∪ 一跳
    let mut expand: Vec<(usize, usize)> = changed
        .iter()
        .chain(hop.callees.iter())
        .chain(hop.callers.iter())
        .map(|d| (d.start, d.end.min(n - 1)))
        .collect();
    // 禁真空（K7）：某 hunk 有改动行不在任何展开区间 → 整 hunk ±N 窗并入
    let covered = |l: usize, ex: &[(usize, usize)]| ex.iter().any(|&(a, b)| a <= l && l <= b);
    for &(lo, hi) in &spans {
        if changed_lines.range(lo..=hi).any(|&l| !covered(l, &expand)) {
            expand.push((lo.saturating_sub(opts.anchor_window), (hi + opts.anchor_window).min(n - 1)));
        }
    }
    let expand = merge_ranges(expand);
    let (body, folds, folded_lines, folded_ranges) = render_slice_body(ctx, &all, &expand);
    let one_hop = hop.callees.len() + hop.callers.len();
    let head = format!("{}:1-{n}（切片：核 {} · 一跳 {one_hop} · 折叠 {folds} 段）", ctx.fc.path, changed.len());
    Some(SliceOut { text: format!("{head}\n{body}\n"), one_hop, folded_lines, folded_ranges })
}

/// 切片渲染机器（组切片 [`slice_file`] 与 reviewer 区域切片〔批 B `pack::region`〕共用，design §10.2 三层同批）：
/// 保留行 = 文件头（≤ `HEADER_MAX_LINES`）+ 全部 def 签名；其余按外围 def 折叠，顶层胶水 ≤ `GLUE_MAX_LINES` 原样。
/// 返回 (正文, 折叠段数, 折叠行数)。非支持语言无 def，「头」只看文件顶部 `HEADER_MAX_LINES` 行（评审修 7）；多行 import 组
/// （Go `import (`、Rust `use x::{`、TS `import {`）按代码内括号配平整组保留（评审修 8；字符串 / 注释里的括号不计）。
fn render_slice_body(
    ctx: &FileCtx<'_>,
    all: &[crate::diffsym::DefRange],
    expand: &[(usize, usize)],
) -> (String, usize, usize, Vec<(u32, u32)>) {
    let lines: Vec<&str> = ctx.content.lines().collect();
    let n = lines.len();
    let mut keep: BTreeSet<usize> = BTreeSet::new();
    let first_def = all.first().map(|d| d.start).unwrap_or(n);
    let header_limit = if ctx.defs.is_some() { first_def } else { HEADER_MAX_LINES.min(n) };
    let mut header_kept = 0usize;
    let mut open_depth: i32 = 0;
    for (i, line) in lines.iter().enumerate().take(header_limit) {
        let t = line.trim();
        if open_depth > 0 {
            keep.insert(i);
            header_kept += 1;
            open_depth += bracket_delta_code(t);
        } else if t.is_empty() {
            continue;
        } else if is_header_line(t) {
            keep.insert(i);
            header_kept += 1;
            if is_import_line(t) {
                open_depth = bracket_delta_code(t).max(0);
            }
        }
        if header_kept >= HEADER_MAX_LINES {
            break;
        }
    }
    for d in all {
        for i in d.start..=sig_end(&lines, d) {
            keep.insert(i);
        }
    }
    // 每行最内层外围 def（折叠标记命名用）：按 start 升序赋值，内层后写覆盖外层
    let mut owner: Vec<Option<usize>> = vec![None; n];
    for (idx, d) in all.iter().enumerate() {
        for o in owner.iter_mut().take(d.end.min(n - 1) + 1).skip(d.start) {
            *o = Some(idx);
        }
    }
    // 渲染
    let mut body = String::new();
    let (mut folds, mut folded_lines) = (0usize, 0usize);
    let mut folded_ranges: Vec<(u32, u32)> = Vec::new();
    let mut ei = 0usize;
    let mut i = 0usize;
    while i < n {
        while ei < expand.len() && expand[ei].1 < i {
            ei += 1;
        }
        if let Some(&(a, b)) = expand.get(ei) {
            if a <= i {
                for l in &lines[i..=b] {
                    body.push_str(l);
                    body.push('\n');
                }
                i = b + 1;
                continue;
            }
        }
        if keep.contains(&i) {
            body.push_str(lines[i]);
            body.push('\n');
            i += 1;
            continue;
        }
        let next_expand = expand.get(ei).map(|e| e.0).unwrap_or(n);
        let own = owner[i];
        let mut j = i;
        while j < n && j < next_expand && !keep.contains(&j) && owner[j] == own {
            j += 1;
        }
        let len = j - i;
        match own {
            Some(idx) => {
                body.push_str(&format!("…（{}-{} 行折叠：{}）\n", i + 1, j, all[idx].name));
                folds += 1;
                folded_lines += len;
                folded_ranges.push((i as u32 + 1, j as u32));
            }
            None if len <= GLUE_MAX_LINES => {
                for l in &lines[i..j] {
                    body.push_str(l);
                    body.push('\n');
                }
            }
            None => {
                body.push_str(&format!("…（{}-{} 行折叠）\n", i + 1, j));
                folds += 1;
                folded_lines += len;
                folded_ranges.push((i as u32 + 1, j as u32));
            }
        }
        i = j;
    }
    (body, folds, folded_lines, folded_ranges)
}

/// 0 壳段：每个改动文件一块，第一级 = 切片块（slice 档、大文件）或全文块（full 档 / 小文件 / 无可解析 hunk），
/// 装不下 → 回落梯子（不砍语义只降粒度）：def 体（支持语言）→ 骨架 → hunk 窗（非支持语言）→ omit 坐标。按 budget 字节封顶。
/// 测试入口（生产路径 `pack_bundle` 走 `body_section_ctx`，复用已准备好的 `FileCtx`）。
#[cfg(test)]
pub(crate) fn body_section(
    bundle: &Bundle,
    root: &Path,
    budget: usize,
    omit: &mut Vec<String>,
    opts: &PackOpts,
    stats: &mut PackStats,
) -> String {
    let files = prepare_files(bundle, root);
    body_section_ctx(&files, budget, omit, opts, stats, &mut Default::default())
}

#[allow(clippy::too_many_arguments)] // 文件 / 预算 / 省略清单 / 选项 / 统计 / 折叠区间各不同源
fn body_section_ctx(
    files: &[FileCtx<'_>],
    budget: usize,
    omit: &mut Vec<String>,
    opts: &PackOpts,
    stats: &mut PackStats,
    folded: &mut std::collections::BTreeMap<String, Vec<(u32, u32)>>,
) -> String {
    let mut out = String::new();
    stats.body = match opts.body {
        PackBody::Slice => "slice".to_string(),
        PackBody::Full => "full".to_string(),
    };
    for ctx in files {
        let (path, content, n_lines, diff) = (&ctx.fc.path, &ctx.content, ctx.n_lines, ctx.diff.as_str());
        stats.files += 1;
        let omit_before = omit.len();
        // 第一级：切片 / 全文
        let full_block = || format!("{path}:1-{n_lines}（全文，{n_lines} 行）\n{content}\n\n");
        let mut sliced: Option<SliceOut> = None;
        let (block, kind) = if opts.body == PackBody::Slice {
            if content.len() <= opts.slice_min_bytes {
                (full_block(), "small")
            } else {
                match slice_file(ctx, opts) {
                    Some(s) => {
                        let text = s.text.clone();
                        sliced = Some(s);
                        (text, "slice")
                    }
                    None => (full_block(), "full"),
                }
            }
        } else {
            (full_block(), "full")
        };
        if out.len() + block.len() <= budget {
            out.push_str(&block);
            // 折叠区间只在块**真的进了 pack** 时才记：没进去的块模型压根没看到，
            // 那属于「预算截断」不是「被折叠」，混为一谈会把回退信号读虚。
            if let Some(s) = &sliced {
                if !s.folded_ranges.is_empty() {
                    folded.entry(path.clone()).or_default().extend(s.folded_ranges.iter().copied());
                }
            }
            match kind {
                "slice" => {
                    stats.sliced += 1;
                    if let Some(s) = sliced {
                        stats.one_hop_defs += s.one_hop;
                        stats.folded_lines += s.folded_lines;
                    }
                }
                "small" => stats.small_full += 1,
                _ => stats.full += 1,
            }
            continue;
        }
        // 梯子：第一级装不下（不砍语义只降粒度）；坐标用重锚定后的 diff
        stats.ladder += 1;
        let Some(defs) = ctx.defs.as_deref() else {
            // 非支持语言超预算：先试 hunk±N 行窗（禁真空升级，Task 5）→ 装不下才 omit。
            if let Some(block) = hunk_window_block(path, content, diff, n_lines, opts.anchor_window) {
                if out.len() + block.len() <= budget {
                    out.push_str(&block);
                    continue;
                }
            }
            omit.push(format!("{path}:1-{n_lines} — 全文超预算（非支持语言无 def 回落）"));
            stats.omitted += 1;
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();
        let mut wrote_def = false; // 至少一个 def 体入了正文
        let mut omitted_def = false; // 至少一个 def 体超预算进了 omit 坐标
        for (start, end) in changed_def_ranges(diff, &lines, defs) {
            let end = end.min(lines.len().saturating_sub(1));
            let body = lines.get(start..=end).unwrap_or(&[]).join("\n");
            if body.is_empty() {
                continue;
            }
            let head = format!("{path}:{}-{}（def 体，全文超预算回落）", start + 1, end + 1);
            let block = format!("{head}\n{body}\n\n");
            if out.len() + block.len() > budget {
                omit.push(format!("{path}:{}-{} — 改动符号体", start + 1, end + 1));
                omitted_def = true;
            } else {
                out.push_str(&block);
                wrote_def = true;
            }
        }
        // 骨架档（回落链第 3 级插档，E8 裁决 D + Task 5，评审修 2 档位互斥）：本文件 def 体
        // **零入正文**（全部超预算或锚定失败）才触发；部分成功保持现状——已入 def + 溢出坐标，
        // 不叠加骨架（防同文件内容双份计预算）。溢出 def 的 omit 坐标保留——骨架只带签名+改动行，
        // 其余 def 体确实缺席，坐标仍真。骨架也装不下 → 落回现状消息（下方）。
        let mut wrote_skeleton = false;
        if !wrote_def {
            let skel = skeleton_of(content, diff, defs);
            let head = format!("{path}:1-{n_lines}（骨架：签名+改动行）");
            let block = format!("{head}\n{skel}\n");
            if out.len() + block.len() <= budget {
                out.push_str(&block);
                wrote_skeleton = true;
            }
        }
        // E8 裁决 D：降级空产出禁真空——def 体没入、骨架没入、连溢出坐标都没有（锚定失败）时
        // 必须 omit 可观测（T42 场景）。
        if !wrote_def && !wrote_skeleton && !omitted_def {
            omit.push(format!("{path}:1-{n_lines} — 全文超预算且 def 锚定失败（降级空产出）"));
        }
        if omit.len() > omit_before {
            stats.omitted += 1;
        }
    }
    if out.is_empty() {
        return String::new();
    }
    format!("## 改动源文（0 壳）\n{out}")
}

/// 骨架档（回落链第 3 级，E8 裁决 D + Task 5）：全文/def 体双超预算时的降级——逐行扫描，
/// 命中 def 正则的签名行全收（骨架地图）+ diff 全部改动行（`hunk_changed_lines`，原样保留，
/// 让改动本身可见——评审修 1：hunk_anchors 只给每 hunk 首行，多行 hunk 会丢内容）；
/// 其余连续行聚合折叠为 `…（N 行折叠）`。只产正文，不含块头（块头由
/// `body_section` 拼路径），故不判 budget——调用方按整块判。
/// ponytail: `changed_def_ranges` 命中的整个外围 def 体不整体保留（可能几百行，撑爆本就
/// 更紧的骨架预算，且骨架仅在 def 体已超预算时触发——含 def 全文必然同样超预算、自败）——
/// 只保信号密度最高的签名行 + 全部改动行，二者通常已覆盖诊断所需。
pub(crate) fn skeleton_of(content: &str, diff: &str, defs: &[(&'static str, Regex)]) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let anchors: std::collections::HashSet<usize> = hunk_changed_lines(diff).into_iter().collect();
    let mut body = String::new();
    let mut fold = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if anchors.contains(&i) || match_def(line, defs).is_some() {
            if fold > 0 {
                body.push_str(&format!("…（{fold} 行折叠）\n"));
                fold = 0;
            }
            body.push_str(line);
            body.push('\n');
        } else {
            fold += 1;
        }
    }
    if fold > 0 {
        body.push_str(&format!("…（{fold} 行折叠）\n"));
    }
    body
}

/// 非支持语言超预算兜底（回落链第 2 级插档，Task 5）：全文放不下、无 def 可回落时，从 diff
/// 各 hunk 头解析新文件起止行 `(c, d)`，各取 `[c−N, c+d+N]` 窗（N = `anchor_window`，批 E K16）、跨 hunk 合并
/// 成一个窗（单 cell diff 罕见多 hunk；合并窗简单可靠，偶尔多带无关行也由调用方 budget 判定兜底裁掉）。
/// 窗内容原样保留、不折叠——非支持语言无符号结构，折叠无意义。无可解析 hunk 头 → None。
fn hunk_window_block(path: &str, content: &str, diff: &str, n_lines: usize, win: usize) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut lo = usize::MAX;
    let mut hi = 0usize;
    for (c, d) in hunk_ranges(diff) {
        lo = lo.min(c.saturating_sub(win).max(1));
        hi = hi.max((c + d + win).min(n_lines));
    }
    if hi == 0 {
        return None;
    }
    let body = lines.get(lo.saturating_sub(1)..hi).unwrap_or(&[]).join("\n");
    if body.is_empty() {
        return None;
    }
    Some(format!("{path}:{lo}-{hi}（hunk±{win} 窗）\n{body}\n\n"))
}

// ---------------------------------------------------------------------------------------------
// 文件外片段（design §10.2 文件外层，批 E E-3 / K11 / K12）：仅 slice 档；caller 调用点 ±N + callee 签名 + doc
// ---------------------------------------------------------------------------------------------

/// 走仓文件数上限（超出停止并记 omit；bound 大仓耗时）。
const OUTSIDE_WALK_MAX_FILES: usize = 20_000;
/// 单文件上限（与 tools `NEED_READ_MAX_BYTES` 同口径）。
const OUTSIDE_FILE_MAX_BYTES: u64 = 512 * 1024;
/// 单文件有效命中上限（只计调用点 / 签名候选行，评审修 2）。
const OUTSIDE_FILE_MAX_HITS: usize = 200;
/// 搜调用点的改动 def 名上限：可调用 def 优先、按填充序（非字典序，评审修 1），超出记 omit 一行。
const OUTSIDE_CALLER_NAMES_MAX: usize = 64;
/// 搜签名的引用名上限（只限合成 regex 体量）：小写开头（函数 / 变量样）优先于大写开头（类型样，多为仓外 std 名），
/// 超出记 omit 一行（复审修 7）。
const OUTSIDE_SIG_NAMES_MAX: usize = 256;
/// 渲染的签名块上限。
const OUTSIDE_SIG_BLOCKS_MAX: usize = 40;
/// 每个改动 def 名最多取的调用点数（路径序前 K 处）。
const OUTSIDE_SITES_PER_NAME: usize = 3;
/// 段帽 = 组预算 / 此值（再受 0 壳之后的剩余预算封顶）。
const OUTSIDE_BUDGET_SHARE: usize = 8;
/// 帽低于此值不走仓（评审修 5：cap≈0 时整仓扫描只为把每块塞进省略清单）。
const OUTSIDE_MIN_CAP: usize = 256;
/// 省略清单里最多逐条列出的超帽块数，其余合并计数。
const OUTSIDE_SPILL_LIST_MAX: usize = 10;
/// 签名上方紧邻 doc / 注释最多带的行数。
const OUTSIDE_DOC_MAX_LINES: usize = 10;

/// 裸 alternation 命中后按标识符边界过滤（前后字符不是 `[A-Za-z0-9_$]`）：非消耗式，相邻两个名不会互吃边界，
/// 含 `$` 的 JS/TS 标识符也能命中（复审修 9）。
fn word_hits<'a>(re: &Regex, line: &'a str) -> Vec<&'a str> {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bytes = line.as_bytes();
    re.find_iter(line)
        .filter(|m| {
            let before = m.start().checked_sub(1).map(|i| bytes[i]);
            let after = bytes.get(m.end()).copied();
            !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
        })
        .map(|m| m.as_str())
        .collect()
}

/// 测试文件口径（逐字移植 `eval/risk_replay.py` `TEST_PATH`，批 E K12；M21b 风险规则同口径）。
fn test_path_re() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(_test\.go$|\.test\.(ts|js|tsx)$|(^|/)tests?/|(^|/)__tests__/|_spec\.)").expect("static test path regex"))
}

fn is_callable_kind(kind: &str) -> bool {
    matches!(kind, "fn" | "func" | "function" | "method")
}

/// 组内改动 def 名（搜调用点，可调用优先、填充序）与改动 def 体内引用、组内任何文件都未定义的名（搜签名）。
/// 测试代码不算：改动文件本身匹配 `TEST_PATH` 整文件跳过；Rust 落在 `#[cfg(test)]` 标注的 item / mod 块内的 def 不计
/// （`rust_test_ranges`，复审修 5）。坐标用重锚定后的 diff（复审修 2）。
struct OutsideNames {
    callers_of: Vec<String>,
    sig_of: Vec<String>,
    /// 名字帽截断数：只在真正走仓时写入省略清单（未搜索的组不报「截断」，合并后审计建议 3）。
    callers_truncated: usize,
    sigs_truncated: usize,
}

fn outside_names(files: &[FileCtx<'_>]) -> OutsideNames {
    let mut callers: Vec<(u8, String)> = Vec::new(); // (优先级, 名)，填充序
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    let mut group_defs: BTreeSet<String> = BTreeSet::new();
    for ctx in files {
        let Some(defs) = ctx.defs.as_deref() else { continue };
        if test_path_re().is_match(&ctx.fc.path) {
            continue;
        }
        let lines: Vec<&str> = ctx.content.lines().collect();
        let all = all_defs(&lines, defs);
        group_defs.extend(all.iter().map(|d| d.name.clone()));
        let test_ranges = if ctx.fc.path.ends_with(".rs") { rust_test_ranges(&lines) } else { Vec::new() };
        for c in changed_defs(&ctx.diff, &all) {
            if test_ranges.iter().any(|&(a, b)| a <= c.start && c.start <= b) {
                continue;
            }
            if ref_worthy(&c.name) && !callers.iter().any(|(_, n)| *n == c.name) {
                callers.push((if is_callable_kind(c.kind) { 0 } else { 1 }, c.name.clone()));
            }
            referenced.extend(body_names(&lines, &c));
        }
    }
    callers.sort_by_key(|(p, _)| *p); // 稳定：同优先级保持填充序
    let callers_truncated = callers.len().saturating_sub(OUTSIDE_CALLER_NAMES_MAX);
    callers.truncate(OUTSIDE_CALLER_NAMES_MAX);
    let callers_of: Vec<String> = callers.into_iter().map(|(_, n)| n).collect();
    let mut sig_of: Vec<String> = referenced.difference(&group_defs).filter(|n| !callers_of.contains(n)).cloned().collect();
    // 小写开头（函数 / 变量样）优先；同类保持字典序（BTreeSet 已排）
    sig_of.sort_by_key(|n| n.chars().next().is_some_and(|c| c.is_uppercase()));
    let sigs_truncated = sig_of.len().saturating_sub(OUTSIDE_SIG_NAMES_MAX);
    sig_of.truncate(OUTSIDE_SIG_NAMES_MAX);
    OutsideNames { callers_of, sig_of, callers_truncated, sigs_truncated }
}

/// 文件外片段段（仅 slice 档，K10）：一条合成 regex 走仓（gitignore 感知、不跟符号链接、隐藏目录跳过），跳过变更集文件 /
/// 测试文件 / 凭据类路径 / 非支持语言 / 超大文件；命中按 (path, line) 排序后每名 ≤ K 处调用点、每名首个签名；段字节 ≤ `cap`，
/// 超帽块进 omit。路径来自 walker 与 git diff、非模型可控，读守卫与 `search_repo` 工具同口径（不逐文件 `git ls-files`）；
/// denylist 只用凭据判据（扩展名 / `.env` / `id_rsa` / 工具目录），不用敏感词子串（`token_bucket.rs` 是源码，评审修 10）。
/// 名字边界用 `[^\w$]`（含 `$` 的 JS/TS 标识符 `items$` 在 `\b` 下永不命中，复审修 9）。
fn outside_section(
    files: &[FileCtx<'_>],
    root: &Path,
    opts: &PackOpts,
    changeset: &[String],
    cap: usize,
    omit: &mut Vec<String>,
    stats: &mut PackStats,
) -> String {
    outside_render(root, opts, changeset, cap, omit, stats, outside_names(files))
}

/// 名字集 → 走仓搜索与段渲染（组 pack [`outside_section`] 与 reviewer 区域爆炸半径〔批 B `pack::region`〕共用）：
/// 一条合成 regex 走仓（gitignore 感知、不跟符号链接、隐藏目录跳过），跳过变更集文件 /
/// 测试文件 / 凭据类路径 / 非支持语言 / 超大文件；命中按 (path, line) 排序后每名 ≤ K 处调用点、每名首个签名；段字节 ≤ `cap`，
/// 超帽块进 omit。路径来自 walker 与 git diff、非模型可控，读守卫与 `search_repo` 工具同口径（不逐文件 `git ls-files`）；
/// denylist 只用凭据判据（扩展名 / `.env` / `id_rsa` / 工具目录），不用敏感词子串（`token_bucket.rs` 是源码，评审修 10）。
/// 名字边界不用 `\b`（`$` 属 \W，`items$` 永不命中）也不用消耗式字符类（相邻两名会互吃边界）：先按裸 alternation 命中，
/// 再手工看前后字符是否标识符字节（`word_hits`，复审修 9）。
fn outside_render(
    root: &Path,
    opts: &PackOpts,
    changeset: &[String],
    cap: usize,
    omit: &mut Vec<String>,
    stats: &mut PackStats,
    names: OutsideNames,
) -> String {
    if names.callers_of.is_empty() && names.sig_of.is_empty() {
        return String::new();
    }
    if cap < OUTSIDE_MIN_CAP {
        omit.push(format!("文件外片段：预算不足（帽 {cap} 字节 < {OUTSIDE_MIN_CAP}），未搜索"));
        return String::new();
    }
    if names.callers_truncated > 0 {
        omit.push(format!("文件外调用点名字截断 +{}", names.callers_truncated));
    }
    if names.sigs_truncated > 0 {
        omit.push(format!("文件外签名名字截断 +{}", names.sigs_truncated));
    }
    let caller_set: std::collections::HashSet<&str> = names.callers_of.iter().map(String::as_str).collect();
    let sig_set: std::collections::HashSet<&str> = names.sig_of.iter().map(String::as_str).collect();
    // 边界不用 `\b`（`$` 属 \W，`items$` 永不命中）也不用消耗式 `[^\w$]`（相邻两个名会互吃边界）：
    // 先按裸 alternation 命中，再手工看前后字符是否标识符字节（复审修 9）。
    // 交替按长度降序（同长按字典序）：regex 交替最左优先，短名是长名前缀（`build_pack` / `build_packs`）时先命中短名、
    // 边界过滤否决后不回溯，长名调用点全部漏检（合并后审计必修）。最长分支被边界否决时任何更短前缀同样被否决，故长度降序即正确。
    let mut alts: Vec<&str> = names.callers_of.iter().chain(names.sig_of.iter()).map(String::as_str).collect();
    alts.sort_by(|a, b| b.len().cmp(&a.len()).then(a.cmp(b)));
    let alt = alts.iter().map(|s| regex::escape(s)).collect::<Vec<_>>().join("|");
    let pat = format!("(?:{alt})");
    let (Ok(re), Ok(matcher)) = (Regex::new(&pat), grep_regex::RegexMatcher::new(&pat)) else { return String::new() };

    let changeset: std::collections::HashSet<&str> = changeset.iter().map(String::as_str).collect();
    let mut defs_by_ext: std::collections::BTreeMap<String, Vec<(&'static str, Regex)>> = Default::default();
    // 一行的分类：定义行（名 ∈ sig_of）→ 签名候选；含 callers_of 名的非定义 / 非注释 / 非 import 行 → 调用点候选
    let classify = |line: &str, defs: &[(&'static str, Regex)]| -> (Vec<String>, Vec<String>) {
        let t = line.trim_start();
        if is_comment_line(t) || is_import_line(t) {
            return (Vec::new(), Vec::new());
        }
        let def_name = match_def(line, defs).map(|(_, n)| n);
        let (mut sig, mut site) = (Vec::new(), Vec::new());
        for name in word_hits(&re, line) {
            if def_name.as_deref() == Some(name) {
                if sig_set.contains(name) {
                    sig.push(name.to_string());
                }
            } else if caller_set.contains(name) {
                site.push(name.to_string());
            }
        }
        (sig, site)
    };
    // (rel, 1-based 行, 名)
    let mut sites: Vec<(String, usize, String)> = Vec::new();
    let mut sigs: Vec<(String, usize, String)> = Vec::new();
    let mut walked = 0usize;
    for dent in ignore::WalkBuilder::new(root).require_git(false).build() {
        let Ok(dent) = dent else { continue };
        if !dent.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
            continue;
        }
        walked += 1;
        if walked > OUTSIDE_WALK_MAX_FILES {
            omit.push(format!("文件外搜索截断（> {OUTSIDE_WALK_MAX_FILES} 文件）"));
            break;
        }
        let Ok(rel) = dent.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if changeset.contains(rel.as_str()) || test_path_re().is_match(&rel) || crate::cli::deny::is_credential_path(&rel) {
            continue;
        }
        let Some(lang) = lang_for(&rel) else { continue };
        if dent.metadata().map(|m| m.len() > OUTSIDE_FILE_MAX_BYTES).unwrap_or(true) {
            continue;
        }
        let ext = rel.rsplit('.').next().unwrap_or("").to_string();
        let defs = defs_by_ext.entry(ext).or_insert_with(|| compile_defs(lang));
        // sink 内直接分类并收集结果（不再回放第二遍）
        let mut local: Vec<(usize, Vec<String>, Vec<String>)> = Vec::new();
        let mut searcher = grep_searcher::SearcherBuilder::new().line_number(true).build();
        let _ = searcher.search_path(
            &matcher,
            dent.path(),
            grep_searcher::sinks::UTF8(|lnum, line| {
                let (sig, site) = classify(line.trim_end(), defs);
                if !sig.is_empty() || !site.is_empty() {
                    local.push((lnum as usize, sig, site));
                }
                Ok(local.len() < OUTSIDE_FILE_MAX_HITS)
            }),
        );
        for (lnum, sig, site) in local {
            sigs.extend(sig.into_iter().map(|n| (rel.clone(), lnum, n)));
            sites.extend(site.into_iter().map(|n| (rel.clone(), lnum, n)));
        }
    }
    sites.sort();
    sites.dedup();
    sigs.sort();
    sigs.dedup();
    // 每名 ≤ K 处调用点（路径序）；每名首个签名，签名块总数 ≤ 上限
    let mut per_name: std::collections::BTreeMap<&str, usize> = Default::default();
    let sites: Vec<&(String, usize, String)> = sites
        .iter()
        .filter(|(_, _, name)| {
            let c = per_name.entry(name.as_str()).or_insert(0);
            *c += 1;
            *c <= OUTSIDE_SITES_PER_NAME
        })
        .collect();
    let mut seen_sig: BTreeSet<&str> = BTreeSet::new();
    let sigs: Vec<&(String, usize, String)> =
        sigs.iter().filter(|(_, _, name)| seen_sig.insert(name.as_str())).take(OUTSIDE_SIG_BLOCKS_MAX).collect();
    // 读文件切窗（同文件窗合并、名集并）
    let mut file_cache: std::collections::BTreeMap<&str, Vec<String>> = Default::default();
    let read_lines = |rel: &str| -> Vec<String> {
        std::fs::read_to_string(root.join(rel)).unwrap_or_default().lines().map(str::to_string).collect()
    };
    // (块, 是否调用点块)
    let mut blocks: Vec<(String, bool)> = Vec::new();
    let mut by_file: std::collections::BTreeMap<&str, Vec<(usize, &str)>> = Default::default();
    for (rel, lnum, name) in &sites {
        by_file.entry(rel.as_str()).or_default().push((*lnum, name.as_str()));
    }
    let n_win = opts.anchor_window;
    for (rel, spans) in by_file {
        let lines = file_cache.entry(rel).or_insert_with(|| read_lines(rel));
        let n = lines.len();
        if n == 0 {
            continue;
        }
        let mut windows: Vec<(usize, usize, BTreeSet<&str>)> = Vec::new();
        for (l, name) in spans {
            let lo = (l - 1).saturating_sub(n_win);
            let hi = (l - 1 + n_win).min(n - 1);
            match windows.last_mut() {
                Some(w) if lo <= w.1 + 1 => {
                    w.1 = w.1.max(hi);
                    w.2.insert(name);
                }
                _ => windows.push((lo, hi, BTreeSet::from([name]))),
            }
        }
        for (lo, hi, ns) in windows {
            let body = lines[lo..=hi].join("\n");
            let names_s = ns.into_iter().collect::<Vec<_>>().join(", ");
            blocks.push((format!("{rel}:{}-{}（调用点：{names_s}）\n{body}\n\n", lo + 1, hi + 1), true));
        }
    }
    for (rel, lnum, name) in &sigs {
        let lines = file_cache.entry(rel.as_str()).or_insert_with(|| read_lines(rel));
        let n = lines.len();
        let l = lnum - 1;
        if l >= n {
            continue;
        }
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let d = DefRange { start: l, end: (l + SIG_MAX_LINES - 1).min(n - 1), name: name.clone(), kind: "" };
        let end = sig_end(&refs, &d);
        let mut start = l;
        while start > 0 && l - start < OUTSIDE_DOC_MAX_LINES && is_comment_line(refs[start - 1].trim_start()) {
            start -= 1;
        }
        blocks.push((format!("{rel}:{}-{}（签名：{name}）\n{}\n\n", start + 1, end + 1, refs[start..=end].join("\n")), false));
    }
    // 段帽：超帽块进 omit（坐标 + 类别 + 名；逐条列前 N 个，其余合并计数——省略清单自身不能无界）
    let mut text = String::new();
    let mut spilled = 0usize;
    for (block, is_site) in &blocks {
        if text.len() + block.len() > cap {
            spilled += 1;
            if spilled <= OUTSIDE_SPILL_LIST_MAX {
                let head = block.lines().next().unwrap_or("");
                let (coord, tag) = head.split_once('（').unwrap_or((head, ""));
                omit.push(format!("{coord} — 文件外片段超帽（{}）", tag.trim_end_matches('）')));
            }
            continue;
        }
        text.push_str(block);
        if *is_site {
            stats.outside_sites += 1;
        } else {
            stats.outside_sigs += 1;
        }
    }
    if spilled > OUTSIDE_SPILL_LIST_MAX {
        omit.push(format!("… 文件外片段超帽 +{}", spilled - OUTSIDE_SPILL_LIST_MAX));
    }
    if text.is_empty() {
        return String::new();
    }
    stats.outside_bytes = text.len();
    format!("## 文件外片段\n{text}")
}

/// 引擎入口：为一个 bundle 产 pack 正文（0 壳 + 文件外片段 + 省略清单）与形态统计，预算近→远填。
/// 不含 `<dossier>` 包裹（那是 prompt 层的活）。空 = 无 pack（降级）。
/// `own_diff_bytes`：本变更集 diff 的字节数（mandate——diff 是 prompt 恒渲染部分，永不砍，
/// 但占预算记账）：先从 budget 扣出预留，pack 各段再按剩余近→远填充。由调用方传入而非在此
/// 对 `bundle.files` 求和：二阶段补查传入的 augmented bundle 前插了 needs 文件，记账仍只按
/// 原始变更集 diff 算。
/// `changeset`：全变更集文件路径（文件外搜索排除；调用方给全部 bundle 的文件）。
pub fn pack_bundle(bundle: &Bundle, root: &Path, budget: usize, own_diff_bytes: usize, opts: &PackOpts, changeset: &[String]) -> Packed {
    let remaining = budget.saturating_sub(own_diff_bytes);
    let mut stats = PackStats { diff_bytes: own_diff_bytes, ..PackStats::default() };
    let mut omit: Vec<String> = Vec::new();
    let files = prepare_files(bundle, root);

    // 0 壳：改动源文（切片 / 全文 + 梯子；overflow → omit）
    let mut folded: std::collections::BTreeMap<String, Vec<(u32, u32)>> = std::collections::BTreeMap::new();
    let bodies = body_section_ctx(&files, remaining, &mut omit, opts, &mut stats, &mut folded);
    // 文件外片段（仅 slice 档）：帽 = min(0 壳后剩余, 组预算 / 8)
    let outside_cap = remaining.saturating_sub(bodies.len()).min(budget / OUTSIDE_BUDGET_SHARE);
    let outside = if opts.body == PackBody::Slice {
        outside_section(&files, root, opts, changeset, outside_cap, &mut omit, &mut stats)
    } else {
        String::new()
    };
    // L4-B 三方契约（M21d 批 A，design §9.4 B）：**与文件外片段共享同一份帽**，且排在它之后拿剩下的——
    // 仓内的调用点与签名更贴近本次改动，三方契约不得把它们挤掉。
    // 非 Cargo 工作区 / Cargo.lock 缺失 / registry 没解包 → 安静降级，只把原因记进 omit 与统计（roadmap 风险节）。
    let contracts = if opts.body == PackBody::Slice {
        let cap = outside_cap.saturating_sub(outside.len());
        // 先判「这个仓有没有可能注入」：非 Cargo 工作区 / 没有 Rust 源文的组，谈不上「预算不足」——
        // 报了只会在零 Rust 仓（PR181 那类）刷出误导性的「N group(s) skipped injection」
        // （2026-09-29 `/code-review` 抓到；与 `outside_render` 的空名集短路同构）。
        let injectable = root.join("Cargo.lock").exists() && files.iter().any(|c| c.fc.path.ends_with(".rs"));
        if !injectable {
            String::new()
        } else if cap < OUTSIDE_MIN_CAP {
            omit.push(format!("三方契约：预算不足（帽 {cap} 字节 < {OUTSIDE_MIN_CAP}），未注入"));
            stats.contract_budget_skipped = true;
            String::new()
        } else {
            let sources: Vec<(&str, &str)> = files.iter().map(|c| (c.fc.path.as_str(), c.content.as_str())).collect();
            let sec = contracts::contracts_section(root, &sources, &contracts::local_module_names(root), cap);
            stats.contracts = sec.blocks;
            stats.contract_misses = sec.misses.len();
            stats.contract_spilled = sec.spilled;
            for miss in sec.misses.iter().take(OUTSIDE_SPILL_LIST_MAX) {
                omit.push(match miss {
                    contracts::LocateMiss::NotLocked(k) => format!("{k} — 三方契约未注入（Cargo.lock 里没有这个包）"),
                    contracts::LocateMiss::NotVendored { krate, version } => {
                        format!("{krate} {version} — 三方契约未注入（registry 无解包目录，`cargo fetch` 后重跑）")
                    }
                    contracts::LocateMiss::WalkTruncated { krate, scanned } => {
                        format!("{krate} — 源码超过 {scanned} 个文件，后面的没搜（三方契约可能缺块）")
                    }
                    contracts::LocateMiss::AmbiguousVersion { krate, versions } => {
                        format!("{krate} — 三方契约未注入（Cargo.lock 里同名多版本：{}，定不出该用哪个）", versions.join(" / "))
                    }
                    contracts::LocateMiss::HitCapReached { krate, cap } => {
                        format!("{krate} — 命中数触到上限 {cap}，后面的名字没取（三方契约可能缺块）")
                    }
                });
            }
            if sec.spilled > 0 {
                omit.push(format!("… 三方契约超帽 +{}", sec.spilled));
            }
            if sec.text.is_empty() {
                String::new()
            } else {
                format!("## 三方契约（依赖库签名 + doc）\n{}", sec.text)
            }
        }
    } else {
        String::new()
    };

    // 组装：非空段拼接 + 省略清单（封顶 + 计数兜底）
    let mut sections: Vec<String> = Vec::new();
    if !bodies.is_empty() {
        sections.push(bodies);
    }
    if !outside.is_empty() {
        sections.push(outside);
    }
    if !contracts.is_empty() {
        sections.push(contracts);
    }
    if !omit.is_empty() {
        let total = omit.len();
        omit.truncate(OMIT_MAX_ENTRIES);
        let mut sec = String::from("## 已省略（预算裁剪）\n");
        for line in &omit {
            sec.push_str(line);
            sec.push('\n');
        }
        if total > OMIT_MAX_ENTRIES {
            sec.push_str(&format!("… +{} more omitted\n", total - OMIT_MAX_ENTRIES));
        }
        sections.push(sec);
    }
    Packed { text: sections.join("\n"), stats, folded }
}

/// 只要正文的薄包装（二阶段补查与测试用）。
pub fn build_pack(bundle: &Bundle, root: &Path, budget: usize, own_diff_bytes: usize, opts: &PackOpts, changeset: &[String]) -> String {
    pack_bundle(bundle, root, budget, own_diff_bytes, opts, changeset).text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_section_renders_full_file() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "fn helper() {}\n\nfn caller() {\n    helper();\n    let z = 2;\n}\n").unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -4,0 +5,1 @@\n+    let z = 2;\n".into(),
        }]);
        let mut omit = Vec::new();
        let s = body_section(&b, root, 1_000_000, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(s.contains("## 改动源文（0 壳）"), "含 0 壳标题: {s}");
        assert!(s.contains("fn caller() {") && s.contains("let z = 2;"), "含 caller 完整体");
        assert!(s.contains("fn helper"), "全文含未改动的 helper（粒度对齐模型文件级消费，E7）");
        assert!(omit.is_empty(), "预算充足无省略");
    }

    #[test]
    fn body_section_gives_full_file_within_budget() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "use std::fmt;\n\nfn helper() {}\n\nfn caller() {\n    helper();\n    let z = 2;\n}\n").unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -6,0 +6,1 @@\n+    let z = 2;\n".into(),
        }]);
        let mut omit = Vec::new();
        let s = body_section(&b, root, 1_000_000, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(s.contains("## 改动源文（0 壳）"), "新段标题: {s}");
        assert!(s.contains("src/a.rs:1-8（全文，8 行）"), "全文块头带行数: {s}");
        // 粒度对齐（E7）：全文包含改动 def 之外的内容——imports 与相邻函数
        assert!(s.contains("use std::fmt;") && s.contains("fn helper() {}"), "含 imports+相邻函数");
        assert!(omit.is_empty(), "预算充足无省略");
    }

    #[test]
    fn body_section_falls_back_to_def_body_when_file_exceeds_budget() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 大文件：filler 函数撑体积 + 一个小的改动函数
        let filler = format!("fn filler() {{\n{}}}\n", "    let x = 0;\n".repeat(200));
        std::fs::write(root.join("src/a.rs"),
            format!("{filler}\nfn caller() {{\n    work();\n}}\n")).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -204,0 +204,1 @@\n+    work();\n".into(),
        }]);
        // 预算 500B：全文（~3.4KB）放不下 → 回落 caller def 体（~30B）放得下
        let mut omit = Vec::new();
        let s = body_section(&b, root, 500, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(s.contains("def 体，全文超预算回落"), "回落标注: {s}");
        assert!(s.contains("fn caller() {") && s.contains("work();"), "回落仍给改动 def 体");
        assert!(!s.contains("fn filler"), "回落不含无关大函数");
    }

    #[test]
    fn body_section_gives_full_file_for_unsupported_lang() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("notes.md"), "# 标题\n\n改动的文档内容。\n").unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "notes.md".into(), diff: "@@ -3,0 +3,1 @@\n+改动的文档内容。\n".into(),
        }]);
        let mut omit = Vec::new();
        let s = body_section(&b, root, 1_000_000, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(s.contains("改动的文档内容。"), "非支持语言也给全文（E7：templates 格整读实证）: {s}");
        // 超预算但装得下 hunk±30 窗（Task 5 骨架档兜底升级：非支持语言不再直接 omit）→ 窗内容替代 omit
        let mut omit2 = Vec::new();
        let s2 = body_section(&b, root, 68, &mut omit2, &PackOpts::full(), &mut PackStats::default());
        assert!(s2.contains("改动的文档内容。") && s2.contains("hunk±30 窗"), "窗兜底取代 omit: {s2}");
        assert!(omit2.is_empty(), "窗已装下，无需 omit: {omit2:?}");
        // 连窗都装不下才落回现状消息（禁真空兜底不能丢）
        let mut omit3 = Vec::new();
        let s3 = body_section(&b, root, 10, &mut omit3, &PackOpts::full(), &mut PackStats::default());
        assert!(s3.is_empty() && omit3.len() == 1 && omit3[0].contains("notes.md"), "窗也装不下才进 omit: {omit3:?}");
    }

    #[test]
    fn body_section_falls_to_skeleton_when_def_body_too_big() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 改动 def 本身巨大（def 体回落也超预算）+ 另有若干小 fn 签名
        let big_body = "    let x = 0;\n".repeat(300);
        std::fs::write(root.join("src/a.rs"),
            format!("fn tiny_one() {{}}\nfn changed_big() {{\n{big_body}}}\nfn tiny_two() {{}}\n")).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -3,0 +3,1 @@\n+    let x = 0;\n".into(),
        }]);
        let mut omit = Vec::new();
        // 预算 800B：全文（~4.5KB）超、def 体（~4.2KB）超 → 骨架（3 签名行 + 折叠标记）应放得下
        let s = body_section(&b, root, 800, &mut omit, &PackOpts::full(), &mut PackStats::default());
        // 块头对齐真实语义（评审修 3）：签名+改动行（非改动 def 全文——那会自败超预算）
        assert!(s.contains("（骨架：签名+改动行）"), "骨架档启用: {s}");
        assert!(s.contains("fn tiny_one()") && s.contains("fn tiny_two()"), "签名行全收");
        assert!(s.contains("折叠"), "折叠标记在场");
    }

    #[test]
    fn skeleton_keeps_all_changed_lines_of_multiline_hunk() {
        // 评审修 1：hunk_anchors 只给每 hunk 首个改动行，多行 hunk 其余改动行被折叠吞掉。
        // 骨架必须保留 hunk 的全部改动行（hunk_changed_lines）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let filler = "    let f = 0;\n".repeat(300);
        let changed = "    let c1 = 1;\n    let c2 = 2;\n    let c3 = 3;\n    let c4 = 4;\n    let c5 = 5;\n";
        std::fs::write(root.join("src/a.rs"),
            format!("fn big_changed() {{\n{filler}{changed}}}\n")).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            // 新文件 302-306 行 = c1..c5（一个 5 行 hunk）
            diff: format!("@@ -301,0 +302,5 @@\n{}", changed.lines().map(|l| format!("+{l}\n")).collect::<String>()),
        }]);
        let mut omit = Vec::new();
        // 全文/def 体（~4.6KB）双超 800B → 骨架；5 个改动行必须全在
        let s = body_section(&b, root, 800, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(s.contains("骨架"), "骨架档启用: {s}");
        for c in ["let c1", "let c2", "let c3", "let c4", "let c5"] {
            assert!(s.contains(c), "多行 hunk 改动行全保留，缺 {c}: {s}");
        }
    }

    #[test]
    fn skeleton_not_stacked_when_some_def_bodies_fit() {
        // 评审修 2：档位互斥——部分 def 体已入正文时不再叠加骨架（防同文件内容双份计预算）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let big_body = "    let x = 0;\n".repeat(300);
        std::fs::write(root.join("src/a.rs"),
            format!("fn small_changed() {{\n    let a = 1;\n}}\nfn big_changed() {{\n{big_body}}}\n")).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            // 两 hunk：一个落 small 体内、一个落 big 体内
            diff: "@@ -2,0 +2,1 @@\n+    let a = 1;\n@@ -5,0 +5,1 @@\n+    let x = 0;\n".into(),
        }]);
        let mut omit = Vec::new();
        // 800B：全文超、small def 体（~100B）入正文、big def 体（~4.2KB）超 → 溢出坐标，无骨架
        let s = body_section(&b, root, 800, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(!s.contains("骨架"), "部分成功不叠加骨架: {s}");
        assert!(s.contains("fn small_changed") && s.contains("let a = 1;"), "已入 def 体保留: {s}");
        assert!(omit.iter().any(|o| o.contains("改动符号体")), "溢出 def 进 omit 坐标: {omit:?}");
    }

    #[test]
    fn body_section_unsupported_lang_falls_to_hunk_window() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        std::fs::write(root.join("notes.md"), &body).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "notes.md".into(),
            diff: "@@ -100,0 +100,1 @@\n+line 100\n".into(),
        }]);
        let mut omit = Vec::new();
        let s = body_section(&b, root, 900, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(s.contains("line 100") && s.contains("line 85"), "hunk±30 窗在场: {s}");
        assert!(!s.contains("line 10\n"), "窗外内容不在");
    }

    #[test]
    fn body_section_budget_cut_body_goes_to_omit() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn caller() {\n    work();\n}\n").unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(), diff: "@@ -2,0 +2,1 @@\n+    work();\n".into(),
        }]);
        // 预算极小 → body 放不下 → 进 omit 坐标、正文空
        let mut omit = Vec::new();
        let s = body_section(&b, root, 5, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert_eq!(s, "", "预算耗尽正文空");
        assert_eq!(omit.len(), 1, "body 降级为坐标");
        assert!(omit[0].contains("src/a.rs:"), "带坐标: {omit:?}");
    }

    #[test]
    fn body_section_empty_when_file_missing_from_worktree() {
        // 文件不存在于工作树（如已删除的文件仍在 diff 里）→ content 空 → 静默跳过，
        // 不进正文也不进 omit（评审修：原名 …_no_supported_lang，实测的一直是本场景）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/gone.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn f() {}\n".into(),
        }]);
        let mut omit = Vec::new();
        assert_eq!(body_section(&b, tmp.path(), 1_000_000, &mut omit, &PackOpts::full(), &mut PackStats::default()), "", "文件缺失 → 空");
        assert!(omit.is_empty(), "文件缺失不进 omit: {omit:?}");
    }

    #[test]
    fn body_section_degrade_empty_output_goes_to_omit() {
        // E8 裁决 D：全文超预算且 def 锚不到（如 hunk 全在 mod tests，T42 场景）→ 禁真空。
        // Task 5 骨架档插档后：即使无签名行匹配，骨架仍能用折叠标记+diff 改动行兜底承接，
        // 不再需要走到最终 omit 消息——骨架本身就是「禁真空」更具体的实现（比空坐标更有信号）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 大文件（超 500B 预算）+ diff 行落在无 def 可锚处（文件头注释区）
        let filler = format!("// header\n{}", "// filler line\n".repeat(200));
        std::fs::write(root.join("src/a.rs"), &filler).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -1,0 +2,1 @@\n+// filler line\n".into(),
        }]);
        let mut omit = Vec::new();
        let s = body_section(&b, root, 500, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(s.contains("src/a.rs") && s.contains("骨架"), "骨架档兜底承接：无签名行也不真空: {s}");
        assert!(omit.is_empty(), "骨架已承接，不再需要 omit 坐标: {omit:?}");
    }

    #[test]
    fn body_section_fills_hub_first() {
        // M21a 批 C K7：填充序 = hub_order（枢纽保全文）——预算只够一个文件全文时，枢纽先得。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let a_src = "fn a() {}\n";
        let hub_src = "pub fn hub_fn() {}\n";
        std::fs::write(root.join("src/a.rs"), a_src).unwrap();
        std::fs::write(root.join("src/hub.rs"), hub_src).unwrap();
        let files = vec![
            FileChange { path: "src/a.rs".into(), diff: "@@ -1,0 +1,1 @@\n+fn a() {}\n".into() },
            FileChange { path: "src/hub.rs".into(), diff: "@@ -1,0 +1,1 @@\n+pub fn hub_fn() {}\n".into() },
        ];
        let hub_block = format!("src/hub.rs:1-1（全文，1 行）\n{hub_src}\n\n");
        let budget = hub_block.len() + 2; // 装得下 hub 全文（或 a 全文），装不下两个
        let mut omit = Vec::new();
        let hub_first = Bundle { files: files.clone(), edges: vec![], hub_order: vec![1, 0], over_budget: false };
        let out = body_section(&hub_first, root, budget, &mut omit, &PackOpts::full(), &mut PackStats::default());
        assert!(out.contains("src/hub.rs:1-1（全文"), "枢纽先填得全文: {out}");
        assert!(!out.contains("src/a.rs:1-1（全文"), "非枢纽回落: {out}");
        let natural = Bundle::from_files(files);
        let out = body_section(&natural, root, budget, &mut Vec::new(), &PackOpts::full(), &mut PackStats::default());
        assert!(out.contains("src/a.rs:1-1（全文") && !out.contains("src/hub.rs:1-1（全文"), "自然序对照: {out}");
    }

    fn bundle_one(path: &str, diff: &str) -> crate::bundle::Bundle {
        crate::bundle::Bundle::from_files(vec![crate::evidence::git::FileChange {
            path: path.into(), diff: diff.into(),
        }])
    }

    #[test]
    fn build_pack_end_to_end_has_bodies_and_diff_reserved() {
        // E8-E 安乐死后：build_pack 唯一内容 = 0 壳全文（无 1 壳/callee 段）。
        // 全文含改动 target 之外的 u1/u2/u3（粒度对齐模型文件级消费，E7），非靠引用点引擎。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "pub fn target() {\n    work();\n}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\n").unwrap();
        let b = bundle_one("src/a.rs", "@@ -1,0 +1,3 @@\n+pub fn target() {\n+    work();\n+}\n");
        let own = b.files.iter().map(|fc| fc.diff.len()).sum();
        let pack = build_pack(&b, root, 1_000_000, own, &PackOpts::full(), &[]);
        assert!(pack.contains("## 改动源文（0 壳）"), "有 0 壳: {pack}");
        assert!(pack.contains("pub fn target() {"), "0 壳含 target 完整体");
        assert!(pack.contains("u1()"), "0 壳全文含 caller（非 1 壳引用点，1 壳已拆除）");
        assert!(!pack.contains("## 引用点（1 壳）"), "1 壳引用点段已随 E8-E 拆除: {pack}");
    }

    #[test]
    fn build_pack_tiny_budget_spills_to_omission_list_with_coords() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"),
            "pub fn target() {}\nfn u1() { target(); }\nfn u2() { target(); }\nfn u3() { target(); }\n").unwrap();
        let b = bundle_one("src/a.rs", "@@ -1,0 +1,1 @@\n+pub fn target() {}\n");
        // 预算极小：引用点填不下 → 省略清单带坐标（无工具默认下不指挥不存在的工具）
        let own = b.files.iter().map(|fc| fc.diff.len()).sum();
        let pack = build_pack(&b, root, 60, own, &PackOpts::full(), &[]);
        assert!(pack.contains("## 已省略（预算裁剪）"), "有省略段: {pack}");
        assert!(!pack.contains("read_files"), "无工具默认下省略段不得指挥不存在的工具");
        assert!(pack.contains("src/a.rs:"), "省略项带 file:line 坐标");
    }

    #[test]
    fn build_pack_full_file_for_unsupported_lang() {
        // 语义翻转（E7）：非支持语言曾经全跳过、pack 空；0 壳升级整文件后即使无 def 可抽，
        // 全文仍在预算内 → pack 非空、含改动源文全文（brief 原断言基于「app.py 未写盘故 pack 空」
        // 的旧读法——此处补写盘以实测真实的「非支持语言给全文」行为，见 task-1 报告）。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("app.py"), "def f(): pass\n").unwrap();
        let b = bundle_one("app.py", "@@ -1,0 +1,1 @@\n+def f(): pass\n");
        let pack = build_pack(&b, root, 1_000_000, 0, &PackOpts::full(), &[]);
        assert!(pack.contains("## 改动源文（0 壳）"), "非支持语言也走 0 壳: {pack}");
        assert!(pack.contains("def f(): pass"), "含全文: {pack}");
    }

    // ---- 批 E E-2：切片配方 ----

    /// 合成 > 8K 的 Rust 文件：模块 doc + use + helper_one（callee）/ changed_target / caller_of_target（caller）/ unrelated_big（600 行体）。
    /// 行索引（0-based）：helper_one 3-6、changed_target 8-11、caller_of_target 13-15、unrelated_big 17-619（n = 620）。
    fn big_rust_file() -> String {
        let mut s = String::from("//! module doc line\nuse std::fmt;\n\n");
        s += "fn helper_one() -> u8 {\n    let h = 1;\n    h + 1\n}\n\n";
        s += "fn changed_target(x: u8) -> u8 {\n    let t = helper_one();\n    t + x\n}\n\n";
        s += "fn caller_of_target() -> u8 {\n    changed_target(2)\n}\n\n";
        s += "fn unrelated_big() -> u8 {\n";
        for i in 0..600 {
            s += &format!("    let u{i} = 7;\n");
        }
        s += "    0\n}\n";
        assert!(s.len() > 8192);
        s
    }

    fn plus_lines(diff: &str) -> Vec<String> {
        diff.lines().filter(|l| l.starts_with('+') && !l.starts_with("+++")).map(|l| l[1..].to_string()).collect()
    }

    fn slice_opts() -> PackOpts { PackOpts::default() }

    #[test]
    fn slice_expands_core_and_one_hop_folds_unrelated_keeps_map() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), big_rust_file()).unwrap();
        let b = Bundle::from_files(vec![FileChange { path: "src/a.rs".into(), diff: "@@ -11,1 +11,1 @@\n+    t + x\n".into() }]);
        let mut omit = Vec::new();
        let mut stats = PackStats::default();
        let s = body_section(&b, root, 1_000_000, &mut omit, &slice_opts(), &mut stats);
        assert!(s.contains("src/a.rs:1-620（切片：核 1 · 一跳 2 · 折叠 1 段）"), "块头: {s}");
        assert!(s.contains("fn changed_target(x: u8) -> u8 {\n    let t = helper_one();\n    t + x\n}"), "核全文: {s}");
        assert!(s.contains("fn helper_one() -> u8 {\n    let h = 1;"), "callee 全文: {s}");
        assert!(s.contains("fn caller_of_target() -> u8 {\n    changed_target(2)"), "caller 全文: {s}");
        assert!(s.contains("//! module doc line\nuse std::fmt;\n"), "文件头保留: {s}");
        assert!(s.contains("fn unrelated_big() -> u8 {\n…（19-620 行折叠：unrelated_big）\n"), "无关函数只留签名 + 带范围与名的折叠标记: {s}");
        assert!(!s.contains("let u7 = 7;"), "无关函数体不在: {s}");
        assert!(omit.is_empty());
        assert_eq!((stats.body.as_str(), stats.files, stats.sliced, stats.small_full, stats.full, stats.ladder), ("slice", 1, 1, 0, 0, 0));
        assert_eq!((stats.one_hop_defs, stats.folded_lines), (2, 602));
    }

    #[test]
    fn slice_no_vacuum_property_rs_go_ts_md() {
        // 禁真空性质（K7）：四语言、多 hunk（文件头 use 区 / 无关大函数体内 / 横跨两 def / 纯删除）——diff 每个 + 行逐字在输出。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), big_rust_file()).unwrap();
        let rs_diff = "@@ -2,1 +2,1 @@\n+use std::fmt;\n@@ -12,3 +12,3 @@\n+}\n+\n+fn caller_of_target() -> u8 {\n@@ -16,1 +15,0 @@\n-removed line\n@@ -101,1 +101,1 @@\n+    let u82 = 7;\n";

        let mut go = String::from("package main\n\nimport \"fmt\"\n\nfunc helperOne() int {\n\treturn 1\n}\n\nfunc changedTarget(x int) int {\n\treturn helperOne() + x\n}\n\nfunc unrelatedBig() int {\n");
        for i in 0..600 { go += &format!("\tunused_{i} := {i}\n"); }
        go += "\treturn 0\n}\n";
        assert!(go.len() > 8192);
        std::fs::write(root.join("src/b.go"), &go).unwrap();
        let go_diff = "@@ -10,1 +10,1 @@\n+\treturn helperOne() + x\n";

        let mut ts = String::from("import { x } from './x';\n\nexport class Widget {\n  helperOne(): number {\n    return 1;\n  }\n  changedTarget(): number {\n    return this.helperOne();\n  }\n  unrelatedBig(): number {\n");
        for i in 0..600 { ts += &format!("    const u{i} = 7;\n"); }
        ts += "    return 0;\n  }\n}\n";
        std::fs::write(root.join("src/c.ts"), &ts).unwrap();
        let ts_diff = "@@ -8,1 +8,1 @@\n+    return this.helperOne();\n";

        let md: String = (1..=1200).map(|i| format!("line {i}\n")).collect();
        std::fs::write(root.join("notes.md"), &md).unwrap();
        let md_diff = "@@ -600,1 +600,1 @@\n+line 600\n";

        let files = vec![
            FileChange { path: "notes.md".into(), diff: md_diff.into() },
            FileChange { path: "src/a.rs".into(), diff: rs_diff.into() },
            FileChange { path: "src/b.go".into(), diff: go_diff.into() },
            FileChange { path: "src/c.ts".into(), diff: ts_diff.into() },
        ];
        let b = Bundle::from_files(files.clone());
        let mut stats = PackStats::default();
        let s = body_section(&b, root, 1_000_000, &mut Vec::new(), &slice_opts(), &mut stats);
        assert_eq!(stats.sliced, 4, "四文件全走切片: {stats:?}");
        for fc in &files {
            for l in plus_lines(&fc.diff) {
                assert!(s.contains(&format!("{l}\n")), "改动行必须在切片里（{}）: {l:?}", fc.path);
            }
        }
        // 横跨两 def 的 hunk：两个 def 都展开；删除点所在行有窗
        assert!(s.contains("fn caller_of_target() -> u8 {\n    changed_target(2)\n}"), "{s}");
        // 无关体只在被改的 hunk 附近展开，其余折叠
        assert!(s.contains("行折叠：unrelated_big）") || s.contains("let u82 = 7;"), "{s}");
        assert!(!s.contains("unused_7 := 7\n") && s.contains("import \"fmt\""), "go 无关体折叠、import 保留: {s}");
        assert!(!s.contains("const u7 = 7;") && s.contains("import { x } from './x';"), "ts 无关体折叠、import 保留: {s}");
        assert!(s.contains("notes.md:1-1200（切片：核 0 · 一跳 0 · 折叠 2 段）"), "非支持语言 = hunk 窗 + 折叠: {s}");
        assert!(s.contains("…（1-569 行折叠）\nline 570\n") && s.contains("line 630\n…（631-1200 行折叠）\n"), "{s}");
        assert!(!s.contains("line 300\n"), "窗外折叠");
    }

    #[test]
    fn slice_small_file_equals_full_bytewise() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), "fn helper() {}\n\nfn caller() {\n    helper();\n    let z = 2;\n}\n").unwrap();
        let b = Bundle::from_files(vec![FileChange { path: "src/a.rs".into(), diff: "@@ -4,0 +5,1 @@\n+    let z = 2;\n".into() }]);
        let mut st_s = PackStats::default();
        let mut st_f = PackStats::default();
        let sl = body_section(&b, root, 1_000_000, &mut Vec::new(), &slice_opts(), &mut st_s);
        let fu = body_section(&b, root, 1_000_000, &mut Vec::new(), &PackOpts::full(), &mut st_f);
        assert_eq!(sl, fu, "≤ slice_min_bytes 逐字节相同（K8）");
        assert_eq!((st_s.small_full, st_s.sliced, st_f.full), (1, 0, 1));
        // 阈值 0：小文件也切
        let opts = PackOpts { slice_min_bytes: 0, ..PackOpts::default() };
        let cut = body_section(&b, root, 1_000_000, &mut Vec::new(), &opts, &mut PackStats::default());
        // `helper` 6 字符纯小写被 ref_worthy 过滤 → 一跳 0，但签名行仍在地图里
        assert!(cut.contains("切片：核 1 · 一跳 0") && cut.contains("fn helper() {}"), "{cut}");
    }

    #[test]
    fn slice_ladder_takes_over_when_over_budget() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.rs"), big_rust_file()).unwrap();
        let b = Bundle::from_files(vec![FileChange { path: "src/a.rs".into(), diff: "@@ -11,1 +11,1 @@\n+    t + x\n".into() }]);
        let huge = body_section(&b, root, 1_000_000, &mut Vec::new(), &slice_opts(), &mut PackStats::default());
        let slice_len = huge.len() - "## 改动源文（0 壳）\n".len();
        let mut omit = Vec::new();
        let mut stats = PackStats::default();
        let s = body_section(&b, root, slice_len - 1, &mut omit, &slice_opts(), &mut stats);
        assert!(s.contains("def 体，全文超预算回落") && s.contains("let t = helper_one();"), "切片装不下 → def 体（K9）: {s}");
        assert!(!s.contains("切片："), "{s}");
        assert_eq!((stats.sliced, stats.ladder, stats.omitted), (0, 1, 0), "{stats:?}");
    }

    #[test]
    fn full_mode_output_unchanged_by_slice() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let content = big_rust_file();
        std::fs::write(root.join("src/a.rs"), &content).unwrap();
        let b = Bundle::from_files(vec![FileChange { path: "src/a.rs".into(), diff: "@@ -11,1 +11,1 @@\n+    t + x\n".into() }]);
        let mut stats = PackStats::default();
        let s = body_section(&b, root, 1_000_000, &mut Vec::new(), &PackOpts::full(), &mut stats);
        assert_eq!(s, format!("## 改动源文（0 壳）\nsrc/a.rs:1-620（全文，620 行）\n{content}\n\n"), "full 档 = 现状整文件（K10）");
        assert_eq!((stats.body.as_str(), stats.full, stats.sliced), ("full", 1, 0));
    }

    #[test]
    fn pack_opts_from_cfg_and_defaults() {
        let d = PackOpts::default();
        assert_eq!((d.body, d.anchor_window, d.slice_min_bytes), (PackBody::Slice, 30, 8192), "= config 缺省〔D8〕");
        let f = PackOpts::full();
        assert_eq!((f.body, f.anchor_window), (PackBody::Full, 30));
        let cfg = crate::config::Config::from_json(r#"{"review":{"pack":{"body":"full","anchorWindow":5,"sliceMinBytes":1}}}"#).unwrap();
        let o = PackOpts::from(&cfg.review.pack);
        assert_eq!((o.body, o.anchor_window, o.slice_min_bytes), (PackBody::Full, 5, 1));
    }

    // ---- 批 E E-3：文件外片段 ----

    /// 跨层接线（A-3）：`pack_bundle` → `contracts_section` → **本机真实 registry**。
    /// M21c 批 A 连续两次栽在「各自绿、接起来不通」，所以这条不用假 registry：临时仓复制本仓
    /// `Cargo.lock`（版本与本机解包目录天然一致），源文按路径调 `serde_json`。registry 缺失时跳过。
    #[test]
    fn contracts_reach_pack_text_through_the_real_chain() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let Some(reg) = contracts::registry_root() else { return };
        if !reg.exists() {
            return; // 没 registry 的机器：降级路径由下一条用例覆盖
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"), root.join("Cargo.lock")).unwrap();
        let body = "use serde_json::Value;\n\npub fn decode(s: &str) -> Value {\n    let v: Value = serde_json::from_str(s).unwrap();\n    v\n}\n";
        std::fs::write(root.join("src/a.rs"), body).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -1,1 +1,1 @@\n+    let v: Value = serde_json::from_str(s).unwrap();\n".into(),
        }]);

        let p = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &["src/a.rs".to_string()]);

        assert!(p.stats.contracts > 0, "真实链路上一块契约都没注入: {}", p.text);
        assert!(p.text.contains("## 三方契约（依赖库签名 + doc）"), "{}", p.text);
        assert!(p.text.contains("pub fn from_str"), "调的是 serde_json::from_str，契约就该是它: {}", p.text);
        let ver = crate::pack::contracts::locked_versions(&std::fs::read_to_string(root.join("Cargo.lock")).unwrap());
        let ver = ver.get("serde_json").cloned().unwrap();
        assert!(p.text.contains(&format!("serde_json {ver}")), "坐标要带**锁定版本**（L4-B 的理由就在版本上）: {}", p.text);
    }

    /// 降级（A-3 / roadmap 风险节）：不是 Cargo 工作区 → 无契约段、统计为 0、其余段照旧，不阻塞评审。
    #[test]
    fn missing_cargo_lock_degrades_without_blocking_the_pack() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let body = "use serde_json::Value;\npub fn decode(s: &str) -> Value { serde_json::from_str(s).unwrap() }\n";
        std::fs::write(root.join("src/a.rs"), body).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -1,1 +1,1 @@\n+pub fn decode(s: &str) -> Value { serde_json::from_str(s).unwrap() }\n".into(),
        }]);

        let p = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &["src/a.rs".to_string()]);

        assert_eq!((p.stats.contracts, p.stats.contract_misses, p.stats.contract_spilled), (0, 0, 0));
        assert!(!p.text.contains("## 三方契约"), "{}", p.text);
        assert!(p.text.contains("pub fn decode"), "正文照旧: {}", p.text);
    }

    /// 交付项 7 的切分键要**在真实评审的报告里读得到**（批 C）：此前 `diff_bytes` 只有 `--pack-only`
    /// 的仪表报得出，日常 dogfood 攒不出「组有多大」的分布，于是拆不拆大组永远得先跑一次仪表。
    #[test]
    fn pack_stats_carry_the_groups_own_diff_bytes() {
        let (tmp, b, changeset) = outside_repo();
        let own: usize = b.files.iter().map(|f| f.diff.len()).sum();
        assert!(own > 0);

        let p = pack_bundle(&b, tmp.path(), 1_000_000, own, &PackOpts::default(), &changeset);

        assert_eq!(p.stats.diff_bytes, own, "组自己的 diff 字节要记进 PackStats");
        // 传 0（`--pack-only` 之外的某些调用点按 0 记账）时不谎报
        let p0 = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &changeset);
        assert_eq!(p0.stats.diff_bytes, 0);
    }

    /// 批 C-1：折叠区间要被采出来，且**与渲染文本里的 `…（a-b 行折叠…）` 逐段一致**——
    /// 两处各算一遍必然漂，回退信号就废了。只有真的进了 pack 的块才算（没进去的是预算截断，不是折叠）。
    #[test]
    fn folded_ranges_match_the_rendered_fold_markers() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        // 造一个够大的文件：改动在 f0，其余 def 的体会被折叠
        let mut body = String::from("// header\n");
        // 要超过 slice_min_bytes（8KB）才会走切片，否则是小文件全文、不产生折叠
        for i in 0..120 {
            body.push_str(&format!(
                "pub fn f{i}(x: u8) -> u8 {{\n    let a{i} = x; // padding padding padding padding\n    let b{i} = a{i} + 1;\n    b{i}\n}}\n\n"
            ));
        }
        assert!(body.len() > 8192, "夹具要够大才会切片: {}", body.len());
        std::fs::write(tmp.path().join("src/big.rs"), &body).unwrap();
        let b = crate::bundle::Bundle::from_files(vec![crate::evidence::git::FileChange {
            path: "src/big.rs".into(),
            diff: "@@ -2,1 +2,1 @@\n+pub fn f0(x: u8) -> u8 {\n".into(),
        }]);

        let p = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &["src/big.rs".to_string()]);

        let ranges = p.folded.get("src/big.rs").expect("该文件应有折叠区间");
        assert!(!ranges.is_empty() && p.stats.folded_lines > 0, "{:?} / {:?}", ranges, p.stats);
        // 渲染文本里的每一段折叠标记，都要能在区间表里找到同样的 (a,b)
        let re = regex::Regex::new(r"…（(\d+)-(\d+) 行折叠").unwrap();
        let rendered: Vec<(u32, u32)> = re
            .captures_iter(&p.text)
            .map(|c| (c[1].parse().unwrap(), c[2].parse().unwrap()))
            .collect();
        assert!(!rendered.is_empty(), "应有折叠标记: {}", &p.text[..p.text.len().min(400)]);
        assert_eq!(rendered, *ranges, "渲染标记与采集的区间必须逐段一致");
    }

    /// 非 Cargo 工作区不该报「预算不足」：那类仓（PR181：15 `.ts` + 10 `.go`、零 Rust）本来就谈不上注入，
    /// 报了只会在 md 上刷出误导性的「N group(s) skipped injection」（2026-09-29 `/code-review` 抓到）。
    #[test]
    fn a_non_cargo_repo_never_reports_a_contract_budget_skip() {
        let (tmp, b, changeset) = outside_repo(); // 没有 Cargo.lock
        for i in 1..=5 {
            let pad = "x".repeat(100);
            std::fs::write(tmp.path().join(format!("src/u{i}.rs")), format!("fn use_{i}() {{ let padding_{i} = \"{pad}\"; compute_total(&[{i}]); }}\n")).unwrap();
        }

        let tight = pack_bundle(&b, tmp.path(), 2_400, 0, &PackOpts::default(), &changeset);

        assert!(tight.text.contains("## 文件外片段"), "仓内上下文照常: {}", tight.text);
        assert!(!tight.stats.contract_budget_skipped, "非 Cargo 仓不该记「跳过注入」");
        assert!(!tight.text.contains("三方契约：预算不足"), "{}", tight.text);
    }

    /// 定位失败要**能被看见**（roadmap 风险节：registry 在 CI / 他人机器缺失 → 降级不阻塞，但要披露原因）。
    /// 用一个永远不会存在的包名，使断言不依赖本机 registry 里有什么。
    #[test]
    fn unlocatable_crate_is_disclosed_in_the_omission_list() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        if contracts::registry_root().is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.lock"), "[[package]]\nname = \"nosuchkit\"\nversion = \"9.9.9\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n").unwrap();
        std::fs::write(root.join("src/a.rs"), "use nosuchkit::thing;\npub fn f() { thing(); }\n").unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            diff: "@@ -1,1 +1,1 @@\n+pub fn f() { thing(); }\n".into(),
        }]);

        let p = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &["src/a.rs".to_string()]);

        assert_eq!((p.stats.contracts, p.stats.contract_misses), (0, 1), "{:?}", p.stats);
        assert!(
            p.text.contains("nosuchkit 9.9.9 — 三方契约未注入（registry 无解包目录"),
            "降级了就要说是哪个包、哪个版本、为什么: {}",
            p.text
        );
    }

    /// 预算优先级（A-3 的立项条件）：三方契约与文件外片段**共享同一份帽**，且排在它之后——
    /// 帽被仓内上下文吃光时，契约整段不注入，只留一行可披露的原因。
    #[test]
    fn contracts_never_crowd_out_in_repo_context() {
        let (tmp, b, changeset) = outside_repo();
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock"), tmp.path().join("Cargo.lock")).unwrap();
        for i in 1..=5 {
            let pad = "x".repeat(100);
            std::fs::write(tmp.path().join(format!("src/u{i}.rs")), format!("fn use_{i}() {{ let padding_{i} = \"{pad}\"; compute_total(&[{i}]); }}\n")).unwrap();
        }

        let tight = pack_bundle(&b, tmp.path(), 2_400, 0, &PackOpts::default(), &changeset);

        assert!(tight.text.contains("## 文件外片段"), "仓内上下文必须先拿到帽: {}", tight.text);
        assert_eq!(tight.stats.contracts, 0, "帽已被仓内上下文吃光，契约不该挤进来");
        assert!(tight.text.contains("三方契约：预算不足"), "降级要说得出原因: {}", tight.text);
        assert!(tight.stats.contract_budget_skipped, "跳过要**记账**——只写进 pack 正文的省略清单，md 与报告上就看不见");
        let rich = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &changeset);
        assert!(!rich.stats.contract_budget_skipped, "帽够时不该标跳过");
    }

    /// 临时仓：core.rs（改动，compute_total 引用本文件未定义的 parse_widget）+ user.rs（caller）+ lib_defs.rs（callee 定义带 doc）
    /// + 应被跳过的：tests/it.rs、scripts/run.py（非支持语言）、id_rsa.rs（凭据基名）、变更集内的 sibling.rs。
    fn outside_repo() -> (tempfile::TempDir, crate::bundle::Bundle, Vec<String>) {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("src/core.rs"), "pub fn compute_total(xs: &[u8]) -> u8 { parse_widget(xs) }\n").unwrap();
        std::fs::write(root.join("src/sibling.rs"), "fn sibling_use() { compute_total(&[]); }\n").unwrap();
        std::fs::write(root.join("src/user.rs"), "/// consumer doc\nfn consume_total() {\n    let t = compute_total(&[1]);\n    // compute_total in a comment must not count\n}\n").unwrap();
        std::fs::write(root.join("src/lib_defs.rs"), "/// Parses a widget.\n/// second doc line\npub fn parse_widget(xs: &[u8]) -> u8 {\n    xs.len() as u8\n}\n").unwrap();
        std::fs::write(root.join("tests/it.rs"), "fn t() { compute_total(&[9]); }\n").unwrap();
        std::fs::write(root.join("scripts/run.py"), "compute_total([1])\n").unwrap();
        std::fs::write(root.join("src/id_rsa.rs"), "fn k() { compute_total(&[7]); }\n").unwrap();
        let diff = "@@ -1,1 +1,1 @@\n+pub fn compute_total(xs: &[u8]) -> u8 { parse_widget(xs) }\n";
        let b = Bundle::from_files(vec![
            FileChange { path: "src/core.rs".into(), diff: diff.into() },
            FileChange { path: "src/sibling.rs".into(), diff: "@@ -1,1 +1,1 @@\n+fn sibling_use() { compute_total(&[]); }\n".into() },
        ]);
        let changeset = vec!["src/core.rs".to_string(), "src/sibling.rs".to_string()];
        (tmp, b, changeset)
    }

    #[test]
    fn outside_caller_window_and_callee_signature_with_doc_skipping_guards() {
        let (tmp, b, changeset) = outside_repo();
        let p = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &changeset);
        let s = &p.text;
        assert!(s.contains("## 文件外片段\n"), "{s}");
        assert!(s.contains("src/user.rs:1-5（调用点：compute_total）\n/// consumer doc\nfn consume_total() {\n    let t = compute_total(&[1]);"), "调用点 ±N 窗: {s}");
        assert!(s.contains("src/lib_defs.rs:1-3（签名：parse_widget）\n/// Parses a widget.\n/// second doc line\npub fn parse_widget(xs: &[u8]) -> u8 {\n\n"), "签名 + 紧邻 doc、不带体: {s}");
        assert!(!s.contains("xs.len() as u8"), "callee 只给签名: {s}");
        for absent in ["tests/it.rs", "run.py", "id_rsa.rs"] {
            assert!(!s.contains(absent), "{absent} 应被跳过: {s}");
        }
        assert_eq!(s.matches("src/sibling.rs").count(), 1, "变更集文件只在 0 壳出现一次，不进文件外片段: {s}");
        assert_eq!((p.stats.outside_sites, p.stats.outside_sigs), (1, 1), "{:?}", p.stats);
        assert!(p.stats.outside_bytes > 0);
        // 确定性
        let p2 = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &changeset);
        assert_eq!(p.text, p2.text);
    }

    #[test]
    fn outside_absent_in_full_mode_and_when_no_names() {
        let (tmp, b, changeset) = outside_repo();
        let full = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::full(), &changeset);
        assert!(!full.text.contains("文件外片段"), "full 档无文件外片段（K10）: {}", full.text);
        assert_eq!((full.stats.outside_sites, full.stats.outside_sigs, full.stats.outside_bytes), (0, 0, 0));
        // 改动 def 名不 ref_worthy（`main`）且体内无外部引用 → 空段、空渲染
        std::fs::write(tmp.path().join("src/m.rs"), "fn main() { let a = 1; }\n").unwrap();
        let bm = crate::bundle::Bundle::from_files(vec![crate::evidence::git::FileChange { path: "src/m.rs".into(), diff: "@@ -1,1 +1,1 @@\n+fn main() { let a = 1; }\n".into() }]);
        let pm = pack_bundle(&bm, tmp.path(), 1_000_000, 0, &PackOpts::default(), &["src/m.rs".to_string()]);
        assert!(!pm.text.contains("文件外片段"), "{}", pm.text);
    }

    #[test]
    fn outside_caps_sites_per_name_and_budget_spills_to_omit() {
        let (tmp, b, changeset) = outside_repo();
        for i in 1..=5 {
            std::fs::write(tmp.path().join(format!("src/u{i}.rs")), format!("fn use_{i}() {{ compute_total(&[{i}]); }}\n")).unwrap();
        }
        let p = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &changeset);
        // 路径序：src/u1 / u2 / u3 入选（K = 3）；user.rs 排在 u5 之后、u4 / u5 / user.rs 落选
        assert!(p.text.contains("src/u1.rs") && p.text.contains("src/u2.rs") && p.text.contains("src/u3.rs"), "{}", p.text);
        assert!(!p.text.contains("src/u4.rs") && !p.text.contains("src/user.rs"), "每名 ≤ 3 处: {}", p.text);
        assert_eq!(p.stats.outside_sites, 3);
        // 预算帽：组预算 / 8 = 300（≥ OUTSIDE_MIN_CAP 会搜索）；调用点块加长到 ~180 字节 → 第一块入、其余与签名块进 omit
        for i in 1..=5 {
            let pad = "x".repeat(100);
            std::fs::write(tmp.path().join(format!("src/u{i}.rs")), format!("fn use_{i}() {{ let padding_{i} = \"{pad}\"; compute_total(&[{i}]); }}\n")).unwrap();
        }
        let tight = pack_bundle(&b, tmp.path(), 2_400, 0, &PackOpts::default(), &changeset);
        assert!(tight.text.contains("## 文件外片段") && tight.stats.outside_bytes <= 300, "{}", tight.text);
        assert_eq!((tight.stats.outside_sites, tight.stats.outside_sigs), (1, 0), "{:?}", tight.stats);
        assert!(tight.text.contains("## 已省略（预算裁剪）") && tight.text.contains("src/u2.rs:1-1 — 文件外片段超帽（调用点：compute_total）"), "{}", tight.text);
        assert!(tight.text.contains("文件外片段超帽（签名：parse_widget）"), "{}", tight.text);
    }

    #[test]
    fn outside_does_not_follow_symlink_out_of_root() {
        let (tmp, b, changeset) = outside_repo();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("evil.rs"), "fn evil_site() { compute_total(&[0]); }\n").unwrap();
        std::os::unix::fs::symlink(outside.path(), tmp.path().join("linked")).unwrap();
        let p = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &changeset);
        assert!(!p.text.contains("evil"), "root 外符号链接目标不入包: {}", p.text);
    }

    // ---- 评审修（2026-09-08 code-review high）----

    #[test]
    fn slice_reanchors_hunk_when_diff_coordinates_drift() {
        // 评审修 4：diff 坐标（index 侧）比工作树内容早 120 行（顶部又加了未暂存的注释块）——按 + 行文本重锚定，
        // 核仍落在 changed_target；纯删除 hunk 无 + 行，保留原坐标由禁真空窗兜底。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let prefix: String = (0..120).map(|i| format!("// drift line {i}\n")).collect();
        std::fs::write(root.join("src/a.rs"), format!("{prefix}{}", big_rust_file())).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/a.rs".into(),
            // 纯删除 hunk 无 + 行 → 保留原坐标（第 2 行，落在顶部注释块）由禁真空窗兜底
            diff: "@@ -11,1 +11,1 @@\n+    t + x\n@@ -3,1 +2,0 @@\n-gone\n".into(),
        }]);
        let mut stats = PackStats::default();
        let s = body_section(&b, root, 1_000_000, &mut Vec::new(), &slice_opts(), &mut stats);
        assert!(s.contains("fn changed_target(x: u8) -> u8 {\n    let t = helper_one();\n    t + x\n}"), "重锚定后核在场: {s}");
        assert!(s.contains("fn helper_one() -> u8 {\n    let h = 1;") && s.contains("changed_target(2)"), "一跳随核: {s}");
        assert!(s.contains("行折叠：unrelated_big）"), "无关体仍折叠: {s}");
        assert!(s.contains("切片：核 1 · 一跳 2"), "{s}");
    }

    #[test]
    fn slice_unsupported_lang_header_limited_to_top_of_file() {
        // 评审修 7：非支持语言只把文件顶部 HEADER_MAX_LINES 行里的标题 / 注释当地图，不扫全篇。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let md: String = (1..=1200).map(|i| if i % 100 == 1 { format!("# heading {i}\n") } else { format!("line {i}\n") }).collect();
        std::fs::write(root.join("notes.md"), &md).unwrap();
        let b = Bundle::from_files(vec![FileChange { path: "notes.md".into(), diff: "@@ -600,1 +600,1 @@\n+line 600\n".into() }]);
        let s = body_section(&b, root, 1_000_000, &mut Vec::new(), &slice_opts(), &mut PackStats::default());
        assert!(s.contains("# heading 1\n") && s.contains("# heading 601\n"), "顶部标题与窗内标题在场: {s}");
        assert!(!s.contains("# heading 301\n") && !s.contains("# heading 1101\n"), "窗外、顶部之外的标题折叠: {s}");
    }

    #[test]
    fn slice_keeps_multiline_import_group() {
        // 评审修 8：Go `import (…)` 整组保留（安全 focus 需要看到 crypto/md5 / math/rand）。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let mut go = String::from("package main\n\nimport (\n\t\"fmt\"\n\t\"crypto/md5\"\n\t\"math/rand\"\n\t\"os\"\n)\n\nfunc changedTarget(x int) int {\n\treturn rand.Intn(x)\n}\n\nfunc unrelatedBig() int {\n");
        for i in 0..600 { go += &format!("\tunused_{i} := {i}\n"); }
        go += "\treturn 0\n}\n";
        std::fs::write(root.join("src/b.go"), &go).unwrap();
        let b = Bundle::from_files(vec![FileChange { path: "src/b.go".into(), diff: "@@ -11,1 +11,1 @@\n+\treturn rand.Intn(x)\n".into() }]);
        let s = body_section(&b, root, 1_000_000, &mut Vec::new(), &slice_opts(), &mut PackStats::default());
        assert!(s.contains("import (\n\t\"fmt\"\n\t\"crypto/md5\"\n\t\"math/rand\"\n\t\"os\"\n)\n"), "多行 import 组整组保留: {s}");
        assert!(!s.contains("unused_7 := 7"), "{s}");
    }

    #[test]
    fn outside_import_lines_are_not_call_sites() {
        // 评审修 6：`use` / `import` 行不算调用点，K 配额留给真正的调用。
        let (tmp, b, changeset) = outside_repo();
        let mut body = String::from("use crate::core::compute_total;\n\n");
        for i in 0..47 { body += &format!("// filler {i}\n"); }
        body += "fn late_caller() -> u8 {\n    compute_total(&[3])\n}\n";
        std::fs::write(tmp.path().join("src/late.rs"), &body).unwrap();
        let p = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &changeset);
        assert!(p.text.contains("src/late.rs:21-52（调用点：compute_total）"), "窗围绕真实调用（第 51 行）而非 use 行: {}", p.text);
        assert!(!p.text.contains("src/late.rs:1-31"), "{}", p.text);
    }

    #[test]
    fn outside_skips_cfg_test_defs_and_test_files_but_searches_sensitive_word_sources() {
        // 评审修 1 / 10：Rust `#[cfg(test)]` 之后的改动 def 不搜调用点；`token_bucket.rs` 这类源码不被敏感词子串跳过。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let (tmp, _, _) = outside_repo();
        let root = tmp.path();
        std::fs::write(root.join("src/core.rs"), "pub fn compute_total(xs: &[u8]) -> u8 { xs.len() as u8 }\n#[cfg(test)]\nmod tests {\n    fn helper_case() -> u8 { compute_total(&[1]) }\n}\n").unwrap();
        std::fs::write(root.join("src/token_bucket.rs"), "fn refill_bucket() -> u8 { compute_total(&[2]) }\n").unwrap();
        std::fs::write(root.join("src/z.rs"), "fn z_call() -> u8 { helper_case() }\n").unwrap();
        let diff = "@@ -1,1 +1,1 @@\n+pub fn compute_total(xs: &[u8]) -> u8 { xs.len() as u8 }\n@@ -4,1 +4,1 @@\n+    fn helper_case() -> u8 { compute_total(&[1]) }\n";
        let b = Bundle::from_files(vec![FileChange { path: "src/core.rs".into(), diff: diff.into() }]);
        let p = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &["src/core.rs".to_string()]);
        assert!(p.text.contains("src/token_bucket.rs:1-1（调用点：compute_total）"), "敏感词源码照搜: {}", p.text);
        assert!(!p.text.contains("src/z.rs"), "测试 fn 的调用点不搜: {}", p.text);
        assert!(!p.text.contains("id_rsa.rs"), "凭据基名仍跳过: {}", p.text);
    }

    /// 合并后审计必修：regex 交替最左优先——短名是长名前缀（build_pack / build_packs）时先命中短名、边界否决后不回溯，
    /// 长名调用点漏检；交替按长度降序后两者都命中。
    #[test]
    fn outside_prefix_names_both_hit_longest_first() {
        let (tmp, _b, _cs) = outside_repo();
        let root = tmp.path();
        std::fs::write(root.join("src/core.rs"), "pub fn build_pack(x: u8) -> u8 { x }\npub fn build_packs(x: u8) -> u8 { x }\n").unwrap();
        std::fs::write(root.join("src/user_pack.rs"), "fn a() { build_pack(1); }\n").unwrap();
        std::fs::write(root.join("src/user_packs.rs"), "fn b() { build_packs(1); }\n").unwrap();
        let diff = "@@ -1,2 +1,2 @@\n+pub fn build_pack(x: u8) -> u8 { x }\n+pub fn build_packs(x: u8) -> u8 { x }\n";
        let b = crate::bundle::Bundle::from_files(vec![crate::evidence::git::FileChange { path: "src/core.rs".into(), diff: diff.into() }]);
        let changeset = vec!["src/core.rs".to_string()];
        let p = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &changeset);
        assert!(p.text.contains("src/user_pack.rs:1-1（调用点：build_pack）"), "{}", p.text);
        assert!(p.text.contains("src/user_packs.rs:1-1（调用点：build_packs）"), "{}", p.text);
        assert_eq!(p.stats.outside_sites, 2, "{:?}", p.stats);
    }

    /// 合并后审计建议 3：帽不足未走仓时不报「名字截断」；走仓时截断行照旧。
    #[test]
    fn outside_name_truncation_reported_only_when_searched() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let n = OUTSIDE_CALLER_NAMES_MAX + 3;
        let src: String = (0..n).map(|i| format!("pub fn changed_symbol_{i:03}() {{}}\n")).collect();
        std::fs::write(root.join("src/core.rs"), &src).unwrap();
        std::fs::write(root.join("src/user.rs"), "fn u() { changed_symbol_000(); }\n").unwrap();
        let diff = format!("@@ -1,{n} +1,{n} @@\n{}", src.lines().map(|l| format!("+{l}\n")).collect::<String>());
        let b = Bundle::from_files(vec![FileChange { path: "src/core.rs".into(), diff }]);
        let changeset = vec!["src/core.rs".to_string()];
        let searched = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &changeset);
        assert!(searched.text.contains("文件外调用点名字截断 +3"), "{}", searched.text);
        let starved = pack_bundle(&b, root, 900, 0, &PackOpts::default(), &changeset);
        assert!(starved.text.contains("未搜索") && !starved.text.contains("名字截断"), "{}", starved.text);
    }

    /// 合并后审计 nit 7：`-U0` 式尾部纯删除（无后随上下文）删除点 = start + count，落在 hunk 跨度外——并入跨度后禁真空窗覆盖它。
    #[test]
    fn slice_tail_deletion_without_context_gets_vacuum_window() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let content: String = (1..=8).map(|i| format!("l{i}\n")).collect();
        std::fs::write(root.join("src/a.rs"), &content).unwrap();
        // 新文件坐标 3..5 为上下文，删除点在其后（0-based 5）
        let diff = "@@ -3,4 +3,3 @@\n l3\n l4\n l5\n-gone\n";
        let b = Bundle::from_files(vec![FileChange { path: "src/a.rs".into(), diff: diff.into() }]);
        let opts = PackOpts { body: PackBody::Slice, anchor_window: 1, slice_min_bytes: 0 };
        let p = pack_bundle(&b, root, 1_000_000, 0, &opts, &["src/a.rs".to_string()]);
        assert!(p.text.contains("（切片："), "{}", p.text);
        assert!(p.text.contains("l5\nl6\nl7\n"), "删除点 ±1 窗（l5..l7）在场: {}", p.text);
    }

    #[test]
    fn outside_budget_too_small_skips_walk() {
        // 评审修 5：帽 < OUTSIDE_MIN_CAP 不走仓，省略清单一行说明。
        let (tmp, b, changeset) = outside_repo();
        let p = pack_bundle(&b, tmp.path(), 900, 0, &PackOpts::default(), &changeset);
        assert!(!p.text.contains("## 文件外片段"), "{}", p.text);
        assert!(p.text.contains("文件外片段：预算不足") && p.text.contains("未搜索"), "{}", p.text);
        assert_eq!((p.stats.outside_sites, p.stats.outside_sigs), (0, 0));
    }

    #[test]
    fn second_pass_opts_full_blocks_with_outside_section() {
        // 评审修 9：二阶段档 = 全文块 + 文件外片段。
        let (tmp, b, changeset) = outside_repo();
        let p = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::second_pass(&PackOpts::default()), &changeset);
        assert!(p.text.contains("src/core.rs:1-1（全文，1 行）") && p.text.contains("## 文件外片段"), "{}", p.text);
        assert_eq!((p.stats.body.as_str(), p.stats.small_full, p.stats.sliced), ("slice", 2, 0));
    }

    // ---- 复审修（2026-09-09 第二轮 code-review high）----

    #[test]
    fn second_pass_opts_follow_user_config() {
        // 复审修 1：二阶段只改 slice_min_bytes，body / anchor_window 沿用用户配置；full 对照臂不被切成 slice。
        let full = PackOpts::second_pass(&PackOpts { anchor_window: 7, ..PackOpts::full() });
        assert_eq!((full.body, full.anchor_window, full.slice_min_bytes), (PackBody::Full, 7, usize::MAX));
        let (tmp, b, changeset) = outside_repo();
        let p = pack_bundle(&b, tmp.path(), 1_000_000, 0, &full, &changeset);
        assert!(!p.text.contains("文件外片段") && p.text.contains("（全文，1 行）"), "{}", p.text);
    }

    #[test]
    fn second_pass_changeset_excludes_needs_file_from_outside_search() {
        // 复审修 8：needs 前插的文件已在 0 壳全文，不再当「文件外」二次注入。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let (tmp, b, mut changeset) = outside_repo();
        let mut files = vec![FileChange { path: "src/user.rs".into(), diff: String::new() }];
        files.extend(b.files.iter().cloned());
        changeset.push("src/user.rs".into());
        let pb = Bundle::from_files(files);
        let p = pack_bundle(&pb, tmp.path(), 1_000_000, 0, &PackOpts::second_pass(&PackOpts::default()), &changeset);
        assert!(p.text.contains("src/user.rs:1-5（全文，5 行）"), "{}", p.text);
        assert!(!p.text.contains("src/user.rs:1-5（调用点"), "needs 文件不再进文件外片段: {}", p.text);
        assert!(p.text.contains("（签名：parse_widget）"), "其余文件外片段照常: {}", p.text);
    }

    #[test]
    fn outside_names_use_reanchored_coordinates() {
        // 复审修 2：diff 坐标比工作树早 15 行（顶部未暂存插入）——文件外搜索用重锚定后的改动 def 名，而不是陈旧坐标处的注释。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let (tmp, _, changeset) = outside_repo();
        let prefix: String = (0..15).map(|i| format!("// drift {i}\n")).collect();
        std::fs::write(tmp.path().join("src/core.rs"), format!("{prefix}pub fn compute_total(xs: &[u8]) -> u8 {{ parse_widget(xs) }}\n")).unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/core.rs".into(),
            diff: "@@ -1,1 +1,1 @@\n+pub fn compute_total(xs: &[u8]) -> u8 { parse_widget(xs) }\n".into(),
        }]);
        let p = pack_bundle(&b, tmp.path(), 1_000_000, 0, &PackOpts::default(), &changeset);
        assert!(p.text.contains("（调用点：compute_total）") && p.text.contains("（签名：parse_widget）"), "{}", p.text);
    }

    #[test]
    fn outside_ts_export_function_is_signature_candidate() {
        // 复审修 3：`export function parseWidget(` 是 def 行，不能被 import 短路掉。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/lib")).unwrap();
        std::fs::write(root.join("src/api.ts"), "export function wrapBody(): number {\n  return parseWidget(1);\n}\n").unwrap();
        std::fs::write(root.join("src/lib/widget.ts"), "/** parse it */\nexport function parseWidget(x: number): number {\n  return x;\n}\nexport const items$ = wrapBody();\n").unwrap();
        std::fs::write(root.join("src/view.ts"), "import { items$ } from './lib/widget';\nexport const view = items$ + wrapBody();\n").unwrap();
        let b = Bundle::from_files(vec![FileChange {
            path: "src/api.ts".into(),
            diff: "@@ -2,1 +2,1 @@\n+  return parseWidget(1);\n".into(),
        }]);
        let p = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &["src/api.ts".to_string()]);
        assert!(p.text.contains("src/lib/widget.ts:1-2（签名：parseWidget）\n/** parse it */\nexport function parseWidget(x: number): number {"), "{}", p.text);
        // 调用点：widget.ts 的 `export const items$ = wrapBody();` 与 view.ts 第 2 行都算；view.ts 的 import 行不算
        assert!(p.text.contains("（调用点：wrapBody）"), "{}", p.text);
        assert!(!p.text.contains("src/view.ts:1-1（"), "{}", p.text);
    }

    #[test]
    fn outside_dollar_names_match_with_custom_boundary() {
        // 复审修 9：`items$` 这类含 `$` 的标识符用 [^\w$] 边界才能命中。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/store.ts"), "export const items$ = makeSubject();\n").unwrap();
        std::fs::write(root.join("src/view.ts"), "const sub = items$.pipe(x => x);\n").unwrap();
        let b = Bundle::from_files(vec![FileChange { path: "src/store.ts".into(), diff: "@@ -1,1 +1,1 @@\n+export const items$ = makeSubject();\n".into() }]);
        let p = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &["src/store.ts".to_string()]);
        assert!(p.text.contains("src/view.ts:1-1（调用点：items$）"), "{}", p.text);
    }

    #[test]
    fn outside_cfg_test_item_level_keeps_following_production_defs() {
        // 复审修 5：item 级 `#[cfg(test)]` 只屏蔽该 item，其后的生产 def 仍搜调用点。
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let (tmp, _, _) = outside_repo();
        let root = tmp.path();
        std::fs::write(root.join("src/core.rs"), "#[cfg(test)]\nfn test_wrapper() -> u8 { 1 }\npub fn compute_total(xs: &[u8]) -> u8 { xs.len() as u8 }\n").unwrap();
        let diff = "@@ -2,2 +2,2 @@\n+fn test_wrapper() -> u8 { 1 }\n+pub fn compute_total(xs: &[u8]) -> u8 { xs.len() as u8 }\n";
        let b = Bundle::from_files(vec![FileChange { path: "src/core.rs".into(), diff: diff.into() }]);
        let p = pack_bundle(&b, root, 1_000_000, 0, &PackOpts::default(), &["src/core.rs".to_string()]);
        assert!(p.text.contains("（调用点：compute_total）"), "{}", p.text);
    }

    #[test]
    fn anchor_offset_scores_informative_lines_and_tolerates_one_edit() {
        // 复审修 4：5 个 + 行里 1 个被后续编辑改过仍能定位（4/5 ≥ 一半且优于原位）；只有 `}` 的 + 行不定位（返回 0）。
        let mut file: Vec<String> = (0..100).map(|i| format!("// pad {i}")).collect();
        file.extend(["fn beta_fn() {", "    let a1 = compute(1);", "    let a2 = compute(2);", "    let a3 = compute(3);", "    let a4 = compute(4);", "    let a5 = EDITED;", "}"].map(String::from));
        let refs: Vec<&str> = file.iter().map(String::as_str).collect();
        let plus: Vec<(usize, String)> = (0..5).map(|k| (1 + k, format!("    let a{} = compute({});", k + 1, k + 1))).collect();
        let h = crate::diffsym::Hunk { start: 1, count: 5, changed: (1..6).collect(), plus, minus: Vec::new() };
        assert_eq!(anchor_offset(&refs, &h), 100, "4/5 吻合 → 偏移 +100");
        let braces = crate::diffsym::Hunk { start: 3, count: 1, changed: vec![3], plus: vec![(3, "}".into())], minus: Vec::new() };
        assert_eq!(anchor_offset(&refs, &braces), 0, "低信息 + 行不参与定位");
    }
}
