# Phase 3 · triage（3.1–3.10 + grill 定案 2026-11-24）

## 背景

hibot audit 执行单元接入的 U4 单元（协调者 session `01a20ba0` 派发，用户已"全默认"批准 grill 结论）。
计划权威：`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §6 Phase 3 / §11 / §12（Q4/Q9）/ 附录 D·E；
契约权威：`~/hibot/.docs/audit/contract.md` §1.2 / §3.2 / §3.4 + `schema/*.schema.json`（**只读**，不复制进本仓）。
人工基线：`~/.pi/agent/skills/pr-triage/SKILL.md`（7 个 sink 面、tally、fit 口径的出处）。

前置：U3（Phase 2 review 端到端）已合入 `hibot-executor`（`ff56a0e`）。本单元继续在
`feature/hibot-audit-exec`，不 rebase、不碰 `hibot-executor`、`~/hibot` 只读、不改 `dev`。

## 2026-11-24 grill 定案（8 条，用户"全默认"）
1. **安全轴 7 面与置信度**：逐字采用 pr-triage 的 7 个编号面（见下表）；模型自评 0–10，
   必须同时给出「source→sink 可达路径」+ 逐字行；给不出路径的封顶 7（即被 `<8` 丢弃）。
   不做第二模型复核；tier2 误升率由 3.8 人工基线度量。
2. **tier2 边界**：面 1/3/4 ∧ `confidence ≥ 8` → tier2；一行命中多面按"任一面命中"；
   **secrets（面 2）conf≥8 也升 tier2**（2026-11-24 用户裁决：secrets 直接挡下班 review）；面 4 要求"用户可控输入可达"，
   不可达不算该面。severity：tier2 候选 `high`、其余安全行 `medium`；`confidence = conf/10`。
3. **`risk` / `fit`**：`risk` 照计划字面（tier≥1 ∨ blockers≥3 → high；blockers≥1 ∨ scope_creep>0 → medium；
   else low）。`fit` 用**混合口径**（替代原 `[拟议]`）：
   - `needs-scope-decision` = 结构性漂移含 placement/naming/ownership/versioning 破坏 **或** 声明未兑现；
   - `possibly-redundant` = 维护者（非作者、非 bot）评论指出重复/已存在能力，且无未兑现声明；
   - 否则 `in-scope`。
4. **意图声明来源**：只取 PR body + linked issues（title/body）；PR title 只当摘要、不作独立声明；
   comments 不作声明（作者评论可在 body/issue 有对应时作补充证据，否则进 `extensions.axes` 的 note）；
   `is_bot=true` 绝不作为作者声明；维护者评论只用于 fit/重复与 scope 信号。冲突时 linked issue > body。
5. **`StandardsMap`**：doc 候选 = `ARCHITECTURE.md` / `CONTRIBUTING.md` / `docs/adr/**` / `AGENTS.md` /
   `CLAUDE.md` / `.rpiv/guidance/**/architecture.md` / 仓内 conventions 文档；逐 changed file 向上走到仓根；
   doc > linter > peer，三者都记录；peer 用同目录 2–3 个兄弟文件；`no-standard` 仅三者皆无。
   `routing.specs` **只放 doc 来源的 repo 相对路径**，去重、blocker 相关优先、上限 8 条；
   linter/peer 只进 `extensions.axes` 披露。
6. **分片 / 并发 / 预算**：安全轴按 changed-file 分组建片 `N = min(4, ceil(files/6))`；
   漂移轴按模块（目录）分片 `N = min(4, modules)`；意图轴单 cell（全局声明，分片无意义）。
   三轴并行，并发**只受 provider 子闸**约束（D72），不设 job 总闸；单轴失败 → `partial` +
   `triage_axis_failed:<axis>`。不加新配置块（**2026-11-24 用户调整：复用 `roles.reviewer`**，用 reviewer 的思考档位；`config check --kind triage` 也改为要求 reviewer）。
   `wall_clock_s` 三档：high=100%（=manifest 上限）、medium=75%、low=50%，下限 300s，恒 ≤ manifest。
   `focus` ≤5 条：structural→`file`、security→`<surface>@file`、intent→`intent:<短句>`。
7. **3.6 `blockers` 落点**：`extensions.redteam.blockers[]` 是**给 hibot 的披露面**；
   review 侧唯一输入仍是 `summary.routing`（Phase 2.3 的「只读 routing」是已验收硬断言）。
   blockers 影响 review 的路径是 `routing.focus`，**不是**让 review 读 blockers 数组。
   测试断言：`findings[]` 与 `blockers[]` 是两个不同数组，且 review 不读 blockers。
8. **3.8 对拍集合与上限**：5 PR = sites #275(tier2,blk2) / #333(tier2,blk0) / #277(tier0,blk8) /
8. **3.8 对拍集合与上限**：5 PR = sites #277(tier0,blk8) / #235(tier0,blk7) / #328(tier0,blk1–4) +
   cli #18(tier0,blk3,local11) + backend #333(tier2,blk0,local)（**2026-11-24 修正**：原拟的
   sites #275/#333 在 sites 仓没有人工产物，早先聚合扫描串仓；backend 用临时 blobless 克隆取数）。
   ≤8 次真跑、单次 ≤10min、≤400k input / 40k output tokens；模型用 `~/.config/redteam/config.toml`
   已配的 agc（GLM/Kimi）+ deepseek；连续 2 次 infra 失败即停。
   已获准对 `~/hiapi/sites`、`~/hiapi/cli` 执行 `git fetch` 取 `pull/<n>/head`（只写 refs，不 checkout、
   不改工作树/分支）；对拍检出放临时目录或 job 目录内。
### 2026-11-24 rubric 校准（协调者裁 A）

- 只有同时给出 **source 端 file:line + sink 端 file:line + 中间传递链** 才算"具体可达路径" → 必须 ≥8；
  缺任一端 → 仍 ≤7 丢弃。模板变更：`triage-security.md`；模板哈希 `ec9c5aa51d9d89a3` → `c567e0d51fe82c91`。
- **2026-11-24 A2 可达性定义补全**：缓解措施（白名单/审批卡/限流/幂等键…）不打断代码路径；
  仍报行、mitigation 只进 `note`，不参与 conf 打分。模板哈希 `c567e0d51fe82c91` → `2c750615f126b5f3`。
- `axes[].dropped` 增加 `dropped_total` 与 `dropped_rows[]`（每轴 ≤20；`{file,line,surface,conf,reason}`），
  只作诊断披露，不进契约必需字段。
### 2026-11-24 A 两阶段 intent（定案）

- **map 键 = diff 切片，claim 是 reduce 键**：不按 claim 拆（那会把完整 diff 复制进每组）；每个切片带
  「完整 claims 列表 + 自己的 diff」，输出 `delivered[]` + `scope_creep[]`。
- **I0 串行**：PR body + linked issues + 非 bot/非作者 comments → `claims[]`（id 由执行单元规范化为 c1..cn）
  + `maintainer_overlap`；预算 `min(150s, remaining/4)`。I0 失败 → intent 快速失败，不占波次门位。
- **I1..In**：N 与 security 同源（`min(4, ceil(files/6))`）；缺证据的 claim 不在该切片判未兑现
  （absence 不是证据），由 reduce 并集判定。
- **partial 语义**：slice 未全成 → `verification_complete=false`；未验证 claim 只披露，
  **不计入 `intent_undelivered`/blockers**（协调者 2026-11-24 裁）。
- 模板：`triage-intent.md` 退役，新增 `triage-intent-claims.md` / `triage-intent-slice.md`；
  模板哈希 `2c750615f126b5f3` → `ad41f9d1df42eb13` → `f8e848e16e926990`（I0 排除 build/test/smoke 等验证性声明，避免虚增 undelivered）。

### 2026-11-24 budget v1（全默认；替代 grill 第 6 条的固定分片公式）

> 规模只决定**预算包线**与分片数，不预测 LLM 实际耗时；常量锚点 = GLM-5.4 high 实测（censored）。

- `cell_timeout = clamp((60 + 6*diff_kb + extra_s) * 1.15, 120, 600)` 秒
  - security：`extra=0`；drift：`extra=pack_kb*1`；intent slice：`extra=20 + claims*0.5`
- `i0_timeout = clamp(120 + 2*text_kb, 120, 240)` 秒（v1.1：GLM-5.4 实测 73.6s 内未返回）；另与 `remaining*0.8` 取 min
- **总预算放大**：`factor = clamp(manifest/1200, 1.0, 1.5)`；cell 与 I0 都乘 factor（cell 上限 900s、I0 上限 360s）
  - 1800s 实测：GLM-5.4 三轴全成、1051s、status=ok；tier0（安全候选 conf7）
- 分片：`n = min(4, ceil(total_diff_kb/45), ceil(files/6))`；drift 再 `min(modules)`
- 每个 cell 的预算独立按自己的 diff 规模算；**分片按字节均衡**（不是按文件数），避免 24/64/44KB 失衡；
  **等 provider 门的时间不计入 cell 预算**（由全局 deadline 兜底），否则排队的轴会被饿死；`axes[].elapsed_ms` 落盘用于校准
- 原 grill 的 `min(4, ceil(files/6))` / `min(4, modules)` 由本节替代。

### tier2 白名单对照表（可复核）

| 面 | 名称 | 判据（摘要） | 可升 tier2 |
|---|---|---|---|
| 1 | supply-chain | 安装/构建钩子联网、不可信/未锁依赖、lockfile 换源 | **是** |
| 2 | secrets | diff 中新增凭据/密钥/token/PEM/连接串 | **是（conf≥8）** |
| 3 | CI/CD 投毒 | workflow/pipeline 以 elevated scope 跑 PR 受控输入，或向 fork PR 暴露长期密钥 | **是** |
| 4 | 代码执行 / 不安全反序列化 | shell/进程 spawn、eval/动态 import、可执行代码的反序列化，且**用户可控输入可达** | **是** |
| 5 | 注入 | 用户输入拼进 SQL/NoSQL/LDAP/XPath/shell 解释器 | 否（tier1） |

> **2026-11-24 用户裁决**：`security_tier=2` = 分诊不通过、直接挡下班 review；因此 secrets（面 2）与面 1/3/4 同权，conf≥8 可升 tier2。面 5–7 恒 tier1。
| 6 | 路径穿越 | 用户可控路径进文件 API 且无规范化 | 否（tier1） |
| 7 | SSRF | 出站请求的 host/protocol 用户可控 | 否（tier1） |

## 数据流

```
job.json ──manifest/artifact 校验──► pr_context / diff_patch / merge_base（+ repo 只读）
    │
    ├─ standards::build ──► StandardsMap（逐 changed file：doc|linter|peer）
    ├─ lenses::security × N ─┐
    ├─ lenses::drift    × N ─┤
    ├─ intent claims     × 1 ─┤（串行 I0：body/issues/comments，无 diff）
    └─ intent slices     × N ─┘（I0 之后：每片 = claims + 自己的 diff）
              │
              ├─ 逐字重锚（quote 必须是 repo 原文；失败 → 丢弃 + 计数）
              ├─ 按轴去重（安全/漂移 file+归一 quote+hint；意图按声明身份）
              ├─ tally（单点确定性函数）──► summary 九键 + routing
              └─ report::triage ──► findings[]（tier≥1 + structural）
                                    extensions.axes[] / blockers[] / language / budget / template_hashes
