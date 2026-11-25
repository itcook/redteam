# U9 · GitHub Actions CD（tag → GitHub Release + R2 镜像）

## 背景

发布目前是手工 `scripts/release.sh vX.Y.Z`（本机 macOS arm64）。U9 把发布搬进 CI：
`push tag v*` 自动构建 → 发 GitHub Release → 镜像到 R2 → 下载回验。

**本轮不真正发布**（不打 tag / 不推 / 不建 Release / 不传 R2），只交付代码 + 本地 `--prepare-only` 验证。

## R2 对象布局（用户已确认）

公开基址：`https://release.hibot.sh/executor/audit/redteam`

```
{prefix}/v{version}/{asset}
{prefix}/v{version}/SHA256SUMS
{prefix}/v{version}/config.schema.json
{prefix}/config.schema.json
{prefix}/install.sh
{prefix}/latest                      # 仅正式版更新
```

- `prefix` = `R2_PREFIX`，默认 `executor/audit/redteam`（vars 可覆盖）。
- **不再有 `releases/` 段**：子域已经叫 `release`，且产品路径进了前缀。

## 范围

1. `scripts/release.sh`
   - `R2_PREFIX` 默认 `executor/audit/redteam`，归一化（去首尾斜杠 / 折叠重复斜杠 / 空值回默认）。
   - `PUBLIC_BASE="https://${R2_BUCKET_DOMAIN:-release.hibot.sh}/${R2_PREFIX}"`，上传 key 与回验共用。
   - 版本 key 去掉 `releases/`；`config.schema.json` 同时放前缀根；`latest` 仅正式版。
   - 新增 `--prepare-only`：前置检查（干净工作区、版本同源、tag 存在时必须指向 HEAD）→ `make verify`
     → `make gen` → 构建 → 打包 + `SHA256SUMS` + release-notes，**不建/不推 tag、不建 Release、不传 R2**；
     不要求 `gh`/`aws`/R2 凭据。
   - 新增 `--skip-verify`（CI build job 已单独跑过门禁时使用）。
   - 新增 `--print-config`（调试/测试用）：打印归一化后的 prefix 与公开基址后退出，零副作用。
2. `dist/install.sh`
   - `R2_DEFAULT_BASE="https://release.hibot.sh/executor/audit/redteam"`。
   - R2 版本目录 `${BASE_URL}/${VERSION}`（GitHub 分支不变）。
3. `src/update/mod.rs`
   - `R2_BASE_URL` 改为新基址；版本目录统一 `{base}/{tag}`（R2 不再有 `/releases`）。
   - 抽出可测的 URL 构造；单测覆盖 R2 / GitHub 差异与未知源。
4. `.github/workflows/release.yml`（新）
   - `push: tags: ['v*']` + `workflow_dispatch`（输入 tag）。
   - prepare（ubuntu）：tag 形态、Cargo 版本同源、prerelease、sha。
   - build（macos-14）：checkout tag → toolchain/cache → `make verify` → `release.sh --prepare-only --skip-verify`
     → upload-artifact。
   - release（ubuntu）：`softprops/action-gh-release@v2`，正式/预发布区分。
   - check-r2 + r2（ubuntu）：四键缺失则跳过镜像（不阻塞）；`aws s3 cp` + `no-cache` + 正确 content-type；
     正式版才更新 `latest`；上传后 HEAD 200 + tar.gz/schema sha 比对。
   - secrets：`R2_ACCESS_KEY_ID` / `R2_SECRET_ACCESS_KEY` / `R2_STATIC_ENDPOINT` / `R2_BUCKET`；
     vars：`R2_PREFIX` / `R2_BUCKET_DOMAIN`。
5. 文档：README 发布段 + `.doc/plans/2026-11-24-redteam-release-runbook.md`（CI 路径、secrets/vars、
   layout、prerelease/latest 规则、schema 版本纠正为 `config/2`）。
6. 重建入库二进制：`R2_BASE_URL` 变更后 `make build`，单独提交（`build: 重建二进制（R2 基址变更）`）。

## 非目标（禁区）

- 不发布：不打 tag、不推 tag、不建 Release、不传 R2。
- 不改版本号、不改 config schema / capabilities、不碰三 kind 逻辑。
- `~/hibot` 只读；不碰 `dev`。

## 验收

- `make verify` + `cargo test` 全绿；告警 0。
- `scripts/release.sh v0.1.0 --prepare-only` 产出 tar.gz / SHA256SUMS（含 schema，sha `246d2c97…`）/
  release-notes / install.sh / config.schema.json；零 git/tag/R2 副作用；可重复执行。
- `bash -n` 通过；workflow YAML 可解析。
- 单测覆盖 R2/GitHub URL 差异、`R2_PREFIX` 空值/带斜杠边界。
- 回报：提交、测试原文、产物清单、workflow 触发与 secrets/vars 清单、`--prepare-only` 幂等性。
