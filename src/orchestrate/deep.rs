//! 深挖循环（M21b 批 C-4）：H0 + 疑点入队 → drain → lease×pop → review_shot → barrier。
//! 无 reviewer / scanner 失败时调用方不进本模块（scan-only / Incomplete）。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::bundle::{cross_index, Bundle};
use crate::config::{EffectiveModel, PackCfg};
use crate::consolidate::scan::merge_suspicions;
use crate::domain::{DeepMeta, Finding, Role};
use crate::orchestrate::dispatch::dispatch_one;
use crate::orchestrate::queue::{DeepQueue, EnqueueOutcome, EnqueueReq, ItemKind};
use crate::orchestrate::review_shot::{run_review_shot_with, FindingSink};
use crate::pack::region::{self as region, Origin as QueueOrigin, QueueItemSpec};
use crate::pack::PackOpts;
use crate::prompt::{build_review_prompt, review_template_hash, ReviewPromptInput};
use crate::provider::registry::ProviderRegistry;
use crate::provider::stream_json::IncrementalFindings;
use crate::risk::{Hotspot, RegionKind, RiskRoute};
use crate::state::CellRecord;

use super::ScanCellResult;

pub struct DeepRun {
    pub findings: Vec<Finding>,
    pub cells: Vec<CellRecord>,
    pub meta: DeepMeta,
    pub warnings: Vec<String>,
    /// reviewer cell 重试耗尽。
    pub reviewer_failed: bool,
}

pub struct DeepInput<'a> {
    pub route: Option<&'a RiskRoute>,
    pub scan_cells: &'a [ScanCellResult],
    pub bundles: &'a [Bundle],
    pub root: &'a Path,
    pub reviewers: &'a [EffectiveModel],
    pub budget_cells: u32,
    pub pack: &'a PackCfg,
    pub spec_docs: &'a str,
    pub target_label: &'a str,
    /// 状态转换时写 queue_items；失败由回调 warning，不败本轮。
    pub snapshot: Option<&'a dyn Fn(&crate::orchestrate::queue::QueueItem)>,
}

pub async fn run_deep(registry: &ProviderRegistry, input: DeepInput<'_>, sink: &dyn FindingSink) -> DeepRun {
    let mut q = DeepQueue::new();
    let mut meta = DeepMeta::default();
    let mut warnings = Vec::new();

    if input.bundles.is_empty() {
        return DeepRun { findings: vec![], cells: vec![], meta, warnings, reviewer_failed: false };
    }
    if let Some(route) = input.route {
        for h in &route.hotspots {
            enqueue_h0(&mut q, h, &mut meta);
        }
    }
    enqueue_scan(&mut q, input.scan_cells, &mut meta);
    snap_all(&q, input.snapshot);

    let skipped = q.drain(input.budget_cells);
    snap_all(&q, input.snapshot);
    meta.budget_skipped = skipped.len() as u32;
    meta.queue_depth_peak = q.items().len() as u32;

    let Some(rev) = input.reviewers.first() else {
        return DeepRun { findings: vec![], cells: vec![], meta, warnings, reviewer_failed: false };
    };
    let idx = cross_index(input.bundles, &[], None);
    let opts = PackOpts::from(input.pack);
    let mut findings = Vec::new();
    let mut cells = Vec::new();
    let mut n = 0u32;
    let mut reviewer_failed = false;

    while q.pending_count() > 0 {
        let Some((lease, item)) = (match dispatch_one(&mut q, registry, &rev.name).await {
            Ok(x) => x,
            Err(e) => {
                warnings.push(format!("reviewer lease failed: {e}"));
                reviewer_failed = true;
                break;
            }
        }) else {
            break;
        };
        snap_one(&item, input.snapshot);
        n += 1;
        let cell_id = format!("review[{n}]@{}", rev.name);
        let t0 = Instant::now();
        let pack = match region::build(&item.spec, pick_bundle(input.bundles, &item.spec.file), input.root, &idx, 64 * 1024, &opts, "") {
            Ok(p) => p,
            Err(e) => {
                warnings.push(format!("{cell_id}: pack {e}"));
                q.mark_settled(&item.id, item.agreement);
                drop(lease);
                continue;
            }
        };
        let file_diff = file_diff(input.bundles, &item.spec.file);
        let prompt = build_review_prompt(&ReviewPromptInput {
            target_label: input.target_label,
            file: &item.spec.file,
            file_diff: &file_diff,
            cross_index: &pack.cross_index,
            notes: &pack.notes,
            dossier: &pack.evidence,
            spec_docs: input.spec_docs,
            repo_brief: "",
        });
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let policy = lease.gate_attempts();
        let call = run_review_shot_with(
            lease.backend(),
            &prompt,
            lease.per_turn(),
            lease.retry_budget(),
            Some(&policy),
            &rev.name,
            &rev.family,
            item.agreement,
            input.root,
            inc,
            sink,
        )
        .await;
        drop(lease);
        match call {
            Ok(c) => {
                let got = c.output.findings().to_vec();
                findings.extend(got.iter().cloned());
                let mut rec = CellRecord::ok(&cell_id, &rev.name, c.output).with_family(&rev.family);
                rec.duration_ms = t0.elapsed().as_millis() as u64;
                rec.input_tokens = c.input_tokens;
                rec.output_tokens = c.output_tokens;
                rec.cached_input_tokens = c.cached_input_tokens;
                rec.iterations = c.iterations;
                rec.attempts = 1 + c.rate_limit_retries;
                rec.queue_wait_ms = c.rate_limit_wait_ms;
                rec.template_hash = Some(review_template_hash());
                if c.stats.stream_discarded > 0 {
                    rec.warn = Some(format!("stream discarded {}", c.stats.stream_discarded));
                }
                meta.stream_discarded += c.stats.stream_discarded;
                meta.reviewer_output_tokens.push(c.output_tokens);
                meta.dispatch_agreement.push(item.dispatch_agreement);
                meta.landed_agreement.push(item.agreement);
                cells.push(rec);
                q.mark_settled(&item.id, item.agreement);
                if let Some(it) = q.get(&item.id) { snap_one(it, input.snapshot); }
            }
            Err(e) => {
                reviewer_failed = true;
                let rec = CellRecord::failed(&cell_id, &rev.name, Role::Reviewer, format!("{e}"));
                cells.push(rec);
                warnings.push(format!("{cell_id} failed: {e}"));
                q.mark_settled(&item.id, item.agreement);
                if let Some(it) = q.get(&item.id) { snap_one(it, input.snapshot); }
            }
        }
    }

    DeepRun {
        findings: crate::consolidate::dedup(findings),
        cells,
        meta,
        warnings,
        reviewer_failed,
    }
}

