//! 作业运行时存取（design 2026-07-03 #8：state 层入库 `.redteam/proj.db`）。
//! 曾为「系统 temp + workspace bucket 分桶 + TTL prune」的临时文件层——sqlite 单库后
//! 整层退役：评审结果是项目资产持久存储、跨项目状态查询 YAGNI、REDTEAM_STATE_DIR 退役。
//! `ws` 参数语义 = **项目根**（含 .redteam/ 的目录），签名兼容旧调用形状。

use std::path::Path;
use serde::{Deserialize, Serialize};

// ── manifest / partial 数据结构 ────────────────────────────────────────────────

/// manifest 每行：输入快照（含渲染后 prompt 全文）。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellInput {
    pub cell: String,
    pub model: String,
    pub prompt: String,
}

/// partial 每 cell：单个 cell 的执行结果（db 同键覆盖：重试成功覆盖失败记录）。
/// 运行学字段（duration/tokens/attempts，2026-07-03 grill 定案）serde default——旧记录零值兼容。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellRecord {
    pub cell: String,
    pub model: String,
    pub ok: bool,
    pub findings: Vec<crate::domain::Finding>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    /// 候补链实际尝试的 model 数（成功=第几家成功；整链失败=链长）。
    #[serde(default)]
    pub attempts: u32,
}

impl CellRecord {
    /// 便捷构造：ok=true, warn=None（运行学字段由调用方按需回填）。
    pub fn ok(cell: impl Into<String>, model: impl Into<String>, findings: Vec<crate::domain::Finding>) -> Self {
        Self {
            cell: cell.into(), model: model.into(), ok: true, findings, warn: None,
            duration_ms: 0, input_tokens: 0, output_tokens: 0, attempts: 0,
        }
    }
}

fn db(ws: &Path) -> anyhow::Result<crate::db::ProjDb> {
    crate::db::ProjDb::open(ws)
}

// ── manifest ──────────────────────────────────────────────────────────────────

/// 写全部 manifest（一次性整列覆盖）。
pub fn write_manifest(ws: &Path, id: &str, inputs: &[CellInput]) -> std::io::Result<()> {
    let json = serde_json::to_string(inputs)?;
    db(ws)
        .and_then(|d| d.job_put_manifest(id, &json))
        .map_err(std::io::Error::other)
}

/// 读 manifest；缺失/损坏 → 空（调用方按「空 manifest」护栏处理）。
pub fn read_manifest(ws: &Path, id: &str) -> Vec<CellInput> {
    db(ws)
        .ok()
        .and_then(|d| d.job_get_manifest(id).ok().flatten())
        .and_then(|j| serde_json::from_str(&j).ok())
        .unwrap_or_default()
}

// ── partial（cell checkpoint）─────────────────────────────────────────────────

/// 单 cell 结果落库（同 (job,cell) 覆盖——重试成功覆盖失败行，读取端零合并逻辑）。
pub fn append_partial(ws: &Path, id: &str, record: &CellRecord) -> std::io::Result<()> {
    let json = serde_json::to_string(record)?;
    db(ws)
        .and_then(|d| d.cell_put(id, &record.cell, &json))
        .map_err(std::io::Error::other)
}

/// 读全部 cell 记录；坏行跳过（坏一条不毁全部）。
pub fn read_partial(ws: &Path, id: &str) -> Vec<CellRecord> {
    db(ws)
        .ok()
        .and_then(|d| d.cell_all(id).ok())
        .map(|rows| rows.iter().filter_map(|j| serde_json::from_str(j).ok()).collect())
        .unwrap_or_default()
}

// ── args / result ─────────────────────────────────────────────────────────────

pub fn write_args_raw(ws: &Path, id: &str, json: &str) -> std::io::Result<()> {
    db(ws).and_then(|d| d.job_put_args(id, json)).map_err(std::io::Error::other)
}

