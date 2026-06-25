/// FNV-1a 64bit，确定性、跨进程稳定 —— 用于 finding 的稳定 id。
pub fn fnv1a64_hex(parts: &[&str]) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for p in parts {
        for b in p.as_bytes() {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        hash ^= 0xff; // 分隔，避免 ["ab","c"] 与 ["a","bc"] 同值
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_and_distinct() {
        assert_eq!(fnv1a64_hex(&["a", "b"]), fnv1a64_hex(&["a", "b"]));
        assert_ne!(fnv1a64_hex(&["ab", "c"]), fnv1a64_hex(&["a", "bc"]));
    }
}
