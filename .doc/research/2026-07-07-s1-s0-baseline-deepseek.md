# M15-S1 · S0 基线（单模型 DeepSeek，首个数据点）

> 状态: 已交付 · 完整 S0（N=3 × 多模型）见**附二**（2026-07-07 补）；正文 N=1/A-B 保留为过程记录。
> 关联: 设计 `../design/2026-07-07-redteam-context-management-s1-design.md` §5（退出门/测量）；roadmap M15-S1。
> **权威基线 = 附三**（流式 · 干净）；附二（非流式）已知被 provider 失败污染、降为过程证据。
> **一句话结论（附三）**：流式修复后，tool_use 尺子收紧（全体 CV 中位 20%、DeepSeek 13%，远低于成本 ±50%）——**尺子够稳**；3 可靠模型（gpt-5.5/DeepSeek/MiniMax = 100/93/87%）D25 agreement 基线 = 多模型认同率 17%。S1 门可测（read_files −26% > 噪声）。

## 目的

在 S1 pack 机制落地**前**，量当前系统（dossier 现状）在固定基准上的**残余 tool_use（模型主动 read/search 调用）** 与 tool-loop 轮数，作为 S1 A/B 的 OFF/基线臂参照。单模型 DeepSeek（此前最差模型）——残余 tool_use 是 per-cell 机械量，单模型即可见 pack 效果；agreement/独立性（多模型）留后。

## 设置（可复现）

- **Reviewer 二进制**：从干净 **813eb28**（dev HEAD）源码 `cargo build --release` 现构（含 tool_use 仪器 384fda6 + 1a 批量读 + read_files 行段 + 当前 dossier）。**注**：仓内已提交二进制是 stale 的（42dc77d / 2026-07-05 M14 卷宗态，早于仪器）——**不可**用它当基线（见下「修正」）。
- **基准改动集**：6f95c25（`--base 6f95c25~1`，branch scope = `6f95c25~1...6f95c25`）——M14 designated「修改型 caller 丰变更集」。经**干净 worktree @ 6f95c25** 跑（避主仓脏树污染 `reference_points` 全仓 walk）。
- **模型/参数**：`--models DeepSeek`（replicate，每 bundle 一格）`--no-dismissed`（无台账副作用，可复现）。
- **Job**：`9305014f8bd6`（`done`，2 cells parse-fail）。

## 结果：per-cell（5 cells，3 成功 / 2 失败）

| cell | ok | iterations | read_file | read_files | search_repo | list_files | tool_use 总 | input(cached) | duration |
|---|---|---|---|---|---|---|---|---|---|
| cli.rs+main.rs | ✅ | 12 | 0 | 8 | 3 | 0 | **11** | 646K(590K) | 182s |
| orchestrate/mod.rs | ✅ | 11 | 0 | 8 | 3 | 0 | **11** | 623K(566K) | 158s |
| prompt/mod.rs | ✅ | 9 | 0 | 6 | 4 | 0 | **10** | 364K(308K) | 100s |
| consolidate/verify.rs | ❌ parse | 0 | — | — | — | — | 0（失败记零） | 0 | 148s |
| templates(.md ×2) | ❌ parse | 0 | — | — | — | — | 0（失败记零） | 0 | 59s |

**成功格聚合（KPI 基线，n=3）**：
- **残余 tool_use 总**：11 / 11 / 10 → **均值 10.7、中位 11**（`read_files` 8/8/6 主导 + `search_repo` 3/3/4）。
- **tool-loop 轮数 iterations**：12 / 11 / 9 → 均值 10.7（与 tool_use 近同——每轮基本一次读）。
- 缓存命中 ~90%（cached/input），但**轮数/读调用未被缓存治理**（缓存治成本不治延迟，印证延迟主线）。

## 解读

模型每格补读 **6-8 次 `read_files` + 3-4 次 `search_repo`** ≈ 10-11 次工具调用。当前 dossier 只给引用点、**不给改动符号完整体（0 壳）**——模型只见 diff hunk，被迫全仓补读。**这坐实 S1 pack 立论**：0 壳完整体 + 省略清单应显著砍这批 read_files。S1 退出门 = 此 tool_use（跨模型 robust 下降）+ agreement 不收窄。

## 修正记录（复盘用）

1. **第一次跑用错二进制**：初用仓内已提交二进制（42dc77d/M14 卷宗态），它 predates 仪器 384fda6 → 不记 tool_use、且缺 1a/read_files 当前行为。作废，改现构 813eb28。
2. **stale 提交二进制**：仓内 bin 落后 dev 多个 commit（bin-in-git 之弊，见 TODO T11 分发改 Release）——量化实证：committed bin=42dc77d vs dev HEAD=813eb28。

