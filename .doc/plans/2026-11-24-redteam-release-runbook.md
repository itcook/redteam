# 发布 runbook（redteam · macOS arm64）

> 发布入口：**`scripts/release.sh vX.Y.Z`**（前置检查 → 构建 → 打包 + SHA256SUMS → tag →
> GitHub Release → 可选 R2 镜像 → 下载回验）。
>
> 首期平台只有 **macOS arm64**（`aarch64-apple-darwin`）。Linux / Windows / Intel Mac 归 M20，
> `dist/install.sh` 与 `redteam update` 都会明确拒绝（不是静默失败）。
>
> 对应计划：`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §10 Phase 7。

## 0. 前提

| 项 | 要求 |
|---|---|
| 仓库 | `itcook/redteam`（public；`gh auth status` 需对它有权限） |
| 工具链 | `cargo`（含 `aarch64-apple-darwin` target）、`tar`、`shasum`、`curl`、`gh`、`aws`（仅 R2 镜像） |
| R2（镜像；客户端默认源） | `.env` 或环境变量：`R2_STATIC_ENDPOINT` / `R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` / `R2_BUCKET`，可选 `R2_PREFIX`（默认 `executor/audit/redteam`）/ `R2_BUCKET_DOMAIN`（默认 `release.hibot.sh`）。**`.env` 不入库**（`.gitignore` 已挡） |
| CI 配置 | secrets 同左四键；vars：`R2_PREFIX` / `R2_BUCKET_DOMAIN`（不设则用默认值） |
| 工作区 | 已跟踪文件干净；`make verify` 绿 |

R2 缺失时 release.sh / CI 都会自动跳过镜像（GitHub Release 不受影响）；`--skip-r2` 是显式跳过。

## 1. 版本准备

1. 改 `Cargo.toml` 的 `version`（唯一真相）。
2. `make gen` —— 把版本写进 `plugins/redteam/.claude-plugin/plugin.json`、
   `plugins/redteam/.codex-plugin/plugin.json`、`capabilities/redteam.json`
   （`scripts/sync-version`；只改 version token，幂等）。
3. 更新 `CHANGELOG.md`：把 `[Unreleased]` 内容挪到 `## [X.Y.Z] - YYYY-MM-DD`，保留空的
   `## [Unreleased]`。release notes 优先取该版本段，取不到才退回 `[Unreleased]`。
4. 本地门禁：

```bash
make verify          # 版本同源 + codex-skills/schema 防漂移 + packaging/version 测试
```

5. 提交并合到发布分支；**确认 `git status --porcelain --untracked-files=no` 为空**。

> 预发布（rc）：tag 用 `v0.1.0-rc.1`，`Cargo.toml` 仍写 `0.1.0`（release.sh 按 base 版本比对）。
> 预发布：GitHub Release 标 prerelease；R2 **不更新** `latest`（`latest` 只指向正式版）。

## 2. 发布

### 2.1 CI 发布（正式路径）

推 tag 即发布：

```bash
git tag v0.1.0 && git push origin v0.1.0              # 正式版
git tag v0.1.0-rc.1 && git push origin v0.1.0-rc.1    # 预发布
```

`.github/workflows/release.yml` 分四段：

1. **prepare**（ubuntu）：校验 tag 形态（`vX.Y.Z[-pre]`）、与 `Cargo.toml` 同源、指向 HEAD，判定 prerelease；
2. **build**（`macos-latest`，arm64）：`make verify` → `scripts/release.sh <tag> --prepare-only --skip-verify`，
   上传 tar.gz / SHA256SUMS / release-notes / install.sh / config.schema.json；
3. **release**：`softprops/action-gh-release@v2` 发布（rc 标 prerelease）；
4. **check_r2 → r2**：R2 四键齐全才镜像（缺失只告警、不阻塞），上传后按公开基址 HEAD 200 + sha 回验。

也可在 Actions → release → Run workflow 手填 tag（需要目标分支已包含本 workflow）。

### 2.2 本机入口与预演

```bash
scripts/release.sh v0.1.0                  # 推 tag，由 CI 发布（默认；脚本到此退出）
scripts/release.sh v0.1.0 --local-publish  # 本机完成 Release + R2（先停掉同 tag 的 Actions run）
scripts/release.sh v0.1.0-rc.1             # 预发布
scripts/release.sh v0.1.0 --prepare-only   # 只产出发行产物（要求工作区干净，零副作用）
scripts/release.sh v0.1.0 --dry-run        # 预演（允许脏工作区，零副作用）
scripts/release.sh v0.1.0 --skip-r2        # 本机发布但只发 GitHub Release
scripts/release.sh v0.1.0 --no-push        # 只建本地 tag（不推、不发 Release、不传 R2）
```

