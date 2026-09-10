# M21b 批C：aacr 接线——reviewers/redteam.py + pipeline 挂载 + eval/README

## Goal

M21b plan 批 C：~/codes/aacr-bench 本地分支 redteam-adapter 加 evaluation/reviewers/redteam.py（范围评审 {path,start_line,end_line,content} 映射、REDTEAM_NEXT_BIN/AACR_BENCH_DIR）+ pipeline 可选挂载；本仓只改 eval/README.md。--preview 真跑通放出口验收（届时批 D 已产 finding）。分支 aacr-bench:redteam-adapter（外部仓）

## Requirements

- TBD

## Acceptance Criteria

- [ ] TBD

## Notes

- Keep `prd.md` focused on requirements, constraints, and acceptance criteria.
- Lightweight tasks can remain PRD-only.
- For complex tasks, add `design.md` for technical design and `implement.md` for execution planning before `task.py start`.
