# auto-review

One-shot "review, post, and maybe approve" pipeline for a PR. It chains three existing skills:

```
panel-review  →  auto-post-panel-review-comments  →  approve-pr (only if clean)
```

It doesn't reimplement any of them — it calls each in turn and adds one new piece of logic: the **approval gate** that decides whether the PR is clean enough to stamp.

## Install

```
npx skills add venables/skills --skill auto-review
```

Requires [`panel-review`](https://github.com/catena-labs/dev-skills/tree/main/skills/panel-review) and [`auto-post-panel-review-comments`](../auto-post-panel-review-comments) installed; [`approve-pr`](../approve-pr) is needed only for the approval step (post-only runs don't use it).

## How to use it

On a PR (or its branch), ask:

- "auto-review this PR"
- "auto review"
- "auto panel review" / "auto panel-review" (any "auto" + review phrasing)
- "review and post the comments"
- "panel-review then auto-post"
- "review it and approve if it's clean"
- "auto-review, but send the LOW/polish ones to Linear"
- "review and post only, don't approve" (runs the review + post, skips approval)

Anything that prefixes **"auto"** onto a review request lands here, not on
`panel-review`, and carries approval intent. The skill never stops to ask
whether to approve: the gate decides, and an ambiguous invocation silently
falls back to post-only.

## What it does

1. **Reviews** — runs the `panel-review` skill end to end (kept separate; its fan-out, streaming, and synthesis are untouched). Captures which panelists returned a verdict and the synthesized findings.
2. **Posts** — runs the `auto-post-panel-review-comments` flow: posts the legitimate findings to the PR (mergeable suggestion / prose / body-only), `+1`s anything a bot already raised, routes uncertain / out-of-PR-scope findings to Linear, honors routing overrides. Posting happens whether or not the PR will be approved.
3. **Approves (only when requested and clean)** — the approval step runs only when the invocation asked for it ("auto-review", "approve if clean", "stamp it if clean"); a post-only or ambiguous request ("review and post the comments") runs steps 1–2 and skips approval, reporting the decision it would have made. When approval is requested and the gate passes, it approves via `approve-pr` with a short LGTM body.

## The approval gate

Approval also requires that the invocation asked for it (see step 3 above); a post-only/ambiguous request skips this gate entirely. When approval was requested, it approves **only when every one** of these holds:

- **Reviewer coverage — ≥ 75% of the panel returned** — at least 75% of the launched panelists returned a verdict (`exit 0`), and the ≥2 floor below holds. One panelist crashing/timing out (e.g. the flaky `database is locked`) in a four-panel run (3/4) still clears this as long as the returning reviewers are clean; dropping below 75% (2/4, 2/3, 1/2) does not → no approval.
- **Enough independent reviewers — at least two, not narrowed** — a hard floor of ≥2 distinct panelists returning `exit 0` (one opinion is never enough — even if it's the only CLI installed on the host), and the panel wasn't narrowed below `panel-review`'s default via `--panelist`. A single-reviewer run is too thin to auto-stamp.
- **No blocking findings** — zero must-fix (CRITICAL/HIGH) and zero should-fix (MEDIUM); only polish (LOW) findings, or none.
- **Sound approach, served purpose** — no substantiated `Approach (questionable)` flag, and no verified `Purpose (stated, not served)` or `Purpose (unknown)` flag. Code that works but has no stated reason to exist, or does something other than what its description says, is not auto-stamped. A verified `Proof (missing)` is bucketed MEDIUM/HIGH by `panel-review`, so the blocking-findings gate already withholds on it.
- **Not a draft** — a draft PR is the author saying "not ready" (`gh pr view --json isDraft`); comments still post, approval is withheld.
- **Not your own PR** — GitHub blocks self-approval; if you authored the PR it reports that instead.
- **Head unchanged** — the PR head SHA is re-fetched before approving and must still match the SHA that was reviewed; if the author pushed during the multi-minute review, approval is withheld (the current head was never reviewed).

When the gate fails on real findings, the inline comments are already posted — it does **not** leave a blocking `request-changes` review by default; it just reports the recommendation (use [`panel-review-loop`](../panel-review-loop) if you want to iterate to clean).

## The approval body

Short, ASCII, one line, no review dump:

- No findings at all → `LGTM`
- Only polish comments posted → `LGTM, just some small comments, nothing blocking`

Handed to `approve-pr` verbatim (so it adds no emoji of its own).

## What it does NOT do

- **No reimplementation.** It composes `panel-review`, `auto-post-panel-review-comments`, and `approve-pr`; all their mechanics live in those skills.
- **No blocking reviews.** It only ever approves (when clean); it never submits `request-changes`.
- **No approval without coverage.** Coverage falling below 75% of the launched panel (or below the ≥2 floor), or a narrowed panel, blocks the stamp even if the findings are all LOW. Draft PRs, your own PRs, a head that moved mid-review, and post-only requests are also withheld (non-finding reasons).

## Gotchas

- **It targets a PR.** If the review runs against a non-PR target (`--uncommitted`, `--base`), there's nothing to post to or approve — it reports the review and stops.
- **Coverage below 75% blocks approval.** Losing one of four panelists (3/4) still allows a clean approval; falling below 75% of the launched panel is not a clean bill of health — comments still post, approval is withheld, and the report names the missing panelist.
- **Same PR throughout.** The PR is resolved once and reused for review, posting, and approval.
- **Different from its parts.** [`panel-review`](https://github.com/catena-labs/dev-skills/tree/main/skills/panel-review) only reviews; [`auto-post-panel-review-comments`](../auto-post-panel-review-comments) only posts; [`panel-review-loop`](../panel-review-loop) iterates fix-and-rereview; [`approve-pr`](../approve-pr) only approves. `auto-review` is the one-pass review → post → maybe-approve composition.
