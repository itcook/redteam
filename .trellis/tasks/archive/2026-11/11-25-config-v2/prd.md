# config v2 重排（T-A 设计 · T-B 实施）

> 基线 `a494e39` · 2026-11-25 · 协调者 `01a20ba0`

## 背景

triage（Phase 3）与 verify（Phase 4）接入后，v1 config 的「模型 → roles」形状不够用：
- 角色信息挂在模型下，roles 的活跃集 = 有该键的模型；triage/verify 只能借用 reviewer
  （`triage_route` 取 `reviewers[0]`，verify 缺省回退 reviewer）。
- `language` 在顶层，与 hibot「仓策略」的归属不一致。
- `provider.kind` 四值里 `openai` 与 `openai-compatible` 行为等价，`google` 从未实现。
- 模型清单按 id 做键，无法携带 id 以外的有序信息。

用户已裁决：**趁 hibot 尚未冻结渲染器，破坏性重排为 v2，硬切、无别名**。

## 交付（两阶段）

### T-A（本阶段，只设计，不改实现代码）

1. v1→v2 **全键**迁移映射 + 语义说明：`.doc/design/2026-11-25-redteam-config-v2-design.md`
2. v2 样例 + v1 基线 + schema 产物副本 + hibot 交付清单：
   `.trellis/tasks/11-25-config-v2/`（`config-v2.sample.json` / `config-v1.before.json` /
   `config-v2.schema.json` / `hibot-delivery.md`）
3. 待拍板问题（8 条）发协调者/用户，**批准后才进入 T-B**。

### T-B（已完成 2026-11-25）

按设计文档 §10 分层实施：schema/常量 → config 解析校验 → provider → 角色/注册表 →
kind 接线 → CLI 写盘 → 文档/生成物 → 测试。
正式产物：`dist/config.schema.json`（`urn:redteam:config/2`）+ `capabilities` 的 `schemaVersion: 2`。
**T-B 只做 verify 的配置契约，不实现 Phase 4，不把 verify 加进 capabilities。**

## 已锁定的决策（不再讨论）

1. `language` → `settings.language`。
2. job/executor 配置只认 `apiKeyEnv`；任何路径出现 `apiKey`（含空串）判 `secret_field`，诊断不输出密钥值。
   **本地配置允许内联 `apiKey`**（Q2 裁决），解析链见设计 §2.7；hibot 渲染绝不写该键。
3. `provider.kind` → `provider.api`，三值 `openai-completions` / `anthropic-messages` /
   `openai-responses`（预留，ERROR）；`google` 删除；`baseUrl` 必填。
4. 硬切、无别名；schema 产物 `$id`/`schemaVersion` 与 capabilities 同步为 **2**（加相等断言）。
   **配置文件本身不新增 `schemaVersion` 字段**（如需，见 Q3）。
5. 角色移到 `settings.review.roles.{scanner,reviewer,refuter}.models[]`、`settings.triage.models[]`、
   `settings.verify.models[]`；条目 `{provider,id,reasoningEffort}`。
6. `settings.*.models[]` = 轮转语义；长度 1 合法；不做失败回退。
   落地需钉死粒度、游标、重试归属（设计 §3.4）。
7. `verify` 不再回退 reviewer；空池 → `missing_role:verifier`。refuter 空池分叉：本地自动异族回退，
   job 跳过并记 `refute_skipped="no_refuter"`（Q6 裁决）。
8. hibot Registry `kind` 不绑定我方词表（映射见交付清单 §3）。triage 缺角码名 `missing_role:triage`（Q7）。

## 验收标准

- [x] T-A：设计文档覆盖 `src/config/mod.rs` 每一个现存键（`ROOT`/`PROVIDER`/`MODEL`/`REVIEW`/
      `REVIEW_SECTIONS` + 结构体字段），无一静默丢弃。
- [x] T-A：v2 样例 `models[]` 长度 1；与 v1 样例的逐字段 diff 完整，且明确「不是无损迁移」。
- [x] T-A：schema 草案 `$id: urn:redteam:config/2`、`schemaVersion: 2`，样例可被其校验通过。
- [x] T-A：待拍板问题带默认值与理由，经协调者/用户确认。
- [x] T-B：`make verify` 绿；`cargo test` 全绿；`dist/config.schema.json` 与 capabilities 双 2。
- [x] T-B：`config check` 对 §2 每个 v1 键给出专属迁移提示（逐键单测，含别名与拆 provider 场景）。
- [x] T-B：v2 样例过 `config check --kind review|triage|verify`（0 error）。
- [x] T-B：hibot 交付 sha256 重发（正式产物）。
- [x] T-B：`Config::validate` 不要求三池齐全（最小集只在 `config check --kind`）。

## 约束

- `~/hibot` **只读**（样例已复制进本任务目录，不改源）。
- 不改 `dev`；不重建入库二进制；不做 Phase 4/5/7。
- 不进入 T-B 直至批准。

## 相关

- 契约：`~/hibot/.docs/runtime/config.md`、`.docs/audit/contract.md`（只读）
- 代码：`src/config/mod.rs`、`src/cli/config_cmd.rs`、`src/job/executor_config.rs`、
  `src/provider/backends.rs`、`src/orchestrate/{mod,deep}.rs`、`src/consolidate/{scan,refute}.rs`、
  `src/triage/mod.rs`、`src/job/run.rs`
- 生成物：`dist/config.schema.json`、`capabilities/redteam.json`、`scripts/release.sh:163`
