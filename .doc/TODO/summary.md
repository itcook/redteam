# redteam TODO · 总览

零散的、够不上 roadmap 主题级别的 polish / UX / 技术债项。主题级排期见 `../roadmap/`。
明细:开放项在 `followup.md`,已交付存档在 `done.md`(按编号对照)。
状态:⏳ 待办 · 🧊 看情况 · 🔨 进行中 · ✅ 已交付 · 🚫 作废 · ➡ 已吸收进里程碑(在该里程碑内追踪,此处只留指针)
**重编于 2026-07-22**(橡皮鸭走读重规划;走读 P1/P2 全清单见 `../reviews/2026-07-22-rubber-duck-walkthrough-review.md`,
其中结构性四件归 M17、缺陷与 P2 清扫归 M18——不再逐条开 T 号,凭评审文档追踪)。

## 开放项

| 编号 | 一句话描述 | 状态 |
|---|---|---|
| T03-余 | 每轮 $ 成本估算(run.usage 已全,仅缺定价表折算;五支柱关账后唯一存活遗产) | 🧊 |
| T25 | T24 残留:tokens 半件✅ M18 批2(失败 CellRecord 回填累计 usage);自收敛无修复轮半件数据门控 | 🧊 半余 |
| T26 | model setup check-before-persist(check 成功才写 config,防死配置) | ✅ M18 批2(setup 收尾自动 probe,失败警告仍落盘) |
| T27-余 | per-model 真预算精度:models.dev 策展表 + 真 tokenizer 校准(3B/token 估算不准再正名);近端 pack 预算记账(omit 段/diff 双计)已归 M18 | 🧊 |
| T28 | pack 语义包裹/分槽重塑(实测 `<dossier>` 标签误导才做,可能永不需要) | 🧊 |
| T30 | 省略清单 cap→分组/提 cap(hiapi 规模频触时) | 🧊 |
| T31 | 省略清单全局 prompt 指令(实测模型无视内联提示才加) | 🧊 |
| T32 | pack 纯新增零 caller 符号「body==diff 跳 0 壳」消冗余(与 M18 diff 双计复核同区,宜同批裁) | ⏳ |
| T33 | M15-S1 终审 Minor 小抛光(注释/断言补强三件) | 🧊 |
| T35 | Kimi 尾部超时残留(3/5,20 并发重载 2 格 timeout)——疑与 D1(200+error 体误分类不可重试)同源,**D1 已修(M17-b 2026-07-27),复测待 Kimi 可达**(当前 unreachable/auth failed),复测仍在再单独治 | ⏳ |
| T37 | regex pack 持续打磨(无 indexer 永久兜底线):0 壳/梯子精度 + **超预算填充排序**(现按 diff 文件序贪心先到先得,改按改动密度/爆炸半径排序、测试文件降权——粒度 A/B 收尾「归 grill」那笔,2026-07-23 点名防丢);B 过滤已兑现 −37% 假阳 | 🔨 长期 |
| T45 | 不可信内容注入·散文级天花板 + spec TOCTOU 窄窗(结构面 D2 归 M18;散文级=LLM 固有,记录在案等业界防御成熟) | ⏳ |
| T48 | verify source_snippet 行契约:行窗切片+越界 clamp 注记 | ✅ M18 批2 |
| T49 | 二阶段/增量轮 prompt 前缀稳定性×provider 缓存:置换→追加语义重开,pack 段字节级稳定让第二趟/轮间命中 prompt cache(token 轮廓类变更独立成批) | ⏳ 后置 M17-b |
| T51 | reap_if_dead/mark_failed_panic 的 exit_code=Some(2) 极窄竞态泄漏进程码（worker 已写 result 但状态未落 Done 即被 reap）——与 exit 五档语义可能不符;pre-existing,批2 终审 M1 记 | 🧊 单独小批 grill |
| T52 | clap 解析错误退出码 2 与五档冲突——try_parse 接管:解析错误退 1、help/version 退 0;e2e 四路锚 | ✅(2026-07-28 merge 8bd6460) |
| T53 | 窗口预算最大化(768K 帽后续):悬崖入配置(VibeCodingFast 声明 256K 经济窗)+帽退役为纯注意力护栏(纯国产队可 1.5-2M)+LITM 验证门(三设计,明细 followup) | ⏳ 观察触发 |
| T50 | needs 观测驱动主动注入:统计 needs_stats 高频文件类别,预算有余时主动进 pack | 🧊 数据门控 |

