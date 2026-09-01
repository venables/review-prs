#!/usr/bin/env bash
# board: the live board under a terminal. Every other file in the suite runs
# the binaries through a pipe, where the board never draws; this one gives
# autoreview a pty that answers like a terminal (tests/pty.py), resizes it
# mid-pass, presses a key, and checks that the terminal comes back cooked.

set -euo pipefail
# shellcheck source=helpers.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/helpers.sh"

echo "board"
if ! command -v python3 >/dev/null 2>&1; then
  echo "  skip  python3 not installed; nothing here can hold a pty"
  finish
  exit 0
fi
setup_sandbox
trap teardown_sandbox EXIT

# Run autoreview under the pty driver, through a shell that prints the exit
# status and the terminal's settings afterwards -- from inside the session,
# because the pty is revoked on macOS the moment its session leader exits.
# The driver's own arguments come first; autoreview's follow the --.
run_on_pty() {
  reset_spawn_log
  rm -rf "$SANDBOX/out/logs"
  set +e
  python3 "$TESTS_DIR/pty.py" --timeout 30 --cols 100 --rows 30 --out "$SANDBOX/out/pty" "$@" -- \
    bash -c 'cd "$1" && TERM=xterm-256color "$2" --log-dir "$3"; echo "autoreview-exit=$?"; stty -a' \
    _ "$SANDBOX/repo" "$AUTOREVIEW" "$SANDBOX/out/logs"
  set -e
  cat "$SANDBOX/out/pty"
}

# The terminal's line settings after the run, from the stty -a the wrapper
# printed. A flag reads "icanon" when set and "-icanon" when not, on both
# BSD and GNU stty.
tty_is_cooked() {
  local settings
  settings="$(tail -c 2000 "$SANDBOX/out/pty" | tr '\r' '\n')"
  [[ "$settings" == *" icanon"* && "$settings" != *"-icanon"* ]] || return 1
  [[ "$settings" == *" echo "* || "$settings" == *" echo"$'\n'* ]] || return 1
  [[ "$settings" != *" -echo "* ]]
}

# --- A pass to the end, with a resize in the middle --------------------------
# Two reviews of two seconds each; at 0.7s the terminal shrinks from 100 to
# 40 columns. The rows are rebuilt for the new width: a title that fit at 100
# is cut at 40, and nothing else on that screen is ever cut, so an ellipsis
# is the proof that the resize was noticed.
out="$(FAKE_CLAUDE_SLEEP=2 run_on_pty --resize 0.7:40x20)"
assert_contains "a pass on a terminal exits 0" "$out" "autoreview-exit=0"
assert_contains "the board drew its spinner" "$out" "⠋"
assert_contains "a finished review leaves a result line" "$out" "✓"
assert_contains "...that names the PR" "$out" "#9"
assert_contains "a resize refits the rows to the new width" "$out" "…"
assert_contains "the summary follows the board" "$out" "╭"
if tty_is_cooked; then
  ok "the terminal is given back cooked"
else
  not_ok "the terminal is given back cooked" "stty -a after the run: $(tail -c 300 "$SANDBOX/out/pty" | tr '\r\n' '  ')"
fi

# --- space shows what a review is doing, q stops the pass -------------------
# The fake dash-p writes #9's transcript as it starts: a line of text and a
# Bash call, the shape Claude Code writes as a reviewer works. #8 gets none.
# At 0.8s a space expands every running row: #9's block names its session
# and what it did, #8's says it is still waiting for a transcript, and the
# footer's hint flips to the key that hides them again. At 1.6s q ends the
# pass.
#
# With the board up the terminal is in raw mode, so ctrl-C is a key; q is the
# same key by another name. Both take the interrupt path: stop every review's
# process group, print the summary, exit 130, and give the terminal back.
out="$(FAKE_CLAUDE_TRANSCRIPT=9 FAKE_CLAUDE_SLEEP=5 run_on_pty --key '0.8: ' --key 1.6:q)"
assert_contains "a running row names the tool the review is in" "$out" "· Bash"
assert_contains "space expands a running row" "$out" "2 turns · 1 tool call"
# One word at a time: the board writes only the cells that changed, and a
# plain space over a blank cell is not a change, so an unstyled sentence
# reaches the terminal as its words with cursor moves between them.
assert_contains "...with what the reviewer said" "$out" "Reading"
assert_contains "...to the end of the line" "$out" "diff."
assert_contains "...and what it ran" "$out" "--quiet"
assert_contains "...and which session it runs in" "$out" "session "
assert_contains "a review with no transcript yet says so" "$out" "waiting for its transcript"
assert_contains "...and the footer offers to hide the details" "$out" "hide · q stop"
assert_contains "q interrupts the pass" "$out" "interrupted; stopping running reviews"
assert_contains "...and exits 130" "$out" "autoreview-exit=130"
assert_contains "...after printing the summary" "$out" "╭"
if tty_is_cooked; then
  ok "the terminal is given back after an interrupt"
else
  not_ok "the terminal is given back after an interrupt" "stty -a after the run: $(tail -c 300 "$SANDBOX/out/pty" | tr '\r\n' '  ')"
fi
sleep 0.5
if pgrep -f "$FAKE_SLEEP_TAG" >/dev/null 2>&1; then
  not_ok "the interrupted reviews are stopped" "a reviewer's child survived the interrupt"
  pkill -f "$FAKE_SLEEP_TAG" >/dev/null 2>&1 || true
else
  ok "the interrupted reviews are stopped"
fi

finish
