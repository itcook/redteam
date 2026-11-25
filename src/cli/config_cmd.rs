//! `config schema` / `config check`（P17 / 1.9）：配置契约外化。
//!
//! `check` 的输出契约（hibot 已定）：诊断**只走 stdout**，格式
//! `ERROR|WARN <JSON Pointer> <code>: <message>`，末行 `RESULT ok|error <n> errors, <m> warnings`；
//! **退出码严格二值 0/非 0**；`WARN` 不影响退出码；不提供 `--format json`。

use crate::config::{check_text, Config, ConfigIssue, MAX_CONCURRENCY_SANE};
use crate::job::manifest::JobKind;

#[derive(Clone, Debug)]
struct Diag {
    level: &'static str,
    pointer: String,
    code: String,
    message: String,
}

fn ptr(p: &str) -> &str {
    if p.is_empty() { "/" } else { p }
}

pub fn run_schema(out: Option<&str>) -> i32 {
    let text = crate::config::render_config_schema();
    match out {
        Some(path) => {
            if let Some(parent) = std::path::Path::new(path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::write(path, text) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: 写 {path} 失败: {e}");
                    1
                }
            }
        }
        None => {
            print!("{text}");
            0
        }
    }
}

/// `input` 可以是文件路径，或以 `{` 开头的内联 JSON（与 `verify-once <finding>` 同风格）。
pub fn run_check(input: &str, kind: Option<&str>) -> i32 {
    let text = if input.trim_start().starts_with('{') {
        input.to_string()
    } else {
        match std::fs::read_to_string(input) {
            Ok(t) => t,
            Err(e) => {
                let d = Diag {
                    level: "ERROR",
                    pointer: "/".into(),
                    code: "input_unreadable".into(),
                    message: format!("cannot read {input}: {e}"),
                };
                println!("{} {} {}: {}", d.level, d.pointer, d.code, d.message);
                println!("RESULT error 1 errors, 0 warnings");
                return 1;
            }
        }
    };
    let kind = kind.and_then(JobKind::parse);
    let (_cfg, errors, warnings) = check(text.as_str(), kind);
    for d in errors.iter().chain(warnings.iter()) {
        println!("{} {} {}: {}", d.level, ptr(&d.pointer), d.code, d.message);
    }
    let verdict = if errors.is_empty() { "ok" } else { "error" };
    println!("RESULT {verdict} {} errors, {} warnings", errors.len(), warnings.len());
    // WARN 不影响退出码（hibot 三点要求）。
    if errors.is_empty() { 0 } else { 1 }
}

