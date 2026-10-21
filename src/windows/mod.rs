pub mod eventlog;
pub mod service;
pub mod volume;

use crate::{
    config::{Config, VolumeConfig},
    engine::{self, Event, Sink},
    journal::parse_batch,
    state::{Checkpoint, FileStore},
    stop::Stop,
};
use anyhow::{Context, Result};
use serde_json::json;
use std::{
    fs::{File, OpenOptions},
    os::windows::fs::OpenOptionsExt,
    path::Path,
    time::{Duration, Instant},
};
use windows_sys::Win32::Foundation::ERROR_JOURNAL_ENTRY_DELETED;

struct Worker {
    config: VolumeConfig,
    checkpoint: Option<Checkpoint>,
    store: FileStore,
    restart: bool,
    invalidated: bool,
    retry_at: Instant,
}

impl Worker {
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
        eprintln!("volume {}: {error:#}", self.config.name);
        sink.write(&Event::new(
            engine::HEALTH,
            json!({
                "volume_name": self.config.name, "configured_path": self.config.path,
                "checkpoint": self.checkpoint, "error": format!("{error:#}"),
                "retry_interval_ms": retry_ms
            }),
        ))?;
        self.restart = true;
        self.retry_at = Instant::now() + Duration::from_millis(retry_ms);
        Ok(())
    }
}

pub fn lock_state(directory: &Path) -> Result<File> {
    anyhow::ensure!(
        directory.is_dir(),
        "state directory does not exist; use the installation script"
    );
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .share_mode(0)
        .open(directory.join("collector.lock"))
        .context("exclusive state lock (another collector may be running)")
}

pub fn run(
    config_path: &Path,
    state_path: &Path,
    stop: Stop,
    ready: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let _lock = lock_state(state_path)?;
    let mut sink = eventlog::EventLog::open()?;
    let result = collect(config_path, state_path, &stop, &mut sink, ready);
    if let Err(error) = &result {
        let event = Event::new(engine::FATAL, json!({"error": format!("{error:#}")}));
        if let Err(report_error) = sink.write(&event) {
            eprintln!("cannot report fatal event: {report_error:#}; original failure: {error:#}");
        }
    }
    result
}

fn collect(
    config_path: &Path,
    state_path: &Path,
    stop: &Stop,
    sink: &mut impl Sink,
    ready: impl FnOnce() -> Result<()>,
) -> Result<()> {
    let config = Config::load(config_path)?;
    let poll = Duration::from_millis(config.poll_interval_ms);
    let interval = Duration::from_millis(config.event_interval_ms);
    let mut workers = Vec::new();
    for volume in config.volumes {
        let store = FileStore::new(state_path, &volume.name);
        let checkpoint = store.load()?;
        workers.push(Worker {
            config: volume,
            checkpoint,
            store,
            restart: true,
            invalidated: false,
            retry_at: Instant::now(),
        });
    }
    sink.write(&Event::new(
        engine::LIFECYCLE,
        json!({"state": "started", "volume_count": workers.len()}),
    ))?;
    ready()?;
    while !stop.requested() {
        for worker in &mut workers {
            if stop.requested() {
                break;
            }
            if Instant::now() < worker.retry_at {
                continue;
            }
            // Only source reads/decoding are retryable. Sink and checkpoint failures stop the collector.
            let read = (|| -> Result<_> {
                let volume = volume::Volume::open(&worker.config.path)?;
                let journal = volume.query(stop)?;
                let plan = engine::reconcile(
                    worker.checkpoint.as_ref(),
                    &volume.identity,
                    &journal.bounds,
                    worker.restart,
                    worker.invalidated,
                )?;
                let bytes = volume.read(plan.checkpoint.next_usn, &journal, stop)?;
                // StartUsn=0 is special: Windows can silently choose a newer oldest record.
                if plan.checkpoint.next_usn == 0 {
                    let after = volume.query(stop)?;
                    anyhow::ensure!(
                        after.bounds.id == journal.bounds.id && after.bounds.oldest()? == 0,
                        "journal bounds changed during zero-cursor read; requery before establishing coverage"
                    );
                }
                let batch = parse_batch(&bytes, plan.checkpoint.next_usn)?;
                Ok((plan, batch))
            })();
            let (plan, batch) = match read {
                Ok(result) => result,
                Err(error) => {
                    if stop.requested() {
                        break;
                    }
                    worker.source_failed(&error, sink, config.retry_interval_ms)?;
                    continue;
                }
            };
            if stop.requested() {
                break;
            }
            let mut checkpoint = engine::apply_plan(plan, sink, &mut worker.store)?;
            worker.restart = false;
            worker.invalidated = false;
            engine::deliver(
                &mut checkpoint,
                &batch,
                sink,
                &mut worker.store,
                stop,
                interval,
            )
            .with_context(|| format!("deliver/checkpoint volume {}", worker.config.name))?;
            worker.checkpoint = Some(checkpoint);
        }
        if stop.wait(poll) {
            break;
        }
    }
    sink.write(&Event::new(engine::LIFECYCLE, json!({"state": "stopped"})))?;
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
        assert_eq!(sink.0[0].id, engine::HEALTH);
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
            worker.restart,
            worker.invalidated,
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

    #[test]
    fn state_lock_excludes_second_collector() {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("lock-test-{}", std::process::id()));
        std::fs::create_dir(&path).unwrap();
        let first = lock_state(&path).unwrap();
        assert!(lock_state(&path).is_err());
        drop(first);
        drop(lock_state(&path).unwrap());
        std::fs::remove_file(path.join("collector.lock")).unwrap();
        std::fs::remove_dir(path).unwrap();
    }
}
