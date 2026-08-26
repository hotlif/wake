//! # Yarn PnP ZIP 归档读取器（stored + DEFLATE）
//!
//! 为支持 Yarn PnP 的 zip-backed 缓存而生（DESIGN §5.1 扩展）。Yarn `compressionLevel`
//! 可生成 stored（method 0）或 DEFLATE（method 8）条目；两者都必须透明读取。
//!
//! 若遇到 Yarn 不会生成的其他压缩算法或 ZIP64 归档，返回错误而非静默出错。
//!
//! ## 布局速览
//! - **EOCD**（End Of Central Directory，签名 `50 4B 05 06`）：从文件尾反扫定位，给出中央目录偏移/条目数。
//! - **中央目录头**（`50 4B 01 02`）：每条目的文件名、压缩方式、（未压缩）大小、local header 偏移。
//! - **local file header**（`50 4B 03 04`）：数据前的头，数据始于 `local_off + 30 + 名长 + extra 长`。

use std::io::{self, Read};

use flate2::read::DeflateDecoder;
use rustc_hash::FxHashMap;

/// 一条归档条目的定位信息。
#[derive(Clone, Copy, Debug)]
struct Entry {
    /// local file header 在归档中的字节偏移。
    local_header_offset: u32,
    compression_method: u16,
    compressed_size: u32,
    /// 未压缩大小。
    size: u32,
}

/// 一个已解析目录的 ZIP 归档，持有整份字节。
///
/// Yarn 缓存 zip 最大约 1~2 MB（如 lodash），整份读入内存简单且够快；真要抠可改 mmap，
/// 但那是后续优化（当前读盘一次远非热点）。
pub struct ZipArchive {
    bytes: Vec<u8>,
    /// 归一化条目名（正斜杠、无前导 `/`）→ 定位。目录条目名以 `/` 结尾。
    entries: FxHashMap<String, Entry>,
}

fn read_u16(b: &[u8], off: usize) -> Option<u16> {
    let s = b.get(off..off + 2)?;
    Some(u16::from_le_bytes([s[0], s[1]]))
}

fn read_u32(b: &[u8], off: usize) -> Option<u32> {
    let s = b.get(off..off + 4)?;
    Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

const EOCD_SIG: u32 = 0x0605_4b50;
const CDH_SIG: u32 = 0x0201_4b50;

fn corrupt(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("zip: {msg}"))
}

impl ZipArchive {
    /// 从整份归档字节解析目录。
    pub fn parse(bytes: Vec<u8>) -> io::Result<ZipArchive> {
        let (cd_offset, cd_count) = find_eocd(&bytes)?;
        let mut entries = FxHashMap::default();
        let mut off = cd_offset as usize;
        for _ in 0..cd_count {
            if read_u32(&bytes, off) != Some(CDH_SIG) {
                return Err(corrupt("中央目录头签名不符"));
            }
            let method = read_u16(&bytes, off + 10).ok_or_else(|| corrupt("头截断"))?;
            let compressed_size = read_u32(&bytes, off + 20).ok_or_else(|| corrupt("头截断"))?;
            let size = read_u32(&bytes, off + 24).ok_or_else(|| corrupt("头截断"))?;
            let name_len = read_u16(&bytes, off + 28).ok_or_else(|| corrupt("头截断"))? as usize;
            let extra_len = read_u16(&bytes, off + 30).ok_or_else(|| corrupt("头截断"))? as usize;
            let comment_len = read_u16(&bytes, off + 32).ok_or_else(|| corrupt("头截断"))? as usize;
            let local_off = read_u32(&bytes, off + 42).ok_or_else(|| corrupt("头截断"))?;
            let name_start = off + 46;
            let name_bytes = bytes
                .get(name_start..name_start + name_len)
                .ok_or_else(|| corrupt("文件名截断"))?;
            let name = String::from_utf8_lossy(name_bytes).into_owned();
            // Yarn compressionLevel 0..9 只会生成 stored(0) 或 DEFLATE(8)。
            if !matches!(method, 0 | 8) {
                return Err(corrupt(&format!(
                    "不支持的压缩方式 {method}（仅 stored/DEFLATE）：{name}"
                )));
            }
            // ZIP64 哨兵：偏移/大小为全 1 表示真值在 extra 区——Yarn 缓存不会触及，显式拒绝。
            if local_off == 0xFFFF_FFFF || compressed_size == 0xFFFF_FFFF || size == 0xFFFF_FFFF {
                return Err(corrupt("不支持 ZIP64 归档"));
            }
            entries.insert(
                normalize_entry(&name),
                Entry {
                    local_header_offset: local_off,
                    compression_method: method,
                    compressed_size,
                    size,
                },
            );
            off = name_start + name_len + extra_len + comment_len;
        }
        Ok(ZipArchive { bytes, entries })
    }

