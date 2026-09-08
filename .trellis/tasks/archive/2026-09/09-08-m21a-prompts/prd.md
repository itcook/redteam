# prd · M21a 批 D 契约与提示词（`feat/m21a-prompts`）

## Goal
scanner 角色的**输出契约 + 提示词**成品：模型输出 → `ScanOutput` 的字段级 parse（quote 不在文件即丢弃）、共享头 + scanner 角色尾模板、
字节稳定的段序（共享头 → 参考 → 证据 → diff 次尾 → 角色尾）、anthropic 三段界 cache 断点、模板哈希入记录、离线回归评分器。
批 F 只需把 `build_scan_prompt` + `parse_scan_output` 接到 scanner cell 即可跑疑点扫描。用户价值：M21 的「不漏」地板第一块——疑点模式
（E3 已验证召回 ≥ finding 模式）从 eval 脚本进入产品。

## Background（已核事实）
- 前提已合回：批 B `ScanOutput / Suspicion / Category / ScannedArea / NeedEntry.kind / CellOutput::Scan`（`src/domain/suspicion.rs`，tag 值
  `scanner`）、`CellRecord.template_hash: Option<String>`（`src/state/mod.rs:182`）；批 C `{{OTHER_CHANGED_FILES}}` 槽已是跨组索引
  （`bundle::render_cross_index`），`build_bundle_prompt`（`src/prompt/mod.rs:107`）三调用点（orchestrate / second_pass / pack-only）。
- 现有可复用件：`parse_lens_output`（`src/provider/parse.rs:10`：`</think>` 切除 / 围栏剥离 / 字符串感知配平扫描）、`REPAIR_PROMPT`（`:99`）、
  `positioning::{safe_read, locate_quote}`（`src/positioning/mod.rs:12,30`，quote 去空白 + 软换行退路）、`neutralize_structure` +
  `DOSSIER_STRUCTURAL_TAGS`（`src/prompt/mod.rs:266,304`）、`render`（单 pass 模板渲染）、anthropic body 的 `cache_control` 单断点
  （`src/provider/backends.rs:70-82`）、`hash::fnv1a64_hex`。
- 底稿：E3 B 臂 `FINDING_BAR_B` / `OUTPUT_CONTRACT_B`（`eval/e3_contract_ab.py:35-54`）、现模板 `adversarial-code-bundle.md`、
  `NO_TOOLS_SYSTEM` 免疫段（`src/provider/single_shot.rs:12`）；评分逻辑 `e3_contract_ab.cmd_score`（`:66-125`）；oracle
  `.next/private/p0-raw/oracle-pr181.json`（`{kind, head, entries[{id,file,quote,...}], topics, sets}`，gitignored）与 `p0_oracle.py hobby` 同形。
- 接缝（followup 已记）：`ScanOutput.suspicions` 无 serde default；`CellOutput` tag = `scanner`；旧 `two_layer_manifest` 双清单只在旧 prompt。

## Requirements
- R1（D-1）`provider::parse::parse_scan_output(raw) -> Result<ScanOutput, ProviderError>`：复用 `</think>` 切除 / 围栏剥离 / 配平扫描；
  逐条校验并**规整**：`file` 相对路径且无 `..`（违者丢）、`line_start ≥ 1`、`line_end ≥ line_start`（否则取 line_start）、`hint` 超 300 字符截断、
  `category` 非法 → None、`confidence` 夹到 [0,1]、`quote` 空白 → 丢；`needs` 超 5 截断、`kind` 缺省 evidence；`scanned_areas.ranges`
  只留形如 `a-b`（a ≤ b）的项。缺 `suspicions` 键 = 违约 → `Parse` 错误（走 repair）。`REPAIR_PROMPT_SCAN` 文案指「complete suspicions JSON object」。
- R2（D-1）`positioning::anchor_suspicions(root, ScanOutput) -> Anchored { output, dropped: u32 }`：quote 定位（`locate_quote` 同口径）
  成功则改写 `line_start/line_end`，失败**丢弃并计数**（进 warning 文案「n suspicions dropped: quote not found」）。
- R3（D-2）`src/prompt/templates/shared-head.md`：`<role>`（通用对抗评审身份）、`<rules>` L0-L5（现文本）、`<operating_stance>`、`<evidence>`
  （改写为：pack 切片 + 跨组索引 + 判例摘要，不发明代码）、`<ci_covered>`（CI 已覆盖类别跳过清单：格式 / lint / 类型与编译错误 / 既有单测失败）、
  `<untrusted_input>`（免疫段，并入 `NO_TOOLS_SYSTEM` 条款）、`<bypass_class>`、`<output_language>`、`<spec_docs>` 槽、`<repo_brief>` 槽（空渲染 `(none)`）。
  `src/prompt/templates/scanner.md`：`<finding_bar>`（E3 B）、`<precedents>` 槽（「以下曾被驳回，若仍报请降低置信」，空渲染 `(none)`）、
  `<needs_policy>`（「缺证据声明 needs，不要猜」）、`<output_contract>`（= R1 字段）。
