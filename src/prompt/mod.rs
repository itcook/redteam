use crate::domain::{Mode, ReviewInput};
use crate::lens::Lens;

const CODE_TPL: &str = include_str!("templates/adversarial-code.md");
const DOC_TPL: &str = include_str!("templates/adversarial-doc.md");

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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Mode, ReviewInput};
    use crate::lens::by_id;
    fn input() -> ReviewInput {
        ReviewInput { mode: Mode::Code, target_label: "branch x".into(),
            collection_guidance: "GUIDE".into(), body: "DIFF".into(), spec_docs: vec![] }
    }
    #[test]
    fn interpolates_all_placeholders() {
        let p = build_prompt(&input(), by_id("L1").unwrap(), "auth");
        assert!(!p.contains("{{"));
        assert!(p.contains("安全与边界") && p.contains("DIFF") && p.contains("auth") && p.contains("GUIDE"));
    }
    #[test]
    fn compliance_lens_injects_severity_cap_rule() {
        let p = build_prompt(&input(), by_id("L5").unwrap(), "");
        assert!(p.to_lowercase().contains("medium"));
    }
}