> **默认不双发**：推 tag 后 CI 会建 Release + 镜像 R2；本机若继续发，两套归档哈希不同，
> 会交叉覆盖同名资产与 `SHA256SUMS`。所以默认发布路径下脚本推送 tag 后即退出；
> 只有 `--local-publish` 才在本机继续（调用方负责先停掉/不触发同 tag 的 workflow）。

`--prepare-only` 与 `--dry-run` 都只落在 `dist/release/`；CI 用前者（干净工作区 + 门禁），
本地快速预演用后者。`--prepare-only` / `--dry-run` 不读 `.env`（不执行本机配置里的任意 shell），
`.env` 只在真正发布阶段加载。`--skip-verify` 只能与 `--prepare-only` 同用。

脚本会自己拒绝这些情况：

- 工作区有未提交改动（dry-run 除外；prepare-only 总是要求干净）；
- `--prepare-only` 与 `--dry-run` / `--no-push` / `--skip-r2` 同用；
- `--skip-verify` 未与 `--prepare-only` 同用；
- tag 已存在但不指向 `HEAD`（产物来自 HEAD，tag 必须同源）；
- tag 与 `Cargo.toml` 版本不一致；
- `gh` 未登录；
- `make verify` 不绿；
- 构建出的二进制 `--version` 与 Cargo 版本不一致。

脚本会自己拒绝这些情况：

- 工作区有未提交改动（dry-run 除外）；
- tag 已存在但不指向 `HEAD`（产物来自 HEAD，tag 必须同源）；
- tag 与 `Cargo.toml` 版本不一致；
- `gh` 未登录；
- `make verify` 不绿；
- 构建出的二进制 `--version` 与 Cargo 版本不一致。

### 2.3 发布后回验（脚本内自动做）

- GitHub Release（CI）：四个资产 HEAD 200；归档与 `config.schema.json` 下载回来比 SHA256。
- R2（启用时）：同样回验版本目录的归档 / `SHA256SUMS` / schema，外加 `install.sh`、前缀根 `config.schema.json`、`latest`（正式版）。
- 任一失败 → 步骤失败（发布不静默成功）。

## 3. 产物核对清单

| 资产 | 内容 / 用途 |
|---|---|
| `redteam-X.Y.Z-aarch64-apple-darwin.tar.gz` | `redteam` 二进制 + `README.md` + `LICENSE`（install/update 都消费它） |
| `SHA256SUMS` | 覆盖归档与 `config.schema.json`（方案 B：schema 可校验） |
| `install.sh` | standalone 安装脚本（`dist/install.sh` 原样发布） |
| `config.schema.json` | **hibot Phase 6 契约资产**：`$id: urn:redteam:config/2`，`schemaVersion: 2`；release notes 里同时列出 sha256 |

核对命令：

```bash
gh release view v0.1.0 --json assets --jq '.assets[].name'
shasum -a 256 dist/release/redteam-*-aarch64-apple-darwin.tar.gz
cat dist/release/SHA256SUMS
```

> **发布包里的 `redteam` 二进制来自 `target/aarch64-apple-darwin/release/redteam`**
> （release.sh 的 `cargo build --release --locked --target` 产物），不是 `plugins/redteam/bin/` 里入库的那份。
>
> 插件渠道（marketplace）的 `plugins/redteam/bin/darwin/aarch64/redteam` **不是** release 资产，
> 由 `make build` 重建、单独提交（`build: 重建二进制`）——插件渠道另行 `make build`。
> 发布时不要顺手把它提交进版本准备。

## 4. 安装验证

### 4.1 干净 HOME 安装

```bash
tmp=$(mktemp -d)
HOME="$tmp" curl -fsSL https://release.hibot.sh/executor/audit/redteam/install.sh | sh
"$tmp/.local/bin/redteam" --version
```

预期：`redteam 0.1.0`；`PATH` 不在时给出提示。
验证 GitHub 备源：`REDTEAM_FROM=github curl -fsSL .../releases/download/v0.1.0/install.sh | sh`。

### 4.2 篡改拒绝

