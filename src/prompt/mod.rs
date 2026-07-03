use crate::bundle::Bundle;
use crate::domain::{Mode, ReviewInput};
use crate::lens::Lens;

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

    // Map to readable name by two-letter language code prefix
    let lang_code = lang_part.split('_').next().unwrap_or("").to_lowercase();
    match lang_code.as_str() {
        "zh" => "Simplified Chinese".to_string(),
        "ja" => "Japanese".to_string(),
        "ko" => "Korean".to_string(),
        "fr" => "French".to_string(),
        "de" => "German".to_string(),
        "es" => "Spanish".to_string(),
        "en" => "English".to_string(),
        _ => "English".to_string(),
    }
}

/// 构造 refute prompt：给定 claimed finding，让 skeptic 对抗复审。
/// 复用 review-as-refute——返回的 findings 空=upheld、非空=refuted（见模板 output_contract）。
/// mode 选模板：Code→code 模板、Doc→doc 模板（空 quote 渲染为遗漏占位说明）。
pub fn build_refute_prompt(finding: &crate::domain::Finding, mode: Mode) -> String {
    let tpl = match mode {
        Mode::Code => REFUTE_TPL,
        Mode::Doc => REFUTE_DOC_TPL,
    };
    let quote = if finding.quote.split_whitespace().next().is_none() {
        "(empty — the finding claims this is omitted from the document)".to_string()
    } else {
        finding.quote.clone()
    };
    tpl.replace("{{FILE}}", &finding.file)
        .replace("{{LINE_START}}", &finding.line_start.to_string())
        .replace("{{LINE_END}}", &finding.line_end.to_string())
        .replace("{{TITLE}}", &finding.title)
        .replace("{{SEVERITY}}", finding.severity.as_str())
        .replace("{{BODY}}", &finding.body)
        .replace("{{QUOTE}}", &quote)
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}