pub fn read_args_raw(ws: &Path, id: &str) -> Option<String> {
    db(ws).ok().and_then(|d| d.job_get_args(id).ok().flatten())
}

/// 评审结果持久存储（json+md 同行；永不过期，评审史资产）。
pub fn write_result(ws: &Path, id: &str, json: &str) -> std::io::Result<()> {
    let d = db(ws).map_err(std::io::Error::other)?;
    let md = d.result_get(id).ok().flatten().map(|(_, m)| m).unwrap_or_default();
    d.result_put(id, json, &md).map_err(std::io::Error::other)
}

pub fn read_result(ws: &Path, id: &str) -> Option<String> {
    db(ws).ok().and_then(|d| d.result_get(id).ok().flatten()).map(|(j, _)| j).filter(|s| !s.is_empty())
}

pub fn write_result_md(ws: &Path, id: &str, md: &str) -> std::io::Result<()> {
    let d = db(ws).map_err(std::io::Error::other)?;
    let json = d.result_get(id).ok().flatten().map(|(j, _)| j).unwrap_or_default();
    d.result_put(id, &json, md).map_err(std::io::Error::other)
}

pub fn read_result_md(ws: &Path, id: &str) -> Option<String> {
    db(ws).ok().and_then(|d| d.result_get(id).ok().flatten()).map(|(_, m)| m).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(cell: &str, ok: bool) -> CellRecord {
        CellRecord { cell: cell.into(), model: "m".into(), ok, findings: vec![], warn: None,
            duration_ms: 0, input_tokens: 0, output_tokens: 0, attempts: 0 }
    }

    #[test]
    fn manifest_roundtrip_and_missing_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = vec![CellInput { cell: "c1".into(), model: "m".into(), prompt: "p".into() }];
        write_manifest(tmp.path(), "j1", &inputs).unwrap();
        let back = read_manifest(tmp.path(), "j1");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].cell, "c1");
        assert!(read_manifest(tmp.path(), "nope").is_empty());
    }

    #[test]
    fn partial_same_cell_overwrites_retry_wins() {
        let tmp = tempfile::tempdir().unwrap();
        append_partial(tmp.path(), "j1", &rec("c1", false)).unwrap();
        append_partial(tmp.path(), "j1", &rec("c1", true)).unwrap();
        append_partial(tmp.path(), "j1", &rec("c2", true)).unwrap();
        let all = read_partial(tmp.path(), "j1");
        assert_eq!(all.len(), 2, "同 cell 覆盖不追加");
        assert!(all.iter().find(|r| r.cell == "c1").unwrap().ok, "重试成功覆盖失败");
    }

    #[test]
    fn args_and_result_roundtrip_persistent() {
        let tmp = tempfile::tempdir().unwrap();
        write_args_raw(tmp.path(), "j1", r#"{"a":1}"#).unwrap();
        assert_eq!(read_args_raw(tmp.path(), "j1").unwrap(), r#"{"a":1}"#);
        write_result(tmp.path(), "j1", "{\"v\":1}").unwrap();
        write_result_md(tmp.path(), "j1", "# md").unwrap();
        // json 与 md 互不覆盖
        assert_eq!(read_result(tmp.path(), "j1").unwrap(), "{\"v\":1}");
        assert_eq!(read_result_md(tmp.path(), "j1").unwrap(), "# md");
        // 结果永不过期：重复读不消费
        assert!(read_result(tmp.path(), "j1").is_some());
        assert!(read_result(tmp.path(), "nope").is_none());
    }

    #[test]
    fn cell_record_old_json_defaults_runtime_fields() {
        // proj.db 里的旧记录没有运行学字段——default 零值兼容,不炸反序列化。
        let old = r#"{"cell":"c","model":"m","ok":true,"findings":[]}"#;
        let rec: CellRecord = serde_json::from_str(old).unwrap();
        assert_eq!(
            (rec.duration_ms, rec.input_tokens, rec.output_tokens, rec.attempts),
            (0, 0, 0, 0)
        );
    }
}
