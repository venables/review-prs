//! What the tool is doing before it has anything to show.
//!
//! Every entry point spends the same few seconds before its first real line:
//! `gh repo view`, `gh api user`, then one GraphQL call for the PR list. On a
//! slow link that is several seconds of nothing at all, which reads as a hung
//! tool rather than a working one -- and the first thing anyone does with a
//! tool that looks hung is press ctrl-C.
//!
//! On a terminal this is one spinner line that rewrites itself and leaves
//! nothing behind, so the report still starts at the top. Anywhere else --
//! cron, CI, a pipe -- it is one plain line per step, on stderr, so a log
//! records what happened while stdout stays the report.

use crate::ui::SPINNER_FRAMES;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::time::Duration;

pub struct Status {
    /// None when there is no terminal to animate: the steps become plain
    /// lines instead.
    bar: Option<ProgressBar>,
}

impl Status {
    pub fn new() -> Status {
        // stderr decides, because that is where this goes: a run whose stdout
        // is piped to a file still has a terminal to spin on.
        if !std::io::stderr().is_terminal() {
            return Status { bar: None };
        }
        let bar = ProgressBar::new_spinner();
        bar.set_draw_target(ProgressDrawTarget::stderr());
        bar.set_style(
            ProgressStyle::with_template("{spinner:.magenta} {msg}")
                .expect("spinner template")
                .tick_strings(SPINNER_FRAMES),
        );
        // Steady, so the line keeps moving while a network call blocks: a
        // frozen spinner says the same thing silence does.
        bar.enable_steady_tick(Duration::from_millis(80));
        Status { bar: Some(bar) }
    }

    /// What is happening now. Replaces the last step on a terminal; adds a
    /// line anywhere else.
    pub fn step(&self, msg: impl Into<String>) {
        let msg = msg.into();
        match &self.bar {
            Some(bar) => bar.set_message(msg),
            None => eprintln!("{msg}"),
        }
    }

    /// A reporter that never draws. What a test wants, and what any caller
    /// wants when the progress would be noise rather than news.
    pub fn silent() -> Status {
        Status { bar: None }
    }

    /// A message that changes as time passes, drawn only where it can be
    /// redrawn. Off a terminal this says nothing at all -- a line every
    /// quarter second is not a log, it is a flood -- which is what separates
    /// it from `step`.
    pub fn tick(&self, msg: impl Into<String>) {
        if let Some(bar) = &self.bar {
            bar.set_message(msg.into());
        }
    }

    /// A line that stays on screen. The spinner steps aside for it and comes
    /// back underneath -- otherwise a permanent line written while the
    /// spinner is live fuses with it, which is how an error ends up reading
    /// as part of a progress message.
    pub fn say(&self, msg: impl Into<String>) {
        let msg = msg.into();
        self.suspend(|| eprintln!("{msg}"));
    }

    /// Print something permanent without the spinner fighting it for the
    /// last line of the terminal.
    pub fn suspend<F: FnOnce() -> T, T>(&self, f: F) -> T {
        match &self.bar {
            Some(bar) => bar.suspend(f),
            None => f(),
        }
    }

    /// Nothing more is coming. The spinner leaves no trace: what the run
    /// found is the report's job to say, not the progress line's.
    pub fn clear(&self) {
        if let Some(bar) = &self.bar {
            bar.finish_and_clear();
        }
    }
}

impl Default for Status {
    fn default() -> Status {
        Status::new()
    }
}

impl Drop for Status {
    /// Every path out, not only the ones that finish. A fetch that fails
    /// returns through `?` without reaching `clear`, and a spinner left
    /// ticking under the error message is the one thing worse than no
    /// spinner at all.
    fn drop(&mut self) {
        self.clear();
    }
}

