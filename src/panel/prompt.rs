//! The prompt every panelist reads: the shared template, then what this
//! particular run is looking at.
//!
//! The template is compiled in rather than read from disk. Both binaries in
//! this crate are self-contained -- nothing is loaded out of the checkout at
//! runtime -- and a panel whose prompt silently changed under it would make
//! two runs incomparable for no visible reason.

const TEMPLATE: &str = include_str!("../../prompts/review.md");

/// What the panelist may do, spelled out. It calibrates the findings: a
/// reviewer told it can run the tests reports a failing test as evidence, and
/// one told it cannot must not claim it ran anything.
fn workspace(isolated: bool) -> &'static str {
    if isolated {
        "You are running inside a dedicated git worktree pinned to this review's target \
         commit -- the actual checkout, not a free-floating diff. You may:\n\n\
         - Read any file in the tree.\n\
         - Grep / rg across the tree to find callers of changed symbols.\n\
         - Edit files locally (the worktree is thrown away on exit).\n\
         - Run build / test / lint commands to investigate downstream effects.\n\
         - Install dev dependencies if a test runner needs them; bound test runs to ~3 minutes.\n\
         - A failing test is a high-signal finding; surface it under Evidence.\n\n\
         Use that capability when it sharpens a finding. Do NOT do investigation theatre -- \
         only run tools when they harden the report. Local writes inside the worktree are \
         fine; the external-network and GitHub-write rules in Hard Constraints still apply."
    } else {
        "You are running in the user's actual working tree with **read-only** access. You may:\n\n\
         - Read any file using your built-in read tools (Read / Glob / Grep).\n\
         - Reason about the diff and the surrounding code.\n\n\
         Do NOT modify files, run tests, install packages, or execute shell commands that \
         change state. The `Evidence:` line in the finding shape is not meaningful in this \
         mode -- leave it out."
    }
}

pub fn build(target_label: &str, isolated: bool, focus: Option<&str>, diff: &str) -> String {
    let mut p = String::from(TEMPLATE);
    p.push_str("\n\n## Review target\n\n");
    p.push_str(target_label);
    p.push_str("\n\n## Workspace\n\n");
    p.push_str(workspace(isolated));
    if let Some(focus) = focus {
        p.push_str("\n\n## Reviewer focus\n\n");
        p.push_str(focus);
    }
    // A fence longer than any backtick run inside the diff. A context line
    // that is itself ``` would otherwise close the block early and the rest of
    // the diff would read as prose.
    let fence = "`".repeat(longest_backtick_run(diff).max(2) + 1);
    p.push_str("\n\n## Diff\n\n");
    p.push_str(&fence);
    p.push_str("diff\n");
    p.push_str(diff);
    if !diff.ends_with('\n') {
        p.push('\n');
    }
    p.push_str(&fence);
    p.push('\n');
    p
}

fn longest_backtick_run(s: &str) -> usize {
    let (mut longest, mut current) = (0, 0);
    for c in s.chars() {
        if c == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_compiled_in_and_carries_its_contract() {
        // The labels the report and the synthesis both match on.
        assert!(TEMPLATE.contains("Model: <model-id>"));
        assert!(TEMPLATE.contains("Goal (clear):"));
        assert!(TEMPLATE.contains("Approach (questionable):"));
        assert!(TEMPLATE.contains("NO_FINDINGS"));
    }

    #[test]
    fn a_run_says_what_it_is_reviewing_and_what_the_reviewer_may_do() {
        let p = build("3 commits on feat/x vs main", true, Some("the auth path"), "diff --git a b\n");
        assert!(p.contains("## Review target\n\n3 commits on feat/x vs main"));
        assert!(p.contains("dedicated git worktree"));
        assert!(p.contains("## Reviewer focus\n\nthe auth path"));
        assert!(p.contains("```diff\ndiff --git a b\n```"));

        let ro = build("uncommitted changes on main", false, None, "d");
        assert!(ro.contains("**read-only**"));
        assert!(!ro.contains("## Reviewer focus"));
        // An unterminated diff still closes its fence.
        assert!(ro.ends_with("```diff\nd\n```\n"));
    }

    #[test]
    fn a_diff_containing_a_fence_cannot_close_the_block() {
        // A markdown file whose own fences are in the diff is the ordinary
        // case here -- this repo's prompts are markdown.
        let diff = "+```rust\n+code\n+```\n";
        let p = build("t", false, None, diff);
        assert!(p.contains("````diff\n"), "fence must outgrow the diff: {p}");
        assert!(p.ends_with("````\n"));
        assert_eq!(longest_backtick_run("no ticks"), 0);
        assert_eq!(longest_backtick_run("a ``` b ```` c"), 4);
    }
}
