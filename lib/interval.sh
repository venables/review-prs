# shellcheck shell=bash
# Babysit-interval parsing for review-prs. The rust autoreview mirrors this
# in src/interval.rs; change both together.
#
# Sourced, never executed: the entry point owns `set -euo pipefail`.

# Normalize an interval to a duration string: a bare number is minutes
# ("30" -> "30m"); an already-suffixed value ("30m", "1h") passes through
# untouched. Anything else is rejected here rather than silently reaching the
# review loop: "0" would re-check with no delay (a hot loop running recheck-pr
# under --dangerously-skip-permissions), and "soon" or a bare "--babysit="
# would arrive as an unparseable duration. Sub-minute units are refused for the
# same hot-loop reason -- re-checking a PR every second is never what you meant.
normalize_interval() {
  local v="$1"
  [[ "$v" =~ ^[0-9]+$ ]] && v="${v}m"
  # Leading zeros are fine ("05" is plainly five), but a value of zero is not --
  # the [1-9] keeps "0", "00" and "0m" rejected.
  if [[ ! "$v" =~ ^0*[1-9][0-9]*[mhd]$ ]]; then
    printf 'error: invalid babysit interval: "%s" (expected a positive duration, e.g. 30, 30m, 1h)\n' "$1" >&2
    return 1
  fi
  printf '%s' "$v"
}

