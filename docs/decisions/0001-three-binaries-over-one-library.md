# Three binaries over one library

Recorded: 2026-09-01
Status: accepted

## Context

Two of the front-ends read a repo's pull requests. They must agree on which
PRs are worth reviewing and on which session each PR belongs to. Two
implementations kept in step by hand would drift.

## Decision

One crate, one library, three thin binaries:

- `autoreview` reviews PRs headlessly through a bounded pool.
- `review-prs` fans the same PRs into one terminal tab each.
- `panel` reviews one diff with several models.

The PR list, the ranking, the picker and the session derivation live in the
library. The binaries only decide what to do with the numbers.

## Consequences

- A change to ranking or session ids reaches every front-end at once.
- The binaries are self-contained. Nothing is read from the checkout at run
  time, so a copy anywhere on `PATH` works.
- Wording shared by the three, such as the progress steps, lives in one place
  (`status::step`) so they say the same thing the same way.
