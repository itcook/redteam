use crate::bundle::Bundle;
use crate::domain::{CellOutput, Finding, Origin, RawFinding, Role, ScanOutput};
use crate::hash::fnv1a64_hex;
use crate::positioning::anchor_suspicions;
use crate::prompt::{build_scan_prompt, ScanPrompt, ScanPromptInput};
use crate::provider::registry::ProviderRegistry;
use crate::state::{CellInput, CellRecord};
use std::sync::Arc;
use tokio::sync::Semaphore;

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
        hard_constraint: raw.hard_constraint,
        quote: raw.quote.clone(),
        previously_dismissed: false,
        cluster_id: None,
        verify: None,
        origin: Origin::New,
    }
}

// ── scanner 扇出（M21a 批 F-2，design §5 / §12.3；prd K1 / K5 / K6 / K7）──────────────────────────────────────────────

/// scanner 条目（由 cli 从 `EffectiveModel` 转出）：route_key = `provider/id`（K1）；`thinking_pinned` = 配置了非 Off 档位
/// （零推理观测门，K5）。
#[derive(Clone, Debug)]
pub struct ScanEntrySpec {
    pub route_key: String,
    pub family: String,
    pub thinking_pinned: bool,
}

impl From<&crate::config::EffectiveModel> for ScanEntrySpec {
    fn from(em: &crate::config::EffectiveModel) -> Self {
        ScanEntrySpec {
            route_key: em.name.clone(),
            family: em.family.clone(),
            thinking_pinned: em.thinking_level.is_some_and(|l| l != crate::config::ThinkingLevel::Off),
        }
    }
}

/// 扇出输入：`bundles` / `dossiers` / `others` 等长等序（`cli::plan_cells` 产物）；`entries` = 活跃 scanner 列表（design §5：每条对每组各跑
/// 一次）；`workers` = 角色层并发（`review.scanners.workers`）；`samples` = 同模型多 lens 采样（`review.scanners.samples`）。
pub struct ScanFanoutInput<'a> {
    pub target_label: &'a str,
    pub bundles: &'a [Bundle],
    pub dossiers: &'a [String],
    pub others: &'a [String],
    pub user_focus: &'a str,
    pub spec_docs: &'a str,
    pub root: &'a std::path::Path,
    pub entries: &'a [ScanEntrySpec],
    pub workers: usize,
    pub samples: u32,
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
}

/// 零推理观测阈值：钉了档位、`reasoning_tokens == Some(0)` 且输出 token 低于此值 → cell warn（不重跑，K5）。
pub const ZERO_REASONING_SHORT_OUTPUT_TOKENS: u64 = 200;

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
        .map(|rec| {
            let (group, route_key, sample) = parse_scan_cell_id(&rec.cell).unwrap_or((0, rec.model.clone(), 1));
            let output = match (&rec.ok, &rec.output) {
                (true, CellOutput::Scan(o)) => Some(o.clone()),
                _ => None,
            };
            ScanCellResult { cell_id: rec.cell.clone(), group, route_key, family: rec.family.clone(), sample, record: rec, output }
        })
        .collect()
}

