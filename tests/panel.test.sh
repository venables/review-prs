#!/usr/bin/env bash
# panel: the fan-out, the retry, the failure reporting, and the worktrees.
#
# Its own sandbox rather than helpers.sh's: panel talks to dash-p and git and
# never to gh, so the shared fakes (a gh that answers GraphQL, a dash-p that
# writes meta envelopes) are the wrong shape here.

set -euo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$TESTS_DIR/.." && pwd)"
PANEL="${PANEL:-$ROOT/target/debug/panel}"

pass_count=0
fail_count=0

ok() { printf '  ok    %s\n' "$1"; pass_count=$((pass_count + 1)); }
not_ok() {
  printf '  FAIL  %s\n' "$1"
  printf '        %s\n' "$2"
  fail_count=$((fail_count + 1))
}
assert_contains() {
  if [[ "$2" == *"$3"* ]]; then ok "$1"; else not_ok "$1" "expected to find: $3"; fi
}
assert_not_contains() {
  if [[ "$2" != *"$3"* ]]; then ok "$1"; else not_ok "$1" "expected NOT to find: $3"; fi
}
assert_equals() {
  if [[ "$2" == "$3" ]]; then ok "$1"; else not_ok "$1" "expected: $3
        actual:   $2"; fi
}

echo "panel"

SANDBOX="$(mktemp -d)"
trap 'rm -rf "$SANDBOX"' EXIT
mkdir -p "$SANDBOX/bin" "$SANDBOX/repo" "$SANDBOX/out"

# A repo with a commit on main and a branch ahead of it, so --base main has a
# diff. An explicit ident: a CI runner has none, and Linux git refuses to
# synthesize one.
git -C "$SANDBOX/repo" init -q -b main
git -C "$SANDBOX/repo" -c user.name=t -c user.email=t@e.com commit -q --allow-empty -m init
git -C "$SANDBOX/repo" checkout -q -b feature
printf 'fn main() {}\n' >"$SANDBOX/repo/a.rs"
git -C "$SANDBOX/repo" add a.rs
git -C "$SANDBOX/repo" -c user.name=t -c user.email=t@e.com commit -q -m "add a.rs"

# A fake dash-p standing in for every backend. It records its argv and its
# stdin, and the FAKE_* knobs make it behave like each failure the real one
# has: nothing at all, nothing then something on the retry, or a hang.
cat >"$SANDBOX/bin/dash-p" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
backend=""
prev=""
for arg in "$@"; do
  case "$prev" in -H) backend="$arg" ;; esac
  prev="$arg"
done
printf '%s\n' "$*" >>"$SANDBOX/out/calls"
# The prompt arrives on stdin, never in argv -- keep a copy so a test can
# check what the panelist was actually asked.
cat >"$SANDBOX/out/stdin-$backend-$$"
attempt="$(cat "$SANDBOX/out/tries-$backend" 2>/dev/null || echo 0)"
attempt=$((attempt + 1))
printf '%s' "$attempt" >"$SANDBOX/out/tries-$backend"

# The synthesis call: a claude with no --dangerously-skip-permissions and a
# prompt that names the synthesis instructions.
if grep -q "Panel synthesis request" "$SANDBOX/out/stdin-$backend-$$"; then
  printf '%s\n' "$SANDBOX/out/stdin-$backend-$$" >"$SANDBOX/out/synthesis-prompt-path"
  printf '### Overview\n\n**Reviewing:** synthesized\n'
  exit 0
fi

case " ${FAKE_EMPTY:-} " in
  *" $backend "*)
    # Empty on the first attempt, a real review on the second.
    if [[ "$attempt" -eq 1 ]]; then echo "transient blip" >&2; exit 1; fi
    ;;
esac
case " ${FAKE_ALWAYS_EMPTY:-} " in
  *" $backend "*) echo "quota exceeded" >&2; exit 1 ;;
