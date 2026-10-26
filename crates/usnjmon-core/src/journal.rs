use anyhow::{Result, bail, ensure};
use serde::Serialize;

pub const BUFFER_SIZE: usize = 64 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub major_version: u16,
    pub minor_version: u16,
    pub record_length: u32,
    pub file_reference: String,
    pub parent_reference: String,
    pub usn: i64,
    pub timestamp_filetime: i64,
    pub reason: u32,
    pub reason_names: Vec<&'static str>,
    pub unknown_reason_bits: u32,
    pub source_info: u32,
    pub security_id: u32,
    pub file_attributes: u32,
    pub filename: String,
    pub filename_utf16: Vec<u16>,
    pub filename_lossy: bool,
}

#[derive(Debug)]
pub struct Batch {
    pub next_usn: i64,
    pub records: Vec<Record>,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        bytes[offset..offset + 2]
            .try_into()
            .expect("validated fixed-width record slice"),
    )
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("validated fixed-width record slice"),
    )
}
fn i64_at(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("validated fixed-width record slice"),
    )
}

pub fn parse_batch(bytes: &[u8], start_usn: i64) -> Result<Batch> {
    ensure!(
        (8..=BUFFER_SIZE).contains(&bytes.len()),
        "invalid journal buffer size {}",
        bytes.len()
    );
    ensure!(start_usn >= 0, "negative requested USN");
    let next_usn = i64_at(bytes, 0);
    ensure!(next_usn >= start_usn, "journal cursor moved backwards");
    let mut offset = 8;
    let mut records = Vec::new();
    let mut previous = None;
    while offset < bytes.len() {
        ensure!(
            bytes.len() - offset >= 8,
            "truncated record header at {offset}"
        );
        let length = u32_at(bytes, offset) as usize;
        ensure!(
            length >= 8 && length % 8 == 0,
            "invalid record length {length} at {offset}"
        );
        ensure!(
            length <= bytes.len() - offset,
            "truncated record at {offset}"
        );
        let record = &bytes[offset..offset + length];
        let major = u16_at(record, 4);
        let minor = u16_at(record, 6);
        let (fixed, id_size, usn_offset) = match major {
            2 => (60, 8, 24),
            3 => (76, 16, 40),
            _ => bail!(
                "unsupported USN record version {major}.{minor} at buffer offset {offset}; cursor not advanced"
            ),
        };
        ensure!(length >= fixed, "truncated V{major} record");
        let filename_length = u16_at(record, fixed - 4) as usize;
        let filename_offset = u16_at(record, fixed - 2) as usize;
        ensure!(
            filename_offset >= fixed
                && filename_offset % 2 == 0
                && filename_length % 2 == 0
                && filename_offset <= length
                && filename_length <= length - filename_offset
                && filename_length <= 510,
            "invalid NTFS filename range at {offset}"
        );
        let usn = i64_at(record, usn_offset);
        ensure!(
            usn >= start_usn && usn < next_usn,
            "record USN outside returned cursor range"
        );
        ensure!(
            previous.is_none_or(|p| usn > p),
            "non-increasing record USNs"
        );
        previous = Some(usn);
        let filename_utf16: Vec<u16> = record[filename_offset..filename_offset + filename_length]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let filename_lossy = String::from_utf16(&filename_utf16).is_err();
        let reason = u32_at(record, usn_offset + 16);
        let (reason_names, unknown_reason_bits) = reasons(reason);
        records.push(Record {
            major_version: major,
            minor_version: minor,
            record_length: length as u32,
            file_reference: hex_id(&record[8..8 + id_size]),
            parent_reference: hex_id(&record[8 + id_size..8 + 2 * id_size]),
            usn,
            timestamp_filetime: i64_at(record, usn_offset + 8),
            reason,
            reason_names,
            unknown_reason_bits,
            source_info: u32_at(record, usn_offset + 20),
            security_id: u32_at(record, usn_offset + 24),
            file_attributes: u32_at(record, usn_offset + 28),
            filename: String::from_utf16_lossy(&filename_utf16),
            filename_utf16,
            filename_lossy,
        });
        offset += length;
    }
    Ok(Batch { next_usn, records })
}

