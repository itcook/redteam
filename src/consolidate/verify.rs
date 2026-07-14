use crate::consolidate::dedup::{quote_overlap, title_jaccard};
use crate::db::DismissCard;
use crate::domain::{Finding, Mode, Severity, Verify, VerifyVerdict};
use crate::prompt::{build_refute_prompt, render_precedents};
use crate::provider::{LensRequest, ReviewProvider};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Semaphore;

/// refute 现场源码段窗口半径（行）与字节帽（design Task 2，E7：refute 复审零 pack 裸奔重读
/// 全文件，解剖实测 14 调用/趟——注入声称位置的快照给它一个起点，不代替它自行验证）。
const SNIPPET_WINDOW: usize = 60;
const SNIPPET_BYTE_CAP: usize = 24_000;
/// 读入帽：与本仓读路径基线对齐（tools::read_one=64KB / do_read_files=128KB /
/// positioning::safe_read=512KB，I2/T2）——对抗式威胁模型含「恶意 PR 塞大文件」，
/// refute 这条新读路径不得重开无界读的口子。
const SNIPPET_READ_MAX_BYTES: usize = 512 * 1024;
/// 锚点文件全文帽（Task 4, E8-F）：64KB 以内给整文件；超帽回落 ±60 行快照（行为不倒退）。
const SNIPPET_FULL_FILE_CAP: usize = 64_000;
const SNIPPET_UNAVAILABLE: &str = "(source unavailable — judge from the finding's quote alone and weigh confidence accordingly)";

/// finding 声称位置的工作树快照：`root.join(&f.file)` 按 `[line_start-60, line_end+60]`
/// 行窗（1-based 夹紧）取内容，头带 `path:起-止（现场快照）`。超 24_000 字节按 char 边界
/// 截断+注记；文件读不到 / 为空 / 窗越界（如 line_start 越过文件末尾太远）→ 统一占位——
/// skeptic 不得被喂假装"已验证"的空内容，占位显式指示它自行用工具读。
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

    // 文件成功读入且 <= 64KB → 返回整文件，头 {path}:1-{N}（锚点文件全文）；
    // 超帽回落 ±60 行快照路径（行为不倒退）。
    if !hit_cap && content.len() <= SNIPPET_FULL_FILE_CAP {
        let lines: Vec<&str> = content.lines().collect();
        let n = lines.len();
        return format!("{}:1-{}（锚点文件全文）\n{}", f.file, n, lines.join("\n"));
    }

    let mut lines: Vec<&str> = content.lines().collect();
    if hit_cap {
        lines.pop(); // 帽处截断的尾行可能不完整，弃之
    }
    let n = lines.len();
    let start = (f.line_start as usize).saturating_sub(SNIPPET_WINDOW).max(1);
    let end = (f.line_end as usize + SNIPPET_WINDOW).min(n);
    // 窗越界，或命中帽且请求窗口伸进未读部分 → 安全降级占位（不喂被帽掐掉的假"现场"，模型自己读）。
    if start > end || (hit_cap && f.line_end as usize + SNIPPET_WINDOW > n) {
        return SNIPPET_UNAVAILABLE.to_string();
    }
    let mut out = format!("{}:{}-{}（现场快照）\n{}", f.file, start, end, lines[start - 1..end].join("\n"));
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

/// T43 层①：refute structured-output parse 失败（repair 重试后仍失败）的前缀嗅探。
/// provider 的 Parse 错误携带原始输出前 120 字符（diagnostic_parse_error）——refute 契约的
/// verdict 前缀（REFUTED/UPHELD）在头部，JSON 包装失败通常不吞语义。只嗅 strip 包装后的
/// 头部前缀，不做全文词搜——refute 散文里 "would be REFUTED if" 会误判。嗅不出 → None，
/// 交层③兜底（保守 Upheld 保 blocking 力：假 approve 吞真问题是 r1 20260703 老事故方向）。
fn sniff_refute_verdict(parse_msg: &str) -> Option<VerifyVerdict> {
    // diagnostic 格式 "…(finish_reason=…): <原文>"——固定前缀先于原文出现，取第一个 "): " 之后。
    let raw = &parse_msg[parse_msg.find("): ")? + 3..];
    let stripped = raw.trim_start_matches(['*', '#', '`', '"', '\'', ' ', '\n']);
    let lower: String = stripped.chars().take(24).collect::<String>().to_lowercase();
    if lower.starts_with("refuted") {
        Some(VerifyVerdict::Refuted)
    } else if lower.starts_with("upheld") || lower.starts_with("claim upheld") {
        Some(VerifyVerdict::Upheld)
    } else {
        None
    }
}

