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
use autoreview::queue::Queue;
use autoreview::rundir::RunDir;
use autoreview::{cli, pool, prlist, queue, repo, select, signals, ui};
use std::collections::HashMap;

fn select_prs(cfg: &Config) -> select::Opts<'static> {
    select::Opts {
        include_approved: cfg.include_approved,
        include_dependabot: cfg.include_dependabot,
        pick: cfg.pick,
        continue_sessions: cfg.continue_sessions,
        sweep_empty_hint: "; pass --pick to choose from every open PR",
    }
}

/// What the sweep would pick up right now, said quietly -- a babysit loop
/// that re-announced the whole list on every interval would be noise. Also
/// returns the titles, so a PR that joined mid-run has one on the board.
fn actionable_now(
    cfg: &Config,
    ctx: &repo::RepoContext,
) -> anyhow::Result<(Vec<u64>, HashMap<u64, String>)> {
    let prs = prlist::fetch(ctx, cfg.include_approved, cfg.include_dependabot)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let rows = prlist::build_rows(&prs, &ctx.me, now);
    let titles = rows.iter().map(|r| (r.number, r.title.clone())).collect();
    let numbers = rows
        .iter()
        .filter(|r| {
            matches!(
                r.engage,
                prlist::Engagement::New | prlist::Engagement::Updated
            )
        })
        .map(|r| r.number)
        .collect();
    Ok((numbers, titles))
}

/// Say what changed since the last pass, so a queue that grew explains itself
/// rather than a count quietly going up.
fn report_intake(intake: &queue::Intake, cfg: &Config) {
    if !intake.joined.is_empty() {
        let list: Vec<String> = intake.joined.iter().map(|n| format!("#{n}")).collect();
        println!(
            "{} joined the queue: {}",
            ui::count(intake.joined.len(), "new or updated PR"),
            list.join(" ")
        );
    }
    for pr in &intake.capped {
        println!(
            "PR #{pr} has had {} in this run; leaving it alone",
            ui::count(cfg.max_passes as usize, "review")
        );
    }
}

fn run(cfg: &Config) -> anyhow::Result<i32> {
    // gum is required by the picker alone, so a sweep runs on a box that has
    // never seen it. pgrep is not optional: refusing to resume a session
    // another process holds is a safety check, not a nicety.
    repo::require_deps(&["gh", "git", "pgrep"])?;
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
    let mut titles = titles;
    let mut tracker = Queue::new(cfg.max_passes);
    let mut refresh_failures = 0u32;
    let mut pass = 1u32;
    let (failures, total) = loop {
        rundir.start_pass(pass)?;
        let jobs = pool::run_pass(&queue, &titles, &cfg, &ctx, &rundir, &dashp, &rx, &tx, &mut ui);
        ui.print_summary(&jobs, &rundir.pass_dir);
        let failures = pool::failures(&jobs);
        tracker.record_pass(&queue);

        let Some(babysit) = cfg.babysit.clone() else {
            break (failures, jobs.len());
        };

        // Read back from GitHub rather than inferred from what the agent
        // said: the review either landed as an approval or it did not, and a
        // run that believed its own report would babysit a PR it never
        // actually approved.
        let mut still_open = Vec::new();
        for &n in &queue {
            if let Some(why) = prlist::pr_babysit_done(n) {
                println!("\nPR #{n} is {why}; dropping it from the babysit loop");
                tracker.mark_done(n);
            } else {
                still_open.push(n);
            }
        }

        // Then look for work that did not exist when the run started. A PR
        // opened while the last pass ran would otherwise wait for a whole new
        // invocation of autoreview.
        let fresh = match actionable_now(&cfg, &ctx) {
            Ok((numbers, fresh_titles)) => {
                refresh_failures = 0;
                titles.extend(fresh_titles);
                numbers
            }
            Err(e) => {
                // A transient API error must not end a loop that is meant to
                // outlive one, and must not silently narrow it either: keep
                // what is already queued and look again next interval.
                refresh_failures += 1;
                eprintln!(
                    "\nwarning: could not refresh the PR list ({e:#}); keeping the current queue"
                );
                if refresh_failures >= 3 {
                    eprintln!("error: the PR list has failed to refresh 3 times; stopping");
                    break (failures, jobs.len());
                }
                Vec::new()
            }
        };

        let intake = tracker.next(&still_open, &fresh);
        report_intake(&intake, &cfg);
        if intake.queue.is_empty() {
            println!("\nnothing left to babysit");
            break (failures, jobs.len());
        }
        queue = intake.queue;
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
