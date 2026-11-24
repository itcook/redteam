# M6 · 提示词 & lens 质量（batch 1）设计

2026-07-02 · grilling 定案。本 spec 覆盖 M6 可立即落地的 prompt/清理三项；harness（M6-b）与层-3
硬预算按决议后置。roadmap 见 M6 段。

## 0. grilling 决议速览

- **lens = 提示词片段、保持 Rust `const`**（不配置化、不 toml——toml 是配置化载体会诱导运行时覆盖
  预期；`max_severity` enum 与 `clamp_compliance` 编译期硬钳咬合，软 prompt 替代不了）。**不动代码**。
- **doc 不按 lens 物理 fan-out（否决）**：doc 检索无 bundle 边界，物理拆 lens = 每 cell 重跑无界全量
  检索、不可摊薄。现状「每 model 全 lens 单 cell + model 候补/replicate」保留。
- 三项落地：**(核心) doc 渐进式检索** + **R7.1 bypass_class** + **死码清理**。全内联，两批次。

## 1. 核心 · doc 渐进式检索（层 1+2，纯 prompt/工具描述，非冻结区）

**现状**：doc 走 tool-loop，单点有界（`read_file` 64KB / `search_repo` 100 命中 / `list_files` 500 /
`DEFAULT_MAX_ITERS` 12 轮）但**累积无界**；且 M11 在 `adversarial-doc-bundle.md` 的 `<tools>` 加的
"Mandatory first step — read_file EVERY referenced file + search_repo EVERY claimed function" 是
**预加载全 repo** 指令，实测把 lgtm design doc 评审推到 785K input tokens。

**改法**：**替换**该 first-step 段为「渐进 targeted 核实」（5 要素，措辞实施可迭代，语义不可动）：

```
Retrieve progressively, not up front. Do NOT pre-load every referenced file. Read the
document first; then, while writing each finding that needs code evidence, retrieve only
what THAT finding requires: search_repo to locate the symbol, then read_file with a line
range to read just that span — not whole files. Stop retrieving once you can judge the
claim. Retrieval is minimized, but verification is NOT: you must still check every claim
of existing implementation and every stated dependency — precisely, not exhaustively.
```

- `<temporal_stance>` 末尾的 "These last three are ACTIVE duties…retrieve the referenced code" **保留**
  （已是逐-claim 驱动，方向一致），仅把 "retrieve" 明确为 targeted（search_repo 定位 → read_file 行范围）。
- `adversarial-refute-doc.md` 的 `<tools>` 未含 first-step 全读，**不改膨胀**；顺带在其 tools 段补一句
  "read only the span needed to judge this one claim（行范围精读，勿整文件）" 对齐渐进（可选，低风险）。
- **层 3 累积硬预算**（tool_loop/tools 冻结区）**不在本 spec**——实测层 1+2 不足再单独授权。

**验收（量化 A/B）**：M11 靶 1（`.doc/design/2026-07-01-breakpoint-loop-lgtm-design.md`）改前后同模型
（Kimi）跑——`input_tokens` 从 ~785K **显著下降**（观察参考 <200K 量级，非硬阈值）；**且**核实类真
drift（针对 as-built 代码，如 spec 陈旧行号 / lgtm.md 状态行格式不一致）**仍在**。token 大降 + 核实
不丢 = 生效。

## 2. R7.1 · bypass_class（纯 prompt）

**落点**：**只注入生产模板 `adversarial-code-bundle.md`**（`adversarial-code.md` 是死模板，§3 待删）；
置于 `<finding_bar>` 之后（承接其中的 Exhaustiveness 指令）。doc 侧不加（教训源自 code review，YAGNI）。

**基线文本**（承接穷尽性、不重述；措辞可迭代，语义不可动）：

```
<bypass_class>
The exhaustiveness rule above requires reporting variants you observe; for input-handling
bypass findings (scheme/host/path allow- or deny-lists, encoding or sanitization, parsers,
validators), go further:
- Characterize the CLASS, not one instance: name the input-space dimension being bypassed
  (case, encoding depth, alternate scheme, protocol-relative, Unicode normalization, path
  traversal…) rather than the single input the code happens to miss.
- In the same finding, enumerate the strongest representative variants (e.g. mixed case,
  double/nested encoding, data:/javascript:/file: schemes, //host, ../ traversal) and note
  "and equivalents" — do not stop at the one case present in the diff.
- The recommended fix MUST be a normalizer / single canonicalization point, not another
  allow/deny-list entry. State that per-case patches leave the class open and are an
  incomplete fix.
</bypass_class>
```

**severity** 走实际阻塞（绕过 auth=critical…），不定规则。

