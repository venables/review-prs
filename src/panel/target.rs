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
        },
        Target::Staged => Resolved {
            label: format!("staged changes on {}", branch(repo_root)),
            diff: git_stdout(repo_root, &["diff", "--cached"])?,
            isolated: false,
            sha: None,
        },
        Target::Base(base) => {
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
                sha: Some(sha),
            }
        }
    };

    // An empty diff is not a review anyone wants: every panelist would spend
    // a model call to report nothing, and the synthesis would agree with them.
    if resolved.diff.trim().is_empty() {
        bail!("nothing to review: the diff for {} is empty", resolved.label);
    }
    Ok(resolved)
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
