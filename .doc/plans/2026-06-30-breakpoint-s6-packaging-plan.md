# breakpoint S6 — 跨宿主插件打包 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `brk` 二进制打包成「一个仓 = marketplace」的跨宿主插件：Claude Code 原生 slash 命令（`commands/` 手写真相源）+ Codex skills（`codex-skills/` 由 `scripts/gen-codex-skills` 确定性生成），二进制预编译提交，防漂移由 Rust 集成测试守。

**Architecture:** marketplace 结构铺在当前 Rust 仓根（与 `Cargo.toml`/`src/` 平级，符合设计 §15「一个仓=marketplace」）。`commands/{review,status,result}.md` 是唯一手写真相源；`gen-codex-skills`（bash）把它机械转成 `codex-skills/<name>/SKILL.md` 并提交。命令正文用 `${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64` 调二进制。所有打包产物（清单 JSON、命令 md、生成的 skills、预编译二进制、Makefile、脚本）入 git；防漂移＝集成测试跑脚本到临时目录、与提交版逐字节比对。

**Tech Stack:** JSON（清单）、Markdown+YAML frontmatter（命令/skill）、Bash（codegen）、Make（构建/生成入口）、Rust 集成测试（`std::process::Command` + `tempfile`，已有 dev-dep）。**零新增 crate 依赖**。

## Global Constraints

- **仓内布局＝铺仓根**：`.claude-plugin/marketplace.json`、`plugins/breakpoint/{.claude-plugin/plugin.json、.codex-plugin/plugin.json、commands/、codex-skills/、bin/}`、`scripts/gen-codex-skills`、`Makefile` 全部与 `Cargo.toml`/`src/` 平级，**不进子目录隔离**。
- **crate 已更名**：lib/package 名 `breakpoint`，二进制名仍 `brk`（commit `a88f788`）。本阶段**不改 `src/`**（除非测试需要），引擎层冻结（`src/tools/`、`src/provider/tool_loop.rs`、`src/provider/backends.rs` 不碰）。
- **命名一致**：marketplace.json `name` = "breakpoint"，plugin.json `name` = "breakpoint" → slash namespace `/breakpoint:<cmd>`，安装 `breakpoint@breakpoint`。
- **version 只在 plugin.json 设**（"1.0.0"），marketplace 条目**不重复** version（plugin.json 是 strict 权威源）。
- **owner/author** = `{ "name": "S.Kai", "email": "laiwei@me.com" }`。
- **命令 frontmatter 仅用官方 schema 字段**：`description`（必填）、`disable-model-invocation`。**不用** `argument-hint`/`allowed-tools`（不在当前官方 schema）。参数用 `$ARGUMENTS`。
- **模型调用策略**：`review` 计费重 → `disable-model-invocation: true`（仅用户触发）；`status`/`result` 轻读 → 不设该字段（允许模型触发）。
- **bin 引用**：命令正文用 `${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64`（v1 仅 darwin-arm64）。
- **codex 转换是 best-effort**：`gen-codex-skills` 的 frontmatter 映射与 `${CLAUDE_PLUGIN_ROOT}`→`${CODEX_PLUGIN_ROOT}` 变量替换是镜像假设，真机 codex `render_command_skill` 细节（设计 §18.2）作 **carry** 待验；本阶段只保证「脚本确定性 + 与提交版同步」。
- **二进制**：`make build`＝`cargo build --release --target aarch64-apple-darwin` → cp 到 `plugins/breakpoint/bin/brk-darwin-arm64` 并提交；`pid_alive` 跨平台 carry 暂不动。
- **零新增 crate 依赖**；**不碰 `.gitignore` 的密钥规则**（`secrets.toml`/`*.key` 仍忽略，打包产物不含密钥）。
- 所有集成测试进 `tests/packaging.rs`，纳入 `cargo test`。

---

## File Structure