**验收（live dogfood 两靶，code 模式需真 git 改动才有 bundle）**：
- **靶 A（升级四判据）**：造含输入绕过漏洞的改动——URL scheme 校验用 case-sensitive 前缀 denylist（只挡
  `javascript:` 小写）。判据：① finding 升级为**类级**（"scheme 校验前缀匹配、整类可绕"而非"缺挡一个
  case"）；② body 枚举多个代表变体；③ recommendation 指向 normalizer 归一化；④ 对照 dev 二进制同靶
  大概率只报单 case（天然 A/B）。
- **靶 B（不误伤）**：正常输入处理、无绕过漏洞的改动 → `<bypass_class>` 不硬套变体清单误报。

## 3. 死码清理（纯删，引擎区但零行为变更）

Phase-1 单 lens fan-out 机制，生产零调用（`cli.rs` 只用 `run_code_fanout`/`run_doc_fanout`）：

- 删 `src/orchestrate/mod.rs::run_fanout`（fn + 其 tests：`explicit_models_*`/`on_cell_*` 等按 run_fanout
  的用例，实施时 grep `run_fanout(` 定位）。
- 删 `src/prompt/mod.rs::build_prompt` + `CODE_TPL`/`DOC_TPL` const + 其 tests（`build_prompt(` 用例）。
- 删模板文件 `src/prompt/templates/adversarial-code.md`、`adversarial-doc.md`。
- 修 M7 `all_adversarial_templates_carry_exhaustiveness_directive`：删对 `adversarial-code.md`/
  `adversarial-doc.md` 两行断言，只留 `adversarial-code-bundle.md`/`adversarial-doc-bundle.md`。
- **前置守卫**：删前 `grep -rn 'run_fanout\|build_prompt\|CODE_TPL\|DOC_TPL' src` 确认无生产引用；
  `by_id`/`select`/`lens` 模块不受影响（rules_block 路径不碰这些）。

## 4. lens 常量化（不动代码，仅记决议）

现状 `src/lens/mod.rs` static 数组即"编译期常量清单"，已满足决议——**不改代码**。仅在 roadmap/spec
锁定"永不 toml/配置化"，防未来漂移。

## 5. 执行方式（内联，两批次）

- **batch 1 死码清理**（先清场，纯删+改断言，低风险）；
- **batch 2 prompt**（doc 渐进检索替换 + bypass_class 注入 + 字符串断言测试 + dogfood）。
  字符串断言：doc-bundle 含 "Retrieve progressively"、code-bundle 含 `<bypass_class>`；M7 exhaustiveness
  断言已在 batch 1 收敛到 2 生产模板。
- 全程 `make build` 重编再 dogfood。措辞按 dogfood 结果迭代（每轮一个变量），同 M11 手法。

## 5.5 验收结论（2026-07-02 交付，已并入 dev merge 69003b2）

三靶 live dogfood 全 PASS（内联执行，feat/m6-prompt-quality 4 commit）：
- **靶 1 渐进检索 A/B**：`input_tokens` 785K → **403K（降 49%）**——达标（观察参考 <200K 是软目标，
  49% 降幅 + 核实不丢即生效）。核实类真 drift 存活（"Spec references stale cli.rs line numbers"
  锚到 line 90）。
- **靶 A bypass_class**：类级刻画（"bypassable via case/whitespace/encoding/protocol"）+ 枚举变体 +
  normalizer 修复（"single canonicalization pipeline, not deny-list"）三判据全过。
- **靶 B 不误伤**：无绕过语义的正常代码不被硬套变体清单。

**计划外引擎修复（dogfood 破案，用户批 A+B）**：渐进检索首轮暴露 `anchor_doc` 对中文 markdown
quote 的误杀——fanout 12 条真 finding（含核实类 drift）被砍到 2，因模型引用中文时轻微去掉 `**`
粗体、跨软换行，逐字匹配失败而 M11 词级退路对无空格中文无效。修复：`positioning/mod.rs::locate_doc`
（去空白+去 markdown 的**字符级**子串匹配，doc 路径专用）+ `anchor_doc` 定位失败**保留 line 0**
（非丢弃，doc finding 靠 body 自证、真幻觉转 verify/refute 尾段兜底）。修复后 fanout **12→12 零丢失**。
是 M11 软换行 anchor 缺陷的同源续集。

## 6. 非目标

- **M6-b 提示词 harness**：后置，先出「素材盘点」研究产物再第二轮 brainstorm。
- **层 3 累积硬预算**（tool_loop/tools 冻结区）：层 1+2 实测不足再授权。
- **doc 按 lens 物理 fan-out**：否决（无界检索 ×N）。
- **lens toml/配置化**：否决（保持 const）。
- **R7.1 注入 doc 模板 / doc 侧 bypass**：不做（教训源自 code）。
- **R7.2 fuzz**：已归 M7 lgtm 内配方。
