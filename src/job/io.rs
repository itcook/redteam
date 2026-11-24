//! job 目录路径与原子写。
//!
//! 契约（§1）：
//! - 相对路径以 **job 目录**为基准，解析（canonicalize）后必须落在 job 目录内；
//!   `paths.repo` 是例外——允许 `../repo` 兄弟目录。
//! - 只写 `paths.out`；`report.json` / `state.json` 恒 `*.tmp` → `rename`（`SIGKILL` 只会留下无报告或上一份完整报告）。
//! - 不依赖 cwd。

use crate::job::manifest::Paths;
use crate::job::status::JobFailure;
use std::io::Write;
use std::path::{Path, PathBuf};

/// 命令行给的 job 目录 → canonical 绝对路径（必须存在且是目录）。
pub fn canonical_job_dir(arg: &Path) -> Result<PathBuf, JobFailure> {
    let canon = arg
        .canonicalize()
        .map_err(|e| JobFailure::new("job_manifest_unreadable", Some(format!("{}: {e}", arg.display()))))?;
    if !canon.is_dir() {
        return Err(JobFailure::new("job_manifest_unreadable", Some(format!("{} is not a directory", canon.display()))));
    }
    Ok(canon)
}

/// `job.json` 是否存在（P13：定位不到 job.json → 退出码 2、不写报告）。
pub fn manifest_path(job_dir: &Path) -> PathBuf {
    job_dir.join("job.json")
}

/// 解析 job 目录内的声明路径；绝对路径原样，但仍须落在 job 目录内。
pub fn resolve_inside(job_dir: &Path, rel: &str, key: &str) -> Result<PathBuf, JobFailure> {
    let raw = Path::new(rel);
    let candidate = if raw.is_absolute() { raw.to_path_buf() } else { job_dir.join(raw) };
    let canon = candidate
        .canonicalize()
        .map_err(|_| JobFailure::new("input_path_invalid", Some(key.to_string())))?;
    if !canon.starts_with(job_dir) {
        return Err(JobFailure::new("input_path_invalid", Some(key.to_string())));
    }
    Ok(canon)
}

/// `paths.repo`：允许 `../repo` 兄弟目录（plan 1.1），其余位置仍拒绝。
pub fn resolve_repo(job_dir: &Path, rel: &str) -> Result<PathBuf, JobFailure> {
    let raw = Path::new(rel);
    let candidate = if raw.is_absolute() { raw.to_path_buf() } else { job_dir.join(raw) };
    let canon = candidate
        .canonicalize()
        .map_err(|_| JobFailure::new("input_path_invalid", Some("paths.repo".into())))?;
    if !canon.is_dir() {
        return Err(JobFailure::new("input_path_invalid", Some("paths.repo".into())));
    }
    if canon.starts_with(job_dir) {
        return Ok(canon);
    }
    if canon.parent() != job_dir.parent() {
        return Err(JobFailure::new("input_path_invalid", Some("paths.repo".into())));
    }
    Ok(canon)
}

/// `paths.out`：必须落在 job 目录内；不存在则创建（hibot 通常会预先建好）。
pub fn out_dir(job_dir: &Path, paths: &Paths) -> Result<PathBuf, JobFailure> {
    let raw = Path::new(&paths.out);
    if raw.is_absolute() {
        return Err(JobFailure::new("input_path_invalid", Some("paths.out".into())));
    }
    // **创建之前**先做词法校验：任何 `..` 一律拒——否则 `create_dir_all` 会在 job 外留下目录
    // （检查失败也已经有副作用）。
    let mut rel = PathBuf::new();
    for c in raw.components() {
        match c {
            std::path::Component::Normal(part) => rel.push(part),
            std::path::Component::CurDir => {}
            _ => return Err(JobFailure::new("input_path_invalid", Some("paths.out".into()))),
        }
    }
    let candidate = job_dir.join(rel);
    std::fs::create_dir_all(&candidate)
        .map_err(|_| JobFailure::new("input_path_invalid", Some("paths.out".into())))?;
    let canon = candidate
        .canonicalize()
        .map_err(|_| JobFailure::new("input_path_invalid", Some("paths.out".into())))?;
    if !canon.starts_with(job_dir) {
        return Err(JobFailure::new("input_path_invalid", Some("paths.out".into())));
    }
    Ok(canon)
}

