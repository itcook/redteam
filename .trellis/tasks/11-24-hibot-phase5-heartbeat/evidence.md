# Phase 5 · 心跳逐项验收对照（U5）

受测提交：`92e859b`（job 心跳）+ `f52dd27`（registry 读数），基线 `cd047e9`。
验证环境：worktree `~/hobby/.worktrees/redteam/hibot-heartbeat`，工作树 clean。

## 命令

```
make verify      → exit 0（gen/schema 无 diff；packaging 7 passed）
cargo test       → 1083 + 21 + 4 + 25 + 7 + 24 passed, 0 failed
```

## 逐条证据

| # | 验收 | 证据（测试 / 断言） |
|---|---|---|
| 1 | 并发 2 → `in_flight==2` | `provider::registry::tests::snapshot_counts_active_requests_by_provider_key`：两 provider 各 `attempt_begin` → `snapshot().in_flight==2`，释放后 0；`job::state::tests::llm_section_matches_registry_snapshot_and_discloses_concurrency`：帧内 `llm.in_flight==2` |
| 2 | 429 退避中 `retrying>0` 且不计入 `in_flight` | `retrying_counts_backoff_waiters_without_inflight_and_cancel_is_clean`：`retrying==1`、`inflight==0`，取消后 `retrying==0`；`llm_section_*`：另一条在途 `in_flight==1`、`retrying==1` |
| 3 | `providers[].name` = 配置 provider key | registry 单测 `names == ["p","q"]`（不是 `p/a` 之类 route_key）；`tests/job_conformance.rs::heartbeat_state_file_*`：`providers[0].name == "p1"` |
| 4 | 心跳值与 gate 快照逐字一致 | `llm_section_*`：`state["llm"] == serde_json::to_value(registry.snapshot())`；两者共用 `snapshot_of_gates()` |
| 5 | 采样三次 `at` 递增且间隔 ≤5s | `job::state::tests::at_advances_within_five_seconds`：2.1s 采样 ×3，字符串递增、秒差 ≤5 |
| 6 | 三 kind phase 在三值内、首帧 `preparing` | `first_frame_is_preparing_and_atomic`（`start` 同步首帧 `preparing`）；`phase_monotonic_and_shutdown_stops_writing`（单调、不跳段）；`triage_and_verify_write_three_value_heartbeat` + `heartbeat_state_file_*`（review/triage/verify 终帧均 `finishing`） |
| 7 | 退出后最后一帧非终态、停写 | 单测与集成测试均断言终帧 `phase=="finishing"`（非 `done`/`fail`）；集成测试等 2.3s 后 `state.json` 字节不变；`shutdown` 在写锁内二次检查 `stopped` |
| 8 | 文件被删不 panic | `deleted_state_file_is_recreated_without_panic`：删除后下一帧重建，无 panic |
| 9 | `state.json` 走 `*.tmp`→rename | 单测/集成均断言无 `state.json.tmp` 残留；写入走 `io::atomic_write_json`（既有 `atomic_write_leaves_only_complete_file`） |

## 范围说明

- 心跳只在 `job::run::run_with`（job 模式）启动；本地 review 无调用点。
- review `progress.unit=cells`，`done` 为 scan cells 完成数（深挖 cell 饱和到 `total`）；verify `unit=items`、`total=findings` 条数、`done=0`（分析未实现，不伪造进度）；triage manifest 无 item 清单，不写 `progress`。
- `llm` 段在 registry 构造后绑定；triage/verify 无 registry，不写 `llm`。
- 未改 `dist/config.schema.json`、capabilities、`src/triage/`、`src/fixverify/`；未 push、未 rebase。