/// 把 rule 集格式化为关注清单（code-bundle 与 doc-bundle 共用）。
fn rules_block(rules: &[&Lens]) -> String {
    rules
        .iter()
        .map(|l| {
            let cap = if l.max_severity.is_some() {
                " (compliance rule: severity MUST be low or medium unless an explicit hard project constraint, then set hard_constraint=true)"
            } else {
                ""
            };
            format!("- [{}] {}: {}{}", l.id, l.title, l.focus, cap)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 构造 doc 对抗 prompt：全 D 系 rule 注入为一份关注清单、整篇文档内联、教模型遗漏类留空 quote。
pub fn build_doc_prompt(
    input: &ReviewInput,
    rules: &[&Lens],
    user_focus: &str,
    dismissed_notes: &str,
) -> String {
    let spec_docs = if input.spec_docs.is_empty() {
        "(none)".to_string()
    } else {
        input.spec_docs.join(", ")
    };
    DOC_BUNDLE_TPL
        .replace("{{RULES}}", &rules_block(rules))
        .replace("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus })
        .replace("{{DISMISSED_NOTES}}", if dismissed_notes.is_empty() { "(none)" } else { dismissed_notes })
        .replace("{{SPEC_DOCS}}", &spec_docs)
        .replace("{{TARGET_LABEL}}", &input.target_label)
        .replace("{{REVIEW_INPUT}}", &input.body)
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}

/// 构造一个文件包的对抗 prompt：所有适用 rule 注入为关注清单，bundle diff 内联。
pub fn build_bundle_prompt(
    target_label: &str,
    bundle: &Bundle,
    rules: &[&Lens],
    user_focus: &str,
    dismissed_notes: &str,
) -> String {
    let rules_str = rules_block(rules);

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

    CODE_BUNDLE_TPL
        .replace("{{TARGET_LABEL}}", target_label)
        .replace("{{RULES}}", &rules_str)
        .replace("{{FILE_LIST}}", &file_list)
        .replace("{{BUNDLE_DIFFS}}", &diffs_block)
        .replace("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus })
        .replace("{{DISMISSED_NOTES}}", if dismissed_notes.is_empty() { "(none)" } else { dismissed_notes })
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
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
        assert_eq!(output_language(), "Simplified Chinese");
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
        use crate::domain::{Finding, Severity};
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "missing auth".into(),
            body: "no check before db write".into(), file: "src/a.rs".into(),
            line_start: 12, line_end: 14, confidence: 0.9, recommendation: "add check".into(),
            lenses: vec!["L1".into()], models: vec!["claude".into()], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: "db.write(token)".into(), verify: None,
            previously_dismissed: false,
        };
        let p = build_refute_prompt(&f, crate::domain::Mode::Code);
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("src/a.rs") && p.contains("12"), "file:line injected");
        assert!(p.contains("missing auth") && p.contains("db.write(token)"), "title+quote injected");
        // upheld=空 findings 的契约必须在 prompt 中
        assert!(p.to_lowercase().contains("findings"), "output contract present");
    }

    #[test]
    fn refute_prompt_doc_omission_frames_empty_quote() {
        use crate::domain::{Finding, Mode, Severity};
        let f = Finding {
            id: "x".into(), severity: Severity::High, title: "missing rollback branch".into(),
            body: "no rollback path on failure".into(), file: "spec.md".into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "specify rollback".into(),
            lenses: vec!["D2".into()], models: vec!["alpha".into()], agreement: 1,
            hard_constraint: false, cluster_id: None, quote: String::new(), verify: None,
            previously_dismissed: false,
        };
        let p = build_refute_prompt(&f, Mode::Doc);
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.to_lowercase().contains("omit"), "doc refute frames omission");
        assert!(!p.contains("Quoted code"), "doc template (not code template)");
    }

    #[test]
    fn bundle_prompt_interpolates_rules_files_and_contract() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        use crate::lens::by_id;
        let bundle = Bundle {
            files: vec![FileChange { path: "src/auth.rs".into(), diff: "+let t = secret();".into() }],
        };
        let rules = vec![by_id("L1").unwrap(), by_id("L5").unwrap()];
        let p = build_bundle_prompt("working tree", &bundle, &rules, "auth", "已驳回:旧误报X");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("[L1]") && p.contains("[L5]"), "rule ids injected");
        assert!(p.contains("src/auth.rs"), "file path injected");
        assert!(p.contains("let t = secret();"), "diff injected");
        assert!(p.contains("quote"), "quote contract present");
        assert!(p.contains("auth"), "user focus injected");
        assert!(p.contains("已驳回:旧误报X"), "dismissed ledger notes injected");
        // L5 是 compliance rule → 注入 severity 封顶提示
        assert!(p.to_lowercase().contains("compliance rule"), "compliance cap note present");
    }

    #[test]
    fn doc_prompt_injects_all_rules_doc_body_and_omission_contract() {
        use crate::domain::{Mode, ReviewInput};
        use crate::lens::by_id;
        let input = ReviewInput {
            mode: Mode::Doc, target_label: "spec.md".into(),
            collection_guidance: String::new(), body: "DOCBODY".into(),
            spec_docs: vec!["design.md".into()],
        };
        let rules = vec![by_id("D1").unwrap(), by_id("D2").unwrap(), by_id("D5").unwrap()];
        let p = build_doc_prompt(&input, &rules, "auth", "已驳回:旧误报Y");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("[D1]") && p.contains("[D2]") && p.contains("[D5]"), "all rules injected");
        assert!(p.contains("DOCBODY"), "doc body inlined");
        assert!(p.contains("design.md"), "spec docs injected");
        assert!(p.contains("auth"), "user focus injected");
        assert!(p.contains("已驳回:旧误报Y"), "dismissed ledger notes injected");
        assert!(p.to_lowercase().contains("omission"), "omission/empty-quote contract present");
        assert!(p.to_lowercase().contains("compliance rule"), "D5 compliance cap note present");
        assert!(p.contains("lens"), "doc prompt must teach model to tag lens id per finding");
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
}
