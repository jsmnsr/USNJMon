use super::*;

#[test]
fn oldest_retained_and_resume_not_now() {
    let cp = checkpoint();
    assert_eq!(cp.next_usn, 100);
    let mut j = journal();
    j.lowest_valid_usn = 120;
    assert_eq!(
        reconcile(
            None,
            "volume-a",
            &j,
            ReconcileContext {
                restart: true,
                invalidated: false
            }
        )
        .unwrap()
        .checkpoint
        .next_usn,
        120
    );
    j = journal();
    j.next_usn = 300;
    let p = reconcile(
        Some(&cp),
        "volume-a",
        &j,
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap();
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
            ReconcileContext {
                restart: false,
                invalidated: kind == "read_cursor_invalidated",
            },
        )
        .unwrap();
        assert_eq!(p.notice.as_ref().unwrap().id, GAP);
        assert_eq!(p.notice.unwrap().payload["kind"], kind);
        assert_eq!(p.checkpoint.next_usn, j.oldest().unwrap());
    }
}
#[test]
fn gap_write_failure_does_not_persist_new_cursor() {
    let cp = checkpoint();
    let mut j = journal();
    j.id = 99;
    let plan = reconcile(
        Some(&cp),
        "volume-a",
        &j,
        ReconcileContext {
            restart: false,
            invalidated: false,
        },
    )
    .unwrap();
    let mut store = Memory::default();
    let mut sink = Memory {
        fail: true,
        ..Default::default()
    };
    assert!(apply_plan(plan, &mut sink, &mut store).is_err());
    assert!(store.checkpoint.is_none());
}
