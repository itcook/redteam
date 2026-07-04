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
