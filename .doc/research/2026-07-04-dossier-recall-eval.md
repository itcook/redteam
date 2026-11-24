# M14 卷宗穷人版查全率评测(2-pre,零 LLM 成本)

2026-07-04。回答 five-pillars §6 遗留问题 2:**穷人版(词边界 grep + 缩进/括号 enclosing
block)在 Rust trait/impl/宏场景的查全率够不够用,还是直接付 tree-sitter 的成本**
(二进制 5M→15-25M + C 工具链 + grammar ABI 维护)。方法:本仓 git 历史 17 个真实
fix commits(跨 M1-M3 / M7-M11 / M13 三期)+ 1 个宏合成样本,穷人版提取对照
rust-analyzer LSIF 全量索引做 ground truth,119 个 changed symbols、792 个 grep 命中、
431 个 GT 引用点逐点比对。harness 在 worktree `dossier-recall-eval` 的 `eval/`
(README 有复现说明,零第三方依赖,LSIF 索引 ~40s/commit)。

**结论先行:穷人版够用,tree-sitter 不立项。** 三句话:

1. **查全率不是问题**:名字桶口径下全场景 recall = **1.000**(431/431,零漏检,
   含 trait 分发调用点、泛型约束位、`crate::` 前缀宏调用)。Rust 引用必然文本含标识符,
   词边界 grep 天然健全;实测例外面(`use as` 重命名)全仓仅 3 处(隐式分发类
   双盲例外与语言外推见 §5——数字仅对 Rust 成立,档位结论对语言稳健)。
2. **查准率/预算才是问题,且是同一个问题**:同名方法(`new`×68、`manifest`×33、`run`×26)
   既是查准率洼地(trait_impl 桶查准 0.284)也是超预算元凶(3/17 bundle 超 8K)。
   一个旋钮同治:**per-name 截断帽 10 + 截断标注 + 歧义标注**,全样本落到 ≤7K tokens,
   中位数 ~2.8K。
3. **tree-sitter 两头不沾**:它能改善的(签名/块边界精度)实测不是痛点(缩进/括号启发式
   在 rustfmt 仓上 119/119 符号全部锚定成功);它不能改善的(同名歧义要类型推断、
   宏生成标识符要宏展开)恰是仅有的两个盲区——那是 rust-analyzer 级语义的活,
   在 five-pillars「不做清单」里(不追求精确调用图,卷宗=定位提示,判断归模型)。

---

## 1. 方法与口径

### 1.1 样本

`git log --grep 'fix('` 49 个候选中取 17 个(全部含 .rs 改动),分期覆盖:

- 早期 M1-M3(06-26~30):ff83803 / 5c24dd1 / 8099fc6 / ac4cf2d / 758afea / b452bdf / 8200541
- 中期 M7-M11(07-01~02):5d2b871 / 0791793 / c970ef8 / fd327ba / 1dafb79
- 近期 M13(07-03~04):6f95c25 / d60625e / 202d2b2 / 76c91c6 / 5132b10

### 1.2 穷人版实现(被评对象,产品可照抄的零依赖启发式)

1. `git show -U0 <c> -- '*.rs'` → def 行正则(fn/struct/enum/trait/type/macro_rules/
   const/static,含 pub/async/unsafe/const 前缀)提 hunk 内 def + 向上扫缩进/括号配平
   找 hunk 的 enclosing def → changed symbols;
2. 每个 symbol 名 `rg -wF` 词边界全仓搜(排除同名 def 行本身)→ 引用点;
3. 引用点 ±3 行 + 头行 = 卷宗条目,bytes/3.5 估 tokens。

### 1.3 Ground truth 与口径选择(重要)

oracle = `rust-analyzer lsif`(1.96.0)全量索引,按 (file, line) 与 grep 命中比对。
**per-symbol 口径会系统性冤枉穷人版**:rust-analyzer 把经 trait 分发的调用点挂在
**trait 声明**名下,不挂 impl——8200541 的 `impl CellSink for JobCellSink::manifest`
per-symbol GT=0,但 trait decl `CellSink::manifest` 名下有 6 个分发点,grep 名字桶
全部覆盖。卷宗语义下模型拿到的就是名字桶(「此名字的全部出现点」),故主口径取
**名字桶**:grep 命中 vs「同名全部符号 GT 引用之并集」。两口径都列(§2)。