    /// 读取一个内部文件。目录 / 不存在 → `Ok(None)`；损坏条目返回错误。
    pub fn read(&self, inner: &str) -> io::Result<Option<Vec<u8>>> {
        let key = normalize_entry(inner);
        if key.ends_with('/') {
            return Ok(None); // 目录不可读为文件
        }
        let Some(e) = self.entries.get(&key) else {
            return Ok(None);
        };
        let lo = e.local_header_offset as usize;
        if read_u32(&self.bytes, lo) != Some(0x0403_4b50) {
            return Err(corrupt("local file header 签名不符"));
        }
        // local header 固定 30 字节，随后是文件名与 extra；数据在其后。
        // 中央目录的名长/extra 长可能与 local header 不同（extra 常不同），故必须读 local header 自身的两个长度。
        let name_len =
            read_u16(&self.bytes, lo + 26).ok_or_else(|| corrupt("local header 截断"))? as usize;
        let extra_len =
            read_u16(&self.bytes, lo + 28).ok_or_else(|| corrupt("local header 截断"))? as usize;
        let data_start = lo + 30 + name_len + extra_len;
        let compressed = self
            .bytes
            .get(data_start..data_start + e.compressed_size as usize)
            .ok_or_else(|| corrupt("条目数据截断"))?;
        let data = match e.compression_method {
            0 => {
                if e.compressed_size != e.size {
                    return Err(corrupt("stored 条目压缩与未压缩大小不一致"));
                }
                compressed.to_vec()
            }
            8 => {
                let mut decoder = DeflateDecoder::new(compressed);
                let mut output = Vec::with_capacity(e.size as usize);
                decoder
                    .read_to_end(&mut output)
                    .map_err(|error| corrupt(&format!("DEFLATE 解压失败：{error}")))?;
                if output.len() != e.size as usize {
                    return Err(corrupt("DEFLATE 解压后大小不符"));
                }
                output
            }
            _ => unreachable!("unsupported compression rejected during parse"),
        };
        Ok(Some(data))
    }

    /// 该内部路径是否为一个文件条目。
    pub fn is_file(&self, inner: &str) -> bool {
        let key = normalize_entry(inner);
        !key.ends_with('/') && self.entries.contains_key(&key)
    }

    /// 该内部路径是否为一个目录（存在同名 `/` 条目，或有任何条目以 `前缀/` 开头）。
    pub fn is_dir(&self, inner: &str) -> bool {
        let mut key = normalize_entry(inner);
        if key.is_empty() {
            return true; // 归档根
        }
        if !key.ends_with('/') {
            key.push('/');
        }
        if self.entries.contains_key(&key) {
            return true;
        }
        self.entries.keys().any(|k| k.starts_with(&key))
    }

    /// 列一个目录的直接子项内部路径（去重、含子目录，子目录以 `/` 结尾）。
    pub fn read_dir(&self, inner: &str) -> Vec<String> {
        let mut prefix = normalize_entry(inner);
        if !prefix.is_empty() && !prefix.ends_with('/') {
            prefix.push('/');
        }
        let mut seen = std::collections::BTreeSet::new();
        for k in self.entries.keys() {
            if let Some(rest) = k.strip_prefix(&prefix) {
                if rest.is_empty() {
                    continue;
                }
                // 取第一段；若后面还有内容说明是子目录。
                match rest.find('/') {
                    Some(i) => {
                        seen.insert(format!("{prefix}{}", &rest[..=i]));
                    }
                    None => {
                        seen.insert(format!("{prefix}{rest}"));
                    }
                }
            }
        }
        seen.into_iter().collect()
    }
}

