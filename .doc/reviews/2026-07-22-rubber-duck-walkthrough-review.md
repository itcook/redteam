# 2026-07-22 · 橡皮鸭走读四路评审(重规划依据)

> 状态: 已交付 · 未发布阶段全仓走读,四路并行(评审管线核心/provider 层/状态·证据·pack 层/prompt·输出·插件面),产出重规划 roadmap M17-M20 的事实底座。任何现有决策/实现都可推翻(用户 2026-07-22 定)。

## 走读方法

Rubber Duck Debugging:每路 agent 把子系统主流程「讲给鸭子听」,凡解释不通/别扭/过度复杂处即为发现。每条发现要求真实文件:行支撑、禁止臆测。四路互不重叠;并发组刚交付(opus 终审+dogfood 双过)明确豁免重挖。

## 四大结构性问题(每路一个,重规划主轴)

1. **尾段双份实现**(评审管线):全量走 `assemble_report`(cli.rs:188 TailCtx 本为统一尾段而抽),增量却在 `run_incremental_review`(cli.rs:920-975)手抄同一套 refute 折账/Usage.refute Box/mark_dismissed/compute_verdict/报告组装——「验证→裁决→报告」这条最关键的正确性链维护成两份且已开始分叉。修=assemble_report 泛化成「尾段引擎 + 可选 carry/merge 钩子」,cli.rs 顺势拆 spec/second_pass/job/incremental 模块。
2. **provider 抽象上下颠倒**(provider 层):整层围绕多轮 agentic tool_loop 搭建,而 M15 后无工具是默认——跑得最多的默认路径在它从不真正执行的 12 轮 loop+FINALIZE 机器里空转;语义恰配默认档的 `DirectProvider` 反而是死码,连带 live 解析器 `parse_lens_output`(direct.rs:39)一起被埋。修=默认路径抽薄 `single_shot + repair`,tool_loop 收缩为 tools=on 诊断档独立分支,DirectProvider 删除。
3. **state 薄壳历史夹层**(状态层):`state/{job,store}.rs` 是 SQLite 迁移后没清的旧门面——不隐藏 SQLite、不加不变量(守卫真身在 db.rs `job_update_guarded`),只做 `open→操作→map_err(io::Error::other)→被调用方 let _ = 丢弃` 的机械转发;每次调用全量重开连接(create_dir_all+8 表 DDL+WAL pragma),sink 每 cell ≥2 次。修=合并为持 ProjDb 的单一 store 类型,anyhow 错误直通+落盘失败上报,「首建 vs 守卫更新」类型区分,消灭 pub 裸 `write_state` 陷阱。
4. **ship.md 自然语言状态机**(插件面):135 行散文承载增量链推进/停滞守卫/销案争议刹车(同 id 二现→停)/升级刹车链(四态迁移+每 id 至多一次复裁)——全是可精确判定的编排却靠 LLM 逐轮记账,逆着「内置命令优先」产品原则(review/status/result 三壳合规极薄,唯 ship 例外)。修=停机判定与刹车链计数下沉 `redteam ship-step` 纯决策子命令,md 只调二进制拿决策+执行副作用。

## P1 缺陷(独立可修)

| # | 位置 | 问题 |
|---|---|---|
| D1 | provider/backends.rs:393 | OpenAI 兼容端点 HTTP 200+`{"error":…}` 体 → parse 无 choices → 误分类 Parse(不可重试)烧掉整格;与「envelope 抖动可重试」意图相悖。疑与 T35 Kimi 尾部超时同源。修=200 体先探 error 字段映射可重试类 |
| D2 | prompt/mod.rs:204-242 | `render_incremental_section`:two_layer_manifest(B 下可控)与 claimed 卡 id/file/title/quote 全未过 neutralize_structure,注入点在 output_contract 与 bundle 之间可越权改契约(=T46③ 实锤);且 title/file 未 oneline,proj.db 换行可伪造假条目(E11-Q5 在 render_precedents 已防、此处漏防) |
| D3 | main.rs:312-317 | `result --format md` 于 md 缺失时 `.or_else(read_result)` 静默回落 JSON——ship.md:102 `> rN.md` 依赖此路径,会把 JSON 写进 .md |
| D4 | cli.rs:900-909 | carried∪new 跨轮无去重:dedup 只跑 new,merge_carry 仅按精确 id 合并;跨轮 quote 漂移致同 bug 双条入报告并各计 stats |
| D5 | main.rs:545(原) | worker deadline 裸 RMW 覆终态——cancel 落读写间被覆回非终态,首个终态封盘第三例绕过;走读+M16 终审双漏,M17-c T2 随 API 收紧修复+T2-fix 补锚 |