```
.claude-plugin/
  marketplace.json                       # T1 — Claude/Codex 市场入口（仓根）
plugins/breakpoint/
  .claude-plugin/plugin.json             # T1 — 插件清单（Codex fallback 亦读）
  .codex-plugin/plugin.json              # T1 — {name,version,skills:"codex-skills"}
  commands/                              # T2 — ★唯一手写真相源（Claude slash）
    review.md  status.md  result.md
  codex-skills/                         # T3 — ★由 gen-codex-skills 生成（Codex slash）
    review/SKILL.md  status/SKILL.md  result/SKILL.md
  bin/brk-darwin-arm64                   # T4 — 预编译二进制（cargo build --release 产物）
scripts/gen-codex-skills                 # T3 — bash 确定性 codegen
Makefile                                 # T4 — make gen / make build / make all
tests/packaging.rs                       # T1–T4 — 打包产物校验 + 防漂移集成测试
```

Task 边界理由：T1 清单（JSON 契约，reviewer 可独立否决字段错误）；T2 命令真相源（frontmatter 策略，独立于 codegen）；T3 codegen + 生成产物（脚本逻辑 + 防漂移，依赖 T2 的 commands/）；T4 构建系统 + 二进制（Makefile + 预编译产物，端到端冒烟）。

---

### Task 1: 插件清单三件（marketplace.json + plugin.json + .codex-plugin/plugin.json）

**Files:**
- Create: `.claude-plugin/marketplace.json`
- Create: `plugins/breakpoint/.claude-plugin/plugin.json`
- Create: `plugins/breakpoint/.codex-plugin/plugin.json`
- Test: `tests/packaging.rs`

**Interfaces:**
- Consumes: 无（首个 task）。
- Produces: `plugins/breakpoint/` 目录树根；marketplace `name`="breakpoint"、plugin `name`="breakpoint"、`.codex-plugin` 的 `skills`="codex-skills"（T3 生成的 skills 落点）。

- [ ] **Step 1: 写失败测试**

在 `tests/packaging.rs`（新建文件）写：

```rust
use std::path::Path;

fn read(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

fn json(p: &str) -> serde_json::Value {
    serde_json::from_str(&read(p)).unwrap_or_else(|e| panic!("parse {p}: {e}"))
}

#[test]
fn manifests_valid_and_named() {
    // marketplace.json
    let m = json(".claude-plugin/marketplace.json");
    assert_eq!(m["name"], "breakpoint", "marketplace name");
    assert_eq!(m["owner"]["name"], "S.Kai", "owner.name");
    let plugins = m["plugins"].as_array().expect("plugins array");
    let entry = plugins.iter().find(|p| p["name"] == "breakpoint").expect("breakpoint entry");
    assert_eq!(entry["source"], "./plugins/breakpoint", "relative source");
    assert!(entry.get("version").is_none(), "version must live in plugin.json only");

    // plugin.json
    let p = json("plugins/breakpoint/.claude-plugin/plugin.json");
    assert_eq!(p["name"], "breakpoint", "plugin name → namespace");
    assert_eq!(p["version"], "1.0.0", "plugin version is authoritative");
    assert_eq!(p["author"]["email"], "laiwei@me.com");

    // .codex-plugin/plugin.json
    let c = json("plugins/breakpoint/.codex-plugin/plugin.json");
    assert_eq!(c["name"], "breakpoint");
    assert_eq!(c["skills"], "codex-skills", "codex skills dir → no Claude collision");
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --test packaging manifests_valid_and_named`
Expected: FAIL（`read .claude-plugin/marketplace.json: No such file`）

- [ ] **Step 3: 写 `.claude-plugin/marketplace.json`**

```json
{
  "name": "breakpoint",
  "owner": {
    "name": "S.Kai",
    "email": "laiwei@me.com"
  },
  "description": "模型无关的对抗式代码/文档评审工具",
  "plugins": [
    {
      "name": "breakpoint",
      "source": "./plugins/breakpoint",
      "description": "对抗式评审：多模型扇出 + 工具循环 + 定向核实，输出 verdict/findings"
    }
  ]
}
```

- [ ] **Step 4: 写 `plugins/breakpoint/.claude-plugin/plugin.json`**

```json
{
  "name": "breakpoint",
  "description": "模型无关的对抗式代码/文档评审工具（brk）",
  "version": "1.0.0",
  "author": {
    "name": "S.Kai",
    "email": "laiwei@me.com"
  },
  "license": "MIT"
}
```

