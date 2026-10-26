use super::*;
use crate::event;
use crate::journal::{self, fixtures};
use serde_json::json;

fn record(reason: u32) -> Event {
    let batch = journal::parse_batch(
        &fixtures::batch(3, 100, reason, &[65, 0, 10, 0xd800, 37, 49]),
        100,
    )
    .unwrap();
    Event::new(
        category(reason).0,
        json!({
            "record": batch.records[0], "event_identity": "stable-fixture-identity",
            "volume_identity": "fixture-volume", "journal_id": 12345678901234567890u64,
            "historical": true, "backfill_end": 1000, "path_status": "unresolved", "full_path": null
        }),
    )
}

#[test]
fn categories_are_deterministic_and_preserve_combined_reasons() {
    for (reason, id) in [
        (0x100, 1000),
        (0x200, 1001),
        (0x1000, 1002),
        (0x2000, 1003),
        (0x77, 1010),
        (0x400000, 1010),
        (0x800, 1020),
        (0x8000, 1030),
        (0x80000000, 1099),
        (8, 1099),
    ] {
        assert_eq!(category(reason).0, id);
    }
    let event = record(0x80002308);
    assert_eq!(event.id, 1001);
    let fields = fields(&event).unwrap();
    assert!(
        fields
            .iter()
            .any(|f| f.name == "ReasonFlags" && f.value == Data::U32(0x80002308))
    );
    assert!(
        fields
            .iter()
            .any(|f| f.name == "UnknownReasonBits" && f.value == Data::U32(8))
    );
    assert!(fields.iter().any(|f| f.name == "Reasons"
        && f.value == Data::Text("FILE_CREATE | FILE_DELETE | RENAME_NEW_NAME | CLOSE".into())));
    assert!(
        fields.iter().any(|f| f.name == "EventIdentity"
            && f.value == Data::Text("stable-fixture-identity".into()))
    );
    assert!(fields.iter().any(|f| f.name == "FileNameUtf16Hex"
        && f.value == Data::Text("0041 0000 000a d800 0025 0031".into())));
    assert!(
        fields
            .iter()
            .any(|f| f.name == "FileNameDisplayEscaped" && f.value == Data::Boolean(true))
    );
    for field in fields {
        field.value.bytes().unwrap();
    }
}

#[test]
fn native_fields_match_every_manifest_template_in_exact_wire_order() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("resources")
        .join("provider.man");
    let source = std::fs::read_to_string(path).unwrap();
    let manifest = roxmltree::Document::parse(&source).unwrap();
    let mut examples: Vec<_> = [0x100, 0x200, 0x1000, 0x2000, 1, 0x800, 0x8000, 0x80000000]
        .into_iter()
        .map(record)
        .collect();
    let coverage = json!({
        "kind":"first_use","volume_identity":"fixture-volume",
        "journal":{"id":1,"first_usn":0,"lowest_valid_usn":0,"next_usn":100},
        "resume_usn":0,"backfill_end":100,"previous_checkpoint":null,
        "coverage":"retained_records_only","lost_record_count":null
    });
    examples.push(Event::new(event::COVERAGE, coverage.clone()));
    let mut gap = coverage;
    gap["previous_checkpoint"] =
        json!({"volume_identity":"old-volume","journal_id":2,"next_usn":55});
    examples.push(Event::new(event::GAP, gap));
    examples.push(Event::new(event::CAUGHT_UP, json!({"volume_identity":"fixture-volume","journal_id":1,"backfill_end":100,"next_usn":100})));
    examples.push(Event::new(event::HEALTH, json!({"volume_name":"fixture","configured_path":"X:\\","error":"fixture error","retry_interval_ms":1000,"checkpoint":null})));
    examples.push(Event::new(event::FATAL, json!({"error":"fixture failure"})));
    examples.push(Event::new(
        event::LIFECYCLE,
        json!({"state":"stopped","volume_count":1}),
    ));
    examples.push(Event::new(
        event::FILTER_SUMMARY,
        json!({
            "volume_name":"fixture","lane":"live","blacklisted":1,"not_whitelisted":2,
            "unresolved_excluded":3,"unresolved_paths":4
        }),
    ));
    for event in examples {
        let node = manifest
            .descendants()
            .find(|n| {
                n.has_tag_name("event")
                    && n.attribute("value") == Some(event.id.to_string().as_str())
            })
            .unwrap();
        assert_eq!(node.attribute("version"), Some("2"));
        let template = manifest
            .descendants()
            .find(|n| {
                n.has_tag_name("template") && n.attribute("tid") == node.attribute("template")
            })
            .unwrap();
        let expected: Vec<_> = template
            .children()
            .filter(|n| n.has_tag_name("data"))
            .map(|n| (n.attribute("name").unwrap(), n.attribute("inType").unwrap()))
            .collect();
        let actual = fields(&event).unwrap();
        assert_eq!(
            actual
                .iter()
                .map(|f| (f.name, f.value.input_type()))
                .collect::<Vec<_>>(),
            expected,
            "event {}",
            event.id
        );
        assert_eq!(actual[0].value, Data::U32(2));
    }
}

#[test]
fn native_scalars_have_exact_sizes_and_invalid_fields_fail() {
    assert_eq!(Data::U16(513).bytes().unwrap(), [1, 2]);
    assert_eq!(Data::Boolean(true).bytes().unwrap(), [1, 0, 0, 0]);
    assert_eq!(Data::I64(-1).bytes().unwrap(), [255; 8]);
    assert_eq!(Data::Filetime(123).bytes().unwrap(), 123u64.to_le_bytes());
    assert_eq!(Data::Text("A".into()).bytes().unwrap(), [65, 0, 0, 0]);
    assert!(Data::Text("a\0b".into()).bytes().is_err());
    assert!(fields(&Event::new(105, json!({}))).is_err());
    let mut event = record(0x100);
    event.id = 1001;
    assert!(fields(&event).is_err());
}
