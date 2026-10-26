use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use usnjmon_evtx::{Element, EvtxWriter, Value};

struct Output(PathBuf);

impl Output {
    fn new() -> (Self, File) {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("usnjmon-evtx-{}-{nonce}.evtx", std::process::id()));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        (Self(path), file)
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        fs::remove_file(&self.0).unwrap();
    }
}

fn document() -> Element<'static> {
    let mut root = Element::new("Event");
    root.attributes.push(("Name", "fixture".into()));
    for (name, value) in [
        (
            "Text",
            Value::Text("unicode \u{03bb} \u{1f4c1} <&>%".repeat(128)),
        ),
        ("U8", Value::U8(4)),
        ("U16", Value::U16(1000)),
        ("U32", Value::U32(u32::MAX)),
        ("U64", Value::U64(u64::MAX)),
        ("I64", Value::I64(-1)),
        ("Boolean", Value::Boolean(true)),
        ("Filetime", Value::Filetime(133_000_000_000_000_000)),
        ("Guid", Value::Guid([0x42; 16])),
        ("Hex64", Value::Hex64(0x8000_0000_0000_0001)),
    ] {
        let mut child = Element::new(name);
        child.text = Some(value);
        root.children.push(child);
    }
    root
}

#[test]
fn multi_chunk_output_matches_pre_refactor_bytes() {
    let (output, file) = Output::new();
    let mut writer = EvtxWriter::try_new(file).unwrap();
    let document = document();
    for _ in 0..128 {
        writer.write(133_000_000_000_000_000, &document).unwrap();
    }
    drop(writer.finish().unwrap());
    let bytes = fs::read(&output.0).unwrap();
    assert_eq!(&bytes[..8], b"ElfFile\0");
    assert!(bytes.len() > 4096 + 65536);
    assert_eq!(&bytes[24..32], &129u64.to_le_bytes());
    // Captured from the original writer at a44005e with this fixed document and timestamp.
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        "4cf6fcbe7a66e257c2ff0d32203dd4d145948750708973e76efec5feb59d06bf"
    );
}

#[test]
fn rejected_event_does_not_advance_or_damage_the_writer() {
    let (output, file) = Output::new();
    let mut writer = EvtxWriter::try_new(file).unwrap();
    let mut oversized = Element::new("Event");
    oversized.text = Some(Value::Text("x".repeat(65536)));
    assert!(writer.write(0, &oversized).is_err());
    let valid = document();
    writer.write(1, &valid).unwrap();
    drop(writer.finish().unwrap());
    let bytes = fs::read(&output.0).unwrap();
    assert_eq!(&bytes[24..32], &2u64.to_le_bytes());
    assert_eq!(&bytes[4096 + 512 + 8..4096 + 512 + 16], &1u64.to_le_bytes());
}
