//! Supervise independent live/history workers under one exclusive state lock.

mod worker;
use super::discovery;
use super::event_log;
use crate::config::Config;
use crate::state::FileStore;
use anyhow::{Context, Result};
use serde_json::json;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use std::time::Duration;
use usnjmon_core::event::{self, Event, Sink};
use usnjmon_core::stop::Stop;
use worker::{Lane, Worker};

struct StopOnExit(Stop);

impl Drop for StopOnExit {
    fn drop(&mut self) {
        self.0.request();
    }
}

fn lock_state(directory: &Path) -> Result<File> {
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
    let mut sink = event_log::EventLog::open(&stop)?;
    let result = collect(config_path, state_path, &stop, &mut sink, ready);
    if let Err(error) = &result {
        let event = Event::new(event::FATAL, json!({"error": format!("{error:#}")}));
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
    let policy = config.path_filters.compile()?;
    let selection = discovery::select(&config.volumes)?;
    for (path, error) in selection.warnings {
        sink.write(&Event::new(
            event::HEALTH,
            json!({
                "volume_name": "discovery", "configured_path": path,
                "checkpoint": null, "error": error, "retry_interval_ms": 0
            }),
        ))?;
    }
    let poll = Duration::from_millis(config.poll_interval_ms);
    let interval = Duration::from_millis(config.event_interval_ms);
    let live_path = state_path.join("live");
    std::fs::create_dir_all(&live_path)
        .context("create private inherited live-checkpoint directory")?;
    let volume_count = selection.volumes.len();
    let mut workers = Vec::new();
    for volume in selection.volumes {
        let live_store = FileStore::new(&live_path, &volume.name);
        let live_checkpoint = live_store.load()?;
        if let Some(checkpoint) = &live_checkpoint {
            checkpoint.validate_live()?;
        }
        let store = FileStore::new(state_path, &volume.name);
        let checkpoint = store.load()?;
        workers.push(Worker::new(
            volume.clone(),
            policy.clone(),
            Lane::Live,
            live_store,
            live_checkpoint,
        ));
        let live_store = FileStore::new(&live_path, &volume.name);
        workers.push(Worker::new(
            volume,
            policy.clone(),
            Lane::History { live_store },
            store,
            checkpoint,
        ));
    }
    sink.write(&Event::new(
        event::LIFECYCLE,
        json!({"state": "started", "volume_count": volume_count}),
    ))?;
    std::thread::scope(|scope| -> Result<()> {
        let _cancel_on_unwind = StopOnExit(stop.clone());
        let mut handles = Vec::new();
        for worker in workers {
            handles.push(scope.spawn(move || {
                let _cancel_on_exit = StopOnExit(stop.clone());
                let label = worker.label();
                worker::collect_worker(worker, stop, poll, interval, config.retry_interval_ms)
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
        event::LIFECYCLE,
        json!({"state": "stopped", "volume_count": volume_count}),
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
