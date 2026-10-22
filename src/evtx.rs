//! Bounded EVTX output for offline conversions; never registers or writes a channel.
//!
//! File/chunk framing follows the publicly documented EVTX 3.1 layout.
//! Binary XML uses typed template substitutions and chunk-relative name/template tables.

use anyhow::{Result, ensure};
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};

const CHUNK_SIZE: usize = 65536;
const FILE_HEADER_SIZE: usize = 4096;
const CHUNK_HEADER_SIZE: usize = 512;

pub struct EvtxWriter {
    file: File,
    chunk: Vec<u8>,
    used: usize,
    last_record: usize,
    first_id: u64,
    next_id: u64,
    chunks: u32,
}

pub struct Element<'text> {
    pub name: &'text str,
    pub attributes: Vec<(&'text str, Value)>,
    pub text: Option<Value>,
    pub children: Vec<Element<'text>>,
}

pub enum Value {
    Text(String),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    I64(i64),
    Boolean(bool),
    Filetime(u64),
    Guid([u8; 16]),
    Hex64(u64),
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}

impl Value {
    fn encoded(&self) -> (u8, Vec<u8>) {
        match self {
            Self::Text(value) => (
                1,
                value
                    .encode_utf16()
                    .chain(Some(0))
                    .flat_map(u16::to_le_bytes)
                    .collect(),
            ),
            Self::U8(value) => (4, vec![*value]),
            Self::U16(value) => (6, value.to_le_bytes().to_vec()),
            Self::U32(value) => (8, value.to_le_bytes().to_vec()),
            Self::I64(value) => (9, value.to_le_bytes().to_vec()),
            Self::U64(value) => (10, value.to_le_bytes().to_vec()),
            Self::Boolean(value) => (13, u32::from(*value).to_le_bytes().to_vec()),
            Self::Guid(value) => (15, value.to_vec()),
            Self::Filetime(value) => (17, value.to_le_bytes().to_vec()),
            Self::Hex64(value) => (21, value.to_le_bytes().to_vec()),
        }
    }
}

struct Fragment {
    bytes: Vec<u8>,
    names: Vec<(u16, usize)>,
    template_id: u32,
}

impl<'text> Element<'text> {
    #[must_use]
    pub fn new(name: &'text str) -> Self {
        Self {
            name,
            attributes: Vec::new(),
            text: None,
            children: Vec::new(),
        }
    }
}

impl EvtxWriter {
    /// Takes ownership of a new, empty output file. Call `finish` before publishing it.
    pub fn try_new(mut file: File) -> Result<Self> {
        file.write_all(&[0; FILE_HEADER_SIZE])?;
        Ok(Self {
            file,
            chunk: vec![0; CHUNK_SIZE],
            used: CHUNK_HEADER_SIZE,
            last_record: 0,
            first_id: 1,
            next_id: 1,
            chunks: 0,
        })
    }

    pub fn write(&mut self, timestamp: u64, element: &Element<'_>) -> Result<()> {
        let mut data = encode(element, self.used + 24)?;
        let mut size = (24 + data.bytes.len() + 4 + 7) & !7;
        ensure!(
            size <= CHUNK_SIZE - CHUNK_HEADER_SIZE,
            "event is too large for an EVTX chunk"
        );
        if self.used + size > CHUNK_SIZE {
            self.flush_chunk()?;
            data = encode(element, self.used + 24)?;
            size = (24 + data.bytes.len() + 4 + 7) & !7;
        }
        self.last_record = self.used;
        let record = &mut self.chunk[self.used..self.used + size];
        record[..4].copy_from_slice(&[0x2a, 0x2a, 0, 0]);
        record[4..8].copy_from_slice(&(size as u32).to_le_bytes());
        record[8..16].copy_from_slice(&self.next_id.to_le_bytes());
        record[16..24].copy_from_slice(&timestamp.to_le_bytes());
        record[24..24 + data.bytes.len()].copy_from_slice(&data.bytes);
        record[size - 4..].copy_from_slice(&(size as u32).to_le_bytes());
        for (hash, offset) in data.names {
            let bucket = 128 + usize::from(hash % 64) * 4;
            let previous: [u8; 4] = self.chunk[bucket..bucket + 4]
                .try_into()
                .expect("four-byte name bucket");
            self.chunk[offset..offset + 4].copy_from_slice(&previous);
            self.chunk[bucket..bucket + 4].copy_from_slice(&(offset as u32).to_le_bytes());
        }
        let bucket = 384 + (data.template_id % 32) as usize * 4;
        let definition = self.used + 24 + 14;
        let previous: [u8; 4] = self.chunk[bucket..bucket + 4]
            .try_into()
            .expect("four-byte template bucket");
        self.chunk[definition..definition + 4].copy_from_slice(&previous);
        self.chunk[bucket..bucket + 4].copy_from_slice(&(definition as u32).to_le_bytes());
        self.used += size;
        self.next_id += 1;
        Ok(())
    }

