# redteam Roadmap · 开放/进行中明细

> 重规划于 2026-07-22(橡皮鸭走读驱动,评审=`../reviews/2026-07-22-rubber-duck-walkthrough-review.md`)。
> 已交付里程碑明细在 `done.md`;本文件只放**活跃**里程碑的展开 + 少量存档指引。

## M17 · 结构重构组 ✅ 全组收官(2026-07-22 立项,2026-07-27 交付;a/b/c/d 各节保留作指针)

根因一句话:M15 把评审范式翻成无工具单发,代码骨架还是 tool-loop 时代的。四批对应走读四大结构问题,
每批建议 grill 定边界 → SDD 实施。**a/b/c 文件面不重叠可并行,d 后置**(依赖 a 稳定退出码/报告面)。

### M17-a 尾段统一 ✅ 已交付(2026-07-24 合 dev 8b9eef2)→ 明细 done.md
- 遗留记账:T1-M1(is_same_bug 同义非全等测试补例)→M18 e2e 扩充搭车;M-1 duration_ms 口径收敛已补记 design §7。

### M17-b provider 翻转 ✅ 已交付(2026-07-27 合 dev 16f118b)→ 明细 done.md
- 遗留:T35 复测待 Kimi 可达(D1 修后同源逻辑本地已覆盖);validate_setup 文案与 alias 双份手写→M18 搭车;
  首发 json_object 强制 A/B(被否项,若重开须独立成批)。T49 解锁(single_shot 路径已稳)。

### M17-c state 夹层合并 ✅ 已交付(2026-07-27 合 dev 06d589e)→ 明细 done.md
- 遗留记债(两侧 opus 终审台账,归 M18):resume 差集补跑 append_partial 披露盲区(design ②级开放问题)·
  连接计数仪器(防未来内部多开连接回归,现靠静态 grep+e2e 双证)· T47③ 真 panic hook 链路 e2e(需真 spawn)·
  validate_setup 错误文案与 serde alias 双份手写(b 侧,pre-existing)。

### M17-d ship 协议硬化 ✅ 已交付(2026-07-27 合 dev 175af20)→ 明细 done.md
- 遗留:live ship 循环验收(T05 出口/状态重建/两层目录全要素)挂 M19 发布前 dogfood;
  <ID> help 参数空描述——✅已随 help 批交付(2026-07-28 merge cd1c5a9,同批:主 help 快速上手+review 描述纠偏)。

## M19 · 发布 1.0 ⏳(现有三宿主,用户裁决 2026-07-22)

- 范围:Claude Code 插件 + Codex 镜像(make gen 现成)+ 裸终端二进制(macOS arm64 已有)。
  linux/win 交叉构建与 OpenCode **不在首发**(→M20)。
- 交付:README/安装/快速上手(以 --help 为纲,--help 与代码同权重维护)· 版本 tag · 分发面定稿
  (bin 入 git 现状 vs T11 GitHub Release,届时定)· `.doc` 私有性复核(外部克隆者以 --help 为准)。
- 发布前最后一轮全仓 dogfood(多模型)作为验收门。

## M20 · 发布后 backlog 🧊

- **M10 OpenCode**(原编号保留习称):三条集成面候选(Custom Commands/Agent Skills[原生读 .claude/skills]/
  npm Plugins),需 brainstorm;软前置=linux 交叉构建。调研存档见 git 历史(2026-07-01)。
- linux/win 交叉构建(cross/CI;M4 布局已就绪仅填 darwin/aarch64)+ T11 GitHub Release 分发。
- M6-b 提示词 harness(先出素材盘点 research;lens 保持 Rust const 的架构决议长期有效)。
- provider 拓展(Google 适配等,设计级=加 config 块,按需)。
- R6 执行级 verify(2026-07-01 用户驳回,远期)。
- **优化群(M18 grill 逐出,2026-07-27)**:pack diff 双注入裁决(+T32)· pack 利用率落账(A/B 前提,群内先做)·
  prompt 三重复段合一 · lens L0 段序——token 轮廓类,须独立 A/B;与 T10 重排/T49 缓存前缀/T50 needs
  主动注入/T27-余 同桶。连接计数仪器(M17-c 终审记债)亦归此。
  **群内已提前交付(2026-07-28 用户拉动,M18 后优化线)**:--context-tokens 档位 flag(merge e80b109)+
  四 provider 声明 1M + **pack 绝对帽 256K→768K**(merge 0ae7a8d,锚 gpt-5.6 272K 计费悬崖之下,
  128K 缺省档 312K 显式入账);后续(窗口最大化两步+LITM 验证门三设计)=**T53** 统一追踪(2026-07-28 挂账,验证门设计已定);
  T49 缓存前缀稳定性权重随帽提升(cache write 1.25x 官方条款,大 pack 不命中反而更贵)。
- T10 prompt 整体重排 A/B、T13 md 骨架 i18n、T12 子命令语义深过(等痛点/真用户)。

## 存档指引(已收口,勿在此追更)

- **M16 发布基线** ✅ 三批全清(2026-07-21)→ 明细 done.md;立项时问题清单见 git 历史。
- **M14 卷宗** ✅ 收口:D9 判定成本线闭+独立性红旗,重定位并入 M15;密度门 `DOSSIER_MIN_REFS=5` 现状保留;
  dossier 模块改名(名不副实)归 M17 顺手。→ done.md
- **M6 batch1** ✅ / **M9 宿主集成** 🚫(技术不可行+运动员裁判立场冲突,2026-07-02 用户决,维持)/
  **M8** 并入 M20。
- **五支柱账目**关账(2026-07-22 用户裁决):1✅ 2✅(重定位) 5 近全(仅 $ 估算→TODO T03-余);
  **3/4 作废**——病灶(tool-loop 二次方尾巴/多轮成本结构)已随无工具默认消失。原表见 git 历史。
