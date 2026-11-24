# M11 · doc 评审守卫化（方向性 drift 指令）实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans (内联执行，Q6 决议). Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 doc 评审在检索代码对照文档时按文档时间性定 drift 方向——消除「待实施 spec 被当 as-built 审」的方向性误报，同时不漏报前置失效/声称已实现类真 drift。

**Architecture:** 纯 prompt 批次：`adversarial-doc-bundle.md` 注入 `<temporal_stance>` 节 + `adversarial-refute-doc.md` 注入精简 Temporal check（双防线一致）+ 模板断言测试防漂移 + `registry.rs:8` 过时注释修正；有效性由三靶 live dogfood 判定。Spec：`.doc/design/2026-07-02-breakpoint-doc-guard-design.md`。

**Tech Stack:** prompt 模板（include_str! 内嵌）、Rust 断言测试、brk live dogfood。

## Global Constraints

- 分支 `feat/doc-guard` 自 dev；commit trailer `Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>`。
- 指令语义以 spec §2 基线为准（措辞可打磨、四类判定不可增删）：待建产出→豁免；声称已存在→验证不符报 drift；依赖前置缺失→必报；待建但代码已有冲突→报 drift。
- 不新增 lens、不加 CLI flag、不动引擎/管线（registry 注释除外）；severity 不为 drift 单开规则。
- refute-doc 末行预存的无配对 `</output>` 标签**不动**（预存杂质，避免混入本批 A/B）。
- dogfood 前必须 `make build` 重编（模板 include_str! 编译期内嵌）。

---

### Task 1: 双模板注入 + 断言测试（TDD）

**Files:**
- Modify: `src/prompt/templates/adversarial-doc-bundle.md`（`</tools>` 之后插入新节）
- Modify: `src/prompt/templates/adversarial-refute-doc.md`（`<method>` 节内追加）
- Test: `src/prompt/mod.rs` tests 模块

**Interfaces:** 无代码接口；模板新增 `<temporal_stance>` 节与 `Temporal check:` 段，Task 3 dogfood 消费其行为。

- [ ] **Step 1: 写失败测试**（`src/prompt/mod.rs` tests 模块末尾，紧邻 exhaustiveness 测试）

```rust
    #[test]
    fn doc_templates_carry_temporal_stance() {
        // spec §2：方向性 drift 指令双注入（主评审 + refute 第二道防线一致）
        assert!(
            include_str!("templates/adversarial-doc-bundle.md").contains("<temporal_stance>"),
            "doc-bundle 缺 temporal_stance 节"
        );
        assert!(
            include_str!("templates/adversarial-refute-doc.md").contains("Temporal check:"),
            "refute-doc 缺 Temporal check 段"
        );
    }
```

- [ ] **Step 2: 确认失败**

Run: `cargo test --lib prompt::tests::doc_templates_carry_temporal_stance`
Expected: FAIL（两个 assert 均不成立）

- [ ] **Step 3: 注入 doc-bundle**——`adversarial-doc-bundle.md` 的 `</tools>` 行之后插入：

```
<temporal_stance>
Documents differ in temporal nature: some describe work TO BE built (specs, designs,
plans), others describe an EXISTING system (READMEs, API docs), and many mix both.
Before reporting any finding that rests on a mismatch between this document and code
you retrieved, classify — per claim, from the document's own wording — how the
document presents that content:
- Planned deliverable (the document says it WILL be built): its absence from the code
  is NOT a defect. Never report "the code does not have X" when X is what the document
  sets out to build.
- Claimed as existing (the document asserts X already exists or is implemented):
  verify in the code; if absent or materially different, report the drift.
- Stated dependency or precondition (the document RELIES on existing module/function/
  behavior X): if X does not exist in the code, that IS a defect — the document's
  hidden assumption fails and implementation will hit a wall.
- Planned deliverable that conflicts with something already in the code: report the
  drift — the document is stale or implementation has diverged.
</temporal_stance>
```

- [ ] **Step 4: 注入 refute-doc**——`<method>` 节内、既有 "Attack the claim: …" 段之后追加一段：

```
Temporal check: if the finding faults the code for lacking something, first classify
from the document's own wording whether that thing is a planned deliverable (to be
built — then the finding is REFUTED: absence of planned work is not a defect), a
claim of existing implementation, or a stated dependency/precondition (absence of
either IS a real defect — uphold).
```

- [ ] **Step 5: 确认通过 + 全量回归**

Run: `cargo test`
Expected: 全绿（含既有 exhaustiveness 断言不受影响）

- [ ] **Step 6: Commit**

