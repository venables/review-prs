//! The checks on a PR's head commit, and the sweep's patience with them.
//!
//! A PR opened a minute ago has a linter still running, and a review posted
//! while the checks are red is a review of code its author is about to
//! change. So the sweep holds a PR until the checks on its head commit pass.
//! A one-shot run has no next poll to look again on, so it waits here for
//! pending checks to settle first; a loop leaves them for its next poll.
//! `--skip-wait-for-ci` turns both off.

use crate::interval::Interval;
use crate::prlist::{self, PrNode};
use crate::status::{Status, step};
use anyhow::Result;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ci {
    /// No checks on the head commit. A repo without CI is not a repo that
    /// must wait for ever, so this reads as ready.
    None,
    Pending,
    Passing,
    Failing,
}

impl Ci {
    /// From the StatusState on the head commit's rollup. The schema has
    /// exactly five values. Anything else reads as no checks, because a
    /// value this code cannot read must not hold a PR for ever.
    pub fn from_state(state: Option<&str>) -> Ci {
        match state {
            Some("SUCCESS") => Ci::Passing,
            Some("PENDING") | Some("EXPECTED") => Ci::Pending,
            Some("FAILURE") | Some("ERROR") => Ci::Failing,
            _ => Ci::None,
        }
    }

    /// The picker's column.
    pub fn label(self) -> &'static str {
        match self {
            Ci::None => "-",
            Ci::Pending => "PENDING",
            Ci::Passing => "PASSING",
            Ci::Failing => "FAILING",
        }
    }

    /// May the sweep review a PR in this state: passing, or nothing to pass.
    pub fn ready(self) -> bool {
        matches!(self, Ci::None | Ci::Passing)
    }

    /// Why a held PR is held, for the line that names it.
    pub fn reason(self) -> &'static str {
        match self {
            Ci::Pending => "pending",
            Ci::Failing => "failing",
            Ci::None | Ci::Passing => "passing",
        }
    }
}

/// How often a waiting run looks again: well inside how long a check takes,
/// and far above anything the API minds.
pub const POLL_SECS: u64 = 30;

/// How many refetches in a row may fail before the wait gives up. One
/// rate-limited answer at minute 25 must not throw the wait away; the same
/// bound the babysit loop puts on its own refresh.
pub const REFETCH_FAILURES: u32 = 3;

/// The line that names the PRs the sweep is not reviewing yet, and the flag
/// that would review them anyway.
pub fn held_line(held: &[(u64, Ci)]) -> String {
    let list: Vec<String> =
        held.iter().map(|(n, ci)| format!("#{n} ({})", ci.reason())).collect();
    let them = if held.len() == 1 { "it" } else { "them" };
    format!(
        "holding {} until CI passes: {}; --skip-wait-for-ci reviews {them} anyway",
        crate::ui::count(held.len(), "PR"),
        list.join(" ")
    )
}

/// The actionable PRs whose checks are still running.
fn pending(prs: &[PrNode], me: &str) -> Vec<u64> {
    prs.iter()
        .filter(|pr| prlist::engagement(pr, me) != prlist::Engagement::Seen)
        .filter(|pr| pr.ci() == Ci::Pending)
        .map(|pr| pr.number)
        .collect()
}

