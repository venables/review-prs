---
name: approve-pr
description: >
  Approve the GitHub pull request currently under discussion — typically
  right after a `panel-review`, `panel-review-loop`, or once review
  comments have been handled. Use whenever the user says things like
  "approve the PR", "approve this", "approve PR 27", "LGTM it", "ship
  it", "stamp it", "give it a thumbs up", or "approve with <message>".
  With no message given, approve with a short, fun body — an LGTM or a
  single silly emoji as the only text; with a message supplied, use it
  verbatim. Auto-detects the target PR from the current branch when the
  user does not name one. After approving, if Slack MCP is available,
  reacts ✅ to this PR's review-request message in #dev and removes the
  user's own 👀 reaction. Do NOT use to *request changes* or leave a
  non-approving review (use a plain `gh pr review` / the PR comment
  skills), to *generate* a review (`panel-review`), or to act on existing
  review comments (`pr-comment-handler`).
---

# approve-pr

Submits an **approving** review on a GitHub PR — the celebratory last
step after the work has been reviewed and is good to go. The whole point
is that it's quick and a little fun: no message needed, just a stamp.

- **No message given** → approve with a short fun body (an emoji or a
  tiny phrase, nothing else).
- **Message given** → approve with that message, verbatim.

## Language

The default bodies below are a fixed list, and a user-supplied message
goes on verbatim — neither is yours to rewrite. Any **other** prose you
put on GitHub (a plain comment review when self-approval is blocked, for
example) is written in ASD-STE100 Simplified Technical English:

- Write short sentences. Keep an instruction to 20 words. Keep a
  statement to 25 words.
- Put one idea in each sentence.
- Use the active voice. Use simple tenses.
- Use one word for one meaning.
- Use the simplest word that is correct. Remove jargon, idioms,
  metaphors, and figures of speech.
- Do not make a noun out of a verb.

## Inputs

1. **PR ref** — usually omitted. Auto-detect from the current branch:
   `gh pr view --json number,url,title,author,state`. If the user names
   a PR explicitly ("approve PR 27", a `github.com/.../pull/N` URL), use
   that.

   If auto-detect finds no PR for the branch, or the repo has several
   and it's ambiguous, ask the user once which PR — don't guess.

2. **Body** — optional. Whatever message the user attached to the
   request ("approve with 'great work, merging Monday'"). If they gave
   none, you pick a fun default (see below).

## Workflow

### 1. Resolve and surface the PR

Run `gh pr view <ref> --json number,url,title,author,state` and show it
back before approving — a cheap sanity check that you're stamping the
right thing:

```
PR #N: <title> — <url>
```

If the PR is already `MERGED` or `CLOSED`, stop and say so rather than
attempting an approval that can't apply.

### 2. Choose the body

If the user gave a message, use it exactly as written — don't embellish
it with extra emoji or prose.

If they gave none, pick **one** short, fun body. Vary it across
invocations rather than always using the same one. Keep it to just the
emoji or tiny phrase — no follow-up sentence. Good options:

- `🤠`
- `Ship it! 🚢`
- `🚀`
- `LGTM 🤠`
- `✅`
- `🎉`
- `🔥`
- `LGTM`
- `🙌`

Match the mood when there's an obvious cue (e.g. `Ship it! 🚢` right
after a clean `panel-review-loop`), otherwise just pick one.

### 3. Approve

```
gh pr review <ref> --approve --body "<body>"
```

Omit `<ref>` to let `gh` resolve the current branch's PR, or pass the
number / URL when the user named one.

### 4. React to the Slack review request (if Slack MCP is present)

Reviews are often requested by dropping the PR link in the **#dev**
channel. After approving, close that loop by adding a ✅ reaction to the
request message. This step is best-effort — it never blocks or undoes
the approval.

Skip the whole step silently when the Slack MCP tools (e.g.
`slack_search_public_and_private`, `slack_add_reaction`) are not
available in the session.

