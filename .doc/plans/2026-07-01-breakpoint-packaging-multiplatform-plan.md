# Breakpoint 打包多平台就绪（M4）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans（单会话内联执行，用户已选）。Steps 用 `- [ ]` 跟踪。

**Goal:** 打包结构多平台就绪——`bin/<os>/<arch>/brk` 布局 + `bin/brk` launcher 探针 + 瘦身，仅填 darwin/aarch64。

**Architecture:** 三块自成任务：(1) 瘦身 profile + 新布局产二进制；(2) launcher 探针；(3) 命令壳/生成/测试接线。scope A：不产 linux/win 二进制。

**Tech Stack:** Rust（Cargo profile）、POSIX sh（launcher）、Make、bash（gen 脚本）。

**Spec:** `.doc/design/2026-07-01-breakpoint-packaging-multiplatform-design.md`（§8 决策为准）。

## Global Constraints

- **scope A**：仅填 `darwin/aarch64`；不做 `cross`/CI 交叉构建，不做 M9 宿主集成，不做 Windows 原生 shell。
- os token `darwin/linux/windows`、arch token `aarch64/x86_64`（Rust target 风）。
- **不要 `cargo test --release`**（release 的 `panic="abort"` 会让 test harness 无法捕获 panic）；`cargo test`/`make verify` 走 dev profile 不受影响。
- 零新增依赖。launcher 用 POSIX `sh`（非 bashism），退出码 127 表 launcher 解析失败。
- PONYTAIL：最短正确 diff；探测逻辑只在 launcher 一处。

---

## File Structure

| 文件 | 职责 |
|---|---|
| `Cargo.toml` | 新增 `[profile.release]` 瘦身 |
| `Makefile` | `BIN` → 新布局路径；`chmod +x` launcher |
| `plugins/breakpoint/bin/brk`（新） | launcher sh：uname→exec 真二进制 |
| `plugins/breakpoint/bin/darwin/aarch64/brk`（新位置） | 真二进制（由 `make build` 产出） |
| `plugins/breakpoint/commands/{review,status,result}.md` | 二进制引用 `brk-darwin-arm64`→`brk` |
| `plugins/breakpoint/codex-skills/*/SKILL.md` | `make gen` 自动更新 |
| `tests/packaging.rs` | 断言更新 + launcher/布局/无残留断言 |

---

## Task 1: 瘦身 profile + 新布局产二进制

**Files:**
- Modify: `Cargo.toml`（新增 `[profile.release]`）
- Modify: `Makefile`（`BIN` 路径）

- [ ] **Step 1: Cargo.toml 加 `[profile.release]`**

在 `Cargo.toml` 末尾（`[dev-dependencies]` 段之后）追加：
```toml
[profile.release]
strip = true
lto = true
codegen-units = 1
panic = "abort"
opt-level = "s"
```

- [ ] **Step 2: Makefile 的 `BIN` 改新布局路径**

`Makefile` 第 1 行 `BIN := plugins/breakpoint/bin/brk-darwin-arm64` 改为：
```makefile
BIN := plugins/breakpoint/bin/darwin/aarch64/brk
```
（`build` 目标里已有 `mkdir -p $(dir $(BIN))`，会自动建 `bin/darwin/aarch64/`。）

- [ ] **Step 3: 构建到新路径 + 删旧扁平二进制**

Run:
```bash
make build
git rm --quiet plugins/breakpoint/bin/brk-darwin-arm64 2>/dev/null || rm -f plugins/breakpoint/bin/brk-darwin-arm64
ls -lh plugins/breakpoint/bin/darwin/aarch64/brk | awk '{print "opt-s size:", $5}'
```
Expected: 新二进制在 `bin/darwin/aarch64/brk`；打印其大小（预计 3-5MB）。

- [ ] **Step 4: 实测 opt-level "z" 对比取小**

Run:
```bash
sed -i '' 's/opt-level = "s"/opt-level = "z"/' Cargo.toml
make build
ls -lh plugins/breakpoint/bin/darwin/aarch64/brk | awk '{print "opt-z size:", $5}'
```
比较 s 与 z 的字节数（`stat -f%z plugins/breakpoint/bin/darwin/aarch64/brk`）。**取更小者**：若 s 更小或相当，把 Cargo.toml 改回 `opt-level = "s"` 并重 `make build`；若 z 明显更小，保留 z。记录最终选择。

- [ ] **Step 5: 验二进制可用（新路径直接跑）**

Run: `plugins/breakpoint/bin/darwin/aarch64/brk --help 2>&1 | head -3`
Expected: 正常打印 brk usage（证明瘦身后二进制功能正常）。

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Makefile plugins/breakpoint/bin/darwin/aarch64/brk
git commit -m "build(packaging): slim [profile.release] + move binary to bin/darwin/aarch64/

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: launcher 探针 `bin/brk`

