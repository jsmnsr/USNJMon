use crate::{
    journal::Batch,
    state::{Checkpoint, Store},
    stop::Stop,
};
use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::{Value, json};
use std::time::Duration;

pub const COVERAGE: u32 = 101;
pub const GAP: u32 = 102;
pub const CAUGHT_UP: u32 = 103;
pub const HEALTH: u32 = 104;
pub const FATAL: u32 = 105;
pub const LIFECYCLE: u32 = 106;
pub const FILTER_SUMMARY: u32 = 107;

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
    fn finish_batch(&mut self) -> Result<()> {
        Ok(())
    }
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
        Some(coverage_event(
            if reason.is_some() && saved.is_some() {
                GAP
            } else {
                COVERAGE
            },
            reason.unwrap_or("resume"),
            saved,
            volume,
            journal,
            &checkpoint,
        ))
    } else {
        None
    };
    Ok(Plan { checkpoint, notice })
}

fn coverage_event(
    id: u32,
    kind: &str,
    saved: Option<&Checkpoint>,
    volume: &str,
    journal: &Journal,
    checkpoint: &Checkpoint,
) -> Event {
    Event::new(
        id,
        json!({
            "kind": kind,
            "volume_identity": volume, "journal": journal, "previous_checkpoint": saved,
            "resume_usn": checkpoint.next_usn, "backfill_end": checkpoint.backfill_end,
            "coverage": "retained_records_only", "lost_record_count": null
        }),
    )
}

pub fn apply_plan(plan: Plan, sink: &mut impl Sink, store: &mut impl Store) -> Result<Checkpoint> {
    if let Some(event) = &plan.notice {
        sink.write(event)?;
        store.save(&plan.checkpoint)?;
    }
    Ok(plan.checkpoint)
}

pub fn reconcile_live(
    saved: Option<&Checkpoint>,
    volume: &str,
    journal: &Journal,
    restart: bool,
    invalidated: bool,
) -> Result<Plan> {
    if let Some(cp) = saved {
        cp.validate_live()?;
    }
    let mut plan = reconcile(saved, volume, journal, false, invalidated)?;
    let same_range = saved.filter(|cp| {
        cp.volume_identity == volume
            && cp.journal_id == journal.id
            && cp.backfill_end <= journal.next_usn
    });
    if let Some(previous) = same_range {
        plan.checkpoint.backfill_end = previous.backfill_end;
        plan.checkpoint.next_usn = plan.checkpoint.next_usn.max(previous.backfill_end);
    } else {
        plan.checkpoint.next_usn = journal.next_usn;
        plan.checkpoint.backfill_end = journal.next_usn;
    }
    // Persist the exclusive historical boundary before the history worker starts.
    plan.checkpoint.caught_up = true;
    if plan.notice.is_none() && restart {
        plan.notice = reconcile(saved, volume, journal, true, false)?.notice;
    }
    if let Some(notice) = &mut plan.notice {
        if notice.id == COVERAGE {
            notice.payload["kind"] = json!(if saved.is_none() {
                "live_start"
            } else {
                "live_resume"
            });
        }
        notice.payload["resume_usn"] = json!(plan.checkpoint.next_usn);
        notice.payload["backfill_end"] = json!(plan.checkpoint.backfill_end);
        notice.payload["coverage"] = json!("live_from_split_boundary");
    }
    Ok(plan)
}