- [ ] **Step 5: 写 `plugins/breakpoint/.codex-plugin/plugin.json`**

```json
{
  "name": "breakpoint",
  "version": "1.0.0",
  "skills": "codex-skills"
}
```

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test --test packaging manifests_valid_and_named`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add .claude-plugin/marketplace.json plugins/breakpoint/.claude-plugin/plugin.json plugins/breakpoint/.codex-plugin/plugin.json tests/packaging.rs
git commit -m "feat(s6): plugin manifests (marketplace + plugin + codex-plugin)"
```

---

### Task 2: commands/ 三个真相源（review / status / result）

**Files:**
- Create: `plugins/breakpoint/commands/review.md`
- Create: `plugins/breakpoint/commands/status.md`
- Create: `plugins/breakpoint/commands/result.md`
- Test: `tests/packaging.rs`（追加）

**Interfaces:**
- Consumes: T1 的 `plugins/breakpoint/` 目录。
- Produces: 命令真相源；frontmatter 契约（`review` 设 `disable-model-invocation: true`，`status`/`result` 不设）；正文统一用 `${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64 <sub> $ARGUMENTS`。T3 的 `gen-codex-skills` 消费这三个文件。

- [ ] **Step 1: 写失败测试**

在 `tests/packaging.rs` 追加：

```rust
fn frontmatter<'a>(body: &'a str, key: &str) -> Option<String> {
    // 取前两个 "---" 之间、形如 "key: value" 的值
    let mut lines = body.lines();
    assert_eq!(lines.next(), Some("---"), "frontmatter must start with ---");
    for l in lines {
        if l == "---" { break; }
        if let Some(v) = l.strip_prefix(&format!("{key}:")) {
            return Some(v.trim().to_string());
        }
    }
    None
}

#[test]
fn commands_frontmatter_and_invocation_policy() {
    for name in ["review", "status", "result"] {
        let body = read(&format!("plugins/breakpoint/commands/{name}.md"));
        assert!(frontmatter(&body, "description").is_some(), "{name}: description required");
        assert!(
            body.contains("${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64"),
            "{name}: must invoke bundled binary via plugin root"
        );
        assert!(body.contains("$ARGUMENTS"), "{name}: must forward $ARGUMENTS");
    }
    // review 计费重 → 禁模型自动调用
    let review = read("plugins/breakpoint/commands/review.md");
    assert_eq!(frontmatter(&review, "disable-model-invocation").as_deref(), Some("true"),
        "review must disable model invocation");
    // status/result 轻读 → 允许模型触发（不设该字段）
    for name in ["status", "result"] {
        let body = read(&format!("plugins/breakpoint/commands/{name}.md"));
        assert!(frontmatter(&body, "disable-model-invocation").is_none(),
            "{name} must allow model invocation");
    }
}
```

- [ ] **Step 2: 运行测试确认失败**

Run: `cargo test --test packaging commands_frontmatter_and_invocation_policy`
Expected: FAIL（读文件失败）

- [ ] **Step 3: 写 `plugins/breakpoint/commands/review.md`**

