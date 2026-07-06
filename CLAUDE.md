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

- 挑活先读 `.doc/roadmap/summary.md`(项目级总览:五支柱账目+里程碑表;明细在同目录 done.md/followup.md);
  零散项在 `.doc/TODO/summary.md`(编号 T01+ 总览表,明细同目录)。注:`.doc/` 是
  维护者私有工作区(gitignored,不随仓分发)——外部克隆者没有这些文件,以 `redteam --help` 为准。
- `make build` 编译并放置插件二进制;`make gen` 从 `plugins/redteam/commands/`(真相源)重生成
  codex-skills;`make verify` 防漂移——它先重新 gen 再要求零 diff,故 commands 与镜像不同步时**任何时刻**跑都会红
  (含已入库的漂移);唯一假红:本批 commands 改动尚未 commit 时,gen 产物与 HEAD 有合法差异。
- 密钥在 `~/.config/redteam/secrets.toml`,永不入仓、永不入主配置。
