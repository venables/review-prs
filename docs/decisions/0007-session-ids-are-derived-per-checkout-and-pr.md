# Session ids are derived per checkout and PR

Recorded: 2026-09-01
Status: accepted

## Context

Both front-ends must agree on which Claude Code session a PR belongs to, so
`--continue` in either resumes the review the other started. A state file
would need keeping in sync.

## Decision

A PR's session id is a v5-form UUID derived from the repo root plus
`owner/name#N`. The repo root is in the hash on purpose: a second clone or
worktree of the same repo gets its own id for the same PR, so one checkout can
never resume, and corrupt, another's session.

Without `--continue`, a PR whose derived session already exists gets no
session flag at all and claude allocates a fresh id. Reusing a taken id is a
hard error, and quietly resuming would be a surprise.

Before resuming, `pgrep` checks whether another process still holds the
session. Claude Code treats an id as taken once the transcript file exists, so
it would not stop two agents writing one transcript.

The summary prints the id from `dash-p`'s meta envelope, which names the
session the review actually ran in, even when claude allocated its own.

## Consequences

- Golden tests pin the ids. Changing the derivation orphans every review
  already on disk.
- A `pgrep` false match only costs a fresh review, so the guard fails safe.
- The transcript is found by searching every project directory, because the
  cwd escaping Claude Code uses is undocumented and has changed before.
