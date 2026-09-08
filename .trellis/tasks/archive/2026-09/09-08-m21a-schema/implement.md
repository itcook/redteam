# implement · M21a 批 B schema v4

工作树 `~/hobby/.worktrees/redteam/m21a-schema`，分支 `feat/m21a-schema`（自 `feat/m21-review-engine` c382e54 切出）。每步：改 → `cargo test` →
`cargo clippy --all-targets` → commit（不 push）。`make build` 全程不跑（会覆盖插件 dev `redteam`）。

## 步骤

### B-1 导出固定集
1. `eval/export_ledger.py`（argparse；`sqlite3` + `json`；照 `eval/p0_oracle.py:178` 的 upheld 读法）。
2. 跑 `python3 eval/export_ledger.py --db ~/hobby/redteam/.redteam/proj.db` → 期望 `dismissed 63 / upheld 48`；抽查两行字段齐全、`file` 无公司仓路径。
3. `eval/README.md` 加一行说明；`git add eval/export_ledger.py eval/fixtures/`。
4. Commit：`chore(eval): 导出旧 proj.db 的 dismissed / upheld 固定集（verifier 离线回归集数据，D10）`

### B-2 domain
5. `src/domain/suspicion.rs`（design §2）+ `src/domain/mod.rs` `pub mod suspicion; pub use suspicion::*;`。
6. `finding.rs`：`NeedKind` + `NeedEntry.kind`（default）+ `Serialize`；补齐 `orchestrate/mod.rs:444`、`second_pass.rs` 六处字面量。
7. 测试（suspicion.rs 内）：三变体 round-trip；缺 tag 报错；`NeedEntry` 无 kind 可读；`Category` 串形；`CellOutput::findings()` 对非 Review 为空。
8. Commit：`feat(domain): 疑点 schema（suspicion / category / scanned_areas / needs.kind）+ 按角色分叉的 CellOutput 枚举（D11 R14）`

### B-3 CellRecord v4 + proj.db v4
9. `src/db.rs`：版本守卫（design §3）→ 建表（cells 加 role 列、两新表）→ `user_version = 4`；`cell_put` 加 `role` 参数。
10. `src/state/mod.rs`：`CellRecord` v4、`ok` / `failed` 构造、`family_of`；`append_partial` 传 `record.role.as_str()`。
11. 消费点适配（design §4）：`orchestrate/mod.rs` `to_record`、`cli/mod.rs` partial_report / run_resume / 合并、测试字面量、`tests/review_e2e.rs:294`。
12. 测试：空库 9 表 + `user_version 4`（`sqlite_master` 查询）；手工建 `user_version 0` + dismissed 一行 → `open_at` Err 且文案含 `export_ledger.py`；
    `user_version 0` 全空 → 打开成功且 v4；CellRecord round-trip；`read_partial` 读回 role；`cells.role` 列值 = 记录 role；`family_of` 三例。
13. Commit：`feat(state): CellRecord 带 role/output 枚举/template_hash；proj.db schema v4 一次重置（cells.role + queue_items + suspicions 表壳）`

### B-4 作业元数据
14. `report.rs`：`SCHEMA_VERSION`、`Coverage` / `CacheHit`、RunMeta 五字段；七处 `"3"` 字面量改常量（`cli/mod.rs:473,2155`、`tail.rs:167`、
    `incremental.rs:315,495`、`tests/review_e2e.rs:612`）；`output/mod.rs:194` 测试的 `"2"` 保留（v2 形状锚）。
15. `state/mod.rs`：`JobState.roles`。
16. 测试：默认态不含五键；赋值 round-trip；旧 v2/v3 JSON 可读；`render_md` 快照不变（既有 `output` 测试即覆盖）；`JobState` 旧 JSON 无 roles 可读。
17. Commit：`feat(report): RunMeta 加 coverage 五集合 / 模板哈希 / cache 命中 / 在途峰值 / refute 率字段；report schema_version 4`

### B-5 收口
18. `cargo test` / `cargo clippy --all-targets` / `make verify` 全绿；`git status` 干净；`git log --oneline -4` 对照 plan 文案。
19. 收口审核改为 code-review 子 agent（用户 2026-09-08 定：redteam 核心能力稳定前无需也不能跑 ship 评审）→ 修 → 由用户 `--no-ff`
    合回 `feat/m21-review-engine`；新问题记 `.next/roadmap/followup.md`，不顺手修。已执行：审核 + 修复两笔（03b5c6c / 3ac2332）。

## 验证命令

```bash
cargo test 2>&1 | tail -3
cargo clippy --all-targets 2>&1 | grep -c warning   # 与切出点基线比较，零新增
make verify
python3 eval/export_ledger.py --db ~/hobby/redteam/.redteam/proj.db
wc -l eval/fixtures/ledger-*.jsonl                      # 63 / 48
```

## 风险文件 / 回滚点

- `src/db.rs` `open_at`：守卫写错会把正常空库拒开（`init` 路径）——AC3 三分支测试必过；回滚 = revert B-3 commit。
- `src/cli/mod.rs:573-604`：与批 A-4 `resolve_models` 邻近，合回顺序在后者 rebase 一次。
- `CellRecord` 字段变更牵连十余处测试字面量：先加 `CellOutput::review(findings)` 辅助再批量替换，避免手改漏项。

## `task.py start` 前检查

- prd / design / implement 三份在场；`implement.jsonl` / `check.jsonl` 各有真实条目。
- 用户已对最终规划摘要明确批准。
- `git status` 干净、分支 `feat/m21a-schema`、HEAD = c382e54。
