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
use autoreview::status::{Status, step};
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
    let prs = prlist::fetch(ctx, cfg.include_approved, cfg.include_dependabot)?.prs;
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

/// Which of these PRs are still worth watching. Approved, merged and closed
/// are all finished, and finished is final for the run -- the sweep lags, and
/// a stale list must not re-queue a PR on the interval that dropped it.
fn drop_finished(prs: &[u64], tracker: &mut Queue) -> Vec<u64> {
    let mut open = Vec::new();
    for &n in prs {
        if let Some(why) = prlist::pr_babysit_done(n) {
            println!("\nPR #{n} is {why}; dropping it from the babysit loop");
            tracker.mark_done(n);
        } else {
            open.push(n);
        }
    }
    open
}

/// True when no PR on the watch list could ever be reviewed again: the list
/// is empty, or everything left on it has had all its passes. Waiting on a
/// capped PR is waiting for something that cannot happen.
fn nothing_left(watching: &[u64], tracker: &Queue) -> bool {
    watching.iter().all(|&n| tracker.is_capped(n))
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
    // Three network calls stand between here and the first thing worth
    // showing. Saying which one is running turns a silent wait into a wait.
    let status = Status::new();
    status.step(step::reading_repo());
    let ctx = repo::load()?;

    let Some((numbers, titles)) = select::run(&ctx, &select_prs(cfg), &status)? else {
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
    // A --pick run may never grow past what was picked; a sweep may.
    let picked = cfg.pick.then(|| queue.clone());
    let mut tracker = Queue::new(cfg.max_passes, picked);
    // Every PR this run is responsible for, which is not the same as the
    // queue: a PR that went quiet is still open, still ours, and still worth
    // waiting on. Rebuilding this from the last pass's queue would forget it.
    let mut watching = queue.clone();
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

        // The first pass recorded each review's session; every later pass
        // resumes it whether or not --continue was passed. Re-reviewing from
        // scratch each interval would throw away the findings the author is
        // in the middle of answering.
        cfg.continue_sessions = true;

        // One place decides what happens next, and it always looks before it
        // decides: a PR opened during the pass that just ran is work, even
        // when everything this run was watching has finished.
        // Whether this stretch of waiting has already spent an interval, so
        // work found by an idle poll is reviewed now rather than an interval
        // later: the interval exists to give an author time to answer, and a
        // PR that just arrived has nothing to answer.
        let mut already_waited = false;
        let mut idle_polls = 0u32;
        let next_queue = loop {
            // The watch list shrinks only here, and only on GitHub's word:
            // the review either landed as an approval or it did not, and a
            // run that believed its own report would babysit a PR it never
            // approved.
            watching = drop_finished(&watching, &mut tracker);
            let exhausted = nothing_left(&watching, &tracker);

            let refresh = Status::new();
            refresh.step(step::fetching(&ctx.owner, &ctx.name));
            let looked = actionable_now(&cfg, &ctx);
            refresh.clear();
            let fresh = match looked {
                Ok((numbers, fresh_titles)) => {
                    refresh_failures = 0;
                    titles.extend(fresh_titles);
                    numbers
                }
                Err(e) => {
                    // A run with nothing left to watch is finished whatever
                    // the list says, so a failed look here must not turn it
                    // into a failed run.
                    if exhausted {
                        // Say the look failed even though the answer does not
                        // depend on it: otherwise the log shows a clean end
                        // and no hint that a PR opened during the last pass
                        // was never looked for.
                        eprintln!("\nwarning: could not refresh the PR list ({e:#})");
                        println!("nothing left to babysit");
                        break None;
                    }
                    // Otherwise conclude nothing from a failed look: deciding
                    // "nothing left to babysit" would end the run on one bad
                    // API call and report it as a finished one.
                    refresh_failures += 1;
                    eprintln!("\nwarning: could not refresh the PR list ({e:#})");
                    if refresh_failures >= 3 {
                        eprintln!("error: the PR list has failed to refresh 3 times; giving up");
                        break None;
                    }
                    println!("looking again in {}", babysit.normalized);
                    interruptible_sleep(&rx, std::time::Duration::from_secs(babysit.secs), &ui);
                    already_waited = true;
                    continue;
                }
            };

            let intake = tracker.next(&watching, &fresh);
            // A PR that joined is this run's responsibility from now on, so it
            // is watched until it is approved or closed -- not only while it
            // happens to be actionable.
            watching.extend(intake.joined.iter().copied());
            report_intake(&intake, &cfg);
            if !intake.queue.is_empty() {
                break Some(intake.queue);
            }
            if exhausted {
                // Nothing open to wait for, or nothing left that may be
                // reviewed again. No interval would change that.
                println!("\nnothing left to babysit");
                break None;
            }
            // An open PR nobody is touching must not keep a process alive for
            // ever -- least of all one cron started, where the next run would
            // pile on top of this one.
            //
            // The check right after a pass does not count. Our own review is
            // the latest activity on everything we just reviewed, so that one
            // is idle by construction and says nothing about whether the
            // author is coming back. Counting it would make --max-idle 1 stop
            // without ever waiting.
            if already_waited {
                idle_polls += 1;
            }
            if idle_polls >= cfg.max_idle {
                println!(
                    "\nnothing has changed in {} since the last review; stopping with {} still open",
                    ui::count(idle_polls as usize, "idle check"),
                    ui::count(watching.len(), "PR")
                );
                break None;
            }
            println!(
                "\nnothing to review right now; next check in {} ({} still open)",
                babysit.normalized,
                ui::count(watching.len(), "PR")
            );
            interruptible_sleep(&rx, std::time::Duration::from_secs(babysit.secs), &ui);
            already_waited = true;
        };

        let Some(next) = next_queue else {
            // Either everything finished, or the list stopped answering. The
            // first is a clean end; the second is not, and a cron wrapper has
            // to be able to tell them apart.
            if refresh_failures >= 3 {
                ui.show_cursor();
                return Ok(1);
            }
            break (failures, jobs.len());
        };
        queue = next;
        // The interval is what gives the author time to answer, so it is
        // spent before the next pass rather than before deciding there is one
        // -- unless the wait above already spent one, in which case spending
        // another would delay the work by twice the interval.
        if !already_waited {
            println!(
                "\nnext check in {} ({} left)",
                babysit.normalized,
                ui::count(watching.len(), "PR")
            );
            interruptible_sleep(&rx, std::time::Duration::from_secs(babysit.secs), &ui);
        }
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
