//! What the next babysit pass reviews.
//!
//! The first pass reviews what was actionable when the run started. Every pass
//! after it has two jobs the first does not: drop the PRs that are finished,
//! and pick up the ones that appeared or changed while the last pass was
//! running. A queue fixed at t=0 misses a PR opened a minute later for the
//! whole run, however long the run is.
//!
//! Dropping and adding come from different sources on purpose. A PR leaves
//! because `gh pr view` says it is approved or closed -- authoritative, per
//! PR. A PR joins because the sweep now ranks it actionable -- a snapshot,
//! which may lag by a poll. Trusting the snapshot to drop things would let a
//! stale list re-review a PR that was approved a second ago.

use std::collections::{HashMap, HashSet};

/// What the next pass will review, and what changed since the last one.
#[derive(Debug, PartialEq, Default)]
pub struct Intake {
    pub queue: Vec<u64>,
    /// Not in the last pass: opened, or pushed to, since.
    pub joined: Vec<u64>,
    /// Actionable, but they have had their passes.
    pub capped: Vec<u64>,
}

/// How many passes each PR has had, and how many it may have.
pub struct Queue {
    passes: HashMap<u64, u32>,
    /// Approved, merged or closed: finished for this run, whatever the sweep
    /// says next. The sweep is a snapshot and lags by up to one poll, so
    /// without this a PR dropped as approved rejoins on the very same
    /// interval that dropped it.
    done: HashSet<u64>,
    max_passes: u32,
}

impl Queue {
    pub fn new(max_passes: u32) -> Queue {
        Queue { passes: HashMap::new(), done: HashSet::new(), max_passes }
    }

    /// This PR is finished for the rest of the run.
    pub fn mark_done(&mut self, pr: u64) {
        self.done.insert(pr);
    }

    pub fn record_pass(&mut self, prs: &[u64]) {
        for &pr in prs {
            *self.passes.entry(pr).or_insert(0) += 1;
        }
    }

    pub fn passes(&self, pr: u64) -> u32 {
        self.passes.get(&pr).copied().unwrap_or(0)
    }

    fn capped(&self, pr: u64) -> bool {
        self.passes(pr) >= self.max_passes
    }

    /// The next queue: what is still open from the last pass, plus whatever
    /// the sweep now finds actionable, minus anything that has had its passes.
    ///
    /// The cap is what stops a conversation becoming a loop. Every review
    /// autoreview posts is activity on the PR, so an author who replies makes
    /// it actionable again, which would make autoreview review it again --
    /// unattended, and for as long as the loop runs.
    pub fn next(&self, still_open: &[u64], actionable: &[u64]) -> Intake {
        let mut intake = Intake::default();
        for &pr in still_open {
            if self.capped(pr) {
                intake.capped.push(pr);
            } else {
                intake.queue.push(pr);
            }
        }
        for &pr in actionable {
            if still_open.contains(&pr) || intake.joined.contains(&pr) || self.done.contains(&pr) {
                continue;
            }
            if self.capped(pr) {
                // Named once, not on every interval: it is already in capped
                // if it was in the last pass, and a PR that keeps being
                // actionable would otherwise be announced forever.
                if !intake.capped.contains(&pr) {
                    intake.capped.push(pr);
                }
                continue;
            }
            intake.queue.push(pr);
            intake.joined.push(pr);
        }
        intake
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pr_that_appears_mid_run_joins_the_queue() {
        let q = Queue::new(3);
        let intake = q.next(&[9, 8], &[9, 8, 12]);
        assert_eq!(intake.queue, vec![9, 8, 12]);
        assert_eq!(intake.joined, vec![12]);
        assert!(intake.capped.is_empty());
    }

    #[test]
    fn an_approved_pr_does_not_rejoin_on_a_stale_sweep() {
        // The sweep is a snapshot: the review that approved #9 lands before
        // the next GraphQL call reflects it. Without the done set, the same
        // interval that drops #9 puts it straight back.
        let mut q = Queue::new(3);
        q.mark_done(9);
        let intake = q.next(&[8], &[9, 8]);
        assert_eq!(intake.queue, vec![8]);
        assert!(intake.joined.is_empty(), "#9 is finished for this run");
    }

    #[test]
    fn a_pr_nobody_has_finished_still_joins() {
        // The other side of the same rule: not in the last pass and not done
        // means it is new work.
        let q = Queue::new(3);
        let intake = q.next(&[8], &[9, 8]);
        assert_eq!(intake.queue, vec![8, 9]);
        assert_eq!(intake.joined, vec![9]);
    }

    #[test]
    fn the_cap_stops_a_conversation_becoming_a_loop() {
        let mut q = Queue::new(2);
        q.record_pass(&[9, 8]);
        q.record_pass(&[9]);
        assert_eq!(q.passes(9), 2);
        assert_eq!(q.passes(8), 1);

        let intake = q.next(&[9, 8], &[9, 8]);
        assert_eq!(intake.queue, vec![8], "9 has had its passes");
        assert_eq!(intake.capped, vec![9]);

        // And it cannot rejoin as a newcomer either.
        let intake = q.next(&[8], &[9, 8]);
        assert_eq!(intake.queue, vec![8]);
        assert_eq!(intake.capped, vec![9]);
        assert!(intake.joined.is_empty());
    }

    #[test]
    fn a_capped_pr_is_named_once_not_twice() {
        let mut q = Queue::new(1);
        q.record_pass(&[9]);
        let intake = q.next(&[9], &[9]);
        assert_eq!(intake.capped, vec![9]);
    }

    #[test]
    fn the_actionable_order_is_kept_for_newcomers() {
        // The sweep ranks NEW before UPDATED, then by recency. A newcomer
        // keeps that ranking rather than landing wherever a set iterated.
        let q = Queue::new(3);
        let intake = q.next(&[], &[12, 9, 8]);
        assert_eq!(intake.queue, vec![12, 9, 8]);
        assert_eq!(intake.joined, vec![12, 9, 8]);
    }

    #[test]
    fn nothing_actionable_and_nothing_open_is_an_empty_queue() {
        let q = Queue::new(3);
        assert_eq!(q.next(&[], &[]), Intake::default());
    }
}
