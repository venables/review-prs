# shellcheck shell=bash
# Fetching, ranking and selecting the PRs to review, shared by review-prs and
# autoreview.
#
# Reads these globals, which the entry point sets from its own flags:
#   $owner $name $me            (lib/repo.sh)
#   $include_approved $include_dependabot $auto $continue_sessions
# Sets $prs_json, $sorted, $resumable_any and the $numbers array.
#
# Sourced, never executed: the entry point owns `set -euo pipefail`.

# Author logins treated as bots: hidden unless --dependabot, dimmed when shown.
# Extend this anchored alternation as more AI coding bots appear, e.g.
#   bot_login_re='^(dependabot|renovate|copilot)'
bot_login_re='^dependabot'

# --- Fetch open non-draft PRs + activity in one GraphQL call --------------
# shellcheck disable=SC2034  # $prs_json is consumed by build_rows
fetch_prs() {
  prs_json="$(gh api graphql \
    -F owner="$owner" -F name="$name" \
    -f query='
      query($owner:String!, $name:String!) {
        repository(owner:$owner, name:$name) {
          pullRequests(states:OPEN, first:50, orderBy:{field:UPDATED_AT, direction:DESC}) {
            nodes {
              number
              title
              isDraft
              updatedAt
              reviewDecision
              author { login }
              comments(last:100) { nodes { author { login } updatedAt } }
              reviews(last:100)  { nodes { author { login } submittedAt } }
              commits(last:100)  { nodes { commit { committedDate author { user { login } } } } }
            }
          }
        }
      }' \
    --jq '.data.repository.pullRequests.nodes | map(select(.isDraft == false))')"

  # Always hide your own PRs -- these tools are for reviewing others' work.
  prs_json="$(jq --arg me "$me" 'map(select((.author.login // "") != $me))' <<<"$prs_json")"

  # Hide bot PRs (Dependabot et al.) unless --dependabot was passed.
  if [[ "$include_dependabot" -eq 0 ]]; then
    prs_json="$(jq --arg botre "$bot_login_re" \
      'map(select((.author.login // "") | test($botre) | not))' <<<"$prs_json")"
  fi

  # Filter out APPROVED unless --all was passed.
  if [[ "$include_approved" -eq 0 ]]; then
    prs_json="$(jq 'map(select(.reviewDecision != "APPROVED"))' <<<"$prs_json")"
  fi

  local count
  count="$(jq 'length' <<<"$prs_json")"
  if [[ "$count" -eq 0 ]]; then
    local hint=""
    [[ "$include_approved"   -eq 0 ]] && hint+=" --all (approved)"
    [[ "$include_dependabot" -eq 0 ]] && hint+=" --dependabot (bots)"
    if [[ -n "$hint" ]]; then
      printf 'no matching open PRs; try:%s\n' "$hint"
    else
      echo "no open non-draft PRs"
    fi
    exit 0
  fi
}

