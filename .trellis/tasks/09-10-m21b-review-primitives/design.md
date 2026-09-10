# 批 B 技术设计：reviewer 静态零件

> 输入：prd + plan 批 B 节（B-1…B-6）+ design §7 / §10.2 / §10.7。决策 D6/D8/D9/D10-1、铁律 3/4/5 已定案，本文只做落地设计。
> 全批无真调用方：零件经合成输入 + `FakeChatBackend` 测试；接线在批 C。

## 1. 边界与文件

| 文件 | 动作 | 内容 |
|---|---|---|
| `src/pack/region.rs` | 新增 | `QueueItemSpec` + `RegionPack::build`（B-1）+ 三标配装配侧（B-5） |
| `src/prompt/templates/reviewer.md` | 新增 | reviewer 角色尾（B-2） |
| `src/prompt/mod.rs` | 修改 | `ReviewPromptInput` + `build_review_prompt` + `review_template_hash()`（B-2） |
| `src/provider/parse.rs` | 修改 | `parse_review_output_with_stats` + `REPAIR_PROMPT_REVIEW` + `ReviewParseStats`（B-3） |
| `src/provider/stream_json.rs` | 新增 | `IncrementalFindings`（B-4；见 §5 决策） |
| `src/provider/tool_loop.rs` | 修改 | `ChatBackend::on_text_delta` 缺省 None + `FakeChatBackend` 逐片 delta（B-4） |
| `src/provider/backends.rs` | 修改 | anthropic `text_delta` / openai `delta.content` 喂回调（B-4） |
| `src/cli/second_pass.rs` | 修改 | 读守卫抽公共函数（B-5，原调用点行为不变） |
| `src/domain/finding.rs` | 修改 | `#[cfg(test)] pub(crate) mod fixtures`（B-3 顺手项） |

与批 A 零交集（A：`src/risk/` + `src/cli/mod.rs` 接线段）。

## 2. `QueueItemSpec`（批 C 对齐契约）

```rust
pub struct QueueItemSpec {
    pub file: String,           // 仓库相对路径
    pub ranges: Vec<(u32, u32)>, // 行区间（1-based，含端点；区并集）
    pub origin: Origin,          // Rule | Scan | Needs（批 C C-1 的 QueueItem 复用此枚举）
    pub notes: Vec<Suspicion>,   // 疑点笔记；origin=Rule 时必须为空
}
```

批 B 只定义形状 + 构造校验（Rule 带笔记 → panic/Err 构造期报错）。C-1 `QueueItem` 与之对齐（字段同名，另加 score/agreement/state 等运行时字段）。

## 3. `RegionPack::build`（B-1）

签名：`build(item: &QueueItemSpec, bundle: &Bundle, root: &Path, cross_index: &str, spec_docs: &str, budget: usize) -> RegionPack`。

装配序（复用映射）：

| 段 | 复用机器 | 说明 |
|---|---|---|
| 区域切片核 | `body_section` / `skeleton_of`（pub(crate)，pack/mod.rs）| ranges 内 def 全文 + diff；构造即切片——不走 `PackOpts.body`（D8） |
| 一跳 / 文件地图 / 折叠 / 兜底梯子 | 同上（切片配方同构） | 小文件 ≤ sliceMinBytes 全文（slice==full 等价性沿用） |
| 爆炸半径邻居 | `outside_names` / `outside_section`（:638/:678）| caller body / callee 签名，受预算帽，超限进 omit 清单 |
| 跨组索引行 | `bundle::render_cross_index` 产出 | 只取本文件相关行（正反向） |
| spec 段 | `--spec` 已有装载 | 不可信证据注入 |
| 三方契约槽 | 新：空渲染函数 | `NeedKind::Api` 预留位，M21d 填；空时不占字节 |
| 笔记段 | 新：notes → 「一位评审者的笔记」 | origin=Scan\|Needs 渲染；origin=Rule 无此段；过 `neutralize_structure`（prompt/mod.rs:284） |

不变量：禁真空（ranges 的 diff 行全在 pack 内，逐行断言）；预算观测口径沿用 `PackStats` 的 `outside_sites/sigs/bytes` 计数形态。

## 4. prompt 与 parse（B-2/B-3）

