#!/usr/bin/env bash
# Shared harness for the review-prs and autoreview tests.
#
# Each test runs the real script against fake `gh`, `gum`, `cmux` and `claude`
# binaries on PATH, inside a throwaway git repo, with $CLAUDE_CONFIG_DIR
# pointed at a throwaway session store. Nothing here touches your real repos,
# your real Claude Code sessions, or GitHub.

set -euo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REVIEW_PRS="$TESTS_DIR/../review-prs"
AUTOREVIEW="$TESTS_DIR/../autoreview"

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
  git -C "$SANDBOX/repo" commit -q --allow-empty -m init

  write_fake_gh
  write_fake_gum
  write_fake_cmux
  write_fake_claude
  write_fake_override

  export PATH="$SANDBOX/bin:$PATH"
  export CLAUDE_CONFIG_DIR="$SANDBOX/claude"
  export FAKE_GH_LOGIN="me"
  export SPAWN_LOG="$SANDBOX/out/spawned"
  export CLAUDE_LOG="$SANDBOX/out/claude-calls"

  # The name the fake claude gives its sleeping grandchild, which the timeout
  # test looks for with `pgrep -f`. pgrep searches every process on the box, so
  # a fixed marker would also match anything that merely mentions it -- an
  # editor holding this file open, a grep, an agent reading this diff -- and
  # fail the test on a machine where nothing leaked. The sandbox name makes it
  # unique per run, and it exists nowhere on disk.
  export FAKE_SLEEP_TAG="fake-claude-sleep-$(basename "$SANDBOX")"

  # Force the cmux spawner: it is the only one that is fully scriptable. Herdr
  # detection and the Ghostty AppleScript path are out of scope here.
  export CMUX_SURFACE_ID="test-surface"
  unset HERDR_ENV TERM_PROGRAM REVIEW_PRS_CMD REVIEW_PRS_AUTO_CMD || true
  unset AUTOREVIEW_CMD AUTOREVIEW_AUTO_CMD AUTOREVIEW_JOBS AUTOREVIEW_TIMEOUT \
        AUTOREVIEW_MAX_BUDGET_USD AUTOREVIEW_LOG_DIR \
        AUTOREVIEW_BABYSIT_INTERVAL || true
  unset FAKE_CLAUDE_FAIL FAKE_CLAUDE_IS_ERROR FAKE_CLAUDE_SLEEP \
        FAKE_GH_APPROVED || true
  # Leave the workspace title alone; the fake cmux ignores it either way.
  export REVIEW_PRS_WORKSPACE=""

  default_prs
}

teardown_sandbox() {
  [[ -n "${SANDBOX:-}" && -d "$SANDBOX" ]] && rm -rf "$SANDBOX"
}

reset_spawn_log() {
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
    # `gh pr view N --json reviewDecision --jq ...`: PRs named in
    # $FAKE_GH_APPROVED read as approved, everything else as undecided.
    shift || true
    num="${1:-}"
    case " ${FAKE_GH_APPROVED:-} " in
      *" $num "*) printf 'APPROVED\n' ;;
      *)          printf '\n' ;;
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

write_fake_claude() {
  cat >"$SANDBOX/bin/claude" <<'EOF'
#!/usr/bin/env bash
# Stands in for `claude -p`: records the call, emits a result envelope, and can
# be told to fail, to report an error inside a zero exit, or to run slowly.
set -uo pipefail

# The prompt is the last argument, and ends with the PR number.
prompt="${!#}"
n="${prompt##* }"

printf '%s\n' "$*" >>"$CLAUDE_LOG"
printf 'start %s\n' "$n" >>"$CLAUDE_LOG.events"

if [[ -n "${FAKE_CLAUDE_SLEEP:-}" ]]; then
  # Sleep in a marked grandchild, so a test can find survivors by name and so
  # stopping this job has to walk past this shell -- which is what a real
  # reviewer's own subprocesses look like. The marker is unique per sandbox:
  # the test finds it with pgrep, which searches the whole box.
  # The subshell and the trailing ":" are both load-bearing: a single-command
  # `bash -c` body is exec'd in place, which would drop the tag from the command
  # line and leave the assertion unable to see a survivor at all. This way the
  # wrapper keeps the tag in its own argv and the sleep carries it via exec -a,
  # so a leak of either one trips the test.
  bash -c '( exec -a "$0-sleep" sleep "$1" ) ; :' \
    "$FAKE_SLEEP_TAG-$n" "$FAKE_CLAUDE_SLEEP"
fi

is_error=false
status=0
case " ${FAKE_CLAUDE_FAIL:-} " in
  *" $n "*) status=1 ;;