/// 原子写：`<path>.tmp` 写完 → `rename(<path>)`。正常结束与 partial 都用它（P8/D73）。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 不跟随预置符号链接：先 unlink（只删链接本身）再 O_EXCL 创建；create_new 在 tmp 已存在时失败，
    // 防「预置 report.json.tmp → /etc/xxx」把写入引到 job 外。
    if tmp.symlink_metadata().is_ok() {
        std::fs::remove_file(&tmp)?;
    }
    {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        // 落盘后再 rename：SIGKILL 留下 tmp 而不是半截正式文件（tmp 不是终态信号）。
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// 序列化 + 原子写 JSON。
pub fn atomic_write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| format!("serialize {}: {e}", path.display()))?;
    atomic_write(path, &bytes).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_leaves_only_complete_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("report.json");
        atomic_write(&p, b"{\"a\":1}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":1}");
        assert!(!d.path().join("report.json.tmp").exists());
        atomic_write(&p, b"{\"a\":2}").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\":2}");
    }

    #[test]
    fn atomic_write_does_not_follow_preexisting_symlink() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("outside.txt");
        std::fs::write(&outside, b"keep").unwrap();
        let target = d.path().join("report.json");
        let tmp = d.path().join("report.json.tmp");
        std::os::unix::fs::symlink(&outside, &tmp).unwrap();
        atomic_write(&target, b"new").unwrap();
        assert_eq!(std::fs::read(&outside).unwrap(), b"keep", "不得截断 tmp 指向的外部文件");
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
    }

    #[test]
    fn resolve_inside_rejects_escape_and_missing() {
        let d = tempfile::tempdir().unwrap();
        let job = d.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("ok.txt"), "x").unwrap();
        std::fs::write(d.path().join("outside.txt"), "x").unwrap();
        let job = job.canonicalize().unwrap();
        assert!(resolve_inside(&job, "ok.txt", "k").is_ok());
        let e = resolve_inside(&job, "../outside.txt", "k").unwrap_err();
        assert_eq!(e.full_code(), "input_path_invalid:k");
        let e2 = resolve_inside(&job, "nope.txt", "k").unwrap_err();
        assert_eq!(e2.full_code(), "input_path_invalid:k");
        // 绝对路径也须落在 job 目录内
        let abs = d.path().join("outside.txt").canonicalize().unwrap();
        assert!(resolve_inside(&job, abs.to_str().unwrap(), "k").is_err());
    }

    #[test]
    fn repo_may_be_a_sibling_but_not_elsewhere() {
        let d = tempfile::tempdir().unwrap();
        let job = d.path().join("job");
        let repo = d.path().join("repo");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        let job = job.canonicalize().unwrap();
        assert_eq!(resolve_repo(&job, "../repo").unwrap(), repo.canonicalize().unwrap());
        let e = resolve_repo(&job, "../../etc").unwrap_err();
        assert_eq!(e.full_code(), "input_path_invalid:paths.repo");
    }

    #[test]
    fn out_dir_is_created_inside_job_dir() {
        let d = tempfile::tempdir().unwrap();
        let job = d.path().join("job");
        std::fs::create_dir_all(&job).unwrap();
        let job = job.canonicalize().unwrap();
        let paths = Paths { repo: "../repo".into(), out: "out".into(), config: "config/executor.redteam.json".into() };
        let out = out_dir(&job, &paths).unwrap();
        assert_eq!(out, job.join("out"));
        let bad = Paths { out: "../escape".into(), ..paths };
        let outside = job.parent().unwrap().join("escape");
        let _ = std::fs::remove_dir_all(&outside);
        assert!(out_dir(&job, &bad).is_err());
        assert!(!outside.exists(), "越界拒绝不得先 create_dir_all 留下目录");
    }
}
