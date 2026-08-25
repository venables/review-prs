#!/usr/bin/env bash
# autoreview: headless reviews -- prompts, concurrency, failure reporting,
# timeouts, overrides and the babysit loop.

set -euo pipefail
# shellcheck source=helpers.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/helpers.sh"

echo "autoreview"
setup_sandbox
trap teardown_sandbox EXIT

# --- Which PRs get reviewed, and with what prompt -------------------------
# No flag at all: the sweep is the default, the picker is opt-in.
out="$(run_autoreview)"
assert_contains "NEW PRs are reviewed" "$(claude_calls)" "/auto-review 9"
assert_contains "UPDATED/CHANGES PRs are reviewed" "$(claude_calls)" "/auto-review 8"
assert_not_contains "SEEN PRs are skipped" "$(claude_calls)" "/auto-review 6"
assert_contains "a clean run reports each PR" "$out" "done    #9"
assert_equals "a clean run exits 0" "$(last_status)" "0"
assert_contains "the sweep names what it picked" "$out" "2 PRs to review: #9 #8"
assert_contains "the pass header counts in english" "$out" "reviewing 2 PRs"
assert_not_contains "...without the PR(s) shape" "$out" "PR(s)"

# The old opt-in spelling still parses, so a cron line or alias keeps working.
run_autoreview --auto >/dev/null
assert_contains "--auto is still accepted" "$(claude_calls)" "/auto-review 9"

assert_contains "reviews always get a meta envelope" \
  "$(claude_call_for '/auto-review 9')" "--meta-file"
assert_contains "reviews ask for the json envelope" \
  "$(claude_call_for '/auto-review 9')" "--output-format json"
assert_contains "reviews carry the timeout" \
  "$(claude_call_for '/auto-review 9')" "--timeout"
assert_contains "a first review pins --session-id" \
  "$(claude_call_for '/auto-review 9')" "--session-id"

FAKE_GUM_PICK="#9" run_autoreview --pick >/dev/null
assert_contains "--pick runs the picker, and a panel review on what it chose" \
  "$(claude_calls)" "/panel-review 9"
assert_not_contains "...and reviews nothing else" "$(claude_calls)" "/panel-review 8"

out="$(run_autoreview --pick)"
assert_contains "--pick with an empty selection reviews nothing" \
  "$out" "no PRs selected"

# --- Session continuity ---------------------------------------------------
run_autoreview --auto >/dev/null
sid9="$(session_id_from "$(claude_call_for '/auto-review 9')")"
assert_equals "the derived id is a 36-char uuid" "${#sid9}" "36"

make_session "$sid9"
run_autoreview --auto >/dev/null
assert_not_contains "no -C: an existing session is not resumed" \
  "$(claude_call_for '/auto-review 9')" "--resume"

out="$(run_autoreview --auto --continue)"
cmd9="$(claude_call_for '/recheck-pr 9')"
assert_contains "-C resumes the derived id" "$cmd9" "--resume $sid9"
assert_contains "-C swaps the prompt to a re-check" "$cmd9" "/recheck-pr 9"
assert_contains "-C leaves a PR with no session on a fresh review" \
  "$(claude_calls)" "/auto-review 8"

# The summary hands back each session id, which is the whole reason losing the
# tab is survivable.
assert_contains "the summary prints session ids" "$out" "$sid9"
assert_contains "the summary says how to reopen one" "$out" "claude --resume"

# --- Failures -------------------------------------------------------------
out="$(FAKE_CLAUDE_FAIL="9" run_autoreview --auto)"
assert_equals "a failed review exits 1" "$(last_status)" "1"
assert_contains "a failed review is named" "$out" "FAILED  #9"
assert_contains "the failure count is reported" "$out" "1 of 2 reviews failed"
assert_contains "the other PR still ran" "$(claude_calls)" "/auto-review 8"

# A turn that reports is_error is exit 10 from dash-p -- the envelope-level
# failure and the exit code are one signal now.
out="$(FAKE_CLAUDE_IS_ERROR="9" run_autoreview --auto)"
assert_equals "is_error in the envelope fails the run" "$(last_status)" "1"
assert_contains "is_error is reported as a failure" "$out" "FAILED  #9"

