# M7 · Loop SKILL 就绪 + 循环对抗层（lgtm）设计

2026-07-01 · grilling 会话定稿（Q1-Q11 + 三处用户补充）。
上游：`.doc/design/2026-06-25-breakpoints-adversarial-review-design.md` §17、
`.doc/research/2026-06-30-live-review-operability-lessons.md` R8.1/R8.2/R7.2、roadmap M7。

## 0. 目标与成功标准

让 brk 具备被外层循环无人值守驱动的能力，并交付该循环本体——**lgtm** SKILL
（`/breakpoint:lgtm`）：反复「评审 → 裁决 → 修复/驳回 → 再评审」直到通过。

**成功标准（一句话）**：一次 `/breakpoint:lgtm` 调用在无人值守下把一个有缺陷的
分支/文档带到 `verdict = approve`，全程零手工回喂——19 轮实测经验中 P-8
（跨轮驳回手工回喂）的人肉环节归零。

**范围**：一个 spec、两个实施批次：
- **批次 1（brk 原语，Rust）**：R8.1 id 换代 + R8.2 `--dismissed` + 穷尽性指令内建
  + `.breakpoint/` 目录迁移。
- **批次 2（lgtm SKILL，零 Rust）**：`commands/lgtm.md` 循环协议本体；R7.2 fuzz
  验证作为其中一条「验证配方」，**不是**独立工作项、不进二进制。

---

## 1. 批次 1 · brk 原语

### 1.1 R8.1 · finding id 换代（quote 锚定、行无关）

现状（`orchestrate/mod.rs::enrich`）：`fnv1a64_hex([file, line_start, line_end,
lowercase(title)])`——行号入哈希，fixup/代码位移即失效，驳回台账跨轮必碎。

**新组成**：

```
id = fnv1a64_hex([ file, normalize_quote(quote), normalize_title(title) ])
```

- `normalize_quote`：**去除全部空白字符（含换行），保留大小写**。rustfmt、缩进、
  换行重排不再影响 id；代码 token 序列即身份。
- `normalize_title`：lowercase + 空白折叠为单空格（现状 lowercase，补折叠）。
- `file`：原样（相对路径，现有产出即已归一）。
- **行号彻底移出哈希**。
- **退化路径**：`quote` 为空（模型未给）时退回
  `fnv1a64_hex([file, line_start, line_end, normalize_title(title)])`；
  文档注明「无 quote 的 finding 不保证跨 fixup 稳定」。
- **碰撞取态**：同 file + 同归一 title + 同归一 quote 的多处重复代码 → 同一 id，
  **接受合并**（同缺陷模式一起修/一起驳），不做 occurrence index（YAGNI）。
- **换代影响**：旧 id 全部失效。当前无任何持久化 id 消费方（dismissed 是本批次
  才引入），免费换代窗口。输出 `schema_version` **bump** 以示语义变更。
  `hash.rs` 的 FNV golden 测试不动（钉算法）；id 组成的单测/golden 随迁。

### 1.2 R8.2 · `--dismissed` 结构化驳回

**flag 形态**：`brk review --dismissed <csv|dir>`（resume 沿用 job 参数自然继承）。
- **csv 模式**：逗号分隔 id，逐个校验 `^[0-9a-f]{16}$`，不合法即报错（exit 2）。
  供临时手用。
- **目录模式**：值为存在的目录时，id = 目录下匹配 `^[0-9a-f]{16}\.md$` 的**文件名**，
  **零内容解析**。这是 lgtm 的默认喂法（`.breakpoint/dismissed/`）。
- 探测顺序：先按路径试（存在且为目录 → 目录模式），否则按 csv 解析。
- 单文件行解析模式**不做**（目录协议取代之）。

**命中行为（三件套，「不影响裁决但保留证据」）**：
1. 命中 dismissed id 的 finding **保留**在 findings 中，标 `previously_dismissed: true`
   （serde skip-if-false）。
2. **verdict 计数豁免**：不参与 reject/needs-fix 判定——与既有 `verify == Refuted`
   的豁免同模式（`compute_verdict` 同一处扩展）。
