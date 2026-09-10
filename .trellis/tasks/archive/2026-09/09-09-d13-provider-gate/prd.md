# D13 并发形态：删角色 workers、provider 门唯一闸 + 429 排队重试（scanner 路径）

> 上位：design v10 `.next/design/2026-09-04-redteam-review-engine-architecture-design.md` §4 / §6 规则 7 / §12.1 / §12.3 / §17 R9 · R16；决策 D13（`.next/decisions/…-decisions.md`，Q1-Q5）；roadmap `.next/roadmap/followup.md`「`scanners.workers` 形态」行（已裁，代码分两步：本任务 = M21a 收口批，只动 scanner 路径；队列侧五态与 agreement 两字段随 M21b）。
> 技术设计 `design.md`，执行清单 `implement.md`；本文 §Key Decisions 的 K 编号被两者引用。
> 分支：直接在 `feat/m21-review-engine`（工作树 `~/hobby/.worktrees/redteam/m21-review-engine`）上做，不另开子分支（单 session、无并行批次；文档 commit 84dee09 已在此分支）。绝不 push。
> 收口：不跑 ship 评审（memory `no-ship-review-until-core-stable`），code-review 子 agent 审核 → 修 → commit。

## Goal

把 D13 的并发形态落进 `redteam-next` 的 scanner 路径：角色级 `workers` 从配置、编排与文档面整体消失，provider 在途门成为唯一一道闸；
门在撞 429 时自动收敛到网关真实上限并让撞门的请求排队重试，而不是 1.2 s 内烧掉 cell；排队与收敛在 run.warnings / RunMeta 可见。
用户价值：PR181 7 格不再因缺省 4 排队 100-130 s；配置面少三个无意义的数字；误配超大 `maxConcurrency`（如把 maxOutputTokens 填进去）不会导致整批 cell 失败。

## Background（仓库实测 2026-09-09，HEAD 84dee09；行号以此 HEAD 为准）

- **两道闸现状**：provider 门 `InflightGate { sem: Semaphore, current, peak }`（`src/provider/registry.rs:14-48`），`lease` 先过门（`:116-123`），`ScanLease::run` 持 `_permit` 并 `activate()` 计峰值后调 `run_scan_shot`（`:139-142`）；角色信号量 `Arc<Semaphore>` 在扇出层（`src/orchestrate/mod.rs:222`），门后取许可（`:242-247`），`queue_wait_ms` = 两道门都过后的时长（`:245`）。角色信号量是**跨 provider 全局一把**：deepseek + qwen 双 scanner 14 格被同一个 4 卡住。
- **排队告警按角色 workers 计**（`src/orchestrate/mod.rs:265-272`），文案指向 `review.scanners.workers`。
- **配置**：`ScannersCfg.workers` / `ReviewersCfg.workers` / `VerifiersCfg.workers`（`src/config/mod.rs:261-298`）、缺省函数 `d_workers_4` / `d_workers_2`（`:336-337`）、`default_concurrency() = 8`（`:372`）、`validate` 的 ≥ 1 列表含三条 workers（`:669-682`）、形状守卫已知键表 `REVIEW_SECTIONS`（`:705-711`）、review 段检查循环（`:778-788`）；已知层级下的无定义键只出 warning（`:713-723`）。design §12.1 样例 JSON 在测试里（`:1075-1078`）；`defaults_identity_family_window` 断言 `scanners.workers == 4`（`:1118`）；`validate_aggregates_kind_window_and_zero_knobs` 用 `workers: 0` 验零值（`:1142-1143`）。`from_json_warned`（`:494-514`）是 warning 出口。
- **cap 装配**：`build_registry` 取 `em.max_concurrency.unwrap_or(cfg.review.max_concurrency)`（`src/main.rs:470`）；`insert_scan` 同 provider 首个 cap 建门（`src/provider/registry.rs:84-96`）。
- **429 现状**：`ProviderError::RateLimited { retry_after }`（`src/provider/mod.rs:23`，SSE overloaded 也归此变体 `src/provider/backends.rs:1234`）；`retry_classified`（`:179-202`）预算 `DEFAULT_RETRY_BUDGET = 2`（`:161`），退避 Retry-After 封顶 30 s（`:174`）或 400 / 800 ms（`:172-173`）；`timed_turn` / `timed_turn_final`（`:211-241`）被 `scan_shot.rs:49,73`、`single_shot.rs:39,59`、`tool_loop.rs:102,131,169,181` 调用。重试期间持门名额；占网关槽位的是分钟级生成请求，1.2 s 内重试必再撞 → cell 失败 → scanner 失败 = 硬 Incomplete。
- **RunMeta**：`provider_inflight_peak: BTreeMap<String, u32>`（`src/domain/report.rs:251-253`，skip 空），v4 字段默认不产键的测试（`:403-425`）；cli 装配 `inflight_peak: registry.inflight_peaks()` 两处（`src/cli/mod.rs:321`、`:940`），`ScanFinish`（`:332-349`），RunMeta 赋值（`:460`）；resume 路径 warnings 起点 `:868`。
- **扇出输入**：`ScanFanoutInput.workers`（`src/orchestrate/mod.rs:105`），cli 传 `cfg.review.scanners.workers`（`src/cli/mod.rs:288`）；测试 helper `input(..., workers, samples)`（`src/orchestrate/mod.rs:459-461`）；大格先发测试用 `workers = 1` 串行化并断言旧告警文案（`:463-486`）；两层并发测试（`:540-555`）。
- **help**：`review` 子命令 doc 并发句（`src/main.rs:33-34`）；`build_registry` 注释（`:447-450`）；`CellRecord.queue_wait_ms` 注释提「角色 worker 许可」（`src/state/mod.rs:171-172`）。
- **测试替身**：`FakeChatBackend::new_results` 按序 pop（跨并发调用共用队列）、`repeating`、`with_delay`、`peak_inflight`（`src/provider/tool_loop.rs:270-360`）；tokio features 含 `sync` / `time`，无 `test-util`（`Cargo.toml:16`）。
- **不在本任务**：verifier 的 `Semaphore(max_concurrency)`（`src/consolidate/verify.rs:268-292`，M21b 改为流消费者时接门）；`single_shot` / `tool_loop` 的 429 行为不变（M21b reviewer 接线时再挂策略）。
- **本机配置**：scanner 在 deepseek（`maxConcurrency` 16）/ qwen（20），reviewer / verifier 在 agc（20）；`review` 节全缺省。
- 基线（HEAD 84dee09）：`cargo test` lib 533 / e2e 17 绿；clippy 7 行；`make verify` 绿。

