use crate::domain::{ReviewReport, Severity};

#[derive(Clone, Copy, Debug)]
pub enum Format { Json, Md }

pub fn render(report: &ReviewReport, fmt: Format) -> String {
    match fmt {
        Format::Json => serde_json::to_string_pretty(report).unwrap(),
        Format::Md => render_md(report),
    }
}

/// token 数自适应缩写：≥1M → x.xxM、≥1K → x.xxK、否则原样。
/// 结果进 markdown 表格 cell——禁用裸 `|`（会被解析成列分隔符撕裂表格），缩写用括号。
fn human_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("`{n}` ({:.2}M)", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("`{n}` ({:.2}K)", n as f64 / 1_000.0)
    } else {
        format!("`{n}`")
    }
}

/// Duration 展示：<1min 显整数秒（如 `≈ 45 s`），否则回落原有分钟粒度（`≈ X.X mins`）——
/// 批 4 polish：秒级任务此前恒显 `≈ 0.0 mins`/`≈ 0.8 mins`，对用户无意义（design §5）。
fn fmt_duration(ms: u64) -> String {
    if ms < 60_000 {
        format!("≈ {} s", (ms as f64 / 1000.0).round() as u64)
    } else {
        format!("≈ {:.1} mins", ms as f64 / 60_000.0)
    }
}

fn sev_emoji(sev: Severity) -> &'static str {
    match sev {
        Severity::Critical => "🔴",
        Severity::High => "🟠",
        Severity::Medium => "🟡",
        Severity::Low => "🔵",
    }
}

