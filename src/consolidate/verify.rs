//! verifier 判定层（M21b 批 D-2，design §8.1〔D17〕）：**零 provider 调用**——
//! 谁该被复核（触发集）、谁可以免验（跳过四类）、交给谁复核（异族选择）。
//! 真正的调用与流式消费在批 D-3 接进深挖主循环。
//!
//! 批 D 之前这里是 pre-M21 的批式 refute（`verify_findings(&dyn ReviewProvider, …)`），
//! 在 M21 路径上从未接线（只剩 `pub use` 与自测在引用）——本批按新架构重写，
//! 只留下仍成立的两个零件：现场快照 `source_snippet` 与判例机械预筛 `matches_precedent`。

use crate::config::{EffectiveModel, RoleSets};
use crate::consolidate::dedup::{quote_overlap, title_jaccard};
use crate::db::DismissCard;
use crate::domain::{Finding, Severity};
use crate::diffsym::{compile_defs, lang_for};
use crate::pack::skeleton_of;
use std::path::Path;

/// refute 现场源码段窗口半径（行）与字节帽（design Task 2，E7：refute 复审零 pack 裸奔重读
/// 全文件，解剖实测 14 调用/趟——注入声称位置的快照给它一个起点，不代替它自行验证）。
#[allow(dead_code)] // D-3 接线前无生产调用方
const SNIPPET_WINDOW: usize = 60;
#[allow(dead_code)] // D-3 接线前无生产调用方
const SNIPPET_BYTE_CAP: usize = 24_000;
/// 读入帽：与本仓读路径基线对齐（tools::read_one=64KB / do_read_files=128KB /
/// positioning::safe_read=512KB，I2/T2）——对抗式威胁模型含「恶意 PR 塞大文件」，
/// refute 这条新读路径不得重开无界读的口子。
#[allow(dead_code)] // D-3 接线前无生产调用方
const SNIPPET_READ_MAX_BYTES: usize = 512 * 1024;
/// 骨架中档触发帽（Task 6, E8-F；T48 后语义变化——表2-4，M18批2）：内容 > 64KB 且语言
/// 受支持时先试骨架；64KB 帽以下与骨架未命中/超帽的情形统一走下方 ±60 行窗（不再对
/// ≤64KB 文件整篇回退，line_start/end 全路径都被用上）。
#[allow(dead_code)] // D-3 接线前无生产调用方
const SNIPPET_FULL_FILE_CAP: usize = 64_000;
#[allow(dead_code)] // D-3 接线前无生产调用方
/// 读不到证据时的占位符（读守卫拒绝时调用方也用它——同一句话，模型看到的形状一致）。
pub(crate) fn snippet_unavailable() -> &'static str {
    SNIPPET_UNAVAILABLE
}

const SNIPPET_UNAVAILABLE: &str = "(source unavailable — judge from the finding's quote alone and weigh confidence accordingly)";

