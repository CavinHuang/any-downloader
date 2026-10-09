//! 崩溃安全进度 journal（DESIGN §5.5）。
//!
//! 不变量：journal 记录 ⊆ 已成功写入 OS 的字节集合（数据先行，journal 滞后推进）。
//! 检查点顺序由调用方（writer 线程）保证：sync(数据文件) → 覆盖写 journal → sync(journal)。
//!
//! 布局：文件由两个 4KiB 槽（A/B）组成，槽内
//! `[magic|seq|total_len|validator_hash|count|reserved|entries..|xxh64]`。
//! 写入为单次覆盖写，不依赖 rename 原子性（Windows 上 rename 目标被占用会失败）；
//! 恢复取 sequence 最大且校验通过的槽。

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use xxhash_rust::xxh64::xxh64;

const SLOT: usize = 4096;
const MAGIC: [u8; 8] = *b"DLCTL01\x00";
const HEADER: usize = 40; // magic 8 + seq 8 + total 8 + validator 8 + count u32 + reserved u32
const ENTRY: usize = 24; // start/end/written 各 u64
const CHECKSUM: usize = 8;
pub const MAX_SEGMENTS: usize = (SLOT - HEADER - CHECKSUM) / ENTRY; // 168

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub start: u64,
    pub end: u64,
    pub written: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Data {
    pub total_len: u64,
    /// xxh64(etag) 串接 xxh64(last_modified)，续传身份校验。
    pub validator_hash: u64,
    pub entries: Vec<Entry>,
}

/// 续传校验器的指纹。ETag/Last-Modified 双缺失时为空串哈希，
/// 由调用方另行做首尾 4KB 指纹探测（DESIGN §5.3）。
pub fn validator_hash(etag: Option<&str>, last_modified: Option<&str>) -> u64 {
    let h = xxh64(etag.unwrap_or("").as_bytes(), 0);
    xxh64(last_modified.unwrap_or("").as_bytes(), h)
}

pub struct Journal {
    path: PathBuf,
    file: File,
    next_seq: u64,
    next_slot: usize,
}