/// 机械预筛（design 2026-07-03 #3）：finding 是否疑似某活跃判例的主题变体。
/// 召回优先（阈值宽）、精判交 refute（判例注入的 skeptic 终审）——机械匹配对
/// 换措辞天然半盲，它只负责把候选送进 refute，不做终审。
/// 精确 id 命中不算（那是 mark_dismissed 豁免的职责，refute 纯浪费）。
fn matches_precedent(f: &Finding, c: &DismissCard) -> bool {
    if f.id == c.id {
        return false;
    }
    let t = title_jaccard(&f.title, &c.title);
    (f.file == c.file && t >= 0.25) || t >= 0.5 || quote_overlap(&f.quote, &c.quote) >= 0.5
}

/// 选交叉 model：第一个不在 finding.models 里的；没有则退化用 models[0]（自审）。
fn pick_cross_model<'a>(models: &'a [String], finding_models: &[String]) -> Option<&'a str> {
    models
        .iter()
        .find(|m| !finding_models.contains(m))
        .or_else(|| models.first())
        .map(|s| s.as_str())
}

/// 对单条孤证做 refute：选交叉 model → build_refute_prompt → provider.run。
/// 返回 (Verify, 可选 warning, usage)：空 findings=Upheld、非空=Refuted；调用失败=保守 Upheld
/// 且携带 warning——验证层静默失效必须可观测（dogfood 20260703 r1：4/6 refute 失败零告警）。
/// usage=(input,output,cached)：无模型/调用失败时为 (0,0,0)（T03：refute 趟 token 曾被丢弃，未入任何账）。
async fn refute_one(
    provider: &dyn ReviewProvider, finding: &Finding, models: &[String], mode: Mode, root: &Path,
    precedents: &[DismissCard],
) -> (Verify, Option<String>, (u64, u64, u64)) {
    let model = match pick_cross_model(models, &finding.models) {
        Some(m) => m.to_string(),
        None => return (
            Verify { verdict: VerifyVerdict::Upheld, model: String::new(), note: "no model".into() },
            None,
            (0, 0, 0),
        ),
    };
    let notes = render_precedents(finding, precedents);
    let ctx = source_snippet(root, finding);
    let prompt = build_refute_prompt(finding, mode, &notes, &ctx);
    match provider.run(LensRequest { model: model.clone(), prompt }).await {
        Ok(call) => {
            let (verdict, note, contract_warn) = classify_refute(&call.findings);
            let warn = contract_warn.map(|w| format!("refute [{}] model {model}: {w}", finding.id));
            let usage = (call.input_tokens, call.output_tokens, call.cached_input_tokens);
            (Verify { verdict, model, note }, warn, usage)
        }
        // 保守：refute 调用失败不冤枉 finding（保留其 blocking 力），但失败必须进 warnings。
        // T43 层①：Parse 失败先嗅原文头部 verdict 前缀（层② repair 重试已由 tool_loop 内建——
        // 覆盖 FINALIZE 收尾轮与 toolless 默认路径首轮直出两条 parse 失败分支，Fix 4：final-review）。
        Err(e) => {
            let sniffed = match &e {
                crate::provider::ProviderError::Parse(msg) => sniff_refute_verdict(msg),
                _ => None,
            };
            match sniffed {
                Some(verdict) => {
                    let warn = format!("refute [{}] model {model}: parse failed, verdict sniffed from raw prefix: {e}", finding.id);
                    (Verify { verdict, model, note: "sniffed from unparsed refute output".into() }, Some(warn), (0, 0, 0))
                }
                None => {
                    let warn = format!("refute [{}] model {model}: {e}", finding.id);
                    (Verify { verdict: VerifyVerdict::Upheld, model, note: "refute unavailable".into() }, Some(warn), (0, 0, 0))
                }
            }
        }
    }
}

