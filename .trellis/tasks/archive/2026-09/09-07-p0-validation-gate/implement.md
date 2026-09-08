# implement · P0 验证门

> 顺序执行；每步的验证命令与产物写明。`P0_RAW=<worktree>/.next/private/p0-raw`、
> `BK=<PR181 工作树绝对路径，只记在 .next/private/>`。

## 0. 预飞
- [x] `git -C $BK rev-parse --short HEAD` = 215afd77b 且 `status --short` 空；`mkdir -p $P0_RAW/{replay,pack/pr181,e3/A,e3/B}`。
- [x] 本仓 `git status` 干净；`cargo build` 通过；`echo ${DEEPSEEK_API_KEY:+ok}`。

## 1. oracle（`eval/p0_oracle.py`）
- [x] `pr181`：读 `$BK/.redteam/proj.db` cells（4065@GLM / 4065@Kimi / 03a7@DeepSeek）+ rpiv md（`**Where**` 行 `path:line[-line]`）
  + `$P0_RAW/curated-pr181.json`（人工：ocr 2 条 + topic 分组）→ `oracle-pr181.json`。断言计数 6 / 3 / 3 / 8 / 2。
- [x] `hobby`：读本仓 proj.db results × jobs.args → upheld 条目带 job/base/head，id 去重 → `oracle-hobby.json`；
  `33c4522` 作业按 created_at 与 `git log` 推 head，推不出记 `skipped`。
- [x] 人工填 `curated-pr181.json` 的 topic 分组（GLM/Kimi/DeepSeek/rpiv/ocr 同主题合并），主裁决集列表打印核对。

## 2. E1 引擎（`eval/risk_replay.py`）
- [x] 移植 `src/diffsym/mod.rs` 三语言 def 正则 + TS 关键字滤 + `block_end` + `enclosing_def_start`。
- [x] `replay <repo> <base> <head> [--history-repo --history-ref --ledger --before --theta --out]`：design §3.1-3.3。
- [x] `score <replay.json> <oracle.json> [--set topic_diff|strict_diff|union|hobby --job <id>]`：design §2 命中口径，两粒度都出。
- [x] `report <dir>`：θ 曲线 + `(code_lines,|H0|,R)` 表 + 三档 `max_cells` 起步值。
- [x] `selftest`：合成样本（rs/go/ts def 各 ≥ 3、块配平、相邻合并、交集判定、ledger 时间守卫）全绿。
- 验证：`python3 eval/risk_replay.py selftest`；`python3 eval/risk_replay.py replay $BK <BASE> <HEAD> --history-repo <公司仓主 checkout> --history-ref dev --out $P0_RAW/replay/pr181-r1.json`。

## 3. E1 回放与裁决
- [x] 第 1 轮：PR181 四集合 + oracle B 五个 diff（`6f95c25~1..6f95c25`、`4f52ac9..8dcaf6f`、`f028f92..290236e`、`e8dc24f..00e23fc`、
  `e8dc24f..3e60804`、`e8dc24f..99138bb`；ledger `--before` 取各作业 created_at）；分布语料 = `git log --no-merges -30` 逐 commit `replay`。
- [x] 裁决：`topic_diff.hit_range ≥ 2/3` 且 `empty_zones_range ≤ 1/2`；不过 → 改规则 / θ，记录规则 diff，最多三轮。
- [x] 产出研究表：对齐表、四集合两粒度命中、空热区、θ 曲线、`(规模,|H0|,R)`、三档起步值。

## 4. 组包仪器（`examples/dump_pack.rs`）
- [x] 实现 design §4.1；`cargo build --example dump_pack`。
- [x] `cd $BK && cargo run --manifest-path <本仓>/Cargo.toml --example dump_pack -- --base a7c827b70 --models DeepSeek --focus "$(sqlite3 .redteam/proj.db "select json_extract(args_json,'$.focus') from jobs where job_id='03a7bffb58fe'")" --out $P0_RAW/pack/pr181`
  （`LANG` 与用户会话一致）。
- [x] 校验 meta：40 文件；`bytes/3.5` 与 180.7K 同量级；user 含 `<dossier>`、`Target: branch a7c827b70...HEAD`。

## 5. E3（`eval/e3_contract_ab.py`）
- [x] `variant`：生成 B 臂 + `variant-diff.txt`，断言差异仅三处。
- [x] 预飞 `run --arm A --n 1 --max-tokens 32768`：看 `finish_reason`；`length` → 两臂改 65536 并记录。
- [x] `run --arm A --n 3`、`run --arm B --n 3`（≤ 2 并发）；落盘 usage / finish_reason / 墙钟 / parse。
- [x] `score`：区间级命中 + 人工主题确认表 → 填表 → 最终命中、噪音比、`c_hi`、schema 观察。
- [x] 裁决：疑点臂命中 ≥ finding 臂 → §4 契约成立；否则记「回 D6 采纳项 1」。

## 6. 写回
- [x] research §15「E1 回放数据」、§16「E3 契约对照」（`.next/private/research/2026-09-07-redteam-p0-validation-gate-research.md`（自 `.next/research/2026-09-04-…-research.md` §15/§16 迁出）），状态头加 v 号。
- [x] `.next/roadmap/followup.md` 实现债表：`examples/dump_pack.rs` → M21a `--pack-only` 落地即删。
- [x] `.next/roadmap/P0-validation-gate.md` 状态头补句「E1 / E3 数据已入 research §15 / §16」。

## 7. 收口验证
- [x] `cargo test`（含 example 编译）零回归；`make verify` 绿。
- [x] `python3 eval/risk_replay.py selftest` 绿。
- [x] `git status` 只含：`eval/p0_oracle.py`、`eval/risk_replay.py`、`eval/e3_contract_ab.py`、`examples/dump_pack.rs`、`eval/README.md`（加三行说明）、
  `.next/roadmap/followup.md`、`.next/roadmap/P0-validation-gate.md`、`.trellis/tasks/09-07-p0-validation-gate/*`。
- [x] AC5 人工过目：`git diff` 中无公司代码 / finding 正文；用被评仓的路径片段与符号名 grep `eval examples .next`（private 除外）为空。
- [x] 提交由 trellis finish-work / 用户批准后进行，不在实现步骤内自动 commit。

## 风险文件 / 回滚点
- 全部为新增文件 + 两处 roadmap 文档一行改动；回滚 = 删文件 / revert 两行。
- 目标仓 `$BK` 只读；若误改，`git -C $BK status` 立即可见。

## `task.py start` 前检查
- [x] 用户对最终规划摘要的明确批准（本轮之后的独立消息）。
- [x] K8：inline 工作流，跳过 implement.jsonl / check.jsonl 门。