fn check(text: &str, kind: Option<JobKind>) -> (Option<Config>, Vec<Diag>, Vec<Diag>) {
    let (cfg, issues) = check_text(text);
    let mut errors: Vec<Diag> = issues.into_iter().map(issue_diag).collect();
    let mut warnings = Vec::new();
    if let Some(c) = &cfg {
        // 角色最小集（v2）：review = scanner + reviewer；triage = settings.triage.models；
        // verify = settings.verify.models（**不再回退 reviewer**）。通用 validate 不查三池齐全。
        match kind {
            Some(JobKind::Review) => {
                if c.settings.review.roles.scanner.models.is_empty() {
                    errors.push(missing_role("scanner"));
                }
                if c.settings.review.roles.reviewer.models.is_empty() {
                    errors.push(missing_role("reviewer"));
                }
            }
            Some(JobKind::Triage) => {
                if c.settings.triage.models.is_empty() {
                    errors.push(missing_role("triage"));
                }
            }
            Some(JobKind::Verify) => {
                if c.settings.verify.models.is_empty() {
                    errors.push(missing_role("verifier"));
                }
            }
            None => {}
        }
        for (pname, p) in &c.providers {
            if let Some(cap) = p.max_concurrency.filter(|c| *c > MAX_CONCURRENCY_SANE) {
                warnings.push(Diag {
                    level: "WARN",
                    pointer: format!("/providers/{}", esc(pname)),
                    code: "max_concurrency_sane".into(),
                    message: format!("maxConcurrency = {cap} looks like a wrong field (maxOutputTokens?)"),
                });
            }
            for (idx, m) in p.models.iter().enumerate() {
                if !c.model_is_used(pname, &m.id) {
                    warnings.push(Diag {
                        level: "WARN",
                        pointer: format!("/providers/{}/models/{idx}", esc(pname)),
                        code: "model_unused".into(),
                        message: format!("model {:?} is not referenced by any role pool", m.id),
                    });
                }
            }
        }
        if c.settings.review.refuters.enabled && c.settings.review.roles.refuter.models.is_empty() {
            warnings.push(Diag {
                level: "WARN",
                pointer: "/settings/review/refuters/enabled".into(),
                code: "refuter_no_models".into(),
                message: "refuters.enabled = true but roles.refuter.models is empty; job mode will skip refute".into(),
            });
        }
        // reasoningEffort 在配置期能判定的问题（无强度槽 / map 抑制）——与运行期 warning 同一份判断。
        let pools: [(&str, &crate::config::RolePool, crate::config::Role); 5] = [
            ("/settings/review/roles/scanner", &c.settings.review.roles.scanner, crate::config::Role::Scanner),
            ("/settings/review/roles/reviewer", &c.settings.review.roles.reviewer, crate::config::Role::Reviewer),
            ("/settings/review/roles/refuter", &c.settings.review.roles.refuter, crate::config::Role::Refuter),
            ("/settings/triage", &c.settings.triage, crate::config::Role::Triage),
            ("/settings/verify", &c.settings.verify, crate::config::Role::Verifier),
        ];
        for (base, pool, role) in pools {
            for (idx, r) in pool.models.iter().enumerate() {
                let Some(em) = c.effective(&r.provider, &r.id, role, r.reasoning_effort) else { continue };
                if let Some((code, message)) = crate::provider::backends::reasoning_effort_warning_code(&em) {
                    warnings.push(Diag { level: "WARN", pointer: format!("{base}/models/{idx}"), code: code.into(), message });
                }
            }
        }
        if c.settings.review.max_concurrency > MAX_CONCURRENCY_SANE as usize {
            warnings.push(Diag {
                level: "WARN",
                pointer: "/settings/review/maxConcurrency".into(),
                code: "max_concurrency_sane".into(),
                message: format!("maxConcurrency = {} is the provider gate default; looks wrong", c.settings.review.max_concurrency),
            });
        }
    }
    (cfg, errors, warnings)
}

fn missing_role(role: &str) -> Diag {
    Diag {
        level: "ERROR",
        pointer: "/providers".into(),
        code: format!("missing_role:{role}"),
        message: format!("no `{role}` role declared"),
    }
}

fn issue_diag(i: ConfigIssue) -> Diag {
    Diag { level: "ERROR", pointer: i.pointer, code: i.code.to_string(), message: i.message }
}

