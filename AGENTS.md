# CRITICAL: read first

- Plain output strings are a test contract. The bash suite greps them
  verbatim, so a wording change ships with its test change in the same commit.
- Run `bash tests/run.sh` before you call work done. It builds, runs
  `cargo test`, then runs the bash suite against fake `gh`, `gum`, `cmux` and
  `dash-p` binaries in a sandbox repo.
- The VERDICT column is read back from GitHub. The agent's own trailer is a
  fallback only. Keep that asymmetry in any code that reports what landed on
  a PR.

## Commands

```sh
cargo build                     # all three binaries into target/debug
cargo test --quiet              # unit tests: wording, arithmetic and parsing pins
bash tests/run.sh               # the whole suite: build, cargo test, bash tests, shellcheck
bash tests/autoreview.test.sh   # one file, after a cargo build
cargo clippy --all-targets      # clean today; keep it clean
```

## Facts

- One crate, Rust 2024 edition, three binaries over one library:
  `autoreview` (headless pool), `review-prs` (one terminal tab per PR) and
  `panel` (one diff, several models).
- Every built-in review runs through `dash-p`, which owns the timeout and
  exits 0 ok, 10 agent-error, 20 timeout.
- `cargo fmt` is not applied to this tree. Clippy is clean.
- The test suite is bash 3.2 compatible because macOS ships 3.2. An empty
  array under `set -u` needs the `${arr[@]+"${arr[@]}"}` guard.
- The suite runs the binaries through pipes, where the board never draws.
  `tests/board.test.sh` is the exception: it gives autoreview a pty through
  `tests/pty.py`, which answers the cursor query that `script(1)` does not.
- The skills under `skills/` are the reviewers the binaries invoke by slash
  name. They are versioned with the binaries.
- Design decisions live in `docs/decisions/`, one file each, with an index in
  its README. Add one in the same PR as the decision it records.

## Conventions

- Argument parsers are hand-rolled. Error strings are byte-exact and bad input
  exits 1. Extend the match and the HELP text; do not add clap.
- Pass flags to `dash-p` in single-token `--flag=value` form. It forwards
  unknown flags only that way, and a silently dropped `--max-budget-usd` is
  the failure the flag exists to prevent.
- Count in English through `ui::count`. "1 PR(s)" is the shape to avoid.
- Board rows carry a plain `#N` label. OSC 8 hyperlinks belong in the summary
  tables only: the board is measured and redrawn in place, and the summary is
  the one place a number links.
- While the board is open the terminal is in raw mode. Print through
  `ui.note`; a bare `println!` lands inside the live area.
- Read crossterm events on the main thread, through the board. A reader
  thread holds the lock the cursor query needs on every resize.
- Progress goes to stderr and the report to stdout. Off a TTY, progress is one
  plain line per step and a ticking message says nothing.
- Doc comments state the failure the code prevents, in prose. Match that
  voice.
- Commit messages are conventional (`feat:`, `fix:`, `docs:`, `test:`,
  `chore:`), one logical change each, written in plain words.

## Working here

- Stop a background test run by its pid. A broad `pkill -f` on a name that
  also appears in a panelist's argv has killed live panelists mid-review.
- The fake `gh` in `tests/helpers.sh` dispatches on exact argv shapes. A new
  `gh` call needs a matching fake before its test can pass.
- The session id derivation in `src/session.rs` is pinned by golden tests.
  Changing it orphans every review session already on disk.
- A PR title is other people's text. Strip control bytes and bidi marks before
  it reaches the terminal (`report::sanitize_for_display`).

# CRITICAL: read last

- Plain output strings are a test contract. Change the test in the same
  commit as the wording.
- Run `bash tests/run.sh` before you call work done.
- The VERDICT column is read back from GitHub, never taken from the agent.
