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

/// 入队结果（五态可观测）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnqueueOutcome {
    /// 无命中 → 新项 agreement = 1。
    New,
    /// 命中排队中 → 并入（异族 agreement+1 / 同家族只合并）。
    MergedPending,
    /// 命中在途 → 只改记录，不重挖。
    MergedInFlight,
    /// 命中本轮已结案 → 并入并沿用结案。
    MergedSettled,
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
}

impl QueueItem {
    fn new(spec: QueueItemSpec, score: f32, kind: ItemKind, families: Vec<String>) -> Self {
        let id = item_id(&spec);
        let families: Vec<String> = dedup_families(families);
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

    pub fn items_mut(&mut self) -> &mut [QueueItem] {
        &mut self.items
    }

    pub fn get(&self, id: &str) -> Option<&QueueItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// 五态入口。判例守卫空过：调用方把跨轮命中当无命中喂进来即可。
    pub fn enqueue(&mut self, req: EnqueueReq) -> EnqueueOutcome {
        let incoming_id = item_id(&req.spec);
        if let Some(idx) = self.find_merge_idx(&req.spec, &incoming_id) {
            return self.merge_into(idx, req);
        }
        self.items.push(QueueItem::new(req.spec, req.score, req.kind, req.families));
        EnqueueOutcome::New
    }

    fn find_merge_idx(&self, spec: &QueueItemSpec, incoming_id: &str) -> Option<usize> {
        self.items.iter().position(|it| same_item(&it.spec, spec) || it.id == incoming_id)
    }

    fn merge_into(&mut self, idx: usize, req: EnqueueReq) -> EnqueueOutcome {
        let it = &mut self.items[idx];
        for f in req.families.into_iter().filter(|f| !f.is_empty()) {
            if !it.families.contains(&f) {
                it.families.push(f);
            }
        }
        it.agreement = agreement_of(&it.families);
        it.spec.ranges = union_ranges(&it.spec.ranges, &req.spec.ranges);
        it.spec.notes = merge_notes(&it.spec.notes, &req.spec.notes);
        if req.score > it.score {
            it.score = req.score;
        }
        it.id = item_id(&it.spec);
        match it.state {
            ItemState::Pending | ItemState::Skipped => EnqueueOutcome::MergedPending,
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
        it.kind == ItemKind::Primary || it.agreement >= 2
    }

    /// 到期 drain：Pending 非必挖且未进保底的标 skipped。
    /// 保底 = ⌈max_cells/4⌉ 条 Suspicion；主区吃穿时帽 = 必挖 + 保底。`max_cells = 0` 只留必挖。
    /// 返回新标 skipped 的 id。
    pub fn drain(&mut self, max_cells: u32) -> Vec<String> {
        let pending = self.pending_order();
        if pending.is_empty() {
            return Vec::new();
        }
        let must: Vec<usize> = pending.iter().copied().filter(|&i| self.is_must(&self.items[i])).collect();
        let floor = if max_cells == 0 { 0 } else { max_cells.div_ceil(4) as usize };
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
        if max_cells > 0 {
            let budget = max_cells as usize;
            for &i in &pending {
                if keep.len() >= budget {
                    break;
                }
                keep.insert(i);
            }
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
        let i = *self.pending_order().first()?;
        self.items[i].state = ItemState::InFlight;
        self.items[i].dispatch_agreement = self.items[i].agreement;
        Some(self.items[i].clone())
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
            let conf = it.spec.notes.iter().map(|n| n.confidence).fold(0.0_f32, f32::max);
            conf * it.agreement as f32 * category_weight(&it.spec.notes)
        }
        ItemKind::Needs => NEEDS_MID_SCORE,
    }
}

fn cmp_priority(a: &QueueItem, b: &QueueItem) -> std::cmp::Ordering {
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

fn same_item(a: &QueueItemSpec, b: &QueueItemSpec) -> bool {
    if a.origin != b.origin {
        return false;
    }
    match a.origin {
        QueueOrigin::Rule => a.file == b.file && ranges_overlap(&a.ranges, &b.ranges),
        QueueOrigin::Scan | QueueOrigin::Needs => {
            a.file == b.file
                && a.notes.iter().any(|na| b.notes.iter().any(|nb| same_suspicion(na, nb)))
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

fn merge_notes(a: &[Suspicion], b: &[Suspicion]) -> Vec<Suspicion> {
    let mut out = a.to_vec();
    for n in b {
        if !out.iter().any(|x| same_suspicion(x, n)) {
            out.push(n.clone());
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
        });
        q.enqueue(EnqueueReq {
            spec: spec_scan("a.rs", 80, 80, "db.write(t);", "h2"),
            score: 0.5,
            kind: ItemKind::Suspicion,
            families: vec!["alpha".into(), "beta".into(), "gamma".into()],
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
        EnqueueReq { spec, score, kind, families: family.map(|f| vec![f.to_string()]).unwrap_or_default() }
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
}