/// finding 声称位置的工作树快照：内容 ≤64KB、或受支持语言的骨架未命中/超帽，统一走
/// `[line_start-60, line_end+60]` 行窗（1-based 夹紧）取内容，头带 `path:起-止（现场快照）`；
/// 受支持语言且原文 >64KB 时先试骨架中档（签名+锚点行，Task 6）。超 24_000 字节按 char
/// 边界截断+注记。声称行号超出文件实际范围（模型谎报 / carried 陈旧行号，T48 行契约，
/// M18 表2-4）→ clamp 到 `[1,n]` 且头部注记，不再占位吞证据；文件读不到 / 为空 / 命中读帽
/// 且窗口伸进未读部分 → 仍统一占位——skeptic 不得被喂假装"已验证"的空内容，占位显式
/// 指示它自行用工具读。
#[allow(dead_code)] // D-3 把它接进复核 prompt 前，只有自测在用
pub(crate) fn source_snippet(root: &Path, f: &Finding) -> String {
    // 有界读取（镜像 tools::read_one 的 I2/T2 模式）：最多 512KB+1 字节进内存，
    // +1 用于区分「文件恰好帽长」与「命中帽被截」。非 UTF-8（含帽边界劈开多字节字符）→ 占位。
    use std::io::Read;
    let mut content = String::new();
    let read_ok = std::fs::File::open(root.join(&f.file))
        .and_then(|fh| fh.take(SNIPPET_READ_MAX_BYTES as u64 + 1).read_to_string(&mut content))
        .is_ok();
    if !read_ok || content.is_empty() {
        return SNIPPET_UNAVAILABLE.to_string();
    }
    let hit_cap = content.len() > SNIPPET_READ_MAX_BYTES;

    let mut lines: Vec<&str> = content.lines().collect();
    if hit_cap {
        lines.pop(); // 帽处截断的尾行可能不完整，弃之
    }
    let n = lines.len();
    if n == 0 {
        return SNIPPET_UNAVAILABLE.to_string();
    }

    // 中档（Task 6，E8-F 补链）：全文超 64KB 帽、语言受支持时先试骨架（签名行+finding 声称的
    // 锚点行，其余折叠）——比 ±60 快照信号密度更高（骨架覆盖全文的 def 签名，快照只见局部）。
    // 骨架复用 pack::skeleton_of（Task 5），但 refute 只有 Finding、没有 diff/hunk——合成一个
    // 伪 hunk（`@@ -0,0 +line_start,N @@` + N 个 `+` 行）让 [line_start, line_end] 落进
    // hunk_changed_lines 的锚点集合、折叠时存活；比传空 diff（纯签名骨架、会把 finding 声称的
    // 现场本身折掉）更贴合 refute 的复核需求。骨架仍超 24KB 帽 → 落回下方 ±60 快照（行为不倒退）。
    // ponytail: `hit_cap`（512KB 读帽触断）不进骨架——那条路径下 `n`/`lines` 只是被截断的
    // 前缀，骨架会静默丢失「目标行在帽外」的安全占位语义（下方窗口分支的 hit_cap 越界检查）；
    // hit_cap 文件本就远超骨架/窗口两级预算，走既有窗口路径的降级判断即可，升级点再拆。
    if !hit_cap && content.len() > SNIPPET_FULL_FILE_CAP {
        if let Some(lang) = lang_for(&f.file) {
            let defs = compile_defs(lang);
            let n_start = (f.line_start as usize).max(1);
            let n_end = (f.line_end as usize).max(n_start).min(n.max(n_start));
            let hunk_len = n_end - n_start + 1;
            let pseudo_diff = format!("@@ -0,0 +{n_start},{hunk_len} @@\n{}", "+x\n".repeat(hunk_len));
            let skel = skeleton_of(&lines.join("\n"), &pseudo_diff, &defs);
            let out = format!("{}:1-{}（骨架：签名+锚点行）\n{}", f.file, n, skel);
            if out.len() <= SNIPPET_BYTE_CAP {
                return out;
            }
        }
    }

    // 行窗（T48 行契约，M18 表2-4）：finding 声称的 line_start/end ± SNIPPET_WINDOW，clamp 到
    // [1,n]（不再对 ≤64KB 文件整篇回退——line_start/end 全路径统一被用上）。
    let claimed_start = (f.line_start as usize).max(1);
    let claimed_end = (f.line_end as usize).max(claimed_start);
    // 命中读帽（非行号谎报，是「还没读到那么远」）且请求窗口伸进未读部分 → 安全降级占位，
    // 不喂被帽掐掉的假"现场"，模型自己读。
    if hit_cap && claimed_end + SNIPPET_WINDOW > n {
        return SNIPPET_UNAVAILABLE.to_string();
    }
    // 声称行号超出文件实际行数（模型谎报 / carried 陈旧锚点）→ clamp 到实际范围 + 头部注记，
    // 不再占位吞掉证据（旧行为：start>end 即整体判死，见 T48 前 revision）；越界时给整个可读
    // 范围（而非把窗口硬夹到末尾一行——那样等于二次吞证据）。
    let out_of_range = claimed_start > n || claimed_end > n;
    let (start, end) = if out_of_range {
        (1, n)
    } else {
        (claimed_start.saturating_sub(SNIPPET_WINDOW).max(1), (claimed_end + SNIPPET_WINDOW).min(n))
    };
    let header = if out_of_range {
        format!(
            "{}:{}-{}（声称行号 {}-{} 超出文件范围 1-{}，已收窄为现场快照）",
            f.file, start, end, claimed_start, claimed_end, n
        )
    } else {
        format!("{}:{}-{}（现场快照）", f.file, start, end)
    };
    let mut out = format!("{header}\n{}", lines[start - 1..end].join("\n"));
    if out.len() > SNIPPET_BYTE_CAP {
        let mut cut = SNIPPET_BYTE_CAP;
        while cut > 0 && !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n…（截断，超 24KB 帽）");
    }
    out
}

