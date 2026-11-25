# Phase 4 增强：verify 并行/fan-out + B-lite 新问题证据 + 降级与真实验证

> 基线 `40a1a80`（`feature/hibot-audit-exec`，已并入 `hibot-executor = ca5d74b`）· 2026-11-25
> 用户 grill 定稿 · 协调者 `01a20ba0` 批准（Phase 4 增强，先于 Phase 6；同分支追加提交）

## 背景（用户提出的四个缺口）

1. Phase 4 缺 grill：verify 的分工/边界没有经过推敲；
2. 缺并行与 fan-out（墙钟是 redteam 必须追求的目标）；
3. 缺「修复是否引入新问题」的视角（当前 `regressed` 只看 finding 自己的文件）；
4. 缺真实验证（只有合成夹具）。

## 决定性事实（已核 hibot 文档 + 真实 PR）

- `flow.md:24,93`：verify 只消费 **review 的 blocking findings**；triage 的修复 = 每个 head 重跑 triage（`:18`）。
- `flow.md:91-92`：fix job 只改 findings 命中文件；**CI 先绿才派 verify**（编译类回归由 CI 挡）。
- `flow.md:96`：任一 `inconclusive` ⇒ Run `incomplete`（阻塞合并、等下一轮）——假 inconclusive 的成本是一整轮。
- 契约 §3.3：`verifications[]` 只能按 `finding_fingerprint` 挂；triage 的 intent blocker 无 file/line/quote（quote 必填）→ 结构上无法表达。
- `hiapi-backend#400`（已合入，4 提交 / 4 轮人工评审，commit 本机可得）推演：现实现会**漏掉**第 2 轮人工抓到的真实回归
  （finding 锚 `execution.go:256`，回归在 `distributor.go:158→174`；字面 related 线索不命中）。

## 范围（六段，每段可编译可测）

| # | 内容 | 验收 |
|---|---|---|
| 1 | **并行 + 观测**：两阶段（prepare 同步装配 → dispatch 只发 LLM）；`buffer_unordered(k)`，`k = min(Σ providers.<p>.maxConcurrency, n)`；**provider 门是硬上限**；接 `AttemptPolicy`（心跳 `in_flight`/`retrying` 准确、429 收缩门、`rate_limit_retries` 进 extensions）；per-item 预算按规模（`budget.rs`）；latency 记录 | 并发峰值 ≤ Σ 帽且 ≤ 每组 provider 帽；`state.json.llm.in_flight` 在调用期间 > 0；`retrying` 可见；报告顺序仍 = 输入顺序；取消后已完成保留、其余 inconclusive |
| 2 | **B-lite 新问题证据**：`<symbol_blast_radius>` = 其它文件 hunk 中引用了「本文件被改符号名」的段 + 该 hunk ±40 行快照（符号集 `diffsym::changed_defs` + `ref_worthy`） | #400 A→B 组的真实回归被这份证据覆盖（夹具断言 prompt 含该段）；无关文件不入选 |
| 3 | **降级策略**：quote 先「严格字面重锚」——命中 fed material 或**整个 post-fix 文件**都算通过（命中文件时把 quote 归一为检出原文）；仍不命中才降级。模型输出解析失败加**一次** repair 轮；重试预算不变 | 反例：只改内部空白仍失败（不得重锚成别的代码）；「窗口外的逐字 quote」不再被误降级；repair 成功后状态照常 |
| 4 | **真实验证夹具**：`tests/fixtures/verify/pr400/` 三组（A→B 回归、B→C 半 resolved+一条 unresolved、C→D 文档类），`expected.json` 写人工判决；`scripts/verify-pr400.sh` 生成 job 并跑；不进 `cargo test`；另有夹具完整性单测 | 三组 findings 的指纹 = `fingerprint::of(file,quote,title)`（单测复算）；expected 覆盖 findings 集合；脚本能对任意 `HIBOT_BACKEND_REPO` 工作 |
| 5 | **真跑记录**：#400 三组用 `deepseek-flash` 真跑，逐条对拍（同/异/无法判定），日志与结果落任务 evidence | 结果文件含每条 expected/actual/latency；差异逐条说明；不伪造判决 |
| 6 | **文档口径**：verify 作用域（anchored finding；triage=重跑）、并行/预算、与 review `carry` 两套口径差异 | README 相应小节 + 代码注释 |

## 约束（协调者明确）

- **fingerprint 只从输入文件取**，不从其它字段复算/映射（写进 PRD）。
- `verify.findings[]` **保持空数组**；整 fix diff 回归 sweep + 新问题落点属契约扩展，本轮不做（future）。
- **保持 4.0 隔离**：`fixverify` 零 `consolidate` 引用；新增证据段不得引入 refute 模板/解析。
- 真跑：3 组 × ≤3 条 finding；单条 ≤240s；总墙钟只记录不设 KPI；不伪造人工判决。
- 不动 review 侧 `carry`；不改 `dev`；不重建入库二进制。

## 提交切分

1. `feat(fixverify): 两阶段 prepare/dispatch + 受限并发 + provider 门计量`
2. `feat(fixverify): 变更符号的跨文件证据（B-lite）`
3. `fix(fixverify): quote 严格重锚 + 解析失败一次 repair`
4. `test(verify): hiapi-backend#400 三组真实夹具 + 脚本`
5. `chore(verify): #400 真跑记录与对拍`
6. `docs(verify): 作用域/预算/与 carry 口径差异`
