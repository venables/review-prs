# Skills are vendored and versioned with the binaries

Recorded: 2026-09-01
Status: accepted

## Context

The reviewer each agent runs is a set of skills invoked by slash name. A flag
the binary passes down is a flag the skill has to understand, so a skill
installed from somewhere else can be a version behind the binary that calls
it. The skills lived in a separate repo.

## Decision

The skills live under `skills/` in this repo, vendored byte for byte, and the
suite lints their shell scripts. They install with `npx skills add` or a
symlink into a skills directory. They are not a Claude plugin.

Open PRs #15 and #16 go one step further: embed the skills in the binaries,
stage them per run, and pass `--add-dir=` to claude, with a `--skills` flag
to point elsewhere. The constraints below are what that design rests on.

## Constraints verified on 2026-09-01

Against claude 2.1.252 and dash-p 0.4.0, with marker skills:

- A user skill in `~/.claude/skills/NAME` wins over the same name from
  `--add-dir DIR/.claude/skills` and over the cwd repo's `.claude/skills`.
- The cwd repo's `.claude/skills/NAME` also wins over `--add-dir`.
- `--setting-sources project,local` makes the `--add-dir` copy win, but drops
  the user's settings.json (model, effort, hooks, plugins). `--settings` would
  add those back, but dash-p rejects it because it injects its own for a Stop
  hook.
- `--bare` breaks OAuth. API key only.
- `claude --add-dir` is variadic. `--add-dir DIR "prompt"` eats the prompt.
  Use `--add-dir=DIR`.
- dash-p forwards unknown flags only in single-token `--flag=value` form.
- `include_dir!` on stable tracks edits to embedded files but not files added
  or removed. A `build.rs` with `rerun-if-changed=skills` covers that.

## Consequences

- An installed copy of a skill shadows the bundled one. That is the override
  mechanism, and the binaries print a note when it happens. Do not try to
  force the bundled copy through setting sources.
- A change to a skill and the flag that reaches it ship in one PR.