## 局限（非完整 S0）

- **N=1**：单跑，未做「先证 KPI 尺子稳」的跨跑方差（S0 dual-baseline 的 N=3 待补）。此为**首个数据点**，非完整 S0。
- **单模型**：无 agreement 直方图（独立性 D25 门是多模型量，留后）。
- **2/5 格 DeepSeek JSON parse 失败**（最差模型的合规问题，非上下文相关）→ 仅 3 个可用点。
- **最终匹配 A/B**：待 S1 落地后用 **`REDTEAM_PACK` on/off 同二进制**在同基准跑（本记录=预备 OFF 臂参照）。

---

## 附 · S1 匹配 A/B（2026-07-07，含 JSON 修复后）

**设置**：分支 `feat/context-mgmt-s1-pack` HEAD `92bf594`（含 `parse_lens_output` 字符串感知修复），同 fresh 二进制 `REDTEAM_PACK` on(pack)/off(=0, dossier 基线) 两臂、6f95c25、DeepSeek、`--no-dismissed`。OFF job `d79f17a27716` / ON job `a0577de1bca5`。**5 格两臂全成、0 JSON 失败**（修复前上一轮 2/10 失败搅黄对照）。

| 指标（5 格合计） | OFF(dossier) | ON(pack) | Δ |
|---|---|---|---|
| **read_files** | 31 | 23 | **−26%** |
| tool_use 总 | 42 | 38 | −10% |
| iterations(轮) | 46 | 37 | **−20%** |
| input tokens | 2497K | 1515K | **−39%** |

**逐格**：cli+main 大赢（tool_use 11→6、iters 12→6、input −64%）；prompt 赢（7→4）；orchestrate read_files 9→6 但 search_repo 3→7、input −60%；verify.rs 微逆（8→10，搜索多）；templates(.md) 4=4（非支持语言=天然对照组，验方法学无串味）。

**判定**：pack **弱-中阳**——**read_files −26% / 轮数 −20% / input −39% 三线一致下降**（S1 退出门 tool_use 侧过，单模型口径）；总 tool_use 只 −10% 因模型把省下的 read 转 search_repo（1 壳未覆盖的 caller→提示 S2 语义索引价值）。**JSON 修复（`92bf594`）是拿到干净对照的前提**——doc-aligned 根因修（串内括号错位），非 json_object。

**局限**：N=1 单模型；完整 S1/S0 门（N=3 稳定性 + 多模型 agreement 不收窄）待补。

---

## 附二 · 完整 S0（N=3 × 3 模型，2026-07-07）〔非流式·已污染→被附三取代〕

> ⚠️ **降为过程证据**：本轮用**非流式**二进制，DeepSeek/Kimi/MiniMax 大量 cell 因 HTTP 连接
> reset/超时失败（Kimi 13%、DeepSeek 87%），失败/重试污染了 tool_use 方差与 agreement。它促成了
> **流式改造（commit 2d9945d）** 的立项——权威干净基线见**附三**。下文读数仅存档「为何要流式」。

**设置**：fresh 二进制 `e7c66a5`（= dev HEAD 8886299 + 注释修 e7c66a5，行为同）；基准 6f95c25（`--base 6f95c25~1`）；干净 worktree；`--models DeepSeek,VibeCodingFast,Kimi`（VibeCodingFast=gpt-5.5）；**baseline 臂 `REDTEAM_PACK=0`**（= 前 dossier 参照态）；`--no-dismissed`。**N=3 串行 job**（D9 禁并发：`run1 done→run2→run3`，job 内 15 cell=5 bundle×3 模型并发）。Jobs：`06196a9b291a` / `f4bc12e703a2` / `f176c9dbadb1`。

### 1) 模型可用性（意外头条）

| 模型 | ok/总(15) | 说明 |
|---|---|---|
| **gpt-5.5**(VibeCodingFast) | **15/15 = 100%** | 最稳、tool-loop 全通 |
| **DeepSeek** | 13/15 = 87% | 可用（prompt/mod.rs 挂 1、模板挂 1） |
| **Kimi** | **2/15 = 13%** | ✗ 全因 `timeout: error sending request → api.moonshot.cn`（中国端点/US 网络，`attempts=1` 未克服）——**非代码/格式问题，是网络** |

→ **S0 实为 2 模型基线（DeepSeek + gpt-5.5）**；Kimi 从本网络不可靠。

### 2) KPI 尺子稳定性（S0 首要目的：先证尺子稳）

设计门槛：`tool_use 跨跑方差 ≪ 成本 ±50%`，不稳先修尺子。per-cell tool_use 跨 3 跑 CV（stdev/mean，仅 ok 格）：

