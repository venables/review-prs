# Babysit rebuilds the queue every interval

Recorded: 2026-09-01
Status: accepted

## Context

A queue fixed when the run started misses a PR opened a minute later for the
whole run, however long the run is. Waiting for an approval that is never
coming would re-review a PR every interval forever.

## Decision

One rule decides membership: the sweep says the PR is actionable now. A review
we post becomes our own latest activity on the PR, so an untouched PR goes
quiet by itself and an author pushing a fix makes it UPDATED again.

Two things overrule the sweep, because the sweep is a snapshot that lags by
up to a poll:

- Leaving is decided per PR by `gh pr view`. Approved, merged and closed are
  final for the run, so a stale list cannot re-queue a PR on the interval that
  dropped it.
- Under `--pick`, only the PRs the user chose are eligible. A run told to
  watch two PRs never grows to five.

Two bounds end the run cleanly:

- `--max-passes` caps reviews per PR, because every review is activity that
  makes the PR actionable again. A push resets the cap, fingerprinted by head
  SHA rather than commit date.
- `--max-idle` caps consecutive checks that found nothing. The check straight
  after a pass does not count: it is idle by construction.

Three consecutive refresh failures end a babysit run with exit 1. A `--watch`
run never ends on its own and backs off instead.

## Consequences

- Every pass after the first resumes the recorded session whether or not
  `--continue` was passed, so the findings the author is answering survive.
- A transient API error keeps the current queue and tries again.
- The loop is the process itself, not an in-session `/loop`, so an interval
  that never converges is one process you can see and kill.
