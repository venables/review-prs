#!/usr/bin/env bash
# Shared harness for the review-prs and autoreview tests.
#
# Each test runs the real binaries against fake `gh`, `gum`, `cmux` and `dash-p`
# binaries on PATH, inside a throwaway git repo, with $CLAUDE_CONFIG_DIR
# pointed at a throwaway session store. Nothing here touches your real repos,
# your real Claude Code sessions, or GitHub.

set -euo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Both binaries; tests/run.sh builds them and exports the paths, the defaults
# cover running one test file by hand after a `cargo build`.
REVIEW_PRS="${REVIEW_PRS:-$TESTS_DIR/../target/debug/review-prs}"
AUTOREVIEW="${AUTOREVIEW:-$TESTS_DIR/../target/debug/autoreview}"

pass_count=0
fail_count=0

ok() {
  printf '  ok    %s\n' "$1"
  pass_count=$((pass_count + 1))
}

not_ok() {
  printf '  FAIL  %s\n' "$1"
  printf '        %s\n' "$2"
  fail_count=$((fail_count + 1))
}

assert_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if [[ "$haystack" == *"$needle"* ]]; then
    ok "$desc"
  else
    not_ok "$desc" "expected to find: $needle"
    printf '        in: %s\n' "$haystack"
  fi
}

assert_not_contains() {
  local desc="$1" haystack="$2" needle="$3"
  if [[ "$haystack" != *"$needle"* ]]; then
    ok "$desc"
  else
    not_ok "$desc" "expected NOT to find: $needle"
    printf '        in: %s\n' "$haystack"
  fi
}

assert_equals() {
  local desc="$1" actual="$2" expected="$3"
  if [[ "$actual" == "$expected" ]]; then
    ok "$desc"
  else
    not_ok "$desc" "expected: $expected"
    printf '        actual:   %s\n' "$actual"
  fi
}

# --- Sandbox --------------------------------------------------------------

setup_sandbox() {
  SANDBOX="$(mktemp -d)"
  export SANDBOX
  mkdir -p "$SANDBOX/bin" "$SANDBOX/repo" "$SANDBOX/claude/projects" \
    "$SANDBOX/fixtures" "$SANDBOX/out"

  git -C "$SANDBOX/repo" init -q
  # An explicit ident: a CI runner has none configured, and Linux git refuses
  # to synthesize one (macOS quietly does, which is why this only ever failed
  # on the Ubuntu leg).
  git -C "$SANDBOX/repo" -c user.name=test -c user.email=test@example.com \
    commit -q --allow-empty -m init

  write_fake_gh
  write_fake_gum
  write_fake_cmux
  write_fake_dashp
  write_fake_override

  export PATH="$SANDBOX/bin:$PATH"
  export CLAUDE_CONFIG_DIR="$SANDBOX/claude"
  export FAKE_GH_LOGIN="me"
  export SPAWN_LOG="$SANDBOX/out/spawned"
  export CLAUDE_LOG="$SANDBOX/out/claude-calls"

  # The name the fake dash-p gives its sleeping grandchild, which the timeout
  # test looks for with `pgrep -f`. pgrep searches every process on the box, so
  # a fixed marker would also match anything that merely mentions it -- an
  # editor holding this file open, a grep, an agent reading this diff -- and
  # fail the test on a machine where nothing leaked. The sandbox name makes it
  # unique per run, and it exists nowhere on disk.
  FAKE_SLEEP_TAG="fake-claude-sleep-$(basename "$SANDBOX")"
  export FAKE_SLEEP_TAG

  # Force the cmux spawner: it is the only one that is fully scriptable. Herdr
  # detection and the Ghostty AppleScript path are out of scope here.
  export CMUX_SURFACE_ID="test-surface"
  unset HERDR_ENV TERM_PROGRAM REVIEW_PRS_CMD REVIEW_PRS_AUTO_CMD || true
  unset AUTOREVIEW_CMD AUTOREVIEW_AUTO_CMD AUTOREVIEW_JOBS AUTOREVIEW_TIMEOUT \
        AUTOREVIEW_MAX_BUDGET_USD AUTOREVIEW_LOG_DIR \
        AUTOREVIEW_BABYSIT_INTERVAL AUTOREVIEW_MAX_PASSES \
        AUTOREVIEW_MAX_IDLE || true
  unset FAKE_CLAUDE_FAIL FAKE_CLAUDE_IS_ERROR FAKE_CLAUDE_SLEEP \
        FAKE_CLAUDE_GARBAGE FAKE_CLAUDE_KILL_JOB FAKE_CLAUDE_TRAILER \
        FAKE_GH_APPROVED FAKE_GH_CLOSED FAKE_GH_MY_REVIEW \
        FAKE_GH_VIEW_FAIL FAKE_GH_GRAPHQL_FAIL_AFTER || true
  # The host may have a real dash-p and an inherited override for it; the
  # sandbox must only ever see its fake on PATH.
  unset DASHP_BIN || true
  # Leave the workspace title alone; the fake cmux ignores it either way.
  export REVIEW_PRS_WORKSPACE=""

  default_prs
}