**Files:**
- Create: `plugins/breakpoint/bin/brk`
- Modify: `Makefile`（chmod 兜底）

- [ ] **Step 1: 写 launcher `plugins/breakpoint/bin/brk`**

创建文件，内容（POSIX sh，逐字取自 spec §4）：
```sh
#!/bin/sh
# brk launcher — resolve <os>/<arch> and exec the matching binary.
# 稳定入口：插件命令壳与未来 loop SKILL 都调本文件；平台探测只此一处。
# 前瞻（roadmap M9-a）：此处的 uname 分派点，未来可扩展为「运行环境探测」
#   （Claude Code / Codex / 裸终端），为 host-model 委派铺路——当前不实现。
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

- [ ] **Step 2: chmod +x + Makefile chmod 兜底**

Run: `chmod +x plugins/breakpoint/bin/brk`

`Makefile` 的 `build` 目标末尾（`cp target/... $(BIN)` 之后）加一行，确保 launcher 权限位：
```makefile
	chmod +x plugins/breakpoint/bin/brk
```

- [ ] **Step 3: 验 launcher 在 darwin 解析到真二进制**

Run: `plugins/breakpoint/bin/brk --help 2>&1 | head -3`
Expected: 与真二进制同样的 brk usage（证明 launcher 正确 exec 到 `darwin/aarch64/brk`）。

- [ ] **Step 4: 验 launcher 缺失分支报错**

Run:
```bash
# 临时把真二进制挪走，验缺失报错
mv plugins/breakpoint/bin/darwin/aarch64/brk /tmp/brk.bak
plugins/breakpoint/bin/brk review 2>&1; echo "exit=$?"
mv /tmp/brk.bak plugins/breakpoint/bin/darwin/aarch64/brk
```
Expected: stderr 含 `no binary for darwin/aarch64`，`exit=127`。

- [ ] **Step 5: Commit**

```bash
git add plugins/breakpoint/bin/brk Makefile
git commit -m "feat(packaging): bin/brk launcher — uname→exec platform binary, 127 on miss

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: 命令壳 + 生成 + packaging 测试接线

**Files:**
- Modify: `plugins/breakpoint/commands/{review,status,result}.md`
- Modify: `tests/packaging.rs`
- Regenerate: `plugins/breakpoint/codex-skills/*/SKILL.md`（via `make gen`）

- [ ] **Step 1: 3 命令壳二进制引用改 `brk`**

对 `plugins/breakpoint/commands/review.md`、`status.md`、`result.md` 各一处：把
`${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64` 改为 `${CLAUDE_PLUGIN_ROOT}/bin/brk`。
（例 review.md：`${CLAUDE_PLUGIN_ROOT}/bin/brk review $ARGUMENTS`。）

- [ ] **Step 2: 重生成 codex-skills**

Run: `make gen`
Expected: `codex-skills/*/SKILL.md` 里 `${CODEX_PLUGIN_ROOT}/bin/brk`（脚本自动透传新名 + 替换 root 变量）。
Run: `grep -rn "brk-darwin-arm64" plugins/breakpoint/ || echo "无残留"`
Expected: `无残留`。

- [ ] **Step 3: 更新 packaging 测试断言**

`tests/packaging.rs` 现 `commands_frontmatter_and_invocation_policy` 里（约 :53-54）：
```rust
        assert!(
            body.contains("${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64"),
```
改为断言新入口 + 无旧残留：
```rust
        assert!(
            body.contains("${CLAUDE_PLUGIN_ROOT}/bin/brk ")
                || body.contains("${CLAUDE_PLUGIN_ROOT}/bin/brk\n"),
            "{name}: must invoke the bin/brk launcher"
        );
        assert!(
            !body.contains("brk-darwin-arm64"),
            "{name}: must not reference the old flat binary name"
        );
```
（`bin/brk ` 带尾空格/换行以避免误配 `bin/brk-...`；旧名已删，此处双保险。）

- [ ] **Step 4: 跑 packaging 测试**

Run: `cargo test --test packaging 2>&1 | tail -8`
Expected: 全绿（命令壳断言 + gen 无漂移断言）。

- [ ] **Step 5: Commit**

