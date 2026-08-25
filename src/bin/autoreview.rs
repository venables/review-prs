// autoreview: review the current repo's open PRs headlessly -- no terminal
// tabs. Each PR is reviewed by a `dash-p` subprocess driving claude; the run
// shows live per-PR progress, prints a summary, and exits nonzero if any
// review failed.
//
// This is the sibling of `review-prs`, the other binary in this crate, which
// fans the same PRs out into one terminal tab each. The two share the library:
// same PR list, same ranking, same picker, same derived session ids.
//
// Pick this one when there is no terminal to spawn into (ssh, cron, CI), when
// the exit status has to mean "the reviews succeeded" rather than "the tabs
// opened", or when a dozen PRs would mean a dozen tabs. Pick review-prs when
// you want to watch a review happen and steer it mid-flight.

use autoreview::cli::Config;
use autoreview::rundir::RunDir;
use autoreview::{cli, pool, prlist, repo, select, signals, ui};

fn select_prs(cfg: &Config) -> select::Opts<'static> {
    select::Opts {
        include_approved: cfg.include_approved,
        include_dependabot: cfg.include_dependabot,
        pick: cfg.pick,
        continue_sessions: cfg.continue_sessions,
        sweep_empty_hint: "; pass --pick to choose from every open PR",
    }
}

fn run(cfg: &Config) -> anyhow::Result<i32> {
    // gum is required by the picker alone, so a sweep runs on a box that has
    // never seen it. pgrep is not optional: refusing to resume a session
    // another process holds is a safety check, not a nicety.
    repo::require_deps(&["gh", "pgrep"])?;
    let dashp = repo::dashp_bin();
    if cfg.review_cmd.is_none() {
        repo::require_deps(&[dashp.as_str()])?;
    }
    let ctx = repo::load()?;

    let Some((numbers, titles)) = select::run(&ctx, &select_prs(cfg))? else {
        return Ok(0);
    };

    let mut rundir = RunDir::new(cfg.log_dir.clone())?;
    let (tx, rx) = std::sync::mpsc::channel();
    signals::install(tx.clone());
    let mut ui = ui::Ui::new(ui::pr_url_base(&ctx.owner, &ctx.name));
    ui.hide_cursor();

    // --babysit re-runs the whole pass on an interval, dropping PRs as they
    // are approved (or closed -- waiting for an approval that is never coming
    // would re-review forever), until nothing is left. The loop is this
    // process, so an interval that never converges is one process you can
    // see and kill.
    let mut cfg = cfg.clone();
    let mut queue = numbers;
    let mut pass = 1u32;
    let (failures, total) = loop {
        rundir.start_pass(pass)?;
        let jobs = pool::run_pass(&queue, &titles, &cfg, &ctx, &rundir, &dashp, &rx, &tx, &mut ui);
        ui.print_summary(&jobs, &rundir.pass_dir);
        let failures = pool::failures(&jobs);

        let Some(babysit) = cfg.babysit.clone() else {
            break (failures, jobs.len());
        };

        // Read back from GitHub rather than inferred from what the agent
        // said: the review either landed as an approval or it did not, and a
        // run that believed its own report would babysit a PR it never
        // actually approved.
        let mut remaining = Vec::new();
        for &n in &queue {
            if let Some(why) = prlist::pr_babysit_done(n) {
                println!("\nPR #{n} is {why}; dropping it from the babysit loop");
            } else {
                remaining.push(n);
            }
        }
        if remaining.is_empty() {
            println!("\nnothing left to babysit");
            break (failures, jobs.len());
        }
        queue = remaining;
        // The first pass recorded each review's session; every later pass
        // resumes it whether or not --continue was passed. Re-reviewing from
        // scratch each interval would throw away the findings the author is
        // in the middle of answering.
        cfg.continue_sessions = true;
        println!(
            "\nnext check in {} ({} left)",
            babysit.normalized,
            ui::count(queue.len(), "PR")
        );
        interruptible_sleep(&rx, std::time::Duration::from_secs(babysit.secs), &ui);
        pass += 1;
    };
    ui.show_cursor();

    // Exit nonzero when any review in the final pass did not complete
    // cleanly, so a cron job or a CI step can tell a finished sweep from a
    // broken one.
    if failures > 0 {
        eprintln!("error: {failures} of {} failed", ui::count(total, "review"));
        return Ok(1);
    }
    Ok(0)
}

/// The babysit interval sleep, listening on the same channel the pass engine
/// uses -- a signal mid-interval must end the loop the same way it ends a
/// pass, not wait out the timer.
fn interruptible_sleep(
    rx: &std::sync::mpsc::Receiver<pool::Event>,
    dur: std::time::Duration,
    ui: &ui::Ui,
) {
    let deadline = std::time::Instant::now() + dur;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        match rx.recv_timeout(left) {
            Ok(pool::Event::Signal) => {
                println!();
                eprintln!("interrupted; stopping running reviews");
                ui.show_cursor();
                std::process::exit(130);
            }
            // Stale job events from a pass that already finished.
            Ok(_) => {}
            Err(_) => return,
        }
    }
}

fn main() {
    let cfg = match cli::parse(cli::args_or_exit(), &cli::real_env) {
        Ok(cli::Parsed::Help) => {
            print!("{}", cli::HELP);
            std::process::exit(0);
        }
        Ok(cli::Parsed::Run(cfg)) => cfg,
        Err(e) => {
            eprintln!("{}", e.msg);
            if e.show_help {
                eprint!("{}", cli::HELP);
            }
            std::process::exit(1);
        }
    };
    for note in &cfg.startup_notes {
        eprintln!("{note}");
    }
    match run(&cfg) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // A tool built for cron and CI must never exit 1 silently:
            // stderr is the only diagnostic channel an unattended run has.
            // Sites that already explained themselves bail AlreadyReported.
            if e.downcast_ref::<repo::AlreadyReported>().is_none() {
                eprintln!("error: {e:#}");
            }
            std::process::exit(1)
        }
    }
}