/// Wait for pending checks on the actionable PRs to settle, one poll at a
/// time, for at most `limit`. Returns the freshest list seen: a PR whose
/// checks finished, and any PR opened meanwhile, are both in it.
///
/// The fetch and the sleep are injected so the tests can run the loop
/// without a network or a clock. Time is counted as the sleeps add up rather
/// than read from the wall, which is what makes the limit testable at all.
pub fn settle<F, S>(
    prs: Vec<PrNode>,
    me: &str,
    limit: &Interval,
    status: &Status,
    mut refetch: F,
    mut sleep: S,
) -> Result<Vec<PrNode>>
where
    F: FnMut() -> Result<Vec<PrNode>>,
    S: FnMut(Duration),
{
    let mut prs = prs;
    let mut waiting = pending(&prs, me);
    if waiting.is_empty() {
        return Ok(prs);
    }
    // Permanent, so a log explains the gap that follows it; then a tick, so
    // the spinner stops claiming to be fetching.
    status.say(step::waiting_for_ci(&waiting, &limit.normalized));
    status.tick(step::still_waiting_for_ci(&waiting, 0));
    let mut waited = 0u64;
    let mut failures = 0u32;
    loop {
        if waited >= limit.secs {
            status.say(step::gave_up_on_ci(&waiting, &limit.normalized));
            return Ok(prs);
        }
        let nap = POLL_SECS.min(limit.secs - waited);
        sleep(Duration::from_secs(nap));
        waited += nap;
        status.tick(step::still_waiting_for_ci(&waiting, waited));
        // A failed look keeps the last list and looks again: the limit still
        // bounds the wait, so this cannot spin. Only a list that stops
        // answering altogether is an error, and that one is the caller's.
        match refetch() {
            Ok(fresh) => {
                failures = 0;
                prs = fresh;
            }
            Err(e) => {
                failures += 1;
                if failures >= REFETCH_FAILURES {
                    return Err(e);
                }
                status.say(format!("warning: could not refresh the PR list ({e:#}); looking again"));
                continue;
            }
        }
        waiting = pending(&prs, me);
        if waiting.is_empty() {
            return Ok(prs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interval;

    fn pr(n: u64, state: Option<&str>) -> PrNode {
        let rollup = match state {
            Some(s) => format!(r#"{{"state":"{s}"}}"#),
            None => "null".to_string(),
        };
        let json = format!(
            r#"{{"number":{n},"title":"t","isDraft":false,
              "updatedAt":"2026-08-10T10:00:00Z","reviewDecision":null,
              "author":{{"login":"alice"}},
              "comments":{{"nodes":[]}},"reviews":{{"nodes":[]}},"commits":{{"nodes":[]}},
              "headCommit":{{"nodes":[{{"commit":{{"statusCheckRollup":{rollup}}}}}]}}}}"#
        );
        serde_json::from_str(&json).unwrap()
    }

    fn seen(n: u64, state: Option<&str>) -> PrNode {
        // me commented last: SEEN, and nothing about its checks matters.
        let mut pr = pr(n, state);
        pr.comments.nodes.push(prlist::CommentNode {
            author: Some(prlist::Actor { login: Some("me".into()) }),
            updated_at: Some("2026-08-11T10:00:00Z".into()),
        });
        pr
    }

    fn numbers(prs: &[PrNode]) -> Vec<u64> {
        prs.iter().map(|p| p.number).collect()
    }

    #[test]
    fn the_five_states_and_the_absence_of_one() {
        assert_eq!(Ci::from_state(Some("SUCCESS")), Ci::Passing);
        assert_eq!(Ci::from_state(Some("PENDING")), Ci::Pending);
        assert_eq!(Ci::from_state(Some("EXPECTED")), Ci::Pending);
        assert_eq!(Ci::from_state(Some("FAILURE")), Ci::Failing);
        assert_eq!(Ci::from_state(Some("ERROR")), Ci::Failing);
        assert_eq!(Ci::from_state(None), Ci::None, "no checks at all");
        // A value outside the schema must not hold a PR for ever.
        assert_eq!(Ci::from_state(Some("SOMETHING_NEW")), Ci::None);
    }

    #[test]
    fn only_green_and_absent_checks_are_ready() {
        assert!(Ci::Passing.ready());
        assert!(Ci::None.ready(), "a repo without CI is not held");
        assert!(!Ci::Pending.ready());
        assert!(!Ci::Failing.ready());
    }

    #[test]
    fn the_held_line_names_each_pr_and_the_way_out() {
        assert_eq!(
            held_line(&[(9, Ci::Pending)]),
            "holding 1 PR until CI passes: #9 (pending); --skip-wait-for-ci reviews it anyway"
        );
        assert_eq!(
            held_line(&[(9, Ci::Pending), (8, Ci::Failing)]),
            "holding 2 PRs until CI passes: #9 (pending) #8 (failing); --skip-wait-for-ci reviews them anyway"
        );
    }

    #[test]
    fn nothing_pending_means_no_wait_and_no_second_fetch() {
        let limit = interval::normalize("30").unwrap();
        let mut fetches = 0;
        let mut slept = Duration::ZERO;
        let out = settle(
            vec![pr(9, Some("SUCCESS")), pr(8, None), pr(7, Some("FAILURE"))],
            "me",
            &limit,
            &Status::silent(),
            || {
                fetches += 1;
                Ok(Vec::new())
            },
            |d| slept += d,
        )
        .unwrap();
        assert_eq!(numbers(&out), vec![9, 8, 7]);
        assert_eq!(fetches, 0, "failing is settled; only pending is waited for");
        assert_eq!(slept, Duration::ZERO);
    }

    #[test]
    fn a_pending_check_is_polled_until_it_settles() {
        let limit = interval::normalize("30").unwrap();
        let mut answers = vec![
            vec![pr(9, Some("PENDING"))],
            vec![pr(9, Some("SUCCESS")), pr(12, Some("PENDING"))],
            vec![pr(9, Some("SUCCESS")), pr(12, Some("FAILURE"))],
        ]
        .into_iter();
        let mut slept = Duration::ZERO;
        let out = settle(
            vec![pr(9, Some("PENDING"))],
            "me",
            &limit,
            &Status::silent(),
            || Ok(answers.next().expect("more fetches than answers")),
            |d| slept += d,
        )
        .unwrap();
        // Three polls: still pending, then a newcomer pending, then settled.
        // The list returned is the freshest one, newcomer included.
        assert_eq!(numbers(&out), vec![9, 12]);
        assert_eq!(slept, Duration::from_secs(3 * POLL_SECS));
    }

    #[test]
    fn the_limit_ends_the_wait_with_the_pr_still_held() {
        // 1m is two polls of 30s. The third would pass the limit, so it is
        // not taken, and the PR comes back still pending for the sweep to
        // hold rather than review.
        let limit = interval::normalize("1").unwrap();
        let mut fetches = 0;
        let mut slept = Duration::ZERO;
        let out = settle(
            vec![pr(9, Some("PENDING"))],
            "me",
            &limit,
            &Status::silent(),
            || {
                fetches += 1;
                Ok(vec![pr(9, Some("PENDING"))])
            },
            |d| slept += d,
        )
        .unwrap();
        assert_eq!(fetches, 2);
        assert_eq!(slept, Duration::from_secs(60));
        assert_eq!(out[0].ci(), Ci::Pending);
    }

    #[test]
    fn a_limit_shorter_than_a_poll_is_still_honoured() {
        // Not reachable from the CLI, whose intervals are whole minutes, but
        // the arithmetic must not sleep past the limit if it ever is.
        let limit = Interval { normalized: "10s".into(), secs: 10 };
        let mut slept = Duration::ZERO;
        settle(
            vec![pr(9, Some("PENDING"))],
            "me",
            &limit,
            &Status::silent(),
            || Ok(vec![pr(9, Some("PENDING"))]),
            |d| slept += d,
        )
        .unwrap();
        assert_eq!(slept, Duration::from_secs(10));
    }

    #[test]
    fn a_seen_pr_is_not_waited_for() {
        // The sweep would not review it, so its checks are not its business.
        let limit = interval::normalize("30").unwrap();
        let mut fetches = 0;
        settle(
            vec![seen(6, Some("PENDING"))],
            "me",
            &limit,
            &Status::silent(),
            || {
                fetches += 1;
                Ok(Vec::new())
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(fetches, 0);
    }

    #[test]
    fn a_refetch_that_fails_once_is_looked_past() {
        // A rate-limited answer at minute 25 must not throw the wait away.
        // The failed look costs one poll and nothing else: the list before
        // it is kept, and the next look proceeds as if it had answered.
        let limit = interval::normalize("30").unwrap();
        let mut answers = vec![
            Err(anyhow::anyhow!("rate limited")),
            Ok(vec![pr(9, Some("SUCCESS"))]),
        ]
        .into_iter();
        let mut slept = Duration::ZERO;
        let out = settle(
            vec![pr(9, Some("PENDING"))],
            "me",
            &limit,
            &Status::silent(),
            || answers.next().expect("more fetches than answers"),
            |d| slept += d,
        )
        .unwrap();
        assert_eq!(out[0].ci(), Ci::Passing);
        assert_eq!(slept, Duration::from_secs(2 * POLL_SECS));
    }

    #[test]
    fn a_list_that_stops_answering_fails_the_wait() {
        // Three in a row is the same bound the babysit loop puts on its own
        // refresh: a list that will not answer is the caller's error to
        // explain, and a stale list must not be reviewed as fresh.
        let limit = interval::normalize("30").unwrap();
        let mut fetches = 0;
        let out = settle(
            vec![pr(9, Some("PENDING"))],
            "me",
            &limit,
            &Status::silent(),
            || {
                fetches += 1;
                Err(anyhow::anyhow!("rate limited"))
            },
            |_| {},
        );
        assert!(out.is_err());
        assert_eq!(fetches, REFETCH_FAILURES);
    }

    #[test]
    fn failed_looks_still_count_against_the_limit() {
        // Tolerating a failure must not make the wait unbounded: each failed
        // look spends a poll, and the limit ends the wait either way.
        let limit = interval::normalize("1").unwrap();
        let mut answers = vec![Err(anyhow::anyhow!("down")), Ok(vec![pr(9, Some("PENDING"))])].into_iter();
        let mut slept = Duration::ZERO;
        let out = settle(
            vec![pr(9, Some("PENDING"))],
            "me",
            &limit,
            &Status::silent(),
            || answers.next().expect("more fetches than answers"),
            |d| slept += d,
        )
        .unwrap();
        assert_eq!(slept, Duration::from_secs(60));
        assert_eq!(out[0].ci(), Ci::Pending);
    }
}
