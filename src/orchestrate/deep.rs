//! 深挖循环（M21b 批 C-4 / C1 / C2）：H0 + 疑点入队（开跑或经通道在途到达）→ 在线额度下门驱动并发派发（try_lease×pop → 一格 future；
//! 门满则等门挂起入 FIFO）→ 关闭入口时收场 drain → barrier → 按最终优先级序组装。
//! 无 reviewer 时调用方不进本模块（scan-only）；scanner 有格失败照进——F3〔D15〕：深挖照跑，判定仍 Incomplete。

use std::path::Path;
use std::sync::{Arc, Mutex};

use futures::stream::{FuturesUnordered, StreamExt};
use futures::FutureExt;
use tokio::sync::mpsc;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

use crate::bundle::{cross_index, Bundle};
use crate::config::{EffectiveModel, PackCfg, ToolsMode};
use crate::domain::{DeepMeta, Finding, Role};
use crate::orchestrate::queue::{cmp_priority, must_dig, DeepQueue, EnqueueOutcome, EnqueueReq, ItemKind, ItemState, QueueItem};
use crate::orchestrate::review_shot::{run_review_shot_with, EmitCtx, FindingSink, ReviewCall};
use crate::pack::region::{self as region, Origin as QueueOrigin, QueueItemSpec};
use crate::pack::PackOpts;
use crate::prompt::{build_review_prompt, review_template_hash, ReviewPromptInput, ScanPrompt};
use crate::provider::parse::ReviewParseStats;
use crate::provider::registry::{GateAttempts, ProviderRegistry, ScanLease};
use crate::provider::ProviderError;
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
    /// 往轮已落盘、本轮沿用未重挖的成功格（resume 回填，进 `coverage.reused`；`run_deep` 恒空）。
    pub reused_cells: Vec<CellRecord>,
    /// 收场时的最终队列项（调用方据此做组内闭包拆簇对账，C2〔D15〕）。
    pub items: Vec<QueueItem>,
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
    /// resume 恢复（C-5）：快照项为队列起点（空快照 = 新队列），未完项重跑；settled 项的已有 finding 经 `prior_findings` 并入（不双计）。
    /// C2〔D15〕起 restore 模式同样走收场 drain：已结案项占预算位 / 保底位，`route` 与 `scan_cells` 照常入队（到达键幂等）。
    pub restore: Option<&'a [crate::orchestrate::queue::QueueItem]>,
    /// resume 补跑过 scanner（首跑收场时信息不全）→ 开跑时把 Skipped 复位 Pending、与补回的疑点一起重选（design §6 RS1〔D15〕）。
    pub reopen_skipped: bool,
    pub prior_findings: Vec<Finding>,
    /// `--tools` 档位（R2 二阶段首个消费者）：TwoStage 时 reviewer needs 触发补查轮。
    pub tools: ToolsMode,
    /// per-cell backend 工厂（批 B 接缝契约）：为本格造独占实例并挂流式旁路回调。
    /// `None` → 退回 registry 共享 backend（无旁路；Fake 测试与无密钥路径）。
    pub backend_factory: Option<&'a PerCellBackend<'a>>,
    /// reviewer cell 落盘（后台 job checkpoint —— resume 靠它认已挖过的格与其 findings）。
    /// `None` → 不落盘（前台一次性跑）。
    pub on_cell: Option<&'a dyn Fn(&CellRecord)>,
    /// 在途入队通道（scanner 流式扇出，C2〔D15〕）：到达项走入口五态（到达键幂等）；通道关闭 = 关闭入口 → 收场 drain。`None` = 开跑即关闭。
    /// barrier 含「通道已关闭」：发送端未全部 drop 前，队列空也不收场。
    pub incoming: Option<mpsc::Receiver<EnqueueReq>>,
}

pub async fn run_deep(registry: &ProviderRegistry, mut input: DeepInput<'_>, sink: &dyn FindingSink) -> DeepRun {
    let t_run = tokio::time::Instant::now();
    let mut meta = DeepMeta::default();
    let mut warnings = Vec::new();
    let prior_findings = input.prior_findings.clone();

    // 空变更集 → 无 bundle 可 pack，直接早退（`pick_bundle` 以 `bundles[0]` 兜底，空片会越界）。
    // resume 侧同样经此：重建 diff 落空（改动已提交 / 已切分支）而快照仍有未完项时，
    // 披露成带债而不是崩在 pack 上。
    if input.bundles.is_empty() {
        let unfinished = input.restore.is_some_and(|items| unfinished(items, input.scan_cells));
        if let Some(items) = input.restore {
            // 早退也带上快照里的截断数（评审修 #3）：否则首跑被预算截掉、从没挖过的项既不计债也不披露。
            meta = restored_meta(items);
        }
        if unfinished {
            warnings.push(
                "deep: 变更集为空（改动已提交 / diff 不可重建），未挖的队列项与疑点无法重跑——带债收口".into(),
            );
        }
        let items = input.restore.map(<[QueueItem]>::to_vec).unwrap_or_default();
        return DeepRun { findings: prior_findings, cells: vec![], meta, warnings, reviewer_failed: unfinished, reused_cells: vec![], items };
    }

    let restore_mode = input.restore.is_some();
    // resume：往轮已派发、未完成的项（快照 InFlight）是往轮选中过的——占预算位 / 保底位、不参与收场 drain，重派即可。
    // 只数 Settled 时，必挖把往轮派发数顶出 K 的场景下 drain 会把它们截成 Skipped（Pending 集合缩水，design C2 §6）。
    let resumed: std::collections::HashSet<&str> = input
        .restore
        .into_iter()
        .flatten()
        .filter(|it| it.state == ItemState::InFlight)
        .map(|it| it.id.as_str())
        .collect();
    let mut q = match input.restore {
        Some(items) => DeepQueue::restore(items.to_vec()),
        None => DeepQueue::new(),
    };
    if input.reopen_skipped {
        // resume 补跑过 scanner（design §6 RS1〔D15〕）：首跑收场时信息不全，被截项复位、与补回的疑点一起重选。
        for i in q.reopen_skipped() {
            snap_one(&q.items()[i], input.snapshot);
        }
    }
    if restore_mode {
        meta = restored_meta(q.items());
    }
    // 在线额度的计数起点（design v11 §6 规则 3〔D15〕）：resume 时往轮派发过的项（已结案 ∪ 在途）占着预算位，其中非必挖疑点占着保底位。
    let prior = |it: &&QueueItem| it.state == ItemState::Settled || resumed.contains(it.id.as_str());
    let mut dispatched = q.items().iter().filter(prior).count();
    let mut nonmust_susp = q.items().iter().filter(prior).filter(|it| counts_against_floor(it)).count();
    let dispatched_before = dispatched;
    // 块与风险档上下文（C3〔D16〕）：整轮共用一份文件内容缓存；同步读计入下面的阻塞观测。
    let mut blocks = BlockCtx::new(input.root, input.route, input.bundles);
    let t_enq = std::time::Instant::now();
    if let Some(route) = input.route {
        for h in &route.hotspots {
            enqueue_h0(&mut q, h, &mut meta, input.snapshot, &mut blocks);
        }
    }
    enqueue_scan_cells(&mut q, input.scan_cells, &mut meta, input.snapshot, &mut blocks);
    note_block(&mut meta, t_enq.elapsed());
    meta.queue_depth_peak = meta.queue_depth_peak.max(q.items().len() as u32);

    let mut incoming = input.incoming.take();
    let k = input.budget_cells as usize;
    let floor_total = if k == 0 { 0 } else { k.div_ceil(4) };
    // 扫描期非必挖额度 K − F：保底 F 格留给后到的疑点（H0 / 涟漪不得吃掉）。
    let quota = k.saturating_sub(floor_total);
    let mut closed = incoming.is_none();
    let mut drained = false;

    if input.reviewers.is_empty() {
        close_drain(&mut q, input.snapshot, &resumed, k.saturating_sub(dispatched), floor_total.saturating_sub(nonmust_susp));
        meta.budget_skipped = skipped_now(&q);
        observe_selection(&q, &mut meta);
        return DeepRun { findings: vec![], cells: vec![], meta, warnings, reviewer_failed: false, reused_cells: vec![], items: q.items().to_vec() };
    }
    let members: &[EffectiveModel] = input.reviewers;
    let idx = cross_index(input.bundles, &[], None);
    let opts = PackOpts::from(input.pack);
    // 读守卫的跟踪文件集每轮只取一次（C2 Q9〔D15〕）：派发路径每项、二阶段每条 need 都查它。整仓 `git ls-files` 是同 task 上的同步段，
    // 与开跑入队一起计入阻塞观测（评审 #11：漏计会让 `spawn_blocking` 的触发判断系统性偏乐观）。
    let t_load = std::time::Instant::now();
    let tracked = crate::cli::second_pass::TrackedFiles::load(input.root);
    note_block(&mut meta, t_load.elapsed());
    if tracked.unavailable() {
        // 整轮读守卫 fail-closed（评审 #13）：逐格只会显示「已拒绝（读守卫）」，报告层得说清是取跟踪集失败
        warnings.push("deep: 取 git 跟踪文件集失败——本轮证据读取与二阶段 needs 一律按未跟踪拒绝（fail-closed）".into());
    }
    let env = CellEnv { input: &input, idx: &idx, tracked: &tracked, opts: &opts, sink };

    // 门驱动并发派发（design §6 规则 5〔D13〕）：provider 门有空位才弹出最高优先的可派项交出去。
    // 同 task 内并发（`FuturesUnordered`）：future 只借共享引用跑一格；队列 / meta / cells / 回调只在本循环写。
    let mut running = FuturesUnordered::new();
    let mut landed: Vec<Landed> = Vec::new();
    // 分派池空（成员全部出局）= 停派：通道未关照收、在途照等，剩余项保持 Pending（resume 可续）。
    let mut pool = Pool::new(members.len());
    // 等门命中拿到的门位（连同成员下标），下一轮填门时先用。
    let mut parked: Option<(ScanLease<'_>, usize)> = None;
    // 等门挂起（G1〔D15〕）：有可派项而各门无位时，为活跃成员各建一个阻塞 lease 排进门的 FIFO。
    // 跨事件保持——通道到达、格完成都不重建，否则每次都重新排到队尾（被后来的 scanner 插在前面）。
    let mut waits = None;
    let mut reviewer_failed = false;

    loop {
        // ① 收场：关闭入口后剩余预算 drain 一次；Skipped 只在此刻产生（design v11 §6 规则 3）
        if closed && !drained {
            drained = true;
            let remaining = k.saturating_sub(dispatched);
            let t_close = std::time::Instant::now();
            close_drain(&mut q, input.snapshot, &resumed, remaining, floor_total.saturating_sub(nonmust_susp));
            note_block(&mut meta, t_close.elapsed());
            meta.close_budget_remaining = remaining as u32;
            meta.scan_phase_dispatched = (dispatched - dispatched_before) as u32;
            if !restore_mode {
                meta.close_ms = t_run.elapsed().as_millis() as u64;
            }
        }
        // ② 填门：池内成员空闲轮转，谁先拿到门位谁消费下一项；无可派项不取门位（免持门空转）
        let fill_t0 = std::time::Instant::now();
        let before = dispatched;
        let mut no_slot = false;
        while !pool.is_empty() && q.has_eligible(|it| is_eligible(it, closed, dispatched, quota)) {
            let next = match parked.take() {
                Some((lease, member)) if pool.alive().any(|i| i == member) => Some((lease, member)),
                Some(_) => pool.next_lease(registry, members), // 等门时出局的成员：门位随旧 lease 归还
                None => pool.next_lease(registry, members),
            };
            let Some((lease, member)) = next else {
                no_slot = true; // 各门全满或有人排队（让位）
                break;
            };
            let Some(item) = q.pop_dispatch_where(|it| is_eligible(it, closed, dispatched, quota)) else { break };
            dispatched += 1;
            if counts_against_floor(&item) {
                nonmust_susp += 1;
            }
            if meta.first_dispatch_ms == 0 {
                meta.first_dispatch_ms = t_run.elapsed().as_millis().max(1) as u64;
            }
            snap_one(&item, input.snapshot);
            let rev = &members[member];
            // cell 名按队列项（id 已定死），跨 resume 调用稳定：同项重跑 upsert 覆盖自身（与 scanner「重试者胜」同义），
            // 不同项永不同名。按本次调用的派发序编号时，resume 挖别的项会同名覆盖首跑的格、丢掉它的 finding。
            let cell_id = review_cell_id(&item.id, &rev.name);
            let pack = match region::build(&item.spec, pick_bundle(input.bundles, &item.spec.file), input.root, &tracked, &idx, 64 * 1024, &opts, "") {
                Ok(p) => p,
                Err(e) => {
                    // 派发时 pack 构造失败：就地结案，仍占一个派发位（与 C1「占 keep 位」同口径）
                    warnings.push(format!("{cell_id}: pack {e}"));
                    q.mark_settled(&item.id, item.agreement);
                    if let Some(it) = q.get(&item.id) {
                        snap_one(it, input.snapshot);
                    }
                    continue; // lease 随本轮作用域归还
                }
            };
            // 排队时长在**派发时**定格：完成时采样量到的是「完成 − 入队」（含模型跑的几分钟），那是墙钟不是队列压力。
            let wait_ms = item.enqueued_ms.map_or(0, |en| now_ms().saturating_sub(en));
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
            pool.assigned(member);
            running.push(run_cell(&env, Dispatched { member, rev, lease, item, prompt, cell_id, wait_ms }));
        }
        if dispatched > before {
            note_block(&mut meta, fill_t0.elapsed());
        }

        // ③ barrier 与等门挂起的撤销：无可派项时不持门、不排队
        let any_eligible = q.has_eligible(|it| is_eligible(it, closed, dispatched, quota));
        if !any_eligible {
            parked = None;
            waits = None;
        }
        // 在途 0 ∧（可派 0 ∨ 池空）∧ 入口已关（design §2；复核在途批 D 前恒 0）
        if running.is_empty() && closed && (pool.is_empty() || !any_eligible) {
            break;
        }
        if pool.is_empty() {
            waits = None;
        } else if any_eligible && no_slot && waits.is_none() {
            waits = Some(
                pool.alive_rotated()
                    .map(|i| async move { (i, registry.lease(&members[i].name).await) })
                    .collect::<FuturesUnordered<_>>(),
            );
        }

        // ④ 等事件：任一格完成（完成即归还门位）/ 经通道到达一项 / 等门命中。
        // 醒来只取一个事件，随后按固定序（完成 → 到达 → 等门命中）把同刻已就绪的其余事件一并排空，再回去填门：
        // 一格的疑点是一批到达（D13 Q-B 逐 cell 交付），逐条派发会让扫描期额度按模型输出序消耗而不是按优先级；
        // 同刻就绪的事件若由 `select!` 随机定先后，同一输入同一时序的选中集合也不可复现（`biased` + 排空后二者都定死）。
        let mut done_now: Vec<CellDone> = Vec::new();
        let mut hit: Option<(usize, Result<ScanLease<'_>, ProviderError>)> = None;
        tokio::select! {
            biased;
            Some(done) = running.next(), if !running.is_empty() => done_now.push(done),
            msg = recv(&mut incoming), if !closed => {
                let t0 = std::time::Instant::now();
                on_arrival(msg, &mut q, &mut meta, &mut closed, input.snapshot, &mut blocks);
                note_block(&mut meta, t0.elapsed());
            }
            Some(got) = next_wait(&mut waits), if waits.is_some() => hit = Some(got),
            else => break,
        }
        while let Some(Some(done)) = running.next().now_or_never() {
            done_now.push(done);
        }
        for done in done_now {
            // 落地是同步段：回填共识 + 落盘 cell（后台 job 写 SQLite）+ 队列结案 + 快照，期间其余在途格的流读取与计时器一起停
            let t_land = std::time::Instant::now();
            let l = land(done, &mut q, input.snapshot, input.on_cell);
            note_block(&mut meta, t_land.elapsed());
            meta.drain_done_ms = t_run.elapsed().as_millis().max(1) as u64;
            reviewer_failed |= l.debt;
            if l.auth_failed {
                // 成员健康（C1 §4.2）：鉴权失败不会自愈 → 出局，后续项只交其余成员。这一格保持失败、不改派（D7 A3 无候补）；
                // 该成员其他在途格照常跑完。只在「队列项 > 并发容量」时起作用——小队列 t=0 就全派出去了。
                pool.remove(l.member, format!("deep: reviewer {} auth failed — removed from pool", members[l.member].name));
            }
            landed.push(l);
        }
        let t_arrivals = std::time::Instant::now();
        let mut drained_arrival = false;
        while !closed {
            match incoming.as_mut().map(mpsc::Receiver::try_recv) {
                Some(Ok(req)) => {
                    on_arrival(Some(req), &mut q, &mut meta, &mut closed, input.snapshot, &mut blocks);
                    drained_arrival = true;
                }
                Some(Err(mpsc::error::TryRecvError::Disconnected)) => {
                    on_arrival(None, &mut q, &mut meta, &mut closed, input.snapshot, &mut blocks);
                    drained_arrival = true;
                }
                _ => break,
            }
        }
        if drained_arrival {
            note_block(&mut meta, t_arrivals.elapsed());
        }
        if hit.is_none() {
            hit = waits.as_mut().and_then(|w| w.next().now_or_never()).flatten();
        }
        if let Some((i, got)) = hit {
            match got {
                Ok(lease) if pool.alive().any(|a| a == i) => {
                    parked = Some((lease, i));
                    waits = None; // 其余成员的等门登记撤销（出队并唤醒后面的人）
                }
                // 等门期间出局的成员命中：门位就地归还，其余成员的排队位保留（撤了要重排到队尾，门位会转给后登记的外部持门者）
                Ok(_) => {
                    if waits.as_ref().is_some_and(FuturesUnordered::is_empty) {
                        waits = None;
                    }
                }
                Err(e) => {
                    pool.remove(i, format!("deep: reviewer {} lease failed: {e} — removed from pool", members[i].name));
                    if waits.as_ref().is_some_and(FuturesUnordered::is_empty) {
                        waits = None;
                    }
                }
            }
        }
    }
    drop(waits);
    drop(running);
    drop(parked);

    // 池观测按成员序披露（完成序不进报告）：出局原因 → 池空剩余 → 未分到格的成员。
    warnings.extend(pool.removed.iter().flatten().cloned());
    if pool.is_empty() && q.pending_count() > 0 {
        reviewer_failed = true;
        warnings.push(format!(
            "deep: reviewer pool empty — {} queued item(s) not dispatched, left pending for resume",
            q.pending_count()
        ));
    }
    if dispatched > dispatched_before {
        // 有格派出去了、某成员却一格没分到——多 reviewer 配置里「白配了」必须可见（批 C dogfood grok-4.6 空转且无声）。
        // 出局的成员已由出局原因披露，不重复报。
        for (i, m) in members.iter().enumerate() {
            if pool.cells[i] == 0 && pool.removed[i].is_none() {
                warnings.push(format!("deep: reviewer {} configured but received 0 cells", m.name));
            }
        }
    }

    // 收场按最终队列项优先级序组装（design C2 §8 D2〔D15〕）：持久化已在完成时做完；报告侧的 findings（→ dedup 以输入序首个为 base）、
    // cells、meta 四数组与每格 warnings 依此序喂。保证：同一选中集合 + 同样的格输出 → 报告逐字节一致，除去三类随时序变的读数：
    // 计时字段、收场时刻读数（`scan_phase_dispatched` / `close_budget_remaining`）、以及**时刻快照式的共识**
    // （`dispatch_agreement` / `landed_agreement` 与 finding 的 `suspicion_agreement`：异族并入发生在派发 / 落地之前还是之后随时序变，
    // 评审 #10）。选中集合本身随到达时序变化是在线预算的已知代价（批 C3：量重合度，不逐字节比）。
    meta.budget_skipped = skipped_now(&q);
    observe_selection(&q, &mut meta);
    landed.sort_by(|a, b| match (q.get(&a.item_id), q.get(&b.item_id)) {
        (Some(x), Some(y)) => cmp_priority(x, y),
        _ => a.item_id.cmp(&b.item_id),
    });
    let mut findings = Vec::new();
    let mut cells = Vec::new();
    let mut bypass_missing: Vec<String> = Vec::new();
    let mut cell_warnings = Vec::new();
    for l in landed {
        match l.stage2 {
            Stage2::Skipped => meta.needs_two_stage_skipped += 1,
            Stage2::Sent | Stage2::Failed => meta.needs_two_stage_triggered += 1,
            Stage2::None => {}
        }
        meta.stream_discarded += l.stream_discarded;
        if l.bypass_missing && !bypass_missing.contains(&l.rec.model) {
            bypass_missing.push(l.rec.model.clone());
        }
        findings.extend(l.rec.output.findings().iter().cloned());
        meta.item_wait_ms.push(l.wait_ms);
        meta.dispatch_agreement.push(l.dispatch_agreement);
        // 与派发时值成对：二者之差即「挖的时候还不知道、挖完才攒够的共识」。失败格记失败时刻的活值。
        meta.landed_agreement.push(l.landed);
        // 失败格记烧掉的 output（CellRecord 已入账）——四数组逐格对齐，不因失败缺位。
        meta.reviewer_output_tokens.push(l.rec.output_tokens);
        cell_warnings.extend(l.warnings);
        cells.push(l.rec);
    }
    for name in bypass_missing {
        // 同一 reviewer 的结果稳定，报一次就够——别按格刷屏
        warnings.push(format!("deep: {name} 无 per-cell backend（缺密钥 / 构造失败）——流式旁路关闭，stream_discarded 恒 0"));
    }
    warnings.extend(cell_warnings);

    findings.extend(prior_findings);
    DeepRun {
        findings: crate::consolidate::dedup(findings),
        cells,
        meta,
        warnings,
        reviewer_failed,
        reused_cells: vec![],
        items: q.items().to_vec(),
    }
}

/// 队列未完（design C2 §6〔D15〕，评审 #1 / #2）：有没挖完的项，或有成功 scanner 格的疑点从没到过队列——
/// 补跑出的新疑点、首跑到达丢失（格落盘成功而快照写失败）都落在后者。diff 重建不了 / 变更集为空时据此计债，
/// 不把「从没挖过」收成干净通过。旧快照（C1 起，项无到达键）只按项状态判：那时疑点是预合并后入队的，到达键无从复算。
pub fn unfinished(items: &[QueueItem], cells: &[ScanCellResult]) -> bool {
    if items.iter().any(|it| matches!(it.state, ItemState::Pending | ItemState::InFlight)) {
        return true;
    }
    if !items.is_empty() && items.iter().all(|it| it.arrivals.is_empty()) {
        return false;
    }
    let seen: std::collections::HashSet<&str> =
        items.iter().flat_map(|it| it.arrivals.iter().map(String::as_str)).collect();
    cells.iter().any(|c| {
        c.output.as_ref().is_some_and(|out| {
            out.suspicions.iter().enumerate().any(|(i, s)| {
                let key = crate::consolidate::scan::arrival_key(&c.cell_id, i, s);
                EnqueueReq::scan(s, &c.family, key.clone()).is_some() && !seen.contains(key.as_str())
            })
        })
    })
}

/// 收场 drain（design v11 §6 规则 3〔D15〕）：留必挖 ∪ 前 `floor` 条非必挖疑点 ∪ 按优先级补到 `remaining`，余者 Skipped；
/// `resumed`（往轮在途项）不参与、恒保留。只写被截项的快照；截断数在收口时按最终状态数（见 `skipped_now`）。
fn close_drain(
    q: &mut DeepQueue,
    snapshot: Option<&dyn Fn(&QueueItem)>,
    resumed: &std::collections::HashSet<&str>,
    remaining: usize,
    floor: usize,
) {
    let skipped = q.drain_among(remaining, floor, |it| !resumed.contains(it.id.as_str()));
    for id in &skipped {
        if let Some(it) = q.get(id) {
            snap_one(it, snapshot);
        }
    }
}

/// 截断数 = **收口时**仍是 Skipped 的项数（评审 #9）：resume 回放把被截项并成必挖挖掉后，按 drain 那一刻计会留下假债
/// （披露「skipped(budget) N」并计 exit 2，而那 N 项其实已经挖了）。首跑 Skipped 只在收场产生、其后无复位，两种口径同值。
fn skipped_now(q: &DeepQueue) -> u32 {
    q.items().iter().filter(|it| it.state == ItemState::Skipped).count() as u32
}

/// 选项观测（design C3 §7〔D16〕）：收场时把队列压成五个标量——合并压缩比、候选与选中的文件面、单文件最大占格。
/// 逐格数组不落（可从 `DeepRun.items` + scan 报告离线复算）。
fn observe_selection(q: &DeepQueue, meta: &mut DeepMeta) {
    let items = q.items();
    meta.items_from_suspicions = items.iter().filter(|it| it.kind == ItemKind::Suspicion).count() as u32;
    let files: std::collections::BTreeSet<&str> = items.iter().map(|it| it.spec.file.as_str()).collect();
    meta.files_candidate = files.len() as u32;
    let mut per_file: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for it in items.iter().filter(|it| matches!(it.state, ItemState::InFlight | ItemState::Settled)) {
        *per_file.entry(it.spec.file.as_str()).or_insert(0) += 1;
    }
    meta.files_selected = per_file.len() as u32;
    meta.max_cells_per_file = per_file.values().copied().max().unwrap_or(0);
}

/// 在线额度下的可派条件（design v11 §6 规则 3〔D15〕）：必挖不限；入口关闭后（收场 drain 已定选中集合）全部可派；
/// 扫描期非必挖项受 `K − F` 额度——每次判定读现值，填门循环里派一项就少一格。
fn is_eligible(it: &QueueItem, closed: bool, dispatched: usize, quota: usize) -> bool {
    must_dig(it) || closed || dispatched < quota
}

/// 占保底位的派发：非必挖的疑点（派发时共识 < 2）。保底 ⌈K/4⌉ 是给疑点的，H0 / 涟漪不占。
fn counts_against_floor(it: &QueueItem) -> bool {
    // 与必挖判定同轴（评审 #4）：C3 起 `must_dig` 看项内 `peak_agreement`，而 `dispatch_agreement` 是区域级——
    // 结构化合并后「一块被两家族碰过但没有一条疑点对上」（agreement 2 / peak 1）是常态，
    // 按区域轴数会把这种非必挖项当成必挖、保底永远不消耗，收场再整份预留一次 ⌈K/4⌉，实际派发超出 maxCells。
    it.kind == ItemKind::Suspicion && !must_dig(it)
}

/// 主循环同步段计时（design C2 §9〔D15〕，评审 #11 补齐覆盖）：取跟踪文件集、开跑入队、填门（pack + prompt + 快照）、
/// 到达处理、落地（落盘 + 结案 + 快照）、收场 drain——同 task 并发下这些段阻塞期间其余在途格的流读取与计时器一起停。
fn note_block(meta: &mut DeepMeta, d: std::time::Duration) {
    let ms = d.as_millis() as u64;
    meta.fill_block_max_ms = meta.fill_block_max_ms.max(ms);
    meta.fill_block_total_ms += ms;
}

/// 等门挂起的下一个命中；无等门时永不就绪（`select!` 分支另由 `waits.is_some()` 守卫关掉）。
async fn next_wait<F: std::future::Future>(waits: &mut Option<FuturesUnordered<F>>) -> Option<F::Output> {
    match waits {
        Some(w) => w.next().await,
        None => std::future::pending().await,
    }
}

/// 一格 future 借用的共享面（全部活过 `run_deep`；同 task 并发，不要求 `Send` / `'static`）。
struct CellEnv<'b> {
    input: &'b DeepInput<'b>,
    idx: &'b crate::bundle::CrossIndex,
    /// 本轮预取的 git 跟踪文件集（读守卫查它，不逐项起子进程）。
    tracked: &'b crate::cli::second_pass::TrackedFiles,
    opts: &'b PackOpts,
    sink: &'b dyn FindingSink,
}

/// reviewer 分派池（design §6 规则 5〔D13〕、C1 §4）：活跃成员空闲轮转——谁先拿到门位谁消费下一项，每项只交一个成员（不双审）。
/// 纯轮转、无权重 / 主次（design「无 priority、顺序字典序无语义」）；不是候补链：成员出局后，它已失败的格不改派（D7 A3）。
/// 同 provider 的成员共享一道门，轮转只决定空位归谁、不加吞吐；跨 provider 各门独立，一门满另一门接。
struct Pool {
    /// 上次派发者下标；初值为末位，首派从 0 号起（单成员时与批 C 同构）。
    cursor: usize,
    /// 每成员分到的格数（0 格成员收场披露）。
    cells: Vec<u32>,
    /// 出局原因（收场按成员序披露）；`None` = 仍在池内——在池与否只此一处真相。
    removed: Vec<Option<String>>,
}

impl Pool {
    fn new(n: usize) -> Self {
        Pool { cursor: n.saturating_sub(1), cells: vec![0; n], removed: vec![None; n] }
    }

    fn is_empty(&self) -> bool {
        self.removed.iter().all(Option::is_some)
    }

    fn alive(&self) -> impl Iterator<Item = usize> + '_ {
        self.removed.iter().enumerate().filter(|(_, r)| r.is_none()).map(|(i, _)| i)
    }

    /// 活跃成员按轮转序（游标下一位起，与 [`Self::next_lease`] 同序）：等门登记按此序进门的 FIFO——
    /// 按成员下标升序登记会让 0 号成员恒拿共门的下一个门位，轮转失效（C1 交替分派的回归）。
    fn alive_rotated(&self) -> impl Iterator<Item = usize> + '_ {
        let n = self.removed.len();
        let cursor = self.cursor;
        (1..=n).map(move |step| (cursor + step) % n).filter(|&i| self.removed[i].is_none())
    }

    /// 从游标下一位起逐个试取门位，第一个拿到的即用；全满 → `None`。未注册的成员就地出局，不挡其余成员。
    fn next_lease<'r>(&mut self, registry: &'r ProviderRegistry, members: &[EffectiveModel]) -> Option<(ScanLease<'r>, usize)> {
        let n = self.removed.len();
        for step in 1..=n {
            let i = (self.cursor + step) % n;
            if self.removed[i].is_some() {
                continue;
            }
            match registry.try_lease(&members[i].name) {
                Ok(Some(lease)) => return Some((lease, i)),
                Ok(None) => {}
                Err(e) => self.remove(i, format!("deep: reviewer {} lease failed: {e} — removed from pool", members[i].name)),
            }
        }
        None
    }

    /// 成员 `i` 领走一格：游标移到它，下一项从它的下一位起试。
    fn assigned(&mut self, i: usize) {
        self.cursor = i;
        self.cells[i] += 1;
    }

    /// 出局（幂等：只记首次原因）。
    fn remove(&mut self, i: usize, reason: String) {
        self.removed[i].get_or_insert(reason);
    }
}

