#!/usr/bin/env bash
# Fetch every review thread on a PR — resolved ones included — as one JSON
# document, annotated with the flags a re-check needs to decide what to touch.
#
# Why GraphQL: the REST `pulls/N/comments` endpoint does not expose
# `isResolved` per-thread, and reconstructing reply chains from a flat
# comment list is fiddly. GraphQL gives threads with resolution state and
# reply chains directly.
#
# Why resolved threads are included by default (unlike pr-comment-handler's
# fetch): recheck-pr must know a thread is already resolved so it can leave
# it entirely alone. Filtering them out here would make "already resolved"
# indistinguishable from "thread not found".
#
# Output shape (stdout, single JSON object):
#   {
#     "pr": { "url": "...", "number": N, "title": "...", "head_sha": "..." },
#     "viewer": "matt",
#     "threads": [
#       {
#         "thread_id": "PRRT_...",
#         "is_resolved": false,
#         "is_outdated": false,
#         "path": "src/foo.ts",
#         "line": 42,
#         "start_line": null,
#         "side": "RIGHT",
#         "mine": true,          # viewer authored the FIRST comment (the finding)
#         "has_reply": true,     # at least one comment after the first
#         "last_author": "author-login",
#         "comments": [
#           {
#             "node_id": "PRRC_...",
#             "database_id": 1234567890,
#             "author": "matt",
#             "is_bot": false,
#             "body": "...",
#             "created_at": "...",
#             "url": "..."
#           }
#         ]
#       }
#     ]
#   }
#
# The three computed flags map directly onto recheck-pr's thread rules:
#   mine=false        -> never resolve; not your thread to close
#   is_resolved=true  -> leave alone entirely
#   has_reply=false   -> a confirmation reply is worth adding before resolving
#   last_author=viewer-> don't stack another reply under your own
#
# `database_id` on the FIRST comment is what the REST replies endpoint wants:
#   POST /repos/{owner}/{repo}/pulls/{N}/comments/{database_id}/replies
# `thread_id` is what the resolveReviewThread GraphQL mutation wants.
#
# Usage:
#   fetch_pr_threads.sh <pr-number-or-url> [--unresolved-only]
#
# Requires: gh, jq.

set -euo pipefail

if [[ $# -lt 1 ]]; then
  echo "usage: $0 <pr-number-or-url> [--unresolved-only]" >&2
  exit 1
fi

pr_ref="$1"
shift

unresolved_only=false
while [[ $# -gt 0 ]]; do
  case "$1" in
    --unresolved-only) unresolved_only=true ;;
    *) echo "unknown flag: $1" >&2; exit 1 ;;
  esac
  shift
done

for tool in gh jq; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    echo "fetch_pr_threads.sh: missing required tool: $tool" >&2
    exit 1
  fi
done

pr_json="$(gh pr view "$pr_ref" --json number,url,title,headRefOid 2>/dev/null || true)"
if [[ -z "$pr_json" ]]; then
  echo "fetch_pr_threads.sh: could not resolve PR '$pr_ref' via gh" >&2
  exit 1
fi

pr_number="$(jq -r '.number' <<<"$pr_json")"
pr_url="$(jq -r '.url' <<<"$pr_json")"
pr_title="$(jq -r '.title' <<<"$pr_json")"
pr_head_sha="$(jq -r '.headRefOid' <<<"$pr_json")"

# Owner/repo come from the URL — handles forks correctly (the base repo owns
# the PR number, not the head repo).
owner_repo="$(printf '%s' "$pr_url" | sed -E 's|https?://github.com/([^/]+)/([^/]+)/pull/.*|\1/\2|')"
owner="${owner_repo%/*}"
repo="${owner_repo#*/}"

read -r -d '' query <<'GRAPHQL' || true
query($owner: String!, $repo: String!, $pr: Int!) {
  viewer { login }
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $pr) {
      reviewThreads(first: 100) {
        nodes {
          id
          isResolved
          isOutdated
          path
          line
          startLine
          diffSide
          comments(first: 50) {
            nodes {
              id
              databaseId
              author { login }
              body
              createdAt
              url
            }
          }
        }
      }
    }
  }
}
GRAPHQL

raw="$(gh api graphql \
  -f query="$query" \
  -F owner="$owner" \
  -F repo="$repo" \
  -F pr="$pr_number")"

jq \
  --argjson unresolved_only "$unresolved_only" \
  --arg pr_url "$pr_url" \
  --arg pr_title "$pr_title" \
  --arg pr_head_sha "$pr_head_sha" \
  --argjson pr_number "$pr_number" '
  def is_bot(login):
    (login | endswith("[bot]"))
    or (login | ascii_downcase | IN(
      "dependabot", "coderabbitai", "copilot-pull-request-reviewer",
      "github-actions", "renovate", "snyk-bot"
    ));

  .data.viewer.login as $viewer
  | {
      pr: {
        url: $pr_url,
        number: $pr_number,
        title: $pr_title,
        head_sha: $pr_head_sha
      },
      viewer: $viewer,
      threads: (
        .data.repository.pullRequest.reviewThreads.nodes
        | map(select(($unresolved_only | not) or (.isResolved | not)))
        | map(
            (.comments.nodes | map({
              node_id: .id,
              database_id: .databaseId,
              author: (.author.login // "ghost"),
              is_bot: is_bot(.author.login // "ghost"),
              body: .body,
              created_at: .createdAt,
              url: .url
            })) as $comments
            | {
                thread_id: .id,
                is_resolved: .isResolved,
                is_outdated: .isOutdated,
                path: .path,
                line: .line,
                start_line: .startLine,
                side: .diffSide,
                mine: (($comments | first | .author) == $viewer),
                has_reply: (($comments | length) > 1),
                last_author: ($comments | last | .author),
                comments: $comments
              }
          )
      )
    }
' <<<"$raw"