teardown_sandbox() {
  [[ -n "${SANDBOX:-}" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"
}

reset_spawn_log() {
  # A fake dash-p killed between mkdir and rmdir leaves its lock directory
  # behind, and every later log_line in this sandbox would then wait out the
  # full timeout before writing. The babysit tests kill runs by design.
  rmdir "$CLAUDE_LOG.lock" "$CLAUDE_LOG.events.lock" 2>/dev/null || true
  rm -f "$SANDBOX/out/graphql-calls"
  : >"$SPAWN_LOG"
  : >"$SPAWN_LOG.labels"
  : >"$SANDBOX/out/header"
  : >"$CLAUDE_LOG"
  : >"$CLAUDE_LOG.events"
  : >"$SANDBOX/out/override"
}

# The command the script sent to the tab for PR $1 (first match wins).
spawned_cmd() {
  grep -m1 -- "\b$1\b" "$SPAWN_LOG" 2>/dev/null || true
}

spawned_labels() {
  cat "$SPAWN_LOG.labels" 2>/dev/null || true
}

picker_header() {
  cat "$SANDBOX/out/header" 2>/dev/null || true
}

# Every argument list the fake claude was invoked with, one call per line.
claude_calls() {
  cat "$CLAUDE_LOG" 2>/dev/null || true
}

# The one call whose arguments contain $1 (first match wins).
claude_call_for() {
  grep -m1 -F -- "$1" "$CLAUDE_LOG" 2>/dev/null || true
}

# Start/end markers in call order, for asserting how much ran at once.
claude_events() {
  cat "$CLAUDE_LOG.events" 2>/dev/null || true
}

# What the fake override binary saw: its arguments and the session environment.
override_calls() {
  cat "$SANDBOX/out/override" 2>/dev/null || true
}

# --- Fakes ----------------------------------------------------------------

write_fake_gh() {
  cat >"$SANDBOX/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
sub="${1:-}"; shift || true
case "$sub" in
  repo)
    cat "$SANDBOX/fixtures/repo.json"
    ;;
  api)
    target="${1:-}"; shift || true
    jq_filter=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --jq) jq_filter="${2:-}"; shift 2 ;;
        *)    shift ;;
      esac
    done
    case "$target" in
      user)
        printf '%s\n' "${FAKE_GH_LOGIN:-me}"
        ;;
      graphql)
        # $FAKE_GH_GRAPHQL_FAIL_AFTER=N lets the first N calls through and
        # fails the rest: the initial selection succeeds and the babysit
        # refresh does not, which is the shape a transient API error has.
        calls="$SANDBOX/out/graphql-calls"
        seen="$(cat "$calls" 2>/dev/null || printf '0')"
        seen=$((seen + 1))
        printf '%s' "$seen" >"$calls"
        if [[ -n "${FAKE_GH_GRAPHQL_FAIL_AFTER:-}" && "$seen" -gt "$FAKE_GH_GRAPHQL_FAIL_AFTER" ]]; then
          echo "fake gh: the PR list is unavailable" >&2
          exit 1
        fi
        if [[ -n "$jq_filter" ]]; then
          jq -r "$jq_filter" "$SANDBOX/fixtures/prs.json"
        else
          cat "$SANDBOX/fixtures/prs.json"
        fi
        ;;
      *)
        echo "fake gh: unhandled api target: $target" >&2
        exit 1
        ;;
    esac
    ;;
  pr)
    # `gh pr view N --json ...`: PRs named in $FAKE_GH_APPROVED read as
    # approved and those in $FAKE_GH_CLOSED as closed; everything else is an
    # open PR with no decision yet. The shapes asked for are a bare decision
    # (--jq), the state+decision object, and the latestReviews list -- the
    # last driven by $FAKE_GH_MY_REVIEW ("9:APPROVED 8:COMMENTED"), stamped
    # now so it reads as this run's review.
    shift || true
    num="${1:-}"
    # A gh that cannot answer at all -- rate limit, expired auth, network.
    [[ "${FAKE_GH_VIEW_FAIL:-0}" == "1" ]] && exit 1
    decision=""
    state="OPEN"
    case " ${FAKE_GH_APPROVED:-} " in
      *" $num "*) decision="APPROVED" ;;
    esac
    case " ${FAKE_GH_CLOSED:-} " in
      *" $num "*) state="CLOSED" ;;
    esac
    case " $* " in
      *" latestReviews "*)
        mystate=""
        for entry in ${FAKE_GH_MY_REVIEW:-}; do
          case "$entry" in
            "$num:"*) mystate="${entry#*:}" ;;
          esac
        done
        if [[ -n "$mystate" ]]; then
          printf '{"latestReviews":[{"author":{"login":"%s"},"state":"%s","submittedAt":"%s"}]}\n' \
            "${FAKE_GH_LOGIN:-me}" "$mystate" "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
        else
          printf '{"latestReviews":[]}\n'
        fi
        ;;
      *" --jq "*) printf '%s\n' "$decision" ;;
      *) printf '{"state":"%s","reviewDecision":"%s"}\n' "$state" "$decision" ;;
    esac
    ;;
  *)
    echo "fake gh: unhandled subcommand: $sub" >&2
    exit 1
    ;;
