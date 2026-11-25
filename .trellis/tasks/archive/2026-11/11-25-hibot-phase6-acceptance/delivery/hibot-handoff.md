# redteam × hibot · Phase 6 交付包（一页）

> 执行单元 `redteam 0.1.0`（分支 `feature/hibot-audit-exec`，基线 `bc71ac3` + 本单元 7 个提交（含 `fix(job)`））
> 日期 2026-11-25 · runner 真调尚未可用（hibot `apps/` 为空），本包是**手工 job 目录联调**的证据。

## 1. 怎么跑

```sh
# 1) 官方示例脚本产合规 job（需非 shallow 的 hiapi-backend 检出；
#    本机 ~/hiapi/backend 是 shallow，故按脚本文档覆盖 checkout）
HIBOT_BACKEND_REPO=<非 shallow 克隆> \
  ~/hibot/.docs/examples/audit-pr-468/prepare-job.sh <triage|review|verify> <绝对目录>

# 2) 投递配置（内容来源 = paths.config，job_native；凭据只走 env 的 apiKeyEnv）
cp <渲染好的 executor config> <目录>/job/config/executor.redteam.json

# 3) 执行（不读 stdin、不依赖 cwd、不写 ~/.redteam）
redteam <kind> --job-dir <目录>/job

# 4) 验收矩阵（18 组合；HIBOT_SCHEMA_DIR 存在时逐份过 hibot schema）
HIBOT_SCHEMA_DIR=~/hibot/.docs/audit/schema scripts/acceptance-matrix.sh
```

产物：`<目录>/job/out/report.json`（终态唯一真相）+ `out/state.json`（可选心跳）。
退出码：`0` ok/partial 已写报告、`1` error 已写报告、`2` manifest 不可读（不写报告）。

## 2. 本次真跑的结果（deepseek-flash，job 配置 env-only）

| kind | status | 墙钟 | 关键产出 | usage(in/out/cached) |
|---|---|---|---|---|
| triage | `ok` | 40s | 0 findings（intent 轴 1 行，未达 blocker） | 35968 / 11985 / 26240 |
| verify | `ok` | 9s | 2/2 逐条 `resolved`；counts 四态正确 | 10486 / 2550 / 10109 |
| review | `partial`（`wall_clock_exhausted`） | 600s（外部上限，SIGTERM 收敛） | 13 findings | 231096 / 390305 / 176256（`wall_clock_ms`=598987） |

每份报告都过了：hibot `report.<kind>-1.json` schema（`$id` registry）、finding 指纹复算、
finding/verification quote 在检出原文逐字命中、verify 输入指纹集合 == 输出集合、`state.json` 三帧 + 终帧 `finishing`。
校验日志：`evidence/real-run/check.log`；预检日志：`evidence/real-run/preflight/preflight.log`。

## 3. 示例数据的三个缺陷 → 修复映射（hibot `91834c2`）

| # | 缺陷（`91834c2` 之前） | 执行单元的表现 | `91834c2` 的修复 |
|---|---|---|---|
| 1 | `inputs/*.diff.patch` 是无 `diff --git` 头的节选 | `status=error` · `input_schema_mismatch:diff_patch` | 脚本改为在真实检出里 `git diff <base>..<head>` 生成完整 patch 并重算 sha256 |
| 2 | `paths.repo` 是 `git archive` 展开的树（无 `.git`） | `status=error` · `repo_incomplete:head` | 脚本改为 `git worktree add --detach`，产出 detached、非 shallow 的真实检出 |
| 3 | triage/review 的 `pr.base_sha` 是仓库里不存在的占位 sha | `status=error` · `repo_incomplete:base` | 脚本把 `pr.base_sha` 钉成真实 merge-base（`179c7e02…`）并同步 manifest |

三条都是**示例数据**问题，不是执行单元缺陷；契约本来就要求「runner 本地生成 diff + 提供完整检出 + 真实 sha」。
`91834c2` 之后用官方脚本重跑，三 kind 全部走通（预检 8 项 × 3 kind 全 PASS）。

注意：`pr.base_sha == inputs.merge_base` 是 hibot 为可执行性做的取舍（契约允许「可不同」，相等合法）；
报告 `input.base_sha` 原样回显该值，**不**当作回归。

## 4. 真跑观察（已修复）

- review 在 600s 外部上限被 SIGTERM 时，引擎按契约收敛出合法 `partial` 报告（13 findings 全部过指纹/quote 校验），
  `status_code = wall_clock_exhausted`——**中断路径可用**；hibot 侧可按 `report.json` 直接消费。
- 修复 1：SIGTERM / 预算耗尽路径的 `status_reason` 曾把同一句渲染两遍；现在每条原因只出现一次。
- 修复 2：中断路径的 `usage.wall_clock_ms` 曾取「最长 cell 时长」；现在与正常路径同口径，是 job 墙钟
  （本次 598987ms ≈ 600s）。hibot 可直接用它估 job 延迟。

## 5. 已知限制

- **runner 真调未接**：hibot `apps/` 为空（M0/M1 未开始），本包不覆盖 runner 的派活/接纳/上传链路。
- `verify` 只核验**有代码锚**的 finding（triage 的 intent 项没有代码锚，不进 verify）；verify 不产 finding。
- job 模式开验真但没有显式 `refuter` 角色时，跳过验真并在 `extensions.redteam.refute_skipped = "no_refuter"` 披露。
- 报告**不含 cost**（`usage` 只有 token 与墙钟）；成本统计按契约归 hibot 侧。
- 本机 `~/hiapi/backend` 是 shallow 检出：官方脚本必须用 `HIBOT_BACKEND_REPO` 指向非 shallow 克隆，否则
  `git worktree add` 得到的仍是 shallow 检出，执行单元按契约报 `repo_incomplete:shallow`。
- 能力声明只含已实现的三个 kind（triage / review / verify）；`requires = {repo_checkout:true, pr_context:false, network:"provider-only"}`。
