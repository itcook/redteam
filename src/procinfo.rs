//! 进程存活探测 + 身份锚（E12 并发组）：区分「原 job worker 仍存活」与「pid 已被 OS 复用」。
//! 存活探测跨平台（`kill(pid,0)`）；身份锚=进程启动时刻，仅 macOS 有实现（`proc_pidinfo`），
//! 其它平台返回 None → 降级为纯存活探测。

/// 读进程启动时刻（Unix 秒），作 pid 身份锚。读不到（进程已死/无权限/非 macOS）→ None。
#[cfg(target_os = "macos")]
pub fn start_time(pid: u32) -> Option<u64> {
    // libc 0.2.x 原生提供 proc_bsdinfo（含 pbi_start_tvsec: u64）与 PROC_PIDTBSDINFO=3。
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    // proc_pidinfo 成功时返回写入字节数（== 结构体大小）；失败返回 0 或负。
    if n == size {
        Some(info.pbi_start_tvsec)
    } else {
        None
    }
}

#[cfg(not(target_os = "macos"))]
pub fn start_time(_pid: u32) -> Option<u64> {
    None
}

/// 判 job worker 进程是否仍存活（且身份未变）：
/// - `kill(pid,0)` ESRCH（进程不存在）→ false；
/// - 存活但记录了 `recorded_start` 且当前 start-time 不符 → false（pid 已被复用，原进程已死）；
/// - 存活但记录了锚却读不到当前 start-time → false（本属我们的活进程必能读到；读不到=非我们的）；
/// - 存活且（无锚[旧状态/非 macOS 降级] 或 锚吻合）→ true。
pub fn is_worker_alive(pid: u32, recorded_start: Option<u64>) -> bool {
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    // ret==0：可发信号=存活；ret==-1 且 errno==EPERM：存在但无权限（仍算存活）。
    let alive = ret == 0
        || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
    if !alive {
        return false;
    }
    match recorded_start {
        None => true, // 无锚 → 降级纯存活探测
        Some(rec) => match start_time(pid) {
            Some(cur) => cur == rec,
            None => false, // 存活却读不到锚 → 非原进程，保守判死
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 锚依赖：非 macOS 无 start_time 实现（恒 None），此测必挂——门控（dogfood F1 抓漏）。
    #[cfg(target_os = "macos")]
    #[test]
    fn start_time_self_is_stable() {
        let me = std::process::id();
        let a = start_time(me);
        let b = start_time(me);
        assert!(a.is_some(), "本进程 start_time 应可读");
        assert_eq!(a, b, "同进程两次读取应一致");
    }

    #[test]
    fn alive_self_with_matching_anchor() {
        let me = std::process::id();
        let anchor = start_time(me);
        assert!(is_worker_alive(me, anchor), "本进程 + 吻合锚 → 活");
    }

    /// 锚依赖：非 macOS 下伪造锚塌缩成 None → 降级判活 → 断言反转，此测必挂——门控（dogfood F1）。
    #[cfg(target_os = "macos")]
    #[test]
    fn alive_self_reused_anchor_is_dead() {
        let me = std::process::id();
        // 伪造一个不吻合的锚 → 判定 pid 已被复用（原进程已死）
        let bogus = start_time(me).map(|t| t.wrapping_add(999_999));
        assert!(!is_worker_alive(me, bogus), "锚不符 → 判死（pid 复用）");
    }

    #[test]
    fn alive_self_no_anchor_degrades_to_liveness() {
        let me = std::process::id();
        assert!(is_worker_alive(me, None), "无锚 → 降级纯存活探测 → 活");
    }

    #[test]
    fn dead_pid_is_not_alive() {
        // 起一个立即退出的子进程、回收，再探测其 pid → 已死
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        let mut child = child;
        child.wait().unwrap(); // 回收 zombie，pid 释放
        assert!(!is_worker_alive(pid, None), "已回收的 pid → 死");
    }
}
