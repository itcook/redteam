# M21a 疑点扫描地基 · plan 派生

> 本任务的交付物是 **plan 文档** `.next/plans/2026-09-07-redteam-m21a-scan-foundation-plan.md`（HOW：批次 / 小步任务 / 验证 / 提交），
> 不是 M21a 的实现。上位：`.next/roadmap/M21a-scan-foundation.md`（范围与验收）；权威框架 `.next/design/…-design.md` v8（锁定）；
> 决策 D3 / D6 / D7 / D8 / D9 / D10 / D11。

## Goal

把 M21a 的 9 项交付（配置节 · schema v4 · 引用图分组 · 跨组索引 · 疑点 schema · 提示词拆分 + 段序 + cache 断点 · 切片装包 ·
多 scanner 并行 · 分支硬切 + `redteam-next` · `--pack-only` · 观测）拆成可独立验收、可按序合入的批次与任务，每个任务给出改动文件、
测试锚、提交信息；出口验收（含 E2 slice vs full A/B）写成可执行的命令与数字门槛。

## Background（仓库事实，2026-09-07）

- 落地序：design §15 第 1 条即 M21a 全部内容；roadmap M21a 表列 9 项 + 落点；D10 硬切（无 `engine` 旗，产物名 `redteam-next`，
  插件与 ship 循环继续用 dev 的 `redteam`）；D11 追加：cell 记录 `role` + 角色分叉输出枚举、台账表 + 队列快照表（台账本身 M21c）、
  scanner 钉推理档、quote 逐字丢弃、无 `[review.risk]`。
- 代码规模（行 / 单元测试数）：`src/config/mod.rs` 578 / 8、`secrets.rs` 113 / 1、`src/cli/mod.rs` 2177 / 46（含 `resolve_models`）、
  `src/main.rs` 1494 / 15（`build_registry` / `model setup`）、`src/orchestrate/mod.rs` 906 / 18、`src/bundle/mod.rs` 570 / 21
  （`group` / `extract_imports` 只认相对 import）、`src/pack/mod.rs` 486 / 14（梯子：全文 → def 体 → 骨架）、`src/prompt/mod.rs` 804 / 29
  + 两份模板、`src/domain/finding.rs` 214（`RawFinding` / `NeedEntry` / `LensOutput`）、`src/provider/parse.rs` 275 / 16、
  `src/state/mod.rs` 665 / 23（`CellRecord` / `JobState` / `CellInput` manifest 含渲染后 prompt）、`src/db.rs` 686（7 张表）、
  `src/provider/registry.rs` 73（按 `route_key` 路由，任意粒度）；`XDG_CONFIG_HOME` 在 src / tests 出现 8 处（测试串行化 hack）。
- 二进制与插件：Cargo `[[bin]] name = "redteam"`；`make build` 编 release 并复制到 `plugins/redteam/bin/darwin/aarch64/redteam`
  （bin 入 git）；`tests/packaging.rs` 断言 commands 走 `bin/redteam` 启动器、插件名 `redteam`；`make verify` = gen 零 diff + packaging 测试。
- 现有 PR181 数据可作 dev 基线：GLM 6 / Kimi 3（作业 4065c30bae9e，~30 min）、DeepSeek 3（03a7bffb58fe，5.3 min）、P0 oracle 22 条
  与 E3 疑点臂三跑（DeepSeek flash，人工主题命中并集 5）——均在 `.next/private/`。
- 用户配置现状：`~/.config/redteam/config.toml` 旧形状（providers agc / deepseek；models GLM 5.3、Kimi K3 均 anthropic kind、
  DeepSeek v4 flash openai kind）；无 gemini。
- plan 文档先例：`.doc/plans/2026-07-27-redteam-m18-release-hardening-plan.md`——单份分批分节、批间依赖、Global Constraints、
  每 task 写 Modify / 测试 / Commit、批收口 opus 终审。

## 拟定批次（技术分解，待最终摘要确认）

