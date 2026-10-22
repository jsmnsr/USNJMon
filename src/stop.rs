use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct Stop {
    signal: Arc<Signal>,
}

#[derive(Default)]
struct Signal {
    requested: Mutex<bool>,
    changed: Condvar,
}

impl Stop {
    pub fn request(&self) {
        *self
            .signal
            .requested
            .lock()
            .expect("cancellation lock poisoned") = true;
        self.signal.changed.notify_all();
    }
    #[must_use]
    pub fn requested(&self) -> bool {
        *self
            .signal
            .requested
            .lock()
            .expect("cancellation lock poisoned")
    }
    #[must_use]
    pub fn wait(&self, duration: Duration) -> bool {
        let (stopped, _) = self
            .signal
            .changed
            .wait_timeout_while(
                self.signal
                    .requested
                    .lock()
                    .expect("cancellation lock poisoned"),
                duration,
                |s| !*s,
            )
            .expect("cancellation lock poisoned while waiting");
        *stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_interrupts_long_wait_and_remains_set() {
        let stop = Stop::default();
        let worker_stop = stop.clone();
        let worker = std::thread::spawn(move || worker_stop.wait(Duration::from_secs(30)));
        stop.request();
        assert!(worker.join().unwrap());
        assert!(stop.requested());
        assert!(stop.wait(Duration::from_secs(30)));
    }
}
