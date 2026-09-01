#!/usr/bin/env bash
# panel-review.sh — fan a code review out to multiple local CLI agents in parallel
#                   and print their raw outputs for the coordinator to synthesize.
#
# Each panelist is one `dash-p -H <backend> ...` subprocess with no shared state.
# dash-p is one uniform non-interactive interface over claude / codex / opencode,
# so this script never builds a per-backend command line — it passes generic flags
# (-H, --model, --cwd, --perms, --dangerously-skip-permissions, --timeout) and
# dash-p maps them onto each CLI's native argv. Captured outputs land in a tempdir;
# the path is printed at the end.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# Two prompt templates:
#   review.md     - mode-agnostic base prompt for diff-embedded targets
#                   (uncommitted/staged/base/commit). The script appends a
#                   `## Workspace` section that tells the panelist what
#                   tool capabilities they actually have in this run.
#   review-pr.md  - PR-specific instruction prompt; panelists run gh pr view
#                   / gh pr diff / gh api comments themselves rather than
#                   getting an embedded diff. Worktree section is also
#                   appended by the script.
PROMPT_TEMPLATE="$SCRIPT_DIR/prompts/review.md"
PROMPT_TEMPLATE_PR="$SCRIPT_DIR/prompts/review-pr.md"
APPROACHES_DIR="$SCRIPT_DIR/prompts/approaches"   # one <name>.md fragment per review approach

# ----- Defaults -----
TARGET="uncommitted"           # uncommitted | staged | base:<ref> | commit:<sha> | pr:<ref>
TARGET_EXPLICIT=0              # set to 1 by any --uncommitted/--staged/--base/--commit/--pr flag
FOCUS=""
RUN_APPROACH=""                # default review approach (empty = standard); per-panelist /approach overrides it
OUT_DIR=""
TIMEOUT_SECS="${PANEL_REVIEW_TIMEOUT:-600}"
MAX_DIFF_BYTES="${PANEL_REVIEW_MAX_DIFF_BYTES:-200000}"
CHECKOUT_MODE=0
CHECKOUT_REQUESTED=0            # set to 1 by the deprecated --checkout flag, used to
                                # detect --checkout combined with a target that no
                                # longer supports a worktree (uncommitted/staged)
INSTRUCTION_MODE=0             # 1 when target is a PR; panelists fetch via gh themselves

# ----- Per-panelist model overrides (env) -----
CODEX_MODEL="${CODEX_MODEL:-}"
CLAUDE_MODEL="${CLAUDE_MODEL:-}"
OPENCODE_MODEL="${OPENCODE_MODEL:-}"

# ----- dash-p driver (env) -----
# Every panelist is invoked as `dash-p -H <backend> ...`. dash-p is the single
# uniform interface over the backend CLIs, so this script passes generic flags and
# lets dash-p build each CLI's native argv. Point DASHP_BIN at a specific build
# (e.g. a release binary not yet on PATH). Override the underlying claude binary
# with dash-p's own DASHP_CLAUDE_BIN.
DASHP_BIN="${DASHP_BIN:-dash-p}"

# ----- Panelist registry (parallel arrays; bash 3.2 has no associative arrays) -----
#
# A "panelist" is one reviewer instance. Multiple instances of the SAME backend
# are allowed (e.g. two opencode panelists on different models), so each panelist
# carries three independent attributes:
#
#   - ID:      unique handle used for filesystem paths (worktree-$id, $id.out),
#              dedup, section headers, and todo/heartbeat matching. Sanitized so
#              it is safe to interpolate into paths and `git worktree add`.
#   - BACKEND: codex | claude | opencode — selects the dash-p -H harness and
#              which *_MODEL default applies.
#   - MODEL:   optional per-panelist model id, passed through to the CLI. Empty
#              means "fall back to the backend's *_MODEL env default (if any)".
PANEL_IDS=()
PANEL_BACKENDS=()
PANEL_MODELS=()
PANEL_APPROACHES=()

# Sanitize an arbitrary string (e.g. a model id) into an id fragment safe for
# filesystem paths and `git worktree add`. Everything outside [A-Za-z0-9._-] —
# including '/' from provider/model ids like 'anthropic/claude' — collapses to
# '-', so the result can never contain a path separator and cannot traverse out
# of $OUT_DIR.
sanitize_id() {
  local s="$1"
  printf '%s' "${s//[^A-Za-z0-9._-]/-}"
}

# Print the index of the panelist with the given id, or return non-zero.
panel_index() {
  local id="$1" i
  for i in "${!PANEL_IDS[@]}"; do
    [[ "${PANEL_IDS[$i]}" == "$id" ]] && { printf '%s' "$i"; return 0; }
  done
  return 1
}

panel_backend()  { local i; i="$(panel_index "$1")" || return 1; printf '%s' "${PANEL_BACKENDS[$i]}"; }
panel_model()    { local i; i="$(panel_index "$1")" || return 1; printf '%s' "${PANEL_MODELS[$i]}"; }
panel_approach() { local i; i="$(panel_index "$1")" || return 1; printf '%s' "${PANEL_APPROACHES[$i]}"; }

# A review approach is valid iff a prompt fragment exists for it. Extension is
# therefore "drop a file in prompts/approaches/" — no code change.
validate_approach() {
  local a="$1" ctx="$2"
  # Constrain to a safe basename: no '/' means the value can never traverse out
  # of $APPROACHES_DIR (the `cat "$APPROACHES_DIR/$approach.md"` in build_argv) or
  # smuggle a path separator into the panelist id (register_panelist) it's later
  # folded into.
  [[ "$a" =~ ^[A-Za-z0-9._-]+$ ]] || \
    die "invalid approach '$a'${ctx:+ in $ctx} (allowed: letters, numbers, . _ -)"
  [[ -f "$APPROACHES_DIR/$a.md" ]] || die "unknown approach '$a'${ctx:+ in $ctx} (no prompts/approaches/$a.md)"
}

# Resolve the approach actually used for a panelist: its own /approach if set,
# else the run-level --approach default (which may be empty = standard review).
effective_approach() {
  local a; a="$(panel_approach "$1")"
  [[ -n "$a" ]] && { printf '%s' "$a"; return; }
  printf '%s' "$RUN_APPROACH"
}

# Resolve the model actually used for a panelist: the explicit per-panelist model
# if set, else the backend's *_MODEL env default (which may itself be empty, i.e.
# let the CLI pick its own default).
effective_model() {
  local id="$1" backend model
  backend="$(panel_backend "$id")"
  model="$(panel_model "$id")"
  if [[ -n "$model" ]]; then printf '%s' "$model"; return; fi
  case "$backend" in
    codex)    printf '%s' "$CODEX_MODEL" ;;
    claude)   printf '%s' "$CLAUDE_MODEL" ;;
    opencode) printf '%s' "$OPENCODE_MODEL" ;;
  esac
}

