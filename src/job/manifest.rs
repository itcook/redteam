//! `job.json`（`hibot.audit.job.<kind>/1`）解析与校验。
//!
//! 契约要点（`~/hibot/.docs/audit/contract.md` §2）：
//! - `schema` 必须与 `kind` 一致（`hibot.audit.job.review/1` ↔ `kind: "review"`）；未知 schema/kind → `job_manifest_unsupported`。
//! - **manifest 不可读 / 不可解析 → 调用方不写报告**（1.8 / hibot B5）；本模块只返回 [`JobFailure`]，不做写盘。
//! - 未知字段忽略（契约 §2：新增字段不 bump 版本）。

use crate::job::status::JobFailure;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// 执行单元名（= capabilities.name）。job 模式的配置文件必须叫 `config/executor.redteam.json`。
pub const CLI_NAME: &str = "redteam";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobKind {
    Triage,
    Review,
    Verify,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Triage => "triage",
            JobKind::Review => "review",
            JobKind::Verify => "verify",
        }
    }
    /// 该 kind 的 manifest schema 常量。
    pub fn job_schema(self) -> &'static str {
        match self {
            JobKind::Triage => "hibot.audit.job.triage/1",
            JobKind::Review => "hibot.audit.job.review/1",
            JobKind::Verify => "hibot.audit.job.verify/1",
        }
    }
    /// 该 kind 的 report schema 常量。
    pub fn report_schema(self) -> &'static str {
        match self {
            JobKind::Triage => "hibot.audit.report.triage/1",
            JobKind::Review => "hibot.audit.report.review/1",
            JobKind::Verify => "hibot.audit.report.verify/1",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "triage" => Some(JobKind::Triage),
            "review" => Some(JobKind::Review),
            "verify" => Some(JobKind::Verify),
            _ => None,
        }
    }
    /// 该 kind 的必需输入（契约 §1.2 表）。
    pub fn required_inputs(self) -> &'static [&'static str] {
        match self {
            JobKind::Triage => &["pr_context", "diff_patch", "merge_base"],
            JobKind::Review => &["diff_patch", "merge_base", "dismissed"],
            JobKind::Verify => &["findings", "diff_patch", "merge_base"],
        }
    }
}

