//! reviewer 单发通路（M21b 批 C-3）：旁路落盘 + 终判权威 + FindingSink。
//! 与 `scan_shot` 同构两轮（turn → parse → 失败则 REPAIR + turn_final）。
//! 批 D 合回前 `sink = &()` no-op，不建 mpsc。

use std::collections::HashSet;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::domain::{CellOutput, Finding};
use crate::orchestrate::enrich;
use crate::positioning::anchor_review_findings;
use crate::prompt::ScanPrompt;
use crate::provider::parse::{
    diagnostic_parse_error, dump_parse_failure, parse_review_output_with_stats, ReviewParseStats, REPAIR_PROMPT_REVIEW,
};
use crate::provider::stream_json::IncrementalFindings;
use crate::provider::tool_loop::{ChatBackend, ChatMessage};
use crate::provider::{timed_turn_final_with, timed_turn_with, AttemptPolicy, ProviderError};

/// 与批 D 的唯一事件（不泄漏 QueueItem）。
#[derive(Clone, Debug)]
pub struct FindingEvent {
    pub finding: Finding,
    /// 区域被几个 scanner 家族标记过——批 D 跳过复核的判据，**不是**认同这条 finding 的模型数
    /// （那是 `Finding.agreement`）。**取值时点 = 派发时快照**：事件在 cell 运行中（旁路）或刚结束
    /// （终判）发出，那时碰不到队列。落盘 finding 另由 `run_deep` 完成时按落地活值回填，**两者分叉时以落盘为准**。
    ///
    /// **不变式 `event ≤ landed`**（批 C1 并发派发后，在途并入会让两者分叉）：队列项 `families` 只增不减 →
    /// agreement 单调不减 → 派发快照不大于落地值。批 D「≥ 2 免复核」读的是较小的事件值，偏差方向只会是
    /// **多复核**、不会**漏复核**——安全方向，故接受滞后（2026-09-13 裁定）。
    pub suspicion_agreement: u32,
    pub reviewer_family: String,
}

/// 缺省 no-op，避免无消费者堵住派发器。
pub trait FindingSink: Send + Sync {
    fn on_finding(&self, ev: FindingEvent);
}

impl FindingSink for () {
    fn on_finding(&self, _ev: FindingEvent) {}
}

/// 发射上下文：旁路与终判共用的身份 / 去重面（一处收拢，免两个函数各带 5-6 个同源参数）。
pub(crate) struct EmitCtx<'a> {
    model: &'a str,
    family: &'a str,
    /// 队列项**派发时**的疑点共识快照——盖到事件与 cell 内产出的 `Finding.suspicion_agreement`。
    /// 不是落地值：本上下文跑在 await 里读不到队列；落地值由 `run_deep` 在主循环回填落盘 finding。
    /// **不碰** `Finding.agreement`：那是认同这条 finding 的模型数，由 `dedup` 算定。
    ///
    /// 批 D〔D17〕改为**项内**轴（`peak_agreement`）而非区域级 `agreement`，且只盖给压在 `hot_ranges` 上的 finding：
    /// §8.1 的跳过规则要的是「这条 finding 所在的那几行被两个家族独立标过」，不是「这一整块里有两个家族来过」。
    /// C3 把项放大成整个 def / 标题节之后，按格盖章会把 reviewer 自己新发现的另一处也送进免验集。
    suspicion_agreement: u32,
    /// 达成共识的那一簇疑点覆盖的行区间（`QueueItem::hot_ranges`）；空 = 一律不盖。
    hot_ranges: Vec<(u32, u32)>,
    /// 本队列项的文件——热区是**这个文件**里的行区间。reviewer 可以合法地报出别的文件
    /// （跨组索引 / 二阶段补查），只比行号会让它白拿共识盖章（收口评审 #2）。
    item_file: &'a str,
    root: &'a Path,
    sink: &'a dyn FindingSink,
    /// 已发射 finding.id——旁路发过的，终判不补发。
    emitted: HashSet<String>,
}

