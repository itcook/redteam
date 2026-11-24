//! 2.4 `dismissed.csv` 闭环（契约 §1.2 / Q8 / 附录 D）。
//!
//! 契约：UTF-8 + RFC4180；首行固定 `fingerprint,actor,reason,created_at`；按 `fingerprint` 精确匹配。
//! 命中者**不复报、不阻塞**，由 hibot 按指纹过滤 verdict/评论；我们只能：
//! - 在 finding 级 `extensions.redteam.ledger = {dismissed, actor, reason}` 留审计；
//! - **绝不写 `refuted: true`**（人的决定 ≠ 证据证伪）。
//!
//! `csv` crate 默认解析偏宽松（裸引号会被当成普通字符），所以先用 [`validate_rfc4180`] 做一遍
//! 严格形态校验，再交给 `csv` 取值——坏格式一律 `input_schema_mismatch:dismissed`。

use crate::job::fingerprint;
use crate::job::status::JobFailure;
use std::collections::BTreeMap;

/// CSV 的一行（审计字段原样保留）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub fingerprint: String,
    pub actor: String,
    pub reason: String,
    pub created_at: String,
}

/// 已解析的台账：指纹 → 行。用 `BTreeMap` 而不是 `HashSet`——匹配语义等价，
/// 但还能把 actor/reason 带进 finding 的审计扩展（附录 D 的 `ledger`）。
#[derive(Clone, Debug, Default)]
pub struct Dismissed {
    entries: BTreeMap<String, Entry>,
}

