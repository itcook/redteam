# Changelog

本文件记录 redteam 的显著变更。格式遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added

- 发布面：`scripts/release.sh`（构建 → 打包 + `SHA256SUMS` → GitHub Release → 可选 R2 镜像 → 下载回验），
  `--dry-run` / `--skip-r2` / `--no-push` 变体。
- CD：`.github/workflows/release.yml`（push tag `v*` → macOS arm64 构建 → GitHub Release → R2 镜像 → 回验）；
  `scripts/release.sh --prepare-only` 是 CI 唯一打包入口；R2 前缀收敛为 `executor/audit/redteam`
  （去掉历史 `releases/` 段，公开基址 `https://release.hibot.sh/executor/audit/redteam`）。
- standalone 安装与自更新：`dist/install.sh`（`latest` 解析、SHA256 校验、原子安装、macOS arm64 守卫）与
  `redteam update`（`--check` / `--version` / `--force`，同目录锁 + 原子替换 + 替换后自检）。
- 版本单一来源：`Cargo.toml` 为唯一真相，`make gen` 同步两个 `plugin.json` 与
  `capabilities/redteam.json`；`make verify` 断言四处与二进制 `--version` 一致。
- 发布 runbook 与 MIT `LICENSE`。