/// 归一化条目名：反斜杠→正斜杠、去前导 `./` 与 `/`。保留尾随 `/`（目录标记）。
fn normalize_entry(name: &str) -> String {
    let mut s = name.replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    s = s.trim_start_matches('/').to_string();
    s
}

/// 从文件尾反扫定位 EOCD，返回（中央目录偏移, 条目数）。
///
/// EOCD 后可跟至多 65535 字节注释，故从末尾向前扫 `22 + 65535` 范围找签名。
fn find_eocd(bytes: &[u8]) -> io::Result<(u32, u16)> {
    if bytes.len() < 22 {
        return Err(corrupt("文件过短，非 zip"));
    }
    let max_back = 22usize.saturating_add(0xFFFF).min(bytes.len());
    let start = bytes.len() - max_back;
    // 从最靠后的可能位置向前找签名（EOCD 固定 22 字节 + 可变注释）。
    let mut i = bytes.len() - 22;
    loop {
        if read_u32(bytes, i) == Some(EOCD_SIG) {
            let total = read_u16(bytes, i + 10).ok_or_else(|| corrupt("EOCD 截断"))?;
            let cd_off = read_u32(bytes, i + 16).ok_or_else(|| corrupt("EOCD 截断"))?;
            if cd_off == 0xFFFF_FFFF || total == 0xFFFF {
                return Err(corrupt("不支持 ZIP64 归档"));
            }
            return Ok((cd_off, total));
        }
        if i == start {
            break;
        }
        i -= 1;
    }
    Err(corrupt("未找到 EOCD（非 zip 或已损坏）"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::DeflateEncoder};
    use std::io::Write;

    /// 手工构造一个含两个 stored 条目 + 一个目录条目的最小 zip。
    fn tiny_zip() -> Vec<u8> {
        // 条目：`a.txt`="hello"、`d/`（目录）、`d/b.txt`="hi"
        let mut buf = Vec::new();
        struct Rec {
            name: &'static str,
            data: &'static [u8],
            local_off: u32,
        }
        let mut recs = Vec::new();

        let add_local = |buf: &mut Vec<u8>, name: &str, data: &[u8]| -> u32 {
            let off = buf.len() as u32;
            buf.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // sig
            buf.extend_from_slice(&[0; 2]); // version
            buf.extend_from_slice(&[0; 2]); // flags
            buf.extend_from_slice(&0u16.to_le_bytes()); // method stored
            buf.extend_from_slice(&[0; 4]); // time/date
            buf.extend_from_slice(&0u32.to_le_bytes()); // crc
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes()); // comp size
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes()); // uncomp size
            buf.extend_from_slice(&(name.len() as u16).to_le_bytes()); // name len
            buf.extend_from_slice(&0u16.to_le_bytes()); // extra len
            buf.extend_from_slice(name.as_bytes());
            buf.extend_from_slice(data);
            off
        };

        for (name, data) in [
            ("a.txt", &b"hello"[..]),
            ("d/", &b""[..]),
            ("d/b.txt", &b"hi"[..]),
        ] {
            let off = add_local(&mut buf, name, data);
            recs.push(Rec {
                name,
                data,
                local_off: off,
            });
        }

        let cd_start = buf.len() as u32;
        for r in &recs {
            buf.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // sig
            buf.extend_from_slice(&[0; 2]); // version made by
            buf.extend_from_slice(&[0; 2]); // version needed
            buf.extend_from_slice(&[0; 2]); // flags
            buf.extend_from_slice(&0u16.to_le_bytes()); // method
            buf.extend_from_slice(&[0; 4]); // time/date
            buf.extend_from_slice(&0u32.to_le_bytes()); // crc
            buf.extend_from_slice(&(r.data.len() as u32).to_le_bytes()); // comp size
            buf.extend_from_slice(&(r.data.len() as u32).to_le_bytes()); // uncomp size
            buf.extend_from_slice(&(r.name.len() as u16).to_le_bytes()); // name len
            buf.extend_from_slice(&0u16.to_le_bytes()); // extra len
            buf.extend_from_slice(&0u16.to_le_bytes()); // comment len
            buf.extend_from_slice(&[0; 2]); // disk
            buf.extend_from_slice(&[0; 2]); // internal
            buf.extend_from_slice(&[0; 4]); // external
            buf.extend_from_slice(&r.local_off.to_le_bytes()); // local off
            buf.extend_from_slice(r.name.as_bytes());
        }
        let cd_size = buf.len() as u32 - cd_start;

        buf.extend_from_slice(&EOCD_SIG.to_le_bytes());
        buf.extend_from_slice(&[0; 2]); // disk
        buf.extend_from_slice(&[0; 2]); // cd disk
        buf.extend_from_slice(&(recs.len() as u16).to_le_bytes()); // records this disk
        buf.extend_from_slice(&(recs.len() as u16).to_le_bytes()); // total records
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_start.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes()); // comment len
        buf
    }

    #[test]
    fn reads_stored_entries() {
        let z = ZipArchive::parse(tiny_zip()).unwrap();
        assert_eq!(z.read("a.txt").unwrap(), Some(b"hello".to_vec()));
        assert_eq!(z.read("d/b.txt").unwrap(), Some(b"hi".to_vec()));
        assert_eq!(z.read("missing.txt").unwrap(), None);
        // 目录不可读为文件。
        assert_eq!(z.read("d/").unwrap(), None);
        assert_eq!(z.read("d").unwrap(), None);
    }

    fn one_entry_zip(method: u16, name: &str, original: &[u8], compressed: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&method.to_le_bytes());
        buf.extend_from_slice(&[0; 4]);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(original.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
        buf.extend_from_slice(compressed);

        let cd_start = buf.len() as u32;
        buf.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&method.to_le_bytes());
        buf.extend_from_slice(&[0; 4]);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&(compressed.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(original.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&[0; 4]);
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(name.as_bytes());
        let cd_size = buf.len() as u32 - cd_start;

        buf.extend_from_slice(&EOCD_SIG.to_le_bytes());
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&[0; 2]);
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes());
        buf.extend_from_slice(&cd_size.to_le_bytes());
        buf.extend_from_slice(&cd_start.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf
    }

    #[test]
    fn reads_deflate_entries() {
        let original = b"Yarn compressionLevel 9 works for Wake PnP";
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(original).unwrap();
        let compressed = encoder.finish().unwrap();
        let z = ZipArchive::parse(one_entry_zip(8, "pkg/index.js", original, &compressed)).unwrap();
        assert_eq!(z.read("pkg/index.js").unwrap(), Some(original.to_vec()));
    }

    #[test]
    fn is_file_and_is_dir() {
        let z = ZipArchive::parse(tiny_zip()).unwrap();
        assert!(z.is_file("a.txt"));
        assert!(!z.is_file("d"));
        assert!(z.is_dir("d"));
        assert!(z.is_dir("d/"));
        // 前缀推断：即使没有显式目录条目，有 `d/b.txt` 也应判 `d` 为目录。
        assert!(!z.is_file("nope"));
        assert!(z.is_dir("")); // 根
    }

    #[test]
    fn read_dir_lists_children() {
        let z = ZipArchive::parse(tiny_zip()).unwrap();
        let root = z.read_dir("");
        assert!(root.contains(&"a.txt".to_string()));
        assert!(root.contains(&"d/".to_string()));
        let d = z.read_dir("d");
        assert_eq!(d, vec!["d/b.txt".to_string()]);
    }

    #[test]
    fn rejects_non_zip() {
        assert!(ZipArchive::parse(b"not a zip file at all".to_vec()).is_err());
        assert!(ZipArchive::parse(vec![]).is_err());
    }

    #[test]
    fn normalize_entry_cases() {
        assert_eq!(normalize_entry("./a/b"), "a/b");
        assert_eq!(normalize_entry("a\\b\\c"), "a/b/c");
        assert_eq!(normalize_entry("/x"), "x");
        assert_eq!(normalize_entry("d/"), "d/");
    }
}
