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

    let repo_root = repo::git_root()?;
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
    let panel = panelist::resolve(&specs);

    let resolved = target::resolve(&cfg.target, &repo_root)?;
    cfg.isolated = resolved.isolated;

    let dir = out_dir(&cfg)?;
    let prompt_text = prompt::build(
        &resolved.label,
        resolved.isolated,
        cfg.focus.as_deref(),
        &resolved.diff,
    );
    let prompt_path = dir.join("review.prompt");
    File::create(&prompt_path)
        .context("creating the panelist prompt file")?
        .write_all(prompt_text.as_bytes())
        .context("writing the panelist prompt")?;

    // One worktree per panelist for a committed target; the user's own tree,
    // read-only, for uncommitted work. The value is held for the whole run so
    // the worktrees are removed even when a panelist fails.
    let (_worktrees, cwds) = match &resolved.sha {
        Some(sha) => {
            eprintln!("panel: materializing {} ...", crate::ui::count(panel.len(), "worktree"));
            let ids: Vec<String> = panel.iter().map(|p| p.id.clone()).collect();
            let wts = Worktrees::create(&repo_root, &dir, &ids, sha)?;
            let dirs = wts.dirs().to_vec();
            (wts, dirs)
        }
        None => (
            Worktrees::none(&repo_root),
            vec![repo_root.clone(); panel.len()],
        ),
    };

    let ids: Vec<&str> = panel.iter().map(|p| p.id.as_str()).collect();
    println!("# Panel review\n");
    println!("- Target: {}", resolved.label);
    println!("- Panelists: {}", ids.join(", "));
    println!("- Outputs: `{}`", dir.display());
    if let Some(focus) = &cfg.focus {
        println!("- Focus: {focus}");
    }
    println!();

    // Installed before anything is spawned: a ctrl-C between the first spawn
    // and the poll loop must still be seen.
    let interrupted = crate::signals::install_flag();
    let outcomes = fanout::run(&panel, &cwds, &cfg, &dashp, &prompt_path, &dir, &interrupted)?;

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

    let report = synthesis::run(&resolved.label, &outcomes, &cfg, &dashp, &repo_root, &dir)?;
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
