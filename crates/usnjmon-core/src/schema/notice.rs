//! Coverage, lifecycle, health and filtering notices in manifest wire order.

use super::values::{checkpoint_fields, i64_field, text, u32_field, u64_field};
use super::{Data, Field};
use crate::event::{self, Event};
use anyhow::{Context, Result, bail};

pub(super) fn append(event: &Event, fields: &mut Vec<Field>) -> Result<()> {
    use Data::{Boolean, Text, U64};
    let p = &event.payload;
    match event.id {
        event::COVERAGE | event::GAP => {
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
        event::CAUGHT_UP => fields.extend([
            Field::new("VolumeIdentity", text(p, "volume_identity")?),
            Field::new("JournalId", U64(u64_field(p, "journal_id")?)),
            Field::new("BackfillEnd", i64_field(p, "backfill_end")?),
            Field::new("NextUsn", i64_field(p, "next_usn")?),
        ]),
        event::HEALTH => {
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
        event::FATAL => fields.push(Field::new("Error", text(p, "error")?)),
        event::LIFECYCLE => fields.extend([
            Field::new("State", text(p, "state")?),
            Field::new("VolumeCount", u32_field(p, "volume_count")?),
        ]),
        event::FILTER_SUMMARY => fields.extend([
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
    Ok(())
}
