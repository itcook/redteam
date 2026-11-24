# redteam TODO · 已交付存档

编号对照 `summary.md`。保留决策依据与残余观察，供后续会话考古。

---

## C 组 · 卷宗内容定稿 ✅（2026-07-06，dev 720f149，328 lib 绿零警告，make verify 绿，sonnet 评审无 Crit/Imp）

**T01② 单行超长 context 截断**（真修）：minified/生成物单行可达 MB → `reference_points` 建 ±3 context 时
一个引用点即撑爆 28KB 卷宗预算、卷宗只剩遗漏注脚。新增 `truncate_line(&str)->Cow<str>` 把单行内容截 ≤512B
（UTF-8 char 边界回退，绝不切多字节字符中间，评审实证 CJK 512→510 精确落点）另加固定截断注记，context 逐行过它。

**T01④ `\ No newline at end of file` 跳过**（防御性卫生，非活 bug）：该标记原走 `_ => new_line+=1` 多加一；
当前架构下 drift 被掩盖（hunk_anchor 只取每 hunk 首个改动行、标记必在其后；每 @@ 又 parse_hunk_new_start 重置
new_line 不跨 hunk），无可观测出口——评审手工删守卫重跑测试仍通过，实证「无害」诚实。加 `starts_with('\\')`
守卫防未来 per-行锚定时暴露。**T01④ 余项**（大 hunk 跨两函数只锚首个查全洞、脏树行号错位）按 note 降级无害保持
已记录，未在本批（修查全洞需 per-改动行解析、撞 T01① 性能顾虑）。

## D9 前置批 ✅（2026-07-06，dev 3a7a234..444c0e3，346 tests 绿零警告，make verify 绿）

**T24 · cell-fail 观测 + FINALIZE 截断修复轮**（3a7a234，sonnet 评审 Approved 六硬约束全过）：根因=OpenAI 系 body
无 `max_tokens`（吃默认 ~4K）+ FINALIZE 强制 json_object → findings 截断半截 JSON → parse 报笼统「no schema-valid
JSON」→ Parse 不可重试烧整格。修：`AssistantTurn.finish_reason`（openai finish_reason/anthropic stop_reason）→
parse 失败错误带诊断（length=截断 vs 形状违约）；FINALIZE 失败点加**一次**修复轮（REPAIR_PROMPT + turn_final，
iterations=max_iters+2，token 累加不漏计），不动 retry_classified（保 T20 边界=新请求非同请求重试）。残留见 T25。

**T03 · refute 趟 token 入账**（a96fd1c）：`refute_one`/`verify_findings` 返回值加 usage 元组，cli `assemble_report`
（3 caller 共享尾）+ `run_incremental_review` 把 refute 用量折进 `run.usage` 总量 + 记 `Usage.refute: Option<Box<Usage>>`
分账子块（镜像 stats.new idiom，serde default 向后兼容）。补齐支柱 5「refute 未入账」欠账。

**T01⑥ · 卷宗降级可观测**（9e86ffc）：`RunMeta.dossier: Option<DossierMeta{bundles,injected,suppressed}>`——
`build_dossiers` 返回覆盖统计，穿 `TailCtx.dossier_meta` 进 RunMeta；A/B 与生产可见卷宗是否生效（几 bundle 拿到
内容、是否被 REDTEAM_NO_DOSSIER 抑制）。doc/resume 路径 None。

**T01③ · 引用点确定性排序**（444c0e3）：`reference_points` 每符号 hits 按 (path,line) 排序——WalkBuilder 遍历序
=OS 目录序非确定，cap-10 留哪 10 跨机不定；排序后 A/B 可复现。

## T14 · dogfood 直修 ✅（2026-07-05，feat/dossier 7c7fce6+9470fd0）

regex 依赖注释事实错误已修正（grep-regex 只拉 regex-automata/regex-syntax，不含 regex 本体）；
`render_dossier_section` 中和摘录内字面 `</dossier>` 防破块（终审 Minor #3 + dogfood 双臂 flagged 的注入面）。
注：dogfood 报的「status/result JSON 非法」经查为**误报**——工具输出合法，是 zsh `echo` 默认解释反斜杠
转义所致（管道直连/写文件均正常），非工具 bug。

## T15 · 主题级 dedup/抑制缺位 ✅（2026-07-02 ship dogfood 最高信号工具缺陷，2026-07-03 交付 42d55c2，grill 定案 B1）

