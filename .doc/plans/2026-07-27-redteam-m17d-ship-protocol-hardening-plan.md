# M17-d ship 协议硬化 · 实施计划

> 状态: 已交付(2026-07-27 合 dev 175af20) · 对应 design `../design/2026-07-27-redteam-m17d-ship-protocol-hardening-design.md`；决策 `../decisions/2026-07-24-redteam-m17-structural-refactor-decisions.md` §D（含原「ship-step 下沉」作废留痕）；里程碑=roadmap M17-d。
> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development，逐 task 派发。

**Goal:** T05 半失败 approve 交人出口 + 跨轮状态盘上重建协议 + resolve add|show 对称化 + T07/T08 薄版随行。

**Architecture:** 2 tasks——Rust 先行（Task 1 resolve 子命令空间，ship.md 修订要引用 `resolve show`），md 协议随后（Task 2 六处修订+模板+gen）。

**分支:** `feat/m17d-ship-protocol`（从 dev 切出；纯本地，永不 push）。

## Global Constraints（每 task 隐含）

- report schema v3 / proj.db DDL / review 命令面零变化；CLI 破坏面仅 resolve 裸形态→add（design §5）。
- 每 task 收尾：`cargo test` 全绿 + `cargo clippy --all-targets` 零新增 + commit。
- Task 2 额外：`make gen` 后 `make verify` 零 diff；ship.md 模型本职规则（裁决/fuzz 配方/refuted 例外目检/复裁语义）一字不丢。
- 注释/commit/协议文案简体中文；只 commit 绝不 push。范围闭集：design §7。

---

### Task 1: resolve 子命令空间化 add|show

**Files:** Modify `src/main.rs`（Cmd::Resolve 结构：平铺 → `ResolveCmd { Add{…}, Show{id} }`，对称 DismissCmd 手法；run_resolve 分派）；db 层零改动（`resolve_get`/`resolve_history` 读口已存在）。

**Steps:**
- [ ] 1.1 TDD：先写测试——①`resolve add` 语义等价锚：现有 resolve 写路径测试（grep main.rs/cli 中 resolve 相关测试）断言零改、仅调用形态换 add；②`resolve show <id>` 输出断言：含 title/file/quote/severity/reason/round + history 逐条（构造两条 history 的卡断言两行都在）；③查无此卡 → 非零退出 + 明确报错。
- [ ] 1.2 实现：clap 结构对称 DismissCmd（Add 保留全部现有参数与行为逐字、Show 新增）；`--help` 文案更新（裸终端唯一文档，与代码同权重）。
- [ ] 1.3 全绿 + clippy + commit：`feat(cli): resolve 子命令空间化 add|show——对称 dismiss，补跨轮重建读口（D-QC1）`

### Task 2: ship.md 协议修订六处 + T08 模板 + gen

**Files:** Modify `plugins/redteam/commands/ship.md`（design §2 六处 + §3 模板）；`make gen` 产物（codex-skills 镜像）。

**Steps:**
- [ ] 2.1 按 design §2 逐处修订（引用处号入 commit message 便于评审对照）：
  1. 停机 §1 加半失败分支（AskUserQuestion 二选一文案）；
  2. 起手后新增「跨轮状态重建」节（铁律 + 三重建口，含 `resolve show`）；
  3. 停机 §2 与内循环 §3 的 disputed 二现判定改查表口径；
  4. 停机 §6 max-rounds 改目录计数口径；
  5. 收轮 rN.result.json 存档义务化措辞；
  6. §0.1 目录两层化 `rounds/<YYYYMMDD>/<HHmmss>/`。
  另：全文 `redteam resolve <id>` → `resolve add <id>`（Task 1 契约）。
- [ ] 2.2 停机 §1 收工步嵌入 design §3 的 session.md 固定模板（「宿主填值，禁自由发挥」措辞）。
- [ ] 2.3 `make gen` → `make verify` 零 diff；ship.md 行数记录（预期 134→150 上下：新增节>删减，本批目标是硬化不是瘦身——瘦身随下沉方案作废）。
- [ ] 2.4 commit：`feat(ship): 协议硬化——T05 交人出口/跨轮状态盘上重建/判定查表化/T07 两层目录/T08 模板（D-QA/QB/QC2）`

---

## 收尾

- 隔离评审（md 侧按 design §6 九项清单逐条核对 + Rust 侧常规）→ 修 findings → 合 dev（--no-ff，纯本地）。规模小，整分支终审与 task 评审可合一（单次评审覆盖两 commit）。
- roadmap M17-d 置 ✅（M17 全组收口）、done.md 记账、T05/T07/T08 标交付、T09 维持、本 plan 状态头改「已交付」。
- live ship 验收挂账 M19 发布前 dogfood。