# Register a panelist, generating a unique id. With a model the id is
# '<backend>-<sanitized-model>' (so two opencode panelists get distinct dirs and
# headers); without a model it is just '<backend>'. Collisions get a numeric
# suffix.
register_panelist() {
  local backend="$1" model="$2" approach="$3" base id n
  if [[ -n "$model" ]]; then
    base="${backend}-$(sanitize_id "$model")"
  else
    base="$backend"
  fi
  # Fold the approach into the id so a holistic + decompose pair of the same
  # backend/model get distinct ids, dirs, and `## <id> / <model>` headers.
  [[ -n "$approach" ]] && base="${base}-${approach}"
  id="$base"
  n=2
  while panel_index "$id" >/dev/null 2>&1; do
    id="${base}-${n}"
    n=$((n + 1))
  done
  PANEL_IDS+=("$id")
  PANEL_BACKENDS+=("$backend")
  PANEL_MODELS+=("$model")
  PANEL_APPROACHES+=("$approach")
}

# Parse a panelist spec 'backend[/approach][:model]' and register it. The model
# is everything after the FIRST ':' (kept verbatim, including any '/' in
# provider/model ids — only the *id* derived from it is sanitized). The optional
# '/approach' lives in the pre-colon part, so a '/' inside a model id is never
# mistaken for an approach. A bare 'backend' uses the backend's *_MODEL env
# default and the standard review approach. Examples:
#   claude:opus-4.8                -> backend claude, model opus-4.8, standard
#   claude/decompose:opus-4.8      -> backend claude, model opus-4.8, decompose
#   opencode:openrouter/qwen-3.7   -> backend opencode, model openrouter/qwen-3.7
add_panelist_spec() {
  local spec="$1" left backend model approach
  if [[ "$spec" == *:* ]]; then left="${spec%%:*}"; model="${spec#*:}"; else left="$spec"; model=""; fi
  if [[ "$left" == */* ]]; then backend="${left%%/*}"; approach="${left#*/}"; else backend="$left"; approach=""; fi
  case "$backend" in
    codex|claude|opencode) ;;
    *) die "--panelist: unknown backend '$backend' in spec '$spec' (allowed: codex, claude, opencode)" ;;
  esac
  [[ -n "$approach" ]] && validate_approach "$approach" "spec '$spec'"
  register_panelist "$backend" "$model" "$approach"
}

usage() {
  cat <<EOF
Usage: panel-review.sh [target] [options]

Targets (pick one; default tries to auto-detect a PR for the current branch via
'gh pr view', falling back to --uncommitted):
  --uncommitted           Review staged + unstaged changes
  --staged                Review only staged changes
  --base BRANCH           Review BRANCH...HEAD
  --commit SHA            Review a single commit
  --pr NUMBER             Review a GitHub PR. Panelists fetch the diff and
                          existing review comments themselves via the 'gh' CLI
                          (no embedded diff in the prompt — eliminates stale-base
                          bugs and the MAX_DIFF_BYTES cap).

Options:
  --focus TEXT            Optional focus / context for the reviewers
  --approach NAME         Default review approach for all panelists that don't
                          set their own (see /approach below). NAME must have a
                          prompts/approaches/NAME.md fragment. Empty = standard
                          whole-diff review. e.g. --approach decompose makes the
                          whole panel review by chunking the diff + a seam pass.
  --panelist SPEC         Add a panelist (repeatable). SPEC is
                          'backend[/approach][:model]' where backend is codex,
                          claude, or opencode. The same backend may be used more
                          than once with different models or approaches — e.g.:
                            --panelist claude:opus-4.8 \\
                            --panelist opencode:qwen-3.7 \\
                            --panelist claude/decompose:opus-4.8
                          '/approach' tells that one panelist how to review
                          (overrides --approach); it must have a
                          prompts/approaches/<approach>.md fragment. A bare
                          'backend' uses that backend's *_MODEL env default and
                          the standard approach. If no --panelist is given, falls
                          back to \$PANEL_REVIEW_PANELISTS, then to auto-detecting
                          every supported CLI on PATH.
  --out-dir DIR           Where to write captured outputs (default: mktemp).
  --timeout SECS          Per-panelist timeout (default: \$PANEL_REVIEW_TIMEOUT or 600).
  -h, --help              Show this help.

Behavior by target:
  --uncommitted/--staged  Local diff embedded in prompt; panelists run from your
                          working tree with read-only / plan permissions.
  --pr/--base/--commit    Panelists run worktree-isolated with write/exec perms
                          (one throwaway git-worktree per panelist, pinned to the
                          target ref). They can grep callers, run tests, and
                          investigate downstream effects. The prompt forbids
                          state-changing network actions (push, gh writes,
                          installer side effects); the worktree is the only
                          enforcement layer.

  The legacy --checkout flag is now the default for committed targets and is
  accepted as a deprecated no-op.

Environment:
  PANEL_REVIEW_PANELISTS  Space- or comma-separated list of 'backend[:model]'
                          specs, used when no --panelist flag is given. Same
                          grammar as --panelist, so you can pick reviewers and
                          their models entirely from the environment, e.g.:
                            PANEL_REVIEW_PANELISTS="claude:opus-4.8 opencode:qwen-3.7 opencode:glm-5.2"
  CODEX_MODEL, CLAUDE_MODEL, OPENCODE_MODEL
                          Default model for a panelist of that backend whose spec
                          did not pin an explicit model (e.g. a bare --panelist
                          claude, or an auto-detected panelist).
  DASHP_BIN               The dash-p binary that drives every panelist (default:
                          dash-p on PATH). Override the underlying claude binary
                          with dash-p's own DASHP_CLAUDE_BIN.
  PANEL_REVIEW_MAX_DIFF_BYTES
                          Cap inline diff size (default 200000). Only applies to
                          diff-embed targets (uncommitted/staged/base/commit). PR
                          mode does not embed the diff and is not subject to this cap.
  PANEL_REVIEW_POLL_GRACE_SECS
                          Extra seconds beyond a panelist's --timeout before the
                          poll loop's wall-clock backstop force-fails a panelist
                          that never wrote a result (default 120). Covers dash-p
                          startup + teardown; raise it on slow machines.

Exit codes:
  0  every panelist returned successfully
  1  setup error (no diff, no panelists, missing template)
  2  one or more panelists failed; raw outputs still written
EOF
}

die() { echo "panel-review: $*" >&2; exit 1; }