根源=skeptic 每轮从零裁判看不到既往人裁（生成侧勿再报注入管不住，裁决成本才是痛点）。
修法：dismissed_notes 穿线 TailCtx→verify_findings→build_refute_prompt，refute 双模板加
`<dismissal_precedents>` 判例段（强异议镜像：同主题/换锚变体无新证据→refute 引判例；有新证据→按代码裁），
refuted 在 ship 协议本就静默跳过——裁决成本人转机器。targeting 门槛未动（仍仅孤证 blocking），
**已知残余：medium 及以下主题变体仍漏到人**，dogfood 证实漏网主体是低严变体再升级（候选：机械预筛 B3）。
同批落地：bundle 全变更集清单注入 9f894ca（跨 bundle「无证据实现」误报）、rec 串台 60d9db0
（merge_into 删「更长 rec 胜」）、status 起手 mode stamp e106a0c、ship.md 尾流守卫 fa9cd8b
（连续 2 轮属实全衍生且不增→原始面已清收工；后随 M13 退役）。

## T16 · refute 模板↔解析 schema 契约同源测试 ✅（2026-07-03 feedback subagent 建议，同日随自审批交付）

r1 第三轮 dogfood 破案——refute verdict 判定是「非空性启发式」，守约模型想表达 upheld 也输出 finding
（"Claim upheld:"）被误判 refuted（12bbeb4c，r2 六字段契约修复的衍生盲区）。修：模板加 "REFUTED: " title
前缀机器信号+「同意只能用空数组」排他条款；classify_refute 语义解析（upheld 前缀→upheld；无前缀 fallback
非空启发式+contract-drift warning 可观测）。同源绊线：prompt 测试断言前缀信号+六必填字段指引在场，双模板同断言。

## T17 · OUTPUT_LANGUAGE 遵从性优化 ✅（2026-07-03 提出，同日交付 0cb4b9e）

事实核查——注入的本就是英文语言名（非 zh/zh-CN 代码），真问题=英文名对部分模型约束力弱（dogfood r1
DeepSeek 马来语实证）。映射表改双语（"Simplified Chinese (简体中文)" 等六语），母语名加固遵从。
效果待后续 dogfood 观察（马来语类违约是否复现）。

## T18 · 耗时统计 + cell 运行调试信息 ✅（2026-07-03 用户提出，同日 grill 五问定案并交付；随发布保留——数据字段+读取视图，非拆除型仪器）

CellRecord 加 duration_ms/tokens/attempts（旧记录 serde default 兼容）；Phase 扩 Refute/Correlate +
PhaseStamp{phase,at,note} 时间线（refute 注记 targets 数，refute_targets 单一真相源）；`status <job> --cells`
cell 级明细；table PHASE 列 running 期带停留时长与注记。**未做**（YAGNI 记录）：provider 内部同 model
重试计数（要动 LensCall 契约，候补链 attempts 已覆盖换家诊断）、refute 逐条耗时/实时进度（join_all 瓶颈=
最慢单条，起止+规模够定位）、cell started_at（duration+phase 线足够）。
（后续 M14 前置件又加 iterations 列——见 roadmap M14。）

## T19 · dismissed 台账机器化 + state 层入库 ✅（2026-07-03 grill 12 项定案全落，c4b0235..0a61cc7）

design：`../design/2026-07-03-dismissed-ledger-machinization-design.md`。单库 `.redteam/proj.db`
（rusqlite bundled；turso beta 不用不留期权）；dismiss 五子命令（add --from 快照自动抄/list/show/
set-status/gc 锚失效回收）；zero-config 自动加载+run.ledger 披露+--no-dismissed；**review 勿再报注入删除**
（生成侧十连穿透实证无效），判定后置=机械预筛（命中判例变体无视 agreement/severity 强制 refute）+
相关性判例渲染（替代 16hex 伪随机取前 1/4 的注入盲区）；**state 层整层退役**（bucket/TTL/prune/
REDTEAM_STATE_DIR/--workspaces/state_ttl），jobs/cells/results 入库，**评审结果持久永不过期**；
53 卡迁移完成（旧 md 归档 dismissed-md-archived/）；ship.md 联动薄化。已随后续 dogfood 活体验证。

## T20 · envelope 解码失败误分类为不可重试 Parse ✅（2026-07-01 记，2026-07-02 修复于 fix/envelope-decode-retryable）

