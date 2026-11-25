# redteam

模型无关的对抗式代码 / 文档评审工具。一条流水线：**风险路由 → 疑点扫描 → 深挖 → finding 报告（可选验真）**。
产出 `redteam` 二进制 + Claude Code / Codex 插件；**裸终端可用**是底线，功能落在二进制里而不是宿主的 prompt 编排。

## 它做什么

对一次改动（当前分支相对基线、或未提交的工作树改动）跑一轮对抗式评审：

1. **风险路由**：按改动形态给出主区（H0）与风险分，决定这轮值得花多少格。
2. **疑点扫描**：按引用图把改动分组装包，每组交给若干 scanner 模型并行扫，产出**疑点**（quote 逐字锚定到代码行）。
3. **深挖**：H0 区、scanner 疑点、scanner 提出的证据需求进同一条优先级队列，reviewer 模型按 provider 并发门消费，
   预算随风险分封顶、必挖集不截。**finding 只由深挖产出**——没配 reviewer 角色就退化成 scan-only（只给疑点索引）。
4. **验真（refute，缺省关）**：finding 落地即可派另一个模型复查；被驳回的不计数、不拦路，但记录在案
   （finding 的 `refute` 字段）。`settings.review.refuters.enabled = true` 打开；没配 `refuter` 角色时自动挑异族模型。

退出码：`0` 干净 / `2` approve 带债 / `3` incomplete（扫描有结构性缺口）/ `10` reject / `1` 命令自身失败。

## 角色

配置在 `~/.redteam/config.json` 的 `settings` 下按角色声明模型池——池里的模型按声明顺序轮转
（长度 1 合法；没有候补链、没有失败回退）：

| 角色 | 池 | 用途 |
|---|---|---|
| `scanner` | `settings.review.roles.scanner.models[]` | 疑点扫描（review 必需） |
| `reviewer` | `settings.review.roles.reviewer.models[]` | 深挖、产出 finding |
| `refuter` | `settings.review.roles.refuter.models[]` | 评审内 finding 验真（可选；`settings.review.refuters.enabled` 打开后生效） |
| `verifier` | `settings.verify.models[]` | 修复验证（独立管线，review 不使用；**不再回退 reviewer**） |
| `triager` | `settings.triage.models[]` | hibot `triage` kind（独立管线） |

条目形状：`{ "provider": "<providers 键>", "id": "<models.id>", "reasoningEffort": "high" }`。
模型固有属性（`api`/`baseUrl`/`family`/`contextWindow`/…）写在 `providers.<name>.models[]`；
`language` 在 `settings.language`。v2 是破坏性重排，旧 v1 配置会报错并逐字段给迁移提示。

## 快速开始

```sh
redteam init                                                   # 项目内建 .redteam/ 骨架
redteam model setup --provider X --model <id> --role scanner   # 至少配一个 scanner
redteam review --base main                                     # 评审当前分支相对 main 的改动
```

配置是**用户级唯一**的 `~/.redteam/config.json`（`REDTEAM_HOME` 可覆盖），密钥另放 `~/.redteam/secrets.json`，
项目层没有配置文件。完整的旗标与语义以 `redteam --help` 与各子命令的 `--help` 为准——那是裸终端用户唯一的在场文档，
与代码同权重维护。

## 插件

`plugins/redteam/` 是 Claude Code / Codex 插件：`bin/redteam` 是 POSIX sh launcher（按 `<os>/<arch>` 找二进制），
`commands/` 是命令壳层（真相源），`codex-skills/` 是它的镜像，由 `make gen` 生成。
`make verify` 会重新生成并要求零 diff，防两边漂移。

## 安装（standalone CLI）

首期仅 **macOS arm64**：

```sh
curl -fsSL https://github.com/itcook/redteam/releases/latest/download/install.sh | sh
redteam update --check     # 只检查；redteam update 升级到最新正式版
```

安装脚本会校验 `SHA256SUMS`、原子替换旧版本，并拒绝非 macOS arm64 平台。
插件渠道（下面的 `plugins/redteam/`）走 marketplace / `git pull`，不用 `install.sh`；
`redteam update` 在插件目录下默认拒写（`--force` 才覆盖）。

## 从源码构建

```sh
make build     # 编译 darwin-arm64 release 并放到 plugins/redteam/bin/darwin/aarch64/redteam
make gen       # 从 commands/ 重生成 codex-skills/
make verify    # gen 后要求零 diff
cargo test     # lib + bin + e2e + packaging
```

二进制随仓分发（marketplace 要它），所以 `make build` 之后工作树会有 5MB 级 diff，按仓内惯例单独提交（`build: 重建二进制`）。

## 文档

`redteam --help` 是用户面的唯一真相源。设计与决策记录在仓内 `.next/`（维护者工作区，随分支走）。

### 历史：回退到 M21 之前的引擎

M21c（2026-09-28）把 M21 重构分支的产物切名成 `redteam`，覆盖了原先入库的 dev 二进制。dev 版仍在 git 历史里，
要回退只需取回那一份：

```sh
git show feebc92:plugins/redteam/bin/darwin/aarch64/redteam > plugins/redteam/bin/darwin/aarch64/redteam
chmod +x plugins/redteam/bin/darwin/aarch64/redteam
```

M21 重构期间（2026-09-05 ~ 09-28）跑过分支产物的项目，库名是 `.redteam/proj-next.db`。
新引擎开项目库时会**自动接管**它（改名为 `proj.db`，连同 `-wal` / `-shm`），作业历史与台账随之继承；
两个库都在时一个都不动——那种情况得你自己决定留哪个。

注意两版的项目库不互通：新引擎用 schema v4 的 `.redteam/proj.db`，dev 版读不了。
新引擎首次打开旧库时会**一次性迁移**（台账带走、评审史丢弃），动手前自动留一份整库快照 `proj-legacy-<日期>.db`
——回退到 dev 版时把那份快照改回 `proj.db` 即可。
