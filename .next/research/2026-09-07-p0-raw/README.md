# P0 原始数据（入库部分：本仓自己的数据）

- `oracle-hobby.json`：本仓 proj.db 38 条 upheld findings（id×head 去重，quote 复核标记），`eval/p0_oracle.py hobby` 产出。
- `replay/hobby-<base>-<head7>.json`（+ `-score` / `-noripple` / `-score-noripple`）：oracle B 七个 diff 的 v4 回放与评分。
- `replay/dist-<sha7>.json`：dev 近 30 个非 merge 提交的回放（预算形状语料）。
- `replay/report-v4.txt`：θ 曲线 + (规模, |H0|, R) 表 + 三档起步值。
- PR181 侧（pack / e3 / oracle-pr181 / curated / replay/pr181-*）在 `../../private/p0-raw/`（gitignored）。
重算：`python3 eval/risk_replay.py replay . <base> <head> --history-repo . --history-ref <base> --ledger .redteam/proj.db --before <job created_at> --theta 1.5`。
