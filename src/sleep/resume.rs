//! CLOCK_BOOTTIME includes suspend; CLOCK_MONOTONIC and wall-clock corrections
//! do not change their difference. This backs up logind, not a Date.now gap.
use rustix::time::{ClockId, clock_gettime};

pub(super) fn suspend_offset() -> Option<i128> {
    fn ns(id: ClockId) -> i128 {
        let t = clock_gettime(id);
        i128::from(t.tv_sec) * 1_000_000_000 + i128::from(t.tv_nsec)
    }
    let before = ns(ClockId::Monotonic);
    let boot = ns(ClockId::Boottime);
    let after = ns(ClockId::Monotonic);
    // Skip a badly preempted measurement rather than inventing a sleep.
    (after - before < 50_000_000).then_some(boot - (before + after) / 2)
}

#[derive(Default)]
pub(super) struct ResumeDetector {
    offset: Option<i128>,
    reported_before_signal: bool,
}

impl ResumeDetector {
    fn changed(&mut self, offset: Option<i128>) -> bool {
        let Some(offset) = offset else {
            return false;
        };
        let changed = self
            .offset
            .is_some_and(|previous| offset - previous > 250_000_000);
        self.offset = Some(offset);
        changed
    }

    pub(super) fn poll(&mut self, offset: Option<i128>) -> bool {
        let changed = self.changed(offset);
        self.reported_before_signal |= changed;
        changed
    }

    pub(super) fn signal(&mut self, offset: Option<i128>) -> bool {
        let changed = self.changed(offset);
        let announce = changed || !self.reported_before_signal;
        self.reported_before_signal = false;
        announce
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_sleep_signal_and_clock_fallback_are_deduplicated() {
        let mut detector = ResumeDetector::default();
        assert!(!detector.poll(Some(0)));
        assert!(detector.signal(Some(100_000_000))); // shorter than polling tolerance
        assert!(!detector.poll(Some(100_000_000)));
        assert!(detector.poll(Some(5_100_000_000))); // missed/delayed signal
        assert!(!detector.signal(Some(5_100_000_000)));
        assert!(detector.signal(Some(10_100_000_000)));
    }
    #[test]
    fn startup_stalls_and_wall_clock_changes_are_not_resumes() {
        let mut detector = ResumeDetector::default();
        assert!(!detector.poll(Some(123_000_000_000)));
        for _ in 0..10 {
            assert!(!detector.poll(Some(123_000_000_000)));
        }
        assert!(!detector.poll(None));
        assert!(suspend_offset().is_some());
    }
}
