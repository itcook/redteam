# 轮次增量协议（M13）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 实现 ship 循环的轮次增量协议——r1 全量、r2..rn 只审 fix delta，未动代码 findings 重锚定携带，销案/假修复回路闭环，ship 会话总 input tokens 较历史降 ≥50%。

**Architecture:** 沿现有单 crate 分层扩展：evidence/git（delta+rename map+stamp）→ consolidate/carry（迁移/重锚/合并）→ cli（四守卫+增量流水线）→ prompt（`<incremental_round>` 条件段）→ ship.md（协议改造）。数据契约 schema v2→v3（Finding.origin + run 聚合数组 + stats.new，serde default 全向后兼容）；resolve 独立台账复用 dismiss 表范式。

**Tech Stack:** Rust（tokio/serde/rusqlite/clap，零新依赖）；hermetic 测试用 tempfile + 真实 git 子进程仓库。

**Spec:** `.doc/design/2026-07-03-redteam-incremental-rounds-design.md`（决策态权威）；任务分解依据 `.doc/reviews/2026-07-03-incremental-rounds-eng-review.md` §6 T1-T7。

## Global Constraints

- 分支 `feat/incremental-rounds`（基于 dev d579948），**主线顺序单 worktree**——批①-④共享 `src/cli.rs`/`report.rs` 文件级高耦合，不并行（评审记录 §8）。
- **交付门槛**：3 个 P1 加固——rename 迁移 R1（Task 7）、健康基线守卫 R2（Task 9）、stamp 权威 R3（Task 2）——未全部落地不得进 live 验收（Task 15）。
- **不动面**（design §4）：裸 `redteam review` 语义、`compute_verdict` 签名、exit code 契约（reject→10 / approve+failed→2 / 0）、dismiss 台账/判例机制、refute 语义前缀契约。
- v1 仅 code 模式 + **branch scope**（working-tree 证据不可 stamp：轮间未提交改动会漏出 delta，锚定不变式被静默击穿——本 plan 将其折入同族守卫，`--help` 与错误文案写明前提）。
- schema_version 全局 `"2"→"3"`；所有新 serde 字段带 `default`（v2 老 JSON 反序列化零破坏）；jq 消费方口径 `(.origin // "new")`（R6）。
- 每个 Task 收尾：`cargo test` 全绿再 commit；commit 消息用仓内惯例 `feat(incremental)`/`fix(incremental)` 前缀 + 简中一句话，尾注 `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`。
- `.doc/` 是 gitignored 维护者工作区——本 plan 文件及验收记录**不 commit**。
- ship.md/feedback.md 改动后必须 `make gen` 重生成 codex 镜像并 `make verify` 零 diff，同一 commit 提交。

## File Structure（改动落点总览）

| 文件 | 动作 | 职责 |
|---|---|---|
| `src/domain/report.rs` | 改 | schema v3：Verdict/Stats/Usage/RunMeta/LedgerMeta/ReviewReport 加 Deserialize；RunMeta 加 head_sha/incremental/delta_base/resolved/resolution_disputed/carried_dropped；Stats 加 new 子块 |
| `src/domain/finding.rs` | 改 | Origin 枚举 + Finding.origin 字段；Cluster 加 Deserialize |
| `src/evidence/git.rs` | 改 | git_at/head_sha_at/is_ancestor/rename_map/changed_files_since/branch_files |
| `src/db.rs` | 改 | resolved + resolved_history 表；ResolveCard/resolve_upsert/resolve_get/resolved_cards |
| `src/main.rs` | 改 | `Resolve` 子命令 + run_resolve；Review 加 `--incremental-from` flag |
| `src/orchestrate/mod.rs` | 改 | finding_id 单一真相源提取；run_code_fanout 加 incremental 参数 |
| `src/positioning/mod.rs` | 改 | anchor_near + locate_quote_near（carry 就近变体） |
| `src/consolidate/carry.rs` | **新建** | migrate_renames/migrate_resolve_cards/carry/merge_carry/CarryOutcome |
| `src/consolidate/mod.rs` | 改 | 挂载 carry 模块 |
| `src/consolidate/verify.rs` | 改 | refute_targets 剔除 origin=carried |
| `src/cli.rs` | 改 | head_sha 穿线（TailCtx/run_code_review/dispatch）；incremental_preflight 四守卫；run_incremental_review 流水线；ReviewArgs.incremental_from；run_resume 增量拒绝 |
| `src/prompt/mod.rs` | 改 | render_incremental_section；build_bundle_prompt 加 incremental 参数 |
| `src/prompt/templates/adversarial-code-bundle.md` | 改 | `{{INCREMENTAL_ROUND}}` 占位 |
| `src/bundle/mod.rs` | 改 | cap_entries 提取 + two_layer_manifest |
| `src/output/mod.rs` | 改 | md 渲染：Incremental 信息行 + carried 标记 |
| `plugins/redteam/commands/ship.md` | 改 | 起评审增量化/销案步/停机节重写/争议刹车/可选终扫 |
| `plugins/redteam/feedback.md` | 改 | jq raised 改 new-only（v2 兼容）+ carried 指标 |
| `plugins/redteam/commands/review.md` | 不动 | 薄透传壳层不枚举 flag，已核实无需改 |

任务依赖顺序（与设计 §5 五批对应，批③内 prompt 段前移一位以满足流水线依赖）：

```
批① 契约与存储    Task 1(schema v3) → Task 2(stamp R3) → Task 3(resolve 台账)
批② carry 引擎    Task 4(git delta/rename) → Task 5(finding_id) → Task 6(anchor_near R4)
                  → Task 7(carry 引擎 R1) → Task 8(refute new-only)
批③ CLI 流水线    Task 9(四守卫 R2 + flag) → Task 10(prompt 段) → Task 11(流水线+假修复靶)
                  → Task 12(md 渲染)
批④ 协议层        Task 13(ship.md + feedback.md + make gen)
批⑤ 验收          Task 14(单跳回放) → Task 15(争议刹车靶 + live 收编)
```

---

### Task 1: schema v3 数据契约（Origin + RunMeta 聚合 + Deserialize）

**Files:**
- Modify: `src/domain/finding.rs`（Origin 枚举、Finding.origin、Cluster+Deserialize）
- Modify: `src/domain/report.rs`（Deserialize 全家 + RunMeta/Stats 新字段）
- Modify: `src/cli.rs`（schema_version "2"→"3" 两处：assemble_report、partial_report）
- Test: 各文件内联 `#[cfg(test)]`

**Interfaces:**
- Produces: `crate::domain::Origin`（`Carried | New`，Default=New，serde lowercase）；`Finding.origin: Origin`；`RunMeta { head_sha: Option<String>, incremental: bool, delta_base: Option<String>, resolved: Vec<String>, resolution_disputed: Vec<String>, carried_dropped: Vec<String>, .. }`；`Stats.new: Option<Box<Stats>>`；`ReviewReport: Deserialize`（后续 Task 9 载上轮 result 依赖）。

- [ ] **Step 1: 写失败测试（finding.rs 追加到 tests mod）**

```rust
    #[test]
    fn origin_defaults_new_and_serializes_lowercase() {
        // v2 老 JSON（无 origin 键）反序列化 → 默认 New（R6：缺省视 new）
        let j = r#"{"id":"x","severity":"high","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r","lenses":[],"models":[],"agreement":1,"hard_constraint":false,"quote":"q"}"#;
        let f: Finding = serde_json::from_str(j).unwrap();
        assert_eq!(f.origin, Origin::New);
        // v3 序列化显式携带（契约字段不靠缺省猜）
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(v["origin"], "new");
        let mut c = f.clone();
        c.origin = Origin::Carried;
        let v: serde_json::Value = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(v["origin"], "carried");
    }
```

（report.rs 追加到 tests mod：）

```rust
    #[test]
    fn report_v2_json_deserializes_with_defaults() {
        // v2 真实形状：无 head_sha/incremental/聚合数组/stats.new/dismissed_reappeared
        let j = r#"{"schema_version":"2","mode":"code","verdict":"approve",
            "stats":{"critical":0,"high":0,"medium":0,"low":0,"total":0},
            "findings":[],"clusters":[],
            "run":{"job_id":"j1","models":["m"],"lenses":["L1"],"duration_ms":1,
                   "usage":{"input_tokens":1,"output_tokens":1},
                   "warnings":[],"failed_models":[]}}"#;
        let r: ReviewReport = serde_json::from_str(j).unwrap();
        assert_eq!(r.verdict, Verdict::Approve);
        assert_eq!(r.run.head_sha, None);
        assert!(!r.run.incremental);
        assert!(r.run.resolved.is_empty() && r.run.carried_dropped.is_empty());
        assert!(r.stats.new.is_none());
    }

    #[test]
    fn runmeta_v3_fields_roundtrip_and_skip_when_default() {
        let mut m = RunMeta { head_sha: Some("abc".into()), incremental: true,
            delta_base: Some("def".into()), resolved: vec!["1".into()],
            resolution_disputed: vec!["2".into()], carried_dropped: vec!["3".into()],
            ..Default::default() };
        m.job_id = "j".into();
        let j = serde_json::to_string(&m).unwrap();
        assert!(j.contains("\"head_sha\":\"abc\"") && j.contains("\"incremental\":true"));
        // 默认态（非增量）不产键——v2 消费者视角零噪音
        let d = serde_json::to_string(&RunMeta::default()).unwrap();
        for k in ["head_sha", "incremental", "delta_base", "resolved", "resolution_disputed", "carried_dropped"] {
            assert!(!d.contains(k), "默认态不应含 {k}: {d}");
        }
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test origin_defaults_new report_v2_json runmeta_v3 2>&1 | tail -5`
Expected: 编译错误（`Origin` 未定义、RunMeta 无新字段、ReviewReport 无 Deserialize）。

- [ ] **Step 3: 实现（finding.rs）**

Origin 枚举加在 Severity 之后：

```rust
/// M13 轮次增量：finding 出身——carried=上轮携带（重锚定成活）、new=本轮 delta 新发。
/// v2 老 JSON 无此键，缺省 New（R6 兼容口径 `(.origin // "new")`）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Carried,
    #[default]
    New,
}
```

Finding 加字段（放在 `previously_dismissed` 之后）：

```rust
    /// M13：出身标记（schema v3 显式序列化；反序列化缺省 New）。
    #[serde(default)]
    pub origin: Origin,
```

Cluster 的 derive 行 `#[derive(Clone, Debug, Serialize)]` 改为 `#[derive(Clone, Debug, Serialize, Deserialize)]`。

- [ ] **Step 4: 实现（report.rs）**

四个 derive 行统一加 Deserialize：
- `Verdict`: `#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]`
- `Stats` / `Usage` / `RunMeta`: `#[derive(Clone, Debug, Default, Serialize, Deserialize)]`
- `LedgerMeta` / `ReviewReport`: `#[derive(Clone, Debug, Serialize, Deserialize)]`

Stats 加子块（结构体尾部）：

```rust
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub critical: u32, pub high: u32, pub medium: u32, pub low: u32, pub total: u32,
    /// M13：仅本轮新发（origin=new）的计数子块；主字段维持合并集口径（verdict 依据）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub new: Option<Box<Stats>>,
}
```

RunMeta：`dismissed_reappeared` 的 serde 属性补 `default`（加 Deserialize 后缺键才不报错）：

```rust
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub dismissed_reappeared: Vec<String>,
```

RunMeta 尾部加六个字段：

```rust
    /// R3 stamp：证据收集实跑刻的 rev-parse HEAD（code 模式全路径回填；doc 为 None）。
    /// 增量轮以上轮此值为 delta base。serde default 兼容 v2。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub head_sha: Option<String>,
    /// M13：本次是否增量轮（--incremental-from 路径）。
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub incremental: bool,
    /// 增量轮 delta 基线（上轮 stamped HEAD）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub delta_base: Option<String>,
    /// 命中 resolve 台账被排除携带的 id（销案生效审计）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub resolved: Vec<String>,
    /// 假修复回路：claimed-resolved 但同 id 再现的 finding id（ship 按修复未生效重开）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub resolution_disputed: Vec<String>,
    /// 重锚定失败被消灭的携带候选 id（使锚失效的改动必在本轮 delta 内，审计用）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub carried_dropped: Vec<String>,
```

- [ ] **Step 5: 编译驱动的机械扫尾**

Run: `cargo build 2>&1 | grep -E "^error" | head -30`

两类必然报错，逐一机械补齐：
1. 全仓 `Finding { ... }` 字面量缺 `origin` 字段 → 每处补 `origin: Origin::New,`（测试夹具全部是 New；`use` 处补 `Origin` 导入）。涉及文件：finding.rs/report.rs/verdict.rs/positioning/mod.rs/consolidate/dedup.rs/consolidate/verify.rs/cli.rs/orchestrate（enrich 函数体也补 `origin: Origin::New,`——它构造的就是新发 finding）。
2. `cli.rs` 两处 RunMeta 字面量（assemble_report、partial_report）→ 各在尾部加 `..Default::default()`（新字段全走默认，语义不变）；同时两处 `schema_version: "2".into()` 改 `"3".into()`。

- [ ] **Step 6: 全量测试通过**

Run: `cargo test 2>&1 | tail -3`
Expected: 全绿（含 Step 1 新测试；若 output 模块有测试断言 `"2"` 需同步——现有 output tests 自建 report 字面量不断言版本号，应无涉）。

- [ ] **Step 7: Commit**

```bash
git add src/domain/ src/cli.rs src/consolidate/ src/positioning/ src/orchestrate/ src/output/
git commit -m "feat(incremental): schema v3 契约——Finding.origin + RunMeta 聚合字段 + 全报告 Deserialize，v2 零破坏"
```

---

### Task 2: head_sha 权威 stamp（R3，P1 交付门槛）

**Files:**
- Modify: `src/evidence/git.rs`（git_at + head_sha_at）
- Modify: `src/cli.rs`（TailCtx.head_sha、run_code_review 签名、run_review_dispatch 回填、run_review/run_resume 传 None）
- Test: `src/evidence/git.rs` + `src/cli.rs` 内联

**Interfaces:**
- Consumes: Task 1 的 `RunMeta.head_sha`。
- Produces: `evidence::git::head_sha_at(dir: &Path) -> anyhow::Result<String>`；`run_code_review(args, provider, cfg, changes, root, head_sha: Option<String>, sink)`（第 6 参新增）；后续 Task 4 的 `git_at(dir, args)` 基建。

- [ ] **Step 1: 写失败测试（git.rs tests mod 追加；git 仓库测试基建后续 Task 4 复用）**

```rust
    /// 测试基建：tempdir 里造真实 git 仓库（后续 rename/delta/谱系测试共用）。
    fn git_repo(dir: &Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
    }

    fn commit_all(dir: &Path, msg: &str) -> String {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", msg]);
        run(&["rev-parse", "HEAD"]).trim().to_string()
    }

    use std::path::Path;

    #[test]
    fn head_sha_at_returns_current_commit() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn main() {}\n").unwrap();
        let sha = commit_all(dir.path(), "c1");
        assert_eq!(head_sha_at(dir.path()).unwrap(), sha);
        // 非 git 目录 → Err（调用方 .ok() 折 None，不炸评审）
        let plain = tempfile::tempdir().unwrap();
        assert!(head_sha_at(plain.path()).is_err());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test head_sha_at 2>&1 | tail -5`
