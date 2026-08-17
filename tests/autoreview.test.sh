#!/usr/bin/env bash
# autoreview: headless reviews -- prompts, concurrency, failure reporting,
# timeouts, overrides and the babysit loop.

set -euo pipefail
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/helpers.sh"

echo "autoreview"
setup_sandbox
trap teardown_sandbox EXIT

# --- Which PRs get reviewed, and with what prompt -------------------------
out="$(run_autoreview --auto)"
assert_contains "NEW PRs are reviewed" "$(claude_calls)" "/auto-review 9"
assert_contains "UPDATED/CHANGES PRs are reviewed" "$(claude_calls)" "/auto-review 8"
assert_not_contains "SEEN PRs are skipped" "$(claude_calls)" "/auto-review 6"
assert_contains "a clean run reports each PR" "$out" "done    #9"
assert_equals "a clean run exits 0" "$(last_status)" "0"

assert_contains "reviews run in print mode" "$(claude_call_for '/auto-review 9')" "-p "
assert_contains "reviews ask for the json envelope" \
  "$(claude_call_for '/auto-review 9')" "--output-format json"
assert_contains "a first review pins --session-id" \
  "$(claude_call_for '/auto-review 9')" "--session-id"

FAKE_GUM_PICK="#9" run_autoreview >/dev/null
assert_contains "the picker path runs a panel review" \
  "$(claude_calls)" "/panel-review 9"

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
assert_contains "the failure count is reported" "$out" "1 of 2 review(s) failed"
assert_contains "the other PR still ran" "$(claude_calls)" "/auto-review 8"

# claude can exit 0 and still report a failed turn in its envelope.
out="$(FAKE_CLAUDE_IS_ERROR="9" run_autoreview --auto)"
assert_equals "is_error in the envelope fails the run" "$(last_status)" "1"
assert_contains "is_error is reported as a failure" "$out" "FAILED  #9"

# --- Concurrency ----------------------------------------------------------
FAKE_CLAUDE_SLEEP=0.4 run_autoreview --auto --jobs 1 >/dev/null
assert_equals "--jobs 1 runs one review at a time" \
  "$(claude_events | tr '\n' ' ')" "start 9 end 9 start 8 end 8 "

FAKE_CLAUDE_SLEEP=0.4 run_autoreview --auto --jobs 2 >/dev/null
assert_equals "--jobs 2 overlaps them" \
  "$(claude_events | head -2 | tr '\n' ' ')" "start 9 start 8 "

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

# --- Logs -----------------------------------------------------------------
run_autoreview --auto >/dev/null
if [[ -s "$SANDBOX/out/logs/pass-1/pr-9.json" ]]; then
  ok "each review's envelope is kept"
else
  not_ok "each review's envelope is kept" "no pr-9.json under pass-1"
fi
assert_contains "the envelope is what claude printed" \
  "$(cat "$SANDBOX/out/logs/pass-1/pr-9.json")" '"result":"reviewed 9"'

# --- Budget ---------------------------------------------------------------
run_autoreview --auto --budget 2.50 >/dev/null
assert_contains "--budget reaches claude" \
  "$(claude_call_for '/auto-review 9')" "--max-budget-usd 2.50"

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

AUTOREVIEW_CMD='my-review' run_autoreview >/dev/null
assert_equals "an attended run uses the override without complaint" \
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

# A box without `column` -- a slim CI image -- still gets its session ids.
cat >"$SANDBOX/bin/column" <<'STUB'
#!/usr/bin/env bash
exit 127
STUB
chmod +x "$SANDBOX/bin/column"
out="$(run_autoreview --auto)"
assert_equals "a summary that cannot be aligned still exits 0" "$(last_status)" "0"
assert_contains "...and still prints the session id" "$out" "00000000-0000-5000-a000-0000000000"
assert_contains "...and still prints the result" "$out" "done"
rm -f "$SANDBOX/bin/column"

