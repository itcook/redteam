# 全量代码评审遗留（2026-07-03）

> 一轮「基于当前代码而非 diff」的全面评审产出。来源：主评审 + 6 个并发 specialist
> （并发/安全/provider/契约/测试/可维护性）+ Codex 独立对抗。基线 commit：`9c74744`
> 评审时；已清批见下。**本文件是维护者私有 backlog（`.doc/` gitignored，不随仓分发）**。
>
> 验证标记：**✅ 主评审已读码复核** · 🔶 specialist grep 确认、主评审未逐行复核 · ⚠️ 设计取舍/威胁模型判断
>
> 状态：⏳ 待办 · ✅ 已交付 · 🧊 看情况

---

## 已交付（本轮当场清掉，commit `a75cabc`）

零争议的机械卫生 + 契约漂移批，已实施并入库，此处仅存档对照：

- ✅ `--models` help 改如实（实际 replicate 复评，非 priority 候补链）
- ✅ 5 处 clap `value_parser` 补齐：`--block-threshold` / `--format`(review+result) /
  `status --format` / `dismiss list --status`+`--format` / `--key-store`——非法值 fail-fast
  （`--block-threshold` 此前静默折 medium 改 reject 阈值无声）
- ✅ `result.md` 删「省略取最近一次」（job_id 实为必填 positional）
- ✅ `ship.md` 轮询终态补 `cancelled`（第四终态）
- ✅ 删 `next_steps` 死字段（恒空却序列化进 JSON 契约）
- ✅ 撤上批错加的 `Phase::Correlate` 打点（打在 correlate 恒空 stub 上）+ 删孤儿枚举值
- ✅ stale 注释 6 处：`partial.ndjson`×2 / `result.json 落盘` / `args 文件` /
  `changed_files「仅 main」`→ 改为 proj.db/实际调用方措辞

---

## P0 — 正确性/安全（需设计决策，未动）

### 并发组（跨进程 state，一组关联）· ✅ Codex + 并发 specialist 双源

根问题：state 层的跨进程写全是**无条件全量覆写陈旧快照**的 read-modify-write，
进程内 `Mutex` 管不住跨进程；worker 猝死后无回收路径；cancel 对复用 pid 误杀，
三者构成死结（猝死 job 只能靠 cancel 清，而 cancel 又会误杀）。

- ⏳ **run_job 收尾 / cancel / JobCellSink 无条件覆写**（`src/cli.rs:815`、`src/main.rs:539`、
  `src/cli.rs:559`）：cancel 与 worker 收尾交错时，Done 终态（含 exit_code/result_path）
  可被回滚成 Cancelled、或 Cancelled 被 worker 回写成 Running。失败场景：用户 cancel 一个
  正在收尾的 job → worker 抢先落 Done → cancel 用陈旧快照覆写回 Cancelled 且 exit_code=None
  → `redteam result` 经 `unwrap_or(0)` 把 reject 报告以退出码 0 放行。
- ⏳ **worker 猝死后状态永久 running**（`src/cli.rs:801` deadline 只在 worker 进程内 tokio::select
  执行）：worker 被 SIGKILL/OOM/panic 杀死后状态永久 running，无 pid 活性检查、无回收，唯一
  出路是人工 cancel。（关联：`join_all` 单 cell future panic 不隔离，绕过两条终态路径，
  stderr 已指 /dev/null → 无痕永久 running。`src/orchestrate/mod.rs:237`）
- ⏳ **cancel SIGTERM 无 pid 活性/身份校验**（`src/main.rs:536`）：pid 被系统复用后
  `redteam cancel <job>` 误杀同用户无关进程。
- **建议方向**：①state 写改条件写（同事务重读 status，终态则放弃写入，on_cell/on_phase/
  on_meta/run_job 收尾共用一个 guarded write helper）；②status/result 读到 Running 时
  `kill(pid,0)` 探活 + 超 `deadline_at` 就地折 TimedOut/Failed 终态并落 partial（或 `status --reap`）；
  ③cancel 前探活，ESRCH 只清状态不发信号；④run_job 外层 `catch_unwind` 把 panic 折成 Failed。
  这是一次并发架构小重构，**方案要先定再动**。

### 安全组 · ⚠️ 威胁模型 = 评审不可信仓库

