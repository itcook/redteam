# design · P0 验证门（E1 / E3）

> 需求与决策见 `prd.md`（K1-K8）；本文只写技术定论。

## 1. 边界

三件交付物全部在 `src/` 之外，产品行为零改动：

| 交付物 | 角色 | 依赖 |
|---|---|---|
| `eval/p0_oracle.py` | 从 proj.db / rpiv markdown / 人工对齐文件组装 oracle JSON | sqlite3 标准库 |
| `eval/risk_replay.py` | E1 引擎：`replay` / `score` / `report` / `selftest` | 标准库 + `git` |
| `eval/e3_contract_ab.py` | E3：`variant` / `run` / `score` | 标准库 `urllib` + `DEEPSEEK_API_KEY` |
| `examples/dump_pack.rs` | 用产品 pub API 落盘单发 prompt（dev-time 仪器，M21a 拆） | lib crate |

私有数据根 `P0_RAW=<worktree>/.next/private/p0-raw/`（gitignored）：
`oracle-pr181.json`、`oracle-hobby.json`、`curated-pr181.json`（人工：ocr 条目 + 主题分组）、`replay/*.json`、
`pack/pr181/{prompt.system.txt,prompt.user.md,meta.json}`、`e3/{A,B}/run-{1,2,3}.json`、`e3/variant-diff.txt`。
脚本一律以参数 / 环境变量接收路径，跟踪文件不含公司路径以外的任何公司内容。

## 2. 数据契约

```
OracleEntry  {id, source: glm|kimi|deepseek|rpiv|ocr|hobby, anchors: [{file, line_start, line_end}]（rpiv 多锚），
              title?, severity?, confidence?, topic?: str, job?: str, base?: str, head?: str, anchored: bool|null, created_at?}
              （实现：hobby id = <8hex>@<head7>，同一 finding 跨 head 不重复计数；anchored=false 的条目评分时剔除）
Zone         {file, ranges: [[s,e]], symbols: [str], signals: [{kind: path|struct|extapi|hist, detail, w}], score}
ReplayOut    {repo, base, head, size: {files, adds, dels, hunks, code_lines}, zones: [Zone],
              h0: [Zone], theta, R, params}
ScoreOut     {set: topic_diff|strict_diff|union|hobby, n, hit_range, hit_file, misses: [id],
              empty_zones_range, empty_zones_file, h0_count, params}
```

- oracle `file/line_*` 为 head 树坐标（产品 findings 已锚定；rpiv `**Where**` 行的 `path:line` / `path:a-b` 直接取；ocr 条目人工填）。
- `topic` 由 `curated-pr181.json` 指派：同主题跨来源同 id；主裁决集 = `{glm 条目 | 其 topic 无任何 deepseek 条目}`。
- 命中（K1）：`range` = 同文件且 `[ls,le] ∩ zone.range ≠ ∅`；`file` = 同文件。空热区 = 对全并集无交集的 H0 区间。

## 3. E1 引擎

### 3.1 输入与树读取
`git -C repo diff --end-of-options base...head`（与产品 `changed_files` 同 range 语义：merge-base…head）；文件内容 `git show head:path`；
fan-in 用 `git grep -w -c <sym> head -- <globs>`。全程不 checkout、不写目标仓。

### 3.2 信号（design §3.3 四类，v1）
| kind | 规则 | 权重 w |
|---|---|---|
| path | 路径 glob 命中敏感目录 / 文件名（auth·security·crypto·payment·billing·config·setting·secret·token·permission·migration·callback·webhook） | 1.0 |
| path | 新增行关键词类：外部输入解析 / 反序列化、raw SQL / exec / eval / unsafe、条件写与幂等键（ETag·If-None-Match·fingerprint·idempot）、超时 / 重试 / 截止、并发原语、删除 / 覆写 | 每类 0.5，封顶 1.5 |
| struct | 改动 def 的签名行本身在 hunk 内（签名面变更） | 1.0 |
| struct | def 被删除；公开面拓宽（新增 `pub` / `export` / Go 大写导出名） | 1.0 / 0.5 |
| struct | 改动符号 fan-in ≥ k（默认 5，`git grep -w` 跨仓计数，排除自身文件） | 1.0 |
| struct | churn：文件 adds+dels 位于本 PR 上四分位 | 0.5 |
| extapi | 关键模块（已有 path 信号）hunk 内外部包符号调用 ≥ 3（Go import 块 / TS import / Rust `use` 解析，排除相对与标准库） | 1.0 |
| hist | `--history-repo/--history-ref` 上该文件近 12 个月 fix·bug·hotfix·revert·regress 类 commit ≥ 3 | 0.5，≥ 6 → 1.0 |
| hist | `--ledger --before <ts>`：文件有时间早于被回放作业的 upheld 记录 / dismissed 卡（仅时间之前，防泄漏） | 1.0 / 0.5 |
| 降权 | 测试（`_test.go`、`.test.ts`、`/test/`、`tests/`）× 0.3；文档 / schema（`.md`、`.json`、`.yaml`、`.toml`）× 0.2 | — |

