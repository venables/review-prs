# recheck-pr

The second look. You panel-reviewed a PR, the findings landed as comments, the author pushed fixes and argued back on a couple. This skill goes and checks whether they actually fixed it, closes the threads it's satisfied with, and approves if the PR clears the bar.

## Install

```
npx skills add venables/skills --skill recheck-pr
```

## How to use it

Ask in plain English, in the same session where you ran the review:

- "double check this PR and approve if you're satisfied"
- "re-check PR 27 — did they remediate or explain the findings?"
- "look at it again now that they've pushed, and if the new changes are significant run another panel review"
- "verify they addressed the review comments and stamp it"

If you don't name a PR, it auto-detects from your current branch.

## What it does

- **Bails immediately when nothing changed.** If the head SHA still matches what you reviewed and nobody replied to your threads, it says `nothing changed since <sha>, N findings still open` and stops. Two API calls, one line of output — no report, no resolve, no approval.
- **Rebuilds the ledger** from the prior review: every finding, its severity, its `file:line`, and the thread it was posted to.
- **Diffs the delta** — `reviewed SHA ... current head` — because that's the code nobody has reviewed yet. Detects force-pushes (`diverged`), which mean the delta can't be isolated.
- **Adjudicates every finding** as `REMEDIATED` / `EXPLAINED` / `MOOT` / `OUTSTANDING`, by reading the new code. A reply saying "fixed in abc1234" is a claim, not evidence. A fix that handles the reported case but leaves the same bug one call site over stays outstanding.
- **Runs another `panel-review`** over the new commits when the delta is significant enough that your read of it isn't adequate review on its own: structural rework, unrelated work carried along, risk-carrying surface (auth, payments, migrations, CI), a force-push, or just size.
- **Replies and resolves** — one confirmation reply on each satisfied thread that hasn't been replied to yet, then resolves it. Skips threads that are already resolved, and never touches threads you didn't open.
- **Approves via `approve-pr`** (which picks its own short fun body) when the gate passes.

## The approval gate

All six must hold:

1. Zero outstanding findings — everything remediated, explained, or moot.
2. Nothing new blocking, from your read of the delta or the fresh panel.
3. If the delta needed a panel re-review, it actually ran.
4. Not a draft.
5. Not your own PR.
6. Head SHA unchanged since you started reading.

Fail any one and it doesn't approve. It also doesn't leave a `request-changes` review — the open threads carry the signal.

## Gotchas

- **The prior review must be in this session's context.** This skill re-checks a review that already happened. Without the findings, it stops and tells you rather than reconstructing a baseline from whatever comments happen to be on the PR (which might be a bot's, or someone else's, or a subset).
- **It doesn't fix the code.** You're the reviewer. An outstanding finding gets a reply, not a commit. For the author's side of that, use `pr-comment-handler`.
- **One extra round, not a loop.** A fresh panel that surfaces must-fixes ends the skill: it reports and stops. For iterate-to-convergence, use `panel-review-loop`.
- **It only resolves its own threads.** Resolving another reviewer's thread speaks for them; resolving a bot's hides signal from a human who hasn't looked.
- **Dry run available.** `RECHECK_PR_DRY_RUN=1` (or just ask for a dry run) reads everything, writes `./recheck.json` + `./report.md`, and replies to / resolves / approves nothing.

## Related

`panel-review` generates the findings. `auto-review` reviews, posts, and stamps in one pass. `approve-pr` just stamps. `pr-comment-handler` is the author-side counterpart: it _acts on_ the comments this skill leaves.
