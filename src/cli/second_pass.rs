//! 二阶段补查零件（design §2.3）：needs 行窗解析、denylist/git-tracked 读守卫、证据段渲染、草稿注入段。
//! `second_pass_prompt`（第二趟 prompt 构造）随旧 fan-out 在 M21a 批 F-4 删除；scanner 的 needs 只记录不补跑（prd K6），
//! 本模块的 needs 二阶段由 M21b reviewer 消费（design R2）。模块级 `allow(dead_code)` 已随之去掉（C2 评审 #15）。

use crate::domain::Finding;
use std::collections::BTreeSet;
use std::path::Path;

/// 二阶段补查行窗（design §2.3）：lines 命中区间外扩 ±30 行。
const NEED_LINES_WINDOW: usize = 30;

/// 补查证据单条段字节帽（Important 修复：此前无上限——长单行文件上 ±30 行窗可逼近
/// read_capped 的 512KB 读帽，doc 线整文件 need 直接就是整文件，均绕开预算纪律）。
/// 量级对齐 verify.rs::SNIPPET_FULL_FILE_CAP（64KB）。
const NEED_SECTION_BYTE_CAP: usize = 64_000;

/// NeedEntry.lines 解析："a-b"（1-based、a<=b）→ Some((a,b))；缺失/坏格式 → None（整文件语义）。
/// `NeedEntry.lines` 的唯一解析口径（C3 起深挖入队也用它，评审 #13）：只认 `"a-b"`，
/// 其余（缺失 / 单个数字 / 倒置 / 非数字）一律 `None` = 无可用行锚。
pub(crate) fn parse_need_lines(lines: Option<&str>) -> Option<(u32, u32)> {
    let (a, b) = lines?.split_once('-')?;
    let a: u32 = a.trim().parse().ok()?;
    let b: u32 = b.trim().parse().ok()?;
    (a >= 1 && a <= b).then_some((a, b))
}

/// needs 读 denylist（E11-Q4）：即便 git-tracked 也拒——凭据类文件 + 工具自建/VCS 内部目录。
/// M18 批 1·1-2：判据本身已抽到 `deny::is_denylisted_path`（与 spec.rs 的 `is_sensitive_spec_path`
/// 共用单一真相源、语义=两清单并集——原本各自独有的判据互补齐），本函数只剩薄委托。
/// needs 读点仍不做内容级 PEM 嗅探（E11-Q4 已裁：路径已够收窄威胁面，与 spec 侧信任模型不同）。
fn is_needs_denylisted(rel: &str) -> bool {
    super::deny::is_denylisted_path(rel)
}

/// 拒读事件统一披露（T46②DRY + T46①入账，M18 批 1）：needs 读守卫拒读原分散两处 eprintln
/// 重复段——本函数收敛为单点：eprintln 供本地终端可见（既有行为不变）+ push 进 warnings
/// 供上游并入 `run.warnings`（CI/bot 可见，复用 M17-c 披露通道手法）。
fn deny_need_read(reason: &str, path: &str, warnings: &mut Vec<String>) {
    let msg = format!("needs 读被拒（{reason}）：{path}");
    eprintln!("redteam: {msg}");
    warnings.push(msg);
}

/// 二阶段补查 needs 读守卫（E11-Q4，dogfood 抓）：needs 是模型声明的路径（"我要读 X 文件"）——
/// 威胁模型 B（本地审不可信仓）下若不设限即为任意文件读。仅放行「git-tracked ∧ 非 denylist」。
/// git 判定失败（非 git 仓/git 缺失/命令报错）→ 保守拒（fail-closed，安全侧优先于可用性）。
/// 单次判定的便捷入口（生产已无调用方，留给用例与将来的一次性判定；每次现取跟踪文件集，同一轮多条路径请先
/// [`TrackedFiles::load`] 一次再逐条走 [`need_read_allowed_in`]）。
#[cfg(test)]
pub(crate) fn need_read_allowed(root: &Path, rel: &str) -> Result<(), &'static str> {
    need_read_allowed_in(&TrackedFiles::load(root), rel)
}

