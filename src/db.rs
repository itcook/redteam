//! `.redteam/proj.db` 持久层：dismissed 判例台账 + jobs/cells/results 评审运行时与评审史。
//! 设计定案见 .doc/design/2026-07-03-dismissed-ledger-machinization-design.md：
//! 单库单连接、WAL、运行时结构存整列 JSON（行数个位~百位，拆列是过度建模）。

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::path::Path;

/// 库文件名（挂在项目 `.redteam/` 下）。M21a-b 并行期与 dev 二进制的 `proj.db` **分库**（用户裁定 2026-09-08：
/// M21c 切名前 dev `redteam` 与分支产物在同一仓 dogfood，v4 的 `cells.role NOT NULL` 会让 dev 的 checkpoint 写失败、
/// 旧库又被 v4 守卫拒开——两者不能共用一个文件）；M21c 切名时定回 `proj.db` 与切换程序（followup）。
pub const DB_FILE: &str = "proj.db";

/// schema 版本（`PRAGMA user_version`）。v4 = M21 一次重置（D10-2）：cells 带 role 列、疑点台账与队列快照表壳。
/// 低于此版本且有数据的库**拒开不迁移**（守卫见 `setup_schema`）。
pub const USER_VERSION: i64 = 4;
/// v4 全部用户表（与 `SCHEMA_SQL` 一一对应；守卫用它识别「是不是 redteam 项目库」与「库是否为空」）。
const TABLES: &[&str] = &[
    "dismissed", "dismissed_history", "resolved", "resolved_history", "jobs", "cells", "results", "queue_items", "suspicions",
];

/// v4 建表语句（`CREATE TABLE IF NOT EXISTS`，幂等）。`queue_items` = 深挖队列快照（resume 读快照重跑未完项，M21b 写）；
/// `suspicions` = 疑点台账（quote 锚 + hint + 状态 + 最近作业 + 激活次数，M21c 写）。本里程碑只建不写（design §8.3 / D11-7）。
const SCHEMA_SQL: &str = "CREATE TABLE IF NOT EXISTS dismissed (
               id TEXT PRIMARY KEY,
               status TEXT NOT NULL,
               title TEXT NOT NULL DEFAULT '',
               file TEXT NOT NULL DEFAULT '',
               quote TEXT NOT NULL DEFAULT '',
               severity TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL,
               round TEXT NOT NULL DEFAULT '',
               updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d','now'))
             );
             CREATE TABLE IF NOT EXISTS dismissed_history (
               seq INTEGER PRIMARY KEY AUTOINCREMENT,
               id TEXT NOT NULL,
               status TEXT NOT NULL,
               round TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );
             CREATE TABLE IF NOT EXISTS resolved (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL DEFAULT '',
               file TEXT NOT NULL DEFAULT '',
               quote TEXT NOT NULL DEFAULT '',
               severity TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               round TEXT NOT NULL DEFAULT '',
               updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d','now'))
             );
             CREATE TABLE IF NOT EXISTS resolved_history (
               seq INTEGER PRIMARY KEY AUTOINCREMENT,
               id TEXT NOT NULL,
               round TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );
             CREATE TABLE IF NOT EXISTS jobs (
               job_id TEXT PRIMARY KEY,
               state_json TEXT NOT NULL,
               args_json TEXT NOT NULL DEFAULT '',
               manifest_json TEXT NOT NULL DEFAULT '',
               created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );
             CREATE TABLE IF NOT EXISTS cells (
               job_id TEXT NOT NULL,
               cell TEXT NOT NULL,
               role TEXT NOT NULL,
               record_json TEXT NOT NULL,
               PRIMARY KEY (job_id, cell)
             );
             CREATE TABLE IF NOT EXISTS results (
               job_id TEXT PRIMARY KEY,
               json TEXT NOT NULL,
               md TEXT NOT NULL DEFAULT '',
               created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );
             CREATE TABLE IF NOT EXISTS queue_items (
               job_id TEXT NOT NULL,
               item_id TEXT NOT NULL,
               state TEXT NOT NULL,
               json TEXT NOT NULL,
               PRIMARY KEY (job_id, item_id)
             );
             CREATE TABLE IF NOT EXISTS suspicions (
               id TEXT PRIMARY KEY,
               file TEXT NOT NULL,
               quote_norm TEXT NOT NULL,
               hint TEXT NOT NULL DEFAULT '',
               status TEXT NOT NULL,
               first_job TEXT NOT NULL DEFAULT '',
               last_job TEXT NOT NULL DEFAULT '',
               activations INTEGER NOT NULL DEFAULT 0,
               json TEXT NOT NULL
             );";

/// dismissed 状态枚举（应用层校验；sqlite 存 TEXT）。
/// withdrawn=改判属实（替代删卡）、stale=gc 判定锚失效——两者读取时视同无卡。
pub const STATUSES: &[&str] = &["dismissed", "re-upheld", "human-confirmed", "withdrawn", "stale"];
/// 活跃状态（参与豁免/预筛/注入）。
const ACTIVE_STATUSES: &str = "('dismissed','re-upheld','human-confirmed')";

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DismissCard {
    pub id: String,
    pub status: String,
    pub title: String,
    pub file: String,
    pub quote: String,
    pub severity: String,
    pub reason: String,
    pub round: String,
    pub updated_at: String,
}

/// M13 销案卡（决议 3）：ship 裁决「属实→已修复」的判定记录。与 DismissCard 对称
/// （快照四件套）但**无 status 状态机**——已修复≠误报，独立表、不进豁免/预筛；
/// 增量轮据此排除携带 + 生成 claimed-resolved 顺带验证清单。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResolveCard {
    pub id: String,
    pub title: String,
    pub file: String,
    pub quote: String,
    pub severity: String,
    pub reason: String,
    pub round: String,
    pub updated_at: String,
}

/// 台账四表在 v4 下的期望列（顺序即 `SCHEMA_SQL` 里的顺序）。旧库要能**带着台账迁移**，
/// 这四张表必须与 v4 逐列一致；差一列就说明不是「同构旧库」，退回拒开路径（宁可让人先导出，也不静默丢卡）。
const LEDGER_COLUMNS: &[(&str, &[&str])] = &[
    ("dismissed", &["id", "status", "title", "file", "quote", "severity", "reason", "round", "updated_at"]),
    ("dismissed_history", &["seq", "id", "status", "round", "reason", "at"]),
    ("resolved", &["id", "title", "file", "quote", "severity", "reason", "round", "updated_at"]),
    ("resolved_history", &["seq", "id", "round", "reason", "at"]),
];

/// 本次打开对库做什么（`plan()` 的产物）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupPlan {
    /// 空文件 / 无表：建表。
    Fresh,
    /// 已是 v4（或 `.dump` 恢复丢版本号）：只补缺表与版本号，不碰数据。
    TopUp,
    /// 旧形状但零数据：丢表重建。
    RebuildEmpty,
    /// 旧形状且有数据：一次性迁移。`keep_ledger` = 台账表在场且同构（在场就必同构，否则 `plan` 已拒开）；
    /// 不在场说明数据都在评审史里，无账可带。
    Migrate { keep_ledger: bool },
}

/// 旧库一次性迁移的结果（2026-09-28 裁：带走台账、丢评审史）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LegacyMigration {
    /// 保住的 dismissed 卡数。
    pub dismissed: i64,
    /// 保住的 resolved 卡数。
    pub resolved: i64,
    /// 丢弃的评审史表（实际存在的那些）。
    pub dropped: Vec<String>,
    /// 迁移前的整库快照。
    pub backup: std::path::PathBuf,
}

/// 疑点台账行（`suspicions` 表，M21c 批 A）：跨轮累积的**休眠疑点**——本轮因预算截断没挖成的那些。
/// 身份 = 文件 + quote 归一（与 finding id 同一套 `normalize_quote`，行号无关、免疫 reformat），
/// 所以同一处疑点跨轮只有一行。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SuspicionRow {
    pub id: String,
    pub file: String,
    pub quote_norm: String,
    pub hint: String,
    /// `dormant` = 休眠（本批只产这一种）；`activated` / `promoted` 留给批 C 的增量激活。
    pub status: String,
    pub first_job: String,
    pub last_job: String,
    /// 被 delta 激活过几次（批 C 的活锁守卫据此封顶）。本批恒 0。
    pub activations: i64,
    /// 明细 JSON：行区间、confidence、category、来源、落账原因、所属队列项。
    pub json: String,
}

pub struct ProjDb {
    conn: Connection,
    legacy_migration: Option<LegacyMigration>,
}

/// M21 分支期的库名。切名（M21c 批 0）前，分支产物 `redteam-next` 用的是这个独立文件名，
/// 于是每个在那期间跑过它的项目都留下一个 `proj-next.db`。切名后二进制只认 `proj.db`，
/// 那些库会变成孤儿——作业历史、resume、`result` 全都取不回。开项目库时顺手改名即可无缝继承（两者都是 v4）。
const BRANCH_ERA_DB_FILE: &str = "proj-next.db";

/// 项目库路径的**单一入口**（M21c 批 A）：`<root>/.redteam/proj.db`，解析时顺手接管 M21 分支期的库名。
///
/// 为什么要收成一个函数：接管一度只挂在 `ProjDb::open()` 上，而真实调用点（台账加载、落账、披露）
/// 走的都是 `open_at(显式路径)` —— 单测各自绿、接起来不通，PR181 上实跑才暴露（库一行没接管、台账一行没落）。
/// **只解析不创建**：调用方按自己的语义决定「不存在就跳过」还是「建库」。
pub fn project_db_path(root: &Path) -> std::path::PathBuf {
    let dir = root.join(crate::project::PROJECT_DIR);
    ProjDb::adopt_branch_era_db(&dir);
    dir.join(DB_FILE)
}

