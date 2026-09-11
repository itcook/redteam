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

    /// Fake 429 注入（AC1，现 D13 语义）：首发 429（短 Retry-After）→ 门收窄 cap−1 → 等待后重试成功；
    /// ScanLease.run 全链路（GateAttempts 排队重试）不丢门位。
    #[tokio::test]
    async fn fake_429_shrinks_gate_then_succeeds() {
        use crate::provider::ProviderError;
        let mut reg = ProviderRegistry::new();
        let ok_turn = AssistantTurn {
            text: r#"{"suspicions":[],"scanned_areas":[]}"#.into(),
            tool_calls: vec![], input_tokens: 1, output_tokens: 1, cached_input_tokens: 0,
            finish_reason: Some("stop".into()), reasoning_tokens: None,
        };
        reg.insert_scan(
            "p/r",
            "p",
            4,
            Box::new(crate::provider::tool_loop::FakeChatBackend::new_results(vec![
                Err(ProviderError::RateLimited { retry_after: Some(std::time::Duration::from_millis(10)) }),
                Ok(ok_turn),
            ])),
            std::time::Duration::from_secs(5),
            0,
        );
        // 单发走 turn_final（无 tools）：首发 429 → GateAttempts.on_rate_limited 等 10ms 后重试成功。
        let prompt = crate::prompt::ScanPrompt {
            system: "s".into(),
            blocks: vec![crate::prompt::PromptBlock { text: "b".into(), cache_boundary: false }],
        };
        let lease = reg.lease("p/r").await.unwrap();
        let call = lease.run(&prompt).await;
        assert!(call.is_ok(), "429 后重试成功: {call:?}");
        drop(lease);
        let binding = reg.gate_stats();
        let st = binding.get("p").unwrap();
        assert_eq!(st.rate_limited, 1, "撞过一次 429");
        assert_eq!(st.effective_cap, 1, "单请求无他人在途：target = min(cap-1, others=0) → 收到下限 1（速率型棘轮形状）");
        assert_eq!(st.cap, 4, "配置门宽不变");
    }

    /// 乱序完成：两个队列项交替 pop，必挖（Primary）先于疑点；租约逐个释放不互堵。
    #[tokio::test]
    async fn dispatch_order_and_drain_under_budget() {
        use crate::orchestrate::queue::EnqueueOutcome;
        let mut q = DeepQueue::new();
        q.enqueue(EnqueueReq { spec: spec_rule("z.rs"), score: 0.1, kind: ItemKind::Primary, family: None });
        q.enqueue(EnqueueReq {
            spec: QueueItemSpec::new("s.rs".into(), vec![(1, 2)], QueueOrigin::Scan, vec![crate::domain::Suspicion {
                file: "s.rs".into(), line_start: 1, line_end: 2, hint: "h".into(), category: None, confidence: 0.9, quote: "q".into(),
            }]).unwrap(),
            score: 0.9, kind: ItemKind::Suspicion, family: Some("a".into()),
        });
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/r", "p", 1, Box::new(FakeChatBackend::new(vec![turn()])), Duration::from_secs(5), 0);
        // 门宽 1：第一个租约未释放前第二个 dispatch 必须等——乱序完成 / 慢 consumer 语义。
        let (l1, i1) = dispatch_one(&mut q, &reg, "p/r").await.unwrap().unwrap();
        assert_eq!(i1.kind, ItemKind::Primary, "必挖先出");
        q.mark_settled(&i1.id, 1);
        drop(l1);
        let (l2, i2) = dispatch_one(&mut q, &reg, "p/r").await.unwrap().unwrap();
        assert_eq!(i2.kind, ItemKind::Suspicion);
        assert_eq!(q.pending_count(), 0);
        q.mark_settled(&i2.id, 1);
        drop(l2);
        assert_eq!(q.enqueue(EnqueueReq { spec: spec_rule("z.rs"), score: 1.0, kind: ItemKind::Primary, family: None }), EnqueueOutcome::MergedSettled, "已结案沿用");
    }
}