    fn flush_chunk(&mut self) -> Result<()> {
        if self.used == CHUNK_HEADER_SIZE {
            return Ok(());
        }
        self.chunk[..8].copy_from_slice(b"ElfChnk\0");
        for offset in [8, 24] {
            self.chunk[offset..offset + 8].copy_from_slice(&self.first_id.to_le_bytes());
        }
        for offset in [16, 32] {
            self.chunk[offset..offset + 8].copy_from_slice(&(self.next_id - 1).to_le_bytes());
        }
        self.chunk[40..44].copy_from_slice(&128u32.to_le_bytes());
        self.chunk[44..48].copy_from_slice(&(self.last_record as u32).to_le_bytes());
        self.chunk[48..52].copy_from_slice(&(self.used as u32).to_le_bytes());
        let records_crc = checksum(&self.chunk[CHUNK_HEADER_SIZE..self.used]);
        self.chunk[52..56].copy_from_slice(&records_crc.to_le_bytes());
        let mut header = self.chunk[..120].to_vec();
        header.extend_from_slice(&self.chunk[128..CHUNK_HEADER_SIZE]);
        self.chunk[124..128].copy_from_slice(&checksum(&header).to_le_bytes());
        self.file.write_all(&self.chunk)?;
        self.chunks = self
            .chunks
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("EVTX chunk count overflow"))?;
        self.chunk.fill(0);
        self.used = CHUNK_HEADER_SIZE;
        self.first_id = self.next_id;
        Ok(())
    }

    pub fn finish(mut self) -> Result<File> {
        self.flush_chunk()?;
        let mut header = [0u8; FILE_HEADER_SIZE];
        header[..8].copy_from_slice(b"ElfFile\0");
        header[16..24].copy_from_slice(&u64::from(self.chunks.saturating_sub(1)).to_le_bytes());
        header[24..32].copy_from_slice(&self.next_id.to_le_bytes());
        header[32..36].copy_from_slice(&128u32.to_le_bytes());
        header[36..38].copy_from_slice(&1u16.to_le_bytes());
        header[38..40].copy_from_slice(&3u16.to_le_bytes());
        header[40..42].copy_from_slice(&(FILE_HEADER_SIZE as u16).to_le_bytes());
        header[42..46].copy_from_slice(&self.chunks.to_le_bytes());
        let crc = checksum(&header[..120]);
        header[124..128].copy_from_slice(&crc.to_le_bytes());
        self.file.seek(SeekFrom::Start(0))?;
        self.file.write_all(&header)?;
        self.file.sync_all()?;
        Ok(self.file)
    }
}

fn checksum(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320u32 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}

fn encode(element: &Element<'_>, base: usize) -> Result<Fragment> {
    let mut bytes = vec![0x0f, 1, 1, 0, 0x0c, 1];
    bytes.extend_from_slice(&[0; 4]);
    bytes.extend_from_slice(&((base + 14) as u32).to_le_bytes());
    bytes.extend_from_slice(&[0; 24]);
    let mut names = Vec::new();
    let mut values = Vec::new();
    bytes.extend_from_slice(&[0x0f, 1, 1, 0]);
    encode_element(element, &mut bytes, base, &mut names, &mut values)?;
    bytes.push(0);
    let length = (bytes.len() - 38) as u32;
    let digest = Sha256::digest(&bytes[38..]);
    let template_id = u32::from_le_bytes(digest[..4].try_into().expect("four-byte template ID"));
    bytes[6..10].copy_from_slice(&template_id.to_le_bytes());
    bytes[18..34].copy_from_slice(&digest[..16]);
    bytes[34..38].copy_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(&(values.len() as u32).to_le_bytes());
    for (kind, value) in &values {
        bytes.extend_from_slice(&u16::try_from(value.len())?.to_le_bytes());
        bytes.extend_from_slice(&[*kind, 0]);
    }
    for (_, value) in values {
        bytes.extend_from_slice(&value);
    }
    bytes.push(0);
    Ok(Fragment {
        bytes,
        names,
        template_id,
    })
}

