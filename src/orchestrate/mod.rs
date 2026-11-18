pub mod blocks;
pub mod deep;
pub mod ledger;
pub mod precedent;
pub mod queue;
pub mod review_shot;

use crate::bundle::Bundle;
use crate::domain::{CellOutput, Finding, Origin, RawFinding, Role, ScanOutput};
use crate::hash::fnv1a64_hex;
use crate::positioning::anchor_suspicions_within;
use crate::prompt::{build_scan_prompt, ScanPrompt, ScanPromptInput};
use crate::provider::registry::ProviderRegistry;
use crate::state::{CellInput, CellRecord};

/// 扇出持久化钩子：扇出前写输入快照（`manifest`，全集一次性），每 cell 完成即写结果（`on_cell`）。
/// `sink=None` 时全跳过——`run_review`/`run_code_review` 无 job 上下文的直调路径行为不变。
pub trait CellSink: Send + Sync {
    fn manifest(&self, inputs: &[CellInput]);
    fn on_cell(&self, rec: CellRecord);
    /// 阶段转换钩子（后台 job 用；默认 no-op，前台 sink=None / 其他 sink 零影响）。
    /// `note`：阶段注记（如 refute 的 "N targets"），进 phase_history 时间线。
    fn on_phase(&self, _phase: crate::state::Phase, _note: Option<String>) {}
    /// resolve 完成即报 models（后台 job stamp 进 state，running 期 status 可见；
    /// 此前 done 时才从 report 回填——r1 20260703 实证运行全程空数组）。默认 no-op。
    fn on_meta(&self, _models: &[String]) {}
    /// 批 F-2：扇出前报本次作业的角色集（`JobState.roles`，status 可见）。默认 no-op。
    fn on_roles(&self, _roles: &[Role]) {}
    /// M21b 批 C：队列项状态转换写 `queue_items` 快照（后台 job resume 用）。默认 no-op；写失败由实现方 warning。
    fn on_queue_item(&self, _item: &crate::orchestrate::queue::QueueItem) {}
    /// M21b 批 C2：深挖开跑前落队列头（预算 K），resume 据此判「首跑是不是 scan-only」并沿用同一预算。默认 no-op。
    fn on_deep_start(&self, _header: crate::state::DeepHeader) {}
}

/// R8.1：quote 去除全部空白（保大小写）——token 序列即身份，免疫 reformat。批 F-3 `consolidate::scan` 复用。
pub(crate) fn normalize_quote(q: &str) -> String {
    q.chars().filter(|c| !c.is_whitespace()).collect()
}

/// R8.1：title lowercase + 空白折叠单空格。
fn normalize_title(t: &str) -> String {
    t.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// finding id 组成的单一真相源（R8.1 + M13 rename 迁移共用）：quote 锚定、行无关；
/// 空 quote 退化行号路径（不保证跨 fixup 稳定）。
pub fn finding_id(file: &str, quote: &str, title: &str, line_start: u32, line_end: u32) -> String {
    let nq = normalize_quote(quote);
    let nt = normalize_title(title);
    if nq.is_empty() {
        fnv1a64_hex(&[file, &line_start.to_string(), &line_end.to_string(), &nt])
    } else {
        fnv1a64_hex(&[file, &nq, &nt])
    }
}

pub fn enrich(raw: &RawFinding, model: &str) -> Finding {
    let id = finding_id(&raw.file, &raw.quote, &raw.title, raw.line_start, raw.line_end);
    Finding {
        id,
        severity: raw.severity,
        title: raw.title.clone(),
        body: raw.body.clone(),
        file: raw.file.clone(),
        line_start: raw.line_start,
        line_end: raw.line_end,
        confidence: raw.confidence,
        recommendation: raw.recommendation.clone(),
        models: vec![model.to_string()],
        agreement: 1,
        // 区域疑点共识由深挖侧回填（`EmitCtx`）；scanner / 非深挖路径留 0。
        suspicion_agreement: 0,
        hard_constraint: raw.hard_constraint,
        quote: raw.quote.clone(),
        previously_dismissed: false,
        cluster_id: None,
        verify: None,
        origin: Origin::New,
        outside_window: false,
    }
}

// ── scanner 扇出（M21a 批 F-2，design §5 / §12.3；prd K1 / K5 / K6 / K7）──────────────────────────────────────────────

/// scanner 条目（由 cli 从 `EffectiveModel` 转出）：route_key = `provider/id`（K1）；`reasoning_pinned` = 配置了
/// `reasoningEffort` 且该 (kind, family) 真有参数映射（零推理观测门，K5）。
#[derive(Clone, Debug)]
pub struct ScanEntrySpec {
    pub route_key: String,
    pub family: String,
    pub reasoning_pinned: bool,
}

impl From<&crate::config::EffectiveModel> for ScanEntrySpec {
    fn from(em: &crate::config::EffectiveModel) -> Self {
        // pinned = 配置了 reasoningEffort **且** 该 (kind, family, id) 真有参数映射并会发出（评审修 9：无映射家族不该触发零推理告警）。
        let pinned = em.reasoning_effort.is_some_and(|e| {
            crate::provider::backends::reasoning_effort_params(em.kind, &em.family, &em.id, e)
                .is_some_and(|v| !v.is_empty())
        });
        ScanEntrySpec { route_key: em.name.clone(), family: em.family.clone(), reasoning_pinned: pinned }
    }
}

/// 扇出输入：`bundles` / `dossiers` / `others` 等长等序（`cli::plan_cells` 产物）；`entries` = 活跃 scanner 列表（design §5：每条对每组各跑
/// 一次）；`samples` = 同模型多 lens 采样（`review.scanners.samples`）。并发无角色级旋钮：只受各 provider 在途门（唯一闸，D13）。
pub struct ScanFanoutInput<'a> {
    pub target_label: &'a str,
    pub bundles: &'a [Bundle],
    pub dossiers: &'a [String],
    pub others: &'a [String],
    pub user_focus: &'a str,
    pub spec_docs: &'a str,
    /// 仓库简介（M21d 批 B-1，design §10.5）：进**稳定前缀**，每格都带、按不可信证据注入。
    /// 空 = 没有 `.redteam/brief.md`，行为与今日逐字节一致。
    pub repo_brief: &'a str,
    pub root: &'a std::path::Path,
    pub entries: &'a [ScanEntrySpec],
    pub samples: u32,
    /// 活跃判例卡（M21c 批 B-1，design §4「判例前移」）：**逐组**按本组文件集筛后注入 scanner prompt。
    ///
    /// 来源必须是 `load_ledger` 产出的 `Ledger.cards`——它已经过 `--no-dismissed`（「本次评审完全
    /// 关闭台账」）那道旗。**不在这一层自己开库**：批 C 刚因为五条台账路径各自解析库路径、集体绕过
    /// 那面旗栽过第六次「同一件事不在两处各有一套」。空片 = 无判例，prompt 逐字节不变。
    pub precedents: &'a [crate::db::DismissCard],
}