## 2. 数据

### 2.1 分场景查全/查准(名字桶口径,主口径)

| 场景 | symbols | GT∪引用 | TP | grep 命中 | 查全 | 查准 |
|---|---|---|---|---|---|---|
| free_fn | 90 | 256 | 256 | 423 | **1.000** | 0.605 |
| inherent_method | 11 | 43 | 43 | 94 | **1.000** | 0.457 |
| trait_impl_method | 8 | 54 | 54 | 190 | **1.000** | 0.284 |
| trait_decl_method | 1 | 2 | 2 | 2 | **1.000** | 1.000 |
| trait_def | 1 | 11 | 11 | 11 | **1.000** | 1.000 |
| type_def | 7 | 64 | 64 | 71 | **1.000** | 0.901 |
| const_static | 1 | 1 | 1 | 1 | **1.000** | 1.000 |
| **合计** | **119** | **431** | **431** | **792** | **1.000** | **0.544** |

per-symbol 口径仅 trait_impl_method 分化(查准 0.005,GT 归属 artifact,见 §1.3),
其余场景两口径一致。119 符号全部在 LSIF 找到锚(no_anchor=0)——**穷人版 def 提取
本身零假阳**;缩进/括号 enclosing 在 rustfmt 化的仓上未见误判。

### 2.2 噪声形态(361 个桶口径假阳的分布)

| 形态 | 数量 | 定性 |
|---|---|---|
| doc 注释提及 | 188 (52%) | **良性偏有用**(«incremental_preflight 产物»类,本就是卷宗想给的上下文) |
| 同名错位代码 | 111 (31%) | 真噪声:同名局部变量/字段(`let turn = …`)、他类型同名方法(`new`) |
| 字符串字面量 | 47 (13%) | 日志/错误消息提及,模型一眼可辨 |
| use 声明碰撞 | 11 | `use std::fmt` vs `fmt` 方法一类的词面碰撞 |
| 属性行 | 4 | — |

危险噪声(可能误导判断的「事实错位」)只有第二类 31%,且形态高度可辨
(局部变量赋值/点号访问);其余对齐「只装事实」红线即可。

### 2.3 卷宗体积 vs ≤8K tokens 硬预算(±3 行档,bytes/3.5 粗估,含 CJK 注释)

17 bundle 无帽:中位 ~2.8K,3 个超 8K(ff83803 9.5K / 6f95c25 9.2K / 8200541 12.5K)。
超预算完全由高频名驱动:`new`×68、`manifest`×33、`on_cell`×27、`run`×26、`turn`×26。
**per-name cap=10 后全样本 ≤6.9K**(cap=20 仍有 2 个超)。cap 命中的名字与查准率
洼地完全重合——截断掉的恰是最不可信的部分,信息损失最小;按红线截断必标注
(「+N more,同名 K 处定义,heuristic」)。

## 3. 分场景解读 → 够用/不够用清单

**够用(实测背书):**

- 自由函数 / 独特命名方法(`resolve_upsert` 8/8、`do_read_file`、`call_anthropic`…):
  查全 1.0,查准 ~0.9-1.0(扣掉 doc 注释类良性命中);本仓 fix commits 的 changed
  symbols 绝大多数属此类(90/119 free_fn,且命名风格长而独特)。
- 类型 / trait 定义 / 常量:查全 1.0、查准 0.9-1.0。trait 名的引用点天然含
  **泛型约束位与 impl 头**(CellSink 11/11)——grep 对 `T: Trait`、`dyn Trait`、
  `impl Trait for` 无盲区(全是词边界文本)。
- trait 方法(桶语义):decl + 各 impl + 分发调用点一网打尽,查全 1.0——比
  per-symbol 语义工具更贴卷宗需求(评审 impl 改动本来就要看分发面)。
- 声明宏(合成样本):`macro_rules!` def 提取 ✓,`name!(…)`/`crate::name!(…)`
  调用点查全 2/2、查准 1.0(词边界对 `!`、`::` 均有效)。