impl Journal {
    /// 打开（不存在则创建）并尝试恢复。返回 (journal, 恢复的数据)。
    pub fn open(path: &Path) -> io::Result<(Journal, Option<Data>)> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let recovered = Self::load(&mut file)?;
        let (next_seq, next_slot) = match &recovered {
            Some((_, seq, slot)) => (seq + 1, 1 - slot),
            None => (0, 0),
        };
        Ok((
            Journal {
                path: path.to_path_buf(),
                file,
                next_seq,
                next_slot,
            },
            recovered.map(|(d, _, _)| d),
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 覆盖写当前槽并 sync。调用前必须已 sync 数据文件。
    pub fn checkpoint(&mut self, data: &Data) -> io::Result<()> {
        assert!(
            data.entries.len() <= MAX_SEGMENTS,
            "segment count exceeds journal capacity"
        );
        self.next_seq += 1;
        let buf = encode(data, self.next_seq);
        self.file
            .seek(SeekFrom::Start((self.next_slot * SLOT) as u64))?;
        self.file.write_all(&buf)?;
        self.file.sync_data()?;
        self.next_slot ^= 1;
        Ok(())
    }

    pub fn remove(path: &Path) {
        let _ = std::fs::remove_file(path);
    }

    fn load(file: &mut File) -> io::Result<Option<(Data, u64, usize)>> {
        let mut best: Option<(Data, u64, usize)> = None;
        let len = file.metadata()?.len();
        for slot in 0..2 {
            let start = (slot * SLOT) as u64;
            // 新建/截断的文件可能不足一个槽：视为无有效记录
            if start + SLOT as u64 > len {
                continue;
            }
            file.seek(SeekFrom::Start(start))?;
            let mut buf = vec![0u8; SLOT];
            file.read_exact(&mut buf)?;
            if let Some((data, seq)) = decode(&buf) {
                if best.as_ref().is_none_or(|(_, s, _)| seq > *s) {
                    best = Some((data, seq, slot));
                }
            }
        }
        Ok(best)
    }
}

fn encode(data: &Data, seq: u64) -> [u8; SLOT] {
    let mut buf = [0u8; SLOT];
    buf[0..8].copy_from_slice(&MAGIC);
    buf[8..16].copy_from_slice(&seq.to_le_bytes());
    buf[16..24].copy_from_slice(&data.total_len.to_le_bytes());
    buf[24..32].copy_from_slice(&data.validator_hash.to_le_bytes());
    buf[32..36].copy_from_slice(&(data.entries.len() as u32).to_le_bytes());
    let mut off = HEADER;
    for e in &data.entries {
        buf[off..off + 8].copy_from_slice(&e.start.to_le_bytes());
        buf[off + 8..off + 16].copy_from_slice(&e.end.to_le_bytes());
        buf[off + 16..off + 24].copy_from_slice(&e.written.to_le_bytes());
        off += ENTRY;
    }
    let sum = xxh64(&buf[0..off], 0);
    buf[off..off + CHECKSUM].copy_from_slice(&sum.to_le_bytes());
    buf
}

fn decode(buf: &[u8]) -> Option<(Data, u64)> {
    if buf[0..8] != MAGIC {
        return None;
    }
    let count = u32::from_le_bytes(buf[32..36].try_into().ok()?) as usize;
    if count > MAX_SEGMENTS {
        return None;
    }
    let end = HEADER + count * ENTRY;
    let stored = u64::from_le_bytes(buf[end..end + CHECKSUM].try_into().ok()?);
    if xxh64(&buf[0..end], 0) != stored {
        return None;
    }
    let mut entries = Vec::with_capacity(count);
    let mut off = HEADER;
    for _ in 0..count {
        entries.push(Entry {
            start: u64::from_le_bytes(buf[off..off + 8].try_into().ok()?),
            end: u64::from_le_bytes(buf[off + 8..off + 16].try_into().ok()?),
            written: u64::from_le_bytes(buf[off + 16..off + 24].try_into().ok()?),
        });
        off += ENTRY;
    }
    Some((
        Data {
            total_len: u64::from_le_bytes(buf[16..24].try_into().ok()?),
            validator_hash: u64::from_le_bytes(buf[24..32].try_into().ok()?),
            entries,
        },
        u64::from_le_bytes(buf[8..16].try_into().ok()?),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Data {
        Data {
            total_len: 1000,
            validator_hash: 42,
            entries: vec![
                Entry {
                    start: 0,
                    end: 500,
                    written: 500,
                },
                Entry {
                    start: 500,
                    end: 1000,
                    written: 123,
                },
            ],
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let buf = encode(&sample(), 7);
        let (data, seq) = decode(&buf).unwrap();
        assert_eq!(seq, 7);
        assert_eq!(data, sample());
    }

    #[test]
    fn corrupt_checksum_rejected() {
        let mut buf = encode(&sample(), 7);
        // 破坏覆盖区内的字节（total_len 字段，偏移 20）
        buf[20] ^= 0xFF;
        assert!(decode(&buf).is_none());
    }

    #[test]
    fn garbage_slot_rejected() {
        let buf = [0xABu8; SLOT];
        assert!(decode(&buf).is_none());
    }

    #[test]
    fn ab_slots_recover_latest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.dlctrl");
        let (mut j, recovered) = Journal::open(&path).unwrap();
        assert!(recovered.is_none());

        j.checkpoint(&sample()).unwrap();
        let mut d2 = sample();
        d2.entries[1].written = 999;
        j.checkpoint(&d2).unwrap();

        let (_, recovered) = Journal::open(&path).unwrap();
        assert_eq!(recovered.unwrap(), d2);
    }

    #[test]
    fn validator_hash_changes_with_inputs() {
        assert_ne!(
            validator_hash(Some("\"a\""), None),
            validator_hash(Some("\"b\""), None)
        );
        assert_eq!(validator_hash(None, None), validator_hash(None, None));
    }
}