# ----- Parse args -----
while [[ $# -gt 0 ]]; do
  case "$1" in
    --uncommitted) TARGET="uncommitted"; TARGET_EXPLICIT=1; shift ;;
    --staged)      TARGET="staged"; TARGET_EXPLICIT=1; shift ;;
    --base)        [[ $# -ge 2 ]] || die "--base needs a branch"; TARGET="base:$2"; TARGET_EXPLICIT=1; shift 2 ;;
    --commit)      [[ $# -ge 2 ]] || die "--commit needs a SHA"; TARGET="commit:$2"; TARGET_EXPLICIT=1; shift 2 ;;
    --pr)          [[ $# -ge 2 ]] || die "--pr needs a number or URL"; TARGET="pr:$2"; TARGET_EXPLICIT=1; shift 2 ;;
    --focus)       [[ $# -ge 2 ]] || die "--focus needs text"; FOCUS="$2"; shift 2 ;;
    --approach)    [[ $# -ge 2 ]] || die "--approach needs a name"; validate_approach "$2" "--approach"; RUN_APPROACH="$2"; shift 2 ;;
    --panelist)
      [[ $# -ge 2 ]] || die "--panelist needs a 'backend[:model]' spec"
      # add_panelist_spec validates the backend against the known set and derives
      # a sanitized, unique id. The id (not the raw spec) is what later gets
      # interpolated into filesystem paths (worktree-$id, $id.out, $id.rc) and
      # passed to git worktree add, so an injected model like '../foo' collapses
      # to a harmless 'opencode-..-foo' filename rather than escaping $OUT_DIR.
      add_panelist_spec "$2"
      shift 2 ;;
    --out-dir)     [[ $# -ge 2 ]] || die "--out-dir needs a path"; OUT_DIR="$2"; shift 2 ;;
    --timeout)     [[ $# -ge 2 ]] || die "--timeout needs seconds"; TIMEOUT_SECS="$2"; shift 2 ;;
    --checkout)
      # Deprecated: PR / --base / --commit reviews now always run worktree-
      # isolated with exec permissions. Accepting the flag as a no-op for those
      # targets so existing scripts / muscle memory don't break; will be removed
      # in a future release. Combined with --uncommitted/--staged the flag is an
      # error (see post-parse check below) — those targets cannot be worktree-
      # isolated, so silently dropping --checkout would give the user a
      # read-only review when they explicitly asked for deep mode.
      CHECKOUT_REQUESTED=1
      shift ;;
    -h|--help)     usage; exit 0 ;;
    *) die "unknown argument: $1 (use -h for help)" ;;
  esac
done

[[ -f "$PROMPT_TEMPLATE" ]] || die "missing prompt template at $PROMPT_TEMPLATE"

# ----- Auto-detect a PR for the current branch (only if no target was explicit) -----
#
# Why: when the user has a stale local main, a default `--base main` (or even
# `--uncommitted`) diff disagrees with what's actually in the PR on GitHub —
# panelists then flag commits that are already on the PR base as if they were
# part of "this change." `gh pr view` (no ref) returns the PR for the current
# branch when one exists, which is the source of truth a human reviewer would
# look at.
#
# Tiebreaker for dirty working trees: if the user has uncommitted changes and a
# PR exists, prefer reviewing the uncommitted work (active edits are usually
# what they want feedback on) and just log the PR's existence with the override
# hint. Clean tree + PR exists → switch to --pr mode silently.
if (( !TARGET_EXPLICIT )) && command -v gh >/dev/null 2>&1 && command -v git >/dev/null 2>&1; then
  if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    # `gh pr view` returns the most-recent PR for the branch regardless of
    # state (open/closed/merged). Without filtering on state we'd silently
    # auto-switch a fresh review to a stale closed/merged PR — confusing
    # and inconsistent with the README/SKILL.md which describe this as
    # detecting an "open PR". Fetch state alongside number and only
    # auto-switch when state is OPEN.
    { read -r auto_pr_state; read -r auto_pr_num; } < <(
      gh pr view --json state,number -q '.state, .number' 2>/dev/null || true
    )
    if [[ "$auto_pr_state" == "OPEN" && -n "$auto_pr_num" ]]; then
      has_uncommitted=0
      if ! git diff-index --quiet HEAD -- 2>/dev/null; then
        has_uncommitted=1
      elif [[ -n "$(git ls-files --others --exclude-standard 2>/dev/null)" ]]; then
        has_uncommitted=1
      fi
      if (( has_uncommitted )); then
        echo "panel-review: detected PR #$auto_pr_num for current branch, but reviewing uncommitted changes (you have local edits). Pass --pr $auto_pr_num to review the PR instead." >&2
      else
        echo "panel-review: detected PR #$auto_pr_num for current branch, using --pr mode. Pass --uncommitted to override." >&2
        TARGET="pr:$auto_pr_num"
      fi
    elif [[ -n "$auto_pr_num" ]]; then
      echo "panel-review: branch has PR #$auto_pr_num (state: $auto_pr_state) — not auto-switching. Pass --pr $auto_pr_num explicitly to review it anyway." >&2
    fi
  fi
fi

# Validate --checkout against the resolved target. For pr/base/commit it's a
# no-op (worktree is already the default). For uncommitted/staged it's an
# error: those targets have no committed ref to materialize a worktree from,
# so the previous behavior of silently dropping the flag would surprise users
# who explicitly requested deep mode. Telling them up-front lets them either
# commit/stash first or drop the flag.
if (( CHECKOUT_REQUESTED )); then
  case "$TARGET" in
    uncommitted|staged)
      die "--checkout cannot be combined with --$TARGET: there is no committed ref to materialize a worktree from. Either drop --checkout (you'll get a local read-only review) or commit/stash your changes first and re-run with --base or --commit."
      ;;
    *)
      echo "panel-review: --checkout is now the default for pr/base/commit targets and is a no-op; the flag will be removed in a future release." >&2
      ;;
  esac
fi

# Mode flags are derived from the resolved target — there is no separate
# user-facing "deep" / "checkout" toggle anymore:
#
#   - INSTRUCTION_MODE: the PR-fetch prompt (panelists run gh themselves
#     instead of getting an embedded diff). PR targets only.
#   - CHECKOUT_MODE: per-panelist throwaway worktree pinned to the target ref,
#     and panelists run with workspace-write / bypass permissions so they can
#     read code, grep callers, and run tests/build commands. Anything with a
#     real ref to materialize: pr / base / commit. uncommitted and staged
#     stay local-only because the changes don't yet exist as a ref.
case "$TARGET" in
  pr:*)            INSTRUCTION_MODE=1; CHECKOUT_MODE=1 ;;
  base:*|commit:*) CHECKOUT_MODE=1 ;;
esac

if (( CHECKOUT_MODE )); then
  command -v git >/dev/null 2>&1 || die "pr/base/commit reviews require git on PATH"
fi

if (( INSTRUCTION_MODE )); then
  [[ -f "$PROMPT_TEMPLATE_PR" ]] || die "missing PR prompt template at $PROMPT_TEMPLATE_PR"
  command -v gh >/dev/null 2>&1 || die "--pr requires the 'gh' CLI on PATH"
fi

# ----- Resolve panelists: --panelist flags > PANEL_REVIEW_PANELISTS env > auto-detect -----
# If no --panelist flag was given, fall back to the env list (same
# 'backend[:model]' grammar, space- or comma-separated). Word-splitting the
# unquoted expansion does the tokenizing; commas are normalized to spaces first.
if [[ ${#PANEL_IDS[@]} -eq 0 && -n "${PANEL_REVIEW_PANELISTS:-}" ]]; then
  # read -ra rather than unquoted word-splitting: a spec containing a glob
  # metacharacter ('*', '?', '[') would otherwise be pathname-expanded against
  # the cwd before parsing (the script never sets -f). Count-guard the loop so a
  # value that is only commas/whitespace yields an empty array without tripping
  # bash 3.2's empty-array @-expansion footgun under set -u.
  read -ra _panel_specs <<< "${PANEL_REVIEW_PANELISTS//,/ }"
  if (( ${#_panel_specs[@]} > 0 )); then
    for spec in "${_panel_specs[@]}"; do
      [[ -n "$spec" ]] && add_panelist_spec "$spec"
    done
  fi
fi

# Still nothing? Auto-detect every backend CLI on PATH. dash-p drives each one,
# but the underlying CLI still has to be installed, so probe the bare backend name.
# Auto-detected panelists carry no explicit model, so they inherit the backend's
# *_MODEL default (if any).
if [[ ${#PANEL_IDS[@]} -eq 0 ]]; then
  for tool in codex claude opencode; do
    command -v "$tool" >/dev/null 2>&1 && register_panelist "$tool" "" ""
  done
fi
[[ ${#PANEL_IDS[@]} -gt 0 ]] || die "no backend CLIs found on PATH (looked for codex, claude, opencode)"

# dash-p is the uniform driver for every panelist; without it nothing can run.
command -v "$DASHP_BIN" >/dev/null 2>&1 || \
  die "dash-p not found on PATH (looked for '$DASHP_BIN'). Install it with 'brew install venabots/tap/dash-p', or point DASHP_BIN at the binary (e.g. DASHP_BIN=~/dev/cli/dash-p/target/release/dash-p)."

# ----- Output dir -----
if [[ -z "$OUT_DIR" ]]; then
  OUT_DIR="$(mktemp -d -t panel-review-XXXXXX)"
else
  mkdir -p "$OUT_DIR"
fi

# ----- Build the diff (or, for PR targets, just the metadata) -----
DIFF_FILE="$OUT_DIR/diff.patch"
TARGET_LABEL=""
PR_BODY=""
pr_ref=""
pr_num=""
pr_title=""
pr_base=""
pr_repo=""
pr_url=""
case "$TARGET" in
  uncommitted)
    {
      git diff --cached --no-ext-diff
      git diff --no-ext-diff
    } > "$DIFF_FILE" || die "git diff failed"
    TARGET_LABEL="uncommitted changes (staged + unstaged)"
    ;;
  staged)
    git diff --cached --no-ext-diff > "$DIFF_FILE" || die "git diff --cached failed"
    TARGET_LABEL="staged changes"
    ;;
  base:*)
    base="${TARGET#base:}"
    git diff --no-ext-diff "$base"...HEAD > "$DIFF_FILE" || die "git diff $base...HEAD failed"
    TARGET_LABEL="changes vs base branch '$base'"
    ;;
  commit:*)
    sha="${TARGET#commit:}"
    git show --no-ext-diff "$sha" > "$DIFF_FILE" || die "git show $sha failed"
    TARGET_LABEL="commit $sha"
    ;;
  pr:*)
    # Instruction mode: don't pre-build the diff. Just resolve metadata once
    # so the prompt header has a useful title and panelists know exactly which
    # PR to fetch. Each panelist runs `gh pr diff` + comment APIs themselves
    # against GitHub, so they always see the current remote state — no risk of
    # the script's view drifting from what reviewers see in the GitHub UI.
    pr_ref="${TARGET#pr:}"
    gh_err="$OUT_DIR/gh-pr-view.err"
    { read -r pr_num; read -r pr_title; read -r pr_base; read -r pr_url; } < <(
      gh pr view "$pr_ref" \
        --json number,title,baseRefName,url \
        -q '.number, .title, .baseRefName, .url' \
        2>"$gh_err" || true
    )
    if [[ -z "$pr_num" || -z "$pr_url" ]]; then
      msg="--pr: failed to resolve PR metadata via 'gh pr view $pr_ref'"
      [[ -s "$gh_err" ]] && msg+=$'\n  gh stderr: '"$(cat "$gh_err")"
      die "$msg"
    fi
    # gh pr view does not expose baseRepository as a top-level json field;
    # parse owner/repo out of the canonical PR URL instead. Works for github.com
    # and GitHub Enterprise (the host portion is preserved up to `/owner/repo/pull/N`).
    pr_repo="$(echo "$pr_url" | sed -E 's|^https?://[^/]+/||; s|/pull/.*$||')"
    if [[ -z "$pr_repo" || "$pr_repo" == "$pr_url" ]]; then
      die "--pr: could not parse owner/repo from PR url '$pr_url'"
    fi
    PR_BODY="$(gh pr view "$pr_ref" --json body -q .body 2>/dev/null || true)"
    TARGET_LABEL="PR #${pr_num}"
    [[ -n "$pr_title" ]] && TARGET_LABEL+=" — $pr_title"
    [[ -n "$pr_base"  ]] && TARGET_LABEL+=" (base: $pr_base)"
    ;;
esac

# Diff-existence check only applies to embed targets. PR mode has no diff file.
if (( !INSTRUCTION_MODE )); then
  [[ -s "$DIFF_FILE" ]] || die "diff is empty for target: $TARGET"
fi

# ----- Optional: materialize one worktree per panelist for deep-mode -----
#
# Why one worktree per panelist (CI matrix style): in --checkout mode, panelists
# run real test suites and may edit files as part of investigation. Sharing one
# worktree across N parallel panelists invites:
#   - test runners racing on lockfiles / build dirs (target/, node_modules/.cache,
#     .next/, dist/) — flaky failures unrelated to the diff
#   - one panelist's edits leaking into another's review state, breaking the
#     "independent observers" guarantee the skill is built around
#   - one panelist's `pnpm install` corrupting another's run if it dies mid-write
# Disk cost is N × repo, but pnpm/cargo/npm caches are shared at the user level so
# most of the bytes are hardlinks. Cleanup loops in the EXIT trap so nothing leaks
# if the script is killed.
declare -a WORKTREES=()
if (( CHECKOUT_MODE )); then
  echo "panel-review: --checkout: materializing one worktree per panelist under $OUT_DIR" >&2

  # Resolve a single commit SHA every worktree will pin to. One fetch (for --pr),
  # then N cheap local checkouts — avoids gh-pr-checkout's branch-naming conflict
  # when worktree #2 tries to claim the same local branch as worktree #1.
  WORKTREE_REF=""
  case "$TARGET" in
    pr:*)
      [[ -n "${pr_num:-}" ]] || die "--pr --checkout: could not resolve PR number"
      # Resolve everything from pr_ref directly. A bare `gh repo view` would
      # return the cwd's default repo, which can disagree with pr_ref when
      # pr_ref is a URL pointing at a different repo (e.g. running from a fork
      # clone but reviewing an upstream PR). Using gh pr view "$pr_ref" keeps
      # repo context end-to-end. Single call returns three lines via jq's
      # comma operator; bash-3.2-compatible read; read; read consumes them.
      # Capture gh's stderr to a file rather than swallowing it — auth errors,
      # network blips, and missing-headRepository (deleted-fork PRs) all need
      # to surface in the die message instead of collapsing to a generic
      # "failed to resolve" line.
      gh_err="$OUT_DIR/gh-pr-view.err"
      { read -r pr_url; read -r pr_head_sha; read -r pr_head_nwo; } < <(
        gh pr view "$pr_ref" --json url,headRefOid,headRepository \
                  -q '.url, .headRefOid, .headRepository.nameWithOwner' 2>"$gh_err" || true
      )
      # Before falling back to remote fetch, see if the head SHA is already in
      # our local object DB. Common case: reviewer is on the PR branch and
      # local HEAD == PR head, so a fetch is just a network round-trip to
      # confirm what we already have. The shortcut also sidesteps any
      # gh-pr-view quirks (empty nameWithOwner returned for some same-repo PRs,
      # deleted-fork PRs whose head SHA was already mirrored locally) — we
      # only need a remote URL when the object is actually missing.
      if [[ -n "$pr_head_sha" ]] && git cat-file -e "${pr_head_sha}^{commit}" 2>/dev/null; then
        echo "panel-review: --pr: head SHA ${pr_head_sha} already in local repo, skipping fetch" >&2
        WORKTREE_REF="$pr_head_sha"
      else
        # Need to fetch from the head repo. From here on, pr_head_nwo must be
        # a usable owner/repo string. jq prints the literal "null" (not empty)
        # for missing object fields when used with -q, so an emptiness check
        # alone misses the deleted-fork case — reject "null" explicitly.
        if [[ "$pr_head_nwo" == "null" ]]; then
          die "--pr: PR #${pr_num} head repository has been deleted (likely a deleted fork) and head SHA ${pr_head_sha:-(unknown)} is not in this local repo. Recovery: --base origin/${pr_base:-<base-branch>} (reviews against the PR's base branch) or --uncommitted (reviews your local edits)."
        fi
        if [[ -z "$pr_url" || -z "$pr_head_sha" || -z "$pr_head_nwo" ]]; then
          msg="--pr --checkout: failed to resolve PR url/SHA/head-repo via 'gh pr view ${pr_ref}'."
          msg+=$'\n  gh returned: url='"'${pr_url:-}'"' head_sha='"'${pr_head_sha:-}'"' head_nwo='"'${pr_head_nwo:-}'"
          [[ -s "$gh_err" ]] && msg+=$'\n  gh stderr: '"$(cat "$gh_err")"
          msg+=$'\n  Recovery: re-run with --base origin/'"${pr_base:-<base-branch>}"' to review against the PR'\''s base branch'
          [[ -n "$pr_head_sha" ]] && msg+=", or --commit ${pr_head_sha} if you have that commit locally"
          msg+=$'.'
          die "$msg"
        fi
        # Mirror origin's URL shape (SSH vs HTTPS) so the fetch uses whatever
        # auth this machine has already set up. Hardcoding HTTPS hangs on a
        # credential prompt for users with SSH-only auth and no HTTPS
        # credential helper. Falls back to HTTPS (host derived from pr_url so
        # GitHub Enterprise works) when origin is missing or in an
        # unrecognized shape.
        pr_host="$(echo "$pr_url" | sed -E 's|^(https?://[^/]+)/.*|\1|')"
        pr_head_https_url="${pr_host}/${pr_head_nwo}.git"
        origin_url="$(git remote get-url origin 2>/dev/null || true)"
        case "$origin_url" in
          ssh://*)
            # ssh://[user@]host[:port]/owner/repo[.git]
            ssh_authority="${origin_url#ssh://}"
            ssh_authority="${ssh_authority%%/*}"
            pr_head_url="ssh://${ssh_authority}/${pr_head_nwo}.git"
            ;;
          *://*)
            # https/http/git/file URL — use HTTPS fallback.
            pr_head_url="$pr_head_https_url"
            ;;
          *:*)
            # SCP-like SSH: [user@]host:path. The bare `host:path` form (no
            # user@) is common with ~/.ssh/config Host aliases like
            # `github-work:owner/repo.git`, so we don't require `@`. The
            # earlier *://* arm has already consumed every URL-form remote,
            # so any colon left here is the SCP separator.
            pr_head_url="${origin_url%%:*}:${pr_head_nwo}.git"
            ;;
          *)
            pr_head_url="$pr_head_https_url"
            ;;
        esac
        git fetch --quiet "$pr_head_url" "$pr_head_sha" >&2 \
          || die "git fetch $pr_head_url $pr_head_sha failed"
        WORKTREE_REF="$pr_head_sha"
      fi
      ;;
    base:*)
      WORKTREE_REF="$(git rev-parse HEAD)"
      ;;
    commit:*)
      WORKTREE_REF="${TARGET#commit:}"
      ;;
  esac

  # Echo resolved scope so the coordinator notices when reality diverges from
  # expectation. Canonical case: `--base main` against a local `main` that's
  # behind `origin/main` silently reviews extra commits not in the PR — the
  # scope line surfaces that the first time the script runs, rather than
  # after panelists return findings about unrelated code.
  scope_base_ref=""
  case "$TARGET" in
    pr:*)
      # Only echo a scope line when we can compute it locally. The PR's base
      # is on the remote (origin/<pr_base>); if the user hasn't fetched it,
      # skip rather than guess. Panelists fetch it themselves via gh.
      if [[ -n "${pr_base:-}" ]] && git rev-parse --verify "origin/${pr_base}" >/dev/null 2>&1; then
        scope_base_ref="origin/${pr_base}"
      fi
      ;;
    base:*)
      scope_base_ref="${TARGET#base:}"
      ;;
    commit:*)
      scope_base_ref="${WORKTREE_REF}^"
      ;;
  esac
  if [[ -n "$scope_base_ref" ]] && git rev-parse --verify "$scope_base_ref" >/dev/null 2>&1; then
    n_commits=$(git rev-list --count "${scope_base_ref}..${WORKTREE_REF}" 2>/dev/null || echo "?")
    shortstat=$(git diff --shortstat "${scope_base_ref}...${WORKTREE_REF}" 2>/dev/null | sed 's/^ *//')
    echo "panel-review: scope vs ${scope_base_ref}: ${n_commits} commits, ${shortstat:-no diff}" >&2
  fi

  # Register cleanup BEFORE the creation loop. If `git worktree add` fails partway
  # through (disk full, ref doesn't exist after a fetch race, etc.), die() exits
  # immediately — without the trap already in place, any worktrees added before
  # the failure would leak into .git/worktrees. The trap body iterates WORKTREES,
  # which is single-quoted and re-expanded at signal time, so it cleans up exactly
  # the dirs we managed to add (zero or more).
  # `${WORKTREES[@]:-}` would expand to a single empty element on bash 3.2 (the
  # macOS default) and produce a confusing error under `set -u`. Guard the loop
  # with a count check so the trap is a no-op when nothing has been added yet.
  trap '
    if (( ${#WORKTREES[@]} > 0 )); then
      for _wt in "${WORKTREES[@]}"; do
        [[ -n "$_wt" ]] && git worktree remove --force "$_wt" >/dev/null 2>&1 || true
      done
    fi
  ' EXIT

  for p in "${PANEL_IDS[@]}"; do
    wt="$OUT_DIR/worktree-$p"
    git worktree add --quiet --detach "$wt" "$WORKTREE_REF" >&2 \
      || die "git worktree add $wt $WORKTREE_REF failed"
    WORKTREES+=("$wt")
  done

  echo "panel-review: --checkout: ${#WORKTREES[@]} worktrees ready, panelists will run with WRITE/EXEC permissions in their own isolated checkouts" >&2
fi

# Diff-size cap only applies when we're embedding the diff in the prompt.
# Instruction-mode panelists fetch via gh and don't need the cap.
if (( !INSTRUCTION_MODE )); then
  DIFF_BYTES=$(wc -c < "$DIFF_FILE" | tr -d ' ')
  if (( DIFF_BYTES > MAX_DIFF_BYTES )); then
    die "diff is $DIFF_BYTES bytes, exceeds cap $MAX_DIFF_BYTES.
  Either narrow the scope (e.g. --commit, --staged, or a smaller --base range),
  or raise the cap, e.g.:  PANEL_REVIEW_MAX_DIFF_BYTES=$((DIFF_BYTES * 2)) bash $0 ...
  Caps exist because each panelist embeds the full diff in its prompt; very large
  diffs blow context windows and cost a lot. (PR mode bypasses this — panelists
  fetch the diff via gh themselves and never embed it.)"
  fi
fi

# ----- Compose the per-run prompt -----
#
# Two template paths:
#   - PR target:        prompts/review-pr.md  - instruction-style; panelists
#                                               run gh themselves, no diff embedded.
#   - everything else:  prompts/review.md     - mode-agnostic; the diff is
#                                               embedded and the script-appended
#                                               `## Workspace` section tells the
#                                               panelist what tools they can use.
ACTIVE_TEMPLATE="$PROMPT_TEMPLATE"
(( INSTRUCTION_MODE )) && ACTIVE_TEMPLATE="$PROMPT_TEMPLATE_PR"
PROMPT_FILE="$OUT_DIR/prompt.md"

# Substitute PR placeholders in the template body. We use bash parameter
# expansion rather than sed so values containing slashes (URLs, owner/repo)
# don't need escaping. Substitution happens whether or not the template uses
# the placeholders — non-PR templates just have nothing to replace.
TEMPLATE_BODY="$(cat "$ACTIVE_TEMPLATE")"
if (( INSTRUCTION_MODE )); then
  TEMPLATE_BODY="${TEMPLATE_BODY//\{\{PR_REF\}\}/$pr_ref}"
  TEMPLATE_BODY="${TEMPLATE_BODY//\{\{PR_NUMBER\}\}/$pr_num}"
  TEMPLATE_BODY="${TEMPLATE_BODY//\{\{PR_REPO\}\}/$pr_repo}"
  TEMPLATE_BODY="${TEMPLATE_BODY//\{\{PR_URL\}\}/$pr_url}"
fi

{
  printf '%s\n' "$TEMPLATE_BODY"
  echo
  echo "## Review target"
  echo
  echo "$TARGET_LABEL"
  if (( INSTRUCTION_MODE )); then
    echo
    echo "## PR identifiers (use these in your gh commands)"
    echo
    echo "- PR ref (pass to \`gh pr view\` / \`gh pr diff\`): \`$pr_ref\`"
    echo "- PR number: \`$pr_num\`"
    echo "- Repo (owner/name) for \`gh api\` calls: \`$pr_repo\`"
    [[ -n "$pr_url"  ]] && echo "- URL: $pr_url"
    [[ -n "$pr_base" ]] && echo "- Base branch: \`$pr_base\`"
  fi
  echo
  echo "## Workspace"
  echo
  if (( CHECKOUT_MODE )); then
    echo "You are running inside a dedicated git worktree pinned to this review's target ref"
    echo "— the actual checkout, not a free-floating diff. You may:"
    echo
    echo "- Read any file in the tree."
    echo "- Grep / rg across the tree to find callers of changed symbols."
    echo "- Edit files locally (the worktree is thrown away on exit)."
    echo "- Run build / test / lint commands to investigate downstream effects."
    echo "  Typical commands: \`pnpm test\` / \`npm test\` / \`cargo test\` / \`go test ./...\` /"
    echo "  \`pytest\` / \`bundle exec rspec\`; type checkers like \`tsc --noEmit\`, \`mypy\`,"
    echo "  \`cargo check\`. Check the repo's README / package.json / Makefile for the right one."
    echo "- Install dev dependencies if a test runner needs them; bound test runs to ~3 minutes."
    echo "- A failing test is a high-signal finding; surface it under Evidence."
    echo
    echo "Use that capability when it sharpens a finding. Do NOT do investigation theatre — only"
    echo "run tools when they harden the report. Local writes inside the worktree are fine; the"
    echo "external-network and GitHub-write rules in Hard Constraints still apply."
  else
    echo "You are running in the user's actual working tree with **read-only** access. You may:"
    echo
    echo "- Read any file using your built-in read tools (Read / Glob / Grep)."
    echo "- Reason about the diff and the surrounding code."
    echo
    echo "Do NOT modify files, run tests, install packages, or execute shell commands that"
    echo "change state. The \`Evidence:\` line in the finding shape is not meaningful in this"
    echo "mode — leave it out."
  fi
  if [[ -n "$PR_BODY" ]]; then
    echo
    echo "## PR description"
    echo
    echo "$PR_BODY"
  fi
  if [[ -n "$FOCUS" ]]; then
    echo
    echo "## Reviewer focus"
    echo
    echo "$FOCUS"
  fi
  if (( !INSTRUCTION_MODE )); then
    echo
    echo "## Diff"
    echo
    echo '```diff'
    cat "$DIFF_FILE"
    echo '```'
  fi
} > "$PROMPT_FILE"

# Read prompt once into memory so each child reads from there.
PROMPT_CONTENT="$(cat "$PROMPT_FILE")"

# Per-panelist timeouts are enforced by dash-p itself (--timeout SECS, exit 20
# on expiry), so there is no external timeout wrapper here.

# ----- Build each panelist's argv -----
#
# Every panelist is one `dash-p -H <backend> ...` invocation; dash-p translates
# the generic flags below into each CLI's native argv. Two permission tiers, keyed
# off CHECKOUT_MODE (set whenever the target is pr/base/commit — anything with a
# real ref to materialize):
#
#   Local mode  (uncommitted/staged): dash-p --perms read-only. Panelists run from
#               the user's working tree; dash-p maps the tier onto each CLI's own
#               mechanism (codex: an OS sandbox; claude: a write/exec tool denial
#               list; opencode: no auto-approve, so its default gating stands).
#               Do NOT hand-roll per-backend flags here — dash-p only forwards
#               unrecognised flags to claude, so a codex/opencode sandbox flag
#               passed that way is silently dropped.
#   Worktree mode (pr/base/commit):   dash-p --dangerously-skip-permissions.
#               Panelists run inside a throwaway per-panelist worktree (passed as
#               --cwd) pinned to the target ref and can read code, grep callers,
#               run tests/build commands. Network/destructive actions are gated by
#               the prompt only — no sandbox-level guarantee.
#
# Sets the global `argv` array (bash 3.2 has no clean way to return one).
build_argv() {
  local id="$1"
  local backend model approach prompt panel_cwd
  backend="$(panel_backend "$id")"
  model="$(effective_model "$id")"
  # Per-panelist prompt: the shared base, plus this panelist's approach fragment
  # (if any) appended as extra instructions on *how* to review.
  approach="$(effective_approach "$id")"
  prompt="$PROMPT_CONTENT"
  [[ -n "$approach" ]] && prompt="$PROMPT_CONTENT"$'\n\n'"$(cat "$APPROACHES_DIR/$approach.md")"

  # --output-format text so stdout is exactly the panelist's final message (its
  # first line is the mandated `Model:` line the synthesizer reads). dash-p owns
  # the per-panelist timeout and exits 20 on expiry, matching the rc handling in
  # print_section / panelist_error_reason.
  argv=("$DASHP_BIN" -H "$backend" --output-format text --timeout "$TIMEOUT_SECS")
  [[ -n "$model" ]] && argv+=(--model "$model")

  if (( CHECKOUT_MODE )); then
    panel_cwd="$OUT_DIR/worktree-$id"
    argv+=(--cwd "$panel_cwd" --dangerously-skip-permissions)
  else
    panel_cwd="$PWD"
    argv+=(--cwd "$panel_cwd" --perms read-only)
  fi

  # Feed the prompt on stdin, not as a positional argv element. A large embedded
  # diff can push the prompt past Linux's per-argument cap (MAX_ARG_STRLEN, 128KB)
  # and make exec fail with E2BIG (macOS has no such per-arg cap, which hides it
  # locally). dash-p reads the prompt from stdin when no positional prompt is
  # given, and stdin has no size limit — so we write it to a file the fan-out
  # redirects into the child's stdin, and pass NO prompt in argv.
  printf '%s' "$prompt" > "$OUT_DIR/$id.prompt" \
    || die "failed to write prompt file for panelist '$id' ($OUT_DIR/$id.prompt)"
  return 0
}

# ----- Fan out -----
echo "panel-review: target=$TARGET_LABEL panelists=${PANEL_IDS[*]} out=$OUT_DIR" >&2

declare -a PIDS=()
for p in "${PANEL_IDS[@]}"; do
  out="$OUT_DIR/$p.out"
  err="$OUT_DIR/$p.err"
  rc="$OUT_DIR/$p.rc"

  backend_bin="$(panel_backend "$p")"
  if ! command -v "$backend_bin" >/dev/null 2>&1; then
    echo "panel-review: '$p' backend CLI '$backend_bin' not on PATH — skipping" >&2
    : >"$out"
    echo "panelist '$p' backend CLI '$backend_bin' not found on PATH" >"$err"
    echo "127" >"$rc"
    continue
  fi

  if ! build_argv "$p"; then
    echo "panel-review: unknown panelist '$p' — skipping" >&2
    : >"$out"
    echo "unknown panelist '$p'" >"$err"
    echo "127" >"$rc"
    continue
  fi

  panel_cwd="$PWD"
  (( CHECKOUT_MODE )) && panel_cwd="$OUT_DIR/worktree-$p"
  # dash-p gets the working dir via --cwd (set in build_argv) and the prompt on
  # stdin from the per-panelist prompt file (see build_argv). The child (dash-p)
  # consumes that stdin itself; the backend it drives gets a null/PTY stdin, so a
  # backend that waits on an open stdin still proceeds.
  #
  # dash-p runs as an inner background job so we can record ITS pid ($p.apid)
  # and, on the wall-clock backstop, signal it directly: dash-p traps SIGTERM
  # and tears down its own backend process group, whereas TERMing only the wrapper
  # subshell would orphan a wedged dash-p + backend. The rc is written
  # atomically (tmp + mv) so the poll loop can never read or clobber a
  # half-written rc.
  ( "${argv[@]}" <"$OUT_DIR/$p.prompt" >"$out" 2>"$err" & apid=$!
    echo "$apid" >"$OUT_DIR/$p.apid"
    wait "$apid"; ec=$?
    echo "$ec" >"$OUT_DIR/$p.rc.tmp" && mv -f "$OUT_DIR/$p.rc.tmp" "$OUT_DIR/$p.rc" ) &
  child=$!
  PIDS+=("$child")
  # Record the wrapper pid so the poll loop can detect a panelist whose wrapper
  # died without writing its .rc (SIGKILL/OOM) and not wait on it forever.
  echo "$child" >"$OUT_DIR/$p.pid"
  echo "panel-review: ${p} started (pid=$child, cwd=$panel_cwd)" >&2
done

# ----- Stream combined results as each panelist finishes -----
#
# Why streaming: each panelist runs in parallel, but the slowest one (often codex)
# dominates wall clock. Printing sections only after `wait` returns means the
# coordinator (Claude or a human) sees nothing until the slowest finishes. Polling
# the per-panelist .rc files lets us print each section the moment it lands, and
# emit a stderr heartbeat so progress is visible when the script is run as a
# background Bash with BashOutput polling. stderr is unbuffered by libc; stdout
# may block-buffer when piped, so heartbeats go to stderr on purpose.
ANY_FAIL=0
echo "# Panel review"
echo
echo "- Target: $TARGET_LABEL"
echo "- Panelists: ${PANEL_IDS[*]}"
echo "- Outputs: \`$OUT_DIR\`"
# Surface the PR URL and repo for PR targets so the synthesizer can wrap
# `file:line` findings as tappable links via skills/panel-review/pr-line-url.sh
# without re-parsing the prompt file. Only emitted in PR mode.
[[ -n "$pr_url"  ]] && echo "- PR URL: $pr_url"
[[ -n "$pr_repo" ]] && echo "- PR Repo: $pr_repo"
[[ -n "$FOCUS" ]] && echo "- Focus: $FOCUS"
echo

# Extract the model id from a panelist's stdout. Each prompt instructs the
# panelist to print `Model: <id>` as the very first line of its output, so the
# script can label per-panelist sections / heartbeats with the actual model
# that produced the review (e.g. "## codex / gpt-5.5 (exit 0)") without having
# to introspect each CLI's default-model config.
#
# Falls back to the env var override if the panelist's first line isn't a
# recognisable Model: line, then to a literal "?". `head -n1` so we never scan
# beyond the first line — Model: appearing anywhere later in the output should
# not influence the header.
extract_model_label() {
  local p="$1"
  local fallback="$2"
  local first_line=""
  [[ -s "$OUT_DIR/$p.out" ]] && first_line="$(head -n1 "$OUT_DIR/$p.out" 2>/dev/null || true)"
  case "$first_line" in
    Model:*)
      local label="${first_line#Model:}"
      # Trim leading whitespace (single space is the common case after `Model:`).
      label="${label# }"
      label="${label#"${label%%[![:space:]]*}"}"
      [[ -n "$label" ]] && { echo "$label"; return; }
      ;;
  esac
  if [[ -n "$fallback" ]]; then
    echo "$fallback"
  else
    echo "?"
  fi
}

# Pull a one-line, human-readable failure reason out of a panelist's captured
# stderr, so an empty/failed panelist reports *why* instead of just a bare exit
# code. dash-p buffers the backend's own stderr and, on failure, prints its own
# `dash-p: <error>` line and exits 20 on timeout. Order: the timeout note, then
# the last non-empty stderr line.
strip_ansi() { sed $'s/\x1b\\[[0-9;]*[A-Za-z]//g'; }

panelist_error_reason() {
  local p="$1"
  local rc_val="$2"
  local errf="$OUT_DIR/$p.err"
  local reason=""
  # 20 is dash-p's own timeout exit; 124 is this script's wall-clock backstop
  # (see the poll loop below), which fires when dash-p itself never returned.
  if [[ "$rc_val" == "20" || "$rc_val" == "124" ]]; then
    reason="timed out (dash-p --timeout ${TIMEOUT_SECS}s, or orchestrator wall-clock backstop)"
  fi
  # Prefer the panelist's own stderr tail — dash-p prints `dash-p: <error>` on
  # failure, which is more specific than any exit-code guess.
  if [[ -z "$reason" && -s "$errf" ]]; then
    reason="$(strip_ansi <"$errf" 2>/dev/null | grep -v '^[[:space:]]*$' \
              | tail -n1 | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
  fi
  # Exit-code fallback only when stderr was empty (e.g. a hard kill left nothing).
  # Many signals collapse onto 137/143 here, so keep these generic.
  if [[ -z "$reason" ]]; then
    case "$rc_val" in
      137)     reason="wrapper exited without a result (signal/OOM?)" ;;
      143|130) reason="terminated by a signal before finishing" ;;
    esac
  fi
  printf '%s' "${reason:-no output and no error detail captured}"
}

print_section() {
  local p="$1"
  local rc_val
  rc_val="$(cat "$OUT_DIR/$p.rc" 2>/dev/null || echo "?")"
  # Fall back to the panelist's resolved model (explicit spec model, else the
  # backend's *_MODEL default) when the panelist did not self-report a Model:
  # line as its first output line.
  local fallback_model
  fallback_model="$(effective_model "$p")"
  local model_label
  model_label="$(extract_model_label "$p" "$fallback_model")"

  # A panelist that exits non-zero OR produces no stdout has failed to deliver a
  # review (the prompt mandates at least the Model:/Goal:/Approach:/Purpose:/
  # Proof: header lines, or NO_FINDINGS).
  # Treat both as failures so the run's exit code and the heartbeat stay honest —
  # an empty panelist on exit 0 (e.g. a swallowed provider error) is the silent
  # case this is meant to catch.
  local empty=0
  [[ -s "$OUT_DIR/$p.out" ]] || empty=1
  local failed=0
  { [[ "$rc_val" != "0" ]] || (( empty )); } && failed=1
  local reason=""
  (( failed )) && reason="$(panelist_error_reason "$p" "$rc_val")"

  echo "## ${p} / ${model_label} (exit ${rc_val})"
  echo
  if (( empty )); then
    echo "_(no stdout — panelist produced no review)_"
    [[ -n "$reason" ]] && { echo; echo "**Failure:** ${reason}"; }
  else
    cat "$OUT_DIR/$p.out"
  fi
  if (( failed )); then
    ANY_FAIL=1
    if [[ -s "$OUT_DIR/$p.err" ]]; then
      echo
      echo "<details><summary>stderr</summary>"
      echo
      echo '```'
      cat "$OUT_DIR/$p.err"
      echo '```'
      echo
      echo "</details>"
    fi
  fi
  echo
  # Keep the `done (exit N)` token intact — the panel-review and
  # bot-panel-review-loop skills poll stderr for it to mark a panelist complete,
  # so renaming it on failure would make the orchestrator wait forever. Append a
  # FAILED suffix (reason truncated) instead, so an empty/errored panelist is
  # loud rather than indistinguishable from a clean run.
  local hb="panel-review: ${p} (${model_label}) done (exit ${rc_val})"
  if (( failed )); then
    local short="$reason"
    [[ ${#short} -gt 120 ]] && short="${short:0:117}..."
    hb="${hb} — FAILED: ${short}"
  fi
  echo "$hb" >&2
}

# Track which panelists have already been printed. Bash 3.2 (macOS default) has
# no associative arrays, so we keep a parallel indexed array of names.
PRINTED=()
TOTAL=${#PANEL_IDS[@]}
DONE_COUNT=0
# Wall-clock backstop for the poll loop. A panelist is normally "done" when its
# child subshell writes .rc. Without the old external `timeout` wrapper, two edge
# cases could otherwise hang this loop forever: the subshell being SIGKILLed
# before it writes .rc, or dash-p wedging past its own --timeout. dash-p caps
# each panelist at TIMEOUT_SECS; the grace (PANEL_REVIEW_POLL_GRACE_SECS, default
# 120) covers its startup + teardown across parallel panelists. `SECONDS` is
# bash's seconds-since-start counter.
POLL_DEADLINE=$(( SECONDS + TIMEOUT_SECS + ${PANEL_REVIEW_POLL_GRACE_SECS:-120} ))
while (( DONE_COUNT < TOTAL )); do
  for p in "${PANEL_IDS[@]}"; do
    is_printed=0
    if [[ ${#PRINTED[@]} -gt 0 ]]; then
      for x in "${PRINTED[@]}"; do
        if [[ "$x" == "$p" ]]; then is_printed=1; break; fi
      done
    fi
    (( is_printed )) && continue
    if [[ ! -s "$OUT_DIR/$p.rc" ]]; then
      # No result yet. The wrapper writes .rc atomically as its final act, so an
      # absent .rc means the wrapper hasn't finished — which pins down both edge
      # cases below race-free: an alive wrapper implies its recorded pids are
      # still valid (no recycled-pid signal), and a dead wrapper's .rc state is
      # final (no clobber). Synthesize a failure rc if the wrapper vanished
      # without writing one, or if the wall-clock deadline passed; else wait.
      ppid=""
      [[ -s "$OUT_DIR/$p.pid" ]] && ppid="$(cat "$OUT_DIR/$p.pid" 2>/dev/null || true)"
      if [[ -n "$ppid" ]] && ! kill -0 "$ppid" 2>/dev/null; then
        # Wrapper fully exited; its .rc is now in final state.
        [[ -s "$OUT_DIR/$p.rc" ]] || echo "137" >"$OUT_DIR/$p.rc"   # gone without a result (SIGKILL/OOM)
      elif (( SECONDS > POLL_DEADLINE )); then
        # Deadline passed with the wrapper still alive. TERM the dash-p child
        # (it tears down its backend group), then the wrapper, and synthesize a
        # 124 only if no real rc landed in the meantime.
        apid=""
        [[ -s "$OUT_DIR/$p.apid" ]] && apid="$(cat "$OUT_DIR/$p.apid" 2>/dev/null || true)"
        # TERM dash-p so it tears down its own backend group, then hard-kill the
        # wrapper (our own subshell) so it can no longer publish a competing .rc.
        # That makes the synthetic 124 the single, deterministic writer — the only
        # way .rc is non-empty here is a real code the wrapper wrote just before
        # the kill, which we keep.
        [[ -n "$apid" ]] && kill -TERM "$apid" 2>/dev/null || true
        kill -KILL "$ppid" 2>/dev/null || true
        [[ -s "$OUT_DIR/$p.rc" ]] || echo "124" >"$OUT_DIR/$p.rc"   # orchestrator wall-clock exceeded
      else
        continue
      fi
    fi
    print_section "$p"
    PRINTED+=("$p")
    DONE_COUNT=$((DONE_COUNT + 1))
  done
  (( DONE_COUNT < TOTAL )) && sleep 1
done

# Reap any background PIDs that already exited; harmless if all are gone.
[[ ${#PIDS[@]} -gt 0 ]] && wait "${PIDS[@]}" 2>/dev/null || true

exit $(( ANY_FAIL ? 2 : 0 ))
