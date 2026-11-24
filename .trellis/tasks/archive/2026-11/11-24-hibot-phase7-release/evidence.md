# Phase 7 自证（U6）— 2026-11-24

基线：`feature/hibot-release`，起点 `cd047e9`。全程**零真实发布**：未推送 tag、未建 GitHub Release、
未上传 R2、未修改任何线上状态。唯一的外部动作是本地 HTTP fixture 与临时目录。

## 提交

| commit | 内容 |
|---|---|
| `0a73610` | 7.1 版本单一来源（sync-version / Makefile / tests/version.rs / CHANGELOG） |
| `1e94887` | 7.2 + 7.5 release.sh（打包 / SHA256SUMS / tag / Release / R2 / 回验）+ LICENSE + .gitignore |
| `1f55d9c` | 7.3 + 7.7 install.sh（latest / 校验 / 原子安装 / 平台守卫）+ README |
| `664a930` | 7.4 `redteam update`（--check / --version / --force / flock / 自检 / 插件守卫） |
| `756591b` | 7.6 runbook + Trellis 任务 |
| `b18b745` | fix：变量后接中文标点用花括号（bash 非 UTF-8 locale 吞字节，首个 dry-run 复现） |
| `e709c2b` | fix：门禁失败打日志、release notes 去段首空行 |

## 1. `make verify`（版本同源 + 防漂移）

```
sync-version: ok（0.1.0，3 处一致）
git diff --exit-code ...（codex-skills / 两个 plugin.json / capabilities / config.schema.json）→ 0
tests/packaging.rs 7 passed；tests/version.rs 4 passed
```

`tests/version.rs` 覆盖：Cargo == 两个 `plugin.json` == `capabilities/redteam.json` ==
二进制 `--version`；`CHANGELOG.md` 顶层为 `[Unreleased]`。

## 2. `scripts/release.sh v0.1.0 --dry-run`（产物齐全，不碰远端）

```
产物：
  config.schema.json  14398 B
  install.sh           7293 B
  redteam-0.1.0-aarch64-apple-darwin.tar.gz  3552867 B
  release-notes.md     1013 B
  SHA256SUMS            193 B

SHA256SUMS：
  <archive>  redteam-0.1.0-aarch64-apple-darwin.tar.gz
  13e042e97e5706ed4c4d3cb1674afeb33c421b7cccc0d397f97ec0e7b5beef6d  config.schema.json
==> dry-run：已跳过 tag / GitHub Release / R2 上传
```

归档内容：`./redteam`、`./README.md`、`./LICENSE`。schema 哈希与 hibot B1 校准记录一致（`13e042e9…ef6d`）。

负路径：

```
scripts/release.sh v0.0.9 --dry-run  → 错误: Cargo.toml 版本 (0.1.0) 与 tag (v0.0.9) 不一致
git tag v0.1.0 HEAD~1; scripts/release.sh v0.1.0 --dry-run
  → 错误: tag v0.1.0 指向 756591b，但 HEAD 是 b18b745 ...（随后 git tag -d，无残留）
scripts/release.sh v0.1.0 --no-push → 本地建 tag 后退出（未推送/未建 Release/未传 R2），tag 已删除
```

## 3. install.sh（干净 HOME / 篡改 / 平台守卫）

本地 fixture（R2 布局 + `python3 -m http.server`），9 项全过：

```
PASS  install.sh 干净 HOME 安装成功（7 行输出）           # redteam 0.1.0
PASS  install.sh 篡改一位 → 中止安装，旧二进制未被覆盖     # 校验和验证失败
PASS  install.sh 非 Darwin/arm64 → 明确报错且未创建安装目录 # 首期仅 macOS arm64
PASS  update --check 零写入（未产生锁/临时文件）
PASS  插件路径默认拒绝（--force 未给，二进制未变）
PASS  插件路径 --check 仍只读可用
PASS  update 成功替换并通过 --version 自检（v0.1.0 → v0.2.0）
PASS  update 校验失败 → 中止且现有二进制未改动
PASS  并发两次 update --force 均成功（第二个等锁后提交）
结果：9 通过 / 0 失败
```

阻塞锁语义另有单元测试 `update::replace::tests::lock_serializes_two_holders`（第二个持有者
在第一个释放前不返回）。

## 4. 单元/集成测试

```
cargo test --lib update::  → 16 passed（版本比较 / 归档解包 / 原子替换 / 自检 / 插件守卫 / 锁）
make verify                → 全绿（见 §1）
```

## 未验证 / 已知边界

- **R2 上传与回验未实跑**：本机无 `.env` 凭据；`--dry-run` 本就不触发。缺凭据时脚本走
  「跳过镜像」分支（代码路径已写，未在真实 R2 上验证）。
- **GitHub Release 创建/覆盖上传未实跑**（零发布约束）；逻辑对照 `~/hiapi/cli/scripts/release.sh`。
- **真实 `latest` 解析未实跑**：仓库尚无 Release，`install.sh`/`update` 走网络时会报
  「无法解析最新版本」——首次发布后才能验证。
- **入库的插件二进制是旧引擎**（不含 `update` 子命令）：插件渠道要拿到本单元能力，需要
  `make build` 重建 + 单独提交二进制（本次按约束未做）。
- 插件市场版本 1.0.0 → 0.1.0 可能被视作降级（已在上报中提示）。
