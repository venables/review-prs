// review-prs: pick open non-draft PRs in the current repo and fan a review
// command into a new terminal tab per selection -- one tab you can watch and
// interrupt per PR.
//
// This is the sibling of `autoreview`, the other binary in this crate, which
// reviews the same PRs headlessly. Pick this one to watch a review happen and
// steer it mid-flight; pick autoreview when there is no terminal to spawn into
// (ssh, cron, CI), or when a dozen PRs would mean a dozen tabs.

use autoreview::repo::AlreadyReported;
use autoreview::tabs::{self, cli};

fn main() {
    let cfg = match cli::parse(autoreview::cli::args_or_exit(), &autoreview::cli::real_env) {
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
    match tabs::run(&cfg) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            // Sites that already explained themselves bail AlreadyReported;
            // everything else is printed here rather than exiting silently.
            if e.downcast_ref::<AlreadyReported>().is_none() {
                eprintln!("error: {e:#}");
            }
            std::process::exit(1)
        }
    }
}
