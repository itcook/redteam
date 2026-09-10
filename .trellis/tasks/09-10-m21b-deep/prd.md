# M21b 批D：深挖队列与 reviewer 运行时——入口五态/优先级预算/门驱动派发/barrier/Phase Deep+resume/观测

## Goal

M21b plan 批 D（依赖批 A/B 合回）：src/orchestrate/queue.rs 队列项+入口五态（merge_suspicions 谓词、agreement 活值）+ 优先级/预算/必挖集不截/保底/graceful drain + tokio mpsc 门驱动单派发器（无角色 workers〔D13〕、scanner 先于 H0）+ run_review_shot（流式落盘 + FindingEvent 发射）+ barrier reduce 接线（findings 报告重建、截断披露）+ Phase Deep/Verify + queue_items 快照 resume + RunMeta 观测收口。分支 feat/m21b-deep

## Requirements

- TBD

## Acceptance Criteria

- [ ] TBD

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
