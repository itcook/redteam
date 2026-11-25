# verify 真实夹具 · `zimacode-network/hiapi-backend#400`

> 来源：<https://github.com/zimacode-network/hiapi-backend/pull/400>（**已合入**，4 个提交 / 4 轮人工评审全文可查）。
> 2026-11-25 · Phase 4 增强 · 用户裁「用 backend #400 做真实验证」。

## 为什么是它

它是真实的多轮 fix→verify 闭环，且**每一轮都有人工判决可对照**：

| 组 | H1（findings 来源） | H2（修复） | 人工判决（`expected.json` 的依据） |
|---|---|---|---|
| `group-a` | `122220fde` 第 1 轮分诊的 2 个阻塞 | `54f4c944d` | 2 个阻塞都修好；**但严格化引入了 🔴1 新问题**（回退重选被拒渠道，`-count=50` 复现 19 次失败）→ 该条期望 `regressed` |
| `group-b` | `54f4c944d` 第 2 轮 3 条阻塞（Q1/Q2/Q7） | `a88eb6458` | Q1、Q7 关闭；Q2 主路径修好、**边界（已切到 llm 的漂移协议）仍开** → `resolved / unresolved / resolved` |
| `group-c` | `a88eb6458` 第 3 轮的 Q2 边界 | `7c036382b` | 本轮关闭（只改错误文案 + 文档三处 + 测试）→ `resolved` |

三组合起来覆盖：`regressed`（真实回归）、`unresolved`（部分修复）、`resolved`（代码修复 + **非代码类修复**）。

## 口径（不伪造判决）

- `expected.json` 的值 = **人工评审的判决**（每轮 review body 明写「已关闭 / 仍开 / 新问题」），
  不是我们跑出来的结果；每条附 `note` 说明依据。
- `findings.json` 的 `quote` **逐字取自 H1 文件**（`line_start/line_end` 落在同一行）；
  `title`/`body`/`recommendation` 按人工评审正文**改写压缩**（不是逐字），`extensions.redteam.fixture.provenance`
  标明出处行号。
- 指纹按契约算法（`sha256(file \0 normalize(quote) \0 normalize(title))`）离线算好并**写死**——
  hibot 是唯一权威；`tests/verify_fixtures.rs` 会复算并断言一致。
- `expected` 里给的是**对这条 finding 的判决**；`group-a` 的 `regressed` 同时记录了「哪个新问题、证据在哪」，
  真跑若判 `resolved` 就是一次可量化的漏判，不粉饰。

## 怎么跑（不进 `cargo test`）

```sh
# 需要：本机 ~/hiapi/backend（可用 HIBOT_BACKEND_REPO 覆盖）+ 一个可用模型
scripts/verify-pr400.sh group-a --model deepseek/deepseek-flash
scripts/verify-pr400.sh group-b --model deepseek/deepseek-flash
scripts/verify-pr400.sh group-c --model deepseek/deepseek-flash
```

脚本会：切一个 detached worktree 到 H2 → 生成 job（`diff H1..H2`、`merge_base=H1`、fixture 的 findings）
→ 真跑 `redteam verify` → 逐条对照 `expected.json`（同 / 异 / 无法判定）→ 把 `report.json`、
`result.json` 与对照表落到 `target/pr400/<group>/`。密钥只经临时 env 注入，不落盘、不打印。

**不伪造人工判决**：差异逐条列出，不改 `expected.json`。

## 已知限制

- 只覆盖「anchored finding」这一层（verify 的作用域，见 README 的 verify 小节）；triage 的 intent claim
  不在 verify 里判。
- 人工评审里提到的「`./middleware/` 整包另有既有失败」等环境噪音与本次判决无关，未纳入夹具。
- H1/H2 都是历史提交；若上游仓被强推/删分支导致 commit 不可达，用 `git fetch` 拉回或换镜像。
