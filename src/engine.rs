use crate::{
    journal::Batch,
    state::{Checkpoint, Store},
    stop::Stop,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

pub const RECORD: u32 = 100;
pub const COVERAGE: u32 = 101;
pub const GAP: u32 = 102;
pub const CAUGHT_UP: u32 = 103;
pub const HEALTH: u32 = 104;
pub const FATAL: u32 = 105;
pub const LIFECYCLE: u32 = 106;

#[derive(Clone, Debug)]
pub struct Event {
    pub id: u32,
    pub payload: Value,
}

impl Event {
    pub fn new(id: u32, mut payload: Value) -> Self {
        payload["schema_version"] = json!(1);
        Self { id, payload }
    }
}

pub trait Sink {
    fn write(&mut self, event: &Event) -> Result<()>;
}

#[derive(Clone, Debug, Serialize)]
pub struct Journal {
    pub id: u64,
    pub first_usn: i64,
    pub next_usn: i64,
    pub lowest_valid_usn: i64,
}

impl Journal {
    pub fn oldest(&self) -> Result<i64> {
        let oldest = self.first_usn.max(self.lowest_valid_usn);
        ensure!(
            self.first_usn >= 0 && self.lowest_valid_usn >= 0 && self.next_usn >= oldest,
            "invalid queried journal bounds"
        );
        Ok(oldest)
    }
}

pub struct Plan {
    pub checkpoint: Checkpoint,
    pub notice: Option<Event>,
}

pub fn reconcile(
    saved: Option<&Checkpoint>,
    volume: &str,
    journal: &Journal,
    restart: bool,
    invalidated: bool,
) -> Result<Plan> {
    let oldest = journal.oldest()?;
    if let Some(cp) = saved {
        cp.validate()?;
    }
    let reason = match saved {
        None => Some("first_use"),
        Some(cp) if cp.volume_identity != volume => Some("volume_replaced"),
        Some(cp) if cp.journal_id != journal.id => Some("journal_recreated_or_restamped"),
        Some(cp) if cp.next_usn < oldest => Some("retained_history_wrapped"),
        Some(cp) if cp.next_usn > journal.next_usn => Some("cursor_ahead_of_journal"),
        Some(_) if invalidated => Some("read_cursor_invalidated"),
        _ => None,
    };
    let mut checkpoint = match (saved, reason) {
        (Some(cp), None) => cp.clone(),
        _ => Checkpoint {
            schema_version: 1,
            volume_identity: volume.into(),
            journal_id: journal.id,
            next_usn: oldest,
            backfill_end: journal.next_usn,
            caught_up: false,
        },
    };
    if restart {
        checkpoint.backfill_end = journal.next_usn;
        checkpoint.caught_up = false;
    }
    let notice = if reason.is_some() || restart {
        Some(Event::new(
            if reason.is_some() && saved.is_some() {
                GAP
            } else {
                COVERAGE
            },
            json!({
                "kind": reason.unwrap_or("resume"),
                "volume_identity": volume, "journal": journal, "previous_checkpoint": saved,
                "resume_usn": checkpoint.next_usn, "backfill_end": checkpoint.backfill_end,
                "coverage": "retained_records_only",
                "lost_record_count": null
            }),
        ))
    } else {
        None
    };
    Ok(Plan { checkpoint, notice })
}

pub fn apply_plan(plan: Plan, sink: &mut impl Sink, store: &mut impl Store) -> Result<Checkpoint> {
    if let Some(event) = &plan.notice {
        sink.write(event)?;
        store.save(&plan.checkpoint)?;
    }
    Ok(plan.checkpoint)
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
        sink.write(&Event::new(RECORD, json!({
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{fixtures, parse_batch};
    use anyhow::bail;

    #[derive(Default)]
    struct Memory {
        events: Vec<Event>,
        checkpoint: Option<Checkpoint>,
        fail: bool,
        fail_after: Option<usize>,
    }
    impl Sink for Memory {
        fn write(&mut self, event: &Event) -> Result<()> {
            if self.fail || self.fail_after == Some(self.events.len()) {
                bail!("injected Event Log failure");
            }
            self.events.push(event.clone());
            Ok(())
        }
    }
    impl Store for Memory {
        fn save(&mut self, cp: &Checkpoint) -> Result<()> {
            if self.fail {
                bail!("injected disk failure");
            }
            self.checkpoint = Some(cp.clone());
            Ok(())
        }
    }
    fn journal() -> Journal {
        Journal {
            id: 1,
            first_usn: 100,
            lowest_valid_usn: 80,
            next_usn: 164,
        }
    }
    fn checkpoint() -> Checkpoint {
        reconcile(None, "volume-a", &journal(), true, false)
            .unwrap()
            .checkpoint
    }

    #[test]
    fn oldest_retained_and_resume_not_now() {
        let cp = checkpoint();
        assert_eq!(cp.next_usn, 100);
        let mut j = journal();
        j.lowest_valid_usn = 120;
        assert_eq!(
            reconcile(None, "volume-a", &j, true, false)
                .unwrap()
                .checkpoint
                .next_usn,
            120
        );
        j = journal();
        j.next_usn = 300;
        let p = reconcile(Some(&cp), "volume-a", &j, true, false).unwrap();
        assert_eq!(p.checkpoint.next_usn, 100);
        assert_eq!(p.checkpoint.backfill_end, 300);
        assert_eq!(p.notice.unwrap().payload["kind"], "resume");
    }
    #[test]
    fn explicit_gap_for_wrap_recreation_identity_and_invalid_cursor() {
        let cp = checkpoint();
        for kind in [
            "retained_history_wrapped",
            "journal_recreated_or_restamped",
            "volume_replaced",
            "cursor_ahead_of_journal",
            "read_cursor_invalidated",
        ] {
            let mut j = journal();
            let mut c = cp.clone();
            let mut volume = "volume-a";
            match kind {
                "retained_history_wrapped" => j.first_usn = 120,
                "journal_recreated_or_restamped" => j.id = 2,
                "volume_replaced" => volume = "volume-b",
                "cursor_ahead_of_journal" => c.next_usn = 500,
                _ => {}
            }
            let p = reconcile(
                Some(&c),
                volume,
                &j,
                false,
                kind == "read_cursor_invalidated",
            )
            .unwrap();
            assert_eq!(p.notice.as_ref().unwrap().id, GAP);
            assert_eq!(p.notice.unwrap().payload["kind"], kind);
            assert_eq!(p.checkpoint.next_usn, j.oldest().unwrap());
        }
    }
    #[test]
    fn writes_then_checkpoints_and_replays_on_failures() {
        let batch = parse_batch(&fixtures::batch(2, 100, 0x100, &[65]), 100).unwrap();
        let mut cp = checkpoint();
        let original = cp.clone();
        let mut sink = Memory {
            fail: true,
            ..Default::default()
        };
        let mut store = Memory::default();
        assert!(
            deliver(
                &mut cp,
                &batch,
                &mut sink,
                &mut store,
                &Stop::default(),
                Duration::ZERO
            )
            .is_err()
        );
        assert_eq!(cp, original);
        assert!(store.checkpoint.is_none());
        sink.fail = false;
        store.fail = true;
        assert!(
            deliver(
                &mut cp,
                &batch,
                &mut sink,
                &mut store,
                &Stop::default(),
                Duration::ZERO
            )
            .is_err()
        );
        let first_identity = sink.events[0].payload["event_identity"].clone();
        assert_eq!(cp, original);
        store.fail = false;
        deliver(
            &mut cp,
            &batch,
            &mut sink,
            &mut store,
            &Stop::default(),
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(sink.events[2].payload["event_identity"], first_identity);
        assert_eq!(store.checkpoint, Some(cp.clone()));
        assert!(cp.caught_up);
        assert_eq!(cp.next_usn, batch.next_usn);
    }
    #[test]
    fn gap_write_failure_does_not_persist_new_cursor() {
        let cp = checkpoint();
        let mut j = journal();
        j.id = 99;
        let plan = reconcile(Some(&cp), "volume-a", &j, false, false).unwrap();
        let mut store = Memory::default();
        let mut sink = Memory {
            fail: true,
            ..Default::default()
        };
        assert!(apply_plan(plan, &mut sink, &mut store).is_err());
        assert!(store.checkpoint.is_none());
    }
    #[test]
    fn cancellation_preserves_batch_for_replay() {
        let batch = parse_batch(&fixtures::batch(2, 100, 1, &[65]), 100).unwrap();
        let mut cp = checkpoint();
        let stop = Stop::default();
        stop.request();
        let mut sink = Memory::default();
        let mut store = Memory::default();
        deliver(
            &mut cp,
            &batch,
            &mut sink,
            &mut store,
            &stop,
            Duration::ZERO,
        )
        .unwrap();
        assert!(sink.events.is_empty());
        assert!(store.checkpoint.is_none());
        assert_eq!(cp.next_usn, 100);
    }

    #[test]
    fn partial_write_failure_and_mixed_backfill_replay() {
        let mut bytes = fixtures::batch(2, 100, 0x100, &[65]);
        let second = fixtures::batch(2, 164, 0x2000, &[66]);
        bytes[..8].copy_from_slice(&second[..8]);
        bytes.extend_from_slice(&second[8..]);
        let batch = parse_batch(&bytes, 100).unwrap();
        let mut cp = checkpoint();
        let mut sink = Memory {
            fail_after: Some(1),
            ..Default::default()
        };
        let mut store = Memory::default();
        assert!(
            deliver(
                &mut cp,
                &batch,
                &mut sink,
                &mut store,
                &Stop::default(),
                Duration::ZERO
            )
            .is_err()
        );
        assert_eq!(cp.next_usn, 100);
        assert!(store.checkpoint.is_none());
        sink.fail_after = None;
        deliver(
            &mut cp,
            &batch,
            &mut sink,
            &mut store,
            &Stop::default(),
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(
            sink.events[0].payload["event_identity"],
            sink.events[1].payload["event_identity"]
        );
        assert_eq!(sink.events[1].payload["historical"], true);
        assert_eq!(sink.events[2].payload["historical"], false);
        assert_eq!(cp.next_usn, 228);
    }

    #[test]
    fn idle_poll_does_not_rewrite_checkpoint() {
        let mut cp = checkpoint();
        cp.next_usn = 164;
        cp.caught_up = true;
        let mut sink = Memory::default();
        let mut store = Memory::default();
        let batch = parse_batch(&164i64.to_le_bytes(), 164).unwrap();
        deliver(
            &mut cp,
            &batch,
            &mut sink,
            &mut store,
            &Stop::default(),
            Duration::ZERO,
        )
        .unwrap();
        assert!(sink.events.is_empty());
        assert!(store.checkpoint.is_none());
    }
}