## P2 发现(按主题归组)

**预算/pack**:omit 段(~12KB)与 body 标题不计入 budget、理论可略超(pack/mod.rs:160);改动行双计(diff 段 own_diff_bytes 预留 + 全文注入,pack/mod.rs:19)——单格全审下 diff 段独立存在性值得复核(联 T27)。
**验证/裁决**:refute targets 不看 conf_floor(verify.rs:199-203,高严低置信孤证永不 blocking 却烧 refute LLM);source_snippet ≤64KB 回整文件、line_start/end 未用(verify.rs:43-47);exit code 区分不了 Incomplete 与降级 approve(cli.rs:115-121,均返 2)。
**安全/健壮**:双 denylist 分叉(cli.rs:369 is_needs_denylisted vs :448 is_sensitive_spec_path,同族独立=漏改风险);`.env` 条件各有 2 条死判(cli.rs:373/459);git base 位置参数未加 `--` 分隔(evidence/git.rs:45/127/146,低危);检查点/结果写全 `let _ =` 吞错(cli.rs:1024-1051/1345-1352——R2.1 流式检查点卖点静默失效风险);--conf-floor 无 0..1 值域校验(main.rs:52-53)。
**provider 杂**:退避 `200 << (attempt+1)` 首试实为 400ms 与注释不符(provider/mod.rs:150);probe 一律 bearer_auth、配 Anthropic 必假失败(model/probe.rs:26);derive_kind 返 String 再 parse 往返(model/mod.rs:91);默认 base/MAX_TOKENS 三处常量手动同步(backends.rs:8);direct.rs:101-149 body 构造双份(随 DirectProvider 删)。
**prompt/输出**:「omission list→降 confidence」指令三处重复(bundle 模板:22/:80 + prompt/mod.rs:301,历史批次叠加);lens 段序 L1-L4,L0,L5 编号错位(模板:8-13);tool_specs 已撤 read_file 但 executor 仍认、e2e 依赖(tools/mod.rs:151/160);Duration 恒 `≈ X.X mins`(output/mod.rs:85);TwoStage 与 Off 在 provider 层完全等价、语义跨层散落(config/mod.rs:46+main.rs:342)。
**命名**:dossier 模块名不副实(引用点引擎已拆,现存 diff/符号解析核心被 pack/verify 活跃复用,非死码勿删)→ 宜改名 symbols/diffsym;LensRequest.model 实为路由键(registry.rs:6/direct.rs:359 自认)→ 正名 route_key(=T34 落点);write_state pub 裸写与封盘不变量冲突、生产仅首建一用(state/job.rs:117)→ 降权/改名 create_initial。
**测试**:e2e 仅 4 例(working-tree 全量+agreement/refute/failed_models),增量轮/md 渲染/台账注入/注入面回归/退出码链全缺——发布覆盖面不足;main.rs:369-490 Review 分支三路径内联过长宜抽 dispatch_review。
**维持现状**:hash.rs/project.rs 单一职责清晰不并;positioning 非残留勿删。

## 与旧账对照(核实结论)

- T04(quotePath)✅ 已修(git.rs:74 quotePath=false 统一配置);T06(dogfood 仪器)✅ 已拆(feedback.md 不存在、ship.md 零命中)。
- T44 增量 --spec 静默忽略被走读再确认(cli.rs:887-892)→ 归尾段统一重构顺带修。
- T46③ 第五注入面由「记债」升级为 P1 实锤(D2);T34 正名升级为 provider 重构组成部分。
- T02 分段读:无工具默认下 tools=on 仅诊断档,不值得优化 → 建议作废。
- 支柱 3(驱逐/分段读):针对 tool-loop 二次方尾巴,无工具默认后病灶已不存在 → 建议作废归档;支柱 4 维持后置。
