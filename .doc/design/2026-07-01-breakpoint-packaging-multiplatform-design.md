# Breakpoint 打包多平台就绪设计（M4，scope A）

> Roadmap：`.doc/roadmap.md` M4。scope A = 让打包**多平台就绪**（布局 + launcher 探针 + 瘦身），
> 仅填 darwin/aarch64，真正的 linux/win 交叉构建（`cross`/CI）留给后续。

## 1. 动机

三个缺口：
1. **布局扁平**：`plugins/breakpoint/bin/brk-darwin-arm64`，os-arch 塞在文件名里，加平台就得改所有引用。
2. **无平台探测**：6 处命令壳硬编码 `brk-darwin-arm64`；非 darwin-arm64 宿主上会遭遇隐式「bad CPU
   type / No such file」（P1-C dogfood 亲历的 finding）。未来 loop SKILL 也需要一个稳定入口。
3. **二进制偏大**：darwin-arm64 10MB、**未 strip（24649 符号）、无 `[profile.release]`**。

## 2. 范围

**做**（scope A）：
- 布局改 `bin/<os>/<arch>/brk`（`os∈{darwin,linux,windows}`、`arch∈{aarch64,x86_64}`，Rust target 风）。
- `bin/brk` launcher 探针：`uname` 映射 os/arch → exec 真二进制；缺失给明确报错。
- `[profile.release]` 瘦身。
- 接线：Makefile、6 命令壳、packaging 测试。

**不做（defer）**：
- 真正产出 linux/win 二进制（`cross`/CI 交叉构建）—— 后续独立步骤，本 spec 只立结构、仅填 darwin/aarch64。
- launcher 的**运行环境探测**（Claude Code/Codex/裸终端）与 **host-model 委派** —— roadmap **M9 宿主集成**，
  未设计。本 spec 的 launcher 结构上为 env 探测预留（§4 前瞻注），但不实现。
- Windows 原生 shell（cmd/pwsh）launcher —— 待真 Windows 二进制时补（§4 caveat）。

## 3. 布局 `bin/<os>/<arch>/brk`

```
plugins/breakpoint/bin/
├── brk                      # launcher（sh，稳定入口，见 §4）
└── darwin/aarch64/brk       # 真二进制（现仅此一个）
    # 未来：linux/x86_64/brk、linux/aarch64/brk、windows/x86_64/brk.exe …
```

- os token：`darwin` / `linux` / `windows`。arch token：`aarch64` / `x86_64`（Rust target 风，
  与本项目 Rust 实现一致）。Windows 二进制名 `brk.exe`，其余 `brk`。
- 现 `bin/brk-darwin-arm64` → `bin/darwin/aarch64/brk`（**删旧扁平文件**）。
- 只填 darwin/aarch64；其余平台目录不存在（launcher 命中不存在时明确报错）。

## 4. launcher 探针 `bin/brk`

稳定入口。命令壳（`commands/*` + `codex-skills/*`）与未来 loop SKILL **都只调 `${ROOT}/bin/brk`**，
探测收敛在此一处（vs 6 处内联，DRY）。POSIX `sh`：

```sh
#!/bin/sh
# brk launcher — resolve <os>/<arch> and exec the matching binary.
# 稳定入口：插件命令壳与未来 loop SKILL 都调本文件；平台探测只此一处。
# 前瞻（roadmap M9-a）：此处的 uname 分派点，未来可扩展为「运行环境探测」
#   （Claude Code / Codex / 裸终端），为 host-model 委派铺路——本 spec 不实现。
set -e
dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

case "$(uname -s)" in
    Darwin) os=darwin ;;
    Linux)  os=linux ;;
    MINGW*|MSYS*|CYGWIN*) os=windows ;;
    *) echo "brk: unsupported OS $(uname -s)" >&2; exit 127 ;;
esac
case "$(uname -m)" in
    arm64|aarch64) arch=aarch64 ;;
    x86_64|amd64)  arch=x86_64 ;;
    *) echo "brk: unsupported arch $(uname -m)" >&2; exit 127 ;;
esac

ext=""
[ "$os" = windows ] && ext=".exe"
target="$dir/$os/$arch/brk$ext"
if [ ! -x "$target" ]; then
    echo "brk: no binary for $os/$arch (shipped: darwin/aarch64; run 'make build' or fetch from release)" >&2
    exit 127
fi
exec "$target" "$@"
```

- **退出码 127**（launcher 解析失败：不支持的 os/arch，或目标二进制缺失）——与 brk 自身退出码
  （0/2/10）不撞，让 loop SKILL 能区分「没起来」与「跑了并给了 verdict」。
- **明确报错**替代隐式 exec 失败——根治 P1-C dogfood 的「bad CPU type」隐式失败 finding。
- **Windows caveat**：sh launcher 需 bash 环境（git-bash/WSL）；原生 cmd/pwsh 跑不了。scope A 不产
  Windows 二进制，故 launcher 现只需在 darwin/linux 工作、windows 分支命中即给清晰报错（未来随真
  Windows 二进制补 `.cmd`/`.ps1` 兄弟）。
- launcher 须 `chmod +x`、随插件提交。

## 5. 瘦身 `[profile.release]`

Cargo.toml 加（现无任何 profile 段）：

