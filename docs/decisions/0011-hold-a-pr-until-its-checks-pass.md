# Hold a PR until its checks pass

Recorded: 2026-09-01
Status: accepted

## Context

A PR opened a minute ago has a linter still running. A review posted while
the checks are red is a review of code its author is about to change, and it
costs a full panel.

## Decision

The sweep holds a NEW or UPDATED PR until the checks on its head commit pass.
The checks come from the same GraphQL query as the PR list, as one aliased
field for the head commit only.

- A one-shot run has no next poll, so it waits here for pending checks, up to
  `$AUTOREVIEW_CI_WAIT`, then gives up on that PR and says so.
- A `--babysit` loop leaves a held PR for its next poll and names it once per
  state, so a PR with red checks never looks ignored.
- `--pick` and `--watch` never wait. The picker shows the checks in a column
  and reviews what you pick whatever it says.
- `--skip-wait-for-ci` turns the gate off everywhere.

## Consequences

- The schema has exactly five states. Anything else reads as no checks at
  all, because a value this code cannot read must not hold a PR forever.
- A rate-limited answer at minute 25 must not throw the wait away. Failures
  are tolerated but bounded, so the wait cannot become unbounded.
- A babysit run that found only held PRs is not finished. It looks again on
  its interval and reviews them as their checks pass.