/// 机械预筛（design 2026-07-03 #3）：finding 是否疑似某活跃判例的主题变体。
/// 召回优先（阈值宽）、精判交 refute（判例注入的 skeptic 终审）——机械匹配对
/// 换措辞天然半盲，它只负责把候选送进 refute，不做终审。
/// 精确 id 命中不算（那是 mark_dismissed 豁免的职责，refute 纯浪费）。
#[allow(dead_code)] // 判例变体触发是 M21c 的空槽，届时由 `decide` 调用
fn matches_precedent(f: &Finding, c: &DismissCard) -> bool {
    if f.id == c.id {
        return false;
    }
    let t = title_jaccard(&f.title, &c.title);
    (f.file == c.file && t >= 0.25) || t >= 0.5 || quote_overlap(&f.quote, &c.quote) >= 0.5
}

/// 一条 finding 的复核决定（进 RunMeta 的跳过分布——「没触发」与「触发了被免验」必须分得开）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyDecision {
    /// 该验。
    Verify,
    /// 低危 / 低置信：`verdict` 层本就不认它，复核它纯烧 token 无裁决增益。
    SkipNotTriggered,
    /// `review.verifiers.enabled = false`（全局关）。
    SkipDisabled,
    /// carried：出生轮已复核过。
    SkipCarried,
    /// 精确 id 已在驳回台账——那是豁免的职责，再验纯浪费。
    SkipPrecedent,
    /// 两个家族的 scanner 独立标记了这条 finding 所在的那几行，且 reviewer 产出了它（§8.1 三方一致）。
    SkipConsensus,
}

impl VerifyDecision {
    /// RunMeta 里的分类键（跳过原因分布）。
    pub fn reason(self) -> &'static str {
        match self {
            VerifyDecision::Verify => "verify",
            VerifyDecision::SkipNotTriggered => "not_triggered",
            VerifyDecision::SkipDisabled => "disabled",
            VerifyDecision::SkipCarried => "carried",
            VerifyDecision::SkipPrecedent => "precedent",
            VerifyDecision::SkipConsensus => "consensus",
        }
    }
    pub fn is_verify(self) -> bool {
        self == VerifyDecision::Verify
    }
}

/// 触发集与跳过规则的单一真相源（design §8.1）。
///
/// **触发** = `severity ≥ block_threshold ∧ confidence ≥ floor`——与 `verdict::compute_verdict` 同源：
/// 「会让本轮 reject 的那批 finding」正是「值得花一次复核的那批」。判例变体触发是 M21c 的空槽。
///
/// **跳过**按固定序判，因为分类要能解释清楚：先看是否根本没触发（那不算被跳过），
/// 再看全局开关，然后是 carried / 台账精确命中，最后才是区域共识。
///
/// 区域共识读的是 `suspicion_agreement`（**批 D-2 起是项内轴 + 热区收紧后的值**，见 `EmitCtx`），
/// 不是 `agreement`——后者是「几个模型认同这条 finding」，单 reviewer 配置下恒为 1，拿它当免验依据等于永不免验。
///
/// 与 pre-M21 的 refute 层①有意不同：那时「模型轴 agreement ≥ 2」也免验；§8.1 没有这一条，本批不带进来
/// （M21b 一项只交一个 reviewer，模型轴 ≥ 2 只在 dedup 合并跨格同一 bug 时出现，稀少且不属于 §8.1 的三方一致）。
pub fn decide(
    f: &Finding,
    precedents: &[DismissCard],
    block_threshold: Severity,
    floor: f32,
    enabled: bool,
) -> VerifyDecision {
    if f.severity < block_threshold || f.confidence < floor {
        return VerifyDecision::SkipNotTriggered;
    }
    if !enabled {
        return VerifyDecision::SkipDisabled;
    }
    if f.origin == crate::domain::Origin::Carried {
        return VerifyDecision::SkipCarried;
    }
    if precedents.iter().any(|c| c.id == f.id) {
        return VerifyDecision::SkipPrecedent;
    }
    if f.suspicion_agreement >= 2 {
        return VerifyDecision::SkipConsensus;
    }
    VerifyDecision::Verify
}

