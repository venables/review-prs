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

use crate::status::{Status, step};
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

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
    ///
    /// Checked between each one: materializing a large repo several times is
    /// the slowest thing this tool does before any model runs, and a ctrl-C
    /// during it should leave nothing behind.
    pub fn create(
        repo_root: &Path,
        base: &Path,
        ids: &[String],
        sha: &str,
        interrupted: &Arc<AtomicBool>,
        status: &Status,
    ) -> Result<Worktrees> {
        // Canonical, because git records a worktree at its realpath. On macOS
        // the default temp directory is /var/folders/..., a symlink to
        // /private/var/..., so a path recorded as given would never match what
        // `git worktree list` prints -- and the leak check below would answer
        // "not registered" for every real leak.
        let base = base.canonicalize().unwrap_or_else(|_| base.to_path_buf());
        let mut wts = Worktrees::none(repo_root);
        for (n, id) in ids.iter().enumerate() {
            // The longest wait in a panel run happens here: N checkouts of the
            // repository before a single model has been asked anything.
            status.step(step::materializing(n, ids.len()));
            if interrupted.load(Ordering::Relaxed) {
                // Whatever was made so far goes with wts on the way out.
                bail!("interrupted while creating worktrees");
            }
            let dir = base.join(format!("worktree-{id}"));
            // Recorded before the add, not after it. git registers a worktree
            // partway through, so a failure -- or a ctrl-C landing between the
            // registration and the return -- would otherwise leave one in the
            // user's repository that Drop never learned about.
            wts.dirs.push(dir.clone());
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
                // the way out of this function, and this one is already in it.
                bail!(
                    "could not create a worktree for '{id}': {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
        }
        Ok(wts)
    }

    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
    }
}

impl Worktrees {
    /// Does git still list this path as a worktree? Asked only after a remove
    /// was refused, to tell a real leaked registration from a worktree that
    /// was never created.
    fn still_registered(&self, dir: &Path) -> bool {
        let Ok(out) = Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(&self.repo_root)
            .stdin(Stdio::null())
            .output()
        else {
            return false;
        };
        let listed = String::from_utf8_lossy(&out.stdout);
        let needle = dir.display().to_string();
        listed
            .lines()
            .any(|l| l.strip_prefix("worktree ").is_some_and(|p| p == needle))
    }
}

impl Drop for Worktrees {
    fn drop(&mut self) {
        // Best-effort, and quiet: the run has already said whatever it had to
        // say, and a worktree that will not go away is not worth a second
        // error on top of the first. `git worktree remove --force` because a
        // panelist may well have left edits behind -- that is what it was for.
        let mut leaked = false;
        for dir in &self.dirs {
            let removed = Command::new("git")
                .args(["worktree", "remove", "--force"])
                .arg(dir)
                .current_dir(&self.repo_root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            // A refused remove is not proof of a leak. `git worktree add` can
            // fail before it registers anything -- a bad ref, an interrupt --
            // and every path is recorded before the add, so most refusals are
            // for a worktree that never existed. Only a path git still lists
            // needs the sweep below.
            leaked |= !removed && self.still_registered(dir);
        }
        // Only for a registration git still lists after refusing to remove
        // it. `--expire now` sweeps every registration in the repository
        // whose directory is missing right now, which includes the user's own
        // worktree on an unmounted volume -- so it runs in the one case it
        // exists for, and never on a clean run.
        if leaked {
            // `git worktree add` registers a worktree before it finishes
            // writing one, so a kill in that window can leave an entry that
            // `remove` refuses as "not a working tree". prune is what clears
            // those, and it is a no-op when there are none.
            let _ = Command::new("git")
                // Plain prune honours gc.worktreePruneExpire, which defaults
                // to three months, so it would leave exactly the registration
                // this is here to clear.
                .args(["worktree", "prune", "--expire", "now"])
                .current_dir(&self.repo_root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}