Expected: 编译错误（head_sha_at 未定义）。

- [ ] **Step 3: 实现（git.rs，加在 `fn git` 之后）**

```rust
/// 在指定目录跑 git（M13 增量基建：hermetic 测试可用 tempdir 仓库，不依赖进程 cwd）。
/// ANSI 关死同 `git()`（color.diff 必须显式 false，命令行 -c 恒优先于文件 config）。
fn git_at(dir: &std::path::Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["-c", "color.ui=false", "-c", "color.diff=false"])
        .args(args)
        .output()?;
    if !out.status.success() {
        anyhow::bail!("git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// R3 stamp：证据 HEAD（权威时刻 = 证据收集实跑刻，调用方与 changed_files 同刻调用）。
pub fn head_sha_at(dir: &std::path::Path) -> anyhow::Result<String> {
    Ok(git_at(dir, &["rev-parse", "HEAD"])?.trim().to_string())
}
```

- [ ] **Step 4: 穿线（cli.rs）**

1. `TailCtx` 加字段（`sink` 之前）：

```rust
    /// R3 stamp：证据收集刻的 HEAD（code dispatch 回填；doc/resume 为 None）。
    head_sha: Option<String>,
```

2. `assemble_report` 的 RunMeta 构造：`..Default::default()` 前加 `head_sha: ctx.head_sha,`。
3. `run_code_review` 签名在 `root: &Path` 后加 `head_sha: Option<String>,`；其 TailCtx 构造加 `head_sha,`。
4. `run_review` 与 `run_resume` 的 TailCtx 构造加 `head_sha: None,`。
5. `run_review_dispatch` Code 分支改为：

```rust
        Mode::Code => {
            let changes = crate::evidence::git::changed_files(sync_args.base.as_deref(), scope_enum == Scope::Branch)?;
            // R3：stamp 权威时刻 = 证据收集实跑刻（与 changed_files 同刻同 cwd）；
            // 非 git 目录折 None——前台裸 review 与后台 job 共用本单点，前后台天然同 sha。
            let head_sha = crate::evidence::git::head_sha_at(cwd).ok();
            run_code_review(&sync_args, provider, cfg, changes, cwd, head_sha, sink).await
        }
```

6. 编译驱动修 `run_code_review` 的测试调用点（code_review_tests 共 3 处）：`root` 参数后插 `None,`。

- [ ] **Step 5: 加 stamp 落 RunMeta 的回归测试（cli.rs code_review_tests 追加）**

```rust
    #[tokio::test]
    async fn code_review_stamps_head_sha_into_runmeta() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::from_layers(&[r#"[review]
default_models=["m"]
"#]).unwrap();
        let provider = FakeProvider::with("m", vec![]);
        let (report, _) = run_code_review(&args(), &provider, &cfg, vec![], dir.path(),
            Some("abc123".into()), None).await.unwrap();
        assert_eq!(report.run.head_sha.as_deref(), Some("abc123"));
        let (report2, _) = run_code_review(&args(), &provider, &cfg, vec![], dir.path(), None, None).await.unwrap();
        assert_eq!(report2.run.head_sha, None, "非 git 环境如实 None，不编造");
    }
```

- [ ] **Step 6: 全量测试通过**

Run: `cargo test 2>&1 | tail -3`
Expected: 全绿。

- [ ] **Step 7: Commit**

```bash
git add src/evidence/git.rs src/cli.rs
git commit -m "feat(incremental): R3 stamp——证据收集刻 rev-parse HEAD 落 RunMeta.head_sha，前后台单点同源"
```

---

### Task 3: resolve 销案台账（独立表 + `redteam resolve` 子命令）

**Files:**
- Modify: `src/db.rs`（resolved/resolved_history 表 + ResolveCard + 三方法）
- Modify: `src/main.rs`（Resolve 子命令 + run_resolve）
- Test: `src/db.rs` 内联；CLI 层走 clap 声明式无需单测（与 dismiss 同标准）

**Interfaces:**
- Consumes: main.rs 既有 `snapshot_from_report(path, id)`、`redteam::project::is_hex16`。
- Produces: `db::ResolveCard { id, title, file, quote, severity, reason, round, updated_at: String }`（无 status——已修复不是状态机）；`ProjDb::resolve_upsert(&mut self, &ResolveCard)`、`resolve_get(&self, id) -> Result<Option<ResolveCard>>`、`resolved_cards(&self) -> Result<Vec<ResolveCard>>`。Task 7/11 消费 resolved_cards。

- [ ] **Step 1: 写失败测试（db.rs tests mod 追加）**

```rust
    fn rcard(id: &str) -> ResolveCard {
        ResolveCard {
            id: id.into(), title: "t".into(), file: "src/a.rs".into(),
            quote: "let x = 1;".into(), severity: "high".into(),
            reason: "加了守卫".into(), round: "r2".into(), updated_at: String::new(),
        }
    }

    #[test]
    fn resolve_upsert_get_cards_roundtrip() {
        let mut db = mem_db();
        db.resolve_upsert(&rcard("a1b2c3d4e5f60718")).unwrap();
        let got = db.resolve_get("a1b2c3d4e5f60718").unwrap().unwrap();
        assert_eq!((got.file.as_str(), got.round.as_str()), ("src/a.rs", "r2"));
        assert!(!got.updated_at.is_empty());
        // 再销案（争议后重修）→ upsert 覆盖 + history 两条
        let mut again = rcard("a1b2c3d4e5f60718");
        again.round = "r4".into();
        db.resolve_upsert(&again).unwrap();
        assert_eq!(db.resolve_get("a1b2c3d4e5f60718").unwrap().unwrap().round, "r4");
        assert_eq!(db.resolved_cards().unwrap().len(), 1, "同 id 覆盖不追加");
    }

    #[test]
    fn resolve_is_disjoint_from_dismiss_exemption() {
        // 已修复 ≠ 误报：resolve 卡绝不进 dismissed 豁免/预筛面（codex #10 核实项的绊线）
        let mut db = mem_db();
        db.resolve_upsert(&rcard("1111111111111111")).unwrap();
        assert!(db.active_ids().unwrap().is_empty(), "resolve 不得漏进豁免 id 集");
        assert!(db.active_cards().unwrap().is_empty(), "resolve 不得漏进判例卡集");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test resolve_upsert resolve_is_disjoint 2>&1 | tail -5`
Expected: 编译错误（ResolveCard/方法未定义）。

- [ ] **Step 3: 实现（db.rs）**

`open_at` 的 execute_batch 尾部追加（CREATE IF NOT EXISTS 幂等，既有库零迁移）：

```sql
             CREATE TABLE IF NOT EXISTS resolved (
               id TEXT PRIMARY KEY,
               title TEXT NOT NULL DEFAULT '',
               file TEXT NOT NULL DEFAULT '',
               quote TEXT NOT NULL DEFAULT '',
               severity TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               round TEXT NOT NULL DEFAULT '',
               updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%d','now'))
             );
             CREATE TABLE IF NOT EXISTS resolved_history (
               seq INTEGER PRIMARY KEY AUTOINCREMENT,
               id TEXT NOT NULL,
               round TEXT NOT NULL DEFAULT '',
               reason TEXT NOT NULL DEFAULT '',
               at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );
```

DismissCard 定义后加：

```rust
/// M13 销案卡（决议 3）：ship 裁决「属实→已修复」的判定记录。与 DismissCard 对称
/// （快照四件套）但**无 status 状态机**——已修复≠误报，独立表、不进豁免/预筛；
/// 增量轮据此排除携带 + 生成 claimed-resolved 顺带验证清单。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ResolveCard {
    pub id: String,
    pub title: String,
    pub file: String,
    pub quote: String,
    pub severity: String,
    pub reason: String,
    pub round: String,
    pub updated_at: String,
}
```

impl ProjDb 内（dismiss 方法族之后）：

```rust
    // ---- resolve 销案台账（M13 决议 3，dismiss 表范式对称）----

    /// 销案/再销案：主表 upsert + history 追加（同一事务）。
    pub fn resolve_upsert(&mut self, card: &ResolveCard) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO resolved (id,title,file,quote,severity,reason,round,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,strftime('%Y-%m-%d','now'))
             ON CONFLICT(id) DO UPDATE SET title=?2,file=?3,quote=?4,severity=?5,
               reason=?6,round=?7,updated_at=strftime('%Y-%m-%d','now')",
            params![card.id, card.title, card.file, card.quote, card.severity, card.reason, card.round],
        )?;
        tx.execute(
            "INSERT INTO resolved_history (id,round,reason) VALUES (?1,?2,?3)",
            params![card.id, card.round, card.reason],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn resolve_get(&self, id: &str) -> Result<Option<ResolveCard>> {
        self.conn
            .query_row(
                "SELECT id,title,file,quote,severity,reason,round,updated_at FROM resolved WHERE id=?1",
                params![id],
                row_to_resolve,
            )
            .optional()
            .map_err(Into::into)
    }

    /// 全量销案卡（增量轮携带排除 + claimed-resolved 清单的数据源）。
    pub fn resolved_cards(&self) -> Result<Vec<ResolveCard>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,title,file,quote,severity,reason,round,updated_at FROM resolved
             ORDER BY updated_at DESC, id",
        )?;
        let rows = stmt.query_map([], row_to_resolve)?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
```

文件尾 `row_to_card` 后加：

```rust
fn row_to_resolve(r: &rusqlite::Row<'_>) -> rusqlite::Result<ResolveCard> {
    Ok(ResolveCard {
        id: r.get(0)?, title: r.get(1)?, file: r.get(2)?, quote: r.get(3)?,
        severity: r.get(4)?, reason: r.get(5)?, round: r.get(6)?, updated_at: r.get(7)?,
    })
}
```

- [ ] **Step 4: 跑 db 测试**

Run: `cargo test --lib db:: 2>&1 | tail -3`
Expected: 全绿。

- [ ] **Step 5: CLI 子命令（main.rs）**

Cmd 枚举 `Dismiss` 之后加：

```rust
    /// 销案：把「属实→已修复」的 finding 记入 resolve 台账（proj.db 独立表，不混 dismissed
    /// ——已修复≠误报，不进豁免/预筛）。增量轮（review --incremental-from）据此不再携带该
    /// finding；quote 未随修复消失的会注入下轮「顺带验证」防假修复。
    Resolve {
        /// finding id（16 位 hex，来自评审报告）。
        id: String,
        /// 评审报告 JSON 路径——按 id 自动抄 title/file/quote/severity 快照。
        #[arg(long)]
        from: Option<String>,
        /// 销案发生轮次（如 r2），可选。
        #[arg(long, default_value = "")]
        round: String,
        /// 修法一句话（可选，进台账与 history 备审计）。
        #[arg(long, default_value = "")]
        reason: String,
        /// 快照字段（无 --from 时必须全给；与 --from 并用时覆盖抄写值）。
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        quote: Option<String>,
        #[arg(long)]
        severity: Option<String>,
    },
```

main() 的 match 加分支（`Cmd::Dismiss` 之后）：

```rust
        Cmd::Resolve { id, from, round, reason, title, file, quote, severity } => {
            run_resolve(id, from, round, reason, title, file, quote, severity)
        }
```

`dismiss_add` 之后加（复用 snapshot_from_report 与 pick 模式）：

```rust
/// resolve 子命令（M13 决议 3）：dismiss add 对称的销案入账。
#[allow(clippy::too_many_arguments)]
fn run_resolve(
    id: String, from: Option<String>, round: String, reason: String,
    title: Option<String>, file: Option<String>, quote: Option<String>, severity: Option<String>,
) -> i32 {
    let r = (|| -> anyhow::Result<()> {
        anyhow::ensure!(redteam::project::is_hex16(&id), "id must be a 16-hex finding id, got '{id}'");
        let snap = match &from {
            Some(path) => Some(snapshot_from_report(path, &id)?),
            None => None,
        };
        let pick = |explicit: Option<String>, from_val: Option<String>, name: &str| -> anyhow::Result<String> {
            explicit.or(from_val).ok_or_else(|| anyhow::anyhow!(
                "missing snapshot field '{name}': pass --from <result.json> or --{name}"))
        };
        let (ft, ff, fq, fs) = match snap {
            Some((t, f, q, s)) => (Some(t), Some(f), Some(q), Some(s)),
            None => (None, None, None, None),
        };
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mut db = redteam::db::ProjDb::open(&cwd)?;
        db.resolve_upsert(&redteam::db::ResolveCard {
            id: id.clone(),
            title: pick(title, ft, "title")?,
            file: pick(file, ff, "file")?,
            quote: pick(quote, fq, "quote")?,
            severity: pick(severity, fs, "severity")?,
            reason,
            round,
            updated_at: String::new(),
        })?;
        println!("resolved {id} recorded");
        Ok(())
    })();
    match r {
        Ok(()) => 0,
        Err(e) => { eprintln!("error: {e}"); 2 }
    }
}
```

- [ ] **Step 6: 全量测试 + help 冒烟**

Run: `cargo test 2>&1 | tail -3 && cargo run -q -- resolve --help 2>&1 | head -8`
Expected: 测试全绿；help 显示销案说明（裸终端在场文档原则）。

- [ ] **Step 7: Commit**

```bash
git add src/db.rs src/main.rs
git commit -m "feat(incremental): resolve 销案台账——独立表+history+子命令，dismiss 对称、不混豁免面"
```

---

### Task 4: delta 收集 + rename 映射 + 谱系判定（git 层）

**Files:**
- Modify: `src/evidence/git.rs`
- Test: 内联（复用 Task 2 的 git_repo/commit_all 基建）

**Interfaces:**
- Consumes: Task 2 的 `git_at`、既有 `dedup_files`/`assemble_changes`/`FileChange`。
- Produces: `is_ancestor(dir, sha) -> Result<bool>`；`rename_map(dir, base) -> Result<Vec<(String, String)>>`（旧→新）；`changed_files_since(dir, base) -> Result<Vec<FileChange>>`；`branch_files(dir, base) -> Result<Vec<String>>`（双层 manifest 第一层）。

- [ ] **Step 1: 写失败测试（git.rs tests mod 追加）**

