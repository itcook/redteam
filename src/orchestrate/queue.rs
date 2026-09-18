//! 深挖队列（M21b 批 C-1，design §6 规则 2〔D13〕）：入口五态 + agreement 活值。
//! 合并谓词复用 `consolidate::scan::same_suspicion`，不复制。跨轮判例守卫本批空过（按无命中入队）。

use crate::consolidate::scan::same_suspicion;
use crate::domain::Suspicion;
use crate::pack::region::{Origin as QueueOrigin, QueueItemSpec};

/// L0..L5 类别权重（design 取舍：M21b 中性 1.0，改数字不改公式）。
pub const CATEGORY_WEIGHT: [f32; 6] = [1.0; 6];
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
}

impl QueueItem {
    fn new(spec: QueueItemSpec, score: f32, kind: ItemKind, families: Vec<String>, arrival: Option<String>) -> Self {
        let id = item_id(&spec);
        let families: Vec<String> = dedup_families(families);
        let members = spec.notes.clone();
        QueueItem {
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
        }
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
fn agreement_of(families: &[String]) -> u32 {
    (families.len() as u32).max(1)
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
    pub fn enqueue(&mut self, req: EnqueueReq) -> EnqueueOutcome {
        self.enqueue_affected(req).0
    }

    /// 同 [`Self::enqueue`]，另回报受影响项下标（新建项 / 并入目标；`Duplicate` 为 `None`）——调用方只写这一项的快照（C2 Q9）。
    /// 到达键先查：已记在某项上即幂等返回，不改项、不计去重命中。
    pub fn enqueue_affected(&mut self, req: EnqueueReq) -> (EnqueueOutcome, Option<usize>) {
        if let Some(key) = &req.arrival {
            if self.items.iter().any(|it| it.arrivals.contains(key)) {
                return (EnqueueOutcome::Duplicate, None);
            }
        }
        let incoming_id = item_id(&req.spec);
        if let Some((idx, by_predicate)) = self.find_merge_idx(&req.spec, &incoming_id) {
            return (self.merge_into(idx, req, by_predicate), Some(idx));
        }
        self.items.push(QueueItem::new(req.spec, req.score, req.kind, req.families, req.arrival));
        (EnqueueOutcome::New, Some(self.items.len() - 1))
    }

    /// 合并目标 + 是否经谓词命中。id 相等（`file:origin:ranges` 逐字相同）也合并——保住「一个区间只挖一次」
    /// 与 id 唯一；但谓词命中优先：同一疑点的共识只该记到谓词认定的那一项上。
    fn find_merge_idx(&self, spec: &QueueItemSpec, incoming_id: &str) -> Option<(usize, bool)> {
        if let Some(i) = self.items.iter().position(|it| same_item(it, spec)) {
            return Some((i, true));
        }
        self.items.iter().position(|it| it.id == incoming_id).map(|i| (i, false))
    }

    fn merge_into(&mut self, idx: usize, req: EnqueueReq, by_predicate: bool) -> EnqueueOutcome {
        let it = &mut self.items[idx];
        // 共识只经谓词（C1 design §2.2；C2〔D15〕改写前提）：疑点逐 scan 格到达，队列侧逐条把新疑点与已有项的笔记比对，
        // **不是**组内传递闭包——A、C 先到互不匹配、B 后到两边都匹配时 B 只并入先命中的那项，A 与 C 永远两项。非传递漏合并
        // 只会少算共识、多复核一次（D15 实测 ≤ 1/19 簇），收场以 `closure_members` 对账、拆簇数进 RunMeta。
        // 只靠 id 捷径命中的是「谓词不认、恰好行区间逐字相同」的两条，可能是 quote 噪声下的同一问题，也可能是同一行上的
        // 两个问题：不计共识。计错了是批 D 免复核错放，不计错了只是多复核一次——取后者。
        // 但 id 捷径并入的**请求自身**共识不能丢（评审修）：上游已合并成多家族共识的疑点（必挖）撞上同区间的单家族项，
        // agreement 同时决定必挖集与排序分，丢了它就可能被预算截掉——家族不跨疑点并，agreement 取大。
        // 两条路径都取大 → agreement 单调不减（事件侧 `event ≤ landed` 的前提）。
        if by_predicate {
            for f in req.families.into_iter().filter(|f| !f.is_empty()) {
                if !it.families.contains(&f) {
                    it.families.push(f);
                }
            }
            it.agreement = it.agreement.max(agreement_of(&it.families));
        } else {
            it.agreement = it.agreement.max(agreement_of(&dedup_families(req.families)));
        }
        if let Some(key) = req.arrival {
            it.arrivals.push(key); // 调用方已查过不在任何项上
        }
        it.spec.ranges = union_ranges(&it.spec.ranges, &req.spec.ranges);
        for n in &req.spec.notes {
            if !it.members.iter().any(|m| m == n) {
                it.members.push(n.clone());
            }
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
    pub fn pending_order(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| it.state == ItemState::Pending)
            .map(|(i, _)| i)
            .collect();
        idx.sort_by(|&a, &b| cmp_priority(&self.items[a], &self.items[b]));
        idx
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
pub(crate) fn must_dig(it: &QueueItem) -> bool {
    it.kind == ItemKind::Primary || it.agreement >= 2
}

fn category_weight(notes: &[Suspicion]) -> f32 {
    notes
        .iter()
        .filter_map(|n| n.category)
        .map(|c| CATEGORY_WEIGHT[c as usize])
        .fold(1.0_f32, f32::max)
}

fn priority_score(it: &QueueItem) -> f32 {
    match it.kind {
        ItemKind::Primary | ItemKind::Ripple => it.score,
        ItemKind::Suspicion => {
            let notes = it.suspicions();
            let conf = notes.iter().map(|n| n.confidence).fold(0.0_f32, f32::max);
            conf * it.agreement as f32 * category_weight(notes)
        }
        ItemKind::Needs => NEEDS_MID_SCORE,
    }
}

/// 优先级全序：种类 → 分数（高在前）→ id。收场按最终项优先级组装报告（C2 Q8）共用。
pub(crate) fn cmp_priority(a: &QueueItem, b: &QueueItem) -> std::cmp::Ordering {
    a.kind
        .cmp(&b.kind)
        .then_with(|| priority_score(b).partial_cmp(&priority_score(a)).unwrap_or(std::cmp::Ordering::Equal))
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

/// 同一项谓词：主区按文件 + 区间相交；疑点 / needs 按文件 + 「到达过本项的任一疑点」与请求的任一疑点相似
/// （成员而非渲染笔记，C2 评审 #3：桥接疑点并入后匹配面要跟着扩大）。
fn same_item(a: &QueueItem, b: &QueueItemSpec) -> bool {
    if a.spec.origin != b.origin {
        return false;
    }
    match a.spec.origin {
        QueueOrigin::Rule => a.spec.file == b.file && ranges_overlap(&a.spec.ranges, &b.ranges),
        QueueOrigin::Scan | QueueOrigin::Needs => {
            a.spec.file == b.file
                && a.suspicions().iter().any(|na| b.notes.iter().any(|nb| same_suspicion(na, nb)))
        }
    }
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
        assert!((priority_score(it) - 0.9 * 3.0 * category_weight(it.suspicions())).abs() < 1e-6, "优先级用最高置信");

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
}
