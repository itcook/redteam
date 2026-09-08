# P0 验证门：E1 PR181 风险规则回放 + E3 疑点契约召回

> 上位定义：`.next/roadmap/P0-validation-gate.md`；架构：`.next/design/2026-09-04-redteam-review-engine-architecture-design.md`（v7）§3.3 / §4 / §6 / §16；决策 D7 B4 / B7。
> 技术设计见同目录 `design.md`，执行清单见 `implement.md`。

## Goal

在动引擎代码前，用零 LLM（E1）与快模型级成本（E3）回答三个决定架构成立与否的问题：
1. 确定性风险规则（design §3.3 四类信号）能否命中慢模型独有的 findings（E1）；
2. 疑点契约是否真的比 finding 契约召回高（E3）；
3. 预算函数 `f(R, 规模)` 该长什么样（E1 顺带）。

任务出口 = E1 / E3 数据与裁决写入 research 新节。其后的 grill 必答项裁决（decisions D7+）、design 锁定、
plan 派生、M21 登记是会话动作，不在本任务代码范围。

## Background（仓库 / 数据实测，2026-09-07）

### PR181 基座
- 仓库 `<PR181 工作树，路径 / 分支 / HEAD 只记在 .next/private/>`（Go + TS），
  评审基线 `a7c827b70`；`git diff --stat a7c827b70 215afd77b` = 40 文件 +1750/−69，与全部评审记录一致。
  merge 进 dev 的 PR 头 `4317c8afce` 与 215afd77b 差 7 文件，**回放以 215afd77b 为准**（三方评审实际看到的树）。
- 该仓是浅克隆（分支深度 88），主 checkout（公司仓）有完整 dev 历史→ 历史信号从主 checkout 取。
- 三方 findings 落点（同仓 `.redteam/proj.db`）：
  - GLM 6 / Kimi 3：作业 `4065c30bae9e`，cells 表按 `@GLM` / `@Kimi` 分开（results 合并 8 条，1 条 GLM∩Kimi）；
    args：base `dev`（当时 = a7c827b70）、focus 文本 7 点、无 spec。
  - DeepSeek 3：作业 `03a7bffb58fe`（`deepseek-v4-flash`，同 focus，base a7c827b70，head 215afd77b，5.3 min，
    主 cell input 180.7K / output 32.6K；output 逼近 32768 上限，E3 须核 finish_reason）。
  - rpiv 8：`<公司仓 .rpiv/artifacts/reviews/…pr-181-*.md，只记在 .next/private/>`（I1-I4、Q1-Q4，`**Where**` 行给 `path:line` 锚）。
  - ocr 3：**磁盘无原始产物**；从 `.doc/reviews/2026-09-04-pr181-three-way-*.md` 可恢复 2 条（错误码分类 fork；失败日志不区分瞬时/终态）。
- finding JSON 字段：`id, severity, title, body, file, line_start, line_end, confidence, recommendation, models, agreement, quote, origin, verify{verdict}`。
- 该仓 proj.db `dismissed` / `resolved` 均为 0 → 台账类历史信号在 PR181 上为空，只剩 git log。
- cells 记录不含 prompt、`.redteam/rounds/` 为空、无模板覆盖旋钮 → 现产品无 pack / prompt 落盘途径。

### 第二个小 oracle（D7 B7，本仓 `~/hobby/redteam/.redteam/proj.db`）
- 26 作业、132 findings，`verify.verdict` upheld 48 / refuted 65 / 无 19；`dismissed` 63 条（假阳集）。
- 可回放（有 base + head）的 upheld 分布：`6f95c25~1..6f95c25` 12 作业 16 条（同一 diff 的 dogfood 多轮）、
  `4f52ac9..8dcaf6f` 2、`f028f92..290236e` 4、`e8dc24f..{00e23fc,3e60804,99138bb}` 12、`33c4522..?`（无 head_sha）4；
  working-tree 作业无 base 不可回放。有效 ≈ 5 个 diff、按 finding id 去重约 20-30 条。

### 现有可复用零件
- `eval/harness.py`（零依赖）：def 正则仅 Rust、`materialize` 只认本仓；`src/diffsym/mod.rs` 有 rs/go/ts 三语言 def 正则与
  brace 配平 `block_end`，可移植到 Python。
- prompt 装配链全部 pub：`evidence::git::changed_files` → `bundle::{select_files, group, other_files_manifest}` →
  `pack::build_pack` → `prompt::{render_dossier_section, build_bundle_prompt}`；三处私有辅助（`merge_bundles` 扁平合一、
  `pack_budget_bytes` = min(768_000, (最小窗−16000)×3−24000)、`build_packs` own_diff_bytes=Σdiff）各 ≤ 10 行可照抄。
- 单发请求形状（`provider/single_shot.rs` + `backends.rs`）：system = `NO_TOOLS_SYSTEM`、user = prompt、首轮无
  response_format、stream=true + include_usage、max_tokens = output_limit 或 `REDTEAM_MAX_TOKENS` 或 32768；parse 失败一次
  `REPAIR_PROMPT` + json_object 收尾。输出语言由 `LC_ALL/LC_MESSAGES/LANG` 决定。