1. **Find the message.** Search for the PR URL scoped to #dev only —
   e.g. query `"<pr-url>" in:#dev` — or read recent #dev history and
   look for the link. Never look in any other channel, and never match
   on just the PR number or title; require the actual PR link.

2. **Confirm it's a review request.** The message must be _asking for a
   review_ — typically a short description of the PR plus the link
   ("Adds retry logic to the webhook worker: <url>", "can someone take a
   look at <url>"). Do **not** react to messages that merely mention the
   PR: questions about it ("is <url> why staging is broken?"), status
   updates, bot notifications, or general discussion. When in doubt,
   don't react.

3. **Handle multi-PR messages.** If the request message contains links
   to several PRs, only react when **all** of them are approved. Check
   each other PR with `gh pr view <url> --json reviewDecision,state` —
   count a PR as done when `reviewDecision` is `APPROVED` or it's
   already `MERGED`. If any PR in the message is still unapproved, add
   no reaction (a partial ✅ would falsely signal the whole batch is
   reviewed).

4. **React.** Add a `white_check_mark` reaction to the message via
   `slack_add_reaction`. If Slack reports you already reacted, that's
   fine — nothing to do.

5. **Clear your 👀.** The current user often marks "I'm looking at
   this" with an `eyes` reaction. Once the ✅ has been added, check the
   message's reactions (`slack_get_reactions`) and, if the current user
   has an `eyes` reaction on it, remove it — ✅ replaces 👀. This only
   applies when the ✅ actually went on: if a multi-PR message was left
   un-reacted (step 3), leave the 👀 alone too — the review of the batch
   isn't done. Use the reaction-removal tool if the Slack MCP provides
   one; if
   it doesn't, leave the 👀 in place and mention that in the report.
   Only ever remove the current user's own `eyes` reaction — never
   anyone else's, and never any other emoji.

If no matching message exists in #dev, just move on; most PRs won't
have one.

### 5. Report back

One line, with the PR link and the body you used:

```
Approved PR #N <url> with "<body>".
```

If you also reacted in Slack, append a short note (e.g. "Added ✅ to the
review request in #dev."). If a multi-PR message was found but left
un-reacted, say which PRs are still awaiting approval.

## Gotchas

- **You can't approve your own PR.** GitHub rejects an approving review
  from the PR author (`gh` returns `Can not approve your own pull
request`). If `author` from step 1 is the current user, don't even try
  — tell the user they can't self-approve and offer to leave a plain
  comment review instead (`gh pr review <ref> --comment --body ...`).

- **Approve, never request-changes here.** This skill only ever passes
  `--approve`. If the user actually wants to block or request changes,
  that's a different action — don't shoehorn it through here.

- **The fun body is the _only_ text.** When defaulting, don't append a
  rationale, a summary of the review, or attribution. The emoji or short
  phrase stands alone — that's the bit the user asked for.

- **Don't re-run the review.** Approval assumes the review already
  happened (usually a prior `panel-review`). This skill doesn't read the
  diff or re-evaluate the code — it just stamps. If the user hasn't
  actually reviewed and wants confidence first, point them at
  `panel-review` before approving.

- **An approval is outward-facing.** It notifies the PR author and other
  watchers and can unblock a merge. Since the user invoked this skill to
  approve, that's authorization — but still surface the PR (step 1)
  first so a wrong auto-detect is caught before the notification goes
  out.

- **Slack step is #dev-only and best-effort.** Never search or react in
  any channel other than #dev, and never let a Slack failure (missing
  MCP, search error, channel not found) change the outcome of the
  approval — the approval already happened; just skip the reaction and
  report normally.

- **The ✅ means "this batch is fully reviewed".** On a multi-PR request
  message the reaction is all-or-nothing: every linked PR must be
  approved (or merged) before reacting. Don't react "for your PR's
  share" of the message.

- **Review requests only.** A Slack message containing the PR link is
  not automatically a review request. If it reads as a question, an
  incident thread, a deploy notice, or bot output, leave it alone — a ✅
  on those messages would answer a question nobody asked.
