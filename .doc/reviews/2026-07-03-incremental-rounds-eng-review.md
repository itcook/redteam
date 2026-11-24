# 轮次增量协议设计 · 工程评审记录（plan-eng-review + codex 双评审）

2026-07-03。评审对象：`.doc/design/2026-07-03-redteam-incremental-rounds-design.md`
（grilling Q1-Q8 定稿版）。方法：plan-eng-review 逐条对照 src/ 实证设计断言 +
codex（gpt-5.5, read-only, reasoning=high）独立读码评审作 outside voice。
**结论：6 项加固决议（3 P1 / 3 P2），全部并入设计文档决议正文；零 cross-model tension。**
本文保存评审的**过程、依据与产物**；最终决策以设计文档为准。

---

## 1. 评审定位

grilling 已覆盖协议逻辑（approve 语义/邻域/携带规则/契约/停机/CLI/验收），但设计的
**代码库假设**（"positioning 可复用重锚定""proj.db 已持久 findings""job 表加列"等）
未经独立核实——本轮评审即补这道检查。核实读码范围：positioning/mod.rs、db.rs、
state/{store,job}.rs、cli.rs（assemble_report/run_code_review/run_review_dispatch/
partial_report）、domain/report.rs、verdict.rs、orchestrate/mod.rs（enrich/id）、
evidence/git.rs、ship.md。

**代码断言核实结果**（设计原文 vs 现实）：
- ✓ 「r1 findings 已在 proj.db 不用重跑」——results 表持久 result JSON（db.rs:84-89,278-295），
  单跳回放可行。
- ✓ 「positioning 可复用重锚定」——anchor() 机制成立（positioning/mod.rs:103-120），
  但需两个变体：报告丢弃 id（carried_dropped 审计）+ 多命中就近选（R4）。
- ✗ 「proj.db job 表加列 stamp head_sha」——JobState 是整列 JSON blob 非 SQL 列；
  且该落点本身错（R3 推翻，改 RunMeta）。
- ✗ 「同 quote+title → 同 id」（决议 4 的 resolution_disputed 碰撞机制前提）——id 含
  file（orchestrate/mod.rs:63-67），rename 下不成立（R1）。

## 2. 加固决议全记录（R1-R6，含依据与用户裁决）

### R1（P1）rename 击穿锚定不变式 —— 用户选 A：delta 取 rename 映射，carry 迁 file/id

- **问题**：`anchor` 按 `finding.file` 旧路径读当前树（positioning/mod.rs:106-108）；
  finding id = hash(file+quote+title)（orchestrate/mod.rs:63-67）。纯 rename（内容不改）：
  旧路径读不到 → carry 进 carried_dropped 被杀，而 `git diff` 对纯 rename 只给 rename
  元数据、不重现原问题 → **真问题静默蒸发**（比误报危险：不可见）。rename 后同问题新 id
  → resolved/dismissed/disputed 的 id 碰撞机制全失效。锚定不变式在 rename 下不成立——
  grilling 完全漏掉；codex #1/#2 独立同根命中。
- **选项**：A) `git diff --name-status -M` 旧→新映射 + carry 迁 file/id（选定）；
  B) 全树 quote 搜索兼容（贵且易误配，id 问题仍在）；C) rename 触发全量终扫（重构轮退化
  全量=省不下钱）。
- **决议**：A。delta 计算取 rename 映射；carry 重锚前迁 `finding.file` 并迁移/重算 id。

### R2（P1）Incomplete/半失败轮不能当 carry 基线 —— 用户选 A：健康基线守卫

- **问题**：锚定不变式默认上轮"干净全量审"。两种不干净：① partial
  （Verdict::Incomplete，cli.rs:599——未跑 bundle 的 findings 压根不存在，拿它当基线 =
  那片代码零覆盖却视作已清）；② 半失败（failed_models 非空 → verdict 仍可 approve 但
  exit=2，cli.rs:297-298——对抗火力不足）。原"同族守卫"只查 mode/scope。codex #5/#6
  独立命中。
- **选项**：A) 硬拒 incomplete + failed_models 非空 + manifest 未全 ok（选定）；
  B) 只拒 incomplete；C) 不堵仅注记。