/// scanner 静态扇出（design §2：纯生产者、无候补）：cell = 组 × 条目 × samples；prompt 每组只构造一次（同组各条目 / samples 字节相同 →
/// 共享头 cache 命中）；并发 = 角色 `workers` 信号量（此处）× provider 在途门（registry 内，D7 A5）。单格：`registry.scan` →
/// `anchor_suspicions`（quote 不在文件即丢，design §4）→ `CellRecord`；parse / 锚定丢弃计数与零推理观测拼进 `warn`（cell 仍 ok）；
/// 失败格 `CellRecord::failed` + 烧掉的 usage 入账，进 `failed`。needs 只随 cell 落库并计数。
pub async fn run_scan_fanout(
    registry: &ProviderRegistry,
    input: &ScanFanoutInput<'_>,
    sink: Option<&dyn CellSink>,
) -> ScanResults {
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
                precedents: "",
                dossier: input.dossiers.get(g).map(String::as_str).unwrap_or(""),
                spec_docs: input.spec_docs,
                repo_brief: "",
            })
        })
        .collect();
    let samples = input.samples.max(1);
    let plan: Vec<(usize, &ScanEntrySpec, u32, String)> = (0..input.bundles.len())
        .flat_map(|g| {
            input.entries.iter().flat_map(move |e| (1..=samples).map(move |k| (g, e, k, scan_cell_id(g, &e.route_key, k))))
        })
        .collect();
    if let Some(s) = sink {
        s.on_roles(&[Role::Scanner]);
        let inputs: Vec<CellInput> = plan
            .iter()
            .map(|(g, e, _, id)| CellInput {
                cell: id.clone(),
                model: e.route_key.clone(),
                prompt: prompts[*g].flatten(),
                scan: Some(prompts[*g].clone()),
            })
            .collect();
        s.manifest(&inputs);
    }
    let workers = Arc::new(Semaphore::new(input.workers.max(1)));
    let template_hash = crate::prompt::scan_template_hash();
    let futs = plan.iter().map(|(g, e, k, id)| {
        let workers = Arc::clone(&workers);
        let prompt = &prompts[*g];
        let template_hash = template_hash.clone();
        async move {
            let _permit = workers.acquire().await.expect("workers semaphore never closed");
            let t0 = std::time::Instant::now();
            let res = registry.scan(&e.route_key, prompt).await;
            let duration_ms = t0.elapsed().as_millis() as u64;
            let (mut record, output) = match res {
                Ok(call) => {
                    let anchored = anchor_suspicions(input.root, call.output);
                    let notes = scan_cell_notes(
                        &call.stats, anchored.dropped, anchored.dropped_unreadable,
                        e.thinking_pinned, call.reasoning_tokens, call.output_tokens,
                    );
                    let mut rec = CellRecord::ok(id.clone(), e.route_key.clone(), CellOutput::Scan(anchored.output.clone()))
                        .with_family(e.family.clone());
                    rec.input_tokens = call.input_tokens;
                    rec.output_tokens = call.output_tokens;
                    rec.cached_input_tokens = call.cached_input_tokens;
                    rec.iterations = call.iterations;
                    rec.warn = if notes.is_empty() { None } else { Some(notes.join("; ")) };
                    (rec, Some(anchored.output))
                }
                Err(err) => {
                    let (i, o, c) = err.burned_usage();
                    let mut rec = CellRecord::failed(id.clone(), e.route_key.clone(), Role::Scanner, err.to_string())
                        .with_family(e.family.clone());
                    rec.input_tokens = i;
                    rec.output_tokens = o;
                    rec.cached_input_tokens = c;
                    (rec, None)
                }
            };
            record.duration_ms = duration_ms;
            record.attempts = 1;
            record.template_hash = Some(template_hash);
            if let Some(s) = sink {
                s.on_cell(record.clone());
            }
            ScanCellResult {
                cell_id: id.clone(), group: *g, route_key: e.route_key.clone(), family: e.family.clone(), sample: *k, record, output,
            }
        }
    });
    let cells: Vec<ScanCellResult> = futures::future::join_all(futs).await;
    let mut out = ScanResults::default();
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