fn hex_id(bytes: &[u8]) -> String {
    bytes.iter().rev().map(|b| format!("{b:02x}")).collect()
}

#[must_use]
pub fn reasons(raw: u32) -> (Vec<&'static str>, u32) {
    const FLAGS: &[(u32, &str)] = &[
        (0x1, "DATA_OVERWRITE"),
        (0x2, "DATA_EXTEND"),
        (0x4, "DATA_TRUNCATION"),
        (0x10, "NAMED_DATA_OVERWRITE"),
        (0x20, "NAMED_DATA_EXTEND"),
        (0x40, "NAMED_DATA_TRUNCATION"),
        (0x100, "FILE_CREATE"),
        (0x200, "FILE_DELETE"),
        (0x400, "EA_CHANGE"),
        (0x800, "SECURITY_CHANGE"),
        (0x1000, "RENAME_OLD_NAME"),
        (0x2000, "RENAME_NEW_NAME"),
        (0x4000, "INDEXABLE_CHANGE"),
        (0x8000, "BASIC_INFO_CHANGE"),
        (0x10000, "HARD_LINK_CHANGE"),
        (0x20000, "COMPRESSION_CHANGE"),
        (0x40000, "ENCRYPTION_CHANGE"),
        (0x80000, "OBJECT_ID_CHANGE"),
        (0x100000, "REPARSE_POINT_CHANGE"),
        (0x200000, "STREAM_CHANGE"),
        (0x400000, "TRANSACTED_CHANGE"),
        (0x800000, "INTEGRITY_CHANGE"),
        (0x80000000, "CLOSE"),
    ];
    let mut unknown = raw;
    let mut names = Vec::new();
    for &(bit, name) in FLAGS {
        unknown &= !bit;
        if raw & bit != 0 {
            names.push(name);
        }
    }
    (names, unknown)
}

#[cfg(any(test, feature = "test-util"))]
pub mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions_combined_reasons_and_lossless_name() {
        for v in [2, 3] {
            let bytes = fixtures::batch(v, 100, 0x80002108, &[65, 0xd800, 37, 49]);
            let batch = parse_batch(&bytes, 100).unwrap();
            let r = &batch.records[0];
            assert_eq!(r.file_reference.len(), if v == 2 { 16 } else { 32 });
            assert_eq!(r.reason_names, ["FILE_CREATE", "RENAME_NEW_NAME", "CLOSE"]);
            assert_eq!(r.unknown_reason_bits, 8);
            assert_eq!(r.filename_utf16, [65, 0xd800, 37, 49]);
            assert!(r.filename_lossy);
        }
    }
    #[test]
    fn rejects_truncation_alignment_offsets_and_versions() {
        let good = fixtures::batch(2, 100, 1, &[65]);
        for length in 0..good.len() {
            if length == 8 {
                continue;
            }
            assert!(parse_batch(&good[..length], 100).is_err(), "{length}");
        }
        for (offset, value) in [(8, 0u8), (8, 63), (12, 4), (64, 3), (66, 0), (66, 255)] {
            let mut bad = good.clone();
            bad[offset] = value;
            assert!(parse_batch(&bad, 100).is_err(), "offset {offset}");
        }
    }
    #[test]
    fn accepts_minor_extension_and_rejects_nonmonotonic_cursors() {
        let mut bytes = fixtures::batch(2, 100, 1, &[65]);
        bytes[14..16].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            parse_batch(&bytes, 100).unwrap().records[0].minor_version,
            1
        );
        assert!(parse_batch(&bytes, 101).is_err());
        bytes[..8].copy_from_slice(&100i64.to_le_bytes());
        assert!(parse_batch(&bytes, 100).is_err());
    }
    #[test]
    fn arbitrary_buffers_do_not_panic() {
        let mut seed = 17u64;
        for length in 0..1024 {
            let bytes: Vec<_> = (0..length)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (seed >> 32) as u8
                })
                .collect();
            let _ = parse_batch(&bytes, 0);
        }
    }
}
