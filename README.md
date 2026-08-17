# review-prs

Pick open GitHub PRs from a multi-select list and fan each one out into its own
terminal tab running a review command per PR. Built for batch-reviewing a repo's
open pull requests without manually opening tabs and typing commands.

Two entry points, same PR list:

| Command                     | Runs each review in            | Use when                                                                |
| --------------------------- | ------------------------------ | ----------------------------------------------------------------------- |
| `review-prs`                | a terminal tab you can steer   | you want to watch a review happen and interrupt it                      |
| [`autoreview`](#autoreview) | a headless `claude -p` process | there is no terminal (ssh, cron, CI), or a dozen PRs means a dozen tabs |

Pairs nicely with the [`panel-review`](https://github.com/catena-labs/dev-skills)
skill, which is the default review command both run.

## What it does

1. Lists the current repo's open, non-draft PRs (via the GitHub GraphQL API).
2. Annotates each with an engagement badge, a review-state flag, and a relative
   "last activity" time, then sorts the most actionable ones to the top.
3. Lets you multi-select with [gum](https://github.com/charmbracelet/gum).
4. Opens a new terminal tab per selection, `cd`s to the repo root, and runs the
   review command (see [Review command](#review-command)) for each PR.

Steps 1-3 are shared with [`autoreview`](#autoreview), which replaces step 4
with a headless subprocess per PR.

## Requirements

- [`gh`](https://cli.github.com) — authenticated (`gh auth login`)
- [`gum`](https://github.com/charmbracelet/gum) — the interactive picker.
  `autoreview --auto` never picks, so it does not need gum.
- [`jq`](https://jqlang.github.io/jq/) — JSON processing
- `pgrep` — required by `autoreview`, which walks a review's process tree with it
  to stop one; without it a timeout would report a stopped review while the whole
  tree kept running. `review-prs` runs fine without it, but `--continue` loses
  its guard against resuming a session another tab still holds. Standard on
  macOS; `procps` on slim Linux images.
- A supported terminal for spawning tabs — **`review-prs` only**:
  - [Herdr](https://herdr.dev) (preferred; detected via `HERDR_ENV`, drives new
    tabs over its socket API via the `herdr` CLI), or
  - [cmux](https://cmux.io) (detected via `CMUX_SURFACE_ID`), or
  - [Ghostty](https://ghostty.org) 1.3+ on macOS (detected via `TERM_PROGRAM`,
    drives new tabs through AppleScript — needs Accessibility permission)

## Install

### Homebrew

```sh
brew install venabots/tap/review-prs
```

### Manual

```sh
git clone git@github.com:venabots/review-prs.git
ln -s "$PWD/review-prs/review-prs" /usr/local/bin/review-prs
ln -s "$PWD/review-prs/autoreview" /usr/local/bin/autoreview
```

Symlinks are fine: both entry points resolve themselves through any links to
find the shared `lib/` next to the real file. Do not copy a single script out of
the checkout on its own — it will not find `lib/`.

## Usage

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

## Review command

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
[`pr-review-tab`](https://github.com/catena-labs/dev-skills) skill:

```sh
claude --dangerously-skip-permissions --session-id <uuid> "pr-review-tab <number>"
```

That skill runs an auto-review and, **when the PR is approved, closes its tab**
so a finished review cleans up after itself. (Tabs are closed via the enclosing
multiplexer — `herdr tab close` / `cmux close-surface` — from inside the tab,
which is why the behavior lives in the skill, not this script.)

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
[`recheck-pr`](https://github.com/catena-labs/dev-skills) skill each interval;
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
they appear dimmed to mark them as lower-priority. (The bot match is a single
anchored regex in the script — extend it as more AI coding bots show up.)

## autoreview

`autoreview` reviews the same PRs without tabs. Each one runs as a headless
`claude -p` subprocess; the run shows live per-PR progress, prints a summary,
and exits nonzero if any review failed.

```sh
autoreview                  # picker, then review each selection headlessly
autoreview --auto           # review every NEW/UPDATED PR
autoreview --auto --jobs 3  # ...three at a time (default 2)
autoreview --continue       # resume earlier sessions for a second look
autoreview --babysit=15     # re-run every 15 min until every PR is approved
autoreview --help           # usage
```

It takes the same selection flags as `review-prs` (`--auto`, `--continue`,
`--all`, `--dependabot`, `--babysit`) plus four of its own: `--jobs`,
`--timeout`, `--budget` and `--log-dir`.

```
auto-reviewing 2 PR(s): #9 #8
reviewing 2 PR(s), 2 at a time
logs: /tmp/autoreview.k3Xq8p/run-40127/pass-1

  +  #9     done                           4m12s
  /  #8     reviewing                      1m47s
```

and when it finishes:

```
PR  RESULT  TIME   COST   SESSION
#9  done    4m12s  $0.51  cc10f740-28c3-58c6-ae64-d9ff37df22a7
#8  done    6m03s  $0.88  fa5ced7b-32dd-578b-a3b9-d4d23195dce1

logs: /tmp/autoreview.k3Xq8p/run-40127/pass-1
reopen any review with: claude --resume <SESSION>
```

### What you trade

Losing the tab loses live steering, not access. A PR with no session yet gets
the same derived id `review-prs` would pin, so `claude --resume <id>` reopens
the review afterwards — which is why the summary prints one per PR. (The id
comes from the result envelope, so it names the review that just ran even when
the run let Claude Code allocate its own.) Intervention becomes on-demand rather
than up-front.

What you gain:

- **No terminal needed.** `review-prs` requires herdr, cmux or Ghostty and
  refuses to run without one. This runs anywhere.
- **An exit status that means something.** `review-prs` can only report whether
  the _tabs opened_. This reports whether the _reviews succeeded_, so a cron job
  or CI step can tell a finished sweep from a broken one. A review that exits
  nonzero, that reports `is_error` inside a zero exit, or that overruns
  `--timeout` all count as failures.
- **Bounded concurrency.** Twelve PRs is twelve tabs under `review-prs`; here it
  is `--jobs 2`. Keep that number low — a panel review is itself several agents,
  so `--jobs 4` can mean a dozen concurrent processes.
- **Per-PR accounting.** `--output-format json` gives cost and turns per PR, and
  `--budget` caps each review's spend (`claude --max-budget-usd`).

### Prompts

Skills are invoked by slash name rather than in prose — an unattended one-shot
has no human to correct a prompt that failed to trigger the skill.

| Run                                                  | Prompt            |
| ---------------------------------------------------- | ----------------- |
| First review                                         | `/panel-review N` |
| `--auto` / `--babysit`                               | `/auto-review N`  |
| `--continue`, and every babysit pass after the first | `/recheck-pr N`   |

There is no `pr-review-tab` here: that skill exists to close its own tab and to
run an in-session `/loop`, and a headless process needs neither.

### Babysit, headless

`--babysit` re-runs the whole pass on an interval, dropping PRs as they become
`APPROVED` and resuming the rest, until nothing is left. Approval is read back
from GitHub rather than inferred from what the agent said — the review either
landed as an approval or it did not. A PR that is closed, or merged without an
approving review, is dropped too; waiting for an approval that is never coming
would re-review it on every interval forever.

The loop is this script, not an in-session `/loop` inside a tab, so an interval
that never converges is one process you can see and kill. Interrupting it stops
the running reviews too, along with their children: an orphan keeps spending and
keeps holding its session open, which would make the next `--continue` refuse to
resume it.

### Logs

Each pass writes to `$log_dir/run-<pid>/pass-N/`, printed at the start of every
run and again in the summary. The per-run directory is what lets two runs share
a `--log-dir` — ordinary under cron, where the default hour-long timeout outlasts
most intervals — without reading each other's results:

- `pr-N.json` — claude's result envelope (the review text is in `.result`)
- `pr-N.log` — stderr, which is where a failure explains itself

`--log-dir` pins the location; the default is a fresh temp directory per run.

### Overrides

`$AUTOREVIEW_CMD`, and `$AUTOREVIEW_AUTO_CMD` for `--auto`/`--babysit` runs,
replace the built-in reviewer. Same substitution rules as `$REVIEW_PRS_CMD` —
the PR number replaces the first `{}`, or is appended if there is no
placeholder:

```sh
AUTOREVIEW_CMD='my-review' autoreview --auto
AUTOREVIEW_CMD='gh pr checkout {} && my-review {}' autoreview
```

An override owns its own session handling and receives
`$REVIEW_PRS_SESSION_ID` and `$REVIEW_PRS_SESSION_RESUME` — the same contract
`review-prs` uses, so one wrapper works with both. Unlike `review-prs`, which
can only reach a new tab through a command string, these arrive in the child's
real environment. Cost is claude's own accounting, so the summary shows `-` for
an overridden reviewer.

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

## Naming

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

```
review-prs        entry point: the picker and the terminal-tab fan-out
autoreview        entry point: the headless runner
lib/repo.sh       dependency checks, repo and user context
lib/pr-list.sh    the GraphQL query, ranking, the picker, PR selection
lib/session.sh    derived session ids, and how a PR attaches to one
lib/interval.sh   babysit-interval parsing
```

Both entry points source all four libraries, so what counts as an actionable PR
and which session it belongs to is decided in exactly one place. Everything
below the selection differs: `review-prs` spawns tabs, `autoreview` runs a job
pool.

### Tests

The tests run the real scripts against fake `gh`, `gum`, `cmux` and `claude`
binaries on `PATH`, inside a throwaway git repo, with `$CLAUDE_CONFIG_DIR`
pointed at a throwaway session store. They never touch your repos, your Claude
Code sessions, or GitHub. `tests/run.sh` also runs `bash -n` over every script
and `shellcheck -x` over both entry points — `-x` so it follows the `source=`
directives into `lib/`, which is the only context where a library's globals are
defined.

The suite takes about a minute and a half, most of it one test: whether a
babysit pass resumes the session the previous pass ran in can only be shown by
running a second pass, and the shortest interval the tool accepts is a minute.

CI runs the same command on macOS and Linux — macOS because it ships bash 3.2
and catches bashisms the scripts must not use (no `wait -n`, no associative
arrays), Linux because it has `md5sum` rather than `md5` and so exercises the
other branch of the hash helper.

## License

MIT
