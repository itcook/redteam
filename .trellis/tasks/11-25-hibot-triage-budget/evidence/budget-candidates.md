# triage 整轮预算包线：公式候选与真实数据对照（2026-11-25）

> 本文是**待批准**的候选方案，不含实现。所有数值由同目录 `budget_estimate.py` 生成，可离线复跑。
> 这些数是**预算包线对照**，不是耗时预测。

## 1. 现状（先核过的代码）

- `src/job/run.rs:444`：triage 的整轮 deadline = `effective_deadline(started, manifest.limits.wall_clock_s, None)`。
  `None` 表示没有更紧的建议预算，因此**整轮 deadline 恒等于 manifest 硬上限**（eval 场景 1800s）。
- `src/triage/budget.rs` 只按规模放大**单 cell / I0 包线**：
  - `cell_timeout = clamp((60 + 6×diff_kb + extra_s) × 1.15, 120, 600)`；
  - `time_factor(manifest) = clamp(manifest/1200, 1, 1.5)`，单 cell 最高 900s；
  - `i0_timeout = clamp(120 + 2×text_kb, 120, 240)`，×factor 后最高 360s；
  - `shard_count = min(4, ceil(diff_kb/45), ceil(files/6))`。
- 三轴在 `triage::run` 里用 `tokio::join!` **并发**执行；并发只受 provider gate 约束。
  历史 job 的 `extensions.redteam.concurrency` 记录 `provider_caps: {deepseek|agc: 4}`。
- `summary.routing.wall_clock_s` 在 `src/triage/tally.rs:404` 由 risk 三档派生（100%/75%/50%，下限 300s），
  **本任务不动它**。

## 2. 数据来源与口径

原始输入全部归档在本目录，与 `/tmp/hibot-triage-eval/jobs` 的历史运行逐字节一致（已核对 SHA256）：

| 文件 | 用途 |
|---|---|
| `diff.hiapi-<repo>-pr-<n>.patch` | 历史运行的精确 `diff_patch`（`git diff merge_base..head`） |
| `pr-context.hiapi-<repo>-pr-<n>.json` | 历史运行的精确 `pr_context` |
| `manifest.hiapi-<repo>-pr-<n>.json` | 历史 manifest（`limits.wall_clock_s`） |
| `metrics.phase3-8.jsonl` | 5 个 DeepSeek ok 轮的整轮墙钟 |
| `report.glm54-1800c.json` | backend #333 GLM-5.4 在 1800s + 1.5× 下唯一 ok 轮（1050.125s） |

口径：

- `diff KB` = patch 的 UTF-8 字节数；`files` = `diff --git` 边界数（= `FileChange` 数）。
- `modules` = 文件 parent dir 去重；根目录文件归 `"."`（与 `standards::module_of` 一致）。
- `text B` = **title + body + linked issues(title+body)** 的 UTF-8 字节（本任务要求的规模口径；空 issues 记 0）。
- A 公式的 I0 输入沿用**现有实现口径**：`body + linked_issues.body + 非 bot 且非作者的 comments`。
  两者不同，文中分别标注。

### 证据缺口（不补造）

- `report.hiapi-backend-pr-333.json`（DeepSeek ok、164.1s）**已缺失**：`/tmp` 的主 job 目录被后续 GLM 轮覆盖成 partial，
  归档目录也没有该报告。164.1s 只存在于 `metrics.phase3-8.jsonl`。
- 5 个 DeepSeek ok 报告的 `axes[].elapsed_ms` **未记录**（旧版本字段为 null），所以无法给出逐轴耗时。
- `partial` 样本只说明中断时的观测，不能当成功完成时间。
- GLM-5.4 只有 backend #333 这一个成功点（1050.1s），不能据此声称"稳定完成"。

## 3. 实测锚点

| PR | 规模（diff / files / modules / text） | DeepSeek ok | GLM-5.4 ok |
|---|---|---|---|
| backend #333 | 130.3 KB / 13 / 4 / 7054 B | 164.1s | **1050.1s**（1800s + 1.5×） |
| cli #18 | 78.9 KB / 28 / 10 / 1996 B | 172.2s | — |
| sites #235 | 253.1 KB / 62 / 15 / 4345 B | 133.1s | — |
| sites #277 | 147.7 KB / 16 / 10 / 8840 B | 116.1s | — |
| sites #328 | 90.6 KB / 30 / 12 / 3374 B | 160.9s | — |

两个可直接读出的事实：

1. **规模与 DeepSeek 整轮耗时基本不相关**（最大 PR #235 反而最快）。所以"按规模估算"只能是包线，不能当 ETA。
2. **整轮墙钟 ≈ 最慢轴 + 收口**：GLM 1800c 的三轴 elapsed 为 217.1 / 617.8 / 1050.0s，整轮 1050.1s。
   轴时间是并发墙钟（含等门），**不能相加**当作整轮耗时。

## 4. 候选公式

记号：`m = manifest.limits.wall_clock_s`；`f = clamp(m/1200, 1, 1.5)`；
`estimate` 最终取 `min(m, 原始值)`（只能收紧，不能放宽）。

### A. 串行求和包线（用户提出的形态）

```
I0 = scaled_i0(i0_text_kb, f)                         // 现有函数，≤360s
Σsec    = Σ 每个 security shard 的 scaled_cell(slice_kb, 0, f)
Σdrift  = Σ 每个 drift shard 的 scaled_cell(slice_kb, 10KB×1.0, f)
Σintent = Σ 每个 intent slice 的 scaled_cell(slice_kb, 20 + 20×0.5, f)
estimate_A = I0 + Σsec + Σdrift + Σintent + 30        // 30s = 锚定/去重/tally/报告收口
```

