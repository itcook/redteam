# M17-a 尾段统一 · 实施计划

> 状态: 已交付(2026-07-24 合 dev 8b9eef2) · 对应 design `../design/2026-07-24-redteam-m17a-tail-unification-design.md`（七裁决闭集）；决策 `../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §A；里程碑=roadmap M17-a。
> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development，逐 task 派发。

**Goal:** 全量/resume/增量三路径共用 assemble_report 尾段引擎；吞 D4/T44；cli.rs 拆五模块；dossier→diffsym。

**Architecture:** 语义变更在前（Task 1-4，flat cli.rs 上完成，测试锚定），机械迁移在后（Task 5-6，零语义 diff）——让语义 commit 小而可审，搬家 commit 大而无脑。

**分支:** `feat/m17a-tail-unification`（从 dev 切出；纯本地，永不 push）。

## Global Constraints（每 task 隐含）

- schema v3 字段集不动、`exit_code` 映射不动、resume/全量路径行为逐字不变（design §7）。
- 跨轮合并 **agreement 不加成**（决议 5 同构，design §3）。
- 每 task 收尾：`cargo test`（393+ lib 全绿）+ `cargo clippy` 零新告警 + commit。
- Task 5-6 额外：**零语义 diff**——签名/逻辑/pub 面不变，仅模块路径与 `pub(crate)` 可调；`cargo test` 含 4 e2e。
- 注释/commit 简体中文；commit 不加 Co-Authored-By 以外的尾注（沿仓库惯例）。
- 范围闭集：design §9 点名排除项一行不碰（D2/D3/exit-Incomplete/conf_floor/检查点吞错/T48/T49）。

---

### Task 1: is_same_bug 共享谓词 + merge_carry 相似回落（D4）

**Files:**
- Modify: `src/consolidate/dedup.rs`（抽谓词，主循环改用）
- Modify: `src/consolidate/carry.rs`（merge_carry 扩展 + 回归测试）

**Interfaces:**
- Produces: `pub(crate) fn is_same_bug(a: &Finding, b: &Finding) -> bool`（consolidate 内共享）；`merge_carry` 签名不变。

**Steps:**

- [ ] **1.1 写失败测试**（carry.rs tests）：
  - `merge_carry_absorbs_quote_drifted_new_into_carried`：carried 一条（file=a.rs, 行 10-12, title "missing auth check", quote Q1, agreement=2, models=[claude]），new 一条（同 file, 行 11-13, title 同义高 Jaccard, quote Q2≠Q1 → id 不同, models=[gpt]）→ 合并集 **1 条**、origin=Carried、agreement **仍 2**、models=[claude,gpt]、severity/confidence/body/rec 保 carried 原值。
  - `merge_carry_similarity_only_absorbs_into_carried`：out 中已 push 的 new 项不吸收后续相似 new（构造两条相似 new + 一条不相关 carried → 合并集 3 条，new-vs-new 不在此合并）。
  - `is_same_bug` 边界（dedup.rs tests）：行不重叠 → false；行重叠但 title Jaccard 与 quote overlap 双低 → false（under-merge 守卫）。
- [ ] **1.2 跑测确认失败**：`cargo test -p redteam --lib consolidate` → 新测试 FAIL。
- [ ] **1.3 实现**：dedup.rs 抽谓词（dedup 主循环 cli.rs:65-68 的条件原样搬入，行为零变化）：
  ```rust
  /// dedup 与 merge_carry 共用的「同一 bug」谓词（D4：单一真相源，不另立第二套阈值）。
  pub(crate) fn is_same_bug(a: &Finding, b: &Finding) -> bool {
      overlaps(a, b)
          && (title_jaccard(&a.title, &b.title) >= TITLE_SIM
              || quote_overlap(&a.quote, &b.quote) >= QUOTE_SIM)
  }
  ```
  carry.rs `merge_carry` 匹配链扩展（精确 id 现状优先，相似回落仅对 Carried）：
  ```rust
  pub fn merge_carry(carried: Vec<Finding>, new: Vec<Finding>) -> Vec<Finding> {
      let mut out = carried;
      for n in new {
          let hit = out.iter_mut().find(|c| {
              c.id == n.id
                  || (c.origin == Origin::Carried && crate::consolidate::dedup::is_same_bug(c, &n))
          });
          if let Some(c) = hit {
              for m in &n.models {
                  if !c.models.contains(m) { c.models.push(m.clone()); }
              }
              // agreement 不变（决议 5：跨轮重看非独立证据；相似回落=id 漂移补丁，语义同构）
          } else {
              out.push(n);
          }
      }
      out
  }
  ```
- [ ] **1.4 全绿**：`cargo test` + clippy。
- [ ] **1.5 Commit**：`fix(consolidate): D4 跨轮 quote 漂移去重——merge_carry 相似回落（is_same_bug 单一真相源，agreement 不加成）`

### Task 2: CarryCtx + assemble_report 泛化（尾段引擎）

**Files:**
- Modify: `src/cli.rs`（TailCtx/assemble_report，此时仍 flat 文件）

**Interfaces:**
- Produces: `struct CarryCtx { carried: Vec<Finding>, resolved_ids: HashSet<String>, resolved_applied: Vec<String>, carried_dropped: Vec<String>, delta_base: String }`；`TailCtx` 增 `carry: Option<CarryCtx>`。Task 3 消费。
- 两个既有调用方（run_code_review/run_resume）只加 `carry: None`，行为逐字不变。

**Steps:**

- [ ] **2.1 写失败测试**（cli.rs tests，直构 TailCtx 需 fake provider——复用现有测试的 provider stub 手法）：
  - `assemble_report_carry_none_unchanged`：carry=None → 报告无 Stats.new、RunMeta.incremental=false、delta_base=None（现状锚）。
  - `assemble_report_carry_some_merges_and_fills_meta`：carry=Some（carried 1 条 + resolved_ids 含一个将复现的 new id）→ 合并集含 carried、`resolution_disputed` 命中、Stats.new 只计 new、RunMeta.incremental=true/delta_base/resolved/carried_dropped 逐字段断言。
- [ ] **2.2 跑测确认失败**。
- [ ] **2.3 实现**：assemble_report 在 dedup 后插入（`let carry = ctx.carry.take();`）：
  ```rust
  let (findings, resolution_disputed, carry_meta) = match carry {
      Some(c) => {
          let mut disputed: Vec<String> = findings.iter()
              .filter(|f| c.resolved_ids.contains(&f.id))
              .map(|f| f.id.clone()).collect();
          disputed.sort(); disputed.dedup();
          let merged = crate::consolidate::merge_carry(c.carried, findings);
          (merged, disputed, Some((c.resolved_applied, c.carried_dropped, c.delta_base)))
      }
      None => (findings, vec![], None),
  };
  ```
  组装处：carry_meta 为 Some 时填 Stats.new（origin==New 子账）+ RunMeta 五字段；None 时沿用 `..Default::default()` 现状。refute/mark_dismissed/verdict 作用于（可能已合并的）findings，顺序不动。
- [ ] **2.4 全绿**（run_code_review/run_resume 调用点补 `carry: None`）。
- [ ] **2.5 Commit**：`refactor(tail): TailCtx 增 CarryCtx 数据扩展——assemble_report 长出增量插入点（A-Q1 裁决 A）`

### Task 3: 增量路径接入尾段引擎 + T44 spec 透传

**Files:**
- Modify: `src/cli.rs`（run_incremental_review：删手抄尾段 ~895-978，改构 TailCtx；spec 三处透传）

**Interfaces:**
- Consumes: Task 2 的 CarryCtx；run_code_review 的 spec 手法（搜 `render_spec_section_capped` 调用点，逐字对齐：预算扣减顺序、守卫、错误处理）。

**Steps:**

- [ ] **3.1 写失败测试**：
  - `second_pass_prompt` 增量分支 spec 段在场（现传 `""` 处改真值后断言 `<spec` 段出现）。
  - 增量尾段等价锚：构造小场景走 run_incremental_review 的 carry 构造逻辑（若需 provider 则用 stub），断言报告字段与 Task 2 的 carry=Some 测试同构。
- [ ] **3.2 实现**：
  - spec：`let (spec_section, spec_bytes) = ...`（逐字复制 run_code_review 手法，含 budget 扣减）；`mk` 闭包与 `run_code_fanout` 的 `""` 占位改 `&spec_section`；删两处 ponytail 注释。
  - 尾段：步骤 8 整段删除，改为构造 `TailCtx { carry: Some(CarryCtx { carried: outcome.carried, resolved_ids, resolved_applied: outcome.resolved_applied, carried_dropped: outcome.carried_dropped, delta_base: base.base_sha }), spec_docs: &spec_section, .. }` → `assemble_report(raw.findings, ctx).await`。Finalizing 打点保留在调用点。
- [ ] **3.3 全绿 + 行为核对**：增量 RunMeta 字段（job_id 空串/duration/usage/head_sha/dossier/needs）与删除前逐字段对照（用 3.1 测试断言固化）。
- [ ] **3.4 Commit**：`refactor(tail): 增量轮接入 assemble_report 尾段引擎，删手抄段；T44 --spec 三处透传生效`

### Task 4: 冒烟增量 e2e

**Files:**
- Modify: `tests/review_e2e.rs`（复用现有 fake-provider + temp git repo 骨架）

**Steps:**

- [ ] **4.1 写测试** `incremental_round_smoke`：①临时仓 commit A → 全量 review（fake provider 出 1 条 finding）→ 结果落库为 prior job；②commit B 造 delta；③`incremental_from: Some(prior)` 跑增量（fake provider 出 1 条新 finding）→ 断言：报告含 carried（origin=Carried）+ new 两条、verdict 合法、`run.incremental == true`、`run.delta_base == Some(A 的 sha)`、exit code 与 verdict 映射一致。
- [ ] **4.2 全绿**：`cargo test --test review_e2e`。
- [ ] **4.3 Commit**：`test(e2e): 增量轮冒烟锚（M17-a 测试网第 5 层；完整矩阵留 M18）`

### Task 5: cli.rs → cli/ 五模块拆分（零语义）

**Files:**
- Create: `src/cli/{mod,tail,spec,second_pass,incremental}.rs`；Delete: `src/cli.rs`

**Steps:**

- [ ] **5.1 按 design §5 表逐项搬移**（表即清单，含各项原行号；测试随主体走；`use` 路径与 `pub(crate)` 最小调整；job 状态机全部留 mod.rs 原文不动）。
- [ ] **5.2 零语义验证**：`cargo test`（lib+e2e 全绿）+ `cargo clippy` 零新告警 + `git diff --stat` 人查无非搬移改动。
- [ ] **5.3 Commit**：`refactor(cli): 上帝文件拆五模块（tail/spec/second_pass/incremental+mod 留守；job 状态机原地，零语义 diff）`

### Task 6: dossier → diffsym 改名（零语义）

**Files:**
- Rename: `src/dossier/` → `src/diffsym/`；Modify: `src/lib.rs`（mod 声明）+ 全部 `crate::dossier` 引用点（pack/verify/cli 等）

**Steps:**

- [ ] **6.1 机械改名**：目录/mod 声明/引用点/模块 doc 注释首行同步（职责表述对齐现实：diff/符号解析核心）。`DossierMeta`/`RunMeta.dossier` 等**对外 schema 名不动**（v3 冻结——只改内部模块路径）。
- [ ] **6.2 全绿 + Commit**：`refactor(diffsym): dossier 模块改名——名副其实（引用点引擎已随 E8-E 拆除，现存 diff/符号解析核心）`

---

## 收尾

- 全分支 opus 终审（superpowers final review，MERGE_BASE=分支起点）→ 修 findings → merge dev（--no-ff，纯本地）。
- roadmap M17-a 置 ✅、done.md 记账、本 plan 状态头改「已交付」。