# Garbage claude output (a crash, prose instead of JSON) is also exit 10 from
# dash-p, with an empty session id in the envelope.
out="$(FAKE_CLAUDE_GARBAGE="9" run_autoreview --auto)"
assert_equals "a built-in reviewer that answers in prose fails the run" \
  "$(last_status)" "1"
assert_contains "...and is named" "$out" "FAILED  #9"
assert_contains "...while the other PR still succeeds" "$out" "done    #8"

# --- Concurrency ----------------------------------------------------------
FAKE_CLAUDE_SLEEP=0.4 run_autoreview --auto --jobs 1 >/dev/null
assert_equals "--jobs 1 runs one review at a time" \
  "$(claude_events | tr '\n' ' ')" "start 9 end 9 start 8 end 8 "

# Overlap means both reviews started before either ended. The two fakes race
# to write their own start lines, so the first two events are asserted as a
# set, not a sequence.
FAKE_CLAUDE_SLEEP=0.4 run_autoreview --auto --jobs 2 >/dev/null
assert_equals "--jobs 2 overlaps them" \
  "$(claude_events | head -2 | sort | tr '\n' ' ')" "start 8 start 9 "

# --- Timeout --------------------------------------------------------------
out="$(FAKE_CLAUDE_SLEEP=30 run_autoreview --auto --jobs 2 --timeout 1)"
assert_contains "a review that overruns --timeout is stopped" "$out" "TIMEOUT #9"
assert_equals "a timed-out review exits 1" "$(last_status)" "1"

# The reviewer's own children have to go too: an orphan keeps spending and keeps
# holding the session open, so the next --continue would refuse to resume it.
sleep 0.5
survivors="$(pgrep -f "$FAKE_SLEEP_TAG" 2>/dev/null | wc -l | tr -d ' ' || true)"
assert_equals "a stopped review leaves nothing behind" "$survivors" "0"

# Job control would announce each killed job on stderr, in the middle of the
# progress block.
assert_not_contains "stopping a review is quiet" "$out" "Terminated"

# A reviewer that ignores TERM must not outlive its timeout: the run waits on
# each job, so anything short of KILL would hang here forever. Bounded by the
# helper, which gives up after 20s -- a regression fails rather than hangs.
# Waiting for the summary rather than the first TIMEOUT line: the run has to
# reach its own end for this to mean anything, and killing it mid-flight would
# orphan the reviewers itself and prove nothing.
out="$(AUTOREVIEW_AUTO_CMD='stubborn-review' \
  run_autoreview_until "reopen any review" 25 --auto --jobs 2 --timeout 1)"
assert_contains "a reviewer that ignores TERM is still stopped" "$out" "TIMEOUT #9"
assert_contains "...and the run reaches its summary instead of hanging" \
  "$out" "reopen any review"
sleep 0.5
survivors="$(pgrep -f "$FAKE_SLEEP_TAG" 2>/dev/null | wc -l | tr -d ' ' || true)"
assert_equals "...and leaves nothing behind either" "$survivors" "0"

# A job killed from outside leaves no status behind, and the slot it holds must
# not be held until the timeout -- with --timeout 0 that would be forever, which
# is why this one runs with no timeout at all and is bounded by the helper.
out="$(FAKE_CLAUDE_KILL_JOB="9" \
  run_autoreview_until "reopen any review" 30 --auto --jobs 2 --timeout 0)"
assert_contains "a job that dies without a status is reported" "$out" "FAILED  #9"
assert_contains "...as having produced nothing" "$out" "no result"
assert_contains "...and the pass still ends" "$out" "reopen any review"

# --- Logs -----------------------------------------------------------------
# Each run keeps its output under a directory of its own, so two runs sharing a
# --log-dir cannot read each other's results.
run_autoreview --auto >/dev/null
envelope="$(echo "$SANDBOX"/out/logs/run-*/pass-1/pr-9.json)"
if [[ -s "$envelope" ]]; then
  ok "each review's envelope is kept"
