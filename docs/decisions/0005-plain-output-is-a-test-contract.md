# Plain output is a test contract

Recorded: 2026-09-01
Status: accepted

## Context

The bash suite runs the real binaries against fake `gh`, `gum`, `cmux` and
`dash-p` and greps what they print. It never has a TTY. People reading a cron
log grep the same strings.

## Decision

One pass has two renderings:

- On a TTY: a live board, one spinner row per running review, finished rows
  promoted to permanent lines above it, a progress bar below, and a summary
  as rounded tables.
- Off a TTY: one plain line per state change (`start`, `done`, `FAILED`,
  `TIMEOUT`) and a plain aligned summary table with the same columns.

The plain strings are byte-identical across refactors. The tests pin them and
so do people's eyes.

## Consequences

- A wording change ships with its test change in the same commit.
- Behavior that only exists on a TTY is pinned by unit tests in `src/ui.rs`
  that take a width as an argument, because the terminal under `cargo test`
  is whatever the developer has and the suite has none.
- The plain table pads by display width, not bytes, because verdict, risk and
  model cells carry agent-authored text.