/// 一格结果：`record` 是落库形态（成功 / 失败一体，`ok` 区分）；`output` = 锚定后的疑点输出（失败 None）。
#[derive(Clone, Debug)]
pub struct ScanCellResult {
    pub cell_id: String,
    /// 组索引（0-based，对应 `bundles[group]`）。
    pub group: usize,
    pub route_key: String,
    pub family: String,
    pub sample: u32,
    pub record: CellRecord,
    pub output: Option<ScanOutput>,
}

#[derive(Default, Debug)]
pub struct ScanResults {
    pub cells: Vec<ScanCellResult>,
    pub warnings: Vec<String>,
    /// (input, output, cached_input)——含失败格烧掉的 usage。
    pub usage: (u64, u64, u64),
    /// 失败格 cell_id（F-5 进 coverage `failed` → verdict 硬 Incomplete，design §8.4）。
    pub failed: Vec<String>,
    /// scanner 声明的 needs 总数（只记录不补跑，M21b 入队；K6）。
    pub needs_declared: u32,
    /// 带行锚的 `needs{kind:evidence}` 条数，与其中**指向被折叠区间**的条数（M21d 批 C-1）。
    ///
    /// design §10.2 的切片回退信号是「scanner **反复索要被折叠的函数体**」——
    /// 光看 needs 条数判不了它（本仓与 PR181 三轮都稳定在三成）。要的是这个比值。
    pub needs_evidence_anchored: u32,
    pub needs_evidence_in_folded: u32,
}

/// 零推理观测阈值：钉了档位、`reasoning_tokens == Some(0)` 且输出 token 低于此值 → cell warn（不重跑，K5）。
pub const ZERO_REASONING_SHORT_OUTPUT_TOKENS: u64 = 200;
/// 零疑点观测阈值：锚定后 0 疑点且输出 token 低于此值 → cell warn（M21a 出口验收：PR181 某格 8 s / 883 token / 0 疑点，
/// 其他跑同格 1-3 条，是 E3 §16.3「零推理短输出」双峰的产品侧再现；正常格输出 8-38K）。不重跑，只观测。
pub const ZERO_SUSPICION_SHORT_OUTPUT_TOKENS: u64 = 2_000;

/// cell 命名（K1）：`scan[g<n>]@<provider/id>#<k>`，n / k 均从 1 起。
pub fn scan_cell_id(group: usize, route_key: &str, sample: u32) -> String {
    format!("scan[g{}]@{}#{}", group + 1, route_key, sample)
}

/// 反解 cell id → (组索引 0-based, route_key, sample)；非 scan 形态（旧作业 `bundle[..]`）→ None。
pub fn parse_scan_cell_id(id: &str) -> Option<(usize, String, u32)> {
    let rest = id.strip_prefix("scan[g")?;
    let (n, rest) = rest.split_once("]@")?;
    let (route, k) = rest.rsplit_once('#')?;
    let n: usize = n.parse().ok()?;
    Some((n.checked_sub(1)?, route.to_string(), k.parse().ok()?))
}

/// 落库记录 → 扇出结果形态（resume / partial 报告复用 `merge_suspicions` 与 `finish_scan_report`）：组与 route_key 从 cell id
/// 反解（反解不了的按组 0、model 作 route_key）；`output` 只在 ok 且为 scanner 输出时在场。
pub fn scan_cells_from_records(records: Vec<CellRecord>) -> Vec<ScanCellResult> {
    records
        .into_iter()
        .map(|mut rec| {
            let (group, route_key, sample) = parse_scan_cell_id(&rec.cell).unwrap_or((0, rec.model.clone(), 1));
            // 旧格式记录（批 B-E 的 reviewer 形态）：不是 scanner 输出 → 按失败格计（评审修 1：不能既 ok 又无疑点地混进 scan-only 报告）。
            if rec.ok && !matches!(rec.output, CellOutput::Scan(_)) {
                rec.ok = false;
                rec.warn = Some(format!("legacy non-scanner record (role {}); re-run review", rec.role().as_str()));
            }
            let output = match (&rec.ok, &rec.output) {
                (true, CellOutput::Scan(o)) => Some(o.clone()),
                _ => None,
            };
            ScanCellResult { cell_id: rec.cell.clone(), group, route_key, family: rec.family.clone(), sample, record: rec, output }
        })
        .collect()
}

