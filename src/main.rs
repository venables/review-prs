// autoreview: review the current repo's open PRs headlessly -- no terminal
// tabs. Each PR is reviewed by a `dash-p` subprocess driving claude; the run
// shows live per-PR progress, prints a summary, and exits nonzero if any
// review failed.
//
// This is the sibling of `review-prs` (bash, in this repo), which fans the
// same PRs out into one terminal tab each. The two agree on what is worth
// reviewing and on which session a PR belongs to: the selection and session
// derivation in src/ mirror lib/*.sh byte for byte, and the golden unit tests
// pin the session ids to lib/session.sh's output.

mod cli;
mod interval;
mod job;
mod prlist;
mod repo;
mod rundir;
mod session;

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
    eprintln!("autoreview: not implemented yet");
    std::process::exit(2);
}
