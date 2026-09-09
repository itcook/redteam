# implement · M21a 批 E

> 顺序执行，每 task 一 commit（简体中文 `type(scope): 摘要——细节`），绝不 push；`make build` 在 F-1 前禁跑。决策编号见 `prd.md` K1-K17，定论见 `design.md`。

## 0. 预飞
- [x] `git status --short` 空、`git branch --show-current` = `feat/m21a-slice`、`git merge-base HEAD feat/m21-review-engine` = 4163c46。
- [x] 基线：`cargo test 2>&1 | grep "test result"`（记通过数：期望 499 / 16 / 7 / 16）、`cargo clippy --all-targets 2>&1 | grep -c "^warning"`（期望 3）、`make verify`。

## 1. E-1 文件内一跳（`src/diffsym/mod.rs`，`src/bundle/mod.rs` 迁移）
- [x] `ref_worthy` + `GENERIC_NAMES` 从 bundle 迁 diffsym（`pub(crate)`），bundle `use crate::diffsym::ref_worthy`；`cargo test bundle::` 绿（零语义变化）。
- [x] `DefRange`、`all_defs`（K4 可调用嵌套过滤）、`changed_defs`（K3 全部改动行）、`body_names`（跳注释行）、`OneHop` + `one_hop`（K5）。
- [x] 测试：`all_defs_skips_nested_in_callable_keeps_class_methods`（rs 局部 const + ts class 方法 + go 方法）、`changed_defs_anchors_every_changed_line`（单 hunk 跨两 def）、`one_hop_callee_and_caller_rust_go_ts`、`one_hop_excludes_self_and_generic_names`、`one_hop_ignores_comment_mentions`。
- [x] Commit：`feat(diffsym): 文件内一跳（改动 def 的同文件 callee / caller）——DefRange / all_defs / changed_defs 全改动行锚定，ref_worthy 迁入`

## 2. E-2 切片配方（`src/pack/mod.rs`）
- [x] `PackOpts` / `PackStats` / `Packed` / `pack_bundle` / `build_pack` 薄包装；`body_section` 收 `opts` + `stats`；`hunk_window_block` 窗宽参数化（K16，两处既有测试预算数字按 ±30 重算）。
- [x] `slice_file`：展开区间（核 ∪ 一跳 ∪ 禁真空窗）、保留行（文件头 ≤ 80 / 多行签名 ≤ 6）、渲染（折叠标记带范围与 def 名、胶水 ≤ 3 行原样）、块头统计。
- [x] 梯子：Slice 第一级 = 切片块，装不下走既有链；`Full` 路径逐字节不变。
- [x] 测试：`slice_no_vacuum_property_rs_go_ts_md`（每个 `+` 行逐字在输出；含 `use` 区改动、跨 def hunk、纯删除 hunk）、`slice_small_file_equals_full_bytewise`、`slice_expands_one_hop_and_folds_unrelated`（折叠标记 `…（a-b 行折叠：name）` 范围正确、无关大函数体不在）、`slice_keeps_file_header_and_all_signatures`、`slice_ladder_takes_over_when_over_budget`（def 体 / 骨架标注在场）、`slice_unsupported_lang_uses_hunk_windows`、`full_mode_output_unchanged`（与 E-2 前快照字符串对比）；既有 pack 测试全过。
- [x] Commit：`feat(pack): 改动中心切片——核/一跳/文件地图/折叠/禁真空兜底，slice_min_bytes 小文件全文（design §10.2〔D8〕）`

## 3. E-3 文件外片段（`src/pack/mod.rs`，`src/cli/mod.rs` 一行）
- [x] `pub(crate) mod deny;`；`TEST_PATH`（移植 `eval/risk_replay.py:221`）；`outside_section`（走仓 / 双集 / 排序截断 / 窗合并 / 签名 + doc / 段帽 / omit）。
- [x] 测试（tempdir 仓）：`outside_caller_window_and_callee_signature_with_doc`、`outside_caps_k_per_name`、`outside_budget_cap_spills_to_omit`、`outside_skips_changeset_tests_denylist_unsupported`（`tests/x.rs`、`.env`、`a.py`、变更集文件）、`outside_does_not_follow_symlink_out_of_root`、`outside_absent_in_full_mode`、`outside_is_deterministic`。
- [x] Commit：`feat(pack): 文件外片段——caller 调用点 ±N 与 callee 签名，受预算帽（design §10.2 文件外层）`