/// scanner 静态扇出（design §2：纯生产者、无候补）：cell = 组 × 条目 × samples；prompt 每组只构造一次（同组各条目 / samples 字节相同 →
/// 共享头 cache 命中）；并发 = provider 在途门（registry `lease`；唯一闸，无角色级 workers，D13）。单格 = [`scan_cell_record`]
/// （resume 同款）。needs 只随 cell 落库并计数。
/// `enqueue`（配了 reviewer 时）= 深挖入队通道的发送端：格落盘后即把该格锚定后的疑点逐条发出（C2 时间重叠〔D15〕）。
pub async fn run_scan_fanout(
    registry: &ProviderRegistry,
    input: &ScanFanoutInput<'_>,
    sink: Option<&dyn CellSink>,
    enqueue: Option<&tokio::sync::mpsc::Sender<queue::DeepMsg>>,
) -> ScanResults {
    // 跨文件重定位的搜索范围（B-3）：整轮的变更文件集算一次。
    let changed_files: Vec<String> = input
        .bundles
        .iter()
        .flat_map(|b| b.files.iter().map(|f| f.path.clone()))
        .collect();
    // 判例按组筛一次（design §4：同文件活跃判例摘要）。逐组算而不是整轮算一份：
    // 别组文件上的判例对这一组没有参考价值，还会白占 prompt 预算。
    let group_precedents: Vec<String> = input
        .bundles
        .iter()
        .map(|b| {
            let files: Vec<String> = b.files.iter().map(|f| f.path.clone()).collect();
            crate::prompt::render_precedents_for_files(&files, input.precedents)
        })
        .collect();
    let prompts: Vec<ScanPrompt> = input
        .bundles
        .iter()
        .enumerate()
        .map(|(g, b)| {
            build_scan_prompt(&ScanPromptInput {
                target_label: input.target_label,
                bundle: b,
                user_focus: input.user_focus,
                cross_index: input.others.get(g).map(String::as_str).unwrap_or(""),
                precedents: &group_precedents[g],
                dossier: input.dossiers.get(g).map(String::as_str).unwrap_or(""),
                spec_docs: input.spec_docs,
                repo_brief: input.repo_brief,
            })
        })
        .collect();
    let samples = input.samples.max(1);
    let plan: Vec<(usize, &ScanEntrySpec, u32, String)> = (0..input.bundles.len())
        .flat_map(|g| {
            input.entries.iter().flat_map(move |e| (1..=samples).map(move |k| (g, e, k, scan_cell_id(g, &e.route_key, k))))
        })
        .collect();
    let template_hash = crate::prompt::scan_template_hash();
    if let Some(s) = sink {
        s.on_roles(&[Role::Scanner]);
        // manifest：`scan` 存完整请求，`prompt` 留空（不双存）；模板哈希随请求落盘（resume 重放盖它，评审修 6）。
        let inputs: Vec<CellInput> = plan
            .iter()
            .map(|(g, e, _, id)| CellInput {
                cell: id.clone(),
                model: e.route_key.clone(),
                prompt: String::new(),
                scan: Some(prompts[*g].clone()),
                template_hash: Some(template_hash.clone()),
            })
            .collect();
        s.manifest(&inputs);
    }
    // 大格先发（M21a 出口验收）：provider 门 FIFO、join_all 按序首轮 poll → 派发序 = 门序。7 组 / 门宽 4 时最大组排第六
    // 让总墙钟 = 排队 + 最慢格；按组 diff 字节（输出量的主因）降序、次键 prompt 字节、再按 plan 序派发，结果仍按 plan 序回收
    // （cell_id / manifest / 报告顺序不变）。门宽 ≥ 路由到该 provider 的格数时无排队，此序无影响。
    let diff_bytes: Vec<usize> = input.bundles.iter().map(|b| b.files.iter().map(|f| f.diff.len()).sum()).collect();
    let prompt_bytes: Vec<usize> = prompts.iter().map(|p| p.system.len() + p.blocks.iter().map(|b| b.text.len()).sum::<usize>()).collect();
    let mut dispatch: Vec<usize> = (0..plan.len()).collect();
    dispatch.sort_by(|&a, &b| {
        let (ga, gb) = (plan[a].0, plan[b].0);
        diff_bytes[gb].cmp(&diff_bytes[ga]).then(prompt_bytes[gb].cmp(&prompt_bytes[ga])).then(a.cmp(&b))
    });
    let futs = dispatch.iter().map(|&pi| {
        let (g, e, k, id) = &plan[pi];
        let prompt = &prompts[*g];
        let template_hash = template_hash.clone();
        let changed_files: &[String] = &changed_files;
        async move {
            let t0 = std::time::Instant::now();
            let mut queue_wait_ms = 0u64;
            // 过 provider 在途门即发出（唯一闸，D13）；未注册条目在此即失败。
            let res = match registry.lease_for(Role::Scanner, &e.route_key).await {
                Ok(lease) => {
                    queue_wait_ms = t0.elapsed().as_millis() as u64; // 门等待 = 排队
                    lease.run(prompt).await
                }
                Err(err) => Err(err),
            };
            let duration_ms = t0.elapsed().as_millis() as u64;
            let (mut record, output) = scan_cell_record(id, &e.route_key, &e.family, e.reasoning_pinned, input.root, changed_files, res, duration_ms, template_hash);
            record.queue_wait_ms += queue_wait_ms; // 门等待 + 429 排队等待（scan_cell_record 已填后者）
            if let Some(s) = sink {
                s.on_cell(record.clone());
            }
            // 格完成即入深挖队列（design C2 §2〔D15〕）：落盘之后发，到达键 `{cell_id}#{i}` 与 resume 回放同形（交付粒度仍是逐格）；
            // 门位此时已归还。发送失败 = 深挖侧已不收（如变更集为空早退）——忽略，疑点照进 scan 报告。
            if let (Some(tx), Some(out)) = (enqueue, &output) {
                for (i, s) in out.suspicions.iter().enumerate() {
                    if let Some(req) = queue::EnqueueReq::scan(s, &e.family, crate::consolidate::scan::arrival_key(id, i, s)) {
                        let _ = tx.send(req.into()).await;
                    }
                }
                // needs 是 design §6 的第三种生产者（C3〔D16〕Q8）：疑点发完再发，层级 `ItemKind::Needs` 天然排在疑点之后
                // ——预算有余才吃格。变更集过滤在深挖侧（那里才有 bundles），这里照发。
                for n in &out.needs {
                    if let Some(req) = queue::EnqueueReq::need(n) {
                        let _ = tx.send(req.into()).await;
                    }
                    // `needs{kind: api}` 不入队（没有自己的区域），但深挖装包要用——**首跑只有这条路**：
                    // 那时 `DeepInput::scan_cells` 恒空（scan 与 deep 并行），不发就等于 A-4 不生效。
                    if n.kind == crate::domain::NeedKind::Api && !n.path.is_empty() {
                        let _ = tx.send(queue::DeepMsg::ApiNeed(n.clone())).await;
                    }
                }
            }
            (pi, ScanCellResult {
                cell_id: id.clone(), group: *g, route_key: e.route_key.clone(), family: e.family.clone(), sample: *k, record, output,
            })
        }
    });
    let mut cells: Vec<(usize, ScanCellResult)> = futures::future::join_all(futs).await;
    cells.sort_by_key(|(pi, _)| *pi);
    let cells: Vec<ScanCellResult> = cells.into_iter().map(|(_, c)| c).collect();
    let mut out = ScanResults::default();
    // 排队告警（M21a 出口验收；D13 按 provider 计，扇出与 resume 共用 registry::queue_warnings，用收口时的有效门宽）
    // + 429 收敛告警（撞过 429 的 provider 各一条「429 × N，门宽 A → B」）。
    out.warnings.extend(registry.queue_warnings(plan.iter().map(|(_, e, _, _)| e.route_key.as_str())));
    out.warnings.extend(registry.gate_warnings());
    for c in &cells {
        out.usage.0 += c.record.input_tokens;
        out.usage.1 += c.record.output_tokens;
        out.usage.2 += c.record.cached_input_tokens;
        if c.record.ok {
            if let Some(o) = &c.output {
                out.needs_declared += o.needs.len() as u32;
            }
        } else {
            out.failed.push(c.cell_id.clone());
            out.warnings.push(format!("scanner cell {} failed: {}", c.cell_id, c.record.warn.clone().unwrap_or_default()));
        }
    }
    out.cells = cells;
    out
}