```bash
git add plugins/breakpoint/commands plugins/breakpoint/codex-skills tests/packaging.rs
git commit -m "feat(packaging): command shells invoke bin/brk launcher; update packaging tests

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: launcher 行为测试 + 布局断言 + 终验

**Files:**
- Modify: `tests/packaging.rs`（launcher/布局断言）

- [ ] **Step 1: packaging 测试加布局 + launcher 断言**

`tests/packaging.rs` 追加一个测试（`use std::path::Path;` 已在文件内则复用）：
```rust
#[test]
fn binary_layout_and_launcher() {
    // launcher 存在且可执行
    let launcher = Path::new("plugins/breakpoint/bin/brk");
    assert!(launcher.exists(), "bin/brk launcher must exist");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(launcher).unwrap().permissions().mode();
        assert!(mode & 0o111 != 0, "bin/brk must be executable, mode={mode:o}");
    }
    // darwin/aarch64 真二进制存在
    assert!(
        Path::new("plugins/breakpoint/bin/darwin/aarch64/brk").exists(),
        "shipped darwin/aarch64 binary must exist"
    );
    // 旧扁平二进制已删
    assert!(
        !Path::new("plugins/breakpoint/bin/brk-darwin-arm64").exists(),
        "old flat binary must be removed"
    );
    // launcher 内容正确分派（uname 映射 + 127 报错）
    let src = std::fs::read_to_string(launcher).unwrap();
    assert!(src.contains("uname -s") && src.contains("uname -m"), "launcher must probe os/arch");
    assert!(src.contains("exit 127"), "launcher must exit 127 on resolution failure");
}
```

- [ ] **Step 2: launcher 行为 shell 测试（缺失→127、命中→透传）**

`tests/packaging.rs` 追加一个用 `std::process::Command` 驱动 launcher 的测试：
```rust
#[test]
fn launcher_resolves_and_errors() {
    use std::process::Command;
    // 命中：当前平台真二进制存在 → launcher --help 成功
    let ok = Command::new("plugins/breakpoint/bin/brk").arg("--help").output().unwrap();
    assert!(ok.status.success(), "launcher --help should succeed on this platform");
    assert!(String::from_utf8_lossy(&ok.stdout).contains("Usage") || !ok.stdout.is_empty(),
        "launcher must forward to brk and produce output");
    // 缺失：临时挪走真二进制 → 127 + 报错
    let real = "plugins/breakpoint/bin/darwin/aarch64/brk";
    let bak = "plugins/breakpoint/bin/darwin/aarch64/brk.bak";
    std::fs::rename(real, bak).unwrap();
    let miss = Command::new("plugins/breakpoint/bin/brk").arg("review").output().unwrap();
    std::fs::rename(bak, real).unwrap(); // 无论断言与否都还原
    assert_eq!(miss.status.code(), Some(127), "missing binary → exit 127");
    assert!(String::from_utf8_lossy(&miss.stderr).contains("no binary for"),
        "missing binary → clear error");
}
```
> 注：此测试假设跑在 darwin/aarch64（本机）。`launcher_resolves_and_errors` 依赖 `darwin/aarch64/brk`
> 存在——CI 若在别的平台跑需 gate；scope A 只 darwin，暂不 gate（本机 make verify 用）。

- [ ] **Step 3: 跑全 packaging 测试**

Run: `cargo test --test packaging 2>&1 | tail -10`
Expected: 全绿（含新 `binary_layout_and_launcher`、`launcher_resolves_and_errors`）。

- [ ] **Step 4: 终验 make verify + 无残留 + 全套**

Run:
```bash
make verify
grep -rn "brk-darwin-arm64" . --include="*.md" --include="*.rs" --include="Makefile" 2>/dev/null | grep -v "\.doc/" || echo "无残留(排除 .doc 历史)"
cargo test --quiet 2>&1 | grep "test result" | tail -6
```
Expected: `make verify` 绿（gen 无漂移 + packaging）；无 `brk-darwin-arm64` 残留（.doc/ 历史文档除外）；全套测试绿。

- [ ] **Step 5: Commit**

```bash
git add tests/packaging.rs
git commit -m "test(packaging): launcher behavior + layout assertions (resolve/127/no-residual)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Self-Review 记录（写计划者自查）

- **Spec 覆盖**：布局=T1;launcher=T2;瘦身=T1;接线(命令壳/gen/Makefile)=T1+T2+T3;测试(布局/launcher/无残留)=T3+T4。§8 决策①-⑤ 全落。
- **占位符**：无 TBD。opt-level s/z 实测取小=T1 Step4 明确命令。
- **一致性**：路径 `bin/darwin/aarch64/brk`、launcher `bin/brk`、退出码 127 跨任务一致。
- **风险点**：
  - `sed -i ''`（T1 Step4）是 macOS BSD sed 语法（本机 darwin，OK）。
  - `make build` 会重写 `darwin/aarch64/brk`——T2/T3/T4 不重 build，故二进制稳定;若中途重 build 记得它仍在新路径。
  - `launcher_resolves_and_errors` 假设本机 darwin/aarch64——scope A 只此平台，`make verify` 本机跑;未来加平台时该测试需 `#[cfg]`/gate（注已写）。
  - git 对二进制的 move：用 `make build` 直接产新路径 + `git rm` 旧文件（非 `git mv`），因二进制内容也变（瘦身）。