# --- Build display rows ---------------------------------------------------
# jq emits "RANK\tUPDATED_AT\tBOT\t#NUM\tENGAGE\tREVIEW\tTIME\t@AUTHOR\tTITLE",
# then we sort by rank+recency and strip the two leading sort columns. The BOT
# flag (0/1) rides along as the first surviving column so we can dim bot rows
# after alignment without re-deriving the author.
build_rows() {
  local now_iso
  now_iso="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  sorted="$(jq -r --arg me "$me" --arg now "$now_iso" --arg botre "$bot_login_re" '
    def rel(ts):
      ((($now | fromdateiso8601) - (ts | fromdateiso8601)) | floor) as $s
      | if   $s < 60     then "\($s)s ago"
        elif $s < 3600   then "\(($s/60)   | floor)m ago"
        elif $s < 86400  then "\(($s/3600) | floor)h ago"
        else                  "\(($s/86400)| floor)d ago"
        end;

    .[]
    | . as $pr
    | (
        ($pr.comments.nodes // [] | map({at: .updatedAt,            who: (.author.login // "")})) +
        ($pr.reviews.nodes  // [] | map({at: .submittedAt,          who: (.author.login // "")})) +
        ($pr.commits.nodes  // [] | map({at: .commit.committedDate, who: (.commit.author.user.login // "")}))
      ) as $events
    | ($events | map(select(.at != null and .who == $me)) | map(.at) | sort | last) as $mine
    | ($events | map(select(.at != null and .who != $me)) | map(.at) | sort | last) as $other
    | (
        if   $mine  == null                     then "NEW"
        elif $other != null and $other > $mine  then "UPDATED"
        else                                         "SEEN"
        end
      ) as $engage
    | (
        if   $engage == "NEW"     then 0
        elif $engage == "UPDATED" then 1
        else                           2
        end
      ) as $rank
    | (
        if   .reviewDecision == "CHANGES_REQUESTED" then "CHANGES"
        elif .reviewDecision == "APPROVED"          then "APPROVED"
        else                                             "-"
        end
      ) as $review
    | (if (.author.login // "") | test($botre) then 1 else 0 end) as $bot
    | "\($rank)\t\(.updatedAt)\t\($bot)\t#\(.number)\t\($engage)\t\($review)\t\(rel(.updatedAt))\t@\(.author.login // "ghost")\t\(.title)"
  ' <<<"$prs_json" | sort -t $'\t' -k1,1n -k2,2r | cut -f3-)"
}

# --- Auto mode: no picker -------------------------------------------------
# Take every NEW/UPDATED PR. SEEN PRs are skipped on purpose -- nothing has
# changed since you last engaged, so an automated sweep has no reason to
# re-review them. $sorted columns are tab-separated:
# 1=bot 2=#NUM 3=engage 4=review 5=time 6=author 7=title.
select_auto() {
  numbers=()
  local n
  while IFS= read -r n; do
    [[ -z "$n" ]] && continue
    numbers+=("$n")
  done < <(awk -F'\t' '$3=="NEW" || $3=="UPDATED" { sub(/^#/, "", $2); print $2 }' <<<"$sorted")

  if [[ "${#numbers[@]}" -eq 0 ]]; then
    echo "no NEW or UPDATED PRs to auto-review"
    exit 0
  fi
  printf 'auto-reviewing %d PR(s): %s\n' "${#numbers[@]}" "$(printf '#%s ' "${numbers[@]}")"
}

# Mark the PRs that already have a local review session, so you can see what
# --continue would resume before you pick. The marker goes in after REVIEW,
# leaving the columns select_auto reads (2=#NUM, 3=engage) where they are. The
# column only appears when something is resumable: on a repo you have never
# reviewed, every row would read "-" and buy nothing.
#
# Marking costs one hash and one glob per PR, so skip the whole loop when no
# session store exists -- there is nothing to find, and a box without Claude
# Code should not pay for the lookup on every picker run.
mark_resumable() {
  resumable_any=0
  [[ -d "$claude_projects_dir" ]] || return 0
  local marked="" bot num engage review rest mark
  while IFS=$'\t' read -r bot num engage review rest; do
    [[ -n "$num" ]] || continue
    if session_exists "$(pr_session_id "${num#\#}")"; then
      mark="RESUMABLE"
      resumable_any=1
    else
      mark="-"
    fi
    marked+="$bot"$'\t'"$num"$'\t'"$engage"$'\t'"$review"$'\t'"$mark"$'\t'"$rest"$'\n'
  done <<<"$sorted"
  if [[ "$resumable_any" -eq 1 ]]; then
    sorted="${marked%$'\n'}"
  fi
}

# --- Picker ---------------------------------------------------------------
run_picker() {
  # Only the picker needs gum, so an --auto sweep runs on a box without it.
  require_deps gum

  # Surviving columns: "BOT\t#NUM\t...\tTITLE". Align everything but the BOT
  # flag, then dim bot rows (256-color gray 245) so they read as lower-priority
  # noise. paste re-zips the flags with the aligned rows, preserving order.
  local dim=$'\033[38;5;245m'
  local reset=$'\033[0m'
  local aligned display legend legend_text selected line
  aligned="$(cut -f2- <<<"$sorted" | column -t -s $'\t')"
  display="$(paste -d $'\t' <(cut -f1 <<<"$sorted") <(printf '%s\n' "$aligned") \
    | while IFS=$'\t' read -r bot row; do
        if [[ "$bot" == "1" ]]; then
          printf '%s%s%s\n' "$dim" "$row" "$reset"
        else
          printf '%s\n' "$row"
        fi
      done)"

  legend_text='NEW = unreviewed by you   UPDATED = activity since your last comment   SEEN = nothing new   CHANGES = changes requested'
  if [[ "$resumable_any" -eq 1 ]]; then
    if [[ "$continue_sessions" -eq 1 ]]; then
      legend_text+='   RESUMABLE = earlier review session, resumed by -C'
    else
      legend_text+='   RESUMABLE = earlier review session; pass -C to resume it'
    fi
  fi
  [[ "$include_dependabot" -eq 1 ]] && legend_text+='   (dimmed = Dependabot)'
  legend="$(gum style --foreground 240 "$legend_text")"

  selected="$(printf '%s\n' "$display" \
    | gum choose --no-limit \
        --header "Pick PRs to review (space toggles, enter confirms)
$legend" \
        --height 20 \
    || true)"

  if [[ -z "${selected// /}" ]]; then
    echo "no PRs selected"
    exit 0
  fi

  numbers=()
  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    if [[ "$line" =~ \#([0-9]+) ]]; then
      numbers+=("${BASH_REMATCH[1]}")
    fi
  done <<<"$selected"

  if [[ "${#numbers[@]}" -eq 0 ]]; then
    echo "error: could not parse PR numbers from selection" >&2
    exit 1
  fi
}

# Fill the $numbers array with the PRs to review, by sweep or by picker.
select_prs() {
  fetch_prs
  build_rows
  if [[ "$auto" -eq 1 ]]; then
    select_auto
  else
    mark_resumable
    run_picker
  fi
}

# Why a babysit loop should stop watching a PR, or empty while it should keep
# watching. Approval is the expected end, but a PR that was closed, or merged
# without ever collecting an approving review, is just as finished -- and
# waiting for an approval it will never get would re-review it on every interval
# for as long as the process lives.
#
# One call for both facts, since this runs per PR per interval. A failed lookup
# yields neither, which reads as "keep waiting".
pr_babysit_done() {
  local json state decision
  json="$(gh pr view "$1" --json state,reviewDecision 2>/dev/null)" || return 0
  state="$(jq -r '.state // ""' <<<"$json" 2>/dev/null || true)"
  decision="$(jq -r '.reviewDecision // ""' <<<"$json" 2>/dev/null || true)"
  if [[ "$decision" == "APPROVED" ]]; then
    printf 'approved'
  elif [[ -n "$state" && "$state" != "OPEN" ]]; then
    printf '%s' "$(printf '%s' "$state" | tr '[:upper:]' '[:lower:]')"
  fi
}
