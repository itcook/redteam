//! 深挖队列（M21b 批 C-1，design §6 规则 2〔D13〕）：入口五态 + agreement 活值。
//! 合并谓词复用 `consolidate::scan::same_suspicion`，不复制。跨轮判例守卫本批空过（按无命中入队）。
//!
//! C3〔D16〕起**结构定项、谓词定共识**：项身份 = 同文件 ∧ 同稳定块（`orchestrate::blocks`），
//! `same_suspicion` 降级为项内聚簇依据，产出两轴——`peak_agreement`（最大簇家族数，定必挖）
//! 与 `agreement`（块被几个家族碰过，进排序）。无块信息（旧快照 / 内容不可读）时退回 C1/C2 的 quote 谓词。

use crate::consolidate::scan::same_suspicion;
use crate::domain::Suspicion;
use crate::orchestrate::blocks::Block;
use crate::pack::region::{Origin as QueueOrigin, QueueItemSpec};

/// needs 中档分（低于疑点层、高于被截项）。
const NEEDS_MID_SCORE: f32 = 0.5;

/// 队列项运行时状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemState {
    Pending,
    InFlight,
    Settled,
    Skipped,
}

/// 优先级分层（越小越先挖）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Primary = 0,
    Ripple = 1,
    Suspicion = 2,
    Needs = 3,
}

/// 入队结果：五态可观测 + C2 的到达键幂等 `Duplicate`，共六种。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// 无命中 → 新项 agreement = 1。
    New,
    /// 命中排队中 → 并入（异族 agreement+1 / 同家族只合并）；或命中被截项、并入后成为必挖 → 复位排队。
    MergedPending,
    /// 命中在途 → 只改记录，不重挖。
    MergedInFlight,
    /// 命中本轮已结案 → 并入并沿用结案。
    MergedSettled,
    /// 命中被预算截掉的项、并入后仍非必挖 → 只改记录，保持截断（不突破 drain 定下的预算）。
    MergedSkipped,
    /// 到达键已记在某项上（同一条疑点再次到达：resume 回放 scanner 格、每轮重入队 H0）→ 不改任何项、不计去重命中。
    /// 收场拆簇对账读的是项上记着的到达键，不经本入口。
    Duplicate,
}

/// 深挖队列项（静态形状对齐 `QueueItemSpec`，另加运行时字段）。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QueueItem {
    pub id: String,
    pub spec: QueueItemSpec,
    pub score: f32,
    pub agreement: u32,
    pub state: ItemState,
    pub kind: ItemKind,
    pub dispatch_agreement: u32,
    pub landed_agreement: Option<u32>,
    /// 已贡献 agreement 的家族（同家族重复不加）。
    pub families: Vec<String>,
    /// 入队时刻（epoch ms，观测：item_wait_ms = 派发时刻 − 此值）。None = 旧快照。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enqueued_ms: Option<u64>,
    /// 并入本项的到达键（疑点 `{scan cell_id}#{i}:{内容指纹}`、H0 `rule:{file}:{start}-{end}`，C2〔D15〕）：再到达即幂等（`EnqueueOutcome::Duplicate`），
    /// 收场据此把组内闭包簇映射回队列项做拆簇对账。H0 无到达键；旧快照空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arrivals: Vec<String>,
    /// 到达过本项的全部疑点（C2 评审 #3）：`spec.notes` 只留渲染给 reviewer 的代表（相似的不重复列），
    /// 匹配面与优先级用这一份全量——桥接疑点若只并入不进匹配面，链式簇会按到达序拆成多项，
    /// 代表也被钉在最先到的那条上（`priority_score` 取笔记里的最高置信）。H0 / needs 无疑点，空。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub members: Vec<Suspicion>,
    /// 与 [`Self::members`] 同下标的家族（该条疑点由哪些家族提出）——项内聚簇算 `peak_agreement` 的输入（C3〔D16〕）。
    /// 实施细化：`Suspicion` 本身不带家族，项级 `families` 是并集、无法还原到条——故并存这一列。空 = 旧快照 / 无家族。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub member_families: Vec<Vec<String>>,
    /// 稳定块（C3〔D16〕）：项身份锚在结构上——同文件同 `block.key` 即同一项，区间取块的行范围。
    /// 空 = 无块信息（旧快照 / 内容不可读）→ 合并退回 quote 谓词，行为与 C2 一致。
    #[serde(default, skip_serializing_if = "Block::is_empty")]
    pub block: Block,
    /// 必挖轴（C3〔D16〕）：项内按 `same_suspicion` 聚簇后**最大簇**的家族数。
    /// 0 = 未算（旧快照 / 无疑点）→ 判定退回区域级 `agreement`。
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub peak_agreement: u32,
    /// 所在风险区档（C3-2 排序用，入队时由 `RiskRoute.zones` 算）：3 = 分 ≥ θ · 2 = ≥ θ/2 · 1 = > 0 · 0 = 无区。
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub risk_band: u8,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

impl QueueItem {
    fn new(req: EnqueueReq, block: Block, risk_band: u8) -> Self {
        let EnqueueReq { mut spec, score, kind, families, arrival } = req;
        // 区间 = 块的行范围（design C3 §2）：项覆盖整个 def / 标题节，id 也随之锚在结构上
        // （同块的两条疑点算出同一 id——取消 id 捷径后 id 唯一仍成立）。
        if !block.is_empty() {
            spec.ranges = vec![(block.start, block.end)];
        }
        let id = item_id(&spec);
        let families: Vec<String> = dedup_families(families);
        let members = spec.notes.clone();
        let member_families = vec![families.clone(); members.len()];
        let mut it = QueueItem {
            id,
            spec,
            score,
            agreement: agreement_of(&families),
            state: ItemState::Pending,
            kind,
            dispatch_agreement: 0,
            landed_agreement: None,
            families,
            enqueued_ms: Some(now_ms()),
            arrivals: arrival.into_iter().collect(),
            members,
            member_families,
            block,
            peak_agreement: 0,
            risk_band,
        };
        it.recompute_peak();
        it
    }

    /// 项内聚簇 → 必挖轴（C3〔D16〕）：`same_suspicion` 并查集分簇，取最大簇的**不同家族数**。
    /// 与 D7 同口径（同家族多 samples 计一次），故不会像区域级共识那样把必挖集撑爆。
    ///
    /// **只增不减**（评审 #5）：项内成员只增不删，算出的值本就单调；取 `max` 是为了旧快照——
    /// 它的 `agreement` 在 `restore` 时被当作必挖轴的下限种进来，新到达一条疑点算出的 1 不得把它降回非必挖。
    fn recompute_peak(&mut self) {
        self.peak_agreement = self.peak_agreement.max(peak_agreement(&self.members, &self.member_families));
    }

    /// 达成必挖轴峰值的那些疑点的行区间（批 D 的盖章面）：finding 压在这些行上才算「两家族对上的那一处」。
    /// 旧快照（无逐条家族）算不出簇 → 空，调用方一律不盖——偏差方向是多复核，不是漏复核。
    pub fn hot_ranges(&self) -> Vec<(u32, u32)> {
        peak_cluster(&self.members, &self.member_families).1
    }

    /// 匹配与排序用的疑点全量：到达过的成员（旧快照为空时退回渲染笔记）。
    pub(crate) fn suspicions(&self) -> &[Suspicion] {
        if self.members.is_empty() {
            &self.spec.notes
        } else {
            &self.members
        }
    }
}

/// agreement ≡ 贡献家族数（与 `consolidate::scan::merge_suspicions` 同口径）；
/// 无家族（H0 / needs）记 1——「至少有一个来源提过」。
/// C3〔D16〕起语义是**区域级**：这一块被几个家族碰过（排序轴）；必挖改看 `peak_agreement`。
fn agreement_of(families: &[String]) -> u32 {
    (families.len() as u32).max(1)
}

/// 项内最大簇的家族数。`members` 与 `fams` 同下标；对不齐或无家族信息 → 0（调用方退回 `agreement`）。
fn peak_agreement(members: &[Suspicion], fams: &[Vec<String>]) -> u32 {
    peak_cluster(members, fams).0
}

/// 项内聚簇的完整产物（C3 算轴、批 D 还要簇的行范围）：
/// `.0` = 最大簇的家族数（必挖轴）；`.1` = **达成该峰值的簇**覆盖的行区间。
///
/// 批 D 用后者收紧盖章：只有落在「≥2 家族真正对上的那几行」上的 finding 才配拿区域共识，
/// 同块里 reviewer 自己新发现的另一处不该白拿免验（C3 把项放大成整个 def 之后这个口子尤其大）。
/// 并列峰值的簇全算——并列意味着同等佐证，没有理由只认其中一个。
fn peak_cluster(members: &[Suspicion], fams: &[Vec<String>]) -> (u32, Vec<(u32, u32)>) {
    if members.is_empty() || fams.len() != members.len() {
        return (0, Vec::new());
    }
    let n = members.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            if same_suspicion(&members[i], &members[j]) {
                let (ri, rj) = (uf_find(&mut parent, i), uf_find(&mut parent, j));
                if ri != rj {
                    parent[rj] = ri;
                }
            }
        }
    }
    let mut clusters: Vec<(usize, Vec<&str>)> = Vec::new();
    for (i, fam) in fams.iter().enumerate() {
        let root = uf_find(&mut parent, i);
        let slot = match clusters.iter_mut().find(|(r, _)| *r == root) {
            Some(s) => &mut s.1,
            None => {
                clusters.push((root, Vec::new()));
                &mut clusters.last_mut().unwrap().1
            }
        };
        for f in fam.iter().filter(|f| !f.is_empty()) {
            if !slot.contains(&f.as_str()) {
                slot.push(f);
            }
        }
    }
    let peak = clusters.iter().map(|(_, f)| f.len() as u32).max().unwrap_or(0);
    if peak == 0 {
        return (0, Vec::new());
    }
    let hot: Vec<usize> = clusters.iter().filter(|(_, f)| f.len() as u32 == peak).map(|(r, _)| *r).collect();
    let ranges = (0..n)
        .filter(|&i| hot.contains(&uf_find(&mut parent, i)))
        .map(|i| (members[i].line_start.max(1), members[i].line_end.max(members[i].line_start.max(1))))
        .collect();
    (peak, ranges)
}

/// finding 是否压在「两家族对上的那几行」上（批 D 盖章面的单一判定，事件侧与落盘侧共用）。
/// 行重叠口径复用 `LINE_GAP`（= 3），与 `same_suspicion` 同源——模型报的行号本来就有 ±1 的抖动。
/// `hot` 为空（旧快照 / 算不出簇）→ 一律不盖：偏差方向是多复核，不是漏复核。
pub(crate) fn in_hot_ranges(line_start: u32, line_end: u32, hot: &[(u32, u32)]) -> bool {
    let gap = crate::consolidate::dedup::LINE_GAP;
    hot.iter().any(|&(a, b)| line_start <= b.saturating_add(gap) && a <= line_end.saturating_add(gap))
}

fn uf_find(parent: &mut [usize], i: usize) -> usize {
    let mut r = i;
    while parent[r] != r {
        r = parent[r];
    }
    let mut c = i;
    while parent[c] != r {
        let next = parent[c];
        parent[c] = r;
        c = next;
    }
    r
}

