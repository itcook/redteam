//! 深挖循环（M21b 批 C-4）：H0 + 疑点入队 → drain → lease×pop → review_shot → barrier。
//! 无 reviewer / scanner 失败时调用方不进本模块（scan-only / Incomplete）。

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

use crate::bundle::{cross_index, Bundle};
use crate::config::{EffectiveModel, PackCfg, ToolsMode};
use crate::consolidate::scan::merge_suspicions;
use crate::domain::{DeepMeta, Finding, Role};
use crate::orchestrate::dispatch::dispatch_one;
use crate::orchestrate::queue::{DeepQueue, EnqueueOutcome, EnqueueReq, ItemKind, ItemState};
use crate::orchestrate::review_shot::{run_review_shot_with, EmitCtx, FindingSink};
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
    /// resume 恢复（C-5）：快照项（跳过入队与 drain，未完项重跑）；settled 项的已有 finding 经此并入（不双计）。
    pub restore: Option<&'a [crate::orchestrate::queue::QueueItem]>,
    pub prior_findings: Vec<Finding>,
    /// `--tools` 档位（R2 二阶段首个消费者）：TwoStage 时 reviewer needs 触发补查轮。
    pub tools: ToolsMode,
}

pub async fn run_deep(registry: &ProviderRegistry, input: DeepInput<'_>, sink: &dyn FindingSink) -> DeepRun {
    let mut meta = DeepMeta::default();
    let mut warnings = Vec::new();
    let prior_findings = input.prior_findings.clone();

    // 空变更集 → 无 bundle 可 pack，直接早退（`pick_bundle` 以 `bundles[0]` 兜底，空片会越界）。
    // resume 侧同样经此：重建 diff 落空（改动已提交 / 已切分支）而快照仍有未完项时，
    // 披露成带债而不是崩在 pack 上。
    if input.bundles.is_empty() {
        let unfinished = input.restore.is_some_and(|items| {
            items.iter().any(|i| matches!(i.state, ItemState::Pending | ItemState::InFlight))
        });
        if unfinished {
            warnings.push(
                "deep: 变更集为空（diff 已不可重建），队列未完项无法重跑——带债收口".into(),
            );
        }
        return DeepRun { findings: prior_findings, cells: vec![], meta, warnings, reviewer_failed: unfinished };
    }

    let mut q = match input.restore {
        Some(items) => DeepQueue::restore(items.to_vec()),
        None => DeepQueue::new(),
    };

    if input.restore.is_some() {
        meta.budget_skipped = q.items().iter().filter(|i| i.state == ItemState::Skipped).count() as u32;
        meta.queue_depth_peak = q.items().len() as u32;
    } else {
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
    }

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
        let mut ctx = EmitCtx::new(&rev.name, &rev.family, item.agreement, input.root, sink);
        let call = run_review_shot_with(
            lease.backend(),
            &prompt,
            lease.per_turn(),
            lease.retry_budget(),
            Some(&policy),
            &mut ctx,
            inc,
        )
        .await;
        drop(lease);
        match call {
            Ok(mut c) => {
                // R2 二阶段（`--tools two-stage` 首个消费者，design §11）：needs 非空 → 补查轮。
                // 注入互斥：带笔记项（origin=scan）不注入 needs 证据（region::build 构造期 Err），计 skipped。
                let needs_count = c.output.needs().len();
                if needs_count > 0 {
                    let eligible = input.tools == ToolsMode::TwoStage && item.spec.notes.is_empty();
                    if eligible {
                        meta.needs_two_stage_triggered += 1;
                        if let Some(c2) = second_stage_round(
                            registry, &input, rev, &item, &idx, &opts, c.output.findings().to_vec(), c.output.needs(), sink,
                        )
                        .await
                        {
                            c.input_tokens += c2.input_tokens;
                            c.output_tokens += c2.output_tokens;
                            c.cached_input_tokens += c2.cached_input_tokens;
                            c.iterations += c2.iterations;
                            c.rate_limit_retries += c2.rate_limit_retries;
                            c.rate_limit_wait_ms += c2.rate_limit_wait_ms;
                            c.stats.stream_discarded += c2.stats.stream_discarded;
                            c.output = c2.output; // 第二轮 = CONFIRM/REVISE 后终判
                        }
                    } else {
                        meta.needs_two_stage_skipped += 1;
                    }
                }
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
                if let Some(en) = item.enqueued_ms {
                    meta.item_wait_ms.push(now_ms().saturating_sub(en));
                }
                // 落地时 agreement 读队列活值（在途期间并入的异族会抬高它）——与派发时值成对，
                // 二者之差即「挖的时候还不知道、挖完才攒够的共识」，观测面按此判读。
                let landed = live_agreement(&q, &item);
                meta.landed_agreement.push(landed);
                cells.push(rec);
                q.mark_settled(&item.id, landed);
                if let Some(it) = q.get(&item.id) { snap_one(it, input.snapshot); }
            }
            Err(e) => {
                reviewer_failed = true;
                let rec = CellRecord::failed(&cell_id, &rev.name, Role::Reviewer, format!("{e}"));
                cells.push(rec);
                warnings.push(format!("{cell_id} failed: {e}"));
                let landed = live_agreement(&q, &item);
                q.mark_settled(&item.id, landed);
                if let Some(it) = q.get(&item.id) { snap_one(it, input.snapshot); }
            }
        }
    }

    findings.extend(prior_findings);
    DeepRun {
        findings: crate::consolidate::dedup(findings),
        cells,
        meta,
        warnings,
        reviewer_failed,
    }
}