3. `run` 块新增 `dismissed_reappeared: ["<id>", …]`（空则 skip），SKILL 读一个
   字段即可决策刹车。**权威源澄清**：findings 上的 `previously_dismissed` 是权威，
   `dismissed_reappeared` 是其派生便利索引（同一次 consolidate 生成，正常不可能
   不一致；万一分歧以 findings 为准）。

**明确不做**：dismissed 内容**不喂模型 prompt**（id 是单向哈希，brk 无法还原原
finding；引导语义走 `--focus`，由 SKILL 从台账生成摘要）。`--focus` 保留，分工：
`--dismissed` = 确定性后处理标记，`--focus` = 启发性引导。
（修正 roadmap 措辞：--dismissed「接管 --focus 的驳回回喂职责」，非整体替代。）

### 1.3 穷尽性指令内建（提示词自治红利）

**分界原则（写入设计约束）**：每轮**无条件**注入的指令 → brk 内建提示词；
轮特定/上下文特定内容 → `--focus`。

据此，原 until-codex-approve 时代靠外层注入的**穷尽性指令**收编进 brk 内建
fanout 模板（code + doc 两侧），一次性生效：

> 本轮报出你能发现的**全部** findings，禁止分轮挤牙膏；同一模式的多个实例，
> 要么逐一列出、要么按类报出并枚举全部变体。

（英文措辞实施时随现有模板语言风格落地；与 M6-a R7.1 类级 prompt 相邻但不越界——
本批次只加穷尽性指令，类级攻击面模板仍归 M6。）

**顺手项**：`cli.rs:179/276` 的陈旧注释与 bail 文案「--background is not yet
implemented (planned for Plan 2)」措辞失真（该守卫防的是 main.rs 分叉不变量，
功能早已实现）——批次 1 顺手改为如实描述（如 "--background forks in main.rs
before dispatch; reaching here is a routing bug"）。2026-07-01 spec dogfood 中
Kimi 据此误报 CRITICAL，是误导性注释的实证代价。

### 1.4 `.breakpoint/` 项目目录（文件组织收编）

```
.breakpoint/
  config.toml        # 项目级配置：原 .breakpoint.toml 硬切迁入
  .gitignore         # 内容仅一行 "*"（自屏蔽）
  dismissed/
    <id>.md          # 一条驳回一个文件；文件名即 id（brk 读）；内容为
                     # finding 快照（title/file/quote/severity）+ 驳回理由 +
                     # 裁决状态行（SKILL 写、人读，brk 不解析）：
                     #   dismissed(rN)           宿主初裁误报
                     #   re-upheld(rM)           再现后宿主复裁仍误报（每 id 至多一次）
                     #   human-confirmed(rK)     人已裁决误报 → 此后永久静默，不再刹车
  rounds/
    <YYYYMMDD-HHmmss>/   # 一次 lgtm 会话（本地时间，秒级防碰撞）
      r1.md …            # 每轮简报（SKILL 领地，brk 不感知）
      r1.result.json …   # 每轮报告原件存档
      session.md         # 收工总结
```

- **config 硬切**：`config_paths()[1]` 由 `.breakpoint.toml` 改为
  `.breakpoint/config.toml`，**不留旧路径 fallback**（pre-1.0 零外部用户，免费
  硬切窗口；与 id 换代同批）。防呆：读配置时若检测到旧 `.breakpoint.toml` 仍
  存在，stderr 打一行迁移提示（不读取其内容）；`brk init` 发现旧文件时提示手动
  迁入（不自动搬，避免静默改语义）。
- **`.gitignore` 内容 `*` 的副作用即特性**：rounds/、台账、config 默认全部不入库，
  「commit 仅含修复」自动成立。团队要共享台账/config 时用负向条目
  （`!dismissed/` 等）自行放行——spec 注明该技巧，默认不做。
- brk 读侧只感知 `config.toml`（配置）与 `dismissed/`（`--dismissed` 目标）；
  `rounds/` 纯 SKILL 领地。