/// The steps themselves, so three front-ends word them the same way.
pub mod step {
    pub fn reading_repo() -> &'static str {
        "reading the repo"
    }

    pub fn fetching(owner: &str, name: &str) -> String {
        format!("fetching open PRs from {owner}/{name}")
    }

    /// Both numbers when the filters removed something, because "found 3
    /// open PRs" on a repo showing 40 in the browser reads as a broken query.
    /// Where the wait actually is for panel: N checkouts of the repository
    /// before a single model has been asked anything.
    pub fn materializing(done: usize, total: usize) -> String {
        format!("materializing worktree {} of {total}", done + 1)
    }

    /// The two long silences in a panel run, both counted up so a reader can
    /// see the run is alive rather than wedged.
    pub fn reviewing(running: usize, elapsed: u64) -> String {
        format!(
            "{} still reviewing, {}",
            crate::ui::count(running, "panelist"),
            crate::ui::fmt_dur(elapsed)
        )
    }

    pub fn synthesizing(backend: &str, elapsed: u64) -> String {
        format!("synthesizing with {backend}, {}", crate::ui::fmt_dur(elapsed))
    }

    /// A count that reached the query's own limit is reported as "50+": the
    /// list is one page, so the real total is unknown, and stating it exactly
    /// would be the same kind of lie in the other direction.
    pub fn found(open: usize, considered: usize) -> String {
        let count = if open >= crate::prlist::QUERY_LIMIT {
            format!("{}+ open PRs", crate::prlist::QUERY_LIMIT)
        } else {
            crate::ui::count(open, "open PR")
        };
        let found = format!("found {count}");
        if considered == open {
            return found;
        }
        format!("{found}, {considered} to consider")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_steps_read_as_sentences() {
        assert_eq!(step::fetching("acme", "widgets"), "fetching open PRs from acme/widgets");
        assert_eq!(step::found(8, 8), "found 8 open PRs");
        // Counted in english, like everything else this crate prints -- and
        // never as "PR(s)", which the sibling tools deliberately dropped.
        assert_eq!(step::found(1, 1), "found 1 open PR");
        assert!(!step::found(2, 2).contains("PR(s)"));
        // Both numbers when they differ: most of those 40 are your own.
        assert_eq!(step::found(40, 3), "found 40 open PRs, 3 to consider");
        // The query asks for one page, so a full page means "at least this".
        assert_eq!(step::found(50, 4), "found 50+ open PRs, 4 to consider");
        // panel's own waits, counted from one rather than from zero.
        assert_eq!(step::materializing(0, 4), "materializing worktree 1 of 4");
        assert_eq!(step::materializing(3, 4), "materializing worktree 4 of 4");
        assert_eq!(step::reviewing(3, 90), "3 panelists still reviewing, 1m30s");
        assert_eq!(step::reviewing(1, 5), "1 panelist still reviewing, 5s");
        assert_eq!(step::synthesizing("claude", 65), "synthesizing with claude, 1m05s");
    }

    #[test]
    fn a_run_with_no_terminal_still_says_its_steps() {
        // Nothing to assert about the spinner itself; what matters is that
        // clearing an absent one is fine, and that clearing twice is too --
        // Drop calls it again after an explicit clear.
        let status = Status::silent();
        status.step("reading the repo");
        // A tick says nothing without a terminal to redraw, and suspend still
        // runs what it was given.
        status.tick("3 panelists still reviewing, 1m30s");
        assert_eq!(status.suspend(|| 7), 7);
        status.clear();
        status.clear();
    }

    #[test]
    fn the_spinner_style_is_valid() {
        // The template and the tick strings are only parsed when there is a
        // terminal to draw on, and they are parsed with expect -- so without
        // this a typo in either reaches a user's terminal as a panic and
        // never reaches CI, which has no tty.
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::with_template("{spinner:.magenta} {msg}")
                .expect("spinner template")
                .tick_strings(SPINNER_FRAMES),
        );
        bar.set_message("reading the repo");
        bar.finish_and_clear();
    }
}