```rust
    #[test]
    fn rename_map_and_delta_since_pure_rename() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("old.rs"), "fn keep() { body(); }\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::fs::rename(dir.path().join("old.rs"), dir.path().join("new.rs")).unwrap();
        commit_all(dir.path(), "rename");
        // R1：纯 rename → 映射一条 (old,new)
        assert_eq!(rename_map(dir.path(), &c1).unwrap(), vec![("old.rs".to_string(), "new.rs".to_string())]);
        // delta：-M 下纯 rename 只产 rename 元数据 diff（不炸整删整加），挂新路径名下
        let delta = changed_files_since(dir.path(), &c1).unwrap();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].path, "new.rs");
        assert!(delta[0].diff.contains("rename from old.rs"), "{}", delta[0].diff);
        assert!(!delta[0].diff.contains("-fn keep"), "纯 rename 不应展开成删加: {}", delta[0].diff);
    }

    #[test]
    fn changed_files_since_content_change_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "line1\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        // HEAD 未动 → 空 delta（守卫④判据）
        assert!(changed_files_since(dir.path(), &c1).unwrap().is_empty());
        std::fs::write(dir.path().join("a.rs"), "line1\nline2\n").unwrap();
        commit_all(dir.path(), "c2");
        let delta = changed_files_since(dir.path(), &c1).unwrap();
        assert_eq!(delta.len(), 1);
        assert!(delta[0].diff.contains("+line2"));
        assert!(rename_map(dir.path(), &c1).unwrap().is_empty(), "内容改动不产 rename 映射");
    }

    #[test]
    fn is_ancestor_true_for_parent_false_after_amend() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        commit_all(dir.path(), "c2");
        assert!(is_ancestor(dir.path(), &c1).unwrap());
        // amend 重写 c2 → 旧 c2 不再是 HEAD 祖先（谱系守卫①判据）
        let out = std::process::Command::new("git").current_dir(dir.path())
            .args(["rev-parse", "HEAD"]).output().unwrap();
        let old_head = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let ok = std::process::Command::new("git").current_dir(dir.path())
            .args(["commit", "-q", "--amend", "-m", "c2-rewritten"]).output().unwrap();
        assert!(ok.status.success());
        assert!(!is_ancestor(dir.path(), &old_head).unwrap(), "amend 后旧 HEAD 非祖先");
    }

    #[test]
    fn branch_files_lists_merge_base_to_head() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("base.rs"), "x\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        // 打 base 分支引用在 c1，之后 main 前进两个 commit
        let ok = std::process::Command::new("git").current_dir(dir.path())
            .args(["branch", "dev0", &c1]).output().unwrap();
        assert!(ok.status.success());
        std::fs::write(dir.path().join("f1.rs"), "a\n").unwrap();
        commit_all(dir.path(), "c2");
        std::fs::write(dir.path().join("f2.rs"), "b\n").unwrap();
        commit_all(dir.path(), "c3");
        let files = branch_files(dir.path(), "dev0").unwrap();
        assert_eq!(files, vec!["f1.rs".to_string(), "f2.rs".to_string()]);
    }

    #[test]
    fn split_diff_by_file_segments_and_takes_b_side_path() {
        let full = "diff --git a/old.rs b/new.rs\nsimilarity index 100%\nrename from old.rs\nrename to new.rs\ndiff --git a/x.rs b/x.rs\nindex 111..222 100644\n--- a/x.rs\n+++ b/x.rs\n@@ -1 +1,2 @@\n line1\n+line2\n";
        let out = split_diff_by_file(full);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, "new.rs", "rename 段挂 b 侧新路径");
        assert!(out[0].diff.contains("rename from old.rs"));
        assert_eq!(out[1].path, "x.rs");
        assert!(out[1].diff.contains("+line2"));
        assert!(split_diff_by_file("").is_empty(), "空 diff → 空 delta");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test rename_map_and_delta changed_files_since_content is_ancestor_true branch_files_lists 2>&1 | tail -5`
Expected: 编译错误（四个函数未定义）。

- [ ] **Step 3: 实现（git.rs，head_sha_at 之后）**

```rust
/// 谱系守卫（增量守卫①）：sha 是否 HEAD 祖先。rebase/reset/amend 后 false
/// （锚定不变式失效——delta 范围不再覆盖「使锚失效的改动」）。
/// merge-base --is-ancestor 以退出码表意（0=是/1=否），不能走 git_at 的 success 判定。
pub fn is_ancestor(dir: &std::path::Path, sha: &str) -> anyhow::Result<bool> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(["merge-base", "--is-ancestor", sha, "HEAD"])
        .output()?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => anyhow::bail!(
            "git merge-base --is-ancestor {sha} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ),
    }
}

/// R1 rename 映射：base..HEAD 的旧→新路径对（-M 相似度检测，status R*）。
/// 相似度 <50% 的 rename 不被 -M 识别 → 不进映射，carry 走 carried_dropped 降级（可审非静默）。
pub fn rename_map(dir: &std::path::Path, base: &str) -> anyhow::Result<Vec<(String, String)>> {
    let out = git_at(dir, &["diff", "--name-status", "-M", &format!("{base}..HEAD")])?;
    Ok(out
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let status = it.next()?;
            if !status.starts_with('R') {
                return None;
            }
            Some((it.next()?.to_string(), it.next()?.to_string()))
        })
        .collect())
}

/// 增量轮 delta（决议 2）：base..HEAD 的 -M diff，按文件切分。
/// **不走既有「逐文件 pathspec 再 diff」模式**：`git diff -M <range> -- <新路径>` 的
/// pathspec 会把旧路径滤出配对域，rename 检测失效 → 纯 rename 膨胀成整文件新增
/// （R1 经济性被击穿）。单次全量 diff + 按 `diff --git` 边界切分则天然保留 rename 元数据。
pub fn changed_files_since(dir: &std::path::Path, base: &str) -> anyhow::Result<Vec<FileChange>> {
    let full = git_at(dir, &["diff", "-M", &format!("{base}..HEAD")])?;
    Ok(split_diff_by_file(&full))
}

/// 按 "diff --git a/X b/Y" 边界切分整份 diff；path 取 b 侧（rename 挂新路径下）。
// ponytail: 路径含 " b/" 子串或引号转义的病态仓不在本项目威胁模型内，rfind 足够；真撞见再上引号解析。
fn split_diff_by_file(full: &str) -> Vec<FileChange> {
    let mut out: Vec<FileChange> = Vec::new();
    for line in full.lines() {
        if let Some(rest) = line.strip_prefix("diff --git a/") {
            let path = rest
                .rfind(" b/")
                .map(|i| rest[i + 3..].to_string())
                .unwrap_or_else(|| rest.to_string());
            out.push(FileChange { path, diff: String::new() });
        }
        if let Some(last) = out.last_mut() {
            last.diff.push_str(line);
            last.diff.push('\n');
        }
    }
    out.retain(|fc| !fc.diff.trim().is_empty());
    out
}

/// 全分支变更文件清单（双层 manifest 第一层）：merge-base(base,HEAD)...HEAD 的 name-only。
pub fn branch_files(dir: &std::path::Path, base: &str) -> anyhow::Result<Vec<String>> {
    let mb = git_at(dir, &["merge-base", base, "HEAD"])?.trim().to_string();
    Ok(dedup_files(&git_at(dir, &["diff", "--name-only", &format!("{mb}...HEAD")])?))
}
```

- [ ] **Step 4: 测试通过 + Commit**

Run: `cargo test --lib evidence 2>&1 | tail -3`（绿后 `cargo test` 全量）

```bash
git add src/evidence/git.rs
git commit -m "feat(incremental): git 层 delta/rename 映射/谱系判定/全分支清单——hermetic 真仓测试"
```

---

### Task 5: finding id 单一真相源提取

**Files:**
- Modify: `src/orchestrate/mod.rs`（enrich 内联逻辑提为 pub fn finding_id）

**Interfaces:**
- Produces: `orchestrate::finding_id(file: &str, quote: &str, title: &str, line_start: u32, line_end: u32) -> String`。Task 7 的 rename 迁移重算 id 依赖它（与 enrich 同源，杜绝两处组成漂移）。
- 不变式: 既有 golden 测试 `id_composition_pinned` / `id_falls_back_to_lines_without_quote` 一字不改仍绿 = 重构无损证明。

- [ ] **Step 1: 实现（orchestrate/mod.rs，normalize_title 之后）**

```rust
/// finding id 组成的单一真相源（R8.1 + M13 rename 迁移共用）：quote 锚定、行无关；
/// 空 quote 退化行号路径（不保证跨 fixup 稳定）。
pub fn finding_id(file: &str, quote: &str, title: &str, line_start: u32, line_end: u32) -> String {
    let nq = normalize_quote(quote);
    let nt = normalize_title(title);
    if nq.is_empty() {
        fnv1a64_hex(&[file, &line_start.to_string(), &line_end.to_string(), &nt])
    } else {
        fnv1a64_hex(&[file, &nq, &nt])
    }
}
```

enrich 的 id 计算段替换为：

```rust
    let id = finding_id(&raw.file, &raw.quote, &raw.title, raw.line_start, raw.line_end);
```

（enrich 内原 `let nq/nt` 与 if/else 删除。）

- [ ] **Step 2: 既有 golden 测试仍绿（重构无损证明）+ Commit**

Run: `cargo test id_composition_pinned id_falls_back id_ignores 2>&1 | tail -3`
Expected: 全绿，零测试改动。

```bash
git add src/orchestrate/mod.rs
git commit -m "refactor(incremental): finding id 组成提为单一真相源 finding_id——rename 迁移重算与 enrich 同源"
```

---

### Task 6: carry 重锚定就近变体（R4）

**Files:**
- Modify: `src/positioning/mod.rs`
- Test: 内联

**Interfaces:**
- Consumes: 既有私有 `safe_read`/`normalize_ws`/`locate_words`。
- Produces: `positioning::anchor_near(root: &Path, file: &str, quote: &str, old_line: u32) -> Option<(u32, u32)>`——Task 7 carry 与 Task 11 claimed-resolved 过滤共用。

- [ ] **Step 1: 写失败测试（positioning tests mod 追加）**

```rust
    #[test]
    fn anchor_near_picks_hit_closest_to_old_line() {
        let dir = tempfile::tempdir().unwrap();
        // 同 quote 出现在第 2 行与第 20 行
        let mut body = String::from("head\nx.unwrap();\n");
        for _ in 0..17 { body.push_str("filler\n"); }
        body.push_str("x.unwrap();\ntail\n");
        write(dir.path(), "a.rs", &body);
        // 上轮锚在 18 行附近 → 就近选 20 行，而非首个命中的 2 行（R4 幽灵 finding 防护）
        assert_eq!(anchor_near(dir.path(), "a.rs", "x.unwrap();", 18), Some((20, 20)));
        // 上轮锚在 3 行附近 → 选 2 行
        assert_eq!(anchor_near(dir.path(), "a.rs", "x.unwrap();", 3), Some((2, 2)));
    }

    #[test]
    fn anchor_near_single_hit_ignores_old_line_and_missing_is_none() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "one\nlet t = secret();\nthree\n");
        // 单一命中：old_line 再离谱也锚到唯一处（零影响承诺）
        assert_eq!(anchor_near(dir.path(), "a.rs", "let t = secret();", 999), Some((2, 2)));
        // quote 消失 → None（调用方收进 carried_dropped）
        assert_eq!(anchor_near(dir.path(), "a.rs", "vanished();", 2), None);
        // 文件消失 → None
        assert_eq!(anchor_near(dir.path(), "gone.rs", "let t = secret();", 2), None);
    }

    #[test]
    fn anchor_near_multiline_quote_spans_from_nearest_first_line() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.rs", "if x {\n    bad();\n}\nmid\nif x {\n    bad();\n}\n");
        // 两处同型块（首行 1 与 5），old_line=6 → 就近选 5，跨度 3 行
        assert_eq!(anchor_near(dir.path(), "a.rs", "if x {\n    bad();\n}", 6), Some((5, 7)));
    }
```

（`write` 复用本文件既有测试 helper；若签名为 `&str` body 直接可用。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test anchor_near 2>&1 | tail -5`
Expected: 编译错误（anchor_near 未定义）。

- [ ] **Step 3: 实现（positioning/mod.rs，locate_words 之后）**

```rust
/// M13 carry 专用定位（R4）：与 locate_quote 同匹配优先级（精确整行 > 子串 > 软换行词序），
/// 差异仅在前两层出现**多命中**时选最接近 old_line 者——carry 语义下首匹配会把 finding
/// 错锁到同文件另一处无辜同文（幽灵 finding 阻 approve）。单一命中与 locate_quote 同结果。
fn locate_quote_near(content: &str, quote: &str, old_line: u32) -> Option<(u32, u32)> {
    let file_lines: Vec<String> = content.lines().map(normalize_ws).collect();
    let raw_quote: Vec<String> = quote.lines().map(normalize_ws).collect();
    let first_idx = raw_quote.iter().position(|l| !l.is_empty())?;
    let last_idx = raw_quote.iter().rposition(|l| !l.is_empty())?;
    let first = &raw_quote[first_idx];
    let exact: Vec<usize> = file_lines.iter().enumerate()
        .filter(|(_, fl)| *fl == first).map(|(i, _)| i).collect();
    let hits: Vec<usize> = if exact.is_empty() {
        file_lines.iter().enumerate()
            .filter(|(_, fl)| fl.contains(first.as_str())).map(|(i, _)| i).collect()
    } else {
        exact
    };
    let hit = match hits.as_slice() {
        [] => return locate_words(content, quote),
        hs => *hs.iter().min_by_key(|&&i| (i as i64 + 1 - i64::from(old_line)).abs())?,
    };
    let ls = (hit as u32) + 1;
    let span = (last_idx - first_idx + 1) as u32;
    let le = (ls + span - 1).min(file_lines.len() as u32).max(ls);
    Some((ls, le))
}

/// carry 重锚定入口：把 quote 定位到当前树（root 约束读取与 anchor() 同防线）。
/// None = 定位失败（调用方收进 carried_dropped 审计，绝不静默）。
pub fn anchor_near(root: &Path, file: &str, quote: &str, old_line: u32) -> Option<(u32, u32)> {
    let content = safe_read(root, file)?;
    locate_quote_near(&content, quote, old_line)
}
```

- [ ] **Step 4: 测试通过 + Commit**

Run: `cargo test --lib positioning 2>&1 | tail -3`（绿后全量 `cargo test`）

```bash
git add src/positioning/mod.rs
git commit -m "feat(incremental): R4 carry 重锚就近变体——多命中选最近旧行，单命中零影响"
```

---

### Task 7: carry 引擎（rename 迁移 R1 + 携带/销案排除/合并去重，P1 交付门槛）

**Files:**
- Create: `src/consolidate/carry.rs`
- Modify: `src/consolidate/mod.rs`（挂载 + re-export）
- Test: carry.rs 内联

**Interfaces:**
- Consumes: `positioning::anchor_near`（Task 6）、`orchestrate::finding_id`（Task 5）、`db::ResolveCard`（Task 3）、`domain::Origin`（Task 1）。
- Produces: `consolidate::{migrate_renames, migrate_resolve_cards, carry, merge_carry, CarryOutcome}`——Task 11 流水线消费。
  - `migrate_renames(&mut [Finding], &[(String, String)])`
  - `migrate_resolve_cards(&mut [ResolveCard], &[(String, String)])`
  - `carry(prior: Vec<Finding>, resolved_ids: &HashSet<String>, root: &Path) -> CarryOutcome`
  - `merge_carry(carried: Vec<Finding>, new: Vec<Finding>) -> Vec<Finding>`
  - `CarryOutcome { carried: Vec<Finding>, carried_dropped: Vec<String>, resolved_applied: Vec<String> }`

- [ ] **Step 1: 建骨架并写失败测试**

`src/consolidate/mod.rs`（现 6 行）改为：

```rust
pub mod carry;
mod correlate;
mod dedup;
mod verify;
pub use carry::{carry, merge_carry, migrate_renames, migrate_resolve_cards, CarryOutcome};
pub use correlate::{apply_clusters, correlate};
pub use dedup::{dedup, dedup_doc};
pub use verify::{refute_targets, verify_findings};
```

（以现文件真实内容为准调整——保持既有导出面不变，只新增 carry 行。）

`src/consolidate/carry.rs` 先只放测试与空实现骨架：

```rust
//! M13 携带引擎（决议 3/5）：上轮合并集 → rename 迁移 → resolved 排除 → 重锚定携带
//! → 与本轮 new 合并去重。纯函数，git/LLM 均不在此层。

