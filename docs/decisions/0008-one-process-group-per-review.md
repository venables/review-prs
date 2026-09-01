# One process group per review

Recorded: 2026-09-01
Status: accepted

## Context

A review is a tree of processes: `dash-p`, claude, and whatever the skill
runs. Stopping it must stop all of them. A dropped ssh session must not
orphan reviewers that keep spending and keep holding their sessions open,
which would make the next `--continue` refuse to resume them.

## Decision

- Each review is spawned with `process_group(0)`, so its pid is its group id.
- Stopping a review is one `killpg`: TERM, TERM again, then KILL. A reviewer
  that shrugs off TERM must not outlive its timeout, because the pass waits
  on it.
- INT, TERM and HUP all mean the same thing: stop the reviews, keep the
  summary, exit 130.
- Each job has a monitor thread whose whole body is `child.wait()`. It sends
  `JobReaped` the moment wait returns, then runs the GitHub readback, then
  sends `JobExited`.

## Consequences

- A job killed from outside is an ordinary wait return with a signal-death
  status, not a special case. The slot frees at once.
- `JobReaped` disarms the deadline and pins the elapsed time, so a slow
  readback never delays the guard, the clock, or the other jobs.
- The deadline guard covers only `dash-p`'s known hang hole, five seconds
  past its own timeout. `dash-p` enforces the real cap.
- The interrupt path and the guard never `killpg` a reaped pid, which the
  kernel may already have recycled.
