//! What the next babysit pass reviews.
//!
//! The first pass reviews what was actionable when the run started. Every pass
//! after it has two jobs the first does not: drop the PRs that are finished,
//! and pick up the ones that appeared or changed while the last pass was
//! running. A queue fixed at t=0 misses a PR opened a minute later for the
//! whole run, however long the run is.
//!
//! One rule decides membership: **the sweep says a PR is actionable now**.
//! That is not a new concept -- a review autoreview posts becomes its own
//! latest activity on the PR, so an unchanged PR goes quiet by itself and an
//! author pushing a fix makes it `UPDATED` again. Using it for staying as well
//! as for joining is what keeps an untouched PR from being re-reviewed every
//! interval until its cap runs out.
//!
//! Two things overrule the sweep, both because the sweep is a snapshot that
//! lags by up to a poll:
//!
//! - a PR that GitHub says is approved or closed is finished for the run, so a
//!   stale list cannot re-queue it on the very interval that dropped it;
//! - under `--pick`, only the PRs the user chose are eligible at all.

use std::collections::HashSet;

/// What the next pass will review, and what changed since the last one.
#[derive(Debug, PartialEq, Default)]
pub struct Intake {
    pub queue: Vec<u64>,
    /// Not in the last pass: opened, or pushed to, since.
    pub joined: Vec<u64>,
    /// Actionable, but they have had their passes. Named once per run.
    pub capped: Vec<u64>,
}

pub struct Queue {
    passes: std::collections::HashMap<u64, u32>,
    /// Approved, merged or closed: finished for this run, whatever the sweep
    /// says next.
    done: HashSet<u64>,
    /// Under --pick, the PRs the user chose. The sweep may not add to it: a
    /// run told to watch two PRs must not quietly grow to five.
    only: Option<HashSet<u64>>,
    /// Capped PRs already announced, so a PR that stays actionable does not
    /// repeat its "leaving it alone" line on every interval.
    announced: HashSet<u64>,
    max_passes: u32,
}