## Requirements

### R1 配置：删三键、缺省 10、两条 warning（K1 / K2 / K7）
- 删 `ScannersCfg.workers` / `ReviewersCfg.workers` / `VerifiersCfg.workers` 字段与 `d_workers_4` / `d_workers_2`；`Default` 同步；`validate` ≥ 1 列表去三条；`REVIEW_SECTIONS` 已知键去 `workers`。
- 形状守卫：`review.{scanners|reviewers|verifiers}.workers` 在场 → 一条专属 warning「已删除（D13）：并发只看 provider `maxConcurrency`（缺省 `review.maxConcurrency` = 10），已忽略」，**不再**叠加通用「无定义」warning（K7）。
- `default_concurrency()` 8 → 10；字段注释改「全局在途上限缺省；provider 级缺省继承；唯一一道闸（D13）」。
- `from_json_warned`：任一 `providers.X.maxConcurrency > 128` 或 `review.maxConcurrency > 128` → warning「像是填错字段（maxOutputTokens？）；撞 429 时门会自动收敛，但建议改回」；只警不拒（K2）。
- 测试：样例 JSON 与 design §12.1 同步（删三键、`maxConcurrency: 10`）；`defaults_identity_family_window` 改断言 `review.max_concurrency == 10`；零值用例改 `scanners.samples: 0` → 错误含 `review.scanners.samples`；新增：含 `workers` 键 → 解析成功且 warning 含「已删除（D13）」且不含「无定义」；`maxConcurrency: 65536` → warning 含「像是填错」。

### R2 扇出：删角色信号量、排队告警按 provider 计（K3 / K4）
- `ScanFanoutInput` 去 `workers`；`run_scan_fanout` 只过 `registry.lease`，`queue_wait_ms` = 取到门名额的等待；派发序（diff 字节降序）不变——`join_all` 首轮 poll 序 = 门 FIFO 序，cell 数 > 门宽时仍决定谁先发。
- 排队告警：按 provider 统计路由到它的 cell 数 `n`（`registry.provider_of(route_key)`），门宽 `cap`（`registry.cap_of(provider)`）；`n > cap` → `scanner cells {n} on provider {p} > maxConcurrency {cap}: {n-cap} cell(s) queued (wall = queue wait + slowest cell; raise providers.{p}.maxConcurrency)`；未注册条目不计。
- 扇出结束后 `out.warnings.extend(registry.gate_warnings())`（429 收敛告警，R3）。
- cli：删 `workers:` 传参；`ScanFinish` 加 `rate_limit: BTreeMap<String, RateLimitMeta>`，两处调用传 `registry.rate_limit_meta()`；resume 路径在 join_all 后 `warnings.extend(registry.gate_warnings())`。
- `CellRecord.queue_wait_ms` 注释改「provider 在途门等待（无角色 worker，D13）」。
- 测试：`input()` helper 去 `workers`；大格先发用例改 `insert_scan(.., cap = 1, ..)` 串行化、断言新文案 `scanner cells 3 on provider p > maxConcurrency 1: 2 cell(s) queued`；两层并发用例改 `scan_fanout_provider_cap_bounds_inflight`（cap 1 → 峰值 1；cap 2 → 峰值 2）；其余用例去 `workers` 参数、断言不变。