fn pick_bundle<'a>(bundles: &'a [Bundle], file: &str) -> &'a Bundle {
    bundles.iter().find(|b| b.files.iter().any(|f| f.path == file)).unwrap_or(&bundles[0])
}

fn file_diff(bundles: &[Bundle], file: &str) -> String {
    bundles.iter().flat_map(|b| b.files.iter()).find(|f| f.path == file).map(|f| f.diff.clone()).unwrap_or_default()
}

fn enqueue_h0(q: &mut DeepQueue, h: &Hotspot, meta: &mut DeepMeta) {
    let start = (h.start as u32).max(1);
    let end = (h.end as u32).max(start);
    let Ok(spec) = QueueItemSpec::new(h.file.clone(), vec![(start, end)], QueueOrigin::Rule, vec![]) else {
        return;
    };
    let kind = match h.kind {
        RegionKind::Primary => ItemKind::Primary,
        RegionKind::Ripple => ItemKind::Ripple,
    };
    note_enq(q.enqueue(EnqueueReq { spec, score: h.score, kind, family: None }), "rule", meta);
}

fn enqueue_scan(q: &mut DeepQueue, cells: &[ScanCellResult], meta: &mut DeepMeta) {
    for m in merge_suspicions(cells) {
        let s = &m.suspicion;
        let Ok(spec) = QueueItemSpec::new(
            s.file.clone(),
            vec![(s.line_start.max(1), s.line_end.max(s.line_start.max(1)))],
            QueueOrigin::Scan,
            vec![s.clone()],
        ) else {
            continue;
        };
        let family = m.families.first().cloned();
        let quote = s.quote.clone();
        let file = s.file.clone();
        note_enq(
            q.enqueue(EnqueueReq { spec, score: s.confidence, kind: ItemKind::Suspicion, family }),
            "scan",
            meta,
        );
        if let Some(it) = q.items_mut().iter_mut().find(|it| {
            it.kind == ItemKind::Suspicion && it.spec.file == file && it.spec.notes.iter().any(|n| n.quote == quote)
        }) {
            it.agreement = m.agreement.max(1);
            it.families = m.families.clone();
        }
    }
}

fn snap_all(q: &DeepQueue, snap: Option<&dyn Fn(&crate::orchestrate::queue::QueueItem)>) {
    if let Some(f) = snap {
        for it in q.items() {
            f(it);
        }
    }
}

fn snap_one(it: &crate::orchestrate::queue::QueueItem, snap: Option<&dyn Fn(&crate::orchestrate::queue::QueueItem)>) {
    if let Some(f) = snap {
        f(it);
    }
}

fn note_enq(o: EnqueueOutcome, reason: &str, meta: &mut DeepMeta) {
    match o {
        EnqueueOutcome::New => {
            *meta.enqueue_reasons.entry(reason.into()).or_insert(0) += 1;
        }
        EnqueueOutcome::MergedPending | EnqueueOutcome::MergedInFlight | EnqueueOutcome::MergedSettled => {
            meta.dedup_hits += 1;
        }
    }
}