/// cell warn 片段：parse 丢弃 / 截断计数、锚定丢弃、零推理观测——不静默（批 D-1 注释「丢弃 / 截断计数由调用方写进 cell warn」）。
fn scan_cell_notes(
    st: &crate::provider::parse::ScanParseStats,
    dropped: u32,
    unreadable: u32,
    pinned: bool,
    reasoning: Option<u64>,
    output_tokens: u64,
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
    if pinned && reasoning == Some(0) && output_tokens < ZERO_REASONING_SHORT_OUTPUT_TOKENS {
        notes.push(format!("pinned thinking but zero reasoning tokens (output {output_tokens} tokens)"));
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
    use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend};
    use crate::provider::ProviderError;
    use std::sync::Mutex;
    use std::time::Duration;

    const REAL: &str = "let t = secret();";

    fn scan_json(extra: &str) -> String {
        format!(r#"{{"suspicions":[{{"file":"src/a.rs","line_start":9,"line_end":9,"hint":"real","confidence":0.7,"quote":"{REAL}"}}{extra}],"needs":[{{"path":"src/b.rs","reason":"r"}}],"scanned_areas":[{{"file":"src/a.rs","ranges":["1-2"]}}]}}"#)
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
        ScanEntrySpec { route_key: key.into(), family: fam.into(), thinking_pinned: pinned }
    }
    struct RecSink { manifest: Mutex<Vec<CellInput>>, cells: Mutex<Vec<CellRecord>>, roles: Mutex<Vec<Role>> }
    impl RecSink { fn new() -> Self { RecSink { manifest: Mutex::new(vec![]), cells: Mutex::new(vec![]), roles: Mutex::new(vec![]) } } }
    impl CellSink for RecSink {
        fn manifest(&self, inputs: &[CellInput]) { self.manifest.lock().unwrap().extend_from_slice(inputs); }
        fn on_cell(&self, rec: CellRecord) { self.cells.lock().unwrap().push(rec); }
        fn on_roles(&self, roles: &[Role]) { self.roles.lock().unwrap().extend_from_slice(roles); }
    }
    fn input<'a>(bundles: &'a [Bundle], dossiers: &'a [String], others: &'a [String], root: &'a std::path::Path, entries: &'a [ScanEntrySpec], workers: usize, samples: u32) -> ScanFanoutInput<'a> {
        ScanFanoutInput { target_label: "wt", bundles, dossiers, others, user_focus: "", spec_docs: "", root, entries, workers, samples }
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
        let res = run_scan_fanout(&reg, &input(&b, &dossiers, &others, repo.path(), &entries, 4, 1), Some(&sink)).await;
        let ids: Vec<&str> = res.cells.iter().map(|c| c.cell_id.as_str()).collect();
        assert_eq!(ids, vec!["scan[g1]@p/a#1", "scan[g1]@q/b#1", "scan[g2]@p/a#1", "scan[g2]@q/b#1"]);
        assert!(res.cells.iter().all(|c| c.record.ok && c.record.role() == Role::Scanner), "{:?}", res.cells);
        assert_eq!(res.cells[1].family, "beta");
        assert_eq!(res.cells[2].group, 1);
        assert!(res.failed.is_empty() && res.warnings.is_empty());
        assert_eq!(res.usage, (40, 1200, 16));
        assert_eq!(res.needs_declared, 4);
        assert!(res.cells.iter().all(|c| c.record.template_hash.as_deref() == Some(crate::prompt::scan_template_hash().as_str())));
        assert!(res.cells.iter().all(|c| c.record.attempts == 1 && c.record.iterations == 1));
        // 锚定：模型报第 9 行，quote 在第 2 行 → 行号改写
        let out = res.cells[0].output.as_ref().unwrap();
        assert_eq!((out.suspicions.len(), out.suspicions[0].line_start), (1, 2));
        // manifest：四条、model = route_key、scan prompt 在场且同组同 prompt；roles = [scanner]
        let m = sink.manifest.lock().unwrap();
        assert_eq!(m.len(), 4);
        assert_eq!(m[0].model, "p/a");
        assert!(m.iter().all(|i| i.scan.is_some() && i.prompt == i.scan.as_ref().unwrap().flatten()));
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
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 4, 2), None).await;
        let ids: Vec<&str> = res.cells.iter().map(|c| c.cell_id.as_str()).collect();
        assert_eq!(ids, vec!["scan[g1]@p/a#1", "scan[g1]@p/a#2"]);
        assert_eq!(res.cells[1].sample, 2);
    }

    /// prd AC2：并发两层——workers 1 / provider 帽 8 → 在途峰值 1；workers 4 / 帽 2 → 峰值 2。
    #[tokio::test]
    async fn scan_fanout_workers_and_provider_cap_bound_inflight() {
        let repo = repo();
        let b = bundles(4);
        let entries = vec![entry("p/a", "alpha", false)];
        let mut reg = ProviderRegistry::new();
        reg.insert_scan("p/a", "p", 8, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)).with_delay(Duration::from_millis(30))), Duration::from_secs(5), 0);
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 1, 1), None).await;
        assert_eq!(res.cells.len(), 4);
        assert_eq!(reg.inflight_peaks().get("p"), Some(&1), "workers 1 封顶");
        let mut reg2 = ProviderRegistry::new();
        reg2.insert_scan("p/a", "p", 2, Box::new(FakeChatBackend::repeating(turn(&scan_json(""), None, 300)).with_delay(Duration::from_millis(30))), Duration::from_secs(5), 0);
        let _ = run_scan_fanout(&reg2, &input(&b, &[], &[], repo.path(), &entries, 4, 1), None).await;
        assert_eq!(reg2.inflight_peaks().get("p"), Some(&2), "provider 帽 2 封顶");
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
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &entries, 4, 1), Some(&sink)).await;
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
        let res = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &pinned, 4, 1), None).await;
        let rec = &res.cells[0].record;
        assert!(rec.ok);
        let w = rec.warn.as_deref().unwrap();
        assert!(w.contains("parse dropped: no_quote=1") && w.contains("1 suspicions dropped: quote not found") && w.contains("zero reasoning tokens (output 50 tokens)"), "{w}");
        assert_eq!(res.cells[0].output.as_ref().unwrap().suspicions.len(), 1);
        assert_eq!(res.needs_declared, 1);
        // 未钉档位 → 不报零推理；输出长 → 不报
        let unpinned = vec![entry("p/a", "alpha", false)];
        let res2 = run_scan_fanout(&reg, &input(&b, &[], &[], repo.path(), &unpinned, 4, 1), None).await;
        assert!(!res2.cells[0].record.warn.as_deref().unwrap().contains("zero reasoning"));
        assert!(!scan_cell_notes(&Default::default(), 0, 0, true, Some(0), 300).iter().any(|n| n.contains("zero reasoning")));
        assert!(scan_cell_notes(&Default::default(), 0, 0, true, Some(0), 10).len() == 1);
        assert!(scan_cell_notes(&Default::default(), 0, 0, true, None, 10).is_empty(), "provider 不分列 → 不报");
    }
}
