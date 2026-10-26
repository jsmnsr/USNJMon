//! Deliver complete batches before advancing durable checkpoints.

use crate::checkpoint::{Checkpoint, Store};
use crate::event::{CAUGHT_UP, Event, Sink};
use crate::journal::Batch;
use crate::stop::Stop;
use anyhow::Result;
use serde_json::json;
use std::time::Duration;

#[must_use]
pub fn historical_batch(mut batch: Batch, boundary: i64) -> Batch {
    batch.records.retain(|record| record.usn < boundary);
    batch.next_usn = batch.next_usn.min(boundary);
    batch
}

pub fn deliver(
    checkpoint: &mut Checkpoint,
    batch: &Batch,
    sink: &mut impl Sink,
    store: &mut impl Store,
    stop: &Stop,
    event_interval: Duration,
) -> Result<()> {
    for record in &batch.records {
        if stop.requested() {
            return Ok(());
        }
        sink.write(&Event::new(crate::schema::category(record.reason).0, json!({
            "event_identity": format!("{}:{:016x}:{}", checkpoint.volume_identity, checkpoint.journal_id, record.usn),
            "volume_identity": checkpoint.volume_identity,
            "journal_id": checkpoint.journal_id,
            "historical": record.usn < checkpoint.backfill_end,
            "backfill_end": checkpoint.backfill_end,
            "path_status": "unresolved",
            "full_path": null,
            "record": record
        })))?;
        if stop.wait(event_interval) {
            return Ok(());
        }
    }
    sink.finish_batch()?;
    let mut next = checkpoint.clone();
    next.next_usn = batch.next_usn;
    if !next.caught_up && next.next_usn >= next.backfill_end {
        sink.write(&Event::new(
            CAUGHT_UP,
            json!({
                "volume_identity": next.volume_identity, "journal_id": next.journal_id,
                "backfill_end": next.backfill_end, "next_usn": next.next_usn
            }),
        ))?;
        next.caught_up = true;
    }
    if next != *checkpoint {
        store.save(&next)?;
        *checkpoint = next;
    }
    Ok(())
}