/// 主循环交给一格 future 的全部所有权面（门位随 `lease` 进 future，整格结束才归还）。
struct Dispatched<'b> {
    /// 分派池成员下标（出局判定用）。
    member: usize,
    rev: &'b EffectiveModel,
    lease: ScanLease<'b>,
    item: QueueItem,
    prompt: ScanPrompt,
    cell_id: String,
    wait_ms: u64,
}

/// R2 二阶段补查的结局（design C1 §5 契约表）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage2 {
    /// 首轮无 needs。
    None,
    /// 有 needs 但不合格（tools off / 带笔记互斥），或全部 needs 被读守卫拒绝 / 不可读 → 第二轮不发、不计债。
    Skipped,
    /// 第二轮发出且成功：第二轮输出为终判。
    Sent,
    /// 第二轮建 pack 内部错误，或发出后失败：保留首轮 finding + warning + 计债。
    Failed,
}

/// 一格 future 的产出（未碰队列）。
struct CellDone<'b> {
    member: usize,
    rev: &'b EffectiveModel,
    item: QueueItem,
    cell_id: String,
    duration_ms: u64,
    wait_ms: u64,
    outcome: Result<ReviewCall, ProviderError>,
    /// 两轮合计的 429 重试 / 门等待（同一 policy，两轮结束统一回读）。
    rl_retries: u32,
    rl_wait_ms: u64,
    stage2: Stage2,
    /// 二阶段失败的原因（`Stage2::Failed` 时有值），写进落盘格的 warn。
    stage2_error: Option<String>,
    bypass_missing: bool,
    /// 首轮或二阶段返回鉴权失败（穿透 `attach_usage` 包装判定）→ 该成员出局。
    auth_failed: bool,
    warnings: Vec<String>,
}

/// 完成时已落盘的一格，暂存待收场按最终项优先级组装。
struct Landed {
    /// 所挖队列项 id（收场按该项最终优先级排序）。
    item_id: String,
    member: usize,
    auth_failed: bool,
    rec: CellRecord,
    wait_ms: u64,
    dispatch_agreement: u32,
    landed: u32,
    stage2: Stage2,
    stream_discarded: u32,
    bypass_missing: bool,
    /// 本格计债：cell 失败，或二阶段失败。
    debt: bool,
    warnings: Vec<String>,
}

/// 一格完整执行：首轮 → （needs 合格）二阶段 → 回读 429 观测 → 归还门位。**全程持同一 lease**（design C1 §3.3 / Q4）：
/// 两轮之间放门会让第二轮与新派发的格抢门——门宽 1 时补查轮排到队尾，墙钟与在途口径都失真。
async fn run_cell<'b>(env: &CellEnv<'b>, d: Dispatched<'b>) -> CellDone<'b> {
    let Dispatched { member, rev, lease, item, prompt, cell_id, wait_ms } = d;
    let t0 = tokio::time::Instant::now();
    let policy = lease.gate_attempts();
    let mut warnings = Vec::new();
    let (first, mut bypass_missing) = shot(env, &lease, &policy, rev, item.agreement, &prompt).await;
    let mut stage2 = Stage2::None;
    let mut stage2_error = None;
    let mut auth_failed = false;
    let outcome = match first {
        Err(e) => {
            auth_failed = is_auth(&e);
            Err(e)
        }
        Ok(mut c) => {
            let needs = c.output.needs().to_vec();
            if !needs.is_empty() {
                // `on` 档在 M21b 还没有工具循环（reviewer 单发恒不带 tools），落到这里至少与 two-stage 等价。
                // 注入互斥：带笔记项（origin=scan）不注入 needs 证据（region::build 构造期 Err）→ 不合格。
                let tiered = matches!(env.input.tools, ToolsMode::TwoStage | ToolsMode::On);
                stage2 = if !(tiered && item.spec.notes.is_empty()) {
                    Stage2::Skipped
                } else {
                    match second_stage_prompt(env, &item, c.output.findings(), &needs, &mut warnings) {
                        Round2::NoEvidence => Stage2::Skipped,
                        Round2::PackFailed(e) => {
                            warnings.push(format!("{cell_id}: second-stage pack failed: {e} — keeping first-round findings (debt)"));
                            stage2_error = Some(format!("second-stage pack failed: {e}"));
                            Stage2::Failed
                        }
                        Round2::Ready(prompt2) => {
                            let (second, missing) = shot(env, &lease, &policy, rev, item.agreement, &prompt2).await;
                            bypass_missing |= missing;
                            match second {
                                Ok(c2) => {
                                    c.input_tokens += c2.input_tokens;
                                    c.output_tokens += c2.output_tokens;
                                    c.cached_input_tokens += c2.cached_input_tokens;
                                    c.iterations += c2.iterations;
                                    // 解析 / 锚定丢弃计数取终判那一轮（首轮草稿被丢的不算这一格的产出丢失）；旁路丢弃两轮都真实发生，累加。
                                    c.stats = ReviewParseStats { stream_discarded: c.stats.stream_discarded + c2.stats.stream_discarded, ..c2.stats };
                                    c.output = c2.output; // 第二轮 = CONFIRM/REVISE 后终判
                                    Stage2::Sent
                                }
                                Err(e) => {
                                    auth_failed = is_auth(&e);
                                    // 首轮声明 needs = 模型自认判断不完整：静默沿用首轮会成单区域假绿灯 → 保留首轮 finding 但计债；
                                    // 第二轮烧掉的 token 照样入账。
                                    let (bi, bo, bc) = e.burned_usage();
                                    c.input_tokens += bi;
                                    c.output_tokens += bo;
                                    c.cached_input_tokens += bc;
                                    // 第二轮确实发出过：轮次记下限 1（失败轮内是否走过 REPAIR 不可观测），attempts 随之计入。
                                    c.iterations += 1;
                                    warnings.push(format!("{cell_id}: second-stage round failed: {e} — keeping first-round findings (debt)"));
                                    stage2_error = Some(format!("second-stage round failed: {e}"));
                                    Stage2::Failed
                                }
                            }
                        }
                    }
                };
            }
            Ok(c)
        }
    };
    // 门等待与 429 排队只有 policy 知道（`ScanLease::run` 的回填在这条路径上不经过）——两轮结束、lease 还活着时读出来。
    let (rl_retries, rl_wait_ms) = (policy.retries(), policy.waited_ms());
    drop(lease);
    CellDone {
        member,
        rev,
        item,
        cell_id,
        duration_ms: t0.elapsed().as_millis() as u64,
        wait_ms,
        outcome,
        rl_retries,
        rl_wait_ms,
        stage2,
        stage2_error,
        bypass_missing,
        auth_failed,
        warnings,
    }
}

