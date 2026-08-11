# review-prs

Pick open GitHub PRs from a multi-select list and fan each one out into its own
terminal tab running a review command per PR. Built for batch-reviewing a repo's
open pull requests without manually opening tabs and typing commands.

Pairs nicely with the [`panel-review`](https://github.com/catena-labs/dev-skills)
skill, which is the default review command each tab runs.

## What it does

1. Lists the current repo's open, non-draft PRs (via the GitHub GraphQL API).
2. Annotates each with an engagement badge, a review-state flag, and a relative
   "last activity" time, then sorts the most actionable ones to the top.
3. Lets you multi-select with [gum](https://github.com/charmbracelet/gum).
4. Opens a new terminal tab per selection, `cd`s to the repo root, and runs the
   review command (see [Review command](#review-command)) for each PR.

## Requirements

- [`gh`](https://cli.github.com) — authenticated (`gh auth login`)
- [`gum`](https://github.com/charmbracelet/gum) — the interactive picker
- [`jq`](https://jqlang.github.io/jq/) — JSON processing
- A supported terminal for spawning tabs:
  - [Herdr](https://herdr.dev) (preferred; detected via `HERDR_ENV`, drives new
    tabs over its socket API via the `herdr` CLI), or
  - [cmux](https://cmux.io) (detected via `CMUX_SURFACE_ID`), or
  - [Ghostty](https://ghostty.org) 1.3+ on macOS (detected via `TERM_PROGRAM`,
    drives new tabs through AppleScript — needs Accessibility permission)

## Install

### Homebrew

```sh
brew install venables/tap/review-prs
```

### Manual

```sh
git clone git@github.com:venables/review-prs.git
ln -s "$PWD/review-prs/review-prs" /usr/local/bin/review-prs
```

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

The tests run the real script against fake `gh`, `gum`, and `cmux` binaries on
`PATH`, inside a throwaway git repo, with `$CLAUDE_CONFIG_DIR` pointed at a
throwaway session store. They never touch your repos, your Claude Code sessions,
or GitHub. `tests/run.sh` also runs `bash -n` and `shellcheck`.

CI runs the same command on macOS and Linux — macOS because it ships bash 3.2
and catches bashisms the script must not use, Linux because it has `md5sum`
rather than `md5` and so exercises the other branch of the hash helper.

## License

MIT
