# M21a · 疑点扫描地基

> 状态: ⏳ · 依赖 P0 · design §3.1-3.2 / §4 / §5 / §10.3 / §10.6 / §12 · 决策 D3, D6, D7
> 硬切（D10）：分支删除旧编排，无 `engine` 旗；本里程碑产物 `redteam-next` 仅 scanner 段可用（无深挖，不产 finding，仅诊断
> 输出疑点）；对照基线 = `dev` 二进制，插件与 ship 循环继续用它

## 目标

把快扫从「finding 单发」改造成「分组并行 + 疑点索引」，同时换上用户级唯一配置、schema v4 与共享前缀段序。里程碑结束时
`redteam-next` 能对 PR181 产出疑点索引与覆盖自报；dev 二进制在 PR181 的 findings 集合归档为后续 A/B 的固定基线。

## 交付项

| # | 项 | design | 落点 |
|---|---|---|---|
| 1 | **配置节**：`~/.redteam/config.toml` + `secrets.toml`，`REDTEAM_HOME` 覆盖；`[providers.X]` 键名表 + `max_concurrency`；`[[scanners]]` / `[[reviewers]]` / `[[verifiers]]` 内联条目，`name` 缺省 `provider/model_id`、`family` 缺省 model_id 首段；`[review.*]` 子表；secrets 新形状（`api_key` + `"<model_id>" = key`，值可内联表）+ 引号守卫 + 未知键警告；解析链；**删除项目层配置与剥离清单**；`--test-command` flag + 推断；`priority` / `--models` / `--models-all` / `XDG_CONFIG_HOME` 退场；`model setup` 写新形状 | §12 | `src/config/mod.rs`, `src/config/secrets.rs`, `src/cli/mod.rs` resolve_models, `src/main.rs` build_registry / setup |
| 1b | **proj.db schema v4 一次重置**（D10）：cell 带角色、suspicions、queue_snapshot、coverage 五集合、runs meta（refute 率 / 台账数 / 模板哈希 / provider 在途峰值）；不写迁移；重置前把旧库 dismissed / upheld findings 导出为 `eval/fixtures/ledger-*.jsonl`（verifier 回归集数据） | §8, R14 | `src/state/mod.rs`, `src/db.rs`, `eval/` |
| 2 | 引用图分组：changed 间引用边 / import 边 / 同目录边 → 连通分量 → 弱缝先切 → 宁降不拆 → 孤立文件 bin-packing；ocr 双阈值门（4 / 200 / 10 起步） | §3.2 | `src/bundle/mod.rs`：`group` / `extract_imports` 重写，**先补** TS 裸模块 / Rust `use crate` / Go 包路径；边集保留 |
| 3 | 正反向跨组索引 + `{{OTHER_CHANGED_FILES}}` 槽升级 + 增量轮 `[delta]` 标注；omit 清单「降质 + 留指针」 | §10.6 | `src/bundle/mod.rs`, `src/prompt/mod.rs` |
| 4 | 疑点 schema：`suspicion{file, line_start, line_end, hint, category?, confidence, quote}`（quote 必填）、`needs.kind = evidence \| api`、`scanned_areas`；parse 字段级对齐 + 逐条校验 | §4 | `src/domain/finding.rs`, `src/provider/parse.rs` |
| 5 | **提示词拆分**：`templates/shared-head.md` + `scanner.md`（疑点模式、疑似即列、判例摘要措辞、needs 声明措辞）；**段序翻转**：共享头 → 参考材料 → diff 次尾 → 角色尾；模板哈希入 RunMeta；scanner 离线回归集（PR181 疑点 oracle + 噪音比）；**cache 断点**：anthropic kind 在共享头末 / 证据段末 / 角色尾前放 `cache_control`，RunMeta 按 cell 记命中率（D10） | §4, §10.3, §10.7 | `src/prompt/templates/*.md`, `src/prompt/mod.rs` |
| 5b | **切片装包**（D8）：`[review.pack] body = slice` 默认，六层配方（核 / 文件内一跳 / 文件地图 / 折叠 / 文件外片段 / 禁真空）三层同批；`slice_min_bytes` 小文件全文；`full` 留旋钮 | §10.2 | `src/pack/mod.rs`（梯子档位升为默认形态 + 文件内一跳解析）, `src/diffsym` |
| 6 | 多活跃 scanner 并行：每条目对每组各跑一次，疑点并集，`agreement` = 家族数 | §5 | `src/orchestrate/mod.rs` |
| 7 | **分支硬切**：删除旧 fan-out / 候补链 / `--models` 四分支 / 项目层配置；v2 scanner 路径（single_shot 形态）+ 疑点落盘（schema v4）；编排层按角色发 route_key；产物二进制名 `redteam-next`（M21c 切回），`make build` 不覆盖插件里的 dev `redteam` | 铁律 6, §12.3, D10 | `src/orchestrate/mod.rs`, `src/state/mod.rs`, `src/provider/registry.rs`, `Cargo.toml` / Makefile |
| 8 | **`--pack-only` 预算仪表**：只装包、按 cell 打印字节与估算 token（÷3.5）、不调 provider | D7 B7 | `src/cli/mod.rs`, `src/pack/mod.rs` |
| 9 | 观测：组数 / 边数 / 跨组边数、每组疑点数、`scanned_areas` 覆盖率、cache 命中率、每 cell 墙钟 | 铁律 1 | RunMeta / phase_history |