/// reviewer cell warn 片段（与 scanner 格 `scan_cell_notes` 同口径，不静默）：二阶段失败原因、parse 层丢弃、needs 丢弃 / 截断、
/// quote 锚定丢弃、旁路丢弃。丢弃计数不落盘时，「模型零发现」与「finding 全被静默丢掉」在报告里分不出来。
fn review_cell_notes(st: &ReviewParseStats, stage2_error: Option<&str>) -> Vec<String> {
    let mut notes = Vec::new();
    if let Some(reason) = stage2_error {
        notes.push(format!("{reason} — first-round findings kept"));
    }
    if st.dropped_no_quote + st.dropped_bad_path + st.dropped_malformed > 0 {
        notes.push(format!(
            "parse dropped: no_quote={} bad_path={} malformed={}",
            st.dropped_no_quote, st.dropped_bad_path, st.dropped_malformed
        ));
    }
    if st.needs_dropped + st.needs_truncated > 0 {
        notes.push(format!("needs dropped={} truncated={}", st.needs_dropped, st.needs_truncated));
    }
    if st.dropped_quote > 0 {
        notes.push(format!("{} findings dropped: quote not anchored", st.dropped_quote));
    }
    if st.stream_discarded > 0 {
        notes.push(format!("stream discarded {}", st.stream_discarded));
    }
    notes
}

/// 鉴权失败（401 / 403）：穿透 `WithUsage` 包装看内层——多轮中途失败的错误带着已烧 usage。
fn is_auth(e: &ProviderError) -> bool {
    matches!(e.inner(), ProviderError::Auth(_))
}

/// 一轮单发：为本轮新造 per-cell backend 与旁路解析器（旁路要挂在本格独占的 backend 上，共享实例会被并发 cell 抢 delta）；
/// 工厂缺席 / 造不出来 → 退回 lease 的共享 backend（无旁路，照跑）。返回 (结果, 旁路是否缺席)。
async fn shot(
    env: &CellEnv<'_>,
    lease: &ScanLease<'_>,
    policy: &GateAttempts<'_>,
    rev: &EffectiveModel,
    dispatch_agreement: u32,
    prompt: &ScanPrompt,
) -> (Result<ReviewCall, ProviderError>, bool) {
    let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
    let inc_feed = inc.clone();
    let per_cell = env
        .input
        .backend_factory
        .and_then(|f| f(rev, Box::new(move |s: &str| inc_feed.lock().unwrap().feed(s))));
    let missing = per_cell.is_none() && env.input.backend_factory.is_some();
    let backend: &dyn ChatBackend = per_cell.as_deref().unwrap_or(lease.backend());
    let mut ctx = EmitCtx::new(&rev.name, &rev.family, dispatch_agreement, env.input.root, env.sink);
    let r = run_review_shot_with(backend, prompt, lease.per_turn(), lease.retry_budget(), Some(policy), &mut ctx, inc).await;
    (r, missing)
}

/// 完成时刻（主循环内）：回填落地共识 → 落盘 cell → 队列结案 → 快照。报告侧组装留到收场按最终项优先级序做。
fn land(
    done: CellDone<'_>,
    q: &mut DeepQueue,
    snapshot: Option<&dyn Fn(&QueueItem)>,
    on_cell: Option<&dyn Fn(&CellRecord)>,
) -> Landed {
    let CellDone {
        member, rev, item, cell_id, duration_ms, wait_ms, outcome, rl_retries, rl_wait_ms, stage2, stage2_error, bypass_missing, auth_failed, mut warnings,
    } = done;
    // 落地时刻的疑点共识回填到**落盘** finding（design §7「落地的 finding 继承记录当时的 agreement」、§8.1「按落地时刻算、
    // 后涨不回溯」）。cell 内的 `EmitCtx` 只能拿派发时快照；在途并入会让落地值更高，事件侧因此滞后（`event ≤ landed`）。
    let landed = live_agreement(q, &item);
    let mut debt = stage2 == Stage2::Failed;
    let (rec, stream_discarded) = match outcome {
        Ok(mut c) => {
            for f in c.output.findings_mut() {
                f.suspicion_agreement = landed;
            }
            let discarded = c.stats.stream_discarded;
            let notes = review_cell_notes(&c.stats, stage2_error.as_deref());
            let mut rec = CellRecord::ok(&cell_id, &rev.name, c.output).with_family(&rev.family);
            rec.duration_ms = duration_ms;
            rec.input_tokens = c.input_tokens;
            rec.output_tokens = c.output_tokens;
            rec.cached_input_tokens = c.cached_input_tokens;
            rec.iterations = c.iterations;
            // 与 scanner 同口径（orchestrate/mod.rs）：attempts = 轮次 + 429 重试。
            rec.attempts = c.iterations + rl_retries;
            rec.queue_wait_ms = rl_wait_ms;
            rec.template_hash = Some(review_template_hash());
            // 二阶段失败（评审修 #2）：首轮 finding 留在格里（报告与 resume 的 prior findings 都从这里取），但格按失败落盘——
            // 进 coverage.failed、resume 按「Settled 以成功格为准」重挖。落成 ok 形态的话，债只活在本次调用的
            // reviewer_failed 里，resume 报告会把它抹成干净通过。
            rec.ok = stage2_error.is_none();
            rec.warn = (!notes.is_empty()).then(|| notes.join("; "));
            (rec, discarded)
        }
        Err(e) => {
            debt = true;
            // 烧掉的 token 照样入账（scanner 同口径）——失败不等于免费，不记就成了报告里对不上的黑洞。
            let (bi, bo, bc) = e.burned_usage();
            let mut rec = CellRecord::failed(&cell_id, &rev.name, Role::Reviewer, format!("{e}")).with_family(&rev.family);
            rec.duration_ms = duration_ms;
            rec.input_tokens = bi;
            rec.output_tokens = bo;
            rec.cached_input_tokens = bc;
            rec.attempts = 1 + rl_retries;
            rec.queue_wait_ms = rl_wait_ms;
            warnings.push(format!("{cell_id} failed: {e}"));
            (rec, 0)
        }
    };
    // 先落盘 cell（finding 的权威载体）再写队列快照：kill 落在两者之间时，快照仍是 InFlight → resume 重跑，
    // 宁可重挖一格也不丢 finding（design §7.1）。
    snap_cell(&rec, on_cell);
    q.mark_settled(&item.id, landed);
    if let Some(it) = q.get(&item.id) {
        snap_one(it, snapshot);
    }
    Landed {
        item_id: item.id.clone(),
        member,
        auth_failed,
        rec,
        wait_ms,
        dispatch_agreement: item.dispatch_agreement,
        landed,
        stage2,
        stream_discarded,
        bypass_missing,
        debt,
        warnings,
    }
}

/// 经入队通道到达一项（scanner 流式扇出）：走入口五态（同一到达键再来记 `Duplicate`）+ 入队原因 + 队列深度峰值（运行中取最大）；只写受影响项的快照
/// （新建项 / 并入目标；重复到达不写，C2 Q9〔D15〕）。可派与否由主循环的在线额度判定。`None` = 发送端全部 drop → 通道关闭。
fn on_arrival(
    msg: Option<EnqueueReq>,
    q: &mut DeepQueue,
    meta: &mut DeepMeta,
    closed: &mut bool,
    snapshot: Option<&dyn Fn(&QueueItem)>,
    ctx: &mut BlockCtx,
) {
    let Some(req) = msg else {
        *closed = true;
        return;
    };
    let reason = match req.spec.origin {
        QueueOrigin::Rule => "rule",
        QueueOrigin::Scan => "scan",
        QueueOrigin::Needs => "needs",
    };
    if !ctx.accepts(&req) {
        *meta.enqueue_reasons.entry("needs_out_of_diff".into()).or_insert(0) += 1;
        return;
    }
    let (outcome, affected) = q.enqueue_affected(ctx.arrival(req));
    note_enq(outcome, reason, meta);
    if let Some(i) = affected {
        snap_one(&q.items()[i], snapshot);
    }
    meta.queue_depth_peak = meta.queue_depth_peak.max(q.items().len() as u32);
}

/// 通道收一项；无通道时永不就绪（`select!` 分支另由 `closed` 守卫关掉）。
async fn recv(rx: &mut Option<mpsc::Receiver<EnqueueReq>>) -> Option<EnqueueReq> {
    match rx {
        Some(r) => r.recv().await,
        None => std::future::pending().await,
    }
}

/// reviewer cell 名：`review[{队列项 id}]@{reviewer}`——按队列项定死、跨 resume 稳定（C1-1：按派发序编号时 resume 会同名覆盖别项的格）。
pub fn review_cell_id(item_id: &str, reviewer: &str) -> String {
    format!("{}{reviewer}", review_cell_prefix(item_id))
}

/// 某队列项的 reviewer cell 名前缀（与 [`review_cell_id`] 同源）：resume 对账按它认格，不论哪个 reviewer 挖的。
pub fn review_cell_prefix(item_id: &str) -> String {
    format!("review[{item_id}]@")
}

/// resume 形态的深挖 meta 起点：截断数与队列深度取自快照。正常续跑会在收口时按最终状态重算截断数（C2〔D15〕起 restore 模式
/// 同样做收场 drain）；挖不动的两条早退路径——变更集为空、diff 重建报错（`cli::run_resume`）——就以快照读数收口，
/// 漏掉它被预算截掉的项就既不计债也不披露。
pub fn restored_meta(items: &[QueueItem]) -> DeepMeta {
    DeepMeta {
        budget_skipped: items.iter().filter(|i| i.state == ItemState::Skipped).count() as u32,
        queue_depth_peak: items.len() as u32,
        ..Default::default()
    }
}

/// 调用方保证 `bundles` 非空（`run_deep` 开头已早退）。
fn pick_bundle<'a>(bundles: &'a [Bundle], file: &str) -> &'a Bundle {
    bundles.iter().find(|b| b.files.iter().any(|f| f.path == file)).unwrap_or(&bundles[0])
}

/// 落地时 agreement：按 id 读队列活值（id 定死、在途并入不改它，恒能查到）；查不到退回派发时值只是防御。
fn live_agreement(q: &DeepQueue, item: &QueueItem) -> u32 {
    q.get(&item.id).map(|it| it.agreement).unwrap_or(item.agreement)
}

/// R2 二阶段第二轮的 prompt 准备结局。
enum Round2 {
    /// 全部 needs 被读守卫拒绝 / 不可读 → 无证据，第二轮不发。
    NoEvidence,
    /// 建 pack 内部错误（首轮已合格过互斥检查，理论不该发生）。
    PackFailed(String),
    Ready(ScanPrompt),
}

/// R2 二阶段补查轮的 prompt：needs → 证据段（`second_pass::need_section`，读守卫 + 字节帽）→ 重建 pack
/// （needs_evidence 槽；与笔记互斥由构造期守卫，调用方已过滤）→ prompt 附加草稿块。
fn second_stage_prompt(
    env: &CellEnv<'_>,
    item: &QueueItem,
    draft: &[Finding],
    needs: &[crate::domain::NeedEntry],
    warnings: &mut Vec<String>,
) -> Round2 {
    let input = env.input;
    let ex = crate::tools::ToolExecutor::new(input.root);
    // 读守卫拒绝（denylist / 非 git-tracked）必须进报告——模型伸手要 ~/.ssh/id_rsa 这种事
    // 只打到 stderr 就等于没披露（second_pass.rs 的 deny_need_read 契约，T46①②）。
    let ev: String = needs
        .iter()
        .filter_map(|n| crate::cli::second_pass::need_section(&ex, env.tracked, n, warnings))
        .collect();
    if ev.trim().is_empty() {
        return Round2::NoEvidence;
    }
    let pack2 = match region::build(&item.spec, pick_bundle(input.bundles, &item.spec.file), input.root, env.tracked, env.idx, 64 * 1024, env.opts, &ev) {
        Ok(p) => p,
        Err(e) => return Round2::PackFailed(e),
    };
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
        text: crate::cli::second_pass::draft_block(draft),
        cache_boundary: false,
    });
    Round2::Ready(prompt2)
}

fn file_diff(bundles: &[Bundle], file: &str) -> String {
    bundles.iter().flat_map(|b| b.files.iter()).find(|f| f.path == file).map(|f| f.diff.clone()).unwrap_or_default()
}

fn enqueue_h0(q: &mut DeepQueue, h: &Hotspot, meta: &mut DeepMeta, snapshot: Option<&dyn Fn(&QueueItem)>, ctx: &mut BlockCtx) {
    let start = (h.start as u32).max(1);
    let end = (h.end as u32).max(start);
    let Ok(spec) = QueueItemSpec::new(h.file.clone(), vec![(start, end)], QueueOrigin::Rule, vec![]) else {
        return;
    };
    let kind = match h.kind {
        RegionKind::Primary => ItemKind::Primary,
        RegionKind::Ripple => ItemKind::Ripple,
    };
    // H0 也记到达键（评审 #8）：resume 每轮按当前 diff 幂等重建 H0——首跑那次入队快照写失败时它才补得回来，
    // 已在队列里的则走到达键幂等、不改项也不计去重命中。
    let arrival = Some(format!("rule:{}:{start}-{end}", h.file));
    let req = EnqueueReq { spec, score: h.score, kind, families: vec![], arrival };
    let (outcome, affected) = q.enqueue_affected(ctx.arrival(req));
    note_enq(outcome, "rule", meta);
    if let Some(i) = affected {
        snap_one(&q.items()[i], snapshot);
    }
}

/// 已完成 scan 格的疑点逐格逐条入队（resume 回放 / 未走通道的调用方）：与首跑流式到达同一形态（`EnqueueReq::scan`、
/// 到达键 `{cell_id}#{i}`）——已经到达过的疑点幂等跳过；格按 cell id 序、格内按疑点下标序，结果与喂入顺序无关。
/// 失败格（`output == None`）无疑点。只写受影响项的快照。
fn enqueue_scan_cells(
    q: &mut DeepQueue,
    cells: &[ScanCellResult],
    meta: &mut DeepMeta,
    snapshot: Option<&dyn Fn(&QueueItem)>,
    ctx: &mut BlockCtx,
) {
    let mut ordered: Vec<&ScanCellResult> = cells.iter().filter(|c| c.output.is_some()).collect();
    ordered.sort_by(|a, b| a.cell_id.cmp(&b.cell_id));
    for c in ordered {
        let Some(out) = &c.output else { continue };
        for (i, s) in out.suspicions.iter().enumerate() {
            let Some(req) = EnqueueReq::scan(s, &c.family, crate::consolidate::scan::arrival_key(&c.cell_id, i, s)) else {
                continue;
            };
            let (outcome, affected) = q.enqueue_affected(ctx.arrival(req));
            note_enq(outcome, "scan", meta);
            if let Some(idx) = affected {
                snap_one(&q.items()[idx], snapshot);
            }
        }
        // needs 与首跑同形（C3〔D16〕）：疑点之后、变更集内的才入队，到达键幂等 → 重放不重复。
        for n in &out.needs {
            let Some(req) = EnqueueReq::need(n) else { continue };
            if !ctx.accepts(&req) {
                *meta.enqueue_reasons.entry("needs_out_of_diff".into()).or_insert(0) += 1;
                continue;
            }
            let (outcome, affected) = q.enqueue_affected(ctx.arrival(req));
            note_enq(outcome, "needs", meta);
            if let Some(idx) = affected {
                snap_one(&q.items()[idx], snapshot);
            }
        }
    }
}

/// 块与风险档的入队侧上下文（C3〔D16〕）：稳定块要读文件内容、风险档要查 `RiskRoute.zones`。
/// 两者都在主循环的同步段里算——内容按文件缓存一次（变更集通常十来个文件），读失败退空内容（`block_of` 退行带）。
/// 读取限在 `root` 之下并设体积帽：`file` 来自模型输出，不能当可信路径用。
struct BlockCtx<'a> {
    root: &'a Path,
    zones: &'a [Hotspot],
    /// 本轮变更集（bundles 覆盖的文件）：needs 只接落在里面的（C3〔D16〕Q8）。
    changed: std::collections::HashSet<&'a str>,
    cache: std::collections::HashMap<String, String>,
}

/// 算块时读单文件的体积上限：超过即退行带（reviewer 也塞不下这种文件）。
const MAX_BLOCK_FILE_BYTES: u64 = 2 * 1024 * 1024;

impl<'a> BlockCtx<'a> {
    fn new(root: &'a Path, route: Option<&'a RiskRoute>, bundles: &'a [Bundle]) -> Self {
        BlockCtx {
            root,
            zones: route.map_or(&[][..], |r| &r.zones),
            changed: bundles.iter().flat_map(|b| b.files.iter()).map(|f| f.path.as_str()).collect(),
            cache: std::collections::HashMap::new(),
        }
    }

    /// 变更集外的 needs 不入队（C3〔D16〕Q8）：两轮实测约六成 needs 指向 diff 外，而 `pick_bundle` 对不在任何 bundle 的文件
    /// 会静默退回 `bundles[0]`，产出「文件名是 A、证据是 B」的错配包。装包归属另开批（C4 / M21d），本批只计数。
    fn accepts(&self, req: &EnqueueReq) -> bool {
        req.spec.origin != QueueOrigin::Needs || self.changed.contains(req.spec.file.as_str())
    }

    fn content(&mut self, file: &str) -> &str {
        if !self.cache.contains_key(file) {
            let text = read_capped(self.root, file);
            self.cache.insert(file.to_string(), text);
        }
        &self.cache[file]
    }