/// 读守卫（集合版，C2 Q9 IO2〔D15〕）：「非 denylist ∧ 在预取的跟踪文件集里」。判定口径与 git 的
/// `ls-files --error-unmatch -- <字面路径>` 逐条一致（测试以子进程版为对照）——rel 只做集合查找，
/// 不再作为 git 参数，`--cached` / `:(glob)` 一类参数或 pathspec 注入天然无从下手。
pub(crate) fn need_read_allowed_in(tracked: &TrackedFiles, rel: &str) -> Result<(), &'static str> {
    if is_needs_denylisted(rel) {
        return Err("敏感路径（denylist 命中）");
    }
    tracked.allows(rel)
}

/// 每轮预取的 git 跟踪文件集（C2 Q9 IO2〔D15〕）：一次 `rev-parse --show-prefix` + 一次 `ls-files -z --full-name -- :/`，
/// 取代读守卫逐项起子进程（派发路径每项一次、二阶段每条 need 一次；同 task 并发下子进程阻塞会让其余在途格一起停）。
/// 快照语义：一轮评审不改仓库，开跑时取一次即可。
pub struct TrackedFiles {
    state: TrackedState,
}

enum TrackedState {
    /// `prefix` = root 相对仓库顶层的前缀（顶层为空，否则以 `/` 结尾）；`files` = 仓库全部跟踪文件（顶层相对路径）；
    /// `root` 留给非 ASCII 路径回退逐条子进程判定（见 [`git_tracked_literal`]）。
    Listed { root: std::path::PathBuf, prefix: String, files: BTreeSet<String> },
    NotRepo,
    GitUnavailable,
}

#[cfg(test)]
thread_local! {
    /// 本线程 `TrackedFiles::load` 调用次数（测试断言「每轮只取一次」；current_thread 运行时下 run_deep 与测试同线程）。
    pub(crate) static TRACKED_LOADS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

impl TrackedFiles {
    /// 取集合失败（git 不可用 / 命令报错）→ 本轮读守卫整体 fail-closed，调用方据此发 warning（评审 #13：
    /// 这类失败原先只体现在逐格的「已拒绝（读守卫）」里，报告层看不见）。非 git 仓不算失败（本就该拒）。
    pub fn unavailable(&self) -> bool {
        matches!(self.state, TrackedState::GitUnavailable)
    }

    /// worktree 下必须用 `root`（`current_dir`），不能信进程 cwd——镜像 evidence/git.rs 的 git_at 手法。
    pub fn load(root: &Path) -> Self {
        #[cfg(test)]
        TRACKED_LOADS.with(|c| c.set(c.get() + 1));
        let git = |args: &[&str]| {
            std::process::Command::new("git")
                .current_dir(root)
                // `:/`（仓库顶层）是本函数自己给的 pathspec 魔法；外部若设了字面 pathspec，列表会是空的。
                .env_remove("GIT_LITERAL_PATHSPECS")
                .args(args)
                .output()
        };
        let prefix = match git(&["rev-parse", "--show-prefix"]) {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim_end_matches(['\n', '\r']).to_string(),
            Ok(_) => return TrackedFiles { state: TrackedState::NotRepo },
            Err(_) => return TrackedFiles { state: TrackedState::GitUnavailable },
        };
        match git(&["ls-files", "-z", "--full-name", "--", ":/"]) {
            Ok(out) if out.status.success() => {
                let files = out
                    .stdout
                    .split(|&b| b == 0)
                    .filter(|p| !p.is_empty())
                    .map(|p| String::from_utf8_lossy(p).into_owned())
                    .collect();
                TrackedFiles { state: TrackedState::Listed { root: root.to_path_buf(), prefix, files } }
            }
            Ok(_) => TrackedFiles { state: TrackedState::NotRepo },
            Err(_) => TrackedFiles { state: TrackedState::GitUnavailable },
        }
    }

    /// 与 git 字面 pathspec 同口径：空串拒；绝对路径拒；相对 root 解析 `.` / `..` / 重复斜杠（`..` 越出仓库顶层拒）；
    /// 命中跟踪文件本身，或是含跟踪文件的目录（`src` / `src/`）即放行；以 `/`、`/.`、`/..` 结尾只按目录匹配
    /// （`src/foo.rs/` 拒、`src/foo.rs/..` 即 `src` 放行）；大小写敏感。
    fn allows(&self, rel: &str) -> Result<(), &'static str> {
        const UNTRACKED: &str = "非 git-tracked";
        let (root, prefix, files) = match &self.state {
            TrackedState::Listed { root, prefix, files } => (root, prefix, files),
            TrackedState::NotRepo => return Err(UNTRACKED),
            TrackedState::GitUnavailable => return Err(GIT_DOWN),
        };
        // 非 ASCII 路径回退逐条子进程判定（评审 #13）：macOS 的 `core.precomposeunicode` 让 `ls-files` 输出按 NFC 预组合，
        // 而 need / 疑点里的路径可能是 NFD 拼写——逐字节比集合会与 C2 之前的口径分叉（NFD 索引的仓反向分叉）。这类路径罕见。
        if !rel.is_ascii() || !prefix.is_ascii() {
            return git_tracked_literal(root, rel);
        }
        if rel.is_empty() || rel.starts_with('/') {
            return Err(UNTRACKED);
        }
        let mut parts: Vec<&str> = Vec::new();
        for seg in prefix.split('/').chain(rel.split('/')) {
            match seg {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return Err(UNTRACKED);
                    }
                }
                s => parts.push(s),
            }
        }
        let path = parts.join("/");
        let dir_only = matches!(rel.rsplit('/').next(), Some("" | "." | ".."));
        let hit = if path.is_empty() {
            !files.is_empty()
        } else {
            let dir = format!("{path}/");
            (!dir_only && files.contains(&path)) || files.range(dir.clone()..).next().is_some_and(|f| f.starts_with(&dir))
        };
        if hit { Ok(()) } else { Err(UNTRACKED) }
    }
}

