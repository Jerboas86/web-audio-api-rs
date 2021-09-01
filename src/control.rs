//! User controls for audio playback (play/pause/loop)

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::AtomicF64;

/// Helper struct to start and stop audio streams
#[derive(Clone, Debug)]
pub struct Scheduler {
    start: Arc<AtomicF64>,
    stop: Arc<AtomicF64>,
}

impl Scheduler {
    /// Create a new Scheduler. Initial playback state will be: inactive.
    pub fn new() -> Self {
        Self {
            start: Arc::new(AtomicF64::new(f64::MAX)),
            stop: Arc::new(AtomicF64::new(f64::MAX)),
        }
    }

    /// Check if the stream should be active at this timestamp
    pub fn is_active(&self, ts: f64) -> bool {
        ts >= self.start.load() && ts < self.stop.load()
    }

    /// Schedule playback start at this timestamp
    pub fn start_at(&self, start: f64) {
        self.start.store(start)
    }

    /// Stop playback at this timestamp
    pub fn stop_at(&self, stop: f64) {
        self.stop.store(stop)
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper struct to control audio streams
#[derive(Clone, Debug)]
pub struct Controller {
    scheduler: Arc<Scheduler>,
    // duration: Arc<AtomicF64>,
    seek: Arc<AtomicF64>,
    loop_: Arc<AtomicBool>,
    loop_start: Arc<AtomicF64>,
    loop_end: Arc<AtomicF64>,
    //playback_rate: Arc<AudioParam>,
}

impl Controller {
    /// Create a new Controller. It will not be active
    pub fn new() -> Self {
        Self {
            scheduler: Arc::new(Scheduler::new()),
            // duration: Arc::new(AtomicF64::new(f64::MAX)),

            // treat NaN as niche: no seeking
            seek: Arc::new(AtomicF64::new(f64::NAN)),

            loop_: Arc::new(AtomicBool::new(false)),
            loop_start: Arc::new(AtomicF64::new(0.)),
            loop_end: Arc::new(AtomicF64::new(f64::MAX)),
            //playback_rate: ... create audio param pair
        }
    }

    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    pub fn loop_(&self) -> bool {
        self.loop_.load(Ordering::SeqCst)
    }

    pub fn set_loop(&self, loop_: bool) {
        self.loop_.store(loop_, Ordering::SeqCst);
    }

    pub fn loop_start(&self) -> f64 {
        self.loop_start.load()
    }

    pub fn set_loop_start(&self, loop_start: f64) {
        self.loop_start.store(loop_start);
    }

    pub fn loop_end(&self) -> f64 {
        self.loop_end.load()
    }

    pub fn set_loop_end(&self, loop_end: f64) {
        self.loop_end.store(loop_end);
    }

    pub fn seek(&self, timestamp: f64) {
        self.seek.store(timestamp);
    }

    pub(crate) fn should_seek(&self) -> Option<f64> {
        let prev = self.seek.swap(f64::NAN);
        if prev.is_nan() {
            None
        } else {
            Some(prev)
        }
    }
}

impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_controller() {
        let controller = Controller::new();

        assert!(!controller.loop_());
        assert!(controller.loop_start() == 0.);
        assert!(controller.loop_end() == f64::MAX);
        assert!(controller.should_seek().is_none());

        controller.seek(1.);
        assert_eq!(controller.should_seek(), Some(1.));
        assert!(controller.should_seek().is_none());
    }
}