    /// 请求 → 到达（带块与风险档）。H0（Rule）按区间相交合并、身份口径不变，只算风险档。
    fn arrival(&mut self, req: EnqueueReq) -> crate::orchestrate::queue::Arrival {
        let range = req.spec.ranges.first().copied().unwrap_or((1, 1));
        if req.spec.origin == QueueOrigin::Rule {
            let band = self.band_for(&req.spec.file, range);
            return crate::orchestrate::queue::Arrival { req, block: Default::default(), risk_band: band };
        }
        let file = req.spec.file.clone();
        let block = {
            let content = self.content(&file);
            crate::orchestrate::blocks::block_of(&file, content, range.0)
        };
        let band = self.band_for(&file, (block.start, block.end));
        crate::orchestrate::queue::Arrival { req, block, risk_band: band }
    }

    /// 与本项区间重叠的风险区里**分最高**的那个 → 档位；无重叠区 = 0。
    fn band_for(&self, file: &str, range: (u32, u32)) -> u8 {
        let best = self
            .zones
            .iter()
            .filter(|z| z.file == file && z.start as u32 <= range.1 && range.0 <= z.end as u32)
            .map(|z| z.score)
            .fold(0.0_f32, f32::max);
        risk_band_of(best)
    }
}

/// 风险分 → 档（design C3 §4）：锚在既有常数 θ 上，不另立阈值。
fn risk_band_of(score: f32) -> u8 {
    if score >= crate::risk::THETA {
        3
    } else if score >= crate::risk::THETA / 2.0 {
        2
    } else if score > 0.0 {
        1
    } else {
        0
    }
}

