# A run directory is made, not named

Recorded: 2026-09-01
Status: accepted

## Context

`--log-dir` is a fixed path, which is the point of passing one. Under cron,
overlapping runs are ordinary: the default hour-long timeout outlasts most
intervals. Two runs sharing state would read each other's results and report
them as their own.

A pid-named directory would be inherited by whichever later run the kernel
hands that pid to. On a container with low, fast-recycled pids, that puts a
run inside an older one's state.

## Decision

Each run creates `run-<random>` under the log directory with `mkdir`, retrying
on collision, and each pass gets `pass-N` inside it. The directory is created
0700 whatever the umask says: it holds the full diff, every prompt, and for
`panel` a checkout per panelist.

`panel` shares the helper for the same reason.

## Consequences

- The path is printed at the start of every run and again in the summary,
  because nobody can guess it.
- Per PR the pass directory holds `pr-N.review.md` (the one to open),
  `pr-N.json` (dash-p's answer), `pr-N.meta.json` (session, cost, model,
  written even on timeout) and `pr-N.log` (stderr).
- A failed-pass marker per PR lives here too, so the next babysit pass
  reviews that PR fresh instead of re-checking a stale session.