**不够用(实测/机制确认的盲区):**

- **同名方法歧义**:`new`/`run`/`fmt` 类,查准 0.28-0.46,桶爆(68 命中)。
  不是漏而是淹——用 cap+歧义标注治理(§2.3),不值得为此上语义工具。
- **宏生成标识符**(合成样本证实):`gen_getter!(width, …)` 生成的 `width`,
  任何 diff 侧提取器都不会把它当 changed symbol——**tree-sitter 同盲**
  (宏体是 token tree,无 fn 节点),只有 rust-analyzer 级展开可见。本仓实测
  零 macro_rules,发生面=0;通用仓风险记录在案,属「不做清单」的精确性范畴。
- **`use … as` 重命名**:重命名后的引用词面断链。全仓实测 3 处(2 处实质,
  ~12K LOC),残余风险量化为「每仓个位数符号」,模板层「卷宗系 heuristic,
  不足再精读」的红线措辞已覆盖。
- 删除/改名符号(样本中 2 例 vanished)未评——旧调用点靠编译器/评审自身兜底,
  卷宗不背。

## 4. 档位建议(直接喂 grilling)

1. **Rust 走穷人版,tree-sitter 不立项**。查全 1.0 无缺口可补;两个真盲区
   (同名歧义、宏生成)tree-sitter 一个也解决不了,15-25M 二进制 + grammar ABI
   的成本没有对价。姊妹篇(tree-sitter 调研)的「体积/工具链/ABI 成本在列」
   自此有了否定性数据答案。
2. **scout 模型版也不需要为 Rust 立项**(为查全付 LLM 成本无标的;卷宗生产
   保持零 LLM,与「事实层只建一次」的确定性立场一致)。
3. **穷人版带三个红线补丁进设计**:
   - per-name 引用点 cap=10 + 截断标注(预算与噪声同治,§2.3);
   - `defs_same_name > 1` 时卷宗条目标注「同名 N 处定义(含 trait 分发),heuristic」
     ——把歧义交给模型判断,对齐「只装事实不装观点」;
   - doc 注释命中保留但标注形态(52% 的「噪声」实为有用上下文,丢了可惜)。
4. **±3 行档够用,enclosing block 档可省**:±3 行 + 签名已支撑判断且体积可控;
   enclosing 整块注入会把 8K 预算打爆(热点函数动辄 40+ 行),不建议默认档。
5. 无 grammar 语言 grep 兜底的降级路径维持原设计——本评测同时是该兜底档的
   置信度背书(词边界 grep 的健全性不依赖 Rust 特性,依赖「引用必然文本含名」,
   对多数语言成立;例外如 Ruby method_missing 类动态派发,属模型精读义务)。

## 5. 局限与语言外推(2026-07-04 用户质询补强)

- **数字仅对 Rust 成立,且 Rust 接近 grep 最优情形**(标识符纪律强、命名长而独特、
  rustfmt 化、零 macro_rules、无字符串反射文化)。查准率/体积/噪声分布对短名+动态
  文化的仓会系统性变差(cap 更频繁触发);查全率的「引用必然文本含名」前提各语言
  例外面梯度:**Go ≈ Rust(最友好)> TS/JS(`export default` 重命名是真查全洞——
  引用名由 import 方任取;`render`/`init` 类桶爆更凶)> Python(dunder 协议调用点
  不含方法名:改 `__init__` 的真引用是 `Foo(...)`、改 `__eq__` 的是 `==`;
  getattr 计算名)> Ruby/重元编程(computed method names 常态)**。
- **档位结论对语言稳健,数字不稳健**:上述每个洞(default export 重命名/dunder
  隐式调用/计算名/运算符分发)都需要名字解析或类型推断才能补——tree-sitter 一个
  也解决不了,与 Rust 的两个盲区同性质(语义级,属「不做清单」)。tree-sitter 在
  多语言下的真实增量是 **per-language def 提取的工程整洁度**(tags 查询统一维护 vs
  每语言 2-3 条正则)——支持语言 >3-4 种时可按维护性论据重开,不是查全率论据,
  且 five-pillars「三档按语言混用、无 grammar 语言 grep 兜底」已容纳该路径。
  若 dogfood 扩到 TS/Python 仓:同一 harness 换 oracle(pyright/tsserver 的
  SCIP/LSIF 导出)复跑即得该语言数字,半天量级,方法论不用重建。