# A reviewer that reports in prose leaves no session id to read back, which must
# not be mistaken for a failure: every review here succeeded.
out="$(AUTOREVIEW_AUTO_CMD='text-review' run_autoreview --auto)"
assert_equals "a non-JSON reviewer still exits 0" "$(last_status)" "0"
assert_contains "a non-JSON reviewer still gets a summary" "$out" "RESULT"
assert_contains "...naming each PR" "$out" "#9"
assert_contains "...with no session to offer" "$out" "-"

# --- Babysit sessions -----------------------------------------------------
# A later pass resumes the session the earlier pass actually ran in, recorded
# under the log dir. Resuming the derived id instead would re-check a review
# nobody wrote.
run_autoreview --auto >/dev/null
mkdir -p "$SANDBOX/out/logs"
printf 'aaaaaaaa-bbbb-5ccc-addd-eeeeeeeeeeee' >"$SANDBOX/out/logs/session-9.id"
reset_spawn_log
( cd "$SANDBOX/repo" && "$AUTOREVIEW" --log-dir "$SANDBOX/out/logs" --auto --continue >/dev/null 2>&1 )
assert_contains "a later pass resumes the session the earlier pass used" \
  "$(claude_call_for '/recheck-pr 9')" "--resume aaaaaaaa-bbbb-5ccc-addd-eeeeeeeeeeee"

# A recorded session another process still holds is not resumed either: two
# agents writing one transcript is the thing the guard exists to prevent, and a
# babysit interval is exactly when someone has `claude --resume` open.
run_autoreview --auto >/dev/null
held="bbbbbbbb-cccc-5ddd-aeee-ffffffffffff"
printf '%s' "$held" >"$SANDBOX/out/logs/session-9.id"
bash -c 'sleep 10; :' "holder-$held" &
holder=$!
sleep 0.5
reset_spawn_log
out="$( cd "$SANDBOX/repo" && "$AUTOREVIEW" --log-dir "$SANDBOX/out/logs" --auto --continue 2>&1 )"
kill "$holder" 2>/dev/null || true
wait "$holder" 2>/dev/null || true
assert_contains "a held session is reported, not resumed" "$out" "open elsewhere"
assert_not_contains "a held session is not resumed" "$(claude_calls)" "--resume $held"
# Refusing the recorded id hands the PR back to the ordinary rules, which here
# resume its own derived session.
assert_contains "...and the PR falls back to the derived session" \
  "$(claude_call_for '/recheck-pr 9')" "--resume $sid9"

# PR #8 has no session, so its review pins one -- and that is the id recorded.
run_autoreview --auto >/dev/null
assert_equals "the id a review ran in is recorded for the next pass" \
  "$(cat "$SANDBOX/out/logs/session-8.id")" \
  "$(session_id_from "$(claude_call_for '/auto-review 8')")"

# --- Bad input ------------------------------------------------------------
out="$(run_autoreview --auto --jobs 0)"
assert_equals "--jobs 0 exits nonzero" "$(last_status)" "1"
assert_contains "--jobs 0 says why" "$out" "expects an integer >= 1"

out="$(run_autoreview --auto --jobs abc)"
assert_equals "a non-numeric --jobs exits nonzero" "$(last_status)" "1"

out="$(run_autoreview --auto --budget lots)"
assert_equals "a non-numeric --budget exits nonzero" "$(last_status)" "1"
assert_contains "a non-numeric --budget says why" "$out" "expects a dollar amount"

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
assert_contains "the loop ends when every PR is approved" "$out" "every babysat PR is approved"
assert_equals "a fully approved babysit run exits 0" "$(last_status)" "0"

# One PR still unapproved: the loop waits for the interval instead of exiting.
out="$(FAKE_GH_APPROVED="9" run_autoreview_until "next check in" 10 --auto --babysit=1)"
assert_contains "an unapproved PR keeps the loop going" "$out" "next check in 1m"
assert_contains "only the unapproved PR is left" "$out" "(1 PR(s) left)"

# --- Nothing to do --------------------------------------------------------
echo '{"data":{"repository":{"pullRequests":{"nodes":[]}}}}' >"$SANDBOX/fixtures/prs.json"
out="$(run_autoreview --auto)"
assert_equals "an empty repo exits 0" "$(last_status)" "0"
assert_contains "an empty repo says why" "$out" "no matching open PRs"

finish