fn dedup_families(families: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in families.into_iter().filter(|f| !f.is_empty()) {
        if !out.contains(&f) {
            out.push(f);
        }
    }
    out
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 入队参数。`families` 空 = 无家族（H0 / needs），agreement 记 1。
/// 传全量家族集（而非单个）让 agreement 在队列内部算定——调用方不必在 enqueue 之后
/// 反查队列改写，那种事后 fixup 会按 (file, quote) 认错项（同文件同 quote 不同行的两条疑点）。
pub struct EnqueueReq {
    pub spec: QueueItemSpec,
    pub score: f32,
    pub kind: ItemKind,
    pub families: Vec<String>,
    /// 疑点到达键 `{scan cell_id}#{i}`（i = 该格锚定后疑点下标；落盘输出同序，resume 可复算）。H0 / 测试构造为 `None`。
    pub arrival: Option<String>,
}

impl EnqueueReq {
    /// scanner 疑点入队请求——首跑流式到达与 resume 回放共用同一形态（C2〔D15〕）：区间 = 疑点行（起始至少第 1 行）、
    /// 笔记 = 该疑点、分 = confidence、家族 = 产出它的 scan 格家族、到达键 = `{cell_id}#{i}`。区间形状非法 → `None`。
    pub fn scan(s: &Suspicion, family: &str, arrival: String) -> Option<EnqueueReq> {
        let start = s.line_start.max(1);
        let spec = QueueItemSpec::new(s.file.clone(), vec![(start, s.line_end.max(start))], QueueOrigin::Scan, vec![s.clone()]).ok()?;
        Some(EnqueueReq { spec, score: s.confidence, kind: ItemKind::Suspicion, families: vec![family.to_string()], arrival: Some(arrival) })
    }

    /// scanner needs 入队请求（C3〔D16〕Q8，design §6 的第三种生产者）：`kind = evidence` 的一条声明成一项——
    /// 区间取 `lines`（解不出行锚 → 不入队，见下）、无笔记（走与 H0 同款的「区域独立审」）、
    /// 到达键 `needs:{path}:{lines}:{理由指纹}`（同一条声明重复到达即幂等）。
    /// 家族留空：needs 不是「谁看见了什么」，不参与共识。变更集过滤在调用方（队列不知道 diff）。
    pub fn need(n: &crate::domain::NeedEntry) -> Option<EnqueueReq> {
        if n.kind != crate::domain::NeedKind::Evidence || n.path.is_empty() {
            return None;
        }
        // 行锚用与二阶段补查同一份解析（评审 #13）：同一个字段在两条路径上解出两个区间是纯粹的坑。
        // 解不出行锚的不入队——它会变成「无笔记地审这个文件的开头」，白烧一格。实测三轮 needs 全部带 lines。
        let (a, b) = crate::cli::second_pass::parse_need_lines(n.lines.as_deref())?;
        // 路径在入口归一一次（评审）：needs 的 path 直接来自模型、不过 `anchor_suspicions` 那道校验，
        // `./src/a.rs` 与 `src/a.rs` 必须是同一个文件——否则变更集判定、装包取文件、与疑点并项三处都对不上。
        let path = norm_path(&n.path);
        let spec = QueueItemSpec::new(path.clone(), vec![(a, b)], QueueOrigin::Needs, vec![]).ok()?;
        let arrival = format!("needs:{path}:{a}-{b}:{}", crate::hash::fnv1a64_hex(&[n.reason.as_str()]));
        Some(EnqueueReq { spec, score: NEEDS_MID_SCORE, kind: ItemKind::Needs, families: vec![], arrival: Some(arrival) })
    }
}

/// 仓内相对路径归一：`./a.rs` / `/a.rs` → `a.rs`。
pub(crate) fn norm_path(p: &str) -> String {
    p.trim_start_matches("./").trim_start_matches('/').to_string()
}

/// 一次到达（C3〔D16〕）：入队请求 + 调用方算好的稳定块与风险档。
/// 块要读文件内容、风险档要看 `RiskRoute`，两者都不是队列该做的事——队列只比键。
/// 直接喂 `EnqueueReq`（经 `From`）即「无块信息」：旧快照回放与单测构造走这条，合并退回 quote 谓词。
pub struct Arrival {
    pub req: EnqueueReq,
    pub block: Block,
    pub risk_band: u8,
}

impl From<EnqueueReq> for Arrival {
    fn from(req: EnqueueReq) -> Self {
        Arrival { req, block: Block::default(), risk_band: 0 }
    }
}

/// 内存队列（C-1 只含入队；派发/预算 C-2/C-3）。
#[derive(Clone, Debug, Default)]
pub struct DeepQueue {
    items: Vec<QueueItem>,
}

impl DeepQueue {
    pub fn new() -> Self {
        Self::default()
    }

    /// 快照恢复（C-5 resume）：settled/skipped 保留状态；pending/inflight 重置 pending（未完重跑，
    /// enqueued_ms 重置避免 wait 虚高）。
    pub fn restore(mut items: Vec<QueueItem>) -> Self {
        let now = now_ms();
        for it in items.iter_mut() {
            // 旧快照（C3 之前）没有必挖轴与逐条家族：把往轮的区域级 `agreement` 种成必挖轴下限（评审 #5）。
            // 不种的话，往轮认定的必挖项在本轮到达一条新疑点后会被算成 peak = 1 而降级、被收场 drain 截掉——
            // 「更多佐证反而失去保护」。种了之后语义就是「旧项按 C2 口径判必挖」，与不动它时一致。
            if it.peak_agreement == 0 && it.member_families.is_empty() {
                it.peak_agreement = it.agreement;
            }
            match it.state {
                ItemState::Pending | ItemState::InFlight => {
                    it.state = ItemState::Pending;
                    it.enqueued_ms = Some(now);
                }
                ItemState::Settled | ItemState::Skipped => {}
            }
        }
        DeepQueue { items }
    }

    pub fn items(&self) -> &[QueueItem] {
        &self.items
    }

    /// 被截项全部复位 Pending（resume 补跑过 scanner 后重选，design §6 RS1〔D15〕）：入队时刻重置，返回复位项下标（调用方写快照）。
    pub fn reopen_skipped(&mut self) -> Vec<usize> {
        let now = now_ms();
        let mut out = Vec::new();
        for (i, it) in self.items.iter_mut().enumerate() {
            if it.state == ItemState::Skipped {
                it.state = ItemState::Pending;
                it.enqueued_ms = Some(now);
                out.push(i);
            }
        }
        out
    }

    pub fn items_mut(&mut self) -> &mut [QueueItem] {
        &mut self.items
    }

    pub fn get(&self, id: &str) -> Option<&QueueItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// 五态入口。判例守卫空过：调用方把跨轮命中当无命中喂进来即可。
    pub fn enqueue(&mut self, arrival: impl Into<Arrival>) -> EnqueueOutcome {
        self.enqueue_affected(arrival).0
    }

    /// 同 [`Self::enqueue`]，另回报受影响项下标（新建项 / 并入目标；`Duplicate` 为 `None`）——调用方只写这一项的快照（C2 Q9）。
    /// 到达键先查：已记在某项上即幂等返回，不改项、不计去重命中。
    pub fn enqueue_affected(&mut self, arrival: impl Into<Arrival>) -> (EnqueueOutcome, Option<usize>) {
        let Arrival { req, block, risk_band } = arrival.into();
        if let Some(key) = &req.arrival {
            if self.items.iter().any(|it| it.arrivals.contains(key)) {
                return (EnqueueOutcome::Duplicate, None);
            }
        }
        let incoming_id = id_for(&req.spec, &block);
        if let Some((idx, by_predicate)) = self.find_merge_idx(&req.spec, &block, &incoming_id) {
            return (self.merge_into(idx, req, risk_band, by_predicate), Some(idx));
        }
        self.items.push(QueueItem::new(req, block, risk_band));
        (EnqueueOutcome::New, Some(self.items.len() - 1))
    }

    /// 合并目标 + 是否经谓词命中。查找序：**结构块优先** → 谓词（quote / 行带）→ id 兜底。
    ///
    /// 结构块优先（评审 #9）：`same_item` 对不同候选项用的谓词不同（旧快照项走 quote），线性取首个命中
    /// 会让一个旧项把本该归给块主人的到达截走，同一个块键裂成两项——恰是本批要消灭的那种分裂。
    ///
    /// id 兜底（评审 #10）：id 唯一是快照 upsert（`(job, id)` 主键）、`mark_settled` / `get` 的线性查找、
    /// cell 命名三处的硬前提。两项同 id 只会静默互相覆盖。同轮内同文件同区间必然算出同一个块键，
    /// 这条兜底走不到；能走到的只有 resume 后内容变了的情形——那时保 id 唯一比保结构分开更要紧。
    fn find_merge_idx(&self, spec: &QueueItemSpec, block: &Block, incoming_id: &str) -> Option<(usize, bool)> {
        // 第一遍只在同样有结构块的项里找（谓词仍是 `same_item`，不另写一份规则——写两份就会互相掩盖，变异检查也照不出来）。
        if block.is_structural() {
            if let Some(i) = self.items.iter().position(|it| it.block.is_structural() && same_item(it, spec, block)) {
                return Some((i, true));
            }
        }
        if let Some(i) = self.items.iter().position(|it| same_item(it, spec, block)) {
            return Some((i, true));
        }
        self.items.iter().position(|it| it.id == incoming_id).map(|i| (i, false))
    }

    fn merge_into(&mut self, idx: usize, req: EnqueueReq, risk_band: u8, by_predicate: bool) -> EnqueueOutcome {
        let it = &mut self.items[idx];
        let members_before = it.members.len();
        // 共识只经谓词（C1 design §2.2；C2〔D15〕改写前提）：疑点逐 scan 格到达，队列侧逐条把新疑点与已有项的笔记比对，
        // **不是**组内传递闭包——A、C 先到互不匹配、B 后到两边都匹配时 B 只并入先命中的那项，A 与 C 永远两项。非传递漏合并
        // 只会少算共识、多复核一次（D15 实测 ≤ 1/19 簇），收场以 `closure_members` 对账、拆簇数进 RunMeta。
        // 只靠 id 捷径命中的是「谓词不认、恰好行区间逐字相同」的两条，可能是 quote 噪声下的同一问题，也可能是同一行上的
        // 两个问题：不计共识。计错了是批 D 免复核错放，不计错了只是多复核一次——取后者。
        // 但 id 捷径并入的**请求自身**共识不能丢（评审修）：上游已合并成多家族共识的疑点（必挖）撞上同区间的单家族项，
        // agreement 同时决定必挖集与排序分，丢了它就可能被预算截掉——家族不跨疑点并，agreement 取大。
        // 两条路径都取大 → agreement 单调不减（事件侧 `event ≤ landed` 的前提）。
        let incoming_families = dedup_families(req.families);
        if by_predicate {
            for f in incoming_families.iter().filter(|f| !f.is_empty()) {
                if !it.families.contains(f) {
                    it.families.push(f.clone());
                }
            }
            it.agreement = it.agreement.max(agreement_of(&it.families));
        } else {
            it.agreement = it.agreement.max(agreement_of(&incoming_families));
        }
        if let Some(key) = req.arrival {
            it.arrivals.push(key); // 调用方已查过不在任何项上
        }
        // 旧快照项（无块）**不就地升级**（评审 #3）：`id` 只在 `new` 时定死、升级会让 id 与 ranges 各说各话
        // ——cell 名、快照主键、coverage 都报着一个 reviewer 没看过的区间。旧项就按旧语义走到底。
        it.risk_band = it.risk_band.max(risk_band);
        // 「区间 = 块」是身份的一部分（design C3 §2）：有块就恒等于块，不并进到达的报告行（评审 #2）。
        // 否则 quote 跨到块尾外的疑点会把区间撑大，且撑成什么样取决于到达序——同样的输入两种 pack。
        if it.block.is_empty() {
            it.spec.ranges = union_ranges(&it.spec.ranges, &req.spec.ranges);
        }
        // scan 与 needs 同一身份空间：层级取高的那个（`Needs` 并进疑点项后按疑点派，反之亦然）。
        it.kind = it.kind.min(req.kind);
        // 旧快照没有 `member_families`（serde default 空）：先补齐到与 `members` 等长，
        // 否则下面的 `get_mut` 拿不到槽位、家族被静默丢掉，两列长度永久错开（评审 #14）。
        if it.member_families.len() < it.members.len() {
            it.member_families.resize(it.members.len(), Vec::new());
        }
        for n in &req.spec.notes {
            // 家族按条记（`member_families` 与 `members` 同下标）：同一条疑点被两个家族提出时，
            // 家族落在同一槽位——项内聚簇据此算最大簇家族数。
            match it.members.iter().position(|m| m == n) {
                Some(i) => {
                    let slot = &mut it.member_families[i];
                    for f in incoming_families.iter().filter(|f| !f.is_empty()) {
                        if !slot.contains(f) {
                            slot.push(f.clone());
                        }
                    }
                }
                None => {
                    it.members.push(n.clone());
                    it.member_families.push(incoming_families.clone());
                }
            }
        }
        // 只在成员或家族真的动了才重算（评审效率项）：项内聚簇是 O(n²)，纯重复到达没必要再跑一遍。
        if it.members.len() != members_before || !incoming_families.is_empty() {
            it.recompute_peak();
        }
        it.spec.notes = merge_notes(&it.spec.notes, &req.spec.notes);
        if req.score > it.score {
            it.score = req.score;
        }
        // id 不随 ranges 重算（只在 `new` 时定死）：在途项被并入扩了区间，完成时 `mark_settled(派发时 id)`
        // 仍须命中；快照按 item_id upsert，重算还会把同一项裂成两行、restore 出重复项。
        match it.state {
            ItemState::Pending => EnqueueOutcome::MergedPending,
            // 被截项并入后成为必挖（异族谓词共识抬到 2）→ 必挖不截（design §6 规则 3）；
            // 入队时刻重置，免 item_wait_ms 把被截的那段也算成排队。
            // C2〔D15〕起 Skipped 只在关闭入口时的收场 drain 产生、之后首跑再无到达——本分支首跑生产不可达；
            // resume 补跑过 scanner 时的复位走 run_deep 的显式重选路径，不经这里。
            ItemState::Skipped if must_dig(it) => {
                it.state = ItemState::Pending;
                it.enqueued_ms = Some(now_ms());
                EnqueueOutcome::MergedPending
            }
            ItemState::Skipped => EnqueueOutcome::MergedSkipped,
            ItemState::InFlight => EnqueueOutcome::MergedInFlight,
            ItemState::Settled => EnqueueOutcome::MergedSettled,
        }
    }

    /// Pending 项按优先级弹出顺序（不改状态）。必挖集排在最前且不被后续 budget 截掉。
    ///
    /// C3〔D16〕**每文件保底一格**：尚无格的文件，其最高项提到队首（层内仍按 [`cmp_priority`]）。
    /// 实测单文件能吃掉 13 格里的 6 格（该文件 34 条疑点占候选集 87%），一份文档就把预算摊完。
    /// 保底随派发自然失效——该文件有格之后，它的其余项回到常规序。
    /// 派发（`pop_dispatch_where`）与收场 drain 都读这一个序，两者不得分叉：否则收场留下的与实际挖的对不上。
    pub fn pending_order(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.state == ItemState::Pending)
            .map(|(i, _)| i)
            .collect();
        idx.sort_by(|&a, &b| cmp_priority(&self.items[a], &self.items[b]));
        let dispatched: std::collections::HashSet<&str> = self
            .items
            .iter()
            .filter(|it| matches!(it.state, ItemState::InFlight | ItemState::Settled))
            .map(|it| it.spec.file.as_str())
            .collect();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        // 保底只给 scanner 疑点（评审 #8）：集中度问题全在疑点上；needs 是「预算有余才吃格」的层级，
        // 让它靠保底插到别的文件的疑点前面，与 design §6「层级 Needs 天然排在疑点之后」冲突。
        let first_slot: Vec<bool> = idx
            .iter()
            .map(|&i| {
                let it = &self.items[i];
                let f = it.spec.file.as_str();
                it.kind == ItemKind::Suspicion && !dispatched.contains(f) && seen.insert(f)
            })
            .collect();
        // 稳定三段分区：H0（主区 / 涟漪）恒在最前——它是 §9 确定性地板，不让位给多样性启发式（评审 #8）；
        // 其次是各文件的保底疑点；再次是其余项。段内保持 `cmp_priority` 的原序。
        let h0 = |i: usize| matches!(self.items[i].kind, ItemKind::Primary | ItemKind::Ripple);
        let mut out: Vec<usize> = idx.iter().copied().filter(|&i| h0(i)).collect();
        out.extend(idx.iter().zip(&first_slot).filter(|(&i, &f)| !h0(i) && f).map(|(&i, _)| i));
        out.extend(idx.iter().zip(&first_slot).filter(|(&i, &f)| !h0(i) && !f).map(|(&i, _)| i));
        out
    }

    /// 主区 ∪ {agreement ≥ 2}。`max_cells = 0` 时主区仍算必挖。
    pub fn is_must(&self, it: &QueueItem) -> bool {
        must_dig(it)
    }

    /// 一次性 drain（开跑即关闭入口）：= `drain_with(max_cells, ⌈max_cells/4⌉)`；`max_cells = 0` 只留必挖。
    pub fn drain(&mut self, max_cells: u32) -> Vec<String> {
        let floor = if max_cells == 0 { 0 } else { max_cells.div_ceil(4) as usize };
        self.drain_with(max_cells as usize, floor)
    }

    /// 收场 drain（C2 在线预算〔D15〕，关闭入口时执行一次）：Pending 中留下必挖全部 ∪ 按优先级前 `floor` 条非必挖疑点
    /// ∪ 按优先级补到留下数 ≥ `budget`；余者标 Skipped，返回其 id。`budget` = 剩余预算（K − 已派发），
    /// `floor` = 保底剩余（⌈K/4⌉ − 已派发的非必挖疑点）。主区吃穿时帽 = 必挖 + 保底。
    pub fn drain_with(&mut self, budget: usize, floor: usize) -> Vec<String> {
        self.drain_among(budget, floor, |_| true)
    }

    /// 同 [`Self::drain_with`]，只在满足 `candidate` 的 Pending 项里选；其余 Pending 项原样保留、不占 `budget` / `floor`
    /// ——resume 时往轮已派发未完成的项已被往轮选中，调用方把它们计入已派发（design C2 §6〔D15〕）。
    pub fn drain_among(&mut self, budget: usize, floor: usize, candidate: impl Fn(&QueueItem) -> bool) -> Vec<String> {
        let pending: Vec<usize> = self.pending_order().into_iter().filter(|&i| candidate(&self.items[i])).collect();
        if pending.is_empty() {
            return Vec::new();
        }
        let must: Vec<usize> = pending.iter().copied().filter(|&i| self.is_must(&self.items[i])).collect();
        let mut keep: std::collections::HashSet<usize> = must.iter().copied().collect();
        let mut reserved = 0usize;
        for &i in &pending {
            if reserved >= floor {
                break;
            }
            if self.items[i].kind == ItemKind::Suspicion && keep.insert(i) {
                reserved += 1;
            }
        }
        for &i in &pending {
            if keep.len() >= budget {
                break;
            }
            keep.insert(i);
        }
        let mut skipped = Vec::new();
        for i in pending {
            if !keep.contains(&i) {
                self.items[i].state = ItemState::Skipped;
                skipped.push(self.items[i].id.clone());
            }
        }
        skipped
    }

    /// 弹出最高 Pending（C-3 派发用）；C-2 测试只验证顺序。
    pub fn peek_highest(&self) -> Option<&QueueItem> {
        self.pending_order().first().map(|&i| &self.items[i])
    }

    /// 取出最高 Pending 并标 InFlight；记下 dispatch_agreement。
    pub fn pop_dispatch(&mut self) -> Option<QueueItem> {
        self.pop_dispatch_where(|_| true)
    }

    /// 取出满足 `eligible` 的最高 Pending（优先级序不变，跳过不可派项取次高）并标 InFlight——在线额度下
    /// 非必挖项可能暂不可派（C2 Q1〔D15〕）。
    pub fn pop_dispatch_where(&mut self, eligible: impl Fn(&QueueItem) -> bool) -> Option<QueueItem> {
        let i = self.pending_order().into_iter().find(|&i| eligible(&self.items[i]))?;
        self.items[i].state = ItemState::InFlight;
        self.items[i].dispatch_agreement = self.items[i].agreement;
        Some(self.items[i].clone())
    }

    /// 有无满足 `eligible` 的 Pending 项（填门前先问——无可派项不取门位，免持门空转）。
    pub fn has_eligible(&self, eligible: impl Fn(&QueueItem) -> bool) -> bool {
        self.items.iter().any(|it| it.state == ItemState::Pending && eligible(it))
    }

    pub fn mark_settled(&mut self, id: &str, landed_agreement: u32) {
        if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
            it.state = ItemState::Settled;
            it.landed_agreement = Some(landed_agreement);
        }
    }

    pub fn pending_count(&self) -> usize {
        self.items.iter().filter(|i| i.state == ItemState::Pending).count()
    }

    pub fn inflight_count(&self) -> usize {
        self.items.iter().filter(|i| i.state == ItemState::InFlight).count()
    }

    pub fn empty_for_barrier(&self) -> bool {
        self.pending_count() == 0 && self.inflight_count() == 0
    }

    #[cfg(test)]
    fn set_state(&mut self, id: &str, state: ItemState) {
        if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
            it.state = state;
        }
    }
}

