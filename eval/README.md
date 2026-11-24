# M14 2-pre:穷人版卷宗查全率评测 harness

任务书:`.doc/worktree-task.md`(主检出);报告:主检出 `.doc/research/2026-07-04-dossier-recall-eval.md`。

- `harness.py run <commit>` — 单 commit 端到端:git archive 材料化 → 穷人版提取
  (diff -U0 → def 正则 + 缩进/括号 enclosing → `rg -wF` 词边界搜引用)→
  rust-analyzer lsif ground truth → per-symbol 打分,写 `out/<commit>.json`。
- `harness.py report` — 聚合 out/*.json:per-symbol 口径分场景查全/查准 + 卷宗体积。
- `bucket.py` — 名字桶口径重算(同名全部符号 GT 引用并集;卷宗真实语义)+ 噪声形态分类。

环境:`rg`、`rustup component add rust-analyzer`、python3(零第三方依赖)。
`EVAL_TREES`(commit 树缓存目录,默认会话 scratchpad)、`EVAL_REPO`(评测目标仓,默认本仓)。
宏合成样本(本仓零 macro_rules,盲区用合成 crate 补测):`macro-synth.sh <dir>` 构造。

评测侧工具不受产品零依赖约束;本目录不进 src/、不加产品依赖。

## P0 验证门（M21，2026-09-07）

- `risk_replay.py` — E1 风险规则零 LLM 回放：`replay <repo> <base> <head>`（只读 git，四类信号 → 热区 H0 + 风险分 R）、
  `score <replay.json> <oracle.json>`（区间级 / 文件级命中、空热区）、`report`（θ 曲线 + 规模/|H0|/R 表）、`selftest`。
  规则定案（v4）见主检出 research §15；M21b 移植 Rust 后本脚本保留为离线回归器。
- `p0_oracle.py` — 从 proj.db cells / rpiv 报告 / 人工对齐文件组装 oracle JSON（`pr181`），或从本仓 proj.db 取 upheld findings（`hobby`）。
- `e3_contract_ab.py` — E3 疑点契约 vs finding 契约：`variant`（由 A 臂 prompt 落盘产物派生 B 臂并断言只差两段；落盘器 `examples/dump_pack.rs` 已随 M21a 批 C 拆除，产品等价物 = `redteam review --pack-only` → `.redteam/packs/<job>/cell-*.md`）、
  `run`（DeepSeek 直连，复刻产品单发请求形状）、`score`（quote 定位 + 区间级命中 oracle + 人工确认表骨架）。
- `scan_score.py` — M21a 批 D-5（design §10.7 离线回归集）：scanner 疑点对 oracle 的零 LLM 评分，**改模板必跑**。
  `score <input>… --oracle <json> --repo <dir> [--head <sha>] [--set union] [--out score.json] [--baseline prev.json] [--fail-on-regression]`；
  输入自动识别 redteam 结果 JSON（`cells[].output.suspicions`，role=scanner）/ E3 run JSON 或目录（`items[]`）/ 裸 `{"suspicions":[]}`；
  口径 = E3 B 臂（`_oracle.py`：去全部空白子串定位、失败按自报行号计命中并计 unanchored；anchors 区间重叠即命中）——**比产品宽两点**
  （产品 `locate_quote` 按行 / 词序匹配、定位失败直接丢），与 P0 基线可比优先；`selftest` 用合成仓自检。
  产品面待接：`review --format json` 今天的 `run.cells` 只有 CellSummary、无 output——F 批需给结果 JSON 加 scanner 输出面（或评分器改读 proj.db `cells` 行）。
  基线 = P0 E3 B 臂三跑对 oracle-pr181 union：union_oracle_hit 9 / mean_oracle_hit 5.33 / mean_noise 0.55（人工主题并集 5 见 P0 研究）。
  用法（F 落地后）：`redteam review --base <base> --format json > out.json && python3 eval/scan_score.py score out.json --oracle … --repo … --baseline …`。
- `refute_score.py` — M21b 批 D-4（design §8.1「去留由数据决定」〔D17〕）：refuter 离线基线。
  `build`（零网络：按 `head_sha` / `updated_at` 锚定历史树 → 物化单文件 → 生成用例，逐条记排除原因）→
  `run`（逐条调隐藏子命令 `redteam refute-once`：**产品同一份** prompt / provider / 判决解析，Python 一行不复刻）→
  `score`（混淆矩阵 + 驳回率 / 误驳率 + Wilson 区间 + 预登记阈值裁定；`--baseline` 对比）；`selftest` 零网络自检。
  固定集 = `fixtures/ledger-{dismissed,upheld}.jsonl`（dismissed 期望 REFUTED、upheld 期望 UPHELD）。
  首轮结论（2026-09-21，41 条可用）：异族 deepseek-flash 驳回 37.5% / 误驳 12.0%、同族 glm-5.3 18.8% / 20.0%，
  两臂误驳都超 5% 门槛 → `review.refuters.enabled` 缺省 **false**。方法与归因见
  `.next/research/2026-09-21-redteam-verifier-offline-baseline-research.md`（历史文档，保留原名）。
- `_oracle.py` — `e3_contract_ab.py score` 与 `scan_score.py` 共用的命中口径（`show_file` / `anchor_quote` / `overlap` / `load_oracle` / `match_entries`）。
- `export_ledger.py` — M21a 批 B-1（D10-2）：schema v4 一次重置前，把旧 proj.db 的 `dismissed` / `resolved` 全表与 `results` 里
  refute=upheld 的 findings 导出为 `fixtures/ledger-{dismissed,resolved,upheld}.jsonl`（refuter 离线回归集数据；本仓自己的数据，入库）。
  `--db` 缺省 `.redteam/proj.db`、`--out-dir` 缺省 `eval/fixtures`；upheld 按 job×finding 计行带 `job_id` / `head_sha`，
  `in_dismissed_ledger` 标出同 id 也被人裁误报的行（消费方须排除或单独计）；status 原样（含 withdrawn / stale）。
  注意：本仓固定集里有 3 行 upheld 指向 gitignored 的 `.doc/` 设计稿、数十行指向已改名的历史路径——quote 须按 `head_sha` 树锚定。
  jobs / cells / results 评审史不导出。**2026-09-28 起口径变了**（M21c 批 0〔D19-6〕）：切名后 redteam 用的就是
  `.redteam/proj.db`，首次打开旧 schema 库会**一次性迁移**——台账带走、评审史（jobs / cells / results）丢弃，
  动手前自动留一份整库快照 `proj-legacy-<日期>.db`。所以旧库不再是「无需删除」而是「会被就地升级」；
  想留一份原样的，迁移前自己 `cp` 或用那份快照。只读查旧库用 `sqlite3 'file:…?mode=ro'`，或走 `--dismissed`（已改只读打开）。
- 被评仓路径、oracle、原始输出一律经参数传入并落 `.next/research/2026-09-07-p0-raw/`（本仓侧，入库）与 `.next/private/p0-raw/`（PR181 侧，gitignored）；本目录不含任何被评仓内容。
