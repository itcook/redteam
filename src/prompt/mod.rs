use crate::bundle::Bundle;
use crate::domain::{Mode, ReviewInput};

const CODE_BUNDLE_TPL: &str = include_str!("templates/adversarial-code-bundle.md");
const REFUTE_TPL: &str = include_str!("templates/adversarial-refute.md");
const REFUTE_DOC_TPL: &str = include_str!("templates/adversarial-refute-doc.md");
const DOC_BUNDLE_TPL: &str = include_str!("templates/adversarial-doc-bundle.md");

/// Detect the preferred output language from locale environment variables.
/// Reads LC_ALL → LC_MESSAGES → LANG in order, extracts the language prefix,
/// and maps it to a human-readable language name.
pub fn output_language() -> String {
    let raw = std::env::var("LC_ALL")
        .ok()
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var("LC_MESSAGES").ok().filter(|v| !v.is_empty()))
        .or_else(|| std::env::var("LANG").ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();

    // Extract language prefix: "zh_CN.UTF-8" → "zh_CN", "en_US" → "en_US", "C"/"POSIX" → ""
    let lang_part = raw.split('.').next().unwrap_or("").trim();

    // Map to a bilingual name (English + native)——纯英文语言名对部分模型约束力弱
    // （dogfood 20260703 r1：DeepSeek 输出全文马来语违约），母语名加固遵从。
    let lang_code = lang_part.split('_').next().unwrap_or("").to_lowercase();
    match lang_code.as_str() {
        "zh" => "Simplified Chinese (简体中文)".to_string(),
        "ja" => "Japanese (日本語)".to_string(),
        "ko" => "Korean (한국어)".to_string(),
        "fr" => "French (français)".to_string(),
        "de" => "German (Deutsch)".to_string(),
        "es" => "Spanish (español)".to_string(),
        _ => "English".to_string(),
    }
}

/// 构造 refute prompt：给定 claimed finding，让 skeptic 对抗复审。
/// 复用 review-as-refute——返回的 findings 空=upheld、非空=refuted（见模板 output_contract）。
/// mode 选模板：Code→code 模板、Doc→doc 模板（空 quote 渲染为遗漏占位说明）。
/// `dismissed_notes`：驳回台账判例（可空）——skeptic 见过往人裁，主题变体一致 refute。
/// `source_context`：finding 声称位置的工作树快照（`verify::source_snippet` 产出，可空）——
/// 免 refute 冷启动零 pack 裸奔重读（E7：解剖实测 14 调用/趟）；空串渲染为 `(none)`。
pub fn build_refute_prompt(
    finding: &crate::domain::Finding,
    mode: Mode,
    dismissed_notes: &str,
    source_context: &str,
) -> String {
    let tpl = match mode {
        Mode::Code => REFUTE_TPL,
        Mode::Doc => REFUTE_DOC_TPL,
    };
    let quote = if finding.quote.split_whitespace().next().is_none() {
        "(empty — the finding claims this is omitted from the document)".to_string()
    } else {
        finding.quote.clone()
    };
    let notes = if dismissed_notes.trim().is_empty() { "(none)" } else { dismissed_notes };
    let source = if source_context.trim().is_empty() { "(none)" } else { source_context };
    render(tpl, &[
        ("{{FILE}}", &finding.file),
        ("{{LINE_START}}", &finding.line_start.to_string()),
        ("{{LINE_END}}", &finding.line_end.to_string()),
        ("{{SEVERITY}}", finding.severity.as_str()),
        ("{{OUTPUT_LANGUAGE}}", &output_language()),
        ("{{TITLE}}", &finding.title),
        ("{{BODY}}", &finding.body),
        ("{{QUOTE}}", &quote),
        ("{{DISMISSED_NOTES}}", notes),
        ("{{SOURCE_CONTEXT}}", source),
    ])
}

