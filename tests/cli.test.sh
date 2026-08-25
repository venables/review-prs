#!/usr/bin/env bash
# Flags, filtering, command overrides, and exit status.

set -euo pipefail
# shellcheck source=helpers.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/helpers.sh"

echo "cli"
setup_sandbox
trap teardown_sandbox EXIT

# --- Which PRs the sweep picks up ----------------------------------------
out="$(run_review_prs --auto)"
assert_contains "NEW PRs are swept" "$out" "#9"
assert_contains "UPDATED/CHANGES PRs are swept" "$out" "#8"
assert_not_contains "SEEN PRs are skipped" "$out" "#6"
assert_not_contains "approved PRs are hidden by default" "$out" "#5"
assert_not_contains "your own PRs are always hidden" "$out" "#4"
assert_not_contains "dependabot PRs are hidden by default" "$out" "#3"
assert_not_contains "draft PRs are hidden" "$out" "#2"

out="$(run_review_prs --auto --dependabot)"
assert_contains "--dependabot includes bot PRs" "$out" "#3"

# --- What the sweep says for itself ---------------------------------------
# Pinned because it is the line a wrapper greps, and because it deliberately
# no longer matches the bash this replaced: the "N PR(s)" shape went away
# across both tools, so the sweep says its count in plain english.
out="$(run_review_prs --auto)"
assert_contains "the sweep names what it picked" "$out" "2 PRs to review: #9 #8"
assert_not_contains "...without the PR(s) shape" "$out" "PR(s)"

# Nothing actionable is not an error, and it says how to see the rest. PR #6 is
# the SEEN one, so a repo holding only it has nothing for a sweep to do.
jq '.data.repository.pullRequests.nodes |= map(select(.number == 6))' \
  "$SANDBOX/fixtures/prs.json" >"$SANDBOX/fixtures/seen-only.json"
mv "$SANDBOX/fixtures/seen-only.json" "$SANDBOX/fixtures/prs.json"
out="$(run_review_prs --auto)"
assert_equals "a sweep with nothing actionable exits 0" "$(last_status)" "0"
assert_contains "...and says so" "$out" "no NEW or UPDATED PRs to review"
assert_contains "...and names the way to see the rest" "$out" "run without --auto"

# --- No terminal ----------------------------------------------------------
# The terminal is only a problem once there is something to spawn, so a run
# with no work exits 0 from a terminal this tool could never have driven.
out="$(CMUX_SURFACE_ID='' run_review_prs --auto)"
assert_equals "nothing to review exits 0 even with no terminal" "$(last_status)" "0"
# Pin the reason for the 0, not just the 0: a fixture that later went empty
# would still exit 0, and this test would pass without covering anything.
assert_contains "...because the sweep found nothing to do" \
  "$out" "no NEW or UPDATED PRs to review"
assert_not_contains "...and says nothing about terminals" "$out" "no supported terminal"

default_prs
out="$(CMUX_SURFACE_ID='' run_review_prs --auto)"
assert_equals "PRs to spawn with no terminal exits 1" "$(last_status)" "1"
assert_contains "...and says what it looked for" "$out" "no supported terminal detected"
assert_contains "...and points at the headless sibling" "$out" "use: autoreview"

FAKE_GUM_PICK="#5" run_review_prs --all >/dev/null
assert_contains "--all includes approved PRs" "$(spawned_cmd 'panel review 5')" "panel review 5"

# --- Babysit interval parsing --------------------------------------------
for good in 05 30 30m 1h 2d; do
  run_review_prs --auto --babysit="$good" >/dev/null
  assert_equals "--babysit=$good is accepted" "$(last_status)" "0"
done

for bad in 0 00 0m soon "" 5s; do
  out="$(run_review_prs --auto --babysit="$bad")"
  if [[ "$(last_status)" -ne 0 && "$out" == *"invalid babysit interval"* ]]; then
    ok "--babysit=$bad is rejected"
  else
    not_ok "--babysit=$bad is rejected" "status=$(last_status) out=$out"
  fi
done

run_review_prs --auto --babysit=05 >/dev/null
assert_contains "a bare number becomes minutes" \
  "$(spawned_cmd 'pr-review-tab 9')" "--babysit 05m"

# --- Unknown flags fail loudly -------------------------------------------
out="$(run_review_prs --nope)"
assert_equals "an unknown flag exits nonzero" "$(last_status)" "1"
assert_contains "an unknown flag says so" "$out" "unknown arg"

# --- Command overrides ----------------------------------------------------
out="$(REVIEW_PRS_AUTO_CMD='my-review' run_review_prs --auto)"
cmd9="$(spawned_cmd 'my-review 9')"
assert_contains "an override without {} gets the number appended" "$cmd9" "my-review 9"
assert_contains "an override is handed the session id" "$cmd9" "export REVIEW_PRS_SESSION_ID="
assert_contains "an override is told it is not resuming" "$cmd9" "REVIEW_PRS_SESSION_RESUME=0"

out="$(REVIEW_PRS_AUTO_CMD='checkout {} && my-review {}' run_review_prs --auto)"
cmd9="$(spawned_cmd 'my-review 9')"
assert_contains "{} is substituted everywhere" "$cmd9" "checkout 9 && my-review 9"
# The export must precede the cd, so the && guard still covers the override.
case "$cmd9" in
  "export REVIEW_PRS_SESSION_ID="*"; cd "*" && checkout 9 && my-review 9")
    ok "the cd guard still covers a compound override" ;;
  *)
    not_ok "the cd guard still covers a compound override" "got: $cmd9" ;;
esac

out="$(REVIEW_PRS_AUTO_CMD='my-review' run_review_prs --auto --babysit=15)"
assert_contains "an override is told the interval cannot reach it" \
  "$out" "not passed to it"

# --- Tab labels -----------------------------------------------------------
FAKE_GUM_PICK="#9" run_review_prs >/dev/null
assert_contains "picker tabs are labelled Review" "$(spawned_labels)" "PR 9 Review"

run_review_prs --auto >/dev/null
assert_contains "auto tabs are labelled Auto-Review" "$(spawned_labels)" "PR 9 Auto-Review"

# --- Exit status ----------------------------------------------------------
run_review_prs --auto >/dev/null
assert_equals "a clean sweep exits 0" "$(last_status)" "0"

out="$(FAKE_CMUX_FAIL_SEND=1 run_review_prs --auto)"
assert_equals "a sweep whose tabs all fail exits 1" "$(last_status)" "1"
assert_contains "the failure count is reported" "$out" "2 of 2 tabs failed to spawn"

# --- Nothing to do --------------------------------------------------------
echo '{"data":{"repository":{"pullRequests":{"nodes":[]}}}}' >"$SANDBOX/fixtures/prs.json"
out="$(run_review_prs --auto)"
assert_equals "an empty repo exits 0" "$(last_status)" "0"
assert_contains "an empty repo says why" "$out" "no matching open PRs"

finish
