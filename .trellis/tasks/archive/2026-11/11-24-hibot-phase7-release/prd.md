# Phase 7 · 发布面与分发（7.0–7.7）

## 背景

hibot audit 执行单元接入的 U6 单元（协调者 session `01a20ba0` 派发）。计划权威：
`.doc/plans/2026-11-23-redteam-hibot-consumer-plan.md` §10 Phase 7 / §14 / §15。
本条是**「可发布」能力建设，不是现在发布**：全程零真实发布（不打 tag、不推、不建 Release、不写 R2）。

## 硬约束（协调者派发原文）

- 禁止：打/推 tag、`gh release create`、上传 R2、修改任何线上状态；全部用 `--dry-run` + 本地临时目录。
- 不 bump 版本（`Cargo.toml` 保持 0.1.0）；不提交重建的 `plugins/redteam/bin/` 二进制（需要时先批准）。
- 不碰 `src/job/`、`src/triage/`、`src/fixverify/`；不改 `dist/config.schema.json` 内容（只打包）。
- 不做签名/公证、不做 Linux/Windows、不做 CI。

## 范围与落点

| 项 | 内容 | 落点 |
|---|---|---|
| 7.1 | 版本单一来源：`Cargo.toml` 为准；两个 `plugin.json` 由 `make gen` 写入/校验；capabilities 同步；新增 `CHANGELOG.md`（`[Unreleased]` + Keep-a-Changelog）；`make verify` 断言 Cargo == plugin.json×2 == capabilities == 二进制 `--version` + CHANGELOG 顶层 `[Unreleased]` | `scripts/sync-version`、`Makefile`、`tests/version.rs`、`tests/packaging.rs`、`CHANGELOG.md` |
| 7.2 | `scripts/release.sh`：前置（干净、tag 指向 HEAD、tag==Cargo、gh 登录、`make verify` 绿）→ `make gen` + `cargo build --release` → 打包 tar.gz（二进制+README+LICENSE）+ `SHA256SUMS`（含 `config.schema.json`）→ tag/推 tag → `gh release create`（预发布 `--prerelease`，已存在 `upload --clobber`）→ notes 从 CHANGELOG 提取 + 附资产 sha256 → R2 镜像 → 下载回验（200 + sha256）；`--dry-run`/`--skip-r2`/`--no-push` | `scripts/release.sh`、`LICENSE` |
| 7.3 | `dist/install.sh`：`REDTEAM_VERSION`/`REDTEAM_INSTALL_DIR`/`REDTEAM_BASE_URL`/`REDTEAM_FROM`（默认 github）；`latest` 解析；`SHA256SUMS` 校验；staged + `mv` 原子安装；PATH 提示；装完 `--version`；平台守卫 Darwin+arm64 | `dist/install.sh` |
| 7.4 | `redteam update`：`--check`/`--version`/`--force`（+`--from`/`--base-url`）；`libc::flock` 同目录锁；原子替换；替换后 `--version` 自检；失败不覆盖；插件路径（`plugins/redteam/bin/`）默认拒绝、`--force` 才写 | `src/update/*`、`src/main.rs` |
| 7.5 | R2 镜像：`redteam/install.sh`、`redteam/latest`、`redteam/releases/vX/...`，统一 `--cache-control no-cache`；凭据走 `.env` 四键 + 可选 `R2_BUCKET_DOMAIN`，`.env` 不入库 | `scripts/release.sh`、`.gitignore` |
| 7.6 | 发布 runbook：前提/版本准备/发布/产物核对/安装验证（含篡改拒绝）/R2/回滚/已知限制（未签名未公证、bin 入库 diff） | `.doc/plans/2026-11-24-redteam-release-runbook.md` |
| 7.7 | 平台扩展位：launcher 已按 `uname` 分派；`install.sh` 的 `TARGET` 单点常量 + 守卫；不实现 Linux/Windows | `dist/install.sh` |

## 验收（协调者给定）

1. `make verify` 断言版本同源 + CHANGELOG `[Unreleased]`；
2. `release.sh --dry-run` 产物齐全（tar.gz + SHA256SUMS + install.sh + config.schema.json），tag≠HEAD 拒发；
3. 干净 `HOME` 下 `install.sh` 装成功、改一位哈希中止且旧件不被覆盖、非 arm64 明确报错；
4. `update --check` 零写入、插件路径默认拒绝、并发两次第二个等锁、失败不覆盖；
5. runbook 从零能照做；
6. 全程零真实发布。

## 决策与偏离

- **release 打包不跑 `make build`**：`make build` 会覆盖入库的插件二进制（产生 5.9MB diff），与本条「不提交重建二进制」冲突；改为直接 `cargo build --release --target` 并打包 `target/.../redteam`。插件渠道仍由 `make build` + 单独提交维护。
- **capabilities 版本也纳入 `make gen` 同步**（计划只说 plugin.json）：否则 `tests/capabilities.rs` 的既存断言在升版时必然失败，且计划要求 capabilities 与 `CARGO_PKG_VERSION` 同源。
- **`plugin.json` 版本 1.0.0 → 0.1.0**：按「Cargo.toml 单一真相」收敛。风险：插件市场可能把 1.0.0→0.1.0 视为降级，已在回报中提示协调者/用户。
- **LICENSE 缺失**：新增 MIT LICENSE（`plugin.json` 已声明 MIT，作者 S.Kai），release 归档需要它。
- **update 依赖**：新增 `flate2`/`tar`（解 tar.gz）与 `semver`（版本比较），均纯 Rust、无 OpenSSL，符合仓内 rustls-only 约束。
