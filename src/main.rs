// autoreview: review the current repo's open PRs headlessly -- no terminal
// tabs. Each PR is reviewed by a `dash-p` subprocess driving claude; the run
// shows live per-PR progress, prints a summary, and exits nonzero if any
// review failed.
//
// This is the sibling of `review-prs` (bash, in this repo), which fans the
// same PRs out into one terminal tab each. The two agree on what is worth
// reviewing and on which session a PR belongs to: the selection and session
// derivation in src/ mirror lib/*.sh, and golden unit tests pin the session
// ids to lib/session.sh's output.
//
// Pick this one when there is no terminal to spawn into (ssh, cron, CI), when
// the exit status has to mean "the reviews succeeded" rather than "the tabs
// opened", or when a dozen PRs would mean a dozen tabs. Pick review-prs when
// you want to watch a review happen and steer it mid-flight.

mod cli;
mod interval;
mod job;
mod picker;
mod pool;
mod prlist;
mod report;
mod repo;
mod rundir;
mod session;
mod signals;
mod ui;

use cli::Config;
use repo::RepoContext;
use rundir::RunDir;

fn mark_resumable(rows: &mut [prlist::Row], ctx: &RepoContext) {
    // Marking costs one hash and one glob per PR, so skip the whole loop when
    // no session store exists -- there is nothing to find, and a box without
    // Claude Code should not pay for the lookup on every picker run.
    if !session::projects_dir().is_dir() {
        return;
    }
    for row in rows {
        let id = session::pr_session_id(&ctx.repo_root, &ctx.owner, &ctx.name, row.number);
        row.resumable = session::session_exists(&id);
    }
}

/// The chosen PR numbers, plus every fetched PR's title for the live board.
type Selection = (Vec<u64>, std::collections::HashMap<u64, String>);

fn select_prs(cfg: &Config, ctx: &RepoContext) -> anyhow::Result<Option<Selection>> {
    let Some(prs) = prlist::fetch_prs(ctx, cfg.include_approved, cfg.include_dependabot)? else {
        return Ok(None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut rows = prlist::build_rows(&prs, &ctx.me, now);
    let titles = rows.iter().map(|r| (r.number, r.title.clone())).collect();
    let numbers = if cfg.pick {
        mark_resumable(&mut rows, ctx);
        picker::run(&rows, cfg.continue_sessions, cfg.include_dependabot)?
    } else {
        prlist::select_auto(&rows)
    };
    Ok(numbers.map(|n| (n, titles)))
}

fn run(cfg: &Config) -> anyhow::Result<i32> {
    // gum is required by the picker alone, so a sweep runs on a box that has
    // never seen it. pgrep is not optional: refusing to resume a
    // session another process holds is a safety check, not a nicety.
    repo::require_deps(&["gh", "pgrep"])?;
    let dashp = repo::dashp_bin();
    if cfg.review_cmd.is_none() {
        repo::require_deps(&[dashp.as_str()])?;
    }
    let ctx = repo::load()?;

    let Some((numbers, titles)) = select_prs(cfg, &ctx)? else {
        return Ok(0);
    };

    let mut rundir = RunDir::new(cfg.log_dir.clone())?;
    let (tx, rx) = std::sync::mpsc::channel();
    signals::install(tx.clone());
    let mut ui = ui::Ui::new();
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
    let cfg = match cli::parse(std::env::args().skip(1), &cli::real_env) {
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