### R3 provider 门：自管计数 + Notify、429 排队重试（K5 / K6 / K8 / K9）
- `InflightGate` 换 `Mutex<GateState { cap_initial, cap, held, active, rate_limited }>` + `slot_free: Notify`（名额释放唤醒一个等门者）+ `completed: Notify` + `completions: AtomicU64`（非 429 的请求结束）+ `peak: AtomicU32`；`enter()` = `held < cap` 即取，否则等 `slot_free`；名额 Drop：`held -= 1`，仅 `held < cap` 时 `notify_one`（门收窄后多余名额释放即丢弃，K6）。
- `provider::AttemptPolicy`（`attempt_begin` / `attempt_end(rate_limited)` / `async on_rate_limited(retry_after) -> bool`）+ `retry_classified_with(budget, policy, op)`（有策略时 429 走 `on_rate_limited`：true 重试且**不计** `budget`，false 原错返回；其余可重试错照旧）；`timed_turn_with` / `timed_turn_final_with`；原三个函数保留、转调 `None`。`scan_shot::run_scan_shot_with(.., policy)`，原函数转调 `None`。
- `registry::GateAttempts { gate, retries: AtomicU32 }` 实现策略，`ScanLease::run` 每次 run 新建一个并传入：
  - `attempt_begin`：`active += 1`，峰值取 max（`inflight_peaks()` 语义不变 = 同时在途 HTTP 请求数）。
  - `attempt_end(rl)`：`active -= 1`；`!rl` → `completions += 1`、`completed.notify_one()`；`active == 0` → `completed.notify_waiters()`（防全员 429 时互相等死，K8）。
  - `on_rate_limited(retry_after)`：`rate_limited += 1`；`others` = 此刻 `active`（本请求已结束）；`cap ← max(1, min(cap − 1, others))`，只降不升（K5）；`retries += 1`，`retries > max(4, cap × 2)` → 返回 false（K9）；`others == 0` → `sleep(retry_after.unwrap_or(500 ms × 2^(retries−1)，封顶 30 s))`；否则记 `seq0 = completions`，循环等 `completed`（先建 `notified()` 再检查）直到 `completions > seq0` 或 `active == 0`（后者转 sleep 退避），并与 `retry_after` 的 sleep 取晚者；返回 true。
- registry 新接口：`provider_of(route_key) -> Option<&str>`、`cap_of(provider) -> Option<u32>`、`gate_stats() -> BTreeMap<String, GateStat { cap, effective_cap, rate_limited, peak }>`、`rate_limit_meta()`（只含 `rate_limited > 0` 的 provider）、`gate_warnings()`（每个 `rate_limited > 0` 的 provider 一条：`provider {p}: 429 x {n}, maxConcurrency {cap} -> {eff} (gate shrank to the gateway's real limit this run; set providers.{p}.maxConcurrency <= {eff})`）；`inflight_peaks()` 保留。
- 删 `ActiveGuard` / `activate`；`tokio::sync::Semaphore` 不再由 registry / orchestrate 引用。
- 测试（registry）：① 门宽 3，`p/b` 两格慢 OK 先发、`p/a` 首发 429 后 OK → 三格全 Ok、`rate_limited == 1`、`effective_cap == 2`、`p/a` 等到了一次完成（耗时 ≥ 慢格延迟）而非立即重试；② 门宽 1、只有一个请求且恒 429（退避基数注入为 1 ms）→ 重试 `max(4, 2) = 4` 次后 `Err(RateLimited)`、`rate_limited == 5`、`effective_cap == 1`；③ 门宽 3 取满后 `#[cfg(test)] shrink_to(1)`，释放两个名额时第四个 `enter()` 仍 pending（`timeout` 断言），释放第三个才进；④ 既有两测（未注册 Unavailable、同 provider 共享门峰值）不变。