显式假设（整轮 deadline 在 I0 之前确定，claims 数/标准包大小未知）：
`claims ≤ 20 条`、`单模块 standards pack ≤ 10 KB`、收口 30s。

### B. 规模连续包线（两个系数档）

```
estimate_B = base + per_diff_kb×diff_kb + per_file×files + per_text_kb×text_kb
           ≥ 900s（`min(manifest, 900)`，2026-11-25 用户批准）
```

| 档 | base | per_diff_kb | per_file | per_text_kb | 说明 |
|---|---|---|---|---|---|
| B1 | 180 | 6.0 | 6.0 | 2.0 | `per_diff_kb` 复用现有 `PER_KB_S`；用 GLM backend #333 单点校准 |
| B2 | 220 | 7.5 | 7.5 | 2.5 | 在 B1 上加约 25% 余量，作为跨模型的保守档 |

## 5. 对照表（统一目标场景 `m = 1800`）

| PR | diff KB | files | modules | text B | A 原始 | A→1800 | A→历史manifest | B1 原始 | B1→1800 | B2 原始 | B2→1800 | 实测 DeepSeek | GLM 锚点 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| backend#333 | 130.3 | 13 | 4 | 7054 | 5114 | **1800** | 1200（cap 1200） | 1054 | **1054** | 1312 | **1312** | 164.1s (ok) | 1050.1s |
| cli#18 | 78.9 | 28 | 10 | 1996 | 3425 | **1800** | 900（cap 900） | 825 | **900** | 1027 | **1027** | 172.2s (ok) | — |
| sites#235 | 253.1 | 62 | 15 | 4345 | 8916 | **1800** | 900（cap 900） | 2079 | **1800** | 2594 | **1800** | 133.1s (ok) | — |
| sites#277 | 147.7 | 16 | 10 | 8840 | 5959 | **1800** | 900（cap 900） | 1179 | **1179** | 1469 | **1469** | 116.1s (ok) | — |
| sites#328 | 90.6 | 30 | 12 | 3374 | 4171 | **1800** | 900（cap 900） | 910 | **910** | 1133 | **1133** | 160.9s (ok) | — |

小 PR 示例：

| 示例 | A 原始 | A→1800 | B1 原始 | B1→1800 | B2 原始 | B2→1800 |
|---|---|---|---|---|---|---|
| 小 PR（1 文件 / 1 KB / 0.3 KB 文本） | 751 | **751** | 193 | **900** | 236 | **900** |
| 空 diff（仅声明核对） | 211 | **211** | 181 | **900** | 221 | **900** |

A 明细（`m = 1800`）：

- backend#333: i0=200s n_sec=3 n_drift=3 Σsec=1659 Σdrift=1410 Σintent=1815 + 收口 30s
- cli#18: i0=186s n_sec=2 n_drift=2 Σsec=1024 Σdrift=1058 Σintent=1127 + 收口 30s
- sites#235: i0=193s n_sec=4 n_drift=4 Σsec=3034 Σdrift=2419 Σintent=3240 + 收口 30s
- sites#277: i0=206s n_sec=3 n_drift=3 Σsec=1839 Σdrift=1891 Σintent=1994 + 收口 30s
- sites#328: i0=190s n_sec=3 n_drift=3 Σsec=1248 Σdrift=1300 Σintent=1403 + 收口 30s

## 6. 三个方案的取舍

| | A 串行求和 | B1 规模连续 | B2 规模连续（保守档） |
|---|---|---|---|
| 依据 | 现有单 cell 包线的机制组合 | 与 GLM 成功点单点校准 | B1 + 25% 余量 |
| 小 PR | 751s（明显收紧） | 900s | 900s |
| 中 PR（#328/#cli） | 顶格 1800 | 910 / 825 | 1133 / 1027 |
| backend #333（GLM 1050） | 顶格 1800 | 1054（1.004×，无余量） | 1312（1.25×） |
| 大 PR（#235） | 顶格 1800 | 顶格 1800 | 顶格 1800 |
| 主要风险 | 绝大多数中大型 PR 不收紧（与现状差别只在小 PR） | 跨模型余量不足；单点拟合 | 系数是经验值，缺第二组慢模型样本 |
| 可解释性 | 最强（每项都能对上现有一行代码） | 强（线性项） | 强（线性项） |

**已批准 B2（2026-11-25）**，理由：

1. A 在 5 个真实 PR 上有 3 个直接顶格，`A→历史manifest` 5/5 顶格——对现状几乎无改善，只剩小 PR 一个收益。
2. B1 的 1054 vs 1050.1 是单点拟合（0.4% 余量），任何模型波动都会截断；不适合作包线。
3. B2 对唯一的慢模型成功样本给出 1.25× 余量，同时中 PR 仍明显收紧（#328 1133、cli 1027），小 PR 收到 900s。
4. 若后续拿到第二个慢模型成功样本，只需重算 `base/per_*` 四个系数，不改结构。

不建议在本任务引入"总 cell 预算 ÷ provider 门宽"：它会漏掉 intent 分批串行、等门与 429 收敛，
而且把运行期调度假设固化进 deadline。

## 7. 用户裁决（2026-11-25）

1. **公式**：B2（规模连续保守档）。
2. **下限**：`min(manifest, 900)`。
3. **报告**：写入 `extensions.redteam.budget.estimate_s`。
4. **接受**“多数大 PR 仍顶到 manifest”：这是包线语义的自然结果，不为好看调低系数。

## 8. 复跑方式

```bash
cd .trellis/tasks/11-25-hibot-triage-budget/evidence
python3 budget_estimate.py
```