esac
case " ${FAKE_CLAUDE_IS_ERROR:-} " in
  *" $n "*) is_error=true ;;
esac

# The envelope reports the session the turn ran in: the pinned or resumed id
# when one was given, otherwise the fresh id claude would have allocated
# itself. That difference is what the caller has to notice.
sid=""
prev=""
for arg in "$@"; do
  case "$prev" in
    --session-id|--resume) sid="$arg" ;;
  esac
  prev="$arg"
done
if [[ -z "$sid" ]]; then
  sid="00000000-0000-5000-a000-0000000000$(printf '%02d' "$n")"
fi

printf 'end %s\n' "$n" >>"$CLAUDE_LOG.events"
printf '{"type":"result","is_error":%s,"session_id":"%s","total_cost_usd":0.42,"num_turns":3,"result":"reviewed %s"}\n' \
  "$is_error" "$sid" "$n"
exit "$status"
EOF
  chmod +x "$SANDBOX/bin/claude"
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
   "author":{"login":"alice"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-10T10:00:00Z","author":{"user":{"login":"alice"}}}}]}},

  {"number":8,"title":"Fix typo","isDraft":false,
   "updatedAt":"2026-08-09T10:00:00Z","reviewDecision":"CHANGES_REQUESTED",
   "author":{"login":"bob"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-09T10:00:00Z","author":{"user":{"login":"bob"}}}}]}},

  {"number":6,"title":"Refactor client","isDraft":false,
   "updatedAt":"2026-08-08T10:00:00Z","reviewDecision":null,
   "author":{"login":"carol"},
   "comments":{"nodes":[{"author":{"login":"me"},"updatedAt":"2026-08-08T10:00:00Z"}]},
   "reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-07T10:00:00Z","author":{"user":{"login":"carol"}}}}]}},

  {"number":5,"title":"Approved already","isDraft":false,
   "updatedAt":"2026-08-06T10:00:00Z","reviewDecision":"APPROVED",
   "author":{"login":"dave"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-06T10:00:00Z","author":{"user":{"login":"dave"}}}}]}},

  {"number":4,"title":"My own work","isDraft":false,
   "updatedAt":"2026-08-05T10:00:00Z","reviewDecision":null,
   "author":{"login":"me"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-05T10:00:00Z","author":{"user":{"login":"me"}}}}]}},

  {"number":3,"title":"Bump lodash","isDraft":false,
   "updatedAt":"2026-08-04T10:00:00Z","reviewDecision":null,
   "author":{"login":"dependabot"},
   "comments":{"nodes":[]},"reviews":{"nodes":[]},
   "commits":{"nodes":[{"commit":{"committedDate":"2026-08-04T10:00:00Z","author":{"user":{"login":"dependabot"}}}}]}},

  {"number":2,"title":"Work in progress","isDraft":true,
   "updatedAt":"2026-08-03T10:00:00Z","reviewDecision":null,
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

# Run autoreview in the background and wait for $1 to appear in its output, up
# to $2 seconds, then kill it. For the babysit loop, whose whole point is that
# it does not exit -- the shortest interval it accepts is a minute, and no test
# should wait one.
run_autoreview_until() {
  local needle="$1" limit="$2"; shift 2
  local out="$SANDBOX/out/bg" waited=0 pid
  reset_spawn_log
  rm -rf "$SANDBOX/out/logs"
  : >"$out"
  ( cd "$SANDBOX/repo" && "$AUTOREVIEW" --log-dir "$SANDBOX/out/logs" "$@" >"$out" 2>&1 ) &
  pid=$!
  while [[ "$waited" -lt "$((limit * 10))" ]]; do
    if grep -qF -- "$needle" "$out" 2>/dev/null; then break; fi
    if ! kill -0 "$pid" 2>/dev/null; then break; fi
    sleep 0.1
    waited=$((waited + 1))
  done
  pkill -P "$pid" >/dev/null 2>&1 || true
  kill "$pid" >/dev/null 2>&1 || true
  wait "$pid" 2>/dev/null || true
  cat "$out"
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
