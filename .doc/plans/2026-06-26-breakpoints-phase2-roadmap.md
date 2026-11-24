# breakpoint Phase 2 实施路线（执行核心重构）

> 状态：路线已定，待逐阶段进 writing-plans
> 日期：2026-06-26
> 设计依据：[`.doc/design/2026-06-25-breakpoints-adversarial-review-design.md`](../design/2026-06-25-breakpoints-adversarial-review-design.md) **§19**（Phase 2 架构修订：工具循环 + 双流水线 + positioning/verify 尾段）
> Phase 1 基线：[`.doc/plans/2026-06-25-breakpoints-core-engine-plan.md`](./2026-06-25-breakpoints-core-engine-plan.md) ·进度账本 `.superpowers/sdd/progress.md`

把 §19 的全量目标 + Phase-1 主动延后的运营面/打包，切成 **5 个可独立验证的实施阶段（S2–S6）**，最大未知（工具循环跨 provider 正确性）front-load 到最前。每阶段 → 一份（或两份）writing-plans 计划 → subagent-driven 执行。

---

## 0. 基线（Phase 1 已交付 + 主动延后）

**已落地（单发同步路径）**：domain 类型、分层 config（密钥 env>secrets、主配置零明文）、lens 注册表（CODE L1–L5 / DOC D1–D5）、prompt 模板 + 输出语言指令、evidence（git diff / doc 读 / 内联阈值）、`ReviewProvider` trait + FakeProvider + DirectProvider（单发，Anthropic + OpenAI 兼容）、orchestrate fan-out（lens×model）、consolidate（dedup under-merge）、verdict（clamp + 阈值）、output（JSON/md）、CLI + e2e。62 测试绿、clippy 干净、无 OpenSSL。

**三处主动延后（Phase 2 接着长，非返工）**：
1. **多模型被显式拒绝** —— `check_multi_model()` 对显式 >1 model 返回 exit 2；多模型未实现。
2. **correlate 是安全降级桩** —— 条件跳过逻辑在，真实 LLM 关联调用留空返回。
3. **`max_concurrency` 信号量未实现** —— 当前 `join_all` 无界并发。

---

## 1. 阶段总览

| 阶段 | 目标 / 交付（可验证增量） | 依赖 | 规模 |
|---|---|---|---|
| **S2 工具循环 + code 文件包 + positioning（单模型）** | 新 code 评审核心：模型自取代码、按文件包并发、行级精确锚定 | — | 大（拆 2 plan：2a 引擎 / 2b 流水线） |
| **S3 多模型 + targeted-verify** | 对抗式多模型 code 评审：交叉一致 + 守 verdict 完整性 | S2 | 中 |
| **S4 doc 流水线** | 文档对抗评审（差异化，OCR 不做） | S2（S3 让 doc 白捡多模型） | 中 |
| **S5 运营面** | 二进制可被 SKILL 异步消费 | 独立（建议 S3 后） | 大（可拆 2 plan：作业 / model 命令） |
| **S6 跨宿主打包** | 可安装 slash 插件（Claude Code + Codex） | 全部 | 中 |

主线 **S2 → S3 → S4** = §19.9 的 ①②③。

---

## 2. 各阶段详情

### S2 — 工具循环 + code 文件包流水线 + positioning（单模型）

整个 Phase 2 的地基，未知最大，能用 FakeProvider 完整隔离测试、不依赖真实 key。拆两份计划：

**S2-2a 工具循环引擎**（不接成用户路径，纯引擎层）
- 关键构建：tool 定义（`read_file` / `search_repo` / `list_files`）；tool 执行器（链入 ripgrep 内核 crate：`ignore` 做 .gitignore 感知遍历 + `grep-searcher`/`grep-regex` 做搜索；`read_file` = fs 读带行号；**绝不 shell 调外部 `rg`/`git grep`**）；多轮 loop 驱动（发 → 模型 tool_calls → 执行 → 回灌 → 终止条件 → 最终 findings）；Anthropic tools adapter + OpenAI function-calling adapter；**单发 prompt-embed-schema 兜底保留**（弱模型 + 极小 diff）。
- 注：`max_concurrency` 信号量原列此处，写计划时调整到 **S2-2b**——2a 是单条工具循环、本身无并发，信号量该在分包扇出引入真并发处加（见 §4 映射）。
- 复用：`ReviewProvider` trait 边界、config、domain 类型。
- 可独立验证：FakeProvider 脚本化一段 tool-call 序列（read→search→输出），断言执行器正确读/搜、loop 正确回灌与终止、两 adapter 协议各自解析正确、兜底路径仍可用。
- 用户可见性：无（库能力，S2-2b 是第一个消费者）。