esac
EOF
  chmod +x "$SANDBOX/bin/gh"
}

write_fake_gum() {
  cat >"$SANDBOX/bin/gum" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
sub="${1:-}"; shift || true
case "$sub" in
  style)
    # Echo the text argument so the caller can embed it.
    printf '%s\n' "${@: -1}"
    ;;
  choose)
    header=""
    while [[ $# -gt 0 ]]; do
      case "$1" in
        --header) header="${2:-}"; shift 2 ;;
        *)        shift ;;
      esac
    done
    printf '%s\n' "$header" >"$SANDBOX/out/header"
    # Select the row the test asked for, else nothing.
    if [[ -n "${FAKE_GUM_PICK:-}" ]]; then
      grep -F -- "$FAKE_GUM_PICK" || true
    else
      cat >/dev/null
    fi
    ;;
  *)
    echo "fake gum: unhandled subcommand: $sub" >&2
    exit 1
    ;;
esac
EOF
  chmod +x "$SANDBOX/bin/gum"
}

write_fake_cmux() {
  cat >"$SANDBOX/bin/cmux" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
sub="${1:-}"; shift || true
case "$sub" in
  new-surface)
    echo "surface:1"
    ;;
  rename-tab)
    while [[ $# -gt 1 && "$1" == --* ]]; do shift 2; done
    printf '%s\n' "${1:-}" >>"$SPAWN_LOG.labels"
    ;;
  send)
    [[ "${FAKE_CMUX_FAIL_SEND:-0}" == "1" ]] && exit 1
    while [[ $# -gt 1 && "$1" == --* ]]; do shift 2; done
    printf '%s\n' "${1:-}" >>"$SPAWN_LOG"
    ;;
  send-key|workspace-action)
    :
    ;;
  *)
    echo "fake cmux: unhandled subcommand: $sub" >&2
    exit 1
    ;;
esac
EOF
  chmod +x "$SANDBOX/bin/cmux"
}

