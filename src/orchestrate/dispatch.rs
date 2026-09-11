//! 门驱动单派发器（M21b 批 C-3，design §6 规则 5〔D13〕）：
//! `registry.lease(reviewer)` 等到门位 → pop 最高 Pending → 调用方跑 cell。
//! 无角色 workers。t=0 入队序由调用方保证 scanner 先于 H0。

use super::queue::{DeepQueue, QueueItem};
use crate::provider::registry::{ProviderRegistry, ScanLease};
use crate::provider::ProviderError;

/// 一轮：等一个 reviewer 门位，弹出最高项。队列空 → None（不占门）。
/// 租约必须活过 cell 调用——调用方 drop lease 才放门。
pub async fn dispatch_one<'a>(
    queue: &mut DeepQueue,
    registry: &'a ProviderRegistry,
    reviewer_key: &str,
) -> Result<Option<(ScanLease<'a>, QueueItem)>, ProviderError> {
    if queue.pending_count() == 0 {
        return Ok(None);
    }
    let lease = registry.lease(reviewer_key).await?;
    Ok(queue.pop_dispatch().map(|item| (lease, item)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::queue::{EnqueueReq, ItemKind};
    use crate::pack::region::{Origin as QueueOrigin, QueueItemSpec};
    use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend};
    use std::time::Duration;

    fn turn() -> AssistantTurn {
        AssistantTurn {
            text: "{}".into(),
            tool_calls: vec![],
            input_tokens: 1,
            output_tokens: 1,
            cached_input_tokens: 0,
            finish_reason: Some("stop".into()),
            reasoning_tokens: None,
        }
    }

    fn spec_rule(file: &str) -> QueueItemSpec {
        QueueItemSpec::new(file.into(), vec![(1, 2)], QueueOrigin::Rule, vec![]).unwrap()
    }

    #[tokio::test]
    async fn pops_highest_primary_first() {
        let mut q = DeepQueue::new();
        q.enqueue(EnqueueReq {
            spec: QueueItemSpec::new(
                "s.rs".into(),
                vec![(1, 2)],
                QueueOrigin::Scan,
                vec![crate::domain::Suspicion {
                    file: "s.rs".into(),
                    line_start: 1,
                    line_end: 2,
                    hint: "h".into(),
                    category: None,
                    confidence: 0.9,
                    quote: "q".into(),
                }],
            )
            .unwrap(),
            score: 9.0,
            kind: ItemKind::Suspicion,
            family: Some("a".into()),
        });
        q.enqueue(EnqueueReq {
            spec: spec_rule("p.rs"),
            score: 0.1,
            kind: ItemKind::Primary,
            family: None,
        });
        let mut reg = ProviderRegistry::new();
        reg.insert_scan(
            "p/r",
            "p",
            8,
            Box::new(FakeChatBackend::new(vec![turn()])),
            Duration::from_secs(5),
            0,
        );
        let (lease, item) = dispatch_one(&mut q, &reg, "p/r").await.unwrap().unwrap();
        drop(lease);
        assert_eq!(item.kind, ItemKind::Primary);
        assert_eq!(item.state, crate::orchestrate::queue::ItemState::InFlight);
        assert_eq!(q.inflight_count(), 1);
        assert_eq!(q.pending_count(), 1);
    }

    #[tokio::test]
    async fn empty_queue_does_not_lease() {
        let mut q = DeepQueue::new();
        let mut reg = ProviderRegistry::new();
        reg.insert_scan(
            "p/r",
            "p",
            1,
            Box::new(FakeChatBackend::new(vec![turn()])),
            Duration::from_secs(5),
            0,
        );
        assert!(dispatch_one(&mut q, &reg, "p/r").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn unknown_reviewer_is_unavailable() {
        let mut q = DeepQueue::new();
        q.enqueue(EnqueueReq {
            spec: spec_rule("p.rs"),
            score: 1.0,
            kind: ItemKind::Primary,
            family: None,
        });
        let reg = ProviderRegistry::new();
        let err = match dispatch_one(&mut q, &reg, "ghost/x").await {
            Err(e) => e,
            Ok(_) => panic!("expected unavailable"),
        };
        assert!(format!("{err:?}").contains("not in registry"));
        assert_eq!(q.pending_count(), 1, "失败不 pop");
    }
}
