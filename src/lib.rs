//! The shared core behind this repo's two front-ends.
//!
//! `review-prs` fans a repo's open PRs into one terminal tab each, to watch and
//! steer. `autoreview` reviews the same PRs headlessly through dash-p, with a
//! progress display and an exit status that means "the reviews succeeded".
//!
//! They agree on what is worth reviewing (`prlist`, `picker`, `select`) and on
//! which session a PR belongs to (`session`) because they are two binaries over
//! one library -- not two implementations kept in step by hand.

pub mod cli;
pub mod interval;
pub mod job;
pub mod panel;
pub mod picker;
pub mod pool;
pub mod prlist;
pub mod queue;
pub mod report;
pub mod repo;
pub mod rundir;
pub mod select;
pub mod session;
pub mod signals;
pub mod status;
pub mod tabs;
pub mod ui;
