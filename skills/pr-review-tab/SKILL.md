---
name: pr-review-tab
description: >
  Run an unattended PR review inside a terminal tab spawned by the
  `review-prs` tool, then manage that tab's lifecycle: delegate to
  `auto-review` (review, post comments, approve if the gate passes), and
  when the PR is approved, close the enclosing Herdr/cmux tab so a finished
  review cleans up after itself. With `--babysit`, a PR that does not come
  back approvable starts an in-session `/loop` that re-runs `recheck-pr`
  every N minutes until it can be approved — then closes the tab, which
  ends the loop. Use whenever a spawned review tab should self-close on
  approval or keep re-checking a not-yet-approvable PR on an interval —
  invoked as `pr-review-tab <PR> [--babysit <interval>]`, which is what
  `review-prs --auto` / `review-prs --babysit` seed into each tab. Do NOT
  use for an ordinary review with no tab lifecycle (`auto-review` /
  `panel-review`), to re-check a PR without the close/loop wrapper
  (`recheck-pr`), or to babysit a PR you *authored* into a mergeable state
  (`babysit-pr`, a different job entirely).
---

# pr-review-tab

The lifecycle wrapper for a review that runs in its own throwaway tab. The
`review-prs` tool fans out one terminal tab per PR and seeds each with a
review command; this skill is that command. It owns two things and nothing
else:

1. **Close on approve.** A review that ends in an approval should not leave
   a dead tab sitting around. Once the PR is approved, close the tab.
2. **Babysit until approvable.** A PR that is not clean yet keeps its tab
   open and re-checks it on an interval, so a fix pushed at 2am gets
   approved without you re-running anything.

It does **not** reimplement reviewing. `auto-review` does the review, the
posting, and the approval gate; `recheck-pr` does the second look;
`approve-pr` does the stamp. This skill only wires them to the tab.

Those skills also own the wording of everything that reaches GitHub, and
each holds itself to ASD-STE100 Simplified Technical English. Write the
one-line outcome you emit before closing the tab the same way: a short
sentence, active voice, no jargon.

```
pr-review-tab <PR> [--babysit <interval>]
      │
      ├─► auto-review <PR>            (review → post → approve-if-clean)
      │
      ├─► approved?  ─yes─►  close this tab.  done.
      │
      ├─► not approved, no --babysit  ─►  leave tab open, report, stop.
      │
      └─► not approved, --babysit
              └─► /loop <interval>: recheck-pr <PR>; approve? → close tab (ends loop)
```

## Where this runs

Inside a Herdr pane (`$HERDR_ENV == 1`, `$HERDR_TAB_ID` set) or a cmux
surface (`$CMUX_SURFACE_ID` set), spawned by `review-prs`, with
`--dangerously-skip-permissions` so the close/loop commands run without
prompting. Those env vars point at **this** tab — that is what makes
self-close possible.

If neither is set (someone ran the skill by hand outside a spawned tab),
still do the review and the babysit loop, but there is no tab to close:
report the decision instead and, in babysit mode, say the loop won't
self-terminate (press Esc to stop it). Never guess a tab id.

## Arguments

`pr-review-tab <PR> [--babysit <interval>]`

- `<PR>` — the PR number (or ref) to review. Required.
- `--babysit <interval>` — turn on the re-check loop; `<interval>` is a
  `/loop` duration like `30m`, `15m`, `1h`. Absent → single pass, no loop.
  Present with no value → default `30m`.

## Prerequisites

`auto-review` (always), and for `--babysit`: `recheck-pr` and the `/loop`
command. `auto-review` itself pulls in `panel-review`,
`auto-post-panel-review-comments`, and `approve-pr`. If a required skill is
missing, stop and say so rather than hand-rolling it — the whole point is
composing the real skills.

## The pass

### 1. Resolve the PR and the tab

Resolve the PR once up front and name it (`PR #N — title — url`) so a wrong
target is visible before anything outward-facing. Capture whether this is a
managed tab and, if so, its handle:

```bash
me="$(gh api user --jq .login)"
# tab handle for the close step (empty when not in a managed tab)
if [[ "${HERDR_ENV:-}" == "1" && -n "${HERDR_TAB_ID:-}" ]]; then
  tab_kind=herdr
elif [[ -n "${CMUX_SURFACE_ID:-}" ]]; then
  tab_kind=cmux
else
  tab_kind=none
fi
```

