# 批 B 实施清单

> 顺序 B-1 → B-6（B-3 的 fixtures 顺手项随 B-3 做；B-5 依赖 B-1 的 region.rs 存在）。每 task 一 commit，简体中文 `type(scope): 摘要——细节`，绝不 push。

## Checklist

### B-1 区域 pack 构造
- [x] `src/pack/region.rs`：`QueueItemSpec`（含 Origin 枚举 + Rule 不带笔记构造校验）
- [x] `RegionPack::build`：区域切片核（`body_section`/`skeleton_of`）+ 一跳/地图/折叠/兜底 + 爆炸半径（`outside_names`/`outside_section`）+ 跨组索引本文件行 + spec 段 + 契约空槽 + 笔记段（`neutralize_structure`）
- [x] 测试：禁真空（ranges diff 行全在 pack）；笔记段中和；预算超限进 omit；H0 无笔记段；小文件 slice==full 逐字节；契约槽空渲染零字节
- [ ] Commit: `feat(pack): reviewer 区域 pack——构造即切片+半径+跨组索引+spec+笔记，契约槽留空（design §7/§10.2）`

### B-2 reviewer.md + build_review_prompt
- [x] `src/prompt/templates/reviewer.md`：`<finding_bar>` 字段名与 parse 一一对应；tag `reviewer`；措辞三件
- [x] `src/prompt/mod.rs`：`ReviewPromptInput` + `build_review_prompt` + `review_template_hash()` + `template_hashes()` 扩
- [x] 测试：共享头与 scanner 逐字节相同；角色尾契约关键字段扫描；模板哈希入册；scanner 哈希不变
- [ ] Commit: `feat(prompt): reviewer.md 角色尾 + build_review_prompt——共享头不变、段序同构、模板哈希（design §10.7〔D9〕）`

### B-3 finding 契约 parse
- [x] `src/provider/parse.rs`：`parse_review_output_with_stats` + `ReviewParseStats` + `REPAIR_PROMPT_REVIEW`（前处理复用 `strip_pre`）
- [x] RawFinding → quote 定位校验（失败丢弃+计数）→ Finding 默认值填充（id/models/agreement 真值批 C 填）；needs cap 5
- [x] 顺手项：`src/domain/finding.rs` `#[cfg(test)] pub(crate) mod fixtures`（finding 构造三份合一 + `NeedEntry::evidence`）
- [x] 测试：围栏/前言/截断/quote 不在文件/needs 超帽；REPAIR 一轮成功路径
- [ ] Commit: `feat(parse): reviewer finding 契约 parse——字段级对齐 + REPAIR 一轮（铁律 5）`

### B-4 SSE 流式旁路
- [x] `src/provider/stream_json.rs`：`IncrementalFindings`（feed/take/discarded；数组定位容忍前导噪声；闭合即解析）
- [x] `src/provider/tool_loop.rs`：`ChatBackend::on_text_delta` 缺省 None；`FakeChatBackend` 逐片 delta
- [x] `src/provider/backends.rs`：anthropic `text_delta`（:222 段）与 openai `delta.content`（:561 段）喂回调（只读旁路）
- [x] 测试：三 finding 分片 → 三 emit + 终态一致；中途截断 → 已闭合保留 + discard 计数；无回调 backend SSE 累积逐字节不变；与 `parse_review_output` 最终一致
- [ ] Commit: `feat(provider): SSE 流式旁路——增量 JSON 解析、finding 对象闭合即落盘回调、未闭合丢弃计数（design §7〔D10〕）`

### B-5 三标配
- [x] `src/pack/region.rs`：笔记段 × needs 证据段互斥（构造期 Err）；区域 pack 总字节帽
- [x] `src/cli/second_pass.rs`：读守卫 `need_read_allowed` 提公共（签名不动，原调用点零改动）
- [x] 测试：互斥构造期报错；预算帽命中；读守卫命中（大文件/越界路径）；second_pass 行为不变
- [ ] Commit: `feat(pack): reviewer 三标配——注入互斥+证据预算帽+读守卫（铁律 4，second_pass 机器复用）`

### B-6 收口
- [x] 模板回归：`eval/scan_score.py` PR181 不退化（scanner 路径零 diff 即过：模板文件零改动 + 哈希不变断言绿；PR181 fixture 在主工作树 .next/private，本工作树不重跑） PR181 不退化（scanner 路径零 diff）
- [x] `cargo test`（577 lib + 17 e2e）+ `cargo clippy --all-targets` 零新增 + `make verify` 绿；改动文件与清单一致
- [ ] ship 评审（dev `redteam`）→ 修 → `--no-ff` 合回 `feat/m21-review-engine`
- [ ] Commit: 按 ship 评审修题

## 验证命令

```bash
cargo test                          # 全绿（含新增 region/prompt/parse/stream_json 测试）
cargo clippy --all-targets          # 零新增
make verify                         # 绿
python3 eval/scan_score.py …        # B-6：scanner 基线不退化（PR181 fixture 路径同 M21a 口径）
```

## 风险文件与回滚

- `backends.rs`（1519 行，SSE 累积核心）：只加喂回调行，不动解析逻辑；逐字节不变测试是门。回滚 = revert B-4 commit。
- `tool_loop.rs` trait 扩展：缺省方法非破坏，全部现有 impl 零改动即编译。
- `parse.rs` / `prompt/mod.rs`：只增不改现有函数；scanner 哈希不变断言锁。
- 整批回滚 = 不合回 `feat/m21-review-engine`。

## task.py start 前

- [ ] implement.jsonl / check.jsonl 各 ≥1 条真实条目（本文件 + design.md + plan/design 章节）
- [ ] 用户批准最终规划摘要
