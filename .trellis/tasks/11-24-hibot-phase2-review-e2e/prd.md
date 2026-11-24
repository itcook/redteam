# Phase 2 · review 端到端（2.0–2.7 + capabilities + P2 验真回退）

## 背景

hibot audit 执行单元接入的 U3 单元（协调者 session `01a20ba0` 派发）。计划权威：
`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §5 Phase 2 / §11 / §12（Q2/Q6/Q8/Q14）/ 附录 A·D·E；
契约权威：`~/hibot/.docs/audit/contract.md` §1.2 / §3.2 / §3.6 + `schema/*.schema.json`（**只读**，不复制进本仓）。

前置：U2（Phase 1 `/Users/weilai/hobby/.worktrees/redteam/hibot-audit-exec` @ `9f27a4a`）已合入 `hibot-executor`。
本单元继续在 `feature/hibot-audit-exec`，不 rebase、不碰 `hibot-executor`。

## 2.0 数据流（先写清再实现）

```
job.json ──manifest::load──► JobManifest ──validate──► kind / limits / paths
inputs.diff_patch ──sha256 已校验──► split_diff_by_file ──► FileChange[]
inputs.merge_base ──读内容──► base（diff 左端；可能 ≠ pr.base_sha，禁止断言相等）
paths.repo ──► root（只读检出；HEAD 必须 = pr.head_sha）
        │
        └──► run_code_review(args, registry, cfg, changes, root, head_sha, sink, carried)
                 （**不经** run_review_dispatch：那条路会重新采集 git diff）

收口：findings ──► 逐字重锚（quote = 检出原文切片）──► fingerprint ──► dismissed 匹配
      ──► hibot 信封（extensions.redteam.*）+ status 判定
```

- 真相源 = `inputs.diff_patch`；git 只用于证据收集（risk / 分组 / 装包）。
- 两边都可得时做一致性校验，不一致**只披露**（`extensions.redteam.input_consistency`），不改行为。

## 范围与落点

| 项 | 内容 | 落点 |
|---|---|---|
| 2.1 | 输入真相源：`diff_patch` → `FileChange[]`；与 git 采集结果比对只披露 | `src/job/projection.rs`、`src/evidence/git.rs` |
| 2.2 | 投影：`root=paths.repo`、`base=merge_base`、`scope=branch`、四个布尔 false、不打印、`HEAD == pr.head_sha` | `src/job/run.rs` |
| 2.3 | `prior_report`：只读 `summary.routing`（focus/specs/wall_clock_s）；specs 须在 repo 内且可读，否则 `partial` + `routing_specs_unreadable`；不改阻断阈值 | `src/job/prior.rs` |
| 2.4 | `dismissed.csv`：RFC4180 + 固定表头 + `csv` crate + HashSet；命中不复报/不阻塞，finding 级 `extensions.redteam.ledger`；**绝不写 `refuted: true`**；`stage0_risk` 不再把 dismissed 当 sqlite | `src/job/dismissed.rs`、`src/cli/mod.rs` |
| 2.5 | quote 逐字校验：重锚 → 用检出原文修正 quote 与行号 → 指纹 → dismissed；未命中丢弃 + 计数 + `partial` | `src/job/quote.rs`、`src/positioning/mod.rs` |
| 2.6 | repo 完整性：非 shallow、head/base 对象可得、merge-base 可算；缺失 → `status=error` | `src/job/repo.rs`、`src/evidence/git.rs` |
| 2.7 | 语言落地：模型产出 / finding 文本 / `status_reason` 随 `language`；`warnings`/`extensions`/心跳不随 | `src/job/report.rs`、`tests/job_conformance.rs` |
| 出口 | `capabilities/redteam.json`：`kinds` **只含 `["review"]`**；`tests` 钉住未实现 kind 不得声明 | `capabilities/redteam.json`、`tests/capabilities.rs` |
| P2 | job 模式 `roles.refuters` 为空 → 跳过验真 + `extensions.redteam.refute_skipped="no_refuter"`；本地不变；仅 job 调用点分流 | `src/orchestrate/deep.rs`、`src/cli/mod.rs`、`src/job/report.rs` |

## 禁区

- 不做 Phase 3 triage / Phase 4 verify 分析；`kind_not_implemented` 与逐条 `inconclusive` 保留
- 不写 `state.json`（Phase 5）；不做发布面（Phase 7）
- `~/hibot` 只读；不复制 schema/样例；不改 `dev`；不重建 `plugins/redteam/bin/`
- 报告不含 cost
- 不改 `pick_refuter` 语义；`--help` 文案不动

## 验收

- 2.1 只用 patch 也能跑完整评审；差异进 `extensions`
- 2.2 投影与手写等价；HEAD 不符 → `error`；不得断言 `merge_base == pr.base_sha`
- 2.3 specs 缺失 → `partial`；阻断阈值不变（断言）；断言只读 `summary.routing`
- 2.4 引号/内嵌逗号/CRLF 正确解析；坏格式 → `error`；命中 → 审计字段完整且 `refuted` 仍为 false
- 2.5 缩进/多空格/差一行 → 修正后命中；只改行号不改空白 **必须判失败**；幻觉 quote → 丢弃且 `partial`
- 2.6 缺失对象构造 → `error`（不是 `partial`/`ok`）
- 2.7 `zh_CN`/`en_US` 各跑一次：cell 输出、finding 文本、`status_reason` 三处语言一致
- 跨阶段不变式：报告原子写、失败路径写合法报告、job 零写 repo、无 cost、指纹唯一实现、quote 逐字可查、job 零读 env

最终：`cargo test` 全绿、`make verify` 零 diff、Conventional Commits 按 2.x 分提交。

## 待协调者确认（**提议，未裁决**）

- **2.6 完整性检查**（按派单：detached、非 shallow、对象可得、merge-base 可算）：
  判 `error` 的情形为「shallow / `pr.head_sha` 对象缺失 / `pr.base_sha` 对象缺失 / `merge_base` 对象缺失 /
  `git merge-base <pr.base_sha> <pr.head_sha>` 不可算」。**不比较** `merge_base == pr.base_sha`（Q2 明令）。
  detached 是 hibot 的输入保证；非 detached 不另判错（同一 commit 的分支检出语义等价），仅在 PRD 记录口径。
- **提议新增机器码**：`repo_incomplete:<what>`（error，`what ∈ shallow|head|base|merge_base`）。附录 E 未定义完整性码。
- **提议新增报告级扩展键**：`refute_skipped`（string，P2 裁决要求）、`input_consistency`（2.1 披露）、
  `prior_routing`（2.3 披露）。附录 D 未定义这三个键。
- **dismissed 审计位置**：附录 D 把 `ledger` 列在 **finding 级**；派单又说「被驳回项不复报」。
  本单元按附录 D 实现为「保留条目 + finding 级 `extensions.redteam.ledger = {dismissed, actor, reason}`，
  且 `refuted` 恒 false」，由 hibot 按指纹过滤 verdict/评论。若协调者要求改为从 `findings` 删除，
  审计需改挂报告级。
- **diff 不一致披露键**：`input_consistency {match, files, git, only_in_patch[], only_in_git[], differing[]}`。
