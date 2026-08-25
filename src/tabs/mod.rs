//! review-prs: fan a repo's open PRs into one terminal tab each.
//!
//! The selection is autoreview's, the session derivation is autoreview's, and
//! what is left is this: pick a terminal, name each tab, and hand it a command.

pub mod cli;
pub mod command;
pub mod spawner;

use crate::repo::{self, AlreadyReported};
use crate::{select, session, ui};
use anyhow::{Result, bail};
use cli::Config;

fn select_opts(cfg: &Config) -> select::Opts<'static> {
    select::Opts {
        include_approved: cfg.include_approved,
        include_dependabot: cfg.include_dependabot,
        pick: !cfg.auto,
        continue_sessions: cfg.continue_sessions,
        sweep_empty_hint: "; run without --auto to choose from every open PR",
    }
}

pub fn run(cfg: &Config) -> Result<i32> {
    // gum is the picker's dependency alone, so an --auto sweep runs on a box
    // that has never seen it; picker::run asks for it when it is reached.
    repo::require_deps(&["gh"])?;

    // Before the picker, not after it: with no terminal to spawn into there is
    // nothing this tool can do, and finding that out after choosing PRs wastes
    // the choosing.
    let spawner = match spawner::detect() {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("{msg}");
            bail!(AlreadyReported);
        }
    };

    let ctx = repo::load()?;
    let Some((numbers, _titles)) = select::run(&ctx, &select_opts(cfg))? else {
        return Ok(0);
    };

    if let Some(label) = &cfg.workspace {
        spawner::rename_workspace(spawner, label);
    }

    let mut failures = 0usize;
    for &n in &numbers {
        // Decide this PR's session before anything else: the flag, the prompt
        // and the tab label all follow from it.
        let plan = session::plan_session(
            &ctx.repo_root,
            &ctx.owner,
            &ctx.name,
            n,
            cfg.continue_sessions,
        );
        if let Some(note) = &plan.note {
            eprintln!("{note}");
        }
        let resuming = if plan.resume { " (resuming earlier review)" } else { "" };
        println!("spawning tab for PR #{n} via {}{resuming}", spawner.name());

        let cmd = command::line(cfg, n, &plan, &ctx.repo_root);
        let label = command::label(cfg, n, plan.resume);
        // A tab that fails to spawn must not take the rest of the sweep with
        // it: warn and keep going, so one bad tab costs one review rather than
        // all of them. The count is what the exit status reports.
        if let Err(msg) = spawner::spawn(spawner, &cmd, &label) {
            eprintln!("{msg}");
            eprintln!("warning: skipped PR #{n}");
            failures += 1;
        }
    }

    // Exit nonzero when any tab failed. A sweep that spawned nothing must not
    // look successful to whatever ran it -- `review-prs --auto` in a script or
    // a cron job would otherwise report success having started no reviews.
    if failures > 0 {
        eprintln!(
            "error: {failures} of {} failed to spawn",
            ui::count(numbers.len(), "tab")
        );
        return Ok(1);
    }
    Ok(0)
}
