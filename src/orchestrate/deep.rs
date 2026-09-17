//! 深挖循环（M21b 批 C-4 / C1）：H0 + 疑点入队 → drain → 门驱动并发派发（try_lease×pop → 一格 future）→ barrier → 按派发序组装。
//! 无 reviewer / scanner 失败时调用方不进本模块（scan-only / Incomplete）。

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::mpsc;

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
use crate::orchestrate::queue::{DeepQueue, EnqueueOutcome, EnqueueReq, ItemKind, ItemState, QueueItem};
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
    /// 在途入队通道（批 C2 接 scanner 流式扇出；生产暂传 `None` = 已关闭）。到达项走入口五态，C1 里不过预算。
    /// barrier 含「通道已关闭」：发送端未全部 drop 前，队列空也不收场。
    pub incoming: Option<mpsc::Receiver<EnqueueReq>>,
}

pub async fn run_deep(registry: &ProviderRegistry, mut input: DeepInput<'_>, sink: &dyn FindingSink) -> DeepRun {
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
        if let Some(items) = input.restore {
            // 早退也带上快照里的截断数（评审修 #3）：否则首跑被预算截掉、从没挖过的项既不计债也不披露。
            meta = restored_meta(items);
        }
        if unfinished {
            warnings.push(
                "deep: 变更集为空（diff 已不可重建），队列未完项无法重跑——带债收口".into(),
            );
        }
        return DeepRun { findings: prior_findings, cells: vec![], meta, warnings, reviewer_failed: unfinished, reused_cells: vec![] };
    }

    let mut q = match input.restore {
        Some(items) => DeepQueue::restore(items.to_vec()),
        None => DeepQueue::new(),
    };

    if let Some(items) = input.restore {
        meta = restored_meta(items);
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

    if input.reviewers.is_empty() {
        return DeepRun { findings: vec![], cells: vec![], meta, warnings, reviewer_failed: false, reused_cells: vec![] };
    }
    let members: &[EffectiveModel] = input.reviewers;
    let mut incoming = input.incoming.take();
    let idx = cross_index(input.bundles, &[], None);
    let opts = PackOpts::from(input.pack);
    let env = CellEnv { input: &input, idx: &idx, opts: &opts, sink };

    // 门驱动并发派发（design §6 规则 5〔D13〕）：provider 门有空位才弹出最高 Pending 交出去。
    // 同 task 内并发（`FuturesUnordered`）：future 只借共享引用跑一格；队列 / meta / cells / 回调只在本循环写。
    let mut running = FuturesUnordered::new();
    let mut landed: BTreeMap<u64, Landed> = BTreeMap::new();
    let mut seq = 0u64;
    let mut closed = incoming.is_none();
    // 分派池空（成员全部出局）= 停派：通道未关照收、在途照等，剩余项保持 Pending（resume 可续）。
    let mut pool = Pool::new(members.len());
    // 兜底阻塞 lease 拿到的门位（连同成员下标），下一轮填门时先用。
    let mut parked: Option<(ScanLease<'_>, usize)> = None;
    let mut reviewer_failed = false;

    loop {
        // ① 填门位：池内成员空闲轮转，谁先拿到门位谁消费下一项
        while !pool.is_empty() && q.pending_count() > 0 {
            let next = match parked.take() {
                Some(p) => Some(p),
                None => pool.next_lease(registry, members),
            };
            let Some((lease, member)) = next else { break }; // 各门全满
            let rev = &members[member];
            let Some(item) = q.pop_dispatch() else { break };
            snap_one(&item, input.snapshot);
            // cell 名按队列项（id 已定死），跨 resume 调用稳定：同项重跑 upsert 覆盖自身（与 scanner「重试者胜」同义），
            // 不同项永不同名。按本次调用的派发序编号时，resume 挖别的项会同名覆盖首跑的格、丢掉它的 finding。
            let cell_id = review_cell_id(&item.id, &rev.name);
            let pack = match region::build(&item.spec, pick_bundle(input.bundles, &item.spec.file), input.root, &idx, 64 * 1024, &opts, "") {
                Ok(p) => p,
                Err(e) => {
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
            seq += 1;
            pool.assigned(member);
            running.push(run_cell(&env, Dispatched { seq, member, rev, lease, item, prompt, cell_id, wait_ms }));
        }

        // ② barrier：在途 0 ∧（Pending 0 ∨ 池空）∧ 通道已关（design §2；复核在途批 D 前恒 0）
        if running.is_empty() && closed && (pool.is_empty() || q.pending_count() == 0) {
            break;
        }
        // 在途 0 ∧ Pending > 0 ∧ 各门仍满 = 门被本循环之外的持有者占着（批 C2 起 scanner 与深挖同门）：
        // 同时等每个活跃成员的门位、先到先用，期间照收通道——不空转、不提前收场。
        if running.is_empty() && !pool.is_empty() && q.pending_count() > 0 {
            let alive: Vec<usize> = pool.alive().collect();
            let mut waits: FuturesUnordered<_> = alive
                .into_iter()
                .map(|i| async move { (i, registry.lease(&members[i].name).await) })
                .collect();
            tokio::select! {
                Some((i, l)) = waits.next() => match l {
                    Ok(l) => parked = Some((l, i)),
                    Err(e) => pool.remove(i, format!("deep: reviewer {} lease failed: {e} — removed from pool", members[i].name)),
                },
                msg = recv(&mut incoming), if !closed => on_arrival(msg, &mut q, &mut meta, &mut closed, input.snapshot),
            }
            continue;
        }

        // ③ 等事件：任一格完成（完成即归还门位）或经通道到达一项
        tokio::select! {
            Some(done) = running.next(), if !running.is_empty() => {
                let l = land(done, &mut q, input.snapshot, input.on_cell);
                reviewer_failed |= l.debt;
                if l.auth_failed {
                    // 成员健康（C1 §4.2）：鉴权失败不会自愈 → 出局，后续项只交其余成员。这一格保持失败、不改派（D7 A3 无候补）；
                    // 该成员其他在途格照常跑完。只在「队列项 > 并发容量」时起作用——小队列 t=0 就全派出去了。
                    pool.remove(l.member, format!("deep: reviewer {} auth failed — removed from pool", members[l.member].name));
                }
                landed.insert(l.seq, l);
            }
            msg = recv(&mut incoming), if !closed => on_arrival(msg, &mut q, &mut meta, &mut closed, input.snapshot),
            else => break,
        }
    }
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
    if seq > 0 {
        // 有格派出去了、某成员却一格没分到——多 reviewer 配置里「白配了」必须可见（批 C dogfood grok-4.6 空转且无声）。
        // 出局的成员已由出局原因披露，不重复报。
        for (i, m) in members.iter().enumerate() {
            if pool.cells[i] == 0 && pool.removed[i].is_none() {
                warnings.push(format!("deep: reviewer {} configured but received 0 cells", m.name));
            }
        }
    }

    // 收场：按派发序组装（design C1 §3.4）。持久化已在完成时做完；报告侧的 findings（→ dedup 以输入序首个为 base）、
    // cells、meta 四数组与每格 warnings 按派发序喂——完成序随网络抖动，按它组装报告文本会逐次漂移、meta 数组互相错位。
    let mut findings = Vec::new();
    let mut cells = Vec::new();
    let mut bypass_missing: Vec<String> = Vec::new();
    let mut cell_warnings = Vec::new();
    for l in landed.into_values() {
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
    }
}

/// 一格 future 借用的共享面（全部活过 `run_deep`；同 task 并发，不要求 `Send` / `'static`）。
struct CellEnv<'b> {
    input: &'b DeepInput<'b>,
    idx: &'b crate::bundle::CrossIndex,
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
    seq: u64,
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
    seq: u64,
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

/// 完成时已落盘的一格，按派发序暂存待收场组装。
struct Landed {
    seq: u64,
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
    let Dispatched { seq, member, rev, lease, item, prompt, cell_id, wait_ms } = d;
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
        seq,
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

/// 完成时刻（主循环内）：回填落地共识 → 落盘 cell → 队列结案 → 快照。报告侧组装留到收场按派发序做。
fn land(
    done: CellDone<'_>,
    q: &mut DeepQueue,
    snapshot: Option<&dyn Fn(&QueueItem)>,
    on_cell: Option<&dyn Fn(&CellRecord)>,
) -> Landed {
    let CellDone {
        seq, member, rev, item, cell_id, duration_ms, wait_ms, outcome, rl_retries, rl_wait_ms, stage2, stage2_error, bypass_missing, auth_failed, mut warnings,
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
        seq,
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

/// 经入队通道到达一项（批 C2 的 scanner 流式扇出接这里）：走入口五态 + 快照 + 入队原因 + 队列深度峰值（运行中取最大）。
/// C1 里到达项**不过预算**——在线预算与疑点保底预留归 C2。`None` = 发送端全部 drop → 通道关闭。
fn on_arrival(
    msg: Option<EnqueueReq>,
    q: &mut DeepQueue,
    meta: &mut DeepMeta,
    closed: &mut bool,
    snapshot: Option<&dyn Fn(&QueueItem)>,
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
    note_enq(q.enqueue(req), reason, meta);
    // 入队不回报受影响项，全量写快照（C1 无生产到达项；C2 流式量大时改为只写受影响项）。
    snap_all(q, snapshot);
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

/// resume 形态的深挖 meta 起点：截断数与队列深度取自快照（drain 在首跑已做，resume 不重跑）。
/// resume 的三条路径共用——正常续跑、变更集为空早退、diff 重建报错（`cli::run_resume`）——任何一条漏掉截断数，
/// 被预算截掉的项就既不计债也不披露。
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
        .filter_map(|n| crate::cli::second_pass::need_section(&ex, input.root, n, warnings))
        .collect();
    if ev.trim().is_empty() {
        return Round2::NoEvidence;
    }
    let pack2 = match region::build(&item.spec, pick_bundle(input.bundles, &item.spec.file), input.root, env.idx, 64 * 1024, env.opts, &ev) {
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
    note_enq(q.enqueue(EnqueueReq { spec, score: h.score, kind, families: vec![], arrival: None }), "rule", meta);
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
                arrival: None,
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
            arrivals: vec![],
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
                backend_factory: None, on_cell: None, incoming: None,
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
                backend_factory: None, on_cell: None, incoming: None,
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
                backend_factory: None, on_cell: None, incoming: None,
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
                backend_factory: None, on_cell: None, incoming: None,
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
                tools: ToolsMode::Off, backend_factory: None, on_cell: None, incoming: None,
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
                tools: ToolsMode::TwoStage, backend_factory: None, on_cell: None, incoming: None,
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
    }

    async fn run_with(r: Run<'_>) -> DeepRun {
        let single = [fake_reviewer()];
        run_deep(
            r.reg,
            DeepInput {
                route: None, scan_cells: &[], bundles: r.bundles, root: r.root,
                reviewers: r.reviewers.unwrap_or(&single), budget_cells: 16,
                pack: &Default::default(), spec_docs: "", target_label: "t",
                snapshot: r.snapshot, restore: Some(r.items), prior_findings: vec![], tools: r.tools,
                backend_factory: None, on_cell: r.on_cell, incoming: r.incoming,
            },
            r.sink,
        )
        .await
    }

    fn plain<'a>(reg: &'a crate::provider::registry::ProviderRegistry, root: &'a Path, bundles: &'a [Bundle], items: &'a [QueueItem]) -> Run<'a> {
        Run { reg, root, bundles, items, tools: ToolsMode::Off, incoming: None, on_cell: None, snapshot: None, sink: &(), reviewers: None }
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

    /// 乱序完成不错配、慢格不堵：完成序 b→c→a，每格 findings 仍对应自己的文件；报告里的 cells 按派发序（a→b→c）；
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
        assert_eq!(report_order, vec!["review[a.rs:rule:1-2]@p/r", "review[b.rs:rule:1-2]@p/r", "review[c.rs:rule:1-2]@p/r"], "报告按派发序");
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
            arrivals: vec![],
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
    /// meta 四数组与 cells 等长且按派发序对齐（失败格也各占一位）。
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
            assert_eq!(m.reviewer_output_tokens, vec![1, 7, 1], "失败格记烧掉的 output、按派发序");
            for c in run.cells.iter_mut() {
                c.duration_ms = 0;
                c.queue_wait_ms = 0;
            }
            run.meta.item_wait_ms.clear();
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
            arrivals: vec![],
        };
        let note = crate::domain::Suspicion {
            file: "s.rs".into(), line_start: 1, line_end: 1, hint: "h".into(), category: None, confidence: 0.5, quote: "fn s() {".into(),
        };
        let scan = QueueItem {
            id: "s.rs:scan:1-1".into(),
            spec: QueueItemSpec::new("s.rs".into(), vec![(1, 1)], QueueOrigin::Scan, vec![note]).unwrap(),
            score: 0.5, agreement: 2, state: ItemState::Pending, kind: ItemKind::Suspicion,
            dispatch_agreement: 0, landed_agreement: None, families: vec!["alpha".into(), "beta".into()], enqueued_ms: None,
            arrivals: vec![],
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
                tools: ToolsMode::TwoStage, backend_factory: None, on_cell: Some(&on_cell), incoming: None,
            },
            &(),
        )
        .await;
        let mut landed = landed.lock().unwrap().clone();
        let snaps = snaps.lock().unwrap().clone();
        // cell 名由派发序（批 C `review[{n}]`）改为按队列项（C1-1 有意改动）：按报告里的派发序映射回 n 再比。
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
}