/// 单 pass 模板渲染：逐段扫描模板，每个占位只被其自身值填充一次，**值内容不再被回扫**——
/// 结构性杜绝占位捕获与外部内容互染（r2 0b176180 / r3 eb685a65 dogfood 系列），
/// 且替换顺序不再是防线（无排序心智负担）。
fn render(tpl: &str, vars: &[(&str, &str)]) -> String {
    // 键之间不得有前缀包含（如 {{FILE}} 与 {{FILE_LIST}} 同集会让短键抢占长键前缀）。
    debug_assert!(
        vars.iter().all(|(a, _)| vars.iter().all(|(b, _)| a == b || !b.starts_with(a))),
        "render vars must be prefix-free"
    );
    let mut out = String::with_capacity(tpl.len() * 2);
    let mut rest = tpl;
    loop {
        let mut best: Option<(usize, &str, &str)> = None;
        for (k, v) in vars {
            if let Some(i) = rest.find(k) {
                if best.is_none_or(|(bi, _, _)| i < bi) {
                    best = Some((i, k, v));
                }
            }
        }
        match best {
            Some((i, k, v)) => {
                out.push_str(&rest[..i]);
                out.push_str(v);
                rest = &rest[i + k.len()..];
            }
            None => {
                out.push_str(rest);
                return out;
            }
        }
    }
}

