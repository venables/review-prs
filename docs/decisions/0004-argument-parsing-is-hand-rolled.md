# Argument parsing is hand-rolled

Recorded: 2026-09-01
Status: accepted

## Context

The contract for bad input is byte-exact error strings, exit 1 rather than 2,
and an `=`-only value form for `--babysit` so a bare `--babysit` can take its
default. The test suite pins all three.

## Decision

Each binary parses its own argv with a match: `cli.rs` for `autoreview`,
`tabs/cli.rs` for `review-prs`, `panel/cli.rs` for `panel`. Interval parsing
is shared in `interval.rs` so a value one front-end accepts and the other
rejects cannot happen. No clap.

## Consequences

- Adding a flag means extending the match, the `HELP` text, and a test that
  pins the error string for a bad value.
- Environment defaults are read only by the runs that use them. A bad value
  in a profile must not refuse a run that never reads it.
- An argument that is not valid UTF-8 is refused rather than lossily decoded
  into a path the caller never asked for.
