#!/usr/bin/env bash
# Run every test file, then lint every script. Exits nonzero if anything fails.

set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$TESTS_DIR/.."
failed=0

# The rust crate first: its unit tests pin the pieces the bash suite then
# exercises end to end, and a binary that does not build makes the rest moot.
echo "cargo"
if (cd "$ROOT" && cargo build --quiet); then
  echo "  ok    autoreview builds"
else
  echo "  FAIL  autoreview does not build"
  failed=1
fi
if (cd "$ROOT" && cargo test --quiet >/dev/null 2>&1); then
  echo "  ok    cargo test passes"
else
  echo "  FAIL  cargo test failed"
  (cd "$ROOT" && cargo test --quiet 2>&1 | tail -20)
  failed=1
fi
echo

for f in "$TESTS_DIR"/*.test.sh; do
  bash "$f" || failed=1
  echo
done

# The two entry points plus the libraries they source. Libraries are listed
# explicitly rather than globbed so a new one that nothing sources still gets
# parsed and linted.
scripts=(
  "$ROOT/review-prs"
  "$ROOT/autoreview"
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
  # Only the entry points are handed to shellcheck: -x follows their
  # `# shellcheck source=` directives into lib/, which is the only context where
  # a library's globals are actually defined. Checking a library on its own
  # would report every one of them unset. Both entry points source all four, so
  # nothing goes unchecked.
  #
  # SC2016 fires on the single-quoted GraphQL query and on literal "$VAR" names
  # inside help text and messages. Both are intentional.
  if shellcheck -x --exclude=SC2016 "$ROOT/review-prs" "$ROOT/autoreview"; then
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
