# Phase 5 · `out/state.json` 心跳（5.1–5.3）

## 背景

hibot audit 执行单元接入的 U5 单元（协调者 session `01a20ba0` 派发；本 session 为 Phase 5 并行支）。
计划权威：`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §8 Phase 5 / §12 Q15·Q17 / 附录 F（P12、D72）；
契约权威：`~/hibot/.docs/audit/contract.md` §3.6 + `schema/audit-state.schema.json`（**只读**）。

前置：分支 `feature/hibot-heartbeat` @ `cd047e9`；**不 rebase、不合入 `hibot-executor`**（协调者负责）。

## 目标

job 模式运行期间周期落 `out/state.json`，供 hibot runner 做接纳刹车（D68）。本地 review 不写。

## 范围

| 项 | 内容 | 落点 |
|---|---|---|
| 5.1 | `InflightGate` 增 `inflight()` / `waiting()` / `retrying()`；429 退避中计数（取消安全）；`ProviderRegistry::snapshot()`；`providers[].name` = 配置 provider key；`llm.max_concurrency` = Σ provider 帽；`extensions.redteam.concurrency={mode:"per-provider",provider_caps:{...}}` | `src/provider/registry.rs` |
| 5.2 | 心跳任务：≤5s 原子覆写（`atomic_write_json`）；`phase` = `preparing → running → finishing`（不写终态）；`progress{done,total}`（review `unit=cells`；triage/verify `unit=items` 但不实现分析逻辑）；首帧 `preparing`；退出后最后一帧非终态 | `src/job/state.rs`、`src/job/run.rs`、`src/job/lifecycle.rs` |
| 5.3 | 缺失/过期语义：停更不影响正确性；退出后不再写；文件被删不 panic | 同上 |

## 计数口径（硬约束）

- `llm.in_flight` = Σ `InflightGate.active`（**唯一来源**）；`queue::inflight_count()` 是队列项，`FakeChatBackend` 的计数是测试替身，均不进心跳。
- 429 退避中：`retrying > 0`，且**不计入** `in_flight`。
- `llm.max_concurrency` = Σ provider 配置帽（`cap_initial`；实际生效口径）。
- `providers[].name` = 配置 provider key（不是 apiType、不是 model id）。
- 心跳值与 `ProviderRegistry::snapshot()` 逐字一致（同一函数产出口径）。

## 验收（自证）

1. 并发 2 → `in_flight==2`；
2. 429 退避中 `retrying>0` 且不计入 `in_flight`；
3. `providers[].name` 是配置 key；
4. 心跳值与 gate 快照逐字一致；
5. 采样三次 `at` 递增且间隔 ≤5s；
6. 三 kind 的 phase 都在三值内、首帧 `preparing`；
7. 退出后最后一帧非终态、停写；
8. 文件被删不 panic；
9. `state.json` 走 `*.tmp` → rename。

## 禁区

- 不碰 Phase 3/4 的 `src/triage/`、`src/fixverify/`（可能并行有别的 session）
- 不改 `dist/config.schema.json`；不动 capabilities
- `~/hibot` 只读；不改 `dev`；不重建入库二进制；报告/state 不含 cost
- 不直挂 `CellSink::on_phase`（只吃内部 `Phase` 枚举；triage/verify 无变体）

## 流程

新建 Trellis 任务；Conventional Commits；5.1 完成给 3–5 行中途简报；完整回报附提交、逐条证据、`make verify` + `cargo test` 原文、`diff --stat`、是否 push。