- **决议**：A。`--incremental-from` 载入上轮 result 后三查，违反显式报错建议全量。

### R3（P1）HEAD stamp 权威时刻与落点 —— 用户选 A：权威=证据 HEAD，写 RunMeta

- **问题**：后台 job 时序 = main.rs 先写 args/state 再 spawn worker，worker 内才
  collect diff（cli.rs:517）。job 创建时刻与证据收集时刻的 HEAD 可能不同（用户轮间
  commit 合法）→ stamp 记错则下轮 delta base 偏移（重审或漏审一段）。且设计原文
  "job 表加列"落点错：JobState 前台裸 review 没有、JobCellSink 并发 read-modify-write
  脆（cli.rs:542-578）、与 findings 异事务。codex #7 独立命中。
- **选项**：A) 权威=changed_files 实跑时刻的 rev-parse HEAD，stamp 写 result JSON 的
  RunMeta.head_sha（选定）；B) 写 JobState（设计原文）；C) 两处都写（双真相源否决）。
- **决议**：A。与 findings 同产物同事务，前后台统一；`--incremental-from` 读上轮
  result 取 base。serde default 兼容 v2，零 SQL 迁移。

### R4（P2）carry 重锚多命中错锁 —— 用户选 A：旧行号就近选

- **问题**：locate_quote 取首个命中（positioning/mod.rs:37-40）。全量审无害（模型每轮
  重判），carry 语义下成正确性问题：fix 改掉 foo.rs:50 的 `x.unwrap()`，但 :20 另有
  无辜同文——重锚命中 :20 → finding 错误存活指向无辜行 → 幽灵 finding 阻 approve。
  另 normalize_ws 折叠空白（:6-9），quote 行 whitespace-only 语义变化仍算锚中（codex
  #3/#4）——后者判 P3 不堵（见 §5 遗留）。
- **选项**：A) carry 记上轮 line_start，多命中就近选（选定）；B) 歧义即不 carry（真在
  delta 外的被错杀）；C) 接受首匹配。
- **决议**：A。carry 专用 locate 变体，单一命中零影响。

### R5（P2）status carried/new 列无数据源 —— 用户选 A：砍

- **问题**：status 只读 JobState（main.rs job_all_states 路径），carried/new 是 finding
  级终态语义，住 ReviewReport.findings（result JSON）。要实现须终态回填 JobState（进度
  视图背终态语义 + 拖脏并发面）或 status 加查 results。codex #8 独立命中。
- **决议**：砍列（YAGNI + 关注点分离）。消费方读 result JSON / rN.md。

### R6（P2）feedback jq v2 兼容 —— 直接定（无分歧）

- **问题**：记分卡 raised 改 new-only，jq 若写 `.origin == "new"` 则 v2 老报告（无
  origin 字段）全掉或混算（feedback.md:38-52）。codex #9 命中。
- **决议**：`(.origin // "new") == "new"`——缺省视 new。resolve 独立表（决议 3 已定）
  经核实无 codex #10 担忧的豁免污染：active_cards（db.rs:182-198）只捞 dismissed
  活跃态，resolved 不会被当误报豁免 refute。

## 3. codex 外部评审处置表（10 漏点）

| # | codex 发现 | 处置 |
|---|---|---|
| 1 | 「同 quote+title→同 id」为假（id 含 file） | **采纳 → R1** |
| 2 | rename 静默杀 carry | **采纳 → R1** |
| 3 | quote 非唯一身份，重锚可错锁同文件另一处 | **采纳 → R4** |
| 4 | normalize_ws 过强，whitespace 语义变化仍算锚中 | **部分采纳 → R4 P3 备注**（live 观察再定） |
| 5 | Incomplete 不能作 carry 基线 | **采纳 → R2** |
| 6 | verdict=approve + exit=2 半失败被 ship 当 approve | **拆两半**：作 carry 基线的一半 → R2 堵；ship.md 停机条件本身的加固 = 既有协议问题（非增量引入）→ 独立顺手项入 `.doc/TODO.md`，不进本批（一批不混两个协议变更） |
| 7 | HEAD stamp 权威时刻未定义、JobState 落点并发脆 | **采纳 → R3** |
| 8 | status carried/new 列无落地路径 | **采纳 → R5** |
| 9 | schema v3 打断 feedback jq 历史口径 | **采纳 → R6** |
| 10 | resolve 表生命周期与 dismissed 豁免混淆风险 | **设计已覆盖**（决议 3 分表 + active_cards 只捞 dismissed 态），核实确认无虞 |