else
  not_ok "each review's envelope is kept" "no pr-9.json under a run dir"
fi
assert_contains "the answer is what dash-p printed" \
  "$(cat "$envelope" 2>/dev/null)" '"answer":"reviewed 9"'

runs_before="$(echo "$SANDBOX"/out/logs/run-* | wc -w | tr -d ' ')"
( cd "$SANDBOX/repo" && "$AUTOREVIEW" --log-dir "$SANDBOX/out/logs" --auto >/dev/null 2>&1 )
runs_after="$(echo "$SANDBOX"/out/logs/run-* | wc -w | tr -d ' ')"
if [[ "$runs_after" -gt "$runs_before" ]]; then
  ok "a second run against the same log dir gets its own directory"
else
  not_ok "a second run against the same log dir gets its own directory" \
    "still $runs_after run dir(s)"
fi

# --- Budget ---------------------------------------------------------------
# The single-token = form: dash-p forwards unrecognized flags only that way,
# and a silently dropped cap on an unattended sweep is exactly the failure the
# flag exists to prevent.
run_autoreview --auto --budget 2.50 >/dev/null
assert_contains "--budget reaches the reviewer" \
  "$(claude_call_for '/auto-review 9')" "--max-budget-usd=2.50"

# --- Overrides ------------------------------------------------------------
AUTOREVIEW_AUTO_CMD='my-review' run_autoreview --auto >/dev/null
assert_contains "an override without {} gets the number appended" \
  "$(override_calls)" "args=9"
assert_contains "an override is handed the session id" "$(override_calls)" "session=$sid9"
assert_contains "an override is told whether it is resuming" "$(override_calls)" "resume=0"
assert_equals "an override replaces claude entirely" "$(claude_calls)" ""

AUTOREVIEW_AUTO_CMD='my-review {} --extra {}' run_autoreview --auto >/dev/null
assert_contains "{} is substituted everywhere" "$(override_calls)" "args=9 --extra 9"

out="$(AUTOREVIEW_CMD='my-review' run_autoreview --auto)"
assert_contains "an unattended run says when it falls back to the built-in reviewer" \
  "$out" 'AUTOREVIEW_CMD is set but $AUTOREVIEW_AUTO_CMD is not'
assert_contains "...and the built-in reviewer is what actually ran" \
  "$(claude_calls)" "/auto-review 9"

FAKE_GUM_PICK="#9" AUTOREVIEW_CMD='my-review' run_autoreview --pick >/dev/null
assert_equals "an attended --pick run uses the override without complaint" \
  "$(claude_calls)" ""

# --- The summary ----------------------------------------------------------
# The session column names the review that just ran, which is not always the
# derived id: PR #9's session already exists (made above) and no -C was passed,
# so no flag was sent and claude allocated its own id.
out="$(run_autoreview --auto)"
assert_not_contains "an unpinned review is not reported under the derived id" \
  "$out" "$sid9"
assert_contains "an unpinned review is reported under the id it actually ran in" \
  "$out" "00000000-0000-5000-a000-000000000009"

# The summary is formatted natively -- `column` is not a dependency, so a slim
# CI image without it still gets its session ids.
out="$(run_autoreview --auto)"
assert_equals "the summary needs no external formatter" "$(last_status)" "0"
assert_contains "...and prints the session id" "$out" "00000000-0000-5000-a000-0000000000"
assert_contains "...and the result" "$out" "done"

# A reviewer that reports in prose leaves no session id to read back, which must
# not be mistaken for a failure: every review here succeeded.
run_autoreview --auto >/dev/null
sid8="$(session_id_from "$(claude_call_for '/auto-review 8')")"
out="$(AUTOREVIEW_AUTO_CMD='text-review' run_autoreview --auto)"
assert_equals "a non-JSON reviewer still exits 0" "$(last_status)" "0"
assert_contains "a non-JSON reviewer still gets a summary" "$out" "RESULT"
assert_contains "...naming each PR" "$out" "#9"
# An override owns its own session handling: it was offered an id and may have
# ignored it, so the summary must not tell you to reopen one.
assert_not_contains "...offering no session it cannot vouch for" "$out" "$sid8"
assert_not_contains "...nor the derived one" "$out" "$sid9"

