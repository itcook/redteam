# 狗粮评审 findings 处置（2026-09-03，作业 c718ce4d7e58）

修复完成后 GLM(anthropic 流式+32K) 对本任务 diff 的完整评审（reject, 3 medium + 1 low）——
对抗评审审出了流式实现自身的边界洞，全部处置如下：

| # | 标题（缩写） | 处置 |
|---|---|---|
| F1 | `contains("data:")` 子串判定误路由 JSON→SSE→Parse 烧格 | **已修**：三层分流（整体 JSON 优先 → 行首 `data:` 锚定 `looks_like_sse` → envelope 兜底），anthropic 与 openai 双路径镜像；+2 单测 |
| F2 | 截断流被当完整轮；畸形 tool args 静默落 `{}` 跑垃圾 | **已修**（anthropic 侧）：finish_reason 完整性门（None → Unavailable 可重试）；args 非空解析失败置 malformed 传导为 finish_reason=None；+1 单测。openai 侧 finish_reason 门**未加**——兼容网关 finish_reason 到达率未知，盲加会把正常流误判截断；记录于此待实测数据 |
| F3 | 默认 32768 超 Claude 3/3.5 输出上限 → 400 | **dismiss 建卡**（产品权衡）：40K 实测输出需求 vs 老模型上限冲突，env 按模型调低；provider 级配置留 followup |
| F4 | env 旋钮静默忽略非法值 | **已修**：两旋钮非法值 eprintln 警告（对齐仓内 warning 先例）。读取时机不一致（per_turn 构造快照 vs max_tokens 每 turn 重读）维持现状——行为语义一致，快照是有意设计 |

## 验证

- `cargo test --lib`：449 passed（含新增 3 测试）
- 狗粮三跑端到端：anthropic 流式 + 32K + 600s → 完整 reject 报告（40.3K output tokens，
  缓存命中 50%，6.1 分钟）——本轮 F1/F2/F4 修复在其后，下轮狗粮自然复验。

## 顺势交付（同日，PR181 dogfood 直接产出，超出本任务原始范围）

- **T54 resume 并行化**：`run_resume` 串行 for → `join_all` 保序并行（wall-clock Σ→max；append_partial Mutex 单连接无写冲突）
- **cell 级可观测（T55 治法之一）**：`RunMeta.cells`（serde default 兼容旧 JSON）+ result md `Cells` 表（model/iter/att/duration/tokens），四条路径统一 attach（fanout Done / write_partial_result / resume 尾段 / result md 现算）
- 实证数据修正 T55 假说：GLM 单轮 19min/30.9K tok（27 tok/s 含思考，iterations=1 无 repair）——吞吐下限而非恶性循环；Kimi 3×600s 全超时系单轮思考 >10min