impl Queue {
    /// `only` is Some for a --pick run and None for a sweep.
    pub fn new(max_passes: u32, only: Option<Vec<u64>>) -> Queue {
        Queue {
            passes: std::collections::HashMap::new(),
            done: HashSet::new(),
            only: only.map(|v| v.into_iter().collect()),
            announced: HashSet::new(),
            max_passes,
        }
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

    /// Has this PR had all the passes it may have? A run whose whole watch
    /// list is capped has nothing left it could ever review, however long it
    /// waits.
    pub fn is_capped(&self, pr: u64) -> bool {
        self.passes(pr) >= self.max_passes
    }

    fn eligible(&self, pr: u64) -> bool {
        !self.done.contains(&pr) && self.only.as_ref().is_none_or(|only| only.contains(&pr))
    }

    /// The next queue: everything the sweep now ranks actionable that this run
    /// is still allowed to review.
    ///
    /// `still_open` is only used to tell a newcomer from a PR that was already
    /// being watched, so the run can say which is which.
    ///
    /// The cap is what stops a conversation becoming a loop. Every review
    /// autoreview posts is activity on the PR, so an author who replies makes
    /// it actionable again, which would make autoreview review it again --
    /// unattended, and for as long as the loop runs.
    pub fn next(&mut self, still_open: &[u64], actionable: &[u64]) -> Intake {
        let mut intake = Intake::default();
        for &pr in actionable {
            if !self.eligible(pr) || intake.queue.contains(&pr) {
                continue;
            }
            if self.is_capped(pr) {
                if self.announced.insert(pr) {
                    intake.capped.push(pr);
                }
                continue;
            }
            intake.queue.push(pr);
            if !still_open.contains(&pr) {
                intake.joined.push(pr);
            }
        }
        intake
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sweep(max: u32) -> Queue {
        Queue::new(max, None)
    }

    #[test]
    fn a_pr_that_appears_mid_run_joins_the_queue() {
        let mut q = sweep(3);
        let intake = q.next(&[9, 8], &[9, 8, 12]);
        assert_eq!(intake.queue, vec![9, 8, 12]);
        assert_eq!(intake.joined, vec![12], "only 12 is new");
        assert!(intake.capped.is_empty());
    }

    #[test]
    fn a_pr_the_sweep_no_longer_ranks_actionable_leaves_the_queue() {
        // The one that matters most: after a review, that review is our own
        // latest activity, so an untouched PR goes SEEN and drops out. Keeping
        // it would re-review an unchanged PR every interval and spend its cap
        // on nothing -- and then ignore the author's real push when it came.
        let mut q = sweep(3);
        let intake = q.next(&[9, 8], &[8]);
        assert_eq!(intake.queue, vec![8]);
        assert!(intake.joined.is_empty());
    }

    #[test]
    fn a_quiet_pr_comes_back_when_its_author_pushes() {
        let mut q = sweep(3);
        assert_eq!(q.next(&[9], &[]).queue, Vec::<u64>::new());
        // The author pushes: UPDATED again, and nothing about it was final.
        let intake = q.next(&[9], &[9]);
        assert_eq!(intake.queue, vec![9]);
    }

    #[test]
    fn an_approved_pr_does_not_rejoin_on_a_stale_sweep() {
        // The sweep is a snapshot: the review that approved #9 lands before
        // the next GraphQL call reflects it. Without the done set, the same
        // interval that drops #9 puts it straight back.
        let mut q = sweep(3);
        q.mark_done(9);
        let intake = q.next(&[8], &[9, 8]);
        assert_eq!(intake.queue, vec![8]);
        assert!(intake.joined.is_empty(), "#9 is finished for this run");
    }

    #[test]
    fn a_picked_run_never_grows_past_what_was_picked() {
        // --pick --babysit means "watch these". The sweep finding other
        // actionable PRs is not an invitation to review them.
        let mut q = Queue::new(3, Some(vec![9]));
        let intake = q.next(&[9], &[9, 8, 12]);
        assert_eq!(intake.queue, vec![9]);
        assert!(intake.joined.is_empty());

        // ...and a picked PR that went quiet still comes back on a push.
        let intake = q.next(&[], &[9]);
        assert_eq!(intake.queue, vec![9]);
        assert_eq!(intake.joined, vec![9]);
    }

    #[test]
    fn a_capped_pr_is_visible_as_capped() {
        let mut q = sweep(1);
        assert!(!q.is_capped(9));
        q.record_pass(&[9]);
        assert!(q.is_capped(9), "the loop needs this to know it can stop");
    }

    #[test]
    fn the_cap_stops_a_conversation_becoming_a_loop() {
        let mut q = sweep(2);
        q.record_pass(&[9, 8]);
        q.record_pass(&[9]);
        assert_eq!(q.passes(9), 2);
        assert_eq!(q.passes(8), 1);

        let intake = q.next(&[9, 8], &[9, 8]);
        assert_eq!(intake.queue, vec![8], "9 has had its passes");
        assert_eq!(intake.capped, vec![9]);
    }

    #[test]
    fn a_capped_pr_is_named_once_per_run_not_once_per_interval() {
        let mut q = sweep(1);
        q.record_pass(&[9]);
        assert_eq!(q.next(&[9], &[9]).capped, vec![9]);
        // Still actionable next interval, and still capped -- but saying so
        // again every interval is noise, not news.
        assert!(q.next(&[], &[9]).capped.is_empty());
        assert!(q.next(&[], &[9]).queue.is_empty());
    }

    #[test]
    fn the_actionable_order_is_kept() {
        // The sweep ranks NEW before UPDATED, then by recency, and the queue
        // reviews in that order rather than whatever a set iterated.
        let mut q = sweep(3);
        let intake = q.next(&[], &[12, 9, 8]);
        assert_eq!(intake.queue, vec![12, 9, 8]);
        assert_eq!(intake.joined, vec![12, 9, 8]);
    }

    #[test]
    fn nothing_actionable_is_an_empty_queue() {
        let mut q = sweep(3);
        assert_eq!(q.next(&[9], &[]), Intake::default());
    }
}