/// 人读评审报告（rN.md 模板,用户 2026-07-03 定 + 同日补充调整）：H1 带 #job_id
/// （`-round {n}` 是 ship 协议层概念且 md 在 job 收尾预渲染入库——由 ship 宿主写文件时补）；
/// Summary(verdict/severity 表/Info 表/Warnings 有则显) + Finding Details(severity 在 body 前)。
fn render_md(r: &ReviewReport) -> String {
    let repo = std::env::current_dir()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Redteam".into());
    let verdict = match r.verdict.as_str() {
        "approve" => "✅ approve",
        "reject" => "❌ reject",
        "scan-only" => "🔎 scan-only（疑点索引，不产 finding）",
        _ => "⚠️ incomplete",
    };
    let mut s = String::new();
    if r.run.job_id.is_empty() {
        s.push_str(&format!("# {repo} Redteam Review\n\n## Summary\n\n"));
    } else {
        s.push_str(&format!("# {repo} Redteam Review #{}\n\n## Summary\n\n", r.run.job_id));
    }
    s.push_str(&format!("- Verdict: {verdict}\n\n"));
    // 被驳回的不计入头条（`Stats` 与 verdict 同口径），但它们**仍列在下面的 Finding Details 里**——
    // 只写一个过滤后的数字，会让用户读到「Findings: 0」正上方挂着一条 high（收口评审 #10）。
    let refuted = r.findings.iter().filter(|f| f.is_refuted()).count();
    if refuted > 0 {
        s.push_str(&format!(
            "- Findings: {} (+{refuted} refuted by verifier — listed below, excluded from counts and verdict)\n",
            r.stats.total
        ));
    } else {
        s.push_str(&format!("- Findings: {}\n", r.stats.total));
    }
    if r.stats.total > 0 {
        s.push('\n');
        s.push_str("  | Severity   | Count |\n  | ---------- | ----- |\n");
        for (e, name, n) in [
            ("🔴", "critical", r.stats.critical),
            ("🟠", "high", r.stats.high),
            ("🟡", "medium", r.stats.medium),
            ("🔵", "low", r.stats.low),
        ] {
            s.push_str(&format!("  | {e} {name} | {n} |\n"));
        }
    }
    // Info 表：Models 显示 model id（缺映射回退 provider 名），failed 标 ❗️fail
    let models: Vec<String> = r
        .run
        .models
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let display = r.run.model_ids.get(i).filter(|s| !s.is_empty()).unwrap_or(name);
            if r.run.failed_models.contains(name) {
                format!("{display} ❗️fail")
            } else {
                display.clone()
            }
        })
        .collect();
    s.push_str("\n- Info\n\n  | Field         | Value |\n  | ------------- | ----- |\n");
    s.push_str(&format!("  | Models        | {} |\n", models.join(", ")));
    s.push_str(&format!("  | Duration      | {} |\n", fmt_duration(r.run.duration_ms)));
    s.push_str(&format!("  | Input Tokens  | {} |\n", human_tokens(r.run.usage.input_tokens)));
    s.push_str(&format!("  | Output Tokens | {} |\n", human_tokens(r.run.usage.output_tokens)));
    // L1 缓存观测：命中读量占总输入的比例（实付全价 ≈ input − cached）。cached=0 且 input 大
    // = 未命中或该 provider 缓存字段未知，如实显示不掩盖。
    let cached = r.run.usage.cached_input_tokens;
    let pct = if r.run.usage.input_tokens > 0 { cached * 100 / r.run.usage.input_tokens } else { 0 };
    s.push_str(&format!("  | Cached Input  | {} ({pct}%) |\n", human_tokens(cached)));
    if let Some(l) = &r.run.ledger {
        s.push_str(&format!("  | Ledger        | {} card(s) from {} |\n", l.loaded, l.source));
    }
    // 休眠疑点线索（M21c 批 A，A-2）：只披露、不进 verdict。**必须进 md**——ship 循环与裸终端读的都是 md
    // （`redteam result <job> --format md`），只落 JSON 等于这个功能对主消费路径不存在（收口评审 #4）。
    if !r.run.dormant_leads.is_empty() {
        s.push_str(&format!("\n- Dormant leads ({}) — 台账里落在本轮变更文件上的历史截断项；**未经深挖、不进裁决**\n\n", r.run.dormant_leads.len()));
        s.push_str("  | File:Line | Hint | First seen | Activations |\n");
        s.push_str("  | --- | --- | --- | --- |\n");
        for l in &r.run.dormant_leads {
            let short = l.first_round.chars().take(9).collect::<String>();
            s.push_str(&format!("  | {}:{} | {} | {} | {} |\n", l.file, l.line, l.hint, short, l.activations));
        }
    }
    // T55（cell 级可观测）：per-model 运行摘要——耗时/轮次（>1=有 repair/多轮）/用量一眼可见，
    // 不必再手查 proj.db。旧报告无此数据（serde default 空）不渲染。
    if !r.run.cells.is_empty() {
        s.push_str("\n- Cells\n\n");
        s.push_str("  | Cell | Model | Status | Iter | Duration | In | Out | Cached |\n");
        s.push_str("  | --- | --- | --- | --- | --- | --- | --- | --- |\n");
        for c in &r.run.cells {
            let st = if c.ok { "✓" } else { "✗" };
            let att = if c.attempts > 1 { format!(" ({} att)", c.attempts) } else { String::new() };
            s.push_str(&format!(
                "  | {} | {} | {st} | {}{} | {} | {} | {} | {} |\n",
                if c.cell.is_empty() { "-" } else { c.cell.as_str() },
                c.model,
                c.iterations,
                att,
                fmt_duration(c.duration_ms),
                human_tokens(c.input_tokens),
                human_tokens(c.output_tokens),
                human_tokens(c.cached_input_tokens),
            ));
        }
    }
    // M13 增量轮注记（R5 决议：carried/new 终态语义住 result/rN.md，不进 status）。
    if r.run.incremental {
        let base = r.run.delta_base.as_deref().unwrap_or("?");
        let carried = r.findings.iter().filter(|f| f.origin == crate::domain::Origin::Carried).count();
        let fresh = r.findings.len() - carried;
        s.push_str(&format!("  | Incremental   | delta since {} |\n", &base[..12.min(base.len())]));
        s.push_str(&format!("  | Carried / New | {carried} / {fresh} |\n"));
        if !r.run.carried_dropped.is_empty() {
            s.push_str(&format!("  | Carry Dropped | {} |\n", r.run.carried_dropped.join(", ")));
        }
        if !r.run.resolved.is_empty() {
            s.push_str(&format!("  | Resolved      | {} |\n", r.run.resolved.join(", ")));
        }
        if !r.run.resolution_disputed.is_empty() {
            s.push_str(&format!("  | ⚠️ Disputed    | {} |\n", r.run.resolution_disputed.join(", ")));
        }
    }
    render_scan_md(r, &mut s);
    if !r.run.warnings.is_empty() {
        s.push_str("\n- Warnings\n\n");
        for (i, w) in r.run.warnings.iter().enumerate() {
            s.push_str(&format!("  - W{}: {w}\n", i + 1));
        }
    }
    if !r.findings.is_empty() {
        s.push_str("\n## Finding Details\n");
        let mut idx = 0;
        for sev in [Severity::Critical, Severity::High, Severity::Medium, Severity::Low] {
            for f in r.findings.iter().filter(|f| f.severity == sev) {
                idx += 1;
                s.push_str(&format!("\n### F{idx}: {}\n\n", f.title));
                s.push_str(&format!("Severity: {} {}\n\n", sev_emoji(sev), sev.as_str()));
                s.push_str(&format!("{}\n\n", f.body));
                s.push_str("| Field      | Value |\n| ---------- | ----- |\n");
                s.push_str(&format!("| File       | {} L{}-L{} |\n", f.file, f.line_start, f.line_end));
                s.push_str(&format!("| Found By   | {} |\n", f.models.join(", ")));
                s.push_str(&format!("| Confidence | {:.2} |\n", f.confidence));
                if f.origin == crate::domain::Origin::Carried {
                    s.push_str("| Origin     | carried from an earlier round |\n");
                }
                if let Some(v) = &f.verify {
                    let tag = match v.verdict {
                        crate::domain::VerifyVerdict::Refuted => "refuted",
                        crate::domain::VerifyVerdict::Upheld => "upheld",
                    };
                    s.push_str(&format!("| Verify     | {tag} by {} |\n", v.model));
                }
                if f.previously_dismissed {
                    s.push_str("| Dismissed  | previously dismissed (exempt from verdict) |\n");
                }
                // rec 多行时后续行补 `> ` 前缀，防止脱出引用块。
                s.push_str(&format!("\n> **Recommendation**\n>\n> {}\n", f.recommendation.replace('\n', "\n> ")));
            }
        }
    }
    if !r.clusters.is_empty() {
        s.push_str("\n## Correlated Clusters\n\n");
        for c in &r.clusters {
            s.push_str(&format!("- **{}** ({}): {} — {}\n", c.theme, c.relation, c.note, c.finding_ids.join(", ")));
        }
    }
    s
}