/// 选 verifier（design §8.1）：verifiers 活跃集非空 → 从中轮转；否则自动异族
/// （family ≠ 该 finding 的 reviewer 家族，优先 reviewers 档 → scanners 档）；
/// 一个异族都没有 → 同族复核 + warning。
///
/// `rotation` = 轮转游标（调用方按已派复核数递增），与 reviewer 分派池同款——多个 verifier 时摊开，不全压一个。
/// 返回 `None` = 一个可用模型都没有（调用方按「无 verifier」跳过并 warning）。
pub fn pick_verifier<'a>(
    roles: &'a RoleSets,
    reviewer: &EffectiveModel,
    rotation: usize,
) -> Option<(&'a EffectiveModel, Option<String>)> {
    let reviewer_family = reviewer.family.as_str();
    if !roles.verifiers.is_empty() {
        let n = roles.verifiers.len();
        let picked = &roles.verifiers[rotation % n];
        // 显式声明也要如实告知（收口评审 #11）：轮到的这个若与产出该 finding 的 reviewer 同模型
        // （自己验自己）或同家族，判决的独立性就打了折——离线基线里同族那臂误驳 20% vs 异族 12%。
        // 只 warning 不改选：这是用户显式配的，他有权这么配。
        let warn = if picked.name == reviewer.name {
            Some(format!("verifier {} 与产出该 finding 的 reviewer 是同一个模型——自己验自己，判决不独立", picked.name))
        } else if picked.family == reviewer.family {
            Some(format!("verifier {} 与 reviewer 同家族（{}）——异族复核更准（离线基线：误驳 12% vs 20%）", picked.name, picked.family))
        } else {
            None
        };
        return Some((picked, warn));
    }
    let cross = roles
        .reviewers
        .iter()
        .chain(roles.scanners.iter())
        .find(|m| m.family != reviewer_family);
    if let Some(m) = cross {
        return Some((m, None));
    }
    // 无异族：同族复核，但必须可观测——§8.1 明写「一个异族都没有 → 同族 + warning」。
    let same = roles.reviewers.iter().chain(roles.scanners.iter()).next()?;
    let warn = format!(
        "no cross-family verifier available (all models are family {reviewer_family}); verified by same family {}",
        same.name
    );
    Some((same, Some(warn)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Severity};

    use crate::domain::finding::fixtures::finding;
    fn finding_at(file: &str, line_start: u32, line_end: u32) -> Finding {
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.file = file.into();
        f.line_start = line_start;
        f.line_end = line_end;
        f
    }
    #[test]
    fn source_snippet_windows_and_clamps() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 大于 64KB 的 fixture，以测试 ±60 行窗口逻辑（不触发全文升档）；.txt 非受支持语言——
        // 骨架中档（Task 6）只对 lang_for 命中的语言生效，此 fixture 就是走窗口路径的对照。
        let body: String = (1..=10_000).map(|i| format!("line{i}\n")).collect();
        std::fs::write(root.join("src/a.txt"), &body).unwrap();
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.file = "src/a.txt".into();
        f.line_start = 100;
        f.line_end = 102;
        let s = source_snippet(root, &f);
        assert!(s.contains("line40") && s.contains("line162"), "±60 行窗: {s}");
        assert!(!s.contains("line39\n") && !s.contains("line163"), "窗外不含");
        assert!(s.contains("src/a.txt:40-162"), "快照头带范围");
        // 读不到 → 占位
        let mut f2 = finding("t", Severity::High, 1, "alpha");
        f2.file = "no/such.rs".into();
        f2.line_start = 1;
        f2.line_end = 1;
        assert_eq!(source_snippet(root, &f2), SNIPPET_UNAVAILABLE);
    }

    #[test]
    fn source_snippet_24kb_cap_truncates_on_char_boundary() {
        // 24KB 帽分支：多字节字符（3 字节 CJK）填满窗口，钉住 is_char_boundary 回退不 panic + 注记在场。
        // .txt 非受支持语言——绕开 Task 6 骨架中档，保持本测试原有窗口路径覆盖。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let line = "码".repeat(100); // 300 字节/行
        // 500 行 × 301 字节 ≈ 150KB > 64KB，不触发全文升档，进 ±60 窗快照路径
        let body: String = (0..500).map(|_| format!("{line}\n")).collect();
        std::fs::write(root.join("big.txt"), &body).unwrap();
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.file = "big.txt".into();
        f.line_start = 61;
        f.line_end = 61;
        let s = source_snippet(root, &f); // 窗口 121 行 × ~301B ≈ 36KB > 24KB 帽
        assert!(s.ends_with("…（截断，超 24KB 帽）"), "截断注记在场: …{}", &s[s.len().min(s.len().saturating_sub(60))..]);
        assert!(s.len() <= SNIPPET_BYTE_CAP + "\n…（截断，超 24KB 帽）".len(), "截断到帽内: {}", s.len());
        assert!(s.starts_with("big.txt:1-121（现场快照）"), "快照头仍在");
    }

    #[test]
    fn source_snippet_over_64kb_rust_file_uses_skeleton_tier() {
        // Task 6 中档：全文超 64KB 帽、.rs 受支持语言 → 应先落骨架（而非直接跳 ±60 快照）；
        // finding 声称的锚点行（伪 hunk 合成）在骨架折叠中存活。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        let body: String = (1..=10_000).map(|i| format!("line{i}\n")).collect();
        std::fs::write(root.join("src/a.rs"), &body).unwrap();
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.file = "src/a.rs".into();
        f.line_start = 100;
        f.line_end = 102;
        let s = source_snippet(root, &f);
        assert!(s.contains("（骨架：签名+锚点行）"), "超帽 rust 文件应走骨架中档而非快照: {s}");
        assert!(!s.contains("现场快照"), "骨架命中不应再落 ±60 快照");
        assert!(s.contains("line100") && s.contains("line101") && s.contains("line102"), "finding 锚点行在骨架中存活: {s}");
        assert!(!s.contains("line50"), "非锚点/非签名行应折叠: {s}");
    }

    #[test]
    fn source_snippet_read_cap_degrades_when_target_beyond_cap() {
        // 512KB 读帽分支（I2/T2 对齐）：大文件 + 目标行在帽外 → 占位；帽内目标仍正常渲染。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let body: String = (1..=70_000).map(|i| format!("line{i}\n")).collect(); // ~700KB > 512KB
        std::fs::write(root.join("huge.rs"), &body).unwrap();
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.file = "huge.rs".into();
        f.line_start = 65_000; // 帽外（512KB ≈ 前 ~52K 行）
        f.line_end = 65_000;
        assert_eq!(source_snippet(root, &f), SNIPPET_UNAVAILABLE, "帽外目标安全降级");
        f.line_start = 100; // 帽内目标不受影响
        f.line_end = 100;
        let s = source_snippet(root, &f);
        assert!(s.starts_with("huge.rs:40-160（现场快照）") && s.contains("line100"), "帽内窗口正常渲染: {}", &s[..60]);
    }

    #[test]
    fn source_snippet_small_file_uses_line_window_not_full_file(){
        // T48 行契约（表2-4，M18批2）：≤64KB 文件不再整篇回退——line_start/end 全路径统一
        // 走行窗；窗口（±60）覆盖了这个 3 行小文件的全部内容，故实际渲染范围恰好是全文，
        // 但契约上走的是「现场快照」窗口分支，不是旧的「锚点文件全文」分支。
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("small.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let f = finding_at("small.rs", 2, 2);
        let s = source_snippet(tmp.path(), &f);
        assert!(s.contains("（现场快照）"), "小文件也走行窗契约，非整篇回退: {s}");
        assert!(!s.contains("锚点文件全文"), "旧的整篇回退分支已移除: {s}");
        assert!(s.contains("fn a()") && s.contains("fn c()"), "窗口覆盖了小文件的全部内容");
    }

    #[test]
    fn source_snippet_out_of_range_line_clamps_with_note() {
        // T48 行契约：finding 声称的行号超出文件实际范围（模型谎报/carried 陈旧锚点）→
        // clamp 到文件实际范围 + 头部注记，不再像旧行为一样整体判死成占位（吞掉证据）。
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("tiny.rs"), "fn a() {}\nfn b() {}\n").unwrap(); // 只有 2 行
        let f = finding_at("tiny.rs", 1000, 1005); // 远超文件实际行数
        let s = source_snippet(tmp.path(), &f);
        assert_ne!(s, SNIPPET_UNAVAILABLE, "越界不该占位吞掉证据");
        assert!(s.contains("超出文件范围"), "头部须注记谎报: {s}");
        assert!(s.contains("fn a()") && s.contains("fn b()"), "clamp 后仍给出文件实际内容: {s}");
    }

    #[test]
    fn source_snippet_falls_back_to_window_over_cap() {
        // 超 64KB 回落快照路径（Task 4, E8-F）：文件 > 64KB → ±60 行窗快照，行为不倒退。
        // .txt 非受支持语言——绕开 Task 6 骨架中档（该级只对 lang_for 命中的语言生效），
        // 保持本测试原有的「无骨架可选、直落窗口」覆盖；受支持语言的骨架优先级见
        // source_snippet_over_64kb_rust_file_uses_skeleton_tier。
        let tmp = tempfile::tempdir().unwrap();
        let big = "x".repeat(70_000) + "\nfn target() {}\n";
        std::fs::write(tmp.path().join("big.txt"), &big).unwrap();
        let f = finding_at("big.txt", 2, 2);
        let s = source_snippet(tmp.path(), &f);
        assert!(s.contains("（现场快照）"), "超帽回落 ±60 快照: {s}");
    }

    // ── 判定层（批 D-2〔D17〕）：旧批式路径里仍成立的契约逐条移植过来 ──────────────────────────

    fn blocking(conf: f32) -> Finding {
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.confidence = conf;
        f
    }

    fn model(name: &str, family: &str) -> EffectiveModel {
        EffectiveModel {
            name: name.into(), display_name: None, id: name.rsplit('/').next().unwrap().into(),
            provider: name.split('/').next().unwrap().into(),
            role: crate::config::Role::Verifier, kind: crate::config::ProviderKind::Openai,
            base_url: None, context_window: 100_000, max_output_tokens: None,
            per_turn_timeout_secs: None, reasoning: false, thinking_level: None,
            family: family.into(), max_concurrency: None,
        }
    }

    /// 触发集与 `verdict` 同源：低于 block_threshold 或低于 conf_floor 的一律不触发——
    /// 它们在裁决层本就永不 blocking，复核纯烧 token。
    #[test]
    fn trigger_set_matches_the_verdict_blocking_predicate() {
        let low_sev = finding("t", Severity::Low, 1, "alpha");
        assert_eq!(decide(&low_sev, &[], Severity::Medium, 0.5, true), VerifyDecision::SkipNotTriggered);
        assert_eq!(decide(&blocking(0.4), &[], Severity::Medium, 0.5, true), VerifyDecision::SkipNotTriggered, "低于 floor 不触发");
        assert_eq!(decide(&blocking(0.5), &[], Severity::Medium, 0.5, true), VerifyDecision::Verify, "正好到 floor 就触发");
        assert_eq!(decide(&blocking(0.9), &[], Severity::Medium, 0.5, true), VerifyDecision::Verify);
    }

    /// 跳过四类各自命中，且分类可读（「没触发」与「触发了被免验」在 RunMeta 里必须分得开）。
    #[test]
    fn skip_reasons_are_classified() {
        let f = blocking(0.9);
        assert_eq!(decide(&f, &[], Severity::Medium, 0.5, false).reason(), "disabled");

        let mut carried = blocking(0.9);
        carried.origin = crate::domain::Origin::Carried;
        assert_eq!(decide(&carried, &[], Severity::Medium, 0.5, true).reason(), "carried");

        let card = DismissCard {
            id: f.id.clone(), status: "dismissed".into(), title: f.title.clone(), file: f.file.clone(),
            quote: f.quote.clone(), severity: "high".into(), reason: "人裁".into(),
            round: String::new(), updated_at: String::new(),
        };
        assert_eq!(decide(&f, std::slice::from_ref(&card), Severity::Medium, 0.5, true).reason(), "precedent");

        let mut consensus = blocking(0.9);
        consensus.suspicion_agreement = 2;
        assert_eq!(decide(&consensus, &[], Severity::Medium, 0.5, true).reason(), "consensus");
        assert!(!decide(&consensus, &[], Severity::Medium, 0.5, true).is_verify());
    }

    /// 免验读的是**区域**轴，不是模型轴：单 reviewer 配置下 `agreement` 恒为 1，
    /// 拿它当免验依据等于永不免验；反过来模型轴高也不该免验（§8.1 要的是两家族 scanner 标记）。
    #[test]
    fn consensus_skip_reads_the_suspicion_axis_not_the_model_axis() {
        let mut model_axis = blocking(0.9);
        model_axis.agreement = 3;
        model_axis.suspicion_agreement = 1;
        assert_eq!(decide(&model_axis, &[], Severity::Medium, 0.5, true), VerifyDecision::Verify, "模型轴高也照验");

        let mut region_axis = blocking(0.9);
        region_axis.agreement = 1;
        region_axis.suspicion_agreement = 2;
        assert_eq!(decide(&region_axis, &[], Severity::Medium, 0.5, true), VerifyDecision::SkipConsensus);
    }

    /// verifiers 活跃集非空 → 从中轮转（多个 verifier 摊开，不全压一个）。
    #[test]
    fn declared_verifiers_win_and_rotate() {
        let roles = RoleSets {
            scanners: vec![model("p/s", "deepseek")],
            reviewers: vec![model("p/r", "glm")],
            verifiers: vec![model("p/v1", "kimi"), model("p/v2", "qwen")],
        };
        let rev = model("p/r", "glm");
        let names: Vec<&str> = (0..4).map(|i| pick_verifier(&roles, &rev, i).unwrap().0.name.as_str()).collect();
        assert_eq!(names, ["p/v1", "p/v2", "p/v1", "p/v2"]);
        assert!(pick_verifier(&roles, &rev, 0).unwrap().1.is_none(), "显式声明的异族 verifier 不报 warning");
    }

    /// 显式声明的 verifier 也要查独立性（收口评审 #11）：轮到的那个与产出该 finding 的 reviewer
    /// 同模型（自己验自己）或同家族时，照用但留 warning——离线基线里同族那臂误驳 20% vs 异族 12%，
    /// 用户有权这么配，但不能不知道自己配了什么。
    #[test]
    fn declared_verifier_that_is_the_reviewer_itself_warns() {
        let same = RoleSets {
            scanners: vec![],
            reviewers: vec![model("p/r", "glm")],
            verifiers: vec![model("p/r", "glm")], // 现配置就是这个形状：一个模型兼两角色
        };
        let (m, warn) = pick_verifier(&same, &model("p/r", "glm"), 0).unwrap();
        assert_eq!(m.name, "p/r");
        assert!(warn.as_deref().is_some_and(|w| w.contains("同一个模型")), "自己验自己必须可观测: {warn:?}");

        let kin = RoleSets {
            scanners: vec![],
            reviewers: vec![model("p/r", "glm")],
            verifiers: vec![model("p/r2", "glm")], // 不同模型、同家族
        };
        let (_, warn) = pick_verifier(&kin, &model("p/r", "glm"), 0).unwrap();
        assert!(warn.as_deref().is_some_and(|w| w.contains("同家族")), "同族也要说一声: {warn:?}");
    }

    /// 未声明 verifiers → 自动异族，优先 reviewers 档再 scanners 档。
    #[test]
    fn auto_pick_prefers_a_cross_family_reviewer_then_scanner() {
        let roles = RoleSets {
            scanners: vec![model("p/s", "deepseek")],
            reviewers: vec![model("p/r", "glm"), model("p/r2", "kimi")],
            verifiers: vec![],
        };
        assert_eq!(pick_verifier(&roles, &model("p/r", "glm"), 0).unwrap().0.name, "p/r2", "异族 reviewer 优先");

        let only_scanner_cross = RoleSets {
            scanners: vec![model("p/s", "deepseek")],
            reviewers: vec![model("p/r", "glm")],
            verifiers: vec![],
        };
        assert_eq!(pick_verifier(&only_scanner_cross, &model("p/r", "glm"), 0).unwrap().0.name, "p/s", "没有异族 reviewer 就取 scanner 档");
    }

    /// 一个异族都没有 → 同族复核 + warning（§8.1 明写；现配置 glm 既是 reviewer 又是 verifier，正是这条路）。
    #[test]
    fn all_same_family_falls_back_with_a_warning() {
        let roles = RoleSets {
            scanners: vec![model("p/s", "glm")],
            reviewers: vec![model("p/r", "glm")],
            verifiers: vec![],
        };
        let (m, warn) = pick_verifier(&roles, &model("p/r", "glm"), 0).unwrap();
        assert_eq!(m.name, "p/r");
        assert!(warn.unwrap().contains("no cross-family verifier"), "同族复核必须可观测");
        assert!(pick_verifier(&RoleSets::default(), &model("p/r", "glm"), 0).is_none(), "一个模型都没有 → None");
    }
}
