# approve-pr

Approve the GitHub PR you're discussing — the quick, fun last step after a review. Give no message and it stamps the PR with a short fun body (an LGTM or a single silly emoji); give a message and it uses that verbatim.

## Install

```
npx skills add venables/skills --skill approve-pr
```

## How to use it

Just ask Claude Code in plain English from the PR's branch:

- "approve the PR"
- "ship it"
- "LGTM it"
- "approve PR 27"
- "approve with 'great work, merging Monday'"

If you don't name a PR, the skill auto-detects from your current branch via `gh pr view`.

## What it does

- Resolves the target PR (current branch, or the one you named) and surfaces `PR #N: <title>` before doing anything.
- With **no message**, approves with one short fun body — `🤠`, `Ship it! 🚢`, `🚀`, `LGTM`, `✅`, etc. — and nothing else, varying it across uses.
- With **a message**, approves with that message verbatim.
- Runs `gh pr review --approve` and reports the PR link plus the body it used.

## Gotchas

- **You can't approve your own PR.** GitHub rejects a self-approval. If you're the author, the skill says so and offers a plain comment review instead.
- **Approve only.** This skill never requests changes — it's purely the approving stamp. For change requests, use `gh pr review --comment`/`--request-changes` directly.
- **It doesn't re-review.** Approval assumes the review already happened (usually a prior `panel-review`). The skill stamps; it doesn't read the diff. Run `panel-review` first if you want confidence before approving.
- **The fun body stands alone.** When defaulting, the emoji or short phrase is the entire body — no rationale, summary, or attribution appended.
