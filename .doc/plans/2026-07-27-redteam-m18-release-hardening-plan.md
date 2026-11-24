# M18 发布硬化 · 实施计划（单份四批分节）

> 状态: 已交付（2026-07-28 收官：四批 SDD 全合 dev + dogfood 三轮 reject→reject→approve 收敛，明细 roadmap done.md M18 节）· 对应 design `../design/2026-07-27-redteam-m18-release-hardening-design.md`（八裁决闭集）；决策 `../decisions/2026-07-27-redteam-m18-release-hardening-decisions.md`；里程碑=roadmap M18。
> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development。每批一个分支逐批走；批内 task 派发。

**Goal:** 安全收尾（批 1）+ 契约定稿（批 2）+ e2e 补网（批 3）+ polish（批 4）；收官多模型 live dogfood。

**批间依赖:** 批 1/2 独立可任选先后；批 3 后置批 2（退出码契约）；批 4 随批 2 搭车或独立。每批独立分支合 dev（--no-ff 纯本地），下一批从新 dev 切。

## Global Constraints（全批隐含）

- design §6 契约面：变化仅白名单四项；schema v3/DDL/正常输入 prompt 轮廓零变化。
- 每 task：`cargo test` 全绿 + `cargo clippy --all-targets` 零新增 + `make verify` + commit（简体中文，绝不 push）。
- 范围闭集 design §8；批内发现的新问题记台账不顺手修（除非 Critical）。

---

## 批 1 · 安全组（分支 feat/m18-security）

### Task 1-A: D2 增量注入面中和 + 注入回归测试（红测法）
- Modify: `src/prompt/mod.rs`（render_incremental_section：manifest 过 neutralize_structure；claimed 卡 id/file/title/quote 过 neutralize+oneline——对齐 render_precedents 的 E11-Q5 手法，抽共享 oneline helper 勿复制）。
- TDD 红测法：先写两向量攻击测试（manifest 路径含 `</output_contract>` 闭标签→断言被中和；claimed title 含换行+伪造行→断言单行化）确认修复前红 → 修复 → 绿。正常输入下渲染逐字节不变测试锚。
- Commit: `fix(prompt): D2 增量注入面中和——manifest+claimed 卡过 neutralize+oneline（T46③/E11-Q5 补防）`

### Task 1-B: denylist 合一 + T46①②④ + git `--`
- Modify: `src/cli/spec.rs`+`src/cli/second_pass.rs`（denylist 单一真相源，语义=并集收紧；+.netrc/.p12；清 .env 死判）、拒读事件进 run.warnings（T46①，接线到调用方 warnings 聚合处）、拒读 eprintln DRY（T46②）、`src/evidence/git.rs` 位置参数补 `--`。
- 测试：并集语义锚（原两清单各自的拒绝项在合一后全拒）+ .netrc/.p12 新拒 + warnings 含拒读条目。
- Commit: `fix(security): denylist 单源合一+T46①②④+git -- 分隔（威胁模型 B 收尾）`

### 批 1 收口: opus 终审（重点：中和不误伤正常输入、并集无漏）→ 修 → 合 dev。

## 批 2 · 契约组（分支 feat/m18-contract，从含批 1 的 dev 切）

### Task 2-A: exit code 五档重整
- Modify: `src/cli/tail.rs`（exit_code 加 Incomplete→3 臂）、`src/main.rs`（全部命令错误 exit(2)→exit(1) 机械替换——grep 全数点名入报告）、`plugins/redteam/commands/ship.md`（停机/守卫处的退出码语义同步）+ `--help`。
- lib 级五档全谱测试；make gen/verify。
- Commit: `feat(cli): exit code 五档契约定稿——1=命令错误/3=incomplete 与降级 approve 分离（发布前最后窗口）`

### Task 2-B: D3 + conf_floor 门 + T48 + 值域
- Modify: `src/main.rs`（result --format md：缺 md 时用 result_json 现算 render_md；双缺报错退 1；--conf-floor clap 值域 0..=1）、`src/consolidate/verify.rs`（refute_targets 加 floor 参+孤证分支门；source_snippet 用 line_start/end 行窗+越界 clamp 注记）。
- 测试：D3 三态（md 有/缺 json 有/双缺）、conf_floor 三锚（design 2-3）、T48 行窗+谎报行号 clamp。
- Commit: `fix(verify+cli): D3 显式渲染/refute conf_floor 门/T48 行契约/值域校验（契约组正确性批）`

### Task 2-C: T25 + T26 + resume 披露 + 批 4 polish 搭车
- Modify: `src/orchestrate/mod.rs`（整链失败 CellRecord 回填累计 usage）、`src/main.rs`（setup 收尾自动 probe 警告不阻断；validate_setup 文案与 alias 收敛；<ID> help 两处）、`src/cli/mod.rs`（resume append_partial eprintln）、`src/output/mod.rs`（Duration <1min 显秒）、TwoStage/read_file 注释对齐。main dispatch 抽取可选（实施者评估，大动就跳过记台账）。
- 测试：T25 失败 usage 入账锚、T26 probe 失败仍落盘、Duration 秒级。
- Commit: `fix(cli): T25 失败 tokens 入账/T26 setup 自动 probe/resume 披露清零 + polish 批（批 4 搭车）`

### 批 2 收口: opus 终审（重点：exit(1) 机械替换无漏/D3 不破 ship 依赖/五档谱完整）→ 修 → 合 dev。

## 批 3 · e2e 扩充（分支 feat/m18-e2e，从含批 2 的 dev 切）

### Task 3-A: REDTEAM_TEST_PANIC 钩子 + 真 panic 全链路 e2e（T47③）
- Modify: `src/main.rs` 或 `src/cli/mod.rs`（Cmd::Worker 路径 fan-out 前：`if std::env::var("REDTEAM_TEST_PANIC").is_ok() { panic!("REDTEAM_TEST_PANIC") }`，注释注明 e2e 回归资产、不进 --help）；新 e2e：真 spawn 二进制（复用 c-T6 判死报告里的 spawn 探索）带 env → 轮询终态 → 断言 Failed+error 含 panic+已有终态不被覆（封盘）。
- Commit: `test(e2e): T47③ 真 panic 全链路——REDTEAM_TEST_PANIC 钩子+真 spawn 断言（M16 以来最后一块端到端盲区）`

### Task 3-B: md 渲染/台账注入/退出码链 e2e + is_same_bug 补例
- Modify: `tests/review_e2e.rs`（3-1/3-2/3-3 三条，复用既有骨架；退出码 3 构造手法自裁并报告）、`src/consolidate/dedup.rs` tests（3-5 同义非全等例）。
- Commit: `test(e2e): 发布覆盖门——md 渲染/台账注入/退出码五档链 + is_same_bug 补例`

### 批 3 收口: sonnet 合一评审（测试真锚核验：改断言应红）→ 合 dev。

## 收官

- **多模型 live dogfood**：`redteam review --base <M17 起点> --models-all`（或多模型显式）审 M17+M18 累计 diff；人裁 findings（修/dismiss 按 ship 内循环规则）；结果记台账。
- roadmap M18 置 ✅、done.md、TODO 批量刷（T25/T26/T46/T47/T48 交付标记）、plan 状态头。
- M19 就绪判定：M18 收官即 M19 启动条件成熟（发布批：T06 拆除/README/tag/分发面）。