/// 必挖集判定（design §6 规则 3）：不依赖队列其余项，合并路径拿着 `&mut` 项也能调；深挖在线额度的可派条件也用它。
///
/// C3〔D16〕起共识分两轴，必挖只认**项内**最大簇的家族数：结构定项后一块里可能落着好几条互不相关的疑点，
/// 按区域级家族并集判定会让必挖集从每轮 4 条涨到 7–9 个块（PR181 规模 18/23）——必挖不受预算截断，`maxCells` 就形同虚设。
pub(crate) fn must_dig(it: &QueueItem) -> bool {
    it.kind == ItemKind::Primary || consensus_axis(it) >= 2
}

/// 必挖轴取值：有 `peak_agreement` 用它；0 = 未算（旧快照 / H0 无疑点）→ 退回区域级 `agreement`（C1/C2 行为）。
fn consensus_axis(it: &QueueItem) -> u32 {
    if it.peak_agreement > 0 {
        it.peak_agreement
    } else {
        it.agreement
    }
}

/// 项的自评置信：疑点取成员里的最高，H0 / needs 用自身分。
/// **只做末位裂解**——跨轮不可比（两轮分布整体平移，中位 0.35 vs 0.45），E3 也实证 flash 级自评与命中无关。
fn confidence_of(it: &QueueItem) -> f32 {
    match it.kind {
        ItemKind::Primary | ItemKind::Ripple => it.score,
        ItemKind::Suspicion => it.suspicions().iter().map(|n| n.confidence).fold(0.0_f32, f32::max),
        ItemKind::Needs => NEEDS_MID_SCORE,
    }
}

/// 文档判定复用阶段 0 的单一口径（评审）：`risk::signals::is_doc_path` 正是 R 分母里把文档行剔出 `code_lines`
/// 的那把尺（`.md/.json/.ya?ml/.toml/.txt/.lock/.svg/.csv`）。自己再写一把只会让「代码格占比」这类读数两处对不上。
/// 不再按 `.next/` / `.doc/` 前缀判：那是本仓私有目录名，别的仓里 `.next/` 是 Next.js 的构建产物。
fn is_doc(file: &str) -> bool {
    crate::risk::signals::is_doc_path(file)
}

