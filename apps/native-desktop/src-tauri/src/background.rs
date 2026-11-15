//! A quit request can be cancelled when an owned process cannot stop safely.
//! Keep workers available until exit is committed, without doing work mid-quit.
use std::{thread, time::Duration};

pub(crate) fn run(
    interval: Duration,
    mut lifecycle: impl FnMut() -> (bool, bool),
    mut work: impl FnMut(),
) {
    loop {
        thread::sleep(interval);
        let (quitting, exit_allowed) = lifecycle();
        if exit_allowed {
            break;
        }
        if !quitting {
            work();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_quit_resumes_work_and_committed_exit_stops_it() {
        let mut states = [
            (false, false),
            (true, false),
            (true, false),
            (false, false),
            (true, true),
            (false, false),
        ]
        .into_iter();
        let mut runs = 0;
        run(Duration::ZERO, || states.next().unwrap(), || runs += 1);
        assert_eq!(runs, 2, "work resumes after an unsuccessful quit");
        assert_eq!(states.next(), Some((false, false)), "exit is terminal");
    }
}
