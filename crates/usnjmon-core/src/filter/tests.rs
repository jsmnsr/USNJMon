use super::*;
use crate::checkpoint::{Checkpoint, Store};
use crate::engine;
use crate::engine::Journal;
use crate::event::{self, Event, Sink};
use crate::journal::{self, fixtures};
use crate::stop::Stop;
use anyhow::{Result, ensure};
use serde_json::json;
use std::time::Duration;

#[derive(Default)]
struct Destination {
    events: Vec<Event>,
    fail_id: Option<u32>,
    saved: Option<Checkpoint>,
}
impl Sink for Destination {
    fn write(&mut self, event: &Event) -> Result<()> {
        ensure!(self.fail_id != Some(event.id), "injected write failure");
        self.events.push(event.clone());
        Ok(())
    }
}
impl Store for Destination {
    fn save(&mut self, checkpoint: &Checkpoint) -> Result<()> {
        self.saved = Some(checkpoint.clone());
        Ok(())
    }
}

#[test]
fn filtered_batches_advance_only_after_summary_and_accepted_writes() {
    let batch = journal::parse_batch(&fixtures::batch(2, 100, 0x100, &[65]), 100).unwrap();
    let initial = engine::reconcile(
        None,
        "fixture",
        &Journal {
            id: 1,
            first_usn: 100,
            lowest_valid_usn: 0,
            next_usn: 164,
        },
        engine::ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap()
    .checkpoint;
    let policy = PathFilters::default().compile().unwrap();
    let mut sink = Destination {
        fail_id: Some(event::FILTER_SUMMARY),
        ..Default::default()
    };
    let mut store = Destination::default();
    let mut checkpoint = initial.clone();
    for failure in [true, false] {
        if !failure {
            sink.fail_id = None;
        }
        let mut filter = FilteringSink::new(
            &mut sink,
            &policy,
            |_: &Event| {
                Ok(ResolvedPath {
                    relative: Some(r"\Windows\Logs\example.log".into()),
                    full: Some(r"\\?\Volume{fixture}\Windows\Logs\example.log".into()),
                    status: "parent_current_only".into(),
                })
            },
            "fixture",
            "history",
        );
        let result = engine::deliver(
            &mut checkpoint,
            &batch,
            &mut filter,
            &mut store,
            &Stop::default(),
            Duration::ZERO,
        );
        if failure {
            assert!(result.is_err());
            assert_eq!(checkpoint, initial);
            assert!(store.saved.is_none());
        } else {
            result.unwrap();
            assert_eq!(checkpoint.next_usn, 164);
            assert_eq!(sink.events[0].id, event::FILTER_SUMMARY);
            assert_eq!(sink.events[0].payload["blacklisted"], 1);
            assert_eq!(sink.events[1].id, event::CAUGHT_UP);
        }
    }
    assert!(
        !sink
            .events
            .iter()
            .any(|e| crate::schema::RECORD_IDS.contains(&e.id))
    );
}

#[test]
fn unresolved_is_visible_health_bypasses_filters_and_record_failures_propagate() {
    let policy = PathFilters {
        whitelist: vec![r"\only".into()],
        ..Default::default()
    }
    .compile()
    .unwrap();
    let mut sink = Destination::default();
    let mut filter = FilteringSink::new(
        &mut sink,
        &policy,
        |_: &Event| Ok(ResolvedPath::unresolved("parent_open_win32_2")),
        "fixture",
        "live",
    );
    filter
        .write(&Event::new(
            1000,
            json!({"full_path":null,"path_status":"unresolved"}),
        ))
        .unwrap();
    filter
        .write(&Event::new(event::HEALTH, json!({"error":"unavailable"})))
        .unwrap();
    filter.finish_batch().unwrap();
    assert_eq!(
        sink.events[0].payload["path_status"],
        "unresolved:parent_open_win32_2"
    );
    assert!(sink.events[0].payload["full_path"].is_null());
    assert_eq!(sink.events[1].id, event::HEALTH);
    assert_eq!(sink.events[2].payload["unresolved_paths"], 1);
    sink.fail_id = Some(1000);
    let mut filter = FilteringSink::new(
        &mut sink,
        &policy,
        |_: &Event| Ok(ResolvedPath::unresolved("missing")),
        "fixture",
        "live",
    );
    assert!(filter.write(&Event::new(1000, json!({}))).is_err());
}

#[test]
fn defaults_preserve_user_activity_and_match_only_complete_components() {
    let policy = PathFilters::default().compile().unwrap();
    for path in [
        r"\Windows\Logs\CBS\CBS.log",
        r"\WINDOWS\TEMP\file",
        r"\ProgramData\UsnEventLog\state\system.json",
        r"\ProgramData\USNJMon\state\system.json",
    ] {
        assert_eq!(policy.decide(Some(path)).unwrap(), Decision::Blacklisted);
    }
    for path in [
        r"\Users\me\file.txt",
        r"\Users\me\AppData\Local\Temp\file",
        r"\Windows\System32\a.dll",
        r"\Windows\LogsBackup\a.log",
        r"\$Recycle.Bin\deleted",
        r"\ProgramData\UsnEventLog-other\a",
    ] {
        assert_eq!(policy.decide(Some(path)).unwrap(), Decision::Include);
    }
    assert_eq!(policy.decide(None).unwrap(), Decision::Include);
}

#[test]
fn blacklist_wins_whitelist_and_unresolved_policy_is_explicit() {
    let policy = PathFilters {
        whitelist: vec![r"\Users".into()],
        blacklist: vec![r"\Users\me\cache".into()],
        unresolved: Unresolved::Exclude,
    }
    .compile()
    .unwrap();
    assert_eq!(
        policy.decide(Some(r"\Users\me\file")).unwrap(),
        Decision::Include
    );
    assert_eq!(
        policy.decide(Some(r"\users\ME\cache\file")).unwrap(),
        Decision::Blacklisted
    );
    assert_eq!(
        policy.decide(Some(r"\Windows\a")).unwrap(),
        Decision::NotWhitelisted
    );
    assert_eq!(policy.decide(None).unwrap(), Decision::Unresolved);
    assert_eq!(normalize_path("/Users/Me/").unwrap(), r"\users\me");
    for bad in [
        "",
        "file",
        r"C:\Users",
        r"\\server\share",
        r"\Users\..\Windows",
        r"\Users\\file",
        r"\Users\*",
        "\\a\0",
    ] {
        assert!(normalize_path(bad).is_err(), "{bad}");
    }
    let empty: PathFilters = serde_json::from_str(r#"{"blacklist":[]}"#).unwrap();
    assert_eq!(
        empty
            .compile()
            .unwrap()
            .decide(Some(r"\Windows\Logs\a"))
            .unwrap(),
        Decision::Include
    );
}
