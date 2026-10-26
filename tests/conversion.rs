#![cfg(windows)]

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};
use usnjmon::windows::converter::convert;
use usnjmon_core::journal::fixtures;
use usnjmon_core::stop::Stop;
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
            return Err(error)
                .with_context(|| format!("read generated EVTX after {} records", documents.len()));
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
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
