//! Schema-2 record fields in manifest wire order, with lossless filename metadata.

use super::values::{bool_field, i64_field, text, u16_field, u32_field, u64_field};
use super::{Data, Field, category};
use crate::event::Event;
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

pub(super) fn append(event: &Event, fields: &mut Vec<Field>) -> Result<()> {
    use Data::{Boolean, Filetime, Text, U32, U64};
    let p = &event.payload;
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
    Ok(())
}