- **骨架创建 = 新子命令 `brk init`（幂等）**：创建 `.breakpoint/{.gitignore(*),
  dismissed/, rounds/}` + 注释版 `config.toml` 模板；已存在的文件一律不覆盖。
  归属批次 1。责任链：用户可显式 `brk init`；**lgtm 首跑兜底调用**（幂等故安全）；
  `--dismissed` 指向不存在的目录时报错并提示 `brk init`。
  依据（2026-07-01 核查）：Claude Code 无安装时钩子（SessionStart 每会话每项目
  触发，用于建项目目录会污染所有仓库）；Codex 无任何 hook 机制——宿主机制不对称
  且不可靠，项目骨架由二进制自己认领，宿主无关（含未来 OpenCode/裸终端）。
- **secrets 是 non-goal**：项目级 secrets.toml **不设**。密钥永驻用户级
  `~/.config/breakpoint/secrets.toml`（chmod 600）；项目差异化走 `api_key_env`
  指项目专属环境变量。理由：项目目录是仓库领地，gitignore 只防 git 一条泄露路径
  （tar/rsync/docker COPY 均不认）。

---

## 2. 批次 2 · lgtm SKILL（`commands/lgtm.md`）

### 2.0 创作方式（非 coding 任务）

lgtm 本体是提示词工程产物：**通过 skill 创作技能（`superpowers:writing-skills` /
`skill-creator`）完成，不当作 coding 任务直接编写/派发**。批次 2 的「实施」=
按本 spec 的协议用上述技能起草、评审、打磨 `commands/lgtm.md`。

### 2.1 形态与参数

- `plugins/breakpoint/commands/lgtm.md`，frontmatter：`description` +
  `disable-model-invocation: true`（烧钱长循环，仅人手动触发，同 review 命令）。
- 调用：`/breakpoint:lgtm $ARGUMENTS`。**参数透传** `brk review`
  （`--mode/--base/--scope/--spec/--lenses/--models` 等，不重新发明参数面）。
- **lgtm 专属参数仅两个**：
  - `--max-rounds <N>`：可选预算护栏。**默认不设上限**——目标是 approve，硬编码
    上限与目标冲突；要消除的是打地鼠，不是跑得久。
  - `--dismissed-dir <path>`：默认 `.breakpoint/dismissed/`。
- `--resume`（续跑会话）：YAGNI 出局。崩了重开 session 目录成本近零，台账本就跨
  会话共享。
- **模式无关**：code/doc 同一循环协议；差别仅修复动作对象（改代码+跑测试 vs
  改文档+自查一致性）。doc 侧承接 M11 定位：对 spec 循环 =「审到能安全实施为止」。
- **双宿主**：commands/ 真相源 → `gen-codex-skills` 顺产 Codex 版（文件集同步
  测试自动覆盖新文件）；**实施时须把 `"lgtm"` 显式加入 `tests/packaging.rs` 的
  frontmatter/调用策略断言清单**（现清单硬编码 review/status/result），并断言
  `disable-model-invocation: true`。

### 2.2 循环协议

**起手**：
1. `brk init` 兜底骨架（幂等），再建本会话目录 `rounds/<YYYYMMDD-HHmmss>/`。
2. code 模式：先跑测试套件，**红树即停**（先修好基线再来，不拿坏基线喂评审）；
   doc 模式跳过。

**每轮 rN**：
1. 组装 `--focus`：从 `dismissed/*.md` 的 title+理由生成驳回摘要
   （「以下问题已人工裁决为误报，勿再报：…」）。这是刹车语义成立的前提——
   模型**被告知仍坚持**才是升级信号。
2. `brk review --background --dismissed <dir> --focus <摘要> <透传参数>`；
   轮询 `brk status <job>`（心跳 `last_active` 辨慢与死、phase 辨尾段与卡死；
   卡死 → `cancel` + 重起）。**轮询期间不动工作树**（working-tree scope 下评审在
   读活文件）。
3. 取 `result`，存档 `rounds/<session>/rN.result.json`。

**内循环（逐条裁决）**：
1. 分拣：`verify == refuted` 跳过（brk 已豁免，简报记一笔）；`previously_dismissed`
   按台账裁决状态分流——`human-confirmed` 或已 `re-upheld` 的静默跳过；仅
   `dismissed(rN)` 状态的首次再现进入刹车链（**每 id 至多走一次刹车链**）。
