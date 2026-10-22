use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
pub struct Stop(Arc<(Mutex<bool>, Condvar)>);

impl Stop {
    pub fn request(&self) {
        let (lock, wake) = &*self.0;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }
    pub fn requested(&self) -> bool {
        *self.0.0.lock().unwrap()
    }
    pub fn wait(&self, duration: Duration) -> bool {
        let (lock, wake) = &*self.0;
        let (stopped, _) = wake
            .wait_timeout_while(lock.lock().unwrap(), duration, |s| !*s)
            .unwrap();
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