impl std::fmt::Display for JobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct RepoRef {
    pub id: i64,
    pub full_name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PrRef {
    pub number: u64,
    pub head_sha: String,
    pub base_sha: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ExecutorRef {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Limits {
    pub wall_clock_s: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Paths {
    /// 相对 job 目录的检出路径（v1 为 `../repo`；允许兄弟目录）。
    pub repo: String,
    /// 相对 job 目录的输出目录（唯一可写）。
    pub out: String,
    /// 固定 `config/executor.redteam.json`。
    pub config: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ArtifactDecl {
    pub path: String,
    /// 64 位小写 hex（不带 `sha256:` 前缀——那是 fingerprint 的形状）。
    pub sha256: String,
    #[serde(default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Clone, Debug)]
pub struct JobManifest {
    pub schema: String,
    pub job_id: String,
    pub kind: JobKind,
    pub repo: RepoRef,
    pub pr: PrRef,
    pub executor: ExecutorRef,
    pub limits: Limits,
    pub paths: Paths,
    pub inputs: BTreeMap<String, ArtifactDecl>,
    pub extensions: serde_json::Value,
}

impl JobManifest {
    /// 从 job 目录读 `job.json`。**不可读/不可解析 → `job_manifest_unreadable`**（调用方据此不写报告）。
    pub fn load(job_dir: &Path) -> Result<Self, JobFailure> {
        let path = job_dir.join("job.json");
        let text = std::fs::read_to_string(&path)
            .map_err(|e| JobFailure::new("job_manifest_unreadable", Some(format!("{}: {e}", path.display()))))?;
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Self, JobFailure> {
        // 契约 §2：未知字段忽略；用中间 Raw 反序列化，避免 extensions 之类被严格化。
        // 只有**不可读 / 不可解析**才返回 `job_manifest_unreadable`（调用方据此不写报告）；
        // schema/kind/paths 的语义问题留给 [`validate`]——可写 error 报告。
        let raw: RawManifest = serde_json::from_str(text)
            .map_err(|e| JobFailure::new("job_manifest_unreadable", Some(e.to_string())))?;
        Ok(JobManifest {
            schema: raw.schema,
            job_id: raw.job_id,
            kind: raw.kind,
            repo: raw.repo,
            pr: raw.pr,
            executor: raw.executor,
            limits: raw.limits,
            paths: raw.paths,
            inputs: raw.inputs,
            extensions: raw.extensions.unwrap_or(serde_json::Value::Null),
        })
    }

    /// schema ↔ kind、limits、`paths` 形状校验（1.1 / 1.2）。
    pub fn validate(&self) -> Result<(), JobFailure> {
        if self.schema != self.kind.job_schema() {
            return Err(JobFailure::new(
                "job_manifest_unsupported",
                Some(format!("schema {:?} does not match kind {}", self.schema, self.kind.as_str())),
            ));
        }
        if self.limits.wall_clock_s == 0 {
            return Err(JobFailure::new("job_manifest_unsupported", Some("limits.wall_clock_s must be >= 1".into())));
        }
        let expected_config = format!("config/executor.{CLI_NAME}.json");
        if self.paths.config != expected_config {
            return Err(JobFailure::new(
                "job_manifest_unsupported",
                Some(format!("paths.config must be {:?}, got {:?}", expected_config, self.paths.config)),
            ));
        }
        if self.paths.repo.trim().is_empty() || self.paths.out.trim().is_empty() {
            return Err(JobFailure::new("job_manifest_unsupported", Some("paths.repo / paths.out must be non-empty".into())));
        }
        Ok(())
    }

    /// 命令行 kind 与 manifest kind 一致（不一致 → `kind_mismatch`；报告 kind 以 manifest 为准）。
    pub fn check_cli_kind(&self, cli: JobKind) -> Result<(), JobFailure> {
        if self.kind != cli {
            return Err(JobFailure::new(
                "kind_mismatch",
                Some(format!("cli kind {} vs manifest {}", cli.as_str(), self.kind.as_str())),
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct RawManifest {
    schema: String,
    job_id: String,
    kind: JobKind,
    repo: RepoRef,
    pr: PrRef,
    executor: ExecutorRef,
    limits: Limits,
    paths: Paths,
    #[serde(default)]
    inputs: BTreeMap<String, ArtifactDecl>,
    #[serde(default)]
    extensions: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"{
      "schema": "hibot.audit.job.review/1",
      "job_id": "J1",
      "kind": "review",
      "repo": {"id": 1, "full_name": "o/r"},
      "pr": {"number": 7, "head_sha": "abcdef0", "base_sha": "1234567"},
      "executor": {"name": "redteam", "version": "0.1.0"},
      "limits": {"wall_clock_s": 600},
      "paths": {"repo": "../repo", "out": "out", "config": "config/executor.redteam.json"}
    }"#;

    #[test]
    fn parses_review_manifest() {
        let m = JobManifest::parse(BASE).unwrap();
        m.validate().unwrap();
        assert_eq!((m.kind, m.job_id.as_str(), m.limits.wall_clock_s), (JobKind::Review, "J1", 600));
        assert_eq!(m.paths.config, "config/executor.redteam.json");
        assert!(m.inputs.is_empty());
    }

    #[test]
    fn rejects_schema_kind_mismatch() {
        let bad = BASE.replace("job.review/1", "job.verify/1");
        let e = JobManifest::parse(&bad).unwrap().validate().unwrap_err();
        assert_eq!(e.code, "job_manifest_unsupported");
    }

    #[test]
    fn rejects_unparseable_manifest_with_unreadable_code() {
        let e = JobManifest::parse("not json").unwrap_err();
        assert_eq!(e.code, "job_manifest_unreadable");
        let missing = BASE.replace("\"job_id\": \"J1\",", "");
        let e2 = JobManifest::parse(&missing).unwrap_err();
        assert_eq!(e2.code, "job_manifest_unreadable");
    }

    #[test]
    fn rejects_noncanonical_config_path() {
        let bad = BASE.replace("config/executor.redteam.json", "config/other.json");
        let e = JobManifest::parse(&bad).unwrap().validate().unwrap_err();
        assert_eq!(e.code, "job_manifest_unsupported");
    }

    #[test]
    fn cli_kind_must_match_manifest() {
        let m = JobManifest::parse(BASE).unwrap();
        assert!(m.validate().is_ok());
        assert!(m.check_cli_kind(JobKind::Review).is_ok());
        let e = m.check_cli_kind(JobKind::Verify).unwrap_err();
        assert_eq!(e.code, "kind_mismatch");
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let with_extra = BASE.replace("\"job_id\": \"J1\",", "\"job_id\": \"J1\", \"future\": {\"x\":1},");
        let m = JobManifest::parse(&with_extra).unwrap();
        assert_eq!(m.job_id, "J1");
    }
}