```bash
git add src/prompt/templates/adversarial-doc-bundle.md src/prompt/templates/adversarial-refute-doc.md src/prompt/mod.rs
git commit -m "feat(prompt): temporal stance for doc reviews — drift direction by document nature

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 2: registry.rs 过时注释修正

**Files:**
- Modify: `src/provider/registry.rs:8`

**Interfaces:** 纯注释，无接口。

- [ ] **Step 1: 替换注释**——第 8 行：

```rust
// 旧：
/// code 路径填 `ToolLoopProvider` 实例、doc 路径填 `DirectProvider` 实例，二者皆 `ReviewProvider`。
// 新：
/// 生产装配（main.rs）对 code/doc 两模式统一填 `ToolLoopProvider`（doc 评审同样具备
/// read_file/search_repo 检索能力）；`DirectProvider` 是无工具单发的备选实现，当前无生产
/// 使用方。二者皆 `ReviewProvider`。
```

- [ ] **Step 2: 验证 + Commit**

Run: `cargo build 2>&1 | tail -1`（注释改动，编译即验证）

```bash
git add src/provider/registry.rs
git commit -m "docs(provider): fix stale registry comment — doc mode uses ToolLoopProvider too

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

---

### Task 3: 三靶 live dogfood（交互，我执行、逐靶裁决）

**Files:** 无代码；产出裁决记录（scratchpad）。

- [ ] **Step 1: 重编** `make build`（模板编译期内嵌，不重编 dogfood 无效）。

- [ ] **Step 2: 靶 1 回归（消误报 A/B）**——在仓库根（有代码可检索）：

```bash
export BRK_STATE_DIR=<scratchpad>/m11-t1-state
plugins/breakpoint/bin/brk review --mode doc .doc/design/2026-07-01-breakpoint-loop-lgtm-design.md --models Kimi --background
# 轮询 status 至 done，取 result
```

判据：M7 dogfood 原 3 条方向性误报（CRITICAL "--background 未实现"、HIGH "--dismissed/brk init 代码中不存在"、HIGH "Finding/RunMeta 字段缺失"）**消失/降级/被 refute**。注意：其中 --dismissed/init/字段三项现已真实实现（M7 已落码），故本靶主要验 CRITICAL 类推理路径 + 整体不再出现「设计项未实现=缺陷」模式的新误报。

- [ ] **Step 3: 靶 2 防矫枉过正 + 靶 3 真缺陷**——scratchpad 建靶工作区：

```bash
WS=<scratchpad>/m11-t2; mkdir -p $WS/src && cd $WS
cat > src/store.py <<'PY'
def save_record(record):
    # writes one record to the store
    return _append(record)

def _append(record):
    return True
PY
cat > migration-spec.md <<'MD'
# Store Migration Spec

## Plan
We WILL build a new `bulk_import(records)` API in src/store.py that batches
writes in one transaction. It does not exist yet; this spec defines it.

## Existing foundation
This design RELIES on the existing `validate_record(record)` function in
src/store.py to reject malformed records before import.

## Current state
Deduplication is already implemented in src/store.py: `save_record` checks
for an existing record with the same key before appending.

## Rollout
The rollout finishes when the migration feels stable.
MD
plugins/breakpoint/bin/brk review --mode doc migration-spec.md --models Kimi --background
```

判据（四条各验一类）：
1. `bulk_import` 待建产出、代码没有 → **不得**出现「代码缺 bulk_import」类 finding（豁免生效）。
2. `validate_record` 声称依赖的既有前置、代码没有 → **必须**报（前置失效）。
3. 「Deduplication is already implemented」声称已实现、代码 `save_record` 根本没查重 → **必须**报（声称已实现的 drift）。
4. 「feels stable」不可测成功标准 → **仍然**报（D4 真缺陷不受影响）。

- [ ] **Step 4: 裁决与收尾**——逐靶记录 PASS/FAIL;若措辞需迭代,改模板 → `cargo test` → `make build` → 重跑失败靶(每轮一个变量);全过后把重编二进制入 commit：

```bash
git add plugins/breakpoint/bin/darwin/aarch64/brk
git commit -m "build(doc-guard): rebuild binary with temporal stance templates

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>"
```

清理 scratch 工作区与 state。

---

## Self-Review 记录

- **Spec 覆盖**：§2.1→T1S3；§2.2→T1S4；§3→T2；§4 断言→T1S1、三靶→T3（靶 2 文档同时承载靶 3 的真缺陷句，与 spec §4「靶 2 文档内自带」一致）。§6 非目标无需任务。无缺口。
- **占位符**：`<scratchpad>` 为会话 scratchpad 路径的指代（执行时代入实际路径），非 TBD；余无。
- **一致性**：`<temporal_stance>`/`Temporal check:` 命名在测试断言与模板文本间一致；四类判定 T1 与 T3 判据一一对应（豁免/前置/声称/真缺陷）。
