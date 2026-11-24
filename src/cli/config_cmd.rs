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
        // 角色最小集（P2）：review = scanner + reviewer；triage = scanner；verify = verifier 或 reviewer。
        let roles = c.active_roles();
        match kind {
            Some(JobKind::Review) => {
                if roles.scanners.is_empty() {
                    errors.push(missing_role("scanner"));
                }
                if roles.reviewers.is_empty() {
                    errors.push(missing_role("reviewer"));
                }
            }
            Some(JobKind::Triage) => {
                if roles.scanners.is_empty() {
                    errors.push(missing_role("scanner"));
                }
            }
            Some(JobKind::Verify) => {
                if roles.verifiers.is_empty() && roles.reviewers.is_empty() {
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
            for (id, m) in &p.models {
                if m.roles.is_empty() {
                    warnings.push(Diag {
                        level: "WARN",
                        pointer: format!("/providers/{}/models/{}", esc(pname), esc(id)),
                        code: "no_roles".into(),
                        message: "model declares no roles and will not participate".into(),
                    });
                }
            }
        }
        if c.review.max_concurrency > MAX_CONCURRENCY_SANE as usize {
            warnings.push(Diag {
                level: "WARN",
                pointer: "/review/maxConcurrency".into(),
                code: "max_concurrency_sane".into(),
                message: format!("maxConcurrency = {} is the provider gate default; looks wrong", c.review.max_concurrency),
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

    const OK: &str = r#"{"language":"en_US","providers":{"p":{"kind":"openai","models":{"s":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}"#;

    #[test]
    fn valid_config_passes_kind_review() {
        assert_eq!(run(OK, Some("review")).0, 0);
        assert_eq!(run(OK, Some("triage")).0, 0);
        assert_eq!(run(OK, Some("verify")).0, 0, "reviewer 是 verifier 的回退");
    }

    #[test]
    fn missing_roles_are_errors_per_kind() {
        let no_scanner = r#"{"providers":{"p":{"kind":"openai","models":{"r":{"roles":{"reviewer":{}}}}}}}"#;
        assert_eq!(run(no_scanner, Some("review")).0, 1);
        assert_eq!(run(no_scanner, Some("triage")).0, 1);
        assert_eq!(run(no_scanner, Some("verify")).0, 0);
        let neither = r#"{"providers":{"p":{"kind":"openai","models":{"s":{"roles":{"scanner":{}}}}}}}"#;
        assert_eq!(run(neither, Some("verify")).0, 1, "verifier 与 reviewer 都缺");
    }

    #[test]
    fn unknown_and_secret_keys_are_errors_warns_do_not_fail() {
        let bad = r#"{"providers":{"p":{"kind":"openai","apiKey":"sk-x","models":{"m":{"roles":{"scanner":{}}},"n":{}}}},"review":{"unknown":1}}"#;
        assert_eq!(run(bad, None).0, 1);
        let warn_only = r#"{"providers":{"p":{"kind":"openai","maxConcurrency":999,"models":{"m":{}}}}}"#;
        let (code, summary) = run(warn_only, None);
        assert_eq!(code, 0, "WARN 不影响退出码");
        assert!(summary.contains("warnings"), "{summary}");
    }

    #[test]
    fn unknown_role_key_fails_strict_check() {
        let bad = r#"{"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"gerbil":{}}}}}}}"#;
        assert_eq!(run(bad, Some("review")).0, 1);
    }

    #[test]
    fn schema_identity_is_stable() {
        let v = crate::config::config_schema_json();
        assert_eq!(v["$id"], "urn:redteam:config/1");
        assert_eq!(v["title"], "redteam executor config");
        assert_eq!(v["schemaVersion"], 1);
        assert!(v["properties"]["language"].is_object(), "language 必须进 schema");
        assert!(v["properties"]["review"].is_object());
        let roles = &v["definitions"]["ModelConfig"]["properties"]["roles"]["propertyNames"]["enum"];
        assert_eq!(roles, &serde_json::json!(["scanner", "reviewer", "refuter", "verifier"]), "roles 键必须枚举");
        assert!(crate::config::render_config_schema().ends_with('\n'));
    }
}