```markdown
---
description: 对当前改动或指定目标做模型无关的对抗式评审（计费重操作，仅用户触发）
disable-model-invocation: true
---

运行 breakpoint 对抗式评审。把用户在命令后给出的全部参数原样转发给 `brk review`（例如目标路径、`--base <分支>`、`--mode code|doc`、`--background` 等；为空则评审当前工作树改动）：

```bash
${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64 review $ARGUMENTS
```

把输出的 `verdict` 与 `findings` 原样呈现给用户。这是计费重操作，未经用户要求不要自动重复调用；若用户传了 `--background`，改用 `/breakpoint:status` 与 `/breakpoint:result` 跟进。
```

- [ ] **Step 4: 写 `plugins/breakpoint/commands/status.md`**

```markdown
---
description: 查看 breakpoint 后台评审作业的状态
---

列出当前工作区的 breakpoint 评审作业状态（参数可选，原样转发给 `brk status`）：

```bash
${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64 status $ARGUMENTS
```

把作业的状态/进度原样呈现给用户。
```

- [ ] **Step 5: 写 `plugins/breakpoint/commands/result.md`**

```markdown
---
description: 取回 breakpoint 评审作业的结果
---

取回 breakpoint 评审结果（参数为 job id，省略则取最近一次；原样转发给 `brk result`）：

```bash
${CLAUDE_PLUGIN_ROOT}/bin/brk-darwin-arm64 result $ARGUMENTS
```

把 `verdict` 与 `findings` 原样呈现给用户。
```

- [ ] **Step 6: 运行测试确认通过**

Run: `cargo test --test packaging commands_frontmatter_and_invocation_policy`
Expected: PASS

- [ ] **Step 7: Commit**

```bash
git add plugins/breakpoint/commands/ tests/packaging.rs
git commit -m "feat(s6): commands/ truth source (review disable-model-invocation; status/result open)"
```

---

### Task 3: gen-codex-skills（bash 确定性 codegen）+ 生成 codex-skills/ + 防漂移测试

**Files:**
- Create: `scripts/gen-codex-skills`
- Create: `plugins/breakpoint/codex-skills/review/SKILL.md`（脚本生成）
- Create: `plugins/breakpoint/codex-skills/status/SKILL.md`（脚本生成）
- Create: `plugins/breakpoint/codex-skills/result/SKILL.md`（脚本生成）
- Test: `tests/packaging.rs`（追加）

**Interfaces:**
- Consumes: T2 的 `commands/*.md`。
- Produces: `codex-skills/<name>/SKILL.md`；转换规则＝frontmatter 取 `description` + 注入 `name: <stem>`（剥离 `disable-model-invocation`），body 做 `${CLAUDE_PLUGIN_ROOT}`→`${CODEX_PLUGIN_ROOT}` 替换。脚本接受可选 `OUT_ROOT` 参数（默认 `plugins/breakpoint/codex-skills`），供防漂移测试生成到临时目录。

- [ ] **Step 1: 写脚本 `scripts/gen-codex-skills`**

```bash
#!/usr/bin/env bash
# gen-codex-skills — 从 Claude commands/（唯一手写真相源）确定性生成 Codex skills。
# best-effort 镜像 codex 内置 render_command_skill（真机转换细节见设计 §18.2，作 carry）：
#   frontmatter: 注入 name=<命令文件 stem> + 透传 description；剥离 Claude-only 的 disable-model-invocation
#   body:        ${CLAUDE_PLUGIN_ROOT} → ${CODEX_PLUGIN_ROOT}（codex 变量名待真机验证）
# 用法: scripts/gen-codex-skills [OUT_ROOT]
#   OUT_ROOT 默认 plugins/breakpoint/codex-skills；防漂移测试传临时目录做逐字节比对。
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
CMD_DIR="$ROOT/plugins/breakpoint/commands"
OUT_ROOT="${1:-$ROOT/plugins/breakpoint/codex-skills}"

for cmd in "$CMD_DIR"/*.md; do
  name="$(basename "$cmd" .md)"
  desc="$(awk '/^---$/{n++; next} n==1 && /^description:/{sub(/^description:[[:space:]]*/,""); print; exit}' "$cmd")"
  body="$(awk 'BEGIN{n=0} /^---$/{n++; next} n>=2{print}' "$cmd")"
  out_dir="$OUT_ROOT/$name"
  mkdir -p "$out_dir"
  {
    printf -- '---\n'
    printf 'name: %s\n' "$name"
    printf 'description: %s\n' "$desc"
    printf -- '---\n'
    printf '%s\n' "$body" | sed 's/\${CLAUDE_PLUGIN_ROOT}/${CODEX_PLUGIN_ROOT}/g'
  } > "$out_dir/SKILL.md"
done
```

- [ ] **Step 2: 赋可执行位并生成**

Run:
```bash
chmod +x scripts/gen-codex-skills
scripts/gen-codex-skills
```
Expected: 无输出、退出 0；生成 `plugins/breakpoint/codex-skills/{review,status,result}/SKILL.md`。

- [ ] **Step 3: 写防漂移测试（先确认它对当前生成版通过，再作为护栏）**

在 `tests/packaging.rs` 追加：

```rust
use std::process::Command;

fn list_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap()
            .map(|e| e.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() { walk(&p, out); } else { out.push(p); }
        }
    }
    walk(root, &mut out);
    out
}

#[test]
fn codex_skills_in_sync_with_commands() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let status = Command::new("bash")
        .arg("scripts/gen-codex-skills")
        .arg(tmp.path())
        .status()
        .expect("run gen-codex-skills");
    assert!(status.success(), "gen-codex-skills exit non-zero");

    let committed = Path::new("plugins/breakpoint/codex-skills");
    let gen_files = list_files(tmp.path());
    let com_files = list_files(committed);

    // 文件集合（相对路径）一致
    let rel = |base: &Path, fs: &[std::path::PathBuf]| -> Vec<String> {
        fs.iter().map(|p| p.strip_prefix(base).unwrap().to_string_lossy().into_owned()).collect()
    };
    assert_eq!(rel(committed, &com_files), rel(tmp.path(), &gen_files),
        "codex-skills 文件集合漂移：commands/ 改了未重跑 gen-codex-skills？");

    // 内容逐字节一致
    for r in rel(committed, &com_files) {
        let a = std::fs::read(committed.join(&r)).unwrap();
        let b = std::fs::read(tmp.path().join(&r)).unwrap();
        assert_eq!(a, b, "codex-skills/{r} 内容漂移：commands/ 改了未重跑 gen-codex-skills");
    }
}

#[test]
fn codex_skills_have_name_and_no_claude_var() {
    for name in ["review", "status", "result"] {
        let s = read(&format!("plugins/breakpoint/codex-skills/{name}/SKILL.md"));
        assert!(s.contains(&format!("name: {name}")), "{name}: skill must declare name");
        assert!(!s.contains("CLAUDE_PLUGIN_ROOT"),
            "{name}: CLAUDE_PLUGIN_ROOT must be rewritten to CODEX_PLUGIN_ROOT");
        assert!(s.contains("${CODEX_PLUGIN_ROOT}/bin/brk-darwin-arm64"),
            "{name}: codex skill must invoke bundled binary");
    }
}
```

- [ ] **Step 4: 运行测试确认通过**

Run: `cargo test --test packaging`
Expected: PASS（`manifests_*`、`commands_*`、`codex_skills_in_sync_with_commands`、`codex_skills_have_name_and_no_claude_var` 全绿）

- [ ] **Step 5: Commit**

```bash
git add scripts/gen-codex-skills plugins/breakpoint/codex-skills/ tests/packaging.rs
git commit -m "feat(s6): gen-codex-skills codegen + generated codex-skills + drift guard test"
```

---

### Task 4: Makefile + 预编译二进制 bin/brk-darwin-arm64

**Files:**
- Create: `Makefile`
- Create: `plugins/breakpoint/bin/brk-darwin-arm64`（`cargo build --release` 产物）
- Test: `tests/packaging.rs`（追加）

**Interfaces:**
- Consumes: T1–T3 全部产物。
- Produces: `make gen`（重生成 codex-skills）/ `make build`（产二进制并 cp 到 bin/）/ `make all`；预编译 `bin/brk-darwin-arm64`。

- [ ] **Step 1: 写 `Makefile`**

```makefile
BIN := plugins/breakpoint/bin/brk-darwin-arm64
TARGET := aarch64-apple-darwin

.PHONY: all gen build verify

all: build gen

# 从 commands/ 真相源重生成 codex-skills/
gen:
	bash scripts/gen-codex-skills

# 编译 darwin-arm64 release 二进制并放入插件 bin/
build:
	cargo build --release --target $(TARGET)
	mkdir -p $(dir $(BIN))
	cp target/$(TARGET)/release/brk $(BIN)

# 本地等价 CI 防漂移：重生成后无 git 改动
verify: gen
	git diff --exit-code plugins/breakpoint/codex-skills/
	cargo test --test packaging
```

- [ ] **Step 2: 写存在性 + 冒烟测试（失败先行）**

在 `tests/packaging.rs` 追加：

```rust
#[test]
fn binary_present_and_executable() {
    let bin = Path::new("plugins/breakpoint/bin/brk-darwin-arm64");
    let meta = std::fs::metadata(bin)
        .unwrap_or_else(|e| panic!("bin missing (run `make build`): {e}"));
    assert!(meta.len() > 0, "binary is empty");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert!(meta.permissions().mode() & 0o111 != 0, "binary not executable");
    }
}

// 真机执行冒烟：仅在 darwin-arm64 本机有意义，跨平台 cargo test 跳过。
#[test]
#[cfg_attr(not(all(target_os = "macos", target_arch = "aarch64")), ignore)]
fn binary_runs_help() {
    let out = Command::new("plugins/breakpoint/bin/brk-darwin-arm64")
        .arg("--help")
        .output()
        .expect("spawn brk");
    assert!(out.status.success(), "brk --help failed");
}
```

- [ ] **Step 3: 运行测试确认失败**

Run: `cargo test --test packaging binary_present_and_executable`
Expected: FAIL（`bin missing (run make build)`）

- [ ] **Step 4: 构建二进制**

Run: `make build`
Expected: 编译完成；`plugins/breakpoint/bin/brk-darwin-arm64` 存在、可执行。

- [ ] **Step 5: 运行测试确认通过**

Run: `cargo test --test packaging`
Expected: PASS（`binary_present_and_executable` 绿；`binary_runs_help` 在本机绿）

- [ ] **Step 6: Commit（含二进制）**

```bash
git add Makefile plugins/breakpoint/bin/brk-darwin-arm64 tests/packaging.rs
git commit -m "feat(s6): Makefile (gen/build/verify) + prebuilt brk-darwin-arm64"
```

---

## 验证（全 task 完成后）

- [ ] `cargo test`：原有 198 + packaging 新增全绿（`binary_runs_help` 本机非 ignored）。
- [ ] `cargo clippy --all-targets`：干净（packaging.rs 无 warning）。
- [ ] `make verify`：`git diff --exit-code codex-skills/` 无改动 + packaging 测试绿（本地防漂移）。
- [ ] 手动冒烟（可选，需 Claude Code）：`/plugin marketplace add .` → `/plugin install breakpoint@breakpoint` → `/breakpoint:review --help` 透传成功。
- [ ] openssl-sys 仍缺席（本阶段不动依赖，惯性校验）。

## Carry（记入 §18 / 账本）

1. **codex 真机转换规则未验**：`${CODEX_PLUGIN_ROOT}` 变量名、codex skill frontmatter schema、`render_command_skill` 确切映射均为镜像假设（设计 §18.2）。需真机 codex 装一次校准。
2. **跨平台二进制**：v1 仅 darwin-arm64。linux/win 二进制 + 命令正文按平台 dispatch bin（v1 硬编码 `brk-darwin-arm64`）+ `pid_alive` 的 `libc::__error()` cfg 分支，并列后补。
3. **bash awk/sed 的 BSD↔GNU 差异**：`gen-codex-skills` 在 darwin 开发机产出；若未来上 linux CI 跑防漂移，需校验 awk/sed 行为一致或改用更可移植写法。
4. **marketplace.json Codex 发现常量**（设计 §18.1）：是否需独立 `.codex-plugin/marketplace.json`，待真机确认。

---

## Self-Review

- **Spec coverage**：§15 目录结构全覆盖（marketplace.json/plugin.json/.codex-plugin/commands/codex-skills/bin/scripts/Makefile）✓；§15 slash 命令策略（review 禁模型/status·result 允）→ T2 ✓；codegen 防双维护 → T3 ✓；预编译二进制「宿主不构建」→ T4 ✓；防漂移（设计的 `make gen` + CI diff）→ T3 集成测试 + T4 `make verify` ✓。
- **Placeholder scan**：每个文件给出完整内容，无 TBD/省略 ✓。
- **Type/名一致**：marketplace name=plugin name="breakpoint"、`source`="./plugins/breakpoint"、`skills`="codex-skills"、bin="brk-darwin-arm64"、变量 `${CLAUDE_PLUGIN_ROOT}`→`${CODEX_PLUGIN_ROOT}` 全程一致 ✓。version 仅 plugin.json（T1 测试断言 marketplace 条目无 version）✓。
- **零新增依赖**：仅用 serde_json（已有）、tempfile（dev-dep 已有）、std ✓。