**S2-2b code 文件包流水线 + positioning**（第一个真实消费者）
- 关键构建：确定性文件筛选（该评 / 过滤）；文件分包启发式（目录邻近 / 同名多语言资源 / 显式 import 关系）；lens→rule（按文件类型/路径匹配注入关注清单，含 L5 规范遵循 rule，severity 仍封顶 medium）；**`bundle×model` 取代 `lens×model`** 编排；positioning 尾段（finding `quote` 字段 + `quote → file:line` 确定性解析器 + 引用缺失判幻觉丢弃/标记）；CLI 接线 + e2e。
- 复用：S2-2a 引擎、evidence（喂文件清单）、consolidate dedup（positioning 准行号反哺 line-overlap）、verdict、output。
- 可独立验证：真实小仓 diff（FakeProvider 驱动）跑出按文件包并发、findings 带准确 file:line、编造引用被过滤；超大 changeset 不再走"甩清单"。

### S3 — 多模型 + targeted-verify

- 关键构建：ProviderRegistry（真多 provider 实例化）；replicate 全复制（同 bundle 集在每 model 各跑）；cross-model `agreement` 计数汇入统一 consolidate；三层 targeted-verify（① agreement≥2 跳过；② 引用存在性已由 positioning 砍编造；③ 定向 refute pass 只对 `severity ≥ block_threshold ∧ agreement=1` 的孤证 spawn skeptic 反驳）；finding 加可选 `verify` 字段；**可选 real correlate 替桩**。
- **分包 import-graph 增强（决策已定 2026-06-28）**：S2-2b 把分包裁成「同目录 OR 同词干」轻量启发式（§S2-2b 第 51 行的「显式 import 关系」当时主动推迟）。S3 提上来——在现有 union-find 基础上加**轻量确定性 import/use/require 提取**：改动文件 A 引用了同在改动集里的 B → 并包。**走确定性路线、不上 LLM 分包**，理由：① 工具循环架构下「包」只是种子 + 强制上下文，模型评审时本就能 `read_file` 整个 repo 追跨文件关联（read-root = cwd，`cli.rs` run_code_review 传入），分包错误代价被工具循环大幅吸收，硬盲区远小于 Phase-1 单发；② import-graph 确定性、可测、不在评审前烧 token，精准命中「跨目录跨名的显式依赖」这一当前盲区；③ LLM 分包非确定（难复现/难测）、更贵（要先读内容才能分，鸡生蛋）、失败模式隐蔽，作默认 ROI 为负。**LLM 分包仅作可选兜底**——只对 import 也连不起的「隐式耦合」（事件总线 / DI 容器 / 字符串 key / 配置约定）才有不可替代价值，非默认。顺带修当前同词干 over-grouping（`mod.rs`/`index.ts`/`__init__.py` 词干撞车并一锅）。天然与本段 correlate（跨包合并同一跨文件问题）配套。
- 复用：S2 引擎、orchestrate、consolidate dedup、verdict（确定性不变）。
- **拆分（决策 2026-06-28）**：S3-a fan-out 侧（ProviderRegistry + replicate + agreement 接通 + import-graph 分包 + 删 check_multi_model）/ S3-b consolidate 尾段（三层 verify + `Finding.verify` + real correlate）。b 依赖 a（agreement 是 verify 层①输入）。
- **model 选取语义（S3-a，决策 2026-06-28）**：统一为「显式列表 → replicate 全复制对抗；无任何显式 → 候补容错」。显式列表来源三选一：CLI `--models a,b`、新增 CLI `--models-all`（展开为所有已配置 provider，按 priority 降序）、config `default_models`（非空即视同显式列表）。三者任一触发 replicate（每 bundle×model 全跑，dedup 汇 agreement，任一格失败记 failed_models → exit 2）。仅当三者全空时回落「所有已配置 provider 按 priority 候补链、首个成功即止」（零配置省钱默认）。`--models` 与 `--models-all` 互斥，后者优先。ProviderRegistry 做成通用基建（code 填 ToolLoop 实例、doc 填 Direct 实例），删 check_multi_model 后 doc 经 run_fanout explicit 笛卡尔积白捡多模型，trait 边界不动。
- 可独立验证：两 model（registry 路由）对同一 diff 产部分重叠 findings → 重叠的 `agreement=2`、孤证触发 refute、被反驳的不翻 verdict；`--models-all` 展开全 provider replicate；移除 `check_multi_model` 拒绝、`--models a,b` 跑通合并后单一 report。

