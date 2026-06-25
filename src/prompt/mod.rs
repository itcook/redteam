use crate::domain::{Mode, ReviewInput};
use crate::lens::Lens;

const CODE_TPL: &str = include_str!("templates/adversarial-code.md");
const DOC_TPL: &str = include_str!("templates/adversarial-doc.md");

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
}