/// 调用方保证 `bundles` 非空（`run_deep` 开头已早退）。
fn pick_bundle<'a>(bundles: &'a [Bundle], file: &str) -> &'a Bundle {
    bundles.iter().find(|b| b.files.iter().any(|f| f.path == file)).unwrap_or(&bundles[0])
}

/// 落地时 agreement：按 id 读队列活值。查不到（在途期间并入改写过 id）退回派发时值。
fn live_agreement(q: &DeepQueue, item: &crate::orchestrate::queue::QueueItem) -> u32 {
    q.get(&item.id).map(|it| it.agreement).unwrap_or(item.agreement)
}

/// R2 二阶段补查轮：needs → 证据段（`second_pass::need_section`，读守卫 + 字节帽）→ 重建 pack
/// （needs_evidence 槽；与笔记互斥由构造期守卫，调用方已过滤）→ prompt 附加草稿块 → 新租约再跑一轮。
#[allow(clippy::too_many_arguments)] // 参数面各异（registry / 输入 / 角色 / 队列项 / 索引 / 草稿 / needs / sink）；收拢 struct 是过度设计
async fn second_stage_round(
    registry: &ProviderRegistry,
    input: &DeepInput<'_>,
    rev: &EffectiveModel,
    item: &crate::orchestrate::queue::QueueItem,
    idx: &crate::bundle::CrossIndex,
    opts: &PackOpts,
    draft: Vec<Finding>,
    needs: &[crate::domain::NeedEntry],
    sink: &dyn FindingSink,
) -> Option<super::review_shot::ReviewCall> {
    let ex = crate::tools::ToolExecutor::new(input.root);
    let mut w2: Vec<String> = Vec::new();
    let ev: String = needs
        .iter()
        .filter_map(|n| crate::cli::second_pass::need_section(&ex, input.root, n, &mut w2))
        .collect();
    if ev.trim().is_empty() {
        return None;
    }
    let pack2 = region::build(
        &item.spec,
        pick_bundle(input.bundles, &item.spec.file),
        input.root,
        idx,
        64 * 1024,
        opts,
        &ev,
    )
    .ok()?;
    let file_diff = file_diff(input.bundles, &item.spec.file);
    let mut prompt2 = build_review_prompt(&ReviewPromptInput {
        target_label: input.target_label,
        file: &item.spec.file,
        file_diff: &file_diff,
        cross_index: &pack2.cross_index,
        notes: &pack2.notes,
        dossier: &pack2.evidence,
        spec_docs: input.spec_docs,
        repo_brief: "",
    });
    prompt2.blocks.push(crate::prompt::PromptBlock {
        text: crate::cli::second_pass::draft_block(&draft),
        cache_boundary: false,
    });
    let lease = registry.lease(&rev.name).await.ok()?;
    let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
    let policy = lease.gate_attempts();
    let mut ctx = EmitCtx::new(&rev.name, &rev.family, item.agreement, input.root, sink);
    let r = run_review_shot_with(
        lease.backend(),
        &prompt2,
        lease.per_turn(),
        lease.retry_budget(),
        Some(&policy),
        &mut ctx,
        inc,
    )
    .await;
    drop(lease);
    r.ok()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::finding::fixtures;
    use crate::evidence::git::FileChange;
    use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend};
    use std::time::Duration;

    fn rev_json(quote: &str) -> String {
        format!(
            r#"{{"findings":[{{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"{quote}"}}],"needs":[]}}"#
        )
    }

    fn turn(text: &str) -> AssistantTurn {
        AssistantTurn { text: text.into(), tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0, finish_reason: Some("stop".into()), reasoning_tokens: None }
    }

    fn snap_item(file: &str, state: ItemState) -> crate::orchestrate::queue::QueueItem {
        let spec = QueueItemSpec::new(file.into(), vec![(1, 2)], QueueOrigin::Rule, vec![]).unwrap();
        crate::orchestrate::queue::QueueItem {
            id: format!("{file}:rule:1-2"),
            spec, score: 1.0, agreement: 1, state, kind: ItemKind::Primary,
            dispatch_agreement: 0, landed_agreement: None, families: vec![], enqueued_ms: None,
        }
    }

    fn fake_reviewer() -> crate::config::EffectiveModel {
        crate::config::EffectiveModel {
            name: "p/r".into(), display_name: None, id: "r".into(), provider: "p".into(),
            role: crate::config::Role::Reviewer, kind: crate::config::ProviderKind::Openai,
            base_url: None, context_window: 100_000, max_output_tokens: None,
            per_turn_timeout_secs: None, reasoning: false, thinking_level: None,
            family: "r".into(), max_concurrency: None,
        }
    }

    /// resume 且重建 diff 落空（改动已提交 / 已切分支）：未完项无 bundle 可 pack ——
    /// 早退带债披露，不崩在 `pick_bundle` 的空片索引上；prior findings 原样带出。
    #[tokio::test]
    async fn restore_with_empty_bundles_is_debt_not_panic() {
        let dir = tempfile::tempdir().unwrap();
        let mut prior_f = fixtures::finding("f1", crate::domain::Severity::Low, 1, "p/r");
        prior_f.quote = "fn c() {".into();
        let reviewer = fake_reviewer();
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![turn(&rev_json("fn c() {"))])), Duration::from_secs(5), 0);
        let run = run_deep(
            &reg,
            DeepInput {
                route: None, scan_cells: &[], bundles: &[], root: dir.path(),
                reviewers: std::slice::from_ref(&reviewer), budget_cells: 4,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: None, restore: Some(&[snap_item("c.rs", ItemState::Pending)]),
                prior_findings: vec![prior_f], tools: ToolsMode::Off,
            },
            &(),
        )
        .await;
        assert!(run.reviewer_failed, "未完项跑不了 → 带债（exit 2），不是干净 approve");
        assert_eq!(run.findings.len(), 1, "prior findings 不丢");
        assert!(run.cells.is_empty());
        assert!(run.warnings.iter().any(|w| w.contains("变更集为空")), "{:?}", run.warnings);
    }

    /// 全部 settled 的快照 + 空变更集：无未完项 → 不算债，静默早退。
    #[tokio::test]
    async fn restore_all_settled_with_empty_bundles_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let reviewer = fake_reviewer();
        let reg = crate::provider::registry::ProviderRegistry::new();
        let run = run_deep(
            &reg,
            DeepInput {
                route: None, scan_cells: &[], bundles: &[], root: dir.path(),
                reviewers: std::slice::from_ref(&reviewer), budget_cells: 4,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: None, restore: Some(&[snap_item("a.rs", ItemState::Settled)]),
                prior_findings: vec![], tools: ToolsMode::Off,
            },
            &(),
        )
        .await;
        assert!(!run.reviewer_failed);
        assert!(run.warnings.is_empty(), "{:?}", run.warnings);
    }

    /// restore 语义（AC3）：pending 重跑、settled 不重跑、重跑项 finding 与 prior 按 id 去重不双计、skipped 保留计数。
    #[tokio::test]
    async fn restore_reruns_pending_and_dedups_with_prior() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {\n").unwrap();
        std::fs::write(dir.path().join("b.rs"), "fn b() {\n").unwrap();
        std::fs::write(dir.path().join("c.rs"), "fn c() {\n").unwrap();
        let bundles = vec![
            Bundle::from_files(vec![FileChange { path: "a.rs".into(), diff: "@@ -1,1 +1,1 @@\n+fn a() {".into() }]),
        ];
        // prior finding 的 quote 与 Fake 将产出的相同 → 同 id → dedup 不双计
        let mut prior_f = fixtures::finding("f1", crate::domain::Severity::Low, 1, "p/r");
        prior_f.quote = "fn a() {".into();
        let snap = vec![
            snap_item("a.rs", ItemState::Settled),
            snap_item("b.rs", ItemState::Skipped),
            snap_item("c.rs", ItemState::InFlight),
        ];
        let reviewer = fake_reviewer();
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![turn(&rev_json("fn a() {"))])), Duration::from_secs(5), 0);
        let run = run_deep(
            &reg,
            DeepInput {
                route: None, scan_cells: &[], bundles: &bundles, root: dir.path(),
                reviewers: std::slice::from_ref(&reviewer), budget_cells: 4,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: None, restore: Some(&snap), prior_findings: vec![prior_f],
                tools: ToolsMode::Off,
            },
            &(),
        )
        .await;
        assert_eq!(run.cells.len(), 1, "只重跑 InFlight/c.rs 一项: {:?}", run.cells);
        assert!(run.cells[0].ok);
        assert_eq!(run.findings.len(), 1, "重跑产出与 prior 同 id → dedup 不双计");
        assert_eq!(run.meta.budget_skipped, 1, "skipped 保留");
    }

    /// R2 二阶段（`--tools two-stage`）：needs 非空 → 补查轮（读守卫放行 git-tracked）→ 终判取第二轮。
    #[tokio::test]
    async fn two_stage_needs_triggers_second_round() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "fn a() {\n").unwrap();
        let g = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(root).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        g(&["init", "-q"]);
        g(&["add", "a.rs"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let bundles = vec![Bundle::from_files(vec![FileChange { path: "a.rs".into(), diff: "@@ -1,1 +1,1 @@\n+fn a() {".into() }])];
        let reviewer = fake_reviewer();
        // 第一轮：一条 finding + 一条 needs（指向 git-tracked a.rs → 补查放行）
        let first = r#"{"findings":[{"severity":"low","title":"draft","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.4,"recommendation":"r","quote":"fn a() {"}],"needs":[{"path":"a.rs","reason":"need context"}]}"#;
        // 第二轮（终判）：needs 消化后确认同一条
        let second = rev_json("fn a() {");
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![turn(first), turn(&second)])), Duration::from_secs(5), 0);
        let run = run_deep(
            &reg,
            DeepInput {
                route: None, scan_cells: &[], bundles: &bundles, root,
                reviewers: std::slice::from_ref(&reviewer), budget_cells: 4,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: None, restore: Some(&[snap_item("a.rs", ItemState::InFlight)]), prior_findings: vec![],
                tools: ToolsMode::TwoStage,
            },
            &(),
        )
        .await;
        assert_eq!(run.meta.needs_two_stage_triggered, 1, "needs 非空触发补查");
        assert_eq!(run.cells.len(), 1);
        assert!(run.cells[0].ok);
        assert_eq!(run.findings.len(), 1, "终判取第二轮 CONFIRM 后同一条");
    }
}