| 批 | 内容（roadmap 交付项） | 依赖 | 主要落点 |
|---|---|---|---|
| A 配置节 | 1：`~/.redteam/` + `REDTEAM_HOME`、providers 键名表 + `max_concurrency`、`[[scanners]]/[[reviewers]]/[[verifiers]]`、`[review.*]`、secrets 新形状 + 守卫、解析链、`model setup` 新形状、`--test-command`；删项目层配置 / 剥离清单 / `priority` / `--models` / `--models-all` / `XDG_CONFIG_HOME` | — | config / secrets / cli resolve_models / main build_registry & setup / tests |
| B schema v4 | 1b + D11 R14：`CellRecord` 加 `role` + 输出枚举（scan / review / verify）、`suspicions` 落盘、queue_snapshot 表（空壳）、疑点台账表（空壳，M21c 填）、coverage 五集合 + runs meta 进作业元数据；旧库 dismissed / upheld 导出 `eval/fixtures/ledger-*.jsonl`；新库空建、不写迁移 | — | state / db / domain / eval |
| C 分组与索引 | 2 + 3 + 8：`extract_imports` 补 TS 裸模块 / Rust `use crate` / Go 包路径 → 引用图分组（连通分量、弱缝先切、宁降不拆、bin-packing、ocr 双阈值门）→ 正反向跨组索引 + `{{OTHER_CHANGED_FILES}}` 升级 + `[delta]` + omit 留指针；`--pack-only` 预算仪表（只装包、按 cell 报字节与 ÷3.5 估算） | — | bundle / prompt / cli / pack |
| D 契约与提示词 | 4 + 5：疑点 schema（`suspicion` / `needs.kind` / `scanned_areas`）+ parse 字段级对齐 + quote 逐字丢弃；`shared-head.md` + `scanner.md` 拆分、段序翻转（共享头 → 参考 → diff 次尾 → 角色尾）、注入段全过 `neutralize_structure`、模板哈希入 RunMeta、anthropic kind 三段界 `cache_control`、scanner 离线回归集（PR181 疑点 oracle + 噪音比） | B（落盘类型） | domain / parse / prompt / templates / backends |
| E 切片装包 | 5b：`[review.pack] body = slice` 默认，六层配方三层同批（禁真空 / 文件内一跳 / 文件地图），`slice_min_bytes` 8K，`full` 留旋钮 | C（分组产物） | pack / diffsym |
| F 硬切与并行 | 6 + 7 + 9：删旧 fan-out / 候补链 / `--models` 四分支；scanner 路径（single_shot 形态）按角色发 route_key、`thinking_level` 首个消费者、多活跃 scanner 并行 + 疑点并集 + `agreement` = 家族数、`samples`；疑点落盘 schema v4；产物名 `redteam-next`，`make build` 不覆盖插件 dev `redteam`；观测（组数 / 边数 / 跨组边 / 每组疑点 / `scanned_areas` 覆盖率 / cache 命中 / 每 cell 墙钟） | A B C D E | orchestrate / provider / state / Cargo / Makefile |
| 出口 | 验收：PR181 v2 scan（分组 ≤ 5、DeepSeek 并行墙钟 ≤ 3 min、疑点 oracle 命中 ≥ E3 基线、双 scanner 并集 / agreement）；E2 slice vs full A/B（各 N=3）；cache 命中 ≥ dev 基线；`--pack-only` 误差 < 20%；模板回归集；dev 基线归档 | F | research / roadmap |

## Requirements（plan 文档必须满足）

- R1 每个任务：Modify 文件清单、测试锚（TDD 红测或回归锚）、`cargo test` + `cargo clippy --all-targets` + `make verify` 门、提交信息（简体中文、`type(scope):`）。
- R2 批间依赖图与可并行性明示；每批一个 Trellis 任务；批收口有终审（对抗评审）步骤。
- R3 铁律映射：新编排 pass 三标配（注入互斥、证据段预算帽、读守卫）、契约 / parse 字段级对齐、前缀稳定回归（cache 命中率）、观测仪表随行。
- R4 出口验收命令化：每条验收给出命令 / 数据落点 / 数字门槛，E2 A/B 复用 `eval/e3_contract_ab.py` 形态（同 bundle 同 focus）。
- R5 与 roadmap M21a「不做」一致：不做深挖 / 队列 / 风险评分 / verifier / 台账填充 / google 后端 / LLM 语义分组。