/// targeted refute 目标集（单一真相源：verify_findings 执行与 phase 打点的 targets 数共用，
/// 防两处筛选逻辑漂移）。层③：孤证 blocking 或预筛命中判例变体；精确 id 在台账者除外。
/// M13：origin=carried 不进 targets——出生轮已过孤证 refute 与判例预筛。
pub fn refute_targets(
    findings: &[Finding],
    precedents: &[DismissCard],
    block_threshold: Severity,
) -> Vec<usize> {
    let exact_ids: std::collections::HashSet<&str> = precedents.iter().map(|c| c.id.as_str()).collect();
    findings
        .iter()
        .enumerate()
        .filter(|(_, f)| f.origin != crate::domain::Origin::Carried)
        .filter(|(_, f)| !exact_ids.contains(f.id.as_str()))
        .filter(|(_, f)| {
            (f.agreement == 1 && f.severity >= block_threshold)
                || precedents.iter().any(|c| matches_precedent(f, c))
        })
        .map(|(i, _)| i)
        .collect()
}

/// refute 输出 → verdict：语义前缀优先，非空性只是违约 fallback（附 warning）。
/// r1 20260703 实证：六字段必填契约后，守约模型想表达 upheld 也会输出 finding
/// （title "Claim upheld: …"），按「非空即 refuted」会静默吞掉真问题。
fn classify_refute(
    findings: &[crate::domain::RawFinding],
) -> (VerifyVerdict, String, Option<&'static str>) {
    let Some(first) = findings.first() else {
        return (VerifyVerdict::Upheld, String::new(), None);
    };
    let title = first.title.trim();
    let lower = title.to_lowercase();
    if lower.starts_with("refuted") {
        // 剥机器信号前缀，note 保留纯理由（"refuted" 为 ASCII，字节切片安全）。
        let note = title["refuted".len()..].trim_start_matches([':', '：', ' ']).to_string();
        (VerifyVerdict::Refuted, note, None)
    } else if lower.starts_with("upheld") || lower.starts_with("claim upheld") {
        (VerifyVerdict::Upheld, title.to_string(), None)
    } else {
        // 违约输出：无 verdict 前缀。沿用旧启发式（非空=refuted）保住变体压制力，但漂移必须可观测。
        (
            VerifyVerdict::Refuted,
            title.to_string(),
            Some("verdict prefix missing in refute output; non-empty treated as refuted (contract drift)"),
        )
    }
}