write_fake_dashp() {
  cat >"$SANDBOX/bin/dash-p" <<'EOF'
#!/usr/bin/env bash

# Append one line to a shared log, atomically.
#
# --jobs 2 means two of these run at once, and each call line carries the whole
# argv including the ~900-character trailer instruction. A plain `>>` of that
# from two processes can split mid-write, which leaves a fragment in the log
# and fails whichever assertion greps for a flag that landed in the other half
# -- intermittently, which is the worst way to fail. mkdir is atomic on every
# filesystem that matters, so it is the lock.
log_line() {
  local file="$1" line="$2" waited=0 held=0
  while [[ "$waited" -le 500 ]]; do
    if mkdir "$file.lock" 2>/dev/null; then
      held=1
      break
    fi
    sleep 0.01
    waited=$((waited + 1))
  done
  printf '%s\n' "$line" >>"$file"
  # Only if this process took it: on the timeout path the lock belongs to
  # someone else, and removing it would hand the log to two writers at once.
  [[ "$held" -eq 1 ]] && rmdir "$file.lock" 2>/dev/null
  return 0
}
# Stands in for dash-p, which drives claude for the built-in reviewer: records
# the call, writes the meta envelope, and reports failures the way the real
# one does -- in its exit code (0 ok, 10 agent-error), with the session id in
# the envelope. The FAKE_CLAUDE_* knobs keep their names; each maps its old
# scenario onto the dash-p contract.
set -uo pipefail

# The prompt is the last argument, and ends with the PR number.
prompt="${!#}"
n="${prompt##* }"

log_line "$CLAUDE_LOG" "$*"
log_line "$CLAUDE_LOG.events" "start $n"

meta=""
sid=""
prev=""
for arg in "$@"; do
  case "$prev" in
    --meta-file) meta="$arg" ;;
    --session-id|--resume) sid="$arg" ;;
  esac
  prev="$arg"
done
# The envelope reports the session the turn ran in: the pinned or resumed id
# when one was given, otherwise the fresh id claude would have allocated
# itself. That difference is what the caller has to notice.
if [[ -z "$sid" ]]; then
  sid="00000000-0000-5000-a000-0000000000$(printf '%02d' "$n")"
fi

if [[ -n "${FAKE_CLAUDE_SLEEP:-}" ]]; then
  # Sleep in a marked grandchild, so a test can find survivors by name and so
  # stopping this job has more than one process to stop -- which is what a
  # real reviewer's own subprocesses look like. The marker is unique per
  # sandbox: the test finds it with pgrep, which searches the whole box.
  # The subshell and the trailing ":" are both load-bearing: a single-command
  # `bash -c` body is exec'd in place, which would drop the tag from the
  # command line. --timeout is deliberately NOT honored here: the timeout
  # tests exercise autoreview's own guard and group kill, not the fake's
  # ability to exit.
  bash -c '( exec -a "$0-sleep" sleep "$1" ) ; :' \
    "$FAKE_SLEEP_TAG-$n" "$FAKE_CLAUDE_SLEEP"
fi

# Die with no result written -- what an OOM kill or a stray pkill does to a
# running review.
case " ${FAKE_CLAUDE_KILL_JOB:-} " in
  *" $n "*)
    kill -9 $$
    sleep 5
    ;;
