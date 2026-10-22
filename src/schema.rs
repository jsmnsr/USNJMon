use crate::engine::{self, Event};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

pub const VERSION: u8 = 2;
pub const RECORD_IDS: [u32; 8] = [1000, 1001, 1002, 1003, 1010, 1020, 1030, 1099];

#[must_use]
pub fn category(reason: u32) -> (u32, &'static str) {
    for (mask, id, name) in [
        (0x200, 1001, "File/directory deleted"),
        (0x2000, 1003, "Rename - new name"),
        (0x1000, 1002, "Rename - old name"),
        (0x100, 1000, "File/directory created"),
        (0x800, 1020, "Security changed"),
        (0x00400077, 1010, "Data changed"),
        (0x00bfc400, 1030, "Attributes/metadata changed"),
    ] {
        if reason & mask != 0 {
            return (id, name);
        }
    }
    (1099, "Other journal change")
}

#[derive(Debug, PartialEq)]
pub enum Data {
    Text(String),
    U16(u16),
    U32(u32),
    U64(u64),
    I64(i64),
    Boolean(bool),
    Filetime(u64),
}

impl Data {
    #[must_use]
    pub fn input_type(&self) -> &'static str {
        match self {
            Self::Text(_) => "win:UnicodeString",
            Self::U16(_) => "win:UInt16",
            Self::U32(_) => "win:UInt32",
            Self::U64(_) => "win:UInt64",
            Self::I64(_) => "win:Int64",
            Self::Boolean(_) => "win:Boolean",
            Self::Filetime(_) => "win:FILETIME",
        }
    }
    pub fn bytes(&self) -> Result<Vec<u8>> {
        Ok(match self {
            Self::Text(value) => {
                ensure!(
                    !value.contains('\0'),
                    "native event string contains embedded NUL"
                );
                value
                    .encode_utf16()
                    .chain(Some(0))
                    .flat_map(u16::to_le_bytes)
                    .collect()
            }
            Self::U16(v) => v.to_le_bytes().to_vec(),
            Self::U32(v) => v.to_le_bytes().to_vec(),
            Self::U64(v) | Self::Filetime(v) => v.to_le_bytes().to_vec(),
            Self::I64(v) => v.to_le_bytes().to_vec(),
            Self::Boolean(v) => u32::from(*v).to_le_bytes().to_vec(),
        })
    }
}

pub struct Field {
    pub name: &'static str,
    pub value: Data,
}

impl Field {
    fn new(name: &'static str, value: Data) -> Self {
        Self { name, value }
    }
}

fn text(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::Text(
        value
            .get(key)
            .and_then(Value::as_str)
            .with_context(|| format!("missing/invalid text field {key}"))?
            .into(),
    ))
}
fn u64_field(value: &Value, key: &str) -> Result<u64> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing/invalid unsigned field {key}"))
}
fn u32_field(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::U32(u64_field(value, key)?.try_into()?))
}
fn u16_field(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::U16(u64_field(value, key)?.try_into()?))
}
fn i64_field(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::I64(
        value
            .get(key)
            .and_then(Value::as_i64)
            .with_context(|| format!("missing/invalid signed field {key}"))?,
    ))
}
fn bool_field(value: &Value, key: &str) -> Result<Data> {
    Ok(Data::Boolean(
        value
            .get(key)
            .and_then(Value::as_bool)
            .with_context(|| format!("missing/invalid boolean field {key}"))?,
    ))
}

fn checkpoint_fields(value: &Value) -> Result<(Data, Data, Data)> {
    if value.is_null() {
        Ok((Data::Text(String::new()), Data::U64(0), Data::I64(0)))
    } else {
        Ok((
            text(value, "volume_identity")?,
            Data::U64(u64_field(value, "journal_id")?),
            i64_field(value, "next_usn")?,
        ))
    }
}

