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
   （finding 的 `refute` 字段）。`settings.review.refuters.enabled = true` 打开；本地模式没配 `refuter` 池时自动挑异族模型（job 模式跳过并披露）。

退出码：`0` 干净 / `2` approve 带债 / `3` incomplete（扫描有结构性缺口）/ `10` reject / `1` 命令自身失败。

## 作为 hibot 执行单元（`--job-dir`）

`redteam triage|review|verify --job-dir <dir>` 按契约读 `job.json` / `inputs`、写 `out/report.json`
（作业模式不读 stdin、不依赖 cwd、不碰 `~/.redteam`）。三 kind 的实现口径：

- **triage**：三轴（security / drift / intent）并发；产出 `summary` + findings + `blockers`。
- **review**：扫描 → 深挖 → 可选验真；finding 为唯一产出。
- **verify**（修复核验）：**只核验 anchored finding**（file + quote + line）的修复——即 review 的 blocking
  findings。triage 的修复不走 verify：每个 head 重跑 triage（`flow.md:18`）；intent claim 没有代码锚，
  `verifications[]` 只能按指纹挂，塞进来只会造假锚点。若 hibot 将来把 triage 的结构性项导出为 findings，
  同一管道可用——**指纹只从输入文件取**，执行单元不从其它字段复算或映射。

verify 的执行形态：
- **两阶段**：prepare（同步装配证据/prompt/规模预算）→ dispatch（只发 LLM）。证据 = 该文件 hunk 块 +
  跨文件 related hunks + **变更符号的调用点段（B-lite）** + post-fix ±60 行快照 + 可选 `prior_report` 佐证。
- **并发**：`buffer_unordered(k)`，`k = min(Σ providers.<p>.maxConcurrency, n)`；provider 门是硬上限。
  墙钟只记录（`extensions.redteam.verify.latency_ms`），不设 KPI。
- **预算**：单条按规模 `clamp((60 + 4×kb)×1.15, 60, 240)s`（乘 manifest 系数后 ≤300s，再与模型 per-turn、
  剩余墙钟取 min）；整体只有 `limits.wall_clock_s`。
- **四态**：`resolved / unresolved / regressed / inconclusive`，证据必填；quote 先查材料、再查整个 post-fix
  检出（严格字面重锚），都不命中才降级；模型失败/无法定位/证据不足 → `inconclusive`（**绝不塞进
  `unresolved`**），出现即整体 `partial` + `status_reason`。解析失败只补**一次** repair 轮。
- **真实夹具**：`tests/fixtures/verify/pr400/`（`hiapi-backend#400` 的三组 fix→verify，带人工判决）+
  `scripts/verify-pr400.sh`；**不进 `cargo test` 门禁**，脚本手动跑并对拍。

### 与本地增量轮的「修复」口径差异

本地 `--incremental-from` 的按轮携带（`src/consolidate/carry.rs`）用**确定性**规则判「修复」：
rename 迁移 → 已 resolve 的排除 → quote 锚得上就携带（行号重锚），锚不上就视为已修；人说修了但 quote 仍在
记为「⚠️ Disputed」。**job 模式的 verify 不用 carry**：它按逐条 finding 让模型看证据判四态。
两套口径并存是有意的：本地增量轮要的是「快、可复现的携带」，hibot 复验要的是「对单条 finding 的可解释判决」。
真实数据（#400）攒够之前不做统一。

### 配置投递、凭据与语言
job 模式（`config_delivery: job_native`）只读 manifest 的 `paths.config`（契约固定 `config/executor.redteam.json`）：
**没有 `--config` flag**，也不读 `~/.redteam/config.json`、不读 stdin、不依赖 cwd、不写 `~/.redteam`。
投递前可自检：`redteam config check <file> --kind triage|review|verify`（形状 + 未知键 + 明文密钥 + 角色最小集）。
- **凭据只走环境变量**：配置里只写 `apiKeyEnv`（模型条目可覆盖 provider 级），job 模式**不读** `secrets.json`；
  配置含 `apiKey` 一类明文字段会在 `config check` 与 job 入口被拒（`config_contains_secret:<pointer>`）。
- **报告语言**：`settings.language`（`zh_CN` / `en_US`，缺省 `en_US`，**不读 `LANG`**）决定 prompt 里的语言指令、
  `status_reason` 与 verify 证据等人读文本；机器字段（`status_code` / `kind` / `verify.counts` / `template_hashes` /
  `warnings`）不随语言。

### 输出与退出码
- 只写 `paths.out`：`report.json`（原子写，**终态唯一真相**）+ `state.json`（可选心跳，每 2s 一帧覆写；
  `phase ∈ preparing|running|finishing`，**不写终态**；`llm.in_flight` / `progress` 从运行态现读，缺失视为允许）。
