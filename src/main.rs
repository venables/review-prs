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

fn select_prs(cfg: &Config, ctx: &RepoContext) -> anyhow::Result<Option<Vec<u64>>> {
    let Some(prs) = prlist::fetch_prs(ctx, cfg.include_approved, cfg.include_dependabot)? else {
        return Ok(None);
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut rows = prlist::build_rows(&prs, &ctx.me, now);
    if cfg.auto {
        Ok(prlist::select_auto(&rows))
    } else {
        mark_resumable(&mut rows, ctx);
        picker::run(&rows, cfg.continue_sessions, cfg.include_dependabot)
    }
}

fn run(cfg: &Config) -> anyhow::Result<i32> {
    // gum is required by the picker alone, so an --auto sweep runs on a box
    // that has never seen it. pgrep is not optional: refusing to resume a
    // session another process holds is a safety check, not a nicety.
    repo::require_deps(&["gh", "pgrep"])?;
    let dashp = repo::dashp_bin();
    if cfg.review_cmd.is_none() {
        repo::require_deps(&[dashp.as_str()])?;
    }
    let ctx = repo::load()?;

    let Some(numbers) = select_prs(cfg, &ctx)? else {
        return Ok(0);
    };

    let mut rundir = RunDir::new(cfg.log_dir.clone())?;
    let (tx, rx) = std::sync::mpsc::channel();
    signals::install(tx.clone());
    let mut ui = ui::Ui::new();
    ui.hide_cursor();

    rundir.start_pass(1)?;
    let jobs = pool::run_pass(&numbers, cfg, &ctx, &rundir, &dashp, &rx, &tx, &mut ui);
    ui.print_summary(&jobs, &rundir.pass_dir);
    ui.show_cursor();

    // Exit nonzero when any review in the final pass did not complete
    // cleanly, so a cron job or a CI step can tell a finished sweep from a
    // broken one.
    let failures = pool::failures(&jobs);
    if failures > 0 {
        eprintln!("error: {failures} of {} review(s) failed", jobs.len());
        return Ok(1);
    }
    Ok(0)
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
        // The message was already printed where the failure happened.
        Err(_) => std::process::exit(1),
    }
}