```bash
# 本地造一个 R2 布局的目录，改一位哈希后装（默认源就是 r2）
mkdir -p /tmp/fake/redteam/v0.1.0
cp dist/release/redteam-0.1.0-aarch64-apple-darwin.tar.gz \
   dist/release/SHA256SUMS /tmp/fake/redteam/v0.1.0/
sed -i '' 's/^./0/' /tmp/fake/redteam/v0.1.0/SHA256SUMS     # 破坏一位
printf 'v0.1.0\n' > /tmp/fake/redteam/latest
REDTEAM_INSTALL_DIR=$(mktemp -d) \
REDTEAM_BASE_URL=file:///tmp/fake/redteam \
  sh dist/install.sh
```

> `file://` 需要 curl 版本支持；更稳的做法是 `python3 -m http.server` 起本地服务。
> 预期：`校验和验证失败，已中止安装（未写入任何文件）`，退出码非 0，安装目录里的旧二进制不变。

### 4.3 平台守卫

在 Linux（或 Intel Mac）上执行 `sh install.sh`：
预期：`首期仅 macOS arm64（检测到 ...）；...`，退出码非 0，且不下载任何文件。

### 4.4 自更新

```bash
~/.local/bin/redteam update --check      # 只读，零写入
~/.local/bin/redteam update              # 升级到最新正式版
~/.local/bin/redteam update --version v0.1.0 --force   # 重装指定版本
```

本地演练（不起网络）：按 R2 布局准备一个目录 + `python3 -m http.server`，然后

```bash
redteam update --base-url http://127.0.0.1:PORT/executor/audit/redteam --check   # 默认源已是 r2
```

## 5. R2 镜像（可选）

布局（桶沿用 `hiapi-static`；公开基址 `https://release.hibot.sh/executor/audit/redteam`
= `${R2_BUCKET_DOMAIN}/${R2_PREFIX}`）：

```
executor/audit/redteam/vX.Y.Z/redteam-X.Y.Z-aarch64-apple-darwin.tar.gz
executor/audit/redteam/vX.Y.Z/SHA256SUMS
executor/audit/redteam/vX.Y.Z/config.schema.json
executor/audit/redteam/install.sh          # 前缀根：当前版稳定入口
executor/audit/redteam/config.schema.json  # 前缀根：hibot 契约不看版本号
executor/audit/redteam/latest              # 仅正式版更新；内容 vX.Y.Z
```

**没有 `releases/` 段**（子域已经叫 `release`，产品路径进了前缀）。

所有对象 `--cache-control no-cache`（CDN 会供旧对象；版本化路径也会被重发覆盖）。

## 6. 回滚

1. **R2 的 `latest` 指回上一版**：

```bash
printf 'v0.0.9\n' > /tmp/latest
AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=... \
  aws s3 cp /tmp/latest s3://$R2_BUCKET/executor/audit/redteam/latest \
    --endpoint-url "$R2_STATIC_ENDPOINT" --content-type text/plain --cache-control no-cache
```

2. **GitHub Release**：`gh release delete vX.Y.Z --yes`（或 edit 成 prerelease 让人看不见）；
   重发资产：`scripts/release.sh vX.Y.Z`（已存在 Release 时走 `upload --clobber`）。
3. 已装用户：`redteam update --version v0.0.9`（显式版本允许降级安装）。

## 7. 已知限制

- **未签名、未公证（notarization）**：从网络下载的二进制首次运行会被 Gatekeeper 拦截。
  用户侧绕过：`xattr -d com.apple.quarantine ~/.local/bin/redteam`，或 系统设置 → 隐私与安全性 →
  「仍要打开」。安装脚本**不会**替用户去掉 quarantine 标记（安全默认）。
- **插件二进制入库的 diff**：`make build` 会重写 `plugins/redteam/bin/darwin/aarch64/redteam`
  （5.9MB），必须单独提交；发布脚本刻意不跑 `make build`，避免把未审二进制混进发布提交。
- **CI**：`.github/workflows/release.yml` 是正式路径；本机 release.sh 保留为兜底与预演入口。
- **仅 macOS arm64**：其他平台 `install.sh` / `update` 明确报错。

## 8. 本地自证（零真实发布）

```bash
make verify
scripts/release.sh v0.1.0 --prepare-only        # 零副作用产物（要求工作区干净）
scripts/release.sh v0.1.0 --dry-run             # 产物齐全，不碰远端
scripts/release.sh v0.0.9 --dry-run             # 应报 Cargo 版本不一致
git tag v9.9.9 HEAD~1 2>/dev/null || true       # 造一个指向非 HEAD 的 tag
scripts/release.sh v9.9.9 --dry-run             # 应报「tag 指向 ... 但 HEAD 是 ...」
```

安装/更新演练见 §4（全部在临时目录 + 本地 HTTP 服务里完成）。
