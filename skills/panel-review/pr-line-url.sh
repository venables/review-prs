#!/usr/bin/env bash
# Compute a GitHub PR file-line anchor URL for a given path:line, so the panel
# synthesizer can wrap every `file:line` reference in the summary as a tappable
# markdown link.
#
# GitHub's per-file PR diff anchor is `diff-<sha256-of-relative-path>`. Append
# `R<n>` for the new (right-side) line in the diff, or `L<n>` for the old/left
# side. This script emits the right-side anchor, which is what reviewers want
# when pointing at code introduced or modified by the PR.
#
# If GitHub ever changes the anchor format, the only thing to update is the
# final printf below — the SHA-256-of-path computation is general.
#
# Usage:
#   pr-line-url.sh <pr-url> <file-path> <line-or-range>
#
# Examples:
#   pr-line-url.sh https://github.com/owner/repo/pull/27 src/app.ts 42
#   pr-line-url.sh https://github.com/owner/repo/pull/27 src/app.ts 42-58
#
# Outputs the URL to stdout. Exits 1 on usage or environment errors.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: $0 <pr-url> <file-path> <line-or-range>" >&2
  exit 1
fi

pr_url="$1"
file_path="$2"
line_spec="$3"

# Strip a trailing slash so we don't produce '...pull/27//files'.
pr_url="${pr_url%/}"

# GitHub's anchor addresses a single line. For ranges (`42-58`) take the start
# line; tapping scrolls the file viewer to that line and the rest of the range
# is visible inline.
start_line="${line_spec%%-*}"

# `printf '%s'` writes the path bytes with no trailing newline, so the hash
# matches what GitHub computes server-side. Pick whichever sha256 tool exists:
# macOS ships `shasum` (Perl-based); most Linux distros ship `sha256sum`.
if command -v shasum >/dev/null 2>&1; then
  sha="$(printf '%s' "$file_path" | shasum -a 256 | awk '{print $1}')"
elif command -v sha256sum >/dev/null 2>&1; then
  sha="$(printf '%s' "$file_path" | sha256sum | awk '{print $1}')"
else
  echo "pr-line-url.sh: need shasum or sha256sum on PATH" >&2
  exit 1
fi

printf '%s/files#diff-%sR%s\n' "$pr_url" "$sha" "$start_line"