/// 优先级全序（C3〔D16〕分层键，design C3 §4）：种类 → 必挖轴 → 风险档 → 烟量 → 代码优先 → 自评置信 → id。
///
/// 取代 C1 的乘积式 `conf × agreement × 类别权重`：类别权重恒 1.0（L0 占比两轮 59% vs 21%，标注自身在漂、不启用）、
/// agreement 只取 1 / 2，乘积实际退化成「按 confidence 排」，而经预算截断后 confidence 就成了门。
/// 分层后每一维含义独立可查，同分才落到下一层。
///
/// 「每文件保底一格」不在这里——它依赖队列全局状态（该文件是否已有格），由 [`DeepQueue::pending_order`] 在本序之上分层。
/// 收场组装报告（C2 Q8）用的也是本序：那时所有项都已派发，保底层对谁都不成立。
pub(crate) fn cmp_priority(a: &QueueItem, b: &QueueItem) -> std::cmp::Ordering {
    let ord = |x: bool| if x { 0 } else { 1 };
    a.kind
        .cmp(&b.kind)
        .then_with(|| consensus_axis(b).cmp(&consensus_axis(a)))
        .then_with(|| b.risk_band.cmp(&a.risk_band))
        .then_with(|| b.suspicions().len().cmp(&a.suspicions().len()))
        .then_with(|| ord(!is_doc(&a.spec.file)).cmp(&ord(!is_doc(&b.spec.file))))
        .then_with(|| confidence_of(b).partial_cmp(&confidence_of(a)).unwrap_or(std::cmp::Ordering::Equal))
        .then_with(|| a.id.cmp(&b.id))
}