/// scan-only 段（批 F-3）：组表（label / 文件数 / cells / 疑点数 / 装包形态）→ 失败 cell → 按组疑点列表
/// （file:a-b · 类别 · 置信 · agreement（家族）· 全部 hint；quote 首行 ≤ 120 字符）→ 覆盖自报与 needs 计数在首行。
fn render_scan_md(r: &ReviewReport, s: &mut String) {
    let Some(scan) = &r.scan else { return };
    s.push_str("\n## Scan\n\n");
    let cov = scan.scanned_coverage.map(|c| format!("{:.0}%", c * 100.0)).unwrap_or_else(|| "n/a".into());
    s.push_str(&format!(
        "- Groups: {} · Cells: {} ok / {} failed · Suspicions: {} (agreement ≥ 2: {}) · Changed-line coverage: {cov} · Needs declared: {}\n",
        scan.groups.len(), scan.stats.cells_ok, scan.stats.cells_failed, scan.stats.total, scan.stats.agreement_ge2, scan.stats.needs_declared
    ));
    // L4-B 三方契约（M21d 批 A）：注入量与**降级原因**都要走 md——ship 与裸终端读的是 md，
    // 只进 JSON 等于对主消费路径不存在（M21c 批 A 收口评审 #4 的同款教训）。全零则不占版面。
    let (inj, miss, spill, skipped) = scan.groups.iter().fold((0, 0, 0, 0), |(a, b, c, d), g| {
        (a + g.pack.contracts, b + g.pack.contract_misses, c + g.pack.contract_spilled, d + usize::from(g.pack.contract_budget_skipped))
    });
    // 深挖侧的 `needs{kind:api}` 注入（A-4）并在同一行：它由 scanner 的声明触发，和扫描账放一起才读得懂。
    let (api, api_miss, api_spill, api_skip) = r.run.deep.as_ref().map_or((0, 0, 0, 0), |d| {
        (d.api_contracts as usize, d.api_contract_misses as usize, d.api_contract_spilled as usize, d.api_contract_budget_skipped as usize)
    });
    // 两族计数**各管各的行**：合用一个门会印出「0 blocks injected · 0 crates not located」的自相矛盾标题，
    // 正下方挂着非零的 needs{api} 子行（scan 侧因预算不足没注入、深挖侧注入了，是真实组合——dogfood 抓到）。
    if inj + miss + spill + skipped > 0 {
        s.push_str(&format!(
            "- Third-party contracts: {inj} blocks injected · {miss} crates not located (degraded, not blocking) · {spill} over cap\n"
        ));
        if skipped > 0 {
            s.push_str(&format!("  - {skipped} group(s) skipped injection: in-repo context used up the shared cap\n"));
        }
    }
    if api + api_miss + api_spill + api_skip > 0 {
        s.push_str(&format!(
            "- Third-party contracts on needs{{api}} (with bodies, into dig cells): {api} injected · {api_miss} not located · {api_spill} over cap · {api_skip} cell(s) skipped (cap used up)\n"
        ));
    }
    if !scan.groups.is_empty() {
        s.push_str("\n  | Group | Files | Cells | Suspicions | Pack |\n  | --- | --- | --- | --- | --- |\n");
        for (i, g) in scan.groups.iter().enumerate() {
            s.push_str(&format!(
                "  | g{} `{}` | {} | {} | {} | {} · sliced {} · folded {} · outside {} · contracts {} |\n",
                i + 1, g.label, g.files.len(), g.cells.len(), g.suspicions, g.pack.body, g.pack.sliced, g.pack.folded_lines,
                g.pack.outside_sites, g.pack.contracts
            ));
        }
    }
    let failed: Vec<&str> = r.run.coverage.failed.values().flatten().map(String::as_str).collect();
    if !failed.is_empty() {
        s.push_str(&format!("\n- Failed cells: {}\n", failed.join(", ")));
    }
    if !scan.suspicions.is_empty() {
        s.push_str("\n### Suspicions\n");
        let mut cur: Option<usize> = None;
        for m in &scan.suspicions {
            if cur != Some(m.group) {
                cur = Some(m.group);
                s.push_str(&format!("\n#### g{}\n\n", m.group + 1));
            }
            let cat = m.suspicion.category.map(|c| format!("[{c:?}] ")).unwrap_or_default();
            s.push_str(&format!(
                "- `{}:{}-{}` {cat}conf {:.2} · agreement {} ({}) — {}\n",
                m.suspicion.file, m.suspicion.line_start, m.suspicion.line_end, m.suspicion.confidence, m.agreement,
                m.families.join(", "), m.hints.iter().map(|h| h.split_whitespace().collect::<Vec<_>>().join(" ")).collect::<Vec<_>>().join(" / ")
            ));
            let first: String = m.suspicion.quote.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(120).collect();
            if !first.is_empty() {
                s.push_str(&format!("  > `{}`\n", first.replace('`', "'")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::*;
    fn report() -> ReviewReport {
        ReviewReport { scan: None, cells: vec![], schema_version:"2".into(), verdict:Verdict::Reject,
            stats:Stats{high:1,total:1,..Default::default()},
            findings:vec![Finding{ id:"1".into(),severity:Severity::High,title:"boom".into(),body:"b".into(),
                file:"a.rs".into(),line_start:3,line_end:5,confidence:0.9,recommendation:"fix".into(),
                models:vec!["claude".into()],agreement:1, suspicion_agreement: 0,hard_constraint:false,
                quote:"snippet".into(),cluster_id:None,verify:None,previously_dismissed:false,
                origin:Origin::New}],
            clusters:vec![], run:RunMeta::default() }
    }

    /// 休眠线索必须进 md（M21c 批 A，收口评审 #4）：ship 循环与裸终端读的都是 md
    /// （`redteam result <job> --format md`），只落 JSON 等于这个功能对主消费路径不存在。
    /// 同时措辞要说清它**未经深挖、不进裁决**，免得被当成 finding 读。
    #[test]
    fn md_renders_dormant_leads_as_disclosure() {
        let mut r = report();
        r.run.dormant_leads = vec![
            DormantLead { file: "a.rs".into(), line: 42, hint: "边界没校验".into(), first_round: "4317c8afce0".into(), activations: 2 },
            DormantLead { file: "b.ts".into(), line: 7, hint: "超时不可调".into(), first_round: "4317c8afce0".into(), activations: 0 },
        ];
        let md = render_md(&r);
        assert!(md.contains("Dormant leads (2)"), "要有披露段: {md}");
        assert!(md.contains("未经深挖") && md.contains("不进裁决"), "措辞要挡住「当 finding 读」: {md}");
        assert!(md.contains("a.rs:42") && md.contains("边界没校验") && md.contains("4317c8afc"), "{md}");
        assert!(md.contains("b.ts:7"), "{md}");
        // 没有线索时一个字都不多写
        r.run.dormant_leads.clear();
        assert!(!render_md(&r).contains("Dormant"), "空则不渲染");
    }

    /// 被驳回的 finding 不计入头条、但仍列在详情里——头条必须把这件事说出来（收口评审 #10）。
    /// 旧行为：`- Findings: 0` 正下方挂着 `### F1: …  Severity: 🟠 high`。
    #[test]
    fn md_headline_discloses_refuted_findings_that_are_still_listed() {
        let mut r = report();
        r.findings[0].set_verdict(Verify {
            verdict: VerifyVerdict::Refuted, model: "p/v".into(), note: "已有守卫".into(),
        });
        r.stats = Stats::from_findings(&r.findings);
        let md = render_md(&r);
        assert!(md.contains("- Findings: 0 (+1 refuted by verifier"), "头条要说清: {}", &md[..md.len().min(400)]);
        assert!(md.contains("### F1: boom"), "详情照列——审计不断链");
    }

    /// L4-B 三方契约（M21d 批 A）：注入量与降级原因必须走 **md**——ship 与裸终端读的是 md，
    /// 只进 JSON 等于对主消费路径不存在（M21c 批 A 收口评审 #4 同款）。
    #[test]
    fn scan_md_discloses_third_party_contract_injection_and_degradation() {
        let mut r = report();
        let pack = |c: usize, m: usize, sp: usize| crate::pack::PackStats {
            body: "slice".into(), contracts: c, contract_misses: m, contract_spilled: sp, ..Default::default()
        };
        r.scan = Some(ScanReport {
            groups: vec![
                ScanGroup { label: "src/a.rs".into(), files: vec!["src/a.rs".into()], cells: vec![], suspicions: 0, pack: pack(3, 1, 0) },
                ScanGroup { label: "src/b.rs".into(), files: vec!["src/b.rs".into()], cells: vec![], suspicions: 0, pack: pack(2, 0, 4) },
            ],
            suspicions: vec![], stats: Default::default(), scanned_coverage: None,
        });
        let md = render_md(&r);
        assert!(
            md.contains("- Third-party contracts: 5 blocks injected · 1 crates not located (degraded, not blocking) · 4 over cap"),
            "注入量与降级原因要跨组合计后披露: {md}"
        );
        assert!(md.contains("· outside 0 · contracts 3 |"), "组表也要给每组的注入数: {md}");

        // 深挖侧的 needs{api} 注入（A-4）自成一行——它由 scanner 的声明触发
        r.run.deep = Some(crate::domain::DeepMeta { api_contracts: 2, api_contract_misses: 1, ..Default::default() });
        assert!(
            render_md(&r).contains("- Third-party contracts on needs{api} (with bodies, into dig cells): 2 injected · 1 not located"),
            "深挖侧注入也要走 md: {}", render_md(&r)
        );

        // scan 侧全零、深挖侧非零是**真实组合**（组 pack 的帽不足没注入，而 scanner 报了 needs{api}、
        // 深挖那格帽够）：两族计数各管各的行，否则会印出「0 blocks injected · 0 crates not located」
        // 的自相矛盾标题，正下方还挂着非零子行（2026-09-29 dogfood 抓到——当时的用例同一步里
        // 既清零 pack 又把 deep 设 None，这个组合从未被渲染过）。
        r.scan.as_mut().unwrap().groups.iter_mut().for_each(|g| g.pack = pack(0, 0, 0));
        let md = render_md(&r);
        assert!(!md.contains("0 blocks injected"), "scan 侧全零就不该印那一行: {md}");
        assert!(md.contains("on needs{api}"), "深挖侧非零仍要披露: {md}");

        r.run.deep = None;
        assert!(!render_md(&r).contains("Third-party contracts"), "全零不占版面（多数仓不是 Rust）");

        // 帽被仓内上下文吃光 → 也要看得见「跳过了」，不能只写进 pack 正文的省略清单
        r.scan.as_mut().unwrap().groups[0].pack.contract_budget_skipped = true;
        let md = render_md(&r);
        assert!(md.contains("1 group(s) skipped injection"), "预算跳过要披露: {md}");
    }

    /// 批 F-3 AC3：scan-only md——verdict 行、组表、失败 cell、按组疑点（类别 / 置信 / agreement / 家族 / hint / quote 首行）、覆盖自报。
    #[test]
    fn scan_only_md_renders_groups_suspicions_and_coverage() {
        let susp = |file: &str, ls: u32, hint: &str, conf: f32, quote: &str| Suspicion { file: file.into(), line_start: ls, line_end: ls, hint: hint.into(), category: Some(Category::L1), confidence: conf, quote: quote.into() };
        let mut run = RunMeta { job_id: "j9".into(), models: vec!["p/a".into(), "q/b".into()], ..Default::default() };
        run.coverage.failed.insert(Role::Scanner, vec!["scan[g2]@q/b#1".into()]);
        run.cells.push(CellSummary { cell: "scan[g1]@p/a#1".into(), model: "p/a".into(), role: "scanner".into(), ok: true, iterations: 1, attempts: 1, duration_ms: 1200, queue_wait_ms: 0, input_tokens: 1000, output_tokens: 50, cached_input_tokens: 400 });
        let scan = ScanReport {
            groups: vec![
                ScanGroup { label: "src/a.rs".into(), files: vec!["src/a.rs".into()], cells: vec!["scan[g1]@p/a#1".into(), "scan[g1]@q/b#1".into()], suspicions: 2, pack: crate::pack::PackStats { body: "slice".into(), sliced: 1, folded_lines: 30, ..Default::default() } },
                ScanGroup { label: "src/b.rs".into(), files: vec!["src/b.rs".into()], cells: vec!["scan[g2]@p/a#1".into(), "scan[g2]@q/b#1".into()], suspicions: 1, pack: crate::pack::PackStats { body: "slice".into(), ..Default::default() } },
            ],
            suspicions: vec![
                MergedSuspicion { group: 0, suspicion: susp("src/a.rs", 3, "leak", 0.8, "let t = secret();\nlog(t);"), agreement: 2, families: vec!["alpha".into(), "beta".into()], cell_ids: vec![], hints: vec!["leak".into(), "secret logged".into()] },
                MergedSuspicion { group: 0, suspicion: susp("src/a.rs", 9, "unchecked", 0.4, "x.unwrap()"), agreement: 1, families: vec!["alpha".into()], cell_ids: vec![], hints: vec!["unchecked".into()] },
                MergedSuspicion { group: 1, suspicion: susp("src/b.rs", 1, "race", 0.5, "static mut X"), agreement: 1, families: vec!["beta".into()], cell_ids: vec![], hints: vec!["race".into()] },
            ],
            scanned_coverage: Some(0.75),
            stats: ScanStats { total: 3, cells_ok: 3, cells_failed: 1, agreement_ge2: 1, needs_declared: 2, ..Default::default() },
        };
        let r = ReviewReport { schema_version: "4".into(), verdict: Verdict::ScanOnly, stats: Stats::default(), findings: vec![], clusters: vec![], run, scan: Some(scan), cells: vec![] };
        let md = render(&r, Format::Md);
        for must in [
            "- Verdict: 🔎 scan-only",
            "## Scan",
            "- Groups: 2 · Cells: 3 ok / 1 failed · Suspicions: 3 (agreement ≥ 2: 1) · Changed-line coverage: 75% · Needs declared: 2",
            "| g1 `src/a.rs` | 1 | 2 | 2 | slice · sliced 1 · folded 30 · outside 0 · contracts 0 |",
            "- Failed cells: scan[g2]@q/b#1",
            "#### g1",
            "- `src/a.rs:3-3` [L1] conf 0.80 · agreement 2 (alpha, beta) — leak / secret logged",
            "  > `let t = secret();`",
            "#### g2",
            "- `src/b.rs:1-1` [L1] conf 0.50 · agreement 1 (beta) — race",
            "| scan[g1]@p/a#1 | p/a | ✓ | 1 | ≈ 1 s | `1000` (1.00K) | `50` | `400` |",
        ] {
            assert!(md.contains(must), "缺「{must}」:\n{md}");
        }
        assert!(!md.contains("## Finding Details"));
    }

    // 批 4 polish：Duration <1min 应显整数秒，而非恒 "0.0 mins"/"0.8 mins" 之类无意义粒度。
    #[test]
    fn fmt_duration_sub_minute_shows_seconds() {
        assert_eq!(fmt_duration(45_000), "≈ 45 s");
        assert_eq!(fmt_duration(0), "≈ 0 s");
        assert_eq!(fmt_duration(59_999), "≈ 60 s", "四舍五入到整秒，边界值容忍显 60 s");
    }
    #[test]
    fn fmt_duration_minute_and_above_keeps_mins_grain() {
        assert_eq!(fmt_duration(60_000), "≈ 1.0 mins");
        assert_eq!(fmt_duration(528_000), "≈ 8.8 mins");
    }
    #[test]
    fn json_contains_verdict_and_provenance() {
        let s = render(&report(), Format::Json);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["verdict"], "reject");
        assert_eq!(v["findings"][0]["models"][0], "claude");
    }
    #[test]
    fn md_template_summary_and_finding_details() {
        let s = render(&report(), Format::Md);
        assert!(s.contains("❌ reject"), "verdict emoji 化: {s}");
        assert!(s.contains("a.rs L3-L5") && s.contains("### F1: boom"), "{s}");
        assert!(s.contains("Severity: 🟠 high"), "severity 在 H3 后 body 前: {s}");
        assert!(s.contains("| 🟠 high | 1 |"), "severity 计数表: {s}");
        assert!(s.contains("> **Recommendation**") && s.contains("> fix"), "{s}");
        assert!(s.contains("Redteam Review"), "H1 存在: {s}");
        assert!(!s.contains("#\n"), "job_id 空时 H1 不带悬空 #: {s}");
    }
    #[test]
    fn md_no_debug_vec_rendering() {
        let s = render(&report(), Format::Md);
        // 不含 Debug-vec 格式（如 `["claude"]`）
        assert!(!s.contains("[\""), "md output must not contain Debug-vec like [\"...\"]");
        // 含 model 名称（不带括号引号）——Found By 字段走 join(", ") 而非 Debug
        assert!(s.contains("claude"), "md output must contain model name claude");
        assert!(s.contains("reject"), "verdict must be lowercase 'reject'");
    }
    #[test]
    fn md_zero_findings_hides_table_and_details() {
        let mut r = report();
        r.findings.clear();
        r.stats = Stats::default();
        r.verdict = Verdict::Approve;
        let s = render(&r, Format::Md);
        assert!(s.contains("✅ approve"));
        assert!(s.contains("- Findings: 0"));
        assert!(!s.contains("| Severity"), "0 findings 不出计数表: {s}");
        assert!(!s.contains("## Finding Details"), "{s}");
    }
    #[test]
    fn md_info_table_models_tokens_ledger_and_jobid_title() {
        let mut r = report();
        r.run.job_id = "abc123".into();
        r.run.models = vec!["DeepSeek".into(), "Kimi".into()];
        r.run.model_ids = vec!["deepseek-v4".into(), "kimi-k2".into()];
        r.run.failed_models = vec!["Kimi".into()];
        r.run.duration_ms = 528_000;
        r.run.usage = Usage { input_tokens: 2_620_877, output_tokens: 87_187, cached_input_tokens: 1_310_439, ..Default::default() };
        r.run.ledger = Some(LedgerMeta { loaded: 53, source: ".redteam/proj.db".into() });
        r.run.warnings = vec!["w-one".into()];
        let s = render(&r, Format::Md);
        assert!(s.contains("# breakpoints Redteam Review #abc123") || s.contains("Redteam Review #abc123"), "{s}");
        assert!(s.contains("deepseek-v4, kimi-k2 ❗️fail"), "model id 显示+失败标注: {s}");
        assert!(s.contains("≈ 8.8 mins"), "{s}");
        assert!(s.contains("`2620877` (2.62M)"), "input token 自适应缩写(无裸管道符): {s}");
        assert!(s.contains("`87187` (87.19K)"), "output token 缩写: {s}");
        assert!(s.contains("53 card(s) from .redteam/proj.db"), "ledger 披露: {s}");
        assert!(s.contains("Cached Input") && s.contains("(50%)"), "缓存命中率(1310438/2620877≈50%): {s}");
        assert!(s.contains("- W1: w-one"), "{s}");
    }
    #[test]
    fn md_cluster_renders_theme_relation_note() {
        use crate::domain::finding::Cluster;
        let mut r = report();
        r.clusters = vec![Cluster {
            id: "C1".into(),
            theme: "null-deref cluster".into(),
            finding_ids: vec!["1".into()],
            relation: "overlapping".into(),
            note: "share same root cause".into(),
            severity: None,
        }];
        let s = render(&r, Format::Md);
        assert!(s.contains("null-deref cluster"), "cluster theme must appear");
        assert!(s.contains("overlapping"), "cluster relation must appear");
        assert!(s.contains("share same root cause"), "cluster note must appear");
    }
    #[test]
    fn md_marks_refuted_verify() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut r = report();
        r.findings[0].verify = Some(Verify { verdict: VerifyVerdict::Refuted, model: "gpt".into(), note: "guarded".into() });
        let s = render(&r, Format::Md);
        assert!(s.to_lowercase().contains("refuted"), "md must mark refuted findings");
    }
    #[test]
    fn md_marks_upheld_verify() {
        use crate::domain::{Verify, VerifyVerdict};
        let mut r = report();
        r.findings[0].verify = Some(Verify { verdict: VerifyVerdict::Upheld, model: "gpt".into(), note: String::new() });
        let s = render(&r, Format::Md);
        assert!(s.to_lowercase().contains("upheld"), "md must mark upheld findings");
    }
    #[test]
    fn md_incremental_rows_and_carried_origin() {
        let mut r = report();
        r.run.incremental = true;
        r.run.delta_base = Some("abc123def4567890".into());
        r.run.resolved = vec!["1111111111111111".into()];
        r.run.resolution_disputed = vec!["2222222222222222".into()];
        r.run.carried_dropped = vec!["3333333333333333".into()];
        r.findings[0].origin = Origin::Carried;
        let s = render(&r, Format::Md);
        assert!(s.contains("| Incremental") && s.contains("abc123def456"), "delta base 短 sha: {s}");
        assert!(s.contains("| Carried / New | 1 / 0 |"), "{s}");
        assert!(s.contains("Carry Dropped") && s.contains("3333333333333333"), "{s}");
        assert!(s.contains("Resolved") && s.contains("1111111111111111"), "{s}");
        assert!(s.contains("Disputed") && s.contains("2222222222222222"), "disputed 是 ship 高信号必须在场: {s}");
        assert!(s.contains("| Origin     | carried"), "携带项标记: {s}");
    }

    #[test]
    fn md_non_incremental_stays_clean() {
        let s = render(&report(), Format::Md);
        assert!(!s.contains("Incremental") && !s.contains("| Origin"), "全量报告零增量噪音: {s}");
    }
}
