# #400 真跑记录（verify · 2026-11-25）

模型 `deepseek/deepseek-flash`（job 模式，临时 v2 executor config，密钥只走临时 env）。
夹具与口径见 `tests/fixtures/verify/pr400/README.md`；`expected` = **人工评审判决**，未按结果修改。

## 最终一轮（代码 `9920206`：含宽容解析 + fix_scope）

| 组 | finding | expected | actual | 同/异 | 墙钟 | 延迟 max |
|---|---|---|---|---|---|---|
| a | affinity/pinned 准入被一并放宽 | `regressed` | `resolved` | **异** | 12.8s | 12.6s |
| a | 迁移 forward switch 门禁被放宽 | `resolved` | `resolved` | 同 | | |
| b | affinity 严格拒绝不是终态（Q1） | `resolved` | `resolved` | 同 | 23.8s | 23.6s |
| b | 先跑 rebind-evidence 再切无效（Q2） | `unresolved` | `resolved` | **异** | | |
| b | 严格比较读缓存行（Q7） | `resolved` | `resolved` | 同 | | |
| c | 恢复指引未覆盖 llm 漂移协议 | `resolved` | `resolved` | 同 | 17.0s | 16.8s |

合计 **4/6 同**；两组 `status=ok`，`warnings=[]`，`quote_rejected=0`，`model_failures=0`，`calls` = 2/3/1（无 repair 轮）。
旧版 verify 上，group-c 会因「quote 落在 ±60 窗口外」被降级——现在通过严格重锚判对。

## 两处「异」的性质（都不是基础设施问题）

1. **a / affinity（expected `regressed`，actual `resolved`）**：B-lite 已经把回归点送进 prompt
   （`- PinnedChannelAllowed referenced at middleware/distributor.go:158` + `:115-195` 快照，
   覆盖 `:158` 拒绝 → `:174` 回退），模型仍只按「strict 半恢复了」判 resolved，
   **没有识别出「回退会重选刚被拒绝的渠道」这个非确定性交互**（人工是用 `-count=50` 复现 19 次失败才定的）。
   三次真跑（含修复前）结论一致 → 这是**模型能力边界**，不是证据缺失。
2. **b / Q2（expected `unresolved`，actual `resolved`）**：Q2 的边界是「**另一个**已经在 llm 来源的漂移协议」，
   人工是在隔离 PG 上按文档完整操作后仍失败才判未闭合；模型只看到 `migration.go` 的新文案
   （现在文案改成「re-save the protocol being switched」），判定「指引已给出真实恢复步骤」。
   `<fix_scope>` 已列出 `docs/llm-config-release-plan.md` 等文件（该 finding 的实质修复有一部分在文档里），
   但模型没有因此转 `inconclusive`——**文档类修复的判定仍是弱点**。

## 观察到的稳定性问题（已在代码里修掉）

- **首轮真跑**（`a0540df` 之前）：group-a 的第二条 → `inconclusive: model call failed: http 400`（一次，未复现）；
  group-a 第二轮 → `inconclusive: model output could not be parsed: invalid JSON: control character ...`（**稳定复现**：
  模型把带裸换行的代码塞进 JSON `quote`，repair 轮也救不回来）。→ 已由 `9920206` 的宽容解析修掉，
  修复后两条都判出 `resolved`。
- 同一 finding 跨轮次的 `inconclusive ↔ resolved/unresolved` 抖动来自模型自身；报告里的
  `counts/calls/model_failures/quote_rejected` 足以事后归因。

## 复跑方式

```sh
cargo build
scripts/verify-pr400.sh group-a --model deepseek/deepseek-flash
scripts/verify-pr400.sh group-b --model deepseek/deepseek-flash
scripts/verify-pr400.sh group-c --model deepseek/deepseek-flash
```

产物：`target/pr400/<group>/{report.json,result.json,job.json}`；本目录下的同名文件是 2026-11-25 的存档。
