# Decisions

One file per decision, numbered in the order they were recorded. Each one
says what was decided, why, and what it costs, in a form that survives the
code being rewritten around it.

Add a file in the same PR as the decision it records. Keep the old one when
a decision changes: mark it superseded and point at the new file.

## Format

```markdown
# Title, as a sentence

Recorded: YYYY-MM-DD
Status: accepted | superseded by NNNN

## Context

What was true, and what problem that caused.

## Decision

What we do now.

## Consequences

What this costs, and what it rules out.
```

Files recorded on 2026-09-01 were reconstructed from the code, its doc
comments and the commit history, not written at the time of the decision.

## Index

- [0001](0001-three-binaries-over-one-library.md) Three binaries over one library
- [0002](0002-the-verdict-is-read-back-from-github.md) The verdict is read back from GitHub
- [0003](0003-the-exit-status-means-the-reviews-succeeded.md) The exit status means the reviews succeeded
- [0004](0004-argument-parsing-is-hand-rolled.md) Argument parsing is hand-rolled
- [0005](0005-plain-output-is-a-test-contract.md) Plain output is a test contract
- [0006](0006-board-rows-carry-no-hyperlinks.md) Board rows carry no hyperlinks
- [0007](0007-session-ids-are-derived-per-checkout-and-pr.md) Session ids are derived per checkout and PR
- [0008](0008-one-process-group-per-review.md) One process group per review
- [0009](0009-a-run-directory-is-made-not-named.md) A run directory is made, not named
- [0010](0010-babysit-rebuilds-the-queue-every-interval.md) Babysit rebuilds the queue every interval
- [0011](0011-hold-a-pr-until-its-checks-pass.md) Hold a PR until its checks pass
- [0012](0012-panel-is-a-program-and-the-judgment-is-one-model-call.md) Panel is a program, and the judgment is one model call
- [0013](0013-skills-are-vendored-and-versioned-with-the-binaries.md) Skills are vendored and versioned with the binaries
- [0014](0014-progress-goes-to-stderr-and-the-report-to-stdout.md) Progress goes to stderr and the report to stdout