pub fn fields(event: &Event) -> Result<Vec<Field>> {
    use Data::{Boolean, Filetime, Text, U32, U64};
    let p = &event.payload;
    let mut fields = vec![Field::new("SchemaVersion", U32(VERSION.into()))];
    if RECORD_IDS.contains(&event.id) {
        let r = p.get("record").context("missing record")?;
        let raw = u64_field(r, "reason")?.try_into()?;
        let (id, category) = category(raw);
        ensure!(id == event.id, "record category/ID mismatch");
        let name = r
            .get("filename")
            .and_then(Value::as_str)
            .context("missing filename")?;
        let display: String = name
            .chars()
            .map(|c| {
                if c.is_control() || matches!(c, '\u{fffe}' | '\u{ffff}') {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect();
        let reasons = r
            .get("reason_names")
            .and_then(Value::as_array)
            .context("missing reason names")?
            .iter()
            .map(|v| v.as_str().context("invalid reason name"))
            .collect::<Result<Vec<_>>>()?
            .join(" | ");
        let units = r
            .get("filename_utf16")
            .and_then(Value::as_array)
            .context("missing filename units")?
            .iter()
            .map(|v| -> Result<String> {
                let unit: u16 = v.as_u64().context("invalid UTF-16 unit")?.try_into()?;
                Ok(format!("{unit:04x}"))
            })
            .collect::<Result<Vec<_>>>()?
            .join(" ");
        let attributes: u32 = u64_field(r, "file_attributes")?.try_into()?;
        let full_path = match p.get("full_path").context("missing full_path")? {
            Value::Null => Text(String::new()),
            Value::String(path) => Text(path.clone()),
            _ => bail!("invalid full_path"),
        };
        fields.extend([
            Field::new("Category", Text(category.into())),
            Field::new("FileName", Text(display.clone())),
            Field::new(
                "ObjectType",
                Text(
                    if attributes & 0x10 != 0 {
                        "Directory"
                    } else {
                        "File"
                    }
                    .into(),
                ),
            ),
            Field::new(
                "OriginalTimestamp",
                Filetime(u64_field(r, "timestamp_filetime")?),
            ),
            Field::new("Historical", bool_field(p, "historical")?),
            Field::new("Reasons", Text(reasons)),
            Field::new("ReasonFlags", U32(raw)),
            Field::new("UnknownReasonBits", u32_field(r, "unknown_reason_bits")?),
            Field::new("Usn", i64_field(r, "usn")?),
            Field::new("JournalId", U64(u64_field(p, "journal_id")?)),
            Field::new("VolumeIdentity", text(p, "volume_identity")?),
            Field::new("FileReference", text(r, "file_reference")?),
            Field::new("ParentReference", text(r, "parent_reference")?),
            Field::new("FileAttributes", U32(attributes)),
            Field::new("SourceInfo", u32_field(r, "source_info")?),
            Field::new("SecurityId", u32_field(r, "security_id")?),
            Field::new("MajorVersion", u16_field(r, "major_version")?),
            Field::new("MinorVersion", u16_field(r, "minor_version")?),
            Field::new("RecordLength", u32_field(r, "record_length")?),
            Field::new("BackfillEnd", i64_field(p, "backfill_end")?),
            Field::new("PathStatus", text(p, "path_status")?),
            Field::new("FullPath", full_path),
            Field::new("FileNameUtf16Hex", Text(units)),
            Field::new("FileNameLossy", bool_field(r, "filename_lossy")?),
            Field::new("FileNameDisplayEscaped", Boolean(display != name)),
            Field::new("EventIdentity", text(p, "event_identity")?),
        ]);
    } else {
        match event.id {
            engine::COVERAGE | engine::GAP => {
                let j = p.get("journal").context("missing queried journal")?;
                let previous = p
                    .get("previous_checkpoint")
                    .context("missing previous checkpoint")?;
                let (volume, journal, next) = checkpoint_fields(previous)?;
                fields.extend([
                    Field::new("Kind", text(p, "kind")?),
                    Field::new("VolumeIdentity", text(p, "volume_identity")?),
                    Field::new("JournalId", U64(u64_field(j, "id")?)),
                    Field::new("FirstUsn", i64_field(j, "first_usn")?),
                    Field::new("LowestValidUsn", i64_field(j, "lowest_valid_usn")?),
                    Field::new("NextUsn", i64_field(j, "next_usn")?),
                    Field::new("ResumeUsn", i64_field(p, "resume_usn")?),
                    Field::new("BackfillEnd", i64_field(p, "backfill_end")?),
                    Field::new("HasPreviousCheckpoint", Boolean(!previous.is_null())),
                    Field::new("PreviousVolumeIdentity", volume),
                    Field::new("PreviousJournalId", journal),
                    Field::new("PreviousNextUsn", next),
                    Field::new("Coverage", text(p, "coverage")?),
                    Field::new("LostRecordCount", Text("unknown".into())),
                ]);
            }
            engine::CAUGHT_UP => fields.extend([
                Field::new("VolumeIdentity", text(p, "volume_identity")?),
                Field::new("JournalId", U64(u64_field(p, "journal_id")?)),
                Field::new("BackfillEnd", i64_field(p, "backfill_end")?),
                Field::new("NextUsn", i64_field(p, "next_usn")?),
            ]),
            engine::HEALTH => {
                let checkpoint = p.get("checkpoint").context("missing checkpoint")?;
                let (volume, journal, next) = checkpoint_fields(checkpoint)?;
                fields.extend([
                    Field::new("VolumeName", text(p, "volume_name")?),
                    Field::new("ConfiguredPath", text(p, "configured_path")?),
                    Field::new("Error", text(p, "error")?),
                    Field::new("RetryIntervalMs", U64(u64_field(p, "retry_interval_ms")?)),
                    Field::new("HasCheckpoint", Boolean(!checkpoint.is_null())),
                    Field::new("VolumeIdentity", volume),
                    Field::new("JournalId", journal),
                    Field::new("NextUsn", next),
                ]);
            }
            engine::FATAL => fields.push(Field::new("Error", text(p, "error")?)),
            engine::LIFECYCLE => fields.extend([
                Field::new("State", text(p, "state")?),
                Field::new("VolumeCount", u32_field(p, "volume_count")?),
            ]),
            engine::FILTER_SUMMARY => fields.extend([
                Field::new("VolumeName", text(p, "volume_name")?),
                Field::new("Lane", text(p, "lane")?),
                Field::new("Blacklisted", Data::U64(u64_field(p, "blacklisted")?)),
                Field::new(
                    "NotWhitelisted",
                    Data::U64(u64_field(p, "not_whitelisted")?),
                ),
                Field::new(
                    "UnresolvedExcluded",
                    Data::U64(u64_field(p, "unresolved_excluded")?),
                ),
                Field::new(
                    "UnresolvedPaths",
                    Data::U64(u64_field(p, "unresolved_paths")?),
                ),
            ]),
            _ => bail!("unsupported event ID {}", event.id),
        }
    }
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;
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
            && f.value
                == Data::Text("FILE_CREATE | FILE_DELETE | RENAME_NEW_NAME | CLOSE".into())));
        assert!(fields.iter().any(|f| f.name == "EventIdentity"
            && f.value == Data::Text("stable-fixture-identity".into())));
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
        let manifest =
            roxmltree::Document::parse(include_str!("..\\resources\\provider.man")).unwrap();
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
        examples.push(Event::new(engine::COVERAGE, coverage.clone()));
        let mut gap = coverage;
        gap["previous_checkpoint"] =
            json!({"volume_identity":"old-volume","journal_id":2,"next_usn":55});
        examples.push(Event::new(engine::GAP, gap));
        examples.push(Event::new(engine::CAUGHT_UP, json!({"volume_identity":"fixture-volume","journal_id":1,"backfill_end":100,"next_usn":100})));
        examples.push(Event::new(engine::HEALTH, json!({"volume_name":"fixture","configured_path":"X:\\","error":"fixture error","retry_interval_ms":1000,"checkpoint":null})));
        examples.push(Event::new(
            engine::FATAL,
            json!({"error":"fixture failure"}),
        ));
        examples.push(Event::new(
            engine::LIFECYCLE,
            json!({"state":"stopped","volume_count":1}),
        ));
        examples.push(Event::new(
            engine::FILTER_SUMMARY,
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
}
