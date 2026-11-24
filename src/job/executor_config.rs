//! job 模式的 executor config：**唯一来源是 manifest `paths.config`**（契约 §1.1.3 / P6）。
//!
//! - 只读该文件；不读 `$REDTEAM_HOME` / `secrets.json`。
//! - 严格校验（D4）：未知键 = error；密钥类字段 = error（凭据只走 env）。
//! - 文件不存在 / 不可读 → `error`（不得静默用空配置）。

use crate::config::{check_text, Config, ConfigIssue};
use crate::job::manifest::JobManifest;
use crate::job::status::JobFailure;
use std::path::Path;

/// 读取并严格校验 `paths.config`。
pub fn load(manifest: &JobManifest, job_dir: &Path) -> Result<Config, JobFailure> {
    let raw = job_dir.join(&manifest.paths.config);
    let canon = raw
        .canonicalize()
        .map_err(|_| JobFailure::new("config_unreadable", None))?;
    if !canon.starts_with(job_dir) {
        // manifest 已限定固定相对路径；解析后越界只可能是符号链接逃逸。
        return Err(JobFailure::new("config_unreadable", None));
    }
    let text = std::fs::read_to_string(&canon).map_err(|_| JobFailure::new("config_unreadable", None))?;
    match check_text(&text) {
        (Some(cfg), issues) if issues.is_empty() => Ok(cfg),
        (_, issues) => {
            for i in &issues {
                eprintln!("config issue {}: {} ({})", i.pointer, i.message, i.code);
            }
            Err(issue_to_failure(&issues))
        }
    }
}

/// 多条 issue 取**第一条**（附录 E：status_code 只保留主导一条）；密钥字段优先上报。
fn issue_to_failure(issues: &[ConfigIssue]) -> JobFailure {
    if let Some(i) = issues.iter().find(|i| i.code == "secret_field") {
        return JobFailure::new("config_contains_secret", Some(pointer_or_root(&i.pointer)));
    }
    let i = &issues[0];
    JobFailure::new("config_invalid", Some(pointer_or_root(&i.pointer)))
}

fn pointer_or_root(p: &str) -> String {
    if p.is_empty() { "/".to_string() } else { p.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::manifest::JobManifest;

    fn manifest() -> JobManifest {
        JobManifest::parse(r#"{
          "schema":"hibot.audit.job.review/1","job_id":"J1","kind":"review",
          "repo":{"id":1,"full_name":"o/r"},
          "pr":{"number":7,"head_sha":"abcdef0","base_sha":"1234567"},
          "executor":{"name":"redteam","version":"0.1.0"},
          "limits":{"wall_clock_s":600},
          "paths":{"repo":"../repo","out":"out","config":"config/executor.redteam.json"},
          "inputs":{}
        }"#).unwrap()
    }

    fn job_with_config(text: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let job = d.path().join("job");
        std::fs::create_dir_all(job.join("config")).unwrap();
        std::fs::write(job.join("config/executor.redteam.json"), text).unwrap();
        (d, job.canonicalize().unwrap())
    }

    #[test]
    fn loads_valid_config() {
        let (_d, job) = job_with_config(r#"{"language":"zh_CN","providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#);
        let cfg = load(&manifest(), &job).unwrap();
        assert_eq!(cfg.language, Some(crate::config::Language::ZhCn));
    }

    #[test]
    fn missing_file_is_error() {
        let d = tempfile::tempdir().unwrap();
        let job = d.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        let job = job.canonicalize().unwrap();
        let e = load(&manifest(), &job).unwrap_err();
        assert_eq!(e.full_code(), "config_unreadable");
    }

    #[test]
    fn unknown_key_is_rejected_not_warned() {
        let (_d, job) = job_with_config(r#"{"language":"en_US","review":{"unknown":1},"providers":{"p":{"kind":"openai","models":{"m":{"roles":{"scanner":{}}}}}}}"#);
        let e = load(&manifest(), &job).unwrap_err();
        assert_eq!(e.code, "config_invalid");
        assert_eq!(e.detail.as_deref(), Some("/review/unknown"));
    }

    #[test]
    fn secret_field_has_dedicated_code() {
        let (_d, job) = job_with_config(r#"{"providers":{"p":{"kind":"openai","apiKey":"sk-x","models":{"m":{"roles":{"scanner":{}}}}}}}"#);
        let e = load(&manifest(), &job).unwrap_err();
        assert_eq!(e.full_code(), "config_contains_secret:/providers/p/apiKey");
    }

    #[test]
    fn api_key_env_is_not_a_secret_field() {
        let (_d, job) = job_with_config(r#"{"providers":{"p":{"kind":"openai","apiKeyEnv":"P_KEY","models":{"m":{"roles":{"scanner":{}}}}}}}"#);
        assert!(load(&manifest(), &job).is_ok());
    }
}
