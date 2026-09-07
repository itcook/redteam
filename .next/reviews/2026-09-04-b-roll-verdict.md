# B roll 结论：aisdk 0.5.2 vs 自建收发层（2026-09-04）

> 状态: 已交付 · 同卷宗（PR181 bundle, base a7c827b70）同 focus 同 600s 旋钮，Kimi 单模型对照

## 数据

| | 自建版（1200s per-provider + 32K 显式） | SDK 版 aisdk 0.5.2 |
|---|---|---|
| 结果 | ✓ 完整产出（~10K 输出 + 3 findings） | ✗ 烧 cell（作业 2b9f189f9474） |
| 输出 | 正常收官 | **恰好 8192 整数撞顶**（AGC 网关默认上限） |
| finish_reason | max_tokens 如实可见 + F2 完整性门 | **丢失**——适配层只能谎报 "stop" |
| 后果 | — | 截断 → structured-output parse failed → fail |
| usage 计量 | T56 低报（GLM/openai 路径） | **166.5K in / 99% cache 全对**（anthropic 路径） |
| 一跑（适配层修正前） | — | `stream ended without End`——SDK channel 不透传 End/usage，终态须从 StreamTextResponse 聚合访问器取 |

## 结论

1. **原始怀疑（自建层未考虑规范化输出边缘）反证成立**：SDK 在同样问题上摔得更狠——
   max_output_tokens 无公开 setter（anthropic conversions unwrap_or(10_000)，网关再
   clamp 到 8192）、AssistantMessage 无 finish_reason（截断不可见）、截断 JSON 烧 cell。
   SDK 完整重演自建版 8192 时代剧本；自建的显式 32K 旋钮 + F2 门是正确且更完善的解。
2. **SDK 唯一胜点 = usage/cache 计量**（99% 命中可见）→ T56 破案线索：AGC 网关 usage
   数据是全的，自建 openai 路径读取方式漏字段（参考 SDK anthropic conversions 读法）。
3. **裁决：不合入 dev**。b-roll/aisdk 分支保留作证据；适配层（aisdk_backend.rs）留档。

## 附：SDK 适配陷阱清单（供后来者）

- stream channel 只透传 Text/Reasoning 两类 Delta；End(AssistantMessage) 与 usage 不经
  channel（Done 分支被 SDK 内部消化更新自身状态）——终态必须走 resp.usage()/content()。
- MessageBuilder 阶段机：system/user 仅 Initial（Message::builder() 起步），后续仅
  user/assistant 链式。
- ProviderStream 私有；LanguageModelStream 无 Default（mem::take 不可用，用借用消费）。
- ProviderKind 新变体需同步 config as_str 的 non-exhaustive match。
