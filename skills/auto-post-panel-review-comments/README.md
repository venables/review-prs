# auto-post-panel-review-comments

The zero-touch twin of [`post-panel-review-comments`](../post-panel-review-comments). Takes a list of review findings (typically from a `panel-review`) and posts the legitimate ones straight to the PR — no select-list, no per-finding prompts, no confirmation. It applies a fixed bar automatically, posts, then reports.

## Install

```
npx skills add venables/skills --skill auto-post-panel-review-comments
```

## How to use it

After running `panel-review` (or with any findings list that has `file:line` refs), ask for it to be posted automatically:

- "auto-post these to the PR"
- "just post the legitimate findings"
- "post them all, no triage"
- "post the panel review comments automatically"
- "fire these onto the PR without asking"
- "auto-post the findings, but send the LOW/polish ones to Linear"

It posts immediately and reports what it did. There is no interactive step. If you want to _pick_ what goes where, use the interactive [`post-panel-review-comments`](../post-panel-review-comments) instead.

## What it does

- **Posts confident, in-scope, located findings to the PR** as standalone inline comments via `POST /pulls/{N}/comments` — no review wrapper, no "X left a review" timeline entry. The comment body is the finding verbatim plus the fix when there is one: a mergeable GitHub ` ```suggestion ` block (one-click **"Commit suggestion"**) when the fix is a clean drop-in replacement for the commented line(s), otherwise a `**Possible Solution:**` prose line. A finding with no suggested fix posts the body alone (no fix line — never an invented one). A concrete `file:line` is required; a fix is not. **No severity, no priority, no panelist attribution.** Sequenced HIGH → MEDIUM → LOW.
- **+1s instead of duplicating.** If another automated reviewer (CodeRabbit, Copilot, Cursor, Greptile, etc.) already raised a finding, it adds a +1 reaction to that comment instead of posting a duplicate (and replies with a short delta only when it has materially new info). Human comments and its own prior comments are never deduped against.
- **Routes uncertain and out-of-scope findings to Linear** (when reachable) rather than the PR — speculative findings that need investigation, and real issues caught by the review that aren't about this PR's changes. Most findings still go to the PR directly.
- **Honors routing overrides.** Say "post the LOW/polish ones to Linear", "only post HIGH/MEDIUM", "everything to the PR", or "don't touch Linear" and it routes accordingly.
- **Auto-falls back to a top-level PR comment** when GitHub rejects an inline comment (line outside the diff hunk), downgrading any suggestion to prose. Nothing gets dropped silently.
- **Reports everything** at the end: posted comment URLs (suggestion vs prose), top-level fallbacks, +1'd duplicates, Linear tickets, anything that needs your attention, and what was dropped.

## What it does NOT do

- **No prompts, ever.** It's zero-touch by design. When something would normally need a question — a missing PR ref, or a Linear target it can't resolve — it degrades to the report (aborts posting for a missing PR; defers Linear findings to a "needs your attention" list) rather than blocking on input.
- **No wording rewrites.** Findings post as written. Soften specific wording before triggering the skill if needed.
- **No review wrapper.** Comments post individually — one notification each.

## Gotchas

- **It won't blast the wrong PR.** It takes the PR ref from context. If none is present it will auto-detect, but only a branch's single unambiguous open PR — anything else aborts with a report rather than guessing.
- **One notification per comment.** Standalone comments don't batch, and auto mode can't warn-then-confirm. For large lists the report makes the count prominent; route more to Linear next time with an override.
- **Mergeable suggestions are inline-only and anchor-exact.** A ` ```suggestion ` block replaces the comment's anchored line range verbatim, so it only goes on an inline comment whose range matches the rewritten lines, with indentation matching the file. Top-level fallbacks and Linear tickets always use prose. When a fix isn't a faithful drop-in, it uses prose — a wrong one-click suggestion is worse than a hint.
- **Severity isn't a gate by default.** A confident LOW with a clean fix still gets a one-line suggestion. Gate by severity only via an override.
- **Different from the interactive skill.** [`post-panel-review-comments`](../post-panel-review-comments) lets you triage via select lists; this one decides automatically. Different from [`pr-comment-handler`](../pr-comment-handler), which acts on comments _already_ on the PR.