## Acceptance Criteria

- [x] AC1 plan 文档存在于 `.next/plans/`，状态头合规（`> 状态: 锁定 · …`），列其 design / roadmap 互链；roadmap M21a 文件与 README 链回 plan。
- [x] AC2 六批 + 出口，每批任务 ≤ 6 个、每任务可独立验证；依赖图无环、与 design §15 落地序一致。
- [x] AC3 出口验收 10 条全部命令化（PR181 v2 scan、E2、cache、`--pack-only`、模板回归、dev 基线、schema v4、`make verify` / `--help`）。
- [x] AC4 与 D10 / D11 零冲突（无 `engine` / `--risk`；`role` + 枚举；quote 丢弃；钉推理档）。

## Out of Scope

M21a 的实现本身（后续每批一个 Trellis 任务）；M21b-e plan（各自派生）；`.doc/roadmap/summary.md` 明细更新（M21a 收官时）。

## Key Decisions

| # | 决策 | 依据 |
|---|---|---|
| K1 | 每批一个子分支 `feat/m21a-<批>`（自 `feat/m21-review-engine` 切出），批内小步提交，收口用 dev `redteam` 做 ship 评审后 `--no-ff` 合回；每批一个 Trellis 任务（**用户 2026-09-08 定**） | M18 先例；A / B / C 可并行；dev 二进制在 M21a-b 期间正是对照评审员（D10） |
| K2 | dev 基线不重跑：复用 P0 已归档的 PR181 作业（GLM 6 / Kimi 3 ~30 min、DeepSeek 3 5.3 min）与 22 条 oracle | dev 二进制自 09-04 起无行为改动；重跑只花钱不添信息 |
| K3 | 双 scanner 验收用 DeepSeek v4 flash + GLM 5.3（anthropic kind） | 用户现有模型里唯一异家族组合；GLM 走 anthropic 路径，顺带验证三段界 cache 断点 |
| K4 | `redteam-next`：分支上 Cargo `[[bin]] name = "redteam-next"`，`make build` 复制到 `plugins/redteam/bin/darwin/aarch64/redteam-next`，不动插件的 dev `redteam`；`tests/packaging.rs` 对插件启动器的断言不变 | D10「切名前分支产物名 redteam-next、插件继续用 dev」 |
| K5 | 配置路径并存：dev `redteam` 读 `~/.config/redteam/`，`redteam-next` 读 `~/.redteam/`（`REDTEAM_HOME`）；secrets 用户手动迁一次，不写迁移（D7 A9） | 两个二进制在同一台机器并跑不互踩 |
| K6 | E2 A/B 用 `redteam-next` 产品路径跑（`pack.body` 两档各 N=3），评分复用 P0 的 oracle 与 `e3_contract_ab.py score` 的定位 / 命中逻辑（抽成 `eval/scan_score.py`） | 同 bundle 同 focus 已由产品保证；不再另写 prompt 落盘 |
| K7 | `--pack-only` 放批 C（分组）而非批 F | 它是分组 / 切片的检视仪表，越早可用越好（D7 B7「顺手」） |

## Risks / Deferred

- 批 A 是最大删改面（层序、剥离、候补链、`--models` 四分支、46 个 cli 测试里的一部分）：先机械拆零语义再接线，`XDG_CONFIG_HOME` hack 换 `REDTEAM_HOME`。
- 段序翻转触碰整份模板：E11 注入加固回归随行；cache 命中率对照 dev 基线（dogfood L1 50% / Kimi 99%）。
- 切片配方若没修好 F3，只会在出口 E2 暴露；回退路径已定（先二跳再 `full`）。
- M21a 期间 `redteam-next` 不产 finding，dogfood 靠 dev 二进制。
