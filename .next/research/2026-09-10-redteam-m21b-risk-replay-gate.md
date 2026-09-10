# M21b 批 A · 风险评分器回放回归门（2026-09-10）

> 裁决依据：D11（R5-a/b/c/d）；规则 spec = P0 §15.4（`../private/research/2026-09-07-redteam-p0-validation-gate-research.md`）。
> 门工具：`eval/risk_replay.py compare`（Python v4 回放 vs Rust `redteam-next risk`），R 容差 1e-6。
> 数据落点：`.next/private/m21b/risk/`（gitignored；PR181 侧另有 `../p0-raw/` 原始记录）。

## 1. Python 侧前置修复（已核实坐实，本批修）

- `eval/risk_replay.py` `parse_diff`：**状态机守卫**——`+++ `/`--- ` 仅在本文件首个 `@@` 之前当文件头。
  修复前：removed 行内容以 `-- ` 开头（SQL/Haskell 注释等）→ 行文本 `--- …` 被当文件头吞掉（dels 少计）；
  added 行内容以 `++ ` 开头 → `+++ …` 误当新文件头（污染 files 字典）。selftest 加用例。
- `eval/e3_contract_ab.py:278`：`"anchored": rng is not None and unanch == unanch` 恒真式 → 改为在
  `anchor_quote` 失败分支记 `was_anchored`，表格列取真值（只影响人工确认表标注，不重跑 E3 实验）。

## 2. 修后重跑基线（口径修正）

- **PR181：不带台账**（P0 §15.1 已裁：r1 首跑误带 `--ledger` 使被测作业 upheld 泄漏进历史信号作废重跑）。
  计划文本「PR181 对拍传 dev 旧 proj.db」系笔误，实际 dev 旧 `proj.db` 是 **oracle B** 侧口径（见下）。
- **oracle B（本仓 7 diff）：`--ledger /Users/weilai/hobby/redteam/.redteam/proj.db`（dev 旧库）+ `--before <job created_at>`**，
  `--history-ref = base`。
- 修后数字与 P0 记录**逐位一致**（该 diff 无 `-- `/`++ ` 内容行，parse_diff 修复对本次语料零影响）：
  PR181 `R=4.336 / H0=17`（= r4 记录）；oracle B 合计 `hit_range=20/34 (0.59) / hit_file=29/34 (0.85)`（= §15.5）。

## 3. Rust vs Python 对拍（本批回归门）

**PR181**（`risk --base a7c827b70 --head 215afd77b`，仓库 `artifact-transfer-reliability`）：

| 指标 | python | rust | 判定 |
|---|---|---|---|
| R | 4.336 | 4.336 | Δ=3.4e-08 ≤ 1e-6 ✓ |
| H0 区 | 17 | 17 | matched 17/17，Jaccard 1.000 ✓ |

oracle 命中（Python 侧，对照 P0 基线 主裁决 4/4、并集 14/22）：**topic_diff 4/4、strict_diff 6/6、union 14/22**——不低于基线。

**oracle B 七 diff**（`risk --base <b> --head <h> --dismissed <dev 旧库> --before <job 时刻>`）：

| diff | python R | rust R | Δ | H0 py/rust | Jaccard |
|---|---|---|---|---|---|
| 33c4522..918f973 | 4.761 | 4.761 | 1.6e-07 | 21/21 | 1.000 |
| 4f52ac9..8dcaf6f | 11.93 | 11.93 | 3.1e-07 | 39/39 | 1.000 |
| 6f95c25~1..6f95c25 | 2.273 | 2.273 | 1.9e-09 | 5/5 | 1.000 |
| e8dc24f..00e23fc | 17.23 | 17.23 | 4.6e-07 | 67/67 | 1.000 |
| e8dc24f..3e60804 | 23.327 | 23.327 | 3.4e-07 | 88/88 | 1.000 |
| e8dc24f..99138bb | 23.323 | 23.323 | 4.6e-08 | 88/88 | 1.000 |
| f028f92..290236e | 2.924 | 2.924 | 2.5e-08 | 8/8 | 1.000 |

七 diff 全部 R Δ ≤ 4.6e-07、H0 Jaccard = 1.000。oracle 命中（Python 侧）与 §15.5 逐 diff 一致。

## 4. 对拍期间发现并修的三处实现差异（Rust → Python 口径对齐）

1. **涟漪 changed_names 漏删除行**：Rust 只扫新增行声明名；Python `rdefs`（删除行声明名）同样入 changed_names
   （PR181 `transfer_callback.go` 涟漪区即由此产生）。修：`regions.rs` 同扫 `h.minus`。
2. **R 分母含删除文件**：Python 丢弃 `+++ /dev/null` 的文件（head 树无此文件、不成区），Rust 曾把其删除行
   计入 code_lines（4f52ac9 差 67 行）。修：`code_lines()` 排除 `+++ /dev/null` 文件。
3. **重命名检测**：Python `git diff -M` 下纯重命名无 hunk（不成区、不计行）；Rust 逐路径 diff 会看成
   「删除+全量新增」产生大量假区（e8dc24f 系列 `src/dossier → src/diffsym` R100）。修：`changed_files_at`
   改「整份 `-M` diff + 按文件切分」。

## 5. 复算命令

```bash
# Python 基线（PR181，不带台账）
python3 eval/risk_replay.py replay <pr181-repo> a7c827b70 215afd77b \
  --history-repo <pr181-repo> --history-ref a7c827b70 --theta 1.5 --out .next/private/m21b/risk/pr181-py.json
# Rust 回放（在 <pr181-repo> 内跑；零 provider）
redteam-next risk --base a7c827b70 --head 215afd77b > .next/private/m21b/risk/pr181-rust.json
python3 eval/risk_replay.py compare .next/private/m21b/risk/pr181-py.json .next/private/m21b/risk/pr181-rust.json
# oracle B（ledger=dev 旧库 + before 守卫；七对同法）
python3 eval/risk_replay.py replay . <base> <head> --history-repo . --history-ref <base> \
  --ledger /Users/weilai/hobby/redteam/.redteam/proj.db --before <job created_at> --theta 1.5 --out .next/private/m21b/risk/ob-<tag>-py.json
```
