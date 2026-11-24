//! job 生命周期：`JobDirCellSink`（成果保存）+ SIGTERM / 墙钟预算 → partial 报告（1.5）。
//!
//! 只写 `paths.out`，不触碰 `JobStore` / `proj.db`（P7）。`out/.cells.jsonl` 是过程存档（非终态信号）。

use crate::config::Config;
use crate::domain::{CellOutput, Finding, Usage};
use crate::job::manifest::JobManifest;
use crate::job::report::{partial_report, HibotReport, UsageOut};
use crate::job::status::{JobFailure, StatusText};
use crate::orchestrate::deep::known_verdicts;
use crate::orchestrate::CellSink;
use crate::state::{CellInput, CellRecord, Phase};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// job 模式的成果保存：内存 + 周期落 `out/.cells.jsonl`；SIGTERM / 超时据此组 partial。
pub struct JobDirCellSink {
    out_dir: PathBuf,
    cells: Mutex<Vec<CellRecord>>,
    phases: Mutex<Vec<(Phase, Option<String>)>>,
}

impl JobDirCellSink {
    pub fn new(out_dir: PathBuf) -> Self {
        Self { out_dir, cells: Mutex::new(Vec::new()), phases: Mutex::new(Vec::new()) }
    }

    pub fn snapshot(&self) -> (Vec<CellRecord>, Vec<(Phase, Option<String>)>) {
        (
            self.cells.lock().map(|v| v.clone()).unwrap_or_default(),
            self.phases.lock().map(|v| v.clone()).unwrap_or_default(),
        )
    }
}

impl CellSink for JobDirCellSink {
    fn manifest(&self, _inputs: &[CellInput]) {
        // Phase 1 不落输入快照（Phase 5 的 state 心跳与 resume 不在本单元）。
    }

    fn on_cell(&self, rec: CellRecord) {
        if let Ok(mut v) = self.cells.lock() {
            v.push(rec.clone());
        }
        // 追加写：失败只 stderr（过程存档，不影响终态报告）。
        let path = self.out_dir.join(".cells.jsonl");
        if let Ok(line) = serde_json::to_string(&rec) {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| writeln!(f, "{line}"));
        }
    }

    fn on_phase(&self, phase: Phase, note: Option<String>) {
        if let Ok(mut v) = self.phases.lock() {
            v.push((phase, note));
        }
    }
}

/// 从已落地的 cell 重建 partial findings：reviewer 格的 findings + refute 格的判决回填。
pub fn findings_from_cells(cells: &[CellRecord]) -> Vec<Finding> {
    let verdicts = known_verdicts(cells);
    let mut out = Vec::new();
    for c in cells.iter().filter(|c| c.ok) {
        if let CellOutput::Review { findings, .. } = &c.output {
            for f in findings {
                let mut f = f.clone();
                if let Some(v) = verdicts.get(&f.id) {
                    f.set_refute(v.clone());
                }
                out.push(f);
            }
        }
    }
    out
}

/// cell 记录的 token 汇总（partial 报告的 usage）。
pub fn usage_from_cells(cells: &[CellRecord]) -> UsageOut {
    let mut u = Usage { input_tokens: 0, output_tokens: 0, cached_input_tokens: 0, refute: None };
    let mut wall = 0u64;
    for c in cells {
        u.input_tokens += c.input_tokens;
        u.output_tokens += c.output_tokens;
        u.cached_input_tokens += c.cached_input_tokens;
        wall = wall.max(c.duration_ms);
    }
    UsageOut {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cached_input_tokens: u.cached_input_tokens,
        wall_clock_ms: wall,
    }
}

/// SIGTERM / 墙钟耗尽：停止派发后写 partial（调用方已 drop 运行 future）。
pub fn interrupted_report(
    manifest: &JobManifest,
    lang: &crate::config::OutputLang,
    cfg: &Config,
    sink: &JobDirCellSink,
) -> HibotReport {
    let (cells, _phases) = sink.snapshot();
    let findings = findings_from_cells(&cells);
    let usage = usage_from_cells(&cells);
    let failure = JobFailure::new("wall_clock_exhausted", None);
    let text: StatusText = failure.status_text(lang);
    partial_report(manifest, lang, &text, findings, usage, cfg, 0)
}

/// 正常收口后的内部报告是否要落到 `out/` 之外？不：Phase 1 只写 `out/report.json`。
pub fn out_dir_of(job_dir: &Path) -> PathBuf {
    job_dir.join("out")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{Refute, RefuteVerdict, Role, Severity};

    fn finding(id: &str) -> Finding {
        let mut f = crate::domain::finding::fixtures::finding(id, Severity::High, 1, "m");
        f.id = id.into();
        f
    }

    #[test]
    fn sink_saves_cells_and_partial_findings_merge_refute_verdicts() {
        let d = tempfile::tempdir().unwrap();
        let sink = JobDirCellSink::new(d.path().to_path_buf());
        let review = CellRecord::ok("review[x]@p/r", "p/r", CellOutput::Review { findings: vec![finding("f1")], needs: vec![] });
        let refute = CellRecord::ok("refute[f1]@p/v", "p/v", CellOutput::Refute {
            verdicts: vec![crate::domain::RefuteEntry { id: "f1".into(), verdict: RefuteVerdict::Refuted, note: "no guard".into() }],
        });
        sink.on_cell(review);
        sink.on_cell(refute);
        let (cells, _) = sink.snapshot();
        assert_eq!(cells.len(), 2);
        let fs = findings_from_cells(&cells);
        assert_eq!(fs.len(), 1);
        assert!(fs[0].is_refuted(), "refute 格判决要回填到 partial finding");
        assert!(d.path().join(".cells.jsonl").exists(), "过程存档要落盘");
        let _ = (Refute { verdict: RefuteVerdict::Upheld, model: "x".into(), note: String::new() }, Role::Reviewer);
    }

    #[test]
    fn usage_sums_cells_and_max_wall() {
        let mut a = CellRecord::ok("a", "m", CellOutput::Scan(Default::default()));
        a.input_tokens = 10; a.output_tokens = 2; a.cached_input_tokens = 1; a.duration_ms = 100;
        let mut b = CellRecord::failed("b", "m", Role::Scanner, "x");
        b.input_tokens = 5; b.output_tokens = 1; b.duration_ms = 300;
        let u = usage_from_cells(&[a, b]);
        assert_eq!((u.input_tokens, u.output_tokens, u.cached_input_tokens, u.wall_clock_ms), (15, 3, 1, 300));
    }

    #[test]
    fn interrupted_report_is_partial_with_reason() {
        let d = tempfile::tempdir().unwrap();
        let sink = JobDirCellSink::new(d.path().to_path_buf());
        let manifest = JobManifest::parse(r#"{
          "schema":"hibot.audit.job.review/1","job_id":"J1","kind":"review",
          "repo":{"id":1,"full_name":"o/r"},
          "pr":{"number":7,"head_sha":"abcdef0","base_sha":"1234567"},
          "executor":{"name":"redteam","version":"0.1.0"},
          "limits":{"wall_clock_s":600},
          "paths":{"repo":"../repo","out":"out","config":"config/executor.redteam.json"},
          "inputs":{}
        }"#).unwrap();
        let lang = crate::config::Language::EnUs.output();
        let r = interrupted_report(&manifest, &lang, &Config::default(), &sink);
        assert_eq!(r.status, crate::job::status::JobStatus::Partial);
        assert!(r.status_reason.unwrap().contains("wall-clock budget"));
        assert_eq!(r.extensions["redteam"]["status_code"], "wall_clock_exhausted");
    }
}