/// 取跟踪集失败（git 不可用 / 命令报错）的统一文案：整轮读守卫 fail-closed，调用方据此披露。
pub(crate) const GIT_DOWN: &str = "git 不可用（fail-closed）";

/// 逐条字面 pathspec 判定（C2 之前的实现）：非 ASCII 路径的回退路径，与 `ls-files --error-unmatch` 逐字节同口径。
fn git_tracked_literal(root: &Path, rel: &str) -> Result<(), &'static str> {
    let out = std::process::Command::new("git")
        .current_dir(root)
        // 字面路径：外部若没设这个变量，`:(glob)` 一类 pathspec 魔法会被 git 解释——守卫要按字面名判定。
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(["ls-files", "--error-unmatch", "--", rel])
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(_) => Err("非 git-tracked"),
        Err(_) => Err(GIT_DOWN),
    }
}

/// 单条 need → `## 补查证据` 段：lines 合法 → ±30 行窗；缺失/坏格式 → 整文件。
/// 路径安全走 ToolExecutor 不变量（绝对路径/`..`/symlink 逃逸/不可读 → None、不 panic）+
/// 读守卫（git-tracked ∧ 非 denylist，E11-Q4；查本轮预取的跟踪文件集）；守卫拒则 skip + 披露
/// （`deny_need_read`：eprintln + 入 `warnings`，T46①②）。
pub(crate) fn need_section(
    ex: &crate::tools::ToolExecutor,
    tracked: &TrackedFiles,
    need: &crate::domain::NeedEntry,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if let Err(reason) = need_read_allowed_in(tracked, &need.path) {
        deny_need_read(reason, &need.path, warnings);
        return None;
    }
    let text = ex.read_capped(&need.path)?;
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    if n == 0 {
        return None;
    }
    let (a, b) = match parse_need_lines(need.lines.as_deref()) {
        Some((la, lb)) => (
            (la as usize).saturating_sub(NEED_LINES_WINDOW).max(1).min(n),
            (lb as usize).saturating_add(NEED_LINES_WINDOW).min(n),
        ),
        None => (1, n),
    };
    let b = b.max(a);
    let mut body = lines[a - 1..b].join("\n");
    if body.len() > NEED_SECTION_BYTE_CAP {
        let mut cut = NEED_SECTION_BYTE_CAP;
        while cut > 0 && !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
        body.push_str("\n…（截断）");
    }
    Some(format!("## 补查证据\n{}:{}-{}\n{}\n\n", need.path, a, b, body))
}

