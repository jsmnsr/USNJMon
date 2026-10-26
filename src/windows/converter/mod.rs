//! Offline conversion. Win32 supplies no-replace publication, which std::fs::rename cannot provide.

use anyhow::{Context, Result, ensure};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufReader, Read, Seek};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use usnjmon_core::event::Event;
use usnjmon_core::schema;
use usnjmon_core::stop::Stop;
use usnjmon_core::usn_file::UsnFile;
use usnjmon_evtx::EvtxWriter;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_READ, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

pub struct Conversion {
    pub records: u64,
    pub padding_bytes: u64,
    pub input_sha256: String,
}

struct PendingOutput {
    path: PathBuf,
}

impl Drop for PendingOutput {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "cannot remove partial conversion {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}

/// Converts a stable, extracted journal without service installation, filtering or host journal access.
/// Never overwrites an existing destination; failed or cancelled output is removed.
pub fn convert(input: &Path, output: &Path, stop: &Stop) -> Result<Conversion> {
    ensure!(
        !output.exists(),
        "output already exists: {}",
        output.display()
    );
    let mut source = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(input)
        .with_context(|| format!("open extracted USN stream read-only: {}", input.display()))?;
    ensure!(
        source.metadata()?.is_file(),
        "input must be an extracted regular $J file"
    );
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        ensure!(
            !stop.requested(),
            "conversion cancelled while hashing input"
        );
        let count = source.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let digest = format!("{:x}", hash.finalize());
    source.rewind()?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .context("output directory must already exist")?;
    let destination = parent.join(output.file_name().context("output needs a filename")?);
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let temporary = parent.join(format!(".usnjmon-{}-{nonce}.partial", std::process::id()));
    let file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .read(true)
        .open(&temporary)?;
    let pending = PendingOutput { path: temporary };
    let mut writer = EvtxWriter::try_new(file)?;
    let mut reader = UsnFile::new(BufReader::with_capacity(65536, source));
    let mut records = 0u64;
    while let Some(item) = reader.next_record(|| stop.requested())? {
        records += 1;
        let timestamp = conversion_time()?;
        let event = Event::new(
            schema::category(item.record.reason).0,
            json!({
                "record": item.record, "event_identity": format!("sha256:{digest}:{}", item.record.usn),
                "volume_identity": "unknown:offline_input", "journal_id": 0,
                "historical": true, "backfill_end": 0, "path_status": "unresolved:offline_input",
                "full_path": null
            }),
        );
        let document = document::document(&event, &digest, item.offset, records, timestamp)?;
        writer.write(timestamp, &document)?;
    }
    ensure!(records > 0, "input contains no USN records");
    ensure!(!stop.requested(), "conversion cancelled");
    drop(writer.finish()?);
    let from: Vec<u16> = pending
        .path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let to: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    ensure!(!to[..to.len() - 1].contains(&0), "output path contains NUL");
    // SAFETY: Both paths are live NUL-terminated buffers. Omitting REPLACE_EXISTING prevents overwrite.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("publish completed EVTX without overwriting");
    }
    Ok(Conversion {
        records,
        padding_bytes: reader.padding_bytes,
        input_sha256: digest,
    })
}

fn conversion_time() -> Result<u64> {
    let duration = SystemTime::now().duration_since(UNIX_EPOCH)?;
    let ticks = duration.as_nanos() / 100 + 116_444_736_000_000_000;
    Ok(ticks.try_into()?)
}

mod document;
