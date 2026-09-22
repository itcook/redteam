//! SSE 流式旁路（M21b 批 B-4，design §7〔D10-1〕）：`findings` 数组的增量 JSON 解析——
//! 对象逐个闭合即 `emit(RawFinding)`，未闭合丢弃计数。**持久化旁路，不改契约**：FINALIZE / REPAIR
//! 仍管最终整体解析（`parse_review_output`），本机器的价值在 cell 中途死亡时已闭合对象不丢。
//! 无旋钮（不加 submit 类配置）；非 findings 段 / 思考文本容忍。
//! 字段规整与终态 parse 同源（`parse::regularize_finding` 单一真相源）——旁路 emit 的条目
//! 与终态 parse 保留的条目语义一致。

use crate::domain::RawFinding;

use super::parse::{regularize_finding, DropReason};

/// seek 阶段滑窗：`"findings"` 键最长 10 字节——窗口外的旧文本不可能再参与匹配，丢弃防长 think 撑爆内存。
const SEEK_WINDOW: usize = 64 * 1024;

/// 增量 findings 解析器：`feed` 喂文本增量（SSE text_delta 原生顺序），`take` 取出已闭合对象，
/// `discarded` 读未闭合 / 坏形状丢弃计数。状态机三阶段：seek `"findings": [` → 数组内字符串感知
/// 配平（对象闭合即规整）→ `]` 后停止消费（后续 delta 忽略）。
#[derive(Default)]
pub struct IncrementalFindings {
    /// 未消费缓冲（跨片拼接；seek 阶段滑窗截断）。
    buf: String,
    /// 已进入 findings 数组。
    in_array: bool,
    /// 数组已闭合（`]` 到达）——后续 feed 全部忽略。
    done: bool,
    emitted: Vec<RawFinding>,
    discarded: u32,
    /// 配平状态（in_array 内）：对象深度 / 字符串内 / 转义。
    depth: i32,
    in_string: bool,
    escaped: bool,
    /// 当前对象的起始（buf 内字节偏移；对象跨 feed 边界时缓冲保留）。
    obj_start: Option<usize>,
    /// 数组内扫描游标（跨 feed 持久——in_string/escaped 是游标处状态，从头重扫会错位）。
    scan_pos: usize,
}

impl IncrementalFindings {
    pub fn new() -> Self {
        Self::default()
    }

    /// 喂一段文本增量。只推进状态机，不返回值——emit 的对象经 `take` 取。
    pub fn feed(&mut self, delta: &str) {
        if self.done {
            return;
        }
        self.buf.push_str(delta);
        self.pump();
    }

    /// 取出至今已闭合的对象（清空 emit 队列）。
    pub fn take(&mut self) -> Vec<RawFinding> {
        std::mem::take(&mut self.emitted)
    }

    /// 丢弃计数：未闭合（截断）+ 闭合但形状不可救（regularize 失败）。
    pub fn discarded(&self) -> u32 {
        self.discarded
    }

    /// 数组是否已闭合（`]` 到达——之后 feed 不再消费）。
    pub fn done(&self) -> bool {
        self.done
    }

    /// 流结束（截断 / cell 死亡收尾）时调用：未闭合对象（`depth > 0` 或顶层字符串中）计入 `discarded`
    /// 并清缓冲——「截断 → 已闭合保留、余下 discard 计数」的兑现点（批 C 在旁路消费结束处调用）。
    pub fn finalize(&mut self) {
        if !self.done && (self.depth > 0 || self.in_string) {
            self.discarded += 1;
        }
        self.buf.clear();
        self.scan_pos = 0;
        self.depth = 0;
        self.in_string = false;
        self.escaped = false;
        self.obj_start = None;
    }

