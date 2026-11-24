//! job 生命周期：`JobDirCellSink`（成果保存）+ SIGTERM / 墙钟预算 → partial 报告（1.5）。
//!
//! 只写 `paths.out`，不触碰 `JobStore` / `proj.db`（P7）。`out/.cells.jsonl` 是过程存档（非终态信号）。

use crate::config::Config;
use crate::domain::{CellOutput, Finding, Usage};
use crate::job::manifest::JobManifest;
use crate::job::report::{partial_report, HibotReport, UsageOut};
use crate::job::state::Heartbeat;
use crate::job::status::{JobFailure, StatusText};
use crate::orchestrate::deep::known_verdicts;
use crate::orchestrate::CellSink;
use crate::state::{CellInput, CellRecord, Phase};
use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// job 模式的成果保存：内存 + 周期落 `out/.cells.jsonl`；SIGTERM / 超时据此组 partial。
pub struct JobDirCellSink {
    out_dir: PathBuf,
    cells: Mutex<Vec<CellRecord>>,
    phases: Mutex<Vec<(Phase, Option<String>)>>,
    /// Phase 5：心跳（progress 的来源；`on_phase` 明确不挂——内部 `Phase` 枚举不是 job 三值）。
    heartbeat: Option<Arc<Heartbeat>>,
    /// `manifest` 报过的 cell id：`progress.done` 只计这些（深挖 cell 不在集合内，不得推高 scan 进度）。
    manifest_cells: Mutex<HashSet<String>>,
}

impl JobDirCellSink {
    pub fn new(out_dir: PathBuf) -> Self {
        Self { out_dir, cells: Mutex::new(Vec::new()), phases: Mutex::new(Vec::new()), heartbeat: None, manifest_cells: Mutex::new(HashSet::new()) }
    }

    /// 带心跳的 sink：`manifest` 设 progress.total（scan cells），`on_cell` 推进 done。
    pub fn with_heartbeat(out_dir: PathBuf, heartbeat: Arc<Heartbeat>) -> Self {
        Self {
            out_dir,
            cells: Mutex::new(Vec::new()),
            phases: Mutex::new(Vec::new()),
            heartbeat: Some(heartbeat),
            manifest_cells: Mutex::new(HashSet::new()),
        }
    }

    pub fn snapshot(&self) -> (Vec<CellRecord>, Vec<(Phase, Option<String>)>) {
        (
            self.cells.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            self.phases.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        )
    }
}

impl CellSink for JobDirCellSink {
    fn manifest(&self, inputs: &[CellInput]) {
        // Phase 1 不落输入快照；Phase 5 只把 scan cells 报给心跳（progress 不参与判定）。
        // 同时记下 cell id 集合：`on_cell` 只对这些 id 推进 done（深挖 cell 是另一个集合）。
        let ids: HashSet<String> = inputs.iter().map(|i| i.cell.clone()).collect();
        *self.manifest_cells.lock().unwrap_or_else(|e| e.into_inner()) = ids;
        if let Some(hb) = &self.heartbeat {
            hb.set_progress_total("cells", inputs.len() as u64);
        }
    }

    fn on_cell(&self, rec: CellRecord) {
        if let Some(hb) = &self.heartbeat {
            // done 与 total 必须对应同一集合（scan cells）；reviewer/refuter cell 不计。
            let in_manifest = self
                .manifest_cells
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .contains(&rec.cell);
            if in_manifest {
                hb.bump_done();
            }
        }
        // 锁中毒（另一个线程 panic）时取回内部数据继续记：静默丢弃会让 partial 少掉已完成成果。
        let mut v = self.cells.lock().unwrap_or_else(|e| e.into_inner());
        v.push(rec.clone());
        drop(v);
        // 追加写：失败要可见（过程存档失败同样意味着 partial 可能缺成果）。
        let path = self.out_dir.join(".cells.jsonl");
        match serde_json::to_string(&rec) {
            Ok(line) => {
                let res = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .and_then(|mut f| writeln!(f, "{line}"));
                if let Err(e) = res {
                    eprintln!("warn: 写 {} 失败: {e}", path.display());
                }
            }
            Err(e) => eprintln!("warn: cell 记录序列化失败: {e}"),
        }
    }

    fn on_phase(&self, phase: Phase, note: Option<String>) {
        self.phases.lock().unwrap_or_else(|e| e.into_inner()).push((phase, note));
    }
}

