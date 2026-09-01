#!/usr/bin/env bash
# Run every test file, then lint the suite itself. Exits nonzero if anything
# fails.

set -uo pipefail

TESTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Resolved rather than "$TESTS_DIR/..": it is also the prefix the lint output
# strips to name each file relatively.
ROOT="$(cd "$TESTS_DIR/.." && pwd)"
failed=0

# The rust crate first: its unit tests pin the pieces this suite then exercises
# end to end, and binaries that do not build make the rest moot.
echo "cargo"
if (cd "$ROOT" && cargo build --quiet); then
  echo "  ok    all three binaries build"
else
  echo "  FAIL  the crate does not build"
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

export AUTOREVIEW="$ROOT/target/debug/autoreview"
export REVIEW_PRS="$ROOT/target/debug/review-prs"
export PANEL="$ROOT/target/debug/panel"

for f in "$TESTS_DIR"/*.test.sh; do
  bash "$f" || failed=1
  echo
done

# The tools are rust and linted by cargo. The bash left in the repo is this
# suite and the helper scripts the vendored skills carry, so both get linted.
echo "lint"
skill_scripts=()
while IFS= read -r f; do
  skill_scripts+=("$f")
done < <(find "$ROOT/skills" -name '*.sh' | sort)
for f in "$TESTS_DIR"/*.sh "${skill_scripts[@]}"; do
  if bash -n "$f"; then
    echo "  ok    ${f#"$ROOT"/} parses"
  else
    echo "  FAIL  ${f#"$ROOT"/} does not parse"
    failed=1
  fi
done

if command -v shellcheck >/dev/null 2>&1; then
  # -x follows each test file's `# shellcheck source=` directive into
  # helpers.sh, which is the only context where the harness's globals are
  # actually defined; --source-path resolves that directive against this
  # directory rather than the caller's cwd. SC2016 fires on literal "$VAR"
  # names inside the strings the tests assert on, which is the point of them.
  if shellcheck -x --source-path="$TESTS_DIR" --exclude=SC2016 "$TESTS_DIR"/*.sh; then
    echo "  ok    shellcheck clean"
  else
    echo "  FAIL  shellcheck reported problems"
    failed=1
  fi
  # The skill scripts run under whatever shell a reviewer's machine has, so
  # they are checked on their own terms: no suite-specific exclusions.
  if shellcheck "${skill_scripts[@]}"; then
    echo "  ok    skill scripts shellcheck clean"
  else
    echo "  FAIL  shellcheck reported problems in skills/"
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
