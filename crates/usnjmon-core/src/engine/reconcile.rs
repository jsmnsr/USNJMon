//! Reconcile durable cursors against retained journal bounds and the live/history split.

use crate::checkpoint::{Checkpoint, Store};
use crate::event::{COVERAGE, Event, GAP, Sink};
use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::json;

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

#[derive(Clone, Copy)]
pub struct ReconcileContext {
    pub restart: bool,
    pub invalidated: bool,
}

pub fn reconcile(
    saved: Option<&Checkpoint>,
    volume: &str,
    journal: &Journal,
    context: ReconcileContext,
) -> Result<Plan> {
    let ReconcileContext {
        restart,
        invalidated,
    } = context;
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
    context: ReconcileContext,
) -> Result<Plan> {
    let ReconcileContext {
        restart,
        invalidated,
    } = context;
    if let Some(cp) = saved {
        cp.validate_live()?;
    }
    let mut plan = reconcile(
        saved,
        volume,
        journal,
        ReconcileContext {
            restart: false,
            invalidated,
        },
    )?;
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
        plan.notice = reconcile(
            saved,
            volume,
            journal,
            ReconcileContext {
                restart: true,
                invalidated: false,
            },
        )?
        .notice;
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
    context: ReconcileContext,
) -> Result<Plan> {
    let ReconcileContext {
        restart,
        invalidated,
    } = context;
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
        reconcile(
            saved,
            volume,
            journal,
            ReconcileContext {
                restart,
                invalidated,
            },
        )?
    };
    plan.checkpoint.backfill_end = live.backfill_end;
    if let Some(notice) = &mut plan.notice {
        notice.payload["backfill_end"] = json!(live.backfill_end);
        notice.payload["coverage"] = json!("retained_history_before_live_split");
    }
    Ok(plan)
}