- 模板 `src/prompt/templates/adversarial-code-bundle.md`：`<finding_bar>` 与 `<output_contract>` 是承载契约的两段，其余段与契约无关。
- 配置：`~/.config/redteam/config.toml` 已有 `DeepSeek = deepseek-v4-flash`（openai kind，1M 窗，无 output_limit），
  `DEEPSEEK_API_KEY` 在环境中。

## Requirements

### R-E1 `eval/risk_replay.py`（零 LLM，零第三方依赖，不进 `src/`）
- R-E1.1 输入 `<repo> <base> <head>`，只读（`git diff` / `git show` / `git grep` 取 head 树，不 checkout）；
  输出 H0 = `[{file, ranges, symbols, signals[], score}]` 与 PR 级 `{R, 规模}` JSON。
- R-E1.2 四类信号初版：路径/类别（路径 glob + hunk 关键词）、结构（签名面变更、删除、公开面拓宽、高 fan-in、churn；测试与文档降权）、
  外部 API（关键模块 hunk 内外部包符号密度 ≥ k）、历史（`--history-repo/--history-ref` 上 fix 类 commit 频次；
  `--ledger <proj.db> --before <ts>` 取时间早于被回放作业的 upheld / dismissed 锚点，防泄漏）。
- R-E1.3 热区形成：hunk → enclosing def 块（rs/go/ts 正则 + brace 配平；无块回退 hunk ± 10 行）→ 同文件重叠 / 相邻 ≤ 5 行合并 → 分数 ≥ θ 入 H0。
- R-E1.4 oracle A（PR181）：**主裁决集 = GLM ∖ DeepSeek 主题级差集**（DeepSeek 完全未触及的主题；对齐表人工判、入 research）；
  同时报严格差集（file+主题）与全并集（GLM ∪ Kimi ∪ DeepSeek ∪ rpiv ∪ ocr）命中率作诊断。
  oracle B（本仓）：按 diff 逐个回放 upheld findings（id 去重）。
- R-E1.5 **命中口径（用户已定）：区间级裁决**（finding `[line_start,line_end]` 与同文件 H0 区间有交集），**文件级同时报出作诊断**。
  空热区 = 与任何来源 finding 都无交集的 H0 区间，报数与比例；两口径都报。
- R-E1.6 裁决：主裁决集区间级命中 ≥ 2/3 → 规则初版成立；否则修规则再回放，最多三轮，每轮规则改动与数字入 research。
  空热区比例 > 1/2 → 收紧 θ。
- R-E1.7 顺带：对回放语料（PR181 + 本仓 5 个 oracle diff + 本仓近 30 个非 merge commit 无 oracle 只取分布）输出
  `(规模, |H0|, R)` 表，按分位反推 `max_cells` 低 / 中 / 高三档起步值（R5）。

### R-E3 疑点契约 vs finding 契约（DeepSeek v4 flash，`eval/e3_contract_ab.py` + `examples/dump_pack.rs`）
- R-E3.1 同 bundle 同 focus：`examples/dump_pack.rs`（dev-time 仪器，M21a `--pack-only` 落地即拆）在 backend 工作树 cwd 下用 pub API
  按产品同一路径组包并落盘（system + user + meta）；不碰 `src/`。
- R-E3.2 两臂：A = 产品 finding 契约原样；B = 疑点契约，**只替换 `<finding_bar>` + `<output_contract>` 两段与 system 中的 schema 短语**，
  其余字节相同（脚本 diff 校验）。疑点 schema 按 design §4：`suspicion{file,line_start,line_end,hint,category,confidence,quote}` +
  `needs` cap 5 + `scanned_areas`；category v1 = 模板 L0-L5 六类。
- R-E3.3 请求形状复刻产品单发（system/user、无首轮 response_format、stream、max_tokens 两臂同值、一次 repair）；各 N=3；
  预飞：A 单跑核 `finish_reason`，若截断则两臂同步上调 max_tokens 并记录。
- R-E3.4 指标：oracle 命中数（区间级 + 人工主题确认表）、疑点 / finding 总数与噪音比、output token、墙钟；报单跑均值与三跑并集。
  oracle = GLM 6 ∪ rpiv 8 ∪ Kimi 3 ∪ ocr 2 去重（约 10-14 条）。
- R-E3.5 裁决：疑点臂命中 ≥ finding 臂 → design §4 契约成立；反之回 D6 重议采纳项 1。
- R-E3.6 顺带：`c_hi`（R11）= 命中 oracle 的疑点 confidence 下沿（报最小值与 P10）；schema 观察（hint 归一是否必要、category 分布）。
- R-E3.7 疑点模板与全部 PR181 衍生物（dump 的 prompt、模型原始输出、oracle JSON、对齐表）只放主 checkout
  `.next/private/p0-raw/`（gitignored；本仓侧 oracle B / 分布语料入库在 `.next/research/2026-09-07-p0-raw/`）；**公司代码与 finding 正文不进本仓任何跟踪文件**。

