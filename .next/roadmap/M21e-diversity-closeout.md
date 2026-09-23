# M21e · 多样性与收官

> 状态: ⏳ · 依赖 M21c, M21d · design §5 / §15 / §17
> 出口：M21 关账；M19 发布线接管 `--help` / README / tag

## 目标

打开多样性轴，补上度量缺口，把校准项收口，拆掉仪器，让 CLI 面与文档跟上新形态。

## 交付项

| # | 项 | design | 落点 |
|---|---|---|---|
| 1 | 第二家族 scanner 落地验证：第二家族模型加 `scanner` 角色（JSON `roles`，D12），疑点并集与 agreement 入优先级键（机制 M21a 已有，此处用 PR181 + dogfood 量收益）；疑点合并口径（quote 轻锚 + hint 归一）收口 | §5 | `src/orchestrate`, `src/consolidate/dedup.rs` |
| 2 | 旧 CLI 面残余清理：`--models` / `--models-all` / `priority` 的文档与错误提示；`resolve_models` 角色驱动版收口 | §15 消亡 | `src/cli/mod.rs` |
| 3 | **aacr `gate` 三方对照**（D7 重启点）：先 `--pack-only` spike 定规模（R20）；legacy / v2 / ocr 三方；ocr = deepseek-v4-pro 直连 chat-completions 流式、版本钉入元数据；GLM judge、`--eval-rounds 3`、正样本 N=3、负样本 N=1；数字写回 research；Claude D 参照后置 | design §16 | `~/codes/aacr-bench`；本仓 `eval/README` |
| 4 | 校准收口：R3 分组阈值、R8 semantic grouping 触发阈值（跨组 import 边数）、R9 provider `maxConcurrency` 缺省值（角色 workers 已删〔D13〕）、R12 激活上限 N、同组 hold 是否加（派发时 / 落地时 agreement 数据〔D13〕）；全部落配置默认值 | — | `src/config` + dogfood 数据 |
| 5 | 仪器拆除（铁律 1）：dev 二进制基线数字归档后不再维护；scratch 实验脚本归档到 `eval/`；`skipped(budget)` 过渡披露并入台账口径 | — | — |
| 6 | `--help` 定稿（裸终端唯一在场文档）；README / 安装 / 快速上手按新 CLI 面重写；codex-skills `make gen` 同步；`make verify` 零 diff | CLAUDE 原则 | `plugins/redteam/commands/`, Makefile 流程 |
| 7 | 关账：`.doc/roadmap/done.md` 记 M21 明细；本目录全部里程碑 ✅；design 状态头「已交付」 | — | — |

## 验收

- 第二家族 scanner 开：PR181 疑点 oracle 覆盖 ≥ 单家族 + 增量可观测；墙钟不增；input 增量 ≈ 条目数倍。
- aacr `smoke`（**从 M21c 项 7 移入，D19-2**）：fork 上 5 个 rs / go / ts 正样本，legacy 与 v2 各 N=1，取 Line-P / Line-R；
  `reviewers/redteam.py` 适配器与 pipeline 挂载随本项接线。移入理由：外部仓、D7 B4 已暂退役、重启点待定，
  不拿一个没在跑的外部基准卡住切名；跨语言召回这条轴在此之前**没有外部对照**（已知缺口）。
- aacr `gate` 跑通，给出 legacy / v2 / ocr 的 Precision / Recall / Noise / 墙钟 / token 对照（写回 research）；
  v2 Recall ≥ legacy，Noise ≤ ocr 的 1.5× 视为收官门通过。
- `make verify` 绿；`--help` 与代码同权（新旋钮全覆盖，旧旋钮零残留）。
- 全量测试绿；dogfood 一轮 approve。

## 风险

- 第二家族快档若只有 30 tok/s 级：疑点体量小仍可落窗，但 verify 交叉模型池要保证与 finder 异族（D7 A4 异族选择语义）。
- oracle 全集依赖历史 dogfood 数据完整度；先小集跑通再扩。