2. 排序：severity 降序为主序，同级按 file 分组。
3. 逐条（宿主读 quote + 文件上下文 + body）：
   - **属实 → 修**：root-cause 修法（共享函数一处守卫优于逐调用点补丁——打地鼠
     半个解药，r15 教训）。
   - **误报 → 写 `dismissed/<id>.md`**（快照 + 理由 + 轮次/日期）。
   - **存疑 → 先验证再定，禁止直接猜**：能执行就执行（微测试/复现脚本）；
     **绕过类/输入处理类强制走 R7.2 fuzz 配方**（步骤见下框）。
     验证后仍不可判 → **攒到轮末批量 AskUser**，不逐条打断。

   > **R7.2 fuzz 配方**（内嵌于本 spec，SKILL 创作时照抄语义）：
   > 触发：finding 属「输入处理绕过」类（编码/大小写/scheme/协议相对/路径遍历
   > 等变体族）。步骤：① 从 finding 提取被绕过的校验点与输入空间维度（编码层数、
   > 大小写、分隔符、前缀 scheme…）；② 对维度组合写 property 测试（Rust 项目用
   > `#[test]` 内联枚举生成即可，不引 proptest 等新依赖——YAGNI）；③ 跑测试
   > 枚举出**全部**失败变体，一次定性整类；④ 修复走类级（normalizer/单点归一），
   > 逐 case 补丁视为未完成；⑤ property 测试留在项目测试套件作回归资产。
   > 产出：一条 finding → 一类修复 + 一个回归测试，终结打地鼠（r12-r18 六轮
   > 变体拉锯的教训）。
   - 先验：`agreement ≥ 2`（多模型共识）加强属实先验；upheld 孤证正常裁决
     （verify 是意见不是判决）。

**收轮**：修复全落 → 运行项目测试命令至绿 → **单次 commit（仅修复）** → 写
`rN.md` 简报（job id、verdict、修复/驳回/存疑清单、commit sha）→ 下一轮。

**测试命令探测协议**（起手基线与收轮共用）：
1. `.breakpoint/config.toml` 的可选 `test_command` 显式配置（权威，init 模板中
   以注释示例）；
2. 无配置则按项目文件推断：`Cargo.toml`→`cargo test`、`package.json` 有
   `scripts.test`→`npm test`、`Makefile` 有 test 目标→`make test`、宿主项目
   记忆/CLAUDE.md 有约定则从之；
3. 推断不出 → AskUser 一次，答案记入本次 `session.md`（本会话内不再问）；
   明确无测试的项目跳过测试步。

### 2.3 停机集合（目标/异常驱动，无默认轮数上限）

停机判定在**每轮内循环完成后**进行——即使本轮 verdict=approve，若有 id 处于
待刹车状态，刹车链先走完（防止「approve 掩盖被错误驳回的真问题」）。

1. **`verdict = approve`** → 收工（唯一正常出口），写 `session.md` 总结。
2. **升级刹车链（每 id 至多一次，靠台账状态机去重）**：`dismissed_reappeared`
   中处于 `dismissed(rN)` 状态的 id（被告知仍坚持、且未复裁过）→ 宿主带新 finding
   body **复裁一次**（对照 `dismissed/<id>.md` 当时的驳回理由）→ 改判属实：修 +
   删台账文件；仍判误报：记 `re-upheld(rM)`，**停，AskUserQuestion 交人裁决**。
   人裁误报 → 记 `human-confirmed(rK)`，此后该 id 永久静默（仍喂 `--dismissed`、
   仍标记豁免，只是不再打扰人）；人裁属实 → 修 + 删台账文件，继续。
3. **修复受阻升级**：某 finding 属实但宿主修不了（需产品/架构决策、改动越权）→
   停，交用户。
4. **停滞守卫**：某轮结束仍有未豁免 findings、但本轮零代码改动（未修也未新驳）→
   循环空转，停，交用户。检测的是「无进展」而非「跑得久」。
   已知残余风险（接受）：振荡式进展（每轮都有改动但 finding 集合循环再现）不被
   本守卫捕获，靠交互会话中的人随时可中断兜底；如实测出现，届时再加集合环检测。