use crate::db::ResolveCard;
use crate::domain::{Finding, Origin};
use crate::orchestrate::finding_id;
use std::collections::HashSet;
use std::path::Path;

pub struct CarryOutcome {
    /// 重锚成活的携带项（行号已更新、origin=Carried；severity/agreement/verify/
    /// dismissed 状态原样——refuted/dismissed 本就不 blocking，携带无害）。
    pub carried: Vec<Finding>,
    /// 重锚失败被消灭的 id（使锚失效的改动必在本轮 delta 内，正被审；审计用）。
    pub carried_dropped: Vec<String>,
    /// 命中 resolve 台账被排除携带的 id（销案生效审计）。
    pub resolved_applied: Vec<String>,
}
```

测试（同文件 `#[cfg(test)] mod tests`）：

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;
    use crate::orchestrate::enrich;
    use crate::domain::RawFinding;
    use std::io::Write;

    fn raw(file: &str, quote: &str, title: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()),
        }
    }
    fn f(file: &str, quote: &str, title: &str) -> Finding { enrich(&raw(file, quote, title), "L1", "m") }
    fn write(dir: &Path, rel: &str, body: &str) {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() { std::fs::create_dir_all(parent).unwrap(); }
        std::fs::File::create(p).unwrap().write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn migrate_renames_moves_file_and_recomputes_id() {
        // R1：纯 rename 内容不变 → 迁移后 file=新路径、id 与「模型在新路径重报同问题」一致
        let mut v = vec![f("old.rs", "let x = md5(pw);", "weak hash")];
        migrate_renames(&mut v, &[("old.rs".into(), "new.rs".into())]);
        assert_eq!(v[0].file, "new.rs");
        let reraised = f("new.rs", "let x = md5(pw);", "weak hash");
        assert_eq!(v[0].id, reraised.id, "迁移后 id 必须与新路径重报碰撞（resolved/disputed 机制依赖）");
        // 未涉及 rename 的 finding 不动
        let untouched = f("keep.rs", "q", "t");
        let old_id = untouched.id.clone();
        let mut v2 = vec![untouched];
        migrate_renames(&mut v2, &[("old.rs".into(), "new.rs".into())]);
        assert_eq!((v2[0].file.as_str(), v2[0].id.as_str()), ("keep.rs", old_id.as_str()));
    }

    #[test]
    fn migrate_resolve_cards_same_id_space() {
        let fnd = f("old.rs", "let x = md5(pw);", "weak hash");
        let mut cards = vec![ResolveCard {
            id: fnd.id.clone(), title: "weak hash".into(), file: "old.rs".into(),
            quote: "let x = md5(pw);".into(), severity: "high".into(),
            reason: String::new(), round: "r1".into(), updated_at: String::new(),
        }];
        migrate_resolve_cards(&mut cards, &[("old.rs".into(), "new.rs".into())]);
        let mut fv = vec![fnd];
        migrate_renames(&mut fv, &[("old.rs".into(), "new.rs".into())]);
        assert_eq!(cards[0].id, fv[0].id, "卡与 finding 迁移后必须落在同一 id 空间");
        assert_eq!(cards[0].file, "new.rs");
    }

    #[test]
    fn carry_excludes_resolved_reanchors_and_drops() {
        let dir = tempfile::tempdir().unwrap();
        // 当前树：q1 仍在（行移位到 3）；q2 已被修掉
        write(dir.path(), "a.rs", "// new header\nfn main() {\n    db.write(token);\n}\n");
        let f1 = f("a.rs", "db.write(token);", "unvalidated write"); // 携带成活
        let f2 = f("a.rs", "md5(pw)", "weak hash");                  // quote 消失 → dropped
        let f3 = f("a.rs", "db.write(token);", "already fixed");     // resolved → 排除
        let resolved: HashSet<String> = [f3.id.clone()].into();
        let (id1, id2, id3) = (f1.id.clone(), f2.id.clone(), f3.id.clone());
        let out = carry(vec![f1, f2, f3], &resolved, dir.path());
        assert_eq!(out.carried.len(), 1);
        assert_eq!(out.carried[0].id, id1);
        assert_eq!(out.carried[0].origin, Origin::Carried);
        assert_eq!((out.carried[0].line_start, out.carried[0].line_end), (3, 3), "行号随重锚更新");
        assert_eq!(out.carried_dropped, vec![id2]);
        assert_eq!(out.resolved_applied, vec![id3]);
    }

    #[test]
    fn merge_carry_same_id_keeps_carried_no_agreement_bump() {
        // 决议 5：re-raise 不涨 agreement——跨轮重看非独立证据
        let mut carried = f("a.rs", "q", "t");
        carried.origin = Origin::Carried;
        carried.agreement = 2;
        carried.models = vec!["alpha".into(), "beta".into()];
        let mut reraise = f("a.rs", "q", "t"); // 同 id
        reraise.models = vec!["gamma".into()];
        let brand_new = f("a.rs", "other quote", "other issue");
        let new_id = brand_new.id.clone();
        let merged = merge_carry(vec![carried], vec![reraise, brand_new]);
        assert_eq!(merged.len(), 2);
        let kept = merged.iter().find(|x| x.origin == Origin::Carried).unwrap();
        assert_eq!(kept.agreement, 2, "agreement 不变（单轮内多模型互证原语义）");
        assert!(kept.models.contains(&"gamma".to_string()), "models 并集记录");
        let fresh = merged.iter().find(|x| x.id == new_id).unwrap();
        assert_eq!(fresh.origin, Origin::New);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test --lib consolidate::carry 2>&1 | tail -5`
Expected: 编译错误（四个函数未实现）。

- [ ] **Step 3: 实现（carry.rs 骨架下方）**

```rust
/// R1 rename 迁移：finding.file 命中旧路径 → 改新路径并按 finding_id 单一真相源重算 id。
/// 不迁则「旧路径读不到 → carry 被杀 + 新 id 不碰撞」——真问题静默蒸发（评审 R1 案）。
pub fn migrate_renames(findings: &mut [Finding], renames: &[(String, String)]) {
    if renames.is_empty() {
        return;
    }
    for f in findings.iter_mut() {
        if let Some((_, new)) = renames.iter().find(|(old, _)| *old == f.file) {
            f.file = new.clone();
            f.id = finding_id(&f.file, &f.quote, &f.title, f.line_start, f.line_end);
        }
    }
}

/// 同规则迁移 resolve 卡——碰撞域（resolved 排除 / resolution_disputed 检测）与
/// findings 必须同一 id 空间。空 quote 卡不重算（无 quote 的 id 含行号，快照行号在
/// 新树无意义——保守保留旧 id；code 模式 quote 是必填契约，实际不会出现）。
pub fn migrate_resolve_cards(cards: &mut [ResolveCard], renames: &[(String, String)]) {
    if renames.is_empty() {
        return;
    }
    for c in cards.iter_mut() {
        if let Some((_, new)) = renames.iter().find(|(old, _)| *old == c.file) {
            c.file = new.clone();
            if !c.quote.trim().is_empty() {
                c.id = finding_id(&c.file, &c.quote, &c.title, 0, 0);
            }
        }
    }
}

/// 决议 3 携带主流程（调用方须先做 rename 迁移）：resolved 排除 → anchor_near 重锚
/// （成活更新行号 + origin=Carried；失败进 carried_dropped）。输出 id 列表排序去重（确定性）。
pub fn carry(prior: Vec<Finding>, resolved_ids: &HashSet<String>, root: &Path) -> CarryOutcome {
    let mut out = CarryOutcome { carried: vec![], carried_dropped: vec![], resolved_applied: vec![] };
    for mut f in prior {
        if resolved_ids.contains(&f.id) {
            out.resolved_applied.push(f.id);
            continue;
        }
        match crate::positioning::anchor_near(root, &f.file, &f.quote, f.line_start) {
            Some((ls, le)) => {
                f.line_start = ls;
                f.line_end = le;
                f.origin = Origin::Carried;
                out.carried.push(f);
            }
            None => out.carried_dropped.push(f.id),
        }
    }
    for v in [&mut out.carried_dropped, &mut out.resolved_applied] {
        v.sort();
        v.dedup();
    }
    out
}

/// 决议 5 合并去重：同 id 保 carried、丢 new 副本、models 并集、**agreement 不变**
/// （跨轮重看非独立证据；顽固性由「仍在携带集」这一事实表达）。new 独有项原样进合并集。
pub fn merge_carry(carried: Vec<Finding>, new: Vec<Finding>) -> Vec<Finding> {
    let mut out = carried;
    for n in new {
        if let Some(c) = out.iter_mut().find(|c| c.id == n.id) {
            for m in &n.models {
                if !c.models.contains(m) {
                    c.models.push(m.clone());
                }
            }
        } else {
            out.push(n);
        }
    }
    out
}
```

- [ ] **Step 4: 测试通过 + Commit**

Run: `cargo test 2>&1 | tail -3`
Expected: 全绿。

```bash
git add src/consolidate/
git commit -m "feat(incremental): carry 引擎——R1 rename 迁移(file+id)+销案排除+就近重锚+同 id 合并不涨 agreement"
```

---

### Task 8: refute 只打 origin=new

**Files:**
- Modify: `src/consolidate/verify.rs`（refute_targets）
- Test: 内联

**Interfaces:**
- Consumes: Task 1 的 `Origin`。
- Produces: `refute_targets` 行为收窄——carried 项永不进 refute targets（执行与 phase 打点共用同一函数，计数自动一致）。签名不变。

- [ ] **Step 1: 写失败测试（verify.rs tests mod 追加）**

```rust
    #[test]
    fn carried_findings_never_enter_refute_targets() {
        use crate::domain::Origin;
        // 决议 5：携带项出生轮已过孤证 refute + 判例预筛，重复烧 LLM 纯浪费；
        // 台账精确 id 豁免（mark_dismissed）每轮照常、免费，不受影响。
        let mut carried = finding("lone blocking", Severity::High, 1, "alpha");
        carried.origin = Origin::Carried;
        let fresh = finding("new lone blocking", Severity::High, 1, "alpha");
        let cards = vec![card("ffffffffffffffff", "a.rs", "lone blocking")]; // 判例变体也不打 carried
        let targets = refute_targets(&[carried, fresh], &cards, Severity::Medium);
        assert_eq!(targets, vec![1], "只有 origin=new 的孤证进 targets（carried 双路径均剔除）");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test carried_findings_never 2>&1 | tail -5`
Expected: FAIL——targets 为 `[0, 1]`（carried 未被剔除）。

- [ ] **Step 3: 实现（refute_targets 过滤链首行加一条）**

```rust
        .filter(|(_, f)| f.origin != crate::domain::Origin::Carried) // M13：refute 只打 new
```

（插在 `.filter(|(_, f)| !exact_ids.contains(...))` 之前；同时函数 doc 注释补一句：「M13：origin=carried 不进 targets——出生轮已过孤证 refute 与判例预筛」。）

- [ ] **Step 4: 测试通过 + Commit**

Run: `cargo test 2>&1 | tail -3`

```bash
git add src/consolidate/verify.rs
git commit -m "feat(incremental): refute targets 剔除 origin=carried——携带项出生轮已验，不重复烧 LLM"
```

---

### Task 9: `--incremental-from` flag + 四守卫（R2 健康基线，P1 交付门槛）

**Files:**
- Modify: `src/cli.rs`（ReviewArgs 字段 + IncrementalBaseline + incremental_preflight）
- Modify: `src/main.rs`（clap flag + args 组装）
- Test: cli.rs 新增 `#[cfg(test)] mod incremental_tests`

**Interfaces:**
- Consumes: Task 1 ReviewReport Deserialize、Task 4 git 函数、`state::store::{read_result, read_args_raw, read_manifest, read_partial}`。
- Produces: `ReviewArgs.incremental_from: Option<String>`；`pub(crate) struct IncrementalBaseline { base_sha: String, prior_findings: Vec<Finding>, renames: Vec<(String, String)>, delta: Vec<FileChange> }`；`pub(crate) fn incremental_preflight(args: &ReviewArgs, prior_job: &str, root: &Path) -> anyhow::Result<IncrementalBaseline>`——Task 11 流水线第一步。

- [ ] **Step 1: ReviewArgs 加字段（cli.rs）+ clap flag（main.rs）**

ReviewArgs 尾部（no_dismissed 之后）：

```rust
    /// M13：`--incremental-from <job-id>`——以该作业为基线的增量轮。serde default 兼容旧 args.json。
    #[serde(default)]
    pub incremental_from: Option<String>,
}
```

main.rs Review 变体（no_dismissed 之后、targets 之前）：

```rust
        /// 增量轮（M13）：以指定作业的评审为基线，只审其 stamped HEAD 以来已提交的 fix delta，
        /// 未动代码的 findings 重锚定携带。仅 code 模式 + branch scope（须 --base）；上轮须干净
        /// 全量（非 incomplete、无 failed_models、经 --background 持久化）。守卫违例显式报错，
        /// 不静默降级全量。
        #[arg(long)]
        incremental_from: Option<String>,
```

main() 的 Review 解构与 `let args = ReviewArgs {...}` 组装各加 `incremental_from`；随后编译驱动补全仓 ReviewArgs 字面量（cli.rs 测试夹具 doc_args/args()/empty_review_args/resume 测试等约 8 处）加 `incremental_from: None,`。

Run: `cargo build 2>&1 | grep -c error` 直至 0；`cargo test 2>&1 | tail -3` 全绿（纯加字段无行为变化）。

- [ ] **Step 2: 写守卫失败测试（cli.rs 新 mod）**

测试基建：tempdir 内造 git 仓 + proj.db 固件（上轮 result/args/manifest/cells）。

```rust
#[cfg(test)]
mod incremental_tests {
    use super::*;
    use crate::domain::{Mode, RawFinding, Severity, Verdict};
    use crate::orchestrate::enrich;
    use crate::state::store::{append_partial, write_args_raw, write_manifest, CellInput, CellRecord};

    pub(super) fn git_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@t"]);
        run(&["config", "user.name", "t"]);
        run(&["config", "commit.gpgsign", "false"]);
    }
    pub(super) fn commit_all(dir: &std::path::Path, msg: &str) -> String {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git").current_dir(dir).args(args).output().unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", msg]);
        run(&["rev-parse", "HEAD"]).trim().to_string()
    }

    pub(super) fn raw(file: &str, quote: &str, title: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(), file: file.into(),
            line_start: 1, line_end: 1, confidence: 0.9, recommendation: "r".into(),
            hard_constraint: false, quote: quote.into(), lens: Some("L1".into()),
        }
    }

    /// 增量 args（branch scope + code mode）。
    pub(super) fn inc_args(base: &str, prior: &str) -> ReviewArgs {
        ReviewArgs {
            mode: Some("code".into()), base: Some(base.into()), scope: "branch".into(),
            spec: vec![], focus: None, lenses: Some("L1".into()), models: Some("m".into()),
            models_all: false, no_correlate: true, block_threshold: None, conf_floor: None,
            background: false, format: "json".into(), targets: vec![], resume: None,
            dismissed: None, no_dismissed: false, incremental_from: Some(prior.into()),
        }
    }

    /// 上轮固件：result(v3, approve, head_sha=Some(sha)) + args(branch/code) + manifest/partial 全 ok。
    pub(super) fn write_prior(root: &std::path::Path, job: &str, head_sha: &str, findings: Vec<Finding>) {
        let stats = Stats::from_findings(&findings);
        let report = ReviewReport {
            schema_version: "3".into(), mode: Mode::Code, verdict: Verdict::Approve,
            stats, findings, clusters: vec![],
            run: RunMeta {
                job_id: job.into(), models: vec!["m".into()], lenses: vec!["L1".into()],
                head_sha: Some(head_sha.into()), ..Default::default()
            },
        };
        crate::state::store::write_result(root, job, &serde_json::to_string(&report).unwrap()).unwrap();
        let prior_args = ReviewArgs { incremental_from: None, background: true, ..inc_args("dev0", "unused") };
        write_args_raw(root, job, &serde_json::to_string(&prior_args).unwrap()).unwrap();
        write_manifest(root, job, &[CellInput { cell: "bundle[a.rs]@m".into(), model: "m".into(), prompt: "p".into() }]).unwrap();
        append_partial(root, job, &CellRecord::ok("bundle[a.rs]@m", "m", vec![])).unwrap();
    }

    #[test]
    fn preflight_happy_path_yields_baseline() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        let ok = std::process::Command::new("git").current_dir(dir.path())
            .args(["branch", "dev0", &c1]).output().unwrap();
        assert!(ok.status.success());
        write_prior(dir.path(), "jprev", &c1, vec![enrich(&raw("a.rs", "db.write(token);", "unvalidated write"), "L1", "m")]);
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n    extra();\n}\n").unwrap();
        commit_all(dir.path(), "fix");
        let base = incremental_preflight(&inc_args("dev0", "jprev"), "jprev", dir.path()).unwrap();
        assert_eq!(base.base_sha, c1);
        assert_eq!(base.prior_findings.len(), 1);
        assert_eq!(base.delta.len(), 1, "delta 只含 c1..HEAD 改动");
        assert!(base.renames.is_empty());
    }

    /// 五类守卫拒绝：每类断言错误文案关键词（文案即裸终端在场文档）。
    #[test]
    fn preflight_rejects_unhealthy_and_mismatched_baselines() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        write_prior(dir.path(), "jok", &c1, vec![]);
        // ④空 delta（HEAD 未动）
        let e = incremental_preflight(&inc_args("dev0", "jok"), "jok", dir.path()).unwrap_err();
        assert!(e.to_string().contains("empty delta"), "{e}");
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        commit_all(dir.path(), "c2");
        // 上轮无 result
        let e = incremental_preflight(&inc_args("dev0", "ghost"), "ghost", dir.path()).unwrap_err();
        assert!(e.to_string().contains("no persisted result"), "{e}");
        // ③R2 健康基线：incomplete
        let mut bad: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jok").unwrap()).unwrap();
        bad.verdict = Verdict::Incomplete;
        crate::state::store::write_result(dir.path(), "jincomplete", &serde_json::to_string(&bad).unwrap()).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jincomplete"), "jincomplete", dir.path()).unwrap_err();
        assert!(e.to_string().contains("incomplete"), "{e}");
        // ③R2：failed_models 非空
        let mut half: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jok").unwrap()).unwrap();
        half.verdict = Verdict::Approve;
        half.run.failed_models = vec!["beta".into()];
        crate::state::store::write_result(dir.path(), "jhalf", &serde_json::to_string(&half).unwrap()).unwrap();
        crate::state::store::write_args_raw(dir.path(), "jhalf", &crate::state::store::read_args_raw(dir.path(), "jok").unwrap()).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jhalf"), "jhalf", dir.path()).unwrap_err();
        assert!(e.to_string().contains("failed models"), "{e}");
        // ③R2：manifest 有 cell 无 ok 记录
        let mut ok2: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jok").unwrap()).unwrap();
        ok2.run.failed_models.clear();
        crate::state::store::write_result(dir.path(), "jpartial", &serde_json::to_string(&ok2).unwrap()).unwrap();
        crate::state::store::write_args_raw(dir.path(), "jpartial", &crate::state::store::read_args_raw(dir.path(), "jok").unwrap()).unwrap();
        crate::state::store::write_manifest(dir.path(), "jpartial", &[
            crate::state::store::CellInput { cell: "c-never-ran".into(), model: "m".into(), prompt: "p".into() },
        ]).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jpartial"), "jpartial", dir.path()).unwrap_err();
        assert!(e.to_string().contains("without an ok record"), "{e}");
        // 无 stamp（v2 老报告）
        let mut nostamp: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jok").unwrap()).unwrap();
        nostamp.run.head_sha = None;
        crate::state::store::write_result(dir.path(), "jnostamp", &serde_json::to_string(&nostamp).unwrap()).unwrap();
        crate::state::store::write_args_raw(dir.path(), "jnostamp", &crate::state::store::read_args_raw(dir.path(), "jok").unwrap()).unwrap();
        crate::state::store::write_manifest(dir.path(), "jnostamp", &[]).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jnostamp"), "jnostamp", dir.path()).unwrap_err();
        assert!(e.to_string().contains("no head_sha stamp"), "{e}");
    }

    #[test]
    fn preflight_rejects_scope_mode_and_lineage_violations() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        write_prior(dir.path(), "jprev", &c1, vec![]);
        std::fs::write(dir.path().join("a.rs"), "x\ny\n").unwrap();
        commit_all(dir.path(), "c2");
        // v1 范围：doc 模式拒
        let mut doc_args = inc_args("dev0", "jprev");
        doc_args.mode = Some("doc".into());
        doc_args.targets = vec!["spec.md".into()];
        let e = incremental_preflight(&doc_args, "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("code mode only"), "{e}");
        // v1 范围：working-tree scope 拒（不可 stamp）
        let mut wt = inc_args("dev0", "jprev");
        wt.scope = "working-tree".into();
        wt.base = None;
        let e = incremental_preflight(&wt, "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("branch scope"), "{e}");
        // ②同族：上轮 scope 与本轮不一致
        let mut prior_wt = ReviewArgs { scope: "working-tree".into(), base: None, ..inc_args("dev0", "x") };
        prior_wt.incremental_from = None;
        crate::state::store::write_args_raw(dir.path(), "jprev", &serde_json::to_string(&prior_wt).unwrap()).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jprev"), "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("mode/scope mismatch"), "{e}");
        // 恢复同族后：①谱系——stamp 指向不在祖先链上的假 sha
        let good_args = ReviewArgs { incremental_from: None, background: true, ..inc_args("dev0", "x") };
        crate::state::store::write_args_raw(dir.path(), "jprev", &serde_json::to_string(&good_args).unwrap()).unwrap();
        let mut rebased: ReviewReport = serde_json::from_str(&crate::state::store::read_result(dir.path(), "jprev").unwrap()).unwrap();
        rebased.run.head_sha = Some("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".into());
        crate::state::store::write_result(dir.path(), "jprev", &serde_json::to_string(&rebased).unwrap()).unwrap();
        let e = incremental_preflight(&inc_args("dev0", "jprev"), "jprev", dir.path()).unwrap_err();
        assert!(e.to_string().contains("not an ancestor"), "{e}");
    }
}
```

注：`is_ancestor` 对不存在的 sha 会走 `_ => bail`（git 报 fatal），错误文案不同——实现时把 bail 的 Err 分支也折成谱系守卫文案（见 Step 4 实现）。

- [ ] **Step 3: 跑测试确认失败**

Run: `cargo test incremental_tests 2>&1 | tail -5`
Expected: 编译错误（incremental_preflight/IncrementalBaseline 未定义）。

- [ ] **Step 4: 实现（cli.rs，partial_report 之前）**

```rust
/// M13 增量基线（incremental_preflight 产物）。
pub(crate) struct IncrementalBaseline {
    /// 上轮 stamped HEAD（delta base）。
    pub base_sha: String,
    /// 上轮合并集（含 refuted/dismissed，原样携带候选）。
    pub prior_findings: Vec<Finding>,
    /// R1 旧→新路径映射。
    pub renames: Vec<(String, String)>,
    /// base..HEAD 逐文件 diff。
    pub delta: Vec<crate::evidence::git::FileChange>,
}

/// 决议 7 前置守卫：违反**显式报错拒跑**（不静默降级全量——成本意外）。
/// ①谱系 ②同族（mode/scope，含 v1 范围收窄 code+branch）③健康基线（R2 三查）④空 delta。
pub(crate) fn incremental_preflight(
    args: &ReviewArgs,
    prior_job: &str,
    root: &Path,
) -> anyhow::Result<IncrementalBaseline> {
    // v1 范围（决议 7）：仅 code 模式——doc id 稳定性弱，锚定携带地基不存在。
    let mode = detect_mode_from_args(args)?;
    anyhow::ensure!(
        mode == Mode::Code,
        "--incremental-from: v1 supports code mode only (doc finding ids drift across rounds); run a full `redteam review`"
    );
    // v1 范围：branch scope——working-tree 证据不可 stamp，轮间未提交改动会静默漏出 delta。
    anyhow::ensure!(
        args.scope == "branch",
        "--incremental-from requires branch scope (pass --base <branch>): the delta only covers committed changes since the prior round's stamped HEAD"
    );

    // 载上轮 result（增量只对经 --background 持久化过 result 的作业有意义）。
    let raw = crate::state::store::read_result(root, prior_job).ok_or_else(|| anyhow::anyhow!(
        "--incremental-from: no persisted result for job {prior_job} (prior round must have run via --background)"
    ))?;
    let prior: ReviewReport = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("--incremental-from: cannot parse prior result for {prior_job}: {e}"))?;

    // ③健康基线守卫（R2）：锚定不变式只对「上轮全量审干净」的基线成立。
    anyhow::ensure!(
        prior.verdict != Verdict::Incomplete,
        "--incremental-from: prior round {prior_job} is incomplete (unreviewed bundles have no findings to carry); run a full `redteam review` to establish a clean baseline"
    );
    anyhow::ensure!(
        prior.run.failed_models.is_empty(),
        "--incremental-from: prior round {prior_job} had failed models {:?} (adversarial coverage was incomplete); run a full `redteam review`",
        prior.run.failed_models
    );
    let manifest = crate::state::store::read_manifest(root, prior_job);
    let ok_cells: std::collections::BTreeSet<String> = crate::state::store::read_partial(root, prior_job)
        .into_iter()
        .filter(|r| r.ok)
        .map(|r| r.cell)
        .collect();
    anyhow::ensure!(
        manifest.iter().all(|c| ok_cells.contains(&c.cell)),
        "--incremental-from: prior round {prior_job} has manifest cells without an ok record (partial coverage); run a full `redteam review`"
    );

    // stamp 在场（v2 老报告无 head_sha——先跑一次全量建立基线）。
    let base_sha = prior.run.head_sha.clone().ok_or_else(|| anyhow::anyhow!(
        "--incremental-from: prior result has no head_sha stamp (predates incremental rounds); run one full `redteam review` to establish a stamped baseline"
    ))?;

    // ②同族守卫：mode/scope 与上轮一致（读上轮 args.json）。
    let prior_args: ReviewArgs = crate::state::store::read_args_raw(root, prior_job)
        .and_then(|j| serde_json::from_str(&j).ok())
        .ok_or_else(|| anyhow::anyhow!("--incremental-from: cannot load prior job args for {prior_job}"))?;
    anyhow::ensure!(
        detect_mode_from_args(&prior_args).ok() == Some(Mode::Code) && prior_args.scope == args.scope,
        "--incremental-from: mode/scope mismatch with prior round (prior scope '{}', current '{}'); incremental carry requires the same review family",
        prior_args.scope,
        args.scope
    );

    // ①谱系守卫：stamped HEAD 必须是当前 HEAD 祖先（rebase/reset 即锚定不变式失效）。
    // is_ancestor 的硬错误（如 sha 不存在于对象库）同属谱系失效，折同一文案。
    let lineage_ok = crate::evidence::git::is_ancestor(root, &base_sha).unwrap_or(false);
    anyhow::ensure!(
        lineage_ok,
        "--incremental-from: prior stamped HEAD {base_sha} is not an ancestor of HEAD (rebase/reset detected — the anchoring invariant no longer holds); run a full `redteam review`"
    );

    // ④空 delta：HEAD 未动 → 显式报错（ship 按停滞守卫处理，不烧空轮）。
    let renames = crate::evidence::git::rename_map(root, &base_sha)?;
    let delta = crate::evidence::git::changed_files_since(root, &base_sha)?;
    anyhow::ensure!(
        !delta.is_empty(),
        "--incremental-from: empty delta since {base_sha} (HEAD unchanged — nothing new to review); commit fixes first, or run a full `redteam review`"
    );

    Ok(IncrementalBaseline { base_sha, prior_findings: prior.findings, renames, delta })
}
```

- [ ] **Step 5: 测试通过 + Commit**

Run: `cargo test 2>&1 | tail -3`
Expected: 全绿。

```bash
git add src/cli.rs src/main.rs
git commit -m "feat(incremental): --incremental-from flag + 四守卫——R2 健康基线/谱系/同族(code+branch)/空 delta 显式拒跑"
```

---

### Task 10: `<incremental_round>` prompt 段 + 双层 manifest

**Files:**
- Modify: `src/prompt/templates/adversarial-code-bundle.md`（占位）
- Modify: `src/prompt/mod.rs`（render_incremental_section + build_bundle_prompt 参数）
- Modify: `src/bundle/mod.rs`（cap_entries 提取 + two_layer_manifest）
- Modify: `src/orchestrate/mod.rs`（run_code_fanout 穿参）
- Modify: `src/cli.rs`（run_code_review 调用点传 `""`）
- Test: prompt/mod.rs + bundle/mod.rs 内联

**Interfaces:**
- Consumes: Task 3 `db::ResolveCard`。
- Produces: `prompt::render_incremental_section(delta_base: &str, two_layer_manifest: &str, claimed: &[db::ResolveCard]) -> String`；`build_bundle_prompt(target_label, bundle, rules, user_focus, other_files, incremental: &str)`（第 6 参）；`run_code_fanout(provider, target_label, bundles, rules, focus, incremental: &str, opts, sink)`（第 6 参）；`bundle::two_layer_manifest(all_files: &[String], delta: &BTreeSet<String>) -> String`。
- 设计钉子（决议 4）: rules/lenses/对抗立场/output contract **一字不动**；不注入 hunk→finding 映射；claimed 项带**原样 title+quote 全文**（机器匹配 = 模型按原 title+quote 再报 → 同 id 碰撞）。

- [ ] **Step 1: 写失败测试**

prompt/mod.rs tests 追加：

```rust
    #[test]
    fn incremental_section_renders_only_when_given() {
        use crate::bundle::Bundle;
        use crate::evidence::git::FileChange;
        use crate::lens::by_id;
        let bundle = Bundle { files: vec![FileChange { path: "src/a.rs".into(), diff: "+x".into() }] };
        let rules = vec![by_id("L1").unwrap()];
        // 全量路径（空段）：模板无残留占位、无增量标记
        let p = build_bundle_prompt("wt", &bundle, &rules, "", "", "");
        assert!(!p.contains("{{"), "no residual placeholders");
        assert!(!p.contains("<incremental_round>"), "全量评审不得出现增量段");
        // 增量路径：段完整注入
        let claimed = vec![crate::db::ResolveCard {
            id: "a1b2c3d4e5f60718".into(), title: "weak hash".into(), file: "src/a.rs".into(),
            quote: "let x = md5(pw);".into(), severity: "high".into(),
            reason: String::new(), round: "r1".into(), updated_at: String::new(),
        }];
        let sect = render_incremental_section("abc123def456", "src/a.rs [delta], src/b.rs", &claimed);
        let pi = build_bundle_prompt("wt", &bundle, &rules, "", "", &sect);
        assert!(pi.contains("<incremental_round>") && pi.contains("abc123def456"));
        assert!(pi.contains("src/a.rs [delta]"), "双层 manifest 注入");
        assert!(pi.contains("let x = md5(pw);") && pi.contains("weak hash"), "claimed 项带原样 title+quote");
        assert!(pi.contains("EXACT original title and quote"), "机器匹配指令在场");
        assert!(pi.contains("[L1]"), "rules 一字不动仍在");
    }

    #[test]
    fn incremental_section_empty_claimed_and_overflow_cap() {
        let s = render_incremental_section("abc", "f.rs [delta]", &[]);
        assert!(s.contains("(none)"), "{s}");
        // 溢出：超总帽的 claimed 项截断并注记条数（静默截断=覆盖假象）
        let big: Vec<crate::db::ResolveCard> = (0..40).map(|i| crate::db::ResolveCard {
            id: format!("{i:016x}"), title: "t".into(), file: "f.rs".into(),
            quote: "q".repeat(400), severity: "high".into(),
            reason: String::new(), round: "r1".into(), updated_at: String::new(),
        }).collect();
        let s = render_incremental_section("abc", "", &big);
        assert!(s.contains("more claimed-resolved items (not listed)"), "{s}");
        assert!(s.chars().count() < 9000, "总量帽生效: {}", s.chars().count());
    }
```

bundle/mod.rs tests 追加：

```rust
    #[test]
    fn two_layer_manifest_marks_delta_membership_and_caps() {
        use std::collections::BTreeSet;
        let all = vec!["src/a.rs".to_string(), "src/b.rs".into(), "src/c.rs".into()];
        let delta: BTreeSet<String> = ["src/b.rs".to_string()].into();
        let s = two_layer_manifest(&all, &delta);
        assert!(s.contains("src/b.rs [delta]"), "{s}");
        assert!(s.contains("src/a.rs") && !s.contains("src/a.rs [delta]"), "{s}");
        // 复用双帽：大清单截断注记
        let many: Vec<String> = (0..500).map(|i| format!("src/file{i:04}.rs")).collect();
        let s = two_layer_manifest(&many, &BTreeSet::new());
        assert!(s.contains("more files"), "{s}");
        assert!(s.chars().count() <= 4000, "{}", s.chars().count());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test incremental_section two_layer_manifest 2>&1 | tail -5`
Expected: 编译错误。

- [ ] **Step 3: 实现（bundle/mod.rs）**

`other_files_manifest` 的双帽截断循环（`// 双帽：…` 起到函数尾）整体提取为：

```rust
/// 双帽截断（条目数 + 总字符预算，先到者截断，尾注被截条数）——
/// other_files_manifest 与 two_layer_manifest 共用同一体积防线。
fn cap_entries(entries: Vec<String>) -> String {
    let total = entries.len();
    let mut kept = 0usize;
    let mut chars = 0usize;
    for e in &entries {
        if kept >= MANIFEST_MAX_ENTRIES
            || chars + e.len() + 2 > MANIFEST_MAX_CHARS - MANIFEST_SUFFIX_RESERVE
        {
            break;
        }
        chars += e.len() + 2;
        kept += 1;
    }
    let mut entries = entries;
    entries.truncate(kept);
    let mut s = entries.join(", ");
    let extra = total - kept;
    if extra > 0 {
        if s.is_empty() {
            s = format!("… and {extra} more files");
        } else {
            s.push_str(&format!(", … and {extra} more files"));
        }
    }
    s
}
```

`other_files_manifest` 尾段改为 `cap_entries(entries)`；其后新增：

```rust
/// M13 双层 manifest（决议 4）：全分支变更文件清单 + 标注哪些在本轮 delta 内——
/// 压增量轮的视野塌缩型「无证据实现」误报。体积防线复用双帽。
pub fn two_layer_manifest(all_files: &[String], delta: &std::collections::BTreeSet<String>) -> String {
    let entries: Vec<String> = all_files
        .iter()
        .map(|p| if delta.contains(p) { format!("{p} [delta]") } else { p.clone() })
        .collect();
    cap_entries(entries)
}
```

- [ ] **Step 4: 实现（模板 + prompt/mod.rs）**

`adversarial-code-bundle.md`：`</output_contract>` 与 `<bundle>` 之间插入一行：

```
{{INCREMENTAL_ROUND}}
```

`build_bundle_prompt` 加第 6 参 `incremental: &str`，render vars 追加 `("{{INCREMENTAL_ROUND}}", incremental)`（与既有键互不为前缀，debug_assert 自证）。既有 prompt 测试调用点补 `""`（约 4 处）。

`render_precedents` 之后新增：

```rust
/// M13 决议 4：<incremental_round> 段——仅增量轮渲染（全量路径传空串填占位）。
/// 内容四件：delta 口径 / 双层 manifest / claimed-resolved 顺带验证清单 / 原样再报指令。
/// 不注入 hunk→finding 映射（fix 意图不注入，防橡皮图章化）。claimed 项必须带原样
/// title+quote **全文**——机器匹配 = 模型按原文再报 → enrich 同 id → 与 resolve 记录碰撞。
pub fn render_incremental_section(
    delta_base: &str,
    two_layer_manifest: &str,
    claimed: &[crate::db::ResolveCard],
) -> String {
    const CLAIMED_TOTAL_CHARS: usize = 6000;
    let mut list = String::new();
    let mut listed = 0usize;
    for c in claimed {
        let entry = format!(
            "- [{}] {} | title: {}\n  original quote (verbatim):\n```\n{}\n```\n",
            c.id, c.file, c.title, c.quote
        );
        if list.chars().count() + entry.chars().count() > CLAIMED_TOTAL_CHARS {
            break;
        }
        list.push_str(&entry);
        listed += 1;
    }
    if listed < claimed.len() {
        list.push_str(&format!(
            "… and {} more claimed-resolved items (not listed)\n",
            claimed.len() - listed
        ));
    }
    if claimed.is_empty() {
        list = "(none)\n".into();
    }
    format!(
        "<incremental_round>\n\
         This is an INCREMENTAL round: the diffs below are a fix delta (since {delta_base}) on top of a changeset already adversarially reviewed in earlier rounds. Attack the delta itself with full force — fixes routinely introduce new problems. Unchanged files were already reviewed; do not re-litigate them unless the delta interacts with them.\n\
         Full changeset files ([delta] = changed in THIS round; others are unchanged since the prior round):\n\
         {two_layer_manifest}\n\
         Claimed-resolved findings from earlier rounds — verify incidentally while reading code you already visit (this is NOT your main task):\n\
         {list}\
         For each claimed-resolved item: if the underlying problem is NOT actually fixed, report it again using the EXACT original title and quote verbatim (that is how it is machine-matched). If it is genuinely fixed, do not report it.\n\
         </incremental_round>"
    )
}
```

- [ ] **Step 5: 穿线 orchestrate（run_code_fanout 加参）**

`run_code_fanout` 签名 `focus: &str,` 后加 `incremental: &str,`；cells 构造处 `build_bundle_prompt(target_label, bundle, rules, focus, &others)` 改为 `build_bundle_prompt(target_label, bundle, rules, focus, &others, incremental)`。编译驱动修调用点：cli.rs `run_code_review` 传 `""`；orchestrate tests 全部补 `""`（约 7 处）。

- [ ] **Step 6: 全量测试 + Commit**

Run: `cargo test 2>&1 | tail -3`
Expected: 全绿（既有 `bundle_prompt_interpolates…`/`all_adversarial_templates…` 测试不改仍绿——rules/contract 未动的机器证明）。

```bash
git add src/prompt/ src/bundle/mod.rs src/orchestrate/mod.rs src/cli.rs
git commit -m "feat(incremental): <incremental_round> 条件段——双层 manifest+claimed-resolved 顺带验证，rules/contract 一字不动"
```

---

### Task 11: run_incremental_review 流水线 + dispatch 路由 + 假修复靶

**Files:**
- Modify: `src/cli.rs`（run_incremental_review、run_review_dispatch 路由、run_resume 增量拒绝）
- Test: `incremental_tests` mod 追加三个 e2e（全链路 FakeProvider）

**Interfaces:**
- Consumes: Tasks 1-10 全部产物。
- Produces: `pub async fn run_incremental_review(args: &ReviewArgs, prior_job: &str, provider: &dyn ReviewProvider, cfg: &Config, root: &Path, sink: Option<&dyn CellSink>) -> anyhow::Result<(ReviewReport, i32)>`；dispatch 起手路由（前台/后台 job 自动生效）。

- [ ] **Step 1: 写失败测试（incremental_tests 追加）**

```rust
    use crate::provider::fake::FakeProvider;

    fn inc_cfg() -> Config {
        Config::from_layers(&[r#"[providers.m]
kind="openai"
model="m1"
priority=10
[review]
default_models=["m"]
"#]).unwrap()
    }

    /// 主链路：F1 携带（quote 存活）、F2 消灭（quote 被修掉）、新 finding 入场 → v3 报告全字段。
    #[tokio::test]
    async fn incremental_carries_drops_and_reports_v3() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n    md5(pw);\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        let f1 = enrich(&raw("a.rs", "db.write(token);", "unvalidated write"), "L1", "m");
        let f2 = enrich(&raw("a.rs", "md5(pw);", "weak hash"), "L1", "m");
        let (id1, id2) = (f1.id.clone(), f2.id.clone());
        write_prior(dir.path(), "jprev", &c1, vec![f1, f2]);
        // fix：修掉 md5 行，引入新问题行
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n    sha256(pw);\n    leak(secret);\n}\n").unwrap();
        commit_all(dir.path(), "fix r1");
        // 本轮模型只报新问题
        let provider = FakeProvider::with("m", vec![raw("a.rs", "leak(secret);", "secret leak")]);
        let (report, code) = run_incremental_review(&inc_args("dev0", "jprev"), "jprev", &provider, &inc_cfg(), dir.path(), None).await.unwrap();
        assert_eq!(report.schema_version, "3");
        assert!(report.run.incremental);
        assert_eq!(report.run.delta_base.as_deref(), Some(c1.as_str()));
        assert!(report.run.head_sha.is_some(), "本轮同样 stamp（下轮的 base）");
        assert_eq!(report.run.carried_dropped, vec![id2], "quote 被修掉 → 消灭进审计");
        let carried = report.findings.iter().find(|f| f.id == id1).expect("F1 应携带");
        assert_eq!(carried.origin, crate::domain::Origin::Carried);
        let fresh = report.findings.iter().find(|f| f.title == "secret leak").expect("新 finding 应入场");
        assert_eq!(fresh.origin, crate::domain::Origin::New);
        let sn = report.stats.new.as_ref().expect("stats.new 子块");
        assert_eq!((sn.total, report.stats.total), (1, 2), "stats 合并集口径 + new 子块");
        assert_eq!(code, 10, "carried High(agreement=1 但携带原样) 或 new High 均 blocking → reject");
    }

    /// 假修复靶（验收 §8.2 机制层）：resolve 了但代码未动 → claimed 再现 → resolution_disputed。
    #[tokio::test]
    async fn fake_fix_yields_resolution_disputed() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    md5(pw);\n    other();\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        let f1 = enrich(&raw("a.rs", "md5(pw);", "weak hash"), "L1", "m");
        let id1 = f1.id.clone();
        write_prior(dir.path(), "jprev", &c1, vec![f1]);
        // 销案（谎称已修）——快照四件套与 finding 同源
        let mut db = crate::db::ProjDb::open(dir.path()).unwrap();
        db.resolve_upsert(&crate::db::ResolveCard {
            id: id1.clone(), title: "weak hash".into(), file: "a.rs".into(),
            quote: "md5(pw);".into(), severity: "high".into(),
            reason: "假装修了".into(), round: "r1".into(), updated_at: String::new(),
        }).unwrap();
        drop(db);
        // fix commit 不触 md5 行
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    md5(pw);\n    other_fixed();\n}\n").unwrap();
        commit_all(dir.path(), "fake fix");
        // 模型按 claimed 指令原样再报
        let provider = FakeProvider::with("m", vec![raw("a.rs", "md5(pw);", "weak hash")]);
        let (report, _) = run_incremental_review(&inc_args("dev0", "jprev"), "jprev", &provider, &inc_cfg(), dir.path(), None).await.unwrap();
        assert_eq!(report.run.resolved, vec![id1.clone()], "resolve 生效：不携带");
        assert_eq!(report.run.resolution_disputed, vec![id1.clone()], "同 id 再现 → 假修复回路");
        let disputed = report.findings.iter().find(|f| f.id == id1).expect("disputed 项以 new 身份在场");
        assert_eq!(disputed.origin, crate::domain::Origin::New);
        assert!(report.run.carried_dropped.is_empty());
    }

    /// re-raise 去重：模型重报携带项 → 保 carried、agreement 不涨。
    #[tokio::test]
    async fn incremental_re_raise_dedups_into_carried() {
        let dir = tempfile::tempdir().unwrap();
        git_repo(dir.path());
        std::fs::write(dir.path().join("a.rs"), "fn f() {\n    db.write(token);\n}\n").unwrap();
        let c1 = commit_all(dir.path(), "c1");
        std::process::Command::new("git").current_dir(dir.path()).args(["branch", "dev0", &c1]).output().unwrap();
        let mut f1 = enrich(&raw("a.rs", "db.write(token);", "unvalidated write"), "L1", "m");
        f1.agreement = 2;
        f1.models = vec!["m".into(), "m2".into()];
        let id1 = f1.id.clone();
        write_prior(dir.path(), "jprev", &c1, vec![f1]);
        std::fs::write(dir.path().join("a.rs"), "// touch\nfn f() {\n    db.write(token);\n}\n").unwrap();
        commit_all(dir.path(), "fix");
        let provider = FakeProvider::with("m", vec![raw("a.rs", "db.write(token);", "unvalidated write")]);
        let (report, _) = run_incremental_review(&inc_args("dev0", "jprev"), "jprev", &provider, &inc_cfg(), dir.path(), None).await.unwrap();
        assert_eq!(report.findings.len(), 1, "同 id 去重");
        assert_eq!(report.findings[0].origin, crate::domain::Origin::Carried);
        assert_eq!(report.findings[0].agreement, 2, "re-raise 不涨 agreement");
        assert_eq!(report.findings[0].id, id1);
        assert_eq!((report.findings[0].line_start, report.findings[0].line_end), (3, 3), "重锚随注释插入移位");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test incremental_carries fake_fix incremental_re_raise 2>&1 | tail -5`
Expected: 编译错误（run_incremental_review 未定义）。

- [ ] **Step 3: 实现 run_incremental_review（cli.rs，run_review_dispatch 之前）**

```rust
/// M13 增量轮主流水线（决议 2-5）：四守卫 → delta fan-out（<incremental_round> 注入）
/// → carry/merge → refute(new-only) → verdict(合并集) → schema v3 报告。
/// 只由 run_review_dispatch 路由进入（前台与后台 worker 共用）。
pub async fn run_incremental_review(
    args: &ReviewArgs,
    prior_job: &str,
    provider: &dyn ReviewProvider,
    cfg: &Config,
    root: &Path,
    sink: Option<&dyn CellSink>,
) -> anyhow::Result<(ReviewReport, i32)> {
    let ledger = load_ledger(args.dismissed.as_deref(), args.no_dismissed, root)?;
    let t0 = std::time::Instant::now();

    // 1. 四守卫 + 基线（违例显式报错拒跑）。
    let base = incremental_preflight(args, prior_job, root)?;
    // 2. 本轮证据 HEAD stamp（R3：与 delta 收集同刻；本轮报告是下一轮的基线）。
    let head_sha = crate::evidence::git::head_sha_at(root).ok();

    // 3. resolve 台账（preflight 刚从 proj.db 读出上轮 result，库必已在；open_at 免建目录副作用）。
    let db_path = root.join(crate::project::PROJECT_DIR).join(crate::db::DB_FILE);
    let mut resolve_cards = if db_path.exists() {
        crate::db::ProjDb::open_at(&db_path)?.resolved_cards()?
    } else {
        Vec::new()
    };

    // 4. R1 rename 迁移：上轮合并集与 resolve 卡同规则迁 file/id（碰撞域一致）。
    let mut prior_findings = base.prior_findings;
    crate::consolidate::migrate_renames(&mut prior_findings, &base.renames);
    crate::consolidate::migrate_resolve_cards(&mut resolve_cards, &base.renames);
    let prior_ids: std::collections::HashSet<String> =
        prior_findings.iter().map(|f| f.id.clone()).collect();
    let resolved_ids: std::collections::HashSet<String> =
        resolve_cards.iter().map(|c| c.id.clone()).collect();

    // 5. carry：resolved 排除 → 就近重锚 → 成活/消灭。
    let outcome = crate::consolidate::carry(prior_findings, &resolved_ids, root);

    // 6. claimed-resolved 清单（防假修复保险）：本谱系已销案（∩上轮合并集）且 quote 仍锚中
    //    ——修真落地的 quote 会消失，仍在场即可疑，搭本轮评审的车顺带验证（零额外 LLM 调用）。
    let claimed: Vec<crate::db::ResolveCard> = resolve_cards
        .iter()
        .filter(|c| prior_ids.contains(&c.id) && !c.quote.trim().is_empty())
        .filter(|c| crate::positioning::anchor_near(root, &c.file, &c.quote, 0).is_some())
        .cloned()
        .collect();

    // 7. delta → bundle + 增量 prompt 段 + fan-out（select_files/group/run_code_fanout 全复用）。
    let bundles = group(select_files(base.delta), Some(root));
    anyhow::ensure!(
        !bundles.is_empty(),
        "--incremental-from: delta since {} is all noise-filtered (nothing reviewable); run a full `redteam review` or let ship's stall guard decide",
        base.base_sha
    );
    let all_files = crate::evidence::git::branch_files(
        root,
        args.base.as_deref().expect("branch scope enforced by preflight"),
    )?;
    let delta_files: std::collections::BTreeSet<String> =
        bundles.iter().flat_map(|b| b.files.iter().map(|f| f.path.clone())).collect();
    let manifest2 = crate::bundle::two_layer_manifest(&all_files, &delta_files);
    let short_base = &base.base_sha[..12.min(base.base_sha.len())];
    let inc_section = crate::prompt::render_incremental_section(short_base, &manifest2, &claimed);

    let lens_ids: Vec<String> = args
        .lenses
        .as_deref()
        .map(|s| s.split(',').map(|x| x.trim().to_string()).collect())
        .unwrap_or_else(|| cfg.review.lenses.code.clone());
    let rules = select(Mode::Code, &lens_ids)?;
    let (models, replicate) = resolve_models(args.models.as_deref(), args.models_all, cfg);
    let model_names: Vec<String> = models.iter().map(|m| m.name.clone()).collect();
    let model_ids = model_ids_for(&model_names, cfg);
    let oopts = OrchestrateOpts {
        models,
        replicate,
        no_fallback: false,
        max_concurrency: cfg.review.max_concurrency,
    };
    let focus = args.focus.clone().unwrap_or_default();
    let target_label = format!("fix delta {short_base}..HEAD (incremental round)");
    if let Some(s) = sink { s.on_meta(&model_names, &lens_ids); }
    let raw = run_code_fanout(provider, &target_label, &bundles, &rules, &focus, &inc_section, &oopts, sink).await;

    // 8. 尾段：anchor→dedup→clamp（仅 new）→ dispute 检测 → merge(carry∪new)
    //    → refute(new-only) → mark_dismissed → verdict(合并集) → v3 报告。
    let block = args.block_threshold.as_deref().map(parse_sev).unwrap_or(cfg.review.block_threshold);
    let floor = args.conf_floor.unwrap_or(cfg.review.conf_floor);
    if let Some(s) = sink { s.on_phase(crate::state::job::Phase::Finalizing, None); }
    let anchored = anchor(raw.findings, root);
    let mut new_findings = dedup(anchored);
    clamp_compliance(&mut new_findings);
    let mut resolution_disputed: Vec<String> = new_findings
        .iter()
        .filter(|f| resolved_ids.contains(&f.id))
        .map(|f| f.id.clone())
        .collect();
    resolution_disputed.sort();
    resolution_disputed.dedup();
    let merged = crate::consolidate::merge_carry(outcome.carried, new_findings);

    let n_targets = refute_targets(&merged, &ledger.cards, block).len();
    if n_targets > 0 {
        if let Some(s) = sink {
            s.on_phase(crate::state::job::Phase::Refute, Some(format!("{n_targets} targets")));
        }
    }
    let (findings, refute_warnings) = verify_findings(
        merged, provider, &model_names, block, cfg.review.max_concurrency, Mode::Code, &ledger.cards,
    ).await;
    let mut warnings = raw.warnings;
    warnings.extend(refute_warnings);
    let mut findings = findings;
    let dismissed_reappeared = mark_dismissed(&mut findings, &ledger.ids);

    let verdict = compute_verdict(&findings, block, floor);
    let new_only: Vec<Finding> = findings
        .iter()
        .filter(|f| f.origin == crate::domain::Origin::New)
        .cloned()
        .collect();
    let mut stats = Stats::from_findings(&findings);
    stats.new = Some(Box::new(Stats::from_findings(&new_only)));
    let report = ReviewReport {
        schema_version: "3".into(),
        mode: Mode::Code,
        verdict,
        stats,
        findings,
        clusters: vec![],
        run: RunMeta {
            job_id: String::new(),
            models: model_names,
            model_ids,
            lenses: lens_ids,
            duration_ms: t0.elapsed().as_millis() as u64,
            usage: Usage { input_tokens: raw.usage.0, output_tokens: raw.usage.1, cached_input_tokens: raw.usage.2 },
            warnings,
            failed_models: raw.failed_models,
            dismissed_reappeared,
            ledger: ledger.meta.clone(),
            head_sha,
            incremental: true,
            delta_base: Some(base.base_sha),
            resolved: outcome.resolved_applied,
            resolution_disputed,
            carried_dropped: outcome.carried_dropped,
        },
    };
    let code = exit_code(verdict, !report.run.failed_models.is_empty());
    Ok((report, code))
}
```

- [ ] **Step 4: dispatch 路由 + resume 拒绝**

`run_review_dispatch` 在 `let detected = detect_mode(&eopts)?;` 之前加：

```rust
    // M13：增量轮路由（前台与后台 worker 共用本单点；mode/scope 守卫在 preflight 内）。
    if let Some(prior) = sync_args.incremental_from.clone() {
        return run_incremental_review(&sync_args, &prior, provider, cfg, cwd, sink).await;
    }
```

`run_resume` 在 args 解析成功后（`let mode = match detect_mode_from_args` 之前）加：

```rust
    // M13：增量 job 的差集重放只补 fan-out cell，不重跑 carry/merge 尾段——直接拒绝，
    // 引导重跑增量评审（守卫与 carry 必须整链执行）。
    if args.incremental_from.is_some() {
        return partial_report(
            ws, job_id, &args,
            "resume: incremental jobs cannot be resumed; re-run `redteam review --incremental-from <prior-job>`".into(),
        );
    }
```

- [ ] **Step 5: 全量测试通过**

Run: `cargo test 2>&1 | tail -3`
Expected: 全绿（三个 e2e 含假修复靶）。

注意点（实现时自查）：`FakeProvider` 是 prompt-blind 的——refute 调用会返回同一脚本导致孤证 new finding 被自审 refute（`lone_blocking_refuted_does_not_reject` 同型）。`incremental_carries_drops_and_reports_v3` 里 carried F1 是 agreement=1 High 但 **origin=carried 不进 refute**（Task 8）→ verify=None 保持 blocking → code=10 断言成立；新 finding `secret leak` 会被 refute 翻掉 confidence，不影响该断言。若断言不符，优先检查 Task 8 的 targets 过滤是否生效。

- [ ] **Step 6: Commit**

```bash
git add src/cli.rs
git commit -m "feat(incremental): run_incremental_review 流水线——carry/merge/dispute/refute(new-only)/v3 报告 + dispatch 路由 + resume 拒绝"
```

---

### Task 12: md 渲染增量注记

**Files:**
- Modify: `src/output/mod.rs`
- Test: 内联

**Interfaces:**
- Consumes: Task 1 的 RunMeta 聚合字段 + Finding.origin。
- Produces: rN.md（`result --format md`）在增量轮显示 Incremental/Carried-New/审计行；carried finding 带 Origin 行。R5 决议：status 不加列，消费方读 result JSON / rN.md——本 Task 即那个「rN.md」出口。

- [ ] **Step 1: 写失败测试（output tests mod 追加）**

```rust
    #[test]
    fn md_incremental_rows_and_carried_origin() {
        let mut r = report();
        r.run.incremental = true;
        r.run.delta_base = Some("abc123def4567890".into());
        r.run.resolved = vec!["1111111111111111".into()];
        r.run.resolution_disputed = vec!["2222222222222222".into()];
        r.run.carried_dropped = vec!["3333333333333333".into()];
        r.findings[0].origin = Origin::Carried;
        let s = render(&r, Format::Md);
        assert!(s.contains("| Incremental") && s.contains("abc123def456"), "delta base 短 sha: {s}");
        assert!(s.contains("| Carried / New | 1 / 0 |"), "{s}");
        assert!(s.contains("Carry Dropped") && s.contains("3333333333333333"), "{s}");
        assert!(s.contains("Resolved") && s.contains("1111111111111111"), "{s}");
        assert!(s.contains("Disputed") && s.contains("2222222222222222"), "disputed 是 ship 高信号必须在场: {s}");
        assert!(s.contains("| Origin     | carried"), "携带项标记: {s}");
    }

    #[test]
    fn md_non_incremental_stays_clean() {
        let s = render(&report(), Format::Md);
        assert!(!s.contains("Incremental") && !s.contains("| Origin"), "全量报告零增量噪音: {s}");
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test md_incremental md_non_incremental 2>&1 | tail -5`
Expected: FAIL（行缺失）。

- [ ] **Step 3: 实现（render_md）**

Ledger 行之后（`if let Some(l) = &r.run.ledger {...}` 块后）插入：

```rust
    // M13 增量轮注记（R5 决议：carried/new 终态语义住 result/rN.md，不进 status）。
    if r.run.incremental {
        let base = r.run.delta_base.as_deref().unwrap_or("?");
        let carried = r.findings.iter().filter(|f| f.origin == crate::domain::Origin::Carried).count();
        let fresh = r.findings.len() - carried;
        s.push_str(&format!("  | Incremental   | delta since {} |\n", &base[..12.min(base.len())]));
        s.push_str(&format!("  | Carried / New | {carried} / {fresh} |\n"));
        if !r.run.carried_dropped.is_empty() {
            s.push_str(&format!("  | Carry Dropped | {} |\n", r.run.carried_dropped.join(", ")));
        }
        if !r.run.resolved.is_empty() {
            s.push_str(&format!("  | Resolved      | {} |\n", r.run.resolved.join(", ")));
        }
        if !r.run.resolution_disputed.is_empty() {
            s.push_str(&format!("  | ⚠️ Disputed    | {} |\n", r.run.resolution_disputed.join(", ")));
        }
    }
```

Finding 明细表 Confidence 行之后插入：

```rust
                if f.origin == crate::domain::Origin::Carried {
                    s.push_str("| Origin     | carried from an earlier round |\n");
                }
```

- [ ] **Step 4: 测试通过 + Commit**

Run: `cargo test 2>&1 | tail -3`

```bash
git add src/output/mod.rs
git commit -m "feat(incremental): rN.md 增量注记——delta base/Carried-New/审计三数组/携带项 Origin 行"
```

---

### Task 13: ship.md 协议改造 + feedback.md jq 兼容（R6）+ 镜像重生成

**Files:**
- Modify: `plugins/redteam/commands/ship.md`
- Modify: `plugins/redteam/feedback.md`
- Regenerate: `plugins/redteam/codex-skills/`（`make gen`）
- 已核实不动: `plugins/redteam/commands/review.md`（薄透传，不枚举 flag）

**Interfaces:**
- Consumes: Task 3 `redteam resolve`、Task 9 `--incremental-from` 与守卫错误文案、Task 12 rN.md 增量注记。
- Produces: 增量化 ship 循环协议（决议 6 停机表全落地）。纯 prompt 层，无 Rust 改动。

- [ ] **Step 1: ship.md「每轮 rN」§1 起评审——整段替换**

原 §1（`1. **起评审**：` 至 `- **轮询期间不改动工作树**…` 行）替换为：

```markdown
1. **起评审**：
   - **r1（或无健康增量基线时）**：`${CLAUDE_PLUGIN_ROOT}/bin/redteam review <透传参数> --background`（全量）。
   - **r2 起（code 模式且用户参数带 --base，即 branch scope）**：追加 `--incremental-from <上一轮 job id>`
     ——只审上轮 stamped HEAD 以来已提交的 fix delta，未动代码的 findings 由 redteam 重锚定携带；
     verdict 仍是合并集口径（携带 ∪ 新发），**不要**自己把旧 findings 复述进 --focus。
     **前提：本轮修复已全部 commit（工作树净）**——delta 只含已提交改动。
     doc 模式或 working-tree scope：不加该 flag，照旧全量循环。
   - **守卫报错处置**（redteam 显式拒跑，不静默降级）：谱系（rebase/reset）/同族/健康基线/无 stamp
     报错 → 本轮**去掉 `--incremental-from` 改跑全量**，简报注记原因与守卫名；
     **空 delta 报错**（"empty delta"）→ 按 §停机 停滞守卫处理（空转，停，交用户）。
   （`--background` 放在全部参数之后——`args_override_self` 语义下后者胜。）
   → 记下 job id（**增量链条的下一环**：本轮 job id 就是下轮 `--incremental-from` 的值）。
   轮询 `${CLAUDE_PLUGIN_ROOT}/bin/redteam status <job>`（间隔约 20-30s），只等**终态**
   （done / failed / timed_out / cancelled）。redteam 后台 job 有 40 分钟 wall-clock deadline watchdog（到点自我
   终止并出部分报告），**永不永久卡死**——因此**不要自己判卡死去 cancel**。
   `cancelled`（外部 `redteam cancel` 中止）→ 视为用户主动中止，停轮、报告用户，不算 approve。
   - `last_active`/`phase` 仅作进度信息（活着/慢/尾段），**不作 cancel 依据**：推理模型的超长单轮
     turn 与 `finalizing` 尾段的 `last_active` 设计上静止，按「心跳停滞 N 分钟」判死会误杀正常慢 job。
   - **轮询期间不改动工作树**（评审与携带重锚定正在读活文件）。
```

- [ ] **Step 2: ship.md「内循环」§1-§3 改造**

§1 分拣段尾部追加一句（`previously_dismissed` 分流句之后）：

```markdown
   增量轮报告的 `run.resolution_disputed` 非空 → 这些 id 是「已销案却再现」的假修复信号，
   在 §3 按属实项优先处理。
```

§2 排序整行替换为：

```markdown
2. **排序**：severity 降序为主序，同级按 file 分组。增量轮先看 origin（rN.md 的 Origin 行 /
   JSON 的 `.origin`）：`carried` = 上轮遗留（多为存疑未决——已裁决过的要么已 resolve 要么已
   dismiss），不重复全量裁决，只推进其未决状态；`new` = 本轮 delta 新发，照常逐条裁决。
```

§3 开头「每条属实项标注**来源：原始/衍生**——衍生 = …防自证式标签空转：」整句删除（含 git blame 取证要求——被 origin 字段 + delta 范围语义吸收，决议 6），改为：

```markdown
3. **逐条**（读 quote + 文件上下文 + body 后裁决）：
```

§3 的裁决动作列表在「**属实 → 修**」条目后新增一条：

```markdown
   - **resolution_disputed 项 → 按修复未生效处理**：重读代码确认——确实没修好 → 重修
     （修好后收轮时重新 resolve）；模型误判（其实已修好）→ 按误报走 dismiss 建卡。
     **同一 id 第二次出现在 resolution_disputed → 走 §停机 销案争议刹车**。
```

- [ ] **Step 3: ship.md「收轮」段——commit 步之后插入销案步**

原「修复全落 → 跑测试命令至绿 → **单次 commit（仅修复，不含 .redteam/）**，消息 `fix(ship): rN <一句话摘要>`」之后插入：

```markdown
→ **销案**（增量链条的关键一环，紧跟 commit）：对本轮每个「属实→已修」的 finding id 逐个
  `${CLAUDE_PLUGIN_ROOT}/bin/redteam resolve <id> --from rounds/<session>/rN.result.json --round rN --reason "<一句修法>"`
  （快照四件套由 --from 自动抄写，与 dismiss add 对称；已修复≠误报，落独立表）。
  下一轮增量据此**不再携带**该 finding；若其 quote 未随修复消失，会自动进下轮「顺带验证」
  ——假修复会以 `resolution_disputed` 回来。**存疑未决项不 resolve**（携带到下轮继续在场）。
```

简报步 3（Adjudication 追加段）中「属实项带**原始/衍生**标（衍生附引入 commit 短 sha）、」改为「属实项带 origin 标（carried/new，报告已算好）、销案 id 清单、」。

- [ ] **Step 4: ship.md「停机」节——整节替换（决议 6 停机表）**

```markdown
## 停机（每轮内循环完成后判定；即使 verdict=approve，待刹车 id 先走完刹车链)
1. **verdict = approve**（合并集口径：携带 ∪ 本轮新发，redteam 已算好）→ 收工：写
   `rounds/<session>/session.md` 总结（轮数、修复/驳回/销案统计、carried 遗留清单、遗留观察），
   向用户报告可 ship。**可选终扫**：用户需要发版级信心时，建议手动裸跑一次全量
   `redteam review`（不带 --incremental-from）——非默认步骤，成本一整轮，收益是对
   跨文件耦合的最后一次全量对抗。
2. **销案争议刹车**：同一 id **第二次**出现在 `run.resolution_disputed` → 停，
   AskUserQuestion 交人裁决（修复者与评审者死锁不该机器无限循环；对称升级刹车链的
   「每 id 至多一次复裁」精神）。
3. **升级刹车链**（每 id 至多一次）：`run.dismissed_reappeared` 中处于 `dismissed` 状态
   的 id → 对照卡上驳回理由（`dismiss show <id>`），带新 finding body **复裁一次**：
   改判属实 → 修 + `dismiss set-status <id> withdrawn --round rM`；仍判误报 →
   `dismiss set-status <id> re-upheld --round rM`，**停，AskUserQuestion 交人裁决**。
   人裁误报 → `set-status <id> human-confirmed --round rK`，此后该 id 永久静默；
   人裁属实 → 修 + `set-status <id> withdrawn --round rK`，继续。
4. **修复受阻**：某 finding 属实但你修不了（需产品/架构决策、改动越权）→ 停，交用户。
   doc 模式全量循环若陷自指尾流僵局（修复自身反复被重审出衍生问题、approve 不可达），
   也按本条停机交人。
5. **停滞守卫**：本轮仍有未豁免 findings 但零代码改动（未修也未新驳）→ 空转，停，交用户。
   增量轮的「empty delta」守卫报错同走本条（无新改动可审 = 空转）。
6. `--max-rounds` 达限（仅用户显式给出时）→ 停，输出未收敛摘要。

> 尾流守卫已随增量协议**退役**：增量轮的新 finding 定义上全是「衍生」（delta 就是上轮修复），
> 「连续全衍生」失去判别力；自指尾流问题由「未动代码携带判决、只审 delta」结构性消解，
> approve 经合并集天然可达。
```

- [ ] **Step 5: feedback.md jq 改口径（R6）+ schema 注记**

Tier-1 jq 中 `raised:` 一行替换、并在其后加 `carried:` 一行：

```
  | { raised:        ([$f[] | select((.origin // "new") == "new")] | length),
      carried:       ([$f[] | select(.origin == "carried")] | length),
```

（`(.origin // "new")`：v2 老报告无 origin 字段——缺省视 new，不掉不混。）

`raised` 的解释行改为：

```markdown
- `raised` = 跨轮按 `id` 去重后的 **new-only** finding 数（M12 记分卡口径改增量语义：携带项
  不重复计 raise）；`carried` = 携带项数；`refuted`/`agreement_ge2`/`anchor_fail` 同基于去重集。
```

ReviewReport schema 块的 `Finding = {...}` 行尾补 `origin?("carried"|"new"，v3;缺省视 new)`；
`run:{...}` 行补 `head_sha?, incremental?, delta_base?, resolved?, resolution_disputed?, carried_dropped?`。

- [ ] **Step 6: 镜像重生成 + 防漂移验证 + Commit**

Run: `make gen && make verify && cargo test 2>&1 | tail -3`
Expected: verify 零 diff 报绿；测试全绿。

```bash
git add plugins/
git commit -m "feat(ship): 增量循环协议——r2 起 --incremental-from/收轮销案/停机表重写(尾流守卫退役+销案争议刹车)/feedback jq v2 兼容"
```

---

### Task 14: 验收①——单跳回放（决议 8.1，手动 dogfood）

**Files:**
- 无代码改动；结果记录追加到 `.doc/roadmap.md` M13 条目（gitignored，不 commit）。

**前置：** Tasks 1-13 全绿 + `make build`（binary 放置插件路径）。真实 LLM 调用，预算 <1M tokens。

- [ ] **Step 1: 定位历史基线**

```bash
ls .redteam/rounds/20260703-004708/          # r1.md / r1.result.json / …
# 从 r1.md 的 H1 `#<job>` 取 r1 job id；确认 result 在库：
./target/release/redteam result <r1-job> | jq '.run | {job_id, models, head_sha}'
# 从该 session 的 git 历史找 r1 时点 HEAD（第一个 fix(ship): r1 commit 的父提交）与 r2 fix commit：
git log --oneline --grep 'fix(ship)' | tail -20
sqlite3 .redteam/proj.db "SELECT args_json FROM jobs WHERE job_id='<r1-job>'"   # 确认 scope/base
```

- [ ] **Step 2: 回填 stamp（历史 v2 报告无 head_sha，一次性 sqlite 手术）**

```bash
WORK=$(mktemp -d)
JOB=<r1-job>; R1SHA=<r1时点HEAD>
sqlite3 .redteam/proj.db "SELECT json FROM results WHERE job_id='$JOB'" > "$WORK/r1-raw.json"
jq --arg sha "$R1SHA" '.run.head_sha=$sha' "$WORK/r1-raw.json" > "$WORK/r1-stamped.json"
python3 - "$JOB" "$WORK/r1-stamped.json" <<'EOF'
import sqlite3, sys
job, path = sys.argv[1], sys.argv[2]
db = sqlite3.connect('.redteam/proj.db')
db.execute("UPDATE results SET json=? WHERE job_id=?", (open(path).read(), job))
db.commit(); print("stamped", job)
EOF
```

- [ ] **Step 3: 按 r1.md 裁决段补打台账**

r1.md `## Adjudication` 的「属实→已修」各 id：

```bash
./target/release/redteam resolve <id> --from "$WORK/r1-raw.json" --round r1 --reason "<r1 简报里的修法一句>"
```

「误报→驳回」各 id 若当时未建卡，补 `dismiss add`（多数应已在库，`dismiss show <id>` 先查）。

- [ ] **Step 4: worktree 检出历史 r2 时点 + 跑增量**

```bash
git worktree add "$WORK/replay" <r2-fix-commit>
mkdir -p "$WORK/replay/.redteam" && cp .redteam/proj.db "$WORK/replay/.redteam/"
cd "$WORK/replay"
<redteam二进制绝对路径> review --mode code --base <历史session的base> --incremental-from "$JOB" --background
# 等终态后：
<redteam二进制绝对路径> result <新job> > "$WORK/replay-r2.json"
```

- [ ] **Step 5: 对照验收线（design 决议 8.1）**

```bash
jq '{verdict, stats, run: {incremental, delta_base, resolved, resolution_disputed, carried_dropped, usage}}' "$WORK/replay-r2.json"
jq -r '.findings[] | "\(.origin)\t\(.id)\t\(.title)"' "$WORK/replay-r2.json" | sort
```

核对清单（全过才算过）：
- [ ] 历史 r2 属实项（对照历史 `r2.result.json` + r2.md 裁决段，特别含衍生案 `661e3973`/`5361b545`）**100% 被捕获（new）或携带（carried）**——逐 id 打勾；
- [ ] `carried_dropped` 逐 id 抽查：其 quote 所在代码确属 r1→r2 delta 内改动（`git diff $R1SHA..<r2-fix-commit> -- <file>` 目检）；
- [ ] `run.resolved` = Step 3 打的 resolve id 集（销案生效）；
- [ ] `run.usage.input_tokens` 对照历史 r2 的 ~2.7M——记录实际数与降幅。
- [ ] 清理：`git worktree remove "$WORK/replay"`；回放结论（含未达标偏差与归因）记入 `.doc/roadmap.md` M13 条目。

**失败处置**：保全率不足 100% → 逐 id 归因（rename？就近锚错？quote 漂移？）——修机制补 hermetic 测试后重放；不得带着已知保全缺口进 Task 15。

---

### Task 15: 验收②③——争议刹车合成靶 + live 收编（决议 8.2/8.3）

**Files:**
- 无代码改动；玩具仓演练 + 下一真实 ship 会话；结论记 `.doc/roadmap.md`。

- [ ] **Step 1: 合成靶 A（假修复→disputed）机制层确认**

已由 Task 11 hermetic 测试 `fake_fix_yields_resolution_disputed` 覆盖（机械触发路径有回归资产）。此处只跑一遍确认在最终二进制上绿：

Run: `cargo test fake_fix_yields_resolution_disputed 2>&1 | tail -3`

- [ ] **Step 2: 合成靶 B（销案争议刹车，ship 协议层演练）**

玩具仓演练脚本（人为制造两次 disputed）：

```bash
WORK=$(mktemp -d)/brake && mkdir -p "$WORK" && cd "$WORK"
git init -q -b main && git config user.email t@t && git config user.name t
cat > app.py <<'EOF'
import hashlib
def store(pw):
    return hashlib.md5(pw.encode()).hexdigest()  # 明显弱哈希靶
EOF
git add -A && git commit -qm "seed" && git branch base
echo "# pad" >> app.py && git add -A && git commit -qm "feature"
```

在该仓起 `/redteam:ship --base base`：
1. r1 全量报出弱哈希 → 裁决属实，**故意不修**，直接 `redteam resolve <id> …`（假销案）+ 提交一个无关小改动 commit；
2. r2 增量 → 期望报告 `resolution_disputed` 含该 id，ship 按「修复未生效」重开——再次**假装修**（再 resolve + 无关 commit）；
3. r3 增量 → 同 id **第二次** disputed → 期望 ship 触发**销案争议刹车**：停机 + AskUserQuestion 交人。
- [ ] 记录：两次 disputed 是否都被报告聚合捕获；ship 是否在第二次准确停机（而非无限循环或误 approve）。

- [ ] **Step 3: live 收编（验收主体，下一真实 ship 会话）**

前置自查：3 个 P1（Task 2/7/9）已 commit、Task 14 保全率 100%。执行：
- 下一真实开发批次照常 `/redteam:ship --base dev ...`（branch scope），全程 r2+ 增量；
- 会话结束后算账：

```bash
jq -s '{rounds: length,
        total_input: ([.[].run.usage.input_tokens] | add),
        per_round: [.[].run.usage.input_tokens],
        carried: ([.[].findings[] | select(.origin == "carried")] | length),
        disputed: ([.[].run.resolution_disputed // [] | .[]] | unique)}' \
  .redteam/rounds/<session>/r*.result.json
```

验收线（design 决议 8.3，指标口径注记：input_tokens 已含 cached 归一，与历史 OpenAI 系口径直接可比）：
- [ ] 同规模会话总 input 较历史基线（17.1M/5 轮，单轮 r2 2.7M）**≥50% 降**；
- [ ] 正常收敛：approve 或合理停机，无携带死锁（同一 carried 集反复空转）；
- [ ] 驳回率不显著恶化（feedback scorecard 的 headline_precision_proxy 对照历史两 session）；
- [ ] feedback jq 在 v2（历史）与 v3（本次）报告上都跑通，raised 口径正确。
- [ ] 结论与数字记入 `.doc/roadmap.md`：达标 → M13 标记「已交付」；未达标 → 记偏差与归因，回批修正。

---

## 非目标（执行期不做，撞见记 TODO 不扩散）

- doc 模式增量、邻域扩张、默认终扫、全会话回放、跨轮 agreement 加权、卷宗/tree-sitter（支柱 2）——design §4 全文有效。
- whitespace-only 语义变化（normalize_ws 折叠后仍锚中，R4 P3）：live 观察，撞见记录不预堵。
- ship.md 停机看 exit code 的加固（approve+exit=2 半失败）：既有协议问题，已挂 `.doc/TODO.md`，**本批不混**。
- status 不加 carried/new 列（R5 已砍）。

## 执行与并行化

主线 Task 1→15 顺序执行（cli.rs/report.rs 高耦合，单 worktree）；唯一可提前的支线是 Task 13 的 feedback.md 半步（plugins/ 壳层与 src/ 无交集），不值当拆，随批④走。每 Task 一 commit，全绿推进；批⑤（Task 14-15）是 dogfood 手动步骤，由维护者本人执行（真实 LLM 计费 + 人裁验收线）。



