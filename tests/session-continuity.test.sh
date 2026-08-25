#!/usr/bin/env bash
# Session continuity: derived ids, --continue, and the prompts each mode seeds.

set -euo pipefail
# shellcheck source=helpers.sh
. "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/helpers.sh"

echo "session-continuity"
setup_sandbox
trap teardown_sandbox EXIT

# --- A first review pins a derived session id ----------------------------
out="$(run_review_prs --auto)"
cmd9="$(spawned_cmd 'pr-review-tab 9')"
assert_contains "first review pins --session-id" "$cmd9" "--session-id"
assert_contains "first review runs the unattended skill" "$cmd9" '"pr-review-tab 9"'
assert_not_contains "first review does not resume" "$cmd9" "--resume"

sid9="$(session_id_from "$cmd9")"
assert_equals "the id is a 36-char uuid" "${#sid9}" "36"

# --- The id is stable across runs ----------------------------------------
run_review_prs --auto >/dev/null
assert_equals "same PR derives the same id on a later run" \
  "$(session_id_from "$(spawned_cmd 'pr-review-tab 9')")" "$sid9"

# --- Different PRs get different ids --------------------------------------
sid8="$(session_id_from "$(spawned_cmd 'pr-review-tab 8')")"
if [[ "$sid8" != "$sid9" ]]; then
  ok "different PRs derive different ids"
else
  not_ok "different PRs derive different ids" "both were $sid9"
fi

# --- An existing session, without --continue, changes nothing -------------
make_session "$sid9"
run_review_prs --auto >/dev/null
cmd9="$(spawned_cmd 'pr-review-tab 9')"
assert_not_contains "no -C: an existing session is not resumed" "$cmd9" "--resume"
assert_not_contains "no -C: a taken id is never re-pinned" "$cmd9" "--session-id"
assert_contains "no -C: still reviews from scratch" "$cmd9" '"pr-review-tab 9"'

# --- --continue resumes it ------------------------------------------------
out="$(run_review_prs --auto --continue)"
cmd9="$(spawned_cmd 'pr-review-tab 9')"
assert_contains "-C resumes the derived id" "$cmd9" "--resume $sid9"
assert_contains "-C seeds the recheck argument" "$cmd9" '"pr-review-tab 9 --recheck"'
assert_contains "-C reports the resume on stdout" "$out" "resuming earlier review"
assert_contains "-C labels the tab Recheck" "$(spawned_labels)" "PR 9 Recheck"

# --- A PR with no session still starts fresh under --continue ------------
cmd8="$(spawned_cmd 'pr-review-tab 8')"
assert_contains "-C pins a new id where no session exists" "$cmd8" "--session-id $sid8"
assert_not_contains "-C does not fake a recheck on a fresh PR" "$cmd8" "--recheck"

# --- Short flag ----------------------------------------------------------
run_review_prs --auto -C >/dev/null
assert_contains "-C is accepted as the short flag" \
  "$(spawned_cmd 'pr-review-tab 9')" "--resume $sid9"

# --- Interactive mode swaps the prompt, not just the flag -----------------
FAKE_GUM_PICK="#9" run_review_prs >/dev/null
assert_contains "picker, no -C: runs a panel review" \
  "$(spawned_cmd 'panel review 9')" '"panel review 9"'

FAKE_GUM_PICK="#9" run_review_prs --continue >/dev/null
cmd9="$(spawned_cmd 'recheck-pr 9')"
assert_contains "picker, -C: runs recheck-pr" "$cmd9" '"recheck-pr 9"'
assert_contains "picker, -C: resumes the session" "$cmd9" "--resume $sid9"

# --- Babysit composes with continue ---------------------------------------
run_review_prs --auto -C --babysit=15 >/dev/null
cmd9="$(spawned_cmd 'pr-review-tab 9')"
assert_contains "babysit + -C recheck and interval both reach the tab" \
  "$cmd9" '"pr-review-tab 9 --recheck --babysit 15m"'
assert_contains "babysit wins the tab label" "$(spawned_labels)" "PR 9 Babysit"

# --- A session another process holds is not resumed -----------------------
bash -c 'sleep 10; :' "holder-$sid9" &
holder=$!
sleep 0.5
out="$(run_review_prs --auto --continue)"
kill "$holder" 2>/dev/null || true
wait "$holder" 2>/dev/null || true
assert_contains "a live session is reported, not resumed" "$out" "open in another tab"
assert_not_contains "a live session is not resumed" "$(spawned_cmd 'pr-review-tab 9')" "--resume"

# --- The picker advertises what is resumable ------------------------------
FAKE_GUM_PICK="#9" run_review_prs >/dev/null
header="$(picker_header)"
assert_contains "resumable PRs are marked in the picker" "$header" "RESUMABLE"

# A repo with no sessions at all shows no SESSION column.
rm -rf "$CLAUDE_CONFIG_DIR/projects"
mkdir -p "$CLAUDE_CONFIG_DIR/projects"
FAKE_GUM_PICK="#9" run_review_prs >/dev/null
assert_not_contains "the column is hidden when nothing is resumable" \
  "$(picker_header)" "RESUMABLE"

finish
