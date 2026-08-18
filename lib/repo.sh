# shellcheck shell=bash
# Dependency checks and repo/user context for review-prs. The rust autoreview
# mirrors this in src/repo.rs; change both together.
#
# Sourced, never executed: the entry point owns `set -euo pipefail`.

require() {
  command -v "$1" >/dev/null 2>&1 && return 0
  printf 'error: missing required command: %s\n' "$1" >&2
  [[ -n "${2:-}" ]] && printf '  install with: %s\n' "$2" >&2
  return 1
}

# Belt-and-suspenders: brew lists these as formula dependencies, but either
# entry point may also be run standalone (curl-pipe-bash, manual install, a
# clone on a box that never saw the tap). Most names here are also their formula
# name; the few that are not get a hint of their own, because a message naming a
# package that does not exist is worse than no message.
dep_hint() {
  case "$1" in
    # Ships with macOS, so this only ever prints on a slim Linux image.
    pgrep) printf 'install procps (it ships with macOS)' ;;
    *)     printf 'brew install %s' "$1" ;;
  esac
}

require_deps() {
  local missing=0 cmd
  for cmd in "$@"; do
    require "$cmd" "$(dep_hint "$cmd")" || missing=1
  done
  [[ "$missing" -eq 0 ]] || exit 1
}

# Sets $owner, $name, $repo_root and $me for the rest of the run.
# shellcheck disable=SC2034  # every assignment here is read by the entry point
load_repo_context() {
  local repo_json
  if ! repo_json="$(gh repo view --json owner,name 2>&1)"; then
    echo "error: not a GitHub repo (or gh not authenticated)" >&2
    echo "$repo_json" >&2
    exit 1
  fi
  owner="$(jq -r '.owner.login' <<<"$repo_json")"
  name="$(jq -r '.name' <<<"$repo_json")"
  repo_root="$(git rev-parse --show-toplevel)"

  # `gh api` can exit 0 and still hand back an empty login, which would silently
  # mislabel every PR's engagement -- so check both the status and the value. The
  # `if !` is also what makes a genuine failure reportable here: a bare
  # `me="$(...)"` assignment would trip `set -e` and exit before the message.
  if ! me="$(gh api user --jq .login 2>&1)"; then
    echo "error: failed to fetch current GitHub user" >&2
    echo "$me" >&2
    exit 1
  fi
  if [[ -z "$me" ]]; then
    echo "error: gh api user returned empty login" >&2
    exit 1
  fi
}
