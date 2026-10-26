//! Bounded EVTX file/chunk framing, record tables and checksums.

use crate::binxml::{self, Element};
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
        let mut data = binxml::encode(element, self.used + 24)?;
        let mut size = (24 + data.bytes.len() + 4 + 7) & !7;
        ensure!(
            size <= CHUNK_SIZE - CHUNK_HEADER_SIZE,
            "event is too large for an EVTX chunk"
        );
        if self.used + size > CHUNK_SIZE {
            self.flush_chunk()?;
            data = binxml::encode(element, self.used + 24)?;
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

#[cfg(test)]
mod tests {
    #[test]
    fn crc32_known_vector() {
        assert_eq!(super::checksum(b"123456789"), 0xcbf4_3926);
    }
}
