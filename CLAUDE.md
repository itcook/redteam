# redteam

模型无关的对抗式代码/文档评审工具。Rust 单 crate,产出 `redteam` 二进制 + Claude Code/Codex 插件。

## 命名(2026-07-02 更名,勿用旧名)

产品/crate/二进制/插件/斜杠空间统一为 **redteam**;循环对抗技能为 **ship**(`/redteam:ship`)。
曾名 **breakpoint**(二进制 `brk`,循环技能 `lgtm`)——旧名已从代码与数据契约全域清除,
历史文档(`.doc/design/`、`.doc/plans/`)与 git 历史中的旧名**保留原样**,阅读时按此对照。
路径对照:`.breakpoint/`→`.redteam/`、`~/.config/breakpoint/`→`~/.config/redteam/`、
`BRK_*` 环境变量→`REDTEAM_*`、`fix(lgtm):` commit 前缀→`fix(ship):`。

## 设计原则:内置命令优先(2026-07-02 定)

**裸终端可用**是底线:功能尽量落进 `redteam` 二进制,而非 prompt 编排——减少宿主 prompt
编排带来的不确定性。宿主(Claude Code/Codex)集成面 = 调命令,commands/*.md 壳层保持薄透传。
`--help` 是裸终端用户唯一的在场文档,与代码同权重维护。判断新功能落点时:能内置就内置,
prompt 只做裁决/编排等模型本职(例外:dev-time 临时仪器如 feedback.md,发布前拆除)。

## 常用

- **M21 评审引擎重构期间(2026-09-05 起,分支 `feat/m21-review-engine`)**:以仓内 **`.next/`**(分支随行文档区,2026-09-07 自主
  checkout `.doc/` 迁入,`.next/README.md` 为索引)为准——`.next/roadmap/`(README + P0 + M21a-e + followup)、
  `.next/design/2026-09-04-redteam-review-engine-architecture-design.md`(v8 锁定)及同名 decisions(D1-D11)/ research;
  **`.next/private/`(gitignored,只在本工作树磁盘)**放含公司仓细节的 PR181 评审记录、P0 明细研究与原始数据——远端是公开仓,**永不入库**。
  `.doc/` 其余 design/plan 为历史,读到冲突以前者为准。分支硬切旧编排(无 `engine` 旗),对照基线 = `dev` 二进制;
  切名前分支产物名 `redteam-next`。
- 挑活先读 `.doc/roadmap/summary.md`(项目级总览:五支柱账目+里程碑表;明细在同目录 done.md/followup.md);
  零散项在 `.doc/TODO/summary.md`(编号 T01+ 总览表,明细同目录)。注:`.doc/` 是
  维护者私有工作区(gitignored,不随仓分发)——外部克隆者没有这些文件,以 `redteam --help` 为准。
- `make build` 编译并放置插件二进制;`make gen` 从 `plugins/redteam/commands/`(真相源)重生成
  codex-skills;`make verify` 防漂移——它先重新 gen 再要求零 diff,故 commands 与镜像不同步时**任何时刻**跑都会红
  (含已入库的漂移);唯一假红:本批 commands 改动尚未 commit 时,gen 产物与 HEAD 有合法差异。
- 密钥在 `~/.redteam/secrets.json`(M21a〔D12〕起:配置 `~/.redteam/config.json` 用户级唯一、无项目层、严格 JSON;dev 二进制仍读旧 `~/.config/redteam/`),永不入仓、永不入主配置。

## 文档规范(.doc,2026-07-07 定)

`.doc/` 维护者私有(gitignored)。七区两形态:
- **账本区**(summary+followup+done 三联):`roadmap/`=项目级里程碑账;`TODO/`=零散 polish/债(T01+)。
- **制品区**(日期文件 `YYYY-MM-DD-<slug>-<kind>.md`,kind∈design/plan/research/decision/review):
  `design/`=子系统**定论/结果**(WHAT,读如 spec、无推理噪音);`decisions/`=**决策记录**(WHY:权衡/被否选项/
  推理过程,design 引它);`plans/`=对应 HOW(小步任务);`research/`=喂决策的实验/评测;`reviews/`=评审记录。
  slug=kebab+现名 redteam;日期=创建日、编辑不改名。
- **状态头**:每份 design/plan/research 顶部一行 `> 状态: <WIP|锁定|已交付|作废(→替代)> · <一句话>`。
- **KIND 不隐性漂移**:grill/brainstorm 流水日志是临时的;想法定型即毕业成 `-design.md`(锁定)、流水日志随之
  改名——不留永久 "notes" 角色。
- **design 与 decision 分离**:design 只写**定论**(读者当 spec 用、无推理噪音);为什么这么定、权衡了什么、否掉
  了什么 → `decisions/<...>-decision.md`(架构级=单份决策日志、分节记,非一决策一文件);design 各节引对应决策。
- **架构级设计(跨里程碑)**:一份 `-design.md`(锁定)=权威框架 → 派生 N 份里程碑 `-plan.md` → 里程碑登记进
  `roadmap/`;三者互链(design 列其 plan+里程碑、plan 列其 design、roadmap 条目列两者)。