## 4. E-4 接线、观测与收口
- [x] `cli::pack_opts`；`build_packs` / `plan_cells` 加 `pack_cfg`（三调用点）；`CellPlan.pack_stats`；`PackCell.pack`；`render_pack_only` 追加 `body=… sliced=… small=… ladder=… folded_lines=… outside_sites=… outside_sigs=…`；`second_pass` 传 `PackOpts::full(N)`（K2）。
- [x] e2e `pack_only_slices_large_file_and_reports_shape`：> 8K 改动文件 + 他文件 caller；`Config::default()` → `sliced=1`、`outside_sites ≥ 1`、`summary.json` `cells[0].pack.body == "slice"`；`Config::from_json({"review":{"pack":{"body":"full"}}})` → `pack_bytes` 更大且无 `## 文件外片段`。四例既有 `pack_only_*` 不改断言。
- [x] 本仓 smoke（不提交产物）：`cargo run -q -- review --pack-only --base feat/m21-review-engine` 默认档；`REDTEAM_HOME=$(mktemp -d)` 写 `config.json` `{"review":{"pack":{"body":"full"}}}` 再跑一次；记录两档每 cell `bytes / est_tokens / pack_bytes` 与形态统计、`time` 墙钟（E-3 走仓耗时）到下方「收口记录」。
- [x] `cargo test`、`cargo clippy --all-targets`（≤ 基线）、`make verify`。
- [x] `.next/roadmap/followup.md`：① 切片层次 / 折叠体走 needs 的措辞交 D-2；② `--pack-body` 旗待裁；③ K=3 / N=30 / 1/8 帽 / 40 名归 R7；④ 关闭「over_budget 未消费」（K9）。
- [x] Commit：`feat(cli): [review.pack] body 接线——slice 默认、full 旋钮、--pack-only 报装包形态（PackStats）` + `docs(followup): 批 E 延后项——切片措辞交 D-2 / --pack-body 旗 / 常数归 R7 / over_budget 定论`（实际：build_packs 收 PackOpts 提前到 E-2、deny 提为 pub(crate) 在 E-3）
- [x] code-review 子 agent 审核（目标 = 本分支相对 `feat/m21-review-engine`，用户 2026-09-08 裁定不跑 ship）→ 修 → 记录到 §6。
- [ ] （用户）`--no-ff` 合回 `feat/m21-review-engine`。

## 5. 收口记录（E-4 填）
| 档 | cells | bytes | est_tokens | pack_bytes | sliced / small / ladder | outside_sites / sigs | 墙钟 |
|---|---|---|---|---|---|---|---|
| slice | 1（gate=grouped，over_budget=1） | 254366 | 72676 | 165916 | 5 / 0 / 0（folded_lines 3642） | 0 / 8 | 1.6 s（含走仓，user 0.64 s） |
| full | 1 | 319032 | 91152 | 230582 | 0 / 0 / 3（三文件超预算回落 def 体） | — | 0.4 s |

同一 diff（`feat/m21-review-engine..HEAD`，5 文件 +diff 82 KB），两档同预算（临时 `REDTEAM_HOME` 只含 `review.pack.body`，无模型 → 312K 缺省预算）。
slice 比 full 省 28% pack 字节 / 20% 全 prompt token；full 档三个大文件本就装不下走了 def 体，slice 档五文件全部以切片入包、零梯子。
outside_sites=0 因本批改动 def 的调用点只在变更集文件与 `tests/`（口径排除）；签名 8 条来自 diffsym / config 类型。E-3 走仓本仓（~5k 文件含 target/ 被 gitignore 跳过）1.2 s。

