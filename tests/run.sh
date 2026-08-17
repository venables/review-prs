#!/usr/bin/env bash
# Run every test file, then lint every script. Exits nonzero if anything fails.

set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$TESTS_DIR/.."
failed=0

for f in "$TESTS_DIR"/*.test.sh; do
  bash "$f" || failed=1
  echo
done

# The entry point plus the libraries it sources. Libraries are listed explicitly
# rather than globbed so a new one that nothing sources still gets parsed and
# linted.
scripts=(
  "$ROOT/review-prs"
  "$ROOT/lib/interval.sh"
  "$ROOT/lib/repo.sh"
  "$ROOT/lib/session.sh"
  "$ROOT/lib/pr-list.sh"
)

echo "lint"
for f in "${scripts[@]}"; do
  if bash -n "$f"; then
    echo "  ok    ${f#"$ROOT"/} parses"
  else
    echo "  FAIL  ${f#"$ROOT"/} does not parse"
    failed=1
  fi
done

if command -v shellcheck >/dev/null 2>&1; then
  # Only the entry point is handed to shellcheck: -x follows its
  # `# shellcheck source=` directives into lib/, which is the only context where
  # a library's globals are actually defined. Checking a library on its own
  # would report every one of them unset.
  #
  # SC2016 fires on the single-quoted GraphQL query and on literal "$VAR" names
  # inside help text and messages. Both are intentional.
  if shellcheck -x --exclude=SC2016 "$ROOT/review-prs"; then
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
