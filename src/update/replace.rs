//! 原子替换、自检、排他锁与插件路径守卫（所有路径均为显式参数，便于 tempdir 测试）。

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::io::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::update::version;

/// 更新锁文件名（与目标二进制同目录）。
pub const LOCK_FILE_NAME: &str = ".redteam-update.lock";
/// 新二进制 `--version` 自检的超时。
pub const SELF_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// 锁的选择：`libc::flock`（计划 §15：`libc` 已在依赖里，零新依赖）。
///
/// 不用 `fs2` 的理由：只差一个 `lock_exclusive` 包装，却要为同目录锁多引一层 crate；
/// flock 的语义（open file description 级别、进程退出自动释放）正好覆盖「并发两次 update」。
/// 锁是**阻塞**获取：第二个进程等第一个提交完成后继续，而不是报错退出。
pub struct UpdateLock {
    file: File,
}

impl UpdateLock {
    /// 阻塞获取 `{exe_dir}/.redteam-update.lock` 的排他锁。调用方应在 `spawn_blocking` 中执行。
    pub fn acquire(exe: &Path) -> Result<Self> {
        let exe = resolve_target(exe);
        let dir = exe_dir(&exe)?;
        let path = dir.join(LOCK_FILE_NAME);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .with_context(|| format!("无法打开更新锁 {}", path.display()))?;
        // SAFETY: `file` 在 fd 有效期内存活到 `Self` 释放。
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if rc != 0 {
            bail!(
                "获取更新锁 {} 失败: {}",
                path.display(),
                std::io::Error::last_os_error()
            );
        }
        Ok(Self { file })
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        // SAFETY: 同上；解锁失败也无从补救，进程退出时内核会释放。
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// 已写入目标同目录、等待提交的临时二进制。
///
/// Drop 时删除临时文件（除非已 `commit`）；`commit` 是唯一的替换点。
pub struct StagedBinary {
    tmp: PathBuf,
    target: PathBuf,
    committed: bool,
}

impl StagedBinary {
    /// 写入同目录临时文件并设置 0755；不触碰目标文件。
    pub fn write(exe: &Path, bytes: &[u8]) -> Result<Self> {
        let exe = resolve_target(exe);
        let dir = exe_dir(&exe)?;
        let tmp = dir.join(format!(
            ".redteam-update-{}-{}.tmp",
            std::process::id(),
            unique_suffix()
        ));

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .with_context(|| format!("无法写入 {}", tmp.display()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                let _ = fs::remove_file(&tmp);
                anyhow::anyhow!("写入临时文件失败（现有二进制未改动）: {error}")
            })?;
        drop(file);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755)).map_err(|error| {
                let _ = fs::remove_file(&tmp);
                anyhow::anyhow!("设置可执行权限失败（现有二进制未改动）: {error}")
            })?;
        }

        Ok(Self {
            tmp,
            target: exe,
            committed: false,
        })
    }

    /// 临时文件路径（替换前的 `--version` 自检用）。
    pub fn path(&self) -> &Path {
        &self.tmp
    }

    /// 原子替换目标文件；失败时目标文件保持不变。
    pub fn commit(mut self) -> Result<()> {
        fs::rename(&self.tmp, &self.target).map_err(|error| {
            anyhow::anyhow!("替换二进制失败（现有二进制未改动）: {error}")
        })?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedBinary {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.tmp);
        }
    }
}

