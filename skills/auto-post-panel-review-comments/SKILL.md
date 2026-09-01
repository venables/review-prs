---
name: auto-post-panel-review-comments
description: >
  Take a list of code-review findings (typically from a prior
  `panel-review`) and post the legitimate ones straight to the PR as
  standalone inline comments — no select-list, no per-finding prompts.
  Each clean finding becomes an inline comment (a mergeable
  ` ```suggestion ` block when the fix is a drop-in, prose otherwise);
  anything an automated reviewer already raised is +1'd instead of
  duplicated; uncertain or out-of-PR-scope findings go to Linear (when
  reachable) instead. Use when the user wants the findings posted
  automatically — "auto-post these", "just post the legitimate findings",
  "post them all to the PR, no triage", "auto mode", "post the panel
  review comments automatically". Honors routing overrides like "post the
  LOW/polish ones to Linear". Do NOT use when the user wants to *pick*
  which findings go where (the interactive `post-panel-review-comments`),
  to *generate* the findings (run `panel-review` first), or to act on
  comments already on the PR (`pr-comment-handler`).
---

# auto-post-panel-review-comments

The zero-touch twin of `post-panel-review-comments`. Same findings, same
comment shape, same dedupe, same GitHub mechanics — but **no interactive
triage**. Instead of two `AskUserQuestion` select stages, this skill
applies a fixed bar automatically and posts. It does not stop to confirm;
it acts, then reports.

**This skill shares its comment-construction rules with
`post-panel-review-comments`.** The body shape, the mergeable-suggestion
rules, the dedupe fetch/match, the inline-posting payload, and the
top-level fallback are all defined there and apply here **verbatim**. This
file specifies only what is _different_ in auto mode (the bar, the
routing, zero-touch behavior) and restates the shared mechanics tightly
enough to execute standalone. When in doubt about a shared detail (exact
payload fields, suggestion anchoring, fallback body), follow
`post-panel-review-comments`.

## Language

Write every word **you** author in ASD-STE100 Simplified Technical
English — the `**Location:**` line, any `Some more info:` reply, the
Linear title and description, and the report.

- Write short sentences. Keep an instruction to 20 words. Keep a
  statement to 25 words.
- Put one idea in each sentence.
- Use the active voice. Write "the handler drops the second charge", not
  "the second charge is dropped by the handler".
- Use simple tenses. Write "the test fails", not "the test has been
  failing".
- Use one word for one meaning. Do not change "function" to "method" to
  "routine" in the same text.
- Use the simplest word that is correct. Remove jargon, idioms,
  metaphors, and figures of speech.
- Do not make a noun out of a verb. Write "when the job starts", not "at
  job initiation".
- Keep a paragraph to six sentences or fewer.

The finding body still posts **verbatim** — this rule governs the text
around it, not the finding itself. `panel-review` holds its panelists to
the same style, so a finding from the panel already reads this way.

## What "auto" changes

|                    | `post-panel-review-comments`               | this skill                               |
| ------------------ | ------------------------------------------ | ---------------------------------------- |
| Which findings act | user picks via select lists                | fixed bar (below), applied automatically |
| Confirmation       | confirms drop count before acting          | none — posts immediately, reports after  |
| PR vs Linear       | Stage 1 (PR) then Stage 2 (Linear)         | auto-routed per finding (below)          |
| Dedupe             | two passes (detect at triage, act at post) | one pass at post time                    |

Everything else — the comment body, suggestions, fallback, Linear ticket
shape — is identical.

## Inputs

1. **Findings** — usually from a prior `panel-review`. Each needs a file
   path, line (or range), and body; a fix and severity when present.
2. **PR ref** — number or URL. Take it from the caller's context (the
   review's scope), exactly as `post-panel-review-comments` does.

   Auto mode can't prompt, so PR resolution has one extra guarded
   fallback: if no PR ref is in context, **list** the current branch's
   open PRs with `gh pr list --head <current-branch> --state open --json number,url,title,headRefOid,headRepositoryOwner,headRepository,isCrossRepository`
   and proceed **only if exactly one** is returned **and that PR's head is
   this repo, not a fork** (`isCrossRepository == false`, or
   `headRepositoryOwner`/`headRepository` matching your origin/push
   remote). The repo-match check matters because `--head` filters by
   branch _name_ only — a same-named branch on someone's fork can be the
   sole match, and posting to it would leak comments onto a stranger's PR.
   Don't use `gh pr view` for this — it resolves a single PR (and will
   happily return a merged/closed one), so it can't prove the "exactly one
   open PR" guard. If the count is zero, more than one, or the sole match
   is a fork PR, **do not post** — abort and report that a PR ref is
   required (list the candidates when there are several). Never blast
   comments at a guessed PR. Always state in the report which PR was
   resolved and how (from context vs. branch auto-detect).

If a finding lacks a file/line, it can't be an inline comment — route it
per "Routing" below (usually Linear or dropped), never guess a location.

## The bar (which findings act, and where)

Apply this to every finding, in order. No prompts.

1. **Drop pure noise.** A finding with no concrete file:line _and_ no
   actionable substance is dropped (counted in the report). Exception: a
   `panel-review` finding located at `PR description` (a promoted
   `Purpose (unknown)` flag) is actionable and in-scope; it has no line to
   anchor to, so post it directly as a top-level PR comment (same body
   shape as the top-level fallback, with `**Location:** PR description`).
2. **Route uncertain / out-of-scope findings to Linear** (see "Routing").
   A finding is uncertain when you cannot confidently confirm it's real
   from the diff (speculative, needs investigation, depends on unverified
   external context). A finding is out-of-scope when it's a real issue the
   review surfaced but it's **not about this PR's changes** (pre-existing
   bug in untouched code, unrelated tech debt).
3. **Everything else → post to the PR.** A finding that is confidently
   in-scope and has a concrete file:line gets posted as a standalone
   inline comment — **unless** an automated reviewer already raised it, in
   which case +1 the existing comment instead (see "Dedupe"). A suggested
   fix is **not** required: when the finding has one, render it (suggestion
   or prose); when it doesn't, post the finding body alone and omit the
   fix line entirely — never invent one.

**"Actionable + deduped" is the default bar** — post confident, in-scope,
located findings; +1 the ones a bot already covered; send the uncertain
and out-of-scope ones to Linear. "Located" means a concrete file:line;
a fix is a bonus, not a gate. Severity is **not** a gate either by default
(a confident LOW with a clean fix is still worth a one-line suggestion).

### Honoring explicit overrides

If the user's request carries routing instructions, they win over the
default bar:

- "post the LOW / polish ones to Linear" → route LOW-severity findings to
  Linear instead of the PR.
- "only post HIGH/MEDIUM" / "drop anything below HIGH" → apply that
  severity gate; drop or Linear-route the rest per their phrasing.
- "everything to the PR" → skip the uncertain/out-of-scope Linear routing
  and post all located findings.
- "don't touch Linear" → PR-only; drop what doesn't qualify for the PR.

Surface which override you applied in the report.

## Routing (PR vs Linear vs drop)

- **PR** — confident, in-scope, has a concrete file:line (a fix is
  optional). Posted inline (suggestion or prose if a fix exists, finding
  body alone otherwise), with the top-level fallback when GitHub rejects
  the inline. This is where _most_ findings should land. A finding located
  at `PR description` skips the inline attempt and posts top-level.
- **Linear** — uncertain/needs-exploration findings, and real-but-out-of-
  PR-scope findings, plus anything an override sends there. Only when
  Linear is reachable from this session (see below).
- **Drop** — pure noise, or findings with no location an override didn't
  rescue. Always counted in the report, never silent.

### Linear availability and target (zero-touch constraint)

File to Linear **only when both** hold:

1. Linear is reachable from this session (an MCP server, a `linear` CLI, a
   configured token — whatever your runtime exposes), **and**
2. A target team/project is resolvable **without a prompt** — named in the
   user's request, present in the caller's context, or set via env/config.

If Linear is unreachable, or no team/project can be resolved without
asking, **do not prompt and do not silently drop** the Linear-bound
findings: list them in the report under a "needs your attention" section
so the user can file them manually. Zero-touch means never blocking on
input — degrade to reporting, not to a question.

Ticket shape is identical to `post-panel-review-comments`: title from the
finding's first sentence (≤ ~80 chars); description with the PR link +
file:line deep-link, `File:`, `Severity:`, blank line, the finding body,
then — **only when the finding has a suggested fix** — the fix as a
`**Possible Solution:**` prose line (**never** a ` ```suggestion ` block —
those are GitHub-inline-only; and never invent a fix when none was given).
**Do not set priority.**

