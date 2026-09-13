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
use crate::provider::tool_loop::ChatBackend;
use crate::risk::{Hotspot, RegionKind, RiskRoute};
use crate::state::CellRecord;

use super::ScanCellResult;

/// per-cell backend 工厂（流式旁路的挂点）：入参 = reviewer 条目 + 本格的 delta 回调，
/// 出参 = 该格独占的 backend 实例；`None` = 造不出来（缺密钥 / 未支持），调用方退回共享实例。
pub type PerCellBackend<'a> =
    dyn Fn(&EffectiveModel, Box<dyn FnMut(&str) + Send>) -> Option<Box<dyn ChatBackend>> + 'a;

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
    /// per-cell backend 工厂（批 B 接缝契约）：为本格造独占实例并挂流式旁路回调。
    /// `None` → 退回 registry 共享 backend（无旁路；Fake 测试与无密钥路径）。
    pub backend_factory: Option<&'a PerCellBackend<'a>>,
    /// reviewer cell 落盘（后台 job checkpoint —— resume 靠它认已挖过的格与其 findings）。
    /// `None` → 不落盘（前台一次性跑）。
    pub on_cell: Option<&'a dyn Fn(&CellRecord)>,
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
    let mut bypass_warned = false;

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
        // 排队时长在**派发时**定格：放到调用返回后采样量到的是「完成 − 入队」（含模型跑的几分钟），
        // 那是墙钟不是队列压力；且失败格根本不会 push，分布还带偏。
        if let Some(en) = item.enqueued_ms {
            meta.item_wait_ms.push(now_ms().saturating_sub(en));
        }
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
        // 流式旁路要挂在本格独占的 backend 上（共享实例会被并发 cell 抢 delta）。
        // 工厂缺席 / 造不出来 → 退回共享 backend：这一格没有旁路，但照跑。
        let inc_feed = inc.clone();
        let per_cell = input
            .backend_factory
            .and_then(|f| f(rev, Box::new(move |s: &str| inc_feed.lock().unwrap().feed(s))));
        if per_cell.is_none() && input.backend_factory.is_some() && !bypass_warned {
            bypass_warned = true; // 同一 reviewer 的结果稳定，报一次就够——别按格刷屏
            warnings.push(format!(
                "deep: {} 无 per-cell backend（缺密钥 / 构造失败）——流式旁路关闭，stream_discarded 恒 0",
                rev.name
            ));
        }
        let backend: &dyn ChatBackend = per_cell.as_deref().unwrap_or(lease.backend());
        let policy = lease.gate_attempts();
        let mut ctx = EmitCtx::new(&rev.name, &rev.family, item.agreement, input.root, sink);
        let call = run_review_shot_with(
            backend,
            &prompt,
            lease.per_turn(),
            lease.retry_budget(),
            Some(&policy),
            &mut ctx,
            inc,
        )
        .await;
        // 门等待与 429 排队只有 policy 知道（`ScanLease::run` 的回填在这条路径上不经过）——
        // 趁 lease 还活着读出来，否则 attempts / queue_wait_ms 永远是死值 0。
        let (rl_retries, rl_wait_ms) = (policy.retries(), policy.waited_ms());
        drop(per_cell);
        drop(lease);
        match call {
            Ok(mut c) => {
                // R2 二阶段（`--tools two-stage` 首个消费者，design §11）：needs 非空 → 补查轮。
                // 注入互斥：带笔记项（origin=scan）不注入 needs 证据（region::build 构造期 Err），计 skipped。
                let needs_count = c.output.needs().len();
                if needs_count > 0 {
                    // `on` 档在 M21b 还没有工具循环（reviewer 单发恒不带 tools），落到这里
                    // 至少要与 two-stage 等价——否则用户选了 --help 里更高的档反而拿到更少能力。
                    let tiered = matches!(input.tools, ToolsMode::TwoStage | ToolsMode::On);
                    let eligible = tiered && item.spec.notes.is_empty();
                    if eligible {
                        meta.needs_two_stage_triggered += 1;
                        if let Some(c2) = second_stage_round(
                            registry, &input, rev, &item, &idx, &opts, c.output.findings().to_vec(), c.output.needs(), sink,
                            &mut warnings,
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
                // 落地时刻的疑点共识回填到**落盘** finding（design §7「落地的 finding 继承记录当时的
                // agreement」、§8.1「按落地时刻算、后涨不回溯」）。cell 内的 `EmitCtx` 只能拿派发时
                // 快照——它跑在 await 里碰不到队列；此处回到主循环读活值，落盘 / 进报告的以此为准。
                // 串行派发下 cell 运行期间队列冻结，两值恒等；批 C1 并发后在途并入会让落地值更高，
                // 届时事件侧（FindingEvent）的值可能滞后，见 plan C1-2。
                let landed = live_agreement(&q, &item);
                for f in c.output.findings_mut() {
                    f.suspicion_agreement = landed;
                }
                let got = c.output.findings().to_vec();
                findings.extend(got.iter().cloned());
                let mut rec = CellRecord::ok(&cell_id, &rev.name, c.output).with_family(&rev.family);
                rec.duration_ms = t0.elapsed().as_millis() as u64;
                rec.input_tokens = c.input_tokens;
                rec.output_tokens = c.output_tokens;
                rec.cached_input_tokens = c.cached_input_tokens;
                rec.iterations = c.iterations;
                // 与 scanner 同口径（orchestrate/mod.rs）：attempts = 轮次 + 429 重试。
                rec.attempts = c.iterations + rl_retries + c.rate_limit_retries;
                rec.queue_wait_ms = rl_wait_ms + c.rate_limit_wait_ms;
                rec.template_hash = Some(review_template_hash());
                if c.stats.stream_discarded > 0 {
                    rec.warn = Some(format!("stream discarded {}", c.stats.stream_discarded));
                }
                meta.stream_discarded += c.stats.stream_discarded;
                meta.reviewer_output_tokens.push(c.output_tokens);
                meta.dispatch_agreement.push(item.dispatch_agreement);
                // 与派发时值成对（`landed` 上面已读）：二者之差即「挖的时候还不知道、挖完才攒够的共识」。
                meta.landed_agreement.push(landed);
                // 先落盘 cell（finding 的权威载体）再写队列快照：kill 落在两者之间时，
                // 快照仍是 InFlight → resume 重跑，宁可重挖一格也不丢 finding（design §7.1）。
                snap_cell(&rec, input.on_cell);
                cells.push(rec);
                q.mark_settled(&item.id, landed);
                if let Some(it) = q.get(&item.id) { snap_one(it, input.snapshot); }
            }
            Err(e) => {
                reviewer_failed = true;
                // 烧掉的 token 照样入账（scanner 同口径）——失败不等于免费，
                // 不记就成了报告里对不上的黑洞。
                let (bi, bo, bc) = e.burned_usage();
                let mut rec = CellRecord::failed(&cell_id, &rev.name, Role::Reviewer, format!("{e}"));
                rec = rec.with_family(&rev.family);
                rec.duration_ms = t0.elapsed().as_millis() as u64;
                rec.input_tokens = bi;
                rec.output_tokens = bo;
                rec.cached_input_tokens = bc;
                rec.attempts = 1 + rl_retries;
                rec.queue_wait_ms = rl_wait_ms;
                snap_cell(&rec, input.on_cell);
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
    warnings: &mut Vec<String>,
) -> Option<super::review_shot::ReviewCall> {
    let ex = crate::tools::ToolExecutor::new(input.root);
    let mut w2: Vec<String> = Vec::new();
    let ev: String = needs
        .iter()
        .filter_map(|n| crate::cli::second_pass::need_section(&ex, input.root, n, &mut w2))
        .collect();
    // 读守卫拒绝（denylist / 非 git-tracked）必须进报告——模型伸手要 ~/.ssh/id_rsa 这种事
    // 只打到 stderr 就等于没披露（second_pass.rs 的 deny_need_read 契约，T46①②）。
    warnings.append(&mut w2);
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
    note_enq(q.enqueue(EnqueueReq { spec, score: h.score, kind, families: vec![] }), "rule", meta);
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
        // 全量家族随请求进队列，agreement 由队列算定（= 家族数，与 merge_suspicions 同口径）。
        // 不再 enqueue 之后反查改写——那按 (file, quote) 找项，同文件同 quote 不同行的两条
        // 疑点会认错人，把后一簇的共识写到前一项上、真共识项反而掉出必挖集。
        note_enq(
            q.enqueue(EnqueueReq {
                spec,
                score: s.confidence,
                kind: ItemKind::Suspicion,
                families: m.families.clone(),
            }),
            "scan",
            meta,
        );
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

fn snap_cell(rec: &CellRecord, on_cell: Option<&dyn Fn(&CellRecord)>) {
    if let Some(f) = on_cell {
        f(rec);
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

    fn one_bundle(dir: &Path) -> Vec<Bundle> {
        std::fs::write(dir.join("a.rs"), "fn a() {\n").unwrap();
        vec![Bundle::from_files(vec![FileChange {
            path: "a.rs".into(),
            diff: "@@ -1,1 +1,1 @@\n+fn a() {".into(),
        }])]
    }

    #[allow(clippy::too_many_arguments)] // 测试夹具：直接映射 DeepInput 的字段面
    async fn run_one(
        reg: &crate::provider::registry::ProviderRegistry,
        root: &Path,
        bundles: &[Bundle],
        tools: ToolsMode,
    ) -> DeepRun {
        let reviewer = fake_reviewer();
        run_deep(
            reg,
            DeepInput {
                route: None, scan_cells: &[], bundles, root,
                reviewers: std::slice::from_ref(&reviewer), budget_cells: 4,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: None, restore: Some(&[snap_item("a.rs", ItemState::Pending)]),
                prior_findings: vec![], tools,
                backend_factory: None, on_cell: None,
            },
            &(),
        )
        .await
    }

    /// 落盘 finding 的 `suspicion_agreement` 与观测面 `landed_agreement` 同源（design §7「落地的 finding
    /// 继承记录当时的 agreement」）。串行派发下派发值恒等于落地值，所以本用例钉的是**契约**——
    /// 回填确实写进落盘与报告；批 C1 并发后两值会分叉，届时若有人删掉回填，落盘会退回派发快照、
    /// 与 `landed_agreement` 对不上。真正的「派发 ≠ 落地」分叉用例由 C1-2 补。
    #[tokio::test]
    async fn persisted_findings_carry_landed_suspicion_agreement() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = one_bundle(dir.path());
        let mut item = snap_item("a.rs", ItemState::Pending);
        item.agreement = 2;
        item.families = vec!["alpha".into(), "beta".into()];
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![turn(&rev_json("fn a() {"))])), Duration::from_secs(5), 0);
        let reviewer = fake_reviewer();
        let run = run_deep(
            &reg,
            DeepInput {
                route: None, scan_cells: &[], bundles: &bundles, root: dir.path(),
                reviewers: std::slice::from_ref(&reviewer), budget_cells: 4,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: None, restore: Some(std::slice::from_ref(&item)),
                prior_findings: vec![], tools: ToolsMode::Off,
                backend_factory: None, on_cell: None,
            },
            &(),
        )
        .await;
        assert_eq!(run.meta.landed_agreement, vec![2]);
        assert_eq!(run.findings.len(), 1);
        assert_eq!(run.findings[0].suspicion_agreement, 2, "报告里的 finding 带落地值");
        assert_eq!(run.cells[0].output.findings()[0].suspicion_agreement, 2, "落盘 CellRecord 同值");
        assert_eq!(run.findings[0].agreement, 1, "模型轴不受影响");
    }

    /// REPAIR 轮进 attempts（scanner 同口径 = 轮次 + 429 重试）。旧实现硬编码
    /// `1 + rate_limit_retries`，而 `rate_limit_retries` 在这条路径上恒 0 → attempts 永远是 1。
    #[tokio::test]
    async fn repair_round_counts_into_attempts() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = one_bundle(dir.path());
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![
            turn("not-json"),                 // 首轮 parse 失败 → REPAIR
            turn(&rev_json("fn a() {")),      // 第二轮好
        ])), Duration::from_secs(5), 0);
        let run = run_one(&reg, dir.path(), &bundles, ToolsMode::Off).await;
        assert_eq!(run.cells.len(), 1);
        assert_eq!(run.cells[0].iterations, 2, "两轮");
        assert_eq!(run.cells[0].attempts, 2, "attempts = 轮次 + 429 重试，不是死值 1");
    }

    /// 失败格烧掉的 token 照样入账（scanner 同口径）+ family 不为空——
    /// 不记就是报告里对不上的黑洞。
    #[tokio::test]
    async fn failed_reviewer_cell_books_burned_usage() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = one_bundle(dir.path());
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![
            turn("not-json"), turn("still-not-json"),
        ])), Duration::from_secs(5), 0);
        let run = run_one(&reg, dir.path(), &bundles, ToolsMode::Off).await;
        assert!(run.reviewer_failed);
        assert_eq!(run.cells.len(), 1);
        assert!(!run.cells[0].ok);
        assert_eq!(run.cells[0].family, "r", "失败格也带 family");
        assert!(
            run.cells[0].input_tokens > 0 || run.cells[0].output_tokens > 0,
            "两轮烧掉的 token 必须入账: {:?}", run.cells[0]
        );
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
                backend_factory: None, on_cell: None,
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
                backend_factory: None, on_cell: None,
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
                tools: ToolsMode::Off, backend_factory: None, on_cell: None,
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
                tools: ToolsMode::TwoStage, backend_factory: None, on_cell: None,
            },
            &(),
        )
        .await;
        assert_eq!(run.meta.needs_two_stage_triggered, 1, "needs 非空触发补查");
        assert_eq!(run.cells.len(), 1);
        assert!(run.cells[0].ok);
        assert_eq!(run.findings.len(), 1, "终判取第二轮 CONFIRM 后同一条");
    }

    /// `--tools on`：M21b 还没有工具循环，`on` 至少不能比 `two-stage` 弱——
    /// 旧实现只认 `== TwoStage`，选了更高档的用户反而只拿到 skipped 计数。
    #[tokio::test]
    async fn tools_on_is_not_weaker_than_two_stage() {
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
        let first = r#"{"findings":[],"needs":[{"path":"a.rs","reason":"need context"}]}"#;
        let second = rev_json("fn a() {");
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![turn(first), turn(&second)])), Duration::from_secs(5), 0);
        let run = run_one(&reg, root, &bundles, ToolsMode::On).await;
        assert_eq!(run.meta.needs_two_stage_triggered, 1, "on 档同样触发补查");
        assert_eq!(run.meta.needs_two_stage_skipped, 0);
    }

    /// 读守卫拒绝必须进报告 warnings（`deny_need_read` 契约 T46①②）：
    /// 模型伸手要非 git-tracked 的路径，旧实现把 `w2` 收了又丢，只剩 stderr。
    #[tokio::test]
    async fn denied_need_read_is_disclosed_in_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.rs"), "fn a() {\n").unwrap();
        std::fs::write(root.join("untracked.txt"), "secret\n").unwrap();
        let g = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(root).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        g(&["init", "-q"]);
        g(&["add", "a.rs"]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let bundles = vec![Bundle::from_files(vec![FileChange { path: "a.rs".into(), diff: "@@ -1,1 +1,1 @@\n+fn a() {".into() }])];
        // needs 指向未入库文件 → 守卫拒 → 必须留下 warning
        let first = r#"{"findings":[],"needs":[{"path":"untracked.txt","reason":"peek"}]}"#;
        let second = rev_json("fn a() {");
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![turn(first), turn(&second)])), Duration::from_secs(5), 0);
        let run = run_one(&reg, root, &bundles, ToolsMode::TwoStage).await;
        assert!(
            run.warnings.iter().any(|w| w.contains("untracked.txt")),
            "守卫拒绝未披露: {:?}", run.warnings
        );
    }
}