### S4 — doc 流水线

- 关键构建：doc evidence 增"读引用 spec/代码"工具；整篇文档进上下文；doc `lens×model` 跑在工具循环上（D1–D5 保留为扇出轴）；章节/引文锚定（positioning 解析到段而非行）；主题/引文去重（dedup 退化到非行号相似度）；矛盾类可确定性验、遗漏类走第 3 层 verify。
- 复用：S2 引擎、S3 多模型/verify、doc lens 集(§7.2)、verdict、output、severity 双保险(§7.4)。
- 可独立验证：一篇含自相矛盾 + 缺失分支的 design 文档 → 矛盾被锚定到两段引文、缺失分支由 verify 判真伪、verdict reject。
- **执行决策（2026-06-29，开 S4 时定）**：
  1. **doc 上工具循环**：doc 改用 `ToolLoopProvider`（与 code 同 provider，引擎层 `tool_loop.rs` 冻结不动），让模型按需 `read_file`/`search_repo` 核对文档对被引用 spec/代码的引述。所有 doc 框架塞进 user prompt（`build_doc_prompt`），不改 generic system prompt。main.rs doc 分支由 `DirectProvider` 换 `ToolLoopProvider`（两 mode 统一构造；`DirectProvider` 退为仅 `parse_lens_output` 复用 + 自身测试）。
  2. **扇出轴合并一包 ×model（偏离本节原文「D1–D5 保留为扇出轴」）**：对齐 code 在 S2-2b 的演进——全 D1–D5 rule 注入一个 prompt、每 model 一次工具循环（`run_doc_fanout`），省 ~5× token、agreement 语义干净。lens×model 原方案放弃（跨 model 已提供对抗多样性，lens 维度多样性收益边际）。
  3. **空引用=遗漏声明**：`anchor_doc` 把「quote 为空」当遗漏类声明——不丢弃、透传到三层 verify 判真伪；「quote 非空但不在文中」=编造丢弃；「命中」=锚定到行。零新 schema；prompt 教模型遗漏类留空 quote。dedup 退化到非行号相似度（`dedup_doc`：同 file + 标题相似即并，不要求行重叠）。doc refute 用 doc 版模板（`build_refute_prompt` 加 `Mode` 参数分流）。
  4. **单 plan 一次做完**：doc 大量复用 S3 成熟 anchor/verify，单 plan（8 task）+ subagent-driven 每 task 评审门。
  - **顺带**：carry-to-S4 ① conf_floor=0 实质修复（`compute_verdict` 显式排除 `verify==Refuted`，不再仅靠 confidence=0）随 Task 1 落地。correlate 仍为桩（doc 沿用现有 stub 调用，real correlate 继续推后）。

### S5 — 运营面