- ⏳ **config.toml 项目层可被恶意仓库覆盖**（`src/config/mod.rs:142`，✅ 已读码确认）：
  项目层 `.redteam/config.toml` 在受审仓库领地内且与用户配置深合并（`merge`，右侧优先级高）。
  评审恶意仓库时，其自带的 `.redteam/config.toml` 可覆盖既有 provider 的 `base_url`
  （把请求+真 key 发到攻击者服务器，`resolve_key` 回落 secrets.toml）或新增 provider 用
  `api_key_env` 指名任意环境变量（如 `AWS_SECRET_ACCESS_KEY`）经 `default_models` 自动选中外传。
  **这是评审工具最该防的反打面。** 方向：项目层只允许无害键（[review] 的 lenses/threshold），
  拒绝 `providers.*`（至少拒 base_url/api_key_env）；或首次加载非用户层 provider 要求信任确认。
- ⏳ **被审内容 prompt injection**（Codex Critical + `src/prompt/mod.rs:151`、
  `src/prompt/templates/adversarial-code-bundle.md`）：被审 diff/文档内容直接内联进 prompt 的
  markdown fence，恶意内容含 ` ``` `/`</bundle>`/伪 `<output_contract>` 可把「证据」提升成
  指令诱导返回 `{"findings":[]}`（空 approve）。LLM 评审工具的固有威胁面，防御要分层
  （不可伪造分隔围栏 + 模板声明围栏内为数据非指令），设计级投入。
- ⏳ **render_precedents 二阶注入**（`src/prompt/mod.rs:199`，✅ 确认）：台账卡 title/quote/reason
  （源自 LLM finding）原样拼进 refute prompt 未剥换行未围栏。恶意模型输出一次含换行+伪造
  `- [id] ... rationale` 判例行的 title，被 dismiss 入库后即成跨轮持久的二阶注入，诱导 skeptic
  系统性 refute 真 finding。方向：注入前单行化 title/reason + 转义行首 `- [`，或整卡套围栏。

---

## P1 — provider 健壮性（多为独立可修，需逐个定策）

- ⏳ **parse_lens_output 花括号扫描不识别字符串字面量**（`src/provider/direct.rs:52`，✅ 确认）：
  深度扫描 `{`/`}` 不跳过 JSON 字符串内的花括号，findings 的 quote/recommendation 常含不配对
  `{`/`}`（单行代码引用），JSON 被散文包裹时深度计数失同步，且首候选解析失败即 `break` 不再
  尝试后续。整格降级为不可重试 Parse、烧光已耗 token（Anthropic 收尾轮无 json_object 强制，
  散文包裹常见）。修：扫描维护 in_string/escape 状态机跳过字符串内花括号；失败后从下个 `{` 续试。
- ⏳ **tool-loop 无累计总时限**（`src/provider/tool_loop.rs:68`，✅ 确认）：只有单轮 150s，无总
  deadline。最坏 (12 探索+1 收尾)×(1+retry=2)×~150s + 退避 ≈ 90min，超宿主 40min watchdog 被外杀
  全损，裸终端（设计底线）完全无上限。**这是 r1 那 27 分钟慢的结构性根源之一。** 修：run_tool_loop
  记起始 Instant 设整体 deadline（10-15min，可配），每轮前检查剩余预算，不足则直接进强制收尾轮。
  **建议优先——修了 dogfood 体验直接改善。**
- ⏳ **探索期纯文本轮解析失败即终结**（`src/provider/tool_loop.rs:80`，✅ 确认）：探索期模型返回
  无 tool_calls 的纯文本轮（闲聊/提前 end_turn）被当最终答案立即 parse，失败即不可重试 Parse
  终结整格，明明还有轮次预算却不给挽救。修：文本轮解析失败且有预算时 push FINALIZE_PROMPT 再给一轮。
- ⏳ **HTTP 200 + error 对象归不可重试 Parse**（`src/provider/backends.rs:257`，🔶 provider+Codex）：
  200 + 合法 JSON 但含 error 对象（OpenAI 兼容代理过载/限流常见形态）被归不可重试 Parse，且错误
  信息无 body 预览。已修 envelope 踩坑在「合法信封 + error 对象」层的重演（direct.rs:161/181、
  backends.rs:132 同模式）。修：抽取失败先探 `resp["error"]` 按 type 分类（overloaded/rate_limit→
  可重试），兜底 Parse 携带 200 字节预览。
- 🧊 **失败路径丢 usage**（`src/provider/tool_loop.rs:110`，🔶）：ProviderError 不带 token 计数，
  收尾轮 parse 失败时跨 12 轮累加的 in/out_tok 丢弃，失败 cell（恰是烧 token 最多的）成本核算
  系统性低估。修：ProviderError 增可选 (input,output) 或 run_tool_loop 返回 (Result, Usage)。
- 🧊 **单轮 tool_calls 数量无上限**（`src/provider/tool_loop.rs:89`，🔶 conf5）：模型一轮请求 N 个
  read_file 线性撑大上下文，最终 413/400 晚期不可重试 BadRequest，白烧前面轮。修：每轮限前 K 个。
- 🧊 **tool_calls arguments 非字符串静默降级**（`src/provider/backends.rs:214`，🔶 conf4）：
  不严格兼容网关把 arguments 返回为对象而非字符串，`as_str` None 后静默降级 `"{}"`，真实参数丢失。

---

## correlate 门面处置 · ✅ 确认（`src/consolidate/correlate.rs:24`）

- ⏳ **correlate() 是恒返回空的 stub**：`--no-correlate` flag、`cfg.review.correlate`、
  `CorrelateOut`/`ClusterRaw` 结构、doc 线的 correlate 调用、`report.clusters`、output 的
  `## Correlated Clusters` 段——全是围绕一个 no-op 的脚手架。本轮已撤误导打点+改诚实 help，
  但**核心决策未做**：实现 LLM 关联分析 vs 删整条 clusters 门面。产品决策，待定。

---

## 死码 · 🔶 maintainability specialist（grep 已验，主评审未逐一复核）

- ⏳ `OrchestrateOpts.no_fallback`（`src/orchestrate/mod.rs:37`，conf9）：全仓无 true 赋值，
  `if opts.replicate || opts.no_fallback` 右侧恒假。删字段及两处条件。
- ⏳ `DirectProvider`（`src/provider/direct.rs:16`，conf7）：无生产使用方（registry.rs:9 注释自认），
  仅 `#[ignore]` smoke 测试触达；只有 `parse_lens_output` 被 tool_loop 真用。方向：parse_lens_output
  迁 tool_loop.rs/provider mod，删 DirectProvider 全套（要「无工具单发」时从 git 史找回）。
- ⏳ `ReviewInput.collection_guidance`（`src/domain/input.rs:12`，conf9）：只写不读，占位符
  `{{REVIEW_COLLECTION_GUIDANCE}}` 在所有模板中不存在。删字段及 evidence/mod.rs 拼接代码。
- ⏳ `collect()` 的 Mode::Code 分支（`src/evidence/mod.rs:34`，conf8）：生产不可达（code 线走
  changed_files→run_code_review），且若走到会 panic（TailCtx.root 恒 None）。连带 git::collect_code、
  decide_inline、INLINE_MAX_* 死码。收窄 collect 为 doc 专用或入口 bail 非 doc。
- 🧊 `Cluster.severity`（`src/domain/finding.rs:101`，conf8）：恒 None（correlate 恒空）。
  correlate 实现前不删——它是 clusters 契约一部分，与 correlate 决策绑定。

---

## 重复/可维护性 · 🔶

- 🧊 `run_one_bundle`/`run_one_doc` 逐行双胞胎（`src/orchestrate/mod.rs:295`，conf8）：候补链构建/
  截断/重试循环/CellRun 组装全同，仅 fallback lens 与错误标签前缀不同。链语义改动须双写。
  方向：合并为 `run_one_cell(provider, prompt, opts, fallback_lens, err_label)`。
- 🧊 scope/mode 字符串→枚举解析在 cli.rs 三份拷贝（`detect_mode_from_args`/`run_review`/
  `run_review_dispatch`，conf8）：新增 mode/scope 值需改三处。抽 `eopts_from_args`。
- 🧊 lens_ids 解析 + block/floor 折算三处逐字重复（`src/cli.rs:724`，conf6）。抽
  `resolve_lens_block_floor`。
- 🧊 `ANTHROPIC/OPENAI_DEFAULT_BASE` 三仓复制（direct.rs/backends.rs/model/mod.rs，conf9）：
  靠注释「保持同步」防漂移。统一引用 `crate::model::*`。
- 🧊 三条流水线 load_ledger 取根方式已分叉（run_review 自取 current_dir / run_code_review 传入
  root / run_resume cwd 参数，`src/cli.rs:320`，conf6）：当前恰都等于进程 cwd 才未出事。
- 🧊 `cli.rs` 1547 行有清晰边界：527-888 是 job 运行时层（now_unix/JobCellSink/partial_report/
  run_resume/run_job）+ 对应测试，可整体切出 `src/job.rs`，不动签名。

---

## 测试缺口 · 🔶 testing specialist（高价值：有 bug 会静默产出错误评审或丢数据）

- ⏳ `dismiss_gc` 零测试（`src/main.rs:789`，conf9）：--apply 批量改写台账状态，retain 消活逻辑
  反转即把存活卡误标 stale，豁免/预筛立即失效、已驳回误报下轮全复活。Given 三张卡（quote 在/
  quote 消失/human-confirmed+quote消失），When gc(apply=true)，Then 仅第二张转 stale。
- ⏳ 跨连接并发写无实测（`src/db.rs:49`，conf8）：busy_timeout 是唯一防线，删掉不红。
  Given 同 db 两连接，When 双线程并发各 upsert N 次，Then 全 Ok 且行数完整。
- ⏳ code 候补链无测试（`src/orchestrate/mod.rs:253`，conf8）：首家失败次家成功时 CellRecord 的
  model=胜者/attempts=2 无绊线（这俩是 `status --cells` 与 resume 重放的数据源）。
- ⏳ 候补链整链全灭无测试（`src/orchestrate/mod.rs:164`，conf8）：replicate=false 全灭只记 warning
  不记 failed_models，若 warn 上浮断链则 0 内容静默 approve exit 0（r1 干净树假绿灯同类）。
- ⏳ resume 补跑仍失败的 Incomplete 降级无测试（`src/cli.rs:768`，conf8）：still_missing>0 强制
  Incomplete 是「部分 cell 拼成完整 Approve」的唯一护栏，删掉现有 resume 测试仍全绿。
- ⏳ 运行期 cancel 竞态无测试（`src/cli.rs:861`，conf7）：dispatch 完成时 state 已被 cancel 则
  收尾须保留 Cancelled 不覆盖 Done，回归后用户取消的 job 被静默改判 done。
- 🧊 `--dismissed` 指向非 sqlite 垃圾文件的 fail-fast 无绊线（`src/cli.rs:86`，conf6）。

---

## 其余安全（🔶 security specialist，本地工具威胁模型下多为 🧊）

- 🧊 model setup `--key` 命令行 argv（进 shell history + ps 可见）+ 明文回显 stdout
  （`src/main.rs:612`，conf7）：ship/Claude Code 宿主下回显进会话转录。改 stdin 读 + 打码。
- 🧊 secrets 写非原子 + 短暂 0644 窗口（`src/config/secrets.rs:33`，conf5）：先默认 umask 写明文
  再 chmod 600。改 `OpenOptions.mode(0o600)` 直接创建或临时文件 rename。
- 🧊 dismiss list/show 终端未过滤 ANSI 控制字符（`src/main.rs:707`，conf6）：LLM 来源 title 含
  ESC 序列可在维护者终端伪装/擦改。加 sanitize_terminal 助手。
- 🧊 dismiss gc 全仓扫描不限定 `DismissCard.file`（`src/main.rs:808`，security+Codex Low）：
  攻击者在无关文件/fixture 放旧 quote 即阻止 stale 回收，使过期判例继续豁免。限定 card.file。

---

## 覆盖边界（🔶 记录而非 bug）

- lock 文件（Cargo.lock 等）被 bundle noise 过滤丢弃（`src/bundle/mod.rs:24`）：只改 Cargo.lock 的
  供应链变更 → 空 bundles → 仅 warning、verdict 仍 approve exit 0。redteam 定位审源码逻辑非依赖
  版本，属已知边界；0-cell warning 已加（上批），但 verdict 未改。若要覆盖供应链需专门 lens。