impl Dismissed {
    pub fn get(&self, fp: &str) -> Option<&Entry> {
        self.entries.get(fp)
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

const HEADER: [&str; 4] = ["fingerprint", "actor", "reason", "created_at"];

/// 命中即标 `previously_dismissed`（内部「不阻塞」口径），并返回指纹 → 审计行。
///
/// 同时按**指纹**去重（协调者 2026-11-24 裁）：同一问题被两个 model 各报一遍时，
/// 契约身份字段相同 → 只保留首条，审计行覆盖其上。
/// **不写 `refuted`**：`previously_dismissed` 只影响内部 blocking 计数，契约层仍是不参与判定的审计事实。
pub fn apply(findings: &mut Vec<crate::domain::Finding>, ledger: &Dismissed) -> BTreeMap<String, Entry> {
    let mut seen = std::collections::BTreeSet::new();
    let mut hits = BTreeMap::new();
    findings.retain_mut(|f| {
        let fp = fingerprint::of(&f.file, &f.quote, &f.title);
        if !seen.insert(fp.clone()) {
            return false;
        }
        if let Some(e) = ledger.get(&fp) {
            f.previously_dismissed = true;
            hits.insert(fp, e.clone());
        }
        true
    });
    hits
}

fn bad() -> JobFailure {
    JobFailure::new("input_schema_mismatch", Some("dismissed".into()))
}

/// 解析 `dismissed.csv`。任何形态/取值问题 → `input_schema_mismatch:dismissed`。
pub fn parse(bytes: &[u8]) -> Result<Dismissed, JobFailure> {
    validate_rfc4180(bytes)?;
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(bytes);
    let headers = rdr.headers().map_err(|_| bad())?;
    if headers.len() != HEADER.len() || !headers.iter().zip(HEADER).all(|(a, b)| a == b) {
        return Err(bad());
    }
    let mut entries = BTreeMap::new();
    for rec in rdr.records() {
        let rec = rec.map_err(|_| bad())?;
        if rec.len() != HEADER.len() {
            return Err(bad());
        }
        let fingerprint = rec[0].trim().to_string();
        let actor = rec[1].to_string();
        let reason = rec[2].to_string();
        let created_at = rec[3].to_string();
        if !fingerprint::is_fingerprint(&fingerprint)
            || actor.trim().is_empty()
            || !is_iso8601_prefix(&created_at)
        {
            return Err(bad());
        }
        // 同一指纹多行：后者覆盖前者（hibot 的导出按时间追加；取最后一次决定）。
        entries.insert(fingerprint.clone(), Entry { fingerprint, actor, reason, created_at });
    }
    Ok(Dismissed { entries })
}

/// `^[0-9]{4}-[0-9]{2}-[0-9]{2}T`（契约 schema 的 pattern）。
fn is_iso8601_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'T'
}

/// 严格 RFC4180 形态校验（UTF-8 + 引号规则 + 每记录 4 字段）。
///
/// 拒绝的东西：未闭合引号、未加引号字段里的裸引号、闭合引号后除 `,`/换行以外的字符、
/// 字段数不是 4、裸 `\r`（只接受 `\r\n`）。这些 `csv` crate 默认会放过，必须自己挡。
fn validate_rfc4180(bytes: &[u8]) -> Result<(), JobFailure> {
    if std::str::from_utf8(bytes).is_err() || bytes.is_empty() {
        return Err(bad());
    }
    #[derive(PartialEq)]
    enum St {
        FieldStart,
        Unquoted,
        Quoted,
        Closed,
    }
    let mut st = St::FieldStart;
    let mut col = 0usize; // 当前记录已走过的分隔符数 → 字段数 = col + 1
    let mut i = 0usize;
    let mut end_record = |col: usize| -> Result<(), JobFailure> {
        // 4 列固定（表头 + 数据行同形）。
        if col + 1 != HEADER.len() {
            return Err(bad());
        }
        Ok(())
    };
    while i < bytes.len() {
        let b = bytes[i];
        match st {
            St::FieldStart => match b {
                b'"' => {
                    st = St::Quoted;
                    i += 1;
                }
                b',' => {
                    col += 1;
                    i += 1;
                }
                b'\n' => {
                    end_record(col)?;
                    col = 0;
                    i += 1;
                }
                b'\r' => {
                    if bytes.get(i + 1) != Some(&b'\n') {
                        return Err(bad());
                    }
                    end_record(col)?;
                    col = 0;
                    i += 2;
                }
                _ => {
                    st = St::Unquoted;
                    i += 1;
                }
            },
            St::Unquoted => match b {
                b'"' => return Err(bad()),
                b',' => {
                    col += 1;
                    st = St::FieldStart;
                    i += 1;
                }
                b'\n' => {
                    end_record(col)?;
                    col = 0;
                    st = St::FieldStart;
                    i += 1;
                }
                b'\r' => {
                    if bytes.get(i + 1) != Some(&b'\n') {
                        return Err(bad());
                    }
                    end_record(col)?;
                    col = 0;
                    st = St::FieldStart;
                    i += 2;
                }
                _ => i += 1,
            },
            St::Quoted => {
                if b == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 2; // 转义的双引号：留在引号内
                    } else {
                        st = St::Closed;
                        i += 1;
                    }
                } else {
                    i += 1;
                }
            }
            St::Closed => match b {
                b',' => {
                    col += 1;
                    st = St::FieldStart;
                    i += 1;
                }
                b'\n' => {
                    end_record(col)?;
                    col = 0;
                    st = St::FieldStart;
                    i += 1;
                }
                b'\r' => {
                    if bytes.get(i + 1) != Some(&b'\n') {
                        return Err(bad());
                    }
                    end_record(col)?;
                    col = 0;
                    st = St::FieldStart;
                    i += 2;
                }
                _ => return Err(bad()),
            },
        }
    }
    if st == St::Quoted {
        return Err(bad()); // 未闭合引号
    }
    // 最后一行没有换行：也算一个记录；空文件/只有表头时 st == FieldStart 且 col == 0。
    if st == St::Unquoted || st == St::Closed {
        end_record(col)?;
    } else if col > 0 {
        end_record(col)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP1: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000001";
    const FP2: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000002";

    fn header() -> &'static str {
        "fingerprint,actor,reason,created_at\n"
    }

    #[test]
    fn parses_quotes_commas_and_crlf() {
        let csv = format!(
            "fingerprint,actor,reason,created_at\r\n\
             {FP1},alice,\"false positive, see #12\",2026-11-20T10:00:00Z\r\n\
             {FP2},bob,ok,2026-11-21T11:00:00Z\r\n"
        );
        let d = parse(csv.as_bytes()).unwrap();
        assert_eq!(d.len(), 2);
        let e = d.get(FP1).unwrap();
        assert_eq!(e.actor, "alice");
        assert_eq!(e.reason, "false positive, see #12");
        assert_eq!(e.created_at, "2026-11-20T10:00:00Z");
    }

    #[test]
    fn escaped_double_quote_is_kept() {
        let csv = format!("{}{FP1},alice,\"he said \"\"no\"\"\",2026-11-20T10:00:00Z\n", header());
        assert_eq!(parse(csv.as_bytes()).unwrap().get(FP1).unwrap().reason, "he said \"no\"");
    }

    #[test]
    fn header_only_is_an_empty_ledger() {
        assert!(parse(header().as_bytes()).unwrap().is_empty());
    }

    #[test]
    fn wrong_header_is_rejected() {
        let csv = format!("fingerprint,actor,reason\n{FP1},a,b\n");
        assert_eq!(parse(csv.as_bytes()).unwrap_err().full_code(), "input_schema_mismatch:dismissed");
        let reordered = format!("actor,fingerprint,reason,created_at\n{FP1},a,b,2026-11-20T10:00:00Z\n");
        assert!(parse(reordered.as_bytes()).is_err());
    }

    #[test]
    fn unclosed_quote_is_rejected() {
        let csv = format!("{}{FP1},alice,\"never closed,2026-11-20T10:00:00Z\n", header());
        assert!(parse(csv.as_bytes()).is_err());
    }

    #[test]
    fn bare_quote_in_unquoted_field_is_rejected() {
        // `csv` crate 默认会把裸引号当普通字符——严格 RFC4180 不接受。
        let csv = format!("{}{FP1},al\"ice,ok,2026-11-20T10:00:00Z\n", header());
        assert!(parse(csv.as_bytes()).is_err());
    }

    #[test]
    fn wrong_column_count_is_rejected() {
        let csv = format!("{}{FP1},alice,ok\n", header());
        assert!(parse(csv.as_bytes()).is_err());
        let extra = format!("{}{FP1},alice,ok,2026-11-20T10:00:00Z,extra\n", header());
        assert!(parse(extra.as_bytes()).is_err());
        let blank_line = format!("{}\n{FP1},alice,ok,2026-11-20T10:00:00Z\n", header());
        assert!(parse(blank_line.as_bytes()).is_err());
    }

    #[test]
    fn field_values_are_validated() {
        for bad in [
            // 指纹形态
            format!("{}{},{},ok,2026-11-20T10:00:00Z\n", header(), "sha256:abc", "alice"),
            // actor 空
            format!("{}{}, ,ok,2026-11-20T10:00:00Z\n", header(), FP1),
            // created_at 不是 ISO 前缀
            format!("{}{},alice,ok,20-11-20\n", header(), FP1),
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn duplicate_fingerprint_last_wins() {
        let csv = format!(
            "{}{FP1},alice,first,2026-11-20T10:00:00Z\n{FP1},bob,second,2026-11-21T10:00:00Z\n",
            header()
        );
        let d = parse(csv.as_bytes()).unwrap();
        assert_eq!(d.len(), 1);
        assert_eq!(d.get(FP1).unwrap().actor, "bob");
    }

    fn finding(file: &str, quote: &str, title: &str) -> crate::domain::Finding {
        let mut f = crate::domain::finding::fixtures::finding("id", crate::domain::Severity::High, 1, "m");
        f.file = file.into();
        f.quote = quote.into();
        f.title = title.into();
        f
    }

    #[test]
    fn apply_marks_dismissed_but_never_refutes() {
        let csv = format!("{}{FP1},alice,false positive,2026-11-20T10:00:00Z\n", header());
        let d = parse(csv.as_bytes()).unwrap();
        let fp = fingerprint::of("src/a.rs", "q", "t");
        let mut findings = vec![
            finding("src/a.rs", "q", "t"),
            // 同指纹的第二条（不同 id）：去重后只剩一条，审计覆盖其上。
            finding("src/a.rs", "q", "t"),
            finding("src/b.rs", "other", "t2"),
        ];
        let hits = apply(&mut findings, &d);
        assert_eq!(findings.len(), 2, "同指纹只保留一条");
        assert!(!findings[0].previously_dismissed, "指纹不匹配就不该命中");
        assert!(hits.is_empty());

        let csv2 = format!("{}{},alice,false positive,2026-11-20T10:00:00Z\n", header(), fp);
        let d2 = parse(csv2.as_bytes()).unwrap();
        let mut fs = vec![finding("src/a.rs", "q", "t")];
        let hits2 = apply(&mut fs, &d2);
        assert!(fs[0].previously_dismissed);
        assert!(!fs[0].is_refuted(), "人的驳回不得写成证据证伪");
        assert_eq!(hits2.get(&fp).unwrap().actor, "alice");
        assert_eq!(hits2.get(&fp).unwrap().reason, "false positive");
    }

    #[test]
    fn empty_file_and_bad_utf8_are_rejected() {
        assert!(parse(b"").is_err());
        assert!(parse(&[0xff, 0xfe]).is_err());
    }
}