### 2. Review — run `auto-review`

Invoke the `auto-review` skill for the PR with approval intent (the `auto`
sweep that seeded this tab _is_ the approval intent). Let it run end to
end: it reviews, posts the legitimate findings, and approves iff its gate
passes. Do not reimplement or second-guess its gate — its decision is the
input to everything below.

### 3. Determine the outcome

`auto-review` reports whether it approved. Confirm authoritatively against
the PR — the tab should close only if an approval actually landed:

```bash
state="$(gh pr view <N> --json reviews \
  --jq "[.reviews[] | select(.author.login == \"$me\")] | last | .state // \"NONE\"")"
# approved when "$state" == APPROVED
```

### 4a. Approved → close the tab

The review is done and stamped; clean up. Then stop — nothing else runs.

```bash
case "$tab_kind" in
  herdr) herdr tab close "$HERDR_TAB_ID" ;;
  cmux)  cmux close-surface --surface "$CMUX_SURFACE_ID" ;;
  none)  echo "approved PR <N>; not in a managed tab, nothing to close" ;;
esac
```

Closing the tab kills this process — that is intended, and it is the last
thing you do. Report the approval first (one line) so the outcome is in the
transcript before the pane goes away.

### 4b. Not approved, no `--babysit`

Leave the tab open — the author needs to see the comments and someone may
want to drive it interactively. Report `auto-review`'s decision and the
blocking reason, and stop. Do **not** close the tab; do **not** loop.

### 4c. Not approved, `--babysit` → the re-check loop

The PR isn't clean yet, but you're babysitting it: re-check it on an
interval until it becomes approvable, then close the tab. This works
**in-session** on purpose — `auto-review` just ran here, so its findings
and the reviewed SHA are in context, which is exactly what `recheck-pr`
needs for its second look. A fresh process would lose that.

Start a `/loop` at the requested interval whose body re-checks the PR and
closes the tab on approval:

```
/loop <interval> Re-check PR <N> with the recheck-pr skill against the
auto-review already in this session's context. recheck-pr's fast path makes
a no-change cycle cheap: if nothing was pushed, it reports one line and
waits for the next wakeup. If the author has pushed and recheck-pr's gate
passes so you approve the PR, close this review tab — `herdr tab close
"$HERDR_TAB_ID"` under Herdr, else `cmux close-surface --surface
"$CMUX_SURFACE_ID"` — which ends the loop.
```

Then stop and let the wakeups run. Notes that matter:

- **The tab close is the loop's terminator.** There is no separate "stop
  the loop" step: when a cycle approves and closes the tab, the process
  dies and the pending wakeups die with it. Until then the pending loop
  keeps the tab alive between cycles (a local session, so it stays up).
- **recheck-pr owns the re-check judgment**, including whether a big delta
  warrants a fresh `panel-review` and whether the gate passes. Don't
  pre-empt it; just hand it the PR each cycle.
- **No managed tab (`tab_kind == none`)?** The loop still re-checks, but it
  can't self-terminate on approval — say so when you start it, and note the
  user can press Esc to cancel pending wakeups. Prefer running babysit from
  a spawned tab.

## Gotchas

- **Don't reimplement the review.** `auto-review` and `recheck-pr` do the
  work; this skill only closes the tab and starts the loop. If you find
  yourself running `panel-review.sh` or posting comments directly, stop.
- **Close only your own tab.** Use `$HERDR_TAB_ID` / `$CMUX_SURFACE_ID`
  from the environment — the tab this session runs in. Never target a tab
  id you read from a listing; that's someone else's work.
- **Approve is `auto-review`'s call, not yours.** This skill never lowers
  the approval bar to close a tab faster. If the gate didn't pass, the tab
  stays open (or keeps looping) — an unearned approval to tidy up is the
  worst possible trade.
- **Report before you close.** The close kills the pane; if the only record
  of the decision was going to be on screen, it's gone. Emit the one-line
  outcome first.
- **Babysit loops in-session for a reason.** `recheck-pr` needs the prior
  review in context. Don't restructure this to spawn a fresh review each
  cycle — you'd lose the baseline and turn every wakeup into a full
  re-review.
