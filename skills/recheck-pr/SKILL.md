---
name: recheck-pr
description: >
  Re-check a PR you already reviewed: for each finding from the prior
  review, decide whether it was actually remediated (read the new code —
  don't trust "fixed in abc123") or adequately explained, reply to and
  resolve the threads you're satisfied with, run a fresh `panel-review`
  over the new commits when the delta warrants it, and approve via
  `approve-pr` when satisfied. Use for "double check this PR", "recheck PR
  27", "did they fix the issues?", "look at it again and stamp it if it's
  good", "verify they addressed the review comments". Also use
  proactively after a `panel-review` /
  `auto-review` when the author pushes fixes. Requires the prior review in
  this session's context — without it, stop and say so rather than
  guessing. Do NOT use to *generate* a first review (`panel-review`), to
  post findings (`auto-post-panel-review-comments`), to act on comments as
  the PR *author* (`pr-comment-handler`), to iterate to convergence
  (`panel-review-loop`), or to approve without re-checking (`approve-pr`).
---

# recheck-pr

The second look. A panel reviewed the PR, the findings landed as comments,
the author pushed fixes and argued back on a couple. Now you go read what
actually changed and decide: is this good enough to stamp?

```
prior findings (in context)
      │
      ├─► head == reviewed SHA and no new replies?  ─yes─►  "Nothing changed." stop.
      │
      ├─► adjudicate each: remediated / explained / moot / outstanding
      │       (read the new code — a reply saying "fixed" is a claim, not evidence)
      │
      ├─► delta significant?  ─yes─►  panel-review over the new commits
      │
      ├─► thread hygiene: reply + resolve the ones you're satisfied with
      │
      └─► gate passes?  ─yes─►  approve-pr
```

You are the **reviewer** here, not the author. You read code, reply to
threads, and approve. You do **not** fix the author's code, commit, or
push — if a finding is still outstanding, that's the author's work, and
your job is to say so clearly and leave the thread open.

## Language

Write every word that lands on GitHub in ASD-STE100 Simplified Technical
English — each thread reply, each new inline comment, and the approval
body.

- Write short sentences. Keep an instruction to 20 words. Keep a
  statement to 25 words.
- Put one idea in each sentence.
- Use the active voice. Write "the retry path bypasses the guard", not
  "the guard is bypassed by the retry path".
- Use simple tenses. Write "the test fails", not "the test has been
  failing".
- Use one word for one meaning. Do not change "function" to "method" to
  "routine" in the same text.
- Use the simplest word that is correct. Remove jargon, idioms,
  metaphors, and figures of speech.
- Do not make a noun out of a verb. Write "when the job starts", not "at
  job initiation".
- Keep a paragraph to six sentences or fewer.

An outstanding reply has to tell the author exactly what to do next.
`The retry path at line 88 still bypasses the guard.` does that in one
sentence; a paragraph of hedging does not.

## Prerequisite: the prior review must be in context

This skill re-checks a review that already happened. It needs two things
from this session's context:

1. **The findings** — the `panel-review` synthesis (buckets + severities +
   `file:line`), the `auto-review` run that posted them, or a findings list
   the user pasted.
2. **The reviewed SHA** — the head the review ran against.

If the findings aren't in context, **stop and say so.** Don't reconstruct a
baseline by reading whatever unresolved comments happen to be on the PR:
those may be someone else's review, a bot's, or a subset of what the panel
actually said, and silently re-checking against the wrong baseline produces
an approval that means nothing. Tell the user the prior review isn't in
context and offer the two real options: paste the review, or run a fresh
`panel-review` / `auto-review`.

The reviewed SHA is the one recoverable piece. If the findings are in
context but the SHA isn't, take it from the `original_commit_id` of the
review comments the prior run posted (`gh api
"repos/{owner}/{repo}/pulls/<N>/comments" --jq '.[].original_commit_id'`)
— they were anchored to it. If that turns up nothing either, stop: without
a reviewed SHA there is no delta to check, and re-reading the whole PR is a
fresh review, not a re-check.

`approve-pr` must also be installed, since the approval step delegates to
it. `panel-review` is needed only if the delta turns out to be significant.

## Target

Resolve the PR once, up front, and reuse it throughout:

```bash
gh pr view <ref> --json number,url,title,author,state,isDraft,headRefOid,headRefName
```

Auto-detect from the current branch when the user didn't name one. Surface
`PR #N — <title> — <url>` before doing anything outward-facing, so a wrong
auto-detect is caught before a resolve or an approval notification goes out.

If the PR is `MERGED` or `CLOSED`, stop and say so — there's nothing to
re-check or approve.

## Fast path: nothing changed

**Do this before anything else.** Most re-checks are asked for prematurely —
the author hasn't pushed yet. Detect that in two cheap calls and get out.

```bash
gh pr view <ref> --json headRefOid --jq '.headRefOid'   # NEW
scripts/fetch_pr_threads.sh <pr>
```

Exit immediately when **both** hold:

- `NEW` equals the reviewed SHA (`OLD`) — no code was pushed.
- No unresolved thread of yours has a reply you haven't seen — i.e. every
  one is `has_reply: false`, or `last_author` is you. Nothing was argued.

There is nothing to adjudicate: no new code, no new argument. Don't rebuild
the ledger, don't diff, don't re-read the PR, don't reply, don't resolve,
don't approve. Say it in one line and stop:

```
PR #61 — nothing changed since aaaa1111. 2 findings still open.
```

Name the open count so the user knows the state without asking. If the PR
was already fully satisfied at `OLD` but you never approved it, say that
instead (`nothing changed; 0 open — rerun with approval intent to stamp it`)
rather than silently approving off a stale read.

Anything else — a push (`NEW != OLD`), or a reply you haven't adjudicated —
means there's real work, so fall through to the full pass below.

## The pass

### 1. Rebuild the ledger

Write down every finding from the prior review as a row: severity,
`file:line`, the one-line claim, and the thread it was posted to (if it was
posted). Status starts as `UNKNOWN` for all of them. This ledger is the
spine of the whole pass — the gate reads it, and the report prints it.

Fetch the threads (the script exists because REST doesn't expose per-thread
`isResolved`, and reply chains have to be reconstructed from a flat list):

```bash
scripts/fetch_pr_threads.sh <pr>
```

It emits every review thread — resolved ones included, because you need to
know which to leave alone — with `is_resolved`, the reply chain, and three
precomputed flags per thread: `mine` (you authored the first comment),
`has_reply` (someone replied after it), and `last_author`. Match each
finding to its thread by `path` + `line` + the comment body.

Findings that were never posted (routed to Linear, or reported to the user
only) still belong in the ledger. They just have no thread, so step 5 skips
them.

### 2. Get the delta

The reviewed SHA is `OLD`; the current head is `NEW`.

```bash
gh api "repos/{owner}/{repo}/compare/${OLD}...${NEW}" \
  --jq '{status, ahead_by, behind_by, files: [.files[] | {filename, additions, deletions, status}], commits: [.commits[] | {sha: .sha[0:8], message: .commit.message}]}'
```

`status` is the important field:

- `ahead` — the author pushed on top of what you reviewed. This is the
  normal case: the delta is exactly `ahead_by` commits, and it's the only
  code nobody has reviewed.
- `identical` — nothing was pushed, so you only got here because the author
  replied. Skip the diff entirely: adjudicate those replies (step 3, the
  EXPLAINED path) and nothing else. Findings nobody argued are still
  outstanding.
- `diverged` — the author rebased or force-pushed, so `OLD` is no longer an
  ancestor. You can still read the compare's file list, but it now includes
  base-branch churn, and there is **no clean delta to isolate**. Treat this
  as a re-review trigger (see step 4) and say why in the report.

Then read the actual changes. `gh pr diff <N>` for the whole PR; for the
delta specifically, the compare above already carries per-file stats, and
`gh api "repos/{owner}/{repo}/compare/${OLD}...${NEW}" -H "Accept:
application/vnd.github.diff"` gives you the patch.

### 3. Adjudicate each finding

For each ledger row, assign exactly one status. The rule that matters:
**the code is the evidence; the reply is a claim.** "Fixed in abc1234"
tells you where to look, not that it's fixed.

- **REMEDIATED** — you read the new code at the site and the problem is
  genuinely gone. Note the commit that did it. A fix that handles the
  reported case but leaves the same bug one call site over is **not**
  remediated; it's outstanding, and saying so is the whole value of a
  second look.
- **EXPLAINED** — the author pushed back and, having checked the code
  yourself, the pushback holds. "There's a guard upstream in `router.ts:40`"
  is only an explanation once you've opened `router.ts:40`. An explanation
  you can't substantiate is outstanding, not explained.
- **MOOT** — the code the finding referred to no longer exists (deleted,
  rewritten around) and the concern doesn't apply to what replaced it.
  Confirm that last clause; a rewrite that carries the bug forward is
  outstanding.
- **OUTSTANDING** — not fixed, not explained, partially fixed, or an
  explanation that didn't survive contact with the code.

Two things the baseline doesn't cover, both of which you own:

- **New problems in the delta.** The fixes are new, unreviewed code and can
  introduce their own bugs. Anything you find gets added to the ledger as a
  `NEW` row at your own severity. This is a real finding, not a nitpick —
  it blocks the gate exactly like an outstanding one.
- **Findings the delta made obsolete or relevant.** A refactor in the delta
  can resurrect a finding you'd marked moot, or moot one you'd marked
  outstanding. Re-read, don't carry statuses forward mechanically.

Don't re-litigate settled ground. A finding the author explained in a way
you accepted stays accepted; you're checking remediation, not running the
review again from scratch.

### 4. Decide whether the panel needs to see the delta

Here's the honest framing: **the panel reviewed `OLD`. Nobody has reviewed
the delta.** So the question isn't "was the change big?" — it's _"is the
delta small and legible enough that me reading it in step 3 counts as
adequate review?"_

Read it yourself and re-run `panel-review` when the answer is no. Signals
that it's no:

- The delta does more than the fixes you asked for — new behavior, a
  refactor carried along, unrelated cleanup.
- Any fix is a **structural rework** (the approach changed, logic moved
  across layers) rather than a local patch. A rework is a new design, and a
  new design deserves independent eyes.
- The delta reaches into risk-carrying surface: auth, payments, migrations,
  dependency or lockfile changes, CI config, anything touching secrets.
- It's simply large. Somewhere north of ~150 changed lines or ~5 files,
  your own read stops being a substitute for a panel — treat those as a
  soft threshold, not a rule, and weigh legibility over raw count (a
  400-line codemod can be more legible than a 40-line concurrency fix).
- `compare` came back `diverged` — you can't isolate what's new, so you
  can't claim to have read it.

When the delta is a handful of targeted, mechanical fixes to the exact
lines the panel flagged, and you've read them, don't burn a fan-out. Say in
the report that you skipped it and why.

**Running the re-review over the delta.** `panel-review --base <ref>` diffs
`<ref>...HEAD` using _local_ `HEAD`, so the PR's new head must be checked
out first:

```bash
gh pr checkout <N>            # requires a clean working tree
panel-review --base <OLD>     # invoke the skill; it drives panel-review.sh
```

If the working tree is dirty, **stop and say so** rather than stashing or
checking out on the user's behalf — that can silently lose work.

If `compare` said `diverged`, `--base <OLD>` will quietly walk back to the
merge base and review the _entire_ PR instead of the delta. That's not
wrong, just wider than advertised — so in that case review `--pr <N>`
deliberately and tell the user the delta couldn't be isolated.

Invoke the `panel-review` **skill**, don't hand-roll its fan-out. Fold its
findings into the ledger as `NEW` rows and adjudicate them like any other.
This is **one** extra round, not a loop: if the fresh panel surfaces
must-fix or should-fix findings, report them and don't approve. Point the
user at `panel-review-loop` if they want iteration to convergence.

### 5. Thread hygiene

Close the loop on the PR so the author can see what you accepted, without
adding noise. Four rules, applied per thread:

- **Only touch threads that are yours.** Resolve nothing you didn't open
  (`mine: false`). Another reviewer's thread is theirs to close, and a bot's
  thread isn't yours to speak for.
- **Already resolved → leave it entirely alone.** No reply, no re-resolve.
- **Satisfied (REMEDIATED / EXPLAINED / MOOT) → resolve it.** Reply first
  _only if the thread has no reply yet_ (`has_reply: false`) — a short
  confirmation, one line: `Confirmed fixed in abc1234.` If the author
  already replied, the resolve _is_ your acknowledgment; a "thanks,
  confirmed" on top of it is noise. Never stack a second reply under your
  own.
- **Outstanding → never resolve.** Reply once, saying specifically what's
  still missing (`The retry path at line 88 still bypasses the guard.`), so
  the author isn't guessing. Skip the reply only if your own comment is
  already the last word on the thread — repeating yourself doesn't help.

```bash
# reply (heredoc so markdown and newlines survive)
gh api --method POST \
  "repos/{owner}/{repo}/pulls/<N>/comments/<parent.database_id>/replies" \
  -f body="$(cat <<'EOF'
Confirmed fixed in abc1234.
EOF
)"

# resolve (needs thread_id, not a comment id)
gh api graphql -F threadId="<thread_id>" -f query='
  mutation($threadId: ID!) {
    resolveReviewThread(input: { threadId: $threadId }) {
      thread { isResolved }
    }
  }'
```

### 6. Approve — only when the gate passes

Evaluate the gate below. If it passes, invoke the `approve-pr` skill. Pass
a body only if the user supplied one — otherwise let `approve-pr` pick its
own short fun body, which is the point of that skill.

If the gate fails, **don't approve**, and don't leave a `request-changes`
review either: the outstanding threads are still open and carry the signal.
A blocking review from an automated second look is heavy-handed. Just
report.

### 7. Report

```
Recheck of PR #N — <title> — <url>
Reviewed at <OLD> -> now <NEW> (<status>, <n> commits, <k> files, +a/-b)

Remediated (n):
  - [SEV] file:line — the finding — fixed in <sha>: what the fix does
Explained (n):
  - [SEV] file:line — the finding — the author's argument, and what you
    checked that convinced you
Moot (n):
  - [SEV] file:line — the finding — why it no longer applies
Outstanding (n):
  - [SEV] file:line — the finding — exactly what is still missing
New (n):
  - [SEV] file:line — a problem in the delta (yours, or from the re-review)

Panel re-review: skipped (<why>) | ran (<risk>, <bucket counts>)
Threads: <n> resolved, <n> replied, <n> left open, <n> skipped (already resolved)
Decision: approved <url> with "<body>" | not approved — <the gate condition that failed>
```

Keep **Explained** honest — record what you _checked_, not just what the
author _said_. That line is how the user audits whether your second look
was real. Same for **Outstanding**: name the specific gap, because the
author is going to read it and needs to act on it.

## The approval gate

Approve only if **every one** of these holds:

1. **Nothing outstanding.** Every baseline finding is REMEDIATED,
   EXPLAINED, or MOOT. Zero `OUTSTANDING`.
2. **Nothing new blocking.** No `NEW` row rises above polish — neither from
   your own read of the delta nor from the fresh panel, and no substantiated
   questionable-approach flag. A wrong-layer fix doesn't get stamped just
   because every line-level finding is LOW.
3. **The delta was actually reviewed.** If step 4 said the delta needed a
   panel, the panel **ran and returned**. You cannot skip the re-review and
   then approve off your own read — that's the judgment call step 4 already
   made against you.
4. **Not a draft.** `gh pr view <ref> --json isDraft --jq '.isDraft'` is
   false. `gh pr review --approve` succeeds on drafts, but a draft is the
   author saying "not ready".
5. **Not your own PR.** Compare `gh api user --jq '.login'` against the PR
   author. GitHub rejects a self-approval; don't attempt it.
6. **Head unchanged since you started.** Re-fetch `headRefOid` immediately
   before approving and confirm it still equals the `NEW` you adjudicated.
   If the author pushed while you were reading, the current head is
   unreviewed — withhold and say a re-check is needed.

Conditions 4-6 have no thread to leave a comment on; they're explained in
the report, not on the PR. Conditions 1-3 always correspond to something
the author can see: an open thread, or a `NEW` finding you must surface —
post it as an inline comment at its `file:line` (or a top-level PR comment
if it has none) before reporting. A withheld approval whose reason isn't
visible on the PR is a silent block.

## Gotchas

- **No push, no argument, no work.** Check the head SHA first and bail in
  one line. A re-check asked for before the author pushed should cost two
  API calls, not a full re-read of the PR — and it should not produce a
  report, a resolve, or an approval.
- **"Fixed in abc1234" is a claim.** The single most common failure of a
  re-check is trusting the reply. Open the file at the new SHA. Half-fixes
  and fixes-at-the-wrong-site both look exactly like fixes from the thread.
- **Partial remediation is outstanding.** If the finding was "no validation
  on quantity" and the author validated `quantity` but not `refundQuantity`
  three lines down, the thread stays open. Being the second look means
  catching precisely this.
- **The delta is unreviewed code.** Fixes introduce bugs. Nobody — not the
  panel, not the author's own reviewers — has looked at the commits pushed
  since `OLD`. That's why step 4 exists, and why `NEW` findings block the
  gate.
- **Force-pushes hide the delta.** A `diverged` compare means you can't
  isolate what's new. Don't paper over it by diffing against the base
  branch and calling it a delta; re-review the whole PR and say so.
- **Resolve only your own threads.** Resolving another reviewer's thread
  speaks for them. Resolving a bot's thread hides signal from a human who
  hasn't looked yet.
- **Don't fix the code.** You're the reviewer. An outstanding finding gets
  a reply, not a commit. (If the user actually wants the fixes made, that's
  `pr-comment-handler` — a different skill, run from the author's side.)
- **One extra round, not a loop.** A fresh panel that surfaces must-fixes
  ends this skill: report and stop. Iterating to convergence is
  `panel-review-loop`.
- **Same PR throughout.** Resolve it once (Target) and reuse it. Don't
  re-detect per step; the branch can drift, and a stray `gh pr view` in a
  worktree resolves differently.
- **An approval is outward-facing.** It notifies the author and can unblock
  a merge. The user invoking this skill with approval intent is the
  authorization, but surface the PR first and re-check the head SHA last.

## Dry-run mode

If the user asks for a dry run, or sets `RECHECK_PR_DRY_RUN=1`, do all the
reading (it's read-only) but reply to nothing, resolve nothing, and approve
nothing. The fast path still short-circuits first — a no-op re-check writes
no files. Otherwise write:

- `./recheck.json` — the ledger plus the decision:

  ```json
  {
    "pr": 61,
    "reviewed_sha": "aaaa1111",
    "head_sha": "bbbb2222",
    "compare_status": "ahead",
    "findings": [
      {
        "severity": "HIGH",
        "location": "src/charges/idempotency.ts:42",
        "status": "REMEDIATED",
        "evidence": "fixed in cc33dd44; key now hashed from request body"
      },
      {
        "severity": "MEDIUM",
        "location": "src/api/orders.ts:60",
        "status": "OUTSTANDING",
        "evidence": "quantity validated, refundQuantity at :63 still unchecked"
      }
    ],
    "panel_rerun": { "needed": false, "reason": "delta is 2 targeted fixes, 18 lines" },
    "threads": [
      { "thread_id": "PRRT_a", "action": "resolve", "reply": "Confirmed fixed in cc33dd44." },
      {
        "thread_id": "PRRT_b",
        "action": "reply-only",
        "reply": "refundQuantity at line 63 is still unvalidated."
      }
    ],
    "approve": false,
    "reason": "1 outstanding finding (orders.ts:60)",
    "body": null
  }
  ```

- `./report.md` — the step 7 report, stating clearly that nothing was
  replied to, resolved, or approved.

Honor user-supplied paths if provided.
