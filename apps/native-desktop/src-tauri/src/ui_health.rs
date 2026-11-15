//! The native settings window polls runtime_status every five seconds. Recover
//! its renderer independently of service startup, without refreshing a merely
//! throttled background window or a healthy form returning from sleep.
use std::time::{Duration, Instant, SystemTime};

const POLL_TIMEOUT: Duration = Duration::from_secs(30);
const MONITOR_GAP: Duration = Duration::from_secs(45);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum UiAction {
    Wait,
    Reload,
    Stop,
}

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

    pub(crate) fn check(
        &mut self,
        now: Instant,
        wall: SystemTime,
        active: bool,
        quitting: bool,
        exit_allowed: bool,
    ) -> UiAction {
        if exit_allowed {
            return UiAction::Stop;
        }
        // A graceful quit can fail and leave the application running. Keep the
        // monitor alive, with fresh foreground grace if that quit is cancelled.
        if self.should_reload(now, wall, active && !quitting) {
            UiAction::Reload
        } else {
            UiAction::Wait
        }
    }

    fn should_reload(&mut self, now: Instant, wall: SystemTime, active: bool) -> bool {
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
            assert_eq!(
                health.check(start + elapsed, wall + elapsed, true, false, false),
                UiAction::Wait
            );
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
                health.check(start + elapsed, wall + elapsed, true, false, false),
                if reload {
                    UiAction::Reload
                } else {
                    UiAction::Wait
                }
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
        assert_eq!(
            health.check(at(5), wall_at(5), true, false, false),
            UiAction::Wait
        );
        assert_eq!(
            health.check(at(3600), wall_at(3600), false, false, false),
            UiAction::Wait
        );
        assert_eq!(
            health.check(at(3601), wall_at(3601), true, false, false),
            UiAction::Wait
        );
        health.heartbeat(at(3606));
        assert_eq!(
            health.check(at(3629), wall_at(3629), true, false, false),
            UiAction::Wait
        );
        health.focus_changed(at(3630));
        assert_eq!(
            health.check(at(3631), wall_at(3631), true, false, false),
            UiAction::Wait
        );
        // The window stayed focused during sleep; no background sample exists.
        assert_eq!(
            health.check(at(7200), wall_at(7200), true, false, false),
            UiAction::Wait
        );
        health.heartbeat(at(7205));
        assert_eq!(
            health.check(at(7230), wall_at(7230), true, false, false),
            UiAction::Wait
        );
        assert_eq!(
            health.check(at(7240), wall_at(7000), true, false, false),
            UiAction::Wait
        );
        assert_eq!(
            health.check(at(7270), wall_at(7030), true, false, false),
            UiAction::Wait
        );
        assert_eq!(
            health.check(at(7272), wall_at(7032), true, false, false),
            UiAction::Reload
        );
    }

    #[test]
    fn final_check_cancels_recovery_after_heartbeat_or_focus_returns() {
        let start = Instant::now();
        let wall = SystemTime::UNIX_EPOCH;
        let mut health = UiHealth::new(start, wall);
        let at = |seconds| start + Duration::from_secs(seconds);
        let wall_at = |seconds| wall + Duration::from_secs(seconds);
        assert_eq!(
            health.check(at(0), wall_at(0), true, false, false),
            UiAction::Wait
        );
        // The worker queues a check after the timeout but does not compute a
        // Reload action. A resumed poll arrives before that task can run.
        health.heartbeat(at(33));
        assert_eq!(
            health.check(at(34), wall_at(34), true, false, false),
            UiAction::Wait
        );
        // The next queued check is also stale by arrival; a new focus grace
        // must win at the final check rather than replaying an earlier action.
        health.focus_changed(at(67));
        assert_eq!(
            health.check(at(68), wall_at(68), true, false, false),
            UiAction::Wait
        );
        assert_eq!(
            health.check(at(99), wall_at(99), true, false, false),
            UiAction::Reload
        );
    }

    #[test]
    fn aborted_quit_keeps_monitoring_until_exit_is_committed() {
        let start = Instant::now();
        let wall = SystemTime::UNIX_EPOCH;
        let mut health = UiHealth::new(start, wall);
        for (seconds, quitting, exit_allowed, expected) in [
            (0, false, false, UiAction::Wait),
            (31, true, false, UiAction::Wait),
            (151, true, false, UiAction::Wait),
            (152, false, false, UiAction::Wait),
            (182, false, false, UiAction::Wait),
            (184, false, false, UiAction::Reload),
            (185, true, true, UiAction::Stop),
        ] {
            let elapsed = Duration::from_secs(seconds);
            assert_eq!(
                health.check(
                    start + elapsed,
                    wall + elapsed,
                    true,
                    quitting,
                    exit_allowed
                ),
                expected
            );
        }
    }
}
