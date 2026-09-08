# design · M21a A/B/C 合并后修补

> 需求见 `prd.md`。全部改动在 `feat/m21-review-engine` 直接落地（小修补，不开子分支）。

## 1. B 守卫（`src/db.rs::setup_schema`）
- 把「陌生表 → 拒开」移到版本分支之外：`if !have.is_empty() { foreign = have − TABLES; ensure!(foreign.is_empty()) }`，再进版本分支。
- `already_v4` / 旧库有数据拒开 / 空库重建逻辑不变。
- 只读快路径：`open_at` 中 `PRAGMA journal_mode=WAL` 移到「需要写库或以可写模式打开成功」之后；只读打开（`OpenFlags::SQLITE_OPEN_READ_ONLY`
  或 pragma 返回 readonly 错误）时跳过并 `eprintln!("warning: … 只读打开，跳过 WAL")`。若现有代码没有显式只读标志，用 pragma 失败降级实现。

## 2. C 引用边（`src/bundle/mod.rs` `group()` 引用边段）
- `owners: BTreeMap<String, Vec<usize>>` 不变；扫描 j 时先算 `self_owned = owners[name].contains(&j)`，为真则整个 name 对 j 跳过
  （不计 j → i）。语义：同名 changed def 是「各自定义」不是「引用」。
- 单测：`group_same_name_defs_do_not_make_reference_edges`（两文件各 `fn validate_input`，互不引用；`edges` 中无 `Reference`）。

## 3. C Rust `super`（`src/bundle/mod.rs::resolve_rust_path`）
- 现状：`allow_root` 允许顶层模块的 `super::` 解析到 `src/lib.rs` / `main.rs`。改为：`super::` 只在「当前文件不是顶层模块」时解析
  到父模块文件；顶层模块（父目录 == crate 根 `src/`，且文件不是 `lib.rs` / `main.rs` / `mod.rs`）内的 `super::` 视为指向自身 → 不成边。
- 保留：`src/a/b.rs` 的 `super::` → `src/a/mod.rs` 或 `src/a.rs`；`src/a/mod.rs` 的 `super::` → `src/lib.rs`（父是根，但 mod.rs 的
  super 是根模块，允许）。
- 单测：AC3 两例。

## 4. C 索引集成测试（`tests/review_e2e.rs` + `src/cli/mod.rs` 测试）
- lib 级：临时仓 4 文件两目录（`src/core/{a,b}.ts` 互相 import + `src/other/{c,d}.ts` 互相 import，每文件 ≥ 60 行改动使 churn ≥ 200），
  `run_pack_only(..)` → 读 `cell-1.md` / `cell-2.md`：各含对方组文件路径与箭头、不含自身文件行；`summary.grouping.gate == "grouped"`。
- 真二进制：`run(&["review", "--pack-only"])` 在临时 git 仓退出码 0。

## 5. A 校验与 key（`src/config/mod.rs`）
- `validate()`：`review.pack.anchorWindow`、`review.grouping.minFiles`、`review.grouping.maxFilesPerGroup` 进现有「≥ 1」数值表；
  `review.reviewers.maxCells` 单独：`MaxCells::Fixed(0)` → 错误。
- `env_nonempty(var) -> Option<String>`：`std::env::var(var).ok().filter(|v| !v.trim().is_empty())`；`resolve_key_with` 两处与
  `resolve_provider_key` 改用。

## 6. C `--pack-only` 不读 secrets（`src/config/mod.rs` + `src/main.rs`）
- `Config::load_with_messages_opts(read_secrets: bool)`；`load_with_messages()` = `true`；新增 `load_no_secrets() -> Result<(Config, Vec<String>)>`
  = `false`。`main.rs` Review 臂在 `pack_only` 时用 `load_no_secrets` 打印 messages 后走 `run_pack_only`（其余路径不变）。

## 7. followup
`.next/roadmap/followup.md` 实现债表新增：`cross_edges` / `weak_cut` 分种计数（F-5）、lib.rs 超级枢纽回放（出口 / M21b 前）、TS 副作用导入、
同顶层模块弱边、`est_tokens` 不含 system 段、`ScanOutput.suspicions` 无 default（D-1）、`over_budget` 未被 pack 消费 + 二阶段重建丢边（E-2）、
`CellOutput` tag 口径（D/F）、setup 预检对原文件无效的文案、并存期 secrets 提示。

## 8. 回滚
单分支小提交，逐条可 revert；无 schema 与数据改动（守卫只更严）。