### R-W 写回
- R-W.1 research（`.next/private/research/2026-09-07-redteam-p0-validation-gate-research.md`，原 `.doc/research/2026-09-04-…` §15/§16）新节 §15「E1 回放数据」、§16「E3 契约对照」：对齐表、原始数字、各轮规则改动、裁决。
- R-W.2 `.next/roadmap/followup.md` 实现债表加一行：`examples/dump_pack.rs` 在 M21a `--pack-only` 落地时删除。
- R-W.3 `.next/roadmap/P0-validation-gate.md` 状态头补一句「E1 / E3 数据已入 research §15 / §16」（✅ 留给锁定动作）。

## Acceptance Criteria

- [x] AC1（R-E1.1-1.3）`python3 eval/risk_replay.py replay <repo> <base> <head>` 对 PR181 与本仓任一 branch 作业产出 H0 + R JSON；
  `selftest` 子命令对合成样本通过（三语言 def、块配平、合并、交集）。
- [x] AC2（R-E1.4-1.7）research §15 含：主题对齐表、主裁决集区间级 / 文件级命中率、严格差集与全并集命中率、空热区数与比例、
  轮次与每轮规则改动、`(规模,|H0|,R)` 表与三档 `max_cells` 起步值、裁决结论。
- [x] AC3（R-E3.1-3.3）私有目录下有 A / B 两臂 prompt 与 diff 校验记录、预飞 finish_reason 记录、各 3 跑原始输出与 usage。
- [x] AC4（R-E3.4-3.6）research §16 含：命中对照表（含人工主题确认）、噪音比、token、墙钟、`c_hi`、schema 观察、裁决。
- [x] AC5（R-E3.7）`git diff` 与新增文件中不含公司代码 / finding 正文；PR181 路径只出现在任务文档与命令示例。
- [x] AC6 `cargo test` 零回归（含 `examples/dump_pack.rs` 编译）、`make verify` 绿、`src/` 零改动。
- [x] AC7（R-W）followup.md 债行、P0 状态头补句已写。

## Key Decisions

| # | 决策 | 依据 |
|---|---|---|
| K1 | 命中口径：区间级裁决 + 文件级诊断（**用户 2026-09-07 定**） | 与 design §3.3 H0 形状、§6 队列项一致；文件级 40 文件下门槛虚高 |
| K2 | 回放头 = 215afd77b（非 merge 头 4317c8afce） | 三方评审实际看到的树；worktree 已在该提交，零 checkout |
| K3 | E1 主裁决集 = GLM ∖ DeepSeek **主题级**差集 | roadmap 原意「慢模型独有」= 快模型完全未触及的主题；严格差集与全并集作诊断 |
| K4 | E3 组包用 `examples/dump_pack.rs`（pub API + 照抄三处私有辅助）而非产品加旋钮 | P0「先于任何实现」；`cargo test` 顺带编译防腐；M21a 拆 |
| K5 | E3 B 臂只换 `<finding_bar>` + `<output_contract>` + system schema 短语 | `finding_bar` 的「只报 material / 宁一强勿多弱」与「疑似即列」直接冲突，不换则测不到契约差异；其余段保持字节相同 |
| K6 | E3 两臂都走脚本单发（无二阶段、无 refute），N=3 | 条件对等；产品作业 03a7 仅作参照 |
| K7 | PR181 衍生物全部落 `.next/private/p0-raw/`（私有） | 本仓远端为个人 GitHub（`itcook/redteam`），公司代码不得入库 |
| K8 | Trellis 走 inline（不派子代理） | 数据驱动迭代（三轮修规则、预飞调 max_tokens），主会话直接做效率最高；跳过 jsonl 门 |

## Out of Scope

- E2（slice vs full）→ M21a 出口 / M21d；aacr-bench 任何操作（D7 B4）。
- 风险评分器进 `src/`（M21b）；疑点契约进产品模板（M21a）；`--pack-only`（M21a）。
- grill、design 锁定、plan 派生、`.doc/roadmap/summary.md` 登记；ocr 第 3 条 finding 的追索（找到即补进 oracle，不阻塞）。

## Risks / Deferred

- DeepSeek 单跑 output 逼近 32768：若预飞 `finish_reason=length`，两臂同步上调 max_tokens；产品作业 03a7 的数字不再作同条件参照。
- oracle 很小（主裁决集约 4-6 条）：≥ 2/3 是粗裁决；research 同时给全并集与 oracle B 数字，grill 时综合判。
- 浅克隆 + 历史信号只看主 checkout dev 历史；PR181 无台账项 → 历史信号在 oracle A 上近乎失活，其效果主要由 oracle B 体现。
- hobby 作业 `33c4522` 无 head_sha：按 created_at 推断 head，推不出则弃（oracle B 少 4 条）。
- E3 成本：6 跑 × ~190K input ≈ 1.2M input token + ≤ 6 × 32K output，DeepSeek flash 价位下可忽略；墙钟 ≈ 30 min（两跑并行减半）。