- 关键构建：状态存储布局（`$BRK_STATE_DIR` 按 workspace-sha256 分桶 / 原子写 rename / `state_ttl` 机会式 prune）；job 状态机（pending→running→done|failed|cancelled）；detached worker（re-exec 自身 + setsid）；`brk review --background` + `status` / `result` / `cancel`；`brk model setup` / `list` / `check`。
- 复用：§6 状态/并发设计、config、退出码约定。
- 可独立验证：`--background` 立即返回 job-id、`status` 轮询进度、`result` 一次性取用 + grace 重取、`cancel` 中止；`model check` 退出码（全可用 0 / 有不可用非 0）。
- 时机：后台作业在 S3 把单轮跑长后才真有价值，默认排 S3 后。
- **执行决策（2026-06-29，开 S5 时定）**：
  1. **拆 2 plan，先 S5-a 作业侧**：S5-a（状态存储 + job 状态机 + detached worker + `--background`/`status`/`result`/`cancel`）/ S5-b（`model setup`/`list`/`check`）。S5-a 含核心未知（detach/原子状态）front-load。
  2. **detach = re-exec + setsid（libc）**：父 `--background` 写 pending 状态 → spawn detached `brk worker <job-id>`（re-exec 自身隐藏 `worker` 子命令 + `libc::setsid` 新会话 + stdio 重定向）→ 立即打印 job-id 退出。**libc v0.2 已在依赖树**（tokio/aws-lc-sys 间接引入），声明为直接依赖零新增 crate。
  3. **测试 = run_job 进程内可测 + detach 薄缝**：抽 `run_job(job_id, args, provider, state)` 写状态机/状态文件，FakeProvider 进程内全测（pending→running→done|failed、status/result/cancel、原子写、机会式 prune）；re-exec spawn glue 作薄缝 reviewed-not-unit-tested（真 spawn 需真进程）。
  - workspace 分桶用既有 `fnv1a64_hex`（非 sha256，避免 sha2 依赖；目录桶键非安全敏感，偏离设计原文 sha256 但理由充分）。
- **S5-b 执行决策（2026-06-30，开 S5-b 时定）**：model 命令（`setup`/`list`/`check`，§13）单 plan ~6 task。
  1. **探测抽可注入接口**：`Prober` trait（`reachable(base)` + `list_models(base,key)`），HttpProber(reqwest 真探测)/FakeProber(测试)。可用性判定（key 就位×端点可达真值表）+ `check` 退出码（全可用 0 / 有不可用非 0）进程内全测；真 HTTP 探测作薄缝 reviewed-not-tested。
  2. **key 默认 env**（设计原文）：`model setup` 默认 env 模式——主配置仅记 `api_key_env`、打印 `export VAR=...` 供加 profile；`secrets.toml`(chmod 600) 作可选（用户显式选）。主配置永不含明文 key。
  3. **纯 flag 驱动**：`model setup` 从 flag 写配置（缺必填项报错；env 模式缺 key 打 export 指引/secrets 模式要求 --key），全可测零交互薄缝。设计的「缺省交互补全 + 隐藏 key 输入（libc termios）」作 carry 往后客。
  - secrets 写入扩 `config/secrets.rs`（`write_secret` + chmod 600）；主配置写走 toml::Value 插 `[providers.<name>]`（保留其他内容、免给 ProviderConfig 加 Serialize）。backends 默认 base 常量（api.anthropic.com / api.openai.com）在 model 模块重定义（backends.rs 冻结、避免触碰）。`model check`=key 就位 + 端点可达（非 key 鉴权验证）。

### S6 — 跨宿主打包