- R4（D-2/D-3）`prompt::build_scan_prompt(ScanPromptInput) -> ScanPrompt { system: String, blocks: Vec<PromptBlock { text, cache_boundary: bool }> }`：
  system = 共享头（含 spec / brief）；blocks 顺序 = 参考（跨组索引 + 判例摘要 + user focus）→ 证据（dossier / pack）→ diff 次尾（target / 文件清单 /
  hunks）→ scanner 尾；`cache_boundary` 在证据块与 diff 块为 true（= 证据段末、角色尾前）。每个注入段只有一个槽；全部注入内容过
  `neutralize_structure`（新增标签进 `DOSSIER_STRUCTURAL_TAGS`）。旧 `build_bundle_prompt` / `adversarial-code-bundle.md` 原样保留（F-4 删）。
- R5（D-4）`ChatMessage::UserBlocks(Vec<PromptBlock>)`：anthropic body → content 数组，`cache_boundary` 块加 `cache_control`，system 带
  `cache_control`（共三段界）；显式断点在场时不再给最后块自动加断点（Anthropic 上限 4）；openai body → 块文本拼接为一条 content（与拼接串逐字节相同）。
  `ScanPrompt::flatten()` 给 openai / 落盘用。
- R6（D-4）`prompt::template_hashes() -> TemplateHashes { shared_head, scanner_tail, legacy_bundle }`（`fnv1a64_hex`）；`CellRecord.template_hash`
  在 `orchestrate::to_record` 填当前路径所用模板哈希（旧 fan-out = `legacy_bundle`；F 换成 head+tail）；`RunMeta.template_hashes` 由 F-5 填。
- R7（D-5）`eval/scan_score.py`：输入 = redteam-next 结果 JSON（`cells[].output.suspicions[]`，role=scanner）或 E3 run JSON（`items[]`）；
  quote 定位（`git show <head>:<path>`）→ 区间级命中 oracle `entries`（`--set` 缺省 union）→ 命中数 / 噪音比 / 未定位数；`--baseline prev.json`
  打印差值；零依赖；`eval/README.md` 登记用法与基线（E3 B 臂三跑：自动区间并集 9、人工主题并集 5）。
- R8 三道门每 task；每 task 一笔提交；不动 `src/pack/`（E 领地）、不动旧 fan-out 行为（`build_bundle_prompt` 输出逐字节不变）。
- R9 D-6：code-review 子 agent 评审（用户定：不跑 ship 评审）→ 修 → 主工作树 `--no-ff` 合回。

## Acceptance Criteria
- AC1 parse：围栏 / 前言 / `</think>` / 截断（→ Parse 错）/ 缺 quote 项丢 / category 非法 → None / confidence 夹 / hint 截断 / needs 6 → 5 /
  ranges 过滤 / 缺 `suspicions` → Parse 错；各一断言。
- AC2 anchor：quote 在文件改写行号；不在文件丢弃且 `dropped` 计数；`..` 路径丢。
- AC3 模板：共享头对两组不同 pack 渲染逐字节相同；scanner 尾含 `suspicions` / `needs` / `scanned_areas` / `quote` / `category` / `kind` 字段名
  （字段名扫描测试：`Suspicion` / `NeedEntry` / `ScannedArea` 的 serde 字段名全部出现在 scanner.md）；共享头含免疫段与 L0-L5；`<precedents>` /
  `<repo_brief>` / `<spec_docs>` 空渲染 `(none)`。
- AC4 段序：`ScanPrompt` 四块顺序固定，`cache_boundary` 恰在第 2、3 块；注入内容（索引路径 / 判例 / brief / spec / diff / 路径）含 `</scanner>`
  `</evidence>` `</output_contract>` 等闭合标签被中和；`flatten()` = system 之外四块按序拼接。
- AC5 backend：anthropic body `system` 为带 `cache_control` 的 block 数组、messages[0].content 四块中第 2、3 块带 `cache_control`、第 4 块不带；
  `UserBlocks` 与等价 `User(flatten)` 的 openai body 逐字节相同；哈希稳定（同输入同哈希、模板改一字节即变）。
- AC6 `orchestrate` 旧路径 CellRecord 带 `template_hash = legacy_bundle`（既有 e2e 取一条断言）。
- AC7 `scan_score.py`：对 `.next/private/p0-raw/e3/B` 三跑复算与 `score-B.json` 的 `union_oracle_hit` 一致（本机核对，不入库）；`selftest`
  子命令用合成仓 + 合成 oracle 跑通命中 / 噪音 / 未定位三数；`--baseline` 差值行。
- AC8 三道门全绿；`build_bundle_prompt` 快照测试不变。

## Out of Scope
- scanner cell 接线、按角色发 route_key、钉推理档、疑点并集与 agreement（批 F）；reviewer / verifier 尾（M21b）；判例摘要填充（M21c）；
  仓库简介内容（M21d）；旧 fan-out 段序改动；`two_layer_manifest` 删除（旧 prompt 随 F-4 整体删）。
