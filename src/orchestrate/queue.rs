//! 深挖队列（M21b 批 C-1，design §6 规则 2〔D13〕）：入口五态 + agreement 活值。
//! 合并谓词复用 `consolidate::scan::same_suspicion`，不复制。跨轮判例守卫本批空过（按无命中入队）。

use crate::consolidate::scan::same_suspicion;
use crate::domain::Suspicion;
use crate::pack::region::{Origin as QueueOrigin, QueueItemSpec};

/// 队列项运行时状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemState {
    Pending,
    InFlight,
    Settled,
    Skipped,
}

/// 优先级分层（C-2 排序键；本批只记录，不排序）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Primary,
    Ripple,
    Suspicion,
    Needs,
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
}

impl QueueItem {
    fn new(spec: QueueItemSpec, score: f32, kind: ItemKind, family: Option<&str>) -> Self {
        let id = item_id(&spec);
        let families = family.filter(|f| !f.is_empty()).map(|f| vec![f.to_string()]).unwrap_or_default();
        QueueItem {
            id,
            spec,
            score,
            agreement: 1,
            state: ItemState::Pending,
            kind,
            dispatch_agreement: 0,
            landed_agreement: None,
            families,
        }
    }
}

/// 入队参数。`family` 空 = 无家族（H0 / needs），不参与异族计数。
pub struct EnqueueReq {
    pub spec: QueueItemSpec,
    pub score: f32,
    pub kind: ItemKind,
    pub family: Option<String>,
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

    pub fn items(&self) -> &[QueueItem] {
        &self.items
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
        self.items.push(QueueItem::new(req.spec, req.score, req.kind, req.family.as_deref()));
        EnqueueOutcome::New
    }

    fn find_merge_idx(&self, spec: &QueueItemSpec, incoming_id: &str) -> Option<usize> {
        self.items.iter().position(|it| same_item(&it.spec, spec) || it.id == incoming_id)
    }

    fn merge_into(&mut self, idx: usize, req: EnqueueReq) -> EnqueueOutcome {
        let it = &mut self.items[idx];
        let family_new = match req.family.as_deref() {
            Some(f) if !f.is_empty() && !it.families.iter().any(|x| x == f) => {
                it.families.push(f.to_string());
                true
            }
            _ => false,
        };
        if family_new {
            it.agreement = it.agreement.saturating_add(1);
        }
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

    #[cfg(test)]
    fn set_state(&mut self, id: &str, state: ItemState) {
        if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
            it.state = state;
        }
    }
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
            family: family.map(str::to_string),
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

    #[test]
    fn different_quotes_same_file_do_not_merge() {
        let mut q = DeepQueue::new();
        q.enqueue(req(spec_scan("a.rs", 3, 5, "v[i]", "idx"), Some("alpha")));
        let o = q.enqueue(req(spec_scan("a.rs", 40, 42, "unwrap()", "opt"), Some("alpha")));
        assert_eq!(o, EnqueueOutcome::New);
        assert_eq!(q.items().len(), 2);
    }
}
