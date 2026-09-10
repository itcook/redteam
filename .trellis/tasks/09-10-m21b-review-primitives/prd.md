# M21b 批B：reviewer 静态零件——区域 pack/reviewer.md/parse/SSE 流式旁路/三标配

## Goal

M21b plan 批 B：RegionPack 构造即切片+爆炸半径+跨组索引+spec+笔记（契约槽留空 M21d）、reviewer.md 角色尾 + build_review_prompt（共享头字节稳定）、parse_review_output（REPAIR 一轮）、ChatBackend on_text_delta + IncrementalFindings 流式旁路（未闭合丢弃计数）、三标配（注入互斥/预算帽/读守卫 second_pass 复用）。分支 feat/m21b-review-primitives

## Requirements

- TBD

## Acceptance Criteria

- [ ] TBD

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
