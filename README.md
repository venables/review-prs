# autoreview

An automated code review system. It watches a repo's pull requests, reviews
each one with a panel of independent models, posts what they found, and
approves the ones that come back clean — then keeps watching, so a PR opened or
pushed to while it runs gets picked up too.

You can also drive every part of it by hand.

| Command                     | Reviews                    | Use when                                                                 |
| --------------------------- | -------------------------- | ------------------------------------------------------------------------ |
| [`autoreview`](#autoreview) | a repo's PRs, headlessly   | the default: no terminal needed, and an exit status that means something |
| [`panel`](#panel)           | one change, several models | you want independent second opinions on a single diff                    |
| [`review-prs`](#review-prs) | a repo's PRs, one tab each | you want to watch a review happen and steer it mid-flight                |

## How a review happens

```
autoreview             picks every PR that is NEW or UPDATED
  └─ dash-p → claude   one headless agent per PR, --jobs at a time
       └─ /auto-review a panel of models reviews the diff independently,
                       their findings are synthesized, verified, and posted
  ← verdict            read back from GitHub, not taken from the agent's word
  ↻ --babysit          watch on an interval: new PRs join, fixed ones leave
```

The panel in the middle is the
[`auto-review`](https://github.com/venables/skills) skill, which each
agent runs against its own PR. [`panel`](#panel) is that same idea as a binary
you can point at any diff, with no PR and no skill involved.

Four things that shape the whole design:

- **The exit status means the reviews succeeded**, not that the processes
  started. A cron job or a CI step can tell a finished sweep from a broken one.
- **The verdict is read back from GitHub.** An agent that believed its own
  report would show "approved" for an approval that never landed.
- **A review runs in a session you can reopen.** Losing the terminal loses live
  steering, not access: every review's session id is printed, and
  `claude --resume <id>` picks it up.
- **Unattended means bounded.** `--jobs` caps concurrency, `--budget` caps each
  review's spend, `--timeout` stops a wedged one, `--max-passes` stops a
  conversation becoming a loop, and `--max-idle` stops a quiet PR keeping a
  cron-started process alive forever.

## Requirements

- [`gh`](https://cli.github.com) — authenticated (`gh auth login`)
- [`gum`](https://github.com/charmbracelet/gum) — the interactive picker only.
  A sweep (`review-prs --auto`, plain `autoreview`) never reaches it.
- [`dash-p`](https://github.com/venabots/dash-p) — **`autoreview` only**: the
  built-in reviewer runs through it (`brew install venabots/tap/dash-p`; set
  `$DASHP_BIN` to point elsewhere). Not needed when `$AUTOREVIEW_CMD` replaces
  the reviewer.
- `pgrep` — refuses to resume a review session another process still holds;
  without it `--continue` loses that guard in both tools, and `autoreview`
  requires it. Standard on macOS; `procps` on slim Linux images.
- A supported terminal for spawning tabs — **`review-prs` only**:
  - [Herdr](https://herdr.dev) (preferred; detected via `HERDR_ENV`, drives new
    tabs over its socket API via the `herdr` CLI), or
  - [cmux](https://cmux.io) (detected via `CMUX_SURFACE_ID`), or
  - [Ghostty](https://ghostty.org) 1.3+ on macOS (detected via `TERM_PROGRAM`,
    drives new tabs through AppleScript — needs Accessibility permission)

## Install

### Homebrew

```sh
brew install venabots/tap/autoreview
```

### Manual

```sh
git clone git@github.com:venabots/autoreview.git
cargo install --path autoreview
```

Either way you get all three binaries. All three are self-contained: nothing is
read from the checkout at runtime, so a copy or a symlink anywhere on `$PATH`
works.

## autoreview

The default way to run all this. Each PR is reviewed by a
[dash-p](https://github.com/venabots/dash-p) subprocess driving claude
headlessly, `--jobs` at a time. The run shows a live per-PR board, reads each
review's verdict back from GitHub when it finishes, and ends with a summary of
verdicts, findings, models and cost. It exits nonzero if any review failed,
which is what makes it safe to put in cron or CI.

```sh
autoreview                  # review every NEW/UPDATED PR
autoreview --jobs 3         # ...three at a time (default 2)
autoreview --pick           # picker, then review each selection headlessly
autoreview --continue       # resume earlier sessions for a second look
autoreview --babysit=15     # re-run every 15 min, picking up new PRs as they open
autoreview --help           # usage
```

**Sweeping is the default, and the picker is the flag.** There are no tabs to
choose between here, so the ordinary run is "review whatever is actionable" and
`--pick` is for the times you want a subset. ([`review-prs`](#review-prs) is
the other way round: it opens a tab per PR, so choosing which tabs to open is
the point.) `--auto` / `-A` still parse —
an old alias or cron line keeps working — they just name the default now.

It takes the same selection flags as `review-prs` (`--continue`, `--all`,
`--dependabot`, `--babysit`) plus ten of its own: `--pick`, `--watch`,
`--focus`, `--no-post`, `--jobs`, `--timeout`, `--budget`, `--log-dir`,
`--max-passes` and `--max-idle`.

**`--focus` steers a run.** It reaches every panelist as the reviewer focus:

```sh
autoreview --focus "be strict about the ledger migration"
```

Use it for what you care about today. Anything the repo always cares about
belongs in its `CLAUDE.md`, which the panelists read anyway.

**`--no-post` leaves the PR alone.** No comments, no approval — the reviews go
to `pr-N.review.md` and the summary says nothing was posted:

```sh
autoreview --no-post
```

This works by choosing the reviewer, not by asking one to hold back: the run
gets the skill that has no posting step. That is also why it is refused
alongside `$AUTOREVIEW_AUTO_CMD`, which decides for itself what it posts. The
session stays resumable, so a later `--continue` run can pick the review up
and post it.

On a terminal the pass is a live board -- finished reviews settle into
permanent result lines, running ones spin, and a progress bar tracks the
pass:

```
2 PRs to review: #9 #8
reviewing 2 PRs · logs: /tmp/autoreview.k3Xq8p/run-Qszknc/pass-1

  ✓ #9 approved · risk LOW · 4m12s · $0.51  @alice Add retry logic
  ⠹ #8 @bob Fix flaky test · rechecking 1m47s
  ━━━━━━━━━━━━╸───────────  1/2 · 1 running
```

Each row says who opened the PR, and whether this is a first look
(`reviewing`) or a second one against the findings already in that session
(`rechecking`) — both questions you would otherwise open the PR to answer.
Every `#N` is an OSC 8 hyperlink, so a terminal that supports them opens the
PR on cmd-click.

The header only mentions concurrency when it actually holds reviews back: with
five PRs and `--jobs 2` it reads `reviewing 5 PRs, 2 at a time`.

The summary is a pair of tables -- what each review concluded, and which models
did the reviewing. Every `#N` is an OSC 8 hyperlink to the PR, so a terminal
that supports them (Ghostty, iTerm2, WezTerm, kitty, VS Code) opens it on
cmd-click. Piped output, `$NO_COLOR` and `TERM=dumb` get plain text instead:

```
╭────┬────────┬────────────────┬────────┬──────────────┬───────┬───────┬────────────────╮
│ PR ┆ RESULT ┆ VERDICT        ┆ RISK   ┆ FINDINGS     ┆ TIME  ┆ COST  ┆ MODEL          │
╞════╪════════╪════════════════╪════════╪══════════════╪═══════╪═══════╪════════════════╡
│ #9 ┆ done   ┆ approved       ┆ LOW    ┆ 1 polish     ┆ 4m12s ┆ $0.51 ┆ claude-fable-5 │
│ #8 ┆ done   ┆ commented      ┆ MEDIUM ┆ 2 should-fix ┆ 6m03s ┆ $0.88 ┆ claude-fable-5 │
│ #7 ┆ done   ┆ nothing posted ┆ LOW    ┆ none         ┆ 2m10s ┆ $0.31 ┆ claude-fable-5 │
╰────┴────────┴────────────────┴────────┴──────────────┴───────┴───────┴────────────────╯
╭────┬─────────────────┬──────────┬──────────┬────────╮
│ PR ┆ MODEL           ┆ STATUS   ┆ FINDINGS ┆ TOP    │
╞════╪═════════════════╪══════════╪══════════╪════════╡
│ #9 ┆ gpt-5.5         ┆ answered ┆ 1        ┆ LOW    │
│ #9 ┆ claude-opus-4.7 ┆ answered ┆ 0        ┆ -      │
│ #8 ┆ gpt-5.5         ┆ answered ┆ 3        ┆ MEDIUM │
│ #8 ┆ claude-opus-4.7 ┆ failed   ┆ -        ┆ -      │
╰────┴─────────────────┴──────────┴──────────┴────────╯
reopen any review with: claude --resume <SESSION>
  #9  cc10f740-28c3-58c6-ae64-d9ff37df22a7
  #8  fa5ced7b-32dd-578b-a3b9-d4d23195dce1
logs: /tmp/autoreview.k3Xq8p/run-Qszknc/pass-1
```

Two columns worth reading carefully:

- **RESULT** is the review process: `done`, `timed out`, `failed (exit 10)`.
- **VERDICT** is what landed on the PR: `approved`, `commented`,
  `changes requested`, or `nothing posted` when the review finished without
  leaving a review behind. `nothing posted` is not a rejection; it means the
  reviewer had nothing to submit, or that GitHub has no record of a submission.

The panel table's **STATUS** answers only "did this panelist come back with a
review", not "did it like the PR" — `answered`, `failed`, or `-` when the
reviewer did not say. The panelist's CLI name is dropped: the model identifies
the row, and a panelist that never reported one falls back to its name
(`opencode`).

Without a TTY -- cron, CI, piped output -- the board becomes one plain line
per state change and the summary a plain aligned table with the same columns,
plus one `panel #N:` line per PR with panel data, which keeps both the CLI
name and the model: `panel #9: codex (gpt-5.5) 1 finding, top LOW`.

### Verdicts and models

The VERDICT column is read back from GitHub, not taken from the agent's
report: after a review finishes, autoreview asks `gh` whether your login
submitted a review on that PR since the job started -- approved, commented,
or changes requested. An agent that believed its own report would show
"approved" for an approval that never landed.

Everything GitHub cannot know comes from the reviewer itself. A system-prompt
instruction sent with every built-in review asks the agent to end its final
reply with a fenced `autoreview` code block: its decision, the synthesized
risk, the finding counts per bucket, and every launched panelist with its
self-reported model, result, finding count and top severity. The block is
best-effort -- a review that never writes it costs a `-` in those columns,
nothing more -- and it is also the verdict fallback when GitHub has nothing
to say.

MODEL is dash-p's accounting (`model_resolved` in its envelope): the model
that drove the review session. The panel table lists the models the
panelists ran on, as they reported them to the session that fanned them out.

### Reaching into a review

There is no tab to interrupt, which loses live steering but not access. Every
review runs in a session with a derived id, and the summary prints one per PR:

```sh
claude --resume cc10f740-28c3-58c6-ae64-d9ff37df22a7
```

That reopens the review as it was, findings and all. (The id comes from the
result envelope, so it names the session the review actually ran in, even when
the run let Claude Code allocate its own.) Intervention becomes on-demand
rather than up-front — which is the trade that makes an unattended sweep
possible at all.

The same trade buys three other things a tab cannot give you:

- **It runs anywhere.** No terminal required, so ssh, cron and CI are all fine.
  ([`review-prs`](#review-prs) needs herdr, cmux or Ghostty and refuses without
  one.)
- **Bounded concurrency.** Twelve PRs is twelve tabs the other way; here it is
  `--jobs 2`. Keep that number low — a panel review is itself several agents,
  so `--jobs 4` can mean a dozen concurrent processes.
- **Per-PR accounting.** dash-p's meta envelope gives cost per PR, and
  `--budget` caps each review's spend (claude's `--max-budget-usd`).

The exit status is the fourth, and it is the one that makes the whole thing
automatable: dash-p's codes are the signal, so an `is_error` turn and garbage
output are both `agent-error`, and a review that overruns `--timeout` counts as
failed too.

### Prompts

Skills are invoked by slash name rather than in prose — an unattended one-shot
has no human to correct a prompt that failed to trigger the skill.

| Run                                                  | Prompt            |
| ---------------------------------------------------- | ----------------- |
| The default sweep, and `--babysit`                   | `/auto-review N`  |
| A first review under `--pick`                        | `/panel-review N` |
| `--continue`, and every babysit pass after the first | `/recheck-pr N`   |

Reaching for `--pick` is the one thing that proves somebody is watching, so it
is what marks a run attended — and a `--babysit` loop outlives that person
either way, so it stays unattended.

There is no `pr-review-tab` here: that skill exists to close its own tab and to
run an in-session `/loop`, and a headless process needs neither.

### Babysit, headless

`--babysit` re-runs the pass on an interval until there is nothing left to do.
Each interval the queue is **rebuilt**, not just shrunk:

- **PRs leave** when `gh pr view` says they are approved or closed. Approval is
  read back from GitHub rather than inferred from what the agent said — the
  review either landed or it did not. A PR merged without an approving review
  leaves too; waiting for an approval that is never coming would re-review it
  every interval forever.
- **PRs join** when the sweep now ranks them actionable — a PR opened while the
  last pass was running, or one the author has just pushed to. Before this, the
  queue was fixed when the run started and a PR opened a minute later waited
  for a whole new invocation.

The two directions come from different sources on purpose. Leaving is decided
per PR by `gh pr view`, which is authoritative. Joining is decided by the sweep,
which is a snapshot and can lag by a poll — so a PR that has left is finished
for the run, and a stale list cannot put it back into the queue that just
dropped it.

Nothing new is needed to decide "should I look again": a review autoreview
posts becomes _your_ latest activity on that PR, so an author pushing a fix
afterwards flips it back to `UPDATED` on its own. The sweep filter already
means "actionable now".

A PR is queued **only while the sweep ranks it actionable**. After a review,
that review is your own latest activity, so an untouched PR goes quiet by
itself and costs nothing until its author moves — and comes back the moment
they push.

`--pick --babysit` stays inside what you picked. A run told to watch two PRs
does not quietly grow to five.

### The two bounds

Both exist because this runs unattended, and both end the run cleanly:

- `--max-passes` (default 3, or `$AUTOREVIEW_MAX_PASSES`) — every review is
  activity on the PR, so an author who answers one makes it actionable again,
  which would make autoreview review it again for as long as the loop runs.
  After that many passes on one PR, it says so and leaves it alone.
- `--max-idle` (default 3, or `$AUTOREVIEW_MAX_IDLE`) — how many checks in a
  row may find nothing to do. A PR nobody is touching should not keep a process
  alive forever, least of all one cron started, where the next run would pile
  on top of this one. The check straight after a pass does not count: your own
  review is the newest thing on every PR you just reviewed, so that one is idle
  by construction.

A transient API error does not end the loop and does not silently narrow it
either: the refresh keeps the current queue and tries again next interval,
giving up only after three consecutive failures.

The loop is the autoreview process itself, not an in-session `/loop` inside a
tab, so an interval that never converges is one process you can see and kill.
Interrupting it stops
the running reviews too, along with their children: an orphan keeps spending and
keeps holding its session open, which would make the next `--continue` refuse to
resume it.

### Logs

Each pass writes to `$log_dir/run-<random>/pass-N/`, printed at the start of every
run and again in the summary. The per-run directory is what lets two runs share
a `--log-dir` — ordinary under cron, where the default hour-long timeout outlasts
most intervals — without reading each other's results:

- `pr-N.review.md` — the review as text, which is the one to open
- `pr-N.json` — dash-p's answer (`{"answer": ..., "metadata": ...}`)
- `pr-N.meta.json` — the metadata envelope (session id, cost, exit status);
  written even when a timeout or interrupt leaves stdout empty
- `pr-N.log` — stderr, which is where a failure explains itself

`--log-dir` pins the location; the default is a fresh temp directory per run.

### Overrides

`$AUTOREVIEW_AUTO_CMD` for unattended runs (the default sweep and `--babysit`),
and `$AUTOREVIEW_CMD` for `--pick` runs, replace the built-in reviewer. Same
substitution rules as `$REVIEW_PRS_CMD` — the PR number replaces the first `{}`,
or is appended if there is no placeholder:

```sh
AUTOREVIEW_AUTO_CMD='my-review' autoreview
AUTOREVIEW_CMD='gh pr checkout {} && my-review {}' autoreview --pick
```

An override owns its own session handling and receives
`$REVIEW_PRS_SESSION_ID` and `$REVIEW_PRS_SESSION_RESUME` — the same contract
`review-prs` uses, so one wrapper works with both. Here they arrive in the
child's real environment; `review-prs` can only reach a new tab through a
command string, so it exports them there instead. Cost, session and model are dash-p's accounting, so the
summary shows `-` for them under an overridden reviewer -- it owns its own
sessions -- and the trailer is not read either. The verdict column still
works: it is read back from GitHub, which does not care who reviewed.

## panel

`panel` reviews **one change with several models at once**. Every backend CLI
on `PATH` (codex, claude, opencode) reviews the same diff in parallel through
dash-p, blind to the others. Their reports print as they land. One more model
call then reads all of them, verifies the questionable claims against the code,
and writes the report.

```sh
panel                         # review what you have not committed yet
panel --base main             # review what this branch added
panel --panelist codex --panelist claude:opus-4.8
panel --focus "the retry path"
panel --no-synthesis          # the raw reports, no synthesis
panel --help
```

### Why it is shaped this way

The same review used to be an agent skill: a coordinator model read 656 lines
of instructions and drove a 1094-line bash script that fanned the panelists
out. That put a model in charge of work that needs no judgment at all —
spawning processes, polling them, retrying a quota blip, collecting output —
and made the fan-out only as reliable as the coordinator's willingness to
follow instructions.

So the mechanical half is a program and the judgment half is one model call:

```
panel  ─┬─ dash-p → codex     ─┐
        ├─ dash-p → claude    ─┼─→ dash-p → claude (synthesis, read-only, in the repo)
        └─ dash-p → opencode  ─┘
```

The synthesis runs **in the repository**, not on the text alone. Verifying a
finding means reading the code it is about — a synthesizer handed only the
panelists' prose can merge their claims but cannot check any of them.

It is told two things a naive pipe would drop: which panelists failed, so their
silence is not read as agreement, and how many answered, so "flagged by 2 of 3"
means what it says.

### Isolation

A committed target (`--base`) gives each panelist its own throwaway git
worktree pinned to the same commit, with write and exec: panelists run the test
suite, grep for callers, and edit files to investigate, and nothing survives
the run. One worktree each rather than one shared, because parallel reviewers
racing on `target/` and `node_modules` produce flaky findings and leak edits
into each other's reading.

Uncommitted work has no ref to pin, so panelists read your actual working tree
with `--perms read-only` and change nothing.

Either way the worktrees are removed when the run ends — including on ctrl-C,
which stops the panelists first.

### What it does not do yet

`--pr` (review a GitHub PR by number, with panelists fetching it themselves),
review approaches (`/decompose`), and `$PANEL_REVIEW_PANELISTS`-style env
configuration all still live in the skill. This is the common path only.

## review-prs

The same PR list, fanned into one terminal tab per PR instead of a headless
process. Reach for it when you want to watch a review happen and interrupt it;
reach for [`autoreview`](#autoreview) for everything else.

Run from inside any GitHub repo:

```sh
review-prs              # open, non-draft, unapproved PRs (excludes yours + bots)
review-prs --auto       # skip the picker; auto-review every NEW/UPDATED PR
review-prs --babysit    # re-check non-approvable PRs on an interval until approved
review-prs --babysit=15 # ...every 15 minutes (default 30)
review-prs --continue   # resume an earlier review session instead of starting over
review-prs --all        # also include PRs already marked APPROVED
review-prs --dependabot # also include Dependabot PRs (shown dimmed)
review-prs --help       # usage
```

In the picker: `space` toggles a PR, `enter` confirms. Each selected PR opens in
a fresh tab.

### The review command

Each spawned tab `cd`s to the repo root and runs a review command for the PR.
By default that is a non-interactive [Claude Code](https://claude.com/claude-code)
panel review:

```sh
claude --dangerously-skip-permissions --session-id <uuid> "panel review <number>"
```

(The `--session-id` is what makes [`--continue`](#continue-mode) possible later.)

Override it with the `REVIEW_PRS_CMD` environment variable. The PR number is
substituted for the first `{}` placeholder, or appended if there is no
placeholder:

```sh
# Append form — runs `review 123` in each tab (e.g. a shell function/alias):
REVIEW_PRS_CMD='review' review-prs

# Placeholder form — substitute the number anywhere in the command:
REVIEW_PRS_CMD='gh pr checkout {} && my-reviewer' review-prs
```

Note that `REVIEW_PRS_CMD` must be on the spawned tab's `PATH` (or be a shell
function/alias defined in its startup files) — the command runs in a fresh
shell, not the one you launched `review-prs` from.

An overridden command owns its own session handling. It still receives the PR's
session id as `$REVIEW_PRS_SESSION_ID`, along with `$REVIEW_PRS_SESSION_RESUME`
(`1` when `--continue` matched an existing session, `0` otherwise), so it can
wire up resumption however it likes.

### Auto mode

`--auto` skips the picker entirely: it fans out **every `NEW` and `UPDATED` PR**
(the actionable ones) and runs an auto-review command in each tab. `SEEN` PRs
are skipped on purpose — nothing has changed since you last engaged, so there's
no reason to re-review them. Combine with `--all` / `--dependabot` to widen the
set.

The per-tab command is `REVIEW_PRS_AUTO_CMD` (same `{}`/append substitution as
`REVIEW_PRS_CMD`), defaulting to the
[`pr-review-tab`](https://github.com/venables/skills) skill:

```sh
claude --dangerously-skip-permissions --session-id <uuid> "pr-review-tab <number>"
```

That skill runs an auto-review and, **when the PR is approved, closes its tab**
so a finished review cleans up after itself. (Tabs are closed via the enclosing
multiplexer — `herdr tab close` / `cmux close-surface` — from inside the tab,
which is why the behavior lives in the skill, not in review-prs.)

### Babysit mode

`--babysit` keeps a not-yet-approvable PR's tab open and **re-checks it on an
interval until it can be approved**, then closes the tab — so a fix pushed
overnight gets stamped without you re-running anything. The interval defaults to
30 minutes; set it with `--babysit=MINUTES` or `$REVIEW_PRS_BABYSIT_INTERVAL`. A
bare number is minutes, and suffixed durations (`30m`, `1h`, `2d`) work too;
anything else is rejected up front rather than seeded into the tab.

It uses the same unattended command as `--auto`, so it composes with both the
sweep (`review-prs --auto --babysit`) and the picker (`review-prs --babysit`,
then choose which PRs to babysit). Under the hood the `pr-review-tab` skill
starts an in-session `/loop` that re-runs the
[`recheck-pr`](https://github.com/venables/skills) skill each interval;
`recheck-pr`'s fast path makes a no-change cycle cheap, and the loop ends when
the tab closes on approval.

### Continue mode

`--continue` (`-C`) reopens the review session a PR already had on this machine
instead of reviewing it from scratch. The resumed tab still holds the earlier
findings, so it takes a **second look** — did the author fix them? — rather than
re-deriving a review the author has already answered.

```sh
review-prs --continue            # picker; RESUMABLE rows reopen their session
review-prs --auto --continue     # sweep, resuming wherever a session exists
```

Each PR gets a session id derived from the repo directory plus
`owner/name#number`, so the same PR in the same checkout maps to the same
session on every run. There is no state file: nothing to sync, nothing to go
stale when a PR closes. A first review pins the id with `--session-id`;
`--continue` reopens it with `--resume` and swaps the prompt:

| Run                    | Prompt                        |
| ---------------------- | ----------------------------- |
| First review           | `panel review <N>`            |
| `--continue`           | `recheck-pr <N>`              |
| `--auto` / `--babysit` | `pr-review-tab <N>`           |
| ...with `--continue`   | `pr-review-tab <N> --recheck` |

PRs with a session show `RESUMABLE` in the picker, so you can see what would be
resumed before you choose. Without `--continue` those PRs review from scratch in
a fresh session, exactly as before.

Two limits worth knowing:

- **A session belongs to one checkout.** Sessions live under
  `~/.claude/projects`, keyed to the repo root the tab `cd`s into, and the id
  hashes that path in. A second clone or a `git worktree` of the same repo
  therefore starts its own session for the same PR rather than reopening the
  other one. Another machine will not find them at all.
- **Sessions grow.** A PR resumed many times accumulates context and eventually
  auto-compacts. That is fine for a second look; it is not a substitute for a
  fresh review when a PR has been rewritten.
- **Only the first review is addressable.** A PR keeps exactly one derived id.
  Reviewing a PR again _without_ `-C` deliberately starts an unnamed session, so
  a later `-C` reopens the first review, not that one. Treat a no-`-C` re-review
  as a throwaway; use `-C` for the thread you want to keep.
- **One tab at a time.** `-C` will not reopen a session another tab still holds
  open — a babysit tab, typically. It says so and reviews fresh instead.

Your own PRs are always excluded — this tool is for reviewing others' work.
Dependabot PRs are hidden by default; pass `--dependabot` to include them, where
they appear dimmed to mark them as lower-priority. (The bot match is one
anchored prefix in `src/prlist.rs` — extend it as more AI coding bots show up.)

### Naming

Under herdr and cmux, review sessions label themselves so a screenful of tabs
stays readable:

- **Workspace** — renamed to `REVIEW_PRS_WORKSPACE` (default `pr reviews`). Set
  it empty (`REVIEW_PRS_WORKSPACE=`) to leave your workspace title alone.
- **Tabs** — named for the PR and what the tab is doing: `PR 27 Review`,
  `PR 27 Auto-Review` (`--auto`), `PR 27 Recheck` (a resumed `--continue`
  session), or `PR 27 Babysit` (`--babysit`, which wins over the others).

Both names are sticky: they survive the terminal-title escapes the review
command emits while it runs, which would otherwise replace them with a generic
agent-generated summary.

The workspace rename and the cmux tab rename are best-effort — a failure there
is ignored. Under herdr the label is applied at tab-create time, so a rejected
label fails that tab's spawn; the sweep warns and continues with the next PR.

Ghostty tabs are left unnamed. It has no sticky-title API, so any title set at
spawn time would be overwritten by the review command within seconds.

## Picking the PRs

`review-prs` and `autoreview` start the same way:

1. List the current repo's open, non-draft PRs (via the GitHub GraphQL API).
2. Annotate each with an engagement badge, a review-state flag, and a relative
   "last activity" time, then sort the most actionable to the top.
3. Take every `NEW` and `UPDATED` PR, or let you multi-select with
   [gum](https://github.com/charmbracelet/gum).

Then they diverge: `review-prs` opens a terminal tab per PR and runs the
[review command](#the-review-command) in it; `autoreview` runs a headless
subprocess per PR instead. `panel` does none of this — it takes one diff and
asks several models about it.

## Columns

```
#NUM   ENGAGEMENT   REVIEW   SESSION   TIME   AUTHOR   TITLE
```

### Engagement

How the PR stands relative to your own comments and reviews:

| Badge     | Meaning                                                       |
| --------- | ------------------------------------------------------------- |
| `NEW`     | You have not commented or reviewed this PR.                   |
| `UPDATED` | New comments, reviews, or commits since your last engagement. |
| `SEEN`    | You engaged and nothing has changed since.                    |

### Review

The PR's overall review decision:

| Flag       | Meaning                                                       |
| ---------- | ------------------------------------------------------------- |
| `CHANGES`  | Reviewers requested changes (`CHANGES_REQUESTED`).            |
| `APPROVED` | Already approved. Hidden by default; shown only with `--all`. |
| `-`        | No decision yet.                                              |

### Session

Whether this machine already has a review session for the PR:

| Flag        | Meaning                                                    |
| ----------- | ---------------------------------------------------------- |
| `RESUMABLE` | An earlier review session exists; `--continue` reopens it. |
| `-`         | No session yet; a review starts from scratch.              |

The column only appears when at least one PR is resumable — on a repo you have
never reviewed, every row would read `-`.

PRs are sorted `NEW` first, then `UPDATED`, then `SEEN`, with most recent
activity breaking ties.

## While it waits

Three network calls stand between launch and the first thing worth showing:
`gh repo view`, `gh api user`, then one GraphQL call for the PR list. The two
PR tools say what they are doing through them:

```
reading the repo
fetching open PRs from acme/widgets
found 40 open PRs, 3 to consider
3 PRs to review: #9 (new) #8 (updated) #6 (new)
```

The two counts are deliberate. The second is what is left after your own PRs,
bots and approved ones are filtered out — and on a repo where most of the open
PRs are yours, "found 3" on its own reads as a broken query rather than a
working filter.

The `found` line stays. The steps around it do not: on a terminal they are one
spinner line that rewrites itself and leaves nothing behind, so the report
still starts at the top. Under cron and CI, or with stderr redirected, each
step is a plain line instead.

**stderr decides**, because that is where all of this goes — a run whose stdout
is piped to a file still has a terminal to spin on, and still leaves that file
holding only the report.

`panel` reports its own three waits the same way, and the last two count up
rather than sitting still:

```
reading the repo
building the diff
materializing worktree 3 of 4
3 panelists still reviewing, 4m12s
synthesizing with claude, 1m30s
```

Materializing a checkout per panelist is the longest thing a panel run does
before a model is asked anything, and the synthesis is the most expensive
silence in the run to mistake for a hang — every panelist has already been
paid for by the time it starts.

## Notes

- The GraphQL query inspects up to the 100 most recent comments, reviews, and
  commits per PR — ample for typical PRs, and always inclusive of the latest
  activity that drives the engagement badge.
- Tabs are spawned serially; Ghostty gets a small delay between tabs to avoid
  racing the new-tab keystroke.

## Development

```sh
bash tests/run.sh
```

### Layout

One crate, one library, two binaries:

```
src/lib.rs         the shared core all three binaries are built on
src/bin/           the three entry points: review-prs, autoreview, panel

src/prlist.rs      the GraphQL query, engagement ranking, the sweep
src/picker.rs      the gum picker
src/select.rs      fetch, rank, then sweep or pick
src/session.rs     derived session ids, and how a PR attaches to one
src/repo.rs        dependency checks, repo and user context
src/interval.rs    babysit-interval parsing
src/cli.rs         autoreview's flags

src/tabs/          review-prs: cli, per-tab command, terminal spawners
src/panel/         panel: cli, target, worktrees, fan-out, synthesis
prompts/           the panelist and synthesis prompts, compiled in
src/pool.rs        autoreview: the event-driven job pool
src/job.rs         autoreview: one review, spawned and classified
src/report.rs      autoreview: verdict readback and the agent's trailer
src/rundir.rs      autoreview: what one run writes under --log-dir
src/ui.rs          autoreview: the live board and the summary
```

The two tools have to agree on what counts as an actionable PR and which
session it belongs to. That is not maintained — it is structural: they select
and derive through the same code. What differs is everything below the
selection, and it is what each binary is: `review-prs` spawns tabs,
`autoreview` runs an event-driven job pool over dash-p subprocesses.

### Tests

`tests/run.sh` builds the crate, runs `cargo test` (the unit layer: interval
parsing, session goldens, ranking, CLI validation, argv and tab-command
shapes), then runs the bash suites — the real binaries against fake `gh`,
`gum`, `cmux` and `dash-p` on `PATH`, inside a throwaway git repo, with
`$CLAUDE_CONFIG_DIR` pointed at a throwaway session store. They never touch
your repos, your Claude Code sessions, or GitHub. It finishes with `bash -n`
and `shellcheck -x` over the suite itself, which is the only bash in the repo.

The suite takes about a minute and a half, most of it one test: whether a
babysit pass resumes the session the previous pass ran in can only be shown by
running a second pass, and the shortest interval the tool accepts is a minute.

CI runs the same command on macOS and Linux. macOS is the primary target;
Linux catches anything that quietly depended on it.

## License

MIT
