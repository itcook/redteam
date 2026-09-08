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
- `export_ledger.py` — M21a 批 B-1（D10-2）：schema v4 一次重置前，把旧 proj.db 的 `dismissed` / `resolved` 全表与 `results` 里
  verify=upheld 的 findings 导出为 `fixtures/ledger-{dismissed,resolved,upheld}.jsonl`（verifier 离线回归集数据；本仓自己的数据，入库）。
  `--db` 缺省 `.redteam/proj.db`、`--out-dir` 缺省 `eval/fixtures`；upheld 按 job×finding 计行带 `job_id` / `head_sha`，
  `in_dismissed_ledger` 标出同 id 也被人裁误报的行（消费方须排除或单独计）；status 原样（含 withdrawn / stale）。
  注意：本仓固定集里有 3 行 upheld 指向 gitignored 的 `.doc/` 设计稿、数十行指向已改名的历史路径——quote 须按 `head_sha` 树锚定。
  jobs / cells / results 评审史不导出；redteam-next 用独立库文件 `.redteam/proj-next.db`，旧库无需删除。
- 被评仓路径、oracle、原始输出一律经参数传入并落 `.next/research/2026-09-07-p0-raw/`（本仓侧，入库）与 `.next/private/p0-raw/`（PR181 侧，gitignored）；本目录不含任何被评仓内容。