fn item_id(spec: &QueueItemSpec) -> String {
    let origin = match spec.origin {
        QueueOrigin::Rule => "rule",
        QueueOrigin::Scan => "scan",
        QueueOrigin::Needs => "needs",
    };
    let ranges = spec
        .ranges
        .iter()
        .map(|&(a, b)| format!("{a}-{b}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("{}:{}:{}", spec.file, origin, ranges)
}

/// 到达请求会成为的 id：有块时区间取块（与 [`QueueItem::new`] 一致），否则按请求区间。
/// 两处算法必须同源——否则 id 捷径拿报告行去比队列里的块 id，永远比不中，唯一性就没人守了。
fn id_for(spec: &QueueItemSpec, block: &Block) -> String {
    if block.is_empty() {
        return item_id(spec);
    }
    let mut probe = spec.clone();
    probe.ranges = vec![(block.start, block.end)];
    item_id(&probe)
}

/// 同一项谓词：主区按文件 + 区间相交；疑点 / needs 按**文件 + 稳定块键**（C3〔D16〕结构定项）。
///
/// 任一侧无块信息（旧快照 / 内容不可读）→ 退回 C1/C2 的 quote 谓词：文件相同 + 「到达过本项的任一疑点」
/// 与请求的任一疑点相似（成员而非渲染笔记，C2 评审 #3：桥接疑点并入后匹配面要跟着扩大）。
fn same_item(a: &QueueItem, b: &QueueItemSpec, b_block: &Block) -> bool {
    let rule = |o| o == QueueOrigin::Rule;
    if rule(a.spec.origin) || rule(b.origin) {
        // 主区身份不变：文件 + 区间相交，与块无关。
        return rule(a.spec.origin) && rule(b.origin) && a.spec.file == b.file && ranges_overlap(&a.spec.ranges, &b.ranges);
    }
    // scan 与 needs 同一身份空间（评审 #6）：needs 只接变更集内的路径，恰是疑点所在的那些文件——
    // 分开记会把同一块的同一段证据装两次包、挖两遍，还把 `max_cells_per_file` 这个本批自己的门槛顶高。
    if a.spec.file != b.file {
        return false;
    }
    if a.block.is_structural() && b_block.is_structural() {
        return a.block.key == b_block.key;
    }
    // 行带 / 无块不是结构身份（评审 #7）：先按 C2 的 quote 谓词判——否则 py / yaml / json 这些
    // `lang_for` 不覆盖的文件里，差一行跨带的两条同 quote 疑点会从「一项共识 2」裂成「两项各 1」，
    // 双双掉出必挖集，成了净回退。带号相同仍然算同项（合并收益保留）。
    a.suspicions().iter().any(|na| b.notes.iter().any(|nb| same_suspicion(na, nb)))
        || (!a.block.is_empty() && !b_block.is_empty() && a.block.key == b_block.key)
}

fn ranges_overlap(a: &[(u32, u32)], b: &[(u32, u32)]) -> bool {
    a.iter().any(|&(as_, ae)| b.iter().any(|&(bs, be)| as_ <= be && bs <= ae))
}

fn union_ranges(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut v: Vec<(u32, u32)> = a.iter().chain(b.iter()).copied().collect();
    if v.is_empty() {
        return v;
    }
    v.sort_unstable();
    let mut out = Vec::new();
    let (mut s, mut e) = v[0];
    for &(ns, ne) in &v[1..] {
        if ns <= e + 1 {
            e = e.max(ne);
        } else {
            out.push((s, e));
            s = ns;
            e = ne;
        }
    }
    out.push((s, e));
    out
}

/// 渲染笔记合并：不相似的追加；相似的只留置信更高的那条（C2 评审 #3：代表与优先级不该钉在最先到达的变体上，
/// reviewer 读到的 hint / quote 同理）。匹配面另由 `QueueItem::members` 保全。
fn merge_notes(a: &[Suspicion], b: &[Suspicion]) -> Vec<Suspicion> {
    let mut out = a.to_vec();
    for n in b {
        match out.iter().position(|x| same_suspicion(x, n)) {
            Some(i) => {
                if n.confidence > out[i].confidence {
                    out[i] = n.clone();
                }
            }
            None => out.push(n.clone()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Category;

    fn sus(file: &str, start: u32, end: u32, quote: &str, hint: &str) -> Suspicion {
        Suspicion {
            file: file.into(),
            line_start: start,
            line_end: end,
            hint: hint.into(),
            category: Some(Category::L2),
            confidence: 0.5,
            quote: quote.into(),
        }
    }

    fn spec_scan(file: &str, start: u32, end: u32, quote: &str, hint: &str) -> QueueItemSpec {
        QueueItemSpec::new(
            file.into(),
            vec![(start, end)],
            QueueOrigin::Scan,
            vec![sus(file, start, end, quote, hint)],
        )
        .unwrap()
    }

    fn spec_rule(file: &str, start: u32, end: u32) -> QueueItemSpec {
        QueueItemSpec::new(file.into(), vec![(start, end)], QueueOrigin::Rule, vec![]).unwrap()
    }

    fn req(spec: QueueItemSpec, family: Option<&str>) -> EnqueueReq {
        EnqueueReq {
            spec,
            score: 1.0,
            kind: ItemKind::Suspicion,
            families: family.map(|f| vec![f.to_string()]).unwrap_or_default(),
            arrival: None,
        }
    }

    #[test]
    fn enqueue_new_is_pending_agreement_one() {
        let mut q = DeepQueue::new();
        let o = q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        assert_eq!(o, EnqueueOutcome::New);
        let it = &q.items()[0];
        assert_eq!(it.state, ItemState::Pending);
        assert_eq!(it.agreement, 1);
        assert_eq!(it.families, vec!["alpha".to_string()]);
    }

    #[test]
    fn merge_pending_other_family_bumps_agreement() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let o = q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("beta")));
        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].agreement, 2);
        assert_eq!(q.items()[0].families, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn same_family_merges_without_agreement() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let o = q.enqueue(req(spec_scan("a.rs", 4, 6, "v[i]", "idx2"), Some("alpha")));
        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].agreement, 1);
        assert_eq!(q.items()[0].spec.ranges, vec![(3, 6)]);
        assert_eq!(q.items()[0].spec.notes.len(), 1, "same_suspicion 合并笔记不重复");
    }

    #[test]
    fn merge_inflight_does_not_reopen() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let id = q.items()[0].id.clone();
        q.set_state(&id, ItemState::InFlight);
        let o = q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("beta")));
        assert_eq!(o, EnqueueOutcome::MergedInFlight);
        assert_eq!(q.items()[0].state, ItemState::InFlight);
        assert_eq!(q.items()[0].agreement, 2);
    }

    #[test]
    fn merge_settled_keeps_settled() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let id = q.items()[0].id.clone();
        q.set_state(&id, ItemState::Settled);
        let o = q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("beta")));
        assert_eq!(o, EnqueueOutcome::MergedSettled);
        assert_eq!(q.items()[0].state, ItemState::Settled);
        assert_eq!(q.items()[0].agreement, 2);
    }

    #[test]
    fn cross_file_does_not_merge() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let o = q.enqueue(req(spec_scan("b.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        assert_eq!(o, EnqueueOutcome::New);
        assert_eq!(q.items().len(), 2);
    }

    #[test]
    fn rule_overlap_merges_without_notes() {
        let mut q = DeepQueue::new();
        let mut r1 = req(spec_rule("hot.rs", 10, 20), None);
        r1.kind = ItemKind::Primary;
        q.enqueue(r1);
        let mut r2 = req(spec_rule("hot.rs", 18, 30), None);
        r2.kind = ItemKind::Primary;
        let o = q.enqueue(r2);
        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].spec.ranges, vec![(10, 30)]);
        assert!(q.items()[0].spec.notes.is_empty());
        assert_eq!(q.items()[0].agreement, 1, "H0 无家族不加");
    }

    /// 同文件同 quote、不同行的两簇疑点：`same_suspicion` 不认（行不重叠）→ 两个独立项，
    /// 各自带自己的家族集。旧实现在 enqueue 之后按 (file, quote) 反查改写，会把后一簇的
    /// 共识写到前一项上、真共识项掉出必挖集（agreement ≥ 2）。
    #[test]
    fn same_quote_different_lines_keep_own_agreement() {
        let mut q = DeepQueue::new();
        q.enqueue(EnqueueReq {
            spec: spec_scan("a.rs", 10, 10, "db.write(t);", "h1"),
            score: 0.5,
            kind: ItemKind::Suspicion,
            families: vec!["alpha".into()],
            arrival: None,
        });
        q.enqueue(EnqueueReq {
            spec: spec_scan("a.rs", 80, 80, "db.write(t);", "h2"),
            score: 0.5,
            kind: ItemKind::Suspicion,
            families: vec!["alpha".into(), "beta".into(), "gamma".into()],
            arrival: None,
        });
        assert_eq!(q.items().len(), 2, "行不重叠不合并");
        let first = q.items().iter().find(|i| i.spec.ranges == vec![(10, 10)]).unwrap();
        let second = q.items().iter().find(|i| i.spec.ranges == vec![(80, 80)]).unwrap();
        assert_eq!(first.agreement, 1, "前一项不该被后一簇改写");
        assert_eq!(second.agreement, 3, "三家族共识留在自己项上");
        assert!(q.is_must(second), "agreement ≥ 2 → 必挖集，不被预算截");
        assert!(!q.is_must(first));
    }

    /// agreement ≡ 家族数：重复家族不计、空串不计（与 merge_suspicions 同口径）。
    #[test]
    fn agreement_equals_distinct_family_count() {
        let mut q = DeepQueue::new();
        q.enqueue(EnqueueReq {
            spec: spec_scan("a.rs", 1, 2, "v[i]", "idx"),
            score: 0.5,
            kind: ItemKind::Suspicion,
            families: vec!["alpha".into(), "alpha".into(), "".into(), "beta".into()],
            arrival: None,
        });
        assert_eq!(q.items()[0].agreement, 2);
        assert_eq!(q.items()[0].families, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn different_quotes_same_file_do_not_merge() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let o = q.enqueue(req(spec_scan("a.rs", 40, 42, "unwrap()", "opt"), Some("alpha")));
        assert_eq!(o, EnqueueOutcome::New);
        assert_eq!(q.items().len(), 2);
    }

    fn req_kind(spec: QueueItemSpec, kind: ItemKind, score: f32, family: Option<&str>) -> EnqueueReq {
        EnqueueReq { spec, score, kind, families: family.map(|f| vec![f.to_string()]).unwrap_or_default(), arrival: None }
    }

    fn spec_needs(file: &str, start: u32, end: u32) -> QueueItemSpec {
        QueueItemSpec::new(
            file.into(),
            vec![(start, end)],
            QueueOrigin::Needs,
            vec![sus(file, start, end, "need", "gap")],
        )
        .unwrap()
    }

    #[test]
    fn pending_order_layers_primary_ripple_suspicion_needs() {
        let mut q = DeepQueue::new();
        q.enqueue(req_kind(spec_needs("n.rs", 1, 2), ItemKind::Needs, 9.0, None));
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 0.1, Some("a")));
        q.enqueue(req_kind(spec_rule("r.rs", 1, 2), ItemKind::Ripple, 0.2, None));
        q.enqueue(req_kind(spec_rule("p.rs", 1, 2), ItemKind::Primary, 0.1, None));
        let kinds: Vec<_> = q.pending_order().into_iter().map(|i| q.items()[i].kind).collect();
        assert_eq!(kinds, vec![ItemKind::Primary, ItemKind::Ripple, ItemKind::Suspicion, ItemKind::Needs]);
    }

    #[test]
    fn drain_zero_budget_keeps_primary() {
        let mut q = DeepQueue::new();
        q.enqueue(req_kind(spec_rule("p.rs", 1, 2), ItemKind::Primary, 1.0, None));
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 1.0, Some("a")));
        q.enqueue(req_kind(spec_needs("n.rs", 1, 2), ItemKind::Needs, 1.0, None));
        let skipped = q.drain(0);
        assert_eq!(skipped.len(), 2);
        assert_eq!(q.items().iter().filter(|i| i.state == ItemState::Pending).count(), 1);
        assert_eq!(q.peek_highest().unwrap().kind, ItemKind::Primary);
    }

    #[test]
    fn drain_zero_keeps_agreement_two_even_if_not_primary() {
        let mut q = DeepQueue::new();
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 1.0, Some("a")));
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 1.0, Some("b")));
        q.enqueue(req_kind(spec_needs("n.rs", 1, 2), ItemKind::Needs, 1.0, None));
        assert_eq!(q.items()[0].agreement, 2);
        let skipped = q.drain(0);
        assert_eq!(skipped.len(), 1);
        assert_eq!(q.items().iter().find(|i| i.state == ItemState::Pending).unwrap().agreement, 2);
    }

    #[test]
    fn drain_floor_reserves_suspicions_when_primary_fills() {
        let mut q = DeepQueue::new();
        for i in 0..4u32 {
            q.enqueue(req_kind(spec_rule(&format!("p{i}.rs"), 1, 2), ItemKind::Primary, 1.0, None));
        }
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 1.0, Some("a")));
        q.enqueue(req_kind(spec_needs("n.rs", 1, 2), ItemKind::Needs, 1.0, None));
        // max_cells=4，必挖 4 主区吃穿；保底 ⌈4/4⌉=1 给疑点 → needs skipped，suspicion pending。
        let skipped = q.drain(4);
        assert_eq!(skipped.len(), 1);
        assert!(q.items().iter().any(|i| i.kind == ItemKind::Suspicion && i.state == ItemState::Pending));
        assert!(q.items().iter().any(|i| i.kind == ItemKind::Needs && i.state == ItemState::Skipped));
        assert_eq!(q.items().iter().filter(|i| i.kind == ItemKind::Primary && i.state == ItemState::Pending).count(), 4);
    }

    /// id 在 `new` 时定死（批 C1-1）：派发后在途并入扩了区间，完成时拿派发时的 id 仍命中该项。
    /// 旧实现在合并里按新 ranges 重算 id → `mark_settled(派发时 id)` 落空，项永远 InFlight、barrier 永不满足。
    #[test]
    fn id_is_stable_across_inflight_merge() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let dispatched = q.pop_dispatch().unwrap();
        assert_eq!(dispatched.id, "a.rs:scan:3-5");
        let o = q.enqueue(req(spec_scan("a.rs", 4, 9, "v[i]", "idx"), Some("beta")));
        assert_eq!(o, EnqueueOutcome::MergedInFlight);
        assert_eq!(q.items()[0].spec.ranges, vec![(3, 9)], "区间照并");
        assert_eq!(q.items()[0].id, dispatched.id, "id 不随 ranges 重算");
        q.mark_settled(&dispatched.id, 2);
        assert_eq!(q.items()[0].state, ItemState::Settled);
        assert_eq!(q.inflight_count(), 0);
    }

    /// 共识只经谓词（design §2.2 / Q2）：同区间、quote 不同的两条疑点（上游已判为不同疑点）经 id 捷径
    /// 合成一次挖掘，但家族不并、agreement 不抬；谓词成立（同 quote、行重叠）的异族才计共识。
    #[test]
    fn id_shortcut_merge_does_not_count_consensus() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let o = q.enqueue(req(spec_scan("a.rs", 3, 5, "unwrap()", "opt"), Some("beta")));
        assert_eq!(o, EnqueueOutcome::MergedPending, "同区间只挖一次");
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].agreement, 1, "id 捷径不计共识");
        assert_eq!(q.items()[0].families, vec!["alpha".to_string()]);
        assert_eq!(q.items()[0].spec.notes.len(), 2, "笔记照并——reviewer 两条都要看");
        let o = q.enqueue(req(spec_scan("a.rs", 4, 5, "v[i]", "idx"), Some("gamma")));
        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items()[0].agreement, 2, "谓词合并照抬");
        assert_eq!(q.items()[0].families, vec!["alpha".to_string(), "gamma".to_string()]);
    }

    /// id 捷径并入不丢**请求自身**的共识（评审修）：上游已合并成多家族共识的疑点（agreement ≥ 2、必挖）撞上同区间的单家族项，
    /// 家族不跨疑点并入，但项的 agreement 取大——否则它掉出必挖集、排序分减半，可能被预算截掉。
    #[test]
    fn id_shortcut_merge_keeps_incoming_consensus() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let o = q.enqueue(EnqueueReq {
            spec: spec_scan("a.rs", 3, 5, "unwrap()", "opt"),
            score: 1.0,
            kind: ItemKind::Suspicion,
            families: vec!["beta".into(), "gamma".into()],
            arrival: None,
        });
        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].families, vec!["alpha".to_string()], "家族不跨疑点并入");
        assert_eq!(q.items()[0].agreement, 2, "请求自身的双家族共识保留");
        assert!(q.is_must(&q.items()[0]));
        assert!(q.drain(0).is_empty(), "必挖项不被预算截");
        // 之后同家族谓词并入重算家族数（仍为 1）也不得把 agreement 拉回去：单调不减（event ≤ landed 的前提）
        q.enqueue(req(spec_scan("a.rs", 4, 5, "v[i]", "idx"), Some("alpha")));
        assert_eq!(q.items()[0].agreement, 2, "agreement 单调不减");
    }

    /// 谓词命中优先于 id 捷径：进来的疑点与 A 区间逐字相同（id 相等）、却与 B 是同一疑点（谓词成立）→
    /// 并进 B、共识记在 B 上，A 不动。按位置取第一个命中会把它并进 A，B 的真共识落空。
    #[test]
    fn predicate_match_wins_over_id_shortcut() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        q.enqueue(req(spec_scan("a.rs", 4, 9, "unwrap()", "opt"), Some("beta")));
        assert_eq!(q.items().len(), 2);
        let o = q.enqueue(req(spec_scan("a.rs", 3, 5, "unwrap()", "opt"), Some("gamma")));
        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 2, "id 相等也不另起一项");
        assert_eq!((q.items()[0].agreement, q.items()[0].spec.notes.len()), (1, 1), "A 不动");
        assert_eq!(q.items()[1].agreement, 2, "共识记在谓词认定的 B 上");
    }

    /// Skipped 并入（2026-09-13 裁定）：并入后成为必挖（异族谓词共识抬到 2）→ 复位 Pending、重置入队时刻；
    /// 仍非必挖（同家族 / id 捷径）→ 保持 Skipped、回 `MergedSkipped`，不突破 drain 定下的预算。
    #[test]
    fn skipped_item_resets_only_when_it_becomes_must() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("s.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        q.enqueue(req(spec_scan("t.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        assert_eq!(q.drain(0).len(), 2, "预算 0 只留必挖 → 两条单家族疑点都截");
        for it in q.items_mut() {
            it.enqueued_ms = Some(0);
        }
        let o = q.enqueue(req(spec_scan("s.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        assert_eq!(o, EnqueueOutcome::MergedSkipped, "同家族并入仍非必挖");
        assert_eq!(q.items()[0].state, ItemState::Skipped);
        let o = q.enqueue(req(spec_scan("t.rs", 3, 5, "unwrap()", "opt"), Some("beta")));
        assert_eq!(o, EnqueueOutcome::MergedSkipped, "id 捷径的异族并入不计共识");
        assert_eq!((q.items()[1].state, q.items()[1].agreement), (ItemState::Skipped, 1));
        let o = q.enqueue(req(spec_scan("s.rs", 4, 5, "v[i]", "idx"), Some("beta")));
        assert_eq!(o, EnqueueOutcome::MergedPending, "谓词的异族并入 → 必挖 → 复位");
        assert_eq!((q.items()[0].state, q.items()[0].agreement), (ItemState::Pending, 2));
        assert!(q.items()[0].enqueued_ms.unwrap() > 0, "入队时刻重置");
    }

    // ── C2 零件〔D15〕 ───────────────────────────────────────────────────────

    fn arrived(spec: QueueItemSpec, family: &str, arrival: &str) -> EnqueueReq {
        EnqueueReq { arrival: Some(arrival.into()), ..req(spec, Some(family)) }
    }

    /// 桥接疑点并入后匹配面要跟着扩大（C2 评审 #3）：链 s3 ~ s1 ~ s2（s3 ≁ s2），到达序 s3、s1、s2 ——
    /// s1 并入 s3 那一项后，s2 与成员 s1 相似即并入同一项；匹配只看渲染笔记时 s1 会被当相似丢掉，s2 另起一项、簇被拆开，
    /// 而 scan 报告里这是一条 agreement 3 的必挖簇。代表笔记与优先级另取最高置信的变体，不钉在最先到达那条上。
    #[test]
    fn predicate_merge_keeps_members_for_matching_and_best_note() {
        let chain = "let t = secret();\nlog(token_value);\nreturn t;";
        let (s3, s1, s2) = (
            spec_scan("a.rs", 10, 10, "let t = secret();", "s3"),
            QueueItemSpec::new(
                "a.rs".into(),
                vec![(10, 12)],
                QueueOrigin::Scan,
                vec![Suspicion { confidence: 0.9, ..sus("a.rs", 10, 12, chain, "s1") }],
            )
            .unwrap(),
            spec_scan("a.rs", 14, 14, "log(token_value);  ", "s2"),
        );
        let mut q = DeepQueue::new();
        assert_eq!(q.enqueue(arrived(s3, "alpha", "c1#0")), EnqueueOutcome::New);
        assert_eq!(q.enqueue(arrived(s1, "beta", "c2#0")), EnqueueOutcome::MergedPending, "s1 ~ s3");
        assert_eq!(q.enqueue(arrived(s2, "gamma", "c3#0")), EnqueueOutcome::MergedPending, "s2 ~ 成员 s1");
        assert_eq!(q.items().len(), 1, "一簇一项: {:?}", q.items().iter().map(|it| &it.id).collect::<Vec<_>>());
        let it = &q.items()[0];
        assert_eq!((it.agreement, it.members.len()), (3, 3), "三家族共识、成员留全");
        assert!(must_dig(it), "共识 ≥ 2 → 必挖");
        let best = it.spec.notes.iter().map(|n| n.confidence).fold(0.0_f32, f32::max);
        assert_eq!((best, it.spec.notes.len()), (0.9, 1), "代表取最高置信：s1 顶掉 s3，与 s1 相似的 s2 不重复列");
        assert!((confidence_of(it) - 0.9).abs() < 1e-6, "末位裂解用成员里的最高置信（C3〔D16〕分层键）");

        // 桥接疑点置信更低时代表笔记留不住它（相似的只留高置信那条），匹配面只能靠成员列表
        let low_bridge = QueueItemSpec::new(
            "a.rs".into(),
            vec![(10, 12)],
            QueueOrigin::Scan,
            vec![Suspicion { confidence: 0.3, ..sus("a.rs", 10, 12, chain, "s1") }],
        )
        .unwrap();
        let mut q2 = DeepQueue::new();
        q2.enqueue(arrived(spec_scan("a.rs", 10, 10, "let t = secret();", "s3"), "alpha", "c1#0"));
        q2.enqueue(arrived(low_bridge, "beta", "c2#0"));
        q2.enqueue(arrived(spec_scan("a.rs", 14, 14, "log(token_value);  ", "s2"), "gamma", "c3#0"));
        assert_eq!(q2.items().len(), 1, "低置信的桥也要扩大匹配面: {:?}", q2.items().iter().map(|it| &it.id).collect::<Vec<_>>());
        assert_eq!(q2.items()[0].spec.notes.len(), 2, "渲染笔记只去相似：s2 与代表 s3 不相似 → 另列");
    }

    /// 到达键幂等：同一条疑点再到达（哪怕家族 / 区间变了）→ `Duplicate`、不回报受影响项、队列零改动。
    #[test]
    fn arrival_key_makes_enqueue_idempotent() {
        let mut q = DeepQueue::new();
        let (o, idx) = q.enqueue_affected(arrived(spec_scan("a.rs", 3, 5, "v[i]", "idx"), "alpha", "scan[g1]@p/a#1#0"));
        assert_eq!((o, idx), (EnqueueOutcome::New, Some(0)));
        let before = q.items().to_vec();
        let (o, idx) = q.enqueue_affected(arrived(spec_scan("a.rs", 3, 9, "v[i]", "idx"), "beta", "scan[g1]@p/a#1#0"));
        assert_eq!((o, idx), (EnqueueOutcome::Duplicate, None));
        assert_eq!(q.items(), before.as_slice(), "重复到达零改动");
    }

    /// 回报受影响项：新建 → 新项下标；谓词并入 / id 捷径并入 → 目标项下标；并入项追加到达键。
    #[test]
    fn enqueue_reports_affected_item_and_records_arrivals() {
        let mut q = DeepQueue::new();
        q.enqueue_affected(arrived(spec_scan("a.rs", 3, 5, "v[i]", "idx"), "alpha", "c1#0"));
        assert_eq!(q.enqueue_affected(arrived(spec_scan("b.rs", 1, 1, "x", "h"), "alpha", "c1#1")), (EnqueueOutcome::New, Some(1)));
        assert_eq!(q.enqueue_affected(arrived(spec_scan("a.rs", 4, 5, "v[i]", "idx"), "beta", "c2#0")), (EnqueueOutcome::MergedPending, Some(0)));
        assert_eq!(q.enqueue_affected(arrived(spec_scan("a.rs", 3, 5, "unwrap()", "opt"), "gamma", "c3#0")), (EnqueueOutcome::MergedPending, Some(0)), "id 捷径");
        assert_eq!(q.items()[0].arrivals, vec!["c1#0".to_string(), "c2#0".to_string(), "c3#0".to_string()]);
        assert_eq!(q.items()[1].arrivals, vec!["c1#1".to_string()]);
        let json = serde_json::to_value(&q.items()[0]).unwrap();
        assert!(json.get("arrivals").is_some());
        let h0 = QueueItem { arrivals: vec![], ..q.items()[1].clone() };
        assert!(serde_json::to_value(&h0).unwrap().get("arrivals").is_none(), "空到达键不序列化（旧快照形状）");
    }

    /// 按可派条件弹出：跳过不可派的更高优先项、取次高；无可派项 → None 且不改状态。
    #[test]
    fn pop_dispatch_where_skips_ineligible_items() {
        let mut q = DeepQueue::new();
        q.enqueue(req_kind(spec_rule("p.rs", 1, 2), ItemKind::Primary, 1.0, None));
        q.enqueue(req_kind(spec_rule("r.rs", 1, 2), ItemKind::Ripple, 1.0, None));
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 1.0, Some("a")));
        let not_primary = |it: &QueueItem| it.kind != ItemKind::Primary;
        assert!(q.has_eligible(not_primary));
        assert_eq!(q.pop_dispatch_where(not_primary).map(|it| it.kind), Some(ItemKind::Ripple), "跳过主区取次高");
        assert_eq!(q.pop_dispatch_where(not_primary).map(|it| it.kind), Some(ItemKind::Suspicion));
        assert!(!q.has_eligible(not_primary));
        assert!(q.pop_dispatch_where(not_primary).is_none());
        assert_eq!(q.pending_count(), 1, "主区仍 Pending");
        assert_eq!(q.pop_dispatch().map(|it| it.kind), Some(ItemKind::Primary));
    }

    fn drain_scenarios() -> Vec<(DeepQueue, u32)> {
        let mut out = Vec::new();
        let mut q = DeepQueue::new();
        q.enqueue(req_kind(spec_rule("p.rs", 1, 2), ItemKind::Primary, 1.0, None));
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 1.0, Some("a")));
        q.enqueue(req_kind(spec_needs("n.rs", 1, 2), ItemKind::Needs, 1.0, None));
        out.push((q.clone(), 0));
        out.push((q, 2));
        let mut q = DeepQueue::new();
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 1.0, Some("a")));
        q.enqueue(req_kind(spec_scan("s.rs", 1, 2, "a", "h"), ItemKind::Suspicion, 1.0, Some("b")));
        q.enqueue(req_kind(spec_needs("n.rs", 1, 2), ItemKind::Needs, 1.0, None));
        out.push((q, 0));
        let mut q = DeepQueue::new();
        for i in 0..4u32 {
            q.enqueue(req_kind(spec_rule(&format!("p{i}.rs"), 1, 2), ItemKind::Primary, 1.0, None));
        }
        for i in 0..3u32 {
            q.enqueue(req_kind(spec_scan(&format!("s{i}.rs"), 1, 2, "a", "h"), ItemKind::Suspicion, 0.5 + i as f32 / 10.0, Some("a")));
        }
        q.enqueue(req_kind(spec_needs("n.rs", 1, 2), ItemKind::Needs, 1.0, None));
        out.push((q.clone(), 4));
        out.push((q.clone(), 8));
        out.push((q, 12));
        out
    }

    /// `drain(K)` = `drain_with(K, ⌈K/4⌉)`：C1 各场景逐项一致（开跑即关闭、未派发时的收场 drain 就是旧 drain）。
    /// 期望值写死（评审 #15：两边同一公式互比是恒真的，实现改错会一起变）——被截项按 (种类 → 分数 → id) 序，必挖与保底疑点不动。
    #[test]
    fn drain_with_matches_drain_on_c1_scenarios() {
        let want: [Vec<&str>; 6] = [
            vec!["s.rs:scan:1-2", "n.rs:needs:1-2"], // K=0：只留必挖主区
            vec!["n.rs:needs:1-2"],                  // K=2：保底留下疑点
            vec!["n.rs:needs:1-2"],                  // K=0、无主区：两族共识的疑点是必挖
            vec!["s1.rs:scan:1-2", "s2.rs:scan:1-2", "n.rs:needs:1-2"], // K=4：4 条必挖主区 + 保底 1 条疑点
            vec![],                                  // K=8 / K=12：装得下
            vec![],
        ];
        for ((q, k), want) in drain_scenarios().into_iter().zip(want) {
            let (mut a, mut b) = (q.clone(), q);
            let floor = if k == 0 { 0 } else { k.div_ceil(4) as usize };
            assert_eq!(a.drain(k), want, "K = {k}");
            assert_eq!(b.drain_with(k as usize, floor), want, "K = {k}（drain_with 同值）");
            assert_eq!(a.items(), b.items(), "K = {k}");
        }
    }

    /// 收场 drain 的两个参数各自生效：主区吃穿剩余预算（budget 0）时帽 = 必挖 + floor 条疑点；floor 取调用方给的剩余保底，
    /// 不按预算重算（已派发的非必挖疑点要从保底里扣掉）。
    #[test]
    fn drain_with_honors_remaining_budget_and_floor() {
        let mut q = DeepQueue::new();
        for i in 0..3u32 {
            q.enqueue(req_kind(spec_rule(&format!("p{i}.rs"), 1, 2), ItemKind::Primary, 1.0, None));
            q.enqueue(req_kind(spec_scan(&format!("s{i}.rs"), 1, 2, "a", "h"), ItemKind::Suspicion, 0.5, Some("a")));
        }
        let kept = |q: &DeepQueue| q.items().iter().filter(|it| it.state == ItemState::Pending).count();
        let mut two = q.clone();
        assert_eq!(two.drain_with(0, 2).len(), 1);
        assert_eq!(kept(&two), 5, "3 主区 + 2 保底疑点");
        let mut none = q.clone();
        assert_eq!(none.drain_with(0, 0).len(), 3, "保底已被已派发疑点用完 → 只留必挖");
        let mut fill = q;
        assert_eq!(fill.drain_with(4, 0).len(), 2, "按优先级补到 4");
    }

    // ---- C3〔D16〕结构定项、谓词定共识 ----

    fn blk(key: &str, start: u32, end: u32) -> Block {
        Block { start, end, key: key.into() }
    }

    fn arr(spec: QueueItemSpec, family: Option<&str>, block: Block) -> Arrival {
        Arrival { req: req(spec, family), block, risk_band: 0 }
    }

    /// 结构定项：同一 def 块里两条**互不相似**的疑点并成一项——现行 quote 谓词会留两项，正是跨轮对不上的来源。
    /// 共识两轴分离：区域级 `agreement` = 2（两个家族碰过这块），必挖轴 `peak_agreement` = 1（最大簇只有一个家族）→ **非必挖**。
    #[test]
    fn same_block_merges_unrelated_suspicions_but_does_not_make_them_must_dig() {
        let mut q = DeepQueue::new();
        let b = blk("def:f1", 10, 40);
        q.enqueue(arr(spec_scan("a.rs", 12, 12, "let x = take(v);", "空指针"), Some("alpha"), b.clone()));
        let o = q.enqueue(arr(spec_scan("a.rs", 33, 33, "drop(guard); // 完全无关", "锁顺序"), Some("beta"), b));

        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 1, "同块 = 同项");
        let it = &q.items()[0];
        assert_eq!((it.agreement, it.peak_agreement), (2, 1), "区域级 2 / 必挖轴 1");
        assert!(!must_dig(it), "两条不相干的疑点凑不出共识");
        assert_eq!(it.spec.ranges, vec![(10, 40)], "项的区间 = 块");
        assert_eq!(it.id, "a.rs:scan:10-40", "id 也锚在块上");
    }

    /// 必挖轴：同一条疑点被两家族提出（quote 相似）→ 最大簇家族数 2 → 必挖。
    #[test]
    fn same_suspicion_from_two_families_lifts_peak_agreement() {
        let mut q = DeepQueue::new();
        let b = blk("def:f1", 10, 40);
        q.enqueue(arr(spec_scan("a.rs", 12, 12, "let x = take(v);", "空指针"), Some("alpha"), b.clone()));
        q.enqueue(arr(spec_scan("a.rs", 13, 13, "let x = take(v);", "同一处"), Some("beta"), b));

        let it = &q.items()[0];
        assert_eq!((it.agreement, it.peak_agreement), (2, 2));
        assert!(must_dig(it));
    }

    /// 同家族多 samples 不抬必挖轴（D7 口径）：两条相似疑点都来自 alpha → 簇内家族数仍 1。
    #[test]
    fn same_family_twice_does_not_lift_peak_agreement() {
        let mut q = DeepQueue::new();
        let b = blk("def:f1", 10, 40);
        q.enqueue(arr(spec_scan("a.rs", 12, 12, "let x = take(v);", "空指针"), Some("alpha"), b.clone()));
        q.enqueue(arr(spec_scan("a.rs", 13, 13, "let x = take(v);", "还是它"), Some("alpha"), b));

        let it = &q.items()[0];
        assert_eq!((it.agreement, it.peak_agreement), (1, 1));
        assert!(!must_dig(it));
    }

    /// 不同块不并——哪怕 quote 完全相同（现行谓词会并成一项）。
    #[test]
    fn different_blocks_stay_separate_even_with_identical_quotes() {
        let mut q = DeepQueue::new();
        q.enqueue(arr(spec_scan("a.rs", 12, 12, "tick();", "h"), Some("alpha"), blk("def:f1", 10, 20)));
        let o = q.enqueue(arr(spec_scan("a.rs", 22, 22, "tick();", "h"), Some("beta"), blk("def:f2", 21, 30)));

        assert_eq!(o, EnqueueOutcome::New);
        assert_eq!(q.items().len(), 2);
    }

    /// id 唯一优先于结构分开（评审 #10）：两条疑点算出同一 id（区间取各自的块、恰好一样）但块键不同 →
    /// 仍并成一项。两项同 id 会在快照 upsert（`(job, id)` 主键）、`mark_settled` 的线性查找与 cell 命名三处静默互相覆盖，
    /// 那比「两个结构被并成一项」严重得多；同轮内同文件同区间必然同键，这条兜底只在 resume 后内容变了时走到。
    #[test]
    fn identical_id_merges_even_when_block_keys_differ() {
        let mut q = DeepQueue::new();
        q.enqueue(arr(spec_scan("a.rs", 1, 1, "alpha_line();", "h"), Some("alpha"), blk("def:f1", 5, 9)));
        let o = q.enqueue(arr(spec_scan("a.rs", 1, 1, "beta_line();", "h2"), Some("beta"), blk("def:f2", 5, 9)));

        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 1, "id 唯一");
        assert_eq!(q.items()[0].id, "a.rs:scan:5-9");
    }

    /// 结构块优先于旧项的 quote 谓词（评审 #9）：线性取首个命中会让一个无块的旧项把本该归给块主人的到达截走，
    /// 同一个块键裂成两项——resume 场景里正是本批要消灭的那种分裂。
    #[test]
    fn structural_match_wins_over_a_blockless_items_quote_match() {
        let mut q = DeepQueue::new();
        // 旧项（无块）：行与 quote 都够得着后面那条桥接到达——不这样构造就测不到「被截走」
        q.enqueue(req(spec_scan("a.rs", 11, 11, "let x = take(v);", "旧"), Some("alpha")));
        // 块主人
        q.enqueue(arr(spec_scan("a.rs", 12, 12, "完全不同的一行();", "新"), Some("beta"), blk("def:f1", 10, 40)));
        let o = q.enqueue(arr(spec_scan("a.rs", 13, 13, "let x = take(v);", "桥"), Some("gamma"), blk("def:f1", 10, 40)));

        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 2, "并进块主人，不是并进旧项: {:?}", q.items().iter().map(|it| &it.id).collect::<Vec<_>>());
        assert_eq!(q.items()[1].members.len(), 2, "块主人收了桥接那条");
        assert_eq!(q.items()[0].members.len(), 1, "旧项没被截走");
    }

    /// 行带不是结构身份（评审 #7）：`lang_for` 不覆盖的语言（py / yaml / json…）里，差一行跨带的两条同 quote 疑点
    /// 在 C2 是一项共识 2（必挖），按块键硬判会裂成两项各 1、双双掉出必挖集——净回退。行带上仍走 quote 谓词。
    #[test]
    fn band_blocks_still_merge_by_quote_across_a_band_boundary() {
        let mut q = DeepQueue::new();
        q.enqueue(arr(spec_scan("app.py", 21, 21, "cursor.execute(sql % arg)", "注入"), Some("alpha"), blk("pad:0", 1, 21)));
        let o = q.enqueue(arr(spec_scan("app.py", 23, 23, "cursor.execute(sql % arg)", "同一处"), Some("beta"), blk("pad:1", 22, 42)));

        assert_eq!(o, EnqueueOutcome::MergedPending, "跨带但同一条疑点");
        assert_eq!(q.items().len(), 1);
        assert!(must_dig(&q.items()[0]), "共识 2 仍是必挖");
    }

    /// H0 恒在最前（评审 #8）：保底是多样性启发式，不得把 §9 确定性地板的主区 / 涟漪挤到后面，
    /// 也不该让 needs 靠保底插到别的文件的疑点之前。
    #[test]
    fn h0_outranks_the_per_file_first_slot() {
        let mut q = DeepQueue::new();
        // a.rs 已有一格在途 → a 的其余项不再有保底位
        let mut dug = item_at("a.rs", 10, 1, 0, 1, 0.9);
        dug.state = ItemState::InFlight;
        q.items.push(dug);
        q.items.push(item_at("a.rs", 20, 1, 0, 1, 0.9));
        // b.rs 从没挖过：一条 H0 涟漪 + 一条 needs
        let ripple_spec = spec_rule("b.rs", 1, 9);
        q.enqueue(req_kind(ripple_spec, ItemKind::Ripple, 0.1, None));
        let needs_spec = QueueItemSpec::new("c.rs".into(), vec![(1, 9)], QueueOrigin::Needs, vec![]).unwrap();
        q.enqueue(EnqueueReq { spec: needs_spec, score: 0.5, kind: ItemKind::Needs, families: vec![], arrival: None });

        let order: Vec<&str> = q.pending_order().into_iter().map(|i| q.items()[i].spec.file.as_str()).collect();

        assert_eq!(order[0], "b.rs", "H0 涟漪在最前，哪怕它所在文件也只是「没挖过」: {order:?}");
        assert_eq!(order[order.len() - 1], "c.rs", "needs 垫底，保底不把它提前: {order:?}");
    }

    /// 无块信息（内容读不到 / 旧快照回放）→ 合并退回 C1/C2 的 quote 谓词；家族仍按条记，必挖轴照算。
    #[test]
    fn missing_block_falls_back_to_quote_predicate() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "let x = take(v);", "h"), Some("alpha")));
        let o = q.enqueue(req(spec_scan("a.rs", 4, 6, "let x = take(v);", "h2"), Some("beta")));

        assert_eq!(o, EnqueueOutcome::MergedPending);
        let it = &q.items()[0];
        assert!(it.block.is_empty());
        assert_eq!((it.agreement, it.peak_agreement), (2, 2), "同一条疑点两家族 → 必挖轴照样成立");
        assert_eq!(it.spec.ranges, vec![(3, 6)], "无块 → 区间按 C2 的并集");
    }

    /// 旧快照项（C2 之前落盘，没有 `member_families` / `peak_agreement` 两个字段）→ 必挖判定退回区域级 `agreement`，
    /// 行为与 C2 逐字一致；不会因为新轴算不出来就把往轮认定的必挖项降级。
    #[test]
    fn legacy_snapshot_item_falls_back_to_region_agreement() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "let x = take(v);", "h"), Some("alpha")));
        q.enqueue(req(spec_scan("a.rs", 4, 6, "let x = take(v);", "h2"), Some("beta")));
        let mut snap = q.items().to_vec();
        snap[0].member_families.clear(); // serde default：旧快照没有这一列
        snap[0].peak_agreement = 0;

        let mut restored = DeepQueue::restore(snap);

        let it = &restored.items()[0];
        assert_eq!(it.peak_agreement, 2, "restore 把往轮的区域级 agreement 种成必挖轴下限");
        assert!(must_dig(it), "行为与 C2 一致");

        // 更多佐证不得把它降回非必挖（评审 #5）：并进来的这条算出的 peak = 1（旧成员没有家族记录）
        // 不能覆盖种下的 2——否则「更多佐证反而失去必挖保护」。
        assert_eq!(restored.enqueue(req(spec_scan("a.rs", 4, 6, "let x = take(v);", "新"), Some("gamma"))), EnqueueOutcome::MergedPending);
        assert_eq!(restored.items().len(), 1);
        assert!(must_dig(&restored.items()[0]), "peak 只增不减: {:?}", restored.items()[0]);
    }

    /// 旧快照项的家族列要先补齐再写（评审 #14）：`members` 有、`member_families` 空（serde default）时，
    /// 按下标写会越界；不补齐则家族被静默丢掉、两列长度永久错开，该项的必挖轴永远算不出来。
    #[test]
    fn merging_into_a_legacy_item_aligns_member_families() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "let x = take(v);", "h"), Some("alpha")));
        q.items[0].member_families.clear(); // 旧快照形态
        q.items[0].peak_agreement = 0;

        let same = sus("a.rs", 3, 5, "let x = take(v);", "h");
        let spec = QueueItemSpec::new("a.rs".into(), vec![(3, 5)], QueueOrigin::Scan, vec![same]).unwrap();
        assert_eq!(q.enqueue(req(spec, Some("beta"))), EnqueueOutcome::MergedPending);

        let it = &q.items()[0];
        assert_eq!(it.members.len(), 1, "同一条疑点不重复记");
        assert_eq!(it.member_families, vec![vec!["beta".to_string()]], "补齐后新家族落进同一槽位");
    }

    /// 旧快照项**不就地升级**（评审 #3）：`id` 只在 `new` 时定死，升级会让 id 与 ranges 各说各话——
    /// cell 名、快照主键、coverage 都报着一个 reviewer 没看过的区间。旧项按旧语义走到底。
    #[test]
    fn legacy_item_keeps_its_own_identity_when_a_blocked_arrival_merges_in() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "let x = take(v);", "h"), Some("alpha")));
        q.enqueue(arr(spec_scan("a.rs", 4, 6, "let x = take(v);", "h2"), Some("beta"), blk("def:f1", 1, 30)));

        assert_eq!(q.items().len(), 1);
        let it = &q.items()[0];
        assert!(it.block.is_empty(), "不升级");
        assert_eq!(it.spec.ranges, vec![(3, 6)], "区间仍按 C2 的并集");
        assert_eq!(it.id, "a.rs:scan:3-5", "id 与区间口径一致（都还是旧的）");
    }

    /// 有块的项，区间恒等于块（评审 #2）：quote 跨到块尾外的疑点不得把区间撑大，
    /// 撑大与否还取决于到达序——同样的输入会出两种 pack。
    #[test]
    fn blocked_item_range_stays_the_block_regardless_of_arrival_order() {
        let b = blk("def:f1", 10, 40);
        let inside = || spec_scan("a.rs", 12, 12, "let x = take(v);", "里");
        let spills = || spec_scan("a.rs", 35, 55, "let x = take(v); // 引到块尾外", "跨");
        let mut fwd = DeepQueue::new();
        fwd.enqueue(arr(inside(), Some("alpha"), b.clone()));
        fwd.enqueue(arr(spills(), Some("beta"), b.clone()));
        let mut bwd = DeepQueue::new();
        bwd.enqueue(arr(spills(), Some("beta"), b.clone()));
        bwd.enqueue(arr(inside(), Some("alpha"), b));

        assert_eq!(fwd.items()[0].spec.ranges, vec![(10, 40)]);
        assert_eq!(bwd.items()[0].spec.ranges, vec![(10, 40)], "与到达序无关");
    }

    /// scan 与 needs 落在同一块 → 一项（评审 #6）：分开记等于同一段证据装两次包、挖两遍，
    /// 还把 `max_cells_per_file` 这个本批自己的门槛顶高。层级取高的那个。
    #[test]
    fn needs_and_scan_in_the_same_block_are_one_item() {
        let b = blk("def:f1", 10, 40);
        let needs_spec = QueueItemSpec::new("a.rs".into(), vec![(10, 40)], QueueOrigin::Needs, vec![]).unwrap();
        let needs = EnqueueReq { spec: needs_spec, score: 0.5, kind: ItemKind::Needs, families: vec![], arrival: Some("needs:a.rs:10-40:f".into()) };
        let mut q = DeepQueue::new();
        q.enqueue(Arrival { req: needs, block: b.clone(), risk_band: 0 });
        let o = q.enqueue(arr(spec_scan("a.rs", 12, 12, "q", "h"), Some("alpha"), b));

        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].kind, ItemKind::Suspicion, "层级取高的");
        assert_eq!(q.items()[0].arrivals, ["needs:a.rs:10-40:f"], "needs 的到达键记在这一项上（疑点侧本用例没带键）");
        assert_eq!(q.items()[0].spec.notes.len(), 1, "疑点的笔记并了进来");
    }

    /// 主区（H0）身份口径不变：仍按文件 + 区间相交，块信息不参与。
    #[test]
    fn rule_items_still_merge_by_range_overlap() {
        let mut q = DeepQueue::new();
        q.enqueue(req_kind(spec_rule("a.rs", 10, 20), ItemKind::Primary, 1.0, None));
        let o = q.enqueue(req_kind(spec_rule("a.rs", 15, 25), ItemKind::Ripple, 0.9, None));

        assert_eq!(o, EnqueueOutcome::MergedPending);
        assert_eq!(q.items().len(), 1);
    }

    /// 热区只覆盖**峰值簇**（批 D-2〔D17〕）：同项里另有单家族的疑点时，它那几行不算共识面——
    /// 否则「一个家族随口提过」也会让压在那儿的 finding 免验。
    #[test]
    fn hot_ranges_cover_only_the_peak_cluster() {
        let mut q = DeepQueue::new();
        let b = blk("def:f1", 1, 60);
        // 第 10 行那条被 alpha / beta 都提（峰值簇），第 40 行那条只有 gamma 提
        q.enqueue(arr(spec_scan("a.rs", 10, 10, "let x = take(v);", "甲"), Some("alpha"), b.clone()));
        q.enqueue(arr(spec_scan("a.rs", 11, 11, "let x = take(v);", "乙"), Some("beta"), b.clone()));
        q.enqueue(arr(spec_scan("a.rs", 40, 40, "完全不同的一行();", "丙"), Some("gamma"), b));

        let it = &q.items()[0];
        assert_eq!(it.peak_agreement, 2);
        let hot = it.hot_ranges();
        assert!(hot.iter().all(|&(a, _)| a < 20), "只含峰值簇那两条: {hot:?}");
        assert!(in_hot_ranges(10, 10, &hot) && in_hot_ranges(11, 11, &hot));
        assert!(!in_hot_ranges(40, 40, &hot), "单家族那条不进共识面");
    }

    /// 算不出簇（旧快照无逐条家族）→ 热区空 → 调用方一律不盖：宁可多复核。
    #[test]
    fn hot_ranges_empty_without_per_member_families() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "let x = take(v);", "h"), Some("alpha")));
        q.items[0].member_families.clear();
        assert!(q.items()[0].hot_ranges().is_empty(), "两列长度不齐 → 算不出");
        assert!(!in_hot_ranges(3, 5, &[]), "空热区什么都不覆盖");

        // 另一条算不出的路径：两列长度齐，但家族名是空的（`item_of` 那种旧形态夹具）——
        // 峰值为 0 时热区必须也是空，退成「整文件」等于给所有 finding 白发免验票。
        // 注意断的是**现算**的 `hot_ranges()`：`peak_agreement` 是入队时算好的存储字段（单调不减），
        // 手工改 `member_families` 不会动它——两者本就不是一回事。
        q.items[0].member_families = vec![Vec::new()];
        assert!(q.items()[0].hot_ranges().is_empty(), "峰值 0 → 热区空，不得退成整文件");
    }

    /// 文档判定就是阶段 0 那把尺（评审）：`.json` / `.yaml` 在 R 分母里已经不算代码行，排序侧不该另有一套；
    /// 也不再按 `.next/` 前缀判——那是本仓私有目录名，别的仓里是 Next.js 的构建产物。
    #[test]
    fn doc_classification_matches_stage_zero() {
        assert!(is_doc("docs/x.md") && is_doc("data/x.json") && is_doc("ci/x.yaml"));
        assert!(!is_doc("src/a.rs") && !is_doc(".next/tool.rs"));
    }

    // ---- C3〔D16〕分层排序键与每文件保底一格 ----

    /// 造一个 Pending 疑点项，逐维可调：文件 / 必挖轴 / 风险档 / 烟量（成员条数）/ 置信。
    fn item_at(file: &str, line: u32, peak: u32, band: u8, smoke: usize, conf: f32) -> QueueItem {
        let notes: Vec<Suspicion> = (0..smoke.max(1))
            .map(|k| Suspicion { confidence: conf, ..sus(file, line + k as u32, line + k as u32, "q", "h") })
            .collect();
        let spec = QueueItemSpec::new(file.into(), vec![(line, line)], QueueOrigin::Scan, notes).unwrap();
        let req = EnqueueReq { spec, score: conf, kind: ItemKind::Suspicion, families: vec!["alpha".into()], arrival: None };
        let mut it = QueueItem::new(req, blk(&format!("def:{file}:{line}"), line, line), band);
        it.peak_agreement = peak;
        it
    }

    fn order_of(items: Vec<QueueItem>) -> Vec<String> {
        let q = DeepQueue { items };
        q.pending_order().into_iter().map(|i| q.items()[i].id.clone()).collect()
    }

    /// 分层键逐维单变量：高层相同才落到下一层；置信只在前面全平时裂解。
    #[test]
    fn layered_key_compares_one_dimension_at_a_time() {
        // 逐项一个文件：保底层对谁都成立、互相抵消，比的就是分层键本身。
        let hi_peak = item_at("a1.rs", 10, 2, 0, 1, 0.1);
        let hi_band = item_at("a2.rs", 20, 1, 3, 1, 0.1);
        let hi_smoke = item_at("a3.rs", 30, 1, 0, 4, 0.1);
        let hi_conf = item_at("a4.rs", 40, 1, 0, 1, 0.9);
        let doc = item_at("notes.md", 50, 1, 0, 1, 0.9);
        let ids: Vec<String> = [&hi_peak, &hi_band, &hi_smoke, &hi_conf, &doc].iter().map(|it| it.id.clone()).collect();

        let got = order_of(vec![doc.clone(), hi_conf.clone(), hi_smoke.clone(), hi_band.clone(), hi_peak.clone()]);

        assert_eq!(got[0], ids[0], "必挖轴最高的先挖");
        assert_eq!(got[1], ids[1], "其次风险档");
        assert_eq!(got[2], ids[2], "再次烟量");
        assert_eq!(got[3], ids[3], "代码优先于文档（置信相同）");
        assert_eq!(got[4], ids[4], "文档垫底");
    }

    /// 置信只做末位裂解：前面各维全平时才看它。
    #[test]
    fn confidence_only_breaks_ties() {
        let low = item_at("a.rs", 10, 1, 1, 1, 0.2);
        let high = item_at("a.rs", 20, 1, 1, 1, 0.8);
        let (lo, hi) = (low.id.clone(), high.id.clone());
        assert_eq!(order_of(vec![low.clone(), high.clone()]), vec![hi.clone(), lo.clone()]);

        // 但前一维（风险档）一分高下，置信就不参与了
        let low_band_high_conf = QueueItem { risk_band: 0, ..high };
        let high_band_low_conf = QueueItem { risk_band: 2, ..low };
        assert_eq!(order_of(vec![low_band_high_conf, high_band_low_conf]), vec![lo, hi]);
    }

    /// 每文件保底一格：A 文件五个高分项、B 文件一个低分项 → 派发序 A₁ → B₁ → A₂。
    #[test]
    fn each_file_gets_a_first_slot() {
        let mut items: Vec<QueueItem> = (0..5).map(|k| item_at("a.rs", 10 + k * 10, 2, 3, 3, 0.9)).collect();
        items.push(item_at("b.rs", 10, 1, 0, 1, 0.1));
        let b_id = items[5].id.clone();

        let got = order_of(items);

        assert_eq!(got[1], b_id, "B 的唯一项插在 A 的第二项之前: {got:?}");
        assert!(got[0].starts_with("a.rs") && got[2].starts_with("a.rs"));
    }

    /// 保底随派发自然失效：A 已有一格在途 → A 的其余项回到常规序，不再抢 B 的保底位。
    #[test]
    fn first_slot_expires_once_the_file_has_a_cell() {
        let mut items: Vec<QueueItem> = (0..3).map(|k| item_at("a.rs", 10 + k * 10, 2, 3, 3, 0.9)).collect();
        items[0].state = ItemState::InFlight;
        items.push(item_at("b.rs", 10, 1, 0, 1, 0.1));
        let b_id = items[3].id.clone();

        let got = order_of(items);

        assert_eq!(got[0], b_id, "B 仍有保底位，排在 A 的剩余项之前: {got:?}");
        assert!(got[1].starts_with("a.rs") && got[2].starts_with("a.rs"));
    }

    /// 收场 drain 与派发共用一个序：保底项不会「被留下却轮不到派」或反过来。
    #[test]
    fn drain_and_dispatch_share_one_order() {
        let mut items: Vec<QueueItem> = (0..4).map(|k| item_at("a.rs", 10 + k * 10, 1, 3, 3, 0.9)).collect();
        items.push(item_at("b.rs", 10, 1, 0, 1, 0.1));
        let mut q = DeepQueue { items };
        let kept: Vec<String> = q.pending_order().into_iter().take(2).map(|i| q.items()[i].id.clone()).collect();

        q.drain_with(2, 0);

        let alive: Vec<String> = q.items().iter().filter(|it| it.state == ItemState::Pending).map(|it| it.id.clone()).collect();
        assert_eq!(alive, kept, "留下的正是序里前两名");
        let first = q.pop_dispatch().unwrap();
        assert_eq!(first.id, kept[0], "派发也从序首取");
    }

    /// 风险档取并入过本项的最高档（同块的到达档位相同；旧快照项被升级时才会变）。
    #[test]
    fn risk_band_takes_the_highest_of_merged_arrivals() {
        let mut q = DeepQueue::new();
        let b = blk("def:f1", 1, 30);
        q.enqueue(Arrival { req: req(spec_scan("a.rs", 3, 3, "q", "h"), Some("alpha")), block: b.clone(), risk_band: 1 });
        q.enqueue(Arrival { req: req(spec_scan("a.rs", 9, 9, "q2", "h2"), Some("beta")), block: b, risk_band: 3 });

        assert_eq!(q.items()[0].risk_band, 3);
    }
}