# --- Verdicts and models --------------------------------------------------
# The verdict is read back from GitHub -- an agent that believed its own
# report would show "approved" for an approval that never landed. The trailer
# (the fenced block the system prompt asks the reviewer for) fills in what
# GitHub cannot know: risk, finding counts, and each panelist's model.
out="$(FAKE_GH_MY_REVIEW='9:APPROVED' FAKE_CLAUDE_TRAILER='8' run_autoreview)"
assert_contains "the trailer request reaches the reviewer" \
  "$(claude_call_for '/auto-review 9')" "--append-system-prompt="
assert_contains "an approval on GitHub is the verdict" "$out" "approved"
assert_contains "the trailer supplies the verdict when GitHub has none" \
  "$out" "commented"
assert_contains "the synthesized risk is shown" "$out" "LOW"
assert_contains "the finding counts are shown" "$out" "1 polish"
assert_contains "the model that drove the review is shown" "$out" "claude-fable-5"
assert_contains "each panelist's model and result are shown" \
  "$out" "codex (gpt-5.5) 1 finding, top LOW"
assert_contains "a clean panelist is shown too" "$out" "claude (claude-opus-4.7) clean"

# A reviewer that never writes the block costs a "-" in the summary, nothing
# more -- and GitHub reading back nothing is not a failure either.
out="$(run_autoreview --auto)"
assert_equals "a run with no trailer and no review still exits 0" "$(last_status)" "0"
assert_not_contains "...and shows no panel it never heard about" "$out" "panel #"
# A bare "-" in the VERDICT column read as a verdict of its own.
assert_contains "...and says outright that no review was posted" "$out" "nothing posted"

# The staleness rule -- a review my login submitted before the run must not
# read as this run's verdict -- is pinned by the rust unit tests; the fake
# always stamps "now". Here: the other review states map through too.
out="$(FAKE_GH_MY_REVIEW='9:CHANGES_REQUESTED' run_autoreview --auto)"
assert_contains "a blocking review reads as changes requested" \
  "$out" "changes requested"

# A readback gh cannot perform is announced rather than silently swapped for
# the agent's own report -- an unattended run must show which verdicts are
# self-reported only.
out="$(FAKE_GH_VIEW_FAIL=1 FAKE_CLAUDE_TRAILER='9' run_autoreview --auto)"
assert_contains "a failed readback is announced" \
  "$out" "could not read PR #9's review back from GitHub; the verdict is the agent's own report"
assert_contains "...and the agent's own report still fills the column" \
  "$out" "commented"
assert_contains "...while a PR with no trailer admits its verdict is unknown" \
  "$out" "could not read PR #8's review back from GitHub; its verdict is unknown"
assert_equals "...without failing the run" "$(last_status)" "0"

# --- Babysit sessions -----------------------------------------------------
# PR #8 has no session, so its review pins one -- and that is the id recorded,
# in this run's own directory rather than one shared with every other run.
run_autoreview --auto >/dev/null
assert_equals "the id a review ran in is recorded for the next pass" \
  "$(cat "$SANDBOX"/out/logs/run-*/session-8.id 2>/dev/null)" \
  "$(session_id_from "$(claude_call_for '/auto-review 8')")"

# A failed review is not worth resuming: /recheck-pr against it has no findings
# to check, so it would never approve and the loop would re-spend every
# interval. Nothing is recorded for it.
FAKE_CLAUDE_IS_ERROR="8" run_autoreview --auto >/dev/null
if [[ -e "$(echo "$SANDBOX"/out/logs/run-*/session-8.id)" ]]; then
  not_ok "a failed review records no session" "session-8.id was written"
else
  ok "a failed review records no session"
fi