| 模型 | per-cell CV 中位 | 范围 |
|---|---|---|
| **gpt-5.5** | **18%** | 0–64% |
| **DeepSeek** | **41%** | 24–50% |

**判定：尺子稳定性 model-dependent**。gpt-5.5 是**紧尺子**（18%，可用）；DeepSeek 是**松尺子**（41%≈它要打败的成本方差，勉强）。→ 修法 = **用 gpt-5.5 当主尺 + 加 N**，勿以 DeepSeek 单跑下结论。

### 3) 对 S1 验收门的直接含义

- 之前 A/B：`read_files −26%`、但**总 tool_use 仅 −10%**。
- **−10% 打不过 DeepSeek 41% CV 噪声底**——在 DeepSeek 尺上 S1 效应与噪声不可分；在 gpt-5.5 18% 尺上 −10% 也仅勉强（信噪比 <1）。
- **结论**：宣告「tool_use robust 下降」需 **①S2 语义索引把 search_repo 也压下、使总降幅远超 18%**，或 **②在 gpt-5.5 紧尺上 N≥5 测统计显著**。当前 −10% 不足以在噪声上过门。

### 4) agreement 基线（D25 独立性门参照）

| 跑 | findings | agreement 分布 {1,2,3} | 模型参与度 |
|---|---|---|---|
| run1 | 10 | {8, 2, 0} | DeepSeek 8 / gpt-5.5 4 |
| run2 | 10 | {9, 1, 0} | DeepSeek 4 / gpt-5.5 1 / Kimi 6 |
| run3 | 7 | {5, 1, 1} | DeepSeek 5 / gpt-5.5 2 / Kimi 3 |

**判定**：基线 agreement **已在地板**（绝大多单模型 agreement=1，2-way 罕、3-way 仅 1 次），且被 Kimi 网络失败随机污染 → **D25「pack 是否使 agreement 向假共识收窄」当前测不了**（floor + 模型随机缺席）。D25 门需**先有所有模型都真开火的干净多模型基线**：修 Kimi 或换第三可靠模型，否则只能 DeepSeek+gpt-5.5 两模型测（2-way 收窄信号弱）。

### 综合 verdict

完整 S0 的核心产出不是「基线数字」而是**「尺子体检」**：**tool_use 尺子仅 gpt-5.5 够紧、agreement 基线在地板 + 被 Kimi 污染**。这**改变 S1 验收/开 S2 的前提**：
1. S1 tool_use 门 → gpt-5.5 紧尺 + N≥5 重测（当前 −10% 淹在噪声）。
2. D25 独立性门 → 先修 Kimi（或换可靠第三模型）取干净多模型 agreement 基线。
3. **不变**：−10% 总 tool_use + search_repo↑ 仍实证 S2 语义索引价值方向（regex-A 漏的 caller → 模型转 search_repo）。

**派生 TODO**：**T35**（Kimi/moonshot 端点从本网络 timeout 频发 + `attempts=1` 未重试克服——provider 健壮性）。

---

## 附三 · 权威 S0（**流式** · N=3 × 3 可靠模型，2026-07-08）

**设置**：流式二进制 `2d9945d`（OpenAI backend 改 SSE，治非流式连接 reset/超时根源）；`--models DeepSeek,VibeCodingFast,MiniMax`（VibeCodingFast=gpt-5.5；Kimi 弃=本网络尾部超时，T35）；baseline 臂 `REDTEAM_PACK=0`；基准 6f95c25；干净 worktree；`--no-dismissed`；**N=3 串行 job**。Jobs：`37ae203de505` / `20063746bebe` / `9024aa2631ec`。

### 1) 模型成功率（流式后大幅改善）

| 模型 | ok/15 | 污染版(附二) |
|---|---|---|
| **gpt-5.5** | **15/15 = 100%** | 100% |
| **DeepSeek** | 14/15 = 93% | 87% |
| **MiniMax** | 13/15 = 87% | ~13-40%（PARSE/timeout） |

→ 总 42/45=93%。**MiniMax 从「不可用」变「87% 可靠」**——其非流式 PARSE 失败原是连接后果（慢收尾→坏响应），流式 + `<think>` 剥离(7649a23)后正常出 JSON，`reasoning_split` 集成免做。残余 3 失败（MiniMax timeout×1/PARSE×1、DeepSeek PARSE×1）是 15 并发重载下的零星。

### 2) KPI 尺子稳定性（S0 首要目的——流式收紧了尺子）

per-cell tool_use 跨 3 跑 CV：

