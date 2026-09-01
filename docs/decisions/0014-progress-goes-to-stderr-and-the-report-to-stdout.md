# Progress goes to stderr and the report to stdout

Recorded: 2026-09-01
Status: accepted

## Context

Every entry point spends several seconds on network calls before its first
real line. On a slow link that reads as a hung tool, and the first thing
anyone does with a hung tool is press ctrl-C. A run whose stdout is piped to
a file still has a terminal to spin on.

## Decision

- Startup progress is a `Status` spinner on stderr when stderr is a terminal:
  one line that rewrites itself and leaves nothing behind, so the report
  still starts at the top. Anywhere else it is one plain line per step.
- A message that changes with time (`tick`) says nothing off a terminal. A
  line every quarter second is not a log, it is a flood.
- A permanent line written while the spinner is live goes through `say` or
  `suspend`, so it never fuses with the progress message.
- `Drop` clears the spinner, so a `?` that returns early cannot leave it
  ticking under the error.
- The pass board itself draws on stdout, because on a terminal it is part of
  the report, and its notes print above the bars.

## Consequences

- The step wording lives in `status::step` and is shared by all three
  binaries, with tests that read each step back as a sentence.
- The spinner template is parsed only on a terminal, so a unit test parses it
  too. A typo would otherwise reach a user as a panic and never reach CI.
- `panel` prints its header and each panelist section inside `suspend`, so
  the two long waits keep ticking around them.
