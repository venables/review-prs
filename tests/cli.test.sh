#!/usr/bin/env bash
# Flags, filtering, command overrides, and exit status.

set -euo pipefail
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
assert_contains "the failure count is reported" "$out" "failed to spawn"

# --- Nothing to do --------------------------------------------------------
echo '{"data":{"repository":{"pullRequests":{"nodes":[]}}}}' >"$SANDBOX/fixtures/prs.json"
out="$(run_review_prs --auto)"
assert_equals "an empty repo exits 0" "$(last_status)" "0"
assert_contains "an empty repo says why" "$out" "no matching open PRs"

finish