/// 单格收口（扇出与 resume 共用，评审修 5）：成功 → `anchor_suspicions`（quote 不在文件即丢，design §4）→ `CellRecord::ok`，parse /
/// 锚定丢弃计数与零推理观测拼进 `warn`（cell 仍 ok）；失败 → `CellRecord::failed` + 烧掉的 usage 入账。`attempts` = HTTP 调用数
/// （轮次 + 429 排队重试，D13；无候补）；`queue_wait_ms` 先填 429 排队等待，调用方再加门等待。
#[allow(clippy::too_many_arguments)]
pub fn scan_cell_record(
    cell_id: &str,
    route_key: &str,
    family: &str,
    reasoning_pinned: bool,
    root: &std::path::Path,
    // `changed` = 本轮变更文件集：跨文件重定位的搜索范围（M21c 批 B-3）。空 = 不做重定位。
    changed: &[String],
    res: Result<crate::provider::scan_shot::ScanCall, crate::provider::ProviderError>,
    duration_ms: u64,
    template_hash: String,
) -> (CellRecord, Option<ScanOutput>) {
    let (mut record, output) = match res {
        Ok(call) => {
            let anchored = anchor_suspicions_within(root, call.output, changed);
            let mut notes = scan_cell_notes(
                &call.stats, anchored.dropped, anchored.dropped_unreadable, anchored.relocated, reasoning_pinned, call.reasoning_tokens, call.output_tokens,
                anchored.output.suspicions.len(),
            );
            if let Some(n) = call.repair_note {
                notes.push(n); // 首发解析失败的诊断进 cell warn（cell 仍 ok；出口验收观察）
            }
            let mut rec = CellRecord::ok(cell_id, route_key, CellOutput::Scan(anchored.output.clone())).with_family(family);
            rec.input_tokens = call.input_tokens;
            rec.output_tokens = call.output_tokens;
            rec.cached_input_tokens = call.cached_input_tokens;
            rec.iterations = call.iterations;
            // `attempts` 是总数；分量单记（schema 批 B-4）。
            // scanner 的 `iterations` 恒为 1（首发即成）或 2（修复后成）——`scan_shot` 就这两个
            // 构造点，所以减 1 就是修复轮数；scanner 没有二阶段补查（那是 reviewer 的）。
            rec.attempts = call.iterations + call.rate_limit_retries;
            rec.repair_rounds = call.iterations.saturating_sub(1);
            rec.rl_retries = call.rate_limit_retries;
            rec.queue_wait_ms = call.rate_limit_wait_ms;
            rec.warn = if notes.is_empty() { None } else { Some(notes.join("; ")) };
            (rec, Some(anchored.output))
        }
        Err(err) => {
            let (i, o, c) = err.burned_usage();
            let mut rec = CellRecord::failed(cell_id, route_key, Role::Scanner, err.to_string()).with_family(family);
            rec.input_tokens = i;
            rec.output_tokens = o;
            rec.cached_input_tokens = c;
            rec.attempts = 1;
            (rec, None)
        }
    };
    record.duration_ms = duration_ms;
    record.template_hash = Some(template_hash);
    (record, output)
}
/// cell warn 片段：parse 丢弃 / 截断计数、锚定丢弃、零推理观测——不静默（批 D-1 注释「丢弃 / 截断计数由调用方写进 cell warn」）。
#[allow(clippy::too_many_arguments)] // 纯展平的观测入参：打包成结构体只会多一层间接、不增可读性
fn scan_cell_notes(
    st: &crate::provider::parse::ScanParseStats,
    dropped: u32,
    unreadable: u32,
    relocated: u32,
    pinned: bool,
    reasoning: Option<u64>,
    output_tokens: u64,
    suspicions: usize,
) -> Vec<String> {
    let mut notes = Vec::new();
    if st.dropped_no_quote + st.dropped_bad_path + st.dropped_malformed > 0 {
        notes.push(format!(
            "parse dropped: no_quote={} bad_path={} malformed={}",
            st.dropped_no_quote, st.dropped_bad_path, st.dropped_malformed
        ));
    }
    if st.needs_dropped + st.needs_truncated > 0 {
        notes.push(format!("needs dropped={} truncated={}", st.needs_dropped, st.needs_truncated));
    }
    if st.ranges_dropped > 0 {
        notes.push(format!("scanned_areas ranges dropped={}", st.ranges_dropped));
    }
    if dropped > 0 {
        notes.push(format!("{dropped} suspicions dropped: quote not found in file"));
    }
    if unreadable > 0 {
        notes.push(format!("{unreadable} suspicions dropped: file unreadable"));
    }
    // 跨文件重定位救回的（B-3）：加了观测就得有出口，否则这一层在真实运行里不可见——
    // 本批自己踩过（字段加完没人消费，和 `Phase::Verify` 同一类形状）。
    if relocated > 0 {
        notes.push(format!("{relocated} suspicions relocated: quote found in another changed file"));
    }
    if pinned && reasoning == Some(0) && output_tokens < ZERO_REASONING_SHORT_OUTPUT_TOKENS {
        notes.push(format!("pinned thinking but zero reasoning tokens (output {output_tokens} tokens)"));
    }
    if suspicions == 0 && output_tokens < ZERO_SUSPICION_SHORT_OUTPUT_TOKENS {
        notes.push(format!("0 suspicions with short output ({output_tokens} tokens)"));
    }
    notes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;

    fn raw_full(file: &str, ls: u32, le: u32, title: &str, quote: &str) -> RawFinding {
        RawFinding {
            severity: Severity::High, title: title.into(), body: "b".into(),
            file: file.into(), line_start: ls, line_end: le, confidence: 0.9,
            recommendation: "r".into(), hard_constraint: false, quote: quote.into(),
        }
    }

    #[test]
    fn id_ignores_line_shift_when_quote_present() {
        let a = enrich(&raw_full("a.rs", 10, 12, "SQL injection", "let q = fmt!(sql);"), "m");
        let b = enrich(&raw_full("a.rs", 99, 101, "SQL injection", "let q = fmt!(sql);"), "m");
        assert_eq!(a.id, b.id, "行号位移不得改变 id");
    }

    #[test]
    fn id_ignores_whitespace_reformat() {
        let a = enrich(&raw_full("a.rs", 1, 2, "SQL injection", "let q =\n    fmt!( sql );"), "m");
        let b = enrich(&raw_full("a.rs", 1, 2, "SQL injection", "letq=fmt!(sql);"), "m");
        assert_eq!(a.id, b.id, "quote 去空白后等价 → 同 id");
    }

    #[test]
    fn id_title_normalization_case_and_ws() {
        let a = enrich(&raw_full("a.rs", 1, 1, "SQL  Injection", "x"), "m");
        let b = enrich(&raw_full("a.rs", 1, 1, "sql injection", "x"), "m");
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn id_composition_pinned() {
        // 组成 golden：钉死组成与归一（FNV 算法本身由 hash.rs golden 钉死）。
        let f = enrich(&raw_full("src/x.rs", 5, 7, "Weak  Hash", "md5( pw )"), "m");
        assert_eq!(f.id, crate::hash::fnv1a64_hex(&["src/x.rs", "md5(pw)", "weak hash"]));
    }

    #[test]
    fn id_falls_back_to_lines_without_quote() {
        let a = enrich(&raw_full("a.rs", 1, 2, "t", ""), "m");
        let b = enrich(&raw_full("a.rs", 3, 4, "t", ""), "m");
        assert_ne!(a.id, b.id, "无 quote 退化回行号路径");
        assert_eq!(a.id, crate::hash::fnv1a64_hex(&["a.rs", "1", "2", "t"]));
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use crate::evidence::git::FileChange;
    use crate::provider::tool_loop::{AssistantTurn, ChatMessage, FakeChatBackend};
    use crate::provider::ProviderError;
    use std::sync::Mutex;
    use std::time::Duration;

    const REAL: &str = "let t = secret();";

    fn scan_json(extra: &str) -> String {
        format!(r#"{{"suspicions":[{{"file":"src/a.rs","line_start":9,"line_end":9,"hint":"real","confidence":0.7,"quote":"{REAL}"}}{extra}],"needs":[{{"path":"src/b.rs","lines":"3-9","reason":"r"}},{{"path":"src/c.rs","reason":"没有行锚"}},{{"path":"src/a.rs","lines":"1-2","reason":"契约要核","kind":"api"}}],"scanned_areas":[{{"file":"src/a.rs","ranges":["1-2"]}}]}}"#)
    }
    fn turn(text: &str, reasoning: Option<u64>, out_tok: u64) -> AssistantTurn {
        AssistantTurn { text: text.into(), tool_calls: vec![], input_tokens: 10, output_tokens: out_tok, cached_input_tokens: 4, finish_reason: None, reasoning_tokens: reasoning }
    }
    fn repo() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/a.rs"), format!("fn a() {{}}\n{REAL}\n")).unwrap();
        d
    }
    fn bundles(n: usize) -> Vec<Bundle> {
        (0..n).map(|i| Bundle::from_files(vec![FileChange { path: format!("src/f{i}.rs"), diff: format!("+line {i}") }])).collect()
    }
    fn entry(key: &str, fam: &str, pinned: bool) -> ScanEntrySpec {
        ScanEntrySpec { route_key: key.into(), family: fam.into(), reasoning_pinned: pinned }
    }
    struct RecSink { manifest: Mutex<Vec<CellInput>>, cells: Mutex<Vec<CellRecord>>, roles: Mutex<Vec<Role>> }
    impl RecSink { fn new() -> Self { RecSink { manifest: Mutex::new(vec![]), cells: Mutex::new(vec![]), roles: Mutex::new(vec![]) } } }
    impl CellSink for RecSink {
        fn manifest(&self, inputs: &[CellInput]) { self.manifest.lock().unwrap().extend_from_slice(inputs); }
        fn on_cell(&self, rec: CellRecord) { self.cells.lock().unwrap().push(rec); }
        fn on_roles(&self, roles: &[Role]) { self.roles.lock().unwrap().extend_from_slice(roles); }
    }
    fn input<'a>(bundles: &'a [Bundle], dossiers: &'a [String], others: &'a [String], root: &'a std::path::Path, entries: &'a [ScanEntrySpec], samples: u32) -> ScanFanoutInput<'a> {
        ScanFanoutInput { precedents: &[], target_label: "wt", bundles, dossiers, others, user_focus: "", spec_docs: "", repo_brief: "", root, entries, samples }
    }

    /// B-1 接线（M21d）：仓库简介要进**真实扇出**的 scanner 前缀。
    /// `--pack-only` 那条路是另一套 prompt 构造——只测它等于没测这条（变异实测：改这里，pack-only 的
    /// e2e 照绿）。缺失时是空槽占位，与今日逐字节一致。
    #[tokio::test]
    async fn repo_brief_reaches_the_scanner_prompt_on_the_real_fanout() {
        let repo = repo();
        let b = bundles(1);
        let entries = vec![entry("p/a", "alpha", false)];
        let backend = std::sync::Arc::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)));
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(std::sync::Arc::clone(&backend)), Duration::from_secs(5), 0);
        let flat = |b: &FakeChatBackend| -> String {
            use crate::provider::tool_loop::ChatMessage as M;
            b.messages_at(0)
                .iter()
                .map(|m| match m {
                    M::System(t) | M::User(t) => t.clone(),
                    M::UserBlocks(bs) => bs.iter().map(|x| x.text.clone()).collect::<Vec<_>>().join("\n"),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut with = input(&b, &[], &[], repo.path(), &entries, 1);
        with.repo_brief = "本仓简介 BRIEF_MARKER_3A";
        let _ = run_scan_fanout(&reg, &with, None, None).await;
        assert!(flat(&backend).contains("BRIEF_MARKER_3A"), "brief 没进真实扇出的 scanner prompt");

        let backend2 = std::sync::Arc::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)));
        let mut reg2 = ProviderRegistry::new();
        reg2.insert_scan("p/a", "p", 8, Box::new(std::sync::Arc::clone(&backend2)), Duration::from_secs(5), 0);
        let _ = run_scan_fanout(&reg2, &input(&b, &[], &[], repo.path(), &entries, 1), None, None).await;
        let w2 = flat(&backend2);
        assert!(!w2.contains("BRIEF_MARKER_3A") && w2.contains("(none)"), "缺失时是空槽占位");
    }

    /// B-3 接线（M21c 批 B）：跨文件重定位要在**真实扇出**里生效。
    ///
    /// `positioning` 那层有单测，但 `scan_cell_record` 的 `changed` 参数传成 `&[]` 它照绿——
    /// 这是本批第三次撞上「某条接线没人盯」。搜索范围 = 本轮全部 bundle 的文件集。
    #[tokio::test]
    async fn cross_file_relocation_applies_on_the_real_fanout() {
        let repo = repo();
        // 两个文件各一组：疑点自称 f1.rs，而 quote 只在 f0.rs 里
        let b = bundles(2);
        std::fs::write(repo.path().join("src/f0.rs"), "fn zero() {\n    let token = secret();\n}\n").unwrap();
        std::fs::write(repo.path().join("src/f1.rs"), "fn one() {\n    nothing();\n}\n").unwrap();
        let entries = vec![entry("p/a", "alpha", false)];
        let scan = r#"{"suspicions":[{"file":"src/f1.rs","line_start":2,"line_end":2,"hint":"串了文件名","confidence":0.8,"quote":"let token = secret();"}],"scanned_areas":[]}"#;
        let backend = std::sync::Arc::new(FakeChatBackend::repeating(turn(scan, None, 300)));
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(std::sync::Arc::clone(&backend)), Duration::from_secs(5), 0);

        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 1), None, None).await;
        let anchored: Vec<_> = res
            .cells
            .iter()
            .filter_map(|c| c.output.as_ref())
            .flat_map(|o| o.suspicions.iter())
            .collect();
        assert!(
            anchored.iter().any(|s| s.file == "src/f0.rs" && s.line_start == 2),
            "quote 真实、file 写错 → 该被重定位救回并改写归属: {anchored:?}"
        );
        // 救回来这件事要在 cell 记录里看得见——加了观测没出口等于这一层在真实运行里不可见
        // （本批自己踩过：`relocated` 字段加完没人消费，和 `Phase::Verify` 同一类形状）。
        assert!(
            res.cells.iter().any(|c| c.record.warn.as_deref().unwrap_or("").contains("relocated")),
            "重定位要进 cell notes: {:?}",
            res.cells.iter().map(|c| &c.record.warn).collect::<Vec<_>>()
        );
    }

    /// B-1 接线（M21c 批 B）：判例摘要要进**真实扇出**的 scanner prompt，而且**按组筛**。
    ///
    /// 照 `repo_brief_reaches_the_scanner_prompt_on_the_real_fanout` 的形状写——同一个理由：
    /// `--pack-only` 是另一套 prompt 构造，只测它等于没测这条。这里连「按组筛」一起钉住：
    /// 两组各自只该看到自己文件上的判例。
    #[tokio::test]
    async fn precedents_reach_the_scanner_prompt_per_group_on_the_real_fanout() {
        let repo = repo();
        let b = bundles(2); // src/f0.rs、src/f1.rs 各一组
        let entries = vec![entry("p/a", "alpha", false)];
        let card = |file: &str, reason: &str| crate::db::DismissCard {
            id: "1111111111111111".into(), status: "dismissed".into(), title: "t".into(),
            file: file.into(), quote: "qq".into(), severity: "high".into(),
            reason: reason.into(), round: "r1".into(), updated_at: String::new(),
        };
        let cards = vec![card("src/f0.rs", "G0_PRECEDENT_MARK"), card("src/f1.rs", "G1_PRECEDENT_MARK")];

        let backend = std::sync::Arc::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)));
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(std::sync::Arc::clone(&backend)), Duration::from_secs(5), 0);
        let flat = |b: &FakeChatBackend, idx: usize| -> String {
            use crate::provider::tool_loop::ChatMessage as M;
            b.messages_at(idx)
                .iter()
                .map(|m| match m {
                    M::System(t) | M::User(t) => t.clone(),
                    M::UserBlocks(bs) => bs.iter().map(|x| x.text.clone()).collect::<Vec<_>>().join("\n"),
                    _ => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        };

        let mut with = input(&b, &[], &[], repo.path(), &entries, 1);
        with.precedents = &cards;
        let _ = run_scan_fanout(&reg, &with, None, None).await;
        assert_eq!(backend.turn_count(), 2, "两组各一格");
        // 两格分别看到自己那条，且**看不到另一组的**
        let seen: Vec<String> = (0..2).map(|i| flat(&backend, i)).collect();
        let g0 = seen.iter().find(|t| t.contains("G0_PRECEDENT_MARK")).expect("G0 的判例没进 prompt");
        let g1 = seen.iter().find(|t| t.contains("G1_PRECEDENT_MARK")).expect("G1 的判例没进 prompt");
        assert!(!g0.contains("G1_PRECEDENT_MARK"), "别组判例串进来了: {g0}");
        assert!(!g1.contains("G0_PRECEDENT_MARK"), "别组判例串进来了: {g1}");

        // 反面：无判例时是空槽占位，prompt 不含任何判例痕迹
        let backend2 = std::sync::Arc::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)));
        let mut reg2 = ProviderRegistry::new();
        reg2.insert_scan("p/a", "p", 8, Box::new(std::sync::Arc::clone(&backend2)), Duration::from_secs(5), 0);
        let _ = run_scan_fanout(&reg2, &input(&b, &[], &[], repo.path(), &entries, 1), None, None).await;
        let w2 = flat(&backend2, 0);
        assert!(!w2.contains("PRECEDENT_MARK") && w2.contains("(none)"), "无判例时空槽占位");
    }

    /// 格完成即入队（design C2 §2〔D15〕）：落盘之后逐条发**锚定后**的疑点——到达键 `{cell_id}#{i}`、家族 = 条目家族、行号取锚定值；
    /// 锚定丢掉的编造疑点不发、失败格不发。接收端已 drop 时发送失败只是忽略，扇出结果不受影响。
    #[tokio::test]
    async fn scan_fanout_streams_anchored_suspicions_after_cell_lands() {
        let repo = repo();
        let fab = r#",{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"fab","confidence":0.9,"quote":"not in the file"}"#;
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(FakeChatBackend::repeating(turn(&scan_json(fab), None, 300))), Duration::from_secs(5), 0);
        let b = bundles(1);
        let entries = vec![entry("p/a", "alpha", false), entry("p/ghost", "ghost", false)];
        let sink = RecSink::new();
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let fanout = async move {
            let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 1), Some(&sink), Some(&tx)).await;
            drop(tx);
            (res, sink, reg, b, entries, repo)
        };
        let recv = async {
            let mut got = Vec::new();
            while let Some(msg) = rx.recv().await {
                match msg {
                    queue::DeepMsg::Item(req) => got.push(*req),
                    // api 声明不入队（A-4）：这条用例只看入队面，另有专测
                    queue::DeepMsg::ApiNeed(_) => {}
                }
            }
            got
        };
        let ((res, sink, reg, b, entries, repo), got) = tokio::join!(fanout, recv);
        let keys: Vec<Option<String>> = got.iter().map(|r| r.arrival.clone()).collect();
        let (susp, needs): (Vec<_>, Vec<_>) = got.iter().partition(|r| r.spec.origin == crate::pack::region::Origin::Scan);
        assert_eq!(susp.len(), 1, "只发锚定后的真疑点，失败格不发: {keys:?}");
        let key = susp[0].arrival.clone().unwrap_or_default();
        assert!(key.starts_with("scan[g1]@p/a#1#0:") && key.len() > "scan[g1]@p/a#1#0:".len(), "到达键带内容指纹: {key}");
        assert_eq!((susp[0].families.clone(), susp[0].spec.file.clone(), susp[0].spec.ranges.clone()), (vec!["alpha".to_string()], "src/a.rs".to_string(), vec![(2, 2)]));
        // needs 也入队（C3〔D16〕Q8）：排在本格疑点之后、无笔记、无家族、层级 Needs；到达键 `needs:{path}:{lines}:{理由指纹}`。
        assert_eq!(needs.len(), 1, "{keys:?}");
        assert!(got.iter().position(|r| r.spec.origin == crate::pack::region::Origin::Needs).unwrap() > 0, "needs 排在疑点之后");
        assert_eq!(
            (needs[0].kind, needs[0].spec.notes.len(), needs[0].families.len(), needs[0].spec.file.clone()),
            (crate::orchestrate::queue::ItemKind::Needs, 0, 0, "src/b.rs".to_string())
        );
        assert!(needs[0].arrival.clone().unwrap_or_default().starts_with("needs:src/b.rs:3-9:"), "{keys:?}");
        // 解不出行锚的那条不入队（评审 #13）：入了也只是「无笔记地审文件开头」。
        assert!(!got.iter().any(|r| r.spec.file == "src/c.rs"), "{keys:?}");
        assert_eq!(res.failed, vec!["scan[g1]@p/ghost#1"]);
        assert_eq!(sink.cells.lock().unwrap().len(), 2);

        let (tx, rx) = tokio::sync::mpsc::channel(1);
        drop(rx);
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 1), None, Some(&tx)).await;
        assert_eq!(res.cells.iter().filter(|c| c.record.ok).count(), 1, "接收端已关：发送失败忽略");
        assert_eq!(res.cells[0].output.as_ref().map(|o| o.suspicions.len()), Some(1));
    }

    /// 落盘先于入队（resume 以落盘格为准复算到达键——先发后落盘时 kill 落在两者之间，深挖挖过的疑点在 resume 里找不到来源格）：
    /// 落盘钩子被调用时，通道里不得已有本格的疑点。同 task 协作调度下接收端总在本格落盘后才被 poll，看不出先后——在钩子里 `try_recv` 查。
    #[tokio::test]
    async fn scan_fanout_sends_only_after_the_cell_is_persisted() {
        struct Probe {
            rx: Mutex<tokio::sync::mpsc::Receiver<crate::orchestrate::queue::DeepMsg>>,
            seen: Mutex<Vec<String>>,
            early: Mutex<Vec<String>>,
            calls: Mutex<usize>,
        }
        impl CellSink for Probe {
            fn manifest(&self, _inputs: &[CellInput]) {}
            fn on_cell(&self, rec: CellRecord) {
                let nth = {
                    let mut c = self.calls.lock().unwrap();
                    *c += 1;
                    *c
                };
                let mut rx = self.rx.lock().unwrap();
                while let Ok(msg) = rx.try_recv() {
                    let key = match msg {
                        queue::DeepMsg::Item(req) => req.arrival.unwrap_or_default(),
                        queue::DeepMsg::ApiNeed(n) => format!("api:{}", n.path),
                    };
                    // 第一次落盘时通道必须是空的——那时只有本格跑完，它的疑点与 needs 都还没发。
                    // needs 的到达键不带 cell id（`needs:{path}:…`），只靠前缀比对漏得掉，故按「第几次落盘」兜住。
                    if nth == 1 || key.starts_with(&format!("{}#", rec.cell)) {
                        self.early.lock().unwrap().push(key.clone());
                    }
                    self.seen.lock().unwrap().push(key);
                }
            }
        }
        let repo = repo();
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300))), Duration::from_secs(5), 0);
        let b = bundles(2);
        let entries = vec![entry("p/a", "alpha", false)];
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let probe = Probe { rx: Mutex::new(rx), seen: Mutex::new(Vec::new()), early: Mutex::new(Vec::new()), calls: Mutex::new(0) };
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 1), Some(&probe), Some(&tx)).await;
        drop(tx);
        let mut rx = probe.rx.into_inner().unwrap();
        let mut seen = probe.seen.into_inner().unwrap();
        while let Ok(msg) = rx.try_recv() {
            match msg {
                queue::DeepMsg::Item(req) => seen.push(req.arrival.unwrap_or_default()),
                // api 声明与入队项同走一条通道但不是队列项（A-4）——这条用例只看入队面
                queue::DeepMsg::ApiNeed(n) => seen.push(format!("api:{}", n.path)),
            }
        }
        assert_eq!(res.cells.len(), 2);
        assert_eq!(probe.early.into_inner().unwrap(), Vec::<String>::new(), "未落盘就入队");
        seen.sort();
        let prefixes: Vec<&str> = seen.iter().map(|k| k.split(':').next().unwrap()).collect();
        // `needs{kind:api}` 不入队（没有自己的区域），但**必须上通道**——首跑时深挖拿不到 scan_cells，
        // 不发就等于 A-4 在正常评审里整个不生效（2026-09-29 `/code-review` 抓到的 high）。
        assert_eq!(
            prefixes,
            ["api", "api", "needs", "needs", "scan[g1]@p/a#1#0", "scan[g2]@p/a#1#0"],
            "两格各发：一条疑点 + 一条 evidence needs（入队）+ 一条 api 声明（不入队，走 DeepMsg::ApiNeed）"
        );
    }

    /// 大格先发：provider 门宽 1 串行化派发，三组 diff 字节 小 / 大 / 中 → 调用序 g2, g3, g1；`res.cells` 仍按 plan 序（g1, g2, g3）。
    #[tokio::test]
    async fn scan_fanout_dispatches_largest_diff_first_but_returns_plan_order() {
        let repo = repo();
        let mut reg = ProviderRegistry::new();
        let backend = std::sync::Arc::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)));
        reg.insert_scan("p/a", "p", 1, Box::new(std::sync::Arc::clone(&backend)), Duration::from_secs(5), 0);
        let b: Vec<Bundle> = [("+s", 1), ("+L", 300), ("+M", 100)]
            .iter()
            .enumerate()
            .map(|(i, (tag, n))| Bundle::from_files(vec![FileChange { path: format!("src/f{i}.rs"), diff: format!("{tag}{}", "x".repeat(*n)) }]))
            .collect();
        let entries = vec![entry("p/a", "alpha", false)];
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 1), None, None).await;
        let ids: Vec<&str> = res.cells.iter().map(|c| c.cell_id.as_str()).collect();
        assert_eq!(ids, vec!["scan[g1]@p/a#1", "scan[g2]@p/a#1", "scan[g3]@p/a#1"], "结果按 plan 序回收");
        let tag_of = |i: usize| -> &'static str {
            let text = backend.messages_at(i).iter().filter_map(|m| match m { ChatMessage::UserBlocks(bl) => Some(bl.iter().map(|b| b.text.clone()).collect::<String>()), _ => None }).next().unwrap();
            if text.contains("+Lxxx") { "L" } else if text.contains("+Mxxx") { "M" } else { "s" }
        };
        assert_eq!((tag_of(0), tag_of(1), tag_of(2)), ("L", "M", "s"), "派发序 = diff 字节降序");
        assert!(res.warnings.iter().any(|w| w.contains("scanner cells 3 on provider p > maxConcurrency 1: 2 cell(s) queued (wall = queue wait + slowest cell; raise providers.p.maxConcurrency)")), "{:?}", res.warnings);
        assert!(res.cells.iter().all(|c| c.record.queue_wait_ms <= c.record.duration_ms), "排队时长含在 duration 内");
    }

    /// prd AC2：两条目 × 两组 = 四 cell；cell_id / route_key / manifest（含 scan prompt、model = route_key）/ roles / 模板哈希 / needs 计数 / usage。
    #[tokio::test]
    async fn scan_fanout_two_entries_two_groups_four_cells_with_manifest() {
        let repo = repo();
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300))), Duration::from_secs(5), 0);
        reg.insert_scan("q/b", "q", 8, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300))), Duration::from_secs(5), 0);
        let b = bundles(2);
        let dossiers = vec!["## d0".to_string(), "## d1".to_string()];
        let others = vec!["src/f1.rs (g2 +1/-0)".to_string(), "src/f0.rs (g1 +1/-0)".to_string()];
        let entries = vec![entry("p/a", "alpha", false), entry("q/b", "beta", false)];
        let sink = RecSink::new();
        let res = run_scan_fanout(&reg, &input(&b, &dossiers, &others, repo.path(), &entries, 1), Some(&sink), None).await;
        let ids: Vec<&str> = res.cells.iter().map(|c| c.cell_id.as_str()).collect();
        assert_eq!(ids, vec!["scan[g1]@p/a#1", "scan[g1]@q/b#1", "scan[g2]@p/a#1", "scan[g2]@q/b#1"]);
        assert!(res.cells.iter().all(|c| c.record.ok && c.record.role() == Role::Scanner), "{:?}", res.cells);
        assert_eq!(res.cells[1].family, "beta");
        assert_eq!(res.cells[2].group, 1);
        assert!(res.failed.is_empty() && res.warnings.is_empty());
        assert_eq!(res.usage, (40, 1200, 16));
        assert_eq!(res.needs_declared, 12, "每格声明 3 条 needs（带行锚 / 不带 / kind=api）× 4 格");
        assert!(res.cells.iter().all(|c| c.record.template_hash.as_deref() == Some(crate::prompt::scan_template_hash().as_str())));
        assert!(res.cells.iter().all(|c| c.record.attempts == 1 && c.record.iterations == 1));
        // 锚定：模型报第 9 行，quote 在第 2 行 → 行号改写
        let out = res.cells[0].output.as_ref().unwrap();
        assert_eq!((out.suspicions.len(), out.suspicions[0].line_start), (1, 2));
        // manifest：四条、model = route_key、scan prompt 在场且同组同 prompt；roles = [scanner]
        let m = sink.manifest.lock().unwrap();
        assert_eq!(m.len(), 4);
        assert_eq!(m[0].model, "p/a");
        assert!(m.iter().all(|i| i.scan.is_some() && i.prompt.is_empty() && i.template_hash.as_deref() == Some(crate::prompt::scan_template_hash().as_str())), "scan 存全文、prompt 留空、模板哈希随 manifest");
        assert_eq!(m[0].scan, m[1].scan, "同组各条目同一 prompt（cache 命中）");
        assert_ne!(m[0].scan, m[2].scan);
        assert!(m[0].scan.as_ref().unwrap().blocks[0].text.contains("src/f1.rs (g2"), "跨组索引进参考块");
        assert_eq!(*sink.roles.lock().unwrap(), vec![Role::Scanner]);
        assert_eq!(sink.cells.lock().unwrap().len(), 4, "on_cell 逐格回调");
    }

    /// samples = 2 → 每条目每组两格（#1 / #2）。
    #[tokio::test]
    async fn scan_fanout_samples_multiply_cells() {
        let repo = repo();
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300))), Duration::from_secs(5), 0);
        let b = bundles(1);
        let entries = vec![entry("p/a", "alpha", false)];
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 2), None, None).await;
        let ids: Vec<&str> = res.cells.iter().map(|c| c.cell_id.as_str()).collect();
        assert_eq!(ids, vec!["scan[g1]@p/a#1", "scan[g1]@p/a#2"]);
        assert_eq!(res.cells[1].sample, 2);
    }

    /// prd AC2（D13 单层）：provider 门宽 1 → 在途峰值 1 + 排队告警；门宽 2 → 峰值 2；门宽 ≥ 格数 → 无排队告警。
    #[tokio::test]
    async fn scan_fanout_provider_cap_bounds_inflight() {
        let repo = repo();
        let b = bundles(4);
        let entries = vec![entry("p/a", "alpha", false)];
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 1, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)).with_delay(Duration::from_millis(30))), Duration::from_secs(5), 0);
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 1), None, None).await;
        assert_eq!(res.cells.len(), 4);
        assert_eq!(reg.inflight_peaks().get("p"), Some(&1), "门宽 1 封顶");
        assert!(res.warnings.iter().any(|w| w.contains("scanner cells 4 on provider p > maxConcurrency 1: 3 cell(s) queued")), "{:?}", res.warnings);
        let mut reg2 = ProviderRegistry::new();
        reg2.insert_scan("p/a", "p", 2, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)).with_delay(Duration::from_millis(30))), Duration::from_secs(5), 0);
        let _ = run_scan_fanout(&reg2, &input(&b, &[], &[], repo.path(), &entries, 1), None, None).await;
        assert_eq!(reg2.inflight_peaks().get("p"), Some(&2), "门宽 2 封顶");
        let mut reg3 = ProviderRegistry::new();
        reg3.insert_scan("p/a", "p", 4, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300))), Duration::from_secs(5), 0);
        let res3 = run_scan_fanout(&reg3, &input(&b, &[], &[], repo.path(), &entries, 1), None, None).await;
        assert!(res3.warnings.is_empty(), "门宽 ≥ 格数无排队告警: {:?}", res3.warnings);
    }

    /// prd AC2：失败格——未注册条目 Unavailable、Auth 带 usage → failed 列表 + warning + 烧掉的 usage 入账；成功格不受影响。
    #[tokio::test]
    async fn scan_fanout_failed_cells_recorded_with_burned_usage() {
        let repo = repo();
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300))), Duration::from_secs(5), 0);
        reg.insert_scan("p/dead", "p", 8, Box::new(FakeChatBackend::new_results(vec![Err(ProviderError::Auth("no key".into()).attach_usage((7, 0, 0)))])), Duration::from_secs(5), 0);
        let b = bundles(1);
        let entries = vec![entry("p/a", "alpha", false), entry("p/dead", "alpha", false), entry("ghost/x", "gamma", false)];
        let sink = RecSink::new();
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 1), Some(&sink), None).await;
        assert_eq!(res.failed, vec!["scan[g1]@p/dead#1", "scan[g1]@ghost/x#1"]);
        assert_eq!(res.warnings.len(), 2);
        assert!(res.warnings[0].contains("auth error") && res.warnings[1].contains("scanner not in registry"), "{:?}", res.warnings);
        let dead = &res.cells[1].record;
        assert!(!dead.ok && dead.output == CellOutput::empty(Role::Scanner) && dead.input_tokens == 7 && dead.attempts == 1 && dead.template_hash.is_some());
        assert!(res.cells[1].output.is_none());
        assert_eq!(res.usage.0, 17, "成功 10 + 失败烧掉 7");
        assert_eq!(sink.cells.lock().unwrap().len(), 3);
    }

    /// prd AC2 / K5：warn 拼接——parse 丢弃（空 quote）、锚定丢弃（编造 quote）、零推理 + 短输出；cell 仍 ok、needs 仍计数。
    #[tokio::test]
    async fn scan_fanout_warn_collects_drops_and_zero_reasoning() {
        let repo = repo();
        let extra = r#",{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"fab","confidence":0.5,"quote":"nothing like this"},{"file":"src/a.rs","line_start":1,"line_end":1,"hint":"noq","confidence":0.5,"quote":"  "}"#;
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(FakeChatBackend::repeating(turn(&scan_json(extra), Some(0), 50))), Duration::from_secs(5), 0);
        let b = bundles(1);
        let pinned = vec![entry("p/a", "alpha", true)];
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &pinned, 1), None, None).await;
        let rec = &res.cells[0].record;
        assert!(rec.ok);
        let w = rec.warn.as_deref().unwrap();
        assert!(w.contains("parse dropped: no_quote=1") && w.contains("1 suspicions dropped: quote not found") && w.contains("zero reasoning tokens (output 50 tokens)"), "{w}");
        assert_eq!(res.cells[0].output.as_ref().unwrap().suspicions.len(), 1);
        assert_eq!(res.needs_declared, 3, "声明数按 scanner 报的算，与能否入队无关（含不入队的 kind=api）");
        // 未钉档位 → 不报零推理；输出长 → 不报
        let unpinned = vec![entry("p/a", "alpha", false)];
        let res2 = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &unpinned, 1), None, None).await;
        assert!(!res2.cells[0].record.warn.as_deref().unwrap().contains("zero reasoning"));
        assert!(!scan_cell_notes(&Default::default(), 0, 0, 0, true, Some(0), 300, 1).iter().any(|n| n.contains("zero reasoning")));
        assert!(scan_cell_notes(&Default::default(), 0, 0, 0, true, Some(0), 10, 1).len() == 1);
        // 零疑点短输出（出口验收）：0 疑点 + 883 token → warn；0 疑点但输出正常体量不 warn；有疑点不 warn。
        assert!(scan_cell_notes(&Default::default(), 0, 0, 0, false, None, 883, 0).iter().any(|n| n.contains("0 suspicions with short output")));
        assert!(scan_cell_notes(&Default::default(), 0, 0, 0, false, None, 9_000, 0).is_empty());
        assert!(scan_cell_notes(&Default::default(), 0, 0, 0, false, None, 883, 1).is_empty());
        assert!(scan_cell_notes(&Default::default(), 0, 0, 0, true, None, 10, 1).is_empty(), "provider 不分列 → 不报");
    }
}