**cross-model tension：零**。plan-eng-review 先命中 incomplete/stamp/status 列，codex
补出 rename/quote 精度根——互补一致，强共识。

## 4. 测试覆盖 diagram（评审产出，实施对照用）

```
新代码路径                                          触发/边界
[+] cli.rs run_incremental_review()                 增量流水线主入口
  ├── 载上轮 result(RunMeta.head_sha/findings)      ├── [必测] 上轮 incomplete → 拒(R2)
  │                                                  ├── [必测] failed_models 非空 → 拒(R2)
  │                                                  └── [必测] 上轮无 result → 拒
  ├── 谱系守卫 merge-base --is-ancestor             ├── [必测] rebase/reset → 拒
  ├── 同族守卫 mode/scope 一致                       └── [必测] mode/scope 变 → 拒
  ├── delta = diff(head_sha..HEAD) + rename 映射     ├── [必测] 空 delta → empty 错(交停滞守卫)
  │                                                  └── [必测·R1] 纯 rename → file/id 迁移不丢
  ├── carry 重锚(上轮合并集 → 当前树)               ├── [必测] quote 消失 → carried_dropped
  │                                                  ├── [必测·R4] 同 quote 多处 → 就近旧行
  │                                                  └── [必测] resolved id → 排除携带
  ├── merge(carry ∪ new) 去重                       ├── [必测] 同 id 再报 → 保 carried,不涨 agreement
  │                                                  └── [必测] resolved+quote 存活 → resolution_disputed
  ├── refute(仅 origin=new)                          └── [必测] carry 项不进 refute targets
  └── verdict(合并集) + schema v3 报告               ├── [必测] origin 字段 + 聚合数组
                                                     └── [必测·R6] feedback jq v2 兼容

[+] ship.md 增量循环                                 协议/prompt
  ├── r1 全量 → r2..rn --incremental-from            ├── [→dogfood] 单跳回放(验收 §8.1)
  ├── 属实→修→resolve 标记                           ├── [→dogfood] 假修复靶 → disputed(§8.2)
  ├── 停机: approve 主出口 / 尾流守卫退役            ├── [→dogfood] 争议二次 → 停机(§8.2)
  └── 销案争议刹车(同 id 二次 disputed)              └── [→live] 全程增量 ≥50% 降(§8.3)

覆盖策略: 机械正确性=hermetic 单测(R1/R2/R4 全覆盖)；协议收敛性=dogfood 单跳回放+合成靶；
成本效果=live 收编。
性能: carry 重锚 = O(上轮 findings 数 × 文件读)，量级 10-15 findings/轮、单读 512KB 帽
——十几次本地读换省百万 token；无新增 DB 查询热点。
```

## 5. 失败模式（每新码路径一例）

| 码路径 | 生产失败 | 有测试 | 有错误处理 | 用户可见 |
|---|---|---|---|---|
| rename carry 迁移 | rename 未被 -M 识别（相似度<50%）→ carry 丢 | T1 | 降级 carried_dropped（非崩） | 是（id 可审） |
| 健康基线守卫 | 上轮 result 读取失败 → 误判可增量 | T2 | 显式报错拒跑 | 是 |
| stamp | 证据收集与 rev-parse 间树变（极窄窗） | T3 | base 偏移→重审浪费非漏审 | 部分 |
| carry 重锚就近 | 旧行号大幅漂移 → 就近仍歧义 | T4 | 退 carried_dropped（保守） | 是 |
| 合并去重 | carry 与 new 同 id 但 quote 已变 | T5 | 保 carried（决议 5） | 否（live 观察） |

