//! `.redteam/proj.db` 持久层：dismissed 判例台账 + jobs/cells/results 评审运行时与评审史。
//! 设计定案见 .doc/design/2026-07-03-dismissed-ledger-machinization-design.md：
//! 单库单连接、WAL、运行时结构存整列 JSON（行数个位~百位，拆列是过度建模）。

use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::path::Path;

/// 库文件名（挂在项目 `.redteam/` 下）。
pub const DB_FILE: &str = "proj.db";

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
    pub fn open_at(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("open db {}", path.display()))?;
        // 跨进程写并发（后台 job 写 state/results × 宿主 dismiss add）下 WAL 写锁冲突
        // 默认立即 SQLITE_BUSY——busy_timeout 让写者排队而非报错。
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS dismissed (
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
               record_json TEXT NOT NULL,
               PRIMARY KEY (job_id, cell)
             );
             CREATE TABLE IF NOT EXISTS results (
               job_id TEXT PRIMARY KEY,
               json TEXT NOT NULL,
               md TEXT NOT NULL DEFAULT '',
               created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );",
        )?;
        Ok(Self { conn })
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
    pub fn cell_put(&self, job_id: &str, cell: &str, record_json: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO cells (job_id, cell, record_json) VALUES (?1,?2,?3)
             ON CONFLICT(job_id, cell) DO UPDATE SET record_json=?3",
            params![job_id, cell, record_json],
        )?;
        Ok(())
    }

    pub fn cell_all(&self, job_id: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare("SELECT record_json FROM cells WHERE job_id=?1 ORDER BY cell")?;
        let rows = stmt.query_map(params![job_id], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// 评审史：结果持久存储，永不过期（设计定案 #8）。
    pub fn result_put(&self, job_id: &str, json: &str, md: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO results (job_id, json, md) VALUES (?1,?2,?3)
             ON CONFLICT(job_id) DO UPDATE SET json=?2, md=?3",
            params![job_id, json, md],
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

        db.cell_put("j1", "bundle[a]@m", r#"{"ok":false}"#).unwrap();
        db.cell_put("j1", "bundle[a]@m", r#"{"ok":true}"#).unwrap();
        let cells = db.cell_all("j1").unwrap();
        assert_eq!(cells.len(), 1, "同 cell 覆盖（重试成功覆盖失败）");
        assert_eq!(cells[0], r#"{"ok":true}"#);

        db.result_put("j1", "{}", "# md").unwrap();
        let (j, m) = db.result_get("j1").unwrap().unwrap();
        assert_eq!((j.as_str(), m.as_str()), ("{}", "# md"));
        assert_eq!(db.job_all_states().unwrap().len(), 1);
    }
}