fn name(
    value: &str,
    bytes: &mut Vec<u8>,
    base: usize,
    names: &mut Vec<(u16, usize)>,
) -> Result<()> {
    let units: Vec<u16> = value.encode_utf16().collect();
    let offset = base + bytes.len() + 4;
    bytes.extend_from_slice(&(offset as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let hash = units.iter().fold(0u16, |hash, unit| {
        hash.wrapping_mul(65599u32 as u16).wrapping_add(*unit)
    });
    names.push((hash, offset));
    bytes.extend_from_slice(&hash.to_le_bytes());
    bytes.extend_from_slice(&u16::try_from(units.len())?.to_le_bytes());
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    Ok(())
}

fn substitution(value: &Value, bytes: &mut Vec<u8>, values: &mut Vec<(u8, Vec<u8>)>) -> Result<()> {
    let index = u16::try_from(values.len())?;
    let encoded = value.encoded();
    ensure!(
        encoded.1.len() <= u16::MAX as usize,
        "EVTX value exceeds substitution limit"
    );
    bytes.push(0x0d);
    bytes.extend_from_slice(&index.to_le_bytes());
    bytes.push(encoded.0);
    values.push(encoded);
    Ok(())
}

fn encode_element(
    element: &Element<'_>,
    bytes: &mut Vec<u8>,
    base: usize,
    names: &mut Vec<(u16, usize)>,
    values: &mut Vec<(u8, Vec<u8>)>,
) -> Result<()> {
    bytes.push(if element.attributes.is_empty() {
        1
    } else {
        0x41
    });
    bytes.extend_from_slice(&u16::MAX.to_le_bytes());
    let size_offset = bytes.len();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    name(element.name, bytes, base, names)?;
    if !element.attributes.is_empty() {
        let attribute_size = bytes.len();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for (index, (key, value)) in element.attributes.iter().enumerate() {
            bytes.push(if index + 1 == element.attributes.len() {
                6
            } else {
                0x46
            });
            name(key, bytes, base, names)?;
            substitution(value, bytes, values)?;
        }
        let length = (bytes.len() - attribute_size - 4) as u32;
        bytes[attribute_size..attribute_size + 4].copy_from_slice(&length.to_le_bytes());
    }
    bytes.push(2);
    if let Some(value) = &element.text {
        substitution(value, bytes, values)?;
    }
    for child in &element.children {
        encode_element(child, bytes, base, names, values)?;
    }
    bytes.push(4);
    let size = (bytes.len() - size_offset - 4) as u32;
    bytes[size_offset..size_offset + 4].copy_from_slice(&size.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_and_bounds() {
        assert_eq!(checksum(b"123456789"), 0xcbf4_3926);
        assert!(
            substitution(
                &Value::Text("x".repeat(65536)),
                &mut Vec::new(),
                &mut Vec::new()
            )
            .is_err()
        );
    }

    #[test]
    fn substitutions_preserve_native_scalar_types() {
        assert_eq!(
            Value::U16(1000).encoded(),
            (6, 1000u16.to_le_bytes().to_vec())
        );
        assert_eq!(
            Value::U32(u32::MAX).encoded(),
            (8, u32::MAX.to_le_bytes().to_vec())
        );
        assert_eq!(Value::Boolean(true).encoded(), (13, vec![1, 0, 0, 0]));
        assert_eq!(
            Value::Filetime(133_000_000_000_000_000).encoded(),
            (17, 133_000_000_000_000_000u64.to_le_bytes().to_vec())
        );
        assert_eq!(Value::I64(-1).encoded(), (9, vec![255; 8]));
    }
}
