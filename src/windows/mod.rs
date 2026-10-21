pub mod eventlog;
pub mod service;
pub mod volume;

use crate::{
    config::{Config, VolumeConfig},
    engine::{self, Event, Sink},
    journal::{Batch, parse_batch},
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
    lane: Lane,
    checkpoint: Option<Checkpoint>,
    store: FileStore,
    restart: bool,
    invalidated: bool,
    retry_at: Instant,
}

enum Lane {
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

struct StopOnExit(Stop);

impl Drop for StopOnExit {
    fn drop(&mut self) {
        self.0.request();
    }
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
        eprintln!(
            "volume {} ({}): {error:#}",
            self.config.name,
            self.lane.name()
        );
        sink.write(&Event::new(
            engine::HEALTH,
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
    let mut sink = eventlog::EventLog::open(&stop)?;
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
    let live_path = state_path.join("live");
    std::fs::create_dir_all(&live_path)
        .context("create private inherited live-checkpoint directory")?;
    let volume_count = config.volumes.len();
    let mut workers = Vec::new();
    for volume in config.volumes {
        let live_store = FileStore::new(&live_path, &volume.name);
        let live_checkpoint = live_store.load()?;
        if let Some(checkpoint) = &live_checkpoint {
            checkpoint.validate_live()?;
        }
        let store = FileStore::new(state_path, &volume.name);
        let checkpoint = store.load()?;
        workers.push(Worker {
            config: volume.clone(),
            lane: Lane::Live,
            checkpoint: live_checkpoint,
            store: live_store,
            restart: true,
            invalidated: false,
            retry_at: Instant::now(),
        });
        let live_store = FileStore::new(&live_path, &volume.name);
        workers.push(Worker {
            config: volume,
            lane: Lane::History { live_store },
            checkpoint,
            store,
            restart: true,
            invalidated: false,
            retry_at: Instant::now(),
        });
    }
    sink.write(&Event::new(
        engine::LIFECYCLE,
        json!({"state": "started", "volume_count": volume_count}),
    ))?;
    std::thread::scope(|scope| -> Result<()> {
        let _cancel_on_unwind = StopOnExit(stop.clone());
        let mut handles = Vec::new();
        for worker in workers {
            handles.push(scope.spawn(move || {
                let _cancel_on_exit = StopOnExit(stop.clone());
                let label = format!("volume {} ({})", worker.config.name, worker.lane.name());
                collect_worker(worker, stop, poll, interval, config.retry_interval_ms)
                    .with_context(|| label)
            }));
        }
        let mut result = ready();
        if result.is_err() {
            stop.request();
        }
        for handle in handles {
            let outcome = handle
                .join()
                .map_err(|_| anyhow::anyhow!("collector worker panicked"))
                .and_then(|result| result);
            if let Err(error) = outcome {
                eprintln!("worker stopped: {error:#}");
                if result.is_ok() {
                    result = Err(error);
                }
            }
        }
        result
    })?;
    sink.write(&Event::new(
        engine::LIFECYCLE,
        json!({"state": "stopped", "volume_count": volume_count}),
    ))?;
    Ok(())
}

fn collect_worker(
    mut worker: Worker,
    stop: &Stop,
    poll: Duration,
    interval: Duration,
    retry_ms: u64,
) -> Result<()> {
    let mut sink = eventlog::EventLog::open(stop)?;
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
                worker.restart,
                worker.invalidated,
            )?
        } else {
            engine::reconcile_live(
                worker.checkpoint.as_ref(),
                &volume.identity,
                &journal.bounds,
                worker.restart,
                worker.invalidated,
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
        let mut batch = parse_batch(&bytes, plan.checkpoint.next_usn)?;
        if live.is_some() {
            batch = engine::historical_batch(batch, plan.checkpoint.backfill_end);
        }
        Ok(Some((plan, batch)))
    })();
    let (plan, batch) = match read {
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

    #[test]
    fn worker_exit_or_panic_cancels_peer_lanes() {
        for panic in [false, true] {
            let stop = Stop::default();
            let peer_stop = stop.clone();
            let peer = std::thread::spawn(move || peer_stop.wait(Duration::from_secs(60)));
            let worker_stop = stop.clone();
            let worker = std::thread::spawn(move || {
                let _cancel = StopOnExit(worker_stop);
                assert!(!panic, "injected worker panic");
            });
            assert_eq!(worker.join().is_err(), panic);
            assert!(peer.join().unwrap());
            assert!(stop.requested());
        }
    }
}