/// 执行 `path --version`，要求退出 0 且报告期望版本。
pub async fn self_check(path: &Path, expected_version: &str) -> Result<()> {
    let mut command = tokio::process::Command::new(path);
    command.arg("--version").kill_on_drop(true);

    let output = match tokio::time::timeout(SELF_CHECK_TIMEOUT, command.output()).await {
        Err(_) => bail!(
            "新版本自检超时（{} ms，现有二进制未改动）",
            SELF_CHECK_TIMEOUT.as_millis()
        ),
        Ok(Err(error)) => bail!("新版本自检无法执行（现有二进制未改动）: {error}"),
        Ok(Ok(output)) => output,
    };
    if !output.status.success() {
        bail!(
            "新版本自检失败（--version 退出码 {}，现有二进制未改动）",
            output.status.code().unwrap_or(-1)
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    let Some(rest) = text.strip_prefix("redteam ") else {
        bail!("新版本自检失败（--version 输出异常: {text}，现有二进制未改动）");
    };
    let reported = rest.split_whitespace().next().unwrap_or_default();
    if !version::same_version(reported, expected_version) {
        bail!(
            "新版本自检失败（--version 报告 {reported}，期望 {expected_version}，现有二进制未改动）"
        );
    }
    Ok(())
}

/// 插件渠道守卫（计划 §10 / 7.0、7.4）：`plugins/redteam/bin/` 下的二进制由插件更新，
/// 自更新默认拒写——否则 `git pull`/市场更新与 `redteam update` 会互相覆盖。
pub fn is_plugin_path(exe: &Path) -> bool {
    let parts: Vec<String> = exe
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    parts
        .windows(3)
        .any(|window| window[0] == "plugins" && window[1] == "redteam" && window[2] == "bin")
}

/// 插件路径的拒绝文案（`--force` 才写）。
pub fn plugin_guard_message(exe: &Path) -> String {
    format!(
        "当前二进制在插件目录（{}）下：插件版随插件更新（marketplace / git pull），或在本仓跑 `make build`。\n\
         确认要覆盖插件二进制，请加 --force。",
        exe.display()
    )
}

/// 解析软链到真实目标；路径不存在或无权解析时回退原路径。
///
/// macOS 的 `std::env::current_exe()` 可能返回调用时的软链路径；若不解析，
/// `rename(tmp, exe)` 会把链接本身替换成普通文件，真实二进制仍是旧版本。
pub fn resolve_target(exe: &Path) -> PathBuf {
    std::fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf())
}

fn exe_dir(exe: &Path) -> Result<PathBuf> {
    exe.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow::anyhow!("无法定位当前可执行文件所在目录"))
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_paths_are_detected() {
        assert!(is_plugin_path(Path::new(
            "/Users/x/repo/plugins/redteam/bin/darwin/aarch64/redteam"
        )));
        assert!(is_plugin_path(Path::new("/Users/x/repo/plugins/redteam/bin/redteam")));
        assert!(!is_plugin_path(Path::new("/Users/x/.local/bin/redteam")));
        // 目录名像但不是插件布局：不误伤
        assert!(!is_plugin_path(Path::new("/Users/x/plugins/other/bin/redteam")));
        assert!(!is_plugin_path(Path::new("/Users/x/plugins/redteam/redteam")));
    }

    #[test]
    fn staged_write_then_commit_replaces_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("redteam");
        fs::write(&exe, b"old").unwrap();

        let staged = StagedBinary::write(&exe, b"new").unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"old", "写入阶段不得触碰旧件");
        let staged_path = staged.path().to_path_buf();
        assert!(staged_path.is_file());
        staged.commit().unwrap();
        assert_eq!(fs::read(&exe).unwrap(), b"new");
        assert!(!staged_path.exists(), "提交后临时文件应消失");
    }

    #[test]
    fn dropped_staged_binary_leaves_old_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("redteam");
        fs::write(&exe, b"old").unwrap();
        let staged = StagedBinary::write(&exe, b"new").unwrap();
        let staged_path = staged.path().to_path_buf();
        drop(staged);
        assert_eq!(fs::read(&exe).unwrap(), b"old");
        assert!(!staged_path.exists());
    }

    #[tokio::test]
    async fn self_check_requires_the_expected_version() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("fake");
        fs::write(&fake, b"#!/bin/sh\necho 'redteam 9.9.9'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(self_check(&fake, "v9.9.9").await.is_ok(), "v 前缀不影响比较");
        let error = self_check(&fake, "v0.1.0").await.unwrap_err().to_string();
        assert!(error.contains("9.9.9") && error.contains("0.1.0"), "{error}");
    }

    #[test]
    fn lock_serializes_two_holders() {
        // 同进程两次 open 得到不同的 open file description，flock 会互斥；
        // 第二次获取放到线程里，确认它会等第一次释放。
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("redteam");
        fs::write(&exe, b"x").unwrap();
        let held = UpdateLock::acquire(&exe).unwrap();

        let exe2 = exe.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let started = std::time::Instant::now();
            let _lock = UpdateLock::acquire(&exe2).unwrap();
            tx.send(started.elapsed()).unwrap();
        });

        std::thread::sleep(Duration::from_millis(150));
        assert!(rx.try_recv().is_err(), "第二个持有者不应在锁释放前返回");
        drop(held);
        let waited = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(waited >= Duration::from_millis(150), "第二个持有者等了 {waited:?}");
        handle.join().unwrap();
    }
}