# Two runs against one --log-dir get separate state, so neither can resume the
# other's review of the same PR.
run_autoreview --auto >/dev/null
( cd "$SANDBOX/repo" && "$AUTOREVIEW" --log-dir "$SANDBOX/out/logs" --auto >/dev/null 2>&1 )
recorded_files="$(echo "$SANDBOX"/out/logs/run-*/session-9.id | wc -w | tr -d ' ')"
assert_equals "each run records its sessions separately" "$recorded_files" "2"

# --- Bad input ------------------------------------------------------------
out="$(run_autoreview --auto --jobs 0)"
assert_equals "--jobs 0 exits nonzero" "$(last_status)" "1"
assert_contains "--jobs 0 says why" "$out" "expects an integer >= 1"

out="$(run_autoreview --auto --jobs abc)"
assert_equals "a non-numeric --jobs exits nonzero" "$(last_status)" "1"

out="$(run_autoreview --auto --budget lots)"
assert_equals "a non-numeric --budget exits nonzero" "$(last_status)" "1"
assert_contains "a non-numeric --budget says why" "$out" "expects a dollar amount"

# An empty "=" value is the same typo as an empty separate value, and an
# unattended sweep must not run uncapped because of which one you typed.
out="$(run_autoreview --auto --budget=)"
assert_equals "an empty --budget= exits nonzero" "$(last_status)" "1"
assert_contains "an empty --budget= says why" "$out" "expects a value"

out="$(run_autoreview --auto --log-dir=)"
assert_equals "an empty --log-dir= exits nonzero" "$(last_status)" "1"

out="$(run_autoreview --auto --timeout)"
assert_equals "a flag with no value exits nonzero" "$(last_status)" "1"
assert_contains "a flag with no value says why" "$out" "expects a value"

out="$(run_autoreview --nope)"
assert_equals "an unknown flag exits nonzero" "$(last_status)" "1"
assert_contains "an unknown flag says so" "$out" "unknown arg"

out="$(run_autoreview --auto --babysit=soon)"
assert_equals "a bad babysit interval exits nonzero" "$(last_status)" "1"
assert_contains "a bad babysit interval says why" "$out" "invalid babysit interval"

# --- Babysit --------------------------------------------------------------
# Every PR approved after the first pass: the loop has nothing left to wait for
# and ends without sleeping.
out="$(FAKE_GH_APPROVED="9 8" run_autoreview --auto --babysit=1)"
assert_contains "an approved PR is dropped from the loop" "$out" "PR #9 is approved"
assert_contains "the loop ends when every PR is approved" "$out" "nothing left to babysit"
assert_equals "a fully approved babysit run exits 0" "$(last_status)" "0"

# A PR that will never be approved because it is no longer open must leave the
# queue too, or the loop re-reviews it every interval for as long as it runs.
out="$(FAKE_GH_APPROVED="8" FAKE_GH_CLOSED="9" run_autoreview --auto --babysit=1)"
assert_contains "a closed PR is dropped from the loop" "$out" "PR #9 is closed"
assert_contains "the loop ends with nothing left" "$out" "nothing left to babysit"

# One PR still unapproved: the loop waits for the interval instead of exiting.
out="$(FAKE_GH_APPROVED="9" run_autoreview_until "next check in" 10 --auto --babysit=1)"
assert_contains "an unapproved PR keeps the loop going" "$out" "next check in 1m"
assert_contains "only the unapproved PR is left" "$out" "(1 PR left)"

# The one behaviour no single pass can show: the pass after the first resumes
# the session its predecessor actually ran in, rather than the derived id, which
# may name an older review. It costs a minute of wall clock because the shortest
# interval the tool accepts is a minute -- deliberately, so a re-check loop
# cannot run hot. Approving #8 leaves one PR for the second pass.
out="$(FAKE_GH_APPROVED="8" run_autoreview_until_call "/recheck-pr 9" 240 1 --auto --babysit=1)"
recorded="$(cat "$SANDBOX"/out/logs/run-*/session-9.id 2>/dev/null || true)"
assert_contains "a second pass re-checks rather than reviews again" \
  "$(claude_calls)" "/recheck-pr 9"
