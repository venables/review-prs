// panel: review one change with several models at once, then synthesize.
//
// The third binary in this crate, and the same machinery as the other two: a
// dash-p fan-out with per-run output directories. The unit of work is what
// differs -- N models on one diff, where autoreview runs N PRs through one
// model.
//
// The shape is flat on purpose. Spawning panelists, polling them, retrying a
// transient failure and collecting the results are mechanical, so a program
// does them. Deciding which findings are real needs a model with the code in
// front of it, so exactly one model call does that, at the end.

use autoreview::panel::{self, cli};
use autoreview::repo::AlreadyReported;

fn main() {
    let cfg = match cli::parse(autoreview::cli::args_or_exit(), &autoreview::cli::real_env) {
        Ok(cli::Parsed::Help) => {
            print!("{}", cli::HELP);
            std::process::exit(0);
        }
        Ok(cli::Parsed::Version) => {
            println!("{}", autoreview::cli::version("panel"));
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
    match panel::run(&cfg) {
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