esac

status=0
label="ok"
case " ${FAKE_CLAUDE_FAIL:-} " in
  *" $n "*) status=10; label="agent-error" ;;
esac
# An is_error turn IS exit 10 under dash-p; the knob keeps its name so the
# test scenarios keep their meaning.
case " ${FAKE_CLAUDE_IS_ERROR:-} " in
  *" $n "*) status=10; label="agent-error" ;;
esac
# Garbage claude output: dash-p exits 10 with an empty session id.
case " ${FAKE_CLAUDE_GARBAGE:-} " in
  *" $n "*) status=10; label="agent-error"; sid="" ;;
esac

log_line "$CLAUDE_LOG.events" "end $n"

if [[ -n "$meta" ]]; then
  printf '{"harness":"claude","drive":"print","exit_status":"%s","session_id":"%s","total_cost_usd":0.42,"num_turns":3,"duration_ms":10,"model_resolved":"claude-fable-5"}\n' \
    "$label" "$sid" >"$meta"
fi
# PRs named in $FAKE_CLAUDE_TRAILER end their answer with the fenced
# ```autoreview block a real reviewer is asked for via the system prompt.
# The \n and \" sequences are literal here: they are JSON escapes for the
# consumer to decode, not printf's to interpret.
trailer=""
case " ${FAKE_CLAUDE_TRAILER:-} " in
  *" $n "*)
    trailer='\n\n```autoreview\n{\"decision\":\"commented\",\"risk\":\"LOW\",\"findings\":{\"must_fix\":0,\"should_fix\":0,\"polish\":1},\"panel\":[{\"name\":\"codex\",\"model\":\"gpt-5.5\",\"ok\":true,\"findings\":1,\"top\":\"LOW\"},{\"name\":\"claude\",\"model\":\"claude-opus-4.7\",\"ok\":true,\"findings\":0}]}\n```'
    ;;
esac
if [[ "$status" -eq 0 ]]; then
  printf '{"answer":"reviewed %s%s","metadata":{"session_id":"%s","total_cost_usd":0.42}}\n' \
    "$n" "$trailer" "$sid"
fi
exit "$status"
EOF
  chmod +x "$SANDBOX/bin/dash-p"
}

# A stand-in for a user-supplied $AUTOREVIEW_CMD / $REVIEW_PRS_CMD.
write_fake_override() {
  cat >"$SANDBOX/bin/my-review" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
printf 'args=%s session=%s resume=%s\n' \
  "$*" "${REVIEW_PRS_SESSION_ID:-unset}" "${REVIEW_PRS_SESSION_RESUME:-unset}" \
  >>"$SANDBOX/out/override"
EOF
  chmod +x "$SANDBOX/bin/my-review"

  # An override that reports in prose rather than JSON -- the ordinary shape of
  # a hand-rolled reviewer, and the one that leaves no session id to read back.
  cat >"$SANDBOX/bin/text-review" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
printf 'reviewed PR %s, looks fine\n' "$1"
EOF
  chmod +x "$SANDBOX/bin/text-review"

  # A reviewer that refuses to stop on TERM, which is what makes --timeout's
  # escalation to KILL load-bearing: without it the run waits on it forever.
  cat >"$SANDBOX/bin/stubborn-review" <<'EOF'
#!/usr/bin/env bash
set -uo pipefail
trap '' TERM
( exec -a "$FAKE_SLEEP_TAG-stubborn" sleep 60 ) &
wait
EOF
  chmod +x "$SANDBOX/bin/stubborn-review"
}

# --- Fixtures -------------------------------------------------------------

