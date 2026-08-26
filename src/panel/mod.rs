//! panel: review one change with several models at once, then synthesize.
//!
//! The same fan-out `autoreview` does, with a different unit of work: N models
//! on one diff rather than N PRs in one repo. The shape is deliberately flat
//! -- spawn, collect, synthesize -- so the only model judgment in the run is
//! the judgment nobody can script: deciding which findings are real.

pub mod cli;
pub mod fanout;
pub mod panelist;
pub mod prompt;
pub mod synthesis;
pub mod target;
pub mod worktree;

use crate::repo::{self, AlreadyReported};
use crate::status::{Status, step};
use anyhow::{Context, Result, bail};
use cli::Config;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use worktree::Worktrees;

/// Where this run keeps its prompts, per-panelist output and the synthesis.
/// A directory of its own per run, for the reason autoreview has one: a fixed
/// --log-dir plus two concurrent runs would otherwise have each reading the
/// other's results. Made, not named after the pid, for the reason in
/// rundir.rs.
fn out_dir(cfg: &Config) -> Result<PathBuf> {
    let base = match &cfg.log_dir {
        Some(dir) => dir.clone(),
        None => std::env::temp_dir(),
    };
    std::fs::create_dir_all(&base).with_context(|| format!("creating {}", base.display()))?;
    crate::rundir::make_unique_dir(&base, "panel.")
}

pub fn run(cfg: &Config) -> Result<i32> {
    repo::require_deps(&["git"])?;
    let dashp = repo::dashp_bin();
    repo::require_deps(&[dashp.as_str()])?;

    // Two waits stand between launch and the first panelist: reading the
    // repo, and materializing a checkout per panelist. The second can be the
    // longest thing a panel run does before a model is asked anything.
    let status = Status::new();
    status.step(step::reading_repo());
    let repo_root = repo::git_root(&status)?;
    let mut cfg = cfg.clone();

    let specs = if cfg.panelists.is_empty() {
        let found = panelist::autodetect();
        if found.is_empty() {
            eprintln!(
                "error: no backend CLIs found on PATH (looked for {})",
                panelist::BACKENDS.join(", ")
            );
            bail!(AlreadyReported);
        }
        found
    } else {
        cfg.panelists.clone()
    };
    // A named backend that is not installed is a typo worth stopping for; an
    // auto-detected one cannot be missing, since that is how it was found.
    for spec in &specs {
        repo::require_deps(&[spec.backend.as_str()])?;
    }
    // The synthesizer is a backend too, and finding out it is missing after
    // every panelist has been paid for is the worst moment to find out.
    if cfg.synthesize {
        repo::require_deps(&[cfg.synth_backend.as_str()])?;
    }
    let panel = panelist::resolve(&specs);

    status.step("building the diff");
    let resolved = target::resolve(&cfg.target, &repo_root)?;
    cfg.isolated = resolved.isolated;

    // Before anything is created: a signal during worktree setup would
    // otherwise kill the process with the default disposition, Drop would
    // never run, and the worktrees made so far would stay registered in the
    // user's real repository.
    let interrupted = crate::signals::install_flag();

    let dir = out_dir(&cfg)?;
    let prompt_text = prompt::build(
        &resolved.label,
        resolved.isolated,
        cfg.focus.as_deref(),
        &resolved.diff,
        &resolved.untracked,
    );
    let prompt_path = dir.join("review.prompt");
    File::create(&prompt_path)
        .context("creating the panelist prompt file")?
        .write_all(prompt_text.as_bytes())
        .context("writing the panelist prompt")?;

    // One worktree per panelist for a committed target; the user's own tree,
    // read-only, for uncommitted work. The value is held for the whole run so
    // the worktrees are removed even when a panelist fails.
    // One worktree per panelist, and one more for the synthesis: the
    // synthesizer verifies findings against the code that was reviewed, and a
    // panelist's worktree may hold that panelist's own investigation edits.
    // For a working-tree target there is no ref to pin, so everyone reads the
    // user's checkout.
    let (_worktrees, cwds, synth_cwd) = match &resolved.sha {
        Some(sha) => {
            let mut ids: Vec<String> = panel.iter().map(|p| p.id.clone()).collect();
            // Only when there is a synthesis to run: materializing a large
            // repo one extra time is the slowest thing here, and --no-synthesis
            // has nothing to put in it.
            if cfg.synthesize {
                ids.push("synthesis".into());
            }
            let wts = match Worktrees::create(&repo_root, &dir, &ids, sha, &interrupted, &status) {
                Ok(wts) => wts,
                Err(e) => {
                    // A ctrl-C reaches the running `git worktree add` too, so
                    // this usually surfaces as git's own failure rather than
                    // the explicit interrupt bail. Either way the user pressed
                    // ctrl-C, and 130 is what says so.
                    if interrupted.load(std::sync::atomic::Ordering::Relaxed) {
                        eprintln!("panel: interrupted while creating worktrees");
                        return Ok(130);
                    }
                    return Err(e);
                }
            };
            let mut dirs = wts.dirs().to_vec();
            let synth = if cfg.synthesize {
                dirs.pop().expect("the synthesis worktree")
            } else {
                repo_root.clone()
            };
            (wts, dirs, synth)
        }
        None => (
            Worktrees::none(&repo_root),
            vec![repo_root.clone(); panel.len()],
            repo_root.clone(),
        ),
    };

    // The header is the report, so the spinner steps out of its way.
    status.clear();
    let ids: Vec<&str> = panel.iter().map(|p| p.id.as_str()).collect();
    println!("# Panel review\n");
    println!("- Target: {}", resolved.label);
    println!("- Panelists: {}", ids.join(", "));
    println!("- Outputs: `{}`", dir.display());
    if let Some(focus) = &cfg.focus {
        println!("- Focus: {focus}");
    }
    println!();

    let outcomes = fanout::run(
        &panel,
        &cwds,
        &cfg,
        &dashp,
        &prompt_path,
        &dir,
        &interrupted,
        &status,
    )?;

    if interrupted.load(std::sync::atomic::Ordering::Relaxed) {
        eprintln!("panel: interrupted; the worktrees are being removed");
        // Ok, not Err: the panelist reports above are real and already
        // printed. 130 is what a shell expects from an interrupted run.
        return Ok(130);
    }

    let answered = outcomes.iter().filter(|o| o.answered()).count();
    if answered == 0 {
        eprintln!("error: no panelist returned a review; there is nothing to synthesize");
        eprintln!("  what each one did is in {}", dir.display());
        bail!(AlreadyReported);
    }

    if !cfg.synthesize {
        eprintln!("panel: --no-synthesis, stopping after {}", crate::ui::count(answered, "report"));
        return Ok(0);
    }

    let report = match synthesis::run(
        &resolved.label,
        &resolved.diff,
        &resolved.untracked,
        &outcomes,
        &cfg,
        &dashp,
        &synth_cwd,
        &dir,
        &interrupted,
        &status,
    ) {
        Ok(report) => report,
        Err(e) => {
            // A synthesis this run stopped on purpose is an interrupt, not a
            // failure: the panelist reports above are real and already
            // printed, and 130 is what a shell expects.
            if interrupted.load(std::sync::atomic::Ordering::Relaxed) {
                eprintln!("panel: interrupted during synthesis");
                return Ok(130);
            }
            return Err(e);
        }
    };
    println!("# Synthesis\n");
    println!("{}", crate::report::sanitize_block(&report));
    println!();
    println!("---");
    println!(
        "{} of {} answered · outputs: `{}`",
        answered,
        outcomes.len(),
        dir.display()
    );
    Ok(0)
}
