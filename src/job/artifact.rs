//! `inputs.*` 的解析与校验：路径约束、sha256、schema/format 常量。
//!
//! 每个声明都要校验（含可选 prior_report）：hibot 的 manifest `inputs` 是唯一权威（契约 §1.2）。

use crate::job::io;
use crate::job::manifest::{ArtifactDecl, JobKind, JobManifest};
use crate::job::status::JobFailure;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Artifact {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

impl Artifact {
    /// UTF-8 文本视图（diff_patch / merge_base 用）；非 UTF-8 → None。
    pub fn text(&self) -> Option<&str> {
        std::str::from_utf8(&self.bytes).ok()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Artifacts {
    map: BTreeMap<String, Artifact>,
}

impl Artifacts {
    pub fn get(&self, key: &str) -> Option<&Artifact> {
        self.map.get(key)
    }
    pub fn text(&self, key: &str) -> Option<&str> {
        self.map.get(key).and_then(Artifact::text)
    }
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.map.keys().map(String::as_str)
    }
}

/// 加载并校验 manifest 声明的全部 artifact。
pub fn load(manifest: &JobManifest, job_dir: &Path) -> Result<Artifacts, JobFailure> {
    for key in manifest.kind.required_inputs() {
        if !manifest.inputs.contains_key(*key) {
            return Err(JobFailure::new("input_missing", Some((*key).to_string())));
        }
    }
    let mut out = BTreeMap::new();
    for (key, decl) in &manifest.inputs {
        let path = io::resolve_inside(job_dir, &decl.path, key)?;
        let bytes = std::fs::read(&path).map_err(|_| JobFailure::new("input_path_invalid", Some(key.clone())))?;
        verify_sha256(&bytes, &decl.sha256, key)?;
        check_schema_format(manifest.kind, key, decl)?;
        out.insert(key.clone(), Artifact { path, bytes });
    }
    Ok(Artifacts { map: out })
}

fn verify_sha256(bytes: &[u8], expected: &str, key: &str) -> Result<(), JobFailure> {
    // 契约：64 位小写 hex，不带 `sha256:` 前缀。
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) {
        return Err(JobFailure::new("input_checksum_mismatch", Some(key.to_string())));
    }
    let mut h = Sha256::new();
    h.update(bytes);
    let got = h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    if got != expected {
        return Err(JobFailure::new("input_checksum_mismatch", Some(key.to_string())));
    }
    Ok(())
}

/// 期望的 `schema` / `format` 常量（契约 §1.2；缺失或不符都算 `input_schema_mismatch`）。
fn expected(kind: JobKind, key: &str) -> (Option<&'static str>, Option<&'static str>) {
    match (kind, key) {
        (JobKind::Triage, "pr_context") => (Some("hibot.audit.pr-context/1"), None),
        (JobKind::Verify, "findings") => (Some("hibot.audit.findings/1"), None),
        (JobKind::Review, "dismissed") => (None, Some("hibot.audit.dismissed-csv/1")),
        (JobKind::Review, "prior_report") => (Some("hibot.audit.report.triage/1"), None),
        (JobKind::Verify, "prior_report") => (Some("hibot.audit.report.review/1"), None),
        _ => (None, None),
    }
}

fn check_schema_format(kind: JobKind, key: &str, decl: &ArtifactDecl) -> Result<(), JobFailure> {
    let (want_schema, want_format) = expected(kind, key);
    if let Some(want) = want_schema {
        if decl.schema.as_deref() != Some(want) {
            return Err(JobFailure::new("input_schema_mismatch", Some(key.to_string())));
        }
    }
    if let Some(want) = want_format {
        if decl.format.as_deref() != Some(want) {
            return Err(JobFailure::new("input_schema_mismatch", Some(key.to_string())));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    fn manifest(kind: &str, inputs: &str) -> JobManifest {
        let text = format!(r#"{{
          "schema": "hibot.audit.job.{kind}/1",
          "job_id": "J1",
          "kind": "{kind}",
          "repo": {{"id": 1, "full_name": "o/r"}},
          "pr": {{"number": 7, "head_sha": "abcdef0", "base_sha": "1234567"}},
          "executor": {{"name": "redteam", "version": "0.1.0"}},
          "limits": {{"wall_clock_s": 600}},
          "paths": {{"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"}},
          "inputs": {inputs}
        }}"#);
        JobManifest::parse(&text).unwrap()
    }

    fn write_job(files: &[(&str, &[u8])]) -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let job = d.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        for (rel, bytes) in files {
            let p = job.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, bytes).unwrap();
        }
        let job = job.canonicalize().unwrap();
        (d, job)
    }

    fn decl(path: &str, bytes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(bytes);
        format!(r#"{{"path":"{path}","sha256":"{}"}}"#, hex(&h.finalize()))
    }

    #[test]
    fn loads_and_verifies_all_declared_artifacts() {
        let (_d, job) = write_job(&[("input/diff.patch", b"+a\n"), ("input/mb.sha", b"abc\n"), ("input/d.csv", b"")]);
        let inputs = format!(
            r#"{{"diff_patch":{},"merge_base":{},"dismissed":{{"path":"input/d.csv","sha256":"{}","format":"hibot.audit.dismissed-csv/1"}}}}"#,
            decl("input/diff.patch", b"+a\n"),
            decl("input/mb.sha", b"abc\n"),
            hex(&Sha256::digest(b"")),
        );
        let m = manifest("review", &inputs);
        let a = load(&m, &job).unwrap();
        assert_eq!(a.text("diff_patch"), Some("+a\n"));
        assert_eq!(a.text("merge_base"), Some("abc\n"));
    }

    #[test]
    fn missing_required_key_is_reported_by_name() {
        let (_d, job) = write_job(&[]);
        let m = manifest("review", "{}");
        let e = load(&m, &job).unwrap_err();
        assert_eq!(e.code, "input_missing");
    }

    #[test]
    fn checksum_mismatch_and_bad_hash_shape_are_errors() {
        let (_d, job) = write_job(&[("input/diff.patch", b"x"), ("input/mb.sha", b"y"), ("input/d.csv", b"")]);
        let got = hex(&Sha256::digest(b"z"));
        let inputs = format!(
            r#"{{"diff_patch":{{"path":"input/diff.patch","sha256":"{}"}},"merge_base":{},"dismissed":{{"path":"input/d.csv","sha256":"{}","format":"hibot.audit.dismissed-csv/1"}}}}"#,
            got,
            decl("input/mb.sha", b"y"),
            hex(&Sha256::digest(b"")),
        );
        let e = load(&manifest("review", &inputs), &job).unwrap_err();
        assert_eq!(e.full_code(), "input_checksum_mismatch:diff_patch");

        let bad_shape = inputs.replace(&got, "SHA256:ABC");
        let e2 = load(&manifest("review", &bad_shape), &job).unwrap_err();
        assert_eq!(e2.full_code(), "input_checksum_mismatch:diff_patch");
    }

    #[test]
    fn path_escape_is_rejected() {
        let (d, job) = write_job(&[("input/diff.patch", b"x"), ("input/mb.sha", b"y")]);
        std::fs::write(d.path().join("outside.csv"), b"").unwrap();
        let inputs = format!(
            r#"{{"diff_patch":{{"path":"../outside.csv","sha256":"{}"}},"merge_base":{},"dismissed":{{"path":"input/mb.sha","sha256":"{}","format":"hibot.audit.dismissed-csv/1"}}}}"#,
            hex(&Sha256::digest(b"")),
            decl("input/mb.sha", b"y"),
            hex(&Sha256::digest(b"y")),
        );
        let e = load(&manifest("review", &inputs), &job).unwrap_err();
        assert_eq!(e.full_code(), "input_path_invalid:diff_patch");
    }

    #[test]
    fn schema_and_format_constants_are_enforced() {
        let (_d, job) = write_job(&[("input/pc.json", b"{}"), ("input/diff.patch", b"x"), ("input/mb.sha", b"y")]);
        let inputs = format!(
            r#"{{"pr_context":{},"diff_patch":{},"merge_base":{}}}"#,
            decl("input/pc.json", b"{}"),
            decl("input/diff.patch", b"x"),
            decl("input/mb.sha", b"y"),
        );
        // triage 的 pr_context 必须声明 schema
        let e = load(&manifest("triage", &inputs), &job).unwrap_err();
        assert_eq!(e.full_code(), "input_schema_mismatch:pr_context");
        let with_schema = format!(
            r#"{{"pr_context":{{"path":"input/pc.json","sha256":"{}","schema":"hibot.audit.pr-context/1"}},"diff_patch":{},"merge_base":{}}}"#,
            hex(&Sha256::digest(b"{}")),
            decl("input/diff.patch", b"x"),
            decl("input/mb.sha", b"y"),
        );
        assert!(load(&manifest("triage", &with_schema), &job).is_ok());
    }
}
