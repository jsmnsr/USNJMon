use super::*;

#[test]
fn writes_then_checkpoints_and_replays_on_failures() {
    let batch = journal::parse_batch(&fixtures::batch(2, 100, 0x100, &[65]), 100).unwrap();
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
fn cancellation_preserves_batch_for_replay() {
    let batch = journal::parse_batch(&fixtures::batch(2, 100, 1, &[65]), 100).unwrap();
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
    let batch = journal::parse_batch(&bytes, 100).unwrap();
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
    let batch = journal::parse_batch(&164i64.to_le_bytes(), 164).unwrap();
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