    /// 一次 HTTP 尝试开始时复位（M21b 批 D-1）：重试复用同一个解析器，断流那次的半截 JSON 必须丢掉——
    /// 否则它会与重试轮的输出拼成一个四不像对象，接真 sink 后就发出去了。
    ///
    /// **保留 `emitted` 与 `discarded`**：已闭合的对象是 D10 立旁路的全部价值（格中途死亡也不丢），
    /// 重复对象由 `EmitCtx` 的 id 去重挡住；丢弃计数是跨尝试累计的观测量。
    /// 比 [`Self::finalize`] 多清 `in_array` / `done`：新一轮的流从头开始，得重走 seek 阶段。
    pub fn reset_for_attempt(&mut self) {
        self.finalize();
        self.in_array = false;
        self.done = false;
    }

    /// 推进状态机直到需要更多字节。
    fn pump(&mut self) {
        loop {
            if !self.in_array && !self.seek_array() {
                return;
            }
            if !self.scan_array() {
                return;
            }
        }
    }

    /// seek 阶段：找 `"findings"` 键后跟 `:` `[`（容忍空白）。找到 → 消费到 `[` 之后、进入数组扫描；
    /// 未找到 → 滑窗截断（保留尾部）返回 false。
    fn seek_array(&mut self) -> bool {
        let bytes = self.buf.as_bytes();
        let needle = "\"findings\"";
        let mut from = 0usize;
        while let Some(rel) = self.buf[from..].find(needle) {
            let key_at = from + rel;
            // 键后到 '[' 之间只允许空白与 ':'
            let mut i = key_at + needle.len();
            let mut saw_colon = false;
            while i < bytes.len() {
                match bytes[i] {
                    b' ' | b'\t' | b'\r' | b'\n' => i += 1,
                    b':' if !saw_colon => {
                        saw_colon = true;
                        i += 1;
                    }
                    b'[' if saw_colon => {
                        // 进入数组：丢弃 '[' 之前的全部缓冲
                        self.buf.drain(..i + 1);
                        self.in_array = true;
                        return true;
                    }
                    _ => break, // 伪匹配（如 "my_findings"）——从下一个位置重找
                }
            }
            from = key_at + needle.len();
        }
        // 未找到：滑窗（保留尾部，防下一段把键劈开；cut 回退到 char boundary——多字节字符中间 drain 会 panic）
        if self.buf.len() > SEEK_WINDOW {
            let mut cut = self.buf.len() - SEEK_WINDOW;
            while cut > 0 && !self.buf.is_char_boundary(cut) {
                cut -= 1;
            }
            self.buf.drain(..cut);
        }
        false
    }