```toml
[profile.release]
strip = true          # 去符号表（现 24649 符号未 strip，单项砍最多）
lto = true            # 跨 crate 死代码消除
codegen-units = 1     # 更充分优化，略小
panic = "abort"       # 去 unwind 表（安全：无 catch_unwind；可靠性靠磁盘 checkpoint 非 unwind）
opt-level = "s"       # 体积优先；实测 "s" vs "z" 取更小者（CLI 以 IO 为主，速度不敏感）
```

- 预计 **10MB → 3-5MB**。实施时实测 `opt-level` 的 `"s"` 与 `"z"`，取二进制更小的一个。
- **`panic="abort"` 安全性核查**（已确认）：`src/` 无 `catch_unwind`/`panic::catch`/`set_hook`；
  可靠性不依赖 unwind（部分结果靠 R2.1 磁盘 checkpoint）。
- **测试不受影响**：`cargo test`/`make verify` 走 dev/test profile（unwind），非 release。
  **约束**：不要 `cargo test --release`（release 的 panic=abort 会让 test harness 无法捕获 panic）。

## 6. 接线

引用二进制路径的点（已 grep 确认）：
- **6 命令壳**：`commands/{review,status,result}.md`（手写真相源）+ `codex-skills/*/SKILL.md`（生成）。
  把 `${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64` → `${CLAUDE_PLUGIN_ROOT}/bin/brk`（codex 侧 `CODEX_`）。
  **只改 `commands/*` 真相源，`make gen`（gen-codex-skills）自动传导**——该脚本原样透传二进制名、
  仅替换 `CLAUDE_PLUGIN_ROOT`→`CODEX_PLUGIN_ROOT`，无需改脚本。
- **Makefile**：`BIN := plugins/breakpoint/bin/darwin/aarch64/brk`；`build` 目标 `mkdir -p $(dir $(BIN))`
  已有、copy 后确保 `chmod +x plugins/breakpoint/bin/brk`（launcher 提交时已 +x，此为兜底）。
- **`tests/packaging.rs:53-54`**：断言 `body.contains("${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64")`
  → 改为 `.../bin/brk`。
- **不涉及**：`.claude-plugin/plugin.json`、`.codex-plugin/plugin.json`（不引用二进制路径）；
  `gen-codex-skills` 脚本逻辑（透传）。

## 7. 测试

- **packaging 测试扩断言**（`tests/packaging.rs`）：
  - 命令壳引用 `${*_PLUGIN_ROOT}/bin/brk`（不再是 `brk-darwin-arm64`）——改现有断言。
  - `bin/brk` launcher 存在且可执行（`is_executable` / 权限位）。
  - `bin/darwin/aarch64/brk` 真二进制存在。
  - 无任何文件残留 `brk-darwin-arm64` 引用（grep 断言）。
- **launcher 行为**（shell 级小测，可放 `tests/` 下一个 `#[test]` 用 `std::process::Command` 调 sh，
  或一个 `scripts/test-launcher.sh`）：
  - 构造临时 `bin/<os>/<arch>/brk` 桩 → launcher 在当前平台 exec 到它、透传 args + 退出码。
  - 目标缺失 → 退出码 127 + stderr 含 "no binary for"。
- **回归**：`make verify`（gen 无漂移 + packaging 测试）仍绿。

## 8. 关键决策

① **scope A：结构就绪、仅填 darwin/aarch64**（用户 2026-07-01 定）。真交叉构建（rustls 的 ring/aws-lc
带 C/asm，macOS→linux/win 本地痛）留给 `cross`/CI，避免现在耗在工具链。

② **os/arch token = `darwin/linux/windows` + `aarch64/x86_64`**（Rust target 风，用户定）——与本项目
Rust 实现一致，`uname` 映射一次即可。

③ **launcher 走稳定入口 + 探测收一处**（vs 6 处内联）：命令壳/loop SKILL 都调 `bin/brk`。launcher
结构为 M9-a「运行环境探测」预留（§4 注），但本 spec 不实现。

④ **launcher 缺失二进制 → 退出 127 + 报错带指引**（用户倾向）：不撞 brk 退出码、给可操作提示
（`make build` / fetch release）。

⑤ **`panic="abort"` 安全**（已核查无 catch_unwind）：更小 + 可靠性不受损（checkpoint 兜底）；
仅约束不跑 `cargo test --release`。

## 9. 触及文件

| 文件 | 改动 |
|---|---|
| `plugins/breakpoint/bin/brk`（新） | launcher sh 脚本（§4），`chmod +x` |
| `plugins/breakpoint/bin/darwin/aarch64/brk`（新，移动） | 现 `bin/brk-darwin-arm64` 移动至此；删旧扁平文件 |
| `Cargo.toml` | 新增 `[profile.release]`（§5） |
| `Makefile` | `BIN` 路径 → `bin/darwin/aarch64/brk`；`chmod +x bin/brk` |
| `plugins/breakpoint/commands/{review,status,result}.md` | 二进制引用 `brk-darwin-arm64` → `brk`；`make gen` 重生成 codex-skills |
| `plugins/breakpoint/codex-skills/*/SKILL.md` | 由 `make gen` 自动更新（不手改） |
| `tests/packaging.rs` | 断言 `brk-darwin-arm64` → `brk`；新增 launcher/布局/无残留断言 |

## 10. 非目标

- 真 linux/win 二进制产出（`cross`/CI）。
- M9 宿主集成（env 探测 / host-model 委派）。
- Windows 原生 shell launcher。
- `cargo bloat` 逐 crate 深度瘦身（如砍 grep-*）——§5 的 profile 已够；深挖留待有实测需求。
