# implement · D13 provider 门唯一闸

> 顺序执行，每步一 commit（简体中文 `type(scope): 摘要——细节`，尾行 `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`），绝不 push；三道门每 commit：`cargo test`（lib + e2e）、`cargo clippy --all-targets`（不高于基线行数）、`make verify`。决策编号见 `prd.md` K1-K12，定论见 `design.md`。直接在 `feat/m21-review-engine` 上做（K12）。

## 0. 预飞
- [ ] `git status --short` 空（除本任务目录）、`git branch --show-current` = `feat/m21-review-engine`、HEAD = 84dee09。
- [x] 基线（2026-09-09 13:00，HEAD 84dee09）：lib 533 / e2e 17；clippy 7 行；`make verify` 绿。
- [ ] `python3 ./.trellis/scripts/task.py start d13-provider-gate` + `set-branch feat/m21-review-engine`（仅在用户批准规划摘要后）。
- [ ] Commit：`chore(task): d13-provider-gate 规划制品（prd / design / implement / jsonl）`。

## 1. 配置 + 扇出 + cli + help（R1 / R2 / R5；`src/config/mod.rs` / `src/orchestrate/mod.rs` / `src/cli/mod.rs` / `src/main.rs` / `src/state/mod.rs`）
- [ ] config：删三 `workers` 字段与 `d_workers_*`、`Default` 同步、`validate` 列表去三条、`REVIEW_SECTIONS` 去 `workers`、shape_guard 专属 warning（K7）、`default_concurrency` 10、`> 128` warning（K2）、字段注释。
- [ ] config 测试：样例 JSON 同步；`defaults_identity_family_window` 改断言；零值用例改 `samples: 0`；新增 `removed_workers_key_warns_without_unknown_noise`、`huge_max_concurrency_warns`。
- [ ] orchestrate：`ScanFanoutInput` 去 `workers`；删信号量；告警按 provider 计（需 registry `provider_of` / `cap_of`——本步先加这两个只读接口，门重写在步 2）；三处注释。
- [ ] orchestrate 测试：`input()` 去 `workers`；大格先发用例 cap 1 + 新文案；`scan_fanout_provider_cap_bounds_inflight`；其余去参数。
- [ ] cli：`:288` 删传参；`state/mod.rs:171-172` 注释；`main.rs:33-34` 并发句 + `:447-450` 注释。
- [ ] 门：`cargo test`、clippy、`make verify`；`./plugins/redteam/bin/darwin/aarch64/redteam-next review --help | grep -c "review.scanners.workers"` = 0（先 `make build`）。
- [ ] Commit：`refactor(config,orchestrate): 删角色 workers 三键与扇出信号量——唯一闸 = provider 门，排队告警按 provider 计，review.maxConcurrency 缺省 10 + >128 warning（design v10 §12.3〔D13〕）`

## 2. provider 门重写 + 429 排队重试 + RunMeta（R3 / R4；`src/provider/{mod,registry,scan_shot}.rs` / `src/domain/report.rs` / `src/cli/mod.rs` / `src/orchestrate/mod.rs`）
- [ ] `provider/mod.rs`：`AttemptPolicy`、`retry_classified_with`、`timed_turn_with` / `timed_turn_final_with`；原函数转调 `None`（`single_shot` / `tool_loop` 调用点零改动）。
- [ ] `scan_shot.rs`：`run_scan_shot_with`；原函数转调。
- [ ] `registry.rs`：`InflightGate` 自管（design §2.1-2.2）、`GateAttempts`（§2.4，`backoff_base` 可注入）、`GatePermit`、`GateStat`、`gate_stats` / `rate_limit_meta` / `gate_warnings`、`#[cfg(test)] shrink_to`；删 `ActiveGuard` / `activate`；`ScanLease::run` 传策略；去 `Semaphore` import。
- [ ] `report.rs`：`RateLimitMeta` + `RunMeta.provider_rate_limit`；v4 默认键测试 + round-trip。
- [ ] cli：`ScanFinish.rate_limit`、两处调用、RunMeta 赋值、resume 路径 `gate_warnings`；orchestrate 扇出末尾 `out.warnings.extend(registry.gate_warnings())`。
- [ ] registry 测试：R3 ①②③（① 两 backend 分离：`p/b` repeating OK `with_delay(200ms)` ×2 先发、20 ms 后发 `p/a` new_results([429, OK])；② `GateAttempts::with_backoff_base(1ms)` 直接配 `retry_classified_with` 恒 429 闭包；③ `shrink_to(1)` + `timeout(50ms, enter())` 断言 pending）。
- [ ] 门：全量绿；clippy；`make verify`。
- [ ] Commit：`feat(provider): InflightGate 自管计数 + Notify，429 排队重试——门宽收敛到网关真实上限、持名额等本 provider 下一完成或 Retry-After、独立计数；RunMeta.provider_rate_limit（design v10 §12.3〔D13〕）`

## 3. 真跑核验（AC5）+ 文档收口（R6）
- [ ] `make build`；DeepSeek 单 scanner、`--format json` 跑本仓 dogfood 或 PR181（记录只进 `.next/private/m21a/` 或 `.next/private/d13/`）：`run.warnings` 无排队告警、`queue_wait_ms` ≈ 0、cell 零失败。
- [ ] 临时配置该 provider `maxConcurrency: 2` 再跑：排队告警在场、被排队格 `queue_wait_ms > 0`、`provider_inflight_peak == 2`；跑完还原配置。
- [ ] `.next/roadmap/followup.md`「`scanners.workers` 形态」行归属列追加「✅ M21a 收口批 <两个 commit 短 sha>」。
- [ ] Commit：`docs(roadmap): D13 收口批落地记录——followup 行归位、真跑核验数字`

## 4. 评审修
- [ ] code-review 子 agent（high）审步 1-2 的 diff（SendMessage 钉范围 = 这两个 commit）；修 → 三道门 → Commit：`fix(provider,config): D13 收口批评审修——<条目>`。
- [ ] `python3 ./.trellis/scripts/task.py finish`；交接写 `~/hobby/redteam/.remember/remember.md`。

## 验证命令
```
cargo test --lib 2>&1 | grep -E "^test result"
cargo test --test review_e2e 2>&1 | grep -E "^test result"
cargo clippy --all-targets 2>&1 | grep -c "^warning\|^error"
make verify
make build && ./plugins/redteam/bin/darwin/aarch64/redteam-next review --help | grep -n "maxConcurrency"
git grep -n "workers" -- src/
```

## 风险文件 / 回滚点
- `src/provider/registry.rs`（门重写，并发正确性）：单测 ①②③ 是回归门；出问题 revert 步 2 的 commit，步 1 独立可用（门仍是旧 Semaphore 语义）。
- `src/config/mod.rs` 形状守卫：改错会把合法配置判错——`legacy_shapes_point_to_d12` 与 `review_risk_and_roleless_model_warn` 必须仍绿。
- `src/provider/mod.rs` 重试循环：`policy = None` 路径必须与旧 `retry_classified` 逐行为等价（`single_shot` / `tool_loop` 既有测试兜底）。