/// 从已落地的 cell 重建 partial findings：reviewer 格的 findings + refute 格的判决回填。
pub fn findings_from_cells(cells: &[CellRecord]) -> Vec<Finding> {
    let verdicts = known_verdicts(cells);
    let mut out: Vec<Finding> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for c in cells.iter().filter(|c| c.ok) {
        if let CellOutput::Review { findings, .. } = &c.output {
            for f in findings {
                // 同一 finding 可能由多个格报出（不同队列项）：partial 也按 id 去重，避免重复计数。
                if !seen.insert(f.id.clone()) {
                    continue;
                }
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
///
/// `root` 给了就跑与正常路径同一条收口链（2.5：逐字重锚 → 指纹 → dismissed），
/// 否则只说「已落地的 cell」——准备阶段就中断时还没有 findings。
pub fn interrupted_report(
    manifest: &JobManifest,
    lang: &crate::config::OutputLang,
    cfg: &Config,
    sink: &JobDirCellSink,
    root: Option<&std::path::Path>,
    dismissed: &crate::job::dismissed::Dismissed,
    base_extras: &crate::job::report::ReviewExtras,
) -> HibotReport {
    let (cells, _phases) = sink.snapshot();
    let findings = findings_from_cells(&cells);
    let upstream = crate::job::quote::upstream_dropped(&cells);
    let (mut findings, self_dropped) = match root {
        Some(root) => {
            let v = crate::job::quote::verify(findings, root);
            (v.findings, v.dropped)
        }
        None => (findings, 0),
    };
    // 2.4：与正常路径同一条链——指纹 → dismissed（只标审计，不写 refuted）。
    let ledger = crate::job::dismissed::apply(&mut findings, dismissed);
    let dropped = self_dropped + upstream;
    let usage = usage_from_cells(&cells);
    let failure = JobFailure::new("wall_clock_exhausted", None);
    let text: StatusText = failure.status_text(lang);
    // 与正常路径共享本次运行事实（input_consistency / prior_routing / refute_skipped / 已有 partial 原因）。
    let mut extras = base_extras.clone();
    extras.dropped_unverified_quotes = dropped;
    extras.ledger = ledger;
    if dropped > 0 {
        extras.extra_partial.push(JobFailure::new("quotes_dropped", Some(dropped.to_string())));
    }
    partial_report(manifest, lang, &text, findings, usage, cfg, 0, &extras)
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

    fn manifest() -> JobManifest {
        JobManifest::parse(r#"{
          "schema":"hibot.audit.job.review/1","job_id":"J1","kind":"review",
          "repo":{"id":1,"full_name":"o/r"},
          "pr":{"number":7,"head_sha":"abcdef0","base_sha":"1234567"},
          "executor":{"name":"redteam","version":"0.1.0"},
          "limits":{"wall_clock_s":600},
          "paths":{"repo":"../repo","out":"out","config":"config/executor.redteam.json"},
          "inputs":{}
        }"#).unwrap()
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
    fn duplicate_findings_across_cells_are_deduped_by_id() {
        let d = tempfile::tempdir().unwrap();
        let sink = JobDirCellSink::new(d.path().to_path_buf());
        let a = CellRecord::ok("review[a]@p/r", "p/r", CellOutput::Review { findings: vec![finding("f1")], needs: vec![] });
        let mut f2 = finding("f1");
        f2.refute = None;
        let b = CellRecord::ok("review[b]@p/r", "p/r", CellOutput::Review { findings: vec![f2], needs: vec![] });
        sink.on_cell(a);
        sink.on_cell(b);
        let (cells, _) = sink.snapshot();
        assert_eq!(findings_from_cells(&cells).len(), 1, "同一 finding id 只保留一条");
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
        let r = interrupted_report(
            &manifest,
            &lang,
            &Config::default(),
            &sink,
            None,
            &crate::job::dismissed::Dismissed::default(),
            &crate::job::report::ReviewExtras::default(),
        );
        assert_eq!(r.status, crate::job::status::JobStatus::Partial);
        assert!(r.status_reason.unwrap().contains("wall-clock budget"));
        assert_eq!(r.extensions["redteam"]["status_code"], "wall_clock_exhausted");
    }

    /// 2.5/2.4 + 2.3：中断路径与正常路径走同一条收口链，并保留准备阶段拿到的运行事实。
    #[test]
    fn interrupted_report_shares_the_finalize_chain_and_run_facts() {
        let d = tempfile::tempdir().unwrap();
        let repo = d.path().join("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/a.rs"), "fn f() {\n    let x  =  1;\n}\n").unwrap();
        let sink = JobDirCellSink::new(d.path().join("out"));

        let mut f = finding("f1");
        f.file = "src/a.rs".into();
        f.quote = "let x = 1;".into(); // 与检出空白不同：必须被检出原文纠正
        f.title = "t".into();
        f.line_start = 99;
        sink.on_cell(CellRecord::ok("review[x]", "p/r", CellOutput::Review { findings: vec![f], needs: vec![] }));
        // 复核判 REFUTED：由 `findings_from_cells` 回填到 finding 上。
        sink.on_cell(CellRecord::ok(
            "refute[f1]",
            "p/v",
            CellOutput::Refute {
                verdicts: vec![crate::domain::RefuteEntry {
                    id: "f1".into(),
                    verdict: RefuteVerdict::Refuted,
                    note: "safe".into(),
                }],
            },
        ));

        // dismissed 按**修正后**的 quote 生成的指纹命中。
        let fp = crate::job::fingerprint::of("src/a.rs", "let x  =  1;", "t");
        let csv = format!("fingerprint,actor,reason,created_at\n{fp},alice,false positive,2026-11-20T10:00:00Z\n");
        let dismissed = crate::job::dismissed::parse(csv.as_bytes()).unwrap();
        let base_extras = crate::job::report::ReviewExtras {
            input_consistency: Some(serde_json::json!({"match": true, "files": 1})),
            prior_routing: Some(serde_json::json!({"present": true})),
            refute_skipped: Some("no_refuter"),
            ..Default::default()
        };
        let lang = crate::config::Language::EnUs.output();
        let r = interrupted_report(
            &manifest(),
            &lang,
            &Config::default(),
            &sink,
            Some(&repo),
            &dismissed,
            &base_extras,
        );

        assert_eq!(r.status, crate::job::status::JobStatus::Partial);
        assert_eq!(r.findings.len(), 1);
        assert_eq!(r.findings[0].quote, "let x  =  1;", "中断报告也要用检出原文修正 quote");
        assert_eq!((r.findings[0].line_start, r.findings[0].line_end), (2, 2), "行号要重锚");
        // 人的驳回优先：即使复核判了 REFUTED，对外仍是 refuted=false，且不叠 refute 扩展。
        assert_eq!(r.findings[0].refuted, false);
        assert!(r.findings[0].extensions["redteam"].get("refute").is_none());
        assert_eq!(r.findings[0].extensions["redteam"]["ledger"]["dismissed"], true);
        assert_eq!(r.extensions["redteam"]["input_consistency"]["match"], true);
        assert_eq!(r.extensions["redteam"]["prior_routing"]["present"], true);
        assert_eq!(r.extensions["redteam"]["refute_skipped"], "no_refuter");
    }

    /// Phase 5：progress.done 只计 manifest 报过的 scan cell；深挖 cell（reviewer/refuter）不得推高进度。
    #[tokio::test]
    async fn progress_counts_only_manifest_cells() {
        let d = tempfile::tempdir().unwrap();
        let hb = crate::job::state::Heartbeat::start(d.path(), "J-progress");
        let sink = JobDirCellSink::with_heartbeat(d.path().to_path_buf(), Arc::clone(&hb));
        let cell = |id: &str| CellInput {
            cell: id.into(),
            model: "p/s".into(),
            prompt: String::new(),
            scan: None,
            template_hash: None,
        };
        sink.manifest(&[cell("g0@p/s#1"), cell("g1@p/s#1")]);
        // 深挖先完成（不在 manifest 集合）：done 不动。
        sink.on_cell(CellRecord::ok(
            "review[f1]@p/r",
            "p/r",
            CellOutput::Review { findings: vec![], needs: vec![] },
        ));
        hb.attach_llm(std::collections::BTreeMap::new());
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(d.path().join("state.json")).unwrap()).unwrap();
        assert_eq!(v["progress"], serde_json::json!({"unit": "cells", "done": 0, "total": 2}));
        // scan cell 完成：done 前进。
        sink.on_cell(CellRecord::ok("g0@p/s#1", "p/s", CellOutput::Scan(Default::default())));
        hb.attach_llm(std::collections::BTreeMap::new());
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(d.path().join("state.json")).unwrap()).unwrap();
        assert_eq!(v["progress"]["done"], 1);
        hb.shutdown();
    }

}
