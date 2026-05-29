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
  - [cmux](https://cmux.io) (preferred; detected via `CMUX_SURFACE_ID`), or
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
claude --dangerously-skip-permissions "panel review <number>"
```

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

Your own PRs are always excluded — this tool is for reviewing others' work.
Dependabot PRs are hidden by default; pass `--dependabot` to include them, where
they appear dimmed to mark them as lower-priority. (The bot match is a single
anchored regex in the script — extend it as more AI coding bots show up.)

## Columns

```
#NUM   ENGAGEMENT   REVIEW      TIME       AUTHOR   TITLE
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

PRs are sorted `NEW` first, then `UPDATED`, then `SEEN`, with most recent
activity breaking ties.

## Notes

- The GraphQL query inspects up to the 100 most recent comments, reviews, and
  commits per PR — ample for typical PRs, and always inclusive of the latest
  activity that drives the engagement badge.
- Tabs are spawned serially; Ghostty gets a small delay between tabs to avoid
  racing the new-tab keystroke.

## License

MIT
