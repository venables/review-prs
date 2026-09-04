# Panel is a program, and the judgment is one model call

Recorded: 2026-09-01
Status: accepted

## Context

The same review was an agent skill. A coordinator model read 656 lines of
instructions and drove a 1094-line bash script that fanned the panelists out.
That put a model in charge of work that needs no judgment, and made the
fan-out only as reliable as the coordinator's willingness to follow
instructions.

## Decision

The mechanical half is Rust: spawn each panelist through `dash-p`, poll,
retry a panelist that produced nothing once, collect output, remove the
worktrees. The judgment half is one synthesis call.

- The synthesis runs in a checkout of the reviewed code with read access. A
  synthesizer handed only the panelists' prose can merge claims but cannot
  check any of them.
- It is told which panelists failed, so silence is not read as agreement, and
  how many answered, so "flagged by 2 of 3" means what it says.
- It is supervised exactly like a panelist. A wedge after every panelist has
  been paid for is the most expensive place in the run to hang.
- A committed target gives each panelist its own throwaway worktree pinned to
  the same commit, plus one for the synthesis. Parallel reviewers racing on
  `target/` and `node_modules` produce flaky findings and leak edits into
  each other's reading.
- Uncommitted work has no ref to pin, so panelists read the working tree with
  `--perms read-only`.

## Consequences

- Worktrees are registered in the user's real repository, so they are removed
  on every exit path including ctrl-C, which stops the panelists first.
- `--pr`, review approaches and `$PANEL_REVIEW_PANELISTS` still live in the
  `panel-review` skill. Teaching the skill to call the binary is the planned
  follow-up.
- Panelist sections print in panel order, not finishing order, so two runs of
  the same panel are comparable.
