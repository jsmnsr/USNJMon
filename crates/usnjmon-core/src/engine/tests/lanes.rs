use super::*;

#[test]
fn live_and_history_cover_disjoint_ranges_and_resume_independently() {
    let mut live_store = Memory::default();
    let mut history_store = Memory::default();
    let mut sink = Memory::default();
    let mut live = apply_plan(
        reconcile_live(
            None,
            "volume-a",
            &journal(),
            ReconcileContext {
                restart: true,
                invalidated: false,
            },
        )
        .unwrap(),
        &mut sink,
        &mut live_store,
    )
    .unwrap();
    assert_eq!(live.next_usn, 164);
    assert_eq!(sink.events[0].payload["kind"], "live_start");
    let mut history = apply_plan(
        reconcile_history(
            None,
            &live,
            "volume-a",
            &journal(),
            ReconcileContext {
                restart: true,
                invalidated: false,
            },
        )
        .unwrap(),
        &mut sink,
        &mut history_store,
    )
    .unwrap();
    assert_eq!(history.next_usn, 100);
    let new = journal::parse_batch(&fixtures::batch(2, 164, 0x100, &[66]), 164).unwrap();
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
    let old = historical_batch(
        journal::parse_batch(&bytes, 100).unwrap(),
        live.backfill_end,
    );
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
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
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
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
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
    let plan = reconcile_live(
        None,
        "volume-a",
        &journal(),
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap();
    assert!(apply_plan(plan, &mut sink, &mut store).is_err());
    assert!(store.checkpoint.is_none());
}

#[test]
fn lane_gaps_preserve_split_on_wrap_and_replace_it_for_new_journal() {
    let live = reconcile_live(
        None,
        "volume-a",
        &journal(),
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap()
    .checkpoint;
    let history = checkpoint();
    let wrapped = Journal {
        id: 1,
        first_usn: 200,
        lowest_valid_usn: 80,
        next_usn: 300,
    };
    let next_live = reconcile_live(
        Some(&live),
        "volume-a",
        &wrapped,
        ReconcileContext {
            restart: false,
            invalidated: false,
        },
    )
    .unwrap();
    assert_eq!(next_live.checkpoint.next_usn, 200);
    assert_eq!(next_live.checkpoint.backfill_end, 164);
    assert_eq!(next_live.notice.unwrap().id, GAP);
    let next_history = reconcile_history(
        Some(&history),
        &next_live.checkpoint,
        "volume-a",
        &wrapped,
        ReconcileContext {
            restart: false,
            invalidated: false,
        },
    )
    .unwrap();
    assert_eq!(next_history.checkpoint.next_usn, 200);
    assert_eq!(next_history.checkpoint.backfill_end, 164);
    assert_eq!(next_history.notice.unwrap().id, GAP);
    let replaced = Journal { id: 2, ..wrapped };
    assert!(
        reconcile_history(
            Some(&history),
            &live,
            "volume-a",
            &replaced,
            ReconcileContext {
                restart: true,
                invalidated: false
            }
        )
        .is_err()
    );
    let replacement = reconcile_live(
        Some(&live),
        "volume-a",
        &replaced,
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap();
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
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap();
    assert_eq!(history.checkpoint.next_usn, 200);
    assert_eq!(history.checkpoint.backfill_end, 300);
}

#[test]
fn live_invalid_cursor_cannot_replay_the_historical_range() {
    let live = reconcile_live(
        None,
        "volume-a",
        &journal(),
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap()
    .checkpoint;
    let plan = reconcile_live(
        Some(&live),
        "volume-a",
        &journal(),
        ReconcileContext {
            restart: false,
            invalidated: true,
        },
    )
    .unwrap();
    assert_eq!(plan.checkpoint.next_usn, 164);
    assert_eq!(plan.checkpoint.backfill_end, 164);
    assert_eq!(
        plan.notice.unwrap().payload["kind"],
        "read_cursor_invalidated"
    );
    let mut malformed = live;
    malformed.next_usn = 100;
    assert!(
        reconcile_live(
            Some(&malformed),
            "volume-a",
            &journal(),
            ReconcileContext {
                restart: true,
                invalidated: false
            }
        )
        .is_err()
    );
}

#[test]
fn completed_history_does_not_report_false_gaps_when_old_records_wrap() {
    let live = reconcile_live(
        None,
        "volume-a",
        &journal(),
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap()
    .checkpoint;
    let completed = live.clone();
    let wrapped = Journal {
        id: 1,
        first_usn: 250,
        lowest_valid_usn: 80,
        next_usn: 300,
    };
    let plan = reconcile_history(
        Some(&completed),
        &live,
        "volume-a",
        &wrapped,
        ReconcileContext {
            restart: false,
            invalidated: false,
        },
    )
    .unwrap();
    assert_eq!(plan.checkpoint, completed);
    assert!(plan.notice.is_none());
    let restarted = reconcile_history(
        Some(&completed),
        &live,
        "volume-a",
        &wrapped,
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap();
    assert_eq!(
        restarted.notice.unwrap().payload["kind"],
        "history_complete"
    );
    let truncated = Journal {
        next_usn: 140,
        first_usn: 100,
        ..wrapped
    };
    let replacement = reconcile_live(
        Some(&live),
        "volume-a",
        &truncated,
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap()
    .checkpoint;
    let history = reconcile_history(
        Some(&completed),
        &replacement,
        "volume-a",
        &truncated,
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap();
    assert_eq!(
        history.notice.unwrap().payload["kind"],
        "cursor_ahead_of_journal"
    );
    assert_eq!(history.checkpoint.next_usn, 100);
    assert_eq!(history.checkpoint.backfill_end, 140);
}
