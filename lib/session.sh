# shellcheck shell=bash
# Review-session continuity, shared by review-prs and autoreview.
#
# A PR's review session id is derived from the repo directory plus
# owner/name#NUM rather than recorded in a state file: the mapping is then
# stable across runs with nothing to keep in sync, and nothing to go stale when
# a PR is closed.
#
# $repo_root is in the hash on purpose, and it is what makes the whole scheme
# safe. Claude Code stores a session under the directory it ran in, and every
# review runs in $repo_root -- so a session belongs to one checkout. Hashing the
# path in gives a second clone or a git worktree of the same repo its own id for
# the same PR. Leave it out and both checkouts derive one id: the second would
# mark the first's session RESUMABLE, resume it (which succeeds, and quietly
# carries the other checkout's context), and -- worse -- would never pin a
# --session-id of its own, so it could never become resumable.
#
# Sourced, never executed: the entry point owns `set -euo pipefail`.

# md5 on macOS, md5sum elsewhere. A box with neither still reviews fine -- it
# just loses session continuity, so degrade instead of failing the run. This is
# a naming hash, not a security one; collisions across PRs of one repo are the
# only thing that matters, and 128 bits is far past enough for that.
if command -v md5 >/dev/null 2>&1; then
  hash_hex() { md5 -q; }
elif command -v md5sum >/dev/null 2>&1; then
  hash_hex() { md5sum | cut -d' ' -f1; }
else
  hash_hex() { printf ''; }
fi

# Shape the digest into a v5-form UUID: claude --session-id rejects anything
# else. Byte 6's high nibble is the version (5) and byte 8's is the variant (a
# => 10xx), which is why those two nibbles are literals rather than digest hex.
pr_session_id() {
  local h
  h="$(printf 'review-prs:%s:%s/%s#%s' "$repo_root" "$owner" "$name" "$1" | hash_hex)"
  [[ ${#h} -ge 32 ]] || return 0
  printf '%s-%s-5%s-a%s-%s' \
    "${h:0:8}" "${h:8:4}" "${h:13:3}" "${h:17:3}" "${h:20:12}"
}

# Sessions live under a directory named for the escaped cwd, but that escaping
# is undocumented and has changed before -- so glob every project directory
# instead of rebuilding the name. Searching wider than $repo_root is safe
# because the id already encodes the checkout: only this checkout's session can
# carry this id.
claude_projects_dir="${CLAUDE_CONFIG_DIR:-$HOME/.claude}/projects"

session_exists() {
  [[ -n "$1" && -d "$claude_projects_dir" ]] || return 1
  compgen -G "$claude_projects_dir/*/$1.jsonl" >/dev/null 2>&1
}

# True when another process still holds this session open. Claude Code treats an
# id as taken once the transcript file exists, so it does not stop a second
# process from reopening a live one -- two agents would then write one
# transcript. A babysit run is long-lived by design, which makes this easy to
# hit. pgrep matches the id on the command line; a false match only costs you a
# fresh review, so it fails safe.
session_in_use() {
  [[ -n "$1" ]] || return 1
  pgrep -f -- "$1" >/dev/null 2>&1
}

# Fail early when ids cannot be derived at all: --continue is the one mode that
# is meaningless without them.
require_session_ids() {
  [[ -n "$(pr_session_id 0)" ]] && return 0
  echo "error: --continue needs md5 or md5sum to derive session ids; neither found" >&2
  exit 1
}

# Decide how PR $1 attaches to a session, setting $session_id, $session_flag
# ("--session-id UUID", "--resume UUID", or empty) and $session_resume (0/1).
#
# Without --continue an existing session gets no flag at all and claude
# allocates a fresh id, which keeps the default behavior unchanged: reusing a
# taken id is a hard error, and quietly resuming would be a surprise.
# shellcheck disable=SC2034  # all three are read by the entry point
plan_session() {
  local n="$1"
  session_id="$(pr_session_id "$n")"
  session_resume=0
  session_flag=""
  if session_exists "$session_id"; then
    if [[ "$continue_sessions" -eq 1 ]]; then
      if session_in_use "$session_id"; then
        printf 'note: PR #%s has a review session open in another tab or process; reviewing fresh\n' "$n" >&2
      else
        session_resume=1
        session_flag="--resume $session_id"
      fi
    fi
  elif [[ -n "$session_id" ]]; then
    session_flag="--session-id $session_id"
  fi
}