**关键 gap 判定**：无「无测试 AND 无错误处理 AND 静默」组合——R1/R2 原为静默失败，
补测试 + carried_dropped 可审后转为可观测。

## 6. Implementation Tasks（P1 阻塞交付 / P2 同批落地）

- [ ] **T1 (P1)** rename 感知 carry 迁移——evidence/git.rs（`--name-status -M` 映射）、
  carry 模块（迁 file）、id 迁移。Verify：纯 rename 内容不变 → carry 存活指新路径、id 一致。
- [ ] **T2 (P1)** 健康基线守卫——cli.rs `--incremental-from` 载入后三查。
  Verify：三种不健康基线各被显式拒 + 文案建议全量。
- [ ] **T3 (P1)** stamp 权威落点——report.rs RunMeta+head_sha（serde default）、cli.rs
  changed_files 处 rev-parse 回填。Verify：前台裸 review 与后台 job 同树同 sha。
- [ ] **T4 (P2)** carry 重锚多命中就近选——positioning carry 专用 locate（带旧 line_start）。
  Verify：同 quote 两处选最近旧行；单命中零变化。
- [ ] **T5 (P2)** schema v3 + origin + 聚合数组——report.rs/finding.rs/cli.rs。
  Verify：v3 序列化 + v2 反序列化兼容（origin 缺省 new）。
- [ ] **T6 (P2)** feedback jq v2 兼容——feedback.md 配方 `(.origin // "new")`。
  Verify：v2/v3 fixture 各跑，raised 口径正确。
- [ ] **T7 (P2)** status carried/new 列不实现（纯决议订正，无代码动作）。

## 7. What already exists（复用清单，反重建）

- carry 重锚 = 复用 `positioning::anchor`（quote 定位/软换行退路/符号链接防护全现成），
  仅加 carry 专用「多命中就近」变体。
- stamp 存储 = 复用 result JSON（RunMeta 加字段，serde default，零 SQL 迁移——results
  表存整列 JSON）。
- resolve 台账 = 复用 db.rs dismiss 表范式（独立表/upsert/history/busy_timeout）。
- 健康基线守卫 = 复用既有 Verdict::Incomplete / failed_models / manifest 语义，纯读判。
- delta 计算 = 复用 evidence::git 封装，加 `--name-status -M` 一个命令。
- 合并集 verdict = compute_verdict 签名不动。

## 8. 并行化分析

批①②③④共享 src/cli.rs（增量主入口+守卫+装配同文件）+ report.rs（schema v3）——文件级
高耦合，**主线顺序实施**；唯一独立支线 = T6 feedback.md（plugins/ 壳层与 src/ 无交集）。
收益有限，单 worktree 顺序推进即可。

## GSTACK REVIEW REPORT

| Review | Trigger | Why | Runs | Status | Findings |
|--------|---------|-----|------|--------|----------|
| CEO Review | `/plan-ceo-review` | Scope & strategy | 0 | — | — |
| Eng Review | `/plan-eng-review` | Architecture & tests (required) | 1 | issues_open | 6 issues (3 P1 / 3 P2), 0 critical silent gaps |
| Design Review | `/plan-design-review` | UI/UX gaps | 0 | — | n/a（CLI/协议，无 UI） |
| Outside Voice | codex (gpt-5.5) | Independent 2nd opinion | 1 | issues_found | 10 漏点：8 采纳/1 设计已覆盖/1 拆分归顺手项 |

- **CODEX:** 独立读码 10 漏点与本轮 6 决议高度重合；处置见 §3。
- **CROSS-MODEL:** 零 tension，互补一致（rename/quote 精度根 vs incomplete/stamp），强共识。
- **VERDICT:** ENG 审毕——6 决议已 fold 入设计文档决议正文（覆盖/修正原决议 2/3/5/7）；
  3 P1（rename/健康基线/stamp）为交付阻塞，须在实施批①②③落地。设计可进 plan 拆批。

**UNRESOLVED DECISIONS:**
- whitespace-only 语义变化（quote 行被改但 normalize_ws 折叠后仍锚中，R4 P3 尾巴）：
  列 NOT-in-scope，live 观察真出现再定——非阻塞；实施期 dogfood 撞见需回补。
