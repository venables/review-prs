//! Signal handling: INT, TERM and HUP all mean "stop the reviews, keep the
//! summary". HUP matters as much as INT -- the first use case for a headless
//! run is an ssh session, and a dropped connection must stop the reviewers
//! rather than orphan them (they would keep spending and keep holding their
//! sessions open, so the next --continue would refuse to resume them).

use crate::pool::Event;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;

pub fn install(tx: Sender<Event>) {
    let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP]).expect("installing signal handlers");
    std::thread::spawn(move || {
        for _sig in signals.forever() {
            if tx.send(Event::Signal).is_err() {
                return;
            }
        }
    });
}

/// A flag that turns true on INT, TERM or HUP, for a loop that polls rather
/// than owning a channel. panel's fan-out is that loop, and what it must do on
/// any of the three is the same: stop the panelists, then let the worktrees go
/// -- worktrees registered in the user's real repository, which is why leaving
/// them behind is worse than an unfinished review.
pub fn install_flag() -> Arc<AtomicBool> {
    let flag = Arc::new(AtomicBool::new(false));
    for sig in [SIGINT, SIGTERM, SIGHUP] {
        let _ = signal_hook::flag::register(sig, Arc::clone(&flag));
    }
    flag
}