根因=推理模型长思考后上游放弃 → HTTP 200 + 纯 keep-alive 空白 body → serde EOF 被误映成不可重试 `Parse`，
绕过在场的 `retry_classified` → 一次抖动烧掉整个 cell。修法：`provider/mod.rs` 新增 `decode_json`/
`parse_envelope_json`（envelope 层解码失败→`Unavailable` 可重试 + 携带 body 前 200 字节证据）；**四处**
同型点收敛调它（原记三处漏了 `backends.rs` openai-tool 路径，而 DeepSeek 走 tool-loop 实际命中的正是它）；
`Parse` 收窄为仅模型内容形状。遗留观察（保留）：`extract_*`/`parse_*_turn` 缺字段（200+error 对象场景）
是否也该重试——先不动。

## T21 · lgtm 心跳判死阈值误 cancel ✅（2026-07-02 记，同日修复）

轮询原用「心跳 `last_active` 停滞 >~5min → cancel 重起」判卡死，但 last_active 是 **cell 级**刷新
（推理模型单 cell 超长 turn + finalizing 尾段设计上静止）→ 误 cancel 正常慢 job。**根因认知修正**：
后台 job 总有 40min deadline watchdog（`main.rs::JOB_DEADLINE_SECS` + `run_job` tokio::select! 硬界
到点 TimedOut+partial），永不永久卡死——SKILL 的 5min cancel 重复且有害。修法（②极致）：SKILL 删自建
心跳判死，完全信任 deadline，轮询只等终态，last_active/phase 仅作进度信息。纯 prompt；codex 镜像同步。
可选后续（未做，YAGNI）：JOB_DEADLINE_SECS 40min 改 `--deadline`/config 可配。

## T22 · CLI 参数面重审 ✅（2026-07-02 记，同日交付 0dd225e + d92b0e7）

**产品原则确立并记入项目 CLAUDE.md——内置命令优先/裸终端可用**（功能尽量落二进制，prompt 只做裁决编排，
--help 与代码同权重）。批A：review 13 flag+TARGETS、model setup 6 flag、--format 值域、顶层 about、
`--version` 全补齐。批B（原则首个应用）：`--dismissed` 下沉双效——豁免之外**台账内容自动注入 prompt
「勿再报」段**（零格式解析原样注入+截断，csv 形态仅豁免），ship.md 删宿主拼 --focus 摘要的整段编排，
--focus 回归纯用户语义，裸终端白得同能力。
（注：该「勿再报」注入后经 T19 dogfood 实证生成侧无效，已删除、判定后置化——见 T19。）

## T23 · `status` 人类友好视图 ✅（2026-07-01 记，2026-07-02 移入 M5 并交付，commit de2442a）

`redteam status --format table`（opt-in），默认 JSON 契约不动。result 渲染 YAGNI 未做
（被 ship 裁决流程 + M12 记分卡覆盖）。

## T25 tokens 半件 + T26 + T46 + T47 + T48 ✅（随 M18 交付，2026-07-28 收官刷账）

- **T25 tokens 半件**（M18 批2）：orchestrate 候补链整链失败路径，失败 CellRecord 回填累计
  usage——真实烧掉的 input 不再记 (0,0,0)。残余半件（自收敛路径无修复轮）改号 T25-余，数据门控留 followup。
- **T26**（M18 批2）：model setup 收尾自动跑该 provider probe——失败警告但仍落盘 + 提示
  `model check`（grill 裁决从「拒写」软化为「警告不阻断」，保留人工判断空间）。已知局限照旧：
  瞬时/并发 403 单发 probe 抓不到。
- **T46 安全组四件**（M18 批1）：①spec/needs 拒读事件进 run.warnings ②两处拒读 eprintln DRY
  收敛单函数 ③增量注入面=D2 中和（manifest+claimed 卡 neutralize+oneline）④denylist 单源合一
  （并集收紧，+.netrc/.p12，清 .env 死判）。
- **T47 并发组测试债三件**：①state_json 反序列化失败分支 ②reap 降级分支（随 M16-M17 批前置）
  ③**真 panic 全链路 e2e**（M18 批3 破局：REDTEAM_TEST_PANIC env 钩子 fan-out 前触发，真 spawn
  二进制断言 Failed+error 含 panic+封盘不变式——无需真模型，e2e 回归资产永久保留）。
- **T48**（M18 批2）：verify source_snippet 用 line_start/end 行窗（±上下文）替代 ≤64KB 整文件；
  谎报行号 clamp 到文件实际范围并注记。