impl ProjDb {
    /// 打开项目库（`<root>/.redteam/proj.db`）；目录/库/表缺失即自动创建（幂等）。
    ///
    /// 开库前先做一次 M21 分支期库名的接管（见 [`BRANCH_ERA_DB_FILE`]）：只在**目标不存在**时改名，
    /// 绝不覆盖已有库；`-wal` / `-shm` 一并搬（留着会让 sqlite 认错日志）。失败只 warning，不挡开库。
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join(crate::project::PROJECT_DIR);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Self::open_at(&project_db_path(root))
    }

    /// M21 分支期库（`proj-next.db`）→ 正式库名。目标已存在就什么都不做（两个库并存时不猜哪个是对的）。
    ///
    /// **顺序是 `-wal` / `-shm` 先、主库最后**（收口评审 #2）：反过来的话，主库改完名而 `-wal` 搬失败
    /// （只读目录 / 磁盘满 / ACL）或此刻崩溃，sqlite 打开新名时找不到配套 WAL，只能读到最后一次 checkpoint
    /// ——上次被 kill 那轮未落盘的 cell 记录连同 resume 资格一起**静默消失**，而函数还会打印「接管成功」。
    /// 按现在的顺序，附属文件搬不动就整体放弃（已搬的搬回去），库停在原名 = 「未接管」，可重试、无损失。
    fn adopt_branch_era_db(dir: &Path) {
        let (old, new) = (dir.join(BRANCH_ERA_DB_FILE), dir.join(DB_FILE));
        if new.exists() || !old.exists() {
            return;
        }
        let side = |base: &str, suffix: &str| dir.join(format!("{base}{suffix}"));
        let mut moved: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
        for suffix in ["-wal", "-shm"] {
            let (o, n) = (side(BRANCH_ERA_DB_FILE, suffix), side(DB_FILE, suffix));
            if !o.exists() {
                continue;
            }
            if let Err(e) = std::fs::rename(&o, &n) {
                eprintln!("warning: M21 分支期库的 {suffix} 搬不动（{e}）：整体放弃接管以免丢 WAL 里未落盘的记录，可重试");
                for (from, to) in moved.iter().rev() {
                    let _ = std::fs::rename(to, from); // 搬回去，回到「未接管」
                }
                return;
            }
            moved.push((o, n));
        }
        if let Err(e) = std::fs::rename(&old, &new) {
            // 主库搬不动 → 附属文件也搬回去，别留下「有 wal 没库」的半途状态
            for (from, to) in moved.iter().rev() {
                let _ = std::fs::rename(to, from);
            }
            // 并发下另一个进程可能刚好接管完（此时 new 已在）——那不是错误，别叫用户去做多余的手工操作
            if new.exists() {
                return;
            }
            eprintln!("warning: M21 分支期库 {} 改名失败（{e}）：作业历史暂取不回，手工改名即可", old.display());
            return;
        }
        eprintln!("note: 已接管 M21 分支期库 {} → {}（作业历史与台账随之继承）", old.display(), new.display());
    }

    /// 按显式路径打开（`--dismissed <db路径>` 覆盖场景）。
    /// 只读快路径：已是 v4 且全表在场 → 不写任何字节（只读文件 / 只读挂载 / 他人检出可开，与 dev 基线一致）；
    /// 否则在一个 `BEGIN IMMEDIATE` 事务里做版本守卫 + 建表 + 写版本号（并发 open 不会互相 DROP、不会撞 no such table）。
    pub fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open db {}", path.display()))?;
        // 跨进程写并发（后台 job 写 state/results × 宿主 dismiss add）下 WAL 写锁冲突
        // 默认立即 SQLITE_BUSY——busy_timeout 让写者排队而非报错。
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let wrote = Self::needs_setup(&conn)?;
        let mut legacy_migration = None;
        if wrote {
            // 备份必须在事务**之前**（`VACUUM INTO` 不能在事务内跑），但**只为真要迁移的库做**——
            // 收口评审 #1：原先用一个比守卫宽松的判据决定备份，于是「陌生表 / 台账不同构」被拒开的库
            // 也会先被复制一份（误传给 `--dismissed` 的业务库 = 未经同意的全量复制 + 无上限磁盘占用）。
            // 现在事务外与事务内用**同一个** `plan()`，它含全部守卫。
            let backup = match Self::plan(&conn, path)? {
                SetupPlan::Migrate { .. } => Some(Self::backup_legacy(&conn, path)?),
                _ => None,
            };
            conn.execute_batch("BEGIN IMMEDIATE")?;
            match Self::setup_schema(&conn, path, backup) {
                Ok(m) => legacy_migration = m,
                Err(e) => {
                    let _ = conn.execute_batch("ROLLBACK");
                    return Err(e);
                }
            }
            conn.execute_batch("COMMIT")?;
        }
        if let Some(m) = &legacy_migration {
            eprintln!(
                "warning: `{p}` 是旧 schema 库，已一次性迁移：保住 dismissed {d} 张 / resolved {r} 张；\
                 丢弃评审史表 {dropped:?}（迁移前整库快照在 `{b}`）",
                p = path.display(), d = m.dismissed, r = m.resolved, dropped = m.dropped, b = m.backup.display()
            );
        }
        // WAL 在守卫之后：被拒开的旧库 / 陌生库不被改写 journal 模式。只读打开（只读文件 / 只读挂载）的 delete 模式 v4 库
        // 切不了 WAL：快路径降级 warning、继续只读使用，不拒开（修补批 X-1）；刚建 / 补过表的库切失败仍是错误。
        if let Err(e) = conn.pragma_update(None, "journal_mode", "WAL") {
            if wrote {
                return Err(e.into());
            }
            eprintln!("warning: {} 只读打开，无法切换 journal_mode=WAL（{e}），继续以只读方式使用", path.display());
        }
        Self::normalize_legacy_activated(&conn, path);
        Ok(Self { conn, legacy_migration })
    }

    /// 三态遗留行的一次性规范化（发布门，2026-10-04 对账落地）。
    ///
    /// 批 C 把台账从 `dormant → activated → promoted` 收成两态时**没有规范化已有数据**。
    /// 旧二进制写下的 `status='activated'` 行在新语义下谁也不认：激活判据只看 `dormant`、
    /// 披露只看 `dormant`，于是它们**永久卡住**——既不会被重新挖，也不会出现在报告里。
    ///
    /// 为什么不能放进 `setup_schema`：那些库**已经是 v4 且表齐全**，`needs_setup` 为假，
    /// 整条迁移路径根本不跑。所以挂在 open 上，幂等、无匹配行时近乎零成本。
    ///
    /// 失败只告警不拒开：这是数据修复，不是打开库的前提条件。
    fn normalize_legacy_activated(conn: &Connection, path: &Path) {
        match conn.execute("UPDATE suspicions SET status='dormant' WHERE status='activated'", []) {
            Ok(0) => {}
            Ok(n) => eprintln!(
                "notice: {} 里有 {n} 条 `status='activated'` 的三态遗留行（批 C 两态化之前的二进制写的），\
                 已复位为 dormant——否则它们既不被激活也不被披露，永久卡住",
                path.display()
            ),
            // 表不在（旧 schema 库走 open_read_only 这类路径）不是错
            Err(e) => {
                if !matches!(&e, rusqlite::Error::SqliteFailure(_, Some(m)) if m.contains("no such table")) {
                    eprintln!("warning: {} 三态遗留行规范化失败（{e}）——不影响本次使用", path.display());
                }
            }
        }
    }

    /// **只读**打开（`--dismissed <db路径>` 这类「只想读一份台账」的入口，收口评审 #2）：
    /// 不建表、不迁移、不切 WAL、不创建文件。旧 schema 库照样能读——台账四表与 v4 逐列相同，
    /// `active_cards` / `active_ids` 的 SQL 只碰 `dismissed`。
    ///
    /// 为什么必须有这条路：`open_at` 现在带一次性迁移，而「拿 dev 库做对拍基线」正是回退路径依赖的场景——
    /// 用读的姿势把基线就地迁成 v4（评审史清空、dev 二进制再也写不了它）会把回退路径本身毁掉。
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("open db read-only {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { conn, legacy_migration: None })
    }

    /// 本次打开是否做了旧库一次性迁移（做了则含保住 / 丢弃的账与备份路径）。
    pub fn legacy_migration(&self) -> Option<&LegacyMigration> {
        self.legacy_migration.as_ref()
    }

    fn user_version(conn: &Connection) -> Result<i64> {
        Ok(conn.pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    /// 用户表名（排除 sqlite 内部表），字母序。
    fn table_names(conn: &Connection) -> Result<Vec<String>> {
        let mut stmt = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let cols = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for c in cols {
            if c? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// 表的列名（顺序即 `PRAGMA table_info` 顺序）。
    fn columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))?;
        let cols = stmt.query_map([], |r| r.get::<_, String>(1))?;
        Ok(cols.collect::<std::result::Result<_, _>>()?)
    }

    /// 旧库是否「同构」：在场的台账表都与 v4 逐列一致（缺席的由建表语句补）。不同构 → 不迁移、退回拒开。
    fn ledger_tables_match_v4(conn: &Connection, have: &[String]) -> Result<bool> {
        for (t, want) in LEDGER_COLUMNS {
            if !have.iter().any(|h| h == t) {
                continue;
            }
            if Self::columns(conn, t)? != *want {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// 旧库里所有表的总行数（判「空库」用）。
    fn total_rows(conn: &Connection, have: &[String]) -> Result<i64> {
        let mut rows = 0i64;
        for t in have {
            rows += conn.query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get::<_, i64>(0))?;
        }
        Ok(rows)
    }

    /// 本次打开该怎么处置这个库——**所有守卫集中在这里**，事务外（决定要不要备份）与事务内（权威执行）
    /// 用的是同一套判据（收口评审 #1 / #5：两处口径不一致会让被拒开的库也留下一份全量副本）。
    fn plan(conn: &Connection, path: &Path) -> Result<SetupPlan> {
        let version = Self::user_version(conn)?;
        let have = Self::table_names(conn)?;
        anyhow::ensure!(
            version <= USER_VERSION,
            "`{p}` 是 schema v{version} 的库，高于本二进制支持的 v{USER_VERSION}：请用更新的 redteam 打开，本版本不降级",
            p = path.display()
        );
        if have.is_empty() {
            return Ok(SetupPlan::Fresh);
        }
        // 陌生表在任何版本号下都拒开（含 user_version 恰为 4 的业务库——修补批 X-1）
        let foreign: Vec<&str> = have.iter().map(String::as_str).filter(|t| !TABLES.contains(t)).collect();
        anyhow::ensure!(
            foreign.is_empty(),
            "`{p}` 不是 redteam 项目库（含陌生表 {foreign:?}），拒绝改写",
            p = path.display()
        );
        if version == USER_VERSION || (have.iter().any(|t| t == "cells") && Self::has_column(conn, "cells", "role")?) {
            // v4（或 `.dump` 恢复丢了版本号的 v4 库）：只补缺表 / 版本号
            return Ok(SetupPlan::TopUp);
        }
        if Self::total_rows(conn, &have)? == 0 {
            return Ok(SetupPlan::RebuildEmpty); // 零数据零风险（如 dev 二进制 init 出的空库）
        }
        // 有数据的旧库：台账在场就必须与 v4 同构，否则拒开（宁可让人先导出，也不静默丢卡）
        let ledger_present = LEDGER_COLUMNS.iter().any(|(t, _)| have.iter().any(|h| h == t));
        if ledger_present {
            anyhow::ensure!(
                Self::ledger_tables_match_v4(conn, &have)?,
                "`{p}` 是 schema v{version}（< v{USER_VERSION}）的旧库，且**台账表与 v{USER_VERSION} 不同构**\
                 （期望 {want:?}）：不改写它，也不带账迁移——宁可让你先导出，也不静默丢卡。\
                 用 `python3 eval/export_ledger.py --db '{p}'` 导出 dismissed / resolved / upheld 固定集\
                 （jobs / cells / results 评审史不导出）",
                p = path.display(),
                want = LEDGER_COLUMNS.iter().map(|(t, _)| *t).collect::<Vec<_>>()
            );
        }
        Ok(SetupPlan::Migrate { keep_ledger: ledger_present })
    }

    /// 迁移前的整库一致快照：`<stem>-legacy-<YYYYMMDD>.db`（`VACUUM INTO`，含 WAL 里未落盘的部分）。
    ///
    /// 两条健壮性（收口评审 #4）：
    /// - **先写 `.part` 再 rename**——`VACUUM INTO` 跑到一半进程被杀会留下截断文件，不 rename 就不会被当成快照；
    /// - **复用前校验**——同名文件存在不等于「有备份」（它可能是上次的残片，或根本不是库）：
    ///   过 `quick_check` 且能读出表名才复用，否则另起名 `-2`、`-3`…，绝不覆盖别人的文件。
    fn backup_legacy(conn: &Connection, path: &Path) -> Result<std::path::PathBuf> {
        let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "proj".into());
        // 日期取自 sqlite（仓内一律如此，不引 chrono）
        let day: String = conn.query_row("SELECT strftime('%Y%m%d','now')", [], |r| r.get(0))?;
        let mut dest = path.with_file_name(format!("{stem}-legacy-{day}.db"));
        for n in 2..100 {
            if !dest.exists() {
                break;
            }
            if Self::is_usable_snapshot(&dest) {
                return Ok(dest);
            }
            dest = path.with_file_name(format!("{stem}-legacy-{day}-{n}.db"));
        }
        anyhow::ensure!(!dest.exists(), "备份旧库：`{}` 一带的名字都被占且都不是可用快照", dest.display());
        let part = dest.with_extension("db.part");
        let _ = std::fs::remove_file(&part); // 上次的残片
        // 路径进 SQL 用参数绑定，避免目录名里的单引号把语句劈开
        conn.execute("VACUUM INTO ?1", [part.to_string_lossy().as_ref()])
            .with_context(|| format!("备份旧库到 {}", part.display()))?;
        std::fs::rename(&part, &dest).with_context(|| format!("快照改名 {} → {}", part.display(), dest.display()))?;
        Ok(dest)
    }

    /// 这个文件是不是一份能用的快照：打得开、`quick_check` 过、至少有一张表。
    fn is_usable_snapshot(p: &Path) -> bool {
        let Ok(c) = Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
            return false;
        };
        let ok = c.query_row("PRAGMA quick_check", [], |r| r.get::<_, String>(0)).map(|v| v == "ok").unwrap_or(false);
        ok && Self::table_names(&c).map(|t| !t.is_empty()).unwrap_or(false)
    }

    /// 是否需要写库：版本不是 v4，或 v4 表不全。
    fn needs_setup(conn: &Connection) -> Result<bool> {
        if Self::user_version(conn)? != USER_VERSION {
            return Ok(true);
        }
        let have = Self::table_names(conn)?;
        Ok(!TABLES.iter().all(|t| have.iter().any(|h| h == t)))
    }

    /// 建表 + 版本号 + 旧库一次性迁移。**在事务内调用**：这里重跑 `plan()` 取权威结论
    /// （事务外那次只用来决定要不要备份；两次之间库可能被别的进程改过）。
    /// 迁移 = 保住台账四表、**丢弃其余所有在场表**、缺表由 `SCHEMA_SQL` 补、版本号置 v4。
    /// 为什么丢「其余所有」而不只是 jobs/cells/results（收口评审 #3）：`queue_items` / `suspicions`
    /// 也在 `TABLES` 里（不算陌生表），旧库若带着它们的半成品形状，`CREATE TABLE IF NOT EXISTS` 补不上缺的列，
    /// 库却被盖上 v4 戳记——此后 `queue_item_put` 永久失败，而守卫再也不触发，进入不可自愈的坏态。
    fn setup_schema(conn: &Connection, path: &Path, backup: Option<std::path::PathBuf>) -> Result<Option<LegacyMigration>> {
        let version = Self::user_version(conn)?;
        let have = Self::table_names(conn)?;
        let plan = Self::plan(conn, path)?;
        let mut migration = None;
        match plan {
            SetupPlan::Fresh | SetupPlan::TopUp => {}
            SetupPlan::RebuildEmpty => {
                for t in &have {
                    conn.execute_batch(&format!("DROP TABLE IF EXISTS \"{t}\""))?;
                }
            }
            SetupPlan::Migrate { keep_ledger } => {
                // 备份是迁移的**硬前置**（收口评审 #5）：事务外判「不迁移」而事务内判「要迁移」的竞态下
                // （别的写者刚插入第一行），原实现会 `unwrap_or_default()` 吞掉缺席的备份、无快照就 DROP 评审史。
                let backup = backup.ok_or_else(|| {
                    anyhow::anyhow!(
                        "`{p}` 需要一次性迁移，但本次打开没有拿到迁移前快照（多半是另一个进程刚写入了这个旧库）：\
                         不做无备份的迁移，请重跑一次",
                        p = path.display()
                    )
                })?;
                let keep: &[&str] = if keep_ledger { &["dismissed", "dismissed_history", "resolved", "resolved_history"] } else { &[] };
                let count = |t: &str| -> Result<i64> {
                    if keep.contains(&t) && have.iter().any(|h| h == t) {
                        Ok(conn.query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))?)
                    } else {
                        Ok(0)
                    }
                };
                let (dismissed, resolved) = (count("dismissed")?, count("resolved")?);
                let mut dropped = Vec::new();
                for t in &have {
                    if keep.contains(&t.as_str()) {
                        continue;
                    }
                    conn.execute_batch(&format!("DROP TABLE IF EXISTS \"{t}\""))?;
                    dropped.push(t.clone());
                }
                migration = Some(LegacyMigration { dismissed, resolved, dropped, backup });
            }
        }
        conn.execute_batch(SCHEMA_SQL)?;
        if version != USER_VERSION {
            conn.pragma_update(None, "user_version", USER_VERSION)?;
        }
        Ok(migration)
    }

    // ---- dismissed 台账 ----

    /// 建卡/整卡更新：主表 upsert + history 追加（同一事务）。
    pub fn dismiss_upsert(&mut self, card: &DismissCard) -> Result<()> {
        anyhow::ensure!(STATUSES.contains(&card.status.as_str()), "unknown status '{}'", card.status);
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO dismissed (id,status,title,file,quote,severity,reason,round,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,strftime('%Y-%m-%d','now'))
             ON CONFLICT(id) DO UPDATE SET status=?2,title=?3,file=?4,quote=?5,severity=?6,
               reason=?7,round=?8,updated_at=strftime('%Y-%m-%d','now')",
            params![card.id, card.status, card.title, card.file, card.quote, card.severity, card.reason, card.round],
        )?;
        tx.execute(
            "INSERT INTO dismissed_history (id,status,round,reason) VALUES (?1,?2,?3,?4)",
            params![card.id, card.status, card.round, card.reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// 状态迁移：改 status/round（reason 未给则沿用旧值）+ history 追加。
    /// 返回迁移后的卡；id 不存在报错。不做状态机硬约束（协议层职责），
    /// 但 human-confirmed（人裁终局）被改时返回的 warning 由调用方展示。
    pub fn dismiss_set_status(
        &mut self,
        id: &str,
        status: &str,
        round: &str,
        reason: Option<&str>,
    ) -> Result<(DismissCard, Option<String>)> {
        anyhow::ensure!(STATUSES.contains(&status), "unknown status '{status}'");
        let old = self.dismiss_get(id)?.with_context(|| format!("no card with id {id}"))?;
        let warn = (old.status == "human-confirmed" && status != "human-confirmed")
            .then(|| format!("card {id} was human-confirmed (final by protocol); overriding to {status}"));
        let reason = reason.unwrap_or(&old.reason);
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE dismissed SET status=?2, round=?3, reason=?4, updated_at=strftime('%Y-%m-%d','now') WHERE id=?1",
            params![id, status, round, reason],
        )?;
        tx.execute(
            "INSERT INTO dismissed_history (id,status,round,reason) VALUES (?1,?2,?3,?4)",
            params![id, status, round, reason],
        )?;
        tx.commit()?;
        let card = self.dismiss_get(id)?.expect("just updated");
        Ok((card, warn))
    }

    pub fn dismiss_get(&self, id: &str) -> Result<Option<DismissCard>> {
        self.conn
            .query_row(
                "SELECT id,status,title,file,quote,severity,reason,round,updated_at FROM dismissed WHERE id=?1",
                params![id],
                row_to_card,
            )
            .optional()
            .map_err(Into::into)
    }

    /// 列卡（status 过滤可选），按 updated_at 降序、id 升序稳定排序。
    pub fn dismiss_list(&self, status: Option<&str>) -> Result<Vec<DismissCard>> {
        let (sql, filter) = match status {
            Some(s) => ("SELECT id,status,title,file,quote,severity,reason,round,updated_at FROM dismissed
                         WHERE status=?1 ORDER BY updated_at DESC, id", Some(s)),
            None => ("SELECT id,status,title,file,quote,severity,reason,round,updated_at FROM dismissed
                      ORDER BY updated_at DESC, id", None),
        };
        let mut stmt = self.conn.prepare(sql)?;
        let rows = match filter {
            Some(s) => stmt.query_map(params![s], row_to_card)?,
            None => stmt.query_map([], row_to_card)?,
        };
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 每张卡的历史事件（升序）。
    pub fn dismiss_history(&self, id: &str) -> Result<Vec<(String, String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT status,round,reason,at FROM dismissed_history WHERE id=?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 豁免 id 集（活跃状态卡；withdrawn/stale 视同无卡）。
    /// 这个库是不是已经是 v4（可以直接写）。**只读探测、零副作用**：库不在 / 打不开 / 版本不符都返回 false。
    ///
    /// 给「写之前先看一眼」的调用方用（收口评审 #3）：`open_at` 带一次性迁移，对一条自认为只是
    /// 「顺手记一笔」的路径来说，触发整库快照 + 丢弃评审史的后果太重——尤其当它是否触发还取决于
    /// 「本轮恰好有没有截断项」这种与库无关的偶然条件。
    pub fn is_current_version(path: &Path) -> bool {
        let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else {
            return false;
        };
        Self::user_version(&conn).map(|v| v == USER_VERSION).unwrap_or(false)
    }

    /// 疑点落账（M21c 批 A，design §8.2）：**幂等 upsert**——同一处疑点跨轮只有一行。
    /// 新行写全量、`activations = 0`；已有行只更新 `last_job` / `hint` / `json`，
    /// **不动** `first_job`（第一次见到它的那轮）、`activations` 与 `status`（激活与晋升是批 C 的事，不能被落账覆盖回 dormant）。
    pub fn suspicion_upsert(&self, row: &SuspicionRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO suspicions (id,file,quote_norm,hint,status,first_job,last_job,activations,json)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
             ON CONFLICT(id) DO UPDATE SET last_job=excluded.last_job, hint=excluded.hint, json=excluded.json",
            rusqlite::params![row.id, row.file, row.quote_norm, row.hint, row.status,
                              row.first_job, row.last_job, row.activations, row.json],
        )?;
        Ok(())
    }

    /// 按文件取休眠疑点（A-2 披露 / 批 C 增量激活的入口）。`files` 空 → 空结果（不退化成全表）。
    pub fn suspicions_for_files(&self, files: &[String]) -> Result<Vec<SuspicionRow>> {
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let ph = (1..=files.len()).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id,file,quote_norm,hint,status,first_job,last_job,activations,json
             FROM suspicions WHERE file IN ({ph}) ORDER BY file, id"
        ))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(files.iter()), |r| {
            Ok(SuspicionRow {
                id: r.get(0)?, file: r.get(1)?, quote_norm: r.get(2)?, hint: r.get(3)?, status: r.get(4)?,
                first_job: r.get(5)?, last_job: r.get(6)?, activations: r.get(7)?, json: r.get(8)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    }

    /// 激活回写（M21c 批 C）：计数 +1、`last_job` 记本轮，**不动 status**。
    ///
    /// 幂等靠 `last_job <> ?2`：同一轮里重复调用（或与别的写者撞上）第二次写不进去，计数不会多加；
    /// 而**换一轮**就能再加一次——两段活锁门（3 / 5）正是靠这个才可达。
    ///
    /// 这里原先写 `status='activated'` 且条件为 `status='dormant'`，于是一行一生只能加一次、
    /// 门成了不可达常量，守卫整个是死代码（批 C dogfood 抓到）。状态机为什么收成两态见
    /// `ledger::STATUS_DORMANT` 的注释。
    pub fn suspicion_activate(&self, id: &str, job: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE suspicions SET activations=activations+1, last_job=?2
             WHERE id=?1 AND status='dormant' AND last_job<>?2",
            rusqlite::params![id, job],
        )?;
        Ok(())
    }

    /// 晋升（M21c 批 C）：这条台账疑点在本轮被挖出了 finding → 退出激活池。
    ///
    /// 没有它，一条已经产出 finding 的疑点会在后续每一轮被反复激活重挖，`activations` 一路涨到停车门，
    /// 然后报告对人说「机制挖不动它」——而它明明挖出来了。语义正好反了。
    ///
    /// 无状态前提（不写 `WHERE status='dormant'`）：两态下 `promoted` 行根本进不了激活池、也就进不了
    /// 晋升的候选，那个条件没有任何可观测效果——加一个没人盯得住的分支不如不加。
    /// 将来真有第三个状态（如 `parked`）时再按那时的语义补。
    pub fn suspicion_promote(&self, id: &str) -> Result<()> {
        self.conn.execute("UPDATE suspicions SET status='promoted' WHERE id=?1", rusqlite::params![id])?;
        Ok(())
    }

    /// 上一轮的裁决（M21c 批 C 的激活闸门，design §8.3「上轮 verdict = Incomplete 时台账疑点不可信」）。
    ///
    /// 取 `results` 里最近一条——激活发生在本轮写结果**之前**，所以最近一条必定是上一轮。
    ///
    /// 三种结果**必须分开**（dogfood 抓到原先把后两种都塞进 `None`）：
    /// - `Ok(None)` = 确实没有往轮（首次在本仓跑）→ 无闸门，台账本来也是空的；
    /// - `Ok(Some(v))` = 读到了裁决；
    /// - `Err(_)` = 有往轮但读不出（JSON 坏了 / 没有 `verdict` 键 / 查询失败）→ 调用方按**保守**处理不激活。
    ///   混成 `None` 的后果是闸门静默失效：恰恰在往轮记录可疑的时候，我们照着它的台账去激活。
    pub fn last_verdict(&self) -> Result<Option<String>> {
        // `json <> ''` 跳过 md-first 写入留下的半行（dogfood medium，agreement 2）：
        // `result_put_md` 先建行时 json 列是空串，被它选中会解析失败 → `Err` → 闸门保守关闭，
        // 于是一次正常的 md 落盘就能把下一轮的激活整个掐掉。
        let mut stmt = self
            .conn
            .prepare("SELECT json FROM results WHERE json <> '' ORDER BY created_at DESC LIMIT 1")?;
        let mut rows = stmt.query([])?;
        let Some(r) = rows.next()? else { return Ok(None) };
        let js: String = r.get(0)?;
        let v: serde_json::Value = serde_json::from_str(&js).context("上一轮结果 JSON 解析失败")?;
        v.get("verdict")
            .and_then(|x| x.as_str())
            .map(|x| Some(x.to_string()))
            .context("上一轮结果里没有 verdict 键")
    }

    pub fn active_ids(&self) -> Result<HashSet<String>> {
        let mut stmt = self
            .conn
            .prepare(&format!("SELECT id FROM dismissed WHERE status IN {ACTIVE_STATUSES}"))?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<HashSet<_>, _>>()?)
    }

    /// 活跃卡全量（机械预筛与判例注入的数据源）。
    pub fn active_cards(&self) -> Result<Vec<DismissCard>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id,status,title,file,quote,severity,reason,round,updated_at FROM dismissed
             WHERE status IN {ACTIVE_STATUSES} ORDER BY updated_at DESC, id"
        ))?;
        let rows = stmt.query_map([], row_to_card)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---- resolve 销案台账（M13 决议 3，dismiss 表范式对称）----

    /// 销案/再销案：主表 upsert + history 追加（同一事务）。
    /// 再销案未给新 reason（空串）→ 沿用旧卡理由（对称 dismiss_set_status 的沿用语义），
    /// 防二次销案静默清空审计理由；history 记当次生效值。
    pub fn resolve_upsert(&mut self, card: &ResolveCard) -> Result<()> {
        let reason = if card.reason.trim().is_empty() {
            self.resolve_get(&card.id)?.map(|old| old.reason).unwrap_or_default()
        } else {
            card.reason.clone()
        };
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO resolved (id,title,file,quote,severity,reason,round,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,strftime('%Y-%m-%d','now'))
             ON CONFLICT(id) DO UPDATE SET title=?2,file=?3,quote=?4,severity=?5,
               reason=?6,round=?7,updated_at=strftime('%Y-%m-%d','now')",
            params![card.id, card.title, card.file, card.quote, card.severity, reason, card.round],
        )?;
        tx.execute(
            "INSERT INTO resolved_history (id,round,reason) VALUES (?1,?2,?3)",
            params![card.id, card.round, reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn resolve_get(&self, id: &str) -> Result<Option<ResolveCard>> {
        self.conn
            .query_row(
                "SELECT id,title,file,quote,severity,reason,round,updated_at FROM resolved WHERE id=?1",
                params![id],
                row_to_resolve,
            )
            .optional()
            .map_err(Into::into)
    }

    /// 全量销案卡（增量轮携带排除 + claimed-resolved 清单的数据源）。
    pub fn resolved_cards(&self) -> Result<Vec<ResolveCard>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,title,file,quote,severity,reason,round,updated_at FROM resolved
             ORDER BY updated_at DESC, id",
        )?;
        let rows = stmt.query_map([], row_to_resolve)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 销案卡每条 history 的轮/理由对（升序）。
    pub fn resolve_history(&self, id: &str) -> Result<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT round, reason FROM resolved_history WHERE id=?1 ORDER BY seq",
        )?;
        let rows = stmt.query_map(params![id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    // ---- jobs / cells / results ----

    pub fn job_put_state(&self, job_id: &str, state_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO jobs (job_id, state_json) VALUES (?1,?2)
             ON CONFLICT(job_id) DO UPDATE SET state_json=?2",
            params![job_id, state_json],
        )?;
        Ok(())
    }

    pub fn job_get_state(&self, job_id: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT state_json FROM jobs WHERE job_id=?1", params![job_id], |r| r.get(0))
            .optional()
            .map_err(Into::into)
    }

    pub fn job_put_args(&self, job_id: &str, args_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO jobs (job_id, state_json, args_json) VALUES (?1,'',?2)
             ON CONFLICT(job_id) DO UPDATE SET args_json=?2",
            params![job_id, args_json],
        )?;
        Ok(())
    }

    pub fn job_get_args(&self, job_id: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT args_json FROM jobs WHERE job_id=?1", params![job_id], |r| r.get(0))
            .optional()
            .map(|v: Option<String>| v.filter(|s| !s.is_empty()))
            .map_err(Into::into)
    }

    pub fn job_put_manifest(&self, job_id: &str, manifest_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO jobs (job_id, state_json, manifest_json) VALUES (?1,'',?2)
             ON CONFLICT(job_id) DO UPDATE SET manifest_json=?2",
            params![job_id, manifest_json],
        )?;
        Ok(())
    }

    pub fn job_get_manifest(&self, job_id: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT manifest_json FROM jobs WHERE job_id=?1", params![job_id], |r| r.get(0))
            .optional()
            .map(|v: Option<String>| v.filter(|s| !s.is_empty()))
            .map_err(Into::into)
    }

    /// 本项目全部 job 的 state_json（status 无 id 时列表用；state 为空的行跳过）。
    pub fn job_all_states(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT state_json FROM jobs WHERE state_json != '' ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// cell checkpoint：同 (job,cell) 覆盖（重试成功覆盖失败记录，读取端零过滤合并）。
    /// `role` 列冗余存 `CellRecord.role`（v4）：M21b resume 按角色差集查表，不必反序列化整列 JSON。
    pub fn cell_put(&self, job_id: &str, cell: &str, role: &str, record_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cells (job_id, cell, role, record_json) VALUES (?1,?2,?3,?4)
             ON CONFLICT(job_id, cell) DO UPDATE SET role=?3, record_json=?4",
            params![job_id, cell, role, record_json],
        )?;
        Ok(())
    }

    /// 测试/诊断：某 cell 的 role 列值（v4 列与记录一致性断言用）。
    pub fn cell_role(&self, job_id: &str, cell: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT role FROM cells WHERE job_id=?1 AND cell=?2", params![job_id, cell], |r| r.get(0))
            .optional()
            .map_err(Into::into)
    }

    pub fn cell_all(&self, job_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT record_json FROM cells WHERE job_id=?1 ORDER BY cell")?;
        let rows = stmt.query_map(params![job_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 深挖队列快照（C-5）：INSERT OR REPLACE。写失败由调用方 warning，不败 job。
    pub fn queue_item_put(&self, job_id: &str, item_id: &str, state: &str, json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO queue_items (job_id, item_id, state, json) VALUES (?1,?2,?3,?4)
             ON CONFLICT(job_id, item_id) DO UPDATE SET state=?3, json=?4",
            params![job_id, item_id, state, json],
        )?;
        Ok(())
    }

    /// 某作业全部队列快照行（item_id, state, json）。零行 = 旧作业 / 无深挖。
    pub fn queue_items_for_job(&self, job_id: &str) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT item_id, state, json FROM queue_items WHERE job_id=?1 ORDER BY item_id",
        )?;
        let rows = stmt.query_map(params![job_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 评审史 json 列单独原子写（M17-c C-Q4：消除「读另一列再写两列」的双事务窗口——
    /// 旧 result_put 整行覆盖已删，json/md 各自单列 upsert，互不读取对方现值）。
    pub fn result_put_json(&self, job_id: &str, json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO results (job_id, json) VALUES (?1,?2)
             ON CONFLICT(job_id) DO UPDATE SET json=excluded.json",
            params![job_id, json],
        )?;
        Ok(())
    }

    /// 评审史 md 列单独原子写（同上，对称）。首建行需显式给 json=''——
    /// json 列 NOT NULL 无 DEFAULT，省略会报约束错。
    pub fn result_put_md(&self, job_id: &str, md: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO results (job_id, json, md) VALUES (?1,'',?2)
             ON CONFLICT(job_id) DO UPDATE SET md=excluded.md",
            params![job_id, md],
        )?;
        Ok(())
    }

    pub fn result_get(&self, job_id: &str) -> Result<Option<(String, String)>> {
        self.conn
            .query_row("SELECT json, md FROM results WHERE job_id=?1", params![job_id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .map_err(Into::into)
    }

    /// 守卫式读改写：`BEGIN IMMEDIATE` 事务内取现 `state_json` → 交调用方闭包 → 闭包返
    /// `Some(新json)` 才 UPDATE、返 `None` 放弃写。空串（args/manifest 先建行、state 未写）
    /// 视同不存在→传 `None`。跨进程原子性靠 SQLite 写锁：`Immediate` 在 BEGIN 即取写锁，
    /// 配合已设的 `busy_timeout(5s)` 让并发写者排队而非丢更新（后台 worker 收尾 × 宿主 cancel
    /// 同刻竞争终态的关键序列化点）。db 层不识 `JobState`——只搬 JSON 字符串。
    pub fn job_update_guarded<F>(&mut self, job_id: &str, f: F) -> Result<()>
    where
        F: FnOnce(Option<String>) -> Option<String>,
    {
        let tx = self
            .conn
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: Option<String> = tx
            .query_row("SELECT state_json FROM jobs WHERE job_id=?1", params![job_id], |r| r.get(0))
            .optional()?;
        let current = current.filter(|s| !s.is_empty());
        if let Some(new_json) = f(current) {
            tx.execute(
                "INSERT INTO jobs (job_id, state_json) VALUES (?1,?2)
                 ON CONFLICT(job_id) DO UPDATE SET state_json=?2",
                params![job_id, new_json],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

}

fn row_to_card(r: &rusqlite::Row<'_>) -> rusqlite::Result<DismissCard> {
    Ok(DismissCard {
        id: r.get(0)?,
        status: r.get(1)?,
        title: r.get(2)?,
        file: r.get(3)?,
        quote: r.get(4)?,
        severity: r.get(5)?,
        reason: r.get(6)?,
        round: r.get(7)?,
        updated_at: r.get(8)?,
    })
}

fn row_to_resolve(r: &rusqlite::Row<'_>) -> rusqlite::Result<ResolveCard> {
    Ok(ResolveCard {
        id: r.get(0)?, title: r.get(1)?, file: r.get(2)?, quote: r.get(3)?,
        severity: r.get(4)?, reason: r.get(5)?, round: r.get(6)?, updated_at: r.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, status: &str) -> DismissCard {
        DismissCard {
            id: id.into(), status: status.into(), title: "t".into(), file: "src/a.rs".into(),
            quote: "let x = 1;".into(), severity: "high".into(), reason: "误报".into(),
            round: "r1".into(), updated_at: String::new(),
        }
    }

    fn mem_db() -> ProjDb {
        let dir = tempfile::tempdir().unwrap();
        // 用文件库而非 :memory:，覆盖 open_at 的建表+WAL 路径；tempdir 随测试清理
        let db = ProjDb::open_at(&dir.path().join("t.db")).unwrap();
        std::mem::forget(dir); // 保持文件到进程尾（测试级泄漏可接受）
        db
    }

    /// 三态遗留行在 open 时被复位（2026-10-04 对账落地的发布门）。
    ///
    /// 批 C 把台账从三态收成两态时没规范化已有数据：旧二进制写下的 `status='activated'`
    /// 行在新语义下谁也不认（激活判据只看 `dormant`、披露也只看 `dormant`），**永久卡住**。
    ///
    /// 关键在于这类库**已经是 v4 且表齐全**——`needs_setup` 为假，`setup_schema` 整条
    /// 迁移路径根本不跑。所以规范化挂在 open 上，这条用例也必须走「关掉再打开」而不是
    /// 建库那一次。
    #[test]
    fn reopening_normalizes_three_state_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        let row = |id: &str, status: &str| SuspicionRow {
            id: id.into(), file: "a.rs".into(), quote_norm: format!("q{id}"), hint: "h".into(),
            status: status.into(), first_job: "j1".into(), last_job: "j1".into(),
            activations: 0, json: "{}".into(),
        };
        {
            let db = ProjDb::open_at(&path).unwrap();
            // `suspicion_upsert` 不动 status，正好用来摆出旧二进制留下的那种行
            db.suspicion_upsert(&row("aaaaaaaaaaaaaaaa", "activated")).unwrap();
            db.suspicion_upsert(&row("bbbbbbbbbbbbbbbb", "dormant")).unwrap();
            db.suspicion_upsert(&row("cccccccccccccccc", "promoted")).unwrap();
        }
        let db = ProjDb::open_at(&path).unwrap();
        let status_of = |id: &str| -> String {
            db.conn
                .query_row("SELECT status FROM suspicions WHERE id=?1", [id], |r| r.get(0))
                .unwrap()
        };
        assert_eq!(status_of("aaaaaaaaaaaaaaaa"), "dormant", "三态遗留行要复位，否则永久卡住");
        assert_eq!(status_of("bbbbbbbbbbbbbbbb"), "dormant", "本来就 dormant 的不动");
        assert_eq!(status_of("cccccccccccccccc"), "promoted", "已晋升的**不能**被复位——那会让它被反复重挖");
    }

    #[test]
    fn upsert_get_roundtrip_and_history() {
        let mut db = mem_db();
        db.dismiss_upsert(&card("a1b2c3d4e5f60718", "dismissed")).unwrap();
        let got = db.dismiss_get("a1b2c3d4e5f60718").unwrap().unwrap();
        assert_eq!(got.status, "dismissed");
        assert_eq!(got.file, "src/a.rs");
        assert!(!got.updated_at.is_empty(), "updated_at 由 sqlite 填充");
        assert_eq!(db.dismiss_history("a1b2c3d4e5f60718").unwrap().len(), 1);
    }

    #[test]
    fn set_status_transitions_and_warns_on_final_override() {
        let mut db = mem_db();
        db.dismiss_upsert(&card("a1b2c3d4e5f60718", "dismissed")).unwrap();
        let (c, w) = db.dismiss_set_status("a1b2c3d4e5f60718", "human-confirmed", "r3", None).unwrap();
        assert_eq!(c.status, "human-confirmed");
        assert!(w.is_none());
        assert_eq!(c.reason, "误报", "reason 未给沿用旧值");
        let (_, w2) = db.dismiss_set_status("a1b2c3d4e5f60718", "withdrawn", "r4", Some("改判属实")).unwrap();
        assert!(w2.is_some(), "human-confirmed 终局被改 → warning");
        assert_eq!(db.dismiss_history("a1b2c3d4e5f60718").unwrap().len(), 3);
    }

    #[test]
    fn active_sets_exclude_withdrawn_and_stale() {
        let mut db = mem_db();
        db.dismiss_upsert(&card("1111111111111111", "dismissed")).unwrap();
        db.dismiss_upsert(&card("2222222222222222", "withdrawn")).unwrap();
        db.dismiss_upsert(&card("3333333333333333", "stale")).unwrap();
        db.dismiss_upsert(&card("4444444444444444", "human-confirmed")).unwrap();
        let ids = db.active_ids().unwrap();
        assert!(ids.contains("1111111111111111") && ids.contains("4444444444444444"));
        assert!(!ids.contains("2222222222222222") && !ids.contains("3333333333333333"));
        assert_eq!(db.active_cards().unwrap().len(), 2);
        assert_eq!(db.dismiss_list(None).unwrap().len(), 4, "list 不过滤时全量可见");
    }

    #[test]
    fn unknown_status_rejected() {
        let mut db = mem_db();
        assert!(db.dismiss_upsert(&card("1111111111111111", "banana")).is_err());
        db.dismiss_upsert(&card("1111111111111111", "dismissed")).unwrap();
        assert!(db.dismiss_set_status("1111111111111111", "banana", "r1", None).is_err());
        assert!(db.dismiss_set_status("ffffffffffffffff", "stale", "r1", None).is_err(), "未知 id 报错");
    }

    #[test]
    fn jobs_cells_results_roundtrip() {
        let db = mem_db();
        db.job_put_args("j1", r#"{"mode":null}"#).unwrap();
        db.job_put_state("j1", r#"{"job_id":"j1"}"#).unwrap();
        db.job_put_manifest("j1", "[]").unwrap();
        assert_eq!(db.job_get_state("j1").unwrap().unwrap(), r#"{"job_id":"j1"}"#);
        assert_eq!(db.job_get_args("j1").unwrap().unwrap(), r#"{"mode":null}"#);
        assert_eq!(db.job_get_manifest("j1").unwrap().unwrap(), "[]");
        assert!(db.job_get_args("nope").unwrap().is_none());

        db.cell_put("j1", "bundle[a]@m", "reviewer", r#"{"ok":false}"#).unwrap();
        db.cell_put("j1", "bundle[a]@m", "scanner", r#"{"ok":true}"#).unwrap();
        let cells = db.cell_all("j1").unwrap();
        assert_eq!(cells.len(), 1, "同 cell 覆盖（重试成功覆盖失败）");
        assert_eq!(cells[0], r#"{"ok":true}"#);
        assert_eq!(db.cell_role("j1", "bundle[a]@m").unwrap().as_deref(), Some("scanner"), "覆盖写同时更新 role 列");
        assert_eq!(db.cell_role("j1", "nope").unwrap(), None);

        db.result_put_json("j1", "{}").unwrap();
        db.result_put_md("j1", "# md").unwrap();
        let (j, m) = db.result_get("j1").unwrap().unwrap();
        assert_eq!((j.as_str(), m.as_str()), ("{}", "# md"));
        assert_eq!(db.job_all_states().unwrap().len(), 1);
    }

    #[test]
    fn result_put_json_and_md_upsert_independently_without_clobbering() {
        // M17-c C-Q4：单列 upsert 不读另一列现值，先后各写一次互不覆盖。
        let db = mem_db();
        db.result_put_json("j1", r#"{"v":1}"#).unwrap();
        // md 尚未写入 → DEFAULT '' 占位
        assert_eq!(db.result_get("j1").unwrap().unwrap(), (r#"{"v":1}"#.to_string(), String::new()));
        db.result_put_md("j1", "# md").unwrap();
        // 写 md 不覆盖已落的 json
        assert_eq!(db.result_get("j1").unwrap().unwrap(), (r#"{"v":1}"#.to_string(), "# md".to_string()));
        // 再写一次 json，md 不受影响
        db.result_put_json("j1", r#"{"v":2}"#).unwrap();
        assert_eq!(db.result_get("j1").unwrap().unwrap(), (r#"{"v":2}"#.to_string(), "# md".to_string()));
    }

    #[test]
    fn result_put_md_first_defaults_json_to_empty_string() {
        // md 先到场景（json 尚未产出）：首建行 json 显式占位 ''，不报 NOT NULL 约束错。
        let db = mem_db();
        db.result_put_md("j1", "# early md").unwrap();
        assert_eq!(db.result_get("j1").unwrap().unwrap(), (String::new(), "# early md".to_string()));
    }

    fn rcard(id: &str) -> ResolveCard {
        ResolveCard {
            id: id.into(), title: "t".into(), file: "src/a.rs".into(),
            quote: "let x = 1;".into(), severity: "high".into(),
            reason: "加了守卫".into(), round: "r2".into(), updated_at: String::new(),
        }
    }

    #[test]
    fn resolve_upsert_get_cards_roundtrip() {
        let mut db = mem_db();
        db.resolve_upsert(&rcard("a1b2c3d4e5f60718")).unwrap();
        let got = db.resolve_get("a1b2c3d4e5f60718").unwrap().unwrap();
        assert_eq!((got.file.as_str(), got.round.as_str()), ("src/a.rs", "r2"));
        assert!(!got.updated_at.is_empty());
        // 再销案（争议后重修）→ upsert 覆盖 + history 两条
        let mut again = rcard("a1b2c3d4e5f60718");
        again.round = "r4".into();
        db.resolve_upsert(&again).unwrap();
        assert_eq!(db.resolve_get("a1b2c3d4e5f60718").unwrap().unwrap().round, "r4");
        assert_eq!(db.resolved_cards().unwrap().len(), 1, "同 id 覆盖不追加");
    }

    #[test]
    fn resolve_is_disjoint_from_dismiss_exemption() {
        // 已修复 ≠ 误报：resolve 卡绝不进 dismissed 豁免/预筛面（codex #10 核实项的绊线）
        let mut db = mem_db();
        db.resolve_upsert(&rcard("1111111111111111")).unwrap();
        assert!(db.active_ids().unwrap().is_empty(), "resolve 不得漏进豁免 id 集");
        assert!(db.active_cards().unwrap().is_empty(), "resolve 不得漏进判例卡集");
    }

    #[test]
    fn resolve_reupset_preserves_reason_when_empty() {
        // 再销案未给新 reason（空串）→ 沿用旧卡理由，防二次销案静默清空审计理由
        let mut db = mem_db();
        let mut first = rcard("a1b2c3d4e5f60718");
        first.reason = "加了守卫".into();
        db.resolve_upsert(&first).unwrap();

        // 再销案：同 id，无新 reason（空串）
        let mut second = rcard("a1b2c3d4e5f60718");
        second.reason = String::new(); // 空，CLI --reason 默认值
        second.round = "r3".into();
        db.resolve_upsert(&second).unwrap();

        // 主表 reason 沿用旧值
        let after = db.resolve_get("a1b2c3d4e5f60718").unwrap().unwrap();
        assert_eq!(after.reason, "加了守卫", "再销案空 reason → 沿用旧值");
        assert_eq!(after.round, "r3", "round 该更新");

        // history 追加两条（两次 upsert）
        assert_eq!(
            db.resolve_history("a1b2c3d4e5f60718").unwrap().len(),
            2,
            "history 应有两条记录"
        );

        // 第二条 history 记的是有效 reason（沿用值）
        let hist = db.resolve_history("a1b2c3d4e5f60718").unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].1, "加了守卫", "首次 history reason");
        assert_eq!(hist[1].1, "加了守卫", "再销案 history reason（沿用值）");
    }

    #[test]
    fn resolve_reupset_updates_reason_when_nonempty() {
        // 再销案提供新 reason → 覆盖旧值
        let mut db = mem_db();
        let mut first = rcard("a1b2c3d4e5f60718");
        first.reason = "加了守卫".into();
        db.resolve_upsert(&first).unwrap();

        let mut second = rcard("a1b2c3d4e5f60718");
        second.reason = "重构 API".into();
        second.round = "r3".into();
        db.resolve_upsert(&second).unwrap();

        let after = db.resolve_get("a1b2c3d4e5f60718").unwrap().unwrap();
        assert_eq!(after.reason, "重构 API", "新 reason 覆盖旧值");

        let hist = db.resolve_history("a1b2c3d4e5f60718").unwrap();
        assert_eq!(hist[1].1, "重构 API", "再销案 history reason（新值）");
    }
}

#[cfg(test)]
mod guard_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn guarded_none_aborts_write() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ProjDb::open(dir.path()).unwrap();
        db.job_put_state("j", r#"{"v":1}"#).unwrap();
        // 闭包返 None → 不写
        db.job_update_guarded("j", |_cur| None).unwrap();
        assert_eq!(db.job_get_state("j").unwrap().as_deref(), Some(r#"{"v":1}"#));
    }

    #[test]
    fn guarded_some_writes_and_sees_current() {
        let dir = tempfile::tempdir().unwrap();
        let mut db = ProjDb::open(dir.path()).unwrap();
        db.job_put_state("j", r#"{"v":1}"#).unwrap();
        db.job_update_guarded("j", |cur| {
            assert_eq!(cur.as_deref(), Some(r#"{"v":1}"#)); // 事务内见现值
            Some(r#"{"v":2}"#.to_string())
        }).unwrap();
        assert_eq!(db.job_get_state("j").unwrap().as_deref(), Some(r#"{"v":2}"#));
    }

    fn table_names(conn: &Connection) -> Vec<String> {
        ProjDb::table_names(conn).unwrap()
    }
    fn user_version(conn: &Connection) -> i64 {
        ProjDb::user_version(conn).unwrap()
    }
    fn journal_mode(conn: &Connection) -> String {
        conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0)).unwrap()
    }
    /// 造一个 v0 旧形状库（dev 二进制风格：无 role 列、无 user_version），可选塞一张 dismissed 卡。
    fn old_shape_db(path: &Path, with_row: bool) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE dismissed (id TEXT PRIMARY KEY, status TEXT NOT NULL, reason TEXT NOT NULL);
             CREATE TABLE dismissed_history (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL);
             CREATE TABLE cells (job_id TEXT NOT NULL, cell TEXT NOT NULL, record_json TEXT NOT NULL, PRIMARY KEY (job_id, cell));",
        ).unwrap();
        if with_row {
            c.execute_batch("INSERT INTO dismissed VALUES ('a1b2c3d4e5f60718', 'dismissed', '误报');").unwrap();
        }
    }

    /// v4 空建：既有七表 + queue_items / suspicions 两张表壳，user_version = 4（出口 8）。
    #[test]
    fn fresh_db_builds_v4_tables_and_user_version() {
        let dir = tempfile::tempdir().unwrap();
        let db = ProjDb::open(dir.path()).unwrap();
        assert!(dir.path().join(crate::project::PROJECT_DIR).join(DB_FILE).is_file(), "库文件名 = DB_FILE");
        assert_eq!(
            table_names(&db.conn),
            ["cells", "dismissed", "dismissed_history", "jobs", "queue_items", "resolved", "resolved_history", "results", "suspicions"]
        );
        assert_eq!(user_version(&db.conn), USER_VERSION);
        // cells 带 role 列；再开一次幂等
        db.cell_put("j", "c", "scanner", "{}").unwrap();
        drop(db);
        let again = ProjDb::open(dir.path()).unwrap();
        assert_eq!(user_version(&again.conn), USER_VERSION);
        assert_eq!(again.cell_role("j", "c").unwrap().as_deref(), Some("scanner"));
    }

    #[test]
    fn queue_item_put_roundtrip_and_empty_job() {
        let dir = tempfile::tempdir().unwrap();
        let db = ProjDb::open(dir.path()).unwrap();
        assert!(db.queue_items_for_job("j").unwrap().is_empty());
        db.queue_item_put("j", "a.rs:rule:1-2", "pending", r#"{"id":"a"}"#).unwrap();
        db.queue_item_put("j", "a.rs:rule:1-2", "settled", r#"{"id":"a","ok":true}"#).unwrap();
        let rows = db.queue_items_for_job("j").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, "a.rs:rule:1-2");
        assert_eq!(rows[0].1, "settled");
        assert!(rows[0].2.contains("ok"));
        assert!(db.queue_items_for_job("other").unwrap().is_empty());
    }

    /// 造一个**与 v4 同构**的旧库（真实 dev 库的形状：台账四表逐列一致、评审史三表在、无 role 列、无 user_version）。
    fn old_shape_db_isomorphic(path: &Path) {
        let c = Connection::open(path).unwrap();
        c.execute_batch(
            "CREATE TABLE dismissed (id TEXT PRIMARY KEY, status TEXT NOT NULL, title TEXT NOT NULL DEFAULT '',
               file TEXT NOT NULL DEFAULT '', quote TEXT NOT NULL DEFAULT '', severity TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL, round TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '');
             CREATE TABLE dismissed_history (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL,
               status TEXT NOT NULL, round TEXT NOT NULL DEFAULT '', reason TEXT NOT NULL DEFAULT '', at TEXT NOT NULL DEFAULT '');
             CREATE TABLE resolved (id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', file TEXT NOT NULL DEFAULT '',
               quote TEXT NOT NULL DEFAULT '', severity TEXT NOT NULL DEFAULT '', reason TEXT NOT NULL DEFAULT '',
               round TEXT NOT NULL DEFAULT '', updated_at TEXT NOT NULL DEFAULT '');
             CREATE TABLE resolved_history (seq INTEGER PRIMARY KEY AUTOINCREMENT, id TEXT NOT NULL,
               round TEXT NOT NULL DEFAULT '', reason TEXT NOT NULL DEFAULT '', at TEXT NOT NULL DEFAULT '');
             CREATE TABLE jobs (job_id TEXT PRIMARY KEY, status TEXT NOT NULL);
             CREATE TABLE cells (job_id TEXT NOT NULL, cell TEXT NOT NULL, record_json TEXT NOT NULL, PRIMARY KEY (job_id, cell));
             CREATE TABLE results (job_id TEXT PRIMARY KEY, json TEXT NOT NULL);
             INSERT INTO dismissed (id,status,title,file,quote,severity,reason,round,updated_at)
               VALUES ('a1b2c3d4e5f60718','dismissed','t','a.rs','let x = 1;','low','误报','r1','2026-01-01');
             INSERT INTO dismissed_history (id,status) VALUES ('a1b2c3d4e5f60718','dismissed');
             INSERT INTO resolved (id,title) VALUES ('ffffffffffffffff','fixed');
             INSERT INTO jobs VALUES ('j1','done');
             INSERT INTO cells VALUES ('j1','scan[g1]@m','{}');
             INSERT INTO results VALUES ('j1','{}');",
        ).unwrap();
    }

    /// 同构旧库（真实 dev 库形状）→ **带账一次性迁移**（2026-09-28 裁，改 D10-2「不写迁移」）：
    /// 台账四表原样留下、评审史三表丢弃、v4 缺的表补齐、版本号补上；动手前有整库快照。
    #[test]
    fn isomorphic_old_db_migrates_keeping_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        old_shape_db_isomorphic(&path);

        let db = ProjDb::open_at(&path).expect("同构旧库应带账迁移而不是拒开");
        let m = db.legacy_migration().expect("应报告迁移事实").clone();
        assert_eq!((m.dismissed, m.resolved), (1, 1), "卡数如实上报");
        assert_eq!(m.dropped, ["cells", "jobs", "results"], "台账之外的表全丢（这里恰好只有评审史三张）");
        assert!(m.backup.is_file(), "迁移前整库快照: {:?}", m.backup);

        // 台账还在、且能按 v4 的接口读出来
        assert_eq!(db.active_ids().unwrap().len(), 1, "dismissed 卡跟着过来了");
        assert_eq!(db.active_cards().unwrap()[0].reason, "误报", "卡片内容原样");
        assert_eq!(user_version(&db.conn), USER_VERSION);
        assert_eq!(
            table_names(&db.conn),
            ["cells", "dismissed", "dismissed_history", "jobs", "queue_items", "resolved", "resolved_history", "results", "suspicions"]
        );
        // 评审史清空、cells 长出 role 列
        assert_eq!(db.conn.query_row("SELECT count(*) FROM cells", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        db.cell_put("j2", "c", "reviewer", "{}").unwrap();
        assert_eq!(db.cell_role("j2", "c").unwrap().as_deref(), Some("reviewer"));

        // 备份里是**迁移前**的库：评审史还在
        let b = Connection::open(&m.backup).unwrap();
        assert_eq!(b.query_row("SELECT count(*) FROM results", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "快照留着评审史");
        assert_eq!(user_version(&b), 0, "快照是迁移前的版本号");

        // 幂等：再开一次不再迁移（已是 v4），也不再动快照
        let before = std::fs::metadata(&m.backup).unwrap().len();
        drop(db);
        let again = ProjDb::open_at(&path).unwrap();
        assert!(again.legacy_migration().is_none(), "第二次打开不该再迁移");
        assert_eq!(std::fs::metadata(&m.backup).unwrap().len(), before, "快照不被覆盖");
    }

    /// 同名文件已在但**不是可用快照**（残片 / 根本不是库）→ 不复用、不覆盖，另起名（收口评审 #4）。
    #[test]
    fn unusable_existing_backup_is_not_reused_nor_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        old_shape_db_isomorphic(&path);
        // 与 backup_legacy 同款命名：<stem>-legacy-<YYYYMMDD>.db，日期同样取自 sqlite
        let day: String = Connection::open_in_memory()
            .unwrap()
            .query_row("SELECT strftime('%Y%m%d','now')", [], |r| r.get::<_, String>(0))
            .unwrap();
        let dest = dir.path().join(format!("proj-legacy-{day}.db"));
        std::fs::write(&dest, b"sentinel-not-a-db").unwrap();

        let db = ProjDb::open_at(&path).expect("同名文件不该阻断迁移");
        let m = db.legacy_migration().expect("仍应迁移").clone();
        assert_eq!(std::fs::read(&dest).unwrap(), b"sentinel-not-a-db", "别人的同名文件一个字节都不该动");
        assert_eq!(m.backup, dir.path().join(format!("proj-legacy-{day}-2.db")), "另起名: {:?}", m.backup);
        assert!(ProjDb::is_usable_snapshot(&m.backup), "新快照必须是能用的库");
        assert_eq!(db.active_ids().unwrap().len(), 1, "台账照常带过来");
        // 同一天再来一次（先造回旧库）：这次 -2 是可用快照 → 复用它，不再新建 -3
        drop(db);
        std::fs::remove_file(&path).unwrap();
        old_shape_db_isomorphic(&path);
        let again = ProjDb::open_at(&path).unwrap();
        assert_eq!(again.legacy_migration().unwrap().backup, m.backup, "可用快照复用、不递增");
    }

    /// 被拒开的库**不留快照文件**（收口评审 #1：原先备份判据比守卫宽松，
    /// 误传给 `--dismissed` 的业务库会被无声复制一份全量副本）。
    #[test]
    fn refused_dbs_leave_no_snapshot() {
        for (name, build) in [
            ("陌生表", (|p: &Path| {
                let c = Connection::open(p).unwrap();
                c.execute_batch("CREATE TABLE customers (id TEXT); INSERT INTO customers VALUES ('x');").unwrap();
            }) as fn(&Path)),
            ("台账不同构", |p: &Path| old_shape_db(p, true)),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("proj.db");
            build(&path);
            assert!(ProjDb::open_at(&path).is_err(), "{name}: 该拒开");
            let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
                .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
                .filter(|n| n.contains("legacy") || n.ends_with(".part")).collect();
            assert!(leftovers.is_empty(), "{name}: 拒开却留下了副本 {leftovers:?}");
        }
    }

    /// 迁移丢弃**台账以外的全部表**，`queue_items` / `suspicions` 的半成品形状不会被盖上 v4 戳记
    /// （收口评审 #3：它们在 TABLES 里不算陌生表，`CREATE TABLE IF NOT EXISTS` 又补不上缺的列，
    /// 库会进入「queue_item_put 永久失败、守卫再也不触发」的不可自愈坏态）。
    #[test]
    fn migration_drops_halfbaked_v4_tables_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        old_shape_db_isomorphic(&path);
        let c = Connection::open(&path).unwrap();
        // 少了 state 列、也没有复合主键的 queue_items
        c.execute_batch("CREATE TABLE queue_items (job_id TEXT NOT NULL, item_id TEXT NOT NULL, json TEXT NOT NULL);
                         INSERT INTO queue_items VALUES ('j','i','{}');").unwrap();
        drop(c);

        let db = ProjDb::open_at(&path).expect("同构旧库仍应迁移");
        assert!(db.legacy_migration().unwrap().dropped.iter().any(|t| t == "queue_items"), "半成品新表也该丢");
        assert_eq!(ProjDb::columns(&db.conn, "queue_items").unwrap(), ["job_id", "item_id", "state", "json"]);
        db.queue_item_put("j", "i", "pending", "{}").expect("重建后写得进去");
        assert_eq!(db.active_ids().unwrap().len(), 1, "台账仍然带过来");
    }

    /// 有数据但台账表全缺席（数据都在评审史里）→ 照样迁移、无账可带，且**仍然备份**。
    #[test]
    fn old_db_without_ledger_tables_migrates_with_nothing_to_keep() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE jobs (job_id TEXT PRIMARY KEY, status TEXT NOT NULL);
                         INSERT INTO jobs VALUES ('j1','done');").unwrap();
        drop(c);
        let db = ProjDb::open_at(&path).unwrap();
        let m = db.legacy_migration().expect("有数据就该走迁移");
        assert_eq!((m.dismissed, m.resolved), (0, 0), "无账可带");
        assert!(m.backup.is_file(), "仍要有快照");
        assert_eq!(user_version(&db.conn), USER_VERSION);
    }

    /// 无快照就不迁移（收口评审 #5）：事务外判「不迁移」而事务内判「要迁移」的竞态下，
    /// 原实现 `unwrap_or_default()` 会吞掉缺席的备份、无快照直接 DROP 评审史。
    #[test]
    fn migration_refuses_without_a_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        old_shape_db_isomorphic(&path);
        let conn = Connection::open(&path).unwrap();
        let err = ProjDb::setup_schema(&conn, &path, None).unwrap_err().to_string();
        assert!(err.contains("没有拿到迁移前快照") && err.contains("请重跑"), "{err}");
        // 库没被动过：评审史还在、版本号还是 0
        assert_eq!(conn.query_row("SELECT count(*) FROM results", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(user_version(&conn), 0);
    }

    /// 只读入口不改库（收口评审 #2）：`--dismissed <dev 库>` 读得出台账，且库一个字节不变。
    #[test]
    fn read_only_open_reads_legacy_ledger_without_touching_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        old_shape_db_isomorphic(&path);
        let before = std::fs::read(&path).unwrap();

        let db = ProjDb::open_read_only(&path).unwrap();
        assert_eq!(db.active_ids().unwrap().len(), 1, "旧库的台账读得出来");
        assert_eq!(db.active_cards().unwrap()[0].reason, "误报");
        assert!(db.legacy_migration().is_none(), "只读不迁移");
        drop(db);

        assert_eq!(std::fs::read(&path).unwrap(), before, "库一个字节都不该变");
        let c = Connection::open(&path).unwrap();
        assert_eq!(user_version(&c), 0, "版本号不该被写成 4");
        assert_eq!(c.query_row("SELECT count(*) FROM results", [], |r| r.get::<_, i64>(0)).unwrap(), 1, "评审史还在");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.contains("legacy-")).collect();
        assert!(leftovers.is_empty(), "只读不该留快照: {leftovers:?}");
    }

    /// 空旧库不触发迁移、也不留快照文件（零数据零风险，走原来的丢表重建）。
    #[test]
    fn empty_old_db_rebuilds_without_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        old_shape_db(&path, false);
        let db = ProjDb::open_at(&path).unwrap();
        assert!(db.legacy_migration().is_none(), "空库不算迁移");
        assert_eq!(user_version(&db.conn), USER_VERSION);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path()).unwrap()
            .filter_map(|e| Some(e.ok()?.file_name().to_string_lossy().into_owned()))
            .filter(|n| n.contains("legacy")).collect();
        assert!(leftovers.is_empty(), "空库不该留快照: {leftovers:?}");
    }

    /// M21 分支期库名接管（M21c 批 A 补批 0 的漏）：切名后二进制只认 `proj.db`，
    /// 而 M21 期间跑过 `redteam-next` 的项目留下的是 `proj-next.db`——不接管就成孤儿，
    /// 作业历史、resume、`result` 全取不回。只在**目标不存在**时改名，绝不覆盖已有库。
    #[test]
    fn branch_era_db_is_adopted_but_never_overwrites() {
        // ① 只有分支期库 → 改名接管，历史跟着过来
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join(crate::project::PROJECT_DIR);
        std::fs::create_dir_all(&rt).unwrap();
        {
            let db = ProjDb::open_at(&rt.join("proj-next.db")).unwrap();
            db.cell_put("j1", "scan[g1]@m", "scanner", r#"{"role":"scanner"}"#).unwrap();
        }
        std::fs::write(rt.join("proj-next.db-wal"), b"wal").unwrap();
        let db = ProjDb::open(dir.path()).unwrap();
        assert_eq!(db.cell_role("j1", "scan[g1]@m").unwrap().as_deref(), Some("scanner"), "作业历史继承");
        assert!(rt.join("proj.db").is_file() && !rt.join("proj-next.db").exists(), "改名而不是复制");
        assert!(rt.join("proj.db-wal").is_file() && !rt.join("proj-next.db-wal").exists(), "-wal 一并搬");

        // ② 两个库都在 → 一个都不动（并存时不猜哪个是对的）
        let dir2 = tempfile::tempdir().unwrap();
        let rt2 = dir2.path().join(crate::project::PROJECT_DIR);
        std::fs::create_dir_all(&rt2).unwrap();
        {
            let old = ProjDb::open_at(&rt2.join("proj-next.db")).unwrap();
            old.cell_put("old", "c", "scanner", "{}").unwrap();
            let new = ProjDb::open_at(&rt2.join("proj.db")).unwrap();
            new.cell_put("new", "c", "scanner", "{}").unwrap();
        }
        let db2 = ProjDb::open(dir2.path()).unwrap();
        assert!(rt2.join("proj-next.db").is_file(), "已有正式库时不得接管");
        assert!(db2.cell_role("new", "c").unwrap().is_some() && db2.cell_role("old", "c").unwrap().is_none());

        // ③ 都不在 → 正常新建，无副作用
        let dir3 = tempfile::tempdir().unwrap();
        ProjDb::open(dir3.path()).unwrap();
        assert!(dir3.path().join(crate::project::PROJECT_DIR).join("proj.db").is_file());
    }

    /// 接管顺序：`-wal` / `-shm` 先搬、主库最后（收口评审 #2）。反过来的话主库改完名而 WAL 搬失败，
    /// sqlite 找不到配套 WAL，只能读到最后一次 checkpoint——上次被 kill 那轮未落盘的记录静默消失，
    /// 而函数还会打印「接管成功」。附属文件搬不动就整体放弃（已搬的搬回去），停在「未接管」可重试。
    #[test]
    fn adoption_moves_sidecars_first_and_rolls_back_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join(crate::project::PROJECT_DIR);
        std::fs::create_dir_all(&rt).unwrap();
        drop(ProjDb::open_at(&rt.join("proj-next.db")).unwrap());
        std::fs::write(rt.join("proj-next.db-wal"), b"wal-bytes").unwrap();
        // 目标 -wal 先被一个**目录**占住 → rename 必失败，触发回滚
        std::fs::create_dir(rt.join("proj.db-wal")).unwrap();

        ProjDb::adopt_branch_era_db(&rt);

        assert!(rt.join("proj-next.db").is_file(), "主库必须停在原名（未接管）");
        assert!(!rt.join("proj.db").exists(), "不得留下「有 wal 没库」的半途状态");
        assert_eq!(std::fs::read(rt.join("proj-next.db-wal")).unwrap(), b"wal-bytes", "WAL 要被搬回原处");
    }

    /// `is_current_version`：只读探测、零副作用——库不在 / 旧 schema / 打不开都返回 false，且不建库不改库。
    #[test]
    fn is_current_version_is_a_read_only_probe() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.db");
        assert!(!ProjDb::is_current_version(&missing));
        assert!(!missing.exists(), "探测不得建库");

        let old = dir.path().join("old.db");
        old_shape_db_isomorphic(&old);
        let before = std::fs::read(&old).unwrap();
        assert!(!ProjDb::is_current_version(&old), "旧 schema → false");
        assert_eq!(std::fs::read(&old).unwrap(), before, "探测不得改库（尤其不得触发迁移）");

        let fresh = dir.path().join("fresh.db");
        drop(ProjDb::open_at(&fresh).unwrap());
        assert!(ProjDb::is_current_version(&fresh));
    }

    /// 疑点台账 upsert 幂等（M21c 批 A）：同一处疑点跨轮只有一行，
    /// 第二轮只更新 `last_job` / `hint` / `json`，**不动** `first_job` / `activations` / `status`
    /// ——否则批 C 的激活计数与晋升状态会被下一轮落账悄悄抹回 dormant。
    #[test]
    fn suspicion_upsert_is_idempotent_and_keeps_activation_state() {
        let dir = tempfile::tempdir().unwrap();
        let db = ProjDb::open(dir.path()).unwrap();
        let row = |job: &str, hint: &str| SuspicionRow {
            id: "abc123".into(), file: "a.rs".into(), quote_norm: "letx=1;".into(), hint: hint.into(),
            status: "dormant".into(), first_job: job.into(), last_job: job.into(), activations: 0,
            json: format!("{{\"round\":\"{job}\"}}"),
        };
        db.suspicion_upsert(&row("sha1", "第一轮说法")).unwrap();
        // 模拟批 C：这条被激活过一次并晋升
        db.conn.execute("UPDATE suspicions SET activations=2, status='activated' WHERE id='abc123'", []).unwrap();

        db.suspicion_upsert(&row("sha2", "第二轮说法")).unwrap();
        let got = db.suspicions_for_files(&["a.rs".into()]).unwrap();
        assert_eq!(got.len(), 1, "跨轮只有一行");
        let r = &got[0];
        assert_eq!((r.first_job.as_str(), r.last_job.as_str()), ("sha1", "sha2"), "first 不动、last 更新");
        assert_eq!((r.activations, r.status.as_str()), (2, "activated"), "激活状态不被落账覆盖");
        assert_eq!(r.hint, "第二轮说法", "hint / json 取最新");
        assert!(r.json.contains("sha2"));

        // 按文件查：不匹配的文件与空入参都不返回（空入参不得退化成全表）
        assert!(db.suspicions_for_files(&["b.rs".into()]).unwrap().is_empty());
        assert!(db.suspicions_for_files(&[]).unwrap().is_empty());
    }

    /// 激活回写：**同轮幂等、跨轮累加**（M21c 批 C；原版在 dogfood 里被判为死代码，见下）。
    ///
    /// 幂等靠 `last_job <> ?`，不是靠改 status。原先这条 UPDATE 写 `status='activated'` 而条件是
    /// `status='dormant'`，于是一行一生只能加一次，`activations` 饱和在 1，而两段门在 3 / 5——
    /// 整个活锁守卫是不可达的死代码。这条用例因此**走真实 DB 往返**而不是手搓中间状态：
    /// 原来的单测直接构造 `activations: 2..4` 的行，那是生产路径产生不出来的，所以测不出门不可达。
    #[test]
    fn activation_writeback_is_idempotent_per_round_but_accumulates_across_rounds() {
        let dir = tempfile::tempdir().unwrap();
        let db = ProjDb::open(dir.path()).unwrap();
        let row = |id: &str, status: &str| SuspicionRow {
            id: id.into(), file: "a.rs".into(), quote_norm: "q".into(), hint: "h".into(),
            status: status.into(), first_job: "sha0".into(), last_job: "sha1".into(),
            activations: 0, json: "{}".into(),
        };
        db.suspicion_upsert(&row("d1", "dormant")).unwrap();
        db.suspicion_upsert(&row("p1", "promoted")).unwrap();

        let count = |id: &str| {
            let got = db.suspicions_for_files(&["a.rs".into()]).unwrap();
            let r = got.into_iter().find(|r| r.id == id).unwrap();
            (r.status, r.activations, r.last_job)
        };

        // 同一轮重复调用只加一次
        db.suspicion_activate("d1", "sha2").unwrap();
        db.suspicion_activate("d1", "sha2").unwrap();
        assert_eq!(count("d1"), ("dormant".into(), 1, "sha2".into()), "同轮幂等，且 status 不动");

        // 换一轮再加一次——门（3 / 5）正是靠这个才可达
        db.suspicion_activate("d1", "sha3").unwrap();
        db.suspicion_activate("d1", "sha4").unwrap();
        assert_eq!(count("d1"), ("dormant".into(), 3, "sha4".into()), "跨轮累加，三轮后踩到强制必挖门");

        // 已晋升的行一个字段都不动
        db.suspicion_activate("p1", "sha2").unwrap();
        assert_eq!(count("p1"), ("promoted".into(), 0, "sha1".into()));

        // 晋升：挖出 finding 的退出激活池，且只对 dormant 生效（不把别的状态拉回来）
        db.suspicion_promote("d1").unwrap();
        assert_eq!(count("d1"), ("promoted".into(), 3, "sha4".into()), "晋升只改 status、不动计数");
        db.suspicion_activate("d1", "sha5").unwrap();
        assert_eq!(count("d1"), ("promoted".into(), 3, "sha4".into()), "晋升后不再被激活");
    }

    /// md-first 写入留下的半行（`json=''`）不得被当成「上一轮的裁决」（dogfood medium，agreement 2）：
    /// 选中它会解析失败 → `Err` → 激活闸门保守关闭，**一次正常的 md 落盘就把下一轮的激活整个掐掉**。
    ///
    /// 用例刻意只放那一行：如果再放一行正常的 json，两行的 `created_at` 在同一秒内写入、
    /// 排序没有 tie-breaker（这本身是另一条已记账的债），断言会变成掷硬币——变异检查当场抓到过。
    #[test]
    fn last_verdict_skips_md_first_stub_rows() {
        let dir = tempfile::tempdir().unwrap();
        let db = ProjDb::open(dir.path()).unwrap();
        assert_eq!(db.last_verdict().unwrap(), None, "没有任何轮次");

        db.result_put_md("j1", "# 只有 md 的那一行").unwrap();
        assert_eq!(db.last_verdict().unwrap(), None, "半行 = 没有可读的往轮裁决，不是读失败");

        db.result_put_json("j1", r#"{"verdict":"approve"}"#).unwrap();
        assert_eq!(db.last_verdict().unwrap().as_deref(), Some("approve"), "补上 json 后读得到");
    }

    /// 旧库有数据、**且台账表与 v4 不同构**（这里 dismissed 只有 3 列）→ 拒开：文案指向导出脚本与库路径、
    /// 不叫用户删库；库文件一个字节不改（含 journal 模式）。同构的旧库走另一条路（带账迁移，见下一条用例）。
    #[test]
    fn old_db_with_data_and_foreign_ledger_shape_is_refused_with_export_hint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        old_shape_db(&path, true);
        let err = ProjDb::open_at(&path).err().expect("旧库有数据必须拒开").to_string();
        assert!(err.contains("export_ledger.py"), "{err}");
        assert!(err.contains("proj.db"), "{err}");
        assert!(err.contains("v0") && err.contains("不同构"), "{err}");
        assert!(!err.contains("删除"), "独立库文件后不再叫用户删旧库: {err}");
        // 守卫只读：旧表、数据、版本号、journal 模式全部原样
        let c = Connection::open(&path).unwrap();
        assert_eq!(user_version(&c), 0);
        assert_eq!(c.query_row("SELECT count(*) FROM dismissed", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(table_names(&c), ["cells", "dismissed", "dismissed_history"]);
        assert_eq!(journal_mode(&c), "delete", "被拒开的库不应被切成 WAL");
    }

    /// 旧形状但全空（dev 二进制 `init` 出的空库）→ 丢表重建为 v4（cells 长出 role 列）。
    #[test]
    fn old_empty_db_is_rebuilt_as_v4() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        old_shape_db(&path, false);
        let db = ProjDb::open_at(&path).unwrap();
        assert_eq!(user_version(&db.conn), USER_VERSION);
        assert_eq!(table_names(&db.conn).len(), 9);
        db.cell_put("j", "c", "verifier", "{}").unwrap();
        assert_eq!(db.cell_role("j", "c").unwrap().as_deref(), Some("verifier"));
    }

    /// 版本高于本二进制 → 拒开且不降级（旧二进制不能把新库盖章成 v4）。
    #[test]
    fn newer_db_is_refused_not_downgraded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        ProjDb::open_at(&path).unwrap();
        Connection::open(&path).unwrap().pragma_update(None, "user_version", 5).unwrap();
        let err = ProjDb::open_at(&path).err().expect("高版本库必须拒开").to_string();
        assert!(err.contains("v5") && err.contains("不降级"), "{err}");
        assert_eq!(user_version(&Connection::open(&path).unwrap()), 5);
    }

    /// 陌生 sqlite 文件（如误传给 `--dismissed` 的业务库）→ 拒开、一张表都不动。
    #[test]
    fn foreign_sqlite_file_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite3");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO users (name) VALUES ('a');").unwrap();
        }
        let err = ProjDb::open_at(&path).err().expect("陌生库必须拒开").to_string();
        assert!(err.contains("陌生表") && err.contains("users"), "{err}");
        let c = Connection::open(&path).unwrap();
        assert_eq!(table_names(&c), ["users"]);
        assert_eq!(c.query_row("SELECT count(*) FROM users", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        // 空表的陌生库同样拒开（不能靠「全空」分支把别人的表丢掉）
        let path2 = dir.path().join("empty.sqlite3");
        Connection::open(&path2).unwrap().execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY);").unwrap();
        assert!(ProjDb::open_at(&path2).is_err());
        assert_eq!(table_names(&Connection::open(&path2).unwrap()), ["users"]);
    }

    /// 修补批 X-1：user_version 恰为 4 的陌生库（业务库把 user_version 当自己的迁移计数）也拒开、零改写。
    #[test]
    fn foreign_v4_versioned_db_is_refused_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.sqlite3");
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT); INSERT INTO users (name) VALUES ('a');").unwrap();
            c.pragma_update(None, "user_version", USER_VERSION).unwrap();
        }
        let before = std::fs::read(&path).unwrap();
        let err = ProjDb::open_at(&path).err().expect("v4 版本号的陌生库必须拒开").to_string();
        assert!(err.contains("陌生表") && err.contains("users"), "{err}");
        assert_eq!(std::fs::read(&path).unwrap(), before, "字节零改写（含 journal 模式）");
        assert_eq!(table_names(&Connection::open(&path).unwrap()), ["users"]);
    }

    /// 修补批 X-1：delete 模式（未切过 WAL）的只读 v4 库也可开——快路径切不了 WAL 时降级 warning 而非拒开。
    #[cfg(unix)]
    #[test]
    fn readonly_delete_mode_v4_db_opens_with_warning() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        {
            let db = ProjDb::open_at(&path).unwrap();
            db.cell_put("j", "c", "scanner", "{}").unwrap();
        }
        // 切回 delete 模式（`.dump | sqlite3` 恢复或旧工具写出的库）
        Connection::open(&path).unwrap().pragma_update(None, "journal_mode", "DELETE").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let db = ProjDb::open_at(&path).expect("只读 delete 模式 v4 库应可开");
        assert_eq!(db.cell_role("j", "c").unwrap().as_deref(), Some("scanner"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// `.dump | sqlite3` 恢复的 v4 库丢了 user_version（归 0）但结构已是 v4 → 视为 v4、只补版本号、数据保留。
    #[test]
    fn v4_shape_with_zero_user_version_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        {
            let db = ProjDb::open_at(&path).unwrap();
            db.cell_put("j", "c", "scanner", "{}").unwrap();
        }
        Connection::open(&path).unwrap().pragma_update(None, "user_version", 0).unwrap();
        let db = ProjDb::open_at(&path).unwrap();
        assert_eq!(user_version(&db.conn), USER_VERSION);
        assert_eq!(db.cell_role("j", "c").unwrap().as_deref(), Some("scanner"), "数据未被丢表");
    }

    /// 只读 v4 库：open 不写任何字节（无 BEGIN IMMEDIATE、无 pragma 写）→ 可开可读。
    #[cfg(unix)]
    #[test]
    fn readonly_v4_db_opens_without_writing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        {
            let mut db = ProjDb::open_at(&path).unwrap();
            db.dismiss_upsert(&DismissCard {
                id: "a1b2c3d4e5f60718".into(), status: "dismissed".into(), title: "t".into(), file: "a".into(),
                quote: "q".into(), severity: "low".into(), reason: "r".into(), round: "r1".into(), updated_at: String::new(),
            }).unwrap();
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let db = ProjDb::open_at(&path).expect("只读 v4 库应可开");
        assert_eq!(db.dismiss_list(None).unwrap().len(), 1);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn guarded_empty_state_is_none() {
        // args/manifest 先建行、state 空串 → 闭包收 None
        let dir = tempfile::tempdir().unwrap();
        let mut db = ProjDb::open(dir.path()).unwrap();
        db.job_put_args("j", r#"{"a":1}"#).unwrap(); // state_json=''
        let mut seen = Some("x".to_string());
        db.job_update_guarded("j", |cur| { seen = cur.clone(); None }).unwrap();
        assert_eq!(seen, None);
    }
}