esac
case " ${FAKE_HANG:-} " in
  *" $backend "*) sleep 120; exit 0 ;;
esac
case " ${FAKE_DIRTY_EXIT:-} " in
  *" $backend "*)
    printf 'Model: %s-model\nGoal (clear): x\n- [LOW] a.rs:1 — nit\n' "$backend"
    exit 3
    ;;
esac
printf 'Model: %s-model\nGoal (clear): x\nApproach (sound): y\n- [LOW] a.rs:1 — nit\n' "$backend"
EOF
chmod +x "$SANDBOX/bin/dash-p"

# Backends only have to exist on PATH; the fake dash-p is what actually runs.
for b in codex claude opencode; do
  printf '#!/usr/bin/env bash\nexit 0\n' >"$SANDBOX/bin/$b"
  chmod +x "$SANDBOX/bin/$b"
done

export PATH="$SANDBOX/bin:$PATH"
export SANDBOX

run_panel() {
  : >"$SANDBOX/out/calls"
  rm -f "$SANDBOX"/out/tries-* "$SANDBOX"/out/stdin-* 2>/dev/null || true
  set +e
  ( cd "$SANDBOX/repo" && "$PANEL" --log-dir "$SANDBOX/out/logs" "$@" 2>&1 )
  local status=$?
  set -e
  printf '%s' "$status" >"$SANDBOX/out/status"
}
last_status() { cat "$SANDBOX/out/status" 2>/dev/null || printf 'unset'; }
calls() { cat "$SANDBOX/out/calls" 2>/dev/null || true; }

# --- The ordinary run ------------------------------------------------------
out="$(run_panel --base main)"
assert_equals "a clean panel exits 0" "$(last_status)" "0"
assert_contains "every backend on PATH is a panelist" "$out" "Panelists: codex, claude, opencode"
assert_contains "each report is printed" "$out" "## codex / codex-model (exit 0)"
assert_contains "...for every panelist" "$out" "## opencode / opencode-model (exit 0)"
assert_contains "the findings come through" "$out" "[LOW] a.rs:1 — nit"
assert_contains "the synthesis runs" "$out" "# Synthesis"
assert_contains "...and its output is printed" "$out" "**Reviewing:** synthesized"
assert_contains "the roster is reported" "$out" "3 of 3 answered"

# The prompt must never ride in argv: a large diff would fail the exec on
# Linux with E2BIG, and macOS would never show it.
assert_not_contains "the prompt is not an argument" "$(calls)" "Code review request"
assert_contains "the diff target reaches the panelist" \
  "$(cat "$SANDBOX"/out/stdin-codex-* 2>/dev/null)" "1 commit on"

# --- Isolation and permissions --------------------------------------------
assert_contains "a committed target gets write/exec in a worktree" \
  "$(calls)" "--dangerously-skip-permissions"
assert_contains "...and runs in that worktree" "$(calls)" "worktree-codex"
assert_equals "no worktree is left behind" \
  "$(git -C "$SANDBOX/repo" worktree list | wc -l | tr -d ' ')" "1"

printf 'fn other() {}\n' >>"$SANDBOX/repo/a.rs"
out="$(run_panel --uncommitted)"
assert_contains "the working tree is read-only" "$(calls)" "--perms read-only"
assert_not_contains "...with no exec" "$(calls)" "--dangerously-skip-permissions"
assert_equals "no worktree is made for uncommitted work" \
  "$(git -C "$SANDBOX/repo" worktree list | wc -l | tr -d ' ')" "1"
git -C "$SANDBOX/repo" checkout -q -- a.rs

# --- Retry -----------------------------------------------------------------
out="$(FAKE_EMPTY="codex" run_panel --base main)"
assert_contains "a panelist that returns nothing is retried" "$out" "trying once more"
assert_contains "...and its second attempt is reported" "$out" "## codex / codex-model (exit 0)"
assert_equals "...so the run is still whole" "$(last_status)" "0"

