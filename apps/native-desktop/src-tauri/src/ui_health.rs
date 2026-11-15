//! The native settings window polls runtime_status every five seconds. Recover
//! its renderer independently of service startup, without refreshing a merely
//! throttled background window or a healthy form returning from sleep.
use std::time::{Duration, Instant, SystemTime};

const POLL_TIMEOUT: Duration = Duration::from_secs(30);
const MONITOR_GAP: Duration = Duration::from_secs(45);

pub(crate) struct UiHealth {
    last_poll: Instant,
    last_check: SystemTime,
    active: bool,
}

impl UiHealth {
    pub(crate) fn new(now: Instant, wall: SystemTime) -> Self {
        Self {
            last_poll: now,
            last_check: wall,
            active: false,
        }
    }

    pub(crate) fn heartbeat(&mut self, now: Instant) {
        self.last_poll = now;
    }

    /// Focus can change between monitor ticks. Start a fresh grace period even
    /// when a brief loss/return of focus was not sampled by the monitor.
    pub(crate) fn focus_changed(&mut self, now: Instant) {
        self.last_poll = now;
        self.active = false;
    }

    pub(crate) fn should_reload(&mut self, now: Instant, wall: SystemTime, active: bool) -> bool {
        // Instant's treatment of suspended time varies by platform. Wall time
        // detects missed monitor ticks; a clock discontinuity also gets grace.
        let resumed = wall
            .duration_since(self.last_check)
            .map_or(true, |gap| gap > MONITOR_GAP);
        self.last_check = wall;
        if !active || !self.active || resumed {
            self.last_poll = now;
            self.active = active;
            return false;
        }
        if now.saturating_duration_since(self.last_poll) > POLL_TIMEOUT {
            // Bound reload attempts if a renderer continues to fail.
            self.last_poll = now;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_polls_preserve_forms_and_stalled_foreground_reloads_with_backoff() {
        let start = Instant::now();
        let wall = SystemTime::UNIX_EPOCH;
        let mut health = UiHealth::new(start, wall);
        for seconds in 0..=120 {
            let elapsed = Duration::from_secs(seconds);
            if seconds % 5 == 0 {
                health.heartbeat(start + elapsed);
            }
            assert!(!health.should_reload(start + elapsed, wall + elapsed, true));
        }
        for (seconds, reload) in [
            (150, false),
            (152, true),
            (154, false),
            (182, false),
            (184, true),
        ] {
            let elapsed = Duration::from_secs(seconds);
            assert_eq!(
                health.should_reload(start + elapsed, wall + elapsed, true),
                reload
            );
        }
    }

    #[test]
    fn background_focus_and_suspended_monitor_receive_a_full_grace_period() {
        let start = Instant::now();
        let wall = SystemTime::UNIX_EPOCH;
        let mut health = UiHealth::new(start, wall);
        let at = |seconds| start + Duration::from_secs(seconds);
        let wall_at = |seconds| wall + Duration::from_secs(seconds);
        assert!(!health.should_reload(at(5), wall_at(5), true));
        assert!(!health.should_reload(at(3600), wall_at(3600), false));
        assert!(!health.should_reload(at(3601), wall_at(3601), true));
        health.heartbeat(at(3606));
        assert!(!health.should_reload(at(3629), wall_at(3629), true));
        health.focus_changed(at(3630));
        assert!(!health.should_reload(at(3631), wall_at(3631), true));
        // The window stayed focused during sleep; no background sample exists.
        assert!(!health.should_reload(at(7200), wall_at(7200), true));
        health.heartbeat(at(7205));
        assert!(!health.should_reload(at(7230), wall_at(7230), true));
        assert!(!health.should_reload(at(7240), wall_at(7000), true));
        assert!(!health.should_reload(at(7270), wall_at(7030), true));
        assert!(health.should_reload(at(7272), wall_at(7032), true));
    }
}