### 3.3 热区与分数
hunk → enclosing def（移植 `src/diffsym` 三语言正则 + brace 配平 `block_end`；Go/TS/Rust 之外或无块 → hunk ± 10 行）→
同 hunk 跨多 def 按 def 拆区；def 区只按重叠合并、padded 区只与 padded 相邻合并（v3 度量修正，避免相邻函数桥接成整文件区）；
涟漪卫星区 = 改动符号在本文件引用点（≤ 4 处）的 enclosing def（v2）；H0 = `score ≥ θ`（定案 1.5）**且 ≥ 2 类信号**（v4）；
`report` 打印 θ 曲线供调参。R = Σ_{zone∈H0} score / (1 + ln(1 + code_lines))，另报 `max zone score`。
最终规则表以 research §15.4 为准（本节为初版形状；实施中经 r1-r4 演进，轨迹见 §15.3）。

### 3.4 裁决与预算形状
`score` 对四个集合各出一行；裁决看 `topic_diff.hit_range ≥ 2/3` 与 `empty_zones_range ≤ 1/2`。
预算形状：回放语料 = PR181 + oracle B 五个 diff + 本仓近 30 个非 merge commit（无 oracle，仅分布），输出 `(code_lines, |H0|, R)`；
按 R 三分位分档，`max_cells` 起步值 = 各档 |H0| 的中位数向上取整（必挖集 = H0 本身，所以起步值不得低于该档 |H0| 中位数）。

## 4. E3

### 4.1 组包（`examples/dump_pack.rs`）
在目标仓 cwd 下运行 `cargo run --example dump_pack -- --base <base> --models DeepSeek --focus "<text>" --out <dir>`：
`Config::load()` → `changed_files(cwd, Some(base), true)` → `select_files` → `group(…, Some(root))` → 扁平合一（照抄 `merge_bundles`）→
budget = min(768_000, (min_ctx − 16_000) × 3 − 24_000)（照抄 `pack_budget_bytes`）→ `render_dossier_section(build_pack(b, root, budget, Σdiff))`
（照抄 `build_packs`）→ `build_bundle_prompt("branch <base>...HEAD", b, focus, other_files_manifest(bundles,0), "", dossier, "")`。
落盘 system（`NO_TOOLS_SYSTEM` 是 `pub(crate)`，example 内照抄该常量文本，meta 记录来源 `src/provider/single_shot.rs`，
E3 脚本再与源文件逐字比对一次）、user、meta（文件数、字节、sha256、output_language、budget）。校验：40 文件；bytes/3.5 ≈ 作业 03a7 的 180.7K input。
无 `--spec` 支持（PR181 作业 spec 为空）。

### 4.2 变体（B 臂）
`e3_contract_ab.py variant`：以正则整段替换 `<finding_bar>…</finding_bar>`、`<output_contract>…</output_contract>`，system 中
`an object with a "findings" array` → `an object with a "suspicions" array`；产出 `variant-diff.txt` 并断言差异只落在这三处。
疑点契约文本（design §4）：疑似即列、不要求论证；`suspicions[]{file, line_start, line_end, hint(≤50 token 一句话), category(L0-L5),
confidence 0..1, quote 逐字}`；`needs` 同产品定义 cap 5；`scanned_areas[]{file, ranges}`；无可疑返回空数组。

### 4.3 运行
`run --arm A|B --n 3 --max-tokens <m>`：POST `https://api.deepseek.com/chat/completions`，`model=deepseek-v4-flash`，
messages [system, user]，`stream=true` + `stream_options.include_usage`，`max_tokens=m`，首轮无 response_format；parse 失败 → 追加产品
`REPAIR_PROMPT` 原文 + `response_format=json_object` 一次。落盘：原始流拼接文本、usage、finish_reason、墙钟、parse 结果。
预飞：A 臂 `--n 1 --max-tokens 32768`，`finish_reason=length` → 两臂改用 65536（DeepSeek 拒绝则取其上限）并记录。
两臂各 3 跑，最多 2 并发。

### 4.4 评分
`score`：quote 在 head 树定位（找不到保留模型行号并标 `unanchored`）→ 区间级命中 oracle（全并集）→ 输出每跑 `{n, hits, noise, out_tok, wall}`
与三跑并集命中；生成人工主题确认表骨架（每条疑点 / finding 对应 oracle topic 或空），人工填后再算最终命中。
`c_hi` = 命中疑点 confidence 的最小值与 P10；schema 观察 = category 分布、hint 长度分布、同主题疑点 hint 是否需要归一才能合并。

## 5. 兼容 / 运维 / 回滚
- 产品零改动；`examples/dump_pack.rs` 随 `cargo test` 编译；M21a `--pack-only` 落地即删（followup 债行）。
- 目标仓只读：E1 用 `git show/grep`，E3 组包只读工作树；不改 backend 工作树状态。
- 回滚 = 删除新增文件；`.next/roadmap/followup.md` 与 `P0-validation-gate.md` 各一行文档改动。
- 隐私：任何含公司代码 / finding 正文的文件只在 `P0_RAW`；提交前 `git diff --cached` 人工过目（AC5）。