## 已吸收进里程碑(指针,勿在此追更)

| 编号 | 去向 |
|---|---|
| T05(停机半失败感知)/ T08(session 模板) | ➡ M17-d ✅ 已交付(2026-07-27,交人出口/md 模板) |
| T07(rounds 两层化) | ➡ M17-d ✅ 已交付(2026-07-27) |
| T09(watch 子命令) | ⏳ 维持等痛点(M17-d 裁决:不随行;ship 实测轮询确成痛点再做) |
| T34(provider/model/config-key 三概念正名) | ➡ M17-b ✅ 已交付(2026-07-27, route_key+derive_kind 强类型) |
| T44(增量轮 --spec 透传) | ➡ M17-a ✅ 已交付(2026-07-24, commit fdaaf42) |
| T46(安全组终审遗留①warning 入账 ②DRY ③增量注入面=D2 ④denylist 合一) | ✅ M18 批1 |
| T47(并发组测试债:反序列化分支/reap 降级分支/panic 全链路 e2e) | ✅ M18(③真 panic e2e=批3 REDTEAM_TEST_PANIC 破局;①②随批前置) |
| T11(GitHub Release 分发) | ➡ **M19** 分发面定稿届时裁 |
| T10(prompt 重排 A/B)/ T12(子命令语义深过)/ T13(md i18n) | ➡ **M20** backlog |

## 作废(2026-07-22 重规划裁决)

| 编号 | 理由 |
|---|---|
| T01 | dossier follow-ups(walk 性能/开关生命周期/查全洞)——reference_points/build_dossier 引擎已随 SCIP/1 壳拆除,对象消失 |
| T02 | 分段读 mini-patch——支柱 3 作废(无工具默认后 tool-loop 二次方尾巴病灶不存在;tools=on 仅诊断档不值得优化) |
| T38 | B 廉价过滤多语言化——1 壳退场后 strip 消费场景消失;strip_rust_noncode 若成死码归 M17 顺手删 |
| T39/T40/T41 | (原已作废:B+ 剥访问/契约分类器多行签名/SCIP 抛光批——随 SCIP 安乐死) |

## 已交付(明细在 done.md)

| 编号 | 一句话描述 | 状态 |
|---|---|---|
| T04 | git 全量路径 quotePath 缺口——已修(git.rs:74 quotePath=false 统一配置,2026-07-22 走读核实) | ✅ |
| T06 | dogfood 自反馈仪器移除(feedback.md+ship.md 段)——随 M16 清理批拆除,2026-07-22 核实零残留 | ✅ |
| T29 | 0 壳小文件全文——被 S2-b' 超集承接交付 | ✅ |
| T36 | streaming provider(SSE 流式,2d9945d) | ✅ |
| T42 | pack 空产出锚定 bug——被 S2-b' 0 壳整文件吞,关账(根因留档) | ✅ |
| T43 | refute parse 失败假 reject——三层韧性落地(M15 批 0+fix wave) | ✅ |
| T24/T03/T01⑥/T01③/T14-T23 | (历史批次,见 done.md) | ✅ |
| T54 | resume 差集补跑串行→并行(join_all 保序;append_partial Mutex 单连接无写冲突)——✅ 2026-09-03 同日交付,449 测试绿 | ✅ |
| T55 | 大 bundle 单 cell 慢——实证修正(2026-09-03 cells 表):GLM 单轮 19min/30.9K tok(27tok/s 含思考)属吞吐下限非恶性循环(iterations=1 无 repair);Kimi 3×600s 全超时系单轮思考>10min。cell 级披露已交付(result md Cells 表);余:per-model per-turn 旋钮/输出精炼约束 | 🧊 余 |
| T56 | openai 流式 usage 计量异常:PR181 作业 input 报 7088 vs 真实 prompt 580K chars(≈145K+ tokens)差 20 倍+,cached 0%——疑 AGC 网关 include_usage 末块只报增量/未命中部分,parse_openai_stream 三字段 fallback 均未覆盖;影响 $ 成本估算(T03-余)与缓存观测。**B roll 线索(09-04)**:SDK 走 anthropic 路径同一网关 usage 全对(166.5K in/99% cache)→网关数据是全的,自建 openai 路径读法漏字段,对照 aisdk anthropic conversions 修 | ⏳ |