/// 草稿注入段（design §2.3）：第一趟 findings 以 CONFIRM-or-REVISE 契约随第二趟 prompt 带回；
/// 空草稿也渲染块（契约「不再声明 needs」对零草稿格同样生效）。
pub(crate) fn draft_block(drafts: &[Finding]) -> String {
    let mut s = String::from(
        "\n<first_pass_draft>\nYour first pass produced these draft findings. CONFIRM or REVISE each using the\nnewly added evidence — drop a draft ONLY if the evidence refutes it, and do not\nre-declare needs.\n",
    );
    for d in drafts {
        let quote_head: String = d.quote.chars().take(80).collect();
        s.push_str(&format!(
            "- [{}] {} @ {}:{}（{}）\n",
            d.severity.as_str(), d.title, d.file, d.line_start, quote_head
        ));
    }
    s.push_str("</first_pass_draft>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 非 ASCII 路径与逐条 git 判定同口径（评审 #13）：macOS 的 `core.precomposeunicode` 让 `ls-files` 输出按 NFC 预组合，
    /// need 里可能是 NFD 拼写——集合逐字节比会与旧口径分叉，故这类路径回退子进程判定，结果以 git 为准。
    #[test]
    fn non_ascii_paths_follow_the_literal_git_guard() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(root).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        let nfc = "caf\u{e9}.rs"; // é 单码点
        let nfd = "cafe\u{301}.rs"; // e + 组合重音
        std::fs::write(root.join(nfc), "fn a() {}\n").unwrap();
        git(&["add", "."]);
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let tracked = TrackedFiles::load(root);
        for rel in [nfc, nfd] {
            let git_says = std::process::Command::new("git")
                .current_dir(root)
                .env("GIT_LITERAL_PATHSPECS", "1")
                .args(["ls-files", "--error-unmatch", "--", rel])
                .output()
                .unwrap()
                .status
                .success();
            assert_eq!(need_read_allowed_in(&tracked, rel).is_ok(), git_says, "{rel:?} 与 git 判定不一致");
        }
    }

    #[test]
    fn needs_rejects_untracked_and_sensitive_paths() {
        // E11-Q4 dogfood 抓：二阶段补查 needs 是模型声明路径——威胁模型 B 下不设限即任意文件读。
        // git repo fixture：tracked src/foo.rs 提交后放行；untracked .env / 工具自建
        // .redteam/proj.db 命中 denylist 拒（即便后者恰好也 untracked，denylist 独立于 git 判定）。
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/foo.rs"), "fn a(){}\n").unwrap();
        crate::cli::incremental::incremental_tests::git_repo(root);
        crate::cli::incremental::incremental_tests::commit_all(root, "init");
        std::fs::write(root.join(".env"), "SECRET=x\n").unwrap(); // 提交后再写，保证真 untracked
        assert!(need_read_allowed(root, "src/foo.rs").is_ok(), "tracked 源文件放行");
        assert!(need_read_allowed(root, ".env").is_err(), "untracked 敏感文件拒");
        assert!(need_read_allowed(root, ".redteam/proj.db").is_err(), "工具自建 untracked 拒（denylist）");

        // fail-closed：非 git 仓（或 git 判定失败）时即便文件存在也保守拒，不放行。
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::write(tmp2.path().join("bare.rs"), "fn b(){}\n").unwrap();
        assert!(need_read_allowed(tmp2.path(), "bare.rs").is_err(), "非 git 仓 fail-closed 拒");

        // dogfood 2026-07-17：git 参数/pathspec 注入——`--` + GIT_LITERAL_PATHSPECS 后，
        // 选项样式路径与 pathspec 魔法均不能让未跟踪内容蒙混放行。
        assert!(need_read_allowed(root, "--cached").is_err(), "选项样式路径须拒（-- 停止选项解析）");
        assert!(need_read_allowed(root, ":(glob)**/*.env").is_err(), "pathspec 魔法须拒（字面路径校验）");
        assert!(need_read_allowed(root, ":!src").is_err(), "pathspec 排除魔法须拒");
    }

    /// 集合版读守卫与原子进程版（`git ls-files --error-unmatch -- <字面路径>`）逐条同口径（C2 Q9〔D15〕「拒绝口径不变」）：
    /// root 在仓库顶层 / 子目录 / 非 git 目录三种形态下，同一组路径（目录、`./` `..` 与重复斜杠、尾斜杠、大小写、空串、
    /// 绝对路径、参数与 pathspec 注入样式、denylist、暂存与未跟踪）的放行与拒绝逐条一致，拒绝原因文案也一致。
    #[test]
    fn tracked_set_guard_matches_pathspec_guard() {
        fn by_pathspec(root: &Path, rel: &str) -> Result<(), &'static str> {
            // 原实现（逐条起子进程），作对照
            if is_needs_denylisted(rel) {
                return Err("敏感路径（denylist 命中）");
            }
            match std::process::Command::new("git")
                .current_dir(root)
                .env("GIT_LITERAL_PATHSPECS", "1")
                .args(["ls-files", "--error-unmatch", "--", rel])
                .output()
            {
                Ok(out) if out.status.success() => Ok(()),
                Ok(_) => Err("非 git-tracked"),
                Err(_) => Err("git 不可用（fail-closed）"),
            }
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src/sub")).unwrap();
        std::fs::write(root.join("src/foo.rs"), "fn a(){}\n").unwrap();
        std::fs::write(root.join("src/sub/bar.rs"), "fn b(){}\n").unwrap();
        std::fs::write(root.join("top.rs"), "fn t(){}\n").unwrap();
        crate::cli::incremental::incremental_tests::git_repo(root);
        crate::cli::incremental::incremental_tests::commit_all(root, "init");
        std::fs::write(root.join("staged.rs"), "fn s(){}\n").unwrap();
        let out = std::process::Command::new("git").current_dir(root).args(["add", "staged.rs"]).output().unwrap();
        assert!(out.status.success());
        std::fs::write(root.join("untracked.rs"), "fn u(){}\n").unwrap();
        std::fs::write(root.join(".env"), "SECRET=x\n").unwrap();
        let bare = tempfile::tempdir().unwrap();
        std::fs::write(bare.path().join("bare.rs"), "fn b(){}\n").unwrap();
        let corpus = [
            "src/foo.rs", "src", "src/", "src/sub", "./src/foo.rs", "src//foo.rs", "src/./foo.rs", "src/sub/../foo.rs",
            "src/foo.rs/", "src/foo.rs/.", "src/foo.rs/..", "src/.", "src/sub/..", "src/sub/../", "./", "SRC/foo.rs", "staged.rs", "untracked.rs", "nonexistent.rs", "", ".", "..", "../top.rs",
            "sub/bar.rs", "bar.rs", "/etc/passwd", "--cached", ":(glob)**/*.rs", ":!src", ".env", ".redteam/proj.db", "bare.rs",
        ];
        for dir in [root.to_path_buf(), root.join("src"), bare.path().to_path_buf()] {
            let tracked = TrackedFiles::load(&dir);
            for rel in corpus {
                assert_eq!(need_read_allowed_in(&tracked, rel), by_pathspec(&dir, rel), "root={dir:?} rel={rel:?}");
            }
        }
    }
}
