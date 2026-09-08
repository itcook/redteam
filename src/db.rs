//! `.redteam/proj.db` 持久层：dismissed 判例台账 + jobs/cells/results 评审运行时与评审史。
//! 设计定案见 .doc/design/2026-07-03-dismissed-ledger-machinization-design.md：
//! 单库单连接、WAL、运行时结构存整列 JSON（行数个位~百位，拆列是过度建模）。

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::path::Path;

/// 库文件名（挂在项目 `.redteam/` 下）。M21a-b 并行期与 dev 二进制的 `proj.db` **分库**（用户裁定 2026-09-08：
/// dev `redteam` 与 `redteam-next` 在同一仓 dogfood，v4 的 `cells.role NOT NULL` 会让 dev 的 checkpoint 写失败、
/// 旧库又被 v4 守卫拒开——两者不能共用一个文件）；M21c 切名时定回 `proj.db` 与切换程序（followup）。
pub const DB_FILE: &str = "proj-next.db";

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

pub struct ProjDb {
    conn: Connection,
}

impl ProjDb {
    /// 打开项目库（`<root>/.redteam/proj.db`）；目录/库/表缺失即自动创建（幂等）。
    pub fn open(root: &Path) -> Result<Self> {
        let dir = root.join(crate::project::PROJECT_DIR);
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        Self::open_at(&dir.join(DB_FILE))
    }

    /// 按显式路径打开（`--dismissed <db路径>` 覆盖场景）。
    /// 只读快路径：已是 v4 且全表在场 → 不写任何字节（只读文件 / 只读挂载 / 他人检出可开，与 dev 基线一致）；
    /// 否则在一个 `BEGIN IMMEDIATE` 事务里做版本守卫 + 建表 + 写版本号（并发 open 不会互相 DROP、不会撞 no such table）。
    pub fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open db {}", path.display()))?;
        // 跨进程写并发（后台 job 写 state/results × 宿主 dismiss add）下 WAL 写锁冲突
        // 默认立即 SQLITE_BUSY——busy_timeout 让写者排队而非报错。
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        if Self::needs_setup(&conn)? {
            conn.execute_batch("BEGIN IMMEDIATE")?;
            if let Err(e) = Self::setup_schema(&conn, path) {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
            conn.execute_batch("COMMIT")?;
        }
        // WAL 在守卫之后：被拒开的旧库 / 陌生库不被改写 journal 模式。
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Ok(Self { conn })
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

    /// 是否需要写库：版本不是 v4，或 v4 表不全。
    fn needs_setup(conn: &Connection) -> Result<bool> {
        if Self::user_version(conn)? != USER_VERSION {
            return Ok(true);
        }
        let have = Self::table_names(conn)?;
        Ok(!TABLES.iter().all(|t| have.iter().any(|h| h == t)))
    }

    /// schema v4 一次重置守卫（D10-2，不写迁移）+ 建表 + 版本号。在事务内调用。分支：
    /// - 版本高于本二进制 → 拒开（不降级）；
    /// - 含陌生表（不是 redteam 项目库，如误传给 `--dismissed` 的业务库）→ 拒开、不改写；
    /// - 版本 < v4 但 `cells` 已有 `role` 列（`.dump` 恢复丢 user_version 的 v4 库）→ 视为 v4，只补版本号；
    /// - 版本 < v4 且有数据 → 拒开、不改写，提示导出固定集；
    /// - 版本 < v4 且全空（如 dev 二进制 `init` 出的空库）→ 丢表重建（零数据零风险）；
    /// - 全新文件 → 建表。
    fn setup_schema(conn: &Connection, path: &Path) -> Result<()> {
        let version = Self::user_version(conn)?;
        let have = Self::table_names(conn)?;
        anyhow::ensure!(
            version <= USER_VERSION,
            "`{p}` 是 schema v{version} 的库，高于本二进制支持的 v{USER_VERSION}：请用更新的 redteam 打开，本版本不降级",
            p = path.display()
        );
        if version < USER_VERSION && !have.is_empty() {
            let foreign: Vec<&str> = have.iter().map(String::as_str).filter(|t| !TABLES.contains(t)).collect();
            anyhow::ensure!(
                foreign.is_empty(),
                "`{p}` 不是 redteam 项目库（含陌生表 {foreign:?}），拒绝改写",
                p = path.display()
            );
            let already_v4 = have.iter().any(|t| t == "cells") && Self::has_column(conn, "cells", "role")?;
            if !already_v4 {
                let mut rows = 0i64;
                for t in &have {
                    rows += conn.query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get::<_, i64>(0))?;
                }
                anyhow::ensure!(
                    rows == 0,
                    "`{p}` 是 schema v{version}（< v{USER_VERSION}）的旧库：本版本不迁移、也不改写它。redteam-next 使用独立库文件 \
                     `.redteam/{DB_FILE}`，旧库可与 dev redteam 继续并存；维护者可用 `python3 eval/export_ledger.py --db '{p}'` \
                     导出 dismissed / resolved / upheld 固定集（jobs / cells / results 评审史不导出）",
                    p = path.display()
                );
                for t in &have {
                    conn.execute_batch(&format!("DROP TABLE IF EXISTS \"{t}\""))?;
                }
            }
        }
        conn.execute_batch(SCHEMA_SQL)?;
        if version != USER_VERSION {
            conn.pragma_update(None, "user_version", USER_VERSION)?;
        }
        Ok(())
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

    /// 旧库（user_version 0 且有数据）拒开、文案指向导出脚本与库路径、不叫用户删库；库文件一个字节不改（含 journal 模式）。
    #[test]
    fn old_db_with_data_is_refused_with_export_hint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proj.db");
        old_shape_db(&path, true);
        let err = ProjDb::open_at(&path).err().expect("旧库有数据必须拒开").to_string();
        assert!(err.contains("export_ledger.py"), "{err}");
        assert!(err.contains("proj.db"), "{err}");
        assert!(err.contains("v0") && err.contains("不迁移"), "{err}");
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