impl<'a> EmitCtx<'a> {
    pub(crate) fn new(
        model: &'a str,
        family: &'a str,
        suspicion_agreement: u32,
        hot_ranges: Vec<(u32, u32)>,
        item_file: &'a str,
        root: &'a Path,
        sink: &'a dyn FindingSink,
    ) -> Self {
        EmitCtx { model, family, suspicion_agreement, hot_ranges, item_file, root, sink, emitted: HashSet::new() }
    }

    /// 这条 finding 该盖多少共识（热区门控的**单一出口**）：压在峰值簇那几行上才盖，否则 0。
    /// 收口评审 #14：`Finding.suspicion_agreement` 曾在调用点直接写 `ctx.suspicion_agreement`（没过门控），
    /// 与同一事件里 `FindingEvent.suspicion_agreement` 打架——而 `decide` 读的是 Finding 上那份。
    fn consensus_for(&self, f: &Finding) -> u32 {
        if crate::orchestrate::queue::in_hot_ranges(&f.file, self.item_file, f.line_start, f.line_end, &self.hot_ranges) {
            self.suspicion_agreement
        } else {
            0
        }
    }

    /// 盖章 + 发射：两件事必须一起做，否则 Finding 与事件两份值会各说各话。
    fn stamp_and_emit(&mut self, f: &mut Finding) {
        f.suspicion_agreement = self.consensus_for(f);
        self.emit(f);
    }

    /// 首次见到该 id 才发射（旁路 / 终判同一去重面）。
    fn emit(&mut self, f: &Finding) {
        if !self.emitted.insert(f.id.clone()) {
            return;
        }
        self.sink.on_finding(FindingEvent {
            finding: f.clone(),
            suspicion_agreement: f.suspicion_agreement,
            reviewer_family: self.family.to_string(),
        });
    }
}

/// 两轮累计用量（参数面收拢）。
#[derive(Clone, Copy, Default)]
struct TurnUsage {
    input: u64,
    output: u64,
    cached: u64,
    reasoning: Option<u64>,
    /// 这次单发里**单轮**最大的输出（首轮与 repair 轮取 max）——`output` 是两轮之和，
    /// 拿它对比每轮都生效的 `max_tokens` 没有意义（M21b 出口验收 D3）。
    max_turn: u64,
}

/// 一次 reviewer 调用成品（对齐 ScanCall + stream_discarded）。
#[derive(Clone, Debug)]
pub struct ReviewCall {
    pub output: CellOutput,
    pub stats: ReviewParseStats,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub iterations: u32,
    /// 其中**修复轮**（schema 批 B-4）。二阶段合并时两轮的修复轮都累加进来。
    pub repair_rounds: u32,
    /// 二阶段补查的轮次（schema 批 B-4）。**不再混进 `iterations`**——原来
    /// `c.iterations += c2.iterations` 把两件事揉成一个数，于是「这格跑了多轮」
    /// 分不出是探索深、修过错、还是声明了 needs。
    pub two_stage_rounds: u32,
    /// 单轮最大输出 token（首轮 / repair 轮 / 二阶段轮取 max；见 `TurnUsage::max_turn`）。
    pub max_turn_output_tokens: u64,
    pub finish_reason: Option<String>,
    pub reasoning_tokens: Option<u64>,
    pub repair_note: Option<String>,
    pub rate_limit_retries: u32,
    pub rate_limit_wait_ms: u64,
}

fn sum_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (x, y) => Some(x.unwrap_or(0) + y.unwrap_or(0)),
    }
}

/// 旁路发射与终判（`finish_parse`）走**同一道 quote 锚定门**：没锚上的不发。
/// 旧实现旁路直接 `enrich` 就发——批 C 的 sink 是 no-op 所以无害，但批 D 一接真 sink，
/// 未经 quote 校验的 finding 会先于终判漏进复核流（终判那边会丢弃，事件却已发出、收不回）。
/// 锚定只改行号不改 `id`（id 由 enrich 按 quote 算定），所以与终判按 id 去重仍然对得上；
/// 旁路丢弃的计数不单记——终判 parse 全文时 `dropped_quote` 会把同一批再算一遍。
fn emit_raw(raws: Vec<crate::domain::RawFinding>, ctx: &mut EmitCtx) {
    if raws.is_empty() {
        return;
    }
    let enriched: Vec<Finding> = raws
        .iter()
        .map(|raw| {
            let mut f = enrich(raw, ctx.model);
            f.models = vec![ctx.model.to_string()];
            f
        })
        .collect();
    let (kept, _dropped, _unreadable) = anchor_review_findings(enriched, ctx.root);
    for mut f in kept {
        ctx.stamp_and_emit(&mut f);
    }
}

