//! What the panel is reviewing, and the two shapes that come in: a diff of
//! the working tree, or a diff of committed work.
//!
//! The distinction decides more than the diff text. Uncommitted work only
//! exists in the user's own checkout, so panelists read it there and touch
//! nothing. Committed work has a ref, so each panelist gets a worktree of its
//! own and may run the tests.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    Uncommitted,
    Staged,
    Base(String),
}

#[derive(Debug)]
pub struct Resolved {
    /// What the report says it reviewed.
    pub label: String,
    pub diff: String,
    /// True when each panelist gets its own worktree and may run commands.
    pub isolated: bool,
    /// The commit every worktree pins to; None for working-tree targets.
    pub sha: Option<String>,
    /// Files git is not tracking. No diff covers them, so the panelists are
    /// told to read them from the tree instead.
    pub untracked: Vec<String>,
}

fn git(repo_root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))
}

fn git_stdout(repo_root: &Path, args: &[&str]) -> Result<String> {
    let out = git(repo_root, args)?;
    if !out.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

pub fn resolve(target: &Target, repo_root: &Path) -> Result<Resolved> {
    let resolved = match target {
        // Against HEAD rather than the index, so staged and unstaged edits
        // both reach the panel -- "what I have not committed yet" is the
        // whole of it, not the half that happens to be staged.
        Target::Uncommitted => Resolved {
            label: format!("uncommitted changes on {}", branch(repo_root)),
            diff: git_stdout(repo_root, &["diff", "HEAD"])?,
            isolated: false,
            sha: None,
            untracked: untracked_files(repo_root)?,
        },
        Target::Staged => Resolved {
            label: format!("staged changes on {}", branch(repo_root)),
            diff: git_stdout(repo_root, &["diff", "--cached"])?,
            isolated: false,
            sha: None,
            // Nothing untracked is in the index, so nothing untracked is part
            // of what this target reviews.
            untracked: Vec::new(),
        },
        Target::Base(base) => {
            // A base that starts with a dash reaches git as an option, not a
            // ref: `--output=<path>` makes git write the diff to a file and
            // print nothing, and the run would then stop with "nothing to
            // review", which names the wrong cause entirely.
            if base.starts_with('-') {
                bail!("--base expects a ref, and \"{base}\" starts with a dash");
            }
            let verified = git(repo_root, &["rev-parse", "--verify", "--quiet", &format!("{base}^{{commit}}")])?;
            if !verified.status.success() {
                bail!("--base: no commit named \"{base}\" in this repository");
            }
            // Three dots: the diff of what this branch added, not of every
            // change on the base since it forked. Reviewing the latter would
            // flag other people's commits as this branch's work.
            let range = format!("{base}...HEAD");
            let diff = git_stdout(repo_root, &["diff", &range])?;
            let sha = git_stdout(repo_root, &["rev-parse", "HEAD"])?.trim().to_string();
            let commits = git_stdout(repo_root, &["rev-list", "--count", &format!("{base}..HEAD")])?
                .trim()
                .parse::<usize>()
                .unwrap_or(0);
            Resolved {
                label: format!(
                    "{} on {} vs {base}",
                    crate::ui::count(commits, "commit"),
                    branch(repo_root)
                ),
                diff,
                isolated: true,
                // A committed target is what the worktrees are pinned to;
                // anything untracked is not part of it.
                sha: Some(sha),
                untracked: Vec::new(),
            }
        }
    };

    // An empty diff is not a review anyone wants: every panelist would spend
    // a model call to report nothing, and the synthesis would agree with them.
    if resolved.diff.trim().is_empty() {
        // A change that only adds files is the common way to land here, and
        // "the diff is empty" is a baffling thing to be told while looking at
        // the new files. Name them.
        // Asked for here rather than taken from the target: an empty diff is
        // the one moment untracked files explain themselves, even for a target
        // that would not otherwise review them.
        // Best-effort here alone: the run is already failing, and a second
        // failure should not replace the reason with a git error.
        let untracked = untracked_files(repo_root).unwrap_or_default();
        if !untracked.is_empty() {
            let (subject, verb) = if untracked.len() == 1 { ("file", "is") } else { ("files", "are") };
            // Staging is enough for a working-tree diff; a base-vs-HEAD diff
            // only sees what has been committed.
            let advice = match target {
                Target::Base(_) => "commit them first",
                _ => "add them with `git add` first",
            };
            bail!(
                "nothing to review: the diff for {} is empty. {} {subject} {verb} not tracked by git yet, so no diff covers them: {}. To review them, {advice}.",
                resolved.label,
                untracked.len(),
                summarize(&untracked)
            );
        }
        bail!("nothing to review: the diff for {} is empty", resolved.label);
    }
    Ok(resolved)
}

/// Every file git is not tracking. The error is propagated rather than read
/// as "there are none": a review that quietly leaves out every new file
/// because one git call failed is worse than one that refuses to start.
fn untracked_files(repo_root: &Path) -> Result<Vec<String>> {
    // -z, so git does not apply core.quotePath: a name with a non-ASCII or
    // unusual character would otherwise reach the prompt in an escaped form
    // that no read tool can open.
    Ok(git_stdout(repo_root, &["ls-files", "--others", "--exclude-standard", "-z"])?
        .split('\0')
        // Only empty fields, not whitespace-only ones: " " is a legal
        // filename, and dropping it would leave a file nobody reviews.
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

/// A readable list for a message, saying how many it did not name rather than
/// stopping silently.
fn summarize(files: &[String]) -> String {
    const SHOWN: usize = 10;
    // Sanitized: -z removed git's quoting, and these names go to a terminal.
    let names: Vec<String> = files
        .iter()
        .take(SHOWN)
        .map(|f| crate::report::sanitize_for_display(f))
        .collect();
    if files.len() <= SHOWN {
        return names.join(", ");
    }
    format!("{}, and {} more", names.join(", "), files.len() - SHOWN)
}

fn branch(repo_root: &Path) -> String {
    git_stdout(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "HEAD".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_base_target_is_isolated_and_a_working_tree_one_is_not() {
        // The property that decides worktrees and permissions, stated once.
        assert_eq!(Target::Base("main".into()), Target::Base("main".into()));
        assert_ne!(Target::Uncommitted, Target::Staged);
    }
}
