//! The native settings window polls runtime_status every five seconds. Recover
//! its renderer independently of service startup, without refreshing a merely
//! throttled background window or a healthy form returning from sleep.
use std::time::{Duration, Instant, SystemTime};

const POLL_TIMEOUT: Duration = Duration::from_secs(30);
// Native checks normally run every two seconds. Missed checks must receive
// grace before the thirty-second renderer deadline can be mistaken for a stall.
const MONITOR_GAP: Duration = Duration::from_secs(10);

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

    fn foreground(health: &mut UiHealth, start: Instant, seconds: u64) -> UiAction {
        let elapsed = Duration::from_secs(seconds);
        health.check(
            start + elapsed,
            SystemTime::UNIX_EPOCH + elapsed,
            true,
            false,
            false,
        )
    }

    #[test]
    fn healthy_polls_preserve_forms_and_stalled_foreground_reloads_with_backoff() {
        let start = Instant::now();
        let mut health = UiHealth::new(start, SystemTime::UNIX_EPOCH);
        for seconds in 0..=120 {
            if seconds % 5 == 0 {
                health.heartbeat(start + Duration::from_secs(seconds));
            }
            assert_eq!(foreground(&mut health, start, seconds), UiAction::Wait);
        }
        // The native monitor continues sampling even while the renderer stalls.
        for seconds in (122..=184).step_by(2) {
            let expected = if matches!(seconds, 152 | 184) {
                UiAction::Reload
            } else {
                UiAction::Wait
            };
            assert_eq!(foreground(&mut health, start, seconds), expected);
        }
    }

    #[test]
    fn background_focus_and_suspended_monitor_receive_a_full_grace_period() {
        let start = Instant::now();
        let wall = SystemTime::UNIX_EPOCH;
        let mut health = UiHealth::new(start, wall);
        let at = |seconds| start + Duration::from_secs(seconds);
        let wall_at = |seconds| wall + Duration::from_secs(seconds);
        assert_eq!(foreground(&mut health, start, 5), UiAction::Wait);
        assert_eq!(
            health.check(at(3600), wall_at(3600), false, false, false),
            UiAction::Wait
        );
        assert_eq!(foreground(&mut health, start, 3601), UiAction::Wait);
        health.heartbeat(at(3606));
        for seconds in (3607..=3629).step_by(2) {
            assert_eq!(foreground(&mut health, start, seconds), UiAction::Wait);
        }
        health.focus_changed(at(3630));
        assert_eq!(foreground(&mut health, start, 3631), UiAction::Wait);
        // The window stayed focused during sleep; no background sample exists.
        assert_eq!(foreground(&mut health, start, 7200), UiAction::Wait);
        health.heartbeat(at(7205));
        for seconds in (7207..=7229).step_by(2) {
            assert_eq!(foreground(&mut health, start, seconds), UiAction::Wait);
        }
        assert_eq!(
            health.check(at(7240), wall_at(7000), true, false, false),
            UiAction::Wait
        );
        for seconds in (7242..=7270).step_by(2) {
            assert_eq!(
                health.check(at(seconds), wall_at(seconds - 240), true, false, false),
                UiAction::Wait
            );
        }
        assert_eq!(
            health.check(at(7272), wall_at(7032), true, false, false),
            UiAction::Reload
        );
    }

    #[test]
    fn final_check_cancels_recovery_after_heartbeat_or_focus_returns() {
        let start = Instant::now();
        let mut health = UiHealth::new(start, SystemTime::UNIX_EPOCH);
        for seconds in (0..=30).step_by(2) {
            assert_eq!(foreground(&mut health, start, seconds), UiAction::Wait);
        }
        // A due check is queued at32; a recovered poll is accepted before it runs.
        health.heartbeat(start + Duration::from_secs(33));
        assert_eq!(foreground(&mut health, start, 34), UiAction::Wait);
        for seconds in (36..=62).step_by(2) {
            assert_eq!(foreground(&mut health, start, seconds), UiAction::Wait);
        }
        // Another due check is queued at64; new focus grace must win at68.
        health.focus_changed(start + Duration::from_secs(67));
        assert_eq!(foreground(&mut health, start, 68), UiAction::Wait);
        for seconds in (70..=98).step_by(2) {
            assert_eq!(foreground(&mut health, start, seconds), UiAction::Wait);
        }
        assert_eq!(foreground(&mut health, start, 100), UiAction::Reload);
    }

    #[test]
    fn aborted_quit_keeps_monitoring_until_exit_is_committed() {
        let start = Instant::now();
        let wall = SystemTime::UNIX_EPOCH;
        let mut health = UiHealth::new(start, wall);
        for (seconds, quitting) in [(0, false), (31, true), (151, true), (152, false)] {
            let elapsed = Duration::from_secs(seconds);
            assert_eq!(
                health.check(start + elapsed, wall + elapsed, true, quitting, false),
                UiAction::Wait
            );
        }
        for seconds in (154..=182).step_by(2) {
            assert_eq!(foreground(&mut health, start, seconds), UiAction::Wait);
        }
        assert_eq!(foreground(&mut health, start, 184), UiAction::Reload);
        assert_eq!(
            health.check(
                start + Duration::from_secs(185),
                wall + Duration::from_secs(185),
                true,
                true,
                true
            ),
            UiAction::Stop
        );
    }

    #[test]
    fn blocked_native_loop_does_not_refresh_a_healthy_renderer_on_return() {
        let start = Instant::now();
        let mut health = UiHealth::new(start, SystemTime::UNIX_EPOCH);
        assert_eq!(foreground(&mut health, start, 0), UiAction::Wait);
        assert_eq!(foreground(&mut health, start, 2), UiAction::Wait);
        // A synchronous native call blocks main-loop checks and heartbeats for
        // 33 seconds. Its queued check can run before the waiting status IPC.
        assert_eq!(foreground(&mut health, start, 35), UiAction::Wait);
        health.heartbeat(start + Duration::from_secs(36));
        for seconds in (37..=65).step_by(2) {
            assert_eq!(foreground(&mut health, start, seconds), UiAction::Wait);
        }
        assert_eq!(foreground(&mut health, start, 67), UiAction::Reload);
    }
}