    /// 数组内扫描（游标 `scan_pos` 跨 feed 持久——`in_string` / `escaped` 是游标处状态，从头重扫会错位）：
    /// 顶层只等 `{`（对象开）或 `]`（数组闭）；对象闭合（深度回 0）→ `close_object` → 缓冲前移、游标归零重扫。
    /// 返回 true = 消费了一个对象（pump 继续）；false = 需要更多字节或 done。
    fn scan_array(&mut self) -> bool {
        let mut closed: Option<(usize, usize)> = None;
        {
            let bytes = self.buf.as_bytes();
            let mut i = self.scan_pos;
            while i < bytes.len() {
                let c = bytes[i];
                if self.depth == 0 && self.in_string {
                    // 顶层裸字符串（契约外畸形条目）：串内 ] 不封数组、串内 { 不开深度——与对象内同款字符串感知
                    if self.escaped {
                        self.escaped = false;
                    } else {
                        match c {
                            b'\\' => self.escaped = true,
                            b'"' => self.in_string = false,
                            _ => {}
                        }
                    }
                } else if self.depth == 0 {
                    match c {
                        b'"' => self.in_string = true, // 顶层字符串开（畸形输入；正常 findings 条目皆对象）
                        b'{' => {
                            self.depth = 1;
                            self.obj_start = Some(i);
                            self.in_string = false;
                            self.escaped = false;
                        }
                        b']' => {
                            self.done = true;
                        }
                        _ => {} // 顶层逗号 / 空白
                    }
                    if self.done {
                        break;
                    }
                } else if self.in_string {
                    if self.escaped {
                        self.escaped = false;
                    } else {
                        match c {
                            b'\\' => self.escaped = true,
                            b'"' => self.in_string = false,
                            _ => {}
                        }
                    }
                } else {
                    match c {
                        b'"' => self.in_string = true,
                        b'{' => self.depth += 1,
                        b'}' => {
                            self.depth -= 1;
                            if self.depth == 0 {
                                closed = Some((self.obj_start.take().unwrap_or(0), i + 1));
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
            if closed.is_none() {
                self.scan_pos = i;
            }
        }
        if self.done {
            self.buf.clear();
            self.scan_pos = 0;
            return false;
        }
        if let Some((start, end)) = closed {
            self.scan_pos = 0;
            self.in_string = false;
            self.escaped = false;
            self.close_object(start, end);
            self.buf.drain(..end);
            return true;
        }
        false
    }

    /// 对象闭合：切片 → JSON 解析 → 规整 → emit；失败丢弃计数。
    fn close_object(&mut self, start: usize, end: usize) {
        let slice = &self.buf[start..end];
        match serde_json::from_str::<serde_json::Value>(slice) {
            Ok(v) => match v.as_object() {
                Some(o) => match regularize_finding(o) {
                    Ok(f) => self.emitted.push(f),
                    Err(DropReason::BadPath | DropReason::NoQuote) => self.discarded += 1,
                },
                None => self.discarded += 1,
            },
            Err(_) => self.discarded += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Severity;

    /// 尝试复位（批 D-1）：半截对象丢掉、不与重试轮拼接；已闭合对象与丢弃计数保留。
    #[test]
    fn reset_for_attempt_drops_the_half_object_but_keeps_what_closed() {
        let mut inc = IncrementalFindings::new();
        // 首试：一条完整的 + 一条只到一半就断了。**不先 take**——真路径上重试之间没有 drain，
        // 已闭合对象就躺在队列里跨复位，这正是 D10 立旁路的全部价值（格中途死亡也不丢）。
        inc.feed(r#"{"findings":[{"severity":"low","title":"done","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"q"},{"severity":"high","title":"half"#);

        inc.reset_for_attempt();
        assert_eq!(inc.discarded(), 1, "半截的计入丢弃");
        let kept = inc.take();
        assert_eq!(kept.len(), 1, "首试已闭合的那条跨复位保留: {kept:?}");
        assert_eq!(kept[0].title, "done");

        // 重试轮：从头一个完整的流
        inc.feed(r#"{"findings":[{"severity":"medium","title":"retry","body":"b","file":"a.rs","line_start":2,"line_end":2,"confidence":0.6,"recommendation":"r","quote":"q2"}]}"#);
        let out = inc.take();
        assert_eq!(out.len(), 1, "重试轮的对象照常发出: {out:?}");
        assert_eq!(out[0].title, "retry");
        assert_eq!(out[0].severity, Severity::Medium, "没有与半截的 high 拼在一起");
        assert_eq!(inc.discarded(), 1, "丢弃计数跨尝试累计、不被复位清零");
    }

    /// 不复位时的退化形状（本用例钉住「为什么要复位」）：半截与重试输出拼在一起，
    /// 解析器还停在首试的数组里，重试轮的 `{"findings":[` 被当成数组内文本。
    #[test]
    fn without_reset_the_retry_stream_is_concatenated() {
        let mut inc = IncrementalFindings::new();
        inc.feed(r#"{"findings":[{"severity":"high","title":"half"#);
        inc.feed(r#"{"findings":[{"severity":"medium","title":"retry","body":"b","file":"a.rs","line_start":2,"line_end":2,"confidence":0.6,"recommendation":"r","quote":"q2"}]}"#);
        let out = inc.take();
        assert!(
            out.first().is_none_or(|f| f.title != "retry"),
            "不复位时拿不到干净的重试对象（拼接或错位），故必须复位: {out:?}"
        );
    }

    fn feed_all(inc: &mut IncrementalFindings, text: &str, chunk: usize) {
        for piece in text.as_bytes().chunks(chunk) {
            inc.feed(&String::from_utf8_lossy(piece));
        }
    }

    /// 三 finding 分片到达 → 三次 emit；与终态 parse_review_output 聚合一致（旁路不改契约）。
    #[test]
    fn three_findings_chunked_emit_and_final_parity() {
        let json = r#"{"findings":[
            {"severity":"high","title":"t1","body":"b1","file":"src/a.rs","line_start":1,"line_end":2,"confidence":0.9,"recommendation":"r1","quote":"let x = 1;"},
            {"severity":"low","title":"t2","body":"b2","file":"src/a.rs","line_start":3,"line_end":3,"confidence":0.5,"recommendation":"r2","quote":"let y = 2;"},
            {"severity":"medium","title":"t3","body":"b3","file":"src/b.rs","line_start":10,"line_end":12,"confidence":0.7,"recommendation":"r3","quote":"fn z() {"}
        ],"needs":[]}"#;
        for chunk in [1usize, 7, 64, 512] {
            let mut inc = IncrementalFindings::new();
            feed_all(&mut inc, json, chunk);
            let emitted = inc.take();
            assert_eq!(emitted.len(), 3, "chunk={chunk}");
            assert_eq!((emitted[0].severity, emitted[0].title.as_str()), (Severity::High, "t1"));
            assert_eq!(emitted[2].file, "src/b.rs");
            assert_eq!(inc.discarded(), 0);
            assert!(inc.done(), "数组闭合后停止消费");
            // 终态一致：parse_review_output 保留同样三条
            let (out, stats) = crate::provider::parse::parse_review_output_with_stats(json, "m").unwrap();
            let crate::domain::CellOutput::Review { findings, .. } = out else { panic!() };
            assert_eq!(findings.len(), 3);
            assert_eq!((stats.dropped_bad_path, stats.dropped_no_quote, stats.dropped_malformed), (0, 0, 0));
            for (e, f) in emitted.iter().zip(findings.iter()) {
                assert_eq!((e.title.as_str(), e.file.as_str(), e.quote.as_str()), (f.title.as_str(), f.file.as_str(), f.quote.as_str()), "旁路与终态一致");
            }
        }
    }

    /// JSON 中途截断 → 已闭合对象保留、未闭合经 finalize 计 discard（D10-1：cell 中途死亡已产出项不丢）。
    #[test]
    fn truncation_keeps_closed_objects_and_counts_discard() {
        let json = r#"{"findings":[
            {"severity":"high","title":"t1","body":"b1","file":"a.rs","line_start":1,"line_end":1,"confidence":0.9,"recommendation":"r","quote":"q1"},
            {"severity":"high","title":"t2","body":"b2","file":"a.rs","line_start":2,"line_end":2,"confidence":0.9,"recommendation":"r","quote":"q2"},
            {"severity":"high","title":"t3","body":"trunc"#;
        let mut inc = IncrementalFindings::new();
        feed_all(&mut inc, json, 16);
        assert_eq!(inc.take().len(), 2, "前两条已闭合保留");
        assert_eq!(inc.discarded(), 0, "finalize 前未闭合不计");
        inc.finalize();
        assert_eq!(inc.discarded(), 1, "finalize 把未闭合对象计入 discard");
        assert!(!inc.done(), "截断流没见到 ]");
        inc.finalize();
        assert_eq!(inc.discarded(), 1, "finalize 幂等");
    }

    /// 顶层裸字符串条目（契约外畸形）：串内 `]` / `{` 不破坏状态机，后续对象仍 emit。
    #[test]
    fn top_level_string_entries_do_not_break_state_machine() {
        let json = r#"{"findings":["see [x] and {y}",{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"q"}]}"#;
        let mut inc = IncrementalFindings::new();
        feed_all(&mut inc, json, 7);
        assert_eq!(inc.take().len(), 1, "串内 ] 不封数组，真对象照常 emit");
        assert!(inc.done());
    }

    /// 滑窗截断落在多字节字符中间不 panic（CJK 前导 > 64KB）。
    #[test]
    fn seek_window_drains_on_char_boundary() {
        let noise = "中文噪声".repeat(20_000); // 120KB，多字节
        let json = format!(r#"{noise}"findings":[{{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"q"}}]"#);
        let mut inc = IncrementalFindings::new();
        feed_all(&mut inc, &json, 1024);
        assert_eq!(inc.take().len(), 1, "多字节滑窗不 panic 且仍命中");
    }

    /// 非 findings 段 / 思考文本容忍：前导 <think>、散文、needs 数组在后都不干扰。
    #[test]
    fn tolerates_think_and_prose_around_findings() {
        let json = "<think>推理 { braces } 与 \"findings\" 字样 </think>\\nHere you go:\\n```json\\n{\"findings\":[{\"severity\":\"low\",\"title\":\"t\",\"body\":\"b\",\"file\":\"a.rs\",\"line_start\":1,\"line_end\":1,\"confidence\":0.5,\"recommendation\":\"r\",\"quote\":\"q\"}],\"needs\":[{\"path\":\"n.rs\",\"reason\":\"r\"}]}```";
        let mut inc = IncrementalFindings::new();
        feed_all(&mut inc, json, 13);
        assert_eq!(inc.take().len(), 1, "think 里的伪 \"findings\" 字样不触发（无冒号+方括号形态）");
        assert_eq!(inc.discarded(), 0);
        assert!(inc.done());
    }

    /// 伪匹配防护：`"my_findings"` / `"findings_extra"` 键不触发（键边界不完整）。
    #[test]
    fn does_not_match_findings_substring_keys() {
        let json = r#"{"my_findings":[1,2],"findings_x":[],"findings":[{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"q"}]}"#;
        let mut inc = IncrementalFindings::new();
        feed_all(&mut inc, json, 8);
        let emitted = inc.take();
        assert_eq!(emitted.len(), 1, "只吃真 findings 数组");
        assert_eq!(emitted[0].title, "t");
    }

    /// 对象闭合但形状不可救（quote 空）→ discard 计数。
    #[test]
    fn closed_but_invalid_object_counts_discard() {
        let json = r#"{"findings":[{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"   "}]}"#;
        let mut inc = IncrementalFindings::new();
        feed_all(&mut inc, json, 24);
        assert_eq!(inc.take().len(), 0);
        assert_eq!(inc.discarded(), 1);
    }

    /// 键跨 feed 边界劈开：`"find` + `ings": [` 分两片 → 仍命中。
    #[test]
    fn key_split_across_feed_boundary() {
        let mut inc = IncrementalFindings::new();
        inc.feed(r#"prelude "fin"#);
        inc.feed(r#"dings": [{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"q"}]}"#);
        assert_eq!(inc.take().len(), 1);
    }

    /// 长 think 滑窗：前导 100KB 噪声不撑爆、findings 仍命中。
    #[test]
    fn seek_window_slides_over_long_prelude() {
        let noise = "x".repeat(100_000);
        let json = format!(r#"{noise}"findings":[{{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"q"}}]"#);
        let mut inc = IncrementalFindings::new();
        feed_all(&mut inc, &json, 1024);
        assert_eq!(inc.take().len(), 1);
    }

    /// done 后 feed 忽略（数组闭合后的尾巴不消费、不计数）。
    #[test]
    fn feed_after_done_is_ignored() {
        let mut inc = IncrementalFindings::new();
        inc.feed(r#"{"findings":[{"severity":"low","title":"t","body":"b","file":"a.rs","line_start":1,"line_end":1,"confidence":0.5,"recommendation":"r","quote":"q"}],"needs":[]}"#);
        assert!(inc.done());
        inc.feed(r#" trailing junk {"findings":"more"}"#);
        assert_eq!(inc.take().len(), 1);
        assert_eq!(inc.discarded(), 0);
    }
}
