#!/usr/bin/env bash
# Run every test file, then lint the script. Exits nonzero if anything fails.

set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
failed=0

for f in "$TESTS_DIR"/*.test.sh; do
  bash "$f" || failed=1
  echo
done

echo "lint"
if bash -n "$TESTS_DIR/../review-prs"; then
  echo "  ok    review-prs parses"
else
  echo "  FAIL  review-prs does not parse"
  failed=1
fi

if command -v shellcheck >/dev/null 2>&1; then
  # SC2016 fires on the single-quoted GraphQL query and on a literal
  # "$REVIEW_PRS_AUTO_CMD" in a message. Both are intentional.
  if shellcheck --exclude=SC2016 "$TESTS_DIR/../review-prs"; then
    echo "  ok    shellcheck clean"
  else
    echo "  FAIL  shellcheck reported problems"
    failed=1
  fi
else
  echo "  skip  shellcheck not installed"
fi

echo
if [[ "$failed" -eq 0 ]]; then
  echo "all tests passed"
else
  echo "TESTS FAILED"
fi
exit "$failed"