## Posting (shared with `post-panel-review-comments`)

For each finding routed to the PR, build and post an **independent inline
comment** via `POST /repos/{owner}/{repo}/pulls/{pull_number}/comments`
with `body`, `commit_id` (head SHA from `headRefOid`), `path`, and the
line-targeting fields. No review wrapper, no batched review. Resolve the
PR once up front with `gh pr view <ref> --json number,url,title,headRefOid`.
`headRefOid` is the `commit_id` for posting; the `{owner}/{repo}` for the
comments endpoint comes from the PR `url`
(`https://github.com/{owner}/{repo}/pull/N` — the base repo, which is
where comments are posted even for fork PRs).

**Caller-pinned commit_id.** If an orchestrator (e.g. `auto-review`) hands
you a specific head SHA to post against, treat it as the **effective
reviewed SHA** (`effective_sha = pinned commit_id ?? headRefOid`) and use
it **everywhere a SHA appears** — the inline `commit_id`, the
`blob/<SHA>/...` deep-link in top-level fallbacks, the Linear ticket
links, and the report — not just the inline `commit_id`. It's pinning the
exact revision that was reviewed so nothing drifts onto a head that was
pushed mid-review (GitHub marks moved inline comments outdated; the blob
and Linear links stay anchored to the reviewed code). Only re-resolve
`headRefOid` when no commit_id was pinned.

