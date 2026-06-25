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

    /// Golden 断言：钉死 FNV-1a 常量与分隔逻辑，防止未来静默破坏跨进程 finding id 稳定性。
    /// 若此测试失败，说明哈希算法或分隔符已变更，finding id 将不再跨进程一致——需刻意迁移。
    #[test]
    fn golden_values() {
        assert_eq!(fnv1a64_hex(&["a", "b"]), "d2b371819297f98a");
        assert_eq!(fnv1a64_hex(&["ab", "c"]), "20ba9b3025a8b421");
        assert_eq!(fnv1a64_hex(&["a", "bc"]), "a0a3542c19b900ab");
    }
}
