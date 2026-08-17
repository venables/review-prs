# shellcheck shell=bash
# Babysit-interval parsing, shared by review-prs and autoreview.
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

# The same duration in seconds, for a sleep. Only ever called on a value
# normalize_interval has already accepted, so the suffix is known good. 10#
# strips leading zeros that arithmetic would otherwise read as octal ("05m").
interval_seconds() {
  local v="$1" n
  n=$((10#${v%[mhd]}))
  case "$v" in
    *m) printf '%s' $((n * 60)) ;;
    *h) printf '%s' $((n * 3600)) ;;
    *d) printf '%s' $((n * 86400)) ;;
  esac
}
