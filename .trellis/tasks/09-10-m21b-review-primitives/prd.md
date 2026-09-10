# M21b 批B：reviewer 静态零件——区域 pack/reviewer.md/parse/SSE 流式旁路/三标配

## Goal

reviewer cell 的全部静态零件：`RegionPack` 构造即切片（+爆炸半径+跨组索引+spec+笔记，三方契约槽留空 M21d）、`reviewer.md` 角色尾与 `build_review_prompt`（共享头字节稳定）、`parse_review_output`（REPAIR 一轮）、ChatBackend `on_text_delta` + `IncrementalFindings` 流式旁路（finding 对象闭合即回调、未闭合丢弃计数）、三标配（注入互斥/证据预算帽/读守卫）。全部可用合成输入 + `FakeChatBackend` 测试，不依赖队列（批 C 接线）。

## Requirements

- 区域 pack：由构造即切片（不受 `[review.pack] body` 控制，D8）；复用 M21a 切片配方与文件外片段机器；笔记段 = 「一位评审者的笔记」（origin=scan|needs），H0（origin=rule）无笔记段；契约槽空渲染（M21d 填）；注入段全过 `neutralize_structure`
- `reviewer.md`：措辞「确认、驳回或补充」、笔记非事实（防锚定）、H0「独立评审此区域」；输出契约与 parse 字段级对齐（铁律 5）；段序与 scanner 同构（前缀稳定，铁律 3）；模板哈希
- parse：复用 `</think>` 切除/围栏剥离/配平/REPAIR 一轮形态；quote 定位失败丢弃 + 计数；needs cap 5；载荷 tag 值 `reviewer`（followup 已裁接缝：`CellOutput` tag == Role 串 == `cells.role` 列，**不是** plan 文本的 `review`；`<output_contract>` 与 parse 同步按此写）
- SSE 流式旁路〔D10-1〕：`ChatBackend` 非破坏扩展（缺省 None 时行为逐字节不变——cache 与对拍基线的锚）；anthropic `content_block_delta` 与 openai `delta.content` 两处喂回调；`findings` 数组增量 JSON 解析，对象闭合即 emit；未闭合 discard 计数进 warning；FINALIZE/REPAIR 仍管最终整体解析（旁路不改契约）；fake 支持逐片 delta
- 三标配（铁律 4）：笔记段与 needs 证据段互斥（构造期报错）；区域 pack 预算帽；读守卫复用 `second_pass.rs` 机器（抽公共函数，原调用点行为不变）
- 模板回归：scanner 路径零 diff（`eval/scan_score.py` 不退化）
- 任务级明细：plan §批 B（Task B-1…B-6）

## Acceptance Criteria

- [x] `cargo test` / `clippy` / `make verify` 全绿；改动只在批 B 列出的文件与测试（外加必要延伸：bundle/mod.rs 索引函数、Cargo.toml reqwest+stream、positioning 计数版 anchor——均在任务语义内）
- [x] 禁真空性质测试（ranges 的 diff 行全在 pack 内）；小文件 slice 与 full 逐字节相同
- [x] 共享头与 scanner prompt 头逐字节相同；模板哈希入册；模板字段名与 parse 一一对应（字段扫描测试）
- [x] 流式旁路：三 finding 分片 → 三次 emit + 最终聚合一致；截断 → 已闭合保留、余下 discard 计数；无回调 backend 逐字节不变
- [x] 互斥断言（笔记 + needs 证据并存 → 构造期报错）；读守卫命中用例（untracked + denylist）
- [ ] 收口：dev `redteam` ship 评审通过 → `--no-ff` 合回

## Out of Scope

- 队列接线与 reviewer 运行时（批 C 独占：`src/orchestrate/`、`src/cli/mod.rs` 主流程、`src/state/`）；B 批零件全部合成输入 + `FakeChatBackend` 可测，无真调用方
- `verifier.md` 与复核消费（批 D）
- 三方契约注入内容（M21d——本批只留 `NeedKind::Api` 空渲染槽）
- 判例摘要槽（M21c 台账数据才有）
- Anthropic thinking×tools 守卫（C-3 顺手项，本批不可达：B 批无工具循环接线）
- FINALIZE/REPAIR 三层简化评估（M21e；本批 REPAIR 一轮形态照抄不动三层）
- 任何新配置旋钮 / 旗（D10-1 流式无旋钮；thinkingLevel 走角色属性 config，M21a 已建形）

## Notes

- design.md / implement.md 不预写：开工时 grill 或脑暴产出（输入 = 本 prd + plan 批 B 节 + design §7 / §10.2 / §10.7）。
- 决策依据：D6（段序）、D8（构造即切片）、D9（角色尾/回归集）、D10-1（流式持久化无旋钮）；铁律 3/4/5。
- 批 C 接口：`RegionPack::build` 的输入类型（队列项形状 `{file, ranges, origin, notes}`）在批 B 先以 spec struct 定义，批 C 的 `QueueItem` 对齐它。