### R4 RunMeta：`provider_rate_limit`（K10）
- `domain::report::RateLimitMeta { hits: u32, cap: u32, effective_cap: u32 }`；`RunMeta.provider_rate_limit: BTreeMap<String, RateLimitMeta>`（`#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`）；`schema_version` 仍 `"4"`（纯增字段、空时不产键）。
- 测试：v4 默认态键清单加 `provider_rate_limit`；赋值 round-trip。

### R5 help 与代码内文档
- `src/main.rs:33-34` 并发句重写：「并发 = 各 provider `maxConcurrency`（缺省继承 `review.maxConcurrency` = 10；无角色级 workers，D13）：路由到同一 provider 的 cell 数超过其上限即排队，墙钟 = 排队 + 最长格（run.warnings 提示；每 cell `queue_wait_ms` 可读）；撞 429 时门宽自动收敛到网关真实上限并排队重试（run.warnings / run.provider_rate_limit 可读）」；`build_registry` 注释、orchestrate 三处并发注释（`:94-95` / `:175-177` / `:223-225`）、`state/mod.rs:171-172` 同步。

### R6 收口
- 三道门每 commit：`cargo test`（lib + e2e）、`cargo clippy --all-targets`（不高于基线行数）、`make verify` 绿。
- `.next/roadmap/followup.md`「`scanners.workers` 形态」行归属列追加「✅ M21a 收口批 <commit>」；`.next/roadmap/M21a-scan-foundation.md` 状态句已含 D13 收口批，不再改。
- code-review 子 agent（high）审 diff → 修 → 单独 commit（`fix(...)` 前缀）。

## Key Decisions

| K | 定案 | 依据 |
|---|---|---|
| K1 | 三个 `workers` 键一起删，旧配置只 warning 不拒 | D13 Q3；形状守卫既有「未知键 warning」语义 |
| K2 | `review.maxConcurrency` 缺省 10；`> 128` 只警不拒 | D13 Q5；R9 实测前不拍更大的数 |
| K3 | 无 `--workers`、无任何单次节流手段 | D13 Q1；跨进程超发靠 429 排队重试兜 |
| K4 | 排队告警按 provider 计、文案指向 `providers.{p}.maxConcurrency` | 唯一闸在 provider 层，用户要知道往哪调 |
| K5 | 收门公式 `max(1, min(cap − 1, others))`、本轮只降不升 | D13 Q5 修订：一波 429 内收敛到网关真实上限，误填 65536 也自愈；不回升免震荡 |
| K6 | 门收窄后多余名额释放时丢弃；每次完成只 `notify_one` 一个 429 等待者 | 防 95 个等待者一起重试再撞 |
| K7 | 删除键给专属 warning，不叠加通用「无定义」 | 用户看到的是「为什么没了、去哪调」而不是「拼错了」 |
| K8 | 等待条件 = `completions > seq0` **或** `active == 0`；无他人在跑时退避 sleep | 全员 429 时若只等完成事件会互相等死 |
| K9 | 429 重试上限 = `max(4, 当时门宽 × 2)` | D13 写「门宽 × 2」；门宽收敛到 1 时只剩 2 次、退避合计 1.5 s 太薄，加下限 4（合计 ≈ 7.5 s 无 Retry-After 时）——偏离处在此显式登记 |
| K10 | RunMeta 新增 `provider_rate_limit`（skip 空），不翻 schema 号 | 与 v4 其余观测字段同款；空时旧消费者零噪音 |
| K11 | 只动 scanner 路径：`single_shot` / `tool_loop` / `verify.rs` 的 429 与信号量不变 | followup 已裁「队列侧随 M21b」；避免碰 M21b 要重写的件 |
| K12 | 不开子分支，直接在 `feat/m21-review-engine` 做 | 单 session、无并行批；文档 commit 已在此分支 |
| K13（评审修） | 429 等待者重试须经准入：`active < 有效门宽` 才预占在途，门满回去等下一次完成；被在途归零广播唤醒者先退避再准入 | code-review 高危：广播后零退避齐发且不受收窄门约束，K6 / K8 同时失效 |
| K14（评审修） | 只有成功完成才唤醒等待者、才计 completions；Timeout / 5xx 只减在途 | 客户端超时不释放网关容量；失败当完成会让等待者撞门并错误收门 |
| K15（评审修） | 429 计数按每次 HTTP 调用独立（`nth` 由重试循环传入，策略无计数状态） | 首发与修复轮共用一份额度会让修复轮首个 429 即放弃 |
| K16（评审修） | `CellRecord.attempts` = 轮次 + 429 重试；`queue_wait_ms` = 门等待 + 429 排队等待 | 兑现「每 cell 门等待可读」，死字段 attempts 复活为真实口径 |
| K17（评审修） | 排队告警扇出 / resume 共用 `registry::queue_warnings`，按收口时有效门宽；已收窄时不劝「调大」 | 与 429 告警不打架；resume 与扇出口径一致 |
| K18（评审修） | HTTP 529 归 RateLimited 进排队重试；503 保持 Server（followup 待裁） | 529 = Anthropic 系 overloaded，与 SSE `overloaded_error` 同义 |
| K19（评审修） | 尝试括号 RAII 守卫：future 被丢弃按 Failed 收尾 | job deadline `select!` 取消不泄漏在途 |
| K20（评审修） | config 已删键统一 `removed` 表（`risk` / `workers`），`check_keys` 单机制 | 同文件不留两种「已删键」手法 |

