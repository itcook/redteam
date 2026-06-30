use crate::bundle::Bundle;
use crate::domain::{Mode, ReviewInput};
use crate::lens::Lens;

const CODE_TPL: &str = include_str!("templates/adversarial-code.md");
const DOC_TPL: &str = include_str!("templates/adversarial-doc.md");
const CODE_BUNDLE_TPL: &str = include_str!("templates/adversarial-code-bundle.md");
const REFUTE_TPL: &str = include_str!("templates/adversarial-refute.md");

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

pub fn build_prompt(input: &ReviewInput, lens: &Lens, user_focus: &str) -> String {
    let tpl = match input.mode { Mode::Code => CODE_TPL, Mode::Doc => DOC_TPL };
    let sev_rule = match lens.max_severity {
        Some(_) => "This is a compliance lens: severity MUST be low or medium unless the violated rule is an explicit hard project constraint (then set hard_constraint=true).",
        None => "",
    };
    tpl.replace("{{LENS_TITLE}}", lens.title)
        .replace("{{LENS_FOCUS}}", lens.focus)
        .replace("{{LENS_SEVERITY_RULE}}", sev_rule)
        .replace("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus })
        .replace("{{COLLECTION_GUIDANCE}}", &input.collection_guidance)
        .replace("{{TARGET_LABEL}}", &input.target_label)
        .replace("{{REVIEW_INPUT}}", &input.body)
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}

/// 构造 refute prompt：给定 claimed finding，让 skeptic 对抗复审。
/// 复用 review-as-refute——返回的 findings 空=upheld、非空=refuted（见模板 output_contract）。
pub fn build_refute_prompt(finding: &crate::domain::Finding) -> String {
    REFUTE_TPL
        .replace("{{FILE}}", &finding.file)
        .replace("{{LINE_START}}", &finding.line_start.to_string())
        .replace("{{LINE_END}}", &finding.line_end.to_string())
        .replace("{{TITLE}}", &finding.title)
        .replace("{{SEVERITY}}", finding.severity.as_str())
        .replace("{{BODY}}", &finding.body)
        .replace("{{QUOTE}}", &finding.quote)
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}

/// 构造一个文件包的对抗 prompt：所有适用 rule 注入为关注清单，bundle diff 内联。
pub fn build_bundle_prompt(
    target_label: &str,
    bundle: &Bundle,
    rules: &[&Lens],
    user_focus: &str,
) -> String {
    let rules_block = rules
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
        .join("\n");

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
        .replace("{{RULES}}", &rules_block)
        .replace("{{FILE_LIST}}", &file_list)
        .replace("{{BUNDLE_DIFFS}}", &diffs_block)
        .replace("{{USER_FOCUS}}", if user_focus.is_empty() { "(none)" } else { user_focus })
        .replace("{{OUTPUT_LANGUAGE}}", &output_language())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Mode, ReviewInput};
    use crate::lens::by_id;
    use std::sync::Mutex;

    // Mutex to serialize env-var tests that mutate process-global env state.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn input() -> ReviewInput {
        ReviewInput { mode: Mode::Code, target_label: "branch x".into(),
            collection_guidance: "GUIDE".into(), body: "DIFF".into(), spec_docs: vec![] }
    }
    #[test]
    fn interpolates_all_placeholders() {
        let p = build_prompt(&input(), by_id("L1").unwrap(), "auth");
        assert!(!p.contains("{{"));
        assert!(p.contains("Security") && p.contains("DIFF") && p.contains("auth") && p.contains("GUIDE"));
    }
    #[test]
    fn compliance_lens_injects_severity_cap_rule() {
        let p = build_prompt(&input(), by_id("L5").unwrap(), "");
        assert!(p.to_lowercase().contains("medium"));
    }
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
    fn build_prompt_contains_language_directive_and_no_residual_placeholders() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("LC_ALL");
            std::env::remove_var("LC_MESSAGES");
            std::env::set_var("LANG", "zh_CN.UTF-8");
        }
        let p = build_prompt(&input(), by_id("L1").unwrap(), "");
        assert!(!p.contains("{{"), "no residual {{}} placeholders");
        assert!(p.contains("Simplified Chinese"), "prompt must contain language name");
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
        };
        let p = build_refute_prompt(&f);
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("src/a.rs") && p.contains("12"), "file:line injected");
        assert!(p.contains("missing auth") && p.contains("db.write(token)"), "title+quote injected");
        // upheld=空 findings 的契约必须在 prompt 中
        assert!(p.to_lowercase().contains("findings"), "output contract present");
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
        let p = build_bundle_prompt("working tree", &bundle, &rules, "auth");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(p.contains("[L1]") && p.contains("[L5]"), "rule ids injected");
        assert!(p.contains("src/auth.rs"), "file path injected");
        assert!(p.contains("let t = secret();"), "diff injected");
        assert!(p.contains("quote"), "quote contract present");
        assert!(p.contains("auth"), "user focus injected");
        // L5 是 compliance rule → 注入 severity 封顶提示
        assert!(p.to_lowercase().contains("compliance rule"), "compliance cap note present");
    }
}
