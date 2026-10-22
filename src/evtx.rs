//! Bounded EVTX output for offline conversions; never registers or writes a channel.
//!
//! File/chunk framing follows the publicly documented EVTX 3.1 layout.
//! Binary XML is emitted as self-contained fragments with chunk-relative names.

use anyhow::{Result, ensure};
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
    pub attributes: Vec<(&'text str, String)>,
    pub text: Option<String>,
    pub children: Vec<Element<'text>>,
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
        let mut size = (24 + data.len() + 4 + 7) & !7;
        ensure!(
            size <= CHUNK_SIZE - CHUNK_HEADER_SIZE,
            "event is too large for an EVTX chunk"
        );
        if self.used + size > CHUNK_SIZE {
            self.flush_chunk()?;
            data = encode(element, self.used + 24)?;
            size = (24 + data.len() + 4 + 7) & !7;
        }
        self.last_record = self.used;
        let record = &mut self.chunk[self.used..self.used + size];
        record[..4].copy_from_slice(&[0x2a, 0x2a, 0, 0]);
        record[4..8].copy_from_slice(&(size as u32).to_le_bytes());
        record[8..16].copy_from_slice(&self.next_id.to_le_bytes());
        record[16..24].copy_from_slice(&timestamp.to_le_bytes());
        record[24..24 + data.len()].copy_from_slice(&data);
        record[size - 4..].copy_from_slice(&(size as u32).to_le_bytes());
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

fn encode(element: &Element<'_>, base: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0x0f, 1, 1, 0];
    encode_element(element, &mut bytes, base)?;
    bytes.push(0);
    Ok(bytes)
}

fn name(value: &str, bytes: &mut Vec<u8>, base: usize) -> Result<()> {
    let units: Vec<u16> = value.encode_utf16().collect();
    bytes.extend_from_slice(&((base + bytes.len() + 4) as u32).to_le_bytes());
    bytes.extend_from_slice(&0u32.to_le_bytes());
    let hash = units.iter().fold(0u16, |hash, unit| {
        hash.wrapping_mul(65599u32 as u16).wrapping_add(*unit)
    });
    bytes.extend_from_slice(&hash.to_le_bytes());
    bytes.extend_from_slice(&u16::try_from(units.len())?.to_le_bytes());
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    Ok(())
}

fn text(value: &str, bytes: &mut Vec<u8>) -> Result<()> {
    let units: Vec<u16> = value.encode_utf16().collect();
    bytes.extend_from_slice(&[5, 1]);
    bytes.extend_from_slice(&u16::try_from(units.len())?.to_le_bytes());
    for unit in units {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    Ok(())
}

fn encode_element(element: &Element<'_>, bytes: &mut Vec<u8>, base: usize) -> Result<()> {
    bytes.push(if element.attributes.is_empty() {
        1
    } else {
        0x41
    });
    let size_offset = bytes.len();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    name(element.name, bytes, base)?;
    if !element.attributes.is_empty() {
        let attribute_size = bytes.len();
        bytes.extend_from_slice(&0u32.to_le_bytes());
        for (index, (key, value)) in element.attributes.iter().enumerate() {
            bytes.push(if index + 1 == element.attributes.len() {
                6
            } else {
                0x46
            });
            name(key, bytes, base)?;
            text(value, bytes)?;
        }
        let length = (bytes.len() - attribute_size - 4) as u32;
        bytes[attribute_size..attribute_size + 4].copy_from_slice(&length.to_le_bytes());
    }
    bytes.push(2);
    if let Some(value) = &element.text {
        text(value, bytes)?;
    }
    for child in &element.children {
        encode_element(child, bytes, base)?;
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
        assert!(text(&"x".repeat(65536), &mut Vec::new()).is_err());
    }
}