**Comment body shape** (verbatim from `post-panel-review-comments`):

- The finding body **verbatim**. No severity, no priority, no
  panelist/agent attribution. In particular, strip any `[LOW]` / `[MEDIUM]`
  / `[HIGH]` grade `panel-review` prefixed onto the finding. It never
  appears on the PR (a LOW becomes the `Small / Optional polish:` prefix
  below; everything else posts with no grade at all).
- When the fix is a **clean drop-in replacement for the commented
  line(s)**, render it as a mergeable suggestion:

  ````markdown
  <finding body verbatim>

  ```suggestion
  <exact replacement for the commented line(s)>
  ```
  ````

  The suggestion is inline-only, must anchor to exactly the lines it
  rewrites (and still include the finding's reported location — don't move
  the anchor off it), and the replacement must be exact with file-matching
  indentation. When the fix isn't a faithful drop-in, use a
  `**Possible Solution:** <fix>` prose line instead. A finding gets a
  suggestion **or** a prose fix line, never both, and never an invented
  fix.

- For **LOW / polish** findings, prefix the body with
  `Small / Optional polish:` (or equivalent soft framing).

See `post-panel-review-comments` → "Comment body shape" and "Mergeable
suggestions" for the full rules.

**Line targeting:** single-line → `line`, `side: "RIGHT"`; multi-line →
`start_line`, `start_side: "RIGHT"`, `line` (the **end** of the range),
`side: "RIGHT"`.

**Order:** sequence the calls HIGH → MEDIUM → LOW (within severity, group
by file) so notifications arrive in priority order. Findings with no
severity sort last, preserving their input order. One HTTP call per
comment, one notification per comment.

**Deep-links:** build with `scripts/pr-line-url.sh <pr-url> <path>
<line-or-range>` for lines in the diff, or the blob fallback
`https://github.com/<OWNER>/<REPO>/blob/<HEAD_SHA>/<PATH>#L<LINE>` for
lines outside the hunk. Used in the report and in any top-level fallback /
Linear body.

### Top-level fallback (shared)

GitHub only accepts inline comments on lines inside the PR diff hunks.
When `POST /pulls/{N}/comments` fails (HTTP 422, `line must be part of the
diff`), **automatically re-post that one finding as a top-level PR issue
comment** via `POST /repos/{owner}/{repo}/issues/{pull_number}/comments`
(or `gh pr comment`). Prepend a `**Location:**` line with the file:line
and blob deep-link, then the same body shape — except a suggestion
**downgrades to a `**Possible Solution:**` prose line** (suggestions don't
work in top-level comments). Per-finding and automatic; never drop a
rejected comment. See `post-panel-review-comments` → "Falling back to a
top-level comment".

**These per-finding fallbacks are the _only_ top-level comments this skill
posts. Never post an overall review-summary / overview comment on the PR**:
no "N findings across the panel", no risk recap, no verdict. Each finding
is its own comment; the summary belongs to the caller's chat report, not
the PR. A summary comment on the PR is a bug, not a courtesy.

## Dedupe — one pass, at post time

Auto mode has no triage pass, so dedupe collapses to a single pass run
just before posting. The fetch and match rules are identical to
`post-panel-review-comments`:

- Fetch both surfaces:
  `gh api repos/{owner}/{repo}/pulls/{N}/comments --paginate` and
  `gh api repos/{owner}/{repo}/issues/{N}/comments --paginate`.
- Treat a comment as a candidate duplicate **only when it's from another
  automated reviewer** (`user.type == "Bot"`, or a known review-bot login:
  `coderabbitai`, `copilot-pull-request-reviewer`, `cursor`, `greptile`,
  `sourcery-ai`, a `github-actions` bot, etc.). Never dedupe against human
  comments or your own prior comments — and exclude your **own** posting
  identity first: if this skill runs in CI under a `github-actions` token,
  its earlier comments are _yours_, not "another bot's". Get your own login
  once per run with `gh api user --jq '.login'` and skip comments authored
  by it before treating any `github-actions` comment as a dedupe candidate.
