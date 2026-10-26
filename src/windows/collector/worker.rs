//! Per-lane polling, retryable source failures and fatal destination failures.

use super::super::event_log;
use super::super::volume;
use crate::config::VolumeConfig;
use crate::state::FileStore;
use anyhow::{Context, Result};
use serde_json::json;
use std::time::{Duration, Instant};
use usnjmon_core::checkpoint::Checkpoint;
use usnjmon_core::engine;
use usnjmon_core::event::{self, Event, Sink};
use usnjmon_core::filter::{FilteringSink, Policy};
use usnjmon_core::journal::{self, Batch};
use usnjmon_core::stop::Stop;
use windows_sys::Win32::Foundation::ERROR_JOURNAL_ENTRY_DELETED;

pub(super) struct Worker {
    config: VolumeConfig,
    policy: Policy,
    lane: Lane,
    checkpoint: Option<Checkpoint>,
    store: FileStore,
    restart: bool,
    invalidated: bool,
    retry_at: Instant,
}

pub(super) enum Lane {
    Live,
    History { live_store: FileStore },
}

impl Lane {
    fn name(&self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::History { .. } => "history",
        }
    }
}

impl Worker {
    pub(super) fn new(
        config: VolumeConfig,
        policy: Policy,
        lane: Lane,
        store: FileStore,
        checkpoint: Option<Checkpoint>,
    ) -> Self {
        Self {
            config,
            policy,
            lane,
            checkpoint,
            store,
            restart: true,
            invalidated: false,
            retry_at: Instant::now(),
        }
    }

    pub(super) fn label(&self) -> String {
        format!("volume {} ({})", self.config.name, self.lane.name())
    }

    fn source_failed(
        &mut self,
        error: &anyhow::Error,
        sink: &mut impl Sink,
        retry_ms: u64,
    ) -> Result<()> {
        if error
            .downcast_ref::<std::io::Error>()
            .and_then(|e| e.raw_os_error())
            == Some(ERROR_JOURNAL_ENTRY_DELETED as i32)
        {
            self.invalidated = true;
        }
        eprintln!(
            "volume {} ({}): {error:#}",
            self.config.name,
            self.lane.name()
        );
        sink.write(&Event::new(
            event::HEALTH,
            json!({
                "volume_name": format!("{} ({})", self.config.name, self.lane.name()), "configured_path": self.config.path,
                "checkpoint": self.checkpoint, "error": format!("{error:#}"),
                "retry_interval_ms": retry_ms
            }),
        ))?;
        self.restart = true;
        self.retry_at = Instant::now() + Duration::from_millis(retry_ms);
        Ok(())
    }
}

pub(super) fn collect_worker(
    mut worker: Worker,
    stop: &Stop,
    poll: Duration,
    interval: Duration,
    retry_ms: u64,
) -> Result<()> {
    let mut sink = event_log::EventLog::open(stop)?;
    while !stop.requested() {
        if Instant::now() >= worker.retry_at {
            step(&mut worker, stop, &mut sink, interval, retry_ms)?;
        }
        if stop.wait(poll) {
            break;
        }
    }
    Ok(())
}