/// 轮间旁路：发已闭合对象，**不** finalize——后续轮次还要继续 feed 同一个解析器。
fn drain_bypass(inc: &Arc<Mutex<IncrementalFindings>>, ctx: &mut EmitCtx) {
    let raws = inc.lock().unwrap().take();
    emit_raw(raws, ctx);
}

/// 错误路径收口（超时 / 网络失败 / 取消）：先发已闭合对象（不丢），再 `finalize`
/// ——未闭合片段计入 `discarded` 并清缓冲，不静默丢。PRD「必调 finalize」的失败半边。
fn close_bypass(inc: &Arc<Mutex<IncrementalFindings>>, ctx: &mut EmitCtx) {
    drain_bypass(inc, ctx);
    inc.lock().unwrap().finalize();
}

/// reviewer 两轮单发。`inc` 由调用方挂到 per-cell backend 的 `with_text_delta` 后再传入。
pub(crate) async fn run_review_shot_with(
    backend: &dyn ChatBackend,
    prompt: &ScanPrompt,
    per_turn: std::time::Duration,
    retry_budget: u32,
    policy: Option<&dyn AttemptPolicy>,
    ctx: &mut EmitCtx<'_>,
    inc: Arc<Mutex<IncrementalFindings>>,
) -> Result<ReviewCall, ProviderError> {
    let mut messages = vec![
        ChatMessage::System(prompt.system.clone()),
        ChatMessage::UserBlocks(prompt.blocks.clone()),
    ];

    // 每次 HTTP 尝试开始复位旁路解析器（批 D-1）：重试复用同一 backend 与同一 `inc`，
    // 不复位则断流那次的半截 JSON 会与重试输出拼成一个四不像对象发给 sink。
    // 只在有内层策略时包装——`None` 的语义（通用退避 + budget 约束）不能被凭空多出的 `on_rate_limited` 改掉。
    let reset_guard = policy.map(|p| crate::provider::ResetOnAttempt::new(p, inc.clone()));
    let policy: Option<&dyn AttemptPolicy> = match &reset_guard {
        Some(g) => Some(g),
        None => None,
    };

    let turn = match timed_turn_with(backend, &messages, &[], per_turn, retry_budget, policy).await {
        Ok(t) => t,
        Err(e) => {
            close_bypass(&inc, ctx);
            return Err(e);
        }
    };
    drain_bypass(&inc, ctx);

    let mut usage = TurnUsage {
        input: turn.input_tokens,
        output: turn.output_tokens,
        cached: turn.cached_input_tokens,
        reasoning: turn.reasoning_tokens,
        max_turn: turn.output_tokens,
    };

    if let Ok(call) = finish_parse(&turn.text, ctx, &inc, 1, turn.finish_reason.clone(), None, usage) {
        return Ok(call);
    }

    dump_parse_failure(&format!("review {} {} t1", ctx.item_file, ctx.model), &turn.text);
    let head: String = turn.text.trim().chars().take(80).collect();
    let repair_note = format!(
        "first turn parse failed (finish={}, text_len={}, head={:?}); repaired",
        turn.finish_reason.as_deref().unwrap_or("none"),
        turn.text.len(),
        head
    );
    if !turn.text.trim().is_empty() {
        messages.push(ChatMessage::Assistant { text: turn.text.clone(), tool_calls: vec![] });
    }
    messages.push(ChatMessage::User(REPAIR_PROMPT_REVIEW.to_string()));
    let repair = match timed_turn_final_with(backend, &messages, per_turn, retry_budget, policy).await {
        Ok(r) => r,
        Err(e) => {
            close_bypass(&inc, ctx);
            return Err(e.attach_usage((usage.input, usage.output, usage.cached)));
        }
    };
    drain_bypass(&inc, ctx);
    usage.input += repair.input_tokens;
    usage.output += repair.output_tokens;
    usage.cached += repair.cached_input_tokens;
    usage.reasoning = sum_opt(usage.reasoning, repair.reasoning_tokens);
    usage.max_turn = usage.max_turn.max(repair.output_tokens);

    match finish_parse(
        &repair.text, ctx, &inc, 2, repair.finish_reason.clone(), Some(repair_note), usage,
    ) {
        Ok(call) => Ok(call),
        Err(_) => {
            dump_parse_failure(&format!("review {} {} t2", ctx.item_file, ctx.model), &repair.text);
            inc.lock().unwrap().finalize();
            Err(diagnostic_parse_error(&repair).attach_usage((usage.input, usage.output, usage.cached)))
        }
    }
}