- A finding **matches** when it's the same issue — same file,
  overlapping/adjacent line, same underlying problem (judge semantically;
  different bot wording still counts). When unsure two are the same issue,
  **post your own** rather than collapsing to a reaction.

For each PR-bound finding:

1. **Matched → react, don't duplicate.** Add a +1 reaction to the existing comment
   and skip posting:
   - inline: `gh api -X POST repos/{owner}/{repo}/pulls/comments/{id}/reactions -f content=+1`
   - top-level: `gh api -X POST repos/{owner}/{repo}/issues/comments/{id}/reactions -f content=+1`

   Optionally reply with a short `Some more info:` delta **only** when you
   have materially new detail (a repro, another affected location, a
   better fix). Default is +1 alone.

2. **No match → post normally** per "Posting".

## Reporting back

Zero-touch means the report is the only feedback the user gets — make it
complete. After acting, report:

- **PR resolution** — which PR (`#N — title — url`) and how it was resolved
  (from context vs. branch auto-detect).
- **Posted** — the inline comment URLs, grouped; note which carried a
  mergeable suggestion vs. a prose fix.
- **Top-level fallbacks** — which findings fell back (GitHub rejected the
  inline) and that any suggestion was downgraded to prose. Call out
  distinctly.
- **+1'd (deduped)** — for each, the existing comment's `html_url`, the bot
  that authored it, and whether you reacted only or also replied.
- **Filed to Linear** — ticket URLs, grouped.
- **Needs your attention** — findings bound for Linear that couldn't be
  filed (Linear unreachable / no resolvable team/project), so the user can
  handle them. Include file:line + deep-link.
- **Dropped** — count and one-line reason each.
- **Any override** applied to the default bar.
- One-line summary:
  `Auto-posted N to PR #X (J top-level fallbacks), +1'd D existing automated comments, filed M to Linear, D2 need attention, dropped K`.
- Any comment that failed **both** inline and the top-level fallback (rare)
  for manual handling.

## Gotchas

- **No prompts, ever.** This is the whole point. If something would
  normally require asking (missing PR ref, unresolved Linear target),
  degrade to the report — abort posting for a missing PR, defer Linear
  findings to "needs your attention" — rather than blocking on input.
- **Don't blast the wrong PR.** The guarded auto-detect posts only to a
  branch's single unambiguous open PR; anything else aborts with a report.
  A wrong-PR auto-post is the worst failure mode here.
- **One notification per posted comment.** Standalone comments don't
  batch, and auto mode can't warn-then-confirm for large lists. If the
  count is large (10+), still post, but make the notification count
  prominent in the report; the user can route more to Linear next time via
  an override.
- **Only dedupe against other automated reviewers.** Same rule as the
  interactive skill — never +1-and-skip because a _human_ mentioned it, and
  never dedupe against your own earlier comments.
- **Suggestions are inline-only and anchor-exact.** A clean drop-in →
  ` ```suggestion `; otherwise prose. Top-level fallbacks and Linear
  tickets always use prose. A wrong one-click suggestion is worse than a
  hint — when unsure it's a faithful drop-in, use prose.
- **Severity isn't a gate by default.** Post confident located findings of
  any severity; gate by severity only when the user says so.

## Dry-run mode

If the user asks for a dry run ("don't actually post", "show me what
you'd do") or sets `AUTO_POST_PANEL_REVIEW_COMMENTS_DRY_RUN=1`, do
everything normally but write instead of calling the API:

- `./comments.json` — array of per-comment payloads (each as it would be
  POSTed to `/pulls/{N}/comments`). Dry-run can't observe which lines
  GitHub rejects, so emit the inline payload for every PR-bound,
  non-deduped finding; for any finding you can already tell sits outside
  the diff hunk, note in the report that it would fall back to top-level.
- `./reactions.json` — array of planned +1 actions for deduped findings
  (`{comment_id, surface, existing_url, author, reply?}`).
- `./linear_tickets.json` — array of `{team, project, title, description}`
  objects for Linear-bound findings (or, when Linear isn't resolvable, an
  empty array plus the findings recorded in the report's "needs your
  attention").
- `./report.md` — the full per-finding disposition (posted as
  suggestion / posted as prose / top-level fallback / +1 on existing
  comment / filed to Linear / needs attention / dropped — with reasons),
  the PR resolution line, and any override applied.

**On abort** (no PR resolved — see "Inputs"): write `./comments.json` as
an empty array `[]` (nothing was postable) and `./report.md` explaining
why posting was aborted and that a PR ref is required; skip
`./reactions.json` and `./linear_tickets.json`. Same shape as a real run's
abort, just written to disk instead of acted on.

Honor user-supplied paths if provided.
