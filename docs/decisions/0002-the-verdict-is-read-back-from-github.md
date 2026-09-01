# The verdict is read back from GitHub

Recorded: 2026-09-01
Status: accepted

## Context

A reviewer is a model. A model that believed its own report would show
"approved" for an approval that never landed on the PR.

## Decision

After a review finishes, ask `gh` whether our own login submitted a review on
that PR since the job started: approved, commented, or changes requested. That
answer fills the VERDICT column.

The agent's own report is a trailer: a system-prompt instruction asks it to
end its reply with a fenced `autoreview` block holding one JSON object. The
trailer supplies risk, finding counts and the panel. Its decision is the
verdict only when GitHub has nothing to say.

A `--no-post` run ignores the trailer's decision entirely. The reviewer ran a
skill with no posting step, so its claim about what landed cannot be true.

## Consequences

- VERDICT works under any reviewer, including a command override that owns
  its own sessions.
- "nothing posted" is not a rejection. It means no review was submitted, or
  GitHub has no record of one.
- A trailer decision that GitHub contradicts prints a note naming the claim.
- The trailer is best-effort. A review that never writes it costs a `-` in
  those columns, nothing more.
