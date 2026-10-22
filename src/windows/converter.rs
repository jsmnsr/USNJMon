//! Offline conversion. Win32 supplies no-replace publication, which std::fs::rename cannot provide.

use super::event_log;
use crate::engine::Event;
use crate::evtx::{Element, EvtxWriter, Value};
use crate::schema;
use crate::schema::Data;
use crate::stop::Stop;
use crate::usn_file::UsnFile;
use anyhow::{Context, Result, ensure};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{BufReader, Read, Seek};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
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
        let document = document(&event, &digest, item.offset, records, timestamp)?;
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

fn field<'text>(name: &'text str, value: Value) -> Element<'text> {
    let mut element = Element::new(name);
    element.text = Some(value);
    element
}

fn document(
    event: &Event,
    digest: &str,
    offset: u64,
    id: u64,
    timestamp: u64,
) -> Result<Element<'static>> {
    let mut root = Element::new("Event");
    root.attributes.push((
        "xmlns",
        "http://schemas.microsoft.com/win/2004/08/events/event".into(),
    ));
    let mut system = Element::new("System");
    let mut provider = Element::new("Provider");
    provider
        .attributes
        .push(("Name", event_log::PROVIDER.into()));
    let guid = event_log::PROVIDER_ID;
    let mut guid_bytes = [0; 16];
    guid_bytes[..4].copy_from_slice(&guid.data1.to_le_bytes());
    guid_bytes[4..6].copy_from_slice(&guid.data2.to_le_bytes());
    guid_bytes[6..8].copy_from_slice(&guid.data3.to_le_bytes());
    guid_bytes[8..].copy_from_slice(&guid.data4);
    provider.attributes.push(("Guid", Value::Guid(guid_bytes)));
    system.children.push(provider);
    for (name, value) in [
        ("EventID", Value::U16(event.id.try_into()?)),
        ("Version", Value::U8(schema::VERSION)),
        ("Level", Value::U8(4)),
        ("Task", Value::U16(0)),
        ("Opcode", Value::U8(0)),
        ("Keywords", Value::Hex64(1)),
    ] {
        system.children.push(field(name, value));
    }
    let mut time = Element::new("TimeCreated");
    time.attributes
        .push(("SystemTime", Value::Filetime(timestamp)));
    system.children.push(time);
    system.children.push(field("EventRecordID", Value::U64(id)));
    system
        .children
        .push(field("Channel", event_log::LOG.into()));
    system
        .children
        .push(field("Computer", "offline-conversion".into()));
    root.children.push(system);
    let mut data = Element::new("EventData");
    for item in schema::fields(event)? {
        let value = match item.value {
            Data::Text(value) => Value::Text(value),
            Data::U16(value) => Value::U16(value),
            Data::U32(value) => Value::U32(value),
            Data::U64(value) => Value::U64(value),
            Data::I64(value) => Value::I64(value),
            Data::Boolean(value) => Value::Boolean(value),
            Data::Filetime(value) => Value::Filetime(value),
        };
        let mut element = field("Data", value);
        element.attributes.push(("Name", item.name.into()));
        data.children.push(element);
    }
    for (name, value) in [
        ("SourceKind", "extracted_usn_stream".into()),
        ("InputSha256", digest.into()),
        ("InputOffset", Value::U64(offset)),
        ("JournalIdentityStatus", "not_present_in_input".into()),
    ] {
        let mut element = field("Data", value);
        element.attributes.push(("Name", name.into()));
        data.children.push(element);
    }
    root.children.push(data);
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::fixtures;
    use std::io;
    use std::ptr;
    use windows_sys::Win32::System::EventLog::{
        EvtClose, EvtNext, EvtQuery, EvtQueryFilePath, EvtQueryForwardDirection, EvtRender,
        EvtRenderEventXml,
    };

    struct EventHandle(isize);

    impl Drop for EventHandle {
        fn drop(&mut self) {
            // SAFETY: This wrapper exclusively owns a successful query/event handle.
            if unsafe { EvtClose(self.0) } == 0 {
                eprintln!(
                    "close native test event handle: {}",
                    io::Error::last_os_error()
                );
            }
        }
    }

    fn read_native(path: &Path, query: &str) -> Result<Vec<String>> {
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let query: Vec<u16> = query.encode_utf16().chain(Some(0)).collect();
        // SAFETY: NUL-terminated buffers remain valid; flags explicitly select an owned file, not a channel.
        let handle = unsafe {
            EvtQuery(
                0,
                path.as_ptr(),
                query.as_ptr(),
                EvtQueryFilePath | EvtQueryForwardDirection,
            )
        };
        ensure!(
            handle != 0,
            "query generated EVTX: {}",
            io::Error::last_os_error()
        );
        let handle = EventHandle(handle);
        let mut documents = Vec::new();
        loop {
            let mut event = 0;
            let mut returned = 0;
            // SAFETY: Query is live and output storage holds one event handle.
            if unsafe { EvtNext(handle.0, 1, &mut event, 1000, 0, &mut returned) } == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(259) {
                    break;
                }
                return Err(error).with_context(|| {
                    format!("read generated EVTX after {} records", documents.len())
                });
            }
            let event = EventHandle(event);
            let mut used = 0;
            let mut properties = 0;
            // SAFETY: Zero buffer size requests required bytes; event and out parameters are live.
            unsafe {
                EvtRender(
                    0,
                    event.0,
                    EvtRenderEventXml,
                    0,
                    ptr::null_mut(),
                    &mut used,
                    &mut properties,
                );
            }
            ensure!(
                io::Error::last_os_error().raw_os_error() == Some(122),
                "query rendered XML size"
            );
            let mut buffer = vec![0u16; used as usize / 2];
            ensure!(
                // SAFETY: Buffer is aligned and has the byte capacity Windows just requested.
                unsafe {
                    EvtRender(
                        0,
                        event.0,
                        EvtRenderEventXml,
                        used,
                        buffer.as_mut_ptr().cast(),
                        &mut used,
                        &mut properties,
                    )
                } != 0,
                "render generated EVTX: {}",
                io::Error::last_os_error()
            );
            documents.push(String::from_utf16(&buffer[..used as usize / 2 - 1])?);
        }
        Ok(documents)
    }

    #[test]
    fn conversion_roundtrips_native_xml_across_chunks_and_never_clobbers() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "conversion-test-{}-{}",
                std::process::id(),
                conversion_time().unwrap()
            ));
        fs::create_dir(&root).unwrap();
        let input = root.join("input.usnj");
        let output = root.join("output.evtx");
        let mut source = vec![0; 65536];
        let mut filename: Vec<u16> = "a<&>\u{03bb}\u{1f4c1}.txt".encode_utf16().collect();
        filename.extend_from_slice(&[0, 10, 0xd800, 0xfffe, 0xffff]);
        for index in 0..128 {
            let version = if index % 2 == 0 { 2 } else { 3 };
            source.extend_from_slice(
                &fixtures::batch(version, 65536 + index * 1024, 0x80000100, &filename)[8..],
            );
        }
        source.extend_from_slice(&[0; 24]);
        fs::write(&input, &source).unwrap();
        let stop = Stop::default();
        let result = convert(&input, &output, &stop).unwrap();
        assert_eq!(result.records, 128);
        assert_eq!(result.padding_bytes, 65560);
        assert_eq!(
            result.input_sha256,
            format!("{:x}", Sha256::digest(&source))
        );
        assert!(fs::metadata(&output).unwrap().len() > 4096 + 65536);
        let xml = read_native(&output, "*[System[EventID=1000]]").unwrap();
        assert_eq!(xml.len(), 128);
        for (index, value) in xml.iter().enumerate() {
            let document = roxmltree::Document::parse(value).unwrap();
            let field = |name| {
                document
                    .descendants()
                    .find(|node| node.has_tag_name("Data") && node.attribute("Name") == Some(name))
                    .unwrap()
                    .text()
                    .unwrap_or("")
            };
            assert_eq!(
                field("FileName"),
                "a<&>\u{03bb}\u{1f4c1}.txt\\u{0}\\n\u{fffd}\\u{fffe}\\u{ffff}"
            );
            assert_eq!(field("FileNameLossy"), "true");
            assert_eq!(field("FileNameDisplayEscaped"), "true");
            assert!(field("FileNameUtf16Hex").ends_with("0000 000a d800 fffe ffff"));
            assert_eq!(field("Usn"), (65536 + index * 1024).to_string());
            assert_eq!(field("JournalIdentityStatus"), "not_present_in_input");
            assert_eq!(field("InputSha256"), result.input_sha256);
            assert_eq!(field("OriginalTimestamp"), "2022-06-18T04:26:40.0000000Z");
            assert_eq!(field("Historical"), "true");
            assert_eq!(field("ReasonFlags"), "2147483904");
            let time = document
                .descendants()
                .find(|node| node.has_tag_name("TimeCreated"))
                .unwrap();
            assert_ne!(
                time.attribute("SystemTime").unwrap(),
                field("OriginalTimestamp")
            );
        }
        let preserved = fs::read(&output).unwrap();
        assert!(convert(&input, &input, &stop).is_err());
        assert_eq!(fs::read(&input).unwrap(), source);
        assert!(convert(&input, &output, &stop).is_err());
        assert_eq!(fs::read(&output).unwrap(), preserved);
        fs::remove_file(&output).unwrap();
        source.extend_from_slice(&[1; 8]);
        fs::write(&input, &source).unwrap();
        assert!(convert(&input, &output, &stop).is_err());
        assert!(!output.exists());
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            1,
            "partial output must be removed"
        );
        fs::write(&input, [0u8; 16]).unwrap();
        assert!(convert(&input, &output, &stop).is_err());
        stop.request();
        assert!(convert(&input, &output, &stop).is_err());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
        assert!(convert(&root.join("absent.usnj"), &output, &Stop::default()).is_err());
        fs::remove_file(input).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
