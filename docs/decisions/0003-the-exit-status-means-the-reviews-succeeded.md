# The exit status means the reviews succeeded

Recorded: 2026-09-01
Status: accepted

## Context

The first use for a headless run is cron or a CI step. Those can only read
an exit status, and "the processes started" is not the fact they need.

## Decision

The exit status says whether the reviews succeeded:

- `dash-p` reports the truth of each review in a stable code: 0 ok, 10
  agent-error (including an `is_error` turn and garbage output), 20 timeout.
  There is no envelope to sniff for a failure it already reported.
- A command override is judged by its exit status alone. Prose on stdout is
  its normal shape, not a failure.
- The run exits 1 when any review in the final pass did not complete, and 130
  on an interrupt.
- The run never exits 1 silently. stderr always explains, and a site that
  already explained itself bails with `AlreadyReported` so the message is not
  printed twice.

## Consequences

- A review that overruns `--timeout` counts as failed.
- A reviewer that cannot even spawn is a failed review, not a dead run. The
  other PRs still get theirs.
- The fake `dash-p` in the test suite speaks the same codes, so the tests
  exercise the real classification.