assert_contains "...resuming the session the first pass ran in" \
  "$(claude_call_for '/recheck-pr 9')" "--resume $recorded"

# A pass whose review failed has nothing to re-check, so the next one reviews
# from scratch rather than re-checking whatever session is on disk -- PR #9 has
# one, made at the top of this file.
out="$(FAKE_CLAUDE_IS_ERROR="9" FAKE_GH_APPROVED="8" \
  run_autoreview_until_call "/auto-review 9" 240 2 --auto --babysit=1)"
assert_not_contains "a pass after a failed review does not re-check it" \
  "$(claude_calls)" "/recheck-pr 9"
assert_not_contains "...and resumes nothing" "$(claude_calls)" "--resume $sid9"

# --- New work joins the queue mid-run -------------------------------------
# The queue used to be fixed when the run started, so a PR opened a minute
# later waited for a whole new invocation of autoreview. Costs a minute of wall
# clock, like the pass test above: one interval has to actually elapse.
default_prs
reset_spawn_log
: >"$SANDBOX/out/bg"
( cd "$SANDBOX/repo" && FAKE_GH_APPROVED="9" "$AUTOREVIEW" \
    --log-dir "$SANDBOX/out/logs" --auto --babysit=1 >"$SANDBOX/out/bg" 2>&1 ) &
bg=$!

# Wait for the first pass to settle into its interval, then open a PR.
waited=0
while [[ "$waited" -lt 600 ]]; do
  grep -q "next check in" "$SANDBOX/out/bg" 2>/dev/null && break
  kill -0 "$bg" 2>/dev/null || break
  sleep 0.1
  waited=$((waited + 1))
done
jq '.data.repository.pullRequests.nodes += [{
      "number":12,"title":"Opened mid-run","isDraft":false,
      "updatedAt":"2026-08-11T10:00:00Z","reviewDecision":null,
      "author":{"login":"frank"},
      "comments":{"nodes":[]},"reviews":{"nodes":[]},
      "commits":{"nodes":[{"commit":{"committedDate":"2026-08-11T10:00:00Z","author":{"user":{"login":"frank"}}}}]}
    }]' "$SANDBOX/fixtures/prs.json" >"$SANDBOX/fixtures/prs.next"
mv "$SANDBOX/fixtures/prs.next" "$SANDBOX/fixtures/prs.json"

waited=0
while [[ "$waited" -lt 1800 ]]; do
  grep -q -- "/auto-review 12" "$CLAUDE_LOG" 2>/dev/null && break
  kill -0 "$bg" 2>/dev/null || break
  sleep 0.1
  waited=$((waited + 1))
done
pkill -P "$bg" >/dev/null 2>&1 || true
kill "$bg" >/dev/null 2>&1 || true
wait "$bg" 2>/dev/null || true

out="$(cat "$SANDBOX/out/bg")"
assert_contains "a PR opened mid-run joins the queue" "$out" "joined the queue: #12"
assert_contains "...and is reviewed without restarting the run" \
  "$(claude_calls)" "/auto-review 12"
assert_contains "...while an approved PR still leaves" "$out" "PR #9 is approved"
# The sweep still lists #9 as NEW -- the fake gh's GraphQL fixture does not know
# about the approval -- so this is the guard against a stale list putting a
# finished PR straight back into the queue that just dropped it.
reviews_of_nine="$(claude_calls | grep -c -- "/auto-review 9" || true)"
assert_equals "...and an approved PR is not re-reviewed by a stale sweep" \
  "$reviews_of_nine" "1"

default_prs

# --- Nothing to do --------------------------------------------------------
echo '{"data":{"repository":{"pullRequests":{"nodes":[]}}}}' >"$SANDBOX/fixtures/prs.json"
out="$(run_autoreview --auto)"
assert_equals "an empty repo exits 0" "$(last_status)" "0"
assert_contains "an empty repo says why" "$out" "no matching open PRs"

finish
