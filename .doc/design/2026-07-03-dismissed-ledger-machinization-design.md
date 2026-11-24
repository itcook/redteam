# dismissed 台账机器化 + state 层入库 — grill 定案纪要

2026-07-03,grill 于 ship dogfood 第二轮(20260703-004708,r5 approve)收工后。
**实施状态:✅ 批A(c4b0235/e823864/54b0997/ceb4dd5/f5f01c5)+批B(0a61cc7)当日全落**;
53 卡已迁,旧 md 归档 .redteam/dismissed-md-archived/;待下轮 ship dogfood 活体验证。

## 动机(全部实证,非猜测)

1. **裸终端建卡无路径**:写卡靠宿主模型手工 Write 16hex.md,违「内置命令优先」原则。
2. **状态机自由文本化**:dismissed→re-upheld→human-confirmed 靠 md 状态行,机器不可读。
3. **注入 1/4 事实**(grill 中核实):53 卡 15.7K chars vs 4000 帽——每轮仅按 16hex 字典序
   (伪随机)前 ~1/4 台账真进 prompt,后 3/4 判例对 skeptic 不可见。
4. **生成侧注入无效实证**:勿再报段全程在场,injection 主题两会话十连穿透——prompt 压不住
   生成侧;有效的是裁决侧(refute 判例击杀 r3-r5 三连 100%)。
5. 宿主手误面:短 id 文件名致豁免静默失效(实发)、行号入卡漂移误导回喂(用户点出)。
6. IO 论据不成立(微秒级),性能从来不是动机。

## 定案(12 项)

1. **存储 = sqlite 单库 `.redteam/proj.db`**,rusqlite(bundled,+~1MB 二进制)。
   turso embedded 对比过:纯 Rust/交叉构建优势真实,但 2026-07 仍 beta,差异化特性
   (MVCC/io_uring/向量)在本用例收益为零;**不留换驱动期权**(发布后换存储影响大,定死)。
   jsonl 方案(曾定案)废:末行胜+全量抄行=手写乞丐版数据库,查询/状态机/gc 全是 db 原生强项。
2. **注入架构重构**:review 主 prompt 勿再报段**删除**(doc/code 双模板 {{DISMISSED_NOTES}} 段),
   判定后置 consolidate。强异议通道自动保留于 refute 层(模型不知判例存在,自然报出;
   skeptic 按「新证据→uphold」义务裁)。
3. **机械预筛扩 refute targeting**:finding 对台账 file 相同/title_jaccard/quote_overlap
   粗匹配(全现成函数),命中「疑似判例变体」→ **无视 agreement/severity 强制进 refute**
   (带判例注入)。机械筛管召回,LLM refute 管精判(B3 的正确用法)。原孤证 blocking targeting
   保留。id 级豁免(mark_dismissed)不变。
4. **schema**:dismissed 主表(id PK/status/title/file/quote/severity/reason/round/updated_at)
   + history 表(append-only 全事件)。status 五枚举:dismissed|re-upheld|human-confirmed|
   withdrawn(改判属实,替代删卡,读取视同无卡)|stale(gc 产物)。**无行号**(行号漂移
   对判例回喂是噪声——用户定);quote 原样存,截断只在渲染层。
5. **命令族** `redteam dismiss` 下:add <id> --reason --from <result.json>(按 id 自动抄
   快照四件套;无 --from 走四件套 flag;都缺报错拒瘦卡)/list [--status][--format]/
   show <id>/set-status <id> <status> --round [--reason](不硬约束状态机,history 留痕,
   human-confirmed 被改仅 warning)/gc [--apply](默认 dry-run)。**无 import 子命令**
   (存量 53 卡=本 repo 私产,一次性脚本,不进产品面)。
6. **自动加载(zero-config)**:项目存在 proj.db 即自动启用豁免+预筛,run meta 强制披露
   (ledger loaded 数);--dismissed 残余职责=csv 临时豁免|覆盖 db 路径;新增 --no-dismissed。
   csv 保留,dir 形态废弃(零垫层,产品未发布)。
7. **增长治理三层**:①语义过期→`gc`:quote 在 repo grep 不到=判例对象消亡→标 stale
   (机械零 LLM,human-confirmed 豁免);②注入预算→**相关性选择替代伪随机**:按被审上下文
   (refute:同 file>title 相似>其余;review 场景已无注入)排序填 4000 帽;③物理体积→
   status 分层,stale/withdrawn 不加载(sqlite 下无需归档文件)。
   明确不做:LLM 语义清理。残余:「代码在但理由过时」由强异议通道自然淘汰。
8. **state 层整层退役**(sqlite 引力,用户点出):workspace bucket 分桶/TTL+prune/
   REDTEAM_STATE_DIR 全删;跨项目查状态 YAGNI。jobs 表(state/args/manifest)+cells 表
   (partial checkpoint)+results 表(**持久,评审史资产,永不过期**)入 proj.db。
   WAL 模式(单写多读)。review --background 库缺失时自动初始化(幂等)。
9. **单库**:dismissed 与 jobs/results 生命周期不同(资产 vs 运行时),但单文件单连接
   压倒分库;团队共享判例是 M10+ 议题(届时 export/import,不分库)。
10. **rounds 联动**:此前 TODO「results/ 子目录放 rN.json」被 db 取代——result 持久可查,
    rounds 目录只留人读 rN-summary.md(table+emoji 那条 TODO 保留其余部分)。
11. **迁移**:53 卡一次性脚本入库(dev 私事);ship.md 联动简化(起评审不拼 --dismissed、
    建卡改 dismiss add --from、刹车链读 status、「flag 顺序权威」段蒸发)。
12. **批次**:批A=引擎+CLI 一次做对(DAL/命令族/自动加载/删段/预筛/相关性渲染/测试,
    读侧与注入耦合不拆);批B=迁移+ship.md+make gen。验证=下次真实 ship 会话(53 判例种子)。

## 明确不做
LLM 语义清理/theme 字段(B1 判例注入已覆盖,无消费者)/import 子命令/turso(不留期权)/
状态机硬约束/跨项目状态查询/分库。

## 效度关联
记分卡观察(2026-07-03):precision proxy 不度量「裁决成本机器化」——机器处理占比
或可作台账机器化后的新指标(feedback.md dev-time 仪器,发布前拆,不深投)。
