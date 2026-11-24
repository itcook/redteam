# U2 验收证据（2026-11-24）

> 环境：`feature/hibot-audit-exec`，debug 二进制 `target/debug/redteam`（与提交一致）。
> 所有命令均在仓根执行；外部依赖仅 `python3`（jsonschema 校验）与 `git`。

## 1. 测试与防漂移

```
$ cargo test        # exit=0
test result: ok. 1011 passed; 0 failed; 0 ignored; 0 measured; 5.52s
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 0.03s
test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 4.85s   # job_conformance
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0.41s   # packaging
test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 7.79s  # e2e
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0.00s

$ make verify       # exit=0
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; finished in 0.16s
```

## 2. hibot JSON Schema（原位只读校验，不复制 schema）

```
manifest PASS 0
report PASS 0
```

命令：Python `jsonschema` + `referencing`，加载 `~/hibot/.docs/audit/schema/*.schema.json` 的 `$id` registry，
对 `job.review-1.json` / `report.review-1.json` 实例逐条校验。

## 3. SIGTERM（含已完成 finding + 挂起的 refuter 请求）

```
cells_before_signal: ['scan[g1]@p1/scan#1', 'review[src/app.js:rule:1-3]@p1/review', 'review[src/app.js:scan:1-3]@p1/review']
exit=0 after_sigterm=0.03s status=partial code=wall_clock_exhausted findings=2
finding: SQL injection src/app.js 2 sha256:2b6349ee...
finding: SQL injection src/app.js 2 sha256:2b6349ee...
```

（该次手工跑发生在去重修复之前：两个队列项报出同一 id。`findings_from_cells` 现已按 id 去重，单测 `duplicate_findings_across_cells_are_deduped_by_id` 钉住。）
空 partial 路径另测：`exit=0 after_sigterm=0.06s status=partial code=wall_clock_exhausted`。

## 4. 墙钟预算

```
timeout_exit=0 elapsed=1.79s status=partial code=wall_clock_exhausted reason=wall-clock budget exhausted or terminated; results flushed
```

（`limits.wall_clock_s=1`，provider 请求挂起。）

## 5. cwd 独立性 + 零写 repo

```
cwd_independent: True | status: ok | findings: 1
repo_clean: True
```

（同一 job 从 `/` 与 `/tmp` 各跑一次，status/fingerprints/input 一致；`git status --porcelain` 为空。）

## 6. config check（stdout 分流 / 退出码 / B1）

```
$ redteam config check ~/hibot/.docs/examples/audit-pr-468/config/executor.redteam.json --kind review   # exit=1
ERROR /review/verifiers unknown_key: unknown key (known: blockThreshold / confFloor / maxConcurrency / tools / scanners / reviewers / refuters / grouping / pack)
ERROR / legacy_shape: 配置形状已升级（JSON，providers → models → roles，design §12.1〔D12〕；见 redteam --help）： review.verifiers 已更名 review.refuters（0.8 术语分家：`verify` 留给 hibot 的修复验证 kind，评审内验真叫 refute）
RESULT error 2 errors, 0 warnings
--- stderr bytes: 0
```

按 P16 应改为 `review.refuters.enabled`（hibot 侧待更新）。

WARN 不影响退出码：

```
$ redteam config check '{"providers":{"p":{"kind":"openai","maxConcurrency":999,"models":{"s":{"roles":{"scanner":{}}},"r":{"roles":{"reviewer":{}}}}}}}' --kind review  # exit=0
WARN /providers/p max_concurrency_sane: maxConcurrency = 999 looks like a wrong field (maxOutputTokens?)
RESULT ok 0 errors, 1 warnings
```

错误类型各一：

```
$ config check '{"review":{"maxConcurrency":"x"},...}' --kind review   # exit=1
ERROR /review invalid_value: 配置形状错误：invalid type: string "x", expected usize
$ config check '{"language":"fr_FR"}' --kind review                   # exit=1
ERROR /language invalid_value: 配置形状错误：unknown variant `fr_FR`, expected `zh_CN` or `en_US`
$ config check '/nope.json'                                           # exit=1
ERROR / input_unreadable: cannot read /nope.json: No such file or directory (os error 2)
$ redteam config check x --format json                                # clap 拒收
error: unexpected argument '--format' found
```

## 7. job 命令面

```
$ redteam review --job-dir X --base main
error: the argument '--job-dir <JOB_DIR>' cannot be used with '--base <BASE>'   # exit=1
$ redteam review --job-dir X --config y
error: unexpected argument '--config' found                                      # exit=1
```

kind 不一致 / 坏 manifest / sha 不符（手工 smoke，退出码与报告）：

```
kind mismatch          → exit=1, status=error, status_code=kind_mismatch:cli kind verify vs manifest review
bad sha256             → exit=1, status=error, status_code=input_checksum_mismatch:diff_patch
manifest 不可解析       → exit=2, report_exists=no
```

## 8. 环境隔离

- `seal_job_env` 启动即清 `REDTEAM_MAX_TOKENS` / `REDTEAM_PER_TURN_SECS` / `REDTEAM_DUMP_DIR` /
  `REDTEAM_HOME` / `LC_ALL` / `LC_MESSAGES` / `LANG`（集成用例断言调用后不存在）。
- `job_mode_key_resolution_is_env_only`：job 分支用 `Secrets::default()`，env 未注入 → 无 key；
  secrets 有 key 的对照证明解析链确实会读到——所以 job 必须传空。
- 语言：配置 `zh_CN` → prompt 含「Simplified Chinese (简体中文)」；无配置 + `LANG=zh_CN` → 仍 English、source=default。
- **未做**：独立子进程 + 诱饵 `~/.redteam` 的端到端证明（advisor 建议的理想形态）——列为覆盖限制。
