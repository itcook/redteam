# implement · M21a A/B/C 合并后修补

三道门每步：`cargo test`、`cargo clippy --all-targets`（零新增）、`make verify`。两笔提交：X-1…X-3（bug 级）一笔，X-4…X-6（边角 + followup）一笔。

## X-1 B 守卫 + 只读快路径（`src/db.rs`）
- [x] 陌生表检查移出版本分支；测试 `foreign_v4_versioned_db_is_refused_untouched`。
- [x] `journal_mode=WAL` 切换失败时：刚写过库仍报错，只读快路径降级 warning；测试 `readonly_delete_mode_v4_db_opens_with_warning`。

## X-2 C 分组与解析（`src/bundle/mod.rs`）
- [x] 引用边跳过 self-owned 名；测试 `group_same_name_defs_do_not_make_reference_edges`。
- [x] 内联 `mod x {` 块栈（行级花括号计数）→ 块内 `super::` 指向自身；测试 `import_edges_rust_inline_mod_super_points_to_self_not_root`（顶层 / 嵌套 / 块后恢复）。

## X-3 C 索引集成测试（`tests/review_e2e.rs`）
- [x] `pack_only_two_groups_cross_index_lists_other_group_only`（gate=grouped 两组，索引只列他组、正反向箭头）；
  `pack_only_binary_exits_zero_without_reading_secrets`（真二进制 exit 0）。

## X-4 A 校验与空串 key（`src/config/mod.rs`）
- [x] `validate()` 加 `pack.anchorWindow` / `grouping.minFiles` / `grouping.maxFilesPerGroup` ≥ 1、`MaxCells::Fixed(0)` 拒；测试扩 `validate_aggregates_kind_window_and_zero_knobs`。
- [x] `env_nonempty`（三处 env 读取）；测试扩 `key_resolution_chain_four_levels`。

## X-5 `--pack-only` 不读 secrets（`src/config/mod.rs`、`src/main.rs`）
- [x] `load_with_messages_opts(read_secrets)` + `load_no_secrets`；Review 臂 `pack_only` 用 `load_config_no_secrets_or_exit`；测试 `load_no_secrets_skips_secrets_file` + 上述二进制 e2e。

## X-6 followup + 提交
- [x] `.next/roadmap/followup.md` 新增九行（design §7；`two_layer_manifest` 已有行不重复）。
- [x] 三道门：`cargo test` 499+16+7+16 绿、clippy 3 条既有、`make verify` 绿。
- [x] Commit 1 820bc46 `fix(m21a): db 陌生库守卫补 v4 分支 + 只读快路径 WAL 降级；同名 def 伪引用边；顶层模块内联 mod 的 super 假边`（db.rs / bundle）
- [x] Commit 2 093b454 `fix(m21a): 多组跨组索引 e2e；pack/grouping 零值与空串 env key 校验；--pack-only 不读 secrets`（config / main / e2e）
- [x] Commit 3 18d9b7c `docs(followup): A/B/C 合并核验接缝九条`

## 风险文件
`src/db.rs`（守卫）、`src/bundle/mod.rs`（分组）、`src/config/mod.rs`、`src/main.rs`、`tests/review_e2e.rs`。不碰 `src/prompt/`、`src/pack/`（D / E 领地）。
