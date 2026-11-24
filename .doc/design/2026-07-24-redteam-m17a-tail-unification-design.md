# M17-a 尾段统一 · 设计

> 状态: 锁定 · 2026-07-24 grill 七裁决全数拍板；WHY 见 `../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §A；实施计划 `../plans/2026-07-24-redteam-m17a-tail-unification-plan.md`；里程碑=roadmap M17-a。

## 1. 目标

评审「验证→裁决→报告」正确性链收敛为**单一实现**：全量/resume/增量三路径共用
`assemble_report`；顺带吞两债（D4 跨轮去重、T44 增量 --spec 透传）；cli.rs 上帝文件
拆五模块；dossier 模块改名 diffsym。

## 2. 尾段引擎（CarryCtx 数据扩展）

`TailCtx` 增字段 `carry: Option<CarryCtx>`：

```rust
struct CarryCtx {
    carried: Vec<Finding>,            // consolidate::carry 产出（已重锚、origin=Carried）
    resolved_ids: HashSet<String>,    // 销案 id 集（dispute 检测用）
    resolved_applied: Vec<String>,    // → RunMeta.resolved
    carried_dropped: Vec<String>,     // → RunMeta.carried_dropped
    delta_base: String,               // → RunMeta.delta_base（完整 sha）
}
```

`assemble_report` 固定两个插入点，`carry` 为 `Some` 时生效：

1. **dedup 之后、refute 之前**：
   - dispute 检测：new findings 中 id ∈ resolved_ids 者 → `resolution_disputed`（排序去重）；
   - `merge_carry(carried, new)` 得合并集，后续步骤（refute/mark_dismissed/verdict）
     全部作用于合并集（refute 的 new-only 语义由 `refute_targets` 的
     `origin != Carried` 过滤承载，尾段零特判）。
2. **报告组装**：
   - `Stats.new = Some(new-only 子账)`；
   - RunMeta 五字段：`incremental: true` / `delta_base` / `resolved` /
     `resolution_disputed` / `carried_dropped`。

`carry` 为 `None`（全量/resume）时上述全部跳过，行为与现状逐字一致。
`run_incremental_review` 尾段手抄段（原 cli.rs:895-978）整体删除，改为构造
`TailCtx { carry: Some(...), spec_docs: 真值, .. }` 调 `assemble_report`。

## 3. D4 修复（merge_carry 相似回落）

- `consolidate/dedup.rs` 抽共享谓词（dedup 主循环同源改用它）：

```rust
pub(crate) fn is_same_bug(a: &Finding, b: &Finding) -> bool {
    overlaps(a, b)
        && (title_jaccard(&a.title, &b.title) >= TITLE_SIM
            || quote_overlap(&a.quote, &b.quote) >= QUOTE_SIM)
}
```

- `merge_carry` 匹配顺序：精确 id（现状）→ 落空的 new 对 **origin=Carried** 项做
  `is_same_bug` 相似回落。两种命中语义同构（决议 5）：保 carried 全体、models 并集、
  **agreement 不变**、new 副本丢弃。new-vs-new 相似不在此处理（上游 dedup 已做）。

## 4. T44（增量 --spec 透传）

与 `run_code_review` 逐字同手法：`render_spec_section_capped`（PEM/denylist 守卫 +
pack 预算扣减）→ 三处透传：fan-out prompt、二阶段 `mk` 闭包、`TailCtx.spec_docs`
（refute）。spec 每轮现读工作树。inc_section 的预算入账/注入中和**不在本批**（D2/M18）。

## 5. 模块拆分（零语义 diff）

`src/cli.rs`（3431 行）→ `src/cli/` 目录：

| 模块 | 迁入（原行号） |
|---|---|
| `cli/tail.rs` | exit_code(115) / mark_dismissed(161) / model_ids_for(181) / TailCtx(191) / CarryCtx(新) / assemble_report(225) |
| `cli/spec.rs` | SPEC_BUDGET_BYTES(444) / is_sensitive_spec_path(448) / contains_pem_private_key(478) / neutralize_spec_delimiters(486) / render_spec_section(_capped)(496-502) |
| `cli/second_pass.rs` | NEED_* 常量(351-356) / parse_need_lines(359) / is_needs_denylisted(369) / need_read_allowed(390) / need_section(412) / draft_block(599) / second_pass_prompt(622) |
| `cli/incremental.rs` | run_incremental_review(807) / IncrementalBaseline(1056) / incremental_preflight(1069) |
| `cli/mod.rs`（留守） | ReviewArgs / Ledger+load_ledger / parse_sev/parse_format / resolve_models / pack 预算常量+pack_budget_bytes+merge_bundles+build_packs / run_code_review / run_review_dispatch / **job 状态机全部原地不动**（JobCellSink/run_job/run_resume/partial_report/write_partial_result/reap_if_dead/mark_failed_panic/now_unix） |

测试随主体走；签名/逻辑/pub 面不变（仅 pub(crate) 路径调整）。

## 6. dossier → diffsym 改名

`src/dossier/` → `src/diffsym/`，独立机械 commit：模块名对齐现实职责
（diff/符号解析核心，被 pack/verify 活跃复用；引用点搜索引擎已随 E8-E 拆除）。

## 7. 契约冻结面

- **零变化承诺**：schema v3 字段集（不 bump）、exit_code 映射、resume 路径行为、
  全量路径行为。
- **预期行为变化（仅三处，修 bug/口径收敛）**：D4——增量报告跨轮同 bug 双条→单条
  （origin=Carried，stats 随之变）；T44——增量 --spec 从静默忽略变生效
  （无 --spec 运行零 token 轮廓变化）；duration_ms——增量轮收敛为不含 refute 趟
  （与全量口径一致，消除 base 时两路径求值点不一致；纯观测字段，终审 M-1 补记）。

## 8. 测试网

1. **tail 单测**：CarryCtx Some/None 两态直测 assemble_report 插入点
   （dispute/merge/Stats.new/RunMeta 五字段）。
2. **D4 回归**：quote 漂移场景 → 单条、origin=Carried、agreement 不加成、models 并集；
   is_same_bug 边界（行不重叠不并、title/quote 双低不并——守 under-merge）。
3. **T44**：second_pass_prompt 增量分支 spec 段在场；TailCtx.spec_docs 真值传递。
4. **迁移零语义门**：393 lib + 4 e2e 全绿 + clippy 零新告警（拆分 commit 单独验）。
5. **冒烟增量 e2e 1 例**：prior job 落库 → delta → `--incremental-from` →
   断言 carried+new 合并 / verdict / RunMeta.incremental 字段（完整矩阵留 M18）。

## 9. 范围闭集

本 design §2-§8 即全部范围。点名排除（见 decisions §A-Q7）：D2/D3/exit code
Incomplete/refute conf_floor/检查点吞错(M17-c)/T48/T49。
