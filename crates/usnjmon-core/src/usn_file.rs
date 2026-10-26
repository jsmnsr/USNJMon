//! Reads extracted `$UsnJrnl:$J` streams, not MFT/disk images or CSV exports.

use crate::journal;
use crate::journal::Record;
use anyhow::{Context, Result, ensure};
use std::io::BufRead;

pub struct FileRecord {
    pub offset: u64,
    pub record: Record,
}

pub struct UsnFile<Reader> {
    reader: Reader,
    offset: u64,
    previous_usn: Option<i64>,
    pub padding_bytes: u64,
}

impl<Reader: BufRead> UsnFile<Reader> {
    #[must_use]
    pub fn new(reader: Reader) -> Self {
        Self {
            reader,
            offset: 0,
            previous_usn: None,
            padding_bytes: 0,
        }
    }

    /// Zero-filled sparse regions are skipped; malformed nonzero data is never carved around.
    pub fn next_record(&mut self, cancelled: impl Fn() -> bool) -> Result<Option<FileRecord>> {
        loop {
            ensure!(!cancelled(), "conversion cancelled");
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                return Ok(None);
            }
            let zero_bytes = available.iter().take_while(|&&byte| byte == 0).count() / 8 * 8;
            if zero_bytes > 0 {
                self.reader.consume(zero_bytes);
                self.offset += zero_bytes as u64;
                self.padding_bytes += zero_bytes as u64;
                continue;
            }
            let start = self.offset;
            let mut header = [0u8; 8];
            self.reader
                .read_exact(&mut header)
                .with_context(|| format!("truncated record header at input offset {start}"))?;
            if header == [0; 8] {
                self.offset += 8;
                self.padding_bytes += 8;
                continue;
            }
            let length =
                u32::from_le_bytes(header[..4].try_into().expect("four-byte record length"))
                    as usize;
            ensure!(
                (8..=journal::BUFFER_SIZE - 8).contains(&length) && length % 8 == 0,
                "invalid record length {length} at input offset {start}"
            );
            let version = u16::from_le_bytes([header[4], header[5]]);
            let usn_offset = match version {
                2 => 24,
                3 => 40,
                _ => anyhow::bail!("unsupported USN version {version} at input offset {start}"),
            };
            ensure!(
                length >= usn_offset + 8,
                "truncated USN at input offset {start}"
            );
            let mut bytes = vec![0u8; length + 8];
            bytes[8..16].copy_from_slice(&header);
            self.reader
                .read_exact(&mut bytes[16..])
                .with_context(|| format!("truncated record at input offset {start}"))?;
            let usn = i64::from_le_bytes(
                bytes[8 + usn_offset..16 + usn_offset]
                    .try_into()
                    .expect("validated eight-byte USN"),
            );
            let next = usn
                .checked_add(length as i64)
                .context("USN overflow in input")?;
            bytes[..8].copy_from_slice(&next.to_le_bytes());
            let mut batch = journal::parse_batch(&bytes, usn)
                .with_context(|| format!("parse record at input offset {start}"))?;
            ensure!(
                self.previous_usn.is_none_or(|previous| usn > previous),
                "non-increasing USN at input offset {start}"
            );
            self.previous_usn = Some(usn);
            self.offset += length as u64;
            return Ok(Some(FileRecord {
                offset: start,
                record: batch
                    .records
                    .pop()
                    .context("record parser returned no record")?,
            }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::fixtures;
    use std::io::{BufReader, Cursor};

    #[test]
    fn streams_mixed_versions_and_sparse_padding() {
        let mut bytes = vec![0; 65536];
        bytes.extend_from_slice(&fixtures::batch(2, 65536, 0x100, &[65])[8..]);
        bytes.extend_from_slice(&[0; 8]);
        bytes.extend_from_slice(&fixtures::batch(3, 65608, 0x200, &[66])[8..]);
        let mut reader = UsnFile::new(BufReader::with_capacity(31, Cursor::new(bytes)));
        let first = reader.next_record(|| false).unwrap().unwrap();
        assert_eq!(first.offset, 65536);
        assert_eq!(first.record.usn, 65536);
        let second = reader.next_record(|| false).unwrap().unwrap();
        assert_eq!(second.record.major_version, 3);
        assert_eq!(reader.padding_bytes, 65544);
        assert!(reader.next_record(|| false).unwrap().is_none());
    }

    #[test]
    fn rejects_truncation_bad_versions_reordering_and_cancelled_scans() {
        let good = fixtures::batch(2, 100, 0x100, &[65])[8..].to_vec();
        for length in 1..good.len() {
            let mut reader = UsnFile::new(Cursor::new(&good[..length]));
            assert!(
                reader.next_record(|| false).is_err(),
                "truncation at {length}"
            );
        }
        let mut bad = good.clone();
        bad[4] = 4;
        assert!(
            UsnFile::new(Cursor::new(bad))
                .next_record(|| false)
                .is_err()
        );
        let duplicate = [good.clone(), good].concat();
        let mut reader = UsnFile::new(Cursor::new(duplicate));
        reader.next_record(|| false).unwrap();
        assert!(reader.next_record(|| false).is_err());
        assert!(
            UsnFile::new(Cursor::new(vec![0; 65536]))
                .next_record(|| true)
                .is_err()
        );
    }
}