/// 三层 targeted-verify（S3-b）+ 判例预筛扩面（design 2026-07-03 #3）。
/// 层①：agreement>=2 跳过（多模型已互证）。层②：引用存在性由 positioning 已砍编造。
/// 层③：孤证 blocking（agreement==1 && severity>=block_threshold）**或预筛命中判例变体**
/// （后者无视 agreement/severity——review 不再注入勿再报后，变体可能多模型同报/低严出现）。
/// 精确 id 已在台账者不 refute（mark_dismissed 将豁免，烧 LLM 纯浪费）。
/// refuted → verify=Refuted 且 confidence=0.0（降下 floor，不翻 verdict）；upheld → verify=Upheld。
/// Semaphore 限 max_concurrency。
/// `precedents`：活跃判例卡——refute prompt 按相关性渲染注入，skeptic 见过往人裁，
/// 主题变体（换文件/措辞/锚点）跨轮一致地被 refute（dogfood 20260702 最高信号缺陷）。
/// 返回第三元素 = refute 趟累计 usage（input,output,cached）；无 targets 时为 (0,0,0)
/// （T03：调用方须把此值折进总账，refute 趟此前从未入过任何账）。
/// `root`：finding.file 的解析基点——喂 `source_snippet`（Task 2），非目标场景（如 doc 模式无
/// established root）传 `Path::new(".")` 即可，读不到时 snippet 自行退化占位。
#[allow(clippy::too_many_arguments)]
pub async fn verify_findings(
    mut findings: Vec<Finding>,
    provider: &dyn ReviewProvider,
    models: &[String],
    block_threshold: Severity,
    max_concurrency: usize,
    mode: Mode,
    root: &Path,
    precedents: &[DismissCard],
) -> (Vec<Finding>, Vec<String>, (u64, u64, u64)) {
    let targets = refute_targets(&findings, precedents, block_threshold);
    if targets.is_empty() {
        return (findings, Vec::new(), (0, 0, 0));
    }
    let sem = Arc::new(Semaphore::new(max_concurrency.max(1)));
    let results = futures::future::join_all(targets.iter().map(|&i| {
        let sem = sem.clone();
        let f = &findings[i];
        async move {
            let _permit = sem.acquire().await.expect("semaphore not closed");
            refute_one(provider, f, models, mode, root, precedents).await
        }
    }))
    .await;
    let mut warnings = Vec::new();
    let mut usage = (0u64, 0u64, 0u64);
    for (&i, (v, warn, u)) in targets.iter().zip(results) {
        if v.verdict == VerifyVerdict::Refuted {
            // 证伪 → 降 confidence 到 0（作为信号，md 渲染/排序可用）。
            // verdict 的实质排除已落在 compute_verdict（显式 verify==Refuted 不 blocking），
            // 不再依赖该 confidence 数值——故退化配置 conf_floor=0 下亦安全。
            findings[i].confidence = 0.0;
        }
        findings[i].verify = Some(v);
        warnings.extend(warn);
        usage.0 += u.0;
        usage.1 += u.1;
        usage.2 += u.2;
    }
    (findings, warnings, usage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Finding, Mode, Origin, Severity, VerifyVerdict};
    use crate::provider::fake::FakeProvider;
    use crate::domain::RawFinding;
    use std::collections::BTreeMap;

    fn finding(title: &str, sev: Severity, agreement: u32, model: &str) -> Finding {
        Finding {
            id: title.into(), severity: sev, title: title.into(), body: "b".into(),
            file: "a.rs".into(), line_start: 1, line_end: 1, confidence: 0.9,
            recommendation: "r".into(), lenses: vec!["L1".into()], models: vec![model.into()],
            agreement, hard_constraint: false, cluster_id: None, quote: "q".into(), verify: None,
            previously_dismissed: false, origin: Origin::New,
        }
    }
    fn finding_at(file: &str, line_start: u32, line_end: u32) -> Finding {
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.file = file.into();
        f.line_start = line_start;
        f.line_end = line_end;
        f
    }
    fn rawf(title: &str) -> RawFinding {
        RawFinding { severity: Severity::High, title: title.into(), body: "b".into(), file: "a.rs".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: "q".into(), lens: None }
    }

    #[test]
    fn source_snippet_windows_and_clamps() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        // 大于 64KB 的 fixture，以测试 ±60 行窗口逻辑（不触发全文升档）
        let body: String = (1..=10_000).map(|i| format!("line{i}\n")).collect();
        std::fs::write(root.join("src/a.rs"), &body).unwrap();
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.file = "src/a.rs".into();
        f.line_start = 100;
        f.line_end = 102;
        let s = source_snippet(root, &f);
        assert!(s.contains("line40") && s.contains("line162"), "±60 行窗: {s}");
        assert!(!s.contains("line39\n") && !s.contains("line163"), "窗外不含");
        assert!(s.contains("src/a.rs:40-162"), "快照头带范围");
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
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let line = "码".repeat(100); // 300 字节/行
        // 500 行 × 301 字节 ≈ 150KB > 64KB，不触发全文升档，进 ±60 窗快照路径
        let body: String = (0..500).map(|_| format!("{line}\n")).collect();
        std::fs::write(root.join("big.rs"), &body).unwrap();
        let mut f = finding("t", Severity::High, 1, "alpha");
        f.file = "big.rs".into();
        f.line_start = 61;
        f.line_end = 61;
        let s = source_snippet(root, &f); // 窗口 121 行 × ~301B ≈ 36KB > 24KB 帽
        assert!(s.ends_with("…（截断，超 24KB 帽）"), "截断注记在场: …{}", &s[s.len().min(s.len().saturating_sub(60))..]);
        assert!(s.len() <= SNIPPET_BYTE_CAP + "\n…（截断，超 24KB 帽）".len(), "截断到帽内: {}", s.len());
        assert!(s.starts_with("big.rs:1-121（现场快照）"), "快照头仍在");
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
    fn source_snippet_upgrades_to_full_file_within_cap() {
        // 64KB 帽分支（Task 4, E8-F）：文件成功读入且 <= 64KB → 整文件，头 {path}:1-{N}（锚点文件全文）
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("small.rs"), "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let f = finding_at("small.rs", 2, 2);
        let s = source_snippet(tmp.path(), &f);
        assert!(s.contains("（锚点文件全文）"), "64KB 内升档全文: {s}");
        assert!(s.contains("fn a()") && s.contains("fn c()"), "含锚点行窗之外的内容");
    }

    #[test]
    fn source_snippet_falls_back_to_window_over_cap() {
        // 超 64KB 回落快照路径（Task 4, E8-F）：文件 > 64KB → ±60 行窗快照，行为不倒退。
        let tmp = tempfile::tempdir().unwrap();
        let big = "x".repeat(70_000) + "\nfn target() {}\n";
        std::fs::write(tmp.path().join("big.rs"), &big).unwrap();
        let f = finding_at("big.rs", 2, 2);
        let s = source_snippet(tmp.path(), &f);
        assert!(s.contains("（现场快照）"), "超帽回落 ±60 快照: {s}");
    }

    #[test]
    fn classify_refute_prefix_semantics() {
        // 空 → upheld（原契约不变）
        let (v, note, w) = classify_refute(&[]);
        assert_eq!((v, note.as_str(), w), (VerifyVerdict::Upheld, "", None));
        // REFUTED 前缀 → refuted，note 剥前缀
        let (v, note, w) = classify_refute(&[rawf("REFUTED: guard exists at L10")]);
        assert_eq!(v, VerifyVerdict::Refuted);
        assert_eq!(note, "guard exists at L10");
        assert!(w.is_none());
        // upheld 语义前缀 → upheld（r1 12bbeb4c 型误杀不再发生）
        let (v, _, w) = classify_refute(&[rawf("Claim upheld: body interpolated raw")]);
        assert_eq!(v, VerifyVerdict::Upheld);
        assert!(w.is_none());
        // 无前缀违约 → fallback refuted + contract-drift warning
        let (v, note, w) = classify_refute(&[rawf("actually guarded above")]);
        assert_eq!(v, VerifyVerdict::Refuted);
        assert_eq!(note, "actually guarded above");
        assert!(w.unwrap().contains("contract drift"));
    }

    #[tokio::test]
    async fn lone_blocking_refuted_drops_confidence_and_marks() {
        // skeptic 返回非空（找到反驳）→ refuted；confidence 降 0
        let provider = FakeProvider::with("beta", vec![rawf("actually guarded above")]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let (out, _w, _u) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(out[0].verify.as_ref().unwrap().model, "beta", "交叉选了非 alpha 的 model");
        assert_eq!(out[0].confidence, 0.0, "refuted 降 confidence 下 floor");
    }

    #[tokio::test]
    async fn lone_blocking_upheld_keeps_confidence() {
        // skeptic 返回空（守不住反驳）→ upheld；confidence 不动
        let provider = FakeProvider::with("beta", vec![]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let (out, _w, _u) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld);
        assert_eq!(out[0].confidence, 0.9);
    }

    #[tokio::test]
    async fn refute_usage_sums_across_targets() {
        // T03：refute 趟 usage 曾被 refute_one 丢弃，run.usage 从未见过这笔账。
        // 两条孤证都进 targets → 两次 provider.run 各记 100/20/5 → 累计应为 200/40/10。
        let provider = FakeProvider::with_tokens("beta", vec![rawf("actually guarded")], (100, 20, 5));
        let f1 = finding("missing auth 1", Severity::High, 1, "alpha");
        let f2 = finding("missing auth 2", Severity::High, 1, "alpha");
        let (_out, _w, usage) = verify_findings(
            vec![f1, f2], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[],
        ).await;
        assert_eq!(usage, (200, 40, 10), "两次 refute 调用的 token 须累加，不能被丢弃");
    }

    #[tokio::test]
    async fn no_targets_yields_zero_usage() {
        // 无 refute target（低于 block_threshold）→ 未发生任何 provider.run 调用 → usage 为 (0,0,0)。
        let provider = FakeProvider::with_tokens("beta", vec![rawf("x")], (100, 20, 5));
        let f = finding("minor", Severity::Low, 1, "alpha");
        let (_out, _w, usage) = verify_findings(
            vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[],
        ).await;
        assert_eq!(usage, (0, 0, 0));
    }

    #[tokio::test]
    async fn consensus_finding_is_skipped() {
        // agreement>=2（层①）→ 不 refute，verify 留 None
        let provider = FakeProvider::with("beta", vec![rawf("x")]);
        let f = finding("agreed bug", Severity::Critical, 2, "alpha");
        let (out, _w, _u) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[]).await;
        assert!(out[0].verify.is_none());
        assert_eq!(out[0].confidence, 0.9);
    }

    #[tokio::test]
    async fn below_block_threshold_is_skipped() {
        // severity < block_threshold → 不 refute
        let provider = FakeProvider::with("beta", vec![rawf("x")]);
        let f = finding("minor", Severity::Low, 1, "alpha");
        let (out, _w, _u) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[]).await;
        assert!(out[0].verify.is_none());
    }

    #[tokio::test]
    async fn doc_omission_lone_blocking_refuted() {
        use crate::domain::Mode;
        // doc 模式：空引用遗漏类孤证 High → refute（doc 模板路径）。skeptic 返回非空 → refuted。
        let provider = FakeProvider::with("beta", vec![rawf("actually specified in section 3")]);
        let mut f = finding("missing rollback branch", Severity::High, 1, "alpha");
        f.quote = String::new();
        let (out, _w, _u) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Doc, Path::new("."), &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted);
        assert_eq!(out[0].confidence, 0.0);
    }

    fn card(id: &str, file: &str, title: &str) -> DismissCard {
        DismissCard {
            id: id.into(), status: "dismissed".into(), title: title.into(), file: file.into(),
            quote: "ledger quote".into(), severity: "high".into(), reason: "既往误报".into(),
            round: "r1".into(), updated_at: String::new(),
        }
    }

    #[tokio::test]
    async fn precedent_prescreen_forces_refute_despite_consensus() {
        // agreement=2 本该层① 跳过，但预筛命中判例变体（同 file + title 相似）→ 强制进 refute
        let provider = FakeProvider::with("beta", vec![rawf("same theme as precedent")]);
        let mut f = finding("prompt injection via ledger notes", Severity::Medium, 2, "alpha");
        f.models = vec!["alpha".into(), "gamma".into()];
        let cards = vec![card("ffffffffffffffff", "a.rs", "prompt injection via dismissed notes")];
        let (out, _w, _u) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::High, 4, Mode::Code, Path::new("."), &cards).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted,
            "预筛命中 → 无视 agreement/severity 进 refute");
    }

    #[tokio::test]
    async fn exact_ledger_id_skips_refute() {
        // 精确 id 已在台账 → mark_dismissed 将豁免，refute 纯浪费 → 跳过
        let provider = FakeProvider::with("beta", vec![rawf("would refute")]);
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let cards = vec![card(&f.id.clone(), "a.rs", "missing auth")];
        let (out, _w, _u) = verify_findings(vec![f], &provider, &["alpha".into(), "beta".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &cards).await;
        assert!(out[0].verify.is_none(), "台账精确命中不烧 refute");
    }

    #[tokio::test]
    async fn refute_failure_is_conservatively_upheld() {
        // provider 对所选 model 无 script → 调用失败 → 保守 upheld（保留 finding 的 blocking 力）
        let provider = FakeProvider::with("alpha", vec![]); // 只有 alpha，单 model
        let f = finding("missing auth", Severity::High, 1, "alpha");
        // models 只有 alpha → 退化自审 alpha；但 FakeProvider 有 alpha script(空) → upheld
        let (out, w, _u) = verify_findings(vec![f], &provider, &["alpha".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld);
        assert!(w.is_empty(), "成功 upheld 无 warning");

        // 真失败：所选 model 无 script → 保守 upheld + warning 上浮（验证层失效可观测）
        let provider2 = FakeProvider { scripted: BTreeMap::new(), tokens: (0, 0, 0) };
        let f2 = finding("missing auth", Severity::High, 1, "ghost");
        let (out2, w2, _u2) = verify_findings(vec![f2], &provider2, &["ghost".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[]).await;
        assert_eq!(out2[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Upheld, "调用失败保守 upheld");
        assert_eq!(out2[0].confidence, 0.9, "upheld 不降 confidence");
        assert_eq!(w2.len(), 1, "refute 失败必须产出 warning");
        assert!(w2[0].starts_with("refute [") && w2[0].contains("model ghost"), "warning 带 finding id 与 model: {}", w2[0]);
    }

    #[test]
    fn carried_findings_never_enter_refute_targets() {
        use crate::domain::Origin;
        // 决议 5：携带项出生轮已过孤证 refute + 判例预筛，重复烧 LLM 纯浪费；
        // 台账精确 id 豁免（mark_dismissed）每轮照常、免费，不受影响。
        let mut carried = finding("lone blocking", Severity::High, 1, "alpha");
        carried.origin = Origin::Carried;
        let fresh = finding("new lone blocking", Severity::High, 1, "alpha");
        let cards = vec![card("ffffffffffffffff", "a.rs", "lone blocking")]; // 判例变体也不打 carried
        let targets = refute_targets(&[carried, fresh], &cards, Severity::Medium);
        assert_eq!(targets, vec![1], "只有 origin=new 的孤证进 targets（carried 双路径均剔除）");
    }

    #[test]
    fn sniff_refute_verdict_prefix_semantics_only() {
        // T43 层①：markdown 包装的 verdict 前缀可救回
        let m = "structured-output parse failed: final JSON parse failed (finish_reason=stop): **REFUTED: The claim is false because…";
        assert_eq!(sniff_refute_verdict(m), Some(VerifyVerdict::Refuted));
        let u = "structured-output parse failed: final JSON parse failed (finish_reason=stop): Claim upheld: the guard is missing";
        assert_eq!(sniff_refute_verdict(u), Some(VerifyVerdict::Upheld));
        // 截断 JSON 头 → 嗅不出，交兜底
        let j = r#"structured-output parse failed: final JSON parse failed (finish_reason=stop): {"findings": ["#;
        assert_eq!(sniff_refute_verdict(j), None);
        // 只嗅头部前缀：正文提及 REFUTED 不得误判（"would be REFUTED if…"）
        let mid = "structured-output parse failed: final JSON parse failed (finish_reason=stop): The claim would be REFUTED if callers pass empty vec";
        assert_eq!(sniff_refute_verdict(mid), None);
    }

    /// 恒返 Parse 错误的 provider——模拟 repair 后仍 parse 失败、但原文头部带 verdict 前缀。
    struct ParseErrProvider(String);
    #[async_trait::async_trait]
    impl crate::provider::ReviewProvider for ParseErrProvider {
        async fn run(&self, _req: crate::provider::LensRequest)
            -> Result<crate::provider::LensCall, crate::provider::ProviderError> {
            Err(crate::provider::ProviderError::Parse(self.0.clone()))
        }
    }

    #[tokio::test]
    async fn refute_parse_failure_sniffs_refuted_prefix() {
        // T43 实证场景（粒度 A/B 两跑）：refute 意图 REFUTED 但 JSON 包装失败 → 现状按原 conf 存活
        // → verdict 假 reject。嗅探后应判 Refuted、confidence 归零，并留 warning 可观测。
        let provider = ParseErrProvider(
            "structured-output parse failed: final JSON parse failed (finish_reason=stop): **REFUTED: trailing text".into());
        let f = finding("missing auth", Severity::High, 1, "alpha");
        let (out, w, _u) = verify_findings(vec![f], &provider, &["alpha".into()], Severity::Medium, 4, Mode::Code, Path::new("."), &[]).await;
        assert_eq!(out[0].verify.as_ref().unwrap().verdict, VerifyVerdict::Refuted, "嗅探救回 REFUTED 语义");
        assert_eq!(out[0].confidence, 0.0, "refuted 照常降 conf 下 floor");
        assert_eq!(w.len(), 1, "嗅探路径仍必须产出 warning（可观测）");
        assert!(w[0].contains("sniffed"), "warning 注明嗅探来源: {}", w[0]);
    }
}
