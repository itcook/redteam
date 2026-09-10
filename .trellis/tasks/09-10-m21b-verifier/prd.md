# M21b 批E：verifier 流式段——verifier.md/触发跳过选择/流消费 refute 率/离线回归基线

## Goal

M21b plan 批 E（依赖批 D 合回）：verifier.md 角色尾拆分（analysis 先于 verdict、不可撤回）+ build_verify_prompt；触发集（blocking+判例槽）+ 跳过（两异族∧reviewer 确认，落地时刻值）+ 异族自动选择（无异族同族+warning）+ enabled 开关；verify.rs 改 FindingEvent 流消费者（失败保守 upheld、refute 率入 RunMeta、接入 barrier）；eval/verify_score.py dismissed/upheld 二分类离线回归与基线。分支 feat/m21b-verifier

## Requirements

- TBD

## Acceptance Criteria

- [ ] TBD

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
