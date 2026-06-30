use crate::hash::fnv1a64_hex;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 状态根目录：$BRK_STATE_DIR → $TMPDIR/breakpoints → /tmp/breakpoints。
pub fn state_root() -> PathBuf {
    if let Ok(d) = std::env::var("BRK_STATE_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    if let Ok(t) = std::env::var("TMPDIR") {
        if !t.is_empty() {
            return PathBuf::from(t).join("breakpoints");
        }
    }
    PathBuf::from("/tmp/breakpoints")
}

/// 按 workspace 路径 fnv1a64 分桶（canonicalize 失败则用原路径字符串）。
pub fn workspace_bucket(root: &Path) -> PathBuf {
    let canon = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let key = fnv1a64_hex(&[&canon.to_string_lossy()]);
    state_root().join(key)
}

pub fn jobs_dir(ws: &Path) -> PathBuf { ws.join("jobs") }
pub fn job_json(ws: &Path, id: &str) -> PathBuf { jobs_dir(ws).join(format!("{id}.json")) }
pub fn job_log(ws: &Path, id: &str) -> PathBuf { jobs_dir(ws).join(format!("{id}.log")) }
pub fn job_result(ws: &Path, id: &str) -> PathBuf { jobs_dir(ws).join(format!("{id}.result.json")) }

/// 原子写：创建父目录 → 写唯一 tmp → rename 覆盖目标。
pub fn atomic_write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let fname = path.file_name().and_then(|s| s.to_str()).unwrap_or("f");
    let tmp = dir.join(format!(".{fname}.tmp.{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// 解析 TTL 字符串（"60s"/"30m"/"24h"/"2d"）；任何解析失败回落 24h。
pub fn parse_ttl(s: &str) -> Duration {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: u64 = match num.parse() {
        Ok(n) => n,
        Err(_) => return Duration::from_secs(86400),
    };
    let secs = match unit.trim() {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => return Duration::from_secs(86400),
    };
    Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn state_root_prefers_brk_state_dir() {
        let _g = ENV_LOCK.lock().unwrap();
        // SAFETY: ENV_LOCK 串行化 env 变更
        unsafe { std::env::set_var("BRK_STATE_DIR", "/custom/brk"); }
        assert_eq!(state_root(), PathBuf::from("/custom/brk"));
        unsafe { std::env::remove_var("BRK_STATE_DIR"); }
    }

    #[test]
    fn workspace_bucket_is_stable_hash_under_root() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var("BRK_STATE_DIR", "/s"); }
        let b1 = workspace_bucket(Path::new("/proj/a"));
        let b2 = workspace_bucket(Path::new("/proj/a"));
        assert_eq!(b1, b2, "same root → same bucket");
        assert!(b1.starts_with("/s"), "bucket under state_root");
        assert_ne!(workspace_bucket(Path::new("/proj/b")), b1, "different root → different bucket");
        unsafe { std::env::remove_var("BRK_STATE_DIR"); }
    }

    #[test]
    fn atomic_write_creates_dirs_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("jobs/x.json");
        atomic_write(&p, b"hello").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "hello");
        // 覆盖写
        atomic_write(&p, b"world").unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "world");
        // 不留 tmp 文件
        let leftover: Vec<_> = std::fs::read_dir(dir.path().join("jobs")).unwrap()
            .filter_map(|e| e.ok()).filter(|e| e.file_name().to_string_lossy().contains(".tmp.")).collect();
        assert!(leftover.is_empty(), "no tmp leftover");
    }

    #[test]
    fn parse_ttl_units() {
        assert_eq!(parse_ttl("60s"), Duration::from_secs(60));
        assert_eq!(parse_ttl("30m"), Duration::from_secs(1800));
        assert_eq!(parse_ttl("24h"), Duration::from_secs(86400));
        assert_eq!(parse_ttl("2d"), Duration::from_secs(172800));
        assert_eq!(parse_ttl("garbage"), Duration::from_secs(86400), "回落 24h");
    }
}