fn read_capped(root: &Path, rel: &str) -> String {
    let Ok(real) = root.join(rel).canonicalize() else {
        return String::new();
    };
    let base = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !real.starts_with(&base) {
        return String::new();
    }
    match std::fs::metadata(&real) {
        Ok(m) if m.is_file() && m.len() <= MAX_BLOCK_FILE_BYTES => std::fs::read_to_string(&real).unwrap_or_default(),
        _ => String::new(),
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
    // 到达并被接受的疑点数（C3〔D16〕观测）：合并进已有项的也算到达，只有到达键重复的不算。
    if reason == "scan" && o != EnqueueOutcome::Duplicate {
        meta.suspicions_arrived += 1;
    }
    match o {
        EnqueueOutcome::New => {
            *meta.enqueue_reasons.entry(reason.into()).or_insert(0) += 1;
        }
        EnqueueOutcome::MergedPending
        | EnqueueOutcome::MergedInFlight
        | EnqueueOutcome::MergedSettled
        | EnqueueOutcome::MergedSkipped => {
            meta.dedup_hits += 1;
        }
        // 同一条疑点再次到达（resume 幂等入队）：队列没有任何变化，不是去重命中
        EnqueueOutcome::Duplicate => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
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
            arrivals: vec![], members: vec![], member_families: vec![], block: Default::default(), peak_agreement: 0, risk_band: 0,
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
                backend_factory: None, on_cell: None, incoming: None, reopen_skipped: false,
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
                backend_factory: None, on_cell: None, incoming: None, reopen_skipped: false,
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

    /// reviewer cell_id 按队列项（批 C1-1）：`review[{item.id}]@{reviewer}`——同项重跑 upsert 覆盖自身、
    /// 不同项永不同名。批 C 按单次调用的派发序编号（每次调用从 1 起），resume 挖别的项时同名覆盖了首跑的格。
    #[tokio::test]
    async fn reviewer_cell_id_follows_queue_item() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = one_bundle(dir.path());
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![turn(&rev_json("fn a() {"))])), Duration::from_secs(5), 0);
        let run = run_one(&reg, dir.path(), &bundles, ToolsMode::Off).await;
        let names: Vec<&str> = run.cells.iter().map(|c| c.cell.as_str()).collect();
        assert_eq!(names, vec!["review[a.rs:rule:1-2]@p/r"]);
    }

    /// 并进被截项（`MergedSkipped`）同样是一次去重命中：入队没有新增挖掘。
    #[test]
    fn merged_skipped_counts_as_dedup_hit() {
        let mut meta = DeepMeta::default();
        note_enq(EnqueueOutcome::MergedSkipped, "scan", &mut meta);
        assert_eq!((meta.dedup_hits, meta.enqueue_reasons.len()), (1, 0));
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
                backend_factory: None, on_cell: None, incoming: None, reopen_skipped: false,
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
                backend_factory: None, on_cell: None, incoming: None, reopen_skipped: false,
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
                tools: ToolsMode::Off, backend_factory: None, on_cell: None, incoming: None, reopen_skipped: false,
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
                tools: ToolsMode::TwoStage, backend_factory: None, on_cell: None, incoming: None, reopen_skipped: false,
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
        let backend = Arc::new(FakeChatBackend::new(vec![turn(first), turn(&second)]));
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(Arc::clone(&backend)), Duration::from_secs(5), 0);
        let run = run_one(&reg, root, &bundles, ToolsMode::TwoStage).await;
        assert!(
            run.warnings.iter().any(|w| w.contains("untracked.txt")),
            "守卫拒绝未披露: {:?}", run.warnings
        );
        // 契约（C1 §5）：全部 needs 被拒 = 无证据，第二轮不发 → skipped、不计 triggered、不计债
        assert_eq!(backend.turn_count(), 1, "第二轮不该发出");
        assert_eq!((run.meta.needs_two_stage_triggered, run.meta.needs_two_stage_skipped), (0, 1));
        assert!(!run.reviewer_failed, "全被拒不计债: {:?}", run.warnings);
    }

    // ── 批 C1-2：门驱动并发派发 ────────────────────────────────────────────────

    /// 按 prompt 里的路由标记（`ROUTE_<name>`，只出现在该文件自己的 diff 里）分派的测试 backend：
    /// 每个路由一条逐轮弹出的脚本 + 固定时延；记录每轮**开始**的路由序，时序断言用。
    type Script = Vec<Result<AssistantTurn, ProviderError>>;
    /// 路由名 → (每轮时延, 逐轮弹出的脚本)。
    type Routes = BTreeMap<String, (Duration, std::collections::VecDeque<Result<AssistantTurn, ProviderError>>)>;

    struct Routed {
        routes: Mutex<Routes>,
        starts: Mutex<Vec<String>>,
    }

    impl Routed {
        fn new(routes: Vec<(&str, u64, Script)>) -> Arc<Self> {
            let routes = routes
                .into_iter()
                .map(|(k, ms, script)| (k.to_string(), (Duration::from_millis(ms), script.into())))
                .collect();
            Arc::new(Routed { routes: Mutex::new(routes), starts: Mutex::new(Vec::new()) })
        }
        fn starts(&self) -> Vec<String> {
            self.starts.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ChatBackend for Routed {
        async fn turn(
            &self,
            messages: &[crate::provider::tool_loop::ChatMessage],
            _tools: &[crate::tools::ToolSpec],
        ) -> Result<AssistantTurn, ProviderError> {
            let text = format!("{messages:?}");
            let (key, delay) = {
                let routes = self.routes.lock().unwrap();
                let (k, (d, _)) = routes.iter().find(|(k, _)| text.contains(&format!("ROUTE_{k}\\n"))).expect("prompt 里没有路由标记");
                (k.clone(), *d)
            };
            self.starts.lock().unwrap().push(key.clone());
            tokio::time::sleep(delay).await;
            let next = self.routes.lock().unwrap().get_mut(&key).unwrap().1.pop_front();
            next.unwrap_or_else(|| Err(ProviderError::Unavailable(format!("route {key}: no scripted turn"))))
        }
    }

    /// 每个名字一个文件 `<n>.rs`（首行 `fn <n>() {`）；diff 里带路由标记。
    fn routed_bundles(dir: &Path, names: &[&str]) -> Vec<Bundle> {
        let changes = names
            .iter()
            .map(|n| {
                std::fs::write(dir.join(format!("{n}.rs")), format!("fn {n}() {{\n    body();\n}}\n")).unwrap();
                FileChange { path: format!("{n}.rs"), diff: format!("@@ -1,1 +1,2 @@\n+fn {n}() {{\n+// ROUTE_{n}\n") }
            })
            .collect();
        vec![Bundle::from_files(changes)]
    }

    /// 锚得上 `<n>.rs` 首行的一条 finding。
    fn found(n: &str) -> Result<AssistantTurn, ProviderError> {
        Ok(turn(&format!(
            r#"{{"findings":[{{"severity":"low","title":"issue in {n}","body":"b","file":"{n}.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"fn {n}() {{"}}],"needs":[]}}"#
        )))
    }

    fn routed_registry(backend: &Arc<Routed>, cap: u32) -> crate::provider::registry::ProviderRegistry {
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", cap, Box::new(Arc::clone(backend)), Duration::from_secs(60), 0);
        reg
    }

    fn pending_items(names: &[&str]) -> Vec<QueueItem> {
        names.iter().map(|n| snap_item(&format!("{n}.rs"), ItemState::Pending)).collect()
    }

    struct Run<'a> {
        reg: &'a crate::provider::registry::ProviderRegistry,
        root: &'a Path,
        bundles: &'a [Bundle],
        items: &'a [QueueItem],
        tools: ToolsMode,
        incoming: Option<mpsc::Receiver<EnqueueReq>>,
        on_cell: Option<&'a dyn Fn(&CellRecord)>,
        snapshot: Option<&'a dyn Fn(&QueueItem)>,
        sink: &'a dyn FindingSink,
        /// `None` = 单成员 `p/r`（`fake_reviewer`）。
        reviewers: Option<&'a [EffectiveModel]>,
        /// 预算 K（`plain` 默认 16）。
        budget: u32,
        /// `true` = 首跑形态（`restore: None`，`items` 不用）；`false` = restore 模式、`items` 为快照。
        first_run: bool,
        scan_cells: &'a [ScanCellResult],
        reopen_skipped: bool,
        route: Option<&'a RiskRoute>,
    }

    async fn run_with(r: Run<'_>) -> DeepRun {
        let single = [fake_reviewer()];
        run_deep(
            r.reg,
            DeepInput {
                route: r.route, scan_cells: r.scan_cells, bundles: r.bundles, root: r.root,
                reviewers: r.reviewers.unwrap_or(&single), budget_cells: r.budget,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: r.snapshot, restore: (!r.first_run).then_some(r.items), prior_findings: vec![], tools: r.tools,
                backend_factory: None, on_cell: r.on_cell, incoming: r.incoming, reopen_skipped: r.reopen_skipped,
            },
            r.sink,
        )
        .await
    }

    fn plain<'a>(reg: &'a crate::provider::registry::ProviderRegistry, root: &'a Path, bundles: &'a [Bundle], items: &'a [QueueItem]) -> Run<'a> {
        Run {
            reg, root, bundles, items, tools: ToolsMode::Off, incoming: None, on_cell: None, snapshot: None, sink: &(), reviewers: None,
            budget: 16, first_run: false, scan_cells: &[], reopen_skipped: false, route: None,
        }
    }

    fn git_commit_all(root: &Path) {
        let g = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(root).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        g(&["init", "-q"]);
        g(&["add", "."]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
    }

    /// 在途峰值 = 门宽，墙钟 ≈ ceil(K/W) × 单格时延（design §6 规则 5：门有空位就派，不串行）。
    #[tokio::test(start_paused = true)]
    async fn inflight_peak_equals_gate_width() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c", "d", "e"];
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(names.iter().map(|n| (*n, 50, vec![found(n)])).collect());
        let reg = routed_registry(&backend, 2);
        let items = pending_items(&names);
        let t0 = tokio::time::Instant::now();
        let run = run_with(plain(&reg, dir.path(), &bundles, &items)).await;
        let elapsed = t0.elapsed();
        assert_eq!(reg.gate_stats()["p"].peak, 2, "在途峰值 = 门宽");
        assert_eq!(run.cells.iter().filter(|c| c.ok).count(), 5, "{:?}", run.warnings);
        assert!(elapsed >= Duration::from_millis(150) && elapsed < Duration::from_millis(200), "ceil(5/2)×50 ms，实测 {elapsed:?}");
    }

    /// 乱序完成不错配、慢格不堵：完成序 b→c→a，每格 findings 仍对应自己的文件；报告里的 cells 按优先级序（a→b→c，C2 D2 起不看派发序）；
    /// 墙钟 ≈ 最慢格而非三格之和。
    #[tokio::test(start_paused = true)]
    async fn out_of_order_completion_lands_on_own_item_and_slow_cell_does_not_block() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c"];
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(vec![("a", 300, vec![found("a")]), ("b", 10, vec![found("b")]), ("c", 20, vec![found("c")])]);
        let reg = routed_registry(&backend, 3);
        let items = pending_items(&names);
        let landed_order = Mutex::new(Vec::new());
        let on_cell = |rec: &CellRecord| landed_order.lock().unwrap().push(rec.cell.clone());
        let t0 = tokio::time::Instant::now();
        let run = run_with(Run { on_cell: Some(&on_cell), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert!(t0.elapsed() < Duration::from_millis(330), "慢格不堵其余：{:?}", t0.elapsed());
        let order = landed_order.lock().unwrap().clone();
        assert_eq!(order, vec!["review[b.rs:rule:1-2]@p/r", "review[c.rs:rule:1-2]@p/r", "review[a.rs:rule:1-2]@p/r"], "完成即落盘");
        let report_order: Vec<&str> = run.cells.iter().map(|c| c.cell.as_str()).collect();
        assert_eq!(report_order, vec!["review[a.rs:rule:1-2]@p/r", "review[b.rs:rule:1-2]@p/r", "review[c.rs:rule:1-2]@p/r"], "报告按优先级序");
        for c in &run.cells {
            let file = c.cell.trim_start_matches("review[").split(':').next().unwrap();
            let fs = c.output.findings();
            assert!(fs.len() == 1 && fs[0].file == file, "{} 错配: {fs:?}", c.cell);
        }
    }

    /// 二阶段持门位（Q4）：门宽 1 时首格两轮之间不放门——第二格在首格补查轮结束后才开始。
    #[tokio::test(start_paused = true)]
    async fn second_stage_round_holds_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a", "b"]);
        git_commit_all(dir.path());
        let with_needs = Ok(turn(r#"{"findings":[],"needs":[{"path":"a.rs","reason":"need context"}]}"#));
        let backend = Routed::new(vec![("a", 50, vec![with_needs, found("a")]), ("b", 50, vec![found("b")])]);
        let reg = routed_registry(&backend, 1);
        let items = pending_items(&["a", "b"]);
        let run = run_with(Run { tools: ToolsMode::TwoStage, ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(backend.starts(), vec!["a", "a", "b"], "补查轮必须紧接首轮，不与新派发的格抢门");
        assert_eq!(run.meta.needs_two_stage_triggered, 1);
        assert_eq!(run.cells.iter().filter(|c| c.ok).count(), 2, "{:?}", run.warnings);
    }

    struct Events(Mutex<Vec<crate::orchestrate::review_shot::FindingEvent>>);
    impl FindingSink for Events {
        fn on_finding(&self, ev: crate::orchestrate::review_shot::FindingEvent) {
            self.0.lock().unwrap().push(ev);
        }
    }

    fn scan_item(file: &str, quote: &str, family: &str) -> (QueueItem, QueueItemSpec) {
        let note = crate::domain::Suspicion {
            file: file.into(), line_start: 1, line_end: 1, hint: "h".into(),
            category: None, confidence: 0.5, quote: quote.into(),
        };
        let spec = QueueItemSpec::new(file.into(), vec![(1, 1)], QueueOrigin::Scan, vec![note]).unwrap();
        let item = QueueItem {
            id: format!("{file}:scan:1-1"), spec: spec.clone(), score: 0.5, agreement: 1, state: ItemState::Pending,
            kind: ItemKind::Suspicion, dispatch_agreement: 0, landed_agreement: None,
            families: vec![family.into()], enqueued_ms: None,
            arrivals: vec![], members: vec![], member_families: vec![], block: Default::default(), peak_agreement: 0, risk_band: 0,
        };
        (item, spec)
    }

    /// 经真实通道的在途并入（design C1 §3.5）：派发时共识 1，挖的过程中异族谓词并入抬到 2 →
    /// 落盘 finding = 落地值 = `landed_agreement` = 2；事件侧是派发快照 1，严格小于落地值（`event ≤ landed`，偏差只会多复核）。
    #[tokio::test(start_paused = true)]
    async fn inflight_merge_via_channel_forks_event_below_landed() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a"]);
        let backend = Routed::new(vec![("a", 100, vec![found("a")])]);
        let reg = routed_registry(&backend, 2);
        let (item, spec) = scan_item("a.rs", "fn a() {", "alpha");
        let items = [item];
        let (tx, rx) = mpsc::channel(4);
        let events = Events(Mutex::new(Vec::new()));
        let feed = async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            tx.send(EnqueueReq { spec, score: 0.5, kind: ItemKind::Suspicion, families: vec!["beta".into()], arrival: None }).await.unwrap();
        };
        let (run, ()) = tokio::join!(
            run_with(Run { incoming: Some(rx), sink: &events, ..plain(&reg, dir.path(), &bundles, &items) }),
            feed,
        );
        assert_eq!(run.meta.dedup_hits, 1, "在途并入走五态（MergedInFlight）");
        assert_eq!((run.meta.dispatch_agreement.clone(), run.meta.landed_agreement.clone()), (vec![1], vec![2]));
        assert_eq!(run.findings.len(), 1, "{:?}", run.warnings);
        assert_eq!(run.findings[0].suspicion_agreement, 2, "报告 finding 带落地值");
        assert_eq!(run.cells[0].output.findings()[0].suspicion_agreement, 2, "落盘 = 落地值");
        let evs = events.0.lock().unwrap();
        assert!(!evs.is_empty(), "终判至少发一条事件");
        assert!(evs.iter().all(|e| e.suspicion_agreement == 1), "事件侧 = 派发快照");
        assert!(evs.iter().all(|e| e.suspicion_agreement < run.findings[0].suspicion_agreement), "event < landed");
        assert_eq!(evs[0].finding.suspicion_agreement, evs[0].suspicion_agreement, "事件载荷内外一致");
    }

    /// 通道未关不收场（barrier 含通道关闭）：队列一开始就空，稍后经通道到达一项 → 照挖；发送端 drop 后才收场。
    #[tokio::test(start_paused = true)]
    async fn open_channel_defers_the_barrier() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a"]);
        let backend = Routed::new(vec![("a", 10, vec![found("a")])]);
        let reg = routed_registry(&backend, 2);
        let (tx, rx) = mpsc::channel(4);
        let feed = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let spec = QueueItemSpec::new("a.rs".into(), vec![(1, 2)], QueueOrigin::Rule, vec![]).unwrap();
            tx.send(EnqueueReq { spec, score: 1.0, kind: ItemKind::Primary, families: vec![], arrival: None }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let t0 = tokio::time::Instant::now();
        let (run, ()) = tokio::join!(run_with(Run { incoming: Some(rx), ..plain(&reg, dir.path(), &bundles, &[]) }), feed);
        assert!(t0.elapsed() >= Duration::from_millis(150), "发送端 drop 前不得收场：{:?}", t0.elapsed());
        assert_eq!(run.cells.len(), 1, "到达项被挖: {:?}", run.warnings);
        assert_eq!(run.meta.enqueue_reasons.get("rule"), Some(&1));
        assert_eq!(run.meta.queue_depth_peak, 1, "运行中取最大值");
    }

    /// 报告确定性（Q6）：同一输入、完成时延完全反过来的两次运行，报告（除计时字段）逐字节一致；
    /// meta 四数组与 cells 等长且按优先级序对齐（失败格也各占一位）。
    #[tokio::test(start_paused = true)]
    async fn report_is_deterministic_under_shuffled_completion() {
        async fn once(delays: [u64; 3]) -> String {
            let dir = tempfile::tempdir().unwrap();
            let names = ["a", "b", "c"];
            let bundles = routed_bundles(dir.path(), &names);
            let bad = Err(ProviderError::BadRequest("boom".into()).attach_usage((5, 7, 0)));
            let backend = Routed::new(vec![
                ("a", delays[0], vec![found("a")]),
                ("b", delays[1], vec![bad]),
                ("c", delays[2], vec![found("c")]),
            ]);
            let reg = routed_registry(&backend, 3);
            let items = pending_items(&names);
            let mut run = run_with(plain(&reg, dir.path(), &bundles, &items)).await;
            let n = run.cells.len();
            let m = &run.meta;
            assert_eq!(
                (m.item_wait_ms.len(), m.dispatch_agreement.len(), m.landed_agreement.len(), m.reviewer_output_tokens.len()),
                (n, n, n, n),
                "meta 四数组逐格对齐"
            );
            assert_eq!(m.reviewer_output_tokens, vec![1, 7, 1], "失败格记烧掉的 output、按优先级序");
            for c in run.cells.iter_mut() {
                c.duration_ms = 0;
                c.queue_wait_ms = 0;
            }
            run.meta.item_wait_ms.clear();
            // 计时字段不在确定性保证内（评审 #11 起阻塞观测含落地段，随真实耗时波动）
            (run.meta.close_ms, run.meta.first_dispatch_ms, run.meta.drain_done_ms) = (0, 0, 0);
            (run.meta.fill_block_max_ms, run.meta.fill_block_total_ms) = (0, 0);
            serde_json::to_string(&(&run.findings, &run.cells, &run.meta, &run.warnings, run.reviewer_failed)).unwrap()
        }
        let forward = once([10, 20, 30]).await;
        let backward = once([30, 20, 10]).await;
        assert_eq!(forward, backward);
    }

    /// 二阶段第二轮失败（Q5）：保留首轮 finding + warning + 计债；第二轮烧掉的 token 入账；triggered 计一次。
    #[tokio::test(start_paused = true)]
    async fn second_stage_failure_keeps_first_round_findings_and_books_debt() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a"]);
        git_commit_all(dir.path());
        let draft = Ok(turn(r#"{"findings":[{"severity":"low","title":"draft","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.4,"recommendation":"r","quote":"fn a() {"}],"needs":[{"path":"a.rs","reason":"need context"}]}"#));
        let boom = Err(ProviderError::BadRequest("round 2 down".into()).attach_usage((3, 4, 0)));
        let backend = Routed::new(vec![("a", 10, vec![draft, boom])]);
        let reg = routed_registry(&backend, 1);
        let items = pending_items(&["a"]);
        let run = run_with(Run { tools: ToolsMode::TwoStage, ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert!(run.reviewer_failed, "第二轮失败计债（exit 2）");
        assert_eq!(run.meta.needs_two_stage_triggered, 1);
        assert_eq!(run.findings.len(), 1);
        assert_eq!(run.findings[0].title, "draft", "保留首轮 finding");
        assert_eq!((run.cells[0].input_tokens, run.cells[0].output_tokens), (1 + 3, 1 + 4), "第二轮烧掉的 token 入账");
        assert!(run.warnings.iter().any(|w| w.contains("second-stage round failed")), "{:?}", run.warnings);
        // 格按失败落盘（评审修 #2）：进 coverage.failed、resume 按「Settled 以成功格为准」重挖；落成 ok 形态的话债只活在本次调用里
        assert!(!run.cells[0].ok, "二阶段失败的格不能以 ok 形态落盘");
        assert_eq!(run.cells[0].output.findings().len(), 1, "失败格仍携带首轮 finding（resume 的 prior findings 载体）");
        assert!(run.cells[0].warn.as_deref().unwrap_or("").contains("second-stage round failed"), "{:?}", run.cells[0].warn);
        // 第二轮确实发出过（token 已入账）→ 轮次与尝试数也要算上它（评审修 #8）
        assert_eq!((run.cells[0].iterations, run.cells[0].attempts), (2, 2), "首轮 1 + 第二轮 1");
    }

    /// 429 注入：门收窄后照样全部完成、无死锁（虚拟时间下死锁会撞 timeout 而不是挂住）。
    #[tokio::test(start_paused = true)]
    async fn rate_limit_injection_converges_without_deadlock() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c"];
        let bundles = routed_bundles(dir.path(), &names);
        let limited = Err(ProviderError::RateLimited { retry_after: None });
        let backend = Routed::new(vec![("a", 50, vec![limited, found("a")]), ("b", 50, vec![found("b")]), ("c", 50, vec![found("c")])]);
        let reg = routed_registry(&backend, 2);
        let items = pending_items(&names);
        let run = tokio::time::timeout(Duration::from_secs(600), run_with(plain(&reg, dir.path(), &bundles, &items)))
            .await
            .expect("429 收门后死锁");
        assert_eq!(run.cells.iter().filter(|c| c.ok).count(), 3, "{:?}", run.warnings);
        let st = reg.gate_stats()["p"];
        assert_eq!((st.rate_limited, st.effective_cap), (1, 1), "撞一次 429、门收到 1");
        let a = run.cells.iter().find(|c| c.cell.starts_with("review[a.rs")).unwrap();
        assert_eq!(a.attempts, 2, "attempts = 轮次 1 + 429 重试 1");
    }

    // ── 批 C1-3：reviewer 分派池 ─────────────────────────────────────────────

    fn member(name: &str, provider: &str) -> EffectiveModel {
        let id = name.rsplit('/').next().unwrap().to_string();
        EffectiveModel { name: name.into(), provider: provider.into(), family: id.clone(), id, ..fake_reviewer() }
    }

    /// (route_key, provider, 门宽, backend) 逐条注册；同 provider 的门按首条门宽建立并共享。
    fn pool_registry(entries: &[(&str, &str, u32, &Arc<Routed>)]) -> crate::provider::registry::ProviderRegistry {
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        for (key, provider, cap, backend) in entries {
            reg.insert_scan(*key, provider, *cap, Box::new(Arc::clone(backend)), Duration::from_secs(60), 0);
        }
        reg
    }

    fn placed(run: &DeepRun) -> Vec<(String, String)> {
        run.cells.iter().map(|c| (c.cell.clone(), c.model.clone())).collect()
    }

    fn cell_of(file: &str, model: &str) -> (String, String) {
        (format!("review[{file}:rule:1-2]@{model}"), model.to_string())
    }

    /// 跨 provider 各门独立（门宽各 1）：两格并行、分落两个成员，每项只交一个成员（不双审）。
    #[tokio::test(start_paused = true)]
    async fn pool_members_on_separate_gates_dig_in_parallel() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a", "b"]);
        let pa = Routed::new(vec![("a", 50, vec![found("a")]), ("b", 50, vec![found("b")])]);
        let qb = Routed::new(vec![("a", 50, vec![found("a")]), ("b", 50, vec![found("b")])]);
        let reg = pool_registry(&[("p/r", "p", 1, &pa), ("q/s", "q", 1, &qb)]);
        let members = [member("p/r", "p"), member("q/s", "q")];
        let items = pending_items(&["a", "b"]);
        let t0 = tokio::time::Instant::now();
        let run = run_with(Run { reviewers: Some(&members), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert!(t0.elapsed() < Duration::from_millis(100), "两门并行：{:?}", t0.elapsed());
        assert_eq!(placed(&run), vec![cell_of("a.rs", "p/r"), cell_of("b.rs", "q/s")]);
        assert_eq!((pa.starts(), qb.starts()), (vec!["a".to_string()], vec!["b".to_string()]), "每项只交一个成员");
        assert!(run.warnings.is_empty(), "{:?}", run.warnings);
    }

    /// 同 provider 共享一道门：轮转只决定空位归谁——两个成员交替领项（批 C dogfood 的 glm-5.3 + grok-4.6 同在 agc 即此形态）。
    #[tokio::test(start_paused = true)]
    async fn pool_members_sharing_a_gate_take_turns() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c", "d"];
        let bundles = routed_bundles(dir.path(), &names);
        let script = || names.iter().map(|n| (*n, 20, vec![found(n)])).collect::<Vec<_>>();
        let (pr, ps) = (Routed::new(script()), Routed::new(script()));
        let reg = pool_registry(&[("p/r", "p", 8, &pr), ("p/s", "p", 8, &ps)]);
        let members = [member("p/r", "p"), member("p/s", "p")];
        let items = pending_items(&names);
        let run = run_with(Run { reviewers: Some(&members), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(
            placed(&run),
            vec![cell_of("a.rs", "p/r"), cell_of("b.rs", "p/s"), cell_of("c.rs", "p/r"), cell_of("d.rs", "p/s")],
            "空闲轮转交替领项"
        );
        assert_eq!(reg.gate_stats()["p"].peak, 4, "共享门宽，不因多成员加吞吐");
    }

    /// 成员健康（Q7）：成员首格鉴权失败（错误带已烧 usage 的包装也认）→ 出局，其余项只交另一成员；
    /// 失败的那一格保持失败、计债，不改派。
    #[tokio::test(start_paused = true)]
    async fn auth_failure_removes_member_without_reassigning() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c", "d"];
        let bundles = routed_bundles(dir.path(), &names);
        let denied = Err(ProviderError::Auth("401".into()).attach_usage((9, 0, 0)));
        let bad = Routed::new(vec![("a", 10, vec![denied]), ("b", 10, vec![]), ("c", 10, vec![]), ("d", 10, vec![])]);
        let good = Routed::new(names.iter().map(|n| (*n, 50, vec![found(n)])).collect());
        let reg = pool_registry(&[("p/r", "p", 1, &bad), ("q/s", "q", 1, &good)]);
        let members = [member("p/r", "p"), member("q/s", "q")];
        let items = pending_items(&names);
        let run = run_with(Run { reviewers: Some(&members), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(bad.starts(), vec!["a".to_string()], "出局后不再领项");
        assert_eq!(good.starts(), vec!["b".to_string(), "c".to_string(), "d".to_string()], "失败格不改派");
        assert!(!run.cells[0].ok && run.cells[1..].iter().all(|c| c.ok && c.model == "q/s"), "{:?}", placed(&run));
        assert!(run.reviewer_failed, "失败格计债");
        assert!(run.warnings.contains(&"deep: reviewer p/r auth failed — removed from pool".to_string()), "{:?}", run.warnings);
        assert!(!run.warnings.iter().any(|w| w.contains("received 0 cells")), "出局成员不重复报 0 格: {:?}", run.warnings);
    }

    /// 二阶段第二轮鉴权失败同样出局（首轮 finding 保留、计债）：它持门位期间别的成员照常领项，出局后空出的门位不再派给它。
    #[tokio::test(start_paused = true)]
    async fn second_stage_auth_failure_removes_member() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c", "d"];
        let bundles = routed_bundles(dir.path(), &names);
        git_commit_all(dir.path());
        let with_needs = Ok(turn(r#"{"findings":[],"needs":[{"path":"a.rs","reason":"need context"}]}"#));
        let denied = Err(ProviderError::Auth("401".into()));
        let pa = Routed::new(vec![("a", 50, vec![with_needs, denied]), ("b", 50, vec![]), ("c", 50, vec![]), ("d", 50, vec![])]);
        let qb = Routed::new(names.iter().map(|n| (*n, 60, vec![found(n)])).collect());
        let reg = pool_registry(&[("p/r", "p", 1, &pa), ("q/s", "q", 1, &qb)]);
        let members = [member("p/r", "p"), member("q/s", "q")];
        let items = pending_items(&names);
        let run = run_with(Run { reviewers: Some(&members), tools: ToolsMode::TwoStage, ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(pa.starts(), vec!["a".to_string(), "a".to_string()], "第二轮 401 后不再领项");
        assert_eq!(qb.starts(), vec!["b".to_string(), "c".to_string(), "d".to_string()]);
        assert!(!run.cells[0].ok, "第二轮失败按失败格落盘（评审修 #2）");
        assert!(run.cells[0].warn.as_deref().unwrap_or("").contains("second-stage round failed"), "{:?}", run.cells[0].warn);
        assert!(run.reviewer_failed);
        assert!(run.warnings.contains(&"deep: reviewer p/r auth failed — removed from pool".to_string()), "{:?}", run.warnings);
    }

    /// 唯一成员出局 = 池空：停派，剩余项保持 Pending（从未派出、resume 可续）、warning 报数、计债（exit 2）。
    #[tokio::test(start_paused = true)]
    async fn sole_member_auth_failure_leaves_the_rest_pending_as_debt() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c"];
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(vec![("a", 10, vec![Err(ProviderError::Auth("401".into()))]), ("b", 10, vec![]), ("c", 10, vec![])]);
        let reg = routed_registry(&backend, 1);
        let items = pending_items(&names);
        let snaps = Mutex::new(Vec::new());
        let snapshot = |it: &QueueItem| snaps.lock().unwrap().push((it.id.clone(), it.state));
        let run = run_with(Run { snapshot: Some(&snapshot), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(backend.starts(), vec!["a".to_string()]);
        assert_eq!(run.cells.len(), 1);
        assert!(run.reviewer_failed);
        assert!(
            run.warnings.iter().any(|w| w == "deep: reviewer pool empty — 2 queued item(s) not dispatched, left pending for resume"),
            "{:?}", run.warnings
        );
        let dispatched: Vec<String> = snaps.lock().unwrap().iter().filter(|(_, st)| *st == ItemState::InFlight).map(|(id, _)| id.clone()).collect();
        assert_eq!(dispatched, vec!["a.rs:rule:1-2".to_string()], "b / c 从未派出");
    }

    /// 配了但一格没分到的成员必须可见（批 C dogfood grok-4.6 空转且无声）；一格都没派出时不报（无从谈起）。
    #[tokio::test(start_paused = true)]
    async fn member_left_without_cells_is_disclosed() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a"]);
        let pr = Routed::new(vec![("a", 10, vec![found("a")])]);
        let ps = Routed::new(vec![("a", 10, vec![found("a")])]);
        let reg = pool_registry(&[("p/r", "p", 8, &pr), ("p/s", "p", 8, &ps)]);
        let members = [member("p/r", "p"), member("p/s", "p")];
        let items = pending_items(&["a"]);
        let run = run_with(Run { reviewers: Some(&members), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(placed(&run), vec![cell_of("a.rs", "p/r")]);
        assert_eq!(run.warnings, vec!["deep: reviewer p/s configured but received 0 cells".to_string()]);
        let idle = run_with(Run { reviewers: Some(&members), ..plain(&reg, dir.path(), &bundles, &[]) }).await;
        assert!(idle.warnings.is_empty(), "队列为空不报 0 格: {:?}", idle.warnings);
    }

    /// 未注册的成员就地出局、不挡其余成员：另一成员照挖完不计债；全员未注册 = 池空、一格未派 → 仍计债（exit 2），不是干净通过。
    #[tokio::test(start_paused = true)]
    async fn unregistered_member_leaves_pool_without_blocking_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b"];
        let bundles = routed_bundles(dir.path(), &names);
        let good = Routed::new(names.iter().map(|n| (*n, 10, vec![found(n)])).collect());
        let reg = pool_registry(&[("q/s", "q", 1, &good)]);
        let items = pending_items(&names);
        let members = [member("p/ghost", "p"), member("q/s", "q")];
        let run = run_with(Run { reviewers: Some(&members), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(run.cells.iter().filter(|c| c.ok && c.model == "q/s").count(), 2, "{:?}", run.warnings);
        assert!(!run.reviewer_failed, "其余成员挖完 → 不计债: {:?}", run.warnings);
        assert!(run.warnings.iter().any(|w| w.starts_with("deep: reviewer p/ghost lease failed:") && w.ends_with("removed from pool")), "{:?}", run.warnings);

        let ghosts = [member("p/ghost", "p")];
        let run = run_with(Run { reviewers: Some(&ghosts), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert!(run.cells.is_empty());
        assert!(run.reviewer_failed, "池空、项未挖 → 计债");
        assert!(run.warnings.iter().any(|w| w.contains("reviewer pool empty — 2 queued item(s)")), "{:?}", run.warnings);
    }

    // ── 批 C1-4：墙钟门与退化零差 ────────────────────────────────────────────

    /// 墙钟门：K = 12 格 × 50 ms、门宽和 W = 4（单成员门宽 4，或跨 provider 两成员门宽 2 + 2）→ 虚拟总时长 = ceil(K/W) × 50 ms
    /// = 150 ms（串行 600 ms）。验收的是墙钟，不是质量——多成员只买吞吐与分散限流（prd Goal）。
    #[tokio::test(start_paused = true)]
    async fn wall_clock_gate_twelve_cells_over_width_four() {
        let names: Vec<String> = (0..12).map(|i| format!("f{i:02}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let script = || names.iter().map(|n| (*n, 50, vec![found(n)])).collect::<Vec<_>>();
        let check = |run: &DeepRun, elapsed: Duration, shape: &str| {
            assert_eq!(run.cells.iter().filter(|c| c.ok).count(), 12, "{shape}: {:?}", run.warnings);
            assert!(
                elapsed >= Duration::from_millis(150) && elapsed < Duration::from_millis(200),
                "{shape}: ceil(12/4) × 50 ms，实测 {elapsed:?}"
            );
        };

        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &names);
        let items = pending_items(&names);

        let single = Routed::new(script());
        let reg = routed_registry(&single, 4);
        let t0 = tokio::time::Instant::now();
        let run = run_with(plain(&reg, dir.path(), &bundles, &items)).await;
        check(&run, t0.elapsed(), "单成员门宽 4");
        assert_eq!(reg.gate_stats()["p"].peak, 4);

        let (pa, qb) = (Routed::new(script()), Routed::new(script()));
        let reg = pool_registry(&[("p/r", "p", 2, &pa), ("q/s", "q", 2, &qb)]);
        let members = [member("p/r", "p"), member("q/s", "q")];
        let t0 = tokio::time::Instant::now();
        let run = run_with(Run { reviewers: Some(&members), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        check(&run, t0.elapsed(), "两成员门宽 2 + 2");
        let stats = reg.gate_stats();
        assert_eq!((stats["p"].peak, stats["q"].peak), (2, 2));
        assert_eq!((pa.starts().len(), qb.starts().len()), (6, 6), "两门各吃一半");
    }

    /// `item_wait_ms` 在并发下仍是**队列等待**（派发时刻 − 入队），不是墙钟（完成时刻 − 入队）：门宽 2、4 格 × 300 ms →
    /// 先派出的两格 ≈ 0、排在一轮之后的两格 ≈ 300 ms；若按完成时刻采样，前两格也会 ≥ 300 ms。
    /// 真实时间：入队时刻取系统时钟（快照要落 epoch ms），虚拟时间推不动它。阈值与时延留足间距（评审第 2 档 #10）：
    /// 派发路径每项同步起一次 `git ls-files`（读守卫），全量并行跑测试时进程启动延迟可达数十 ms。
    #[tokio::test]
    async fn item_wait_ms_measures_queue_wait_under_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c", "d"];
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(names.iter().map(|n| (*n, 300, vec![found(n)])).collect());
        let reg = routed_registry(&backend, 2);
        let items = pending_items(&names);
        let run = run_with(plain(&reg, dir.path(), &bundles, &items)).await;
        let w = &run.meta.item_wait_ms;
        assert_eq!(w.len(), 4, "{:?}", run.warnings);
        assert!(w[0] < 150 && w[1] < 150, "先派出的两格不排队: {w:?}");
        assert!(w[2] >= 280 && w[3] >= 280, "后两格排在一轮之后: {w:?}");
    }

    // ── 退化零差基线（批 C1-4）──────────────────────────────────────────────

    /// 退化零差场景：单 reviewer、门宽 1、restore 形态——主区 / 波及 / 疑点各一项待挖（波及项走二阶段补查），另有一项预算截断、
    /// 一项已结案；prior findings 含已结案项的一条与主区产出同一 bug 的一条（dedup 合并）。计时字段与模板哈希归一化掉
    /// （与派发形态无关，随墙钟 / 模板演进漂移）；落盘序（on_cell）与队列快照序一并比。
    async fn degenerate_serial_scenario() -> serde_json::Value {
        use crate::orchestrate::queue::QueueItem;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let names = ["p", "r", "s", "k", "d"];
        for n in names {
            std::fs::write(root.join(format!("{n}.rs")), format!("fn {n}() {{\n    body();\n}}\n")).unwrap();
        }
        let g = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(root).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        g(&["init", "-q"]);
        g(&["add", "."]);
        g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]);
        let bundles = vec![Bundle::from_files(
            names.iter().map(|n| FileChange { path: format!("{n}.rs"), diff: format!("@@ -1,1 +1,1 @@\n+fn {n}() {{") }).collect(),
        )];
        let rule = |file: &str, kind: ItemKind, state: ItemState| QueueItem {
            id: format!("{file}:rule:1-2"),
            spec: QueueItemSpec::new(file.into(), vec![(1, 2)], QueueOrigin::Rule, vec![]).unwrap(),
            score: 1.0, agreement: 1, state, kind, dispatch_agreement: 0, landed_agreement: None, families: vec![], enqueued_ms: None,
            arrivals: vec![], members: vec![], member_families: vec![], block: Default::default(), peak_agreement: 0, risk_band: 0,
        };
        let note = crate::domain::Suspicion {
            file: "s.rs".into(), line_start: 1, line_end: 1, hint: "h".into(), category: None, confidence: 0.5, quote: "fn s() {".into(),
        };
        let scan = QueueItem {
            id: "s.rs:scan:1-1".into(),
            spec: QueueItemSpec::new("s.rs".into(), vec![(1, 1)], QueueOrigin::Scan, vec![note]).unwrap(),
            score: 0.5, agreement: 2, state: ItemState::Pending, kind: ItemKind::Suspicion,
            dispatch_agreement: 0, landed_agreement: None, families: vec!["alpha".into(), "beta".into()], enqueued_ms: None,
            arrivals: vec![], members: vec![], member_families: vec![], block: Default::default(), peak_agreement: 0, risk_band: 0,
        };
        let items = vec![
            rule("r.rs", ItemKind::Ripple, ItemState::Pending),
            scan,
            rule("p.rs", ItemKind::Primary, ItemState::Pending),
            rule("k.rs", ItemKind::Ripple, ItemState::Skipped),
            rule("d.rs", ItemKind::Primary, ItemState::Settled),
        ];
        let finding = |n: &str, title: &str| format!(
            r#"{{"severity":"low","title":"{title}","body":"b","file":"{n}.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"fn {n}() {{"}}"#
        );
        let script = vec![
            turn(&format!(r#"{{"findings":[{}],"needs":[]}}"#, finding("p", "issue in p"))),
            turn(&format!(r#"{{"findings":[{}],"needs":[{{"path":"r.rs","reason":"need context"}}]}}"#, finding("r", "draft in r"))),
            turn(&format!(r#"{{"findings":[{}],"needs":[]}}"#, finding("r", "confirmed in r"))),
            turn(&format!(r#"{{"findings":[{}],"needs":[]}}"#, finding("s", "issue in s"))),
        ];
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 1, Box::new(FakeChatBackend::new(script)), Duration::from_secs(5), 0);
        let mut prior_same = fixtures::finding("issue in p", crate::domain::Severity::Low, 1, "p/old");
        (prior_same.file, prior_same.quote) = ("p.rs".into(), "fn p() {".into());
        let mut prior_settled = fixtures::finding("issue in d", crate::domain::Severity::Medium, 1, "p/r");
        (prior_settled.file, prior_settled.quote) = ("d.rs".into(), "fn d() {".into());
        let snaps = std::sync::Mutex::new(Vec::new());
        let snapshot = |it: &QueueItem| snaps.lock().unwrap().push(serde_json::json!([it.id, it.state]));
        let landed = std::sync::Mutex::new(Vec::new());
        let on_cell = |rec: &CellRecord| landed.lock().unwrap().push(rec.cell.clone());
        let reviewer = fake_reviewer();
        let mut run = run_deep(
            &reg,
            DeepInput {
                route: None, scan_cells: &[], bundles: &bundles, root,
                reviewers: std::slice::from_ref(&reviewer), budget_cells: 4,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: Some(&snapshot), restore: Some(&items), prior_findings: vec![prior_same, prior_settled],
                tools: ToolsMode::TwoStage, backend_factory: None, on_cell: Some(&on_cell), incoming: None, reopen_skipped: false,
            },
            &(),
        )
        .await;
        let mut landed = landed.lock().unwrap().clone();
        let snaps = snaps.lock().unwrap().clone();
        // cell 名由派发序（批 C `review[{n}]`）改为按队列项（C1-1 有意改动）：按报告序映射回 n 再比
        // （报告按最终优先级序组装，单成员门宽 1、开跑即收场时与派发序相同）。
        let seq_names: BTreeMap<String, String> =
            run.cells.iter().enumerate().map(|(i, c)| (c.cell.clone(), format!("review[{}]@{}", i + 1, c.model))).collect();
        for c in run.cells.iter_mut() {
            c.cell = seq_names[&c.cell].clone();
        }
        for name in landed.iter_mut() {
            *name = seq_names[name.as_str()].clone();
        }
        for c in run.cells.iter_mut() {
            c.duration_ms = 0;
            c.queue_wait_ms = 0;
            c.template_hash = None;
        }
        for w in run.meta.item_wait_ms.iter_mut() {
            *w = 0;
        }
        // 批 C 之后新增的观测字段（C2-3）：确定性读数先断言再归零、计时归零——基线文件保持批 C 原样。
        // restore 模式开跑即收场：剩余预算 = K 4 − 已结案 1；收场前无派发。
        assert_eq!((run.meta.close_budget_remaining, run.meta.scan_phase_dispatched, run.meta.close_ms), (3, 0, 0));
        assert!(run.meta.first_dispatch_ms > 0 && run.meta.drain_done_ms >= run.meta.first_dispatch_ms, "{:?}", run.meta);
        run.meta.close_budget_remaining = 0;
        // C3-3 新增的选项观测（同上：确定性读数先断言再归零，基线文件保持批 C 原样）。
        // 快照里 5 个文件各一项（疑点项 1 个），派发 4 项、每文件至多 1 格；restore 模式无疑点到达。
        assert_eq!(
            (run.meta.suspicions_arrived, run.meta.items_from_suspicions, run.meta.files_candidate, run.meta.files_selected, run.meta.max_cells_per_file),
            (0, 1, 5, 4, 1),
            "{:?}",
            run.meta
        );
        (run.meta.items_from_suspicions, run.meta.files_candidate, run.meta.files_selected, run.meta.max_cells_per_file) = (0, 0, 0, 0);
        (run.meta.first_dispatch_ms, run.meta.drain_done_ms, run.meta.fill_block_max_ms, run.meta.fill_block_total_ms) = (0, 0, 0, 0);
        serde_json::json!({
            "findings": run.findings, "cells": run.cells, "meta": run.meta, "warnings": run.warnings,
            "reviewer_failed": run.reviewer_failed, "on_cell": landed, "snapshots": snaps,
        })
    }

    /// 退化零差（C1-4）：单 reviewer + 门宽 1 时并发派发循环退化为串行——报告（findings / cells / meta / warnings / 债）、
    /// 落盘序与队列快照序与批 C 串行实现同构。基线由批 C 代码（`eac0a60`）跑同一场景采得，不是凭推理写的期望值；
    /// 与批 C 的有意差异只有 cell 名形态（已按派发序映射回去）。并发改造的第一道回归哨。
    #[tokio::test]
    async fn single_member_gate_one_matches_batch_c_serial_baseline() {
        let got = degenerate_serial_scenario().await;
        let want: serde_json::Value = serde_json::from_str(include_str!("testdata/deep_serial_baseline.json")).unwrap();
        assert!(
            got == want,
            "与批 C 串行基线不同构\n--- got ---\n{}\n--- want ---\n{}",
            serde_json::to_string_pretty(&got).unwrap(),
            serde_json::to_string_pretty(&want).unwrap()
        );
    }

    // ── C1 评审第 2 档 ──────────────────────────────────────────────────────

    /// reviewer 格的丢弃计数进 cell warn（评审第 2 档，与 scanner 格同口径）：parse 层丢弃（quote 空白等）与 quote 锚定丢弃
    /// 算了却不落盘时，「模型零发现」与「finding 全被静默丢掉」在报告里分不出来（C1 dogfood 里 grok 零产出的结论因此打折）。
    #[tokio::test]
    async fn reviewer_cell_warn_discloses_dropped_findings() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = one_bundle(dir.path());
        let text = r#"{"findings":[
            {"severity":"low","title":"ghost","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"no such line"},
            {"severity":"low","title":"blank","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":""},
            {"severity":"low","title":"real","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"fn a() {"}
        ],"needs":[]}"#;
        let mut reg = crate::provider::registry::ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 8, Box::new(FakeChatBackend::new(vec![turn(text)])), Duration::from_secs(5), 0);
        let run = run_one(&reg, dir.path(), &bundles, ToolsMode::Off).await;
        assert_eq!(run.findings.len(), 1);
        let w = run.cells[0].warn.as_deref().unwrap_or("");
        assert!(w.contains("parse dropped: no_quote=1"), "{w}");
        assert!(w.contains("1 findings dropped: quote not anchored"), "{w}");
    }

    /// 二阶段成功时 cell 的解析统计取终判（第二轮）——首轮草稿被锚定门丢掉的不再报成这一格的丢弃。
    #[tokio::test]
    async fn two_stage_success_reports_final_round_drop_counts() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a"]);
        git_commit_all(dir.path());
        let draft = Ok(turn(r#"{"findings":[{"severity":"low","title":"ghost draft","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.4,"recommendation":"r","quote":"no such line"}],"needs":[{"path":"a.rs","reason":"need context"}]}"#));
        let backend = Routed::new(vec![("a", 10, vec![draft, found("a")])]);
        let reg = routed_registry(&backend, 1);
        let items = pending_items(&["a"]);
        let run = run_with(Run { tools: ToolsMode::TwoStage, ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(run.meta.needs_two_stage_triggered, 1);
        assert!(run.cells[0].ok);
        assert_eq!(run.cells[0].warn, None, "终判干净 → 不报首轮草稿的丢弃");
    }

    // ── C2-2：读守卫预取 ──────────────────────────────────────────────────────

    /// 读守卫的跟踪文件集每轮只取一次（C2 Q9〔D15〕）：3 项派发（每项 region::build 过守卫）+ 一次二阶段补查
    /// （need 过守卫）全程 `TrackedFiles::load` 恰一次——不再逐项起 `git ls-files` 子进程。
    #[tokio::test(start_paused = true)]
    async fn tracked_files_are_loaded_once_per_run() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c"];
        let bundles = routed_bundles(dir.path(), &names);
        git_commit_all(dir.path());
        let with_needs = Ok(turn(r#"{"findings":[],"needs":[{"path":"b.rs","reason":"need context"}]}"#));
        let backend = Routed::new(vec![("a", 10, vec![with_needs, found("a")]), ("b", 10, vec![found("b")]), ("c", 10, vec![found("c")])]);
        let reg = routed_registry(&backend, 3);
        let items = pending_items(&names);
        crate::cli::second_pass::TRACKED_LOADS.with(|c| c.set(0));
        let run = run_with(Run { tools: ToolsMode::TwoStage, ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(run.meta.needs_two_stage_triggered, 1, "{:?}", run.warnings);
        assert_eq!(run.cells.iter().filter(|c| c.ok).count(), 3, "{:?}", run.warnings);
        assert_eq!(crate::cli::second_pass::TRACKED_LOADS.with(|c| c.get()), 1, "每轮只取一次跟踪文件集");
    }

    // ── C2-3：深挖主循环 ──────────────────────────────────────────────────────

    /// 队列项构造：种类 / 状态 / 派发时共识（其余同 `snap_item`；疑点带一条锚得上首行的笔记）。
    fn item_of(name: &str, kind: ItemKind, state: ItemState, dispatch_agreement: u32) -> QueueItem {
        let file = format!("{name}.rs");
        let (id, spec) = if kind == ItemKind::Suspicion {
            let note = crate::domain::Suspicion {
                file: file.clone(), line_start: 1, line_end: 1, hint: "h".into(), category: None, confidence: 0.5,
                quote: format!("fn {name}() {{"),
            };
            (format!("{file}:scan:1-1"), QueueItemSpec::new(file.clone(), vec![(1, 1)], QueueOrigin::Scan, vec![note]).unwrap())
        } else {
            (format!("{file}:rule:1-2"), QueueItemSpec::new(file.clone(), vec![(1, 2)], QueueOrigin::Rule, vec![]).unwrap())
        };
        // 与 `QueueItem::new` 同形：members = 笔记全量、member_families 同下标（家族未知留空 → 必挖轴退回 agreement）。
        let members = spec.notes.clone();
        let member_families = vec![Vec::new(); members.len()];
        QueueItem {
            id, spec, score: 1.0, agreement: 1, state, kind, dispatch_agreement, landed_agreement: None,
            families: vec![], enqueued_ms: None, arrivals: vec![], members, member_families,
            block: Default::default(), peak_agreement: 0, risk_band: 0,
        }
    }

    /// resume 的收场 drain 不截往轮已派发的项（design §6「Pending 集合不缩」）：扫描期被 kill 的首跑，必挖把派发数顶出 K
    /// （K = 4：两条主区已结案 + 三条涟漪与一条保底疑点在途）、另有一条从未派发的涟漪 r4。往轮在途项占预算位 / 保底位、不参与 drain，
    /// 四条全部重挖；剩余预算 0 → r4 被截。只数 Settled 时：在途项参与 drain → 被截掉两条；不参与但不计入 → 剩余预算虚高、r4 也被挖。
    #[tokio::test(start_paused = true)]
    async fn resume_drain_keeps_prior_inflight_items_when_must_overshot_budget() {
        let dir = tempfile::tempdir().unwrap();
        let redo = ["r1", "r2", "r3", "s1"];
        let bundles = routed_bundles(dir.path(), &["m1", "m2", "r1", "r2", "r3", "r4", "s1"]);
        let backend = Routed::new(["r1", "r2", "r3", "r4", "s1"].iter().map(|n| (*n, 10, vec![found(n)])).collect());
        let reg = routed_registry(&backend, 4);
        let items = vec![
            item_of("m1", ItemKind::Primary, ItemState::Settled, 1),
            item_of("m2", ItemKind::Primary, ItemState::Settled, 1),
            item_of("r1", ItemKind::Ripple, ItemState::InFlight, 1),
            item_of("r2", ItemKind::Ripple, ItemState::InFlight, 1),
            item_of("r3", ItemKind::Ripple, ItemState::InFlight, 1),
            item_of("s1", ItemKind::Suspicion, ItemState::InFlight, 1),
            item_of("r4", ItemKind::Ripple, ItemState::Pending, 0),
        ];
        let run = run_with(Run { budget: 4, ..plain(&reg, dir.path(), &bundles, &items) }).await;
        let mut dug = backend.starts();
        dug.sort();
        assert_eq!(dug, redo, "往轮在途项全部重挖: {:?}", run.warnings);
        assert_eq!((run.meta.budget_skipped, run.meta.close_budget_remaining), (1, 0));
        let states: Vec<(&str, ItemState)> = run.items.iter().map(|it| (it.spec.file.as_str(), it.state)).collect();
        assert!(states.iter().all(|(f, st)| (*f == "r4.rs") == (*st == ItemState::Skipped)), "只截从未派发的 r4: {states:?}");
    }

    /// 经通道到达的 scanner 疑点：`<name>.rs` 首行、分 = `confidence`、族 `family`、到达键 `{cell}#{i}`。
    fn scan_req(name: &str, confidence: f32, family: &str, cell: &str, i: usize) -> EnqueueReq {
        let s = crate::domain::Suspicion {
            file: format!("{name}.rs"), line_start: 1, line_end: 1, hint: "h".into(), category: None, confidence,
            quote: format!("fn {name}() {{"),
        };
        EnqueueReq::scan(&s, family, crate::consolidate::scan::arrival_key(cell, i, &s)).unwrap()
    }

    /// 经通道到达的 H0 形态项（主区 / 涟漪）。
    fn rule_req(name: &str, kind: ItemKind, score: f32) -> EnqueueReq {
        let spec = QueueItemSpec::new(format!("{name}.rs"), vec![(1, 2)], QueueOrigin::Rule, vec![]).unwrap();
        EnqueueReq { spec, score, kind, families: vec![], arrival: None }
    }

    /// 虚拟时钟下「自测试开始起第 `ms` 毫秒」（各用例开头即调用，t0 = 当前）。
    fn at(t0: tokio::time::Instant, ms: u64) -> tokio::time::Instant {
        t0 + Duration::from_millis(ms)
    }

    /// 扫描期额度（design v11 §6 规则 3〔D15〕）：K = 8（F = 2）、宽门、首批 10 条非必挖疑点 t = 0 到达 → 收场前只派 K − F = 6；
    /// 400 ms 关闭入口 → 剩余预算 2 按 drain 补齐（保底已被 6 条疑点用完），余 2 条 Skipped。新观测字段与之对得上。
    #[tokio::test(start_paused = true)]
    async fn scan_phase_quota_holds_back_floor_until_close() {
        let t0 = tokio::time::Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..10).map(|i| format!("s{i}")).collect();
        let names: Vec<&str> = names.iter().map(String::as_str).collect();
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(names.iter().map(|n| (*n, 100, vec![found(n)])).collect());
        let reg = routed_registry(&backend, 16);
        let (tx, rx) = mpsc::channel(16);
        let probe = Arc::clone(&backend);
        let sent = names.clone();
        let feed = async move {
            // 分数降序到达：先到先派的 6 条恰是前 6，收场补的是其余里最高的 2 条（s6、s7）
            for (i, n) in sent.iter().enumerate() {
                tx.send(scan_req(n, 0.9 - i as f32 * 0.05, "alpha", "scan[g1]@p/a#1", i)).await.unwrap();
            }
            tokio::time::sleep_until(at(t0, 50)).await;
            assert_eq!(probe.starts().len(), 6, "扫描期非必挖只派 K − F");
            tokio::time::sleep_until(at(t0, 400)).await;
            assert_eq!(probe.starts().len(), 6, "额度耗尽后到收场前不再派");
        };
        let run_fut = run_with(Run { incoming: Some(rx), budget: 8, first_run: true, ..plain(&reg, dir.path(), &bundles, &[]) });
        let (run, ()) = tokio::join!(run_fut, feed);
        let mut dug = backend.starts();
        dug.sort();
        assert_eq!(dug, ["s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7"], "{:?}", run.warnings);
        let m = &run.meta;
        assert_eq!((m.budget_skipped, m.scan_phase_dispatched, m.close_budget_remaining), (2, 6, 2));
        assert_eq!((m.first_dispatch_ms, m.close_ms, m.drain_done_ms), (1, 400, 500), "t = 0 起挖、400 ms 收场、收场补派的两格 500 ms 落地");
        let skipped: Vec<&str> = run.items.iter().filter(|it| it.state == ItemState::Skipped).map(|it| it.spec.file.as_str()).collect();
        assert_eq!(skipped, ["s8.rs", "s9.rs"]);
    }

    /// 必挖不截（K = 0）：主区照挖；疑点在后到的异族并入抬到共识 2 时才变必挖、随即派发；其余非必挖收场全 Skipped。
    #[tokio::test(start_paused = true)]
    async fn must_items_are_never_cut_even_with_zero_budget() {
        let t0 = tokio::time::Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["p", "a", "b"]);
        let backend = Routed::new(["p", "a", "b"].iter().map(|n| (*n, 20, vec![found(n)])).collect());
        let reg = routed_registry(&backend, 4);
        let (tx, rx) = mpsc::channel(8);
        let probe = Arc::clone(&backend);
        let feed = async move {
            tx.send(rule_req("p", ItemKind::Primary, 1.0)).await.unwrap();
            tx.send(scan_req("a", 0.5, "alpha", "scan[g1]@p/alpha#1", 0)).await.unwrap();
            tx.send(scan_req("b", 0.9, "alpha", "scan[g1]@p/alpha#1", 1)).await.unwrap();
            tokio::time::sleep_until(at(t0, 40)).await;
            assert_eq!(probe.starts(), ["p"], "K = 0：非必挖疑点扫描期不可派");
            tx.send(scan_req("a", 0.5, "beta", "scan[g1]@p/beta#1", 0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 100)).await;
        };
        let run_fut = run_with(Run { incoming: Some(rx), budget: 0, first_run: true, ..plain(&reg, dir.path(), &bundles, &[]) });
        let (run, ()) = tokio::join!(run_fut, feed);
        assert_eq!(backend.starts(), ["p", "a"], "{:?}", run.warnings);
        assert_eq!(run.meta.dispatch_agreement, [1, 2], "a 在共识 2 时才派出（报告按优先级序：主区在前）");
        assert_eq!(run.meta.budget_skipped, 1);
        let b = run.items.iter().find(|it| it.spec.file == "b.rs").unwrap();
        assert_eq!(b.state, ItemState::Skipped);
    }

    /// Skipped 只在收场产生（design v11 §6 规则 3）：额度耗尽后到达的项在入口关闭前一直 Pending（快照里没有 Skipped）；
    /// 收场一次性截断，每个被截项只写一次快照，`budget_skipped` = 最终 Skipped 数。
    #[tokio::test(start_paused = true)]
    async fn skipped_only_appears_at_close() {
        let t0 = tokio::time::Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let names = ["r1", "r2", "r3", "r4", "s1", "s2", "s3"];
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(names.iter().map(|n| (*n, 300, vec![found(n)])).collect());
        let reg = routed_registry(&backend, 8);
        let snaps = Mutex::new(Vec::new());
        let snapshot = |it: &QueueItem| snaps.lock().unwrap().push((t0.elapsed(), it.spec.file.clone(), it.state));
        let (tx, rx) = mpsc::channel(8);
        let feed = async move {
            for n in ["r1", "r2", "r3"] {
                tx.send(rule_req(n, ItemKind::Ripple, 1.0)).await.unwrap();
            }
            tokio::time::sleep_until(at(t0, 20)).await;
            tx.send(rule_req("r4", ItemKind::Ripple, 1.0)).await.unwrap();
            for (i, n) in ["s1", "s2", "s3"].iter().enumerate() {
                tx.send(scan_req(n, 0.9 - i as f32 * 0.1, "alpha", "scan[g1]@p/alpha#1", i)).await.unwrap();
            }
            tokio::time::sleep_until(at(t0, 200)).await;
        };
        let run_fut = run_with(Run {
            incoming: Some(rx), budget: 4, first_run: true, snapshot: Some(&snapshot), ..plain(&reg, dir.path(), &bundles, &[])
        });
        let (run, ()) = tokio::join!(run_fut, feed);
        let snaps = snaps.lock().unwrap().clone();
        let skipped: Vec<_> = snaps.iter().filter(|(_, _, st)| *st == ItemState::Skipped).collect();
        assert!(skipped.iter().all(|(t, _, _)| *t >= Duration::from_millis(200)), "收场前不得出现 Skipped: {snaps:?}");
        let mut files: Vec<&str> = skipped.iter().map(|(_, f, _)| f.as_str()).collect();
        files.sort();
        assert_eq!(files, ["r4.rs", "s2.rs", "s3.rs"], "保底留 s1、剩余预算 1 已被它占满；每项只写一次");
        assert_eq!(run.meta.budget_skipped, 3);
        assert_eq!(run.items.iter().filter(|it| it.state == ItemState::Skipped).count(), 3);
        assert_eq!(run.cells.len(), 4, "{:?}", run.warnings);
    }

    /// 外部持门者放门及时补位（C1 评审 #6，G1〔D15〕）：门 2、外部先占 1、10 ms 后放 → 深挖的等门挂起被 FIFO 唤醒立刻补位，
    /// 4 项 × 100 ms 墙钟 = 210 ms；C1「在途非空不等门」要等首格落地才补，= 300 ms。
    #[tokio::test(start_paused = true)]
    async fn external_release_wakes_parked_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c", "d"];
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(names.iter().map(|n| (*n, 100, vec![found(n)])).collect());
        let reg = routed_registry(&backend, 2);
        let items = pending_items(&names);
        let hold = reg.try_lease("p/r").unwrap().expect("外部先占一个门位");
        let release = async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(hold);
        };
        let t0 = tokio::time::Instant::now();
        let (run, ()) = tokio::join!(run_with(plain(&reg, dir.path(), &bundles, &items)), release);
        assert_eq!(run.cells.iter().filter(|c| c.ok).count(), 4, "{:?}", run.warnings);
        assert_eq!(t0.elapsed(), Duration::from_millis(210), "放门即补位");
    }

    /// 共门不插队（G1〔D15〕）：门 1，深挖格在跑时 scanner（`registry.lease` 模拟）先来排队，深挖的下一项后到——
    /// 深挖格放门后门位归 scanner（队头），深挖等 scanner 放门才派下一项；不按「谁先被 poll 谁拿」。
    #[tokio::test(start_paused = true)]
    async fn shared_gate_release_goes_to_waiting_scanner_first() {
        let t0 = tokio::time::Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a", "b"]);
        let backend = Routed::new(vec![("a", 100, vec![found("a")]), ("b", 10, vec![found("b")])]);
        let reg = routed_registry(&backend, 1);
        let landed = Mutex::new(Vec::new());
        let on_cell = |rec: &CellRecord| landed.lock().unwrap().push((rec.cell.clone(), t0.elapsed()));
        let (tx, rx) = mpsc::channel(4);
        let feed = async move {
            tx.send(rule_req("a", ItemKind::Primary, 1.0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 20)).await;
            tx.send(rule_req("b", ItemKind::Primary, 1.0)).await.unwrap();
        };
        let scanner = async {
            tokio::time::sleep_until(at(t0, 10)).await;
            let lease = reg.lease("p/r").await.unwrap();
            let got = t0.elapsed();
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(lease);
            got
        };
        let run_fut = run_with(Run { incoming: Some(rx), first_run: true, on_cell: Some(&on_cell), ..plain(&reg, dir.path(), &bundles, &[]) });
        let (run, (), scanner_got) = tokio::join!(run_fut, feed, scanner);
        assert_eq!(run.cells.len(), 2, "{:?}", run.warnings);
        assert_eq!(scanner_got, Duration::from_millis(100), "深挖首格放门 → 门位归先排队的 scanner");
        let landed = landed.lock().unwrap().clone();
        assert_eq!(landed[1], ("review[b.rs:rule:1-2]@p/r".to_string(), Duration::from_millis(160)), "b 等 scanner 放门（150 ms）才派");
    }

    /// 等门挂起跨事件保持（G1〔D15〕）：门 1 被外部占到 100 ms；深挖 t = 0 起排队，scanner 10 ms 排在其后，20 ms 一次通道到达——
    /// 到达事件不得重建深挖的等门（重建 = 退出 FIFO 重排队尾、被 scanner 插到前面）。放门后门位归深挖，scanner 等深挖格放门。
    #[tokio::test(start_paused = true)]
    async fn parked_waits_keep_fifo_position_across_arrivals() {
        let t0 = tokio::time::Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a", "c"]);
        let backend = Routed::new(vec![("a", 50, vec![found("a")]), ("c", 50, vec![found("c")])]);
        let reg = routed_registry(&backend, 1);
        let hold = reg.try_lease("p/r").unwrap().expect("外部先占门");
        let landed = Mutex::new(Vec::new());
        let on_cell = |rec: &CellRecord| landed.lock().unwrap().push((rec.cell.clone(), t0.elapsed()));
        let (tx, rx) = mpsc::channel(4);
        let feed = async move {
            tx.send(rule_req("a", ItemKind::Primary, 2.0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 20)).await;
            tx.send(rule_req("c", ItemKind::Primary, 1.0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 100)).await;
            drop(hold);
        };
        let scanner = async {
            tokio::time::sleep_until(at(t0, 10)).await;
            let lease = reg.lease("p/r").await.unwrap();
            let got = t0.elapsed();
            tokio::time::sleep(Duration::from_millis(30)).await;
            drop(lease);
            got
        };
        let run_fut = run_with(Run { incoming: Some(rx), first_run: true, on_cell: Some(&on_cell), ..plain(&reg, dir.path(), &bundles, &[]) });
        let (run, (), scanner_got) = tokio::join!(run_fut, feed, scanner);
        assert_eq!(run.cells.len(), 2, "{:?}", run.warnings);
        let landed = landed.lock().unwrap().clone();
        assert_eq!(landed[0], ("review[a.rs:rule:1-2]@p/r".to_string(), Duration::from_millis(150)), "放门归深挖（先排队）");
        assert_eq!(scanner_got, Duration::from_millis(150), "scanner 排在深挖之后");
        assert_eq!(landed[1].1, Duration::from_millis(230), "c 等 scanner 放门（180 ms）");
    }

    /// 报告确定性（D2〔D15〕）：同一选中集合、到达序与完成时延都反过来的两次运行（门 1 → 派发序不同），报告（除计时字段）逐字节一致；
    /// meta 四数组与 cells 等长、按最终优先级序对齐（失败格也各占一位）。
    /// 本构造三项互不并入，故 `dispatch_agreement` / `landed_agreement` 也稳定；它们本身是时刻快照、随并入时序变（评审 #10），
    /// 不在逐字节保证内。
    #[tokio::test(start_paused = true)]
    async fn report_order_follows_final_priority_not_dispatch_order() {
        async fn once(reverse: bool) -> (serde_json::Value, Vec<String>) {
            let t0 = tokio::time::Instant::now();
            let dir = tempfile::tempdir().unwrap();
            let names = ["p", "r", "s"];
            let bundles = routed_bundles(dir.path(), &names);
            let bad = Err(ProviderError::BadRequest("boom".into()).attach_usage((5, 7, 0)));
            let d = |fast: u64, slow: u64| if reverse { slow } else { fast };
            let backend = Routed::new(vec![
                ("p", d(10, 40), vec![found("p")]),
                ("r", d(20, 30), vec![bad]),
                ("s", d(30, 10), vec![found("s")]),
            ]);
            let reg = routed_registry(&backend, 1);
            let (tx, rx) = mpsc::channel(4);
            let feed = async move {
                let mut reqs = vec![
                    rule_req("p", ItemKind::Primary, 1.0),
                    rule_req("r", ItemKind::Ripple, 1.0),
                    scan_req("s", 0.5, "alpha", "scan[g1]@p/alpha#1", 0),
                ];
                if reverse {
                    reqs.reverse();
                }
                // 到达 0 / 1 / 2 ms、入口随之关闭，最快的格 10 ms 才完成：到达与完成不同刻（同刻时 `select!` 随机取分支，
                // 收场读数会随之抖动）
                for (i, req) in reqs.into_iter().enumerate() {
                    tokio::time::sleep_until(at(t0, i as u64)).await;
                    tx.send(req).await.unwrap();
                }
            };
            let run_fut = run_with(Run { incoming: Some(rx), first_run: true, ..plain(&reg, dir.path(), &bundles, &[]) });
            let (mut run, ()) = tokio::join!(run_fut, feed);
            let n = run.cells.len();
            let m = &mut run.meta;
            assert!(n == 3 && [m.item_wait_ms.len(), m.dispatch_agreement.len(), m.landed_agreement.len(), m.reviewer_output_tokens.len()] == [n; 4]);
            m.item_wait_ms.iter_mut().for_each(|w| *w = 0);
            (m.close_ms, m.first_dispatch_ms, m.drain_done_ms, m.fill_block_max_ms, m.fill_block_total_ms) = (0, 0, 0, 0, 0);
            for c in run.cells.iter_mut() {
                (c.duration_ms, c.queue_wait_ms, c.template_hash) = (0, 0, None);
            }
            let report = serde_json::json!({ "findings": run.findings, "cells": run.cells, "meta": run.meta, "warnings": run.warnings });
            (report, backend.starts())
        }
        let (forward, fwd_starts) = once(false).await;
        let (backward, bwd_starts) = once(true).await;
        assert_ne!(fwd_starts, bwd_starts, "构造前提：两次派发序不同");
        assert_eq!(forward, backward);
        let cells: Vec<&str> = forward["cells"].as_array().unwrap().iter().map(|c| c["cell"].as_str().unwrap()).collect();
        // 疑点项的区间 = 稳定块（C3〔D16〕）：夹具里 `s.rs` 第 1 行落在 1-3 行的 def 块内，故 id 是 `scan:1-3` 而非报告行 `scan:1-1`。
        assert_eq!(cells, ["review[p.rs:rule:1-2]@p/r", "review[r.rs:rule:1-2]@p/r", "review[s.rs:scan:1-3]@p/r"], "种类 → 分数 → id");
    }

    /// 快照只写受影响项（Q9 IO2〔D15〕）：新建 1 项、重复到达 0 项、并入只写并入目标；收场只写被截项。
    #[tokio::test(start_paused = true)]
    async fn arrival_snapshots_only_the_affected_item() {
        let t0 = tokio::time::Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a", "b"]);
        let backend = Routed::new(vec![]);
        let reg = routed_registry(&backend, 4);
        let snaps = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&snaps);
        let snapshot = move |it: &QueueItem| log.lock().unwrap().push((it.spec.file.clone(), it.state));
        let (tx, rx) = mpsc::channel(8);
        let probe = Arc::clone(&snaps);
        let feed = async move {
            let writes = || probe.lock().unwrap().len();
            tx.send(scan_req("a", 0.5, "alpha", "scan[g1]@p/alpha#1", 0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 1)).await;
            assert_eq!(writes(), 1, "新建项写一次");
            tx.send(scan_req("a", 0.5, "alpha", "scan[g1]@p/alpha#1", 0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 2)).await;
            assert_eq!(writes(), 1, "重复到达（同到达键）不写");
            tx.send(scan_req("a", 0.5, "alpha", "scan[g1]@p/alpha#2", 0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 3)).await;
            assert_eq!(writes(), 2, "并入只写并入目标（同族不抬共识、仍不可派）");
            tx.send(scan_req("b", 0.4, "alpha", "scan[g1]@p/alpha#1", 1)).await.unwrap();
            tokio::time::sleep_until(at(t0, 4)).await;
            assert_eq!(writes(), 3);
        };
        let run_fut = run_with(Run {
            incoming: Some(rx), budget: 0, first_run: true, snapshot: Some(&snapshot), ..plain(&reg, dir.path(), &bundles, &[])
        });
        let (run, ()) = tokio::join!(run_fut, feed);
        assert_eq!(run.meta.dedup_hits, 1);
        let snaps = snaps.lock().unwrap().clone();
        assert_eq!(snaps.len(), 5, "收场只写两个被截项: {snaps:?}");
        assert_eq!(snaps[3..], [("a.rs".to_string(), ItemState::Skipped), ("b.rs".to_string(), ItemState::Skipped)]);
    }

    /// 阻塞观测覆盖落地段（评审 #11）：落盘回调里的同步写（后台 job 是两笔 SQLite 事务）同样让其余在途格停摆，
    /// `fill_block_*` 漏计它会让 `spawn_blocking` 的触发判断偏乐观。
    #[tokio::test]
    async fn block_timing_covers_the_landing_segment() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a"]);
        let backend = Routed::new(vec![("a", 0, vec![found("a")])]);
        let reg = routed_registry(&backend, 2);
        let items = pending_items(&["a"]);
        let on_cell = |_: &CellRecord| std::thread::sleep(Duration::from_millis(50));
        let run = run_with(Run { on_cell: Some(&on_cell), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        assert_eq!(run.cells.len(), 1, "{:?}", run.warnings);
        assert!(run.meta.fill_block_max_ms >= 50, "落地段未计入阻塞观测: {:?}", run.meta);
    }

    /// 入队即算稳定块与风险档（C3〔D16〕接线）：`a.rs` 只有一个 def（1-3 行），两家族各报一条**互不相似**的疑点
    /// （第 2、3 行）→ 结构定项并成一项、区间取块 1-3、id 随之；与该区重叠的风险区分 2.0 ≥ θ → 档 3。
    /// 共识两轴：区域级 2（两家族碰过这块）、必挖轴 1（最大簇只有一个家族）。
    #[tokio::test(start_paused = true)]
    async fn enqueue_computes_block_and_risk_band() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a"]); // a.rs = "fn a() {\n    body();\n}\n"
        let susp = |line: u32, quote: &str, hint: &str| crate::domain::Suspicion {
            file: "a.rs".into(), line_start: line, line_end: line, hint: hint.into(), category: None, confidence: 0.5,
            quote: quote.into(),
        };
        let cell = |id: &str, family: &str, s: crate::domain::Suspicion| {
            let out = crate::domain::ScanOutput { suspicions: vec![s], ..Default::default() };
            let route_key = format!("p/{family}");
            ScanCellResult {
                cell_id: id.into(), group: 0, family: family.into(), sample: 1,
                record: CellRecord::ok(id, &route_key, crate::domain::CellOutput::Scan(out.clone())),
                route_key,
                output: Some(out),
            }
        };
        let cells = [
            cell("scan[g1]@p/alpha#1", "alpha", susp(2, "    body();", "调用没校验")),
            cell("scan[g1]@p/beta#1", "beta", susp(3, "}", "结尾漏了收尾")),
        ];
        let route = RiskRoute {
            hotspots: vec![],
            zones: vec![Hotspot { file: "a.rs".into(), start: 1, end: 3, score: 2.0, kind: RegionKind::Ripple, signals: vec![] }],
            r: 1.0, tier: crate::risk::Tier::Mid, budget_cells_hint: 4, code_lines: 3,
        };
        let backend = Routed::new(vec![("a", 5, vec![found("a")])]);
        let reg = routed_registry(&backend, 4);

        let run = run_with(Run { scan_cells: &cells, route: Some(&route), ..plain(&reg, dir.path(), &bundles, &[]) }).await;

        assert_eq!(run.items.len(), 1, "同一 def 块里的两条疑点并成一项: {:?}", run.items);
        let it = &run.items[0];
        assert_eq!(it.spec.ranges, vec![(1, 3)], "区间 = def 块");
        assert_eq!(it.id, "a.rs:scan:1-3");
        assert!(it.block.key.starts_with("def:"), "{:?}", it.block);
        assert_eq!(it.risk_band, 3, "分 2.0 ≥ θ 1.5");
        assert_eq!((it.agreement, it.peak_agreement), (2, 1), "区域级 2 / 必挖轴 1");
    }

    /// 保底位的消耗与必挖判定同轴（评审 #4）：结构化合并后「一块被两家族碰过、但没有一条疑点对上」
    /// （agreement 2 / peak 1）是常态；按区域轴数会把这种非必挖项当必挖、保底永不消耗，
    /// 收场再整份预留一次 ⌈K/4⌉ → 实际派发超出 maxCells。
    #[test]
    fn floor_counting_uses_the_must_dig_axis() {
        let mut it = item_of("a", ItemKind::Suspicion, ItemState::Settled, 2);
        (it.agreement, it.peak_agreement) = (2, 1);
        assert!(counts_against_floor(&it), "非必挖疑点占着保底位");
        it.peak_agreement = 2;
        assert!(!counts_against_floor(&it), "必挖项不占保底位");
        let h0 = item_of("b", ItemKind::Ripple, ItemState::Settled, 1);
        assert!(!counts_against_floor(&h0), "保底只留给 scanner 疑点");
    }

    /// scanner needs 只接变更集内的（C3〔D16〕Q8）：`a.rs` 在变更集里 → 入队成 `Needs` 项（无笔记、排在疑点之后）；
    /// `verify.rs` 不在 → 只计 `needs_out_of_diff`，不入队（`pick_bundle` 对不在任何 bundle 的文件会静默退回 bundles[0]，
    /// 产出「文件名是 A、证据是 B」的错配包）。
    #[tokio::test(start_paused = true)]
    async fn needs_enqueue_only_inside_the_changeset() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a"]);
        let need = |path: &str| crate::domain::NeedEntry {
            path: path.into(), lines: Some("1-3".into()), reason: "要看实现".into(), kind: crate::domain::NeedKind::Evidence,
        };
        let out = crate::domain::ScanOutput {
            suspicions: vec![crate::domain::Suspicion {
                file: "a.rs".into(), line_start: 2, line_end: 2, hint: "h".into(), category: None, confidence: 0.5,
                quote: "    body();".into(),
            }],
            needs: vec![need("a.rs"), need("verify.rs")],
            ..Default::default()
        };
        let id = "scan[g1]@p/alpha#1";
        let cells = [ScanCellResult {
            cell_id: id.into(), group: 0, route_key: "p/alpha".into(), family: "alpha".into(), sample: 1,
            record: CellRecord::ok(id, "p/alpha", crate::domain::CellOutput::Scan(out.clone())),
            output: Some(out),
        }];
        let backend = Routed::new(vec![("a", 5, vec![found("a")])]);
        let reg = routed_registry(&backend, 4);

        let run = run_with(Run { scan_cells: &cells, budget: 4, ..plain(&reg, dir.path(), &bundles, &[]) }).await;

        // `a.rs` 的 needs 与该文件的疑点落在同一个 def 块 → 并成一项（评审 #6），到达键两条都记着；
        // `verify.rs` 不在变更集 → 只计数、不入队。
        let kinds: Vec<(&str, ItemKind)> = run.items.iter().map(|it| (it.spec.file.as_str(), it.kind)).collect();
        assert_eq!(kinds, [("a.rs", ItemKind::Suspicion)], "{:?}", run.items);
        assert_eq!(run.items[0].arrivals.iter().filter(|k| k.starts_with("needs:a.rs:")).count(), 1, "{:?}", run.items[0].arrivals);
        assert_eq!(run.meta.enqueue_reasons.get("needs_out_of_diff"), Some(&1));
        assert_eq!(run.meta.suspicions_arrived, 1, "needs 不计进疑点到达数");
    }

    /// 选项观测五量与手算一致（design C3 §7〔D16〕）：三文件四疑点（a.rs 的两条同块并一项）→ 到达 4、疑点项 3、候选文件 3；
    /// K = 2 只派两格（每文件至多一格，c.rs 的项被截）→ 选中文件 2、单文件最大占格 1。
    #[tokio::test(start_paused = true)]
    async fn selection_observables_match_hand_count() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a", "b", "c"]);
        let susp = |file: &str, line: u32, quote: &str, hint: &str| crate::domain::Suspicion {
            file: file.into(), line_start: line, line_end: line, hint: hint.into(), category: None, confidence: 0.5,
            quote: quote.into(),
        };
        let out = crate::domain::ScanOutput {
            suspicions: vec![
                susp("a.rs", 2, "    body();", "一"),
                susp("a.rs", 3, "}", "二"),
                susp("b.rs", 2, "    body();", "三"),
                susp("c.rs", 2, "    body();", "四"),
            ],
            ..Default::default()
        };
        let id = "scan[g1]@p/alpha#1";
        let cells = [ScanCellResult {
            cell_id: id.into(), group: 0, route_key: "p/alpha".into(), family: "alpha".into(), sample: 1,
            record: CellRecord::ok(id, "p/alpha", crate::domain::CellOutput::Scan(out.clone())),
            output: Some(out),
        }];
        let backend = Routed::new(vec![("a", 5, vec![found("a")]), ("b", 5, vec![found("b")]), ("c", 5, vec![found("c")])]);
        let reg = routed_registry(&backend, 4);

        let run = run_with(Run { scan_cells: &cells, budget: 2, ..plain(&reg, dir.path(), &bundles, &[]) }).await;

        let m = &run.meta;
        assert_eq!(
            (m.suspicions_arrived, m.items_from_suspicions, m.files_candidate, m.files_selected, m.max_cells_per_file),
            (4, 3, 3, 2, 1),
            "{m:?}"
        );
        assert_eq!(m.budget_skipped, 1, "第三个文件的项被预算截掉——候选 3 个文件、选中 2 个");
    }

    /// 回放把被截项并成必挖挖掉后不留假债（评审 #9）：截断数按收口时的最终状态数，不按 drain 那一刻——
    /// 按 drain 那一刻计会披露「skipped(budget) 1」并计 exit 2，而那一项其实已经挖了。
    #[tokio::test(start_paused = true)]
    async fn replayed_merge_reopens_skipped_item_without_leaving_debt() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["k"]);
        let cell_id = "scan[g1]@p/beta#1";
        let s = crate::domain::Suspicion {
            file: "k.rs".into(), line_start: 1, line_end: 1, hint: "另一族的同一处".into(), category: None, confidence: 0.6,
            quote: "fn k() {".into(),
        };
        let out = crate::domain::ScanOutput { suspicions: vec![s], ..Default::default() };
        let cells = [ScanCellResult {
            cell_id: cell_id.into(), group: 0, route_key: "p/beta".into(), family: "beta".into(), sample: 1,
            record: CellRecord::ok(cell_id, "p/beta", crate::domain::CellOutput::Scan(out.clone())),
            output: Some(out),
        }];
        let mut k = item_of("k", ItemKind::Suspicion, ItemState::Skipped, 0);
        k.families = vec!["alpha".into()];
        // C3〔D16〕：必挖轴看项内最大簇的家族数 → 已有那条疑点得记着是谁提的（`QueueItem::new` 本来就这么填）。
        k.member_families = vec![vec!["alpha".into()]];
        let backend = Routed::new(vec![("k", 10, vec![found("k")])]);
        let reg = routed_registry(&backend, 4);

        let run = run_with(Run { scan_cells: &cells, budget: 4, ..plain(&reg, dir.path(), &bundles, &[k]) }).await;

        assert_eq!(backend.starts(), ["k"], "异族并入抬到共识 2 → 必挖不截: {:?}", run.warnings);
        assert_eq!(run.meta.budget_skipped, 0, "复位并挖掉后不留假债");
    }

    /// 同一格的疑点是一批到达（D13 Q-B 逐 cell 交付）：排空后再填门，扫描期额度按优先级消耗，不随模型输出序（评审 #4）。
    /// K = 3（额度 2）：H0 占 1 格后，同批 [低 0.3, 高 0.9] 只能派一格——派高的；低的留到收场与后到的中 0.7 比优先级，输给它被截。
    #[tokio::test(start_paused = true)]
    async fn batched_arrivals_spend_quota_by_priority_not_output_order() {
        let t0 = tokio::time::Instant::now();
        let dir = tempfile::tempdir().unwrap();
        let names = ["p", "lo", "hi", "mid"];
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(names.iter().map(|n| (*n, 20, vec![found(n)])).collect());
        let reg = routed_registry(&backend, 8);
        let (tx, rx) = mpsc::channel(8);
        let feed = async move {
            tx.send(rule_req("p", ItemKind::Primary, 1.0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 10)).await;
            // 同一格背靠背发出（通道未满不让出执行权 → 深挖侧一次唤醒全收）
            tx.send(scan_req("lo", 0.3, "alpha", "scan[g1]@p/alpha#1", 0)).await.unwrap();
            tx.send(scan_req("hi", 0.9, "alpha", "scan[g1]@p/alpha#1", 1)).await.unwrap();
            tokio::time::sleep_until(at(t0, 50)).await;
            tx.send(scan_req("mid", 0.7, "alpha", "scan[g1]@p/beta#1", 0)).await.unwrap();
            tokio::time::sleep_until(at(t0, 100)).await;
        };
        let run_fut = run_with(Run { incoming: Some(rx), budget: 3, first_run: true, ..plain(&reg, dir.path(), &bundles, &[]) });
        let (run, ()) = tokio::join!(run_fut, feed);
        let mut dug = backend.starts();
        dug.sort();
        assert_eq!(dug, ["hi", "mid", "p"], "同批只派高优先的那条: {:?}", run.warnings);
        let skipped: Vec<&str> = run.items.iter().filter(|it| it.state == ItemState::Skipped).map(|it| it.spec.file.as_str()).collect();
        assert_eq!(skipped, ["lo.rs"]);
        assert_eq!((run.meta.scan_phase_dispatched, run.meta.budget_skipped), (2, 1));
    }

    /// 共门时等门挂起按轮转序登记（评审 #6 回归）：两成员同 provider、门宽 1、4 项 → 交替分派；
    /// 按成员下标升序登记时门位恒归 0 号，另一成员一格不分。
    #[tokio::test(start_paused = true)]
    async fn shared_gate_members_take_turns_through_waits() {
        let dir = tempfile::tempdir().unwrap();
        let names = ["a", "b", "c", "d"];
        let bundles = routed_bundles(dir.path(), &names);
        let backend = Routed::new(names.iter().map(|n| (*n, 50, vec![found(n)])).collect());
        let reg = pool_registry(&[("p/r", "p", 1, &backend), ("p/s", "p", 1, &backend)]);
        let members = [member("p/r", "p"), member("p/s", "p")];
        let items = pending_items(&names);
        let run = run_with(Run { reviewers: Some(&members), ..plain(&reg, dir.path(), &bundles, &items) }).await;
        let models: Vec<&str> = run.cells.iter().map(|c| c.model.as_str()).collect();
        assert_eq!(models, ["p/r", "p/s", "p/r", "p/s"], "{:?}", run.warnings);
        assert!(!run.warnings.iter().any(|w| w.contains("received 0 cells")), "{:?}", run.warnings);
    }

    /// restore 模式参数（design C2 §6〔D15〕）：`scan_cells` 按到达键幂等入队（已到达过的疑点不改项、不计去重），新疑点照常入队；
    /// `reopen_skipped` 把被截项复位、与新疑点一起经收场 drain 重选；不复位时 Skipped 沿用。`items` 带回最终队列项。
    #[tokio::test(start_paused = true)]
    async fn restore_replays_scan_cells_idempotently_and_reopens_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let bundles = routed_bundles(dir.path(), &["a", "b", "k"]);
        let cell_id = "scan[g1]@p/alpha#1";
        let susp = |name: &str, conf: f32| crate::domain::Suspicion {
            file: format!("{name}.rs"), line_start: 1, line_end: 1, hint: "h".into(), category: None, confidence: conf,
            quote: format!("fn {name}() {{"),
        };
        let out = crate::domain::ScanOutput { suspicions: vec![susp("a", 0.5), susp("b", 0.6)], ..Default::default() };
        let cells = [ScanCellResult {
            cell_id: cell_id.into(), group: 0, route_key: "p/alpha".into(), family: "alpha".into(), sample: 1,
            record: CellRecord::ok(cell_id, "p/alpha", crate::domain::CellOutput::Scan(out.clone())),
            output: Some(out),
        }];
        // 快照：a 已由首跑流式到达并挖完；k 首跑收场被截
        let mut a = item_of("a", ItemKind::Suspicion, ItemState::Settled, 1);
        a.arrivals = vec![crate::consolidate::scan::arrival_key(cell_id, 0, &susp("a", 0.5))];
        let items = vec![a, item_of("k", ItemKind::Ripple, ItemState::Skipped, 0)];
        for reopen in [false, true] {
            let backend = Routed::new(vec![("b", 10, vec![found("b")]), ("k", 10, vec![found("k")])]);
            let reg = routed_registry(&backend, 4);
            let run = run_with(Run {
                scan_cells: &cells, reopen_skipped: reopen, ..plain(&reg, dir.path(), &bundles, &items)
            })
            .await;
            assert_eq!((run.meta.dedup_hits, run.meta.enqueue_reasons.get("scan")), (0, Some(&1)), "a 幂等跳过、b 新入队");
            let dug = backend.starts();
            let state = |f: &str| run.items.iter().find(|it| it.spec.file == f).map(|it| it.state);
            if reopen {
                assert_eq!(dug, ["k", "b"], "k 复位后与 b 一起重选（涟漪优先于疑点）: {:?}", run.warnings);
                assert_eq!((state("k.rs"), run.meta.budget_skipped), (Some(ItemState::Settled), 0));
            } else {
                assert_eq!(dug, ["b"], "{:?}", run.warnings);
                assert_eq!((state("k.rs"), run.meta.budget_skipped), (Some(ItemState::Skipped), 1), "不复位 → Skipped 沿用、照计");
            }
            assert_eq!(run.items.len(), 3);
        }
    }
}