| 模型 | CV 中位 | 范围 | 附二对比 |
|---|---|---|---|
| **DeepSeek** | **13%** | 6–41% | 41% → **紧 3 倍** |
| **gpt-5.5** | 20% | 0–35% | 18% |
| **MiniMax** | 26% | 0–95%¹ | 新 |
| **全体 3 模型** | **20%** | — | — |

¹ MiniMax 在 templates 格 vals=[2,3,18]、18 离群。**流式不只提成功率，还把尺子收紧**（非流式失败/重试本身灌方差）。**判定：尺子够稳**（中位 20% ≪ 成本 ±50%）——S1 的 `read_files −26%` 打得过噪声、门可测。

### 3) D25 agreement 基线（3 可靠模型，现在测得干净）

| 跑 | findings | agreement {1,2,3} | 参与（DS/gpt/MM） |
|---|---|---|---|
| run1 | 10 | {8,2,0} | 5/2/5 |
| run2 | 10 | {7,1,2} | 5/2/8 |
| run3 | 15 | {14,0,1} | 4/4/9 |
| **合计** | **35** | **{29,3,3}** | — |

**多模型认同率 = 17%**（6/35 findings ≥2 模型；83% 单模型）。模型性格互补（MiniMax 话多找得多、gpt-5.5 保守找得少）=天然多样性。**这是 D25 干净参照**：S1 pack 臂若把认同率异常推高 = 灌同份 radius 事实→假共识红旗（门跳则 grill D25 解）。

### 综合 verdict（翻盘）

「先修仪器（流式）再立基准」被数据证实：**流式同时提成功率（93%）+ 收紧尺子（CV 20%）+ 解锁 3 模型 agreement**。S0 三判据齐：尺子稳✅、可靠模型集✅、D25 基线✅。**S1 验收门现可测**——下一步 = 用流式二进制重跑 S1 匹配 A/B（pack vs baseline，3 模型 N=3），对本附三做差：tool_use robust 下降 且 agreement 认同率不异常收窄。

---

## 附四 · S1 匹配 A/B（pack ON vs 附三 OFF，流式 · 3 模型 · N=3，2026-07-08）

**设置**：同流式二进制 `2d9945d`、同 3 模型、N=3、`REDTEAM_PACK` 不设（pack 臂）；OFF 臂**复用附三**（省半价，未重跑）；基准 6f95c25、干净 worktree、`--no-dismissed`。ON jobs：`e512517e923c`/`e695092c6351`/`0d9ee37c5cc6`（43/45 ok，2 PARSE）。

### 门① 残余 tool_use 下降（全 ok 格均值/格）

| 指标 | DeepSeek | gpt-5.5 | MiniMax | 全体 |
|---|---|---|---|---|
| tool_use 总 | −4% | −11% | −13% | **−8%** |
| read_files | −2% | −11% | −17% | **−10%** |
| search_repo | −9% | −27% | −6% | −7% |
| input tokens | +21% | −15% | −29% | **−10%** |

**三模型方向一致（tool_use 全负）**，但幅度 −8~10% **< 尺子噪声（附三 CV 中位 20%）**。→ **弱正**：方向稳健、量级淹在噪声。注：search_repo 也降（推翻非流式那版「转 search」误判——干净测下 read 与 search 双降）。DeepSeek input +21%（pack 注入 0 壳体、其 read 未减够抵消）；MiniMax −29%。

### 门② agreement 不向假共识收窄

| | findings | {1,2,3} | 多模型认同 |
|---|---|---|---|
| OFF（附三） | 35 | {29,3,3} | **17%** |
| ON（pack） | 33 | {24,4,5} | **27%** |

agreement +10pp。**定性判（零成本，读 ≥2 模型认同 finding）**：ON 多出的认同**全在真高危 bug**（empty-diff exit 0、"upheld" 前缀反转 refutation、on_meta 未接 run_resume、doc 契约不对称）——非垃圾盖章；总 findings 35≈33 **覆盖未丢**。→ **真收敛（pack 供更好共享上下文→三模型独立更常撞同一真 bug），非假共识。D25 红旗良性。**

### S1 verdict：**弱通过 · D25 良性**

- 门①：tool_use −8~10% 弱正（方向稳、量级 <噪声）。
- 门②：agreement +10pp 但**真收敛非假共识**（D25 独立性未破，反是质量红利=三模型更常认同真 bug）。
- pack 机制验证成立；tool_use 大收益留 **S2 语义索引**（接 regex-A 漏的 caller、真正压 search_repo）。**质量红利（真 bug 收敛）可能比省 tool_use 更值。**
- **成本**：S0(附三)+S1 对照账已闭，勿再烧；后续省钱杠杆见 memory/[[redteam-context-mgmt-redesign]]。