out="$(FAKE_ALWAYS_EMPTY="codex" run_panel --base main)"
assert_contains "a panelist that never answers is reported" "$out" "FAILED:"
assert_contains "...naming what went wrong" "$out" "quota exceeded"
assert_contains "...and the rest of the panel still runs" "$out" "## claude / claude-model"
assert_contains "...and the roster says how many answered" "$out" "2 of 3 answered"
assert_equals "...without failing the run" "$(last_status)" "0"

# --- A review that arrives with a bad exit ---------------------------------
out="$(FAKE_DIRTY_EXIT="codex" run_panel --base main)"
assert_contains "a report with a bad exit is still counted" "$out" "3 of 3 answered"
assert_contains "...and still printed" "$out" "[LOW] a.rs:1 — nit"
assert_contains "...while saying the run was not clean" "$out" "exited 3 after producing output"
synth="$(cat "$(cat "$SANDBOX/out/synthesis-prompt-path" 2>/dev/null)" 2>/dev/null || true)"
assert_contains "the synthesizer is told to count it" "$synth" "did not exit cleanly"
assert_not_contains "...and not to discount it" "$synth" "codex (codex-model): exited 3 after producing output
- "

# --- Every panelist failing -------------------------------------------------
out="$(FAKE_ALWAYS_EMPTY="codex claude opencode" run_panel --base main)"
assert_equals "a panel where nobody answered exits 1" "$(last_status)" "1"
assert_contains "...and says there is nothing to synthesize" "$out" "nothing to synthesize"
assert_equals "...leaving no worktree behind" \
  "$(git -C "$SANDBOX/repo" worktree list | wc -l | tr -d ' ')" "1"

# --- The timeout guard ------------------------------------------------------
# A panelist that ignores its own timeout must not hang the run: the poll loop
# has its own deadline and kills the process group. Bounded by the test itself,
# so a regression fails rather than hangs.
start=$SECONDS
out="$(FAKE_HANG="codex" run_panel --base main --timeout 1)"
elapsed=$((SECONDS - start))
assert_contains "a wedged panelist is killed" "$out" "timed out and had to be killed"
if [[ "$elapsed" -lt 60 ]]; then
  ok "...rather than hanging the run (${elapsed}s)"
else
  not_ok "...rather than hanging the run" "took ${elapsed}s"
fi
assert_contains "...and the rest of the panel is still reported" "$out" "## claude / claude-model"
sleep 0.5
survivors="$(pgrep -f "$SANDBOX/bin/dash-p" 2>/dev/null | wc -l | tr -d ' ' || true)"
assert_equals "...leaving no process behind" "$survivors" "0"
assert_equals "...and no worktree" \
  "$(git -C "$SANDBOX/repo" worktree list | wc -l | tr -d ' ')" "1"

# --- Nothing to review ------------------------------------------------------
out="$(run_panel --staged)"
assert_equals "nothing staged exits 1" "$(last_status)" "1"
assert_contains "...and says why" "$out" "nothing to review"

printf 'untracked\n' >"$SANDBOX/repo/new-file.txt"
out="$(run_panel --staged)"
assert_contains "an untracked-only change names the files" "$out" "new-file.txt"
assert_contains "...and says how to include them" "$out" "git add"
rm -f "$SANDBOX/repo/new-file.txt"

# --- Bad input --------------------------------------------------------------
out="$(run_panel --base "--output=/tmp/pwned")"
assert_equals "a base that is really a git option is refused" "$(last_status)" "1"
assert_contains "...saying so" "$out" "starts with a dash"

out="$(run_panel --base no-such-ref)"
assert_equals "a base that names no commit is refused" "$(last_status)" "1"
assert_contains "...saying so" "$out" "no commit named"

printf '\n%d passed, %d failed\n' "$pass_count" "$fail_count"
[[ "$fail_count" -eq 0 ]]
