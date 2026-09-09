# implement · M21a 批 D

三道门每 task：`cargo test`、`cargo clippy --all-targets`（零新增）、`make verify`。每 task 一笔提交，绝不 push。

## D-1 parse + anchor
- [x] `parse.rs`：`extract_json_object` 抽出（`parse_lens_output` 改调）；`parse_scan_output` / `parse_scan_output_with_stats`；`REPAIR_PROMPT_SCAN`。
- [x] `positioning`：`AnchoredScan` / `anchor_suspicions`。
- [x] 测试：`scan_parse_regularizes_each_item_and_counts_drops`、`anchor_suspicions_rewrites_lines_and_counts_dropped`；既有 parse 测试不变。
- [x] Commit f84b5d7。

## D-2 / D-3 模板拆分 + 段序 + 注入互斥（合一笔：`build_scan_prompt` 一次成形）
- [x] `templates/shared-head.md`、`templates/scanner.md`；`build_scan_prompt` 四块 + `cache_boundary`；`DOSSIER_STRUCTURAL_TAGS` 扩 8 标签；`flatten`。
- [x] 测试：共享头字节稳定 / 字段名扫描 / 段序与断点 / 各槽伪造闭合标签中和；旧 prompt 测试全部不变。
- [x] Commit e360cdf。

## D-4 三段界 cache + 模板哈希
- [x] `ChatMessage::UserBlocks`；anthropic system/blocks `cache_control`（显式断点在场时末块不自动加）；openai 拼接；`template_hashes()`；`to_record` 填 `template_hash`。
- [x] 测试：`user_blocks_three_cache_boundaries_and_openai_flatten`、`template_hashes_stable_and_distinct`、e2e cell 记录带哈希。
- [x] Commit 86521b2。

## D-5 评分器
- [x] `eval/_oracle.py`、`eval/scan_score.py`（score / selftest / --baseline / --fail-on-regression）、`eval/README.md`；本机对 E3 B 三跑复算：union 9 / mean 5.33 / noise 0.55 与 score-B.json 一致。
- [x] Commit（chore(eval)）。

## D-6 收口
- [x] code-review 子 agent（1 必修 + 11 建议 + 10 nit）→ 修复提交 ced42e7：user_focus 框架语、scan/legacy 中和表分离（旧 prompt 逐字节不变）、
  骨架入哈希、行号 clamp、路径 / ranges 规范化、`locate_quote_near` 就近命中 + `dropped_unreadable`、分块会话尾断点、评分器口径与边角；
  未修（记 followup）：结果 JSON 无 scanner 输出面（F）、增量轮框架语无槽（M21c）。三道门：509+16+7+16 绿、clippy 3 条既有、make verify 绿。
- [ ] 主工作树 `git merge --no-ff feat/m21a-prompts`（退出本工作树后）。

## 风险文件
`src/provider/{parse,tool_loop,backends,single_shot}.rs`、`src/positioning/mod.rs`、`src/prompt/{mod.rs,templates/*}`、`src/orchestrate/mod.rs`、`eval/*`。
不碰 `src/pack/`（E）、`src/bundle/`、`src/config/`。
