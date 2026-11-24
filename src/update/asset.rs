//! 发布产物命名、SHA256SUMS 解析与归档解包。

use std::fmt::Write as _;
use std::io::Read;
use std::path::Component;

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

/// 单个二进制的大小上限，避免异常归档耗尽内存。
pub const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;

const SHA256_HEX_LEN: usize = 64;

/// 归档名：`redteam-{plain}-{target}.tar.gz`（与 `scripts/release.sh` 一致）。
pub fn asset_name(version: &str, target: &str) -> String {
    let plain = version
        .trim()
        .strip_prefix('v')
        .or_else(|| version.trim().strip_prefix('V'))
        .unwrap_or_else(|| version.trim());
    format!("redteam-{plain}-{target}.tar.gz")
}

/// 从 `shasum -a 256` 格式的文本中取指定文件的 SHA256（小写）。
///
/// 兼容 `<hash>  <name>`、`<hash> *<name>` 与多余空白；不匹配或格式异常返回 None。
pub fn checksum_for(sums: &str, asset: &str) -> Option<String> {
    for line in sums.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        let name = name.strip_prefix('*').unwrap_or(name);
        if name != asset {
            continue;
        }
        let hash = hash.to_ascii_lowercase();
        if hash.len() == SHA256_HEX_LEN && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Some(hash);
        }
    }
    None
}

/// 计算字节的 SHA256（小写十六进制）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(SHA256_HEX_LEN);
    for byte in digest {
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// 从 tar.gz 中取出根目录的 `redteam` 普通文件。
///
/// 只接受 `redteam` 或 `./redteam`；拒绝嵌套路径、符号链接、重复候选。
pub fn extract_binary(archive: &[u8]) -> Result<Vec<u8>> {
    let decoder = GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let mut found: Option<Vec<u8>> = None;

    for entry in tar.entries().context("读取 tar.gz 条目失败")? {
        let entry = entry.context("读取 tar.gz 条目失败")?;
        let path = entry.path().context("解析归档内路径失败")?.into_owned();
        if normalize_entry_path(&path).as_deref() != Some("redteam") {
            continue;
        }
        let header = entry.header();
        if !header.entry_type().is_file() {
            bail!("归档中的 redteam 不是普通文件（可能是符号链接）");
        }
        if header.size().unwrap_or(0) > MAX_BINARY_BYTES {
            bail!("归档中的 redteam 超过 {MAX_BINARY_BYTES} 字节上限");
        }
        if found.is_some() {
            bail!("归档中出现多个 redteam 条目");
        }
        let mut bytes = Vec::new();
        entry
            .take(MAX_BINARY_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("读取归档中的 redteam 失败")?;
        if bytes.len() as u64 > MAX_BINARY_BYTES {
            bail!("归档中的 redteam 超过 {MAX_BINARY_BYTES} 字节上限");
        }
        found = Some(bytes);
    }

    found.ok_or_else(|| anyhow::anyhow!("归档中没有 redteam 二进制"))
}

/// 只保留正常路径段并把 `.` 折叠掉；`redteam` 与 `./redteam` 都归一为 `redteam`。
fn normalize_entry_path(path: &std::path::Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_names_follow_the_release_contract() {
        assert_eq!(
            asset_name("v0.1.0", "aarch64-apple-darwin"),
            "redteam-0.1.0-aarch64-apple-darwin.tar.gz"
        );
        assert_eq!(
            asset_name("0.1.0-rc.1", "aarch64-apple-darwin"),
            "redteam-0.1.0-rc.1-aarch64-apple-darwin.tar.gz"
        );
    }

    #[test]
    fn checksum_lines_are_parsed_conservatively() {
        let sums = "# comment\n\
                    ABCDEF0123456789abcdef0123456789abcdef0123456789abcdef0123456789  redteam-0.1.0-aarch64-apple-darwin.tar.gz\n\
                    1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef *other.tar.gz\n";
        assert_eq!(
            checksum_for(sums, "redteam-0.1.0-aarch64-apple-darwin.tar.gz").as_deref(),
            Some("abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789")
        );
        assert!(checksum_for(sums, "missing.tar.gz").is_none());
        assert!(checksum_for("not-a-hash  x.tar.gz\n", "x.tar.gz").is_none());
    }

    #[test]
    fn sha256_matches_known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    fn archive_with(name: &str, payload: &[u8]) -> Vec<u8> {
        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder.append_data(&mut header, name, payload).unwrap();
        builder.into_inner().unwrap().finish().unwrap()
    }

    use flate2::write::GzEncoder;
    use flate2::Compression;

    #[test]
    fn extracts_the_root_binary() {
        for name in ["redteam", "./redteam"] {
            let archive = archive_with(name, b"binary-bytes");
            assert_eq!(extract_binary(&archive).unwrap(), b"binary-bytes", "{name}");
        }
    }

    #[test]
    fn rejects_nested_or_missing_binary() {
        let nested = archive_with("pkg/redteam", b"x");
        assert!(extract_binary(&nested).is_err());
        let other = archive_with("README.md", b"x");
        assert!(extract_binary(&other).is_err());
    }
}
