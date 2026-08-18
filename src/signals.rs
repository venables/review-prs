//! Signal handling: INT, TERM and HUP all mean "stop the reviews, keep the
//! summary". HUP matters as much as INT -- the first use case for a headless
//! run is an ssh session, and a dropped connection must stop the reviewers
//! rather than orphan them (they would keep spending and keep holding their
//! sessions open, so the next --continue would refuse to resume them).

use crate::pool::Event;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
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