# Three open PRs by other people, one draft, one of yours, one Dependabot.
# PR 9 and 8 are NEW (no engagement by "me"); 6 is SEEN (you commented last).
default_prs() {
  cat >"$SANDBOX/fixtures/repo.json" <<'EOF'
{"owner":{"login":"acme"},"name":"widgets"}
EOF

  cat >"$SANDBOX/fixtures/prs.json" <<'EOF'
{"data":{"repository":{"pullRequests":{"nodes":[
  {"number":9,"title":"Add retry logic","isDraft":false,
   "updatedAt":"2026-08-10T10:00:00Z","reviewDecision":null,
   "headRefOid":"sha9",
   "author":{"login":"alice"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-10T10:00:00Z","author":{"user":{"login":"alice"}}}}]}},

  {"number":8,"title":"Fix typo","isDraft":false,
   "updatedAt":"2026-08-09T10:00:00Z","reviewDecision":"CHANGES_REQUESTED",
   "headRefOid":"sha8",
   "author":{"login":"bob"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-09T10:00:00Z","author":{"user":{"login":"bob"}}}}]}},

  {"number":6,"title":"Refactor client","isDraft":false,
   "updatedAt":"2026-08-08T10:00:00Z","reviewDecision":null,
   "headRefOid":"sha6",
   "author":{"login":"carol"},
   "comments":{"nodes":[{"author":{"login":"me"},"updatedAt":"2026-08-08T10:00:00Z"}]},
   "reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-07T10:00:00Z","author":{"user":{"login":"carol"}}}}]}},

  {"number":5,"title":"Approved already","isDraft":false,
   "updatedAt":"2026-08-06T10:00:00Z","reviewDecision":"APPROVED",
   "headRefOid":"sha5",
   "author":{"login":"dave"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-06T10:00:00Z","author":{"user":{"login":"dave"}}}}]}},

  {"number":4,"title":"My own work","isDraft":false,
   "updatedAt":"2026-08-05T10:00:00Z","reviewDecision":null,
   "headRefOid":"sha4",
   "author":{"login":"me"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-05T10:00:00Z","author":{"user":{"login":"me"}}}}]}},

  {"number":3,"title":"Bump lodash","isDraft":false,
   "updatedAt":"2026-08-04T10:00:00Z","reviewDecision":null,
   "headRefOid":"sha3",
   "author":{"login":"dependabot"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-04T10:00:00Z","author":{"user":{"login":"dependabot"}}}}]}},

  {"number":2,"title":"Work in progress","isDraft":true,
   "updatedAt":"2026-08-03T10:00:00Z","reviewDecision":null,
   "headRefOid":"sha2",
   "author":{"login":"erin"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-03T10:00:00Z","author":{"user":{"login":"erin"}}}}]}}
]}}}}
EOF
}

# Run review-prs inside the sandbox repo. Stdout and stderr are combined and
# echoed, so callers usually wrap this in `$(...)` -- which runs it in a
# subshell, so the exit status goes to a file rather than a variable. Read it
# back with last_status.
run_review_prs() {
  reset_spawn_log
  set +e
  ( cd "$SANDBOX/repo" && "$REVIEW_PRS" "$@" 2>&1 )
  local status=$?
  set -e
  printf '%s' "$status" >"$SANDBOX/out/status"
}

last_status() {
  cat "$SANDBOX/out/status" 2>/dev/null || printf 'unset'
}

# Run autoreview inside the sandbox repo. Same contract as run_review_prs:
# combined output on stdout, exit status via last_status. Logs go somewhere the
# test can read rather than a temp directory.
run_autoreview() {
  reset_spawn_log
  rm -rf "$SANDBOX/out/logs"
  set +e
  ( cd "$SANDBOX/repo" && "$AUTOREVIEW" --log-dir "$SANDBOX/out/logs" "$@" 2>&1 )
  local status=$?
  set -e
  printf '%s' "$status" >"$SANDBOX/out/status"
}

# run_autoreview with a wall-clock bound in seconds ($1), for a run that is
# asserted to exit. Without the bound a regression that removes the exit hangs
# the whole suite rather than failing the one test. A run still alive at the
# limit records the status "timeout", which no assertion accepts.
run_autoreview_bounded() {
  local limit="$1"; shift
  reset_spawn_log
  set +e
  ( cd "$SANDBOX/repo" && "$AUTOREVIEW" --log-dir "$SANDBOX/out/logs" "$@" 2>&1 ) &
  local pid=$! waited=0
  while kill -0 "$pid" 2>/dev/null && [[ "$waited" -lt $((limit * 10)) ]]; do
    sleep 0.1
    waited=$((waited + 1))
  done
  if kill -0 "$pid" 2>/dev/null; then
    pkill -P "$pid" >/dev/null 2>&1
    kill "$pid" >/dev/null 2>&1
    wait "$pid" 2>/dev/null
    printf '%s' "timeout" >"$SANDBOX/out/status"
    set -e
    return
  fi
  wait "$pid"
  local status=$?
  set -e
  printf '%s' "$status" >"$SANDBOX/out/status"
}

# Run autoreview in the background, wait for $1 to appear in the file $3, up to
# $2 seconds, then kill it. For the babysit loop, whose whole point is that it
# does not exit -- the shortest interval it accepts is a minute, and no test
# should wait one.
run_autoreview_watching() {
  local needle="$1" limit="$2" watch="$3" want="$4"; shift 4
  local out="$SANDBOX/out/bg" waited=0 pid seen
  reset_spawn_log
  rm -rf "$SANDBOX/out/logs"
  : >"$out"
  ( cd "$SANDBOX/repo" && "$AUTOREVIEW" --log-dir "$SANDBOX/out/logs" "$@" >"$out" 2>&1 ) &
  pid=$!
  while [[ "$waited" -lt "$((limit * 10))" ]]; do
    # `grep -c` prints its count and still exits nonzero when that count is
    # zero, so a `|| printf 0` fallback would append a second line and make the
    # comparison below a syntax error rather than a false.
    seen="$(grep -cF -- "$needle" "$watch" 2>/dev/null || true)"
    [[ "$seen" =~ ^[0-9]+$ ]] || seen=0
    if [[ "$seen" -ge "$want" ]]; then break; fi
    if ! kill -0 "$pid" 2>/dev/null; then break; fi
    sleep 0.1
    waited=$((waited + 1))
  done
  pkill -P "$pid" >/dev/null 2>&1 || true
  kill "$pid" >/dev/null 2>&1 || true
  wait "$pid" 2>/dev/null || true
  cat "$out"
}

# Wait for something autoreview printed.
run_autoreview_until() {
  local needle="$1" limit="$2"; shift 2
  run_autoreview_watching "$needle" "$limit" "$SANDBOX/out/bg" 1 "$@"
}

# Wait for the $3'th call autoreview made matching $1. A header is printed
# before the work it announces, so waiting on one and then killing the run races
# whatever it was about to do; the recorded call is the event itself. The count
# is what makes a later pass distinguishable, since it repeats the same calls.
run_autoreview_until_call() {
  local needle="$1" limit="$2" want="${3:-1}"; shift 3
  run_autoreview_watching "$needle" "$limit" "$CLAUDE_LOG" "$want" "$@"
}

# Create the session file that makes $1 look like an existing session.
make_session() {
  mkdir -p "$CLAUDE_CONFIG_DIR/projects/-fake-project"
  touch "$CLAUDE_CONFIG_DIR/projects/-fake-project/$1.jsonl"
}

# Pull the session id out of a spawned command, whichever flag carries it.
session_id_from() {
  printf '%s\n' "$1" \
    | grep -oE -- '--(session-id|resume) [0-9a-f-]{36}' \
    | head -1 | awk '{print $2}'
}

finish() {
  printf '\n%d passed, %d failed\n' "$pass_count" "$fail_count"
  [[ "$fail_count" -eq 0 ]]
}
