# panel-review

Fan a code review out to multiple local CLI coding agents (codex, claude,
opencode) running in parallel, then synthesize their findings into one report.
Every panelist is driven through `dash-p` — one uniform non-interactive
interface over the backend CLIs — so the script passes generic flags and lets
`dash-p` build each CLI's native command line. For PR / branch / commit
targets, each agent gets its own isolated git worktree so they can run tests and
chase downstream effects in parallel without stepping on each other.

## Install

```bash
npx skills add venables/skills --skill panel-review
```

## Requirements

- **`dash-p`** on `PATH` (or set `DASHP_BIN` to the binary) — it's the
  uniform driver every panelist runs through. Install it with
  `brew install venabots/tap/dash-p`.
- At least one backend CLI installed: `codex`, `claude`, and/or `opencode`. The
  panel auto-detects whichever are on `PATH`.
- `git`, and `gh` for PR targets.

## How to use it

Just ask Claude Code in plain English — the skill picks up the target and
panelists from your phrasing:

- "panel review" — auto-detects an open PR for the current branch via `gh` and
  reviews that; falls back to uncommitted work if there is no PR or your tree is
  dirty
- "panel review my latest changes on this branch"
- "panel review my staged changes"
- "panel review PR 27"
- "panel review this branch against main"
- "panel review the auth changes, focus on session handling"
- "panel review with just codex and claude"
- "panel review with claude on opus-4.8 and two opencode reviewers, one on
  qwen-3.7 and one on glm-5.2" — pick reviewers _and_ their models, including
  running the same backend more than once

## Choosing reviewers and models

Each reviewer is a `--panelist backend[/approach][:model]` spec, where backend
is `codex`, `claude`, or `opencode`, and the optional `:model` is the **exact**
model id that backend's CLI expects. It's forwarded verbatim through
`dash-p --model` (which becomes each CLI's own model flag).
The same backend can appear multiple times with different models, so you can fan
one change out to several models — even several models of the same tool.

The optional `/approach` tells that panelist _how_ to review (e.g.
`--panelist claude/decompose:opus-4.8` reviews by chunking the diff + a seam
pass). `--approach <name>` sets a default approach for the whole panel.
Approaches are prompt fragments under `prompts/approaches/`; `decompose` ships
built in, and adding one is just dropping a `prompts/approaches/<name>.md` file.

A concrete four-reviewer panel — claude on Opus 4.8, codex on GPT-5.5, and two
opencode reviewers on different `opencode-go` models:

```bash
panel-review.sh \
  --panelist claude:claude-opus-4-8 \
  --panelist codex:gpt-5.5 \
  --panelist opencode:opencode-go/glm-5.2 \
  --panelist opencode:opencode-go/qwen3.7-max
```

Equivalently, set it from the environment (used when no `--panelist` flag is
passed; space- or comma-separated):

```bash
PANEL_REVIEW_PANELISTS="claude:claude-opus-4-8 codex:gpt-5.5 opencode:opencode-go/glm-5.2 opencode:opencode-go/qwen3.7-max" \
  panel-review.sh
```

That panel produces four reviewers with these ids and headers:

```
- Panelists: claude-claude-opus-4-8 codex-gpt-5.5 opencode-opencode-go-glm-5.2 opencode-opencode-go-qwen3.7-max
## claude-claude-opus-4-8 / claude-opus-4-8 (exit 0)
## codex-gpt-5.5 / gpt-5.5 (exit 0)
## opencode-opencode-go-glm-5.2 / opencode-go/glm-5.2 (exit 0)
## opencode-opencode-go-qwen3.7-max / opencode-go/qwen3.7-max (exit 0)
```

### Finding the right model id per backend

The `:model` part must match what each CLI accepts — don't guess:

- **claude** — an alias (`opus`, `sonnet`, `fable`) or a full id like
  `claude-opus-4-8`. The alias tracks the latest of that family; use the full id
  to pin a specific version. `claude --help` documents `--model`.
- **codex** — a model id such as `gpt-5.5`. Your default is in
  `~/.codex/config.toml` (`model = "..."`); `codex -m <id>` overrides it.
- **opencode** — always `provider/model`, e.g. `opencode-go/glm-5.2`. List every
  installed `provider/model` pair with `opencode models`. (The `/` is preserved
  in the model passed to the CLI; it's only sanitized to `-` in the panelist
  id.)

A bare backend (`--panelist claude`) uses that backend's `CLAUDE_MODEL` /
`CODEX_MODEL` / `OPENCODE_MODEL` env default, or the CLI's own default if unset.
Each reviewer gets a unique id (e.g. `opencode-opencode-go-glm-5.2`) so two
reviewers on the same backend keep separate worktrees, output files, and report
sections.

## What it does

- Auto-detects whether the current branch has an open GitHub PR and switches to
  PR mode by default — no more "stale local main" reviews flagging commits that
  are not actually in the PR.
- For PR targets, panelists fetch the live diff and existing review comments
  themselves via `gh` (no embedded diff in the prompt). For `--base` /
  `--commit` targets it builds a unified diff with `git` and embeds it in the
  prompt. For `--uncommitted` / `--staged` it embeds the local diff.
- For any target with a real ref (`--pr` / `--base` / `--commit`), spins up **a
  dedicated, throwaway git worktree per panelist** pinned to the same commit.
  Panelists can run tests, install deps, and grep callers in parallel without
  racing each other's `node_modules/` / `target/` / `.next/`. One network fetch
  up front, then local-only worktree creation; everything is torn down on exit.
- `--uncommitted` / `--staged` skip the worktree (the changes only exist
  locally) and panelists run read-only against your working tree.
- Spawns each panelist as a fresh, non-interactive `dash-p` subprocess with no
  shared conversation state — the whole point is independent second opinions.
- Asks every panelist four header questions before the line-level findings:
  `Goal:` (what the change does), `Approach:` (is it at the right layer),
  `Purpose:` (why it should exist, and does the diff serve that reason without
  hidden cost), and `Proof:` (what shows it works: tests in the diff, a testing
  note, a run in the worktree). Correct code with no stated reason, or no
  evidence it works, is surfaced as a finding, not waved through.
- Streams each panelist's section back as it lands, then groups results into a
  synthesized summary with overview / risk / goal, approach, purpose, and proof
  checks / must-fix / should-fix / polish / disagreements. Every line in the
  summary points at `file:line` with a suggested fix; for PR targets, the lines
  are tappable links straight to the PR file view.

## Gotchas

- **Background Bash + `BashOutput` polling is required.** Codex dominates wall
  clock, so foreground calls block silently for minutes. Do not launch
  `panel-review.sh` via the `Agent` tool / subagents — there's no
  streaming-output API for in-flight subagents and the heartbeats become
  invisible.
- **Worktree mode is strictly less safe than the local-diff mode.** It gives
  panelists write/exec access in their worktree and shares your parent repo's
  `.git` objects, so a stray `git push` from a panelist would publish from your
  machine. The prompt forbids it, but the prompt is a firewall, not a sandbox.
- **Diff-embed targets cap at 200KB.** `--base` / `--commit` reviews of big
  rename / refactor changes blow past it — bump `PANEL_REVIEW_MAX_DIFF_BYTES`
  rather than trimming. PR mode bypasses this cap entirely (no embedded diff).
- Panelists pick up the project's `AGENTS.md` / `CLAUDE.md` — that's
  intentional, but worth knowing if the file biases their review.
