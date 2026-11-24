//! hibot finding 指纹（契约 §3.2，**唯一实现**——hibot 会复算校验）。
//!
//! ```text
//! fingerprint = "sha256:" + sha256( file + "\0" + normalize(quote) + "\0" + normalize(title) ).hex
//! normalize(s) = 逐行去首尾空白 → 丢弃空行 → 用单个空格连接
//! ```
//!
//! 注意：**不 lowercase**（与内部 fnv1a64 id 不同）。

use sha2::{Digest, Sha256};

/// 契约的 normalize：逐行 trim、丢空行、单空格 join。
pub fn normalize(s: &str) -> String {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// `"sha256:<64 hex>"`。
pub fn of(file: &str, quote: &str, title: &str) -> String {
    let mut h = Sha256::new();
    h.update(file.as_bytes());
    h.update([0u8]);
    h.update(normalize(quote).as_bytes());
    h.update([0u8]);
    h.update(normalize(title).as_bytes());
    format!("sha256:{}", hex(&h.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 校验 `sha256:<64 lowercase hex>` 形状（findings.json 输入用）。
pub fn is_fingerprint(s: &str) -> bool {
    s.strip_prefix("sha256:")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expected(file: &str, quote: &str, title: &str) -> String {
        // 独立实现（契约原文）用于对拍
        use sha2::{Digest, Sha256};
        let norm = |s: &str| s.lines().map(str::trim).filter(|l| !l.is_empty()).collect::<Vec<_>>().join(" ");
        let mut h = Sha256::new();
        h.update(file.as_bytes());
        h.update(b"\0");
        h.update(norm(quote).as_bytes());
        h.update(b"\0");
        h.update(norm(title).as_bytes());
        format!("sha256:{}", h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>())
    }

    #[test]
    fn normalizes_whitespace_and_drops_empty_lines() {
        assert_eq!(normalize("  a  \n\n\tb\n"), "a b");
        assert_eq!(normalize(""), "");
        assert_eq!(normalize("one"), "one");
    }

    #[test]
    fn matches_contract_vector() {
        let f = "src/db.ts";
        let q = "  db.query(`SELECT *\n\n  FROM users`)  ";
        let t = " SQL 注入 \n";
        assert_eq!(of(f, q, t), expected(f, q, t));
        assert!(is_fingerprint(&of(f, q, t)));
    }

    #[test]
    fn case_is_preserved_not_lowercased() {
        assert_ne!(of("a", "Q", "T"), of("a", "q", "t"));
        // 行首尾缩进不影响身份（行内空白保留：契约只 trim 每行、不折叠行内空格）
        assert_eq!(of("a", "  x\n    y  ", "t"), of("a", "x\ny", "t"));
        assert_ne!(of("a", "x   y", "t"), of("a", "x y", "t"));
    }

    #[test]
    fn separator_is_nul_so_shifted_content_differs() {
        assert_ne!(of("a", "bc", "d"), of("a", "b", "cd"));
    }

    #[test]
    fn fingerprint_shape_validation() {
        assert!(is_fingerprint("sha256:0000000000000000000000000000000000000000000000000000000000000000"));
        assert!(!is_fingerprint("sha256:abc"));
        assert!(!is_fingerprint("sha256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"));
        assert!(!is_fingerprint("md5:0000000000000000000000000000000000000000000000000000000000000000"));
    }
}