```

## 范围与落点

| 项 | 内容 | 落点 |
|---|---|---|
| 3.1 | 输入：`pr_context`+`diff_patch`+`merge_base` 必需；repo 只读；输入缺失 → error | `src/triage/mod.rs`、`src/job/run.rs` |
| 3.2 | `StandardsMap`：doc→linter→peer；`no-standard` 仅三者皆无 | `src/triage/standards.rs` |
| 3.3 | 三轴 lens + 扇出；行内置信度；语言随 `language` | `src/triage/lenses.rs`、`src/prompt/templates/triage-*.md` |
| 3.4 | 按轴去重；无锚意图项按声明身份 | `src/triage/tally.rs` |
| 3.5 | 确定性 tally 与字段生成（表驱动单测） | `src/triage/tally.rs` |
| 3.6 | `extensions.blockers[]` 仅披露；review 不消费 | `src/job/report.rs` |
| 3.7 | 输出契约：findings/summary（九键+routing 内嵌）/extensions.axes | `src/job/report.rs` |
| 3.8 | 5 个真实 PR 对拍；机读差异表 + 原始三轴行 | `.trellis/tasks/11-24-hibot-phase3-triage/evidence/` |
| 3.9 | 用真 triage 报告回归 2.3（focus/specs/预算生效、阻断阈值不变） | `tests/` |
| 3.10 | capabilities 加 `triage`；`requires.pr_context` 保持 false | `capabilities/redteam.json`、`tests/capabilities.rs` |

## 输出契约要点

- `summary` 九键**只在 `status=ok` 必需**；`routing` 内嵌在 `summary`，不得提到顶层；`partial` 不得用零值凑九键。
- `findings[]` = 安全 tier≥1 + **结构性**漂移（带指纹）；`local` 漂移只计数（`local_count`）并进 `extensions.axes`。
- `blockers[]` = structural findings + 意图未兑现项（可无代码锚）；只披露，不改变 review 阻断阈值。
- `extensions.redteam.axes[]` = 每轴 `{axis, ok, rows, error?}`；`template_hashes` 只列本次用到的。
- 报告不含 cost；任何失败路径（manifest 不可解析除外）都写出合法 `report.json`。

## 验收与证据

- `cargo test` / `make verify` 全绿；三轴 fake backend 单测；`en_US` 下无中文残留。
- 3.8：5 PR 的机读对照表 + 至少 1 个 PR 的安全轴丢弃率（>50% 先停手找协调者）。
- 3.9：真 triage 报告驱动 review 的 2.3 用例。
- 出口：删除 triage 的 `kind_not_implemented` stub；verify stub 与逐条 `inconclusive` 保留。

## 不做

- 不做 Phase 4/5/7；不改 hibot 契约；不重建入库二进制；不把 findings 全量当 blockers。