## Acceptance Criteria

- [x] AC1 `Config::from_json(样例 v10)` 通过；含 `review.scanners.workers` 的旧配置解析成功、warning 含「已删除（D13）」且无「无定义」；`Config::default().review.max_concurrency == 10`；`maxConcurrency: 65536` 出「像是填错」warning；`samples: 0` 报错。
- [x] AC2 `run_scan_fanout`：4 格 / 门宽 1 → `inflight_peaks == 1`；门宽 2 → 2；3 格 / 门宽 1 → 告警 `scanner cells 3 on provider p > maxConcurrency 1: 2 cell(s) queued` 且派发序仍 diff 字节降序、结果 plan 序、`queue_wait_ms ≤ duration_ms`。
- [x] AC3 registry 四个 429 用例（R3 ①②③ + 评审修 ④ 准入 / 失败不算完成 / 预占）通过；既有共享门 / 未注册用例不变。
- [x] AC4 `RunMeta::default()` 序列化不含 `provider_rate_limit`；赋值 round-trip；`redteam-next review --help` 含「无角色级 workers」且不含 `review.scanners.workers`。
- [x] AC5（本仓 dogfood 2 格：缺省门宽零排队；门宽 1 → 排队告警 + g1 queue_wait 173.5 s + 峰值 1；记录 `.next/private/d13/`）真跑一次（PR181 或本仓 dogfood，DeepSeek 单 scanner，`--format json`）：`run.cells[*].queue_wait_ms` 在门宽 ≥ cell 数时全为 0 附近、`run.warnings` 无排队告警；临时把该 provider `maxConcurrency` 设 2 再跑一次：出现排队告警、被排队格 `queue_wait_ms` > 0、`provider_inflight_peak == 2`；两跑 cell 零失败。429 路径以单测为准（网关无法可靠触发）。
- [x] AC6（lib 538 / e2e 17 / clippy 7 / verify 绿）`cargo test` lib / e2e 全绿，clippy 不高于基线，`make verify` 绿；`git grep -n "workers"` 在 `src/` 只剩注释性历史引用或零命中。

## Out of Scope

- 深挖队列入口五态并入、派发时 / 落地时 agreement 两字段、H0 入队序（M21b，design §6）。
- verifier / reviewer 路径挂 `AttemptPolicy`；`verify.rs` 的 Semaphore（M21b 改流消费者时接门）。
- 门宽本轮回升（AIMD 加法半边）、跨进程共享门、RPM 令牌桶（R16 / M20）。
- 每角色在途峰值拆分（现只有 scanner 角色；M21b 接 reviewer 时把 `provider_inflight_peak` 按角色分列）。
- `redteam model setup` 对 `--max-concurrency` 的额外校验（沿用 ≥ 1）。

## Risks / Deferred

- `Notify` 的唤醒语义靠「先建 `notified()` 再检查条件」保证不丢事件；`notify_one` 会存一次许可，可能造成一次虚假唤醒，靠条件重检吸收（design §2.3）。
- 收门只降不升：一次偶发 429 让该 provider 本轮少一槽；RunMeta 可见，接受。
- 真跑无法验证 429 路径；`GateAttempts` 逻辑用注入退避基数的单测覆盖，网关侧留 followup 观测项（PR181 每轮 `provider_rate_limit`）。
- `FakeChatBackend::new_results` 队列跨并发共用，429 用例用两个 backend（慢 OK 与 429→OK）分离以保证确定性。