pub fn reconcile_history(
    saved: Option<&Checkpoint>,
    live: &Checkpoint,
    volume: &str,
    journal: &Journal,
    restart: bool,
    invalidated: bool,
) -> Result<Plan> {
    live.validate_live()?;
    ensure!(
        live.volume_identity == volume
            && live.journal_id == journal.id
            && live.backfill_end <= journal.next_usn,
        "live/history boundary does not match the queried journal"
    );
    let completed = saved.filter(|cp| {
        cp.volume_identity == volume
            && cp.journal_id == journal.id
            && cp.caught_up
            && cp.backfill_end == live.backfill_end
            && cp.next_usn >= live.backfill_end
            && cp.next_usn <= journal.next_usn
    });
    let mut plan = if let Some(cp) = completed {
        cp.validate()?;
        Plan {
            checkpoint: cp.clone(),
            notice: restart
                .then(|| coverage_event(COVERAGE, "history_complete", saved, volume, journal, cp)),
        }
    } else {
        reconcile(saved, volume, journal, restart, invalidated)?
    };
    plan.checkpoint.backfill_end = live.backfill_end;
    if let Some(notice) = &mut plan.notice {
        notice.payload["backfill_end"] = json!(live.backfill_end);
        notice.payload["coverage"] = json!("retained_history_before_live_split");
    }
    Ok(plan)
}

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

    #[test]
    fn live_and_history_cover_disjoint_ranges_and_resume_independently() {
        let mut live_store = Memory::default();
        let mut history_store = Memory::default();
        let mut sink = Memory::default();
        let mut live = apply_plan(
            reconcile_live(None, "volume-a", &journal(), true, false).unwrap(),
            &mut sink,
            &mut live_store,
        )
        .unwrap();
        assert_eq!(live.next_usn, 164);
        assert_eq!(sink.events[0].payload["kind"], "live_start");
        let mut history = apply_plan(
            reconcile_history(None, &live, "volume-a", &journal(), true, false).unwrap(),
            &mut sink,
            &mut history_store,
        )
        .unwrap();
        assert_eq!(history.next_usn, 100);
        let new = parse_batch(&fixtures::batch(2, 164, 0x100, &[66]), 164).unwrap();
        deliver(
            &mut live,
            &new,
            &mut sink,
            &mut live_store,
            &Stop::default(),
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(live.next_usn, 228);
        assert_eq!(
            history.next_usn, 100,
            "live delivery must not wait for history"
        );
        assert_eq!(sink.events.last().unwrap().payload["historical"], false);
        let mut bytes = fixtures::batch(2, 100, 0x200, &[65]);
        bytes[..8].copy_from_slice(&228i64.to_le_bytes());
        bytes.extend_from_slice(&fixtures::batch(2, 164, 0x100, &[66])[8..]);
        let old = historical_batch(parse_batch(&bytes, 100).unwrap(), live.backfill_end);
        assert_eq!(old.records.len(), 1);
        assert_eq!(old.next_usn, 164);
        deliver(
            &mut history,
            &old,
            &mut sink,
            &mut history_store,
            &Stop::default(),
            Duration::ZERO,
        )
        .unwrap();
        assert_eq!(history.next_usn, 164);
        assert!(history.caught_up);
        let records: Vec<_> = sink
            .events
            .iter()
            .filter(|event| crate::schema::RECORD_IDS.contains(&event.id))
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].payload["record"]["usn"], 164);
        assert_eq!(records[1].payload["record"]["usn"], 100);
        assert_eq!(records[1].payload["historical"], true);
        let mut current = journal();
        current.next_usn = 500;
        let resumed = reconcile_live(
            live_store.checkpoint.as_ref(),
            "volume-a",
            &current,
            true,
            false,
        )
        .unwrap();
        assert_eq!(
            resumed.checkpoint.next_usn, 228,
            "do not skip downtime changes"
        );
        assert_eq!(
            resumed.checkpoint.backfill_end, 164,
            "split is immutable on restart"
        );
        assert_eq!(resumed.notice.unwrap().payload["kind"], "live_resume");
        let resumed_history = reconcile_history(
            history_store.checkpoint.as_ref(),
            &live,
            "volume-a",
            &current,
            true,
            false,
        )
        .unwrap();
        assert_eq!(resumed_history.checkpoint.next_usn, 164);
        assert_eq!(resumed_history.checkpoint.backfill_end, 164);
    }

    #[test]
    fn live_split_is_not_persisted_after_notice_failure() {
        let mut store = Memory::default();
        let mut sink = Memory {
            fail: true,
            ..Default::default()
        };
        let plan = reconcile_live(None, "volume-a", &journal(), true, false).unwrap();
        assert!(apply_plan(plan, &mut sink, &mut store).is_err());
        assert!(store.checkpoint.is_none());
    }

    #[test]
    fn lane_gaps_preserve_split_on_wrap_and_replace_it_for_new_journal() {
        let live = reconcile_live(None, "volume-a", &journal(), true, false)
            .unwrap()
            .checkpoint;
        let history = checkpoint();
        let wrapped = Journal {
            id: 1,
            first_usn: 200,
            lowest_valid_usn: 80,
            next_usn: 300,
        };
        let next_live = reconcile_live(Some(&live), "volume-a", &wrapped, false, false).unwrap();
        assert_eq!(next_live.checkpoint.next_usn, 200);
        assert_eq!(next_live.checkpoint.backfill_end, 164);
        assert_eq!(next_live.notice.unwrap().id, GAP);
        let next_history = reconcile_history(
            Some(&history),
            &next_live.checkpoint,
            "volume-a",
            &wrapped,
            false,
            false,
        )
        .unwrap();
        assert_eq!(next_history.checkpoint.next_usn, 200);
        assert_eq!(next_history.checkpoint.backfill_end, 164);
        assert_eq!(next_history.notice.unwrap().id, GAP);
        let replaced = Journal { id: 2, ..wrapped };
        assert!(
            reconcile_history(Some(&history), &live, "volume-a", &replaced, true, false).is_err()
        );
        let replacement = reconcile_live(Some(&live), "volume-a", &replaced, true, false).unwrap();
        assert_eq!(replacement.checkpoint.next_usn, 300);
        assert_eq!(replacement.checkpoint.backfill_end, 300);
        assert_eq!(
            replacement.notice.unwrap().payload["kind"],
            "journal_recreated_or_restamped"
        );
        let history = reconcile_history(
            Some(&history),
            &replacement.checkpoint,
            "volume-a",
            &replaced,
            true,
            false,
        )
        .unwrap();
        assert_eq!(history.checkpoint.next_usn, 200);
        assert_eq!(history.checkpoint.backfill_end, 300);
    }

    #[test]
    fn live_invalid_cursor_cannot_replay_the_historical_range() {
        let live = reconcile_live(None, "volume-a", &journal(), true, false)
            .unwrap()
            .checkpoint;
        let plan = reconcile_live(Some(&live), "volume-a", &journal(), false, true).unwrap();
        assert_eq!(plan.checkpoint.next_usn, 164);
        assert_eq!(plan.checkpoint.backfill_end, 164);
        assert_eq!(
            plan.notice.unwrap().payload["kind"],
            "read_cursor_invalidated"
        );
        let mut malformed = live;
        malformed.next_usn = 100;
        assert!(reconcile_live(Some(&malformed), "volume-a", &journal(), true, false).is_err());
    }

    #[test]
    fn completed_history_does_not_report_false_gaps_when_old_records_wrap() {
        let live = reconcile_live(None, "volume-a", &journal(), true, false)
            .unwrap()
            .checkpoint;
        let completed = live.clone();
        let wrapped = Journal {
            id: 1,
            first_usn: 250,
            lowest_valid_usn: 80,
            next_usn: 300,
        };
        let plan =
            reconcile_history(Some(&completed), &live, "volume-a", &wrapped, false, false).unwrap();
        assert_eq!(plan.checkpoint, completed);
        assert!(plan.notice.is_none());
        let restarted =
            reconcile_history(Some(&completed), &live, "volume-a", &wrapped, true, false).unwrap();
        assert_eq!(
            restarted.notice.unwrap().payload["kind"],
            "history_complete"
        );
        let truncated = Journal {
            next_usn: 140,
            first_usn: 100,
            ..wrapped
        };
        let replacement = reconcile_live(Some(&live), "volume-a", &truncated, true, false)
            .unwrap()
            .checkpoint;
        let history = reconcile_history(
            Some(&completed),
            &replacement,
            "volume-a",
            &truncated,
            true,
            false,
        )
        .unwrap();
        assert_eq!(
            history.notice.unwrap().payload["kind"],
            "cursor_ahead_of_journal"
        );
        assert_eq!(history.checkpoint.next_usn, 100);
        assert_eq!(history.checkpoint.backfill_end, 140);
    }
}
