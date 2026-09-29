# M21d 上下文深化 · 实施计划（三批）

> 状态: 锁定（2026-09-29 派生自 design §9.4 / §10.2 / §10.4 / §10.5 与裁决 **D20**）· 上位 `../roadmap/M21d-context-depth.md`
> **不背 PR181 那批覆盖缺口的指标**〔D20-2〕；L4-B 的验收对象是**本仓自评审**〔D20-1〕。

## 已就位零件（本计划的地基，不重做）

- **文件外片段**（`src/pack/mod.rs` 尾段，批 E E-3 / K11 / K12）已经在做「caller 调用点 ±N + **callee 签名 + doc**」，
  只是搜索面限于**仓内**。L4-B = 把同一条链扩到 registry 源，**渲染、预算帽、省略清单、标识符边界过滤全部复用**：
  `OUTSIDE_SIG_BLOCKS_MAX`(40) / `OUTSIDE_DOC_MAX_LINES`(10) / `OUTSIDE_BUDGET_SHARE`(8) / `OUTSIDE_MIN_CAP`(256)。
- `src/risk` 的 extapi 信号 v4 已随 M21b 入库（交付项 3 只做「关键模块」判定与 k 阈值校准）。
- `src/positioning` 的 quote 锚定与跨轮 carry 已在（交付项 6 只加确定性后滤）。

## 全批隐含约束

1. 每处行为改动配会转红的用例 + 变异检查；**变异脚本本身也要被怀疑**（M21c 批 0 有 2 条假绿源于脚本判据与范围写错）。
2. **跨层接线一律配一条集成级用例**——M21c 批 A 连续两次栽在「各自绿、接起来不通」（接管挂错层、resume 第二条出口漏挂）。
3. registry / brief 缺失一律**降级不阻塞**并披露进 RunMeta；私有数据只落 `.next/private/`。
4. `make verify` 绿；改代码走小分支、合回前过 `/code-review`。

## 批 A · L4-B 三方契约注入（`feat/m21d-contracts`）✅ 2026-09-29

> 验收对象 = **本仓自评审**（Rust 单 crate、16 直接依赖、registry 在本机）。
> 记录 `.next/private/m21d/2026-09-29-batchA-contracts-record.md`；边界与受益面改述见 **D21**。

### Task A-1 Rust 定位链（纯函数，离线可测）
`use` 语句 → crate 名 → `Cargo.lock` 精确版本 → `~/.cargo/registry/src/<index>/<crate>-<ver>/`。
- 同名歧义（多个 crate 导出同名函数）→ 全给并带 crate 标签，受 cap 约束。
- 边界：`CARGO_HOME` 覆盖、registry 目录不存在、Cargo.lock 缺失 / 版本对不上 → 一律返回空 + 可披露的原因。

### Task A-2 契约抽取
在定位到的 crate 源里找 `pub fn <name>` 的签名 + 紧邻上方的 `///` 块（复用 `OUTSIDE_DOC_MAX_LINES`）。
函数体默认不带（design §9.4 B「签名 + doc，函数体按需」），`needs{kind:api}` 命中时才带（A-4）。

### Task A-3 注入与观测
并进文件外片段、与仓内签名块**共享同一份预算帽**（三方契约不得挤掉仓内上下文）。
RunMeta 新增：注入的契约块数、input 增量、registry 缺失的降级原因。

### Task A-4 `needs{kind: api}` 联动
scanner 提出 `needs{kind:api}` 时，深挖 cell 带对应契约 **+ 函数体**。

### Task A-5 验收 ✅（三项过、一项改判）
- ✅ 本仓自评审：批 A 范围注入 **21 块 / 1 miss**；`CARGO_HOME=/nonexistent` 对照 **0 注入、全部计 miss、pack 照出、退出码 0**；
  input 增量 +1.9% 字节 / +1.5% est_tokens（A-4 提交那一段的同范围对照）。
- ⚠️ 「版本相关 API 误用」**改判**〔D21〕：靶子（`tempfile::tempdir` 的 TempDir drop）在**有契约 / 无契约两轮都被抓到**
  （critical · conf 0.98 · agreement 2）→ 名气大的 API 上模型先验已够，这条验收测不出注入的功劳。
  签名级漂移归编译门；L4-B 的正面证明改挂「先验薄的靶子」followup。
- ✅ PR181 回归：注入面三组 **contracts=0 / misses=0 / spilled=0**（零 Rust 一个字节都不多，也不产生垃圾 miss）；
  全量一轮（2026-09-29）**verdict 一致 reject、findings 在基线带内、失败格 0**，判据满足。
- 验收反过来抓到三处自身缺陷并已修：同名方法给错归属（`c997614`）、`use krate::Type;` 形态抽不出目标（`b588b0e`）、
  省略清单刷假 miss（`7bc0f0c`）——**都是单测照不到、只有真实仓库数据才暴露的**。

## 批 B · 稳定前缀与确定性后滤（`feat/m21d-prefix-anchor`）✅ 2026-09-29

> 记录 `.next/private/m21d/2026-09-29-batchB-prefix-anchor-record.md`。开工调研改了两处范围理解：
> B-1 的槽位 / 段序 / 中和表**本就在模板里**（只差读取与接线）；B-2 的 `anchorWindow` 键已被
> 装包的禁真空窗半径占用 → 裁定新开 `findingWindow`、先只观测、降级默认关。

### Task B-1 仓库简介入稳定前缀（交付项 4）✅
`.redteam/brief.md`（帽 8KB）进 `<repo_brief>` 槽，与 `--spec` 共用防线（非常规文件拒读、有界读、
PEM 嗅探、**符号链接逃逸检查**）；缺失时与今日逐字节一致。
中和**按三表并集**做（各角色表标签集不同，不统一会让稳定前缀分叉——dogfood 抓到，
`--spec` 的同款问题随 code-review 一并修）。brief 字节从 pack 预算里扣、`.gitignore` 放行 brief.md。

### Task B-2 `findingWindow = ±N` 确定性后滤（交付项 6）✅
新键 `review.pack.findingWindow`（默认 30，0 = 关）。**默认只观测**：恒标记 `Finding.outside_window`
+ RunMeta 计数 + md 逐条披露；`findingWindowEnforce` 打开才「不计入 verdict 与计数、条目照留」。
carried 项、变更集外文件、diff 重建不了三种情形都不判（各有理由，都有用例）。
实测本仓一轮：窗外 2 / 41（4.9%），样本太小不足以支持开 enforce。

## 批 C · 按信号触发（`feat/m21d-tuning`）

- 交付项 3：「关键模块」判定与 k 阈值校准（与 A 的注入联动）。
- 交付项 5：切片调优——**只在回退信号触发时介入**；触发则加二跳选项、校准 `slice_min_bytes`，复跑 A/B。
- 交付项 7：拆大组——**已撤墙钟验收**〔2026-09-23 实测〕，改按「每格 diff 字节」与预算分配判。

## 与其它里程碑的边界

| 事项 | 归属 |
|---|---|
| Go / TS 的三方契约定位链 | **不做**（M20 backlog）——PR181 这类仓享受不到批 A，已知缺口 |
| PR181 那批覆盖缺口（T6/T7/T8/T11/T12） | 单独立项〔D20-3〕：reviewer 缺陷类别清单 / 同项多采样 |
| 判例前移、收敛三维 | M21c 批 B / C（等日常 dogfood 攒出台账素材） |