- **隐式分发双盲(Rust 评测内也存在,自曝)**:运算符/协议 trait 的调用点不含
  方法名(`Add`→`a+b`、`Display::fmt`→`format!("{}")`、`Iterator::next`→for 循环、
  `Drop` 隐式),grep 与 LSIF oracle 同盲(oracle 把这类分发挂到仓外 std trait
  声明名下)、未计入分母——**查全 1.000 不覆盖此类**。属模型语义常识兜底面
  (「改了 Display impl 就去看格式化点」),同样非 tree-sitter 可解。
- oracle 局限:rust-analyzer 对 derive 展开引用同样不可见,双方同盲的部分
  (如 serde 字段改动的波及)未计入分母。
- 体积为 bytes/3.5 粗估;正式实现应换真 tokenizer 口径再校一次帽值。

## 6. 大仓/多语言实测补充(2026-07-04 追加:hiapi/backend 现场扫描)

用户以真实公司仓(new-api 二开,Go 206K LOC + web JS/TS 115K LOC,~27× 于本仓;
90 天 507 fix commits)验证语言外推,纯 grep 扫描(无 oracle),脚本
`eval/crosslang-scan.py`。**结论:两个新语言的数据点都落在 §5 机制预测区间内,
档位决策不需要重评;数字如下**:

- **规模不转化为桶爆炸(反直觉)**:Go 侧 24 个近期 fix bundle **无帽全部 ≤4K tokens**
  (vs 本仓 3/17 超 8K)——new-api 命名长且包作用域清晰;web 侧 10 bundle 无帽 ≤5K、
  cap10 后 ≤2.2K。**8K 预算在 320K LOC 仓比在本仓还宽裕**,体积风险与仓规模无关、
  只与命名文化相关(§5 判断证实)。
- **TS default export 改名率 1.0%**(3/288 具名 default import,其中 2 例系 HOC 包装
  的检测伪影)——「import 名=组件名」约定几乎恒成立,查全洞发生率与本仓 `use as`
  (3 处)同量级;`export default` 具名率 324/326(匿名仅 2)。
- **Go 陷阱面比 Rust 还小**:886 处 import 别名**不伤符号级查全**(别名只改包限定符,
  `alias.Func()` 的 `Func` 词面在场);reflect 仅 9 文件;interface 仅 28 个。
  枢纽名 `init`(39 处 def)/`TableName`(22 处 def,GORM 反射调用、仓内零调用点)
  由歧义标注旋钮覆盖,无需新机制。Go 隐式分发面 = `String()`/`Error()`/
  `MarshalJSON()` 等 stdlib 接口协议,与 §5 Rust 运算符 trait 同性质同治法。
- **真缺口在提取器而非查全率**:Go/TS def 正则各 3-5 条即跑通本扫描,但 TS 类方法/
  对象字面量方法未覆盖——per-language 提取器是工程活;tree-sitter tags 的维护性论据
  在语言数增多时才有分量(§5 已述,两语言 <10 条正则远够不上对价)。
- 评测工程注记:rg 在 stdin 为管道时启发式**改搜 stdin 而静默全 0**(heredoc 调用
  场景踩坑)——凡 shell 出 rg 必须显式传搜索路径;harness 已加固。
- 建议:hiapi/backend 可作 M14 2-A 引擎的**验收样本仓**(Go+TS 双语言、量级真实);
  若届时要精确查全数字,scip-go / scip-typescript 换 oracle 复跑本 harness,半天量级。

## 7. 复现

worktree `dossier-recall-eval`:`eval/harness.py run <commit>` → `eval/harness.py report`
→ `eval/bucket.py`(名字桶口径 + 噪声分类);宏合成样本 `eval/macro-synth.sh`
(4 文件 micro-crate:声明宏 + 宏生成 fn + 跨文件调用,构好后跑同一 harness)。