## 6. 评审修（2026-09-08，`/code-review high`，范围 feat/m21-review-engine...HEAD）
8 角度 42 候选 → 15 条送验证（14 CONFIRMED / 1 PLAUSIBLE），前十条全部核实成立并一次修掉（commit `fix(pack): 评审修复…`）：
1. 调用点名字帽按 BTreeSet ASCII 序截断 → 可调用 def 优先、填充序、上限 64；改动文件匹配 TEST_PATH / Rust `#[cfg(test)]` 后的 def 不搜。
2. 签名名混入 std 类型与局部变量挤占帽与每文件 200 命中 → 签名名只限 regex 体量（256）、渲染 ≤ 40；sink 只计调用点 / 签名候选行。
3. Go `var` / TS 无分号 `const` / Go `type ID int` 经 `block_end` 吞下一个函数 → `value_end` 三种括号配平。
4. 切片直接用 diff 坐标切工作树内容（staged + unstaged 拼接 / branch scope 脏树错位）→ 每 hunk 用 + 行文本核对，不符则 ±2000 行内找整体偏移；纯删除 hunk 保留原坐标由禁真空窗兜底。
5. cap≈0 仍整仓走 + omit 不受帽 → 帽 < 256 字节不走仓（一行 omit 说明）；超帽块逐条列 10 条其余合并计数。
6. `use` / `import` 行算调用点吃掉 K 配额 → `is_import_line` 排除。
7. 非支持语言「文件头」扫全篇 → 只看顶部 80 行。
8. 多行 import 组续行被折叠 → 按括号配平整组保留。
9. 二阶段恒 full 丢文件外片段 → `PackOpts::second_pass()`（全文块 + 文件外片段，K2 修订）。
10. denylist 敏感词子串跳过 `token_bucket.rs` 等源码 → `deny::is_credential_path` 拆出，走仓只用凭据判据。
帽外项（`--help` 已补装包形态文案；N 组 N 次走仓 / 截断非确定 / 增量 `extra_paths` 泄漏 PLAUSIBLE / over_budget 组切片仍大）记 followup。
修后：lib 524 / e2e 17 绿，clippy 7 = 基线，make verify 绿；本仓复跑 2 组 2.0 s（cell-2：sliced 6 / ladder 1 / outside_sites 1 / sigs 5）。

### 第二轮复审（2026-09-09，用户要求再跑一轮 `/code-review high`，范围同上 + 首轮十修）
8 角度 → 12 条送验证（11 CONFIRMED / 1 PLAUSIBLE），前十条：九条成立一次修掉（commit `fix(pack): 复审修复…`），一条不改：
1. 二阶段硬编码 `PackOpts::second_pass()` 无视 `review.pack`（full 对照臂被切成 slice）→ `second_pass(&user)` 只改 `slice_min_bytes`，`second_pass_prompt` 穿 `PackOpts`（两处调用方传 `cfg.review.pack`）。
2. 重锚定只在 slice_file，梯子与 outside_names 仍用陈旧坐标 → `FileCtx` 一次读取 / 编译 / 重锚定，全部消费者共用（顺带修帽外效率项：改动文件读 3 次、compile_defs 2 次、classify 跑两遍）。
3. `is_import_line` 含 `export ` 短路掉 `export function foo(` → export 只算 re-export；连带 `body_names` 剔除能过 ref_worthy 的关键字（`function` 等）。
4. `anchor_offset` 精确全匹配 + 低信息 `}` 行误锚 → 只按有信息量 + 行打分，容忍个别行被改，纯括号行不定位。
5. 首个 `#[cfg(test)]` 之后一刀切 → `rust_test_ranges`（mod 级整块 / item 级单 item）。
6. `value_end` 把字符串 / 注释里的括号算进深度（`const RE: &str = r"(";` 吞后续 fn）→ `bracket_delta_code`。
7. sig_of ASCII 序截断偏向大写 std 名 → 小写开头优先再截。
8. 二阶段 `changeset_paths` 不含增强包文件 → needs 文件同内容双份 → 排除集并上 `pb.files`。
9. `\b` 边界让 `items$` 永不命中 → 裸 alternation + 手工标识符边界（非消耗式，顺带修「相邻两名互吃边界」）。
10. **不改**：非支持语言 hunk 窗 ±20→±30 破坏「full = 现状」——plan E-2 原文即 `hunk_window_block(±anchor_window)`，prd K10 / K16 已明示例外；E2 A/B 的两臂都在同一二进制内比较，dev 基线是单独归档的对照集。
帽外：`changed_def_names`（分组边）与 `outside_names`（文件外键）口径分叉（hunk 首行 + block_end vs 全改动行 + value_end + 测试排除）记 followup；`is_comment_line` 已统一到 diffsym；`hunk_window_block` 上界多算一行为基线既有。
修后：lib 534 / e2e 17 绿，clippy 7 = 基线，make verify 绿。

## 验证命令
```
cargo test
cargo test diffsym:: pack::
cargo clippy --all-targets
make verify
cargo run -q -- review --pack-only --base feat/m21-review-engine
git diff --stat feat/m21-review-engine..HEAD -- src/prompt src/config src/orchestrate src/state src/db src/domain src/main.rs plugins   # 须为空
```

## 风险文件 / 回滚点
- `src/pack/mod.rs`：重写面最大；`Full` 路径逐字节回归测试是护栏。
- `src/cli/mod.rs`：与 D 潜在碰撞面，只加参数 / 字段。
- 回滚 = 逆序 revert 四个 commit；运行期逃生口 `review.pack.body = "full"`。