/// 构造 doc 对抗 prompt：D 系攻击面清单为模板固有文本（E8-I 去实体化）、整篇文档内联、
/// 教模型遗漏类留空 quote。台账判例不再注入 review prompt（design 2026-07-03 #2：生成侧
/// 注入两会话十连穿透实证无效，判定后置 consolidate——机械预筛 + refute 判例裁决）。
pub fn build_doc_prompt(input: &ReviewInput, user_focus: &str) -> String {
    let spec_docs = if input.spec_docs.is_empty() {
        "(none)".to_string()
    } else {
        input.spec_docs.join(", ")
    };
    render(DOC_BUNDLE_TPL, &[
        ("{{SPEC_DOCS}}", &spec_docs),
        ("{{TARGET_LABEL}}", &input.target_label),
        ("{{OUTPUT_LANGUAGE}}", &output_language()),
        ("{{REVIEW_INPUT}}", &input.body),
        ("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus }),
    ])
}

/// 构造一个文件包的对抗 prompt：攻击面清单为模板固有文本（E8-I 去实体化），bundle diff 内联。
/// `other_files`：本包外全变更集文件清单（`bundle::other_files_manifest` 产出，可空）——
/// 防「无证据实现」式跨 bundle 误报。
pub fn build_bundle_prompt(
    target_label: &str,
    bundle: &Bundle,
    user_focus: &str,
    other_files: &str,
    incremental: &str,
    dossier: &str,
) -> String {
    let diffs_block = bundle
        .files
        .iter()
        .map(|fc| format!("### {}\n```diff\n{}\n```", fc.path, fc.diff))
        .collect::<Vec<_>>()
        .join("\n\n");

    let file_list = bundle
        .files
        .iter()
        .map(|fc| fc.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    render(CODE_BUNDLE_TPL, &[
        ("{{TARGET_LABEL}}", target_label),
        ("{{FILE_LIST}}", &file_list),
        ("{{OUTPUT_LANGUAGE}}", &output_language()),
        ("{{BUNDLE_DIFFS}}", &diffs_block),
        ("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus }),
        ("{{OTHER_CHANGED_FILES}}",
            if other_files.is_empty() { "(none — this bundle covers the entire changeset)" } else { other_files }),
        ("{{INCREMENTAL_ROUND}}", incremental),
        ("{{DOSSIER}}", dossier),
    ])
}

/// refute 判例注入渲染（design 2026-07-03 #7②）：按与被验 finding 的相关性排序填帽，
/// 替代旧「16hex 文件名伪随机取前 1/4」——同 file 优先、title 相似次之；
/// 单卡 600 / 总 4000 字符帽（帽内永远是最相关子集，台账体积与 prompt 解耦）。
pub fn render_precedents(finding: &crate::domain::Finding, cards: &[crate::db::DismissCard]) -> String {
    const PER_CARD_CHARS: usize = 600;
    const TOTAL_CHARS: usize = 4000;
    let mut ranked: Vec<(&crate::db::DismissCard, f32)> = cards
        .iter()
        .map(|c| {
            let mut score = crate::consolidate::dedup::title_jaccard(&finding.title, &c.title);
            if c.file == finding.file {
                score += 1.0; // 同 file 整级优先
            }
            (c, score)
        })
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.id.cmp(&b.0.id)));
    let mut out = String::new();
    for (c, _) in ranked {
        let quote_line = c.quote.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
        let entry: String = format!(
            "- [{}] {} {} | {} | quote: {} | rationale({}): {}\n",
            c.id, c.severity, c.file, c.title, quote_line, c.status, c.reason
        )
        .chars()
        .take(PER_CARD_CHARS)
        .collect();
        if out.chars().count() + entry.chars().count() > TOTAL_CHARS {
            break;
        }
        out.push_str(&entry);
    }
    out
}

/// M13 决议 4：<incremental_round> 段——仅增量轮渲染（全量路径传空串填占位）。
/// 内容四件：delta 口径 / 双层 manifest / claimed-resolved 顺带验证清单 / 原样再报指令。
/// 不注入 hunk→finding 映射（fix 意图不注入，防橡皮图章化）。claimed 项必须带原样
/// title+quote **全文**——机器匹配 = 模型按原文再报 → enrich 同 id → 与 resolve 记录碰撞。
pub fn render_incremental_section(
    delta_base: &str,
    two_layer_manifest: &str,
    claimed: &[crate::db::ResolveCard],
) -> String {
    const CLAIMED_TOTAL_CHARS: usize = 6000;
    let mut list = String::new();
    let mut listed = 0usize;
    for c in claimed {
        let entry = format!(
            "- [{}] {} | title: {}\n  original quote (verbatim):\n```\n{}\n```\n",
            c.id, c.file, c.title, c.quote
        );
        if list.chars().count() + entry.chars().count() > CLAIMED_TOTAL_CHARS {
            break;
        }
        list.push_str(&entry);
        listed += 1;
    }
    if listed < claimed.len() {
        list.push_str(&format!(
            "… and {} more claimed-resolved items (not listed)\n",
            claimed.len() - listed
        ));
    }
    if claimed.is_empty() {
        list = "(none)\n".into();
    }
    format!(
        "<incremental_round>\n\
         This is an INCREMENTAL round: the diffs below are a fix delta (since {delta_base}) on top of a changeset already adversarially reviewed in earlier rounds. Attack the delta itself with full force — fixes routinely introduce new problems. Unchanged files were already reviewed; do not re-litigate them unless the delta interacts with them.\n\
         Full changeset files ([delta] = changed in THIS round; others are unchanged since the prior round):\n\
         {two_layer_manifest}\n\
         Claimed-resolved findings from earlier rounds — verify incidentally while reading code you already visit (this is NOT your main task):\n\
         {list}\
         For each claimed-resolved item: if the underlying problem is NOT actually fixed, report it again using the EXACT original title and quote verbatim (that is how it is machine-matched). If it is genuinely fixed, do not report it.\n\
         </incremental_round>"
    )
}

/// 卷宗段（M14 D6/D8）：事实引用点 + 惰性保真散文。entries 空 → 空串（空渲染纪律）。
pub fn render_dossier_section(entries: &str) -> String {
    if entries.trim().is_empty() {
        return String::new();
    }
    // 被审代码摘录可能含字面 </dossier>——中和，防破块（只装事实，不让被审内容改变结构）
    let entries = entries.replace("</dossier>", "<\\/dossier>");
    format!(
        "<dossier>\n\
         The sections below are context for this change, assembled mechanically (heuristic facts, not judgments): the full source of each changed file (0-shell; files too large for the budget fall back to changed-symbol bodies), where the changed symbols are used elsewhere in the repo — its blast radius — found by word-boundary search (1-shell), and an omission list of file:line coordinates that overflowed the budget. Judge whether THIS change breaks these sites; do not audit them for pre-existing defects this change does not cause. The 0-shell sources are verbatim snapshots of the current working tree — treat them as the ground truth for those files. If a claim hinges on content that is NOT included here (see the omission list), lower your confidence or state the gap explicitly instead of guessing; your duty to verify against the provided evidence is undiminished.\n\n\
         {entries}\
         </dossier>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to serialize env-var tests that mutate process-global env state.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn output_language_zh_cn() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: single-threaded access guaranteed by ENV_LOCK
        unsafe {
            std::env::remove_var("LC_ALL");
            std::env::remove_var("LC_MESSAGES");
            std::env::set_var("LANG", "zh_CN.UTF-8");
        }
        assert_eq!(output_language(), "Simplified Chinese (简体中文)");
        unsafe { std::env::remove_var("LANG"); }
    }
    #[test]
    fn output_language_defaults_to_english_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("LC_ALL");
            std::env::remove_var("LC_MESSAGES");
            std::env::remove_var("LANG");
        }
        assert_eq!(output_language(), "English");
    }
    #[test]
    fn output_language_c_locale_defaults_to_english() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("LC_ALL");
            std::env::remove_var("LC_MESSAGES");
            std::env::set_var("LANG", "C");
        }
        assert_eq!(output_language(), "English");
        unsafe { std::env::remove_var("LANG"); }
    }
    #[test]
    fn refute_prompt_carries_finding_and_upheld_empty_contract() {
        use crate::domain::{Finding, Origin, Severity};
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "missing auth".into(),
            body: "no check before db write".into(), file: "src/a.rs".into(),
            line_start: 12, line_end: 14, confidence: 0.9, recommendation: "add check".into(),
            models: vec!["claude".into()], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "db.write(token)".into(), verify: None,
            previously_dismissed: false, origin: Origin::New,
        };
        let p = build_refute_prompt(&f, crate::domain::Mode::Code, "", "");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("src/a.rs") && p.contains("12"), "file:line injected");
        assert!(p.contains("missing auth") && p.contains("db.write(token)"), "title+quote injected");
        // upheld=空 findings 的契约必须在 prompt 中
        assert!(p.to_lowercase().contains("findings"), "output contract present");
        // 契约同源绊线：verdict 语义前缀（verify.rs::classify_refute 解析依据）+ 必填字段指引
        // （r2 20260702 契约不匹配、r1 20260703 upheld 误杀两案的防再漂移）。
        assert!(p.contains("\"REFUTED: \""), "refuted 前缀信号指引在场");
        assert!(p.contains("ONLY by the empty array"), "upheld=空数组排他表达在场");
        for field in ["severity", "file", "line_start", "line_end", "confidence", "recommendation"] {
            assert!(p.contains(field), "必填字段指引缺失: {field}");
        }
        // doc 模板同约束（双模板同源）
        let pd = build_refute_prompt(&f, crate::domain::Mode::Doc, "", "");
        assert!(pd.contains("\"REFUTED: \"") && pd.contains("ONLY by the empty array"), "doc 模板 verdict 信号同源");
        // 空台账 → 判例段渲染 (none)；非空 → 原样注入（主题变体跨轮一致抑制）
        assert!(p.contains("Precedents: (none)"), "empty notes render (none)");
        let p2 = build_refute_prompt(&f, crate::domain::Mode::Code, "台账: 旧误报X 已人裁驳回", "");
        assert!(p2.contains("台账: 旧误报X 已人裁驳回"), "ledger precedents injected");
    }

    #[test]
    fn refute_prompt_injects_source_context() {
        use crate::domain::{Finding, Origin, Severity};
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "missing auth".into(),
            body: "no check before db write".into(), file: "src/a.rs".into(),
            line_start: 12, line_end: 14, confidence: 0.9, recommendation: "add check".into(),
            models: vec!["claude".into()], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "db.write(token)".into(), verify: None,
            previously_dismissed: false, origin: Origin::New,
        };
        let p = build_refute_prompt(&f, crate::domain::Mode::Code, "", "src/a.rs:40-162（现场快照）\nfn x() {}");
        assert!(p.contains("Source at the claimed location"), "段标题在场");
        assert!(p.contains("fn x() {}"), "快照内容注入");
        let p2 = build_refute_prompt(&f, crate::domain::Mode::Code, "", "");
        assert!(p2.contains("(none)"), "空快照渲染占位");
    }

    #[test]
    fn refute_prompt_doc_omission_frames_empty_quote() {
        use crate::domain::{Finding, Mode, Origin, Severity};
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "missing rollback branch".into(),
            body: "no rollback path on failure".into(), file: "spec.md".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "specify rollback".into(),
            models: vec!["alpha".into()], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: String::new(), verify: None,
            previously_dismissed: false, origin: Origin::New,
        };
        let p = build_refute_prompt(&f, Mode::Doc, "", "");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.to_lowercase().contains("omit"), "doc refute frames omission");
        assert!(!p.contains("Quoted code"), "doc template (not code template)");
    }

    #[test]
    fn render_precedents_ranks_same_file_first_and_caps_total() {
        use crate::db::DismissCard;
        use crate::domain::{Finding, Origin, Severity};
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "injection via notes".into(),
            body: "b".into(), file: "src/a.rs".into(), line_start: 1, line_end: 1,
            confidence: 0.9, recommendation: "r".into(), models: vec![],
            agreement: 1, hard_constraint: false, quote: "q".into(), cluster_id: None,
            verify: None, previously_dismissed: false, origin: Origin::New,
        };
        let card = |id: &str, file: &str, title: &str, reason: &str| DismissCard {
            id: id.into(), status: "dismissed".into(), title: title.into(), file: file.into(),
            quote: "qq".into(), severity: "high".into(), reason: reason.into(),
            round: "r1".into(), updated_at: String::new(),
        };
        let cards = vec![
            card("1111111111111111", "src/other.rs", "unrelated topic entirely", "别处"),
            card("2222222222222222", "src/a.rs", "injection theme", "同文件判例"),
        ];
        let out = render_precedents(&f, &cards);
        assert!(out.find("2222222222222222").unwrap() < out.find("1111111111111111").unwrap(),
            "同 file 判例排前: {out}");
        assert!(out.contains("同文件判例") && out.contains("rationale"), "{out}");
        // 帽：大量长卡不超 4000 chars
        let many: Vec<DismissCard> = (0..50)
            .map(|i| card(&format!("{i:016x}"), "z.rs", "t", &"理".repeat(500)))
            .collect();
        assert!(render_precedents(&f, &many).chars().count() <= 4000, "总量帽生效");
    }

    /// E8-I 去实体化：攻击面清单是模板固有文本，非渲染产物——不再构造 rules vec。
    #[test]
    fn bundle_prompt_carries_builtin_attack_surface() {
        let bundle = bundle_one();
        let p = build_bundle_prompt("working tree", &bundle, "auth", "", "", "");
        assert!(p.contains("[L1] Security & Boundaries"), "code 攻击面固有: {p}");
        assert!(p.contains("[L5] Standards Compliance") && p.contains("hard_constraint=true"), "compliance 散文注记在场");
        assert!(!p.contains("{{RULES}}"), "占位符已烘干");
    }

    fn bundle_one() -> Bundle {
        use crate::evidence::git::FileChange;
        Bundle {
            files: vec![FileChange { path: "src/auth.rs".into(), diff: "+let t = secret();".into() }],
        }
    }

    #[test]
    fn bundle_prompt_interpolates_files_and_contract() {
        let bundle = bundle_one();
        let p = build_bundle_prompt("working tree", &bundle, "auth", "", "", "");
        assert!(!p.contains("{{"), "no residual placeholders");
        // 空清单 → 渲染「本包即全变更集」占位；非空 → 原样注入
        assert!(p.contains("(none — this bundle covers the entire changeset)"), "empty manifest placeholder");
        let pm = build_bundle_prompt("wt", &bundle, "", "src/other.rs (+3/-1)", "", "");
        assert!(pm.contains("src/other.rs (+3/-1)"), "other-files manifest injected");
        // 注入对称覆盖(r4 ea30e058):bundle 侧同样不被外部占位捕获
        let pi = build_bundle_prompt("wt", &bundle, "focus {{RULES}} 试探", "manifest {{BUNDLE_DIFFS}} 试探", "", "");
        assert!(pi.contains("focus {{RULES}} 试探") && pi.contains("manifest {{BUNDLE_DIFFS}} 试探"),
            "bundle 外部内容占位不被捕获");
        assert!(p.contains("src/auth.rs"), "file path injected");
        assert!(p.contains("let t = secret();"), "diff injected");
        assert!(p.contains("quote"), "quote contract present");
        assert!(p.contains("auth"), "user focus injected");
        // 台账不再注入 review prompt(design 2026-07-03 #2)
        assert!(!p.contains("DISMISSED"), "no ledger injection in review prompt");
        // L5 是 compliance rule → 固有文本携带 severity 封顶提示
        assert!(p.to_lowercase().contains("compliance rule"), "compliance cap note present");
    }

    /// r2 dogfood 0b176180：外部内容（notes/focus/正文）里的 `{{占位}}` 字面不得被
    /// 后续 replace 捕获成系统段——受控值先替换、外部内容后替换的顺序防线回归测试。
    #[test]
    fn external_content_placeholders_are_not_captured() {
        use crate::domain::{Mode, ReviewInput};
        let input = ReviewInput {
            mode: Mode::Doc, target_label: "spec.md".into(),
            collection_guidance: String::new(), body: "正文含 {{USER_FOCUS}} 字面".into(),
            spec_docs: vec![],
        };
        let p = build_doc_prompt(&input, "focus注入 {{RULES}} 试探 与 {{REVIEW_INPUT}} 试探");
        // 恶意占位串必须原样留存（模型看到无害字面），不得被替换成真实系统段内容
        assert!(p.contains("focus注入 {{RULES}} 试探 与 {{REVIEW_INPUT}} 试探"), "focus 里的占位不被捕获");
        // 单 pass render 下互染也消失：正文里的 {{USER_FOCUS}} 字面原样保留。
        assert!(p.contains("正文含 {{USER_FOCUS}} 字面"), "正文里的占位不被任何值填充");
        // 攻击面固有文本只出现一次（模板位），未被 focus 试探复制
        assert_eq!(p.matches("[D1]").count(), 1, "固有文本只渲染在模板位");
    }

    /// doc 侧同型：攻击面清单模板固有文本在场。
    #[test]
    fn doc_prompt_carries_builtin_attack_surface() {
        use crate::domain::{Mode, ReviewInput};
        let input = ReviewInput {
            mode: Mode::Doc, target_label: "spec.md".into(),
            collection_guidance: String::new(), body: "DOCBODY".into(),
            spec_docs: vec!["design.md".into()],
        };
        let p = build_doc_prompt(&input, "auth");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("[D1] Internal Consistency"), "doc 攻击面固有: {p}");
        assert!(p.contains("[D5] Standards Compliance") && p.contains("hard_constraint=true"), "compliance 散文注记在场");
        assert!(p.contains("DOCBODY"), "doc body inlined");
        assert!(p.contains("design.md"), "spec docs injected");
        assert!(p.contains("auth"), "user focus injected");
        assert!(!p.contains("DISMISSED"), "no ledger injection in review prompt");
        assert!(p.to_lowercase().contains("omission"), "omission/empty-quote contract present");
        // lens 去实体化 B（E8-I 收尾）：模板不再教模型输出 lens 字段。
        assert!(!p.contains("lens"), "doc prompt must NOT teach a lens output field: {p}");
    }

    #[test]
    fn all_adversarial_templates_carry_exhaustiveness_directive() {
        for t in [
            include_str!("templates/adversarial-doc-bundle.md"),
            include_str!("templates/adversarial-code-bundle.md"),
        ] {
            assert!(t.contains("Exhaustiveness:"), "穷尽性指令缺失（spec §1.3 分界原则）");
        }
    }

    #[test]
    fn doc_review_uses_progressive_retrieval_not_preload() {
        let t = include_str!("templates/adversarial-doc-bundle.md");
        assert!(t.contains("Retrieve progressively"), "doc-bundle 缺渐进检索指令（M6 §1）");
        assert!(!t.contains("read_file every code/spec file"), "膨胀元凶 first-step 全读指令未替换");
    }

    #[test]
    fn code_review_carries_bypass_class() {
        assert!(
            include_str!("templates/adversarial-code-bundle.md").contains("<bypass_class>"),
            "code-bundle 缺 bypass_class（M6 §2）"
        );
    }

    #[test]
    fn doc_templates_carry_temporal_stance() {
        // M11 spec §2：方向性 drift 指令双注入（主评审 + refute 第二道防线一致）
        assert!(
            include_str!("templates/adversarial-doc-bundle.md").contains("<temporal_stance>"),
            "doc-bundle 缺 temporal_stance 节"
        );
        assert!(
            include_str!("templates/adversarial-refute-doc.md").contains("Temporal check:"),
            "refute-doc 缺 Temporal check 段"
        );
    }

    #[test]
    fn incremental_section_renders_only_when_given() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        let bundle = Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "+x".into() }] };
        // 全量路径（空段）：模板无残留占位、无增量标记
        let p = build_bundle_prompt("wt", &bundle, "", "", "", "");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(!p.contains("<incremental_round>"), "全量评审不得出现增量段");
        // 增量路径：段完整注入
        let claimed = vec![crate::db::ResolveCard {
            id: "a1b2c3d4e5f60718".into(), title: "weak hash".into(), file: "src/a.rs".into(),
            quote: "let x = md5(pw);".into(), severity: "high".into(),
            reason: String::new(), round: "r1".into(), updated_at: String::new(),
        }];
        let sect = render_incremental_section("abc123def456", "src/a.rs [delta], src/b.rs", &claimed);
        let pi = build_bundle_prompt("wt", &bundle, "", "", &sect, "");
        assert!(pi.contains("<incremental_round>") && pi.contains("abc123def456"));
        assert!(pi.contains("src/a.rs [delta]"), "双层 manifest 注入");
        assert!(pi.contains("let x = md5(pw);") && pi.contains("weak hash"), "claimed 项带原样 title+quote");
        assert!(pi.contains("EXACT original title and quote"), "机器匹配指令在场");
        assert!(pi.contains("[L1]"), "攻击面固有文本一字不动仍在");
    }

    #[test]
    fn incremental_section_empty_claimed_and_overflow_cap() {
        let s = render_incremental_section("abc", "f.rs [delta]", &[]);
        assert!(s.contains("(none)"), "{s}");
        // 溢出：超总帽的 claimed 项截断并注记条数（静默截断=覆盖假象）
        let big: Vec<crate::db::ResolveCard> = (0..40).map(|i| crate::db::ResolveCard {
            id: format!("{i:016x}"), title: "t".into(), file: "f.rs".into(),
            quote: "q".repeat(400), severity: "high".into(),
            reason: String::new(), round: "r1".into(), updated_at: String::new(),
        }).collect();
        let s = render_incremental_section("abc", "", &big);
        assert!(s.contains("more claimed-resolved items (not listed)"), "{s}");
        assert!(s.chars().count() < 9000, "总量帽生效: {}", s.chars().count());
    }

    #[test]
    fn dossier_section_empty_renders_empty() {
        assert_eq!(render_dossier_section(""), "");
    }

    #[test]
    fn dossier_section_wraps_with_prose_and_scope_guard() {
        let s = render_dossier_section("## foo (fn)\nsrc/a.rs:3\n    foo();\n");
        assert!(s.starts_with("<dossier>"));
        assert!(s.contains("blast radius"), "D8 措辞在场");
        assert!(s.contains("do not audit them for pre-existing defects"), "范围划线在场");
        assert!(s.contains("duty to verify against the provided evidence is undiminished"));
        assert!(s.contains("## foo (fn)"), "entries 在场");
        assert!(s.trim_end().ends_with("</dossier>"));
    }

    #[test]
    fn dossier_section_neutralizes_closing_delimiter_in_entries() {
        // 被审文件的摘录里若含字面 </dossier> 不得破块
        let s = render_dossier_section("## foo (fn)\nsrc/a.rs:3\n    let x = \"</dossier>\";\n");
        // 结果里只应有一个真正的闭合标签（在最末尾），正文里的被中和
        assert_eq!(s.matches("</dossier>").count(), 1, "正文里的闭合标签被中和，只剩包裹用的那个: {s}");
        assert!(s.trim_end().ends_with("</dossier>"));
    }

    #[test]
    fn dossier_section_prose_mentions_changed_bodies_and_omission() {
        let s = render_dossier_section("## 改动源文（0 壳）\nsrc/a.rs:1-3\nfn t() {}\n");
        assert!(s.starts_with("<dossier>"));
        assert!(s.contains("full source") && s.contains("changed file"), "散文提及改动源文全文");
        assert!(s.contains("fall back to changed-symbol bodies"), "散文提及超预算回落语义");
        // 信任声明（无工具默认转正，2026-07-13 design §2.1）：0 壳=工作树逐字快照——不再指挥
        // 不存在的工具，证据不在场则降 conf 或说明缺口，而非命令重读。
        assert!(s.contains("verbatim snapshots of the current working tree"), "0 壳快照声明在场");
        assert!(s.contains("lower your confidence or state the gap"), "证据不在场的降 conf 指令在场");
        assert!(!s.contains("read_files"), "无工具默认下散文不得指挥不存在的工具");
        assert!(!s.contains("tool calls"), "无工具默认下散文不得提及 tool calls");
        assert!(s.trim_end().ends_with("</dossier>"));
    }

    #[test]
    fn bundle_prompt_carries_dossier_and_tools_reconciliation() {
        let b = crate::bundle::Bundle { files: vec![crate::evidence::git::FileChange {
            path: "src/a.rs".into(), diff: "@@ -1 +1 @@\n-a\n+b".into(),
        }] };
        let dossier = render_dossier_section("## foo (fn)\nsrc/a.rs:3\n    foo();\n");
        let p = build_bundle_prompt("wt", &b, "", "", "", &dossier);
        assert!(p.contains("<dossier>"), "卷宗块注入");
        assert!(p.contains("start from the files it names"), "<evidence> 卷宗起点句在场");
        assert!(p.contains("No tools are available in this session"), "无工具默认转正（2026-07-13 design §2.1）——不再指挥不存在的工具");
        // 空卷宗 → 无 <dossier> 块，行为不变
        let p2 = build_bundle_prompt("wt", &b, "", "", "", "");
        assert!(!p2.contains("<dossier>"), "空卷宗不渲染块");
    }
}