- 关键构建：`plugins/breakpoint/.claude-plugin/plugin.json` + `.claude-plugin/marketplace.json`；`commands/`（唯一手写真相源，Claude slash）；`scripts/gen-codex-skills`（bash，镜像 codex `render_command_skill`）；生成并提交 `codex-skills/`；Makefile（`make gen` + CI diff 防漂移）；跨平台二进制（先 darwin-arm64）。
- 复用：§15 设计、全部前序产物。
- 可独立验证：两宿主原生 marketplace 命令装得上、`/breakpoint:review`·`status`·`result` 可调；`make gen` 后 `codex-skills/` 无 diff。
- **S6 执行决策（2026-06-30，开 S6 时定）**：单 plan 一次做完；沿用 subagent-driven 执行 + 末尾 opus 全分支评审；finishing 保留分支。**前置已完成**：crate/lib + 运行时路径 + S6 包名整体 `breakpoints→breakpoint` 更名（commit `a88f788`，198 测试绿）。
  1. **仓内布局＝铺仓根、源码共仓**：`.claude-plugin/marketplace.json`、`plugins/breakpoint/{.claude-plugin/plugin.json、.codex-plugin/plugin.json、commands/、codex-skills/、bin/}`、`scripts/gen-codex-skills`、`Makefile` 与 `Cargo.toml`/`src/` 平级同仓（符合「一个仓=marketplace」；宿主 `marketplace add 你/breakpoint` 直指本仓）。代价：插件清单+预编译二进制混进源码仓根，接受。
  2. **Codex 侧＝完整三件套 + carry**：`commands/{review,status,result}.md` 唯一手写真相源 → `scripts/gen-codex-skills`（bash，best-effort 镜像 codex `render_command_skill`：frontmatter 映射 + `$ARGUMENTS` 约定）生成并提交 `codex-skills/<name>/SKILL.md`；防漂移＝Rust 集成测试调脚本断言重生成与提交版逐字节一致（纳入 cargo test，本地即验，取代无 CI 的 `make gen` diff）。真机 codex 转换细节（§18.2）作 carry 待验。
  3. **二进制＝commit darwin-arm64、跨平台 carry 暂不修**：Makefile `make build`＝`cargo build --release` 产 `bin/brk-darwin-arm64` 并提交（宿主不构建、读仓里有什么）；命令正文用插件根变量拼 bin 路径。S5-a 遗留 `pid_alive` 的 macOS-specific `libc::__error()` 跨平台 carry 暂不动（v1 只 darwin-arm64，仍只在 mac 跑）。linux/win 二进制 + `pid_alive` cfg 分支并列后补。
  - marketplace.json / plugin.json / commands frontmatter（`disable-model-invocation` 等）确切 schema 以 claude-code-guide 核实为准；`review` 计费重 → 禁模型自动调用，`status`/`result` 轻读 → 允模型触发。

---

## 3. 排序与弹性

- **S2 → S3 → S4** 为质量核心主线，强烈建议按此序：S2 含最大未知必先跑通；S3 多模型是跨切能力，先于 S4 让 doc 线白捡多模型 + verify。
- **S2 内部 a/b 切分**：把"工具循环对不对"（2a，隔离测试）与"流水线重构对不对"（2b，第一个消费者）分开验，各自独立评审。
- **S5 时机弹性**：默认 S3 后；若等待体验早早卡手，可提到 S3 前。
- **S6 可提前 dogfood**：S2 落地即有"单模型同步"可用品，此时把**最小打包**（仅 `/breakpoint:review`）提前，能在真实 diff 上感受质量、反哺 S3/S4 调参。代价是一次上下文切换，不作默认。

---

## 4. Phase-1 遗留 Minor 并入映射

| 遗留项 | 并入 | 理由 |
|---|---|---|
| `max_concurrency` 信号量未实现 | S2-2b | 分包扇出引入真并发处限流（2a 单条循环无并发需求） |
| correlate 真实 LLM 调用（桩） | S3 | 与 verify 一起把 consolidate 做实 |
| 分包 import-graph 增强（轻量推迟项提上来） | S3 | 确定性抓跨目录跨名显式依赖；不上 LLM 分包（详见 §S3 关键构建决策）；与 correlate 配套 |
| reqwest rustls 默认 aws-lc-rs → 可选 ring 后端 | S2-2a | 引擎期一并处理 TLS 后端依赖 |
| ENV_LOCK 测试加固 / LC_ALL 优先级测试 / zh_TW→Simplified | 就近 plan 顺带 | 触及相关代码时随手补 |
| select() 不按 mode 过滤 / title_jaccard 死代码 / verdict 边界测试 | 就近 plan 顺带 | 改到对应模块时清理 |

---

## 5. 执行约定

- 每阶段独立进 **superpowers:writing-plans** 出 task 级计划（含完整 TDD 代码），再 **subagent-driven-development** 执行：fresh implementer + reviewer per task，model 选型（haiku 纯转写 / sonnet 集成 / opus 最难 + 终审）。
- 进度账本：每阶段新建/续写 `.superpowers/sdd/progress.md`（git-ignored scratch），resume 以账本 + `git log` 为准。
- 分支：每阶段一条 feature 分支，完成走 finishing-a-development-branch。

---

## 6. 起步建议

从 **S2-2a（工具循环引擎）** 起步——地基、未知最大、可 FakeProvider 完整隔离测试、零 key 依赖。确认后进 writing-plans 拆 S2-2a。