## 不做

深挖 / 队列 / 风险评分 / verifier（M21b）· 台账（M21c）· google 后端（R17）· LLM 语义分组（`semantic = false`，R8 后议）。

## 验收

- 配置：旧路径 `~/.config/redteam/` 与项目 `.redteam/config.toml` 存在时不读、打印一行提示；引用不存在的 provider →
  聚合报错；secrets 里裸写带点 id → 「是否漏了引号」提示；未知 model_id 键 → warning；`--test-command` 覆盖推断。
- PR181 v2 scan：分组 ≤ 5 组、跨组边数可观测；DeepSeek 分组并行墙钟 ≤ 3 min；疑点 oracle 命中 ≥ E3 基线；两个活跃
  scanner 时疑点并集与 agreement 正确。
- 段序翻转后 dogfood cache 命中率 ≥ 翻转前。
- `--pack-only` 对 PR181 报出的字节数与真实 usage 同量级（差 < 20%，以字节为准）。
- **E2 slice vs full A/B**（design §16）：PR181 + dogfood，v2 scanner 两种 `pack.body` 各 N=3，比疑点 oracle 命中、噪音比、
  输入 token、cache 命中率；slice 命中 ≥ full 则维持默认，否则按 §10.2 回退信号先二跳再 full。
- 模板哈希在 RunMeta 可读；改 scanner 模板后回归集（疑点 oracle 命中 + 噪音比）不退化。
- 对照基线：dev 二进制在 PR181 的 findings 集合与墙钟归档进 research，作后续 A/B 的固定基线；`redteam-next` scanner 疑点对该
  集合的命中可读。
- schema v4：旧库导出的 dismissed / upheld 固定集可读、条数与旧库一致；新库从空建，不写迁移。
- cache：anthropic kind 命中率按 cell 可读、≥ dev 基线。
- `make verify` 绿；`--help` 含 `engine` / `--risk`（占位）/ `--test-command` / `--pack-only` 与配置路径说明。

## 风险与前置件

- `extract_imports` 三语言缺口决定分组质量，先补再分组（交付项 2 内序）。
- 段序翻转触碰 `adversarial-code-bundle.md` 全模板：E11 注入加固回归随行；`neutralize_structure` 覆盖新注入段。
- 配置节是本里程碑最大的删改面（层序、剥离、候补链、`--models` 四分支）：先机械拆零语义再接线；测试里 `XDG_CONFIG_HOME`
  串行化 hack 换成 `REDTEAM_HOME`。