- `ReviewPromptInput` 同构 `ScanPromptInput`（target_label/bundle/cross_index/precedents(M21c 空)/dossier(=RegionPack 正文)/spec_docs/repo_brief）；块序 = 共享头 → 参考材料 → 证据 → diff 次尾 → reviewer 角色尾（§10.3；前缀与 scanner 逐字节相同——`ScanPrompt` 的块骨架直接复用）。
- `reviewer.md` 角色尾：`<finding_bar>`（severity/title/body(危害链)/file/line/quote/confidence/recommendation/hard_constraint——字段名与 parse 一一对应，字段扫描测试锁）+ 措辞三件（「确认、驳回或补充」/ 笔记非事实 / H0「独立评审此区域」）。**载荷 tag = `reviewer`**（followup 已裁：`CellOutput` serde rename 即 `reviewer`，契约文本同步）。
- `review_template_hash()` = fnv1a64(shared_head) + "+" + fnv1a64(reviewer_tail)，同构 `scan_template_hash()`（prompt/mod.rs:152）。
- `parse_review_output_with_stats(raw) -> Result<(CellOutput::Review 载荷, ReviewParseStats)>`：
  - 前处理复用 `strip_pre`（parse.rs:10 抽出的公共前处理：`</think>` 切除 → 围栏剥离 → 配平扫描）；REPAIR 一轮用 `REPAIR_PROMPT_REVIEW`（同 `REPAIR_PROMPT_SCAN` 句式，指向 findings 对象；顶层缺 `findings` 键即违约走 REPAIR）。
  - 字段层：serde 反序列化 `Vec<RawFinding>` → quote 定位校验（复用 positioning 机器；失败丢弃 + `ReviewParseStats.dropped_quote` 计数）→ 转 `Finding`（id/models/agreement 等 B 批填默认空值/1，真值由批 C run_review_shot 按落地时刻填——D13）。
  - needs cap 5：超出截断 + 计数进 stats。
  - `ReviewParseStats` 形态对齐 `ScanParseStats`（:81）。

## 5. `IncrementalFindings`（B-4）与 SSE 旁路

**决策：新文件 `src/provider/stream_json.rs`**（plan 留白二选一）。理由：增量 JSON 解析是独立纯函数状态机（feed → objects closed → emit），与 parse.rs 的整体解析+REPAIR 职责正交；parse.rs 已 521 行。

```rust
pub struct IncrementalFindings { /* 缓冲 + 状态机 */ }
impl IncrementalFindings {
    pub fn new(cap: usize) -> Self;
    pub fn feed(&mut self, delta: &str);          // 只吃文本增量
    pub fn take(&mut self) -> Vec<RawFinding>;     // 取出已闭合对象
    pub fn discarded(&self) -> u32;                // 未闭合丢弃计数
}
```

- 状态机：定位 `"findings"` 数组（容忍前导思考/非 JSON 文本）→ 逐字符深度计数配平 → 对象闭合即 serde 解析为 `RawFinding` → 成功入 emit 队列、失败计入 discard；数组闭合后不再消费。
- `ChatBackend` 加 `fn on_text_delta(&self) -> Option<Box<dyn FnMut(&str) + Send>> { None }`（缺省实现，非破坏）；anthropic `content_block_delta.text_delta`（backends.rs:222）与 openai `choices[].delta.content`（:561 段）在现有拼接处同步喂回调——**回调只读，不改请求/聚合路径**。
- FINALIZE/REPAIR 最终整体解析不变（旁路不改契约；最终 `parse_review_output` 是权威）。
- `FakeChatBackend` 加逐片吐 delta 能力（构造时给 chunk 尺寸切 final 文本）。

## 6. 三标配（B-5）

1. **注入互斥**：`RegionPack` 装配时笔记段与 needs 证据段（second_pass 机器产出的段）至多一个——并存 → 构造期 `Err`。断言：构造期报错用例。
2. **预算帽**：区域 pack 总字节帽复用证据段预算口径（`build_pack` 的 budget 语义同构）；超限邻居进 omit 清单「降质 + 留指针」。
3. **读守卫**：`second_pass.rs` 的 `need_read_allowed`（:46，pub(crate)）提成 `pub(crate)` 公共函数（签名不动），region.rs 装配路径上的文件读取全走它；second_pass 原调用点零改动（行为不变测试）。

## 7. 兼容与回滚

- **逐字节不变锚**：无回调 backend（`on_text_delta` 缺省 None）请求/聚合行为不变——anthropic cache 三段界与 PR181 对拍基线依赖此；测试断言 SSE 累积输出与改前逐字节相同。
- scanner 路径零 diff：不改 `build_scan_prompt` / `scanner.md` / `parse_scan_output*` 行为；B-6 跑 `eval/scan_score.py` 验证不退化。
- 模板哈希入册：`template_hashes()`（prompt/mod.rs:142）加 reviewer_tail 哈希；scanner 哈希值不变（shared-head.md 与 scanner.md 零改动）。
- 回滚 = 不合回（子分支独立）；单 task 粒度 revert。