5. `--max-rounds` 达限（仅用户显式设置时存在）→ 停，输出未收敛摘要。

### 2.4 循环内的 brk 故障处理

- job 终态 `failed/timed_out` 但有部分报告（`verdict: incomplete`，R4.3 产物）→
  用部分 findings 照常裁决，简报注记 partial，**不算 approve**。
- job 硬失败无产出（全 provider 挂、auth 失效等）→ **重试一次**，再失败停下报告
  用户。基础设施故障不消耗循环轮次、不触发停滞守卫。

---

## 3. 数据契约变更汇总（批次 1）

| 项 | 变更 |
|---|---|
| finding `id` | 组成换代为 `[file, 去空白 quote, 归一 title]`（quote 空退化行号路径） |
| finding 新字段 | `previously_dismissed: bool`（skip-if-false） |
| `run` 块新字段 | `dismissed_reappeared: [String]`（skip-if-empty） |
| verdict 计数 | `previously_dismissed` 豁免（与 `verify==Refuted` 同处扩展） |
| `schema_version` | bump（id 语义变更 + 新字段） |
| CLI | `review` 增 `--dismissed <csv|dir>`；新子命令 `brk init`（幂等建 `.breakpoint/` 骨架）；`--focus` 语义不变 |
| 配置路径 | 项目层 `.breakpoint.toml` → `.breakpoint/config.toml`（硬切） |
| 内建 prompt | code/doc fanout 模板增穷尽性指令 |

## 4. 测试与验收

**批次 1**：
- 单测：id 组成/归一/退化/碰撞语义；`--dismissed` 三态解析（csv/dir/坏输入
  exit 2）；标记+豁免+聚合；schema_version 断言；config 新路径生效且旧路径不读；
  id 组成 golden 更新（FNV 算法 golden 不动）；`brk init` 幂等（重复执行不覆盖
  既有文件、缺失项补齐）。
- **端到端本证**：造已知 finding → 手建 `dismissed/<id>.md` → 在 quote 上方插行
  （制造位移）→ 重跑 → 断言同 id 再现、`previously_dismissed=true`、verdict 不受
  影响、`dismissed_reappeared` 含该 id。一条链同时证 R8.1+R8.2。

**批次 2（验收即 dogfood）**：
- (a) 收敛路径：种子缺陷分支（**≥3 个已知缺陷，至少 1 个为绕过类以踩 R7.2 配方**）
  上跑 `/breakpoint:lgtm` 至 approve；`rounds/<session>/` 工件齐全；commit 序列
  纯修复；预期 ≤3 轮收敛（观察参考，非硬性门槛）。
- (b) 刹车路径：预埋误报驳回并使其再现，验证复裁 → AskUser 刹车 → 人裁后正确续跑。
- (c) **self-hosting 靶**：lgtm 审 M7 批次 1 自己的分支，兼任批次 1 终审补充。

## 5. 非目标

- 项目级 secrets.toml（密钥永驻用户级）。
- `--resume` 会话续跑。
- id occurrence index（重复代码碰撞接受合并）。
- dismissed 内容喂模型 prompt（引导走 --focus 摘要）。
- R7.2 进二进制（它是 lgtm 内的验证配方；R6 执行级 verify 维持驳回）。
- 反向推导等设计方法论（归外部设计技能域，M11 已划界，不进 brk 任何层）。
- R7.1 类级攻击面模板（归 M6-a）。

## 6. 与 roadmap 的关系

- 本 spec 落地即 M7 全项（R8.1/R8.2/R7.2/capstone SKILL）。
- roadmap M7 措辞修正：R8.2「替代 prose --focus」→「接管 --focus 的驳回回喂职责，
  --focus 保留为启发性引导通道」。
- 相邻不越界：穷尽性指令入内建（本批）vs 类级模板（M6-a）；doc 模式循环适用
  （本批）vs doc 守卫视角打磨（M11）；lgtm 命令名与 M5 命名体系联动（届时统一重审）。