fn step(
    worker: &mut Worker,
    stop: &Stop,
    sink: &mut impl Sink,
    interval: Duration,
    retry_ms: u64,
) -> Result<()> {
    let live = match &worker.lane {
        Lane::Live => None,
        Lane::History { live_store } => match live_store.load()? {
            Some(checkpoint) => {
                checkpoint.validate_live()?;
                Some(checkpoint)
            }
            None => return Ok(()),
        },
    };
    // Only source reads/decoding are retryable. Sink and checkpoint failures stop the collector.
    let read = (|| -> Result<_> {
        let volume = volume::Volume::open(&worker.config.path)?;
        let journal = volume.query(stop)?;
        let plan = if let Some(live) = &live {
            if live.volume_identity != volume.identity
                || live.journal_id != journal.bounds.id
                || live.backfill_end > journal.bounds.next_usn
            {
                return Ok(None);
            }
            engine::reconcile_history(
                worker.checkpoint.as_ref(),
                live,
                &volume.identity,
                &journal.bounds,
                engine::ReconcileContext {
                    restart: worker.restart,
                    invalidated: worker.invalidated,
                },
            )?
        } else {
            engine::reconcile_live(
                worker.checkpoint.as_ref(),
                &volume.identity,
                &journal.bounds,
                engine::ReconcileContext {
                    restart: worker.restart,
                    invalidated: worker.invalidated,
                },
            )?
        };
        if live.is_some() && plan.checkpoint.next_usn >= plan.checkpoint.backfill_end {
            let next_usn = plan.checkpoint.next_usn;
            return Ok(Some((
                plan,
                Batch {
                    next_usn,
                    records: Vec::new(),
                },
                volume,
            )));
        }
        let bytes = volume.read(plan.checkpoint.next_usn, &journal, stop)?;
        // StartUsn=0 is special: Windows can silently choose a newer oldest record.
        if plan.checkpoint.next_usn == 0 {
            let after = volume.query(stop)?;
            anyhow::ensure!(
                after.bounds.id == journal.bounds.id && after.bounds.oldest()? == 0,
                "journal bounds changed during zero-cursor read; requery before establishing coverage"
            );
        }
        let mut batch = journal::parse_batch(&bytes, plan.checkpoint.next_usn)?;
        if live.is_some() {
            batch = engine::historical_batch(batch, plan.checkpoint.backfill_end);
        }
        Ok(Some((plan, batch, volume)))
    })();
    let (plan, batch, volume) = match read {
        Ok(Some(result)) => result,
        Ok(None) => return Ok(()),
        Err(error) => {
            if stop.requested() {
                return Ok(());
            }
            return worker.source_failed(&error, sink, retry_ms);
        }
    };
    if stop.requested() {
        return Ok(());
    }
    let mut checkpoint = engine::apply_plan(plan, sink, &mut worker.store)?;
    worker.restart = false;
    worker.invalidated = false;
    let mut filtered = FilteringSink::new(
        sink,
        &worker.policy,
        |event: &Event| volume.resolve_record_path(event),
        &worker.config.name,
        worker.lane.name(),
    );
    engine::deliver(
        &mut checkpoint,
        &batch,
        &mut filtered,
        &mut worker.store,
        stop,
        interval,
    )
    .with_context(|| format!("deliver/checkpoint volume {}", worker.config.name))?;
    worker.checkpoint = Some(checkpoint);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Foundation::ERROR_DEVICE_NOT_CONNECTED;

    #[derive(Default)]
    struct Memory(Vec<Event>);
    impl Sink for Memory {
        fn write(&mut self, event: &Event) -> Result<()> {
            self.0.push(event.clone());
            Ok(())
        }
    }

    use std::path::Path;
    #[test]
    fn disappearing_volume_preserves_cursor_and_reports_health_then_resume() {
        let checkpoint = Checkpoint {
            schema_version: 1,
            volume_identity: "fixture-volume".into(),
            journal_id: 5,
            next_usn: 100,
            backfill_end: 100,
            caught_up: true,
        };
        let mut worker = Worker {
            policy: usnjmon_core::filter::PathFilters::default()
                .compile()
                .unwrap(),
            lane: Lane::Live,
            config: VolumeConfig {
                name: "fixture".into(),
                path: r"X:\".into(),
            },
            checkpoint: Some(checkpoint.clone()),
            store: FileStore::new(Path::new("."), "unused"),
            restart: false,
            invalidated: false,
            retry_at: Instant::now(),
        };
        let mut sink = Memory::default();
        let error = std::io::Error::from_raw_os_error(ERROR_DEVICE_NOT_CONNECTED as i32).into();
        worker.source_failed(&error, &mut sink, 10_000).unwrap();
        assert_eq!(worker.checkpoint, Some(checkpoint.clone()));
        assert!(worker.retry_at > Instant::now());
        assert_eq!(sink.0[0].id, event::HEALTH);
        let query = engine::Journal {
            id: 5,
            first_usn: 80,
            next_usn: 200,
            lowest_valid_usn: 0,
        };
        let plan = engine::reconcile(
            worker.checkpoint.as_ref(),
            "fixture-volume",
            &query,
            engine::ReconcileContext {
                restart: worker.restart,
                invalidated: worker.invalidated,
            },
        )
        .unwrap();
        assert_eq!(plan.checkpoint.next_usn, 100);
        assert_eq!(plan.checkpoint.backfill_end, 200);
        assert_eq!(plan.notice.unwrap().payload["kind"], "resume");
        let error = std::io::Error::from_raw_os_error(ERROR_JOURNAL_ENTRY_DELETED as i32).into();
        worker.source_failed(&error, &mut sink, 10_000).unwrap();
        assert!(worker.invalidated);
        assert_eq!(worker.checkpoint, Some(checkpoint));
    }
}
