# Phase 5 · 心跳逐项验收对照（U5）

受测提交：`33f5248`（job 心跳收口）+ `b4e04c2`（registry retrying 回归），基线 `cd047e9`。
验证环境：worktree `~/hobby/.worktrees/redteam/hibot-heartbeat`，最终代码工作树 clean。

## 命令（原始日志与退出码见 `evidence/`）

| 命令 | 退出码 | 原始日志 |
|---|---|---|
| `make verify` | 0（`evidence/make-verify.exit`） | `evidence/make-verify.log`（gen/schema 无 diff；packaging 7 passed） |
| `cargo test` | 0（`evidence/cargo-test.exit`） | `evidence/cargo-test.log`（1090 + 21 + 4 + 25 + 7 + 24 passed, 0 failed） |

`cargo test` 各测试目标的结果行：

```
test result: ok. 1090 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 25 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

（另有一个 0 测试的 doctest 目标同样 ok。）

## 逐条证据

| # | 验收 | 证据（测试 / 断言） |
|---|---|---|
| 1 | 并发 2 → `in_flight==2` | `job::state::tests::llm_snapshot_matches_registry_while_two_requests_are_in_turn`：两个可控 backend 把真实 `registry.scan()` 请求**停在 turn 内**，断言 `snapshot.in_flight == 2`，并比较落盘 `state["llm"]` 与 `registry.snapshot()`；`provider::registry::tests::snapshot_observes_real_scan_inflight_and_retry_backoff`：真实 scan 路径的 429/在途组合 |
| 2 | 429 退避中 `retrying>0` 且不计入 `in_flight` | 同上：释放 429 方后进入**稳定退避**，断言 `(in_flight, retrying) == (1, 1)` 并再次逐字比较落盘 llm 与快照；`retrying_counts_backoff_waiters_without_inflight_and_cancel_is_clean`：取消后 `retrying==0`；`retrying_guard_does_not_double_decrement_after_admission`：两个等待者，一人准入后另一个仍为 1，已准入 guard drop 不重复减 |
| 3 | `providers[].name` = 配置 provider key | registry 单测 `names == ["p","q"]`；`tests/job_conformance.rs::heartbeat_state_file_*`：`providers[0].name == "p1"` |
| 4 | 心跳值与 gate 快照逐字一致 | 真实在途（`in_flight==2`）与稳定退避（`(1,1)`）两个状态都断言 `state["llm"] == serde_json::to_value(registry.snapshot())`；`snapshot_of_gates()` 每个 gate 一次 `load()` 同锁读全字段，心跳与 registry 共用该函数 |
| 5 | 采样三次 `at` 递增且间隔 ≤5s | `at_advances_within_five_seconds`：每次等下一次落帧后采样，按完整 epoch 秒比较；`at_parser_handles_midnight`：跨午夜/跨年差值正确，不下溢 |
| 6 | 三 kind phase 在三值内、首帧 `preparing` | `first_frame_is_preparing_and_atomic`（`start` 同步首帧 `preparing`）；`phase_monotonic_and_shutdown_stops_writing` + `set_phase_finishing_does_not_skip_running`（单调、不跳段）；`triage_and_verify_write_three_value_heartbeat` + `heartbeat_state_file_*`（review/triage/verify 终帧均 `finishing`） |
| 7 | 退出后最后一帧非终态、停写 | 终帧 `phase=="finishing"`（非 `done`/`fail`）；集成测试等 2.3s 后 `state.json` 字节不变；`shutdown` 在写锁内推进阶段、写最后一帧并置 `stopped`，随后 join 周期线程；`Drop` 也持写锁置 `stopped` 并 join |
| 8 | 文件被删不 panic | `deleted_state_file_is_recreated_without_panic`：删除后下一帧重建，无 panic；写失败静默 |
| 9 | `state.json` 走 `*.tmp`→rename | 单测/集成均断言无 `state.json.tmp` 残留；写入走 `io::atomic_write_json`（既有 `atomic_write_leaves_only_complete_file`） |
| 附 | 同步准备期不饿死心跳 | `periodic_tick_survives_long_blocking_preparation`：`current_thread`（单 async worker）+ 同步阻塞 5.2s，心跳线程仍落 ≥2 帧，`at` 间隔 ≤5s（心跳用独立 OS 线程，不依赖 runtime worker） |
| 附 | progress 集合与去重 | `job::lifecycle::tests::progress_counts_only_manifest_cells`：深挖 cell 不推进；同一 scan cell 重复完成通知只 +1；第二个 scan 完成才到 `total`；`total` 用去重 id 数 |

## 边界与未覆盖项（如实披露）

- **首帧起点**：心跳从 **manifest 解析成功 + `paths.out` 校验/创建之后** 起手（`Heartbeat::start` 需要可信的 `job_id` 与输出位置）。读取 manifest 之前没有可信身份/输出路径，不提前写、不猜路径。
- **triage 不写 progress**：`job.triage` 的 manifest 没有可数的 item 清单（`diff_patch`/`pr_context` 不是 item），不伪造 `progress`；verify 写 `{"unit":"items","total":findings 数,"done":0}`（分析未实现）。
- **llm 段**：仅 review（registry 构造成功后绑定）；triage/verify 无 registry，不写 `llm`。
- **本地 review**：不写 `state.json`（只 `job::run::run_with` 启动心跳）。
- **线程模型**：心跳是独立 OS 线程（100ms 停止轮询、2s 落帧），不占用也不依赖 tokio worker；`shutdown`/`Drop` 会 join 线程，返回后不再写。
- 未改 `dist/config.schema.json`、capabilities、`src/triage/`、`src/fixverify/`；未 push、未 rebase。