fn finish_parse(
    text: &str,
    ctx: &mut EmitCtx,
    inc: &Arc<Mutex<IncrementalFindings>>,
    iterations: u32,
    finish_reason: Option<String>,
    repair_note: Option<String>,
    usage: TurnUsage,
) -> Result<ReviewCall, ProviderError> {
    let (out, mut stats) = parse_review_output_with_stats(text, ctx.model)?;
    let CellOutput::Review { findings, needs } = out else {
        return Err(ProviderError::Parse("review parse did not yield Review".into()));
    };
    let (kept, dropped, dropped_unreadable) = anchor_review_findings(findings, ctx.root);
    stats.dropped_quote = dropped + dropped_unreadable;

    let mut final_findings = Vec::new();
    for mut f in kept {
        ctx.stamp_and_emit(&mut f);
        final_findings.push(f);
    }

    {
        let mut g = inc.lock().unwrap();
        g.finalize();
        stats.stream_discarded = g.discarded();
    }

    Ok(ReviewCall {
        output: CellOutput::Review { findings: final_findings, needs },
        stats,
        input_tokens: usage.input,
        output_tokens: usage.output,
        cached_input_tokens: usage.cached,
        iterations,
        // review 路径的 `iterations` 恒为 1（首轮即成）或 2（修复后成）——两个调用点就这两种，
        // 所以减 1 就是修复轮数，不是估算。
        repair_rounds: iterations.saturating_sub(1),
        // 二阶段在 `deep.rs` 合并两次调用时才产生，单次 shot 恒 0。
        two_stage_rounds: 0,
        max_turn_output_tokens: usage.max_turn,
        finish_reason,
        reasoning_tokens: usage.reasoning,
        repair_note,
        rate_limit_retries: 0,
        rate_limit_wait_ms: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrate::finding_id;
    use crate::prompt::{PromptBlock, ScanPrompt};
    use crate::provider::parse::parse_review_output_with_stats;
    use crate::provider::tool_loop::{AssistantTurn, FakeChatBackend};
    use std::time::Duration;

    const TWO: &str = r#"{"findings":[
        {"severity":"low","title":"t1","body":"b1","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r1","quote":"fn a() {"},
        {"severity":"medium","title":"t2","body":"b2","file":"a.rs","line_start":2,"line_end":2,"confidence":0.6,"recommendation":"r2","quote":"fn b() {"}
    ],"needs":[]}"#;

    fn turn(text: &str) -> AssistantTurn {
        AssistantTurn {
            text: text.into(),
            tool_calls: vec![],
            input_tokens: 10,
            output_tokens: 4,
            cached_input_tokens: 0,
            finish_reason: Some("stop".into()),
            reasoning_tokens: None,
        }
    }

    fn prompt() -> ScanPrompt {
        ScanPrompt {
            system: "<role>head</role>".into(),
            blocks: vec![PromptBlock { text: "<reviewer>t</reviewer>".into(), cache_boundary: false }],
        }
    }

    struct Rec(Mutex<Vec<FindingEvent>>);
    impl FindingSink for Rec {
        fn on_finding(&self, ev: FindingEvent) {
            self.0.lock().unwrap().push(ev);
        }
    }

    fn tmp_file() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("a.rs"), "fn a() {\nfn b() {\n").unwrap();
        d
    }

    /// 无门场景的最小策略（`ResetOnAttempt` 只在有内层策略时安装，故本用例要有一个）。
    struct NoopPolicy;
    #[async_trait::async_trait]
    impl AttemptPolicy for NoopPolicy {
        fn attempt_begin(&self) {}
        fn attempt_end(&self, _o: crate::provider::AttemptOutcome) {}
        async fn on_rate_limited(&self, _n: u32, _a: Option<Duration>) -> bool {
            false
        }
    }

    /// 断流重试后旁路仍然有效（批 D-1，followup「接真 sink 前须处理」）：
    /// 首试流到一半断了、重试轮流出一条**完整**对象后整格失败——这正是 D10 立旁路要保住的场景
    /// （格中途死亡，已闭合对象不丢）。不复位时半截对象会把重试轮的流整个吞进去、永不闭合，
    /// 旁路无声作废（实测 `emitted=[]`、`discarded=0`），sink 一条都收不到。
    #[tokio::test]
    async fn bypass_still_delivers_after_a_cut_stream_retry() {
        let dir = tmp_file();
        let rec = Rec(Mutex::new(Vec::new()));
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let inc2 = inc.clone();
        let half = r#"{"findings":[{"severity":"high","title":"draft","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r","quote":"fn a() {"#;
        // 重试轮：一条完整对象（数组未闭合——格就死在这儿）
        let closed = r#"{"findings":[{"severity":"medium","title":"t9","body":"b9","file":"a.rs","line_start":2,"line_end":2,"confidence":0.7,"recommendation":"r9","quote":"fn b() {"},"#;
        let backend = FakeChatBackend::new_results(vec![
            Err(crate::provider::ProviderError::Unavailable("cut 1".into())),
            Err(crate::provider::ProviderError::Unavailable("cut 2".into())),
        ])
        .with_err_partials(vec![half, closed])
        .with_text_delta(Box::new(move |s: &str| inc2.lock().unwrap().feed(s)));
        let mut ctx = EmitCtx::new("m", "alpha", 1, vec![(1, 9)], "a.rs", dir.path(), &rec);

        let err = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 1, Some(&NoopPolicy), &mut ctx, inc,
        )
        .await;

        assert!(err.is_err(), "两轮都失败，格作废");
        let titles: Vec<String> = rec.0.lock().unwrap().iter().map(|e| e.finding.title.clone()).collect();
        assert_eq!(titles, ["t9"], "重试轮已闭合的那条经旁路留下来了（不复位时这里是空）");
    }

    /// 出口验收诊断接线：首轮、修复轮都解析失败 → 两轮原文各落一份（t1 / t2），且整段不截断。
    /// 进程级环境变量：持 `ENV_LOCK` 串行化；并行的其他用例若恰好也解析失败会写进同一目录——
    /// 按本用例独有的 model 名筛，互不干扰；先 remove_var 再让 tempdir 析构，窗口只有本用例的几毫秒。
    // ENV_LOCK 是**测试专用**的 env 串行器（仓内约定，见 config::ENV_LOCK 的用法）：
    // 跨 await 持它是刻意的——保护窗口必须覆盖到读 env 的那段异步调用，否则别的用例的 set_var 仍构成竞争。
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn parse_failures_of_both_turns_are_dumped_when_dir_is_set() {
        let _g = crate::config::ENV_LOCK.lock().unwrap();
        let dump = tempfile::tempdir().unwrap();
        // SAFETY: ENV_LOCK 串行化（仓内既定约定；set_var 与**任意**线程的 env 访问都是竞争，
        // 不只是读同一个键——收口评审 #4）
        unsafe { std::env::set_var("REDTEAM_DUMP_DIR", dump.path()); }
        let dir = tmp_file();
        let rec = Rec(Mutex::new(Vec::new()));
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let bad1 = format!("{{\"findings\":[{{\"quote\":\"{}\\x00\"}}]}}", "q".repeat(300));
        let bad2 = "{\"findings\": oops}";
        let backend = FakeChatBackend::new(vec![turn(&bad1), turn(bad2)]);
        let mut ctx = EmitCtx::new("dump-probe-model", "alpha", 1, vec![], "svc/a.go", dir.path(), &rec);
        let err = run_review_shot_with(&backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc).await;
        unsafe { std::env::remove_var("REDTEAM_DUMP_DIR"); }

        assert!(err.unwrap_err().to_string().contains("serde:"), "终判错误带 serde 根因");
        let mut mine: Vec<(String, String)> = std::fs::read_dir(dump.path())
            .unwrap()
            .filter_map(|e| {
                let p = e.ok()?.path();
                let name = p.file_name()?.to_string_lossy().into_owned();
                name.contains("dump-probe-model").then(|| (name, std::fs::read_to_string(&p).unwrap()))
            })
            .collect();
        mine.sort();
        assert_eq!(mine.len(), 2, "t1 + t2 各一份: {:?}", mine.iter().map(|m| &m.0).collect::<Vec<_>>());
        assert!(mine.iter().any(|(n, b)| n.ends_with("_t1.txt") && b.ends_with(&bad1)), "首轮原文整段落盘");
        assert!(mine.iter().any(|(n, b)| n.ends_with("_t2.txt") && b.ends_with(bad2)), "修复轮原文落盘");
        assert!(mine.iter().all(|(n, _)| n.contains("svc_a.go")), "标签带队列项文件");
    }

    /// 出口验收金丝雀定案的端到端：首轮 quote 含裸 tab → 不走修复轮、首轮即成，且逐字锚定到 tab 缩进的源码行。
    #[tokio::test]
    async fn raw_tab_quote_parses_first_turn_and_anchors_to_tabbed_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "func a() {\n\tx := 1\n\treturn\n}\n").unwrap();
        let rec = Rec(Mutex::new(Vec::new()));
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let text = "{\"findings\":[{\"severity\":\"high\",\"title\":\"t\",\"body\":\"b\",\"file\":\"a.go\",\"line_start\":2,\
                    \"line_end\":3,\"confidence\":0.7,\"recommendation\":\"r\",\"quote\":\"\tx := 1\\n\treturn\"}],\"needs\":[]}";
        let backend = FakeChatBackend::new(vec![turn(text)]);
        let mut ctx = EmitCtx::new("m", "alpha", 1, vec![], "a.go", dir.path(), &rec);
        let call = run_review_shot_with(&backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc).await.unwrap();
        assert_eq!(call.iterations, 1, "首轮即成，不烧修复轮");
        let CellOutput::Review { findings, .. } = &call.output else { panic!() };
        assert_eq!(findings.len(), 1, "锚定保留（quote 解出的真 tab 与源码逐字一致）");
        assert_eq!((findings[0].line_start, findings[0].line_end), (2, 3));
        assert_eq!(call.stats.dropped_quote, 0);
    }

    #[tokio::test]
    async fn stream_then_final_does_not_double_emit() {
        let dir = tmp_file();
        let rec = Rec(Mutex::new(Vec::new()));
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let inc2 = inc.clone();
        let backend = FakeChatBackend::new(vec![turn(TWO)])
            .with_delta_chunk(11)
            .with_text_delta(Box::new(move |s: &str| inc2.lock().unwrap().feed(s)));
        let mut ctx = EmitCtx::new("m", "alpha", 2, vec![(1, 9)], "a.rs", dir.path(), &rec);
        let call = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc,
        )
        .await
        .unwrap();
        let events = rec.0.lock().unwrap();
        assert_eq!(events.len(), 2, "两条各发一次");
        assert!(events.iter().all(|e| e.suspicion_agreement == 2 && e.reviewer_family == "alpha"));
        let CellOutput::Review { findings, .. } = &call.output else { panic!() };
        assert_eq!(findings.len(), 2);
        // 队列共识落在 suspicion_agreement；agreement 仍是「认同这条 finding 的模型数」，
        // 由 enrich 起手 1、dedup 按 models.len() 算定——深挖不得代笔（design §8.1 两条规则各读各的轴）。
        assert_eq!(findings[0].suspicion_agreement, 2);
        assert_eq!(findings[0].agreement, 1, "单 reviewer 产出 → 模型数 1，不被疑点共识顶高");
        assert_eq!(call.stats.stream_discarded, 0);
        assert_eq!(call.iterations, 1);
        let ids: HashSet<_> = events.iter().map(|e| e.finding.id.clone()).collect();
        assert_eq!(ids.len(), 2);
    }

    /// 旁路发射必须过 quote 锚定门（2026-09-12 dogfood reviewer finding，high）：
    /// quote 在文件里找不到的「幽灵 finding」一条事件都不能发——终判会丢它，但旁路先发的事件收不回。
    /// 走真旁路：小块 delta + 回调 feed，`drain_bypass` 先于 `finish_parse` 发射。
    #[tokio::test]
    async fn bypass_does_not_emit_unanchored_findings() {
        let dir = tmp_file(); // a.rs = "fn a() {\nfn b() {\n"
        let rec = Rec(Mutex::new(Vec::new()));
        let with_ghost = r#"{"findings":[
            {"severity":"low","title":"real","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"fn a() {"},
            {"severity":"high","title":"ghost","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r","quote":"this line is nowhere in the file"}
        ],"needs":[]}"#;
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let inc2 = inc.clone();
        let backend = FakeChatBackend::new(vec![turn(with_ghost)])
            .with_delta_chunk(9)
            .with_text_delta(Box::new(move |s: &str| inc2.lock().unwrap().feed(s)));
        let mut ctx = EmitCtx::new("m", "alpha", 1, vec![(1, 9)], "a.rs", dir.path(), &rec);
        let call = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc,
        )
        .await
        .unwrap();
        let events = rec.0.lock().unwrap();
        assert!(
            events.iter().all(|e| e.finding.title != "ghost"),
            "锚不上的 finding 不得经旁路发射: {:?}",
            events.iter().map(|e| &e.finding.title).collect::<Vec<_>>()
        );
        assert_eq!(events.len(), 1, "只发锚得上的那条");
        let CellOutput::Review { findings, .. } = &call.output else { panic!() };
        assert_eq!(findings.len(), 1, "终判同样丢弃幽灵");
        assert_eq!(call.stats.dropped_quote, 1, "丢弃计数由终判记一次，旁路不重复记");
    }

    #[tokio::test]
    async fn unit_sink_does_not_block() {
        let dir = tmp_file();
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let backend = FakeChatBackend::new(vec![turn(TWO)]);
        let mut ctx = EmitCtx::new("m", "alpha", 1, vec![(1, 9)], "a.rs", dir.path(), &());
        let call = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc,
        )
        .await
        .unwrap();
        let CellOutput::Review { findings, .. } = call.output else { panic!() };
        assert_eq!(findings.len(), 2);
    }

    #[tokio::test]
    async fn truncated_finalizes_and_keeps_closed() {
        let dir = tmp_file();
        let rec = Rec(Mutex::new(Vec::new()));
        let truncated = &TWO[..TWO.len() - 40];
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let inc2 = inc.clone();
        let backend = FakeChatBackend::new(vec![turn(truncated), turn(truncated)])
            .with_delta_chunk(8)
            .with_text_delta(Box::new(move |s: &str| inc2.lock().unwrap().feed(s)));
        let mut ctx = EmitCtx::new("m", "alpha", 1, vec![(1, 9)], "a.rs", dir.path(), &rec);
        let err = run_review_shot_with(
            &backend, &prompt(), Duration::from_secs(5), 0, None, &mut ctx, inc.clone(),
        )
        .await
        .unwrap_err();
        let msg = format!("{err:?}");
        assert!(msg.contains("Parse") || msg.contains("parse"), "{msg}");
        assert!(!rec.0.lock().unwrap().is_empty(), "已闭合对象已发射");
        assert!(inc.lock().unwrap().discarded() >= 1, "两轮都 parse 失败——终态路径已 finalize");
    }

    /// AC：超时 / 网络失败路径也必调 `finalize`——未闭合片段计入 `discarded`，不静默丢。
    #[tokio::test]
    async fn error_path_finalizes_bypass() {
        let dir = tmp_file();
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        // 流到一半（对象未闭合）即超时。
        inc.lock().unwrap().feed(r#"{"findings":[{"severity":"low","title":"half"#);
        let backend = FakeChatBackend::new_results(vec![Err(ProviderError::Timeout("probe".into()))]);
        let mut ctx = EmitCtx::new("m", "alpha", 1, vec![(1, 9)], "a.rs", dir.path(), &());
        let err = run_review_shot_with(
            &backend, &prompt(), Duration::from_millis(50), 0, None, &mut ctx, inc.clone(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ProviderError::Timeout(_)), "{err:?}");
        assert_eq!(inc.lock().unwrap().discarded(), 1, "未闭合片段经 finalize 计数");
    }

    /// REPAIR 轮失败（第二轮 turn 直接 Err）同样收口——错误半边两条路径都不漏 finalize。
    /// M21b 出口验收 D3：`max_turn_output_tokens` 是**单轮**最大、`output_tokens` 是两轮之和。
    /// 输出上限逐轮生效，拿两轮之和去比上限会把「两轮都很安全」误判成「撞帽」（dogfood 里 55.6k 那格就是这么来的）。
    #[tokio::test]
    async fn max_turn_output_is_the_largest_single_turn_not_the_sum() {
        let dir = tmp_file();
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let mut bad = turn(r#"{"findings":[{"severity":"low","title":"half"#); // 首轮截断成半截 JSON → 走 repair
        bad.output_tokens = 300;
        let mut good = turn(TWO);
        good.output_tokens = 120;
        let backend = FakeChatBackend::new_results(vec![Ok(bad), Ok(good)]);
        let mut ctx = EmitCtx::new("m", "alpha", 1, vec![(1, 9)], "a.rs", dir.path(), &());
        let call = run_review_shot_with(&backend, &prompt(), Duration::from_millis(50), 0, None, &mut ctx, inc)
            .await
            .unwrap();
        assert_eq!(call.iterations, 2, "前提：走了 repair");
        assert_eq!(call.output_tokens, 420, "总量 = 两轮之和");
        assert_eq!(call.max_turn_output_tokens, 300, "单轮最大 = 首轮那 300，不是 420");
    }

    #[tokio::test]
    async fn repair_turn_error_finalizes_bypass() {
        let dir = tmp_file();
        let inc = Arc::new(Mutex::new(IncrementalFindings::new()));
        let inc2 = inc.clone();
        let backend = FakeChatBackend::new_results(vec![
            Ok(turn(r#"{"findings":[{"severity":"low","title":"half"#)),
            Err(ProviderError::Timeout("repair".into())),
        ])
        .with_delta_chunk(8)
        .with_text_delta(Box::new(move |s: &str| inc2.lock().unwrap().feed(s)));
        let mut ctx = EmitCtx::new("m", "alpha", 1, vec![(1, 9)], "a.rs", dir.path(), &());
        let err = run_review_shot_with(
            &backend, &prompt(), Duration::from_millis(50), 0, None, &mut ctx, inc.clone(),
        )
        .await
        .unwrap_err();
        // REPAIR 轮的错误带首轮用量回传（WithUsage 包一层），断言内层成因。
        let msg = format!("{err:?}");
        assert!(msg.contains("Timeout"), "{msg}");
        assert_eq!(inc.lock().unwrap().discarded(), 1, "REPAIR 轮失败也 finalize");
    }

    #[test]
    fn finding_id_matches_enrich() {
        let (out, _) = parse_review_output_with_stats(TWO, "m").unwrap();
        let CellOutput::Review { findings, .. } = out else { panic!() };
        let id = finding_id(
            &findings[0].file, &findings[0].quote, &findings[0].title,
            findings[0].line_start, findings[0].line_end,
        );
        assert_eq!(id, findings[0].id);
    }
}
