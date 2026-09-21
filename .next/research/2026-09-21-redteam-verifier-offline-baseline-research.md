> 状态: 已交付 · verifier 离线基线两臂跑完，按预登记阈值裁定 `review.verifiers.enabled` 缺省 **false**。

# verifier 离线基线（M21b 批 D-4）

对照：design §8.1「去留由数据决定」〔D17〕；实现见 `eval/verify_score.py` + `redteam-next verify-once`。
原始产物（逐条判决与理由、prompt 快照）在 `.next/private/m21b/verify/`（不入库）。

## 结论

| 臂 | 模型 | 驳回率（dismissed 被驳，n=16） | 误驳率（upheld 被驳，n=25） | 预登记裁定 |
|---|---|---|---|---|
| 异族 | `deepseek/deepseek-flash` | **37.5%**（6/16，95%CI 18.5–61.4%）✅ ≥30% | **12.0%**（3/25，95%CI 4.2–30.0%）❌ >5% | 不通过 |
| 同族 | `agc/glm-5.3` | 18.8%（3/16，95%CI 6.6–43.0%）❌ | 20.0%（5/25，95%CI 8.9–39.1%）❌ | 不通过 |

两臂都没过 → **`review.verifiers.enabled` 缺省 `false`**（`src/config/mod.rs`），要开显式写 `"enabled": true`。
阈值是跑之前定死的（误驳 ≤ 5% ∧ 驳回 ≥ 30%，D17 Q6）：误驳会**静默删掉一条已被人确认的真缺陷**，
比漏驳贵得多，所以门收得紧；驳回率不到三成则这一路只是在烧 token。

**附带结论：异族优于同族**，两根轴同向（驳回高一倍、误驳低八个点）——D17「异族选择」不是推测，本批有数。

## 方法

固定集 = 本仓自己的人裁台账（`eval/fixtures/ledger-{dismissed,upheld}.jsonl`，`export_ledger.py` 从 schema v4
重置前的旧 `proj.db` 导出）。`dismissed` = 人读后判定的误报 → 期望 REFUTED；`upheld` = 人确认的真缺陷 → 期望 UPHELD。

链路刻意**不在 Python 复刻任何产品逻辑**：prompt（`build_verify_prompt`）、provider（含 anthropic / openai 两种
请求形状与 thinkingLevel）、判决解析（`parse_verify_output` + 读不懂倒向 upheld）全走产品二进制的隐藏子命令
`verify-once`；Python 只管固定集锚定、物化、并发与评分。复刻一份，测的就不是产品了。

证据树按**锚点**取，不是按当前 HEAD：`upheld` 行带 `head_sha`；`dismissed` 行只有 `updated_at`，取该日在 `dev` 上的
末次提交。这一步不是细节——HEAD 树里 22 条带引用的 dismissed 只有 7 条还能定位，按日锚定后 21 条能定位。

## 样本：111 → 41（与计划的最大出入）

计划写的是「各 111 条」，实际可用 **41 条（dismissed 16 / upheld 25）**。排除是逐条可审计的（`cases.json`）：

| 排除原因 | 条数 | 为什么排 |
|---|---|---|
| `no_quote`（dismissed） | 41 | 产品的 parse 层直接丢掉无 quote 的 finding，这种形状线上根本不存在 |
| `id_in_both_ledgers` | 14 | 同一 id 既被人裁误报又被人确认 —— oracle 自相矛盾，两边都不要 |
| `quote_not_found_at_anchor` | 9 | 固定集自己的声明与它记的树对不上（导出截断 / 行号漂 / 合成引用如 `—  pub lenses:`）。这种条目上 REFUTED 其实是对的，算进误驳会冤枉模型 |
| `no_anchor`（upheld） | 6 | 行里 `head_sha` 为空，锚不到树 |

后果要说清：**误驳率 5% 的门槛在 n=25 上等价于「最多错 1 条」**，95% 置信区间宽到 4–30%。
本批的裁定是「按预登记规则执行」，不是「统计上证明了不行」。样本要扩得靠 M21c 台账回流攒新数据。

## 误驳归因：8 条全是「overstated」一类，没有一条是读错代码

两臂 8 条误驳逐条读完，形态高度一致——模型**没有**读错文件、没有编造证据，全部是在争**程度与意图**：

- 「这是文档化的有意行为」：引用同文件的测试名或代码里的历史裁决注释（`r1 20260703` 这类），证明现状是设计而非缺陷；
- 「被声称的危害被夸大了」：承认事实成立，但论证最坏情况只是 nit / 措辞问题，够不上声称的 severity；
- 「claim 自相矛盾」：finding 正文里自己写了「比之前好」，却报成 medium 回归。

这正是 verifier 角色尾明写授权的三条 REFUTED 理由之一（false positive / already guarded / **overstated**）。
成功驳回的 6 条（异族臂）则集中在另外两类——「这条路径不可达」「守卫已经在了」「没有危害链」——质量很高。

**假说（未验证）**：把 REFUTED 的门收到「事实不成立 / 已有守卫」两条，把「overstated」改成降级 severity 而不是驳回，
误驳可能掉到接近 0 而驳回率损失有限。**本批不动**：改完 prompt 再拿同一份固定集重测就是 p-hacking——
阈值是预登记的，改了口径要重新预登记再跑。记进 followup。

## 成本（41 条 / 臂）

| 臂 | 输入 tok（含缓存命中） | 输出 tok | 单条墙钟中位 / 最长 |
|---|---|---|---|
| deepseek-flash | 141k（缓存 36k） | 336k | 31s / 120s |
| glm-5.3 | 132k（缓存 32k） | 186k | 61s / 230s |

单条输入约 3.2–3.4k token（共享头 + claim + ±60 行快照），输出反而是输入的 1.4–2.4 倍（两个模型都 reasoning 开着）。
按线上一轮 10–20 条 blocking finding 估，复核这一路的输出 token 与深挖同一量级——这也是「默认关」的第二个理由。

## 复跑

```bash
python3 eval/verify_score.py build --out eval/out/verify-cases        # 零网络：锚定 + 物化 + 生成用例
python3 eval/verify_score.py run eval/out/verify-cases --verifier deepseek/deepseek-flash \
    --bin ./target/aarch64-apple-darwin/release/redteam-next --parallel 6 --out run-a.json
python3 eval/verify_score.py score run-a.json [--baseline run-b.json]
python3 eval/verify_score.py selftest                                  # 定位口径 / 计数 / 阈值边界
```

本批跑用的配置指纹（跑前跑后一致）：`~/.redteam/config.json` sha256 `06986246fb541b39933bc48ceaaf57bc0888609e6439d726cfef696cc1028d32`；
prompt 模板哈希 `f9d9cd80e5874f5c+539f1e2db271fe38`（两臂同一份）。
