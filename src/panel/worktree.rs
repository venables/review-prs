//! One throwaway git worktree per panelist, for targets that have a commit
//! to pin to.
//!
//! One worktree each rather than one shared: panelists run test suites and
//! edit files to investigate. Sharing a checkout across parallel reviewers
//! means test runners racing on lockfiles and build directories, and one
//! panelist's edits leaking into another's reading of the code -- which
//! quietly breaks the independence the whole panel is built on.
//!
//! Disk cost is N copies of the repo, but cargo/pnpm/npm caches live at the
//! user level, so most of the bytes are shared.

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The worktrees for one run, removed when this value is dropped -- including
/// on the error paths, which is the point of tying it to a value rather than
/// to the end of a function.
pub struct Worktrees {
    repo_root: PathBuf,
    dirs: Vec<PathBuf>,
}

impl Worktrees {
    pub fn none(repo_root: &Path) -> Worktrees {
        Worktrees { repo_root: repo_root.to_path_buf(), dirs: Vec::new() }
    }

    /// One worktree per id, each detached at the same commit.
    pub fn create(repo_root: &Path, base: &Path, ids: &[String], sha: &str) -> Result<Worktrees> {
        let mut wts = Worktrees::none(repo_root);
        for id in ids {
            let dir = base.join(format!("worktree-{id}"));
            let out = Command::new("git")
                .args(["worktree", "add", "--detach", "--quiet"])
                .arg(&dir)
                .arg(sha)
                .current_dir(repo_root)
                .stdin(Stdio::null())
                .output()
                .context("running git worktree add")?;
            if !out.status.success() {
                // Whatever was made so far still comes back out: wts drops on
                // the way out of this function.
                bail!(
                    "could not create a worktree for panelist '{id}': {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            wts.dirs.push(dir);
        }
        Ok(wts)
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }
}

impl Drop for Worktrees {
    fn drop(&mut self) {
        // Best-effort, and quiet: the run has already said whatever it had to
        // say, and a worktree that will not go away is not worth a second
        // error on top of the first. `git worktree remove --force` because a
        // panelist may well have left edits behind -- that is what it was for.
        for dir in &self.dirs {
            let _ = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(dir)
                .current_dir(&self.repo_root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}
