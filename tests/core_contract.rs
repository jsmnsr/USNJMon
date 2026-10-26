use anyhow::{Result, ensure};
use std::time::Duration;
use usnjmon_core::checkpoint::{Checkpoint, Store};
use usnjmon_core::engine::{self, Journal, ReconcileContext};
use usnjmon_core::event::{self, Event, Sink};
use usnjmon_core::journal::{self, fixtures};
use usnjmon_core::schema::{self, Data, Field};
use usnjmon_core::stop::Stop;

#[derive(Default)]
struct Destination {
    fields: Vec<Vec<Field>>,
    reject_finish: bool,
}

impl Sink for Destination {
    fn write(&mut self, event: &Event) -> Result<()> {
        self.fields.push(schema::fields(event)?);
        Ok(())
    }

    fn finish_batch(&mut self) -> Result<()> {
        ensure!(!self.reject_finish, "injected batch completion failure");
        Ok(())
    }
}

#[derive(Default)]
struct JsonStore(Option<String>);

impl Store for JsonStore {
    fn save(&mut self, checkpoint: &Checkpoint) -> Result<()> {
        self.0 = Some(serde_json::to_string(checkpoint)?);
        Ok(())
    }
}

#[test]
fn external_sink_failure_replays_schema_fields_before_committing_a_compatible_checkpoint() {
    let plan = engine::reconcile(
        None,
        "fixture-volume",
        &Journal {
            id: 7,
            first_usn: 100,
            next_usn: 164,
            lowest_valid_usn: 0,
        },
        ReconcileContext {
            restart: true,
            invalidated: false,
        },
    )
    .unwrap();
    assert_eq!(plan.notice.as_ref().unwrap().id, event::COVERAGE);
    let mut destination = Destination::default();
    let mut store = JsonStore::default();
    let initial = engine::apply_plan(plan, &mut destination, &mut store).unwrap();
    let persisted = store.0.clone();
    let batch = journal::parse_batch(&fixtures::batch(2, 100, 0x100, &[65]), 100).unwrap();
    let mut checkpoint = initial.clone();
    destination.reject_finish = true;
    assert!(
        engine::deliver(
            &mut checkpoint,
            &batch,
            &mut destination,
            &mut store,
            &Stop::default(),
            Duration::ZERO,
        )
        .is_err()
    );
    assert_eq!(checkpoint, initial);
    assert_eq!(store.0, persisted);
    destination.reject_finish = false;
    engine::deliver(
        &mut checkpoint,
        &batch,
        &mut destination,
        &mut store,
        &Stop::default(),
        Duration::ZERO,
    )
    .unwrap();
    let fields = |index: usize| {
        destination.fields[index]
            .iter()
            .map(|field| (field.name, &field.value))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        fields(1),
        fields(2),
        "replay must preserve every native field"
    );
    assert!(fields(2).contains(&(
        "EventIdentity",
        &Data::Text("fixture-volume:0000000000000007:100".into())
    )));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(store.0.as_ref().unwrap()).unwrap(),
        serde_json::json!({
            "schema_version": 1, "volume_identity": "fixture-volume", "journal_id": 7,
            "next_usn": 164, "backfill_end": 164, "caught_up": true
        })
    );
    let restored: Checkpoint = serde_json::from_str(store.0.as_ref().unwrap()).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored, checkpoint);
}