fn esc(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &str, kind: Option<&str>) -> (i32, String) {
        // 直接测内部（stdout 打印路径由 main 调用；这里断言判定与计数）。
        let (cfg, errors, warnings) = check(input, kind.and_then(JobKind::parse));
        let _ = cfg;
        let code = if errors.is_empty() { 0 } else { 1 };
        (code, format!("{} errors, {} warnings", errors.len(), warnings.len()))
    }

    const OK: &str = r#"{"settings":{"language":"en_US","review":{"roles":{"scanner":{"models":[{"provider":"p","id":"s"}]},"reviewer":{"models":[{"provider":"p","id":"r"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"s"},{"id":"r"}]}}}"#;

    #[test]
    fn valid_config_passes_kind_review() {
        assert_eq!(run(OK, Some("review")).0, 0);
        assert_eq!(run(OK, Some("triage")).0, 1, "v2 triage 池独立，缺则 missing_role:triage");
        assert_eq!(run(OK, Some("verify")).0, 1, "v2 verify 不再回退 reviewer");
        let full = r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"p","id":"s"}]},"reviewer":{"models":[{"provider":"p","id":"r"}]}}},"triage":{"models":[{"provider":"p","id":"r"}]},"verify":{"models":[{"provider":"p","id":"r"}]}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"s"},{"id":"r"}]}}}"#;
        for k in ["review", "triage", "verify"] {
            assert_eq!(run(full, Some(k)).0, 0, "{k}");
        }
    }

    #[test]
    fn missing_roles_are_errors_per_kind() {
        let no_scanner = r#"{"settings":{"review":{"roles":{"reviewer":{"models":[{"provider":"p","id":"r"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"r"}]}}}"#;
        assert_eq!(run(no_scanner, Some("review")).0, 1);
        assert_eq!(run(no_scanner, Some("triage")).0, 1, "triage 池独立");
        assert_eq!(run(no_scanner, Some("verify")).0, 1);
        let no_reviewer = r#"{"settings":{"review":{"roles":{"scanner":{"models":[{"provider":"p","id":"s"}]}}}},"providers":{"p":{"api":"openai-completions","baseUrl":"https://x","models":[{"id":"s"}]}}}"#;
        assert_eq!(run(no_reviewer, Some("triage")).0, 1, "triage 缺 triage 池");
        assert_eq!(run(no_reviewer, Some("verify")).0, 1, "verifier 池缺");
    }

    #[test]
    fn unknown_and_secret_keys_are_errors_warns_do_not_fail() {
        let bad = r#"{"settings":{"review":{"unknown":1,"roles":{"scanner":{"models":[{"provider":"p","id":"m"}]}}}},"providers":{"p":{"api":"openai-completions","apiKey":"sk-x","baseUrl":"https://x","models":[{"id":"m"},{"id":"n"}]}}}"#;
        assert_eq!(run(bad, None).0, 1);
        let warn_only = r#"{"settings":{"review":{}},"providers":{"p":{"api":"openai-completions","maxConcurrency":999,"baseUrl":"https://x","models":[{"id":"m"}]}}}"#;
        let (code, summary) = run(warn_only, None);
        assert_eq!(code, 0, "WARN 不影响退出码");
        assert!(summary.contains("warnings"), "{summary}");
    }

    #[test]
    fn unknown_role_key_fails_strict_check() {
        // 真实非法角色键（不是靠空池 missing_role 通过）。
        let bad = r#"{"settings":{"review":{"roles":{"gerbil":{"models":[]}}}},"providers":{}}"#;
        let (_cfg, errors, _warnings) = check(bad, Some(JobKind::Review));
        assert!(
            errors.iter().any(|d| d.code == "unknown_key" && d.pointer == "/settings/review/roles/gerbil"),
            "{errors:?}"
        );
        assert_eq!(run(bad, Some("review")).0, 1);
        // 合法角色键但空池 → 才是 missing_role。
        let empty = r#"{"settings":{"review":{"roles":{"scanner":{"models":[]},"reviewer":{"models":[]}}}},"providers":{}}"#;
        let (_c, errs, _) = check(empty, Some(JobKind::Review));
        assert!(errs.iter().any(|d| d.code == "missing_role:scanner"), "{errs:?}");
    }

    #[test]
    fn schema_identity_is_stable() {
        let v = crate::config::config_schema_json();
        assert_eq!(v["$id"], "urn:redteam:config/2");
        assert_eq!(v["title"], "redteam executor config");
        assert_eq!(v["schemaVersion"], 2);
        assert!(v["properties"]["settings"].is_object(), "settings 必须进 schema");
        assert!(v["properties"]["providers"].is_object());
        assert!(v["definitions"]["Settings"]["properties"]["language"].is_object(), "language 在 settings 下");
        assert!(v["definitions"]["ReviewConfig"]["properties"]["roles"].is_object());
        assert!(v["definitions"]["ProviderConfig"]["properties"]["api"].is_object());
        assert!(crate::config::render_config_schema().ends_with('\n'));
    }
}