- 退出码（本地调试便利，hibot 不读）：`0` = `ok` / `partial` 报告已写出；`1` = `error` 报告已写出；
  `2` = `job.json` 定位不到或不可解析（**不写报告**，runner 按 no-report=error）。

### 临时限制（v1）
- **runner 真调尚未接入**（hibot M0/M1 未开始）：当前验收走手工 job 目录联调 + `scripts/acceptance-matrix.sh`
  （3 kind × 3 夹具 × 2 语言 = 18 组合，`HIBOT_SCHEMA_DIR` 存在时逐份过 hibot 契约 schema）。
- `verify` 只核验**有代码锚**的 finding（triage 的 intent 项没有代码锚，不进 verify）；不产 finding。
- job 模式开验真但未声明 `refuter` 时**跳过并披露**（`extensions.redteam.refute_skipped = "no_refuter"`），
  不静默、也不塞 `refuted`。
- 报告**不含 cost**（`usage` 只有 token 与墙钟；成本统计归 hibot 侧）。

## 角色

配置在 `~/.redteam/config.json` 的 `settings` 下按角色声明模型池——池里的模型按声明顺序轮转
（长度 1 合法；没有候补链、没有失败回退）：

| 角色 | 池 | 用途 |
|---|---|---|
| `scanner` | `settings.review.roles.scanner.models[]` | 疑点扫描（review 必需） |
| `reviewer` | `settings.review.roles.reviewer.models[]` | 深挖、产出 finding |
| `refuter` | `settings.review.roles.refuter.models[]` | 评审内 finding 验真（可选；`settings.review.refuters.enabled` 打开后生效） |
| `verifier` | `settings.verify.models[]` | 修复验证（独立管线，review 不使用；**不再回退 reviewer**） |
| `triage` | `settings.triage.models[]` | hibot `triage` kind（独立管线） |

条目形状：`{ "provider": "<providers 键>", "id": "<models.id>", "reasoningEffort": "high" }`。
provider 级通道写在 `providers.<name>`（`api` / `baseUrl` / `apiKeyEnv` / `maxConcurrency`）；
模型固有属性（`id`/`family`/`contextWindow`/`maxOutputTokens`/…）写在 `providers.<name>.models[]`；
模型条目可选覆盖 `api`（有效 api = `models[].api ?? provider.api`，同一 provider 可混多种 api；
`baseUrl` 仍在 provider 级）；`language` 在 `settings.language`。v2 是破坏性重排，旧 v1 配置会报错并逐字段给迁移提示。

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
curl -fsSL https://release.hibot.sh/executor/audit/redteam/install.sh | sh
redteam update --check     # 只检查；redteam update 升级到最新正式版
```

安装脚本会校验 `SHA256SUMS`、原子替换旧版本，并拒绝非 macOS arm64 平台。
安装与自更新默认都走 R2 镜像（`https://release.hibot.sh/executor/audit/redteam`），
GitHub Release 仅作为发布备源保留（需要时 `--from github`）。
插件渠道（下面的 `plugins/redteam/`）走 marketplace / `git pull`，不用 `install.sh`；
`redteam update` 在插件目录下默认拒写（`--force` 才覆盖）。

## 发布（维护者）

发布由 CI 驱动：推一个 `vX.Y.Z` tag → `.github/workflows/release.yml` 在 macOS arm64 上构建、
发 GitHub Release，并把产物镜像到 R2（<https://release.hibot.sh/executor/audit/redteam>）。

```sh
# 1) 版本准备：改 Cargo.toml（唯一真相）→ make gen → 更新 CHANGELOG
# 2) 本地预演（零副作用，产物在 dist/release/）
scripts/release.sh v0.1.0 --prepare-only
# 3) 提交后打 tag 并推送（这一步会触发发布）
git tag v0.1.0 && git push origin v0.1.0
```

R2 布局：`v{version}/{asset}`、`v{version}/SHA256SUMS`、`v{version}/config.schema.json`、
`install.sh`、`config.schema.json`、`latest`（仅正式版更新）。`scripts/release.sh v0.1.0` 默认只推 tag、
由 CI 发布；要在本机完成 Release + R2，用 `--local-publish`（需先停掉同 tag 的 Actions run）。
完整流程与 secrets/vars 见 `.doc/plans/2026-11-24-redteam-release-runbook.md`。

## 从源码构建

```sh
make build     # 编译 darwin-arm64 release 并放到 plugins/redteam/bin/darwin/aarch64/redteam
make gen       # 从 commands/ 重生成 codex-skills/
make verify    # gen 后要求零 diff
cargo test     # lib + bin + e2e + packaging
scripts/acceptance-matrix.sh   # hibot 验收矩阵（18 组合；HIBOT_SCHEMA_DIR 存在时逐份过契约 schema）
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
